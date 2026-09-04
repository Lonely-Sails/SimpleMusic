//! 设置窗口：界面字体 / 音质偏好 / 桌面歌词 / 播放音量。

use crate::fonts::SystemFont;
use crate::state::{AudioQuality, UiFont};
use crate::theme;
use eframe::egui::{self, Align2, RichText};
use std::path::Path;
use super::MusicApp;

impl MusicApp {
    pub(crate) fn show_settings_window(&mut self, ctx: &egui::Context) {
        // open 标志提为局部变量：ui_font_picker 走 &mut self 方法调用，闭包需要
        // 整个 *self 的独占借用，与 Window 持有的 &mut self.settings_window_open 冲突。
        let mut open = self.settings_window_open;
        egui::Window::new("设置")
            .id(egui::Id::new("settings_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.heading(RichText::new("设置").color(theme::TEXT_PRIMARY).strong());
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

                // ── 界面字体 ──
                self.ui_font_picker(ui, ctx);
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);

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
        // 用户点关闭按钮时 open 变 false —— 写回。
        self.settings_window_open = open;
    }

    /// 「界面字体」选择器：自动 / 内嵌 Noto / 系统字体列表（带过滤），选择即时生效。
    ///
    /// 字体候选列表由后台线程扫描（首次展开时触发，回填 `font_list`）；
    /// `Specific` 选中项持久化绝对路径，重启自动恢复；文件失效时启动/选择
    /// 均回退自动并提示。
    fn ui_font_picker(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.label(
            RichText::new("界面字体")
                .color(theme::TEXT_SECONDARY)
                .strong(),
        );

        // 两个内置选项：自动 / 强制内嵌。
        for (variant, label, hint) in [
            (
                UiFont::Auto,
                "自动（系统优先）",
                "探测系统 UI 字体，失败回退内嵌 Noto",
            ),
            (UiFont::Embedded, "内嵌 Noto Sans SC", "跨机器观感一致"),
        ] {
            // 悬停说明：radio 的 Response 上挂 tooltip（egui 0.36 惯用 API）。
            let radio = ui.radio(
                self.settings.ui_font == variant,
                RichText::new(label).color(theme::TEXT_PRIMARY),
            );
            if radio.clicked() && self.settings.ui_font != variant {
                self.apply_font_setting(ctx, &variant);
                self.settings.ui_font = variant;
            }
            radio.on_hover_text(hint);
        }

        // 自定义：从系统字体列表里挑。
        let specific_active = matches!(self.settings.ui_font, UiFont::Specific(_));
        if ui
            .radio(
                specific_active,
                RichText::new("自定义…").color(theme::TEXT_PRIMARY),
            )
            .clicked()
        {
            // 触发后台扫描（幂等）；已有结果时直接展开列表。
            self.spawn_font_scan();
        }

        if specific_active {
            // 当前选中文件的回显（可能已失效——失效时启动已回退自动，这里仅显示）。
            if let Some(path) = self.settings.ui_font.path() {
                ui.label(
                    RichText::new(format!("当前: {}", short_path(path)))
                        .color(theme::TEXT_WEAK)
                        .small(),
                );
            }

            ui.horizontal(|ui| {
                ui.label(RichText::new("过滤").color(theme::TEXT_SECONDARY).small());
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.font_filter)
                        .desired_width(200.0)
                        .hint_text("输入关键字筛选…"),
                );
                if resp.changed() {
                    resp.request_focus();
                }
                if self.font_scanning {
                    ui.label(
                        RichText::new("正在扫描系统字体…")
                            .color(theme::TEXT_WEAK)
                            .small(),
                    );
                } else if self.font_list.is_empty() && self.font_scan_started {
                    if ui
                        .button(RichText::new("重新扫描").small())
                        .clicked()
                    {
                        self.font_scan_started = false;
                        self.spawn_font_scan();
                    }
                }
            });

            // 过滤后的候选列表（滚动区，高度受限防止撑爆设置窗）。
            let filter = self.font_filter.to_lowercase();
            let candidates: Vec<SystemFont> = self
                .font_list
                .iter()
                .filter(|f| filter.is_empty() || f.family.to_lowercase().contains(&filter))
                .cloned()
                .take(200)
                .collect();
            if !self.font_scanning && !candidates.is_empty() {
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        for f in &candidates {
                            let selected = self
                                .settings
                                .ui_font
                                .path()
                                .map(|p| Path::new(p) == f.path)
                                .unwrap_or(false);
                            let label = RichText::new(&f.family).color(if selected {
                                theme::ACCENT
                            } else {
                                theme::TEXT_PRIMARY
                            });
                            if ui.radio(selected, label).clicked() {
                                let new_font =
                                    UiFont::Specific(f.path.display().to_string());
                                // 即时生效；失败（文件刚被删等）时复位成自动。
                                if self.apply_font_setting(ctx, &new_font) {
                                    self.settings.ui_font = new_font;
                                } else {
                                    self.settings.ui_font = UiFont::Auto;
                                }
                            }
                        }
                    });
                ui.label(
                    RichText::new("选择后立即生效；含汉字由内嵌 Noto 自动兜底")
                        .color(theme::TEXT_WEAK)
                        .small(),
                );
            } else if !self.font_scanning && self.font_scan_started && candidates.is_empty() {
                ui.label(
                    RichText::new("没有匹配的字体")
                        .color(theme::TEXT_WEAK)
                        .small(),
                );
            }
        }
    }
}

/// 路径缩短显示：只保留文件名。
fn short_path(p: &str) -> String {
    Path::new(p)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(p)
        .to_owned()
}