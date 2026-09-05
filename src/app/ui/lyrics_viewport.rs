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
//! 大号歌词文本用单向柔影（垂直方向逐层衰减）保证任意桌面背景上的可读性——刻意
//! 避免四周描边式的硬黑边。
//!
//! ## 歌词切换过渡动画
//!
//! 当前歌行推进到下一行时不是瞬时替换，而是旧行淡出并继续上移、新行从下方淡入升起
//! （quadratic_out 缓动，约 0.45s，滑动距离刻意收窄到 14px），下一行预览同步交叉淡化。
//! 帧率抖动时观感依然平滑：位移小 + 缓动前段平缓，掉一帧只挪 0.5px。过渡状态（上一帧
//! 渲染的文本 + 过渡起点时间）存在共享 `Context` 的 data 槽里——deferred 闭包是 `Fn`，
//! 不能持可变状态，但所有 viewport 共享同一个 `egui::Context`，`IdTypeMap` 即跨调用内存。
//! 动画期间闭包内 `request_repaint()` 连续唤醒浮窗自身 viewport，呈现节奏交给
//! vsync/合成器对齐（egui 内建动画同款）——固定 1/60s 定时器会与 vblank 相位漂移，
//! 出现 16.7/33.4ms 交替帧距，观感一顿一顿；过渡结束自动停止，空闲时浮窗仍完全静止。
//!
//! ## 真·模糊柔影（text-shadow 观感）
//!
//! 当前句/下一句下方垫一张**高斯模糊后的字形位图**（见 [`crate::text_shadow`]）：
//! skrifa 取字形轮廓 → vello_cpu 离屏光栅化 → 盒滤波近似高斯 → egui 纹理，光晕向
//! 四周均匀晕开，等价 CSS `text-shadow: 0 2px 12px rgba(0,0,0,.55)`。纹理按文本缓存
//! （data 槽 [`SHADOW_SLOT`]），过渡动画期间逐帧复用，不重复光栅化。刻意避免多层
//! 文字副本叠加（放大是同心硬边，晕不开）与四周描边式硬黑边。
//!
//! 每帧的文本布局只做一次：`layout_no_wrap` 用 `Color32::PLACEHOLDER` 拿到不带
//! 真实颜色的 galley，主体层复用同一 galley 绘制——动画期间每帧最多 2 次布局查询
//! （当前行 + 下一行）。

use crate::text_shadow::{CachedShadow, ShadowCache, ShadowStyle, rasterize_shadow};
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
const SWITCH_DURATION: f32 = 0.45;
/// 过渡期间歌词垂直滑动的距离（px）：新行从下方滑入，旧行向上滑出。
/// 刻意收窄——位移小则掉帧不可见（30fps 下每帧仅约 0.7px）。
const SWITCH_SLIDE: f32 = 14.0;

/// 柔影参数（当前句）：σ 与垂直下坠随字号缩放，等价 CSS `0 2px 12px rgba(0,0,0,.55)`。
const SHADOW_SIGMA_SCALE: f32 = 0.22; // σ ≈ 0.22 × 字号（26px 字 → σ ≈ 5.7px）
const SHADOW_STRENGTH: f32 = 0.55;
const SHADOW_DY: f32 = 1.5;
/// 柔影参数（下一句预览）：字号小、信息弱，光晕相应更轻更聚。
const NEXT_SHADOW_STRENGTH: f32 = 0.4;
const NEXT_SHADOW_DY: f32 = 1.0;

/// data 槽：柔影纹理缓存（当前句/下一句/过渡中的旧行共用，按文本键控）。
const SHADOW_SLOT: &str = "simple_music_lyrics_shadow";

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
                // 悬停提示仅用背景亮度变化 + 大扩散低透明度的柔光，不加描边。
                let show_bg = response.hovered() && !locked;
                if show_bg {
                    for (expand, alpha) in [(10.0, 14), (4.0, 30)] {
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
                let font_pt = 26.0 * scale;
                let next_font_pt = 14.0 * scale;

                // ── 歌词切换过渡 ──
                // 每行独立过渡：文本相对上一帧变化时，旧行转为淡出方并重启计时，
                // quadratic_out 缓动驱动交叉淡化 + 上滑。过渡期间 request_repaint()
                // 连续唤醒本 viewport（呈现节奏由 vsync/合成器对齐），空闲时零重绘。
                // 这里只取绘制所需的 outgoing 文本与进度，不整状态克隆（免每帧两次
                // String 拷贝）。
                let now = Instant::now();
                let (cur_outgoing, cur_p, next_outgoing, next_p) = ui.ctx().data_mut(|d| {
                    let st = d.get_temp_mut_or::<(LineFade, LineFade)>(
                        Id::new(TRANSITION_SLOT),
                        (
                            LineFade::settled(current.clone(), now),
                            LineFade::settled(next.clone(), now),
                        ),
                    );
                    st.0.update(&current, now);
                    st.1.update(&next, now);
                    (
                        st.0.outgoing.clone(),
                        st.0.progress(now),
                        st.1.outgoing.clone(),
                        st.1.progress(now),
                    )
                });
                if cur_p < 1.0 || next_p < 1.0 {
                    // 连续重绘：eframe/winit 以 request_redraw 驱动，swap 的 vsync
                    // 节流把帧距钉在刷新周期上；固定 16ms 定时器则与 vblank 相位
                    // 漂移，帧距 16.7/33.4ms 交替 → 观感卡顿。
                    ui.ctx().request_repaint();
                }
                let cur_ease = egui::emath::easing::quadratic_out(cur_p);
                let next_ease = egui::emath::easing::quadratic_out(next_p);

                // 当前行：新行从下方 SWITCH_SLIDE px 淡入升起，旧行淡出并继续上移。
                // 「等待播放」占位只随 incoming 层淡入（outgoing 为空 = 首帧没有
                // 可淡出的内容，不凭空放出占位文本）。
                if !cur_outgoing.is_empty() && cur_ease < 1.0 {
                    draw_current_layer(
                        ui,
                        center,
                        &cur_outgoing,
                        font_pt,
                        1.0 - cur_ease,
                        -SWITCH_SLIDE * cur_ease,
                    );
                }
                draw_current_layer(
                    ui,
                    center,
                    current.as_str(),
                    font_pt,
                    cur_ease,
                    SWITCH_SLIDE * (1.0 - cur_ease),
                );

                // 下一行预览：交叉淡化，旧行向上滑出、新行从下方轻微上浮，与当前行的
                // 流动方向一致（整组歌词向上推进）。
                if next_ease < 1.0 {
                    draw_next_layer(
                        ui,
                        center,
                        &next_outgoing,
                        next_font_pt,
                        1.0 - next_ease,
                        -SWITCH_SLIDE * 0.5 * next_ease,
                    );
                }
                draw_next_layer(
                    ui,
                    center,
                    next.as_str(),
                    next_font_pt,
                    next_ease,
                    SWITCH_SLIDE * 0.5 * (1.0 - next_ease),
                );
            },
        );
    }
}

/// 取（或生成）一行文本的柔影纹理；`None` = 字形为空（纯空白）或光栅化失败。
///
/// **锁纪律**：缓存查/写是两个独立 `data_mut` 短临界区，光栅化 + `load_texture`
/// 在锁外执行——`load_texture` 内部会再取同一把 `ContextImpl` 写锁，嵌在
/// `data_mut` 闭包里就是同线程递归加写锁 = 死锁（epaint RwLock 不可重入）。
fn lyrics_shadow_texture(
    ctx: &egui::Context,
    text: &str,
    font_pt: f32,
    sigma: f32,
    strength: f32,
) -> Option<egui::TextureHandle> {
    let ppi = ctx.pixels_per_point();
    let font_px = font_pt * ppi;
    let style = ShadowStyle {
        sigma: sigma * ppi,
        strength,
    };
    // 1) 查缓存（短临界区）。
    let cached = ctx.data_mut(|d| {
        d.get_temp_mut_or::<ShadowCache>(Id::new(SHADOW_SLOT), ShadowCache::default())
            .get(text, font_px, style)
    });
    // 2) 未命中 → 锁外光栅化 + 上传。
    let (key, tex) = match cached {
        CachedShadow::Ready(tex) => return Some(tex),
        CachedShadow::Failed => return None,
        CachedShadow::Miss(key) => {
            let font = crate::fonts::active_text_font();
            let tex = rasterize_shadow(ctx, &font, 0, text, font_px, style);
            (key, tex)
        }
    };
    // 3) 写缓存（短临界区，失败也缓存，避免每帧重试）。
    ctx.data_mut(|d| {
        d.get_temp_mut_or::<ShadowCache>(Id::new(SHADOW_SLOT), ShadowCache::default())
            .insert(key, tex.clone());
    });
    tex
}

/// 绘制当前句歌词（含向四周晕开的模糊柔影）；`alpha` 为过渡透明度，`dy` 为相对
/// 锚点的垂直偏移（负=上移）。`text` 为空表示「等待播放」占位层（18px 固定字号）。
fn draw_current_layer(ui: &egui::Ui, center: Pos2, text: &str, font_pt: f32, alpha: f32, dy: f32) {
    let painter = ui.painter();
    if alpha <= f32::EPSILON {
        return;
    }
    if text.is_empty() {
        painter.text(
            center + Vec2::new(0.0, -12.0 + dy),
            Align2::CENTER_CENTER,
            "桌面歌词（等待播放…）",
            FontId::proportional(18.0),
            theme::TEXT_SECONDARY.gamma_multiply(alpha),
        );
        return;
    }
    let font = FontId::proportional(font_pt);
    let ctx = ui.ctx().clone();
    // PLACEHOLDER：布局时不带真实颜色，tessellator 会用 `Painter::galley` 的
    // fallback_color 替换——柔影贴图与主体文字以同一锚点对齐。
    let galley = painter.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    // galley 以左上角定位，此处换算回 CENTER_CENTER 锚点语义。
    let anchor = center + Vec2::new(0.0, -12.0 + dy) - galley.size() / 2.0;
    // 柔影：贴图墨迹中心 = 文字墨迹中心（galley.mesh_bounds 是墨迹紧致盒）+ 轻微
    // 下坠，光晕向四周均匀晕开。
    let shadow_alpha = SHADOW_STRENGTH * alpha;
    if shadow_alpha > f32::EPSILON {
        let sigma = font_pt * SHADOW_SIGMA_SCALE;
        if let Some(tex) = lyrics_shadow_texture(&ctx, text, font_pt, sigma, SHADOW_STRENGTH) {
            let ppi = ctx.pixels_per_point();
            let ink_center = anchor + galley.mesh_bounds.center().to_vec2();
            let size_pt = tex.size_vec2() / ppi;
            let rect = Rect::from_center_size(ink_center + Vec2::new(0.0, SHADOW_DY), size_pt);
            painter.image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::from_black_alpha((shadow_alpha * 255.0).round().clamp(1.0, 255.0) as u8),
            );
        }
    }
    painter.galley(anchor, galley, theme::LYRIC_CURRENT.gamma_multiply(alpha));
}

/// 绘制下一行歌词预览（含更轻的柔影）；`alpha` 为过渡透明度，`dy` 为相对锚点的
/// 垂直偏移（负=上移）。
fn draw_next_layer(ui: &egui::Ui, center: Pos2, text: &str, font_pt: f32, alpha: f32, dy: f32) {
    if alpha <= f32::EPSILON || text.is_empty() {
        return;
    }
    let painter = ui.painter();
    let ctx = ui.ctx().clone();
    let font = FontId::proportional(font_pt);
    // 与 draw_current_layer 同款 galley + mesh_bounds 墨迹对齐（CENTER_CENTER 锚的
    // 语义锚点是布局矩形中心，含行高上下空白；墨迹中心才是阴影该贴的位置）。
    let galley = painter.layout_no_wrap(text.to_owned(), font, Color32::PLACEHOLDER);
    let anchor = center + Vec2::new(0.0, 26.0 + dy) - galley.size() / 2.0;
    let shadow_alpha = NEXT_SHADOW_STRENGTH * alpha;
    if shadow_alpha > f32::EPSILON {
        let sigma = font_pt * SHADOW_SIGMA_SCALE;
        if let Some(tex) = lyrics_shadow_texture(&ctx, text, font_pt, sigma, NEXT_SHADOW_STRENGTH) {
            let ppi = ctx.pixels_per_point();
            let ink_center = anchor + galley.mesh_bounds.center().to_vec2();
            let size_pt = tex.size_vec2() / ppi;
            let rect = Rect::from_center_size(ink_center + Vec2::new(0.0, NEXT_SHADOW_DY), size_pt);
            painter.image(
                tex.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::from_black_alpha((shadow_alpha * 255.0).round().clamp(1.0, 255.0) as u8),
            );
        }
    }
    painter.galley(
        anchor,
        galley,
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
        let expected = 0.1 / SWITCH_DURATION;
        assert!(
            (f.progress(t2) - expected).abs() < 1e-4,
            "过渡中不应重置计时"
        );

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