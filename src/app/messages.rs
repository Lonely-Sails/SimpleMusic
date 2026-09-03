//! 后台线程消息与异步派发。
//!
//! - [`AsyncMsg`]：所有后台线程回主线程的消息类型（登录/收藏/解析/歌词）。
//! - `spawn_*`：在后台 `std::thread` 执行阻塞网络/IO，结果经 `mpsc` 发回主线程。
//! - [`handle_msg`](MusicApp::handle_msg)：主线程每帧排空通道后的消息处理。

use crate::modules::bilibili::{BiliClient, FavFolder, FavItem, QrPoll, StreamUrl};
use crate::modules::lyrics::{self, Lyrics};
use crate::state::{AudioQuality, QueueItem};
use std::sync::atomic::Ordering as AtomicOrdering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::MusicApp;

// ---------------------------------------------------------------------------
// 后台线程回传消息
// ---------------------------------------------------------------------------

pub(crate) enum AsyncMsg {
    /// 登录：二维码已生成（含 bool 矩阵）。
    LoginStarted {
        key: String,
        matrix: Option<Vec<Vec<bool>>>,
    },
    /// 登录：阶段状态文案（请扫描 / 请确认 …）。
    LoginPollStatus(String),
    /// 登录成功（携带 mid）。`gen_id` 是本次登录线程的代号，用于丢弃过期线程的结果。
    LoginSuccess { gen_id: u64, mid: u64 },
    /// 当前登录用户信息（nav 接口取回的昵称；None = 未登录或拉取失败，状态栏回退显示 UID）。
    UserInfo { uname: Option<String> },
    /// 登录失败。`gen_id` 同上。
    LoginFailed { gen_id: u64, msg: String },
    /// 登录轮询线程已因取消而退出。`gen_id` 同上。
    LoginEnded { gen_id: u64 },
    /// 收藏夹文件夹列表。
    FavFolders(Result<Vec<FavFolder>, String>),
    /// 收藏夹资源分页。
    FavResources {
        media_id: i64,
        pn: u32,
        result: Result<(Vec<FavItem>, i64), String>,
    },
    /// 一首歌已解析好（导入或收藏点击），可播。`seq` 是播放请求代号，
    /// 只有最新一次请求的结果会被采纳（防止快速连点时旧结果覆盖新选择）。
    PlayReady {
        seq: u64,
        result: Result<(QueueItem, StreamUrl), String>,
    },
    /// 一首歌的歌词候选已取回（按 bvid 装配，多源候选供「歌词选择」弹窗）。
    LyricsFetched {
        key: String,
        candidates: Vec<Lyrics>,
    },
}

impl MusicApp {
    // ---- 登录派发 ----

    pub(crate) fn spawn_login(&mut self) {
        self.login_visible = true;
        if self.login_running {
            self.login_stop.store(false, AtomicOrdering::Relaxed);
            return;
        }
        self.login_running = true;
        self.login_gen += 1;
        let gen_id = self.login_gen;
        self.login_stop.store(false, AtomicOrdering::Relaxed);
        self.login_qr = None;
        self.login_status.clear();
        let bili = self.bili.clone();
        let tx = self.tx.clone();
        let stop = self.login_stop.clone();
        std::thread::spawn(move || {
            loop {
                if stop.load(AtomicOrdering::Relaxed) {
                    let _ = tx.send(AsyncMsg::LoginEnded { gen_id });
                    return;
                }
                let start = match bili.lock() {
                    Ok(b) => b.generate_qrcode(),
                    Err(e) => {
                        let _ = tx.send(AsyncMsg::LoginFailed {
                            gen_id,
                            msg: format!("客户端锁中毒: {e}"),
                        });
                        return;
                    }
                };
                let start = match start {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = tx.send(AsyncMsg::LoginFailed {
                            gen_id,
                            msg: format!("生成二维码失败: {e}"),
                        });
                        return;
                    }
                };
                let matrix = BiliClient::qrcode_matrix(&start.url).ok();
                let _ = tx.send(AsyncMsg::LoginStarted {
                    key: start.qrcode_key.clone(),
                    matrix,
                });
                loop {
                    std::thread::sleep(Duration::from_secs(2));
                    if stop.load(AtomicOrdering::Relaxed) {
                        let _ = tx.send(AsyncMsg::LoginEnded { gen_id });
                        return;
                    }
                    let poll = match bili.lock() {
                        Ok(mut b) => b.poll_login(&start.qrcode_key),
                        Err(e) => {
                            let _ = tx.send(AsyncMsg::LoginFailed {
                                gen_id,
                                msg: format!("客户端锁中毒: {e}"),
                            });
                            return;
                        }
                    };
                    match poll {
                        Ok(QrPoll::WaitingScan) => {
                            let _ = tx.send(AsyncMsg::LoginPollStatus("请用手机扫描二维码".into()));
                        }
                        Ok(QrPoll::WaitingConfirm) => {
                            let _ = tx.send(AsyncMsg::LoginPollStatus("已扫码，请在手机上确认".into()));
                        }
                        Ok(QrPoll::Expired) => {
                            let _ = tx.send(AsyncMsg::LoginPollStatus("二维码已过期，正在重新生成…".into()));
                            break;
                        }
                        Ok(QrPoll::Success { mid, .. }) => {
                            let _ = tx.send(AsyncMsg::LoginSuccess { gen_id, mid });
                            return;
                        }
                        Err(e) => {
                            let _ = tx.send(AsyncMsg::LoginFailed {
                                gen_id,
                                msg: format!("登录轮询失败: {e}"),
                            });
                            return;
                        }
                    }
                }
            }
        });
    }

    pub(crate) fn cancel_login(&mut self) {
        self.login_stop.store(true, AtomicOrdering::Relaxed);
        self.login_visible = false;
    }

    /// 后台拉取当前登录用户昵称（nav 接口），回传 [`AsyncMsg::UserInfo`]。
    pub(crate) fn spawn_user_info_fetch(&mut self) {
        let bili = self.bili.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let uname = match bili.lock() {
                Ok(b) => b.nav_user().ok().flatten().map(|u| u.uname),
                Err(_) => None,
            };
            let _ = tx.send(AsyncMsg::UserInfo { uname });
        });
    }

    // ---- 收藏夹派发 ----

    pub(crate) fn spawn_fav_folders(&mut self) {
        if self.fav_folders_loading {
            return;
        }
        self.fav_folders_loading = true;
        let bili = self.bili.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = match bili.lock() {
                Ok(b) => b.list_favorite_folders().map_err(|e| e.to_string()),
                Err(e) => Err(format!("客户端锁中毒: {e}")),
            };
            let _ = tx.send(AsyncMsg::FavFolders(result));
        });
    }

    pub(crate) fn spawn_fav_resources(&mut self, media_id: i64, pn: u32) {
        if self.fav_loading {
            return;
        }
        self.fav_loading = true;
        let bili = self.bili.clone();
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let result = match bili.lock() {
                Ok(b) => b.list_favorite_resources(media_id, pn).map_err(|e| e.to_string()),
                Err(e) => Err(format!("客户端锁中毒: {e}")),
            };
            let _ = tx.send(AsyncMsg::FavResources {
                media_id,
                pn,
                result,
            });
        });
    }

    // ---- 播放解析派发 ----

    pub(crate) fn spawn_play_resolve(&mut self, bvid: String) {
        self.play_seq += 1;
        let seq = self.play_seq;
        if self.pending_import {
            self.pending_import = false;
            self.import_seq = None;
        }
        let bili = self.bili.clone();
        let tx = self.tx.clone();
        let quality = self.settings.audio_quality;
        std::thread::spawn(move || {
            let result = resolve_playable(&bili, &bvid, quality);
            let _ = tx.send(AsyncMsg::PlayReady { seq, result });
        });
    }

    pub(crate) fn spawn_import(&mut self, raw: String) {
        self.play_seq += 1;
        let seq = self.play_seq;
        self.pending_import = true;
        self.import_seq = Some(seq);
        // 导入后清空搜索过滤，确保新歌在列表中可见。
        self.search_text.clear();
        let bili = self.bili.clone();
        let tx = self.tx.clone();
        let quality = self.settings.audio_quality;
        std::thread::spawn(move || {
            let result = (|| -> Result<(QueueItem, StreamUrl), String> {
                let guard = bili.lock().map_err(|e| format!("客户端锁中毒: {e}"))?;
                let bvid = guard
                    .parse_bvid(&raw)
                    .ok_or_else(|| "无法识别 BV 号或链接（支持纯 BV / video/BV.. / b23.tv 短链）".to_string())?;
                let detail = guard.video_info(&bvid).map_err(|e| e.to_string())?;
                let stream = guard
                    .resolve_stream_with_cid(&bvid, detail.cid, quality)
                    .map_err(|e| e.to_string())?;
                let item = QueueItem::new_with_cover(
                    bvid,
                    detail.info.title,
                    detail.info.uploader,
                    detail.info.duration_secs,
                    detail.info.cover_url.clone().unwrap_or_default(),
                );
                Ok((item, stream))
            })();
            let _ = tx.send(AsyncMsg::PlayReady { seq, result });
        });
    }

    // ---- 歌词派发 ----

    pub(crate) fn spawn_lyrics_fetch(&self, key: String, title: String, uploader: String) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let candidates = lyrics::LyricsProvider::fetch_all(&title, &uploader);
            let _ = tx.send(AsyncMsg::LyricsFetched { key, candidates });
        });
    }

    // ---- 消息处理 ----

    pub(crate) fn handle_msg(&mut self, msg: AsyncMsg) {
        match msg {
            AsyncMsg::LoginStarted { key, matrix } => {
                self.login_qr = Some((key, matrix.unwrap_or_default()));
                self.login_status = "请用手机扫描二维码".into();
            }
            AsyncMsg::LoginPollStatus(s) => {
                self.login_status = s;
            }
            AsyncMsg::LoginSuccess { gen_id, mid } => {
                if gen_id != self.login_gen {
                    return;
                }
                self.login_running = false;
                self.login_visible = false;
                self.login_stop.store(true, AtomicOrdering::Relaxed);
                self.mid = Some(mid);
                self.uname = None;
                self.login_status = format!("已登录，mid={mid}");
                self.fav_initiated = false;
                // 拉取用户昵称（状态栏显示用）。
                self.spawn_user_info_fetch();
            }
            AsyncMsg::LoginFailed { gen_id, msg } => {
                if gen_id != self.login_gen {
                    return;
                }
                self.login_running = false;
                self.login_visible = false;
                self.login_stop.store(true, AtomicOrdering::Relaxed);
                self.error(format!("登录失败: {msg}"));
            }
            AsyncMsg::UserInfo { uname } => {
                // 退出登录后线程结果才回来时：logged_in 已为 false，丢弃。
                if self.logged_in() {
                    self.uname = uname;
                } else {
                    self.uname = None;
                }
            }
            AsyncMsg::LoginEnded { gen_id } => {
                if gen_id != self.login_gen {
                    return;
                }
                self.login_running = false;
                if self.login_visible {
                    self.spawn_login();
                }
            }
            AsyncMsg::FavFolders(result) => {
                self.fav_folders_loading = false;
                // 先记住当前选中（用户点开的在线歌单），避免刷新文件夹列表时
                // 被重置成第一个——否则重启/重复同步后收藏夹会「变了」。
                let prev_selected = self.fav_selected;
                match result {
                    Ok(folders) => {
                        self.fav_folders = folders;
                        // 之前已选中且该收藏夹仍存在：保留用户选择；若资源还没加载
                        // （例如刚从本地歌单切过来）则补拉一次，但绝不清空当前列表。
                        let keep = prev_selected
                            .filter(|id| self.fav_folders.iter().any(|f| f.id == *id));
                        match keep {
                            Some(id) => {
                                if self.fav_items.is_empty() && !self.fav_loading {
                                    self.spawn_fav_resources(id, 1);
                                }
                            }
                            // 无选中或选中已失效：回退到列表中的第一个收藏夹。
                            None => {
                                if let Some(f) = self.fav_folders.first().cloned() {
                                    self.fav_selected = Some(f.id);
                                    self.fav_items.clear();
                                    self.fav_page = 0;
                                    self.fav_total = 0;
                                    self.fav_has_more = false;
                                    self.spawn_fav_resources(f.id, 1);
                                }
                            }
                        }
                    }
                    Err(e) => self.error(format!("收藏夹加载失败: {e}")),
                }
            }
            AsyncMsg::FavResources {
                media_id,
                pn,
                result,
            } => {
                self.fav_loading = false;
                if self.fav_selected != Some(media_id) {
                    return;
                }
                match result {
                    Ok((items, total)) => {
                        if pn == 1 {
                            self.fav_items.clear();
                        }
                        let page: Vec<FavItem> = items.clone();
                        for it in page {
                            if let Some(u) = it.cover_url {
                                self.covers.request(&it.bvid, &u);
                            }
                        }
                        self.fav_items.extend(items);
                        self.fav_page = pn;
                        self.fav_total = total;
                        self.fav_has_more = (self.fav_items.len() as i64) < total;
                    }
                    Err(e) => self.error(format!("收藏夹资源加载失败: {e}")),
                }
            }
            AsyncMsg::PlayReady { seq, result } => {
                if seq < self.play_seq {
                    return;
                }
                match result {
                    Ok((item, stream)) => {
                        if self.import_seq == Some(seq) {
                            self.import_seq = None;
                            self.pending_import = false;
                            self.import_text.clear();
                        }
                        self.play_prepared(item, stream);
                    }
                    Err(e) => {
                        if self.import_seq == Some(seq) {
                            self.import_seq = None;
                            self.pending_import = false;
                        }
                        self.error(format!("无法播放: {e}"));
                    }
                }
            }
            AsyncMsg::LyricsFetched { key, candidates } => {
                let is_current = self.current_item().map(|i| i.bvid == key).unwrap_or(false);
                if is_current {
                    self.lyrics_candidates = candidates.clone();
                    if let Some(first) = candidates.first() {
                        self.apply_lyrics(first);
                    } else {
                        self.current_lyrics = None;
                        self.lyrics_lines.clear();
                        self.lyrics_plain.clear();
                        self.update_lyrics_line();
                    }
                }
            }
        }
    }
}

/// 后台解析一首歌（bvid → VideoDetail → StreamUrl），返回可播对的 (QueueItem, StreamUrl)。
fn resolve_playable(
    bili: &Arc<Mutex<BiliClient>>,
    bvid: &str,
    quality: AudioQuality,
) -> Result<(QueueItem, StreamUrl), String> {
    let guard = bili.lock().map_err(|e| format!("客户端锁中毒: {e}"))?;
    let detail = guard.video_info(bvid).map_err(|e| e.to_string())?;
    let stream = guard
        .resolve_stream_with_cid(bvid, detail.cid, quality)
        .map_err(|e| e.to_string())?;
    let item = QueueItem::new_with_cover(
        bvid,
        detail.info.title,
        detail.info.uploader,
        detail.info.duration_secs,
        detail.info.cover_url.clone().unwrap_or_default(),
    );
    Ok((item, stream))
}

/// 收藏夹列表刷新后应保留的选中 id：之前选中的收藏夹仍存在则保留，
/// 否则返回 None（由调用方回退到列表第一个）。防止刷新/重启后收藏夹被「换掉」。
fn preserved_folder_id(folders: &[FavFolder], prev_selected: Option<i64>) -> Option<i64> {
    prev_selected.filter(|id| folders.iter().any(|f| f.id == *id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: i64) -> FavFolder {
        FavFolder {
            id,
            title: format!("收藏夹{id}"),
            media_count: 3,
        }
    }

    #[test]
    fn preserved_folder_id_keeps_existing_selection() {
        let folders = vec![folder(1), folder(2), folder(3)];
        assert_eq!(preserved_folder_id(&folders, Some(2)), Some(2));
        assert_eq!(preserved_folder_id(&folders, Some(1)), Some(1));
    }

    #[test]
    fn preserved_folder_id_falls_back_when_selection_vanished() {
        let folders = vec![folder(1), folder(2)];
        assert_eq!(preserved_folder_id(&folders, Some(99)), None);
        assert_eq!(preserved_folder_id(&folders, None), None);
    }
}
