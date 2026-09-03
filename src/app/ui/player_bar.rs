//! 底部播放条：控制按钮（上一首/播放暂停/下一首）+ 进度条 + 时间 + 切歌模式 +
//! 桌面歌词开关 + 封面/标题 + 加载进度 + 音量 + 错误/轻提示。

use crate::modules::audio::PlaybackStatus;
use crate::state::PlayMode;
use crate::util::fmt::{format_bytes, format_secs};
use crate::{icons, theme};
use eframe::egui::{
    self, Color32, RichText, Sense, Stroke, Vec2,
};
use super::MusicApp;
use super::widgets::{spinner_arc, transport_button, truncate_label};

/// 播放条：播放/暂停圆形按钮直径。
const PLAY_BTN_SIZE: f32 = 36.0;
/// 播放条：上一首/下一首圆形按钮直径。
const TRANSPORT_BTN_SIZE: f32 = 30.0;

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
                // 第一行：播放控制 + 进度条 + 时间
                ui.horizontal(|ui| {
                    // 桌面歌词 toggle
                    self.lyrics_capsule(ui);
                    ui.add_space(8.0);

                    // 上一首
                    if transport_button(ui, TRANSPORT_BTN_SIZE, icons::prev) {
                        self.prev_track();
                    }
                    // 播放/暂停
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
                    // 下一首
                    if transport_button(ui, TRANSPORT_BTN_SIZE, icons::next) {
                        self.next_track();
                    }

                    ui.add_space(10.0);
                    // 进度条
                    let dur = self.state.duration_secs;
                    let max = if dur > 0.0 { dur } else { 1.0 };
                    let mut val = if self.seek_dragging {
                        self.seek_preview
                    } else {
                        self.state.position_secs
                    };
                    let resp = ui.add(
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
                    ui.label(
                        RichText::new(format!(
                            "{} / {}",
                            format_secs(self.state.position_secs),
                            format_secs(self.state.duration_secs)
                        ))
                        .color(theme::TEXT_WEAK)
                        .monospace(),
                    );

                    // 切歌模式选择
                    ui.add_space(8.0);
                    ui.label(RichText::new("切歌模式").color(theme::TEXT_SECONDARY).small());
                    let mode = &mut self.settings.play_mode;
                    let mode_label = mode.label();
                    egui::ComboBox::from_id_salt("play_mode")
                        .width(110.0)
                        .selected_text(RichText::new(mode_label).color(theme::TEXT_PRIMARY))
                        .show_ui(ui, |ui| {
                            for m in PlayMode::ALL {
                                let label = m.label();
                                if ui
                                    .selectable_label(*mode == *m, RichText::new(label).color(theme::TEXT_PRIMARY))
                                    .clicked()
                                {
                                    *mode = *m;
                                }
                            }
                        });
                });

                // 第一行与第二行之间留白
                ui.add_space(10.0);
                // 第二行：封面 + 标题 + 音量
                ui.horizontal(|ui| {
                    if let Some((bvid, cover)) =
                        self.current_item().map(|i| (i.bvid.clone(), i.cover_url.clone()))
                    {
                        let cover_rect = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover()).0;
                        self.draw_cover_row(ui, cover_rect, &bvid, &cover);
                        ui.add_space(8.0);
                    }
                    if self.state.title.is_empty() {
                        ui.label(RichText::new("（未在播放）").color(theme::TEXT_WEAK));
                    } else {
                        let title = truncate_label(ui, &self.state.title, 200.0);
                        let artist = truncate_label(ui, &self.state.artist, 150.0);
                        ui.label(
                            RichText::new(title).color(theme::TEXT_PRIMARY).strong(),
                        );
                        ui.label(
                            RichText::new(format!(" — {artist}")).color(theme::TEXT_SECONDARY),
                        );
                        // 歌曲位置提示
                        if let Some(ct) = self.current_track {
                            let len = self.active_songs().len();
                            if len > 0 && self.current_item().is_some() {
                                ui.label(
                                    RichText::new(format!("　第 {}/{} 首", ct + 1, len))
                                        .color(theme::TEXT_WEAK)
                                        .small(),
                                );
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 加载进度
                        if st.loading {
                            ui.spinner();
                            if let Some(total) = st.total_bytes {
                                if total > 0 {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}/{}",
                                            format_bytes(st.downloaded_bytes),
                                            format_bytes(total)
                                        ))
                                        .color(theme::TEXT_WEAK),
                                    );
                                }
                            }
                            ui.add_space(6.0);
                        }
                        // 音量
                        ui.label(RichText::new("音量").color(theme::TEXT_SECONDARY).small());
                        let mut vol = self.state.volume;
                        if ui
                            .add(
                                egui::Slider::new(&mut vol, 0.0..=1.0)
                                    .show_value(false)
                                    .trailing_fill(true),
                            )
                            .changed()
                        {
                            self.state.volume = vol;
                            self.audio.set_volume(vol);
                            self.settings.volume = vol;
                        }
                    });
                });

                // 错误信息
                let mut err = self.ui_error.clone();
                if let Some(e) = &st.error {
                    err = Some(e.clone());
                }
                if let Some(e) = err {
                    ui.label(RichText::new(e).color(theme::TEXT_ERROR).small());
                }
                // 轻提示（金色，4 秒自动消失）
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

    // ---- 桌面歌词胶囊 toggle ----

    fn lyrics_capsule(&mut self, ui: &mut egui::Ui) {
        let on = self.settings.desktop_lyrics_enabled;
        // 用填充色 + 文字色表达状态，不加描边；悬停/按下由主题的 bg 变色反馈。
        let (fill, fg) = if on {
            (theme::ACCENT, theme::TEXT_ON_ACCENT)
        } else {
            (theme::BG_CARD, theme::TEXT_SECONDARY)
        };
        let btn = egui::Button::new(RichText::new("桌面歌词").color(fg))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(16))
            .selected(on);
        if ui.add(btn).clicked() {
            self.settings.desktop_lyrics_enabled = !self.settings.desktop_lyrics_enabled;
        }
    }
}