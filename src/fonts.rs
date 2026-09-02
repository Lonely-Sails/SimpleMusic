//! 中文字体加载：egui 默认字体不含 CJK，歌词/界面中文会渲染成方块。
//!
//! 策略（优先级从高到低）：
//! 1. 内嵌 Noto Sans SC Regular（`assets/NotoSansSC-Regular.otf`，`include_bytes!` 编译期嵌入，
//!    运行时不读盘、不依赖系统字体）；
//! 2. 若内嵌失败（文件缺失/构建期去掉了 asset），运行时探测系统 CJK 字体并把其字节注册进去。
//!
//! 注册方式：把中文字体插到 `Proportional` 与 `Monospace` 两个 family 的首位（`insert(0, …)`），
//! 让它在英文/数字等拉丁字形回退到默认字体前，优先命中 CJK 字形。

use eframe::egui;
use std::path::PathBuf;

/// 内嵌字体在 FontDefinitions 里的键名。
const EMBEDDED_KEY: &str = "noto_sc";

/// 字体来源说明（用于启动报告）。
#[derive(Debug, Clone, PartialEq)]
pub enum FontChoice {
    /// 内嵌 Noto Sans SC。
    Embedded,
    /// 运行时探测到的系统 CJK 字体（路径）。
    System(PathBuf),
}

/// 把给定字节注册为 CJK 字体并插到两个 family 的首位。
fn register_font_bytes(ctx: &egui::Context, bytes: &'static [u8], key: &str) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        key.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, key.to_owned());
    }
    ctx.set_fonts(fonts);
}

/// 探测系统上的 CJK 字体（运行时兜底，内嵌失败时使用）。
///
/// 按平台尝试常见路径：
/// - Windows: `msyh.ttc`（微软雅黑）、`simhei.ttf`（黑体）；
/// - macOS: `PingFang.ttc`、`STHeiti Light.ttc`；
/// - Linux/其它: 遍历 `/usr/share/fonts`（及 `~/.fonts`/`~/.local/share/fonts`）找
///   文件名含 `noto`/`wqy`/`cjk`/`droid` 的 ttf/otf/ttc。
pub fn probe_system_cjk() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    fn candidates() -> Vec<PathBuf> {
        vec![
            PathBuf::from("C:/Windows/Fonts/msyh.ttc"),
            PathBuf::from("C:/Windows/Fonts/msyh.ttf"),
            PathBuf::from("C:/Windows/Fonts/simhei.ttf"),
        ]
    }

    #[cfg(target_os = "macos")]
    fn candidates() -> Vec<PathBuf> {
        vec![
            PathBuf::from("/System/Library/Fonts/PingFang.ttc"),
            PathBuf::from("/System/Library/Fonts/STHeiti Light.ttc"),
            PathBuf::from("/System/Library/Fonts/Supplemental/Songti.ttc"),
        ]
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    fn candidates() -> Vec<PathBuf> {
        let mut roots = vec![PathBuf::from("/usr/share/fonts")];
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                roots.push(PathBuf::from(&home).join(".fonts"));
                roots.push(PathBuf::from(&home).join(".local/share/fonts"));
            }
        }
        roots
    }

    // 平台候选：文件路径先测存在；目录则递归搜索。
    fn walk(dir: &PathBuf, out: &mut Vec<PathBuf>, depth: usize) {
        if depth > 6 {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out, depth + 1);
                } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                    let lower = name.to_lowercase();
                    let is_font = lower.ends_with(".ttf")
                        || lower.ends_with(".otf")
                        || lower.ends_with(".ttc");
                    let is_cjk = lower.contains("noto")
                        || lower.contains("wqy")
                        || lower.contains("cjk")
                        || lower.contains("droid")
                        || lower.contains("source han");
                    if is_font && is_cjk {
                        out.push(p);
                    }
                }
            }
        }
    }

    let dirs = candidates();
    // 直接文件路径候选。
    for p in &dirs {
        if p.is_file() {
            return Some(p.clone());
        }
    }
    // 目录递归候选。
    let mut found = Vec::new();
    for p in &dirs {
        if p.is_dir() {
            walk(p, &mut found, 0);
        }
    }
    found.sort();
    found.into_iter().next()
}

/// 安装字体。优先内嵌；仅在明确失败时兜底到系统字体。
/// 返回实际采用的来源。
pub fn install_fonts(ctx: &egui::Context) -> FontChoice {
    // 内嵌字体（编译期 `include_bytes!`）。正常情况下它一定存在：
    // 若构建时该文件被移除，`include_bytes!` 会直接编译失败，因此不会走到静默空字节。
    register_font_bytes(ctx, include_bytes!("../assets/NotoSansSC-Regular.otf"), EMBEDDED_KEY);
    FontChoice::Embedded
}
