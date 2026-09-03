//! 歌单管理：切换、创建/删除/重命名、添加到其他歌单、在线歌单定位。

use crate::state::{PlaylistKind, QueueItem};

use super::MusicApp;
use crate::app::player::enqueue_dedup;

impl MusicApp {
    /// 切换活跃歌单并重置当前曲目索引（指向原歌单，不再有效）。
    pub(crate) fn switch_active_playlist(&mut self, idx: usize) {
        if idx == self.active_playlist {
            return;
        }
        self.active_playlist = idx;
        self.current_track = None;
        self.search_text.clear();
    }

    /// 把一首歌添加到指定的本地歌单（去重）。
    pub(crate) fn add_song_to_local_playlist(&mut self, item: QueueItem, pl_idx: usize) {
        if let Some(pl) = self.playlists.get_mut(pl_idx) {
            if pl.is_online() {
                return;
            }
            let name = pl.name.clone();
            let (_, added) = enqueue_dedup(&mut pl.songs, item);
            self.queue_dirty = true;
            if added {
                self.notice(format!("已添加到「{name}」"));
            } else {
                self.notice(format!("「{name}」中已存在该歌曲"));
            }
        }
    }

    /// 删除歌单（至少保留一个）。
    pub(crate) fn delete_playlist(&mut self, idx: usize) {
        if self.playlists.len() <= 1 {
            self.notice("至少保留一个歌单");
            return;
        }
        let is_online = self.playlists[idx].is_online();
        let was_active = idx == self.active_playlist;
        let name = self.playlists[idx].name.clone();
        self.playlists.remove(idx);
        if was_active {
            if self.current_track.is_some() {
                self.stop_current();
            }
            self.search_text.clear();
            self.active_playlist = self.active_playlist.min(self.playlists.len().saturating_sub(1));
            if is_online {
                self.fav_selected = None;
                self.fav_items.clear();
                self.fav_page = 0;
                self.fav_total = 0;
                self.fav_has_more = false;
            }
        } else if idx < self.active_playlist {
            self.active_playlist -= 1;
        }
        self.queue_dirty = true;
        self.notice(format!("已删除歌单「{name}」"));
    }

    /// 重命名本地歌单。
    pub(crate) fn rename_playlist(&mut self, idx: usize, new_name: &str) {
        let name = new_name.trim().to_string();
        if name.is_empty() {
            self.notice("歌单名不能为空");
            return;
        }
        if let Some(pl) = self.playlists.get_mut(idx) {
            if pl.is_online() {
                return;
            }
            pl.name = name.clone();
        }
        self.queue_dirty = true;
        self.notice(format!("已重命名为「{name}」"));
        self.renaming_idx = None;
    }

    /// 按 media_id 查找在线歌单的下标。
    pub(crate) fn online_playlist_index(&self, media_id: i64) -> Option<usize> {
        self.playlists.iter().position(|p| match &p.kind {
            PlaylistKind::Online {
                media_id: m, ..
            } => *m == media_id,
            _ => false,
        })
    }
}