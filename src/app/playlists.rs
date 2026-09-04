//! 歌单管理：切换、创建/删除/重命名、添加到其他歌单、在线歌单定位。

use crate::state::{PlaylistKind, QueueItem};

use super::MusicApp;
use crate::app::player::enqueue_dedup;

impl MusicApp {
    /// 切换活跃歌单：当前选中歌单就是播放列表，切走后原「当前曲目」不属于
    /// 新的播放列表，直接停止播放并清掉当前曲目标记。
    pub(crate) fn switch_active_playlist(&mut self, idx: usize) {
        if idx == self.active_playlist {
            return;
        }
        self.active_playlist = idx;
        self.settings.active_playlist = idx;
        if self.current_bvid.is_some() {
            self.stop_current();
        }
        self.search_text.clear();
    }

    /// 当前活跃歌单如果是在线收藏夹，返回其 media_id；否则返回 None。
    pub(crate) fn active_online_media_id(&self) -> Option<i64> {
        match self.playlists.get(self.active_playlist)?.kind {
            PlaylistKind::Online { media_id, .. } => Some(media_id),
            _ => None,
        }
    }

    /// 重启/登录后恢复收藏夹视图：若活跃歌单是在线收藏夹，把 `fav_selected`
    /// 指回该收藏夹（而不是等 `FavFolders` 响应自动选第一个）。
    /// 已登录时顺带拉取该收藏夹的资源；未登录时只保留选中，登录成功后再拉。
    pub(crate) fn restore_favorites_selection(&mut self) {
        if let Some(media_id) = self.active_online_media_id() {
            let changed = self.fav_selected != Some(media_id);
            if changed {
                self.fav_selected = Some(media_id);
                self.fav_items.clear();
                self.fav_page = 0;
                self.fav_total = 0;
                self.fav_has_more = false;
            }
            if changed && self.logged_in() {
                self.fav_loading = false;
                self.spawn_fav_resources(media_id, 1);
            }
        }
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
        // 正在播放的歌如果属于被删的歌单，先停止播放。
        let playing_from_deleted = was_active
            && self
                .playlists[idx]
                .songs
                .iter()
                .any(|s| self.current_bvid.as_deref() == Some(s.bvid.as_str()));
        self.playlists.remove(idx);
        if was_active {
            if playing_from_deleted {
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