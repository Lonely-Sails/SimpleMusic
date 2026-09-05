//! 本地歌词缓存条目语义（磁盘读写见 [`crate::modules::storage`]）。

use std::collections::BTreeMap;

use super::model::Lyrics;


// ===========================================================================
// 本地歌词缓存（条目语义；磁盘读写见 modules/storage.rs）
// ===========================================================================

/// 一条歌词缓存：上次生效的歌词 + 抓取到的全部候选。
///
/// - `selected` = 当前生效歌词（自动抓取结果或用户在「歌词选择」弹窗的手选），
///   下次播放同曲直接应用，**零网络请求**；
/// - `candidates` 存全部候选原文，重启后「歌词选择」弹窗仍可切换；
/// - `saved_at_unix` 仅供排查，不参与过期判断。
///
/// 缓存按 bvid 的 md5 键控（与音频缓存同方案），整表序列化为
/// `~/.cache/simple-music/lyrics.json`。坏文件静默降级为缓存未命中。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LyricsCacheEntry {
    /// 当前生效（上次使用）的歌词。
    #[serde(default)]
    pub selected: Option<Lyrics>,
    /// 全部歌词候选（供「歌词选择」弹窗；与 selected 一起落盘）。
    #[serde(default)]
    pub candidates: Vec<Lyrics>,
    /// 落盘时间（Unix 秒）。
    #[serde(default)]
    pub saved_at_unix: u64,
}

/// 用 bvid 生成缓存键（复用音频缓存的 md5 键控方案）。
pub fn cache_key(bvid: &str) -> String {
    crate::modules::bilibili::md5_hex(bvid)
}

/// 单曲读写接口：按 bvid 读缓存（无则 `None`）。
pub fn cache_lookup<'a>(
    cache: &'a BTreeMap<String, LyricsCacheEntry>,
    bvid: &str,
) -> Option<&'a LyricsCacheEntry> {
    cache.get(&cache_key(bvid))
}

/// 单曲写入接口：更新 `selected`（当前生效歌词），返回新 entry 供调用方存表。
pub fn cache_update_selected<'a>(
    cache: &'a mut BTreeMap<String, LyricsCacheEntry>,
    bvid: &str,
    selected: Lyrics,
) -> &'a mut LyricsCacheEntry {
    let key = cache_key(bvid);
    let entry = cache.entry(key).or_default();
    entry.selected = Some(selected);
    entry.saved_at_unix = now_unix();
    entry
}

/// 单曲写入接口：记录一次完整抓取结果（selected + candidates）。
pub fn cache_store_fetch(
    cache: &mut BTreeMap<String, LyricsCacheEntry>,
    bvid: &str,
    selected: Option<Lyrics>,
    candidates: Vec<Lyrics>,
) {
    let key = cache_key(bvid);
    let entry = cache.entry(key).or_default();
    entry.selected = selected;
    entry.candidates = candidates;
    entry.saved_at_unix = now_unix();
}

/// 当前 Unix 秒。
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::lyrics::model::{Lyrics, LrcSearchResult};

    fn sample_lyrics(tag: &str) -> Lyrics {
        Lyrics {
            lrc: Some(format!("[00:01.00]第一句{tag}\n[00:03.00]第二句{tag}")),
            plain: format!("第一句{tag}\n第二句{tag}"),
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
        }
    }

    #[test]
    fn lyrics_cache_key_is_stable_md5_of_bvid() {
        let k = cache_key("BV1GJ411x7h7");
        assert_eq!(k.len(), 32);
        assert_eq!(k, cache_key("BV1GJ411x7h7"));
        assert_ne!(k, cache_key("BV1xx411c7mD"));
    }

    #[test]
    fn lyrics_json_roundtrip_preserves_candidates() {
        let entry = LyricsCacheEntry {
            selected: Some(sample_lyrics("A")),
            candidates: vec![sample_lyrics("A"), sample_lyrics("B")],
            saved_at_unix: 1_700_000_000,
        };
        let text = serde_json::to_string(&entry).unwrap();
        let back: LyricsCacheEntry = serde_json::from_str(&text).unwrap();
        assert_eq!(back, entry);
        // 旧文件缺字段也能反序列化（serde default）。
        let bare: LyricsCacheEntry = serde_json::from_str("{}").unwrap();
        assert_eq!(bare, LyricsCacheEntry::default());
    }
}
