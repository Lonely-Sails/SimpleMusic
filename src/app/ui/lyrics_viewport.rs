//! 桌面歌词悬浮窗：独立透明置顶 viewport（**延迟模式**）。
//!
//! ## 为什么用 deferred 而不是 immediate
//!
//! `show_viewport_immediate` 要求「父子窗口任一需要重绘，双方都重绘」，主窗口播放时
//! 每帧都在重绘（进度条动画），会强制浮窗每帧一起重绘 → 双倍渲染开销，主界面变卡。
//! `show_viewport_deferred` 则让浮窗**只在自身需要重绘时**才执行 UI 闭包：
//!
//! - 歌词文本/字号/锁定状态变化 → 主线程 [`MusicApp::request_lyrics_repaint`] 按需唤醒；
//! - 浮窗收到输入事件（鼠标移动/点击/拖动）→ egui 自动重绘该 viewport；
//! - 其余时间浮窗完全静止，与主窗口互不拖累。
//!
//! ## 浮窗 ↔ 主线程通信
//!
//! deferred 闭包是 `Fn + Send + Sync + 'static`，不能借用 `&mut MusicApp`，因此
//! 浮窗内的交互结果通过共享 `egui::Context` 的 data 槽（`IdTypeMap`）回传：
//!
//! - 关闭按钮点击 → 写 `CLOSE_SLOT`，主线程下帧读取并关闭开关；
//! - 首次绘制捕获窗口位置 → 写 `POS_SLOT`，主线程读取用于持久化。
//!
//! 约定：锁定时（鼠标穿透）永远透明；未锁定时仅鼠标悬浮才绘制背景卡片（含外圈柔光）；
//! 大号歌词文本用多次偏移重绘近似描边阴影。

use crate::{icons, theme};
use eframe::egui::{
    self, Align2, Color32, FontId, Id, Pos2, Rect, Sense, Vec2, ViewportBuilder, ViewportCommand,
    ViewportId,
};
use super::MusicApp;
use super::widgets::fit_text;

/// 桌面歌词悬浮窗固定尺寸。
const LYRICS_VIEWPORT_SIZE: Vec2 = Vec2::new(800.0, 104.0);

/// 桌面歌词 viewport 的稳定 id。
pub(crate) fn lyrics_viewport_id() -> ViewportId {
    ViewportId(Id::new("simple_music_desktop_lyrics"))
}

/// data 槽：浮窗「关闭」请求（bool）。
const CLOSE_SLOT: &str = "simple_music_lyrics_close";
/// data 槽：浮窗首次绘制时的窗口位置（Pos2）。
const POS_SLOT: &str = "simple_music_lyrics_pos";

impl MusicApp {
    /// 桌面歌词浮窗内容变化时由 `logic` 调用：只唤醒浮窗 viewport 重绘，
    /// 不影响主窗口的重绘节奏。
    pub(crate) fn request_lyrics_repaint(&self, ctx: &egui::Context) {
        ctx.request_repaint_of(lyrics_viewport_id());
    }

    pub(crate) fn show_lyrics_viewport(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let viewport_id = lyrics_viewport_id();

        // ── 处理浮窗回传的交互 ──
        // 关闭按钮：deferred 闭包写入 CLOSE_SLOT，这里消费并关闭开关。
        if ctx.data(|d| d.get_temp::<bool>(Id::new(CLOSE_SLOT))).unwrap_or(false) {
            ctx.data_mut(|d| d.remove_temp::<bool>(Id::new(CLOSE_SLOT)));
            self.settings.desktop_lyrics_enabled = false;
        }
        // 首次位置捕获：deferred 闭包写入 POS_SLOT，这里读走用于持久化。
        if self.lyrics_pos.is_none() {
            if let Some(p) = ctx.data(|d| d.get_temp::<Pos2>(Id::new(POS_SLOT))) {
                self.lyrics_pos = Some(p);
            }
        }

        let locked = self.settings.lyrics_locked;
        if self.last_pass_through_applied != Some(locked) {
            ctx.send_viewport_cmd_to(viewport_id, ViewportCommand::MousePassthrough(locked));
            self.last_pass_through_applied = Some(locked);
        }

        let mut builder = ViewportBuilder::default()
            .with_title("SimpleMusic 桌面歌词")
            .with_transparent(true)
            .with_has_shadow(false)
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_mouse_passthrough(locked)
            .with_inner_size(LYRICS_VIEWPORT_SIZE);
        if let Some(p) = self.lyrics_pos {
            builder = builder.with_position(p);
        }

        // 每帧重建闭包（捕获最新文本/字号），但只在浮窗需要重绘时才执行。
        let current = self.state.current_lrc_line.clone();
        let next = self.lyrics_next_line.clone();
        let scale = self.settings.font_scale;

        ctx.show_viewport_deferred(
            viewport_id,
            builder,
            move |ui: &mut egui::Ui, _class: egui::ViewportClass| {
                // 首次绘制：上报窗口位置（供主线程持久化）。
                if ui.ctx().data(|d| d.get_temp::<Pos2>(Id::new(POS_SLOT))).is_none() {
                    if let Some(p) = ui.ctx().input(|i| i.viewport().outer_rect.map(|r| r.min)) {
                        ui.ctx().data_mut(|d| d.insert_temp(Id::new(POS_SLOT), p));
                    }
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
                        // 回传关闭请求：主线程下帧消费。
                        ui.ctx().data_mut(|d| d.insert_temp(Id::new(CLOSE_SLOT), true));
                        ui.ctx().send_viewport_cmd(ViewportCommand::Close);
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