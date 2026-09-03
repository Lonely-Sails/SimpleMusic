//! 播放控制：上下曲、seek、音量、移除、键盘快捷键。
//!
//! 也包含 `clamp_seek` / `enqueue_dedup` 两个纯函数（带单测）。

use crate::modules::bilibili::StreamUrl;
use crate::state::{PlayMode, QueueItem};
use crate::util::rand::rand_idx;
use eframe::egui;

use super::MusicApp;

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

/// 进度条拖拽值钳制：时长已知时钳到 `[0, duration]`，未知时只限制下界。
pub fn clamp_seek(value: f64, duration: f64) -> f64 {
    if duration > 0.0 {
        value.clamp(0.0, duration)
    } else {
        value.max(0.0)
    }
}

/// 把条目加入播放列表（按 bvid 去重）：已存在返回其下标，否则追加并返回新下标。
/// 返回 `(index, added)`，`added` 标记是否真的新增。
pub fn enqueue_dedup(songs: &mut Vec<QueueItem>, item: QueueItem) -> (usize, bool) {
    if let Some(i) = songs.iter().position(|q| q.bvid == item.bvid) {
        return (i, false);
    }
    songs.push(item);
    (songs.len() - 1, true)
}

impl MusicApp {
    // ---- 播放控制 ----

    pub(crate) fn play_prepared(&mut self, item: QueueItem, stream: StreamUrl) {
        let songs = self.active_songs_mut();
        let (idx, added) = enqueue_dedup(songs, item.clone());
        if added {
            self.queue_dirty = true;
        }
        self.current_track = Some(idx);
        self.audio.play_stream(&stream, &item.bvid);
        self.state.title = item.title.clone();
        self.state.artist = item.uploader.clone();
        if !item.cover_url.is_empty() {
            self.covers.request(&item.bvid, &item.cover_url);
        }
        self.current_lyrics = None;
        self.lyrics_lines.clear();
        self.lyrics_plain.clear();
        self.lyrics_next_line.clear();
        self.update_lyrics_line();
        self.spawn_lyrics_fetch(item.bvid, item.title, item.uploader);
    }

    pub(crate) fn play_track(&mut self, idx: usize) {
        let songs = self.active_songs();
        if let Some(item) = songs.get(idx).cloned() {
            self.current_track = Some(idx);
            self.spawn_play_resolve(item.bvid);
        }
    }

    pub(crate) fn next_track(&mut self) {
        let songs = self.active_songs();
        if songs.is_empty() {
            return;
        }
        let cur = self.current_track.unwrap_or(0);
        let next = match self.settings.play_mode {
            PlayMode::SingleRepeat => cur,
            PlayMode::Shuffle => {
                let mut r = rand_idx(songs.len());
                if songs.len() > 1 && r == cur {
                    r = (r + 1) % songs.len();
                }
                r
            }
            PlayMode::Sequence => (cur + 1) % songs.len(),
        };
        self.play_track(next);
    }

    pub(crate) fn prev_track(&mut self) {
        let songs = self.active_songs();
        if songs.is_empty() {
            return;
        }
        let cur = self.current_track.unwrap_or(0);
        let prev = match self.settings.play_mode {
            PlayMode::SingleRepeat => cur,
            PlayMode::Shuffle => {
                let mut r = rand_idx(songs.len());
                if songs.len() > 1 && r == cur {
                    r = (r + 1) % songs.len();
                }
                r
            }
            PlayMode::Sequence => {
                if cur == 0 {
                    songs.len() - 1
                } else {
                    cur - 1
                }
            }
        };
        self.play_track(prev);
    }

    pub(crate) fn remove_track(&mut self, idx: usize) {
        if self.active_playlist_is_online() {
            return; // 在线歌单禁止删除
        }
        let len = {
            let songs = self.active_songs_mut();
            if idx >= songs.len() {
                return;
            }
            songs.remove(idx);
            songs.len()
        };
        let was_current = self.current_track == Some(idx);
        self.queue_dirty = true;
        if let Some(ct) = self.current_track {
            if ct > idx {
                self.current_track = Some(ct - 1);
            } else if ct == idx {
                self.current_track = if len == 0 { None } else { Some(idx.min(len - 1)) };
            }
        }
        if was_current {
            self.audio.stop();
            if len > 0 {
                let next = self.current_track.unwrap_or(0);
                self.play_track(next);
            }
        }
    }

    /// 停止当前播放并重置界面状态。
    pub(crate) fn stop_current(&mut self) {
        self.audio.stop();
        self.current_track = None;
        self.state.title = "未在播放".into();
        self.state.artist = "SimpleMusic".into();
        self.state.current_lrc_line.clear();
        self.current_lyrics = None;
        self.lyrics_lines.clear();
        self.lyrics_plain.clear();
        self.lyrics_next_line.clear();
    }

    /// 调整音量并同步到 state/settings/audio。
    pub(crate) fn change_volume(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        self.state.volume = v;
        self.settings.volume = v;
        self.audio.set_volume(v);
    }

    // ---- 键盘快捷键 ----

    /// 全局快捷键（无控件持有键盘焦点时生效，避免与文本输入冲突）：
    /// 空格 播放/暂停；←/→ 快退/快进 5 秒；↑/↓ 音量 ±5%；N/P 下一首/上一首。
    pub(crate) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let free = ctx.memory(|m| m.focused().is_none());
        if !free {
            return;
        }
        let (space, left, right, up, down, n, p) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::N),
                i.key_pressed(egui::Key::P),
            )
        });

        let st = self.audio.status();
        if space && !st.loading {
            if st.playing {
                self.audio.pause();
            } else {
                self.audio.resume();
            }
        }
        const SEEK_STEP: f64 = 5.0;
        if left {
            let dur = self.state.duration_secs;
            self.audio.seek(clamp_seek(self.state.position_secs - SEEK_STEP, dur));
        }
        if right {
            let dur = self.state.duration_secs;
            self.audio.seek(clamp_seek(self.state.position_secs + SEEK_STEP, dur));
        }
        const VOL_STEP: f32 = 0.05;
        if up {
            self.change_volume(self.state.volume + VOL_STEP);
        }
        if down {
            self.change_volume(self.state.volume - VOL_STEP);
        }
        if n {
            self.next_track();
        }
        if p {
            self.prev_track();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::QueueItem;

    #[test]
    fn clamp_seek_bounds_and_unknown_duration() {
        assert_eq!(clamp_seek(-5.0, 100.0), 0.0);
        assert_eq!(clamp_seek(50.0, 100.0), 50.0);
        assert_eq!(clamp_seek(150.0, 100.0), 100.0);
        assert_eq!(clamp_seek(-3.0, 0.0), 0.0);
        assert_eq!(clamp_seek(30.0, 0.0), 30.0);
    }

    #[test]
    fn enqueue_dedup_adds_once_and_returns_index() {
        let mut q = Vec::new();
        let a = QueueItem::new("BV1", "A", "U", 10.0);
        let (i0, added0) = enqueue_dedup(&mut q, a.clone());
        assert_eq!(i0, 0);
        assert!(added0);
        let (i1, added1) = enqueue_dedup(&mut q, a);
        assert_eq!(i1, 0);
        assert!(!added1);
        let b = QueueItem::new("BV2", "B", "U2", 20.0);
        let (i2, added2) = enqueue_dedup(&mut q, b);
        assert_eq!(i2, 1);
        assert!(added2);
        assert_eq!(q.len(), 2);
    }
}