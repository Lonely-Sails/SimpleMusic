//! 主界面 UI：按区域拆分的组件 + 主窗口组装（`show_main`）。
//!
//! 文件分工：
//! - `mod.rs`：主窗口组装 `show_main`。
//! - `widgets.rs`：跨区域复用的纯 egui 小组件/文本工具（按钮、转圈、封面占位、二维码、截断）。
//! - `title_bar.rs`：自定义标题栏 + 窗口控制按钮 + 右下角缩放把手。
//! - `status_bar.rs`：状态栏（当前曲目 + 登录态 + 设置按钮）。
//! - `playlist_bar.rs`：歌单选择栏 + 收藏夹选择弹窗 + 歌单管理窗口。
//! - `song_list.rs`：本地歌单 / 在线收藏夹的歌曲列表（含右键菜单、搜索过滤）。
//! - `import.rs`：导入 B 站歌曲输入栏。
//! - `player_bar.rs`：底部播放条（控制按钮/进度/音量/切歌模式/桌面歌词开关）。
//! - `settings.rs`：设置窗口。
//! - `login.rs`：扫码登录弹窗。
//! - `lyrics_viewport.rs`：桌面歌词悬浮窗（独立 viewport）。
//!
//! 约定（沿用旧版 `app.rs` 的借用模式）：
//! - 所有界面图标走 `crate::icons::*`，颜色一律用 `crate::theme::` 语义常量。
//! - 闭包内不改 `open`（弹窗开关用外部 `close_after` 标志，`show` 之后再写回）。

pub mod import;
pub mod login;
pub mod lyrics_viewport;
pub mod playlist_bar;
pub mod player_bar;
pub mod settings;
pub mod song_list;
pub mod status_bar;
pub mod title_bar;
pub mod toast;
pub mod widgets;

use crate::theme;
use eframe::egui::{self, Color32, Stroke, StrokeKind};
use super::MusicApp;

impl MusicApp {
    pub(crate) fn show_main(&mut self, ui: &mut egui::Ui) {
        let st = self.audio.status();
        self.sync_playback(&st);

        // ── 悬浮卡片背景（透明窗口 + 圆角） ──
        let card = ui.max_rect();
        ui.painter().rect_filled(card, theme::CORNER_XL, theme::BG_WINDOW);
        ui.painter().rect_stroke(
            card,
            theme::CORNER_XL,
            Stroke::new(1.0, theme::BORDER_SOFT),
            StrokeKind::Inside,
        );

        // ── 自定义标题栏（窗口控制） ──
        self.show_custom_title_bar(ui);

        // ── 顶部栏：用户头像 + 昵称 + 登录态 + 设置按钮 ──
        self.show_status_bar(ui);

        // ── 歌单选择栏 ──
        self.show_playlist_selector(ui);

        // ── 底部控制区（先于 CentralPanel，遵循 egui "CentralPanel 最后添加" 规则） ──
        self.show_player_bar(ui, &st);

        // ---- 歌单内容 + 导入输入框 ----
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(Color32::TRANSPARENT)
                    // 左右让出歌曲区域留白；右边际设 0，让滚动条真正贴到窗口最右。
                    .inner_margin(egui::Margin {
                        left: 18,
                        right: 0,
                        top: 10,
                        bottom: 8,
                    }),
            )
            .show(ui, |ui| {
                if self.active_playlist_is_online() {
                    self.show_online_songs(ui);
                } else {
                    self.show_local_songs(ui);
                    ui.separator();
                    self.show_import(ui);
                }
            });

        // ── 右下角缩放把手 ──
        self.show_resize_grip(ui);
    }
}

