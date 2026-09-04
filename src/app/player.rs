//! 播放控制：上下曲、seek、音量、移除、键盘快捷键。
//!
//! 也包含 `clamp_seek` / `enqueue_dedup` 两个纯函数（带单测）。
//!
//! 播放列表语义：**当前选中的歌单就是播放列表**，没有独立的播放队列，也
//! 不在播放时把歌隐式写进歌单。上下曲/随机/曲终自动切歌都直接遍历选中
//! 歌单的内容：本地歌单取其 `songs`，在线歌单取已加载的收藏夹条目。

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

/// 把条目加入歌单（按 bvid 去重）：已存在返回其下标，否则追加并返回新下标。
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

    /// 播放列表 = 当前选中歌单的内容（只读快照，按需构建）：
    /// 本地歌单取其歌曲列表；在线歌单取已加载的收藏夹条目。
    pub(crate) fn playback_songs(&self) -> Vec<QueueItem> {
        if self.active_playlist_is_online() {
            self.fav_items
                .iter()
                .map(|it| {
                    QueueItem::new_with_cover(
                        it.bvid.clone(),
                        it.title.clone(),
                        it.owner.clone(),
                        it.duration_secs,
                        it.cover_url.clone().unwrap_or_default(),
                    )
                })
                .collect()
        } else {
            self.active_songs().to_vec()
        }
    }

    /// 点播一首歌（按 bvid）：先标记当前曲目（列表立即高亮），再后台解析播放流。
    pub(crate) fn play_bvid(&mut self, bvid: String) {
        self.current_bvid = Some(bvid.clone());
        self.spawn_play_resolve(bvid);
    }

    /// 播放播放列表中第 `idx` 首（下标相对于 [`Self::playback_songs`]）。
    pub(crate) fn play_track(&mut self, idx: usize) {
        let songs = self.playback_songs();
        if let Some(item) = songs.get(idx) {
            let bvid = item.bvid.clone();
            self.play_bvid(bvid);
        }
    }

    /// 播放已解析好的歌：只更新当前曲目标记与界面状态，**绝不写进歌单**。
    pub(crate) fn play_prepared(&mut self, item: QueueItem, stream: StreamUrl) {
        self.current_bvid = Some(item.bvid.clone());
        self.audio.play_stream(&stream, &item.bvid);
        self.state.title = item.title.clone();
        self.state.artist = item.uploader.clone();
        // 用歌单条目已知时长作为进度条区间兜底。B 站 fMP4 音频流常读不出容器
        // 时长，音频引擎会报 duration_secs=0；若不兜底，进度条 max 退化为 1，
        // 位置一旦超过 1s 就锁死在全满、且只能在 [0,1] 内拖动。
        if item.duration_secs > 0.0 {
            self.state.duration_secs = item.duration_secs;
        }
        if !item.cover_url.is_empty() {
            self.covers.request(&item.bvid, &item.cover_url);
        }
        self.current_lyrics = None;
        self.lyrics_candidates.clear();
        self.lyrics_lines.clear();
        self.lyrics_plain.clear();
        self.lyrics_next_line.clear();
        self.update_lyrics_line();
        self.spawn_lyrics_fetch(
            item.bvid.clone(),
            item.title.clone(),
            item.uploader.clone(),
            item.duration_secs,
            item.cid,
        );
    }

    /// 当前曲目在播放列表中的位置（按 bvid 定位；不在此列表时为 None）。
    fn current_position_in(&self, songs: &[QueueItem]) -> Option<usize> {
        self.current_bvid
            .as_deref()
            .and_then(|b| songs.iter().position(|s| s.bvid == b))
    }

    pub(crate) fn next_track(&mut self) {
        let songs = self.playback_songs();
        if songs.is_empty() {
            return;
        }
        let cur = self.current_position_in(&songs).unwrap_or(0);
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
        let songs = self.playback_songs();
        if songs.is_empty() {
            return;
        }
        let cur = self.current_position_in(&songs).unwrap_or(0);
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
        let removed_bvid = {
            let songs = self.active_songs_mut();
            if idx >= songs.len() {
                return;
            }
            songs.remove(idx).bvid
        };
        self.queue_dirty = true;
        // 当前曲目按 bvid 记忆：删其他歌不影响正在播的；正在播的被删则停止。
        let was_current = self.current_bvid.as_deref() == Some(removed_bvid.as_str());
        if was_current {
            self.stop_current();
        }
    }

    /// 停止当前播放并重置界面状态。
    pub(crate) fn stop_current(&mut self) {
        self.audio.stop();
        self.current_bvid = None;
        self.state.title = "未在播放".into();
        self.state.artist = "SimpleMusic".into();
        self.state.current_lrc_line.clear();
        self.current_lyrics = None;
        self.lyrics_candidates.clear();
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
