//! 歌曲列表：本地歌单列表（`show_local_songs`）与在线收藏夹列表（`show_online_songs`）。
//!
//! 两者共用行绘制（封面 + 标题 + 副标题 + 删除/右键菜单），区别在于数据源
//! （`QueueItem` vs `FavItem`）与删除权限（在线列表只读）。

use super::MusicApp;
use super::widgets::{
    paint_cover_image, paint_placeholder_cover, song_search_field, truncate_label,
};
use crate::modules::bilibili::FavItem;
use crate::state::QueueItem;
use crate::util::filter::song_matches_query;
use crate::util::fmt::format_secs;
use crate::{icons, theme};
use eframe::egui::{
    self, Align2, Color32, FontId, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, Vec2,
};

/// 歌曲列表内容相对滚动区/窗口两边的水平留白（滚动条贴右，内容左右留白）。
const LIST_PAD_X: f32 = 14.0;
/// 歌曲行高。60 给封面（46）上下各留 7px，比 56 更透气。
const ROW_H: f32 = 60.0;
/// 行与行之间的竖向间距（行底矩形收缩，形成"卡片间缝"）。
const ROW_GAP: f32 = 2.0;
/// 行内封面边长。
const COVER_SIZE: f32 = 46.0;
/// 封面左内边距。
const COVER_PAD: f32 = 10.0;
/// 标题/副标题文字的左起点 = 封面右边 + 间距。
const TEXT_X: f32 = COVER_PAD + COVER_SIZE + 12.0;

impl MusicApp {
    // ---- 本地歌单歌曲列表 ----

    pub(crate) fn show_local_songs(&mut self, ui: &mut egui::Ui) {
        // 克隆条目，避免闭包内 self 借冲突。
        let rows: Vec<(usize, QueueItem)> =
            self.active_songs().iter().cloned().enumerate().collect();
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
            // 计数做成一个小「药丸」徽标：点缀色淡底 + 圆角，比裸文字更成组。
            let label = if query.is_empty() {
                format!("歌曲 {total}")
            } else {
                format!("歌曲 {}/{}", visible.len(), total)
            };
            egui::Frame::new()
                .fill(theme::ACCENT_SOFT)
                .corner_radius(theme::CORNER_SM)
                .inner_margin(egui::Margin::symmetric(9, 3))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(label)
                            .strong()
                            .size(12.0)
                            .color(theme::ACCENT_HOVER),
                    );
                });
            // 固定 id_salt 的搜索框：输入时不再失焦/打断中文输入法，清空按钮
            // 常驻占位不挤动布局；点击「×」在组件内就地清空（见 widgets::song_search_field）。
            song_search_field(ui, &mut self.search_text);
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
                            ui.label(RichText::new("没有匹配的歌曲").color(theme::TEXT_WEAK));
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
                let row_h = ROW_H;

                for (i, item) in &visible {
                    let i = *i;
                    let selected = self.current_bvid().map(|b| b == item.bvid).unwrap_or(false);
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), row_h),
                        Sense::click(),
                    );
                    // 行内容在滚动区两侧留白（滚动条本身贴右边），上下各收 ROW_GAP/2 形成行缝。
                    let row = rect
                        .shrink2(Vec2::new(LIST_PAD_X, 0.0))
                        .shrink2(Vec2::new(0.0, ROW_GAP * 0.5));
                    let bg = if selected {
                        theme::ACCENT_SOFT
                    } else if resp.hovered() {
                        theme::BG_HOVER
                    } else {
                        Color32::TRANSPARENT
                    };
                    {
                        let painter = ui.painter();
                        if bg != Color32::TRANSPARENT {
                            painter.rect_filled(row, theme::CORNER_LG, bg);
                        }
                        // 选中态：左侧圆角竖条（点缀色），并给整行补一圈极淡描边。
                        if selected {
                            painter.rect_filled(
                                Rect::from_min_size(row.min, Vec2::new(3.0, row.height())),
                                2.0,
                                theme::ACCENT,
                            );
                            painter.rect_stroke(
                                row,
                                theme::CORNER_LG,
                                Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.35)),
                                StrokeKind::Inside,
                            );
                        }
                    }
                    // 封面 46×46 圆角
                    let cover_rect = Rect::from_min_size(
                        Pos2::new(row.left() + COVER_PAD, row.center().y - COVER_SIZE * 0.5),
                        Vec2::splat(COVER_SIZE),
                    );
                    self.draw_cover_row(ui, cover_rect, &item.bvid, &item.cover_url);
                    let painter = ui.painter();
                    let text_x = row.left() + TEXT_X;
                    let max_w = row.width() - TEXT_X - 44.0;
                    // 正在播放的行在标题前画一个点缀色音符指示。
                    if selected {
                        let badge = Rect::from_min_size(
                            Pos2::new(text_x, row.top() + 12.0),
                            Vec2::splat(12.0),
                        );
                        icons::note(&painter, badge, theme::ACCENT);
                    }
                    let title_x = if selected { text_x + 18.0 } else { text_x };
                    // 选中行标题右移让位给音符徽标，可用宽度相应减少，避免压到删除按钮。
                    let title_w = if selected { max_w - 18.0 } else { max_w };
                    let title = truncate_label(ui, &item.title, title_w);
                    painter.text(
                        Pos2::new(title_x, row.top() + 11.0),
                        Align2::LEFT_TOP,
                        title,
                        FontId::proportional(13.0),
                        if selected {
                            theme::ACCENT_HOVER
                        } else {
                            theme::TEXT_PRIMARY
                        },
                    );
                    let sub = format!("{} · {}", item.uploader, format_secs(item.duration_secs));
                    let sub = truncate_label(ui, &sub, max_w);
                    painter.text(
                        Pos2::new(text_x, row.top() + 33.0),
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
                    let btn_resp =
                        ui.interact(btn_rect, ui.id().with(("song_remove", i)), Sense::click());
                    if btn_resp.hovered() {
                        ui.painter()
                            .rect_filled(btn_rect, theme::CORNER_SM, theme::BG_ACTIVE);
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
                ui.label(RichText::new("登录后可查看 B 站收藏夹").color(theme::TEXT_WEAK));
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
            let label = if query.is_empty() {
                format!("歌曲 {count}/{total}")
            } else {
                format!("歌曲 {}/{}", fav_items.len(), count)
            };
            egui::Frame::new()
                .fill(theme::ACCENT_SOFT)
                .corner_radius(theme::CORNER_SM)
                .inner_margin(egui::Margin::symmetric(9, 3))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(label)
                            .strong()
                            .size(12.0)
                            .color(theme::ACCENT_HOVER),
                    );
                });
            // 固定 id_salt 的搜索框（同 show_local_songs，见 widgets::song_search_field）。
            song_search_field(ui, &mut self.search_text);
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
                let row_h = ROW_H;
                if fav_items.is_empty() && count > 0 {
                    // 有歌曲但搜索无匹配
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("没有匹配的歌曲").color(theme::TEXT_WEAK));
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
                    // 行内容在滚动区两侧留白（滚动条本身贴右边），上下各收 ROW_GAP/2 形成行缝。
                    let row = rect
                        .shrink2(Vec2::new(LIST_PAD_X, 0.0))
                        .shrink2(Vec2::new(0.0, ROW_GAP * 0.5));
                    let bg = if selected {
                        theme::ACCENT_SOFT
                    } else if resp.hovered() {
                        theme::BG_HOVER
                    } else {
                        Color32::TRANSPARENT
                    };
                    {
                        let painter = ui.painter();
                        if bg != Color32::TRANSPARENT {
                            painter.rect_filled(row, theme::CORNER_LG, bg);
                        }
                        if selected {
                            painter.rect_filled(
                                Rect::from_min_size(row.min, Vec2::new(3.0, row.height())),
                                2.0,
                                theme::ACCENT,
                            );
                            painter.rect_stroke(
                                row,
                                theme::CORNER_LG,
                                Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.35)),
                                StrokeKind::Inside,
                            );
                        }
                    }
                    // 封面 46×46 圆角
                    let cover_rect = Rect::from_min_size(
                        Pos2::new(row.left() + COVER_PAD, row.center().y - COVER_SIZE * 0.5),
                        Vec2::splat(COVER_SIZE),
                    );
                    let cover_url = item.cover_url.as_deref().unwrap_or("");
                    self.draw_cover_row(ui, cover_rect, &item.bvid, cover_url);
                    let painter = ui.painter();
                    let text_x = row.left() + TEXT_X;
                    let max_w = row.width() - TEXT_X - 44.0;
                    if selected {
                        let badge = Rect::from_min_size(
                            Pos2::new(text_x, row.top() + 12.0),
                            Vec2::splat(12.0),
                        );
                        icons::note(&painter, badge, theme::ACCENT);
                    }
                    let title_x = if selected { text_x + 18.0 } else { text_x };
                    let title_w = if selected { max_w - 18.0 } else { max_w };
                    let title = truncate_label(ui, &item.title, title_w);
                    painter.text(
                        Pos2::new(title_x, row.top() + 11.0),
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
                        Pos2::new(text_x, row.top() + 33.0),
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
    ///
    /// 若该行**落在当前可视区域内**且封面尚未加载，则以高优先级请求下载：
    /// 大歌单/收藏夹的后台预取可能有上百张在排队，可视区域内的必须插队，
    /// 否则用户滚动后要等预取排完才看得到封面。
    pub(crate) fn draw_cover_row(
        &mut self,
        ui: &mut egui::Ui,
        cover_rect: Rect,
        key: &str,
        url: &str,
    ) {
        if !url.is_empty() {
            if let Some(tex) = self.covers.texture(key) {
                // 纯绘制圆角图片：不创建 widget，避免改变行间距导致封面加载后整列跳位。
                paint_cover_image(ui.painter(), cover_rect, tex);
                return;
            }
            // 可视区域内（含少量预读余量）→ 提升到高优先级队列。
            if ui.is_rect_visible(cover_rect) {
                self.covers.request_visible(key, url);
            }
        }
        paint_placeholder_cover(ui.painter(), cover_rect);
    }
}
