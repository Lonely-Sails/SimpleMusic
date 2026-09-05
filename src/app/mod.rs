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
//! - `mod.rs`：结构与生命周期、跨模块小工具（`current_bvid` / `logged_in` / `notice` …）。
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
use crate::modules::lyrics::{LrcLine, Lyrics, LyricsCacheEntry};
use crate::modules::storage;
use crate::state::{PlaybackState, Playlist, QueueItem, Settings, LyricsFont};
use crate::tray;
use crate::app::ui::settings::SettingsTab;
use crate::app::ui::toast::{show_toasts, Toast, ToastKind};
use eframe::egui;
use messages::AsyncMsg;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 播放中主窗口自醒重绘的间隔（秒）。进度条按此节流刷新；约 5Hz 对人眼而言
/// 已是平滑的进度推进，同时大幅降低播放期间主窗口的渲染占用。
const PLAY_REPAINT_INTERVAL: f32 = 0.2;
/// 主窗口比歌词切换点提前醒来的秒数（缓冲）：保证切行帧落在切换点之前，
/// 过渡动画从正确的时刻开始。
const LYRICS_SWITCH_WAKE_EARLY: f64 = 0.02;

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
    // 当前播放曲目的 bvid（播放列表 = 当前选中歌单的内容，不再有独立队列）。
    current_bvid: Option<String>,
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
    // 设置窗口
    settings_window_open: bool,
    // 歌词
    current_lyrics: Option<Lyrics>,
    lyrics_candidates: Vec<Lyrics>,
    lyrics_lines: Vec<LrcLine>,
    lyrics_plain: Vec<String>,
    lyrics_next_line: String,
    /// 歌词本地缓存（按 bvid 的 md5 键控）：上次生效歌词 + 全部候选 + 手动选择。
    /// 启动时从 `~/.config/simple-music/lyrics.json` 加载；歌词线程查命中/写抓取
    /// 结果、UI 线程写用户手选，落盘统一在后台线程（临时表快照）。
    lyrics_cache: Arc<Mutex<BTreeMap<String, LyricsCacheEntry>>>,
    // 桌面歌词（浮窗位置存 settings.lyrics_pos，见 ui/lyrics_viewport.rs）
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
    /// 上一帧是否处于最小化（用于恢复时补发重绘，见 `logic` 里的恢复逻辑）。
    was_minimized: bool,
    /// 上次「播放中节流重绘」的时刻：播放时主窗口按 [`PLAY_REPAINT_INTERVAL`]
    /// 低频自醒（进度条同步），其余时间不连续重绘——把渲染线程让给桌面歌词
    /// 浮窗的过渡动画。
    last_playing_repaint: Option<Instant>,
    /// 重绘保活线程的退出标志（`on_exit` 置位；进程正常退出前兜底）。
    keepalive_stop: Arc<AtomicBool>,
    // 搜索过滤
    search_text: String,
    // 字体选择（设置页）：候选列表（后台扫描回填）/ 扫描状态 / 选择框过滤词 /
    // 当前选中的导航页。
    font_list: Vec<crate::fonts::SystemFont>,
    font_scan_started: bool,
    font_scanning: bool,
    font_filter: String,
    settings_tab: SettingsTab,
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
        // 清掉历史版本「点播即隐式加入在线歌单」行为残留的脏数据：
        // 在线歌单只是收藏夹引用，歌曲列表永远由 B 站接口拉取，不应有本地积累。
        let mut playlists = playlists;
        for pl in playlists.iter_mut() {
            if pl.is_online() && !pl.songs.is_empty() {
                pl.songs.clear();
            }
        }
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
            current_bvid: None,
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
            settings_window_open: false,
            current_lyrics: None,
            lyrics_candidates: Vec::new(),
            lyrics_lines: Vec::new(),
            lyrics_plain: Vec::new(),
            lyrics_next_line: String::new(),
            lyrics_cache: Arc::new(Mutex::new(storage::load_lyrics_cache())),
            last_pass_through_applied: None,
            last_lyrics_frame: None,
            tx,
            rx,
            last_save: None,
            last_queue_save: None,
            queue_dirty: false,
            was_minimized: false,
            last_playing_repaint: None,
            keepalive_stop: Arc::new(AtomicBool::new(false)),
            search_text: String::new(),
            font_list: Vec::new(),
            font_scan_started: false,
            font_scanning: false,
            font_filter: String::new(),
            settings_tab: SettingsTab::default(),
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

    // 重绘保活线程：规避 eframe/winit 最小化后事件循环「饿死」的已知缺陷
    // （egui #8246：最小化再恢复后 UI 永久冻结；egui #5136 / PR #8414：合成器对
    // 不可见/最小化窗口扣留重绘回调，`logic` 从此不再执行）。
    //
    // 原理：`Context::request_repaint` 会经 eframe 的 event-loop proxy 唤醒事件循环，
    // 即使窗口处于最小化/不可见状态也能到达（Windows 上隐藏窗口收不到系统
    // `RedrawRequested`，eframe 0.34+ 对这类窗口会在收到重绘请求时直接跑一遍
    // `run_ui_and_paint`，viewport 命令因此得以处理）。线程独立于 egui 存活期间，
    // 无论 UI 线程是否已被平台「饿死」，恢复窗口后最多 200ms 内必有一次真帧。
    //
    // 开销：每秒 5 次 proxy 唤醒，空闲功耗可忽略；应用退出时置位停止。
    {
        let ctx = cc.egui_ctx.clone();
        let stop = Arc::clone(&app.keepalive_stop);
        std::thread::Builder::new()
            .name("render-keepalive".into())
            .spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    ctx.request_repaint();
                }
            })
            .expect("启动渲染保活线程失败");
    }

    app
}

    // ---- 跨模块小工具 ----

    /// 应用设置中的字体（设置页切选时调用）：重建字体表**即时生效**，
    /// 无需重启；持久化交给设置落盘机制（每 5s 兜底 + 退出保存）。
    ///
    /// 主界面恒用内嵌字体（`UiFont::Auto`/`Embedded` 等价，仅保留旧配置兼容）；
    /// 歌词字体按 `LyricsFont` 解析。`Specific` 指向的文件失效（被删/格式不支持）
    /// 时回退内嵌并弹 toast 说明；返回值表示该选择是否成功生效（UI 据此复位选择框）。
    pub(crate) fn apply_font_setting(&mut self, ctx: &egui::Context, font: &LyricsFont) -> bool {
        let adopted = crate::fonts::install_fonts(ctx, font);
        // 只对「指定了文件却没生效」的 Specific 报错：FollowUi/Embedded 本来就
        // 解析成内嵌，adopted 必为 Embedded，不是失败。
        if let LyricsFont::Specific(path) = font
            && adopted != *font
        {
            self.error(format!(
                "字体 {path} 不可用（读取失败或格式不支持），已回退内嵌字体"
            ));
        }
        // 字体变更后：柔影缓存按文本键控、不含字体维度，必须整体失效；
        // deferred 浮窗文本指纹未变（同一句歌词），需显式唤醒一次重绘换新字体。
        crate::app::ui::lyrics_viewport::clear_shadow_cache(ctx);
        self.request_lyrics_repaint(ctx);
        adopted == *font
    }

    /// 当前播放曲目的 bvid。
    pub(crate) fn current_bvid(&self) -> Option<&str> {
        self.current_bvid.as_deref()
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
        // toast 文本同样净化：消息里常拼歌单/曲目名（可能带 emoji）。
        self.toasts
            .push(Toast::new(crate::fonts::sanitize_text(&msg.into()), ToastKind::Notice));
    }

    /// 错误提示：顶部弹暖红色 toast。
    pub(crate) fn error(&mut self, msg: impl Into<String>) {
        self.toasts
            .push(Toast::new(crate::fonts::sanitize_text(&msg.into()), ToastKind::Error));
    }

    // ---- 每帧同步 ----

    fn sync_playback(&mut self, st: &PlaybackStatus) {
        self.state.playing = st.playing;
        self.state.position_secs = st.position_secs;
        if st.duration_secs > 0.0 {
            self.state.duration_secs = st.duration_secs;
        }
    }

    fn handle_finished(&mut self, st: &PlaybackStatus) -> bool {
        if st.finished && self.audio.take_finished() {
            self.next_track();
            return true;
        }
        false
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

        // 状态同步与曲终推进；它们引发的即时重绘统一由下方 `repaint_msg` /
        // 播放节流块决定（`sync_playback` 的播放态本身由节流块处理）。
        let st = self.audio.status();
        self.sync_playback(&st);
        let track_switched = self.handle_finished(&st);

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

        // 最小化恢复卡死的兜底修复（egui #8246 / #5136：最小化后部分平台不再投递
        // RedrawRequested，eframe 只在重绘事件里跑 logic/UI，恢复后事件循环等不到
        // 重绘 → 界面永久冻结）。
        //
        // 1) macOS 上 `update_viewport_info` 为规避运行时查询死锁（egui #3494）不会
        //    刷新 `minimized`，而标题栏发的 `ViewportCommand::Minimized(true)` 会把它
        //    锁死为 `Some(true)`（egui #8246）→ eframe 从此认为窗口不可见，跳过全部
        //    UI 绘制，还原窗口后界面冻结。这里检测「egui 认为最小化、但窗口实际已
        //    恢复」的矛盾态——真最小化窗口拿不到焦点，`inner_rect` 也会被置 None；
        //    二者任一恢复即说明窗口已还原——补发 `Minimized(false)` 清掉锁存
        //    （非 macOS 平台 minimized 每帧从真实状态刷新：真最小化时 inner_rect 必为
        //    None，该条件不可能成立，无副作用）。
        // 2) 恢复瞬间（最小化 → 正常）补发一次重绘，把可能卡住的事件循环踹醒。
        let (minimized_flag, focused, has_rect) = ctx.input(|i| {
            (
                i.viewport().minimized,
                i.viewport().focused,
                i.viewport().inner_rect.is_some(),
            )
        });
        if minimized_flag == Some(true) && (focused == Some(true) || has_rect) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        }

        let minimized = minimized_flag.unwrap_or(false);
        if self.was_minimized != minimized {
            self.was_minimized = minimized;
            ctx.request_repaint();
        }

        // 播放中的自醒重绘：进度条按 [`PLAY_REPAINT_INTERVAL`] 节流（人眼对进度
        // 平滑度不敏感），事件驱动帧（输入/消息/转场）照常即时响应。这同时把渲染
        // 线程让给桌面歌词浮窗——浮窗过渡动画期间若主窗口也在全速重绘，二者在
        // winit 全局重绘队列里互相踩踏，是浮窗动画掉帧的主因。
        if self.state.playing || st.loading {
            let now = Instant::now();
            let due = match self.last_playing_repaint {
                Some(t) => now.duration_since(t).as_secs_f32() >= PLAY_REPAINT_INTERVAL,
                None => true,
            };
            if due {
                self.last_playing_repaint = Some(now);
                // 醒来时刻取「节流间隔」与「下一个歌词切换点」的较早者：进度条
                // 低频刷新的同时，切行动画不会因节流而迟到（迟到会吃掉过渡前段）。
                let lyrics_delay = crate::app::lyrics::next_switch_delay_secs(
                    &self.lyrics_lines,
                    self.lyrics_plain.len(),
                    self.state.position_secs,
                    self.state.duration_secs,
                )
                .map(|d| (d - LYRICS_SWITCH_WAKE_EARLY) as f32)
                .filter(|d| *d > 0.05);
                let delay = lyrics_delay.unwrap_or(PLAY_REPAINT_INTERVAL);
                ctx.request_repaint_after(std::time::Duration::from_secs_f32(delay));
            }
        } else {
            self.last_playing_repaint = None;
        }

        // 后台线程消息（取流完成/失败等）与曲终切歌必须下一帧立即上屏。
        if repaint_msg || track_switched {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        let _ = self.settings.save();
        if self.queue_dirty {
            let _ = storage::save_playlists(&self.playlists);
        }
        // 停掉重绘保活线程（应用退出后不再唤醒事件循环）。
        self.keepalive_stop
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.tray.stop();
        eprintln!("[app] 托盘已关闭，应用退出。");
    }
}

use eframe::glow;
