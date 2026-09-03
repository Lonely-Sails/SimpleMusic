//! 状态栏：左侧当前曲目，右侧登录态 + 设置按钮。

use crate::{icons, theme};
use eframe::egui::{self, RichText, Sense, Vec2};
use super::MusicApp;
use super::widgets::truncate_label;

impl MusicApp {
    pub(crate) fn show_status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top(egui::Id::new("status_bar"))
            .frame(egui::Frame::new().fill(egui::Color32::TRANSPARENT).inner_margin(egui::Margin {
                left: 18,
                right: 16,
                top: 10,
                bottom: 8,
            }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // 左：当前播放曲目（简洁）
                    if let Some(item) = self.current_item() {
                        let (note_rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        icons::note(ui.painter(), note_rect, theme::ACCENT);
                        ui.add_space(2.0);
                        let label = truncate_label(ui, &item.title, 200.0);
                        ui.label(RichText::new(label).color(theme::TEXT_PRIMARY).size(12.0));
                    }

                    // 右：登录 + 设置
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(4.0);
                        // 设置按钮
                        if super::widgets::icon_button(ui, 26.0, icons::gear, "设置").clicked()
                        {
                            self.settings_window_open = true;
                        }
                        // 登录状态：优先显示昵称，未知时回退 UID。
                        if self.logged_in() {
                            let label = match self.uname.as_deref() {
                                Some(u) if !u.is_empty() => truncate_label(ui, u, 90.0),
                                _ => {
                                    let mid = self.mid.unwrap_or(0);
                                    format!("UID {mid}")
                                }
                            };
                            ui.label(
                                RichText::new(label)
                                    .color(theme::TEXT_WEAK)
                                    .small(),
                            );
                            if ui
                                .add(theme::small_button("退出"))
                                .on_hover_text("退出登录")
                                .clicked()
                            {
                                if let Ok(mut b) = self.bili.lock() {
                                    let _ = b.logout();
                                }
                                self.mid = None;
                                self.uname = None;
                                self.fav_initiated = false;
                                self.fav_folders.clear();
                                self.fav_items.clear();
                                self.fav_selected = None;
                            }
                        } else {
                            if ui.add(theme::small_button("登录")).clicked() {
                                self.spawn_login();
                            }
                            ui.label(
                                RichText::new("未登录").color(theme::TEXT_WEAK).small(),
                            );
                        }
                    });
                });
            });
    }
}