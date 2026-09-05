//! vkeys.cn 聚合源（QQ 音乐 / 网易云音乐歌词，中文歌曲覆盖率高）。


use super::model::{Lyrics, LrcSearchResult, SongHint};
use super::lrc;
use super::matching::best_match_if_acceptable;
use super::{
    MIN_ACCEPT_SCORE, VKEYS_NETEASE_LYRIC, VKEYS_NETEASE_SEARCH, VKEYS_QQ_LYRIC, VKEYS_QQ_SEARCH,
};

// ===========================================================================

/// vkeys 搜索响应：`data` 可能是数组（QQ）、单对象（网易）或 null。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct VkeySearchResp {
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// vkeys 歌词响应：`data` 内含 `lrc` / `trans`（QQ）/ `tlyric`（网易）。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct VkeyLyricResp {
    #[serde(default)]
    pub data: Option<VkeyLyricData>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct VkeyLyricData {
    /// 主歌词（可能是字符串，也可能是 `{"lyric": ".."}` 对象）。
    #[serde(default)]
    pub lrc: Option<LyricText>,
    /// QQ 翻译。
    #[serde(default)]
    pub trans: Option<LyricText>,
    /// 网易翻译（`{"lyric": ".."}`）。
    #[serde(default)]
    pub tlyric: Option<LyricText>,
}

/// 歌词文本字段：兼容字符串与 `{"lyric": ".."}` 两种形态。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(untagged)]
pub enum LyricText {
    Str(String),
    Obj {
        #[serde(default)]
        lyric: Option<String>,
    },
}

impl LyricText {
    fn text(&self) -> String {
        match self {
            LyricText::Str(s) => s.trim().to_string(),
            LyricText::Obj { lyric } => lyric.as_deref().unwrap_or("").trim().to_string(),
        }
    }
}

/// 数据源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum VkSource {
    /// QQ 音乐（priority 1）。
    Qq,
    /// 网易云音乐（priority 0）。
    Netease,
}

impl VkSource {
    fn search_url(&self) -> &'static str {
        match self {
            VkSource::Qq => VKEYS_QQ_SEARCH,
            VkSource::Netease => VKEYS_NETEASE_SEARCH,
        }
    }

    fn lyric_url(&self) -> &'static str {
        match self {
            VkSource::Qq => VKEYS_QQ_LYRIC,
            VkSource::Netease => VKEYS_NETEASE_LYRIC,
        }
    }

    /// 取歌词时用的 id 参数名：QQ 用 `mid`，网易用 `id`。
    fn id_param(&self) -> &'static str {
        match self {
            VkSource::Qq => "mid",
            VkSource::Netease => "id",
        }
    }
}

/// 从 vkeys 单个源搜索并取回歌词；未命中/无歌词返回 `None`。
#[allow(clippy::too_many_arguments)]
pub(super) fn vkeys_source_fetch(
    client: &reqwest::blocking::Client,
    src: VkSource,
    query: &str,
    title: &str,
    uploader: &str,
    hint: Option<&SongHint>,
) -> Option<Lyrics> {
    let resp = client
        .get(src.search_url())
        .query(&[("word", query), ("page", "1"), ("num", "8")])
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let items = vkeys_extract_items(&resp.json::<VkeySearchResp>().ok()?);
    if items.is_empty() {
        return None;
    }
    let candidates: Vec<LrcSearchResult> = items
        .iter()
        .filter_map(|it| vkey_item_to_candidate(src, it))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let (best_idx, best) =
        best_match_if_acceptable(&candidates, title, uploader, hint, MIN_ACCEPT_SCORE)?;
    let best_id = vkey_item_id(src, &items[best_idx])?;
    let lyric = vkeys_lyric_fetch(client, src, &best_id)?;
    let mut ly = build_vkey_lyrics(lyric)?;
    // 带上候选元信息（用于「歌词选择」弹窗显示曲名/歌手）。
    let mut meta = best.clone();
    if meta.album_name.is_empty() {
        meta.album_name = match src {
            VkSource::Qq => "QQ音乐".to_string(),
            VkSource::Netease => "网易云".to_string(),
        };
    }
    ly.source = Some(meta);
    Some(ly)
}

/// vkeys 搜索响应 → 歌曲条目数组（`data` 数组 / 单对象 / 空）。
fn vkeys_extract_items(resp: &VkeySearchResp) -> Vec<serde_json::Value> {
    match &resp.data {
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        Some(v @ serde_json::Value::Object(_)) => vec![v.clone()],
        _ => Vec::new(),
    }
}

/// 取歌曲条目 id：QQ 用 `mid`（字符串），网易用 `id`（数字或字符串）。
fn vkey_item_id(src: VkSource, item: &serde_json::Value) -> Option<String> {
    let key = match src {
        VkSource::Qq => "mid",
        VkSource::Netease => "id",
    };
    match item.get(key) {
        Some(v) if v.is_string() => v.as_str().map(|s| s.to_string()),
        Some(v) if v.is_number() => v.as_i64().map(|n| n.to_string()),
        _ => None,
    }
}

/// 取歌曲标题：按常见字段名依次探测（vkeys 实际返回 `song`）。
fn vkey_item_title(item: &serde_json::Value) -> String {
    for k in ["song", "name", "title", "songname", "songName"] {
        if let Some(s) = item.get(k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

/// 取歌手：按常见字段名依次探测（字符串或数组；QQ 返回 `singer` 字符串
/// 且带 `singer_list` 数组，网易返回 `singer` 字符串）。
fn vkey_item_artist(item: &serde_json::Value) -> String {
    for k in ["singer", "singers", "singer_list", "singerList", "artist", "artists"] {
        if let Some(v) = item.get(k) {
            let s = flatten_names(v);
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}

/// 把歌手字段压平为 "A / B"：字符串直接用；数组取每项 `name` 或字符串元素。
fn flatten_names(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Array(arr) => {
            let names: Vec<String> = arr
                .iter()
                .filter_map(|it| {
                    if let Some(s) = it.as_str() {
                        Some(s.trim().to_string())
                    } else if let Some(s) = it.get("name").and_then(|n| n.as_str()) {
                        Some(s.trim().to_string())
                    } else {
                        None
                    }
                })
                .filter(|s| !s.is_empty())
                .collect();
            names.join(" / ")
        }
        _ => String::new(),
    }
}

/// 取时长（秒）。支持：
/// - 数字毫秒（`duration`/`dt`，>1000 时自动÷1000）
/// - 中文 interval 如 `"4分29秒"`（QQ 音乐返回格式）
fn vkey_item_duration_secs(item: &serde_json::Value) -> f64 {
    for k in ["duration", "dt"] {
        let secs = match item.get(k) {
            Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(serde_json::Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
            _ => 0.0,
        };
        if secs > 0.0 {
            return if secs > 1000.0 { secs / 1000.0 } else { secs };
        }
    }
    // QQ 音乐用中文 interval 如 "4分29秒" 或 "3分" 或 "45秒"
    if let Some(interval) = item.get("interval").and_then(|v| v.as_str()) {
        if let Some(secs) = parse_cn_interval(interval) {
            return secs;
        }
    }
    0.0
}

/// 解析中文时长格式（"4分29秒" / "3分" / "45秒"）。
fn parse_cn_interval(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0f64;
    if let Some(pos) = s.find('分') {
        let mins: f64 = s[..pos].trim().parse().ok()?;
        total += mins * 60.0;
        let rest = &s[(pos + '分'.len_utf8())..];
        if let Some(s2) = rest.find('秒') {
            let secs: f64 = rest[..s2].trim().parse().ok()?;
            total += secs;
        }
        return Some(total);
    }
    if let Some(pos) = s.find('秒') {
        let secs: f64 = s[..pos].trim().parse().ok()?;
        return Some(secs);
    }
    None
}

/// vkeys 条目 → 候选（复用标题/歌手匹配打分）。
fn vkey_item_to_candidate(src: VkSource, item: &serde_json::Value) -> Option<LrcSearchResult> {
    let id = vkey_item_id(src, item)?;
    let title = vkey_item_title(item);
    if title.is_empty() {
        return None;
    }
    Some(LrcSearchResult {
        id: id.parse().unwrap_or(0),
        track_name: title,
        artist_name: vkey_item_artist(item),
        album_name: String::new(),
        duration: vkey_item_duration_secs(item),
        instrumental: false,
        plain_lyrics: String::new(),
        synced_lyrics: String::new(),
    })
}

/// 拉取歌词文本（`mid` / `id`）。
pub(super) fn vkeys_lyric_fetch(
    client: &reqwest::blocking::Client,
    src: VkSource,
    id: &str,
) -> Option<VkeyLyricData> {
    let resp = client
        .get(src.lyric_url())
        .query(&[(src.id_param(), id)])
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<VkeyLyricResp>().ok()?.data
}

/// 把 vkeys 歌词数据打包成 [`Lyrics`]（合并翻译歌词）。
fn build_vkey_lyrics(data: VkeyLyricData) -> Option<Lyrics> {
    let lrc = data.lrc.as_ref().map(LyricText::text).unwrap_or_default();
    let trans = data
        .trans
        .as_ref()
        .map(LyricText::text)
        .or_else(|| data.tlyric.as_ref().map(LyricText::text))
        .unwrap_or_default();
    let (merged_lrc, plain) = merge_lrc_translation(&lrc, &trans);
    if merged_lrc.is_empty() && plain.is_empty() {
        return None;
    }
    Some(Lyrics {
        lrc: if merged_lrc.is_empty() { None } else { Some(merged_lrc) },
        plain,
        source: None,
    })
}

/// 秒 → `[mm:ss.xx]` LRC 时间标签。
fn fmt_lrc_time(secs: f64) -> String {
    let m = (secs / 60.0).floor() as u64;
    let s = secs - m as f64 * 60.0;
    format!("[{:02}:{:05.2}]", m, s)
}

/// 把翻译歌词按时间戳并入主歌词：同一句时间相差 ≤0.5s 视为对应，
/// 输出「主句\n翻译」同行（桌面歌词可整句显示）。返回 (合并 LRC, 纯文本)。
///
/// 主歌词为空 → 全部为空；翻译无时间标签（纯文本）→ 不合并，仅保留主歌词。
fn merge_lrc_translation(lrc: &str, trans: &str) -> (String, String) {
    let main = lrc::parse(lrc);
    if main.is_empty() {
        return (String::new(), String::new());
    }
    let tr = lrc::parse(trans);
    let merged_lrc: Vec<String> = main
        .iter()
        .map(|l| {
            let tr_text = tr
                .iter()
                .filter(|t| (t.time_secs - l.time_secs).abs() <= 0.5)
                .map(|t| t.text.trim())
                .find(|t| !t.is_empty() && *t != l.text.trim())
                .unwrap_or("");
            let text = if tr_text.is_empty() {
                l.text.clone()
            } else {
                format!("{}\n{}", l.text, tr_text)
            };
            format!("{}{}", fmt_lrc_time(l.time_secs), text)
        })
        .collect();
    let merged = merged_lrc.join("\n");
    let plain = merged
        .lines()
        .map(lrc::plain_line)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (merged, plain)
}

// ===========================================================================
// 小工具
// ===========================================================================

/// 提取书名号《…》内文本（取第一个）；无则 `None`。

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn vkey_lyric_text_untagged_string() {
        let v: LyricText = serde_json::from_str(r#""[00:01.00]故事的小黄花""#).unwrap();
        assert_eq!(v.text(), "[00:01.00]故事的小黄花");
    }

    #[test]
    fn vkey_lyric_text_untagged_object() {
        let v: LyricText = serde_json::from_str(r#"{"lyric":"[00:01.00]故事的小黄花"}"#).unwrap();
        assert_eq!(v.text(), "[00:01.00]故事的小黄花");
    }

    #[test]
    fn vkey_search_qq_parse_array() {
        let json = r#"{"data": [
            {"mid":"003a1uRx2cRwY1","name":"晴天","singer":[{"id":4558,"mid":"...","name":"周杰伦"}],"duration":269000}
        ]}"#;
        let resp: VkeySearchResp = serde_json::from_str(json).unwrap();
        let items = vkeys_extract_items(&resp);
        assert_eq!(items.len(), 1);
        let id = vkey_item_id(VkSource::Qq, &items[0]).unwrap();
        assert_eq!(id, "003a1uRx2cRwY1");
        let title = vkey_item_title(&items[0]);
        assert_eq!(title, "晴天");
        let artist = vkey_item_artist(&items[0]);
        assert_eq!(artist, "周杰伦");
        let dur = vkey_item_duration_secs(&items[0]);
        assert!((dur - 269.0).abs() < 1.0, "got {dur}");
    }

    #[test]
    fn vkey_search_netease_parse_array() {
        let json = r#"{"data": [
            {"id":186016,"name":"晴天","artists":[{"id":6452,"name":"周杰伦"}],"duration":269000}
        ]}"#;
        let resp: VkeySearchResp = serde_json::from_str(json).unwrap();
        let items = vkeys_extract_items(&resp);
        assert_eq!(items.len(), 1);
        let id = vkey_item_id(VkSource::Netease, &items[0]).unwrap();
        assert_eq!(id, "186016");
        assert_eq!(vkey_item_title(&items[0]), "晴天");
        assert_eq!(vkey_item_artist(&items[0]), "周杰伦");
    }

    #[test]
    fn vkey_search_netease_single_object() {
        let json = r#"{"data": {"id":7,"name":"夜曲","duration":200000}}"#;
        let resp: VkeySearchResp = serde_json::from_str(json).unwrap();
        let items = vkeys_extract_items(&resp);
        assert_eq!(items.len(), 1);
        assert_eq!(vkey_item_title(&items[0]), "夜曲");
    }

    #[test]
    fn vkey_item_artist_string_singer() {
        let item: serde_json::Value = serde_json::from_str(r#"{"id":1,"singer":"周杰伦"}"#).unwrap();
        assert_eq!(vkey_item_artist(&item), "周杰伦");
    }

    #[test]
    fn vkey_item_artist_empty_when_missing() {
        let item: serde_json::Value = serde_json::from_str(r#"{"id":1}"#).unwrap();
        assert_eq!(vkey_item_artist(&item), "");
    }

    #[test]
    fn vkey_item_duration_millis_converted() {
        let item: serde_json::Value = serde_json::from_str(r#"{"duration":269000}"#).unwrap();
        let dur = vkey_item_duration_secs(&item);
        assert!((dur - 269.0).abs() < 1.0, "got {dur}");
    }

    #[test]
    fn vkey_item_duration_seconds_kept() {
        let item: serde_json::Value = serde_json::from_str(r#"{"duration":240.0}"#).unwrap();
        let dur = vkey_item_duration_secs(&item);
        assert!((dur - 240.0).abs() < 0.1, "got {dur}");
    }

    #[test]
    fn vkey_item_to_candidate_qq() {
        let item: serde_json::Value = serde_json::from_str(
            r#"{"mid":"abc","name":"晴天","singer":[{"name":"周杰伦"}],"duration":269000}"#,
        )
        .unwrap();
        let cand = vkey_item_to_candidate(VkSource::Qq, &item).unwrap();
        assert_eq!(cand.track_name, "晴天");
        assert_eq!(cand.artist_name, "周杰伦");
        assert!((cand.duration - 269.0).abs() < 1.0);
    }

    #[test]
    fn vkey_merge_lrc_translation_aligns() {
        let lrc = "[00:01.00]故事的小黄花\n[00:03.00]从出生那年就飘着";
        let trans = "[00:01.00]The yellow flower\n[00:03.00]Floating since birth";
        let (merged, plain) = merge_lrc_translation(lrc, trans);
        // 合并后的 LRC 应该包含翻译文本
        assert!(merged.contains("The yellow flower"), "got: {merged}");
        assert!(merged.contains("Floating since birth"), "got: {merged}");
        // 纯文本也应包含两行
        assert!(plain.contains("故事的小黄花"));
        assert!(plain.contains("The yellow flower"));
    }

    #[test]
    fn vkey_merge_lrc_translation_empty_trans() {
        let lrc = "[00:01.00]a\n[00:02.00]b";
        let (merged, plain) = merge_lrc_translation(lrc, "");
        assert!(merged.contains("[00:01.00]a"));
        assert_eq!(plain, "a\nb");
    }

    #[test]
    fn vkey_merge_lrc_translation_empty_lrc() {
        let (merged, plain) = merge_lrc_translation("", "[00:01.00]trans");
        assert!(merged.is_empty());
        assert!(plain.is_empty());
    }

    #[test]
    fn flatten_names_string() {
        let v: serde_json::Value = serde_json::from_str(r#""周杰伦""#).unwrap();
        assert_eq!(flatten_names(&v), "周杰伦");
    }

    #[test]
    fn flatten_names_array() {
        let v: serde_json::Value =
            serde_json::from_str(r#"[{"name":"周杰伦"},{"name":"方文山"}]"#).unwrap();
        assert_eq!(flatten_names(&v), "周杰伦 / 方文山");
    }

    #[test]
    fn flatten_names_array_of_strings() {
        let v: serde_json::Value = serde_json::from_str(r#"["周杰伦","方文山"]"#).unwrap();
        assert_eq!(flatten_names(&v), "周杰伦 / 方文山");
    }

    #[test]
    fn fmt_lrc_time_formats_correctly() {
        assert_eq!(fmt_lrc_time(0.0), "[00:00.00]");
        assert_eq!(fmt_lrc_time(61.5), "[01:01.50]");
        assert_eq!(fmt_lrc_time(3661.0), "[61:01.00]");
    }

    #[test]
    fn vkey_lyric_data_parse_without_lrc_fallback() {
        // 只有 tlyric 没有 lrc 的场景
        let json = r#"{"data":{"tlyric":{"lyric":"翻译歌词"}}}"#;
        let resp: VkeyLyricResp = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert!(data.lrc.is_none());
        assert!(data.trans.is_none());
        assert!(data.tlyric.is_some());
        assert_eq!(data.tlyric.unwrap().text(), "翻译歌词");
    }

    #[test]
    fn lrc_plain_line_strips_tags() {
        assert_eq!(lrc::plain_line("[00:01.00]hello"), "hello");
        assert_eq!(lrc::plain_line("[00:01.00][00:03.00]aaa"), "aaa");
        assert_eq!(lrc::plain_line("[ti:title]"), "");
    }

    #[test]
    fn parse_cn_interval_min_sec() {
        assert!((parse_cn_interval("4分29秒").unwrap() - 269.0).abs() < 1.0);
    }

    #[test]
    fn parse_cn_interval_only_min() {
        assert!((parse_cn_interval("3分").unwrap() - 180.0).abs() < 0.1);
    }

    #[test]
    fn parse_cn_interval_only_sec() {
        assert!((parse_cn_interval("45秒").unwrap() - 45.0).abs() < 0.1);
    }

    #[test]
    fn parse_cn_interval_empty() {
        assert!(parse_cn_interval("").is_none());
    }

    #[test]
    fn vkey_item_title_extracts_song_field() {
        let item: serde_json::Value = serde_json::from_str(r#"{"song":"晴天","id":1}"#).unwrap();
        assert_eq!(vkey_item_title(&item), "晴天");
    }

    #[test]
    fn vkey_item_duration_parses_cn_interval() {
        let item: serde_json::Value =
            serde_json::from_str(r#"{"interval":"4分29秒","id":1}"#).unwrap();
        let dur = vkey_item_duration_secs(&item);
        assert!((dur - 269.0).abs() < 1.0, "got {dur}");
    }
}
