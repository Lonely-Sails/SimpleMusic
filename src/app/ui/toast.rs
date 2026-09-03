//! 顶部 Toast 提示：错误（暖红）/轻提示（金色），在窗口顶部居中弹出、自动淡出。
//!
//! 取代原先直接内联在底部播放条里的文字提示（错误用红色、轻提示用金色），改为更通用、
//! 不打断布局的临时浮层。多条同时出现时上下堆叠（最新在最上方）。

use crate::theme;
use eframe::egui::{self, Align2, RichText, Stroke, Vec2};
use std::time::{Duration, Instant};

/// 轻提示展示时长。
const NOTICE_MS: Duration = Duration::from_millis(3000);
/// 错误提示展示时长（稍长，方便看清）。
const ERROR_MS: Duration = Duration::from_millis(5200);

/// Toast 类型：决定配色。`Error` = 暖红（错误），`Notice` = 金色（成功/信息类轻提示）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Error,
    Notice,
}

/// 一条待展示的 toast。
pub struct Toast {
    pub msg: String,
    pub kind: ToastKind,
    pub at: Instant,
}

impl Toast {
    pub fn new(msg: impl Into<String>, kind: ToastKind) -> Self {
        Self {
            msg: msg.into(),
            kind,
            at: Instant::now(),
        }
    }

    fn ttl(&self) -> Duration {
        match self.kind {
            ToastKind::Error => ERROR_MS,
            ToastKind::Notice => NOTICE_MS,
        }
    }

    /// 透明度：展示期不透明，最后 20% 平滑淡出。
    fn alpha(&self) -> f32 {
        let ttl = self.ttl().as_secs_f32();
        let frac = (1.0 - self.at.elapsed().as_secs_f32() / ttl).clamp(0.0, 1.0);
        if frac < 0.2 {
            (frac / 0.2).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    pub fn expired(&self) -> bool {
        self.at.elapsed() > self.ttl()
    }
}

/// 绘制顶部 toast 叠层，并清理已过期的 toast。
///
/// 务必每帧调用；内部会按需 `request_repaint_after`，保证即使无播放等其它重绘源，
/// toast 也能按时淡出消失。
pub fn show_toasts(ctx: &egui::Context, toasts: &mut Vec<Toast>) {
    toasts.retain(|t| !t.expired());
    if toasts.is_empty() {
        return;
    }
    // 空闲时也要持续重绘，驱动淡出与移除。
    ctx.request_repaint_after(Duration::from_millis(40));

    // 最新在最上方：逆序迭代（Vec 本身按时间先后存放，便于过期清理）。
    let items: Vec<&Toast> = toasts.iter().rev().collect();

    egui::Area::new(egui::Id::new("simple_music_toasts"))
        // 顶部居中弹出；y 偏移避开 40px 高的自定义标题栏（不遮挡窗口控制按钮）。
        .anchor(Align2::CENTER_TOP, [0.0, 52.0])
        .order(egui::Order::Foreground)
        .interactable(false)
        .show(ctx, |ui| {
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing = Vec2::new(0.0, 8.0);
                for t in items {
                    let is_err = t.kind == ToastKind::Error;
                    let alpha = t.alpha();
                    let bg = (if is_err {
                        theme::BG_ACTIVE
                    } else {
                        theme::BG_CARD
                    })
                    .gamma_multiply(alpha);
                    let fg = (if is_err {
                        theme::TEXT_ERROR
                    } else {
                        theme::GOLD
                    })
                    .gamma_multiply(alpha);
                    let border = if is_err {
                        theme::TEXT_ERROR
                    } else {
                        theme::BORDER_SOFT
                    };
                    egui::Frame::new()
                        .fill(bg)
                        .stroke(Stroke::new(1.0, border.gamma_multiply(alpha)))
                        .corner_radius(theme::CORNER)
                        .inner_margin(egui::Margin::symmetric(16, 10))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&t.msg).color(fg));
                        });
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_expires_after_ttl() {
        let mut t = Toast::new("hi", ToastKind::Notice);
        // 构造一个已过期的 toast：把 at 前移。
        t.at = Instant::now() - NOTICE_MS - Duration::from_secs(1);
        assert!(t.expired());
        assert!(!t.alpha().is_normal() || t.alpha() <= 0.0, "过期后透明度应趋于 0");
    }

    #[test]
    fn toast_new_is_not_expired() {
        let t = Toast::new("hi", ToastKind::Error);
        assert!(!t.expired());
        assert_eq!(t.alpha(), 1.0);
    }

    /// show_toasts 应在帧内正常绘制且保留未过期 toast（API 冒烟）。
    #[test]
    fn show_toasts_renders_without_panic() {
        let ctx = egui::Context::default();
        let mut toasts = vec![Toast::new("测试提示", ToastKind::Notice)];
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 400.0));
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(screen);
        let mut full = ctx.run_ui(input, |ui| {
            show_toasts(ui.ctx(), &mut toasts);
        });
        full.textures_delta.clear();
        assert_eq!(toasts.len(), 1, "未过期 toast 应保留");
    }

    /// show_toasts 应清理已过期的 toast。
    #[test]
    fn show_toasts_prunes_expired() {
        let ctx = egui::Context::default();
        let mut t = Toast::new("过期", ToastKind::Error);
        t.at = Instant::now() - ERROR_MS - Duration::from_secs(1);
        let mut toasts = vec![t];
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 400.0));
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(screen);
        let mut full = ctx.run_ui(input, |ui| {
            show_toasts(ui.ctx(), &mut toasts);
        });
        full.textures_delta.clear();
        assert!(toasts.is_empty(), "过期 toast 应被清理");
    }
}
