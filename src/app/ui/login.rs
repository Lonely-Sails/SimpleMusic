//! 扫码登录弹窗：显示二维码矩阵 + 阶段状态 + 取消。

use crate::theme;
use eframe::egui::{self, Align2, RichText};
use super::MusicApp;
use super::widgets::draw_qr;

/// 二维码渲染的边长（含留白边框）。
const QR_SIZE: f32 = 260.0;

impl MusicApp {
    pub(crate) fn show_login_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("扫码登录")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(
                        RichText::new("B 站扫码登录")
                            .color(theme::TEXT_PRIMARY)
                            .strong(),
                    );
                    ui.add_space(10.0);
                    match &self.login_qr {
                        Some((_, matrix)) if !matrix.is_empty() => {
                            draw_qr(ui, matrix, QR_SIZE);
                        }
                        _ => {
                            ui.weak("正在生成二维码…");
                        }
                    }
                    ui.add_space(10.0);
                    let (status, color) = if self.login_status.is_empty() {
                        ("请使用手机 B 站 App 扫码".to_string(), theme::TEXT_WEAK)
                    } else {
                        (self.login_status.clone(), theme::TEXT_SECONDARY)
                    };
                    ui.label(RichText::new(status).color(color));
                    ui.add_space(10.0);
                    if ui
                        .add(egui::Button::new(RichText::new("取消").color(theme::TEXT_SECONDARY)))
                        .clicked()
                    {
                        self.cancel_login();
                    }
                });
            });
    }
}