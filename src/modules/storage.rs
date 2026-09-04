//! 配置持久化。
//!
//! 极简 JSON 读写：
//! - `~/.config/simple-music/config.json` —— 用户设置（Settings）。
//! - `~/.config/simple-music/session.json` —— B 站会话（cookies + buvid，见 `BiliSession`）。
//!
//! 安全约定：`BiliSession` 的 Debug 输出已脱敏，SESSDATA/bili_jct 等凭据不得出现在日志里。
//!
//! 后续扩展点：
//! - TODO: 保存收藏列表 / 播放历史 / 播放队列（可另存 playlist.json，避免与设置互相覆盖）。
//! - TODO: 写入改为防抖/事件驱动（当前 app/mod.rs 每 5 秒兜底保存一次）。

use crate::state::{Playlist, QueueItem, Settings};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// B 站会话数据：登录/Set-Cookie 捕获到的 cookies + 未登录也需要的 buvid。
///
/// cookies 键名与 B 站一致，重要的有：
/// - `SESSDATA`（登录凭证）、`bili_jct`（CSRF token）、`DedeUserID`（用户 mid）
/// - `buvid3` / `buvid4`（设备指纹，未登录请求也需要）
#[derive(Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BiliSession {
    /// Cookie 名 -> Cookie 值。
    pub cookies: BTreeMap<String, String>,
    /// 会话最后落盘时间（Unix 秒）。
    #[serde(default)]
    pub saved_at_unix: u64,
}

/// Debug 输出脱敏：只暴露 cookie 键名，绝不打印值（防 SESSDATA 泄漏到日志）。
impl std::fmt::Debug for BiliSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&str> = self.cookies.keys().map(String::as_str).collect();
        f.debug_struct("BiliSession")
            .field("cookies", &keys)
            .field("saved_at_unix", &self.saved_at_unix)
            .finish()
    }
}

impl BiliSession {
    /// 读取单个 cookie。
    pub fn get(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(String::as_str)
    }

    /// 写入单个 cookie。
    pub fn set(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.cookies.insert(name.into(), value.into());
    }

    /// 删除单个 cookie，返回旧值。
    pub fn remove(&mut self, name: &str) -> Option<String> {
        self.cookies.remove(name)
    }

    /// 是否已登录（SESSDATA + DedeUserID 齐全）。
    pub fn logged_in(&self) -> bool {
        self.get("SESSDATA").map_or(false, |v| !v.is_empty())
            && self.get("DedeUserID").map_or(false, |v| !v.is_empty())
    }

    /// 拼接成 HTTP `Cookie` 头的值，如 `a=1; b=2`。
    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// 会话文件完整路径：`~/.config/simple-music/session.json`。
pub fn session_path() -> PathBuf {
    let mut p = config_dir();
    p.push("session.json");
    p
}

/// 从指定路径读取会话；不存在或损坏时返回错误（由调用方决定回退默认值）。
pub fn load_session_from(path: &Path) -> std::io::Result<BiliSession> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "session.json 不存在，未登录/无 buvid 缓存",
            ))
        }
        Err(e) => return Err(e),
    };
    serde_json::from_str(&text)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// 把会话写入指定路径（目录不存在会自动创建）。会话含 SESSDATA 等登录凭据，
/// 落盘后把文件权限收紧为仅属主可读写（0600），避免多用户机器上其他用户读到。
pub fn save_session_to(path: &Path, session: &BiliSession) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(session)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 读取默认路径下的会话。
pub fn load_session() -> std::io::Result<BiliSession> {
    load_session_from(&session_path())
}

/// 把会话写入默认路径。
pub fn save_session(session: &BiliSession) -> std::io::Result<()> {
    save_session_to(&session_path(), session)
}

/// 配置目录：`$HOME/.config/simple-music`（未设置 HOME 时回退到当前目录 `.config/simple-music`）。
pub fn config_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            let mut p = PathBuf::from(home);
            p.push(".config");
            p.push("simple-music");
            return p;
        }
    }
    PathBuf::from(".config/simple-music")
}

/// 配置文件完整路径。
pub fn config_path() -> PathBuf {
    let mut p = config_dir();
    p.push("config.json");
    p
}

/// 读取设置；文件不存在或解析失败时返回错误，由调用方决定是否回退默认值。
pub fn load_settings() -> std::io::Result<Settings> {
    let path = config_path();
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "config.json 不存在，使用默认设置",
            ))
        }
        Err(e) => return Err(e),
    };
    serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// 保存设置（目录不存在会自动创建）。
pub fn save_settings(settings: &Settings) -> std::io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(settings)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, text)
}

/// 播放队列文件完整路径：`~/.config/simple-music/playlist.json`。
/// 已被 playlists.json 取代，仅保留路径常量用于**旧文件迁移**（见
/// [`load_playlists`]）；读取/写入一律走歌单 API。
pub fn playlist_path() -> PathBuf {
    let mut p = config_dir();
    p.push("playlist.json");
    p
}

// ---------------------------------------------------------------------------
// 播放列表（歌单）持久化
// ---------------------------------------------------------------------------

/// 播放列表文件完整路径：`~/.config/simple-music/playlists.json`。
pub fn playlists_path() -> PathBuf {
    let mut p = config_dir();
    p.push("playlists.json");
    p
}

/// 读取所有歌单；文件不存在时尝试从旧版 playlist.json 迁移，否则返回空 Vec。
pub fn load_playlists() -> Vec<Playlist> {
    let path = playlists_path();
    if path.exists() {
        return load_playlists_from(&path).unwrap_or_default();
    }
    // 尝试从旧版 playlist.json 迁移。
    let legacy = playlist_path();
    if legacy.exists() {
        if let Ok(text) = fs::read_to_string(&legacy) {
            if let Ok(items) = serde_json::from_str::<Vec<QueueItem>>(&text) {
                let playlist = Playlist::local("默认歌单");
                let result = vec![Playlist {
                    songs: items,
                    ..playlist
                }];
                let _ = save_playlists_to(&path, &result);
                // 迁移后删除旧文件。
                let _ = fs::remove_file(&legacy);
                return result;
            }
        }
    }
    // 纯新用户：返回一个默认歌单。
    vec![Playlist::local("默认歌单")]
}

/// 从指定路径读取歌单列表。
pub fn load_playlists_from(path: &Path) -> std::io::Result<Vec<Playlist>> {
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// 保存所有歌单到磁盘。
pub fn save_playlists(playlists: &[Playlist]) -> std::io::Result<()> {
    save_playlists_to(&playlists_path(), playlists)
}

/// 保存歌单到指定路径。
pub fn save_playlists_to(path: &Path, playlists: &[Playlist]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(playlists)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, text)
}

// ---------------------------------------------------------------------------
// 歌词缓存持久化
// ---------------------------------------------------------------------------

/// 歌词缓存文件完整路径：`~/.cache/simple-music/lyrics.json`（与音频缓存同根）。
pub fn lyrics_cache_path() -> PathBuf {
    let mut p = cache_dir();
    p.push("lyrics.json");
    p
}

/// 读取歌词缓存；文件不存在/损坏时返回空表（缓存未命中语义，绝不报错打断启动）。
pub fn load_lyrics_cache() -> BTreeMap<String, crate::modules::lyrics::LyricsCacheEntry> {
    load_lyrics_cache_from(&lyrics_cache_path())
}

/// 从指定路径读取歌词缓存（测试用）。
pub fn load_lyrics_cache_from(
    path: &Path,
) -> BTreeMap<String, crate::modules::lyrics::LyricsCacheEntry> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return BTreeMap::new(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 保存歌词缓存到默认路径（目录不存在自动创建；失败由调用方静默处理）。
pub fn save_lyrics_cache(
    cache: &BTreeMap<String, crate::modules::lyrics::LyricsCacheEntry>,
) -> std::io::Result<()> {
    save_lyrics_cache_to(&lyrics_cache_path(), cache)
}

/// 保存歌词缓存到指定路径（测试用）。
pub fn save_lyrics_cache_to(
    path: &Path,
    cache: &BTreeMap<String, crate::modules::lyrics::LyricsCacheEntry>,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(path, text)
}

/// 缓存目录：`$HOME/.cache/simple-music`（与音频缓存同根；未设置 HOME 回退当前目录）。
pub fn cache_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            let mut p = PathBuf::from(home);
            p.push(".cache");
            p.push("simple-music");
            return p;
        }
    }
    PathBuf::from(".cache/simple-music")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct TestCfg {
        name: String,
        value: i32,
    }

    #[test]
    fn test_json_roundtrip_generic() {
        let cfg = TestCfg {
            name: "简单音乐".to_string(),
            value: -42,
        };
        let text = serde_json::to_string(&cfg).expect("序列化失败");
        let back: TestCfg = serde_json::from_str(&text).expect("反序列化失败");
        assert_eq!(back, cfg);
    }

    #[test]
    fn test_settings_roundtrip() {
        let s = Settings {
            desktop_lyrics_enabled: true,
            lyrics_locked: true,
            font_scale: 1.25,
            play_mode: crate::state::PlayMode::Sequence,
            audio_quality: crate::state::AudioQuality::High,
            volume: 0.8,
            active_playlist: 2,
            ui_font: crate::state::UiFont::Embedded,
            lyrics_pos: Some([1920.0, 1040.0]),
        };
        let text = serde_json::to_string_pretty(&s).expect("序列化失败");
        let back: Settings = serde_json::from_str(&text).expect("反序列化失败");
        assert_eq!(back, s);
        assert!(text.contains("desktop_lyrics_enabled"));
        assert!(text.contains("\"active_playlist\": 2"));
        assert!(text.contains("\"lyrics_pos\""));
        assert!(text.contains("\"ui_font\""));
    }

    #[test]
    fn test_settings_old_json_missing_active_playlist_defaults_to_zero() {
        // 旧版 config.json 没有 active_playlist 字段，必须可反序列化（serde default）。
        let old = r#"{
            "desktop_lyrics_enabled": false,
            "lyrics_locked": false,
            "font_scale": 1.0,
            "play_mode": "Sequence",
            "audio_quality": "High",
            "volume": 0.8
        }"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(s.active_playlist, 0);
    }

    #[test]
    fn test_load_missing_file_is_not_found_error() {
        // config_path() 依赖 HOME；无论指向哪里，一个必然不存在的文件应返回 NotFound。
        let missing = config_dir().join("definitely-not-exist.json");
        let text = fs::read_to_string(&missing);
        assert!(text.is_err());
        assert_eq!(text.unwrap_err().kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn test_config_path_layout() {
        // 布局契约：目录名固定为 simple-music，文件名固定为 config.json。
        let p = config_path();
        assert_eq!(p.file_name().unwrap(), "config.json");
        assert_eq!(p.parent().unwrap().file_name().unwrap(), "simple-music");
    }

    #[test]
    fn test_session_roundtrip_on_disk() {
        let mut s = BiliSession::default();
        s.set("SESSDATA", "dummy-sessdata-value");
        s.set("bili_jct", "csrf-token");
        s.set("DedeUserID", "12345");
        s.saved_at_unix = 1_700_000_000;
        let path = std::env::temp_dir().join("sm-test-session-roundtrip.json");
        save_session_to(&path, &s).expect("写会话失败");
        let back = load_session_from(&path).expect("读会话失败");
        assert_eq!(back, s);
        assert!(back.logged_in());
        assert_eq!(back.cookie_header(), "DedeUserID=12345; SESSDATA=dummy-sessdata-value; bili_jct=csrf-token");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_session_debug_is_redacted() {
        // 凭据绝不能出现在 Debug 输出里。
        let mut s = BiliSession::default();
        s.set("SESSDATA", "SECRET-VALUE");
        s.set("buvid3", "BUVID-VALUE");
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("SECRET-VALUE"), "Debug 泄漏 SESSDATA: {dbg}");
        assert!(!dbg.contains("BUVID-VALUE"), "Debug 泄漏 buvid3: {dbg}");
        assert!(dbg.contains("SESSDATA"));
        assert!(dbg.contains("buvid3"));
    }

    #[test]
    fn test_session_path_layout() {
        let p = session_path();
        assert_eq!(p.file_name().unwrap(), "session.json");
        assert_eq!(p.parent().unwrap().file_name().unwrap(), "simple-music");
    }

    #[test]
    fn test_playlist_path_layout() {
        let p = playlist_path();
        assert_eq!(p.file_name().unwrap(), "playlist.json");
        assert_eq!(p.parent().unwrap().file_name().unwrap(), "simple-music");
    }

    #[test]
    fn test_playlists_roundtrip() {
        let path = std::env::temp_dir().join("sm-test-playlists-roundtrip.json");
        let playlists = vec![
            Playlist::local("我的歌单"),
            Playlist {
                name: "收藏夹".into(),
                songs: vec![QueueItem::new("BV1", "T", "U", 10.0)],
                kind: crate::state::PlaylistKind::Online {
                    media_id: 7,
                    folder_title: "我的收藏".into(),
                },
            },
        ];
        save_playlists_to(&path, &playlists).expect("写歌单失败");
        let back = load_playlists_from(&path).expect("读歌单失败");
        assert_eq!(back, playlists);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_playlists_path_layout() {
        let p = playlists_path();
        assert_eq!(p.file_name().unwrap(), "playlists.json");
        assert_eq!(p.parent().unwrap().file_name().unwrap(), "simple-music");
    }

    // ---- 歌词缓存持久化 ----

    fn sample_cache_entry(tag: &str) -> crate::modules::lyrics::LyricsCacheEntry {
        use crate::modules::lyrics::{LrcSearchResult, Lyrics, LyricsCacheEntry};
        LyricsCacheEntry {
            selected: Some(Lyrics {
                lrc: Some(format!("[00:01.00]第一句{tag}")),
                plain: format!("第一句{tag}"),
                source: Some(LrcSearchResult {
                    id: 1,
                    track_name: format!("晴天{tag}"),
                    artist_name: "周杰伦".into(),
                    album_name: "叶惠美".into(),
                    duration: 269.0,
                    instrumental: false,
                    plain_lyrics: String::new(),
                    synced_lyrics: String::new(),
                }),
            }),
            candidates: vec![],
            saved_at_unix: 1_700_000_000,
        }
    }

    #[test]
    fn test_lyrics_cache_roundtrip_and_miss() {
        use crate::modules::lyrics::{cache_key, cache_lookup, LyricsCacheEntry};
        let path = std::env::temp_dir().join("sm-test-lyrics-cache.json");
        let _ = fs::remove_file(&path);
        // 无文件 = 空缓存。
        assert!(load_lyrics_cache_from(&path).is_empty());

        let mut cache = BTreeMap::new();
        cache.insert(cache_key("BV1GJ411x7h7"), sample_cache_entry("A"));
        cache.insert(
            cache_key("BV1xx411c7mD"),
            LyricsCacheEntry {
                selected: None,
                candidates: vec![],
                saved_at_unix: 0,
            },
        );
        save_lyrics_cache_to(&path, &cache).expect("写歌词缓存失败");

        let back = load_lyrics_cache_from(&path);
        assert_eq!(back.len(), 2);
        let hit = cache_lookup(&back, "BV1GJ411x7h7").expect("按 bvid 命中");
        assert_eq!(hit.selected.as_ref().unwrap().lrc.as_deref(), Some("[00:01.00]第一句A"));
        // 坏文件静默降级为空缓存。
        fs::write(&path, "{not json").unwrap();
        assert!(load_lyrics_cache_from(&path).is_empty());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_lyrics_cache_path_layout() {
        let p = lyrics_cache_path();
        assert_eq!(p.file_name().unwrap(), "lyrics.json");
        // 与音频缓存同根：~/.cache/simple-music。
        let parent = p.parent().unwrap();
        assert_eq!(parent.file_name().unwrap(), "simple-music");
        assert!(parent.to_string_lossy().contains(".cache"));
    }
}
