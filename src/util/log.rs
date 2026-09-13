//! 极简分级日志（不引入 log/env_logger 等依赖，保持依赖树最小）。
//!
//! # 使用约定（全项目统一）
//! - **打点密度**：每个有外部副作用的动作打一条——启动/退出、网络请求失败、
//!   播放状态切换、缓存命中/落盘、托盘/字体等子系状态变化。纯 UI 重绘、
//!   每帧轮询、循环内的常规迭代**一律不打**，避免刷屏。
//! - **级别**：错误（操作失败但应用继续）用 [`error`]；关键路径里程碑
//!   （启动完成、开始播放、登录成功）用 [`info`]；诊断细节（重试、降级、
//!   命中/未命中）用 [`debug`]。
//! - **格式**：`[时间 级别 模块] 消息`，时间精确到秒；`debug` 级别额外带
//!   线程名（后台线程的诊断问题大多和「哪个线程」强相关）。
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

/// 组装好的单行日志：`[HH:MM:SS LEVEL 模块] 消息（线程名）`。
struct LogLine {
    text: String,
}

impl LogLine {
    fn new(level: Level, module: &str, message: &str) -> Self {
        let mut text = String::with_capacity(48 + message.len());
        text.push('[');
        text.push_str(&hhmmss_now());
        text.push(' ');
        text.push_str(level.tag());
        text.push(' ');
        text.push_str(module);
        text.push_str("] ");
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

/// 当前时刻的 `HH:MM:SS`（UTC，本地时区在无显示环境的容器里不可靠）。
fn hhmmss_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 一天内偏移（不引入 chrono）：直接算 时:分:秒。
    let day_secs = secs % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        day_secs / 3600,
        (day_secs % 3600) / 60,
        day_secs % 60
    )
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
    fn line_format_has_time_level_module_message() {
        let line = LogLine::new(Level::Info, "app", "启动完成");
        let s = line.to_string();
        assert!(s.starts_with('['), "行首应是时间戳: {s}");
        assert!(s.contains(" INFO "), "应含级别标签: {s}");
        assert!(s.contains(" app] "), "应含模块名: {s}");
        assert!(s.ends_with("启动完成"), "应以消息结尾: {s}");
    }

    #[test]
    fn hhmmss_is_zero_padded() {
        let t = hhmmss_now();
        assert_eq!(t.len(), 8, "HH:MM:SS 共 8 字符: {t}");
        assert_eq!(t.as_bytes()[2], b':');
        assert_eq!(t.as_bytes()[5], b':');
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
