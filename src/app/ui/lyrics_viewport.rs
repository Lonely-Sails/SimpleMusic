//! 桌面歌词悬浮窗：独立透明置顶 viewport。
//!
//! - 默认全透明；仅「未锁定 + 鼠标悬浮」时绘制背景卡片（含外圈柔光）。
//! - 锁定（鼠标穿透）时永远透明，通过 `ViewportCommand::MousePassthrough` 运行期切换。
//! - 大号歌词文本用多次偏移重绘近似描边阴影。

use crate::{icons, theme};
use eframe::egui::{
    self, Align2, Color32, FontId, Rect, Sense, Vec2, ViewportBuilder, ViewportCommand,
    ViewportId,
};
use super::MusicApp;
use super::widgets::fit_text;

/// 桌面歌词悬浮窗固定尺寸。
const LYRICS_VIEWPORT_SIZE: Vec2 = Vec2::new(800.0, 104.0);

/// 桌面歌词 viewport 的稳定 id。
fn lyrics_viewport_id() -> ViewportId {
    ViewportId(egui::Id::new("simple_music_desktop_lyrics"))
}

impl MusicApp {
    pub(crate) fn show_lyrics_viewport(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        let locked = self.settings.lyrics_locked;
        if self.last_pass_through_applied != Some(locked) {
            ctx.send_viewport_cmd_to(
                lyrics_viewport_id(),
                ViewportCommand::MousePassthrough(locked),
            );
            self.last_pass_through_applied = Some(locked);
        }

        let pos = self.lyrics_pos;
        let mut builder = ViewportBuilder::default()
            .with_title("SimpleMusic 桌面歌词")
            .with_transparent(true)
            .with_has_shadow(false)
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_mouse_passthrough(locked)
            .with_inner_size(LYRICS_VIEWPORT_SIZE);
        if let Some(p) = pos {
            builder = builder.with_position(p);
        }

        let current = self.state.current_lrc_line.clone();
        let next = self.lyrics_next_line.clone();
        let scale = self.settings.font_scale;
        let viewport_id = lyrics_viewport_id();

        ctx.show_viewport_immediate(
            viewport_id,
            builder,
            |ui: &mut egui::Ui, _class: egui::ViewportClass| {
                if self.lyrics_pos.is_none() {
                    self.lyrics_pos = ui
                        .ctx()
                        .input(|i| i.viewport().outer_rect.map(|r| r.min));
                }

                let (rect, response) =
                    ui.allocate_exact_size(ui.available_size(), Sense::drag());

                // 默认全透明：只有「解锁 + 鼠标悬浮」时才绘制背景卡片（含外圈柔光），
                // 让歌词无边框地浮在桌面上；锁定（鼠标穿透）时不会触发 hover，永远透明。
                // 悬停提示仅用背景亮度变化，不加描边。
                let show_bg = response.hovered() && !locked;
                if show_bg {
                    for (expand, alpha) in [(6.0, 26), (3.0, 40)] {
                        ui.painter().rect_filled(
                            rect.expand(expand),
                            theme::CORNER,
                            Color32::from_black_alpha(alpha),
                        );
                    }
                    ui.painter().rect_filled(rect, theme::CORNER, theme::LYRIC_BG);
                }

                if !locked && response.drag_started() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                }

                if !locked && response.hovered() {
                    let btn_rect = Rect::from_min_size(
                        rect.right_top() - Vec2::new(28.0, 4.0),
                        Vec2::new(24.0, 24.0),
                    );
                    let btn = ui.allocate_rect(btn_rect, Sense::click());
                    let btn_hovered = btn.hovered();
                    ui.painter()
                        .circle_filled(btn_rect.center(), 11.0, theme::BG_ACTIVE);
                    icons::cross(
                        &ui.painter(),
                        btn_rect.shrink(5.0),
                        if btn_hovered {
                            theme::TEXT_PRIMARY
                        } else {
                            theme::TEXT_SECONDARY
                        },
                    );
                    if btn.clicked() {
                        self.settings.desktop_lyrics_enabled = false;
                    }
                }

                let font = FontId::proportional(26.0 * scale);
                let next_font = FontId::proportional(14.0 * scale);
                let max_w = rect.width() - 24.0;
                let current = fit_text(ui.ctx(), &current, &font, max_w);
                let next = fit_text(ui.ctx(), &next, &next_font, max_w);
                let center = rect.center();
                if !current.is_empty() {
                    let cur_center = center + Vec2::new(0.0, -12.0);
                    for (dx, dy) in [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)] {
                        ui.painter().text(
                            cur_center + Vec2::new(dx, dy),
                            Align2::CENTER_CENTER,
                            current.as_str(),
                            font.clone(),
                            Color32::from_black_alpha(120),
                        );
                    }
                    ui.painter().text(
                        cur_center,
                        Align2::CENTER_CENTER,
                        current.as_str(),
                        font,
                        theme::LYRIC_CURRENT,
                    );
                } else {
                    ui.painter().text(
                        center,
                        Align2::CENTER_CENTER,
                        "桌面歌词（等待播放…）",
                        FontId::proportional(18.0),
                        theme::TEXT_SECONDARY,
                    );
                }
                if !next.is_empty() {
                    let next_center = center + Vec2::new(0.0, 26.0);
                    ui.painter().text(
                        next_center,
                        Align2::CENTER_CENTER,
                        next.as_str(),
                        next_font,
                        theme::LYRIC_NEXT,
                    );
                }
            },
        );
    }
}