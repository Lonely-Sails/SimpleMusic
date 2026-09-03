//! 状态栏：左侧用户头像 + 昵称，右侧登录态 + 设置按钮。

use crate::{icons, theme};
use eframe::egui::{self, RichText, Sense, Vec2};
use super::MusicApp;
use super::widgets::{icon_button, paint_avatar, truncate_label};

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
                // 用文字按钮的实际高度计算，让「设置」图标与「登录/退出」文字按钮高度一致。
                let ctrl_h = ui.text_style_height(&egui::TextStyle::Button)
                    + 2.0 * ui.spacing().button_padding.y;
                ui.horizontal(|ui| {
                    // 左：用户头像 + 昵称
                    if self.logged_in() {
                        self.show_user_area(ui);
                    }

                    // 右：设置 + 登录/退出
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(4.0);
                        // 设置按钮
                        if icon_button(ui, ctrl_h, icons::gear, "设置").clicked() {
                            self.settings_window_open = true;
                        }
                        ui.add_space(6.0);
                        if self.logged_in() {
                            if ui
                                .add(theme::small_button("退出").min_size(Vec2::new(0.0, ctrl_h)))
                                .on_hover_text("退出登录")
                                .clicked()
                            {
                                self.do_logout();
                            }
                        } else {
                            if ui
                                .add(theme::small_button("登录").min_size(Vec2::new(0.0, ctrl_h)))
                                .clicked()
                            {
                                self.spawn_login();
                            }
                            ui.add_space(6.0);
                            ui.label(RichText::new("未登录").color(theme::TEXT_WEAK).small());
                        }
                    });
                });
            });
    }

    /// 左侧用户区：圆形头像（异步加载）+ 昵称。
    fn show_user_area(&mut self, ui: &mut egui::Ui) {
        let key = self.avatar_key();
        let texture = self.covers.texture(&key);
        let initial = self
            .uname
            .as_deref()
            .and_then(|s| s.chars().next())
            .map(|c| c.to_uppercase().to_string());
        let (r, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
        paint_avatar(ui.painter(), r, texture, initial.as_deref());
        ui.add_space(8.0);
        let name = match self.uname.as_deref() {
            Some(u) if !u.is_empty() => truncate_label(ui, u, 140.0),
            _ => {
                let mid = self.mid.unwrap_or(0);
                format!("UID {mid}")
            }
        };
        ui.label(RichText::new(name).color(theme::TEXT_SECONDARY).size(12.0));
    }

    /// 登出：清空会话与登录态，并重置收藏夹视图。
    fn do_logout(&mut self) {
        if let Ok(mut b) = self.bili.lock() {
            let _ = b.logout();
        }
        self.login_state = false;
        self.mid = None;
        self.uname = None;
        self.face = None;
        self.fav_initiated = false;
        self.fav_folders.clear();
        self.fav_items.clear();
        self.fav_selected = None;
    }
}
