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
//! - 每次绘制上报当前窗口位置 → 写 `POS_SLOT`，主线程读走写进 `Settings.lyrics_pos`
//!   持久化（随设置的每 5 秒兜底 + 退出保存落盘），下次启动自动恢复到关闭前的位置。
//!
//! 约定：锁定时（鼠标穿透）永远透明；未锁定时仅鼠标悬浮才绘制背景卡片（含外圈柔光）；
//! 大号歌词文本用多次偏移重绘近似描边阴影。
//!
//! ## 歌词切换过渡动画
//!
//! 当前歌行推进到下一行时不是瞬时替换，而是旧行淡出并继续上移、新行从下方淡入升起
//! （cubic_out 缓动，约 0.4s），下一行预览同步交叉淡化。过渡状态（上一帧渲染的文本 +
//! 过渡起点时间）存在共享 `Context` 的 data 槽里——deferred 闭包是 `Fn`，不能持可变
//! 状态，但所有 viewport 共享同一个 `egui::Context`，`IdTypeMap` 即跨调用内存。
//! 动画期间闭包内 `request_repaint_after(16ms)` 只唤醒浮窗自身 viewport（约 60fps），
//! 过渡结束自动停止，空闲时浮窗仍完全静止，不影响主窗口重绘节奏。

use crate::{icons, theme};
use eframe::egui::{
    self, Align2, Color32, FontId, Id, Pos2, Rect, Sense, Vec2, ViewportBuilder, ViewportCommand,
    ViewportId,
};
use std::time::Instant;
use super::MusicApp;
use super::widgets::fit_text;

/// 桌面歌词悬浮窗固定尺寸。
const LYRICS_VIEWPORT_SIZE: Vec2 = Vec2::new(800.0, 104.0);

/// 歌词切换过渡动画时长。
const SWITCH_DURATION: f32 = 0.4;
/// 过渡期间歌词垂直滑动的距离（px）：新行从下方滑入，旧行向上滑出。
const SWITCH_SLIDE: f32 = 14.0;
/// 过渡动画的目标帧间隔（约 60fps）。
const ANIM_FRAME_INTERVAL: f32 = 1.0 / 60.0;

/// 桌面歌词 viewport 的稳定 id。
pub(crate) fn lyrics_viewport_id() -> ViewportId {
    ViewportId(Id::new("simple_music_desktop_lyrics"))
}

/// data 槽：浮窗「关闭」请求（bool）。
const CLOSE_SLOT: &str = "simple_music_lyrics_close";
/// data 槽：浮窗重绘时上报的当前窗口位置（Pos2，外框左上角屏幕坐标）。
const POS_SLOT: &str = "simple_music_lyrics_pos";
/// data 槽：歌词过渡状态（上一次绘制的当前行/下一行文本 + 过渡起点时间）。
const TRANSITION_SLOT: &str = "simple_music_lyrics_transition";

/// 歌词文本的过渡状态：记录上一次绘制的文本，文本变化时以此作淡出方，
/// 配合过渡起点时间画出「旧行淡出上移 + 新行淡入升起」。
#[derive(Clone)]
struct LineFade {
    /// 淡出方：变化前渲染的文本（过渡结束后保留，下次变化时被覆盖）。
    outgoing: String,
    /// 上一帧实际渲染的文本（变化检测基准）。
    drawn: String,
    /// 本次过渡的起点时间。
    started: Instant,
}

impl LineFade {
    /// 初始状态：视作「上一轮过渡已完成」（进度恒 1），首帧只画当前文本，
    /// 不会把空的 `outgoing` 当占位层放出来。
    fn settled(drawn: String, now: Instant) -> Self {
        Self {
            outgoing: String::new(),
            drawn,
            started: now - std::time::Duration::from_secs_f32(SWITCH_DURATION),
        }
    }

    /// 吸收本帧要渲染的文本：与上帧不同则旧文本转为淡出方并重启计时。
    fn update(&mut self, text: &str, now: Instant) {
        if self.drawn != text {
            self.outgoing = std::mem::take(&mut self.drawn);
            self.drawn = text.to_owned();
            self.started = now;
        }
    }

    /// 过渡进度 [0, 1]，1 = 完成。
    fn progress(&self, now: Instant) -> f32 {
        ((now - self.started).as_secs_f32() / SWITCH_DURATION).clamp(0.0, 1.0)
    }
}

/// 是否运行在原生 Wayland 会话（winit 在设置了 `WAYLAND_DISPLAY` 时优先选 Wayland 后端）。
/// xdg_shell 协议下客户端拿不到也设置不了窗口全局位置——位置记录/恢复都无意义，
/// 且若上报到的是 (0,0) 之类的占位值会污染跨会话（如切回 X11）的正确记录。
fn wayland_session_from(env: Option<&str>) -> bool {
    env.map(|v| !v.trim().is_empty()).unwrap_or(false)
}

fn wayland_session() -> bool {
    wayland_session_from(std::env::var("WAYLAND_DISPLAY").ok().as_deref())
}

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
        // 实时位置回传：浮窗每次重绘都上报当前位置（外框左上角），这里读走写进
        // `settings.lyrics_pos`，随设置的「每 5 秒兜底 + 退出保存」落盘，重启后恢复；
        // 本会话内关掉再开浮窗也直接回到该位置。读后即删，槽位只在浮窗真正重绘过时
        // 才有值，主线程不会拿旧值覆盖新拖动的位置。
        if let Some(p) = ctx.data(|d| d.get_temp::<Pos2>(Id::new(POS_SLOT))) {
            ctx.data_mut(|d| d.remove_temp::<Pos2>(Id::new(POS_SLOT)));
            let pos = [p.x, p.y];
            if self.settings.lyrics_pos.as_ref() != Some(&pos) {
                self.settings.lyrics_pos = Some(pos);
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
        // 恢复上次记录的位置（见模块注释：仅 X11 有意义，原生 Wayland 忽略窗口位置）。
        if !wayland_session() {
            if let Some([x, y]) = self.settings.lyrics_pos {
                builder = builder.with_position(Pos2::new(x, y));
            }
        }

        // 每帧重建闭包（捕获最新文本/字号），但只在浮窗需要重绘时才执行。
        let current = self.state.current_lrc_line.clone();
        let next = self.lyrics_next_line.clone();
        let scale = self.settings.font_scale;

        ctx.show_viewport_deferred(
            viewport_id,
            builder,
            move |ui: &mut egui::Ui, _class: egui::ViewportClass| {
                // 每次重绘都上报当前位置（外框左上角）：拖动由系统处理，移动结束至少
                // 触发一次重绘（ConfigureNotify），最终位置必被上报；主线程读走后随
                // 设置持久化，重启后恢复。Wayland 拿不到窗口全局位置，跳过上报。
                if !wayland_session() {
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

                // ── 歌词切换过渡 ──
                // 每行独立过渡：文本相对上一帧变化时，旧行转为淡出方并重启计时，
                // cubic_out 缓动驱动交叉淡化 + 上滑。过渡期间 request_repaint_after
                // 只唤醒本 viewport（约 60fps），空闲时零重绘。
                let now = Instant::now();
                let (cur_fade, next_fade) = ui.ctx().data_mut(|d| {
                    let st = d.get_temp_mut_or::<(LineFade, LineFade)>(
                        Id::new(TRANSITION_SLOT),
                        (
                            LineFade::settled(current.clone(), now),
                            LineFade::settled(next.clone(), now),
                        ),
                    );
                    st.0.update(&current, now);
                    st.1.update(&next, now);
                    (st.0.clone(), st.1.clone())
                });
                let cur_p = cur_fade.progress(now);
                let next_p = next_fade.progress(now);
                if cur_p < 1.0 || next_p < 1.0 {
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(
                        ANIM_FRAME_INTERVAL,
                    ));
                }
                let cur_ease = egui::emath::easing::cubic_out(cur_p);
                let next_ease = egui::emath::easing::cubic_out(next_p);

                // 当前行：新行从下方 SWITCH_SLIDE px 淡入升起，旧行淡出并继续上移。
                // 「等待播放」占位只随 incoming 层淡入（outgoing 为空 = 首帧没有
                // 可淡出的内容，不凭空放出占位文本）。
                if !cur_fade.outgoing.is_empty() && cur_ease < 1.0 {
                    draw_current_layer(
                        ui.painter(),
                        center,
                        &cur_fade.outgoing,
                        font.clone(),
                        1.0 - cur_ease,
                        -SWITCH_SLIDE * cur_ease,
                    );
                }
                draw_current_layer(
                    ui.painter(),
                    center,
                    current.as_str(),
                    font,
                    cur_ease,
                    SWITCH_SLIDE * (1.0 - cur_ease),
                );

                // 下一行预览：交叉淡化，旧行向上滑出、新行从下方轻微上浮，与当前行的
                // 流动方向一致（整组歌词向上推进）。
                if next_ease < 1.0 {
                    draw_next_layer(
                        ui.painter(),
                        center,
                        &next_fade.outgoing,
                        next_font.clone(),
                        1.0 - next_ease,
                        -SWITCH_SLIDE * 0.5 * next_ease,
                    );
                }
                draw_next_layer(
                    ui.painter(),
                    center,
                    next.as_str(),
                    next_font,
                    next_ease,
                    SWITCH_SLIDE * 0.5 * (1.0 - next_ease),
                );
            },
        );
    }
}

/// 绘制一行当前歌词（含描边阴影）；`alpha` 为过渡透明度，`dy` 为相对锚点的
/// 垂直偏移（负=上移）。`text` 为空表示「等待播放」占位层（18px 固定字号）。
fn draw_current_layer(
    painter: &egui::Painter,
    center: Pos2,
    text: &str,
    font: FontId,
    alpha: f32,
    dy: f32,
) {
    if alpha <= f32::EPSILON {
        return;
    }
    let anchor = center + Vec2::new(0.0, -12.0 + dy);
    if text.is_empty() {
        painter.text(
            anchor,
            Align2::CENTER_CENTER,
            "桌面歌词（等待播放…）",
            FontId::proportional(18.0),
            theme::TEXT_SECONDARY.gamma_multiply(alpha),
        );
        return;
    }
    let shadow_alpha = ((120.0 * alpha).round() as u8).max(1);
    for (dx, dy) in [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)] {
        painter.text(
            anchor + Vec2::new(dx, dy),
            Align2::CENTER_CENTER,
            text,
            font.clone(),
            Color32::from_black_alpha(shadow_alpha),
        );
    }
    painter.text(
        anchor,
        Align2::CENTER_CENTER,
        text,
        font,
        theme::LYRIC_CURRENT.gamma_multiply(alpha),
    );
}

/// 绘制下一行歌词预览（无描边）；`alpha` 为过渡透明度，`dy` 为相对锚点的
/// 垂直偏移（负=上移）。
fn draw_next_layer(painter: &egui::Painter, center: Pos2, text: &str, font: FontId, alpha: f32, dy: f32) {
    if alpha <= f32::EPSILON || text.is_empty() {
        return;
    }
    painter.text(
        center + Vec2::new(0.0, 26.0 + dy),
        Align2::CENTER_CENTER,
        text,
        font,
        theme::LYRIC_NEXT.gamma_multiply(alpha),
    );
}

#[cfg(test)]
mod tests {
    use super::{wayland_session_from, LineFade, SWITCH_DURATION};
    use std::time::{Duration, Instant};

    #[test]
    fn wayland_session_detects_env_var() {
        // 设置了 WAYLAND_DISPLAY（非空）= 原生 Wayland 会话，位置记录/恢复跳过。
        assert!(wayland_session_from(Some("wayland-0")));
        assert!(wayland_session_from(Some("wayland-1")));
        // 空串/纯空白视同未设置（winit 不会选中 Wayland 后端）。
        assert!(!wayland_session_from(Some("")));
        assert!(!wayland_session_from(Some("  ")));
        // 未设置 = X11（或无显示环境），位置功能正常。
        assert!(!wayland_session_from(None));
    }

    /// 状态机：首次吸收文本不产生过渡（无占位残留被误淡出）；
    /// 文本变化把旧文本转为淡出方并重启计时；同文本重复吸收不重启。
    #[test]
    fn line_fade_starts_and_retargets() {
        let t0 = Instant::now();
        let mut f = LineFade::settled("第一句".into(), t0);
        // 首帧同文本：无过渡。
        f.update("第一句", t0);
        assert_eq!(f.progress(t0), 1.0, "无变化不应进入过渡");
        assert!(f.outgoing.is_empty());

        // 变化：旧文本转为淡出方，进度从 0 开始。
        let t1 = t0 + Duration::from_millis(500);
        f.update("第二句", t1);
        assert_eq!(f.outgoing, "第一句");
        assert_eq!(f.drawn, "第二句");
        assert_eq!(f.progress(t1), 0.0);

        // 过渡中重复吸收同文本：不重启（进度继续推进）。
        let t2 = t1 + Duration::from_millis(100);
        f.update("第二句", t2);
        assert_eq!(f.outgoing, "第一句");
        assert!((f.progress(t2) - 0.25).abs() < 1e-4, "过渡中不应重置计时");

        // 过渡中途再变（快速歌词）：淡出方换成上次渲染的文本，计时重置。
        let t3 = t2 + Duration::from_millis(50);
        f.update("第三句", t3);
        assert_eq!(f.outgoing, "第二句");
        assert_eq!(f.progress(t3), 0.0);
    }

    /// 进度钳制：超过时长后停在 1（过渡自然结束，不再请求重绘）。
    #[test]
    fn line_fade_progress_clamps_to_one() {
        let t0 = Instant::now();
        let f = LineFade {
            outgoing: "旧".into(),
            drawn: "新".into(),
            started: t0,
        };
        assert_eq!(f.progress(t0), 0.0);
        assert_eq!(
            f.progress(t0 + Duration::from_secs_f32(SWITCH_DURATION)),
            1.0
        );
        assert_eq!(
            f.progress(t0 + Duration::from_secs_f32(SWITCH_DURATION * 3.0)),
            1.0
        );
    }

    /// 空文本（停止播放 → 等待占位）同样触发过渡：占位淡入、歌词淡出，
    /// 反向（开始播放）占位淡出、歌词淡入。
    #[test]
    fn line_fade_handles_placeholder_swaps() {
        let t0 = Instant::now();
        let mut f = LineFade::settled(String::new(), t0);
        f.update("", t0);
        assert_eq!(f.progress(t0), 1.0);

        let t1 = t0 + Duration::from_millis(100);
        f.update("歌词出现", t1);
        assert!(f.outgoing.is_empty(), "占位无文字，淡出方为空层");
        assert_eq!(f.drawn, "歌词出现");

        let t2 = t1 + Duration::from_millis(100);
        f.update("", t2);
        assert_eq!(f.outgoing, "歌词出现");
        assert!(f.drawn.is_empty());
    }
}