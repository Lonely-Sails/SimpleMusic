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
            // 预留右侧「添加并播放」按钮 + 右侧留白，输入框占满剩余空间。
            let btn_reserve = 132.0;
            let field_w = (ui.available_width() - btn_reserve).max(140.0);
            // 显式 id_salt：输入框前方的 spinner（pending_import）出现/消失时，
            // 自动 id 会漂移导致失焦/打断中文输入法（与歌单搜索框同一坑）。
            ui.add(
                egui::TextEdit::singleline(&mut self.import_text)
                    .id_salt(egui::Id::new("import_bvid_field"))
                    .hint_text("BV 号 / 视频链接 / b23.tv 短链")
                    .desired_width(field_w),
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