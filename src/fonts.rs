//! 字体加载：egui 默认字体不含 CJK，歌词/界面中文会渲染成方块；界面图标
//! 不用 emoji（跨平台字形缺失会显示 "?"），改用内嵌的 Phosphor 图标字体。
//!
//! 策略（优先级从高到低）：
//! 1. 内嵌 Noto Sans SC Regular（`assets/NotoSansSC-Regular.otf`，`include_bytes!` 编译期嵌入，
//!    运行时不读盘、不依赖系统字体），负责全部 CJK + 拉丁字形；
//! 2. 内嵌 Phosphor 图标字体（`assets/Phosphor.ttf`，MIT 协议，48 万字节），负责界面
//!    PUA 码点字形（音乐/齿轮/关闭/播放控制等），所有图标走 `crate::icons::*`；
//! 3. egui 默认字体（含 NotoEmoji/EmojiIcon）仍在列表中作最终兜底。
//!
//! 注册顺序：CJK 字体插到 `Proportional`/`Monospace` 两个 family 的首位，图标字体紧随其后
//! （index 1）。egui 按 family 列表顺序逐个查字形，PUA 码点只有 Phosphor 收录，会自动回退
//! 到它，不会干扰正常文字的命中。

use eframe::egui;
use std::path::PathBuf;

/// 内嵌 CJK 字体在 FontDefinitions 里的键名。
const EMBEDDED_KEY: &str = "noto_sc";
/// 内嵌图标字体（Phosphor，MIT）在 FontDefinitions 里的键名。
const PHOSPHOR_KEY: &str = "phosphor_icons";

/// 字体来源说明（用于启动报告）。
#[derive(Debug, Clone, PartialEq)]
pub enum FontChoice {
    /// 内嵌 Noto Sans SC。
    Embedded,
    /// 运行时探测到的系统 CJK 字体（路径）。
    System(PathBuf),
}

/// 安装字体：CJK + Phosphor 图标字体，均编译期内嵌。
/// 返回实际采用的来源（内嵌必成功；系统探测仅作历史兼容保留）。
pub fn install_fonts(ctx: &egui::Context) -> FontChoice {
    let mut fonts = egui::FontDefinitions::default();
    // CJK 字体。
    fonts.font_data.insert(
        EMBEDDED_KEY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(
            include_bytes!("../assets/NotoSansSC-Regular.otf"),
        )),
    );
    // 图标字体（Phosphor，PUA 码点）。
    fonts.font_data.insert(
        PHOSPHOR_KEY.to_owned(),
        std::sync::Arc::new(egui::FontData::from_static(
            include_bytes!("../assets/Phosphor.ttf"),
        )),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let list = fonts.families.entry(family).or_default();
        // CJK 优先；图标字体紧随其后（PUA 码点在此命中，不干扰正常文字）。
        list.insert(0, EMBEDDED_KEY.to_owned());
        list.insert(1, PHOSPHOR_KEY.to_owned());
    }
    ctx.set_fonts(fonts);
    FontChoice::Embedded
}

/// 探测系统上的 CJK 字体（运行时兜底，历史兼容；内嵌字体必存在，一般不会走到）。
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
