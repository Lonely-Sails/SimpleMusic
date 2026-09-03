//! 应用层：`MusicApp` 主结构 + eframe 生命周期（`ui` / `logic` / `on_exit`）。
//!
//! 架构约定（沿用旧版 `app.rs` 的线程模型，仅物理拆分）：
//!
//! - **线程模型**：所有 blocking 网络/IO 都丢到后台线程，结果经单个 `mpsc` 通道
//!   （[`messages::AsyncMsg`]）回主线程；每帧在 [`eframe::App::logic`] 里
//!   `try_recv` 排空通道并更新状态。
//! - **播放**：UI 只调用 `AudioEngine` 的 `&mut` 控制命令，进度/错误由轮询 `audio.status()` 同步。
//! - **解析**：导入/收藏点击时后台线程 `BiliClient::video_info + resolve_stream`，回传 `(QueueItem, StreamUrl)`。
//! - **歌词**：切歌时后台线程 `LyricsProvider::fetch`，回传 `Option<Lyrics>`，按 bvid 丢弃过期结果。
//!
//! 文件分工：
//! - `mod.rs`：结构与生命周期、跨模块小工具（`current_item` / `logged_in` / `notice` …）。
//! - `messages.rs`：后台线程消息（`AsyncMsg` + `spawn_*` 派发 + `handle_msg`）。
//! - `player.rs`：播放控制（上下曲/seek/音量/移除）+ 键盘快捷键。
//! - `playlists.rs`：歌单管理（切换/删除/重命名/添加到歌单）。
//! - `lyrics.rs`：歌词同步（当前句/下一句）。
//! - `window.rs`：窗口关闭/隐藏、系统托盘事件轮询。
//! - `ui/`：主界面各区域（标题栏/状态栏/歌单/歌曲列表/导入/播放条/设置/登录/桌面歌词）。

pub mod lyrics;
pub mod messages;
pub mod player;
pub mod playlists;
pub mod ui;
pub mod window;

use crate::cover::CoverCache;
use crate::modules::audio::{AudioEngine, PlaybackStatus};
use crate::modules::bilibili::{BiliClient, FavFolder, FavItem};
use crate::modules::lyrics::{LrcLine, Lyrics};
use crate::modules::storage;
use crate::state::{PlaybackState, Playlist, Settings};
use crate::tray;
use crate::app::ui::toast::{show_toasts, Toast, ToastKind};
use eframe::egui::{self, Pos2};
use messages::AsyncMsg;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
    /// 缓存登录态（是否已登录）。UI 渲染热路径只在启动/登录/登出时更新，
    /// 渲染时读取该缓存即可，绝不在每帧走到 `bili` 互斥锁上——否则后台线程
    /// 做网络解析（网络请求）时会阻塞渲染进程、切歌卡顿。
    login_state: bool,
    /// 登录用户昵称（nav 接口；None/空 = 未知，状态栏回退显示 UID）。
    uname: Option<String>,
    /// 登录用户头像 URL（nav 接口 face；用于状态栏圆形头像）。
    face: Option<String>,
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
    lyrics_candidates: Vec<Lyrics>,
    lyrics_lines: Vec<LrcLine>,
    lyrics_plain: Vec<String>,
    lyrics_next_line: String,
    // 桌面歌词
    lyrics_pos: Option<Pos2>,
    last_pass_through_applied: Option<bool>,
    /// 上次推送给浮窗的内容指纹（当前句/下一句/字号/锁定），用于按需重绘浮窗。
    last_lyrics_frame: Option<(String, String, f32, bool)>,
    // 异步通道
    tx: Sender<AsyncMsg>,
    rx: Receiver<AsyncMsg>,
    // 持久化
    last_save: Option<Instant>,
    last_queue_save: Option<Instant>,
    queue_dirty: bool,
    // 搜索过滤
    search_text: String,
    // 歌单管理
    playlist_mgmt_open: bool,
    renaming_idx: Option<usize>,
    rename_text: String,
    // 顶部 toast（错误/轻提示浮层）
    toasts: Vec<Toast>,
    /// 上次已提示过的音频错误（去重，避免同一错误每帧重复弹 toast）。
    last_err_shown: Option<String>,
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
        // 缓存登录态（每帧渲染读取的字段，避免渲染时锁 bili；网络解析持锁时不再卡 UI）。
        let login_state = bili.logged_in();
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
        // 重启后恢复上次停留的歌单（歌单可能被删/变化，钳制到合法范围）。
        let restored_active = settings
            .active_playlist
            .min(playlists.len().saturating_sub(1));
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
            active_playlist: restored_active,
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
            login_state,
            uname: None,
            face: None,
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
            lyrics_candidates: Vec::new(),
            lyrics_lines: Vec::new(),
            lyrics_plain: Vec::new(),
            lyrics_next_line: String::new(),
            lyrics_pos: None,
            last_pass_through_applied: None,
            last_lyrics_frame: None,
            tx,
            rx,
            last_save: None,
            last_queue_save: None,
            queue_dirty: false,
            search_text: String::new(),
            playlist_mgmt_open: false,
            renaming_idx: None,
            rename_text: String::new(),
            toasts: Vec::new(),
            last_err_shown: None,
            tray,
            window_hidden: false,
            force_quit: false,
        };

    // 恢复过登录态时拉一次用户昵称（后台线程，避免阻塞 UI）。
    if app.logged_in() {
        app.spawn_user_info_fetch();
    }
    // 重启后若上次停留在在线歌单（B 站收藏夹），恢复 fav_selected 指向该收藏夹，
    // 避免收藏夹视图跳回列表中的第一个。
    app.restore_favorites_selection();
    app
}

    // ---- 跨模块小工具 ----

    /// 当前播放曲目（active 歌单中的条目）。
    pub(crate) fn current_item(&self) -> Option<&QueueItem> {
        let pl = self.playlists.get(self.active_playlist)?;
        self.current_track.and_then(|i| pl.songs.get(i))
    }

    /// 是否已登录（读取缓存字段，不锁 `bili`——`bili` 锁会被后台网络解析长时间持有，
    /// 每帧渲染调用的话会阻塞渲染进程、切歌卡顿）。
    pub(crate) fn logged_in(&self) -> bool {
        self.login_state
    }

    /// 头像缓存键（按 mid，切换账号不会互相串图）。
    pub(crate) fn avatar_key(&self) -> String {
        format!("avatar-{}", self.mid.unwrap_or(0))
    }

    pub(crate) fn active_songs_mut(&mut self) -> &mut Vec<QueueItem> {
        &mut self.playlists[self.active_playlist].songs
    }

    pub(crate) fn active_songs(&self) -> &[QueueItem] {
        self.playlists
            .get(self.active_playlist)
            .map(|p| p.songs.as_slice())
            .unwrap_or(&[])
    }

    pub(crate) fn active_playlist_is_online(&self) -> bool {
        self.playlists
            .get(self.active_playlist)
            .map(|p| p.is_online())
            .unwrap_or(false)
    }

    /// 轻提示：顶部弹金色 toast（成功/信息类）。
    pub(crate) fn notice(&mut self, msg: impl Into<String>) {
        self.toasts.push(Toast::new(msg, ToastKind::Notice));
    }

    /// 错误提示：顶部弹暖红色 toast。
    pub(crate) fn error(&mut self, msg: impl Into<String>) {
        self.toasts.push(Toast::new(msg, ToastKind::Error));
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
}

use crate::state::QueueItem;

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

        // 顶部 toast：音频错误按内容去重，避免同一错误每帧重复弹。
        let cur_err = self.audio.status().error.clone();
        if cur_err != self.last_err_shown {
            self.last_err_shown = cur_err.clone();
            if let Some(e) = cur_err {
                self.error(e);
            }
        }
        show_toasts(ui.ctx(), &mut self.toasts);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_shortcuts(ctx);

        // 系统托盘菜单事件轮询（即使窗口隐藏也运行）。
        self.poll_tray_events(ctx);

        // 系统级关闭请求（如 Alt+F4）：托盘可用时改为最小化到托盘。
        if ctx.input(|i| i.viewport().close_requested()) {
            if !self.force_quit && self.tray.is_enabled() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
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

        // 同步当前歌单下标到设置（重启后恢复同一视图）。
        self.settings.active_playlist = self.active_playlist;

        self.covers.poll();
        self.update_lyrics_line();

        // 桌面歌词：内容变化时才唤醒浮窗重绘（deferred 模式不与主窗口互拖）。
        if self.settings.desktop_lyrics_enabled {
            let key = (
                self.state.current_lrc_line.clone(),
                self.lyrics_next_line.clone(),
                self.settings.font_scale,
                self.settings.lyrics_locked,
            );
            if self.last_lyrics_frame.as_ref() != Some(&key) {
                self.last_lyrics_frame = Some(key);
                self.request_lyrics_repaint(ctx);
            }
        } else {
            self.last_lyrics_frame = None;
        }

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

use eframe::glow;
