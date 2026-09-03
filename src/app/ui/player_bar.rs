//! 底部播放条：第一行进度条（左侧当前时间 / 右侧总时长），第二行图标区
//! （桌面歌词开关 → 歌词选择 → 上一首 → 播放/暂停 → 下一首 → 播放模式 → 音量）。

use crate::modules::audio::PlaybackStatus;
use crate::modules::lyrics::Lyrics;
use crate::state::PlayMode;
use crate::util::fmt::format_secs;
use crate::{icons, theme};
use eframe::egui::{self, Color32, RichText, Sense, Vec2};

use super::MusicApp;
use super::widgets::{spinner_arc, transport_button, truncate_label};

/// 播放条：播放/暂停圆形按钮直径。
const PLAY_BTN_SIZE: f32 = 36.0;
/// 播放条：图标按钮直径。
const ICON_BTN_SIZE: f32 = 30.0;

impl MusicApp {
    pub(crate) fn show_player_bar(&mut self, ui: &mut egui::Ui, st: &PlaybackStatus) {
        egui::Panel::bottom(egui::Id::new("player_bar"))
            .frame(
                egui::Frame::new()
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(egui::Margin {
                        left: 22,
                        right: 22,
                        top: 12,
                        bottom: 14,
                    }),
            )
            .show(ui, |ui| {
                // ── 第一行：进度条（左：当前播放进度，右：歌曲总时长） ──
                let dur = self.state.duration_secs;
                let max = if dur > 0.0 { dur } else { 1.0 };
                let mut val = if self.seek_dragging {
                    self.seek_preview
                } else {
                    self.state.position_secs
                };
                let left = format_secs(if self.seek_dragging { self.seek_preview } else { self.state.position_secs });
                let right = format_secs(dur);
                let font = egui::FontId::monospace(12.0);
                let w_of = |s: &str| {
                    ui.ctx()
                        .fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE))
                        .size()
                        .x
                };
                let left_w = w_of(&left);
                let right_w = w_of(&right);
                let slider_w = (ui.available_width() - left_w - right_w - 12.0).max(40.0);

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(left)
                            .color(theme::TEXT_SECONDARY)
                            .monospace(),
                    );
                    ui.add_space(6.0);
                    let resp = ui.add_sized(
                        [slider_w, 18.0],
                        egui::Slider::new(&mut val, 0.0..=max)
                            .show_value(false)
                            .min_decimals(0)
                            .max_decimals(0)
                            .trailing_fill(true),
                    );
                    if resp.drag_started() {
                        self.seek_dragging = true;
                        self.seek_preview = self.state.position_secs;
                    }
                    if self.seek_dragging {
                        self.seek_preview = val.clamp(0.0, max);
                        if resp.drag_stopped() {
                            self.seek_dragging = false;
                            self.audio.seek(crate::app::player::clamp_seek(val, dur));
                        }
                    }
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(right)
                            .color(theme::TEXT_SECONDARY)
                            .monospace(),
                    );
                });

                ui.add_space(10.0);

                // ── 第二行：图标区（从左到右） ──
                ui.horizontal(|ui| {
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
                    ui.add_space(6.0);

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
                    ui.add_space(6.0);

                    // 3. 上一首
                    if transport_button(ui, ICON_BTN_SIZE, icons::prev) {
                        self.prev_track();
                    }

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

                    // 5. 下一首
                    if transport_button(ui, ICON_BTN_SIZE, icons::next) {
                        self.next_track();
                    }
                    ui.add_space(6.0);

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
                    ui.add_space(6.0);

                    // 7. 音量（鼠标悬浮出现音量滑条）
                    let vol = self.state.volume;
                    let vol_icon = if vol <= 0.001 {
                        icons::volume_mute
                    } else {
                        icons::volume
                    };
                    let resp = self.icon_btn(ui, ICON_BTN_SIZE, vol_icon, theme::TEXT_PRIMARY, "");
                    resp.on_hover_ui(|ui| {
                        ui.set_min_width(140.0);
                        let mut v = self.state.volume;
                        if ui
                            .add(
                                egui::Slider::new(&mut v, 0.0..=1.0)
                                    .show_value(false)
                                    .trailing_fill(true),
                            )
                            .changed()
                        {
                            self.change_volume(v);
                        }
                    });
                });

                // ── 错误信息 / 轻提示 ──
                let mut err = self.ui_error.clone();
                if let Some(e) = &st.error {
                    err = Some(e.clone());
                }
                if let Some(e) = err {
                    ui.label(RichText::new(e).color(theme::TEXT_ERROR).small());
                }
                let notice = self.last_notice.clone();
                if let Some((msg, at)) = notice {
                    if at.elapsed() < std::time::Duration::from_secs(4) {
                        ui.label(RichText::new(msg).color(theme::GOLD).small());
                    } else {
                        self.last_notice = None;
                    }
                }
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
}
