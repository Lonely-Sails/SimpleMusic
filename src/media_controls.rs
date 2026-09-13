//! 系统媒体控制（可选 feature `media-control`）。
//!
//! 把当前播放曲目推送给操作系统，让系统能「检测到」正在播放的音乐，并在
//! 系统自带的媒体面板里控制它：
//!
//! - **macOS**：控制中心 / 菜单栏「正在播放」（`MPNowPlayingInfoCenter`）+
//!   媒体键与 AirPods 手势（`MPRemoteCommandCenter`）；
//! - **Windows**：系统媒体传输控件 SMTC（音量浮层里的播放卡片）；
//! - **Linux**：MPRIS（D-Bus），GNOME/KDE 面板、`playerctl` 均可读写。
//!
//! 未启用 feature 时是 no-op 桩（[`MediaControls::is_enabled`] 恒为 `false`），
//! 主程序照常运行。
//!
//! ## 线程模型与平台约束
//!
//! - `souvlaki::MediaControls` **不是 `Send`**（内部是 objc 对象 / D-Bus 连接），
//!   因此它只能待在**主线程**：`MusicApp` 持有本模块的包装类型，每帧调用
//!   [`MediaControls::sync`]，不做任何跨线程共享。
//! - **macOS 要求事件循环已在运行**（`NSApplication` 已就绪）才能注册远程命令；
//!   `MusicApp::new` 在 eframe 启动回调里调用 [`MediaControls::init`]，满足该约束。
//! - 系统事件（上一首/下一首/拖动进度…）由 souvlaki 内部回调线程推入 mpsc 通道，
//!   主线程每帧 `try_recv` 排空（[`MediaControls::drain_events`]），与托盘事件同款。
//!
//! ## 为什么封面要自己先下到本地
//!
//! macOS 侧 souvlaki 用 `NSImage initWithContentsOfURL:` 加载封面，**只认本地
//! 文件或 http(s) URL**；B 站图床校验 UA 防盗链，直接把 URL 交给它大概率拿不到图。
//! 所以本模块在后台线程用项目共享 HTTP 客户端（带 B 站 UA/Referer）把封面下到
//! 缓存目录，再把 `file://` 路径交给 souvlaki。下载失败不影响其它元数据。
//!
//! 元数据（标题/歌手/时长）变更频率低，只在**曲目或播放态变化**时才推送；
//! 进度则按 [`POSITION_PUSH_INTERVAL`] 节流推送，避免每帧跨平台调用。

#[cfg(feature = "media-control")]
mod inner {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    use souvlaki::{
        MediaControlEvent, MediaControls as SouvlakiControls, MediaMetadata, MediaPlayback,
        MediaPosition, PlatformConfig, SeekDirection,
    };

    /// 系统事件 → 主线程的通道。souvlaki 的回调只要求 `Fn + Send + 'static`
    /// （非 `'static` 引用问题：闭包内不能借用 `MusicApp`），因此走全局通道，
    /// 与托盘 `MenuEvent::receiver()` 同款做法。
    static EVENT_TX: OnceLock<Sender<MediaControlEvent>> = OnceLock::new();
    static EVENT_RX: OnceLock<Mutex<Receiver<MediaControlEvent>>> = OnceLock::new();

    /// 进度推送节流间隔。系统面板自己会按播放态推进进度条，这里只需偶尔校正
    /// （暂停/恢复、seek 后立即校正一次）。
    const POSITION_PUSH_INTERVAL: Duration = Duration::from_secs(2);

    /// 封面缓存目录（`~/.cache/simple-music/cover`）。
    fn cover_cache_dir() -> PathBuf {
        let base = std::env::var("XDG_CACHE_HOME")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .filter(|h| !h.is_empty())
                    .map(|h| PathBuf::from(h).join(".cache"))
            })
            .unwrap_or_else(std::env::temp_dir);
        base.join("simple-music").join("cover")
    }

    /// 封面在本地缓存里的路径：按封面 URL 的 md5 命名（URL 变即换名，天然去重）。
    pub(crate) fn cover_cache_path(url: &str) -> PathBuf {
        cover_cache_dir().join(format!("{}.img", crate::modules::bilibili::md5_hex(url)))
    }

    /// 已经交给系统的封面 URL（避免每帧重复查盘/重复下载）。
    /// 这是本模块唯一需要跨线程的共享状态（下载任务在后台线程回填）。
    fn fetched_covers() -> &'static Mutex<HashSet<String>> {
        static S: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
        S.get_or_init(|| Mutex::new(HashSet::new()))
    }

    /// 后台下载封面到本地缓存（best-effort，失败静默）。已下过/已在飞则跳过。
    ///
    /// 走项目共享 tokio runtime（`crate::net`），不新建线程、不新建 HTTP 客户端。
    fn ensure_cover_cached(url: &str) {
        if url.is_empty() {
            return;
        }
        {
            let Ok(mut set) = fetched_covers().lock() else {
                return;
            };
            if !set.insert(url.to_string()) {
                return; // 已下载或已在飞
            }
        }
        let path = cover_cache_path(url);
        if path.is_file() {
            return;
        }
        let url = url.to_string();
        crate::net::spawn(async move {
            let fetched = crate::net::http_client()
                .get(&url)
                .header("User-Agent", crate::modules::bilibili::USER_AGENT)
                .header("Referer", crate::modules::bilibili::REFERER)
                .timeout(Duration::from_secs(15))
                .send()
                .await
                .and_then(|r| r.error_for_status());
            let bytes = match fetched {
                Ok(r) => r.bytes().await,
                Err(e) => Err(e),
            };
            match bytes {
                Ok(bytes) if !bytes.is_empty() => {
                    if let Some(dir) = path.parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    // 先写临时文件再原子重命名：避免半截文件被当成有效封面。
                    let tmp = path.with_extension("part");
                    if std::fs::write(&tmp, &bytes).is_ok() {
                        let _ = std::fs::rename(&tmp, &path);
                    }
                }
                _ => {
                    // 失败：允许下次重试。
                    if let Ok(mut set) = fetched_covers().lock() {
                        set.remove(&url);
                    }
                }
            }
        });
    }

    /// 一次「推送请求」：主线程把当前播放态整理成纯数据，交由本模块决定推什么。
    pub struct NowPlaying<'a> {
        pub title: &'a str,
        pub artist: &'a str,
        /// 封面 URL（远程；模块内部负责下到本地再交给系统）。
        pub cover_url: &'a str,
        pub duration_secs: f64,
        pub position_secs: f64,
        pub playing: bool,
        /// 曲目是否已结束/停止（此时应把系统面板置为 Stopped）。
        pub stopped: bool,
    }

    /// 系统媒体控制句柄（主线程持有）。
    pub struct MediaControls {
        inner: Option<SouvlakiControls>,
        /// 上次推送的元数据指纹（标题/歌手/时长/封面 URL/封面是否已下好）：
        /// 变化才重推，避免每帧跳平台调用。
        last_meta: Option<(String, String, u64, String, bool)>,
        /// 上次推送的播放态（播放/暂停），用于状态变化时立即推送。
        last_playing: Option<bool>,
        /// 上次推送进度的时刻（节流）。
        last_position_push: Option<Instant>,
        /// 上次已上报给系统的进度（秒）：与当前偏差过大（seek）时立即推送。
        last_position: f64,
    }

    impl MediaControls {
        /// 未初始化状态（`init` 前）。
        pub(crate) fn disabled() -> Self {
            Self {
                inner: None,
                last_meta: None,
                last_playing: None,
                last_position_push: None,
                last_position: 0.0,
            }
        }

        /// 在主线程创建媒体控制并注册系统事件回调（必须在事件循环运行后调用）。
        ///
        /// 失败（平台不支持/无 D-Bus 等）静默降级为 disabled，不影响播放。
        pub(crate) fn init(&mut self) {
            if self.inner.is_some() {
                return;
            }
            let (tx, rx) = std::sync::mpsc::channel();
            let _ = EVENT_TX.set(tx);
            let _ = EVENT_RX.set(Mutex::new(rx));

            let config = PlatformConfig {
                display_name: "SimpleMusic",
                dbus_name: "simple_music",
                hwnd: None,
            };
            let mut controls = match SouvlakiControls::new(config) {
                Ok(c) => c,
                Err(e) => {
                    crate::util::log::warn(
                        "media",
                        &format!("系统媒体控制不可用（{e:?}），已跳过"),
                    );
                    return;
                }
            };
            // 回调在线程 A 执行，只做「投递事件」，具体处理在主线程 `drain_events`。
            let attached = controls.attach(|event: MediaControlEvent| {
                if let Some(tx) = EVENT_TX.get() {
                    let _ = tx.send(event);
                }
            });
            if let Err(e) = attached {
                crate::util::log::warn(
                    "media",
                    &format!("注册系统媒体控制事件失败（{e:?}），已跳过"),
                );
                return;
            }
            crate::util::log::info("media", "系统媒体控制已就绪");
            self.inner = Some(controls);
        }

        /// 系统媒体控制是否可用。
        pub(crate) fn is_enabled(&self) -> bool {
            self.inner.is_some()
        }

        /// 停用系统媒体控制（移除系统面板条目并解绑媒体键）。
        ///
        /// 对应设置页「系统媒体控制」开关关闭：souvlaki 的 `Drop` 会自动
        /// `detach`，因此直接丢掉句柄即可。
        pub(crate) fn shutdown(&mut self) {
            self.inner.take();
            self.last_meta = None;
            self.last_playing = None;
            self.last_position_push = None;
        }

        /// 排空系统媒体事件并翻译成控制意图（主线程每帧调用）。
        ///
        /// 回调收到的是与平台无关的 [`ControlIntent`]，调用方无需依赖 souvlaki。
        pub(crate) fn drain_events(&self, mut handle: impl FnMut(ControlIntent)) {
            let Some(rx) = EVENT_RX.get() else {
                return;
            };
            let Ok(rx) = rx.lock() else {
                return;
            };
            while let Ok(event) = rx.try_recv() {
                if let Some(intent) = intent_of(&event) {
                    handle(intent);
                }
            }
        }

        /// 推送当前播放态到系统（元数据变化才重推；进度节流推送）。
        pub(crate) fn sync(&mut self, np: &NowPlaying<'_>) {
            let Some(controls) = self.inner.as_mut() else {
                return;
            };

            // ---- 元数据（标题/歌手/时长/封面）----
            // 停止态（当前没有曲目）**不推元数据**：此时 `state.title` 是「未在播放」
            // 这类占位文案，写进系统面板反而奇怪；保留最后一首的信息（与系统自带
            // 音乐播放器一致），只把播放态置为 Stopped。
            //
            // 封面优先用已下好的本地文件（`file://` 才能被 macOS 侧加载）；尚未
            // 下完时先不传，但**把「本地封面是否就绪」也算进指纹**，这样下载完成
            // 后的下一帧会重新推送一次，把封面补上。
            if !np.stopped {
                let local_cover = if np.cover_url.is_empty() {
                    None
                } else {
                    ensure_cover_cached(np.cover_url);
                    let path = cover_cache_path(np.cover_url);
                    path.is_file().then(|| format!("file://{}", path.display()))
                };
                let meta_key = (
                    np.title.to_string(),
                    np.artist.to_string(),
                    np.duration_secs.max(0.0).round() as u64,
                    np.cover_url.to_string(),
                    local_cover.is_some(),
                );
                if self.last_meta.as_ref() != Some(&meta_key) {
                    self.last_meta = Some(meta_key);
                    let duration =
                        (np.duration_secs > 0.0).then(|| Duration::from_secs_f64(np.duration_secs));
                    let metadata = MediaMetadata {
                        title: Some(np.title),
                        artist: Some(np.artist),
                        album: None,
                        cover_url: local_cover.as_deref(),
                        duration,
                    };
                    let _ = controls.set_metadata(metadata);
                }
            }

            // ---- 播放态 ----
            let progress = (np.position_secs >= 0.0)
                .then(|| MediaPosition(Duration::from_secs_f64(np.position_secs.max(0.0))));
            let playback = if np.stopped {
                MediaPlayback::Stopped
            } else if np.playing {
                MediaPlayback::Playing { progress }
            } else {
                MediaPlayback::Paused { progress }
            };
            let playing_key = np.playing && !np.stopped;
            if self.last_playing != Some(playing_key) {
                self.last_playing = Some(playing_key);
                let _ = controls.set_playback(playback);
                self.last_position = np.position_secs;
                self.last_position_push = Some(Instant::now());
                return;
            }

            // ---- 进度（节流；seek 造成的跳变立即推送）----
            let now = Instant::now();
            let jumped = (np.position_secs - self.last_position).abs() > 3.0;
            let due = match self.last_position_push {
                Some(t) => now.duration_since(t) >= POSITION_PUSH_INTERVAL,
                None => true,
            };
            if jumped || due {
                self.last_position = np.position_secs;
                self.last_position_push = Some(now);
                let _ = controls.set_playback(playback);
            }
        }

        /// 把系统音量事件回报给系统（仅 MPRIS 后端需要；其它平台忽略）。
        ///
        /// `set_volume` 只在 Linux（MPRIS）后端存在，故按平台 cfg 分派。
        pub(crate) fn report_volume(&mut self, volume: f64) {
            if let Some(controls) = self.inner.as_mut() {
                #[cfg(target_os = "linux")]
                {
                    let _ = controls.set_volume(volume);
                }
                #[cfg(not(target_os = "linux"))]
                {
                    let _ = (controls, volume);
                }
            }
        }
    }

    /// 把系统事件翻译成本项目的控制意图（纯函数，便于单测）。
    ///
    /// 返回 `None` 表示该事件与播放控制无关（如 `Raise`/`Quit` 由调用方另行处理）。
    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum ControlIntent {
        Toggle,
        Play,
        Pause,
        Next,
        Prev,
        /// 相对跳转（秒，可正可负）。
        SeekBy(f64),
        /// 绝对定位（秒）。
        SeekTo(f64),
        Volume(f64),
        /// 显示主窗口。
        Raise,
        /// 退出应用。
        Quit,
    }

    /// 系统事件 → 控制意图。
    pub(super) fn intent_of(event: &MediaControlEvent) -> Option<ControlIntent> {
        match event {
            MediaControlEvent::Play => Some(ControlIntent::Play),
            MediaControlEvent::Pause => Some(ControlIntent::Pause),
            MediaControlEvent::Toggle => Some(ControlIntent::Toggle),
            MediaControlEvent::Next => Some(ControlIntent::Next),
            MediaControlEvent::Previous => Some(ControlIntent::Prev),
            MediaControlEvent::Stop => Some(ControlIntent::Pause),
            // 无明确幅度的 seek：按项目快捷键同款步长（5 秒）。
            MediaControlEvent::Seek(dir) => Some(ControlIntent::SeekBy(match dir {
                SeekDirection::Forward => 5.0,
                SeekDirection::Backward => -5.0,
            })),
            MediaControlEvent::SeekBy(dir, d) => {
                let secs = d.as_secs_f64();
                Some(ControlIntent::SeekBy(match dir {
                    SeekDirection::Forward => secs,
                    SeekDirection::Backward => -secs,
                }))
            }
            MediaControlEvent::SetPosition(p) => Some(ControlIntent::SeekTo(p.0.as_secs_f64())),
            MediaControlEvent::SetVolume(v) => Some(ControlIntent::Volume(*v)),
            MediaControlEvent::Raise => Some(ControlIntent::Raise),
            MediaControlEvent::Quit => Some(ControlIntent::Quit),
            // souvlaki 的事件枚举未来可能新增变体：忽略未知事件而非编译失败。
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cover_cache_path_is_stable_and_distinct() {
            let a = cover_cache_path("https://i0.hdslb.com/x.jpg");
            let b = cover_cache_path("https://i0.hdslb.com/x.jpg");
            let c = cover_cache_path("https://i0.hdslb.com/y.jpg");
            assert_eq!(a, b, "同 URL 同路径");
            assert_ne!(a, c, "不同 URL 不同路径");
            assert!(a.to_string_lossy().ends_with(".img"));
        }

        #[test]
        fn intent_maps_core_events() {
            assert_eq!(
                intent_of(&MediaControlEvent::Toggle),
                Some(ControlIntent::Toggle)
            );
            assert_eq!(
                intent_of(&MediaControlEvent::Next),
                Some(ControlIntent::Next)
            );
            assert_eq!(
                intent_of(&MediaControlEvent::Previous),
                Some(ControlIntent::Prev)
            );
            assert_eq!(
                intent_of(&MediaControlEvent::Seek(SeekDirection::Forward)),
                Some(ControlIntent::SeekBy(5.0))
            );
            assert_eq!(
                intent_of(&MediaControlEvent::SeekBy(
                    SeekDirection::Backward,
                    Duration::from_secs(10)
                )),
                Some(ControlIntent::SeekBy(-10.0))
            );
            assert_eq!(
                intent_of(&MediaControlEvent::SetPosition(MediaPosition(
                    Duration::from_secs_f64(42.5)
                ))),
                Some(ControlIntent::SeekTo(42.5))
            );
            assert_eq!(
                intent_of(&MediaControlEvent::SetVolume(0.25)),
                Some(ControlIntent::Volume(0.25))
            );
            assert_eq!(
                intent_of(&MediaControlEvent::Raise),
                Some(ControlIntent::Raise)
            );
        }
    }
}

#[cfg(feature = "media-control")]
pub use inner::{ControlIntent, MediaControls, NowPlaying};

#[cfg(not(feature = "media-control"))]
mod stub {
    /// 未启用 `media-control` 时的 no-op 桩：保持与启用时相同的 API 面。
    pub struct MediaControls;

    /// 一次推送请求（桩版本不读字段，仅保持签名一致）。
    #[allow(dead_code)]
    pub struct NowPlaying<'a> {
        pub title: &'a str,
        pub artist: &'a str,
        pub cover_url: &'a str,
        pub duration_secs: f64,
        pub position_secs: f64,
        pub playing: bool,
        pub stopped: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    #[allow(dead_code)]
    pub enum ControlIntent {
        Toggle,
        Play,
        Pause,
        Next,
        Prev,
        SeekBy(f64),
        SeekTo(f64),
        Volume(f64),
        Raise,
        Quit,
    }

    impl MediaControls {
        pub fn disabled() -> Self {
            Self
        }
        pub fn init(&mut self) {}
        pub fn is_enabled(&self) -> bool {
            false
        }
        pub fn shutdown(&mut self) {}
        pub fn drain_events(&self, _handle: impl FnMut(ControlIntent)) {}
        pub fn sync(&mut self, _np: &NowPlaying<'_>) {}
        pub fn report_volume(&mut self, _volume: f64) {}
    }
}

#[cfg(not(feature = "media-control"))]
pub use stub::{ControlIntent, MediaControls, NowPlaying};
