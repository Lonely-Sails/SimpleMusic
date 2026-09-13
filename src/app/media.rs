//! 系统媒体控制接入：把播放态推给系统，并处理系统媒体键/控制中心事件。
//!
//! 与托盘（`window.rs::poll_tray_events`）同款模式：每帧在 `logic` 里轮询。
//! 具体平台实现见 [`crate::media_controls`]（feature `media-control`，未启用时为 no-op 桩）。

use eframe::egui;

use crate::media_controls::{ControlIntent, NowPlaying};

use super::MusicApp;

impl MusicApp {
    /// 按设置开关启停系统媒体控制（设置页勾选变化时调用）。
    ///
    /// 开启时重新注册（下次 `logic` 的 `sync_media_controls` 会立即补推当前曲目）；
    /// 关闭时从系统面板移除并解绑媒体键。
    pub(crate) fn apply_media_control_setting(&mut self, enabled: bool) {
        if enabled {
            self.media.init();
        } else {
            self.media.shutdown();
        }
    }

    /// 当前曲目的封面 URL（按 bvid 在歌单/收藏夹里查；找不到返回空串）。
    ///
    /// 在线歌单的曲目只存在于 `fav_items`，本地歌单在 `playlists[].songs`，
    /// 两处都查一遍才能覆盖所有播放来源。
    fn current_cover_url(&self) -> String {
        let Some(bvid) = self.current_bvid() else {
            return String::new();
        };
        if let Some(item) = self
            .playlists
            .iter()
            .flat_map(|p| p.songs.iter())
            .find(|s| s.bvid == bvid)
        {
            return item.cover_url.clone();
        }
        self.fav_items
            .iter()
            .find(|it| it.bvid == bvid)
            .and_then(|it| it.cover_url.clone())
            .unwrap_or_default()
    }

    /// 把当前播放态同步到系统媒体控制（每帧调用；内部做变更/节流判定）。
    pub(crate) fn sync_media_controls(&mut self) {
        if !self.media.is_enabled() {
            return;
        }
        // 当前曲目：`current_bvid` 为空 = 未在播放任何曲目 → 让系统面板进入停止态。
        let stopped = self.current_bvid.is_none();
        let cover_url = self.current_cover_url();
        let np = NowPlaying {
            title: &self.state.title,
            artist: &self.state.artist,
            cover_url: &cover_url,
            duration_secs: self.state.duration_secs,
            position_secs: self.state.position_secs,
            playing: self.state.playing,
            stopped,
        };
        self.media.sync(&np);
    }

    /// 排空系统媒体事件（上一首/下一首/播放暂停/seek/音量…）。
    ///
    /// 事件由 souvlaki 的回调线程投递到全局通道，这里在主线程逐条应用，
    /// 与项目内其它控制入口（快捷键/按钮）走同一批 `MusicApp` 方法。
    pub(crate) fn poll_media_events(&mut self, ctx: &egui::Context) {
        // 先把事件收进本地 Vec：`drain_events` 借用了 `self.media`，
        // 而处理事件需要 `&mut self`（借用冲突），分两步走。
        let mut intents = Vec::new();
        self.media.drain_events(|intent| intents.push(intent));
        for intent in intents {
            self.apply_media_intent(ctx, intent);
        }
    }

    /// 应用单条系统媒体控制意图。
    fn apply_media_intent(&mut self, ctx: &egui::Context, intent: ControlIntent) {
        match intent {
            ControlIntent::Toggle => {
                let st = self.audio.status();
                if st.playing {
                    self.audio.pause();
                } else {
                    self.audio.resume();
                }
            }
            ControlIntent::Play => self.audio.resume(),
            ControlIntent::Pause => self.audio.pause(),
            ControlIntent::Next => self.next_track(),
            ControlIntent::Prev => self.prev_track(),
            ControlIntent::SeekBy(delta) => {
                let dur = self.state.duration_secs;
                let target = super::player::clamp_seek(self.state.position_secs + delta, dur);
                self.audio.seek(target);
            }
            ControlIntent::SeekTo(pos) => {
                let dur = self.state.duration_secs;
                self.audio.seek(super::player::clamp_seek(pos, dur));
            }
            ControlIntent::Volume(v) => {
                self.change_volume(v as f32);
                // MPRIS 后端要求收到音量事件后回报给系统（其它平台忽略）。
                self.media.report_volume(self.state.volume as f64);
            }
            ControlIntent::Raise => {
                // 显示并聚焦主窗口（与托盘左键单击同款）。
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.window_hidden = false;
            }
            ControlIntent::Quit => {
                self.force_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}
