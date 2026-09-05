//! 数据模型：[`SongHint`]（识别音乐提示）、[`LrcLine`]、[`LrcSearchResult`]、[`Lyrics`]。



use super::lrc;


/// ① 生成比视频标题更准的查询词；② 校准候选打分（标题/歌手/时长匹配度）。
///
/// 全字段可选式：`None` 提示时行为与旧版完全一致（按 title/uploader 搜索）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SongHint {
    /// 官方曲名（如 "Unwelcome School"），来自曲库而非视频标题。
    pub title: String,
    /// 官方歌手名。
    pub artist: String,
    /// 视频实际时长（秒，来自 B 站稿件信息）：官方歌曲时长与视频时长接近时
    /// 强烈暗示候选正确（整曲/原曲向视频），差得远则可能是二创混剪。
    pub duration_secs: f64,
}

impl SongHint {
    /// 是否可用于生成查询（至少要有曲名）。
    pub fn has_query(&self) -> bool {
        !self.title.trim().is_empty()
    }
}

// ===========================================================================
// 数据模型
// ===========================================================================

/// 一行带时间轴的歌词。
#[derive(Debug, Clone, PartialEq)]
pub struct LrcLine {
    /// 这句开始的时间（秒）。
    pub time_secs: f64,
    /// 歌词文本。
    pub text: String,
}

/// LRCLIB 返回的一条歌词结果（搜索数组元素与 GET 单对象同构）。
///
/// 用宽松反序列化：缺失字段取默认值，避免结果只缺 `syncedLyrics` 时整条失败。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LrcSearchResult {
    #[serde(default)]
    pub id: u64,
    #[serde(rename = "trackName", default)]
    pub track_name: String,
    #[serde(rename = "artistName", default)]
    pub artist_name: String,
    #[serde(rename = "albumName", default)]
    pub album_name: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub instrumental: bool,
    #[serde(rename = "plainLyrics", default)]
    pub plain_lyrics: String,
    #[serde(rename = "syncedLyrics", default)]
    pub synced_lyrics: String,
}

/// `fetch` 的最终产物。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lyrics {
    /// 同步 LRC 原文（无同步时为 `None`）。
    pub lrc: Option<String>,
    /// 纯文本歌词（无时间标签），无同步歌词时的兜底展示。
    pub plain: String,
    /// 命中的来源元信息（LRCLIB 结果），用于展示所用专辑/艺术家等。
    pub source: Option<LrcSearchResult>,
}

impl Lyrics {
    /// 是否有同步（时间轴）歌词。
    pub fn has_synced(&self) -> bool {
        self.lrc.is_some()
    }

    /// 把同步歌词解析成时间轴行；无同步歌词时返回空。
    pub fn lrc_lines(&self) -> Vec<LrcLine> {
        match self.lrc.as_deref() {
            Some(l) => lrc::parse(l),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_deserialize_search_array() {
        let json = r#"[
          {"id":11,"trackName":"晴天","artistName":"周杰伦","albumName":"叶惠美",
           "duration":269,"instrumental":false,
           "plainLyrics":"故事的小黄花\n从出生那年就飘着",
           "syncedLyrics":"[00:01.00]故事的小黄花\n[00:03.00]从出生那年就飘着"}
        ]"#;
        let v: Vec<LrcSearchResult> = serde_json::from_str(json).expect("反序列化失败");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, 11);
        assert_eq!(v[0].track_name, "晴天");
        assert_eq!(v[0].artist_name, "周杰伦");
        assert_eq!(v[0].duration, 269.0);
        assert!(!v[0].instrumental);
        assert!(v[0].synced_lyrics.contains("故事的小黄花"));
    }

    #[test]
    fn json_deserialize_get_missing_optional_fields() {
        // GET 单对象，syncedLyrics 缺失（只有纯文本）：仍应解析成功，synced_lyrics 为空。
        let json = r#"{"id":22,"trackName":"晴天","artistName":"周杰伦",
            "albumName":"叶惠美","duration":269,"instrumental":false,
            "plainLyrics":"纯文本歌词"}"#;
        let v: LrcSearchResult = serde_json::from_str(json).expect("反序列化失败");
        assert_eq!(v.track_name, "晴天");
        assert_eq!(v.synced_lyrics, "");
        assert_eq!(v.plain_lyrics, "纯文本歌词");
    }

}
