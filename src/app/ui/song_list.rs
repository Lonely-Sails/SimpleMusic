//! 歌曲列表：本地歌单列表（`show_local_songs`）与在线收藏夹列表（`show_online_songs`）。
//!
//! 两者共用行绘制（封面 + 标题 + 副标题 + 删除/右键菜单），区别在于数据源
//! （`QueueItem` vs `FavItem`）与删除权限（在线列表只读）。

use crate::modules::bilibili::FavItem;
use crate::state::QueueItem;
use crate::util::filter::song_matches_query;
use crate::util::fmt::format_secs;
use crate::{icons, theme};
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Vec2,
};
use super::MusicApp;
use super::widgets::{icon_button, paint_cover_image, paint_placeholder_cover, truncate_label};

/// 歌曲列表内容相对滚动区/窗口两边的水平留白（滚动条贴右，内容左右留白）。
const LIST_PAD_X: f32 = 14.0;

impl MusicApp {
    // ---- 本地歌单歌曲列表 ----

    pub(crate) fn show_local_songs(&mut self, ui: &mut egui::Ui) {
        // 克隆条目，避免闭包内 self 借冲突。
        let rows: Vec<(usize, QueueItem)> = self
            .active_songs()
            .iter()
            .cloned()
            .enumerate()
            .collect();
        let total = rows.len();
        let query = self.search_text.trim().to_lowercase();
        let visible: Vec<(usize, QueueItem)> = if query.is_empty() {
            rows
        } else {
            rows.into_iter()
                .filter(|(_, it)| song_matches_query(&it.title, &it.uploader, &query))
                .collect()
        };
        // 标题行：歌曲数量 + 搜索框
        ui.horizontal(|ui| {
            ui.add_space(LIST_PAD_X);
            if query.is_empty() {
                ui.label(
                    RichText::new(format!("歌曲 ({total})"))
                        .strong()
                        .color(theme::TEXT_PRIMARY),
                );
            } else {
                ui.label(
                    RichText::new(format!("歌曲 ({}/{})", visible.len(), total))
                        .strong()
                        .color(theme::TEXT_PRIMARY),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(LIST_PAD_X);
                if !query.is_empty() {
                    if icon_button(ui, 24.0, icons::cross, "清空搜索").clicked() {
                        self.search_text.clear();
                    }
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_text)
                        .hint_text("搜索标题 / UP 主")
                        .desired_width(180.0),
                );
            });
        });
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if visible.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        if total == 0 {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::hover());
                            icons::note(ui.painter(), r, theme::TEXT_WEAK);
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new("歌单为空\n从下方链接导入歌曲")
                                    .color(theme::TEXT_WEAK),
                            );
                        } else {
                            ui.label(
                                RichText::new("没有匹配的歌曲").color(theme::TEXT_WEAK),
                            );
                            ui.add_space(6.0);
                            if ui.add(theme::small_button("清空搜索")).clicked() {
                                self.search_text.clear();
                            }
                        }
                    });
                    return;
                }

                let mut actions: Vec<(usize, bool)> = Vec::new();
                let mut remove: Option<usize> = None;
                let row_h = 56.0;

                for (i, item) in &visible {
                    let i = *i;
                    let selected = self.current_bvid().map(|b| b == item.bvid).unwrap_or(false);
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), row_h),
                        Sense::click(),
                    );
                    // 行内容在滚动区两侧留白（滚动条本身贴右边）。
                    let row = rect.shrink2(Vec2::new(LIST_PAD_X, 0.0));
                    let bg = if selected {
                        theme::BG_CARD
                    } else if resp.hovered() {
                        theme::BG_HOVER
                    } else {
                        Color32::TRANSPARENT
                    };
                    {
                        let painter = ui.painter();
                        if bg != Color32::TRANSPARENT {
                            painter.rect_filled(row, theme::CORNER, bg);
                        }
                        if selected {
                            painter.rect_filled(
                                Rect::from_min_size(row.min, Vec2::new(3.0, row.height())),
                                2.0,
                                theme::ACCENT,
                            );
                        }
                    }
                    // 封面 44×44 圆角
                    let cover_rect = Rect::from_min_size(
                        Pos2::new(row.left() + 10.0, row.center().y - 22.0),
                        Vec2::splat(44.0),
                    );
                    self.draw_cover_row(ui, cover_rect, &item.bvid, &item.cover_url);
                    let painter = ui.painter();
                    let text_x = row.left() + 64.0;
                    let max_w = row.width() - 100.0;
                    let title = truncate_label(ui, &item.title, max_w);
                    painter.text(
                        Pos2::new(text_x, row.top() + 10.0),
                        Align2::LEFT_TOP,
                        title,
                        FontId::proportional(13.0),
                        if selected {
                            theme::ACCENT_HOVER
                        } else {
                            theme::TEXT_PRIMARY
                        },
                    );
                    let sub = format!(
                        "{} · {}",
                        item.uploader,
                        format_secs(item.duration_secs)
                    );
                    let sub = truncate_label(ui, &sub, max_w);
                    painter.text(
                        Pos2::new(text_x, row.top() + 32.0),
                        Align2::LEFT_TOP,
                        sub,
                        FontId::proportional(11.0),
                        theme::TEXT_SECONDARY,
                    );
                    // 删除按钮 ×
                    let btn_rect = Rect::from_center_size(
                        Pos2::new(row.right() - 20.0, row.center().y),
                        Vec2::splat(24.0),
                    );
                    let btn_resp = ui.interact(
                        btn_rect,
                        ui.id().with(("song_remove", i)),
                        Sense::click(),
                    );
                    if btn_resp.hovered() {
                        ui.painter().rect_filled(btn_rect, theme::CORNER, theme::BG_ACTIVE);
                    }
                    icons::cross(
                        &ui.painter(),
                        btn_rect.shrink(5.0),
                        if btn_resp.hovered() {
                            theme::TEXT_PRIMARY
                        } else {
                            theme::TEXT_SECONDARY
                        },
                    );
                    // 右键菜单：复制 BV 号 / 添加到其他本地歌单
                    resp.context_menu(|ui| {
                        ui.set_min_width(170.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("复制 BV 号").color(theme::TEXT_PRIMARY),
                                )
                                .fill(theme::BG_CARD)
                                .corner_radius(theme::CORNER),
                            )
                            .clicked()
                        {
                            ui.ctx().copy_text(item.bvid.clone());
                            ui.close();
                        }
                        ui.separator();
                        let targets: Vec<(usize, String)> = self
                            .playlists
                            .iter()
                            .enumerate()
                            .filter(|(j, p)| *j != self.active_playlist && !p.is_online())
                            .map(|(j, p)| (j, p.name.clone()))
                            .collect();
                        if targets.is_empty() {
                            ui.add_enabled(
                                false,
                                egui::Button::new(
                                    RichText::new("没有其他本地歌单").color(theme::TEXT_WEAK),
                                ),
                            );
                        }
                        for (j, name) in &targets {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("添加到「{name}」"))
                                            .color(theme::TEXT_PRIMARY),
                                    )
                                    .fill(theme::BG_CARD)
                                    .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                self.add_song_to_local_playlist(item.clone(), *j);
                                ui.close();
                            }
                        }
                    });
                    if resp.clicked() {
                        actions.push((i, true));
                    }
                    if btn_resp.clicked() {
                        remove = Some(i);
                    }
                }
                for (i, item) in actions {
                    let _ = item;
                    self.play_track(i);
                }
                if let Some(i) = remove {
                    self.remove_track(i);
                }
            });
    }

    // ---- 在线歌单（B站收藏夹） ----

    pub(crate) fn show_online_songs(&mut self, ui: &mut egui::Ui) {
        if !self.logged_in() {
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(24.0), Sense::hover());
                icons::note_double(ui.painter(), r, theme::TEXT_WEAK);
                ui.label(
                    RichText::new("登录后可查看 B 站收藏夹").color(theme::TEXT_WEAK),
                );
            });
            return;
        }
        if self.fav_folders_loading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("正在加载收藏夹…").color(theme::TEXT_SECONDARY));
            });
        }

        let count = self.fav_items.len();
        let total = self.fav_total;
        let query = self.search_text.trim().to_lowercase();
        let fav_items: Vec<FavItem> = if query.is_empty() {
            self.fav_items.clone()
        } else {
            self.fav_items
                .iter()
                .filter(|it| song_matches_query(&it.title, &it.owner, &query))
                .cloned()
                .collect()
        };
        ui.horizontal(|ui| {
            ui.add_space(LIST_PAD_X);
            if query.is_empty() {
                ui.label(
                    RichText::new(format!("歌曲 ({count}/{total})"))
                        .strong()
                        .color(theme::TEXT_PRIMARY),
                );
            } else {
                ui.label(
                    RichText::new(format!("歌曲 ({}/{})", fav_items.len(), count))
                        .strong()
                        .color(theme::TEXT_PRIMARY),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(LIST_PAD_X);
                if !query.is_empty() {
                    if icon_button(ui, 24.0, icons::cross, "清空搜索").clicked() {
                        self.search_text.clear();
                    }
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.search_text)
                        .hint_text("搜索标题 / UP 主")
                        .desired_width(180.0),
                );
            });
        });
        ui.add_space(10.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.fav_loading && self.fav_items.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("正在加载歌曲…").color(theme::TEXT_SECONDARY));
                    });
                }
                let mut play: Option<String> = None;
                let row_h = 56.0;
                if fav_items.is_empty() && count > 0 {
                    // 有歌曲但搜索无匹配
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new("没有匹配的歌曲").color(theme::TEXT_WEAK),
                        );
                        ui.add_space(6.0);
                        if ui.add(theme::small_button("清空搜索")).clicked() {
                            self.search_text.clear();
                        }
                    });
                }
                for item in &fav_items {
                    let selected = self.current_bvid().map(|b| b == item.bvid).unwrap_or(false);
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), row_h),
                        Sense::click(),
                    );
                    // 行内容在滚动区两侧留白（滚动条本身贴右边）。
                    let row = rect.shrink2(Vec2::new(LIST_PAD_X, 0.0));
                    let bg = if selected {
                        theme::BG_CARD
                    } else if resp.hovered() {
                        theme::BG_HOVER
                    } else {
                        Color32::TRANSPARENT
                    };
                    {
                        let painter = ui.painter();
                        if bg != Color32::TRANSPARENT {
                            painter.rect_filled(row, theme::CORNER, bg);
                        }
                        if selected {
                            painter.rect_filled(
                                Rect::from_min_size(row.min, Vec2::new(3.0, row.height())),
                                2.0,
                                theme::ACCENT,
                            );
                        }
                    }
                    // 封面 44×44 圆角
                    let cover_rect = Rect::from_min_size(
                        Pos2::new(row.left() + 10.0, row.center().y - 22.0),
                        Vec2::splat(44.0),
                    );
                    let cover_url = item.cover_url.as_deref().unwrap_or("");
                    self.draw_cover_row(ui, cover_rect, &item.bvid, cover_url);
                    let painter = ui.painter();
                    let text_x = row.left() + 64.0;
                    let max_w = row.width() - 100.0;
                    let title = truncate_label(ui, &item.title, max_w);
                    painter.text(
                        Pos2::new(text_x, row.top() + 10.0),
                        Align2::LEFT_TOP,
                        title,
                        FontId::proportional(13.0),
                        if selected {
                            theme::ACCENT_HOVER
                        } else {
                            theme::TEXT_PRIMARY
                        },
                    );
                    let sub = format!("{} · {}", item.owner, format_secs(item.duration_secs));
                    let sub = truncate_label(ui, &sub, max_w);
                    painter.text(
                        Pos2::new(text_x, row.top() + 32.0),
                        Align2::LEFT_TOP,
                        sub,
                        FontId::proportional(11.0),
                        theme::TEXT_SECONDARY,
                    );
                    // 右键菜单：复制 BV 号 / 收藏到本地歌单
                    resp.context_menu(|ui| {
                        ui.set_min_width(170.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("复制 BV 号").color(theme::TEXT_PRIMARY),
                                )
                                .fill(theme::BG_CARD)
                                .corner_radius(theme::CORNER),
                            )
                            .clicked()
                        {
                            ui.ctx().copy_text(item.bvid.clone());
                            ui.close();
                        }
                        ui.separator();
                        let targets: Vec<(usize, String)> = self
                            .playlists
                            .iter()
                            .enumerate()
                            .filter(|(_, p)| !p.is_online())
                            .map(|(j, p)| (j, p.name.clone()))
                            .collect();
                        if targets.is_empty() {
                            ui.add_enabled(
                                false,
                                egui::Button::new(
                                    RichText::new("没有本地歌单").color(theme::TEXT_WEAK),
                                ),
                            );
                        }
                        for (j, name) in &targets {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("收藏到「{name}」"))
                                            .color(theme::TEXT_PRIMARY),
                                    )
                                    .fill(theme::BG_CARD)
                                    .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                let qi = QueueItem::new_with_cover(
                                    item.bvid.clone(),
                                    item.title.clone(),
                                    item.owner.clone(),
                                    item.duration_secs,
                                    item.cover_url.clone().unwrap_or_default(),
                                );
                                self.add_song_to_local_playlist(qi, *j);
                                ui.close();
                            }
                        }
                    });
                    if resp.clicked() {
                        play = Some(item.bvid.clone());
                    }
                }
                if let Some(item) = play {
                    // 播放列表 = 当前选中的在线歌单（收藏夹条目），直接点播。
                    self.play_bvid(item);
                }
            });
    }

    /// 绘制封面缩略图行（有纹理画图，否则画占位符）。
    pub(crate) fn draw_cover_row(&mut self, ui: &mut egui::Ui, cover_rect: Rect, key: &str, url: &str) {
        if !url.is_empty() {
            if let Some(tex) = self.covers.texture(key) {
                // 纯绘制圆角图片：不创建 widget，避免改变行间距导致封面加载后整列跳位。
                paint_cover_image(ui.painter(), cover_rect, tex);
                return;
            }
        }
        paint_placeholder_cover(ui.painter(), cover_rect);
    }
}