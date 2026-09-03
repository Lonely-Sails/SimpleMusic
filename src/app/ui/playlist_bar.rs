//! 歌单选择栏 + 相关弹窗：创建歌单/同步收藏夹的 Popup、收藏夹选择窗口、歌单管理窗口。

use crate::{icons, theme};
use crate::state::{Playlist, PlaylistKind};
use eframe::egui::{self, Align2, Color32, ComboBox, RichText, Sense, Stroke, Vec2};
use super::MusicApp;

impl MusicApp {
    pub(crate) fn show_playlist_selector(&mut self, ui: &mut egui::Ui) {
        // 预取歌单选项（避免闭包内对 self 的借冲突）
        let playlist_options: Vec<(usize, String, bool, Option<i64>)> = self
            .playlists
            .iter()
            .enumerate()
            .map(|(i, pl)| {
                let label = pl.name.clone();
                let media_id = match pl.kind {
                    PlaylistKind::Online { media_id, .. } => Some(media_id),
                    _ => None,
                };
                (i, label, pl.is_online(), media_id)
            })
            .collect();
        let current_name = self
            .playlists
            .get(self.active_playlist)
            .map(|p| p.name.as_str())
            .unwrap_or("默认歌单")
            .to_owned();

        egui::Panel::top(egui::Id::new("playlist_bar"))
            .frame(
                egui::Frame::new()
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(egui::Margin {
                        left: 22,
                        right: 20,
                        top: 6,
                        bottom: 6,
                    }),
            )
            .show(ui, |ui| {
                // 分割线：与上方状态栏分隔。
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    ui.max_rect().top() + 0.5,
                    Stroke::new(1.0, theme::BORDER_SOFT),
                );
                ui.horizontal(|ui| {
                    // 左侧：歌单选择框，占满剩余空间（预留右侧按钮区宽度）。
                    let reserve = 126.0;
                    let combo_w = (ui.available_width() - reserve).max(120.0);
                    ComboBox::from_id_salt("playlist_selector")
                        .width(combo_w)
                        .selected_text(RichText::new(current_name).color(theme::TEXT_PRIMARY))
                        .show_ui(ui, |ui| {
                            for (i, label, is_online, media_id) in &playlist_options {
                                let label = label.as_str();
                                // 在线歌单行：文件夹图标 + 名称
                                let mut picked = false;
                                ui.horizontal(|ui| {
                                    if *is_online {
                                        let (r, _) =
                                            ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                                        icons::folder(ui.painter(), r, theme::TEXT_SECONDARY);
                                        ui.add_space(4.0);
                                    }
                                    picked |= ui
                                        .selectable_value(
                                            &mut self.active_playlist,
                                            *i,
                                            RichText::new(label).color(theme::TEXT_PRIMARY),
                                        )
                                        .changed();
                                });
                                if picked {
                                    // 切换歌单：当前曲目下标（属于原歌单）不再有效。
                                    self.current_track = None;
                                    if *is_online {
                                        if let Some(mid) = media_id {
                                            self.fav_selected = Some(*mid);
                                            self.fav_items.clear();
                                            self.fav_page = 0;
                                            self.fav_total = 0;
                                            self.fav_has_more = false;
                                            self.fav_loading = false;
                                            self.spawn_fav_resources(*mid, 1);
                                        }
                                    }
                                }
                            }
                        });

                    // 右侧：创建歌单(+)/管理按钮，保持在右侧。
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 管理按钮：重命名 / 删除歌单
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("管理").color(theme::TEXT_SECONDARY),
                                )
                                .fill(theme::BG_CARD)
                                .stroke(eframe::egui::Stroke::NONE)
                                .corner_radius(theme::CORNER),
                            )
                            .clicked()
                        {
                            self.playlist_mgmt_open = true;
                        }
                        ui.add_space(6.0);
                        // + 按钮：创建歌单（用 Popup 菜单）
                        let add_button = ui.add(
                            egui::Button::new(RichText::new("+").color(theme::TEXT_PRIMARY))
                                .fill(theme::BG_CARD)
                                .corner_radius(theme::CORNER),
                        );

                        egui::Popup::menu(&add_button).show(|ui| {
                            ui.set_min_width(160.0);
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("创建本地歌单").color(theme::TEXT_PRIMARY),
                                    )
                                    .fill(theme::BG_CARD)
                                    .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                self.playlists.push(Playlist::local(format!(
                                    "新歌单 {}",
                                    self.playlists.len() + 1
                                )));
                                let new_idx = self.playlists.len() - 1;
                                self.switch_active_playlist(new_idx);
                                self.queue_dirty = true;
                                ui.close();
                            }
                            if self.logged_in() {
                                if ui
                                    .add(
                                        egui::Button::new(
                                            RichText::new("同步B站收藏夹").color(theme::TEXT_PRIMARY),
                                        )
                                        .fill(theme::BG_CARD)
                                        .corner_radius(theme::CORNER),
                                    )
                                    .clicked()
                                {
                                    self.syncing_online = true;
                                    self.spawn_fav_folders();
                                    ui.close();
                                }
                            } else {
                                ui.add_enabled(
                                    false,
                                    egui::Button::new(
                                        RichText::new("同步B站收藏夹（需登录）")
                                            .color(theme::TEXT_WEAK),
                                    ),
                                );
                            }
                        });
                    });
                });
            });
    }

    /// 在线歌单文件夹选择弹窗（由 `ui()` 调用）。
    pub(crate) fn show_online_folder_selector(&mut self, ctx: &egui::Context) {
        let mut open = self.syncing_online;
        let mut close_after = false;
        // 预取收藏夹列表（避免闭包内对 self 的借冲突）。
        let folders: Vec<crate::modules::bilibili::FavFolder> = self.fav_folders.clone();
        let loading = self.fav_folders_loading;

        egui::Window::new("选择B站收藏夹")
            .id(egui::Id::new("online_folder_selector"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                if loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("正在加载收藏夹…").color(theme::TEXT_SECONDARY));
                    });
                    return;
                }
                if folders.is_empty() {
                    ui.label(RichText::new("暂无收藏夹").color(theme::TEXT_WEAK));
                    return;
                }
                ui.label(RichText::new("选择一个收藏夹作为歌单：").color(theme::TEXT_SECONDARY));
                ui.add_space(6.0);
                for f in folders {
                    let mut clicked = false;
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        icons::folder(ui.painter(), r, theme::TEXT_SECONDARY);
                        ui.add_space(4.0);
                        clicked = ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("{} ({})", f.title, f.media_count))
                                        .color(theme::TEXT_PRIMARY),
                                )
                                .fill(theme::BG_CARD)
                                .corner_radius(theme::CORNER),
                            )
                            .clicked();
                    });
                    if clicked {
                        // 检查是否已添加；新导入的收藏夹必须落盘，否则重启后丢失。
                        let mut newly_added = false;
                        if self.online_playlist_index(f.id).is_none() {
                            self.playlists.push(Playlist {
                                name: f.title.clone(),
                                songs: Vec::new(),
                                kind: PlaylistKind::Online {
                                    media_id: f.id,
                                    folder_title: f.title.clone(),
                                },
                            });
                            newly_added = true;
                        }
                        if newly_added {
                            self.queue_dirty = true;
                        }
                        // 切换到该歌单
                        if let Some(idx) = self.online_playlist_index(f.id) {
                            self.switch_active_playlist(idx);
                            self.fav_selected = Some(f.id);
                            self.fav_items.clear();
                            self.fav_page = 0;
                            self.fav_total = 0;
                            self.fav_has_more = false;
                            self.fav_loading = false;
                            self.spawn_fav_resources(f.id, 1);
                        }
                        close_after = true;
                    }
                }
                ui.add_space(6.0);
                if ui
                    .add(egui::Button::new(RichText::new("取消").color(theme::TEXT_SECONDARY)))
                    .clicked()
                {
                    close_after = true;
                }
            });
        if close_after {
            open = false;
        }
        self.syncing_online = open;
    }

    // ---- 歌单管理（重命名 / 删除） ----

    pub(crate) fn show_playlist_manage_window(&mut self, ctx: &egui::Context) {
        let mut open = self.playlist_mgmt_open;
        // 预取歌单快照，避免闭包内对 self 的借冲突。
        let snapshot: Vec<(usize, String, bool, usize)> = self
            .playlists
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.name.clone(), p.is_online(), p.songs.len()))
            .collect();

        let mut close_after = false;
        egui::Window::new("歌单管理")
            .id(egui::Id::new("playlist_manage_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("本地歌单可重命名；在线歌单可删除（B 站收藏夹不受影响）")
                        .color(theme::TEXT_WEAK)
                        .small(),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                for (i, name, is_online, count) in &snapshot {
                    let mut do_delete = false;
                    let mut do_rename = false;
                    ui.horizontal(|ui| {
                        if *is_online {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                            icons::folder(ui.painter(), r, theme::TEXT_SECONDARY);
                            ui.add_space(2.0);
                        }
                        ui.label(RichText::new(format!("{name} ({count})")).color(theme::TEXT_PRIMARY));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("删除").color(theme::TEXT_SECONDARY))
                                        .fill(theme::BG_CARD)
                                        .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                do_delete = true;
                            }
                            if !*is_online
                                && ui
                                    .add(
                                        egui::Button::new(RichText::new("重命名").color(theme::TEXT_SECONDARY))
                                            .fill(theme::BG_CARD)
                                            .corner_radius(theme::CORNER),
                                    )
                                    .clicked()
                            {
                                do_rename = true;
                            }
                        });
                    });
                    if do_rename {
                        self.renaming_idx = Some(*i);
                        self.rename_text = name.clone();
                    }
                    if do_delete {
                        self.delete_playlist(*i);
                        // 删除后快照索引已失效，标记关闭让用户重新打开查看最新状态。
                        close_after = true;
                    }
                    if self.renaming_idx == Some(*i) {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.rename_text)
                                    .desired_width(180.0)
                                    .hint_text("新歌单名"),
                            );
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("确定").color(theme::TEXT_ON_ACCENT))
                                        .fill(theme::ACCENT)
                                        .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                let text = self.rename_text.clone();
                                self.rename_playlist(*i, &text);
                            }
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("取消").color(theme::TEXT_SECONDARY))
                                        .fill(theme::BG_CARD)
                                        .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                self.renaming_idx = None;
                            }
                        });
                    }
                }
            });
        if close_after {
            open = false;
        }
        if !open {
            // 窗口关闭时清掉未完成的重命名状态，避免下次打开残留。
            self.renaming_idx = None;
        }
        self.playlist_mgmt_open = open;
    }
}