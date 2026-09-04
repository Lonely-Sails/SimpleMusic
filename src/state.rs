//! 共享状态：播放状态、播放列表、用户设置与播放模式。

use serde::{Deserialize, Serialize};

/// 全局播放状态（由 AudioEngine 更新，UI 只读）。
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackState {
    pub playing: bool,
    /// 当前播放位置（秒）。
    pub position_secs: f64,
    /// 当前曲目总时长（秒），未知时为 0。
    pub duration_secs: f64,
    /// 音量 0.0 ~ 1.0。
    pub volume: f32,
    pub title: String,
    pub artist: String,
    /// 当前正在播放的 LRC 行文本（桌面歌词显示用）。
    pub current_lrc_line: String,
}

impl Default for PlaybackState {
    fn default() -> Self {
        Self {
            playing: false,
            position_secs: 0.0,
            duration_secs: 0.0,
            volume: 0.8,
            title: "未在播放".to_string(),
            artist: "SimpleMusic".to_string(),
            current_lrc_line: String::new(),
        }
    }
}

impl PlaybackState {
    /// 前进 `dt` 秒，遇到结尾自动停止并回到起点（供无循环播放语义使用）。
    pub fn advance(&mut self, dt: f64) {
        if !self.playing {
            return;
        }
        self.position_secs += dt.max(0.0);
        if self.duration_secs > 0.0 && self.position_secs >= self.duration_secs {
            self.position_secs = self.duration_secs;
            self.playing = false;
        }
    }

    /// 跳转：把位置限制在 [0, duration] 内；未知时长时只限制下界。
    /// 返回实际生效的位置。
    pub fn seek(&mut self, secs: f64) -> f64 {
        let max = if self.duration_secs > 0.0 {
            self.duration_secs
        } else {
            f64::INFINITY
        };
        self.position_secs = secs.clamp(0.0, max);
        self.position_secs
    }

    /// 播放进度 0.0 ~ 1.0（时长未知时返回 0.0）。
    pub fn progress(&self) -> f32 {
        if self.duration_secs > 0.0 {
            (self.position_secs / self.duration_secs).clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    }
}

// ---------------------------------------------------------------------------
// 曲目与播放列表
// ---------------------------------------------------------------------------

/// 播放队列中的一条曲目。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    /// B 站视频 BV 号。
    pub bvid: String,
    pub title: String,
    pub uploader: String,
    /// 时长（秒），未知时为 0。
    pub duration_secs: f64,
    /// 封面 URL（B 站图床），旧版 playlist.json 无此字段时反序列化为空串。
    #[serde(default)]
    pub cover_url: String,
    /// 第一分 P 的 cid（解析播放时回填）。歌词线程用它调 B 站「识别音乐」接口
    /// 免去重复拉取 video_info；旧版 playlist.json 无此字段时反序列化为 0。
    #[serde(default)]
    pub cid: i64,
}

impl QueueItem {
    /// 队列持久化的 serde 序列化用（无封面）。
    pub fn new(
        bvid: impl Into<String>,
        title: impl Into<String>,
        uploader: impl Into<String>,
        duration_secs: f64,
    ) -> Self {
        Self {
            bvid: bvid.into(),
            title: title.into(),
            uploader: uploader.into(),
            duration_secs: duration_secs.max(0.0),
            cover_url: String::new(),
            cid: 0,
        }
    }

    /// 带封面的构造。
    pub fn new_with_cover(
        bvid: impl Into<String>,
        title: impl Into<String>,
        uploader: impl Into<String>,
        duration_secs: f64,
        cover_url: impl Into<String>,
    ) -> Self {
        Self {
            cover_url: cover_url.into(),
            ..Self::new(bvid, title, uploader, duration_secs)
        }
    }
}

/// 播放列表种类。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PlaylistKind {
    /// 本地歌单（用户创建，可编辑）。
    Local,
    /// 在线歌单（同步 B 站收藏夹，只读）。
    Online { media_id: i64, folder_title: String },
}

/// 一个播放列表（歌单）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playlist {
    pub name: String,
    pub songs: Vec<QueueItem>,
    pub kind: PlaylistKind,
}

impl Playlist {
    pub fn local(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            songs: Vec::new(),
            kind: PlaylistKind::Local,
        }
    }

    /// 是否为在线歌单（只读）。
    pub fn is_online(&self) -> bool {
        matches!(self.kind, PlaylistKind::Online { .. })
    }
}

// ---------------------------------------------------------------------------
// 播放模式
// ---------------------------------------------------------------------------

/// 切歌模式。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PlayMode {
    /// 顺序循环（列表播完回到第一首）。
    Sequence,
    /// 单曲循环。
    SingleRepeat,
    /// 随机播放。
    Shuffle,
}

impl PlayMode {
    /// 所有模式名（popup menu 用）。
    pub const ALL: &'static [PlayMode] = &[PlayMode::Sequence, PlayMode::SingleRepeat, PlayMode::Shuffle];

    pub fn label(&self) -> &'static str {
        match self {
            PlayMode::Sequence => "顺序循环",
            PlayMode::SingleRepeat => "单曲循环",
            PlayMode::Shuffle => "随机播放",
        }
    }
}

impl Default for PlayMode {
    fn default() -> Self {
        PlayMode::Sequence
    }
}

// ---------------------------------------------------------------------------
// 音质
// ---------------------------------------------------------------------------

/// 音频质量偏好。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum AudioQuality {
    /// 流畅 64kbps (id 30216)
    Low,
    /// 标准 128kbps (id 30232)
    Medium,
    /// 高质 320kbps (id 30280)
    High,
    /// 无损 FLAC (id 30255) / Dolby
    Lossless,
}

impl AudioQuality {
    pub const ALL: &'static [AudioQuality] = &[
        AudioQuality::Low,
        AudioQuality::Medium,
        AudioQuality::High,
        AudioQuality::Lossless,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            AudioQuality::Low => "低码率 (64kbps)",
            AudioQuality::Medium => "标准 (128kbps)",
            AudioQuality::High => "高码率 (320kbps)",
            AudioQuality::Lossless => "无损 / 杜比",
        }
    }
}

impl Default for AudioQuality {
    fn default() -> Self {
        AudioQuality::High
    }
}

// ---------------------------------------------------------------------------
// 用户设置
// ---------------------------------------------------------------------------

/// 界面字体选择（设置页「界面字体」）。
///
/// - `Auto`：系统探测优先（`fonts::load_system_font`），失败回退内嵌 Noto；
/// - `Embedded`：强制内嵌 Noto Sans SC（观感跨机器一致）；
/// - `Specific`：用户在设置页挑选的系统字体文件（存绝对路径字符串）。
///
/// 持久化为带 tag 的枚举；旧配置没有该字段时 `#[serde(default)]` 落到 Auto。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum UiFont {
    /// 自动：系统探测优先。
    Auto,
    /// 强制内嵌 Noto Sans SC。
    Embedded,
    /// 指定系统字体文件（绝对路径）。
    Specific(String),
}

impl Default for UiFont {
    fn default() -> Self {
        UiFont::Auto
    }
}

impl UiFont {
    pub const fn label(&self) -> &'static str {
        match self {
            UiFont::Auto => "自动（系统优先）",
            UiFont::Embedded => "内嵌 Noto Sans SC",
            UiFont::Specific(_) => "自定义",
        }
    }

    /// Specific 时返回字体文件路径。
    pub fn path(&self) -> Option<&str> {
        match self {
            UiFont::Specific(p) => Some(p),
            _ => None,
        }
    }
}

/// 用户设置（持久化到 ~/.config/simple-music/config.json，由 modules::storage 负责）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub desktop_lyrics_enabled: bool,
    /// 歌词锁定 = 鼠标穿透。
    pub lyrics_locked: bool,
    pub font_scale: f32,
    /// 切歌模式。
    #[serde(default)]
    pub play_mode: PlayMode,
    /// 音质偏好。
    #[serde(default)]
    pub audio_quality: AudioQuality,
    /// 音量 0.0 ~ 1.0。
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// 上次打开的歌单下标（重启后恢复；歌单数量变化时会被钳制）。
    #[serde(default)]
    pub active_playlist: usize,
    /// 界面字体选择（设置页可选自动/内嵌/任意系统字体；详见 [`UiFont`]）。
    #[serde(default)]
    pub ui_font: UiFont,
    /// 桌面歌词浮窗位置（屏幕坐标 `[x, y]`，`None` = 首次由系统默认决定）。
    /// 浮窗每次上报当前位置时更新并随设置落盘，重启后恢复到关闭前的位置。
    /// 存 `[f32; 2]` 而非 egui 的 `Pos2`：项目未启用 eframe 的 serde feature，
    /// `Pos2` 不实现 Serialize，且数据模型层不应反向依赖 egui。
    /// 仅 X11 下可靠；Wayland 上窗口位置由合成器决定，恢复可能被忽略。
    #[serde(default)]
    pub lyrics_pos: Option<[f32; 2]>,
}

fn default_volume() -> f32 {
    0.8
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            desktop_lyrics_enabled: false,
            lyrics_locked: false,
            font_scale: 1.0,
            play_mode: PlayMode::default(),
            audio_quality: AudioQuality::default(),
            volume: 0.8,
            active_playlist: 0,
            ui_font: UiFont::Auto,
            lyrics_pos: None,
        }
    }
}

impl Settings {
    /// 从磁盘加载；文件不存在或损坏时返回 None。
    pub fn load() -> Option<Self> {
        crate::modules::storage::load_settings().ok()
    }

    /// 保存到磁盘（静默失败，骨架阶段不弹出错误 UI）。
    pub fn save(&self) -> std::io::Result<()> {
        crate::modules::storage::save_settings(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_advance_moves_position_and_stops_at_end() {
        let mut st = PlaybackState {
            playing: true,
            duration_secs: 10.0,
            ..Default::default()
        };
        st.advance(3.5);
        assert_eq!(st.position_secs, 3.5);
        assert!(st.playing);

        st.advance(20.0);
        assert_eq!(st.position_secs, 10.0);
        assert!(!st.playing, "播完应自动停止");
    }

    #[test]
    fn test_seek_clamps_to_bounds() {
        let mut st = PlaybackState {
            playing: true,
            duration_secs: 100.0,
            ..Default::default()
        };
        assert_eq!(st.seek(-5.0), 0.0);
        assert_eq!(st.seek(150.0), 100.0);
        assert_eq!(st.seek(42.0), 42.0);

        // 时长未知时只限制下界。
        let mut unknown = PlaybackState::default();
        assert_eq!(unknown.seek(-1.0), 0.0);
        assert_eq!(unknown.seek(30.0), 30.0);
    }

    #[test]
    fn test_advance_ignored_when_paused() {
        let mut st = PlaybackState::default();
        st.advance(5.0);
        assert_eq!(st.position_secs, 0.0);
    }

    #[test]
    fn test_progress_bounds() {
        let mut st = PlaybackState {
            position_secs: 50.0,
            duration_secs: 200.0,
            ..Default::default()
        };
        assert_eq!(st.progress(), 0.25);
        st.duration_secs = 0.0;
        assert_eq!(st.progress(), 0.0);
    }

    #[test]
    fn queue_item_old_json_without_cover_url_still_loads() {
        // 旧版 playlist.json 没有 cover_url 字段，必须可反序列化（serde default）。
        let old = r#"{"bvid":"BV1","title":"标题","uploader":"UP","duration_secs":10.0}"#;
        let q: QueueItem = serde_json::from_str(old).unwrap();
        assert_eq!(q.cover_url, "");
        assert_eq!(q.bvid, "BV1");
        // 新字段正常序列化/反序列化。
        let item = QueueItem::new_with_cover("BV2", "T2", "U2", 5.0, "https://i0.hdslb.com/x.jpg");
        let json = serde_json::to_string(&item).unwrap();
        let back: QueueItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.cover_url, "https://i0.hdslb.com/x.jpg");
    }

    #[test]
    fn test_playlist_kind_serde() {
        let local = Playlist::local("我的歌单");
        let json = serde_json::to_string(&local).unwrap();
        let back: Playlist = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "我的歌单");
        assert!(back.songs.is_empty());
        assert!(!back.is_online());

        let online = Playlist {
            name: "收藏夹".into(),
            songs: vec![],
            kind: PlaylistKind::Online {
                media_id: 12345,
                folder_title: "我的收藏".into(),
            },
        };
        let json = serde_json::to_string(&online).unwrap();
        let back: Playlist = serde_json::from_str(&json).unwrap();
        assert!(back.is_online());
    }

    #[test]
    fn test_play_mode_labels() {
        assert_eq!(PlayMode::Sequence.label(), "顺序循环");
        assert_eq!(PlayMode::SingleRepeat.label(), "单曲循环");
        assert_eq!(PlayMode::Shuffle.label(), "随机播放");
    }

    #[test]
    fn test_audio_quality_default() {
        assert_eq!(AudioQuality::default(), AudioQuality::High);
    }

    #[test]
    fn test_play_mode_default() {
        assert_eq!(PlayMode::default(), PlayMode::Sequence);
    }

    #[test]
    fn settings_old_json_without_lyrics_pos_still_loads() {
        // 旧版 config.json 没有 lyrics_pos 字段，必须可反序列化（serde default = None）。
        let old = r#"{"desktop_lyrics_enabled":true,"lyrics_locked":false,"font_scale":1.2}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert!(s.desktop_lyrics_enabled);
        assert_eq!(s.lyrics_pos, None);
    }

    #[test]
    fn settings_lyrics_pos_roundtrip() {
        let mut s = Settings::default();
        assert_eq!(s.lyrics_pos, None);
        s.lyrics_pos = Some([123.5, -8.0]);
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.lyrics_pos, Some([123.5, -8.0]));
    }

    #[test]
    fn settings_old_json_without_ui_font_loads_as_auto() {
        // 旧版 config.json 没有 ui_font 字段 → serde default 落到 Auto。
        let old = r#"{"desktop_lyrics_enabled":false,"lyrics_locked":false,"font_scale":1.0}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.ui_font, UiFont::Auto);
    }

    #[test]
    fn ui_font_roundtrip_all_variants() {
        for f in [
            UiFont::Auto,
            UiFont::Embedded,
            UiFont::Specific("/usr/share/fonts/x.ttf".into()),
        ] {
            let json = serde_json::to_string(&f).unwrap();
            let back: UiFont = serde_json::from_str(&json).unwrap();
            assert_eq!(back, f, "roundtrip 失败: {json}");
        }
        // 序列化形态（tag/content 布局，便于人工排查配置文件）。
        // 注意：Auto/Embedded 无 content 字段，serde 省略 "path" 键。
        assert_eq!(
            serde_json::to_string(&UiFont::Auto).unwrap(),
            r#"{"kind":"auto"}"#
        );
        assert_eq!(
            serde_json::to_string(&UiFont::Embedded).unwrap(),
            r#"{"kind":"embedded"}"#
        );
        assert_eq!(
            serde_json::to_string(&UiFont::Specific("/a.ttf".into())).unwrap(),
            r#"{"kind":"specific","path":"/a.ttf"}"#
        );
    }

    #[test]
    fn ui_font_label_and_path_accessors() {
        assert_eq!(UiFont::Auto.label(), "自动（系统优先）");
        assert_eq!(UiFont::Embedded.label(), "内嵌 Noto Sans SC");
        assert_eq!(UiFont::Specific("/x.ttf".into()).label(), "自定义");
        assert_eq!(UiFont::Specific("/x.ttf".into()).path(), Some("/x.ttf"));
        assert_eq!(UiFont::Auto.path(), None);
        assert_eq!(UiFont::Embedded.path(), None);
    }
}