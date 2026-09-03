//! 界面图标 —— Phosphor 图标字体（内嵌，MIT 协议）。
//!
//! 背景：早期用 emoji/媒体控制码点（⏮⏭⏸▶…）做图标，egui 默认字体缺字形，
//! macOS 上渲染成 "?"；随后改用手绘 painter 矢量图标，用户仍嫌丑。现在改为
//! 专业图标字体 Phosphor（`assets/Phosphor.ttf`，见 [`crate::fonts`]），字形统一、
//! 观感专业，且彻底规避字体缺失问题。
//!
//! 约定：所有函数签名统一为
//! `fn xxx(painter: &egui::Painter, rect: egui::Rect, color: egui::Color32)`，
//! 内部把字形画到 `rect` 中心，字体大小按 `rect` 与各自 scale 折算。
//!
//! 码点取自 Phosphor 官方 selection.json（PUA 区），修改前请对照
//! `https://github.com/phosphor-icons/phosphor-icons` 确认。

use eframe::egui::{Align2, Color32, FontId, Painter, Rect};

/// Phosphor 图标码点（PUA）。仅本模块内部使用。
mod glyph {
    pub const PLAY: char = '\u{e3d0}';
    pub const PAUSE: char = '\u{e39e}';
    pub const SKIP_BACK: char = '\u{e5a4}';
    pub const SKIP_FORWARD: char = '\u{e5a6}';
    pub const SPEAKER_HIGH: char = '\u{e44a}';
    pub const MUSIC_NOTE: char = '\u{e33c}';
    pub const MUSIC_NOTES: char = '\u{e340}';
    pub const FOLDER: char = '\u{e24a}';
    pub const GEAR: char = '\u{e270}';
    pub const X: char = '\u{e4f6}';
    pub const MINUS: char = '\u{e32a}';
    pub const CORNERS_OUT: char = '\u{e1d0}';
}

/// 把单个 Phosphor 字形画到 `rect` 中心。
///
/// `scale` 是相对 `rect` 短边的字体放大系数：Phosphor 字形墨水约占 em 的
/// 0.8 左右，用 ~1.2 让图标视觉上接近铺满原 painter 的 rect。
fn paint(painter: &Painter, rect: Rect, ch: char, scale: f32, color: Color32) {
    let size = rect.height().min(rect.width()) * scale;
    let font_id = FontId::proportional(size.max(1.0));
    painter.text(rect.center(), Align2::CENTER_CENTER, ch.to_string(), font_id, color);
}

/// 播放：实心右三角。
pub fn play(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::PLAY, 1.05, color);
}

/// 暂停：两条圆头竖杠。
pub fn pause(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::PAUSE, 1.1, color);
}

/// 上一首（Phosphor skip-back：竖条 + 左三角）。
pub fn prev(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::SKIP_BACK, 1.05, color);
}

/// 下一首（Phosphor skip-forward：右三角 + 竖条）。
pub fn next(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::SKIP_FORWARD, 1.05, color);
}

/// 音量（喇叭 + 声波）。
pub fn volume(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::SPEAKER_HIGH, 1.1, color);
}

/// 单音符。
pub fn note(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::MUSIC_NOTE, 1.15, color);
}

/// 双音符。
pub fn note_double(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::MUSIC_NOTES, 1.15, color);
}

/// 文件夹。
pub fn folder(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::FOLDER, 1.1, color);
}

/// 齿轮（设置）。
pub fn gear(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::GEAR, 1.15, color);
}

/// 关闭 ✕。
pub fn cross(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::X, 0.95, color);
}

/// 标题栏：最小化 —— 一条短横线。
pub fn window_minimize(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::MINUS, 1.4, color);
}

/// 标题栏/角落：右下角缩放把手 —— 四角外扩箭头。
pub fn window_resize(painter: &Painter, rect: Rect, color: Color32) {
    paint(painter, rect, glyph::CORNERS_OUT, 1.3, color);
}
