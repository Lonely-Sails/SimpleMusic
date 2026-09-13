//! 极简分级日志（不引入 log/env_logger 等依赖，保持依赖树最小）。
//!
//! # 使用约定（全项目统一）
//! - **打点密度**：每个有外部副作用的动作打一条——启动/退出、网络请求失败、
//!   播放状态切换、缓存命中/落盘、托盘/字体等子系状态变化。纯 UI 重绘、
//!   每帧轮询、循环内的常规迭代**一律不打**，避免刷屏。
//! - **级别**：错误（操作失败但应用继续）用 [`error`]；关键路径里程碑
//!   （启动完成、开始播放、登录成功）用 [`info`]；诊断细节（重试、降级、
//!   命中/未命中）用 [`debug`]。
//! - **格式**：`时间 [级别] 模块 | 消息`，时间为本地时区 `YYYY-MM-DD HH:MM:SS`；
//!   `debug` 级别额外带线程名（后台线程的诊断问题大多和「哪个线程」强相关）。
//! - **目标**：stderr（终端可重定向；GUI 应用不受 stdout 缓冲影响）。
//! - **脱敏**：消息由调用方负责——cookie/SESSDATA 等凭据绝不入日志
//!   （`BiliSession` 的 Debug 已脱敏，沿用该约定）。
//!
//! # 级别控制
//! 环境变量 `SIMPLEMUSIC_LOG`：`error` / `warn` / `info` / `debug`（大小写不敏感，
//! 默认 `info`）。解析失败回退 `info`，绝不 panic。
//!
//! ```
//! # use simple_music::util::log;
//! log::info("app", "启动完成");
//! let e = "timeout";
//! log::error("audio", &format!("下载失败: {e}"));
//! ```

use std::fmt;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// 日志级别（严重度从低到高）。
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 诊断细节（重试/降级/缓存命中），默认不显示。
    Debug = 0,
    /// 关键路径里程碑，默认显示。
    Info = 1,
    /// 容易出问题的分支（回退、降级、可恢复失败）。
    Warn = 2,
    /// 操作失败但应用继续运行。
    Error = 3,
}

impl Level {
    /// 级别标签（日志行里显示的名字）。
    fn tag(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }

    /// 从 `SIMPLEMUSIC_LOG` 环境变量解析级别；未设置/非法值回退 `Info`。
    fn from_env() -> Level {
        match std::env::var("SIMPLEMUSIC_LOG") {
            Ok(v) => Level::parse(&v).unwrap_or(Level::Info),
            Err(_) => Level::Info,
        }
    }

    /// 解析级别名（大小写不敏感）；`log` / `all` 视为 `Debug`（最详尽）。
    pub fn parse(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "debug" | "trace" | "all" | "log" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// 当前生效级别（进程内只解析一次环境变量）。
fn threshold() -> Level {
    static THRESHOLD: OnceLock<Level> = OnceLock::new();
    *THRESHOLD.get_or_init(Level::from_env)
}

/// 一条日志是否会被输出（调用方可用它跳过昂贵的消息拼接）。
pub fn enabled(level: Level) -> bool {
    level >= threshold()
}

/// 写一行日志。公开为 `log::write` 仅为可测试性；常规代码用 [`info`] 等包装。
pub fn write(level: Level, module: &str, message: &str) {
    if !enabled(level) {
        return;
    }
    let mut line = LogLine::new(level, module, message);
    // debug 级别带线程名：后台线程（歌词/解析/播放）的问题大多和线程强相关。
    if level == Level::Debug {
        if let Some(name) = std::thread::current().name() {
            line.push(" ", name);
        }
    }
    eprintln!("{line}");
}

/// `INFO` 级日志：关键路径里程碑。
pub fn info(module: &str, message: &str) {
    write(Level::Info, module, message);
}

/// `WARN` 级日志：可恢复的异常分支（回退/降级）。
pub fn warn(module: &str, message: &str) {
    write(Level::Warn, module, message);
}

/// `ERROR` 级日志：操作失败但应用继续。
pub fn error(module: &str, message: &str) {
    write(Level::Error, module, message);
}

/// `DEBUG` 级日志：诊断细节（默认不显示，`SIMPLEMUSIC_LOG=debug` 开启）。
pub fn debug(module: &str, message: &str) {
    write(Level::Debug, module, message);
}

/// 组装好的单行日志：`YYYY-MM-DD HH:MM:SS [LEVEL] 模块 | 消息（线程名）`。
struct LogLine {
    text: String,
}

impl LogLine {
    fn new(level: Level, module: &str, message: &str) -> Self {
        let mut text = String::with_capacity(64 + message.len());
        text.push_str(&local_datetime_now());
        text.push_str(" [");
        text.push_str(level.tag());
        text.push_str("] ");
        text.push_str(module);
        text.push_str(" | ");
        text.push_str(message);
        Self { text }
    }

    fn push(&mut self, sep: &str, extra: &str) {
        self.text.push_str(sep);
        self.text.push_str(extra);
    }
}

impl fmt::Display for LogLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// 当前本地时刻的 `YYYY-MM-DD HH:MM:SS`。
///
/// 走系统 C 库的本地时间转换（Unix `localtime_r` / Windows `localtime_s`），
/// 自动跟随系统时区与夏令时；**不引入 chrono 等额外依赖**。转换失败时回退
/// UTC 手算（日志时间戳不参与业务逻辑，只需保证不 panic）。
fn local_datetime_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let time = secs as libc::time_t;

    // SAFETY: `tm` 是 POD，零初始化合法；两个 libc 函数都会把结果写入该结构体，
    // 失败时返回空指针 / 非零错误码，已在下面检查。
    unsafe {
        let mut tm = std::mem::zeroed::<libc::tm>();
        #[cfg(unix)]
        {
            if libc::localtime_r(&time, &mut tm).is_null() {
                return utc_datetime(secs);
            }
        }
        #[cfg(windows)]
        {
            if libc::localtime_s(&mut tm, &time) != 0 {
                return utc_datetime(secs);
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            return utc_datetime(secs);
        }
        #[cfg(any(unix, windows))]
        return format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
        );
    }
}

/// 本地时间转换不可用时的兜底：按 UTC 手算 `YYYY-MM-DD HH:MM:SS`。
fn utc_datetime(secs: u64) -> String {
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let day_secs = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60
    )
}

/// 把「1970-01-01 起的天数」转成公历 `(年, 月, 日)`（Howard Hinnant 的算法）。
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_parse_accepts_common_names_case_insensitive() {
        assert_eq!(Level::parse("debug"), Some(Level::Debug));
        assert_eq!(Level::parse(" INFO "), Some(Level::Info));
        assert_eq!(Level::parse("Warning"), Some(Level::Warn));
        assert_eq!(Level::parse("ERROR"), Some(Level::Error));
        assert_eq!(Level::parse("all"), Some(Level::Debug));
        assert_eq!(Level::parse("verbose"), None);
        assert_eq!(Level::parse(""), None);
    }

    #[test]
    fn level_ordering_matches_severity() {
        assert!(Level::Error > Level::Warn);
        assert!(Level::Warn > Level::Info);
        assert!(Level::Info > Level::Debug);
    }

    #[test]
    fn line_format_is_time_level_module_pipe_message() {
        let line = LogLine::new(Level::Info, "app", "启动完成");
        let s = line.to_string();
        // 形如 `2026-09-13 14:30:22 [INFO] app | 启动完成`
        let (head, rest) = s.split_once(' ').expect("应有时间戳: {s}");
        assert_eq!(head.len(), 10, "日期应为 YYYY-MM-DD: {s}");
        assert_eq!(head.as_bytes()[4], b'-');
        assert_eq!(head.as_bytes()[7], b'-');
        assert!(
            rest.contains(" [INFO] app | "),
            "级别/模块/分隔符格式错误: {s}"
        );
        assert!(s.ends_with("启动完成"), "应以消息结尾: {s}");
    }

    #[test]
    fn local_datetime_is_well_formed() {
        let t = local_datetime_now();
        // `YYYY-MM-DD HH:MM:SS` 共 19 字符。
        assert_eq!(t.len(), 19, "本地时间戳格式: {t}");
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[10..11], " ");
        assert_eq!(&t[13..14], ":");
        assert_eq!(&t[16..17], ":");
        let year: i32 = t[..4].parse().expect("年份应可解析");
        assert!((1970..=9999).contains(&year), "年份越界: {t}");
    }

    #[test]
    fn civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1)); // 2024-01-01
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
    }

    #[test]
    fn enabled_filters_by_threshold() {
        // 默认 Info：Debug 不可见，Error 一定可见。
        // （CI 环境可能设了 SIMPLEMUSIC_LOG，这里只断言相对关系。）
        if enabled(Level::Info) {
            assert!(enabled(Level::Warn) && enabled(Level::Error));
        } else {
            assert!(!enabled(Level::Debug));
        }
    }

    #[test]
    fn write_never_panics_with_weird_input() {
        // 空串/超长串/控制字符都要能安全输出（写 stderr 失败会被 eprintln 吞掉）。
        write(Level::Error, "", "");
        write(Level::Debug, "模\n块", &"x".repeat(4000));
    }
}
