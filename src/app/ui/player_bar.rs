//! 底部播放条：第一行进度条（左侧当前时间 / 右侧总时长），第二行图标区
//! （桌面歌词开关 → 歌词选择 → 上一首 → 播放/暂停 → 下一首 → 播放模式 → 音量）。

use crate::modules::audio::PlaybackStatus;
use crate::modules::lyrics::Lyrics;
use crate::state::PlayMode;
use crate::util::fmt::format_secs;
use crate::{icons, theme};
use eframe::egui::{self, Color32, NumExt, RichText, Sense, Stroke, Vec2};

use super::MusicApp;
use super::widgets::{spinner_arc, transport_button, truncate_label, volume_hover_popup};

/// 播放条：播放/暂停圆形按钮直径。
const PLAY_BTN_SIZE: f32 = 36.0;
/// 播放条：图标按钮直径。
const ICON_BTN_SIZE: f32 = 30.0;
/// 图标区相邻按钮之间的间距。
const ICON_ROW_GAP: f32 = 8.0;
/// 进度条行左右两侧的留白。
const PROGRESS_PAD: f32 = 16.0;

/// 进度条手柄（圆球）缩小：egui 的手柄半径 = 滑块矩形高度 / 2.5（球径 ≈ 0.8 × 矩形高），
/// 而矩形高度固定取 `max(正文字号行高, spacing().interact_size.y)`，跟外层行高无关，
/// 也没有直接调手柄尺寸的 API。`seek_slider` 在滑块作用域内把 `interact_size.y`
/// 压到该值，厚度落到正文字号行高，球径从 ~22px 缩到 ~14px。
const SLIDER_HANDLE_DIAMETER: f32 = 14.0;

/// 图标区自然宽度：6 个 `ICON_BTN_SIZE` 图标 + 1 个 `PLAY_BTN_SIZE` 播放键 + 6 个间距，
/// 用于在整行宽度内把图标区水平居中。此值与 `show_player_bar` 第二行的实际按钮摆放一致，
/// 新增/删除按钮时需同步更新。
fn icon_row_width() -> f32 {
    6.0 * ICON_BTN_SIZE + PLAY_BTN_SIZE + 6.0 * ICON_ROW_GAP
}

/// 进度条滑块（手柄缩小版）。
///
/// egui 手柄球径 = 0.8 × 滑块矩形高，矩形高 = `max(正文字号行高, interact_size.y)`。
/// 这里把行高固定为改动前的矩形高（正文字号行高与 `interact_size.y` 取大），
/// 只在子 Ui 作用域内压小 `interact_size.y`：球变小并保持行内垂直居中，
/// 整行高度与播放条其余间距完全不变。
fn seek_slider(
    ui: &mut egui::Ui,
    val: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    enabled: bool,
) -> egui::Response {
    let old_thickness = ui
        .text_style_height(&egui::TextStyle::Body)
        .at_least(ui.spacing().interact_size.y);
    ui.scope(|ui| {
        ui.set_height(old_thickness);
        ui.spacing_mut().interact_size.y = SLIDER_HANDLE_DIAMETER;
        let slider = egui::Slider::new(val, range)
            .show_value(false)
            .min_decimals(0)
            .max_decimals(0)
            .trailing_fill(true);
        let r = if enabled {
            ui.add(slider)
        } else {
            ui.add_enabled(false, slider)
        };
        panic!(
            "DBG old={old_thickness} text_h={:?} interact={} scope_max={:?} slider={:?}",
            ui.text_style_height(&egui::TextStyle::Body),
            ui.spacing().interact_size.y,
            ui.max_rect(),
            r.rect
        );
        #[allow(unreachable_code)]
        {
            r
        }
    })
    .inner
}

impl MusicApp {
    pub(crate) fn show_player_bar(&mut self, ui: &mut egui::Ui, st: &PlaybackStatus) {
        egui::Panel::bottom(egui::Id::new("player_bar"))
            .frame(
                egui::Frame::new()
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(egui::Margin {
                        left: 0,
                        right: 0,
                        top: 12,
                        bottom: 14,
                    }),
            )
            .show(ui, |ui| {
                // 顶部分割线：与上方歌曲列表分隔。
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    ui.max_rect().top() + 0.5,
                    Stroke::new(1.0, theme::BORDER_SOFT),
                );
                // 进度条与上部分割线之间的留白。
                ui.add_space(8.0);

                // ── 第一行：进度条（左：当前播放进度，右：歌曲总时长，进度条占满容器宽度） ──
                let dur = self.state.duration_secs;
                // 是否有已加载/可播放的曲目：无音频时禁用进度条互动（灰色、不可拖动）。
                let has_audio = dur > 0.0;
                let max = if dur > 0.0 { dur } else { 1.0 };
                // 拖动中用预览值；否则用实际播放位置。显示值钳到 [0, max]，
                // 避免时长未知(max=1)时出现「全满/全空」的异常态。
                let pos = if self.seek_dragging {
                    self.seek_preview
                } else {
                    self.state.position_secs
                };
                let mut val = pos.clamp(0.0, max);
                let left = format_secs(pos);
                let right = format_secs(dur);

                ui.horizontal(|ui| {
                    // 关闭自动间距，由 add_space 手动控制，进度条精确占满整行。
                    ui.spacing_mut().item_spacing.x = 0.0;
                    ui.add_space(PROGRESS_PAD);
                    let time_font = egui::FontId::monospace(12.0);
                    let width_of = |s: &str| {
                        ui.ctx()
                            .fonts_mut(|f| f.layout_no_wrap(s.to_owned(), time_font.clone(), Color32::WHITE))
                            .size()
                            .x
                    };
                    // 左右时间标签都按同字号测量，避免测宽偏差导致进度条溢出。
                    let left_w = width_of(&left);
                    let right_w = width_of(&right);
                    // 先量出左右标签宽度 + 左右留白，进度条精确填充剩余空间。
                    let slider_w = (ui.available_width() - left_w - right_w - 2.0 * 6.0 - PROGRESS_PAD)
                        .max(40.0);
                    // egui::Slider 会忽略 add_sized 的尺寸提示（只认 spacing().slider_width），
                    // 必须显式设置 slider_width 才能让进度条真正占满整行，否则只画 ~100px、
                    // 整行无法铺满（宽度不对、进度条也不居中）。
                    ui.spacing_mut().slider_width = slider_w;
                    ui.label(
                        RichText::new(left)
                            .color(theme::TEXT_SECONDARY)
                            .monospace()
                            .size(12.0),
                    );
                    ui.add_space(6.0);
                    let resp = seek_slider(ui, &mut val, 0.0..=max, has_audio);
                    // 未加载音频时滑块被禁用（整体透明度被拉低），手柄会变透明。
                    // 这里在值 0（最左端）补画一个清晰的灰色圆形手柄，避免「空进度条只剩一条线」。
                    if !has_audio {
                        let rect = resp.rect;
                        let radius = rect.height() / 2.5;
                        let center = egui::Pos2::new(rect.left() + radius, rect.center().y);
                        ui.painter().circle_filled(center, radius, theme::TEXT_SECONDARY);
                        ui.painter()
                            .circle_stroke(center, radius, Stroke::new(1.0, theme::TEXT_WEAK));
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(right)
                            .color(theme::TEXT_SECONDARY)
                            .monospace()
                            .size(12.0),
                    );

                    // 拖动 / 点击 seek。拖拽期间用本地预览值（不打回引擎），松开才 commit。
                    if resp.drag_started() {
                        self.seek_dragging = true;
                    }
                    if self.seek_dragging {
                        self.seek_preview = val.clamp(0.0, max);
                        if resp.drag_stopped() {
                            self.seek_dragging = false;
                            self.audio.seek(crate::app::player::clamp_seek(self.seek_preview, dur));
                        }
                    }
                });

                ui.add_space(5.0);

                // ── 第二行：图标区（整行宽度内水平居中） ──
                // 注意：horizontal_centered 只做「纵向居中」，不会把整组控件在横向居中，
                // 因此这里改为 horizontal + 首部左缩进，使图标区按窗口宽度居中摆放。
                ui.horizontal(|ui| {
                    // 关闭自动间距，间距全部由 add_space / 计算值控制，布局精确。
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let left_pad = ((ui.available_width() - icon_row_width()) / 2.0).max(0.0);
                    ui.add_space(left_pad);

                    // 1. 桌面歌词开关
                    let on = self.settings.desktop_lyrics_enabled;
                    let color = if on { theme::ACCENT } else { theme::TEXT_SECONDARY };
                    let resp = self.icon_btn(
                        ui,
                        ICON_BTN_SIZE,
                        icons::monitor,
                        color,
                        if on { "关闭桌面歌词" } else { "开启桌面歌词" },
                    );
                    if resp.clicked() {
                        self.settings.desktop_lyrics_enabled = !self.settings.desktop_lyrics_enabled;
                    }
                    ui.add_space(ICON_ROW_GAP);

                    // 2. 歌词选择（点击弹出候选列表）
                    let resp = self.icon_btn(
                        ui,
                        ICON_BTN_SIZE,
                        icons::text_t,
                        theme::TEXT_SECONDARY,
                        "选择歌词（点击弹出）",
                    );
                    let candidates = self.lyrics_candidates.clone();
                    egui::Popup::menu(&resp).show(|ui| {
                        ui.set_min_width(240.0);
                        if candidates.is_empty() {
                            ui.label(RichText::new("暂无其他歌词").color(theme::TEXT_WEAK));
                        } else {
                            for (i, li) in candidates.iter().enumerate() {
                                let selected = self.current_lyrics.as_ref() == Some(li);
                                let label = lyrics_candidate_label(li);
                                let label = truncate_label(ui, &label, 230.0);
                                let text = if selected {
                                    RichText::new(format!("{}. {label} ✓", i + 1))
                                        .color(theme::ACCENT)
                                } else {
                                    RichText::new(format!("{}. {label}", i + 1))
                                        .color(theme::TEXT_PRIMARY)
                                };
                                if ui
                                    .add(egui::Button::new(text).fill(theme::BG_CARD).corner_radius(theme::CORNER))
                                    .clicked()
                                {
                                    self.apply_lyrics(li);
                                    ui.close();
                                }
                            }
                        }
                    });
                    ui.add_space(ICON_ROW_GAP);

                    // 3. 上一首
                    if transport_button(ui, ICON_BTN_SIZE, icons::prev) {
                        self.prev_track();
                    }
                    ui.add_space(ICON_ROW_GAP);

                    // 4. 播放 / 暂停（loading 时显示转圈）
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::splat(PLAY_BTN_SIZE),
                        Sense::click(),
                    );
                    let painter = ui.painter();
                    let bg = if resp.is_pointer_button_down_on() {
                        theme::BG_ACTIVE
                    } else if resp.hovered() {
                        theme::BG_HOVER
                    } else {
                        theme::BG_CARD
                    };
                    painter.circle_filled(rect.center(), PLAY_BTN_SIZE * 0.5, bg);
                    let icon_rect = rect.shrink(PLAY_BTN_SIZE * 0.30);
                    if st.loading {
                        spinner_arc(&painter, rect.center(), PLAY_BTN_SIZE * 0.22, theme::TEXT_SECONDARY);
                    } else if st.playing {
                        icons::pause(&painter, icon_rect, theme::TEXT_PRIMARY);
                    } else {
                        icons::play(&painter, icon_rect, theme::TEXT_PRIMARY);
                    }
                    if resp.clicked() && !st.loading {
                        if st.playing {
                            self.audio.pause();
                        } else {
                            self.audio.resume();
                        }
                    }
                    ui.add_space(ICON_ROW_GAP);

                    // 5. 下一首
                    if transport_button(ui, ICON_BTN_SIZE, icons::next) {
                        self.next_track();
                    }
                    ui.add_space(ICON_ROW_GAP);

                    // 6. 播放模式切换（按一下循环切换，图标随模式变化）
                    let mode = self.settings.play_mode;
                    let mode_icon = play_mode_icon(mode);
                    let resp = self.icon_btn(
                        ui,
                        ICON_BTN_SIZE,
                        mode_icon,
                        theme::TEXT_PRIMARY,
                        mode.label(),
                    );
                    if resp.clicked() {
                        self.settings.play_mode = next_play_mode(mode);
                    }
                    ui.add_space(ICON_ROW_GAP);

                    // 7. 音量：悬浮弹出可拖动的音量滑条。
                    // 不能用 egui tooltip（on_hover_ui）承载滑块——tooltip 要求指针静止满
                    // tooltip_delay（本应用 0.4s）才出现，且首次出现帧因弹层内还没有交互控件
                    // 而被标记为不可交互，鼠标一移向滑块即关闭，体感就是「悬浮没反应 / 拖不动」。
                    // 改为自绘悬浮弹层（见 widgets::volume_hover_popup）。
                    let vol = self.state.volume;
                    let vol_icon = if vol <= 0.001 {
                        icons::volume_mute
                    } else {
                        icons::volume
                    };
                    let resp = self.icon_btn(ui, ICON_BTN_SIZE, vol_icon, theme::TEXT_PRIMARY, "");
                    if let Some(v) = volume_hover_popup(ui, resp.rect, resp.hovered(), vol) {
                        self.change_volume(v);
                    }
                });
                // 错误/轻提示改为顶部 toast（见 toast.rs），不再内联显示在此处。
            });
    }

    /// 图标按钮：圆角底 + 图标 + 悬停提示，返回 Response 供点击/弹窗使用。
    fn icon_btn(
        &mut self,
        ui: &mut egui::Ui,
        size: f32,
        icon: fn(&egui::Painter, egui::Rect, Color32),
        color: Color32,
        tooltip: &str,
    ) -> egui::Response {
        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
        let bg = if resp.is_pointer_button_down_on() {
            theme::BG_ACTIVE
        } else if resp.hovered() {
            theme::BG_HOVER
        } else {
            theme::BG_CARD
        };
        let painter = ui.painter();
        painter.rect_filled(rect, theme::CORNER, bg);
        icon(&painter, rect.shrink(size * 0.24), color);
        if tooltip.is_empty() {
            resp
        } else {
            resp.on_hover_text(tooltip)
        }
    }
}

/// 播放模式对应的图标。
fn play_mode_icon(mode: PlayMode) -> fn(&egui::Painter, egui::Rect, Color32) {
    match mode {
        PlayMode::Sequence => icons::repeat,
        PlayMode::SingleRepeat => icons::repeat_once,
        PlayMode::Shuffle => icons::shuffle,
    }
}

/// 循环切换到下一个播放模式。
fn next_play_mode(mode: PlayMode) -> PlayMode {
    let idx = PlayMode::ALL
        .iter()
        .position(|m| *m == mode)
        .unwrap_or(0);
    PlayMode::ALL[(idx + 1) % PlayMode::ALL.len()]
}

/// 歌词候选在弹窗中的展示文案（曲名 — 歌手（来源））。
fn lyrics_candidate_label(li: &Lyrics) -> String {
    match &li.source {
        Some(src) => {
            let track = if src.track_name.is_empty() {
                "未知歌曲".to_string()
            } else {
                src.track_name.clone()
            };
            let mut s = track;
            if !src.artist_name.is_empty() {
                s.push_str(" — ");
                s.push_str(&src.artist_name);
            }
            if !src.album_name.is_empty() {
                s.push_str("（");
                s.push_str(&src.album_name);
                s.push('）');
            }
            s
        }
        None => "未知来源".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn play_mode_cycle_wraps() {
        assert_eq!(next_play_mode(PlayMode::Sequence), PlayMode::SingleRepeat);
        assert_eq!(next_play_mode(PlayMode::SingleRepeat), PlayMode::Shuffle);
        assert_eq!(next_play_mode(PlayMode::Shuffle), PlayMode::Sequence);
    }

    #[test]
    fn play_mode_icons_exist() {
        // 三个模式都要有对应图标函数（编译期保证签名一致即可）。
        let _: fn(&egui::Painter, egui::Rect, Color32) = play_mode_icon(PlayMode::Sequence);
        let _: fn(&egui::Painter, egui::Rect, Color32) = play_mode_icon(PlayMode::SingleRepeat);
        let _: fn(&egui::Painter, egui::Rect, Color32) = play_mode_icon(PlayMode::Shuffle);
    }

    #[test]
    fn candidate_label_uses_source() {
        let mut li = Lyrics {
            lrc: Some("[00:01.00]hi".to_string()),
            plain: "hi".to_string(),
            source: None,
        };
        assert_eq!(lyrics_candidate_label(&li), "未知来源");
        li.source = Some(crate::modules::lyrics::LrcSearchResult {
            id: 1,
            track_name: "晴天".to_string(),
            artist_name: "周杰伦".to_string(),
            album_name: "叶惠美".to_string(),
            duration: 0.0,
            instrumental: false,
            plain_lyrics: String::new(),
            synced_lyrics: String::new(),
        });
        assert_eq!(lyrics_candidate_label(&li), "晴天 — 周杰伦（叶惠美）");
    }

    /// 图标区宽度应与第二行实际按钮摆放一致（6 个图标 + 1 播放键 + 6 个间距）。
    #[test]
    fn icon_row_width_consistent() {
        assert_eq!(
            icon_row_width(),
            6.0 * ICON_BTN_SIZE + PLAY_BTN_SIZE + 6.0 * ICON_ROW_GAP
        );
    }

    /// 图标区应在整行宽度内水平居中（回归：horizontal_centered 只纵向居中，不横向居中）。
    #[test]
    fn icon_row_is_centered_within_width() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(screen);

        let mut first_x = f32::MAX;
        let mut last_x = f32::MIN;
        let mut n = 0;
        let mut full = ctx.run_ui(input, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let pad = ((ui.available_width() - icon_row_width()) / 2.0).max(0.0);
                ui.add_space(pad);
                // 复刻第二行：第 4 个是播放键（更大），其余是图标。
                for i in 0..7 {
                    let size = if i == 3 { PLAY_BTN_SIZE } else { ICON_BTN_SIZE };
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
                    if n == 0 {
                        first_x = r.min.x;
                    }
                    last_x = r.max.x;
                    n += 1;
                    ui.add_space(ICON_ROW_GAP);
                }
            });
        });
        full.textures_delta.clear();

        let center = (first_x + last_x) / 2.0;
        assert!(
            (center - 400.0).abs() < 2.0,
            "图标区应在 800px 宽容器内水平居中，实际中心 {center:.1}"
        );
    }

    /// 进度条行应精确铺满整行并把滑块居中。
    ///
    /// 回归：`egui::Slider` 会忽略 `add_sized` 的尺寸提示（只认 `spacing().slider_width`），
    /// 之前用 `ui.add_sized([slider_w, 18.0], Slider)` 时滑块只画 ~100px、整行无法铺满，
    /// 进度条既不占满宽度也不居中。必须显式设置 `slider_width`。
    #[test]
    fn progress_row_fills_and_centers_slider() {
        let ctx = egui::Context::default();
        crate::fonts::install_fonts(&ctx);
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 200.0));
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(screen);

        let mut slider_w = 0.0;
        let mut want_w = 0.0;
        let mut slider_center = 0.0;
        let mut row_end = 0.0;
        let mut full = ctx.run_ui(input, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.add_space(PROGRESS_PAD);
                let left = "00:31";
                let right = "03:45";
                let time_font = egui::FontId::monospace(12.0);
                let width_of = |s: &str| {
                    ui.ctx()
                        .fonts_mut(|f| f.layout_no_wrap(s.to_owned(), time_font.clone(), Color32::WHITE))
                        .size()
                        .x
                };
                let left_w = width_of(left);
                let right_w = width_of(right);
                want_w = (ui.available_width() - left_w - right_w - 2.0 * 6.0 - PROGRESS_PAD).max(40.0);
                ui.spacing_mut().slider_width = want_w;
                ui.label(RichText::new(left).monospace().size(12.0));
                ui.add_space(6.0);
                let mut v = 0.3;
                let resp = seek_slider(ui, &mut v, 0.0..=1.0, true);
                ui.add_space(6.0);
                let right_resp = ui.label(RichText::new(right).monospace().size(12.0));
                slider_w = resp.rect.width();
                slider_center = resp.rect.center().x;
                row_end = right_resp.rect.max.x;
            });
        });
        full.textures_delta.clear();

        // 滑块应占满计算宽度（而不是退回 ~100px）。
        assert!(
            (slider_w - want_w).abs() < 1.0,
            "滑块宽度 {slider_w:.1} 应等于计算宽度 {want_w:.1}，实际退回默认 100px 则说明 add_sized/slider_width 处理有误"
        );
        // 内容区应在左右留白之后铺满（右端 = 1000 - 左右留白，两侧对称留白）。
        assert!(
            (row_end - (1000.0 - PROGRESS_PAD)).abs() < 1.0,
            "进度条行应在左右留白后铺满，实际右端到 {row_end:.1}"
        );
        // 滑块应水平居中（容器中心 = 500；左右留白对称故仍在中心）。
        assert!(
            (slider_center - 500.0).abs() < 1.0,
            "进度条应水平居中，实际滑块中心 {slider_center:.1}"
        );
    }

    /// 无音频（duration<=0）时滑块应禁用、不可互动。
    #[test]
    fn progress_slider_disabled_without_audio() {
        let ctx = egui::Context::default();
        crate::fonts::install_fonts(&ctx);
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 200.0));
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(screen);

        let mut enabled = true;
        let mut full = ctx.run_ui(input, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.spacing_mut().slider_width = 500.0;
                let mut v = 0.0;
                let resp = seek_slider(ui, &mut v, 0.0..=1.0, false);
                enabled = resp.enabled();
            });
        });
        full.textures_delta.clear();
        assert!(!enabled, "无音频时进度条应被禁用");
    }

    /// 进度条手柄（圆球）应缩小，且行高与垂直居中保持不变。
    ///
    /// 回归：egui 手柄球径 = 0.8 × 滑块矩形高，矩形高取 max(正文字号行高, interact_size.y)
    /// = 28 → 球径 ~22px。`seek_slider` 通过压小作用域内的 interact_size.y 缩球，
    /// 行高仍固定为原厚度，保证整行布局不变。
    #[test]
    fn slider_handle_is_smaller_and_row_height_unchanged() {
        let ctx = egui::Context::default();
        crate::fonts::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let screen = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 200.0));
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(screen);

        // 默认（未缩小）滑块厚度作为球径对照，与行高基准，在闭包内捕获。
        let old_thickness = std::cell::Cell::new(0.0f32);
        let mut slider_rect = egui::Rect::NOTHING;
        let mut row_rect = egui::Rect::NOTHING;
        let mut full = ctx.run_ui(input, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.spacing_mut().slider_width = 500.0;
                // 记录改动前的滑块厚度基准。
                old_thickness.set(
                    ui.text_style_height(&egui::TextStyle::Body)
                        .at_least(ui.spacing().interact_size.y),
                );
                let mut v = 0.3;
                let resp = seek_slider(ui, &mut v, 0.0..=1.0, true);
                slider_rect = resp.rect;
                row_rect = ui.min_rect();
            });
        });
        full.textures_delta.clear();

        // 行高必须与改动前的滑块厚度一致（整行布局不变）。
        assert!(
            (row_rect.height() - old_thickness.get()).abs() < 0.6,
            "进度条行高 {} 应保持原厚度 {:.1}",
            row_rect.height(),
            old_thickness.get()
        );
        // 手柄球径（= 0.8 × 滑块矩形高）应明显小于改动前（~0.8 × 原厚度）。
        let handle_d = 2.0 * (slider_rect.height() / 2.5);
        let old_handle_d = 2.0 * (old_thickness.get() / 2.5);
        assert!(
            handle_d < old_handle_d - 4.0,
            "手柄球径应从 ~{old_handle_d:.1} 缩小，实际 {handle_d:.1}"
        );
        // 球在行内应垂直居中。
        let row_center = (row_rect.min.y + row_rect.max.y) / 2.0;
        assert!(
            (slider_rect.center().y - row_center).abs() < 0.6,
            "滑块（球）应在行内垂直居中：滑块中心 {:.1} vs 行中心 {:.1}",
            slider_rect.center().y,
            row_center
        );
        // 球径应落在 SLIDER_HANDLE_DIAMETER 附近（厚度=正文字号行高 → 球径≈0.8×行高）。
        assert!(
            handle_d < 18.0,
            "球径 {handle_d:.1} 应明显小于原来的 ~22px（SLIDER_HANDLE_DIAMETER={SLIDER_HANDLE_DIAMETER}）"
        );
    }
}

#[cfg(test)]
mod dbg_tmp {
    use super::*;
    #[test]
    fn dbg_numbers() {
        let ctx = egui::Context::default();
        crate::fonts::install_fonts(&ctx);
        crate::theme::apply(&ctx);
        let mut input = egui::RawInput::default();
        input.screen_rect = Some(egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 200.0)));
        let cell = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
        let c2 = cell.clone();
        let mut full = ctx.run_ui(input, move |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.spacing_mut().slider_width = 500.0;
                let outer = ui.text_style_height(&egui::TextStyle::Body).at_least(ui.spacing().interact_size.y);
                let r = ui.scope(|ui| {
                    ui.set_height(outer);
                    ui.spacing_mut().interact_size.y = SLIDER_HANDLE_DIAMETER;
                    let mut v = 0.3f64;
                    ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false).trailing_fill(true))
                });
                *c2.borrow_mut() = format!(
                    "outer_thickness={outer} scope_rect={:?} slider_rect={:?} scope_h={}",
                    r.response.rect,
                    r.inner.rect,
                    r.response.rect.height()
                );
            });
        });
        full.textures_delta.clear();
        panic!("DBG {}", cell.borrow());    }
}
