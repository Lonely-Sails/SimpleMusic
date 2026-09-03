//! 自定义标题栏：拖动区域 + 窗口控制按钮（最小化/关闭）+ 右下角缩放把手。

use crate::{icons, theme};
use eframe::egui::{
    self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, Vec2,
};
use super::MusicApp;

/// 自定义标题栏高度。
const TITLEBAR_HEIGHT: f32 = 48.0;
/// 标题栏左右留白（略大于上下，让四周边距更透气）。
const TITLEBAR_SIDE_PAD: f32 = 20.0;

impl MusicApp {
    pub(crate) fn show_custom_title_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top(egui::Id::new("title_bar"))
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::ZERO))
            .show(ui, |ui| {
                ui.set_min_height(TITLEBAR_HEIGHT);
                let bar = ui.max_rect();
                // 顶部两角圆角，与卡片衔接
                let corner = CornerRadius {
                    nw: theme::CORNER_XL,
                    ne: theme::CORNER_XL,
                    sw: 0,
                    se: 0,
                };
                ui.painter().rect_filled(bar, corner, theme::TITLEBAR_BG);
                // 底部分隔线
                ui.painter().line_segment(
                    [bar.left_bottom() + Vec2::new(0.0, -0.5), bar.right_bottom() + Vec2::new(0.0, -0.5)],
                    Stroke::new(1.0, theme::BORDER_SOFT),
                );

                // 拖动区域（底层）
                ui.interact(bar, ui.id().with("titlebar_drag"), Sense::drag());

                ui.horizontal(|ui| {
                    ui.add_space(TITLEBAR_SIDE_PAD);
                    // 音符图标 + 应用名（拖动把手）。
                    let (note_rect, note_resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::drag());
                    icons::note(ui.painter(), note_rect, theme::ACCENT);
                    ui.add_space(4.0);
                    let title = egui::Label::new(
                        RichText::new("SimpleMusic").strong().color(theme::TEXT_PRIMARY),
                    )
                    .selectable(false)
                    .sense(Sense::drag());
                    let tr = ui.add(title);
                    if tr.drag_started() || note_resp.drag_started() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                    }
                    if tr.hovered() || note_resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    tr.on_hover_text("拖动移动窗口");

                    // 右侧：窗口控制按钮（外边距与左侧对称，比上下略大）
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(TITLEBAR_SIDE_PAD);
                        // 关闭（最小化到托盘 / 退出）
                        if self.window_ctrl_button(ui, icons::cross, "关闭").clicked() {
                            self.request_close(ui.ctx());
                        }
                        ui.add_space(4.0);
                        // 最小化
                        if self.window_ctrl_button(ui, icons::window_minimize, "最小化").clicked() {
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                        }
                    });
                });
            });
    }

    /// 窗口控制按钮（圆角小方块）。
    fn window_ctrl_button(
        &self,
        ui: &mut egui::Ui,
        icon: fn(&egui::Painter, Rect, Color32),
        tooltip: &str,
    ) -> egui::Response {
        let size = Vec2::splat(24.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let bg = if resp.is_pointer_button_down_on() {
            theme::BG_ACTIVE
        } else if resp.hovered() {
            theme::BG_HOVER
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, CornerRadius::same(theme::CORNER), bg);
        }
        icon(ui.painter(), rect.shrink(4.0), theme::TEXT_SECONDARY);
        resp.on_hover_text(tooltip)
    }

    // ---- 右下角缩放把手 ----

    pub(crate) fn show_resize_grip(&mut self, ui: &mut egui::Ui) {
        let size = Vec2::splat(18.0);
        // 固定在窗口实际右下角（不随面板布局偏移）。
        let win_rect = ui.ctx().input(|i| i.viewport().inner_rect);
        let bottom_right = win_rect.map(|r| r.right_bottom()).unwrap_or_else(|| ui.max_rect().right_bottom());
        let rect = Rect::from_min_size(bottom_right - size, size);
        let resp = ui.interact(rect, ui.id().with("resize_grip"), Sense::drag());
        icons::window_resize(ui.painter(), rect, theme::TEXT_WEAK);
        if resp.drag_started() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(
                egui::ResizeDirection::SouthEast,
            ));
        }
        resp.on_hover_text("调整窗口大小");
    }
}