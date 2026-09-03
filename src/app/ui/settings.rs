//! 设置窗口：音质偏好 / 桌面歌词 / 播放音量。

use crate::state::AudioQuality;
use crate::theme;
use eframe::egui::{self, Align2, RichText};
use super::MusicApp;

impl MusicApp {
    pub(crate) fn show_settings_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("设置")
            .id(egui::Id::new("settings_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut self.settings_window_open)
            .show(ctx, |ui| {
                ui.heading(RichText::new("设置").color(theme::TEXT_PRIMARY).strong());
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                // ── 音质 ──
                ui.label(
                    RichText::new("音质偏好")
                        .color(theme::TEXT_SECONDARY)
                        .strong(),
                );
                for q in AudioQuality::ALL {
                    let label = q.label();
                    if ui
                        .radio(
                            self.settings.audio_quality == *q,
                            RichText::new(label).color(theme::TEXT_PRIMARY),
                        )
                        .clicked()
                    {
                        self.settings.audio_quality = *q;
                    }
                }
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                // ── 桌面歌词 ──
                ui.label(
                    RichText::new("桌面歌词")
                        .color(theme::TEXT_SECONDARY)
                        .strong(),
                );
                ui.checkbox(
                    &mut self.settings.desktop_lyrics_enabled,
                    "启用桌面歌词",
                );
                ui.checkbox(
                    &mut self.settings.lyrics_locked,
                    "歌词锁定（鼠标穿透）",
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new("歌词字号").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::Slider::new(&mut self.settings.font_scale, 0.5..=2.0)
                            .text("倍")
                            .show_value(true)
                            .trailing_fill(true),
                    );
                });
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

                // ── 播放 ──
                ui.label(
                    RichText::new("播放")
                        .color(theme::TEXT_SECONDARY)
                        .strong(),
                );
                ui.horizontal(|ui| {
                    ui.label(RichText::new("音量").color(theme::TEXT_SECONDARY));
                    ui.add(
                        egui::Slider::new(&mut self.settings.volume, 0.0..=1.0)
                            .show_value(true)
                            .trailing_fill(true),
                    );
                });
                // 音量同步到 state
                self.state.volume = self.settings.volume;
                self.audio.set_volume(self.settings.volume);

                ui.add_space(4.0);
                ui.label(
                    RichText::new("音质切换后，需要重新播放歌曲才能生效")
                        .color(theme::TEXT_WEAK)
                        .small(),
                );
            });
    }
}