//! 导入 B 站歌曲输入栏（纯 BV 号 / 视频链接 / b23.tv 短链）。

use crate::theme;
use eframe::egui::{self, RichText};
use super::MusicApp;

impl MusicApp {
    pub(crate) fn show_import(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("导入 B 站歌曲")
                .strong()
                .color(theme::TEXT_PRIMARY),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.import_text)
                    .hint_text("BV 号 / 视频链接 / b23.tv 短链")
                    .desired_width(f32::INFINITY),
            );
            let can_submit = !self.import_text.trim().is_empty();
            if ui
                .add_enabled(can_submit, theme::primary_button("添加并播放"))
                .clicked()
            {
                let raw = self.import_text.trim().to_string();
                self.spawn_import(raw);
            }
            if self.pending_import {
                ui.spinner();
            }
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("支持：纯 BV 号、www.bilibili.com/video/BV..、b23.tv 短链")
                .color(theme::TEXT_WEAK)
                .small(),
        );
    }
}