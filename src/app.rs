//! SimpleMusic 主界面与桌面歌词悬浮窗。
//!
//! 架构：所有 blocking 网络/IO 都丢到后台线程，结果经一个 `mpsc` 通道回主线程；
//! 主线程每帧在 [`eframe::App::logic`] 里 `try_recv` 排空通道并更新状态。
//!
//! - 播放：UI 只调用 [`AudioEngine`] 的 `&mut` 控制命令，进度/错误由轮询 `audio.status()` 同步。
//! - 解析：导入/收藏点击时后台线程 `BiliClient::video_info + resolve_stream`，回传 `(QueueItem, StreamUrl)`。
//! - 歌词：切歌时后台线程 `LyricsProvider::fetch`，回传 `Option<Lyrics>`，按 bvid 丢弃过期结果。

use crate::cover::CoverCache;
use crate::modules::audio::{AudioEngine, PlaybackStatus};
use crate::modules::bilibili::{BiliClient, FavFolder, FavItem, StreamUrl};
use crate::modules::lyrics::{self, Lyrics, LrcLine};
use crate::modules::storage;
use crate::state::{
    AudioQuality, PlayMode, Playlist, PlaylistKind, PlaybackState, QueueItem, Settings,
};
use crate::{icons, theme, tray};
use eframe::egui::{
    self, load::SizedTexture, epaint::PathStroke, epaint::StrokeKind, Align2, Color32,
    ComboBox, CornerRadius, FontId, Pos2, Rect, RichText, Sense, Stroke, Vec2, ViewportBuilder,
    ViewportCommand, ViewportId,
};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 桌面歌词悬浮窗固定尺寸。
const LYRICS_VIEWPORT_SIZE: Vec2 = Vec2::new(800.0, 104.0);
/// 二维码渲染的边长（含留白边框）。
const QR_SIZE: f32 = 260.0;
/// 播放条：播放/暂停圆形按钮直径。
const PLAY_BTN_SIZE: f32 = 36.0;
/// 播放条：上一首/下一首圆形按钮直径。
const TRANSPORT_BTN_SIZE: f32 = 30.0;
/// 自定义标题栏高度。
const TITLEBAR_HEIGHT: f32 = 40.0;

/// 桌面歌词 viewport 的稳定 id。
fn lyrics_viewport_id() -> ViewportId {
    ViewportId(egui::Id::new("simple_music_desktop_lyrics"))
}

// ---------------------------------------------------------------------------
// 后台线程回传消息
// ---------------------------------------------------------------------------

enum AsyncMsg {
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
    /// 一首歌的歌词已取回（按 bvid 装配）。
    LyricsFetched {
        key: String,
        lyrics: Option<Lyrics>,
    },
}

// ---------------------------------------------------------------------------
// 纯函数工具（可单测）
// ---------------------------------------------------------------------------

/// 进度条拖拽值钳制：时长已知时钳到 `[0, duration]`，未知时只限制下界。
pub fn clamp_seek(value: f64, duration: f64) -> f64 {
    if duration > 0.0 {
        value.clamp(0.0, duration)
    } else {
        value.max(0.0)
    }
}

/// 无同步歌词时按播放进度近似取行：返回 `plain` 的下标（非空时必在界内）。
pub fn pick_plain_line_index(plain: &[String], progress: f64) -> usize {
    if plain.is_empty() {
        return 0;
    }
    let p = progress.clamp(0.0, 1.0);
    let idx = (p * plain.len() as f64) as usize;
    idx.min(plain.len() - 1)
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

// ---------------------------------------------------------------------------
// 全局应用
// ---------------------------------------------------------------------------

pub struct MusicApp {
    // 引擎与客户端
    audio: AudioEngine,
    bili: Arc<Mutex<BiliClient>>,
    // 封面缓存
    covers: CoverCache,
    // 播放状态
    state: PlaybackState,
    settings: Settings,
    // 播放列表（歌单）
    playlists: Vec<Playlist>,
    active_playlist: usize,
    // 当前播放曲目（在 active 播放列表中的下标）
    current_track: Option<usize>,
    // 寻求拖拽
    seek_dragging: bool,
    seek_preview: f64,
    // 登录
    login_visible: bool,
    login_running: bool,
    login_stop: Arc<AtomicBool>,
    login_gen: u64,
    login_qr: Option<(String, Vec<Vec<bool>>)>,
    login_status: String,
    mid: Option<u64>,
    /// 登录用户昵称（nav 接口；None/空 = 未知，状态栏回退显示 UID）。
    uname: Option<String>,
    // 收藏夹（B站在线歌单用）
    fav_initiated: bool,
    fav_folders: Vec<FavFolder>,
    fav_folders_loading: bool,
    fav_selected: Option<i64>,
    fav_items: Vec<FavItem>,
    fav_page: u32,
    fav_total: i64,
    fav_loading: bool,
    fav_has_more: bool,
    // 导入
    import_text: String,
    pending_import: bool,
    import_seq: Option<u64>,
    play_seq: u64,
    // 在线歌单选择流程
    syncing_online: bool,
    new_playlist_name: String,
    // 设置窗口
    settings_window_open: bool,
    // 歌词
    current_lyrics: Option<Lyrics>,
    lyrics_lines: Vec<LrcLine>,
    lyrics_plain: Vec<String>,
    lyrics_next_line: String,
    // 桌面歌词
    lyrics_pos: Option<Pos2>,
    last_pass_through_applied: Option<bool>,
    // 异步通道
    tx: Sender<AsyncMsg>,
    rx: Receiver<AsyncMsg>,
    // 持久化
    last_save: Option<Instant>,
    last_queue_save: Option<Instant>,
    queue_dirty: bool,
    // 状态栏（红色错误）
    ui_error: Option<String>,
    // 搜索过滤
    search_text: String,
    // 歌单管理
    playlist_mgmt_open: bool,
    renaming_idx: Option<usize>,
    rename_text: String,
    // 轻提示（金色）
    last_notice: Option<(String, Instant)>,
    // 系统托盘（独立 GTK 线程）
    tray: tray::Tray,
    /// 窗口是否因「最小化到托盘」而隐藏（用于托盘菜单切换）。
    window_hidden: bool,
    /// 托盘菜单「退出」触发的强制退出（避免被最小化到托盘逻辑拦截）。
    force_quit: bool,
}

impl MusicApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        settings: Settings,
        mut tray: tray::Tray,
    ) -> Self {
        // macOS/Windows：在主线程（事件循环已运行）创建原生托盘图标；Linux 上是 no-op。
        tray.init_on_main_thread();
        let _ = cc;
        let bili = BiliClient::with_session().unwrap_or_else(|_| {
            BiliClient::new().expect("初始化 BiliClient 失败")
        });
        let mid = bili.mid();
        // 后台补齐 buvid3/buvid4 设备指纹（阻塞网络，不能放 UI 线程）：
        // 部分接口缺 buvid 易被 B 站风控 412；失败静默（smoke 之外这是唯一调用点）。
        let bili = Arc::new(Mutex::new(bili));
        {
            let bili = Arc::clone(&bili);
            std::thread::spawn(move || {
                if let Ok(mut c) = bili.lock() {
                    let _ = c.ensure_buvid();
                }
            });
        }
        let (tx, rx) = mpsc::channel();
        let mut audio = AudioEngine::new();
        // 启动时应用已保存的音量。
        audio.set_volume(settings.volume);
        let playlists = storage::load_playlists();
        let mut covers = CoverCache::new(cc.egui_ctx.clone());
        let mut state = PlaybackState::default();
        state.volume = settings.volume;
        // 启动时预取封面。
        for pl in &playlists {
            for item in &pl.songs {
                if !item.cover_url.is_empty() {
                    covers.request(&item.bvid, &item.cover_url);
                }
            }
        }
        let mut app = Self {
            audio,
            bili,
            covers,
            state,
            settings,
            playlists,
            active_playlist: 0,
            current_track: None,
            seek_dragging: false,
            seek_preview: 0.0,
            login_visible: false,
            login_running: false,
            login_stop: Arc::new(AtomicBool::new(false)),
            login_gen: 0,
            login_qr: None,
            login_status: String::new(),
            mid,
            uname: None,
            fav_initiated: false,
            fav_folders: Vec::new(),
            fav_folders_loading: false,
            fav_selected: None,
            fav_items: Vec::new(),
            fav_page: 0,
            fav_total: 0,
            fav_loading: false,
            fav_has_more: false,
            import_text: String::new(),
            pending_import: false,
            import_seq: None,
            play_seq: 0,
            syncing_online: false,
            new_playlist_name: String::new(),
            settings_window_open: false,
            current_lyrics: None,
            lyrics_lines: Vec::new(),
            lyrics_plain: Vec::new(),
            lyrics_next_line: String::new(),
            lyrics_pos: None,
            last_pass_through_applied: None,
            tx,
            rx,
            last_save: None,
            last_queue_save: None,
            queue_dirty: false,
            ui_error: None,
            search_text: String::new(),
            playlist_mgmt_open: false,
            renaming_idx: None,
            rename_text: String::new(),
            last_notice: None,
            tray,
            window_hidden: false,
            force_quit: false,
        };

    // 恢复过登录态时拉一次用户昵称（后台线程，避免阻塞 UI）。
    if app.logged_in() {
        app.spawn_user_info_fetch();
    }
    app
}

    // ---- 小工具 ----

    fn current_item(&self) -> Option<&QueueItem> {
        let pl = self.playlists.get(self.active_playlist)?;
        self.current_track.and_then(|i| pl.songs.get(i))
    }

    fn logged_in(&self) -> bool {
        self.bili.lock().map(|b| b.logged_in()).unwrap_or(false)
    }

    fn active_songs_mut(&mut self) -> &mut Vec<QueueItem> {
        &mut self.playlists[self.active_playlist].songs
    }

    fn active_songs(&self) -> &[QueueItem] {
        self.playlists
            .get(self.active_playlist)
            .map(|p| p.songs.as_slice())
            .unwrap_or(&[])
    }

    fn active_playlist_is_online(&self) -> bool {
        self.playlists
            .get(self.active_playlist)
            .map(|p| p.is_online())
            .unwrap_or(false)
    }

    // ---- 辅助方法 ----

    /// 设置轻提示（金色，显示约 4 秒）。
    fn notice(&mut self, msg: impl Into<String>) {
        self.last_notice = Some((msg.into(), Instant::now()));
    }

    /// 切换活跃歌单并重置当前曲目索引（指向原歌单，不再有效）。
    fn switch_active_playlist(&mut self, idx: usize) {
        if idx == self.active_playlist {
            return;
        }
        self.active_playlist = idx;
        self.current_track = None;
        self.search_text.clear();
    }

    /// 停止当前播放并重置界面状态。
    fn stop_current(&mut self) {
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

    /// 把一首歌添加到指定的本地歌单（去重）。
    fn add_song_to_local_playlist(&mut self, item: QueueItem, pl_idx: usize) {
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
    fn delete_playlist(&mut self, idx: usize) {
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
    fn rename_playlist(&mut self, idx: usize, new_name: &str) {
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

    /// 调整音量并同步到 state/settings/audio。
    fn change_volume(&mut self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        self.state.volume = v;
        self.settings.volume = v;
        self.audio.set_volume(v);
    }

    // ---- 在线歌单帮助 ----

    fn online_playlist_index(&self, media_id: i64) -> Option<usize> {
        self.playlists.iter().position(|p| match &p.kind {
            PlaylistKind::Online {
                media_id: m, ..
            } => *m == media_id,
            _ => false,
        })
    }

    // ---- 异步派发 ----

    fn spawn_login(&mut self) {
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
                        Ok(crate::modules::bilibili::QrPoll::WaitingScan) => {
                            let _ = tx.send(AsyncMsg::LoginPollStatus("请用手机扫描二维码".into()));
                        }
                        Ok(crate::modules::bilibili::QrPoll::WaitingConfirm) => {
                            let _ = tx.send(AsyncMsg::LoginPollStatus("已扫码，请在手机上确认".into()));
                        }
                        Ok(crate::modules::bilibili::QrPoll::Expired) => {
                            let _ = tx.send(AsyncMsg::LoginPollStatus("二维码已过期，正在重新生成…".into()));
                            break;
                        }
                        Ok(crate::modules::bilibili::QrPoll::Success { mid, .. }) => {
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

    fn cancel_login(&mut self) {
        self.login_stop.store(true, AtomicOrdering::Relaxed);
        self.login_visible = false;
    }

    /// 后台拉取当前登录用户昵称（nav 接口），回传 [`AsyncMsg::UserInfo`]。
    fn spawn_user_info_fetch(&mut self) {
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

    fn spawn_fav_folders(&mut self) {
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

    fn spawn_fav_resources(&mut self, media_id: i64, pn: u32) {
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

    fn spawn_play_resolve(&mut self, bvid: String) {
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

    fn spawn_import(&mut self, raw: String) {
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

    fn spawn_lyrics_fetch(&self, key: String, title: String, uploader: String) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let lyrics = lyrics::LyricsProvider::fetch(&title, &uploader);
            let _ = tx.send(AsyncMsg::LyricsFetched { key, lyrics });
        });
    }

    // ---- 播放控制 ----

    fn play_prepared(&mut self, item: QueueItem, stream: StreamUrl) {
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

    fn play_track(&mut self, idx: usize) {
        let songs = self.active_songs();
        if let Some(item) = songs.get(idx).cloned() {
            self.current_track = Some(idx);
            self.spawn_play_resolve(item.bvid);
        }
    }

    fn next_track(&mut self) {
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

    fn prev_track(&mut self) {
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

    fn remove_track(&mut self, idx: usize) {
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

    // ---- 消息处理 ----

    fn handle_msg(&mut self, msg: AsyncMsg) {
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
                self.ui_error = Some(format!("登录失败: {msg}"));
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
                match result {
                    Ok(folders) => {
                        self.fav_folders = folders;
                        if let Some(f) = self.fav_folders.first().cloned() {
                            self.fav_selected = Some(f.id);
                            self.fav_items.clear();
                            self.fav_page = 0;
                            self.fav_total = 0;
                            self.fav_has_more = false;
                            self.spawn_fav_resources(f.id, 1);
                        }
                    }
                    Err(e) => self.ui_error = Some(format!("收藏夹加载失败: {e}")),
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
                    Err(e) => self.ui_error = Some(format!("收藏夹资源加载失败: {e}")),
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
                        self.ui_error = Some(format!("无法播放: {e}"));
                    }
                }
            }
            AsyncMsg::LyricsFetched { key, lyrics } => {
                let is_current = self.current_item().map(|i| i.bvid == key).unwrap_or(false);
                if is_current {
                    if let Some(li) = lyrics {
                        self.current_lyrics = Some(li.clone());
                        self.lyrics_lines = li.lrc_lines();
                        self.lyrics_plain = li
                            .plain
                            .lines()
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                    } else {
                        self.current_lyrics = None;
                        self.lyrics_lines.clear();
                        self.lyrics_plain.clear();
                    }
                    self.update_lyrics_line();
                }
            }
        }
    }

    // ---- 每帧同步 ----

    fn sync_playback(&mut self, st: &PlaybackStatus) -> bool {
        let mut repaint = false;
        self.state.playing = st.playing;
        self.state.position_secs = st.position_secs;
        if st.duration_secs > 0.0 {
            self.state.duration_secs = st.duration_secs;
        }
        if st.playing || st.loading {
            repaint = true;
        }
        repaint
    }

    fn handle_finished(&mut self, st: &PlaybackStatus) -> bool {
        if st.finished && self.audio.take_finished() {
            self.next_track();
            return true;
        }
        false
    }

    fn update_lyrics_line(&mut self) {
        let pos = self.state.position_secs;
        let dur = self.state.duration_secs;
        let (cur, next) = if !self.lyrics_lines.is_empty() {
            let cur = lyrics::lrc::current_line(&self.lyrics_lines, pos)
                .map(|l| l.text.clone())
                .unwrap_or_default();
            let next = lyrics::lrc::next_line(&self.lyrics_lines, pos)
                .map(|l| l.text.clone())
                .unwrap_or_default();
            (cur, next)
        } else if !self.lyrics_plain.is_empty() {
            let progress = if dur > 0.0 { pos / dur } else { 0.0 };
            let idx = pick_plain_line_index(&self.lyrics_plain, progress);
            let cur = self.lyrics_plain.get(idx).cloned().unwrap_or_default();
            let next = self
                .lyrics_plain
                .get(idx + 1)
                .cloned()
                .unwrap_or_default();
            (cur, next)
        } else {
            (self.state.title.clone(), String::new())
        };
        let prelude = !self.lyrics_lines.is_empty() && self.lyrics_lines[0].time_secs > pos;
        let current_line = if prelude {
            "前奏…".to_string()
        } else {
            cur.clone()
        };
        self.state.current_lrc_line = current_line;
        self.lyrics_next_line = if prelude { cur } else { next };
    }

    // ---- 键盘快捷键 ----

    /// 全局快捷键（无控件持有键盘焦点时生效，避免与文本输入冲突）：
    /// 空格 播放/暂停；←/→ 快退/快进 5 秒；↑/↓ 音量 ±5%；N/P 下一首/上一首。
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
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

    // ---- 主界面 ----

    fn show_main(&mut self, ui: &mut egui::Ui) {
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

        // ── 顶部栏：登录状态 + 设置按钮 ──
        self.show_status_bar(ui);

        // ── 歌单选择栏 ──
        self.show_playlist_selector(ui);

        // ---- 歌单内容 + 导入输入框 ----
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::same(18)))
            .show(ui, |ui| {
                if self.active_playlist_is_online() {
                    self.show_online_songs(ui);
                } else {
                    self.show_local_songs(ui);
                    ui.separator();
                    self.show_import(ui);
                }
            });

        // ── 底部控制区 ──
        self.show_player_bar(ui, &st);

        // ── 右下角缩放把手 ──
        self.show_resize_grip(ui);
    }

    // ---- 自定义标题栏 ----

    fn show_custom_title_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top(egui::Id::new("title_bar"))
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin::ZERO))
            .show(ui, |ui| {
                ui.set_min_height(TITLEBAR_HEIGHT);
                let bar = ui.max_rect();
                // 顶部两角圆角，与卡片衔接
                let corner = CornerRadius {
                    nw: theme::CORNER_XL,
                    ne: theme::CORNER_XL,
                    sw: 0,
                    se: 0,
                };
                ui.painter().rect_filled(bar, corner, theme::TITLEBAR_BG);
                // 底部分隔线
                ui.painter().line_segment(
                    [bar.left_bottom() + Vec2::new(0.0, -0.5), bar.right_bottom() + Vec2::new(0.0, -0.5)],
                    Stroke::new(1.0, theme::BORDER_SOFT),
                );

                // 拖动区域（底层）
                ui.interact(bar, ui.id().with("titlebar_drag"), Sense::drag());

                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    // 音符图标 + 应用名（拖动把手）。
                    let (note_rect, note_resp) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::drag());
                    icons::note(ui.painter(), note_rect, theme::ACCENT);
                    ui.add_space(4.0);
                    let title = egui::Label::new(
                        RichText::new("SimpleMusic").strong().color(theme::TEXT_PRIMARY),
                    )
                    .selectable(false)
                    .sense(Sense::drag());
                    let tr = ui.add(title);
                    if tr.drag_started() || note_resp.drag_started() {
                        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                    }
                    if tr.hovered() || note_resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                    }
                    tr.on_hover_text("拖动移动窗口");

                    // 右侧：窗口控制按钮（外边距 14px，与左侧对称）
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(14.0);
                        // 关闭（最小化到托盘 / 退出）
                        if self.window_ctrl_button(ui, icons::cross, "关闭").clicked() {
                            self.request_close(ui.ctx());
                        }
                        ui.add_space(4.0);
                        // 最小化
                        if self.window_ctrl_button(ui, icons::window_minimize, "最小化").clicked() {
                            ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                        }
                    });
                });
            });
    }

    /// 窗口控制按钮（圆角小方块）。
    fn window_ctrl_button(
        &self,
        ui: &mut egui::Ui,
        icon: fn(&egui::Painter, Rect, Color32),
        tooltip: &str,
    ) -> egui::Response {
        let size = Vec2::splat(24.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
        let bg = if resp.is_pointer_button_down_on() {
            theme::BG_ACTIVE
        } else if resp.hovered() {
            theme::BG_HOVER
        } else {
            Color32::TRANSPARENT
        };
        if bg != Color32::TRANSPARENT {
            ui.painter().rect_filled(rect, CornerRadius::same(theme::CORNER), bg);
        }
        icon(ui.painter(), rect.shrink(4.0), theme::TEXT_SECONDARY);
        resp.on_hover_text(tooltip)
    }

    // ---- 状态栏（登录 + 设置） ----

    fn show_status_bar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top(egui::Id::new("status_bar"))
            .frame(egui::Frame::new().fill(Color32::TRANSPARENT).inner_margin(egui::Margin {
                left: 18,
                right: 16,
                top: 10,
                bottom: 8,
            }))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    // 左：当前播放曲目（简洁）
                    if let Some(item) = self.current_item() {
                        let (note_rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        icons::note(ui.painter(), note_rect, theme::ACCENT);
                        ui.add_space(2.0);
                        let label = truncate_label(ui, &item.title, 200.0);
                        ui.label(RichText::new(label).color(theme::TEXT_PRIMARY).size(12.0));
                    }

                    // 右：登录 + 设置
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(4.0);
                        // 设置按钮
                        if icon_button(ui, 26.0, icons::gear, "设置").clicked()
                        {
                            self.settings_window_open = true;
                        }
                        // 登录状态：优先显示昵称，未知时回退 UID。
                        if self.logged_in() {
                            let label = match self.uname.as_deref() {
                                Some(u) if !u.is_empty() => truncate_label(ui, u, 90.0),
                                _ => {
                                    let mid = self.mid.unwrap_or(0);
                                    format!("UID {mid}")
                                }
                            };
                            ui.label(
                                RichText::new(label)
                                    .color(theme::TEXT_WEAK)
                                    .small(),
                            );
                            if ui
                                .add(theme::small_button("退出"))
                                .on_hover_text("退出登录")
                                .clicked()
                            {
                                if let Ok(mut b) = self.bili.lock() {
                                    let _ = b.logout();
                                }
                                self.mid = None;
                                self.uname = None;
                                self.fav_initiated = false;
                                self.fav_folders.clear();
                                self.fav_items.clear();
                                self.fav_selected = None;
                            }
                        } else {
                            if ui.add(theme::small_button("登录")).clicked() {
                                self.spawn_login();
                            }
                            ui.label(
                                RichText::new("未登录").color(theme::TEXT_WEAK).small(),
                            );
                        }
                    });
                });
            });
    }

    // ---- 右下角缩放把手 ----

    fn show_resize_grip(&mut self, ui: &mut egui::Ui) {
        let size = Vec2::splat(18.0);
        // 固定在窗口实际右下角（不随面板布局偏移）。
        let win_rect = ui.ctx().input(|i| i.viewport().inner_rect);
        let bottom_right = win_rect.map(|r| r.right_bottom()).unwrap_or_else(|| ui.max_rect().right_bottom());
        let rect = Rect::from_min_size(bottom_right - size, size);
        let resp = ui.interact(rect, ui.id().with("resize_grip"), Sense::drag());
        icons::window_resize(ui.painter(), rect, theme::TEXT_WEAK);
        if resp.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(
                egui::ResizeDirection::SouthEast,
            ));
        }
        resp.on_hover_text("调整窗口大小");
    }

    // ---- 关闭/隐藏窗口 ----

    /// 点击关闭按钮：托盘可用时最小化到托盘，否则直接退出。
    fn request_close(&mut self, ctx: &egui::Context) {
        if self.tray.is_enabled() {
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            self.window_hidden = true;
        } else {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    /// 轮询系统托盘事件（窗口隐藏时也会被 `logic` 调用）。
    ///
    /// 交互约定：**左键单击 = 显示/聚焦主窗口**；**右键 = 托盘菜单**（由系统弹出，
    /// 事件经 `MenuEvent` 回来）。Linux/libappindicator 不上报图标点击事件（点击由
    /// 系统面板打开菜单），属平台限制。
    #[cfg(feature = "tray")]
    fn poll_tray_events(&mut self, ctx: &egui::Context) {
        use tray_icon::menu::MenuEvent;
        use tray_icon::TrayIconEvent;

        // 图标点击：左键（松开为准，避免与按下/双击重复触发）直接显示主窗口。
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            let show = match event {
                TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                } => true,
                // 双击（Windows）同样打开主窗口。
                TrayIconEvent::DoubleClick { .. } => true,
                _ => false,
            };
            if show && self.window_hidden {
                ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(ViewportCommand::Focus);
                self.window_hidden = false;
            }
        }

        // 菜单事件
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = &event.id;
            if id == tray::MENU_TOGGLE {
                // 切换窗口可见性
                if self.window_hidden {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    self.window_hidden = false;
                } else {
                    ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                    self.window_hidden = true;
                }
            } else if id == tray::MENU_QUIT {
                self.force_quit = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    /// 无托盘编译时的桩方法。
    #[cfg(not(feature = "tray"))]
    fn poll_tray_events(&mut self, _ctx: &egui::Context) {}

    // ---- 歌单选择器 ----

    fn show_playlist_selector(&mut self, ui: &mut egui::Ui) {
        // 预取歌单选项（避免闭包内对 self 的借冲突）
        let playlist_options: Vec<(usize, String, bool, Option<i64>)> = self
            .playlists
            .iter()
            .enumerate()
            .map(|(i, pl)| {
                let label = pl.name.clone();
                let media_id = match pl.kind {
                    PlaylistKind::Online { media_id, .. } => Some(media_id),
                    _ => None,
                };
                (i, label, pl.is_online(), media_id)
            })
            .collect();
        let current_name = self
            .playlists
            .get(self.active_playlist)
            .map(|p| p.name.as_str())
            .unwrap_or("默认歌单")
            .to_owned();

        egui::Panel::top(egui::Id::new("playlist_bar"))
            .frame(
                egui::Frame::new()
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(egui::Margin {
                        left: 22,
                        right: 20,
                        top: 6,
                        bottom: 6,
                    }),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("歌单").color(theme::TEXT_SECONDARY).small());
                    ui.add_space(6.0);

                    ComboBox::from_id_salt("playlist_selector")
                        .width(200.0)
                        .selected_text(RichText::new(current_name).color(theme::TEXT_PRIMARY))
                        .show_ui(ui, |ui| {
                            for (i, label, is_online, media_id) in &playlist_options {
                                let label = label.as_str();
                                // 在线歌单行：文件夹图标 + 名称
                                let mut picked = false;
                                ui.horizontal(|ui| {
                                    if *is_online {
                                        let (r, _) =
                                            ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                                        icons::folder(ui.painter(), r, theme::TEXT_SECONDARY);
                                        ui.add_space(4.0);
                                    }
                                    picked |= ui
                                        .selectable_value(
                                            &mut self.active_playlist,
                                            *i,
                                            RichText::new(label).color(theme::TEXT_PRIMARY),
                                        )
                                        .changed();
                                });
                                if picked {
                                    // 切换歌单：当前曲目下标（属于原歌单）不再有效。
                                    self.current_track = None;
                                    if *is_online {
                                        if let Some(mid) = media_id {
                                            self.fav_selected = Some(*mid);
                                            self.fav_items.clear();
                                            self.fav_page = 0;
                                            self.fav_total = 0;
                                            self.fav_has_more = false;
                                            self.fav_loading = false;
                                            self.spawn_fav_resources(*mid, 1);
                                        }
                                    }
                                }
                            }
                        });

                    // + 按钮：创建歌单（用 Popup 菜单）
                    let add_button = ui.add(
                        egui::Button::new(RichText::new("+").color(theme::TEXT_PRIMARY))
                            .fill(theme::BG_CARD)
                            .corner_radius(theme::CORNER),
                    );

                    egui::Popup::menu(&add_button).show(|ui| {
                        ui.set_min_width(160.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("创建本地歌单").color(theme::TEXT_PRIMARY),
                                )
                                .fill(theme::BG_CARD)
                                .corner_radius(theme::CORNER),
                            )
                            .clicked()
                        {
                            self.playlists.push(Playlist::local(format!(
                                "新歌单 {}",
                                self.playlists.len() + 1
                            )));
                            let new_idx = self.playlists.len() - 1;
                            self.switch_active_playlist(new_idx);
                            self.queue_dirty = true;
                            ui.close();
                        }
                        if self.logged_in() {
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("同步B站收藏夹").color(theme::TEXT_PRIMARY),
                                    )
                                    .fill(theme::BG_CARD)
                                    .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                self.syncing_online = true;
                                self.spawn_fav_folders();
                                ui.close();
                            }
                        } else {
                            ui.add_enabled(
                                false,
                                egui::Button::new(
                                    RichText::new("同步B站收藏夹（需登录）")
                                        .color(theme::TEXT_WEAK),
                                ),
                            );
                        }
                    });

                    // 管理按钮：重命名 / 删除歌单
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("管理").color(theme::TEXT_SECONDARY),
                            )
                            .fill(theme::BG_CARD)
                            .stroke(Stroke::NONE)
                            .corner_radius(theme::CORNER),
                        )
                        .clicked()
                    {
                        self.playlist_mgmt_open = true;
                    }
                });
            });
    }

    /// 在线歌单文件夹选择弹窗（由 `ui()` 调用）。
    fn show_online_folder_selector(&mut self, ctx: &egui::Context) {
        let mut open = self.syncing_online;
        let mut close_after = false;
        // 预取收藏夹列表（避免闭包内对 self 的借冲突）。
        let folders: Vec<FavFolder> = self.fav_folders.clone();
        let loading = self.fav_folders_loading;

        egui::Window::new("选择B站收藏夹")
            .id(egui::Id::new("online_folder_selector"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                if loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(RichText::new("正在加载收藏夹…").color(theme::TEXT_SECONDARY));
                    });
                    return;
                }
                if folders.is_empty() {
                    ui.label(RichText::new("暂无收藏夹").color(theme::TEXT_WEAK));
                    return;
                }
                ui.label(RichText::new("选择一个收藏夹作为歌单：").color(theme::TEXT_SECONDARY));
                ui.add_space(6.0);
                for f in folders {
                    let mut clicked = false;
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                        icons::folder(ui.painter(), r, theme::TEXT_SECONDARY);
                        ui.add_space(4.0);
                        clicked = ui
                            .add(
                                egui::Button::new(
                                    RichText::new(format!("{} ({})", f.title, f.media_count))
                                        .color(theme::TEXT_PRIMARY),
                                )
                                .fill(theme::BG_CARD)
                                .corner_radius(theme::CORNER),
                            )
                            .clicked();
                    });
                    if clicked
                    {
                        // 检查是否已添加
                        if self.online_playlist_index(f.id).is_none() {
                            self.playlists.push(Playlist {
                                name: f.title.clone(),
                                songs: Vec::new(),
                                kind: PlaylistKind::Online {
                                    media_id: f.id,
                                    folder_title: f.title.clone(),
                                },
                            });
                        }
                        // 切换到该歌单
                        if let Some(idx) = self.online_playlist_index(f.id) {
                            self.switch_active_playlist(idx);
                            self.fav_selected = Some(f.id);
                            self.fav_items.clear();
                            self.fav_page = 0;
                            self.fav_total = 0;
                            self.fav_has_more = false;
                            self.fav_loading = false;
                            self.spawn_fav_resources(f.id, 1);
                        }
                        close_after = true;
                    }
                }
                ui.add_space(6.0);
                if ui
                    .add(egui::Button::new(RichText::new("取消").color(theme::TEXT_SECONDARY)))
                    .clicked()
                {
                    close_after = true;
                }
            });
        if close_after {
            open = false;
        }
        self.syncing_online = open;
    }

    // ---- 歌单管理（重命名 / 删除） ----

    fn show_playlist_manage_window(&mut self, ctx: &egui::Context) {
        let mut open = self.playlist_mgmt_open;
        // 预取歌单快照，避免闭包内对 self 的借冲突。
        let snapshot: Vec<(usize, String, bool, usize)> = self
            .playlists
            .iter()
            .enumerate()
            .map(|(i, p)| (i, p.name.clone(), p.is_online(), p.songs.len()))
            .collect();

        let mut close_after = false;
        egui::Window::new("歌单管理")
            .id(egui::Id::new("playlist_manage_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("本地歌单可重命名；在线歌单可删除（B 站收藏夹不受影响）")
                        .color(theme::TEXT_WEAK)
                        .small(),
                );
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(6.0);

                for (i, name, is_online, count) in &snapshot {
                    let mut do_delete = false;
                    let mut do_rename = false;
                    ui.horizontal(|ui| {
                        if *is_online {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                            icons::folder(ui.painter(), r, theme::TEXT_SECONDARY);
                            ui.add_space(2.0);
                        }
                        ui.label(RichText::new(format!("{name} ({count})")).color(theme::TEXT_PRIMARY));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("删除").color(theme::TEXT_SECONDARY))
                                        .fill(theme::BG_CARD)
                                        .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                do_delete = true;
                            }
                            if !*is_online
                                && ui
                                    .add(
                                        egui::Button::new(RichText::new("重命名").color(theme::TEXT_SECONDARY))
                                            .fill(theme::BG_CARD)
                                            .corner_radius(theme::CORNER),
                                    )
                                    .clicked()
                            {
                                do_rename = true;
                            }
                        });
                    });
                    if do_rename {
                        self.renaming_idx = Some(*i);
                        self.rename_text = name.clone();
                    }
                    if do_delete {
                        self.delete_playlist(*i);
                        // 删除后快照索引已失效，标记关闭让用户重新打开查看最新状态。
                        close_after = true;
                    }
                    if self.renaming_idx == Some(*i) {
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.rename_text)
                                    .desired_width(180.0)
                                    .hint_text("新歌单名"),
                            );
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("确定").color(theme::TEXT_ON_ACCENT))
                                        .fill(theme::ACCENT)
                                        .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                let text = self.rename_text.clone();
                                self.rename_playlist(*i, &text);
                            }
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("取消").color(theme::TEXT_SECONDARY))
                                        .fill(theme::BG_CARD)
                                        .corner_radius(theme::CORNER),
                                )
                                .clicked()
                            {
                                self.renaming_idx = None;
                            }
                        });
                    }
                }
            });
        if close_after {
            open = false;
        }
        if !open {
            // 窗口关闭时清掉未完成的重命名状态，避免下次打开残留。
            self.renaming_idx = None;
        }
        self.playlist_mgmt_open = open;
    }

    // ---- 本地歌单歌曲列表 ----

    fn show_local_songs(&mut self, ui: &mut egui::Ui) {
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
                    let selected = self.current_track == Some(i);
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), row_h),
                        Sense::click(),
                    );
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
                            painter.rect_filled(rect, theme::CORNER, bg);
                        }
                        if selected {
                            painter.rect_filled(
                                Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
                                2.0,
                                theme::ACCENT,
                            );
                        }
                    }
                    // 封面 44×44 圆角
                    let cover_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + 10.0, rect.center().y - 22.0),
                        Vec2::splat(44.0),
                    );
                    self.draw_cover_row(ui, cover_rect, &item.bvid, &item.cover_url);
                    let painter = ui.painter();
                    let text_x = rect.left() + 64.0;
                    let max_w = rect.width() - 100.0;
                    let title = truncate_label(ui, &item.title, max_w);
                    painter.text(
                        Pos2::new(text_x, rect.top() + 10.0),
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
                        Pos2::new(text_x, rect.top() + 32.0),
                        Align2::LEFT_TOP,
                        sub,
                        FontId::proportional(11.0),
                        theme::TEXT_SECONDARY,
                    );
                    // 删除按钮 ×
                    let btn_rect = Rect::from_center_size(
                        Pos2::new(rect.right() - 20.0, rect.center().y),
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
                for (i, _) in actions {
                    self.play_track(i);
                }
                if let Some(i) = remove {
                    self.remove_track(i);
                }
            });
    }

    // ---- 在线歌单（B站收藏夹） ----

    fn show_online_songs(&mut self, ui: &mut egui::Ui) {
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
                    let selected = self
                        .current_item()
                        .map(|c| c.bvid == item.bvid)
                        .unwrap_or(false);
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), row_h),
                        Sense::click(),
                    );
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
                            painter.rect_filled(rect, theme::CORNER, bg);
                        }
                        if selected {
                            painter.rect_filled(
                                Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())),
                                2.0,
                                theme::ACCENT,
                            );
                        }
                    }
                    // 封面 44×44 圆角
                    let cover_rect = Rect::from_min_size(
                        Pos2::new(rect.left() + 10.0, rect.center().y - 22.0),
                        Vec2::splat(44.0),
                    );
                    let cover_url = item.cover_url.as_deref().unwrap_or("");
                    self.draw_cover_row(ui, cover_rect, &item.bvid, cover_url);
                    let painter = ui.painter();
                    let text_x = rect.left() + 64.0;
                    let max_w = rect.width() - 100.0;
                    let title = truncate_label(ui, &item.title, max_w);
                    painter.text(
                        Pos2::new(text_x, rect.top() + 10.0),
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
                        Pos2::new(text_x, rect.top() + 32.0),
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
                if let Some(bvid) = play {
                    self.spawn_play_resolve(bvid);
                }
                if self.fav_has_more {
                    ui.add_space(4.0);
                    if ui.add(theme::primary_button("加载更多")).clicked() {
                        if let Some(id) = self.fav_selected {
                            self.fav_loading = false;
                            self.spawn_fav_resources(id, self.fav_page + 1);
                        }
                    }
                }
            });
    }

    // ---- 导入 ----

    fn show_import(&mut self, ui: &mut egui::Ui) {
        ui.label(
            RichText::new("导入 B 站歌曲")
                .strong()
                .color(theme::TEXT_PRIMARY),
        );
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.import_text)
                    .hint_text("BV 号 / 视频链接 / b23.tv 短链")
                    .desired_width(f32::INFINITY),
            );
            let can_submit = !self.import_text.trim().is_empty();
            if ui
                .add_enabled(can_submit, theme::primary_button("添加并播放"))
                .clicked()
            {
                let raw = self.import_text.trim().to_string();
                self.spawn_import(raw);
            }
            if self.pending_import {
                ui.spinner();
            }
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new("支持：纯 BV 号、www.bilibili.com/video/BV..、b23.tv 短链")
                .color(theme::TEXT_WEAK)
                .small(),
        );
    }

    // ---- 底部控制栏 ----

    fn show_player_bar(&mut self, ui: &mut egui::Ui, st: &PlaybackStatus) {
        egui::Panel::bottom(egui::Id::new("player_bar"))
            .frame(
                egui::Frame::new()
                    .fill(Color32::TRANSPARENT)
                    .inner_margin(egui::Margin {
                        left: 22,
                        right: 22,
                        top: 12,
                        bottom: 14,
                    }),
            )
            .show(ui, |ui| {
                // 第一行：播放控制 + 进度条 + 时间
                ui.horizontal(|ui| {
                    // 桌面歌词 toggle
                    self.lyrics_capsule(ui);
                    ui.add_space(8.0);

                    // 上一首
                    if transport_button(ui, TRANSPORT_BTN_SIZE, icons::prev) {
                        self.prev_track();
                    }
                    // 播放/暂停
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::splat(PLAY_BTN_SIZE),
                        Sense::click(),
                    );
                    let painter = ui.painter();
                    let bg = if resp.is_pointer_button_down_on() {
                        theme::BG_ACTIVE
                    } else if resp.hovered() {
                        theme::BG_HOVER
                    } else {
                        theme::BG_CARD
                    };
                    painter.circle_filled(rect.center(), PLAY_BTN_SIZE * 0.5, bg);
                    let icon_rect = rect.shrink(PLAY_BTN_SIZE * 0.30);
                    if st.loading {
                        spinner_arc(&painter, rect.center(), PLAY_BTN_SIZE * 0.22, theme::TEXT_SECONDARY);
                    } else if st.playing {
                        icons::pause(&painter, icon_rect, theme::TEXT_PRIMARY);
                    } else {
                        icons::play(&painter, icon_rect, theme::TEXT_PRIMARY);
                    }
                    if resp.clicked() && !st.loading {
                        if st.playing {
                            self.audio.pause();
                        } else {
                            self.audio.resume();
                        }
                    }
                    // 下一首
                    if transport_button(ui, TRANSPORT_BTN_SIZE, icons::next) {
                        self.next_track();
                    }

                    ui.add_space(10.0);
                    // 进度条
                    let dur = self.state.duration_secs;
                    let max = if dur > 0.0 { dur } else { 1.0 };
                    let mut val = if self.seek_dragging {
                        self.seek_preview
                    } else {
                        self.state.position_secs
                    };
                    let resp = ui.add(
                        egui::Slider::new(&mut val, 0.0..=max)
                            .show_value(false)
                            .min_decimals(0)
                            .max_decimals(0)
                            .trailing_fill(true),
                    );
                    if resp.drag_started() {
                        self.seek_dragging = true;
                        self.seek_preview = self.state.position_secs;
                    }
                    if self.seek_dragging {
                        self.seek_preview = val.clamp(0.0, max);
                        if resp.drag_stopped() {
                            self.seek_dragging = false;
                            self.audio.seek(clamp_seek(val, dur));
                        }
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} / {}",
                            format_secs(self.state.position_secs),
                            format_secs(self.state.duration_secs)
                        ))
                        .color(theme::TEXT_WEAK)
                        .monospace(),
                    );

                    // 切歌模式选择
                    ui.add_space(8.0);
                    ui.label(RichText::new("切歌模式").color(theme::TEXT_SECONDARY).small());
                    let mode = &mut self.settings.play_mode;
                    let mode_label = mode.label();
                    egui::ComboBox::from_id_salt("play_mode")
                        .width(110.0)
                        .selected_text(RichText::new(mode_label).color(theme::TEXT_PRIMARY))
                        .show_ui(ui, |ui| {
                            for m in PlayMode::ALL {
                                let label = m.label();
                                if ui
                                    .selectable_label(*mode == *m, RichText::new(label).color(theme::TEXT_PRIMARY))
                                    .clicked()
                                {
                                    *mode = *m;
                                }
                            }
                        });
                });

                // 第一行与第二行之间留白
                ui.add_space(10.0);
                // 第二行：封面 + 标题 + 音量
                ui.horizontal(|ui| {
                    if let Some((bvid, cover)) =
                        self.current_item().map(|i| (i.bvid.clone(), i.cover_url.clone()))
                    {
                        let cover_rect = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover()).0;
                        self.draw_cover_row(ui, cover_rect, &bvid, &cover);
                        ui.add_space(8.0);
                    }
                    if self.state.title.is_empty() {
                        ui.label(RichText::new("（未在播放）").color(theme::TEXT_WEAK));
                    } else {
                        let title = truncate_label(ui, &self.state.title, 200.0);
                        let artist = truncate_label(ui, &self.state.artist, 150.0);
                        ui.label(
                            RichText::new(title).color(theme::TEXT_PRIMARY).strong(),
                        );
                        ui.label(
                            RichText::new(format!(" — {artist}")).color(theme::TEXT_SECONDARY),
                        );
                        // 歌曲位置提示
                        if let Some(ct) = self.current_track {
                            let len = self.active_songs().len();
                            if len > 0 && self.current_item().is_some() {
                                ui.label(
                                    RichText::new(format!("　第 {}/{} 首", ct + 1, len))
                                        .color(theme::TEXT_WEAK)
                                        .small(),
                                );
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 加载进度
                        if st.loading {
                            ui.spinner();
                            if let Some(total) = st.total_bytes {
                                if total > 0 {
                                    ui.label(
                                        RichText::new(format!(
                                            "{}/{}",
                                            format_bytes(st.downloaded_bytes),
                                            format_bytes(total)
                                        ))
                                        .color(theme::TEXT_WEAK),
                                    );
                                }
                            }
                            ui.add_space(6.0);
                        }
                        // 音量
                        ui.label(RichText::new("音量").color(theme::TEXT_SECONDARY).small());
                        let mut vol = self.state.volume;
                        if ui
                            .add(
                                egui::Slider::new(&mut vol, 0.0..=1.0)
                                    .show_value(false)
                                    .trailing_fill(true),
                            )
                            .changed()
                        {
                            self.state.volume = vol;
                            self.audio.set_volume(vol);
                            self.settings.volume = vol;
                        }
                    });
                });

                // 错误信息
                let mut err = self.ui_error.clone();
                if let Some(e) = &st.error {
                    err = Some(e.clone());
                }
                if let Some(e) = err {
                    ui.label(RichText::new(e).color(theme::TEXT_ERROR).small());
                }
                // 轻提示（金色，4 秒自动消失）
                let notice = self.last_notice.clone();
                if let Some((msg, at)) = notice {
                    if at.elapsed() < Duration::from_secs(4) {
                        ui.label(RichText::new(msg).color(theme::GOLD).small());
                    } else {
                        self.last_notice = None;
                    }
                }
            });
    }

    // ---- 设置窗口 ----

    fn show_settings_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("设置")
            .id(egui::Id::new("settings_window"))
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut self.settings_window_open)
            .show(ctx, |ui| {
                ui.heading(RichText::new("设置").color(theme::TEXT_PRIMARY).strong());
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);

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
    }

    // ---- 封面绘制 ----

    fn draw_cover_row(&mut self, ui: &mut egui::Ui, cover_rect: Rect, key: &str, url: &str) {
        if !url.is_empty() {
            if let Some(tex) = self.covers.texture(key) {
                ui.put(
                    cover_rect,
                    egui::Image::new(SizedTexture::new(tex, cover_rect.size()))
                        .corner_radius(egui::CornerRadius::same(theme::CORNER)),
                );
                return;
            }
        }
        paint_placeholder_cover(ui.painter(), cover_rect);
    }

    // ---- 桌面歌词胶囊 toggle ----

    fn lyrics_capsule(&mut self, ui: &mut egui::Ui) {
        let on = self.settings.desktop_lyrics_enabled;
        // 用填充色 + 文字色表达状态，不加描边；悬停/按下由主题的 bg 变色反馈。
        let (fill, fg) = if on {
            (theme::ACCENT, theme::TEXT_ON_ACCENT)
        } else {
            (theme::BG_CARD, theme::TEXT_SECONDARY)
        };
        let btn = egui::Button::new(RichText::new("桌面歌词").color(fg))
            .fill(fill)
            .stroke(Stroke::NONE)
            .corner_radius(egui::CornerRadius::same(16))
            .selected(on);
        if ui.add(btn).clicked() {
            self.settings.desktop_lyrics_enabled = !self.settings.desktop_lyrics_enabled;
        }
    }

    // ---- 扫码登录弹窗 ----

    fn show_login_window(&mut self, ctx: &egui::Context) {
        egui::Window::new("扫码登录")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.heading(
                        RichText::new("B 站扫码登录")
                            .color(theme::TEXT_PRIMARY)
                            .strong(),
                    );
                    ui.add_space(10.0);
                    match &self.login_qr {
                        Some((_, matrix)) if !matrix.is_empty() => {
                            draw_qr(ui, matrix, QR_SIZE);
                        }
                        _ => {
                            ui.weak("正在生成二维码…");
                        }
                    }
                    ui.add_space(10.0);
                    let (status, color) = if self.login_status.is_empty() {
                        ("请使用手机 B 站 App 扫码".to_string(), theme::TEXT_WEAK)
                    } else {
                        (self.login_status.clone(), theme::TEXT_SECONDARY)
                    };
                    ui.label(RichText::new(status).color(color));
                    ui.add_space(10.0);
                    if ui
                        .add(egui::Button::new(RichText::new("取消").color(theme::TEXT_SECONDARY)))
                        .clicked()
                    {
                        self.cancel_login();
                    }
                });
            });
    }

    // ---- 桌面歌词悬浮窗 ----

    fn show_lyrics_viewport(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();

        let locked = self.settings.lyrics_locked;
        if self.last_pass_through_applied != Some(locked) {
            ctx.send_viewport_cmd_to(
                lyrics_viewport_id(),
                ViewportCommand::MousePassthrough(locked),
            );
            self.last_pass_through_applied = Some(locked);
        }

        let pos = self.lyrics_pos;
        let mut builder = ViewportBuilder::default()
            .with_title("SimpleMusic 桌面歌词")
            .with_transparent(true)
            .with_has_shadow(false)
            .with_decorations(false)
            .with_always_on_top()
            .with_resizable(false)
            .with_mouse_passthrough(locked)
            .with_inner_size(LYRICS_VIEWPORT_SIZE);
        if let Some(p) = pos {
            builder = builder.with_position(p);
        }

        let current = self.state.current_lrc_line.clone();
        let next = self.lyrics_next_line.clone();
        let scale = self.settings.font_scale;
        let viewport_id = lyrics_viewport_id();

        ctx.show_viewport_immediate(
            viewport_id,
            builder,
            |ui: &mut egui::Ui, _class: egui::ViewportClass| {
                if self.lyrics_pos.is_none() {
                    self.lyrics_pos = ui
                        .ctx()
                        .input(|i| i.viewport().outer_rect.map(|r| r.min));
                }

                let (rect, response) =
                    ui.allocate_exact_size(ui.available_size(), Sense::drag());

                // 默认全透明：只有「解锁 + 鼠标悬浮」时才绘制背景卡片（含外圈柔光），
                // 让歌词无边框地浮在桌面上；锁定（鼠标穿透）时不会触发 hover，永远透明。
                // 悬停提示仅用背景亮度变化，不加描边。
                let show_bg = response.hovered() && !locked;
                if show_bg {
                    for (expand, alpha) in [(6.0, 26), (3.0, 40)] {
                        ui.painter().rect_filled(
                            rect.expand(expand),
                            theme::CORNER,
                            Color32::from_black_alpha(alpha),
                        );
                    }
                    ui.painter().rect_filled(rect, theme::CORNER, theme::LYRIC_BG);
                }

                if !locked && response.drag_started() {
                    ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                }

                if !locked && response.hovered() {
                    let btn_rect = egui::Rect::from_min_size(
                        rect.right_top() - Vec2::new(28.0, 4.0),
                        Vec2::new(24.0, 24.0),
                    );
                    let btn = ui.allocate_rect(btn_rect, Sense::click());
                    let btn_hovered = btn.hovered();
                    ui.painter()
                        .circle_filled(btn_rect.center(), 11.0, theme::BG_ACTIVE);
                    icons::cross(
                        &ui.painter(),
                        btn_rect.shrink(5.0),
                        if btn_hovered {
                            theme::TEXT_PRIMARY
                        } else {
                            theme::TEXT_SECONDARY
                        },
                    );
                    if btn.clicked() {
                        self.settings.desktop_lyrics_enabled = false;
                    }
                }

                let font = FontId::proportional(26.0 * scale);
                let next_font = FontId::proportional(14.0 * scale);
                let max_w = rect.width() - 24.0;
                let current = fit_text(ui.ctx(), &current, &font, max_w);
                let next = fit_text(ui.ctx(), &next, &next_font, max_w);
                let center = rect.center();
                if !current.is_empty() {
                    let cur_center = center + Vec2::new(0.0, -12.0);
                    for (dx, dy) in [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)] {
                        ui.painter().text(
                            cur_center + Vec2::new(dx, dy),
                            Align2::CENTER_CENTER,
                            current.as_str(),
                            font.clone(),
                            Color32::from_black_alpha(120),
                        );
                    }
                    ui.painter().text(
                        cur_center,
                        Align2::CENTER_CENTER,
                        current.as_str(),
                        font,
                        theme::LYRIC_CURRENT,
                    );
                } else {
                    ui.painter().text(
                        center,
                        Align2::CENTER_CENTER,
                        "桌面歌词（等待播放…）",
                        FontId::proportional(18.0),
                        theme::TEXT_SECONDARY,
                    );
                }
                if !next.is_empty() {
                    let next_center = center + Vec2::new(0.0, 26.0);
                    ui.painter().text(
                        next_center,
                        Align2::CENTER_CENTER,
                        next.as_str(),
                        next_font,
                        theme::LYRIC_NEXT,
                    );
                }
            },
        );
    }
}

impl eframe::App for MusicApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.show_main(ui);
        if self.login_visible {
            self.show_login_window(ui.ctx());
        }
        if self.syncing_online {
            self.show_online_folder_selector(ui.ctx());
        }
        if self.playlist_mgmt_open {
            self.show_playlist_manage_window(ui.ctx());
        }
        if self.settings.desktop_lyrics_enabled {
            self.show_lyrics_viewport(ui);
        }
        if self.settings_window_open {
            self.show_settings_window(ui.ctx());
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(ctx);

        // 系统托盘菜单事件轮询（即使窗口隐藏也运行）。
        self.poll_tray_events(ctx);

        // 系统级关闭请求（如 Alt+F4）：托盘可用时改为最小化到托盘。
        if ctx.input(|i| i.viewport().close_requested()) {
            if !self.force_quit && self.tray.is_enabled() {
                ctx.send_viewport_cmd(ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(ViewportCommand::Visible(false));
                self.window_hidden = true;
            }
            self.force_quit = false;
        }

        let st = self.audio.status();
        let mut repaint = self.sync_playback(&st);
        repaint |= self.handle_finished(&st);

        let mut repaint_msg = false;
        while let Ok(msg) = self.rx.try_recv() {
            self.handle_msg(msg);
            repaint_msg = true;
        }

        // 收藏夹自动初始化（登录后）。
        if self.logged_in() && !self.fav_initiated {
            self.fav_initiated = true;
            self.spawn_fav_folders();
        }

        self.covers.poll();
        self.update_lyrics_line();

        // 持久化
        let now = Instant::now();
        let should_save_settings = match self.last_save {
            Some(t) => now.duration_since(t).as_secs_f64() >= 5.0,
            None => true,
        };
        if should_save_settings {
            self.last_save = Some(now);
            let _ = self.settings.save();
        }
        if self.queue_dirty {
            let should_save_queue = match self.last_queue_save {
                Some(t) => now.duration_since(t).as_secs_f64() >= 2.0,
                None => true,
            };
            if should_save_queue {
                self.last_queue_save = Some(now);
                self.queue_dirty = false;
                let _ = storage::save_playlists(&self.playlists);
            }
        }

        // 窗口隐藏时保持事件循环活跃（托盘菜单可唤醒窗口）。
        if self.window_hidden {
            ctx.request_repaint();
        }

        if repaint || repaint_msg || self.state.playing || st.loading {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        let _ = self.settings.save();
        if self.queue_dirty {
            let _ = storage::save_playlists(&self.playlists);
        }
        self.tray.stop();
        eprintln!("[app] 托盘已关闭，应用退出。");
    }
}

// ---------------------------------------------------------------------------
// 模块级工具函数
// ---------------------------------------------------------------------------

fn paint_placeholder_cover(painter: &egui::Painter, rect: Rect) {
    painter.rect_filled(rect, theme::CORNER, theme::BG_TRACK);
    let c = rect.center();
    let r = (rect.width() * 0.14).max(2.0);
    let dot = Pos2::new(c.x - r * 0.6, c.y + r * 0.9);
    painter.circle_filled(dot, r, theme::TEXT_WEAK);
    let stroke = Stroke::new((r * 0.30).max(1.5), theme::TEXT_WEAK);
    let stem_x = dot.x + r;
    let stem_top = Pos2::new(stem_x, c.y - r * 1.2);
    painter.line_segment(
        [Pos2::new(stem_x, dot.y), stem_top],
        stroke,
    );
    painter.line_segment(
        [stem_top, Pos2::new(stem_top.x + r * 1.5, stem_top.y + r * 0.8)],
        stroke,
    );
}

fn draw_qr(ui: &mut egui::Ui, matrix: &[Vec<bool>], size: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter().rect_filled(rect, 4.0, Color32::WHITE);
    let rows = matrix.len();
    if rows == 0 {
        return;
    }
    let cols = matrix[0].len();
    if cols == 0 {
        return;
    }
    let quiet = 4.0;
    let inner = rect.shrink(quiet);
    let cell = inner.width().min(inner.height()) / cols.max(rows) as f32;
    let qr_w = cell * cols as f32;
    let qr_h = cell * rows as f32;
    let org = inner.center() - Vec2::new(qr_w / 2.0, qr_h / 2.0);
    let dark = Color32::from_rgb(25, 25, 25);
    for (r, row) in matrix.iter().enumerate() {
        for (c, &is_dark) in row.iter().enumerate() {
            if is_dark {
                let min = org + Vec2::new(c as f32 * cell, r as f32 * cell);
                let cell_rect = Rect::from_min_size(min, Vec2::new(cell + 0.4, cell + 0.4));
                ui.painter().rect_filled(cell_rect, 0.0, dark);
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

/// 歌曲标题/UP 主匹配查询（不区分大小写）。
fn song_matches_query(title: &str, uploader: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    title.to_lowercase().contains(&query)
        || uploader.to_lowercase().contains(&query)
}

fn fit_text(ctx: &egui::Context, text: &str, font: &FontId, max_width: f32) -> String {
    if text.is_empty() {
        return String::new();
    }
    let width_of = |s: &str| {
        ctx.fonts_mut(|f| f.layout_no_wrap(s.to_owned(), font.clone(), Color32::WHITE))
            .size()
            .x
    };
    if width_of(text) <= max_width {
        return text.to_owned();
    }
    const ELLIPSIS: &str = "…";
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let cand: String = chars.iter().collect::<String>() + ELLIPSIS;
        if width_of(&cand) <= max_width {
            return cand;
        }
    }
    ELLIPSIS.to_string()
}

fn truncate_label(ui: &egui::Ui, text: &str, max_width: f32) -> String {
    if max_width <= 0.0 {
        return text.to_owned();
    }
    if ui
        .ctx()
        .fonts_mut(|f| f.layout_no_wrap(text.to_owned(), FontId::proportional(13.0), Color32::WHITE))
        .size()
        .x
        <= max_width
    {
        return text.to_owned();
    }
    fit_text(ui.ctx(), text, &FontId::proportional(13.0), max_width)
}

fn format_secs(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

fn format_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// 简单随机数（Xorshift，不用 rand crate）。
fn rand_idx(max: usize) -> usize {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut x = seed.wrapping_add(0x9e3779b97f4a7c15);
    // Xorshift
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    (x as usize) % max
}

// ---------------------------------------------------------------------------
// 播放条圆形按钮与加载转圈
// ---------------------------------------------------------------------------

fn transport_button(
    ui: &mut egui::Ui,
    size: f32,
    icon: fn(&egui::Painter, egui::Rect, egui::Color32),
) -> bool {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let bg = if resp.is_pointer_button_down_on() {
        theme::BG_ACTIVE
    } else if resp.hovered() {
        theme::BG_HOVER
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter();
    if bg != Color32::TRANSPARENT {
        painter.circle_filled(rect.center(), size * 0.5, bg);
    }
    let icon_rect = rect.shrink(size * 0.30);
    icon(&painter, icon_rect, theme::TEXT_PRIMARY);
    resp.clicked()
}

/// 通用图标小按钮（卡片底、圆角、hover 仅变色不加描边）。
fn icon_button(
    ui: &mut egui::Ui,
    size: f32,
    icon: fn(&egui::Painter, egui::Rect, egui::Color32),
    tooltip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    let bg = if resp.is_pointer_button_down_on() {
        theme::BG_ACTIVE
    } else if resp.hovered() {
        theme::BG_HOVER
    } else {
        theme::BG_CARD
    };
    let painter = ui.painter();
    painter.rect_filled(rect, theme::CORNER, bg);
    icon(&painter, rect.shrink(size * 0.24), theme::TEXT_SECONDARY);
    resp.on_hover_text(tooltip)
}

fn spinner_arc(painter: &egui::Painter, center: Pos2, radius: f32, color: Color32) {
    use std::f32::consts::TAU;
    let points: Vec<Pos2> = (0..=10)
        .map(|i| {
            let t = (i as f32 / 10.0) * TAU * 0.75;
            center + Vec2::angled(t) * radius
        })
        .collect();
    painter.line(points, PathStroke::new(2.0, color));
}

use eframe::glow;

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_seek_bounds_and_unknown_duration() {
        assert_eq!(clamp_seek(-5.0, 100.0), 0.0);
        assert_eq!(clamp_seek(50.0, 100.0), 50.0);
        assert_eq!(clamp_seek(150.0, 100.0), 100.0);
        assert_eq!(clamp_seek(-3.0, 0.0), 0.0);
        assert_eq!(clamp_seek(30.0, 0.0), 30.0);
    }

    #[test]
    fn pick_plain_line_index_clamped() {
        let plain = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(pick_plain_line_index(&plain, 0.0), 0);
        assert_eq!(pick_plain_line_index(&plain, 0.9), 2);
        assert_eq!(pick_plain_line_index(&plain, 1.5), 2);
        assert_eq!(pick_plain_line_index(&plain, -1.0), 0);
        assert_eq!(pick_plain_line_index(&[], 0.5), 0);
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

    #[test]
    fn format_secs_pads_and_format_bytes() {
        assert_eq!(format_secs(0.0), "00:00");
        assert_eq!(format_secs(65.4), "01:05");
        assert_eq!(format_secs(3630.0), "60:30");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn song_matches_query_case_insensitive() {
        // 标题命中
        assert!(song_matches_query("晴天", "周杰伦", "晴"));
        assert!(song_matches_query("晴天", "周杰伦", "晴天"));
        // UP 主命中
        assert!(song_matches_query("晴天", "周杰伦", "杰伦"));
        // 大小写不敏感（英文）
        assert!(song_matches_query("Hello World", "Someone", "hello"));
        assert!(song_matches_query("Hello World", "Someone", "WORLD"));
        // 空查询恒匹配
        assert!(song_matches_query("任何", "标题", ""));
        // 不匹配
        assert!(!song_matches_query("晴天", "周杰伦", "阴天"));
        assert!(!song_matches_query("A", "B", "C"));
    }
}