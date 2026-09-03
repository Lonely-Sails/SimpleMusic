//! 自定义标题栏：拖动区域 + 窗口控制按钮（最小化/关闭）+ 右下角缩放把手。

use crate::{icons, theme};
use eframe::egui::{
    self, Color32, CornerRadius, Rect, RichText, Sense, Stroke, Vec2,
};
use super::MusicApp;

/// 标题栏内容高度（窗口控制按钮即为此高度，是整行最高的元素）。
const TITLEBAR_CONTENT_HEIGHT: f32 = 24.0;
/// 标题栏上下留白。
const TITLEBAR_V_PAD: f32 = 12.0;
/// 标题栏左右留白（略大于上下，让四周边距更透气）。
const TITLEBAR_SIDE_PAD: f32 = 20.0;
/// 自定义标题栏高度 = 上下留白 + 内容高度（多余空间由内容行均分到上下两侧）。
const TITLEBAR_HEIGHT: f32 = TITLEBAR_CONTENT_HEIGHT + 2.0 * TITLEBAR_V_PAD;

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
                    // 让整行占满标题栏高度：egui 的横向布局会把每个元素在行内垂直居中，
                    // 于是上下留白各为 TITLEBAR_V_PAD，而不是全部堆到内容下方。
                    ui.set_height(TITLEBAR_HEIGHT);
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

                    // 右侧：窗口控制按钮（右边距与左边距对称，均为 TITLEBAR_SIDE_PAD）
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
        let size = Vec2::splat(TITLEBAR_CONTENT_HEIGHT);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// 在 800x600 的假窗口里复刻标题栏面板结构，返回（整条标题栏, 内容元素）的矩形。
    ///
    /// `row_fill_height` 为 true 时复刻修复后的写法（行 set_height 占满标题栏）。
    fn layout_probe(row_fill_height: bool) -> (Rect, Rect) {
        let ctx = egui::Context::default();
        let bar_cell: Rc<RefCell<Rect>> = Rc::new(RefCell::new(Rect::NOTHING));
        let item_cell: Rc<RefCell<Rect>> = Rc::new(RefCell::new(Rect::NOTHING));

        let mut input = egui::RawInput::default();
        input.screen_rect = Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(800.0, 600.0),
        ));

        let bar = Rc::clone(&bar_cell);
        let item = Rc::clone(&item_cell);
        let out = ctx.run_ui(input, move |ui| {
            // 与 show_custom_title_bar 相同的面板框架。
            egui::Panel::top(egui::Id::new("title_bar_test"))
                .frame(
                    egui::Frame::new()
                        .fill(Color32::TRANSPARENT)
                        .inner_margin(egui::Margin::ZERO),
                )
                .show(ui, |ui| {
                    ui.set_min_height(TITLEBAR_HEIGHT);
                    *bar.borrow_mut() = ui.max_rect();

                    // 与内容行相同的横向布局（按钮/标题在行内垂直居中）。
                    ui.horizontal(|ui| {
                        if row_fill_height {
                            ui.set_height(TITLEBAR_HEIGHT);
                        }
                        let (r, _) = ui.allocate_exact_size(
                            Vec2::splat(TITLEBAR_CONTENT_HEIGHT),
                            Sense::hover(),
                        );
                        *item.borrow_mut() = r;
                    });
                });
        });
        out.drop_without_applying_deltas();
        (*bar_cell.borrow(), *item_cell.borrow())
    }

    /// 标题栏四周留白必须对称：上下各 TITLEBAR_V_PAD，不能把总高减内容高的余量全堆到底部。
    ///
    /// 回归：之前内容行只靠 `set_min_height` 撑面板、行本身贴在面板顶部，
    /// 导致「上下间距」全部表现为标题栏的下面间距（上 0 / 下 24）。
    #[test]
    fn titlebar_padding_is_symmetric() {
        let (bar, item) = layout_probe(true);

        assert!(
            (bar.height() - TITLEBAR_HEIGHT).abs() < 0.6,
            "标题栏总高应为 {TITLEBAR_HEIGHT}，实际 {}",
            bar.height()
        );
        let top = item.min.y - bar.min.y;
        let bottom = bar.max.y - item.max.y;
        assert!(
            (top - bottom).abs() < 0.6,
            "上下留白不对称: top={top} bottom={bottom}"
        );
        assert!(
            (top - TITLEBAR_V_PAD).abs() < 0.6,
            "上下留白应为 {TITLEBAR_V_PAD}，实际 top={top}"
        );

        // 对照：若行不占满高度（旧写法），余量会全部落到内容下方。
        let (_, item0) = layout_probe(false);
        let top0 = item0.min.y - bar.min.y;
        let bottom0 = bar.max.y - item0.max.y;
        assert!(
            top0 < 1.0 && bottom0 > 20.0,
            "旧行为应是贴顶、余量堆在底部 (top={top0} bottom={bottom0})"
        );
    }

    /// 常量间的不变式：总高 = 上下留白 x2 + 内容高。
    #[test]
    fn titlebar_height_matches_padding() {
        assert_eq!(TITLEBAR_HEIGHT, TITLEBAR_CONTENT_HEIGHT + 2.0 * TITLEBAR_V_PAD);
    }
}
