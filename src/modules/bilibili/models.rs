//! B 站数据模型：对外数据结构（VideoInfo/StreamUrl/FavFolder…）与
//! API 响应的结构体（只取需要的字段，其余 serde 忽略）。
//!
//! 私有响应结构体一律 `pub(super)`：仅模块内 client/login/fav/resolve 使用。

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{REFERER, USER_AGENT};

// ---------------------------------------------------------------------------
// 数据模型（保持与骨架兼容：VideoInfo / StreamUrl / PlaySource 字段只增不改）
// ---------------------------------------------------------------------------

/// B 站视频条目。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VideoInfo {
    pub bvid: String,
    pub title: String,
    pub uploader: String,
    /// 视频时长（秒），用于展示与预估算位。
    pub duration_secs: f64,
    pub cover_url: Option<String>,
}

/// 解析出的媒体流直链。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StreamUrl {
    /// 音频流直链（B 站 DASH 通常音视频分离）。
    pub audio_url: String,
    /// 视频流直链（纯听歌场景可为 None）。
    pub video_url: Option<String>,
    /// 流过期时间提示（秒）。
    pub ttl_secs: u64,
    /// 以下为 v0.2 扩展字段（带 serde default，旧 JSON 仍可解析）：
    /// 选中的音频 DASH 流 id（如 30280 = 320kbps、30216 = 64kbps）。
    #[serde(default)]
    pub audio_id: Option<i64>,
    /// 音频编码（如 mp4a.40.2 / dolby / flac）。
    #[serde(default)]
    pub audio_codec: Option<String>,
    /// 音频码率（bps）。
    #[serde(default)]
    pub bandwidth: Option<i64>,
    /// 音频文件大小（字节，部分响应缺省）。
    #[serde(default)]
    pub size_bytes: Option<i64>,
    /// 音频备用 CDN 地址（base_url 403 时按序尝试）。
    #[serde(default)]
    pub audio_backup_urls: Vec<String>,
    /// 下载该流必须携带的 HTTP 头（见 README/返回报告），供音频 Worker 直接使用。
    #[serde(default)]
    pub required_headers: Vec<(String, String)>,
    /// 本次取流是否使用了 WBI 签名。
    #[serde(default)]
    pub signed_with_wbi: bool,
}

impl StreamUrl {
    /// 音频 Worker 下载时必须携带的请求头：
    /// - `User-Agent`: 桌面 Chrome UA（[`USER_AGENT`]）
    /// - `Referer`: `https://www.bilibili.com/`
    /// - 可选 `Cookie`: buvid3 等（未登录取流即可，带上更稳）
    /// - 可用 `Range: bytes=0-...` 断点/分段下载（CDN 支持 206）
    pub(super) fn build_required_headers(cookie_header: &str) -> Vec<(String, String)> {
        let mut v = vec![
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            ("Referer".to_string(), REFERER.to_string()),
        ];
        if !cookie_header.is_empty() {
            v.push(("Cookie".to_string(), cookie_header.to_string()));
        }
        v
    }
}

/// 收藏夹（文件夹）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FavFolder {
    /// media_id（后面 list_favorite_resources 的参数）。
    pub id: i64,
    pub title: String,
    pub media_count: i64,
}

/// 收藏夹里的资源条目。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FavItem {
    pub bvid: String,
    pub title: String,
    pub owner: String,
    pub duration_secs: f64,
    pub cover_url: Option<String>,
}

/// 视频详情：展示信息 + 取流必需的 cid。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VideoDetail {
    pub info: VideoInfo,
    /// 第一分 P 的 cid（playurl 必需）。
    pub cid: i64,
    /// 总分 P 数（多 P 视频取流需按 p 逐个换 cid，当前只处理 P1）。
    pub pages: u64,
}

/// B 站「识别音乐」提示：来自官方曲库的版权/背景音乐标注，比视频标题干净得多。
///
/// 来源接口（按可靠性排序，`detect_music` 依次探测）：
/// 1. `/x/player/v2` 的 `bgm_info`（UP 主挂载的官方 BGM 卡片，music_id 形如 `MA…`）；
/// 2. `/x/web-interface/view/detail/tag` 中 `tag_type == "bgm"` 的 TAG
///    （UP 主投稿时选择的「识别音乐/BGM」标签，同样带 `MA…` music_id）。
///
/// 两者都拿到 music_id 后再调 `/x/copyright-music-publicity/bgm/detail`（B 站音乐
/// 开放平台曲库，无需登录、未风控）换取**官方曲名 + 歌手 + 专辑**——这正是歌词搜索
/// 缺失的高质量查询词：视频标题常带「【4K】【燃剪】xxx 4K修复版」之类噪音，而这里的
/// `music_title`/`origin_artist` 是曲库标准名。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MusicHint {
    /// 官方曲库歌曲名（如 "Unwelcome School"）。
    pub title: String,
    /// 歌手名（取 `origin_artist`，多个以 ` / ` 连接 `artists_list`）。
    pub artist: String,
    /// 专辑名（常与曲名相同，可为空）。
    pub album: String,
    /// 曲库 music_id（`MA…`，空表示未识别）。
    pub music_id: String,
}

impl MusicHint {
    /// 是否拿到了可用的识别结果（至少要有曲名）。
    pub fn is_usable(&self) -> bool {
        !self.title.trim().is_empty()
    }
}

// ---------------------------------------------------------------------------
// API 响应模型（只取需要的字段，其余忽略）
// ---------------------------------------------------------------------------

/// B 站标准响应信封 `{code, message, data}`（公开仅为可见性，字段内部使用）。
#[derive(Debug, Deserialize)]
pub struct ApiEnvelope<T> {
    pub(super) code: i64,
    #[serde(default)]
    pub(super) message: String,
    pub(super) data: Option<T>,
}

#[derive(Debug, Deserialize)]
pub(super) struct ViewResp {
    pub(super) bvid: String,
    #[serde(default)]
    pub(super) cid: i64,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) pic: String,
    #[serde(default)]
    pub(super) duration: i64,
    #[serde(default)]
    pub(super) videos: i64,
    pub(super) owner: Owner,
}

/// `/x/player/v2` 响应：只取 `bgm_info`（识别音乐卡），其余忽略。
#[derive(Debug, Deserialize)]
pub(super) struct PlayerInfoResp {
    #[serde(default)]
    pub(super) bgm_info: Option<PlayerBgmInfo>,
}

/// `bgm_info`：UP 主挂载的官方 BGM 信息（music_id 形如 `MA…`）。
#[derive(Debug, Deserialize)]
pub(super) struct PlayerBgmInfo {
    #[serde(default)]
    pub(super) music_id: String,
}

/// `view/detail/tag` 数组元素：普通 TAG 与 BGM TAG 同构，`music_id` 仅 bgm 有效。
#[derive(Debug, Deserialize)]
pub(super) struct BgmTagItem {
    #[serde(default)]
    pub(super) music_id: String,
    #[serde(default)]
    pub(super) tag_type: String,
}

/// B 站人名字段：兼容字符串与 `{"name": ".."}` 对象两种形态
/// （`bgm/detail` 的 `artists_list` 返回对象数组）。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(untagged)]
pub enum BiliNameValue {
    Str(String),
    Obj {
        #[serde(default)]
        name: Option<String>,
    },
}

impl BiliNameValue {
    fn text(&self) -> String {
        match self {
            BiliNameValue::Str(s) => s.trim().to_string(),
            BiliNameValue::Obj { name } => name.as_deref().unwrap_or("").trim().to_string(),
        }
    }
}

/// 音乐开放平台 `bgm/detail` 响应：官方曲名/歌手/专辑。
#[derive(Debug, Deserialize)]
pub(super) struct CopyrightMusicDetail {
    #[serde(default)]
    pub(super) music_title: String,
    #[serde(default)]
    pub(super) album: String,
    /// 原曲歌手（官方展示名，常为本地化写法如 "ミツキヨ"）。
    #[serde(default)]
    pub(super) origin_artist: String,
    /// `origin_artist` 为空时的兜底：歌手名列表（按官方原始写法）。
    #[serde(default)]
    pub(super) artists_list: Vec<BiliNameValue>,
}

impl CopyrightMusicDetail {
    /// 展示用歌手名：优先 `origin_artist`，否则压平 `artists_list` 为 "A / B"。
    pub(super) fn artist_display(&self) -> String {
        if !self.origin_artist.trim().is_empty() {
            return self.origin_artist.trim().to_string();
        }
        self.artists_list
            .iter()
            .map(BiliNameValue::text)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" / ")
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct Owner {
    pub(super) name: String,
}

/// playurl 原始响应（公开供诊断/probe 使用）。
#[derive(Debug, Deserialize)]
pub struct PlayUrlResp {
    pub code: i64,
    #[serde(default)]
    pub message: String,
    pub data: Option<PlayUrlData>,
}

/// playurl 的 data 段（公开供诊断/probe 使用）。
#[derive(Debug, Deserialize)]
pub struct PlayUrlData {
    #[serde(default)]
    pub timelength: i64,
    #[serde(default)]
    pub dash: Option<Dash>,
    #[serde(default)]
    pub durl: Option<Vec<DurlEntry>>,
}

/// DASH 段（公开供诊断/probe 使用）。
#[derive(Debug, Deserialize)]
pub struct Dash {
    #[serde(default)]
    pub video: Vec<DashStream>,
    #[serde(default)]
    pub audio: Vec<DashStream>,
}

/// DASH 流条目（公开供诊断/probe 使用）。
/// 响应里 `baseUrl`/`base_url`（以及 backupUrl/backup_url）双写并存，
/// 经 [`DashStreamRaw`] 捕获后合并，`#[serde(from)]` 保证两键任一存在即可解析。
#[derive(Debug, Clone, Deserialize)]
#[serde(from = "DashStreamRaw")]
pub struct DashStream {
    #[serde(default)]
    pub id: i64,
    #[serde(alias = "base_url", rename = "baseUrl")]
    pub base_url: String,
    #[serde(alias = "backup_url", rename = "backupUrl", default)]
    pub backup_url: Vec<String>,
    #[serde(default)]
    pub bandwidth: i64,
    #[serde(default)]
    pub codecs: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
}

// 实际响应同时包含 `baseUrl` 与 `base_url`（B 站双写），serde 对 rename+alias 的
// 同字段映射会报 duplicate field，所以先用 Option 原样捕获再合并。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct DashStreamRaw {
    pub(super) id: i64,
    /// JSON 键 `baseUrl`（camel，现行响应）。
    #[serde(rename = "baseUrl")]
    pub(super) base_url: Option<String>,
    /// JSON 键 `base_url`（snake，双写/老格式）。
    #[serde(rename = "base_url")]
    pub(super) base_url_snake: Option<String>,
    /// JSON 键 `backupUrl`。
    #[serde(rename = "backupUrl")]
    pub(super) backup_url: Option<Vec<String>>,
    /// JSON 键 `backup_url`。
    #[serde(rename = "backup_url")]
    pub(super) backup_url_snake: Option<Vec<String>>,
    pub(super) bandwidth: i64,
    pub(super) codecs: Option<String>,
    pub(super) size: Option<i64>,
}

impl From<DashStreamRaw> for DashStream {
    fn from(r: DashStreamRaw) -> Self {
        let base_url = r
            .base_url
            .filter(|s| !s.is_empty())
            .or(r.base_url_snake.filter(|s| !s.is_empty()))
            .unwrap_or_default();
        let backup_url = match (r.backup_url.filter(|v| !v.is_empty()), r.backup_url_snake.filter(|v| !v.is_empty())) {
            (Some(a), Some(b)) => {
                let mut v = a;
                v.extend(b);
                v
            }
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => Vec::new(),
        };
        Self {
            id: r.id,
            base_url,
            backup_url,
            bandwidth: r.bandwidth,
            codecs: r.codecs,
            size: r.size,
        }
    }
}

/// durl 老格式条目（公开供诊断/probe 使用）。
#[derive(Debug, Deserialize)]
pub struct DurlEntry {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub size: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct QrGenerateResp {
    #[serde(default)]
    pub(super) qrcode_key: String,
    #[serde(default)]
    pub(super) url: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct QrPollResp {
    #[serde(default)]
    pub(super) code: i64,
    #[serde(default)]
    pub(super) url: String,
    #[serde(default)]
    pub(super) message: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct NavResp {
    #[serde(default)]
    pub(super) wbi_img: WbiImg,
    /// 用户 mid（未登录时缺省为 0）。
    #[serde(default)]
    pub(super) mid: u64,
    /// 用户昵称（未登录时缺省为空）。
    #[serde(default)]
    pub(super) uname: String,
    /// 用户头像 URL。
    #[serde(default)]
    pub(super) face: String,
}

/// 当前登录用户信息（nav 接口）。
#[derive(Debug, Clone, PartialEq)]
pub struct NavUser {
    pub mid: u64,
    pub uname: String,
    pub face: String,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct WbiImg {
    #[serde(default)]
    pub(super) img_url: String,
    #[serde(default)]
    pub(super) sub_url: String,
}

/// 收藏夹分页响应（created/list 与 collected/list 同构）。
#[derive(Debug, Default, Deserialize)]
pub(super) struct FolderListResp {
    #[serde(default)]
    pub(super) list: Vec<FolderEntry>,
    /// 是否还有下一页（没有该字段时按 false 处理，单页即止）。
    #[serde(default)]
    pub(super) has_more: bool,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct FolderEntry {
    #[serde(default)]
    pub(super) id: i64,
    #[serde(default)]
    pub(super) title: String,
    #[serde(rename = "media_count", alias = "mediaCount", default)]
    pub(super) media_count: i64,
}

#[derive(Debug, Deserialize)]
pub(super) struct ResourceListResp {
    /// 收藏夹元信息（media_count 在这里，v3 接口如此）。
    #[serde(default)]
    pub(super) info: FolderEntry,
    #[serde(default)]
    pub(super) medias: Vec<MediaEntry>,
}

#[derive(Debug, Deserialize)]
pub(super) struct MediaEntry {
    #[serde(default)]
    pub(super) bvid: String,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) cover: String,
    #[serde(default)]
    pub(super) duration: i64,
    pub(super) upper: Owner,
}

// ---------------------------------------------------------------------------
// 扫码登录
// ---------------------------------------------------------------------------

/// 扫码登录第一步：`generate_qrcode()` 的返回。
#[derive(Debug, Clone, PartialEq)]
pub struct QrLoginStart {
    /// 轮询用的 key（有效期约 180 秒）。
    pub qrcode_key: String,
    /// 二维码内容（B 站登录页 URL），喂给 [`BiliClient::qrcode_matrix`] 或外部扫码器。
    pub url: String,
}

/// 扫码登录轮询状态。
#[derive(Debug, Clone, PartialEq)]
pub enum QrPoll {
    /// 86101：二维码生成，等待用户扫码。
    WaitingScan,
    /// 86090：已扫码，等待用户在手机上确认。
    WaitingConfirm,
    /// 86038：二维码已过期，需重新 generate。
    Expired,
    /// 0：登录成功，cookies 已捕获（SESSDATA/bili_jct/DedeUserID 等）。
    Success {
        /// 已合并捕获的 cookies（调用方还应持久化）。
        cookies: BTreeMap<String, String>,
        /// 用户 mid（DedeUserID）。
        mid: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::bilibili::client::BiliClient;
    use crate::modules::bilibili::client::parse_query_params;


    #[test]
    fn player_info_resp_parses_bgm_info() {
        let json = r#"{"code":0,"message":"OK","data":{
            "bgm_info":{"music_id":"MA436038343856245020",
            "music_title":"Unwelcome school","jump_url":"https://x"}}}"#;
        let env: ApiEnvelope<PlayerInfoResp> = serde_json::from_str(json).unwrap();
        let bgm = env.data.unwrap().bgm_info.unwrap();
        assert_eq!(bgm.music_id, "MA436038343856245020");
    }

    #[test]
    fn player_info_resp_without_bgm_is_none() {
        let json = r#"{"code":0,"message":"OK","data":{"aid":80433022}}"#;
        let env: ApiEnvelope<PlayerInfoResp> = serde_json::from_str(json).unwrap();
        assert!(env.data.unwrap().bgm_info.is_none());
    }

    #[test]
    fn bgm_tag_picks_tag_type_bgm_music_id() {
        let json = r#"{"code":0,"message":"OK","data":[
            {"tag_id":15223081,"tag_name":"Never Gonna Give You Up","music_id":"","tag_type":"old_channel","jump_url":""},
            {"tag_id":1,"tag_name":"被发现的神曲","music_id":"MA456128506519140428","tag_type":"bgm","jump_url":"https://x"}
        ]}"#;
        let env: ApiEnvelope<Vec<BgmTagItem>> = serde_json::from_str(json).unwrap();
        let picked = env
            .data
            .unwrap()
            .into_iter()
            .filter(|t| t.tag_type == "bgm")
            .find_map(|t| BiliClient::valid_music_id(&t.music_id))
            .unwrap();
        assert_eq!(picked, "MA456128506519140428");
    }

    #[test]
    fn copyright_music_detail_parse_and_artist_fallback() {
        // origin_artist 存在：直接用。
        let json = r#"{"code":0,"message":"0","data":{
            "music_title":"Unwelcome School","album":"Unwelcome School",
            "origin_artist":"ミツキヨ",
            "artists_list":[{"mid":1,"name":"ミツキヨ","identity":"演唱者"}]}}"#;
        let env: ApiEnvelope<CopyrightMusicDetail> = serde_json::from_str(json).unwrap();
        let d = env.data.unwrap();
        assert_eq!(d.music_title, "Unwelcome School");
        assert_eq!(d.artist_display(), "ミツキヨ");

        // origin_artist 为空：压平 artists_list（对象数组）。
        let json2 = r#"{"code":0,"data":{"music_title":"富士山下",
            "origin_artist":"","artists_list":[{"name":"陈奕迅"},{"name":"泽日生"}]}}"#;
        let env2: ApiEnvelope<CopyrightMusicDetail> = serde_json::from_str(json2).unwrap();
        assert_eq!(env2.data.unwrap().artist_display(), "陈奕迅 / 泽日生");

        // artists_list 为字符串数组也可。
        let json3 = r#"{"code":0,"data":{"origin_artist":"","artists_list":["A","B"]}}"#;
        let env3: ApiEnvelope<CopyrightMusicDetail> = serde_json::from_str(json3).unwrap();
        assert_eq!(env3.data.unwrap().artist_display(), "A / B");
    }

    #[test]
    fn music_hint_usable_requires_title() {
        let mut h = MusicHint::default();
        assert!(!h.is_usable());
        h.title = "晴天".into();
        assert!(h.is_usable());
    }

    // ---- JSON 模型反序列化（真实响应样例，节选自抓包） ----

    #[test]
    fn test_deserialize_view_resp() {
        let body = r#"{
            "code": 0, "message": "0", "ttl": 1,
            "data": {
                "bvid": "BV1xx411c7mD", "aid": 2, "videos": 1,
                "title": "字幕君交流场所",
                "pic": "https://i0.hdslb.com/bfs/archive/transparent.png",
                "duration": 2055,
                "cid": 62131,
                "owner": { "mid": 2, "name": "碧诗", "face": "https://i0.hdslb.com/bfs/face/x.jpg" }
            }
        }"#;
        let env: ApiEnvelope<ViewResp> = serde_json::from_str(body).expect("view 解析失败");
        assert_eq!(env.code, 0);
        let d = env.data.unwrap();
        assert_eq!(d.bvid, "BV1xx411c7mD");
        assert_eq!(d.cid, 62131);
        assert_eq!(d.title, "字幕君交流场所");
        assert_eq!(d.owner.name, "碧诗");
        assert_eq!(d.duration, 2055);
    }

    #[test]
    fn test_deserialize_playurl_dash() {
        // 真实响应结构节选（dash 音视频分离）。
        let body = r#"{
            "code": 0, "message": "OK", "ttl": 1,
            "data": {
                "from": "local", "result": "suee", "quality": 32,
                "format": "flv480", "timelength": 2055637,
                "accept_quality": [32, 16],
                "dash": {
                    "duration": 2056, "minBufferTime": 1.5,
                    "video": [{
                        "id": 32, "baseUrl": "https://upos.example.com/v.m4s?e=abc",
                        "backupUrl": ["https://backup.example.com/v.m4s"],
                        "bandwidth": 500000, "codecid": 7,
                        "codecs": "avc1.64001F", "size": 123456789
                    }],
                    "audio": [
                        { "id": 30216, "baseUrl": "https://upos.example.com/a64.m4s?e=abc",
                          "backupUrl": [], "bandwidth": 69000, "codecid": 12,
                          "codecs": "mp4a.40.5", "size": 17000000 },
                        { "id": 30232, "baseUrl": "https://upos.example.com/a128.m4s?e=abc",
                          "backupUrl": ["https://backup.example.com/a128.m4s"],
                          "bandwidth": 155622, "codecid": 12,
                          "codecs": "mp4a.40.2", "size": 40000000 }
                    ]
                }
            }
        }"#;
        let env: ApiEnvelope<PlayUrlData> = serde_json::from_str(body).expect("playurl 解析失败");
        let data = env.data.unwrap();
        let audio = &data.dash.as_ref().unwrap().audio;
        assert_eq!(audio.len(), 2);
        let best = audio.iter().max_by_key(|a| a.bandwidth).unwrap();
        assert_eq!(best.id, 30232);
        assert_eq!(best.codecs.as_deref(), Some("mp4a.40.2"));
        assert_eq!(best.backup_url, vec!["https://backup.example.com/a128.m4s"]);
        // base_url 别名兼容（老字段名）。
        let lower: ApiEnvelope<PlayUrlData> =
            serde_json::from_str(body.replace("baseUrl", "base_url").replace("backupUrl", "backup_url").as_str())
                .expect("snake_case 字段应可解析");
        assert_eq!(
            lower.data.unwrap().dash.unwrap().audio[0].base_url,
            "https://upos.example.com/a64.m4s?e=abc"
        );
    }

    #[test]
    fn test_deserialize_playurl_durl_fallback() {
        // 老格式（durl 混合流，无 dash）。
        let body = r#"{
            "code": 0, "message": "OK",
            "data": { "timelength": 60000, "quality": 16, "durl": [
                { "url": "https://upos.example.com/mixed.flv", "size": 8000000, "length": 60000 }
            ]}
        }"#;
        let env: ApiEnvelope<PlayUrlData> = serde_json::from_str(body).unwrap();
        let data = env.data.unwrap();
        assert!(data.dash.is_none());
        assert_eq!(data.durl.unwrap()[0].url, "https://upos.example.com/mixed.flv");
    }

    #[test]
    fn test_deserialize_qrcode_and_fav_models() {
        let gen_body = r#"{"code":0,"message":"0","ttl":1,"data":{"url":"https://passport.bilibili.com/h5-app/passport/login/scan?navhide=1&qrcode_key=abc123","qrcode_key":"abc123"}}"#;
        let env: ApiEnvelope<QrGenerateResp> = serde_json::from_str(gen_body).unwrap();
        let d = env.data.unwrap();
        assert_eq!(d.qrcode_key, "abc123");
        assert!(d.url.contains("qrcode_key=abc123"));

        let poll = r#"{"code":0,"message":"0","ttl":1,"data":{"url":"https://passport.biligame.com/crossDomain?DedeUserID=123&DedeUserID__ckMd5=abc&Expires=86400&SESSDATA=sessdata%2Cenc&bili_jct=jct&gourl=x","refresh_token":"rt","timestamp":0,"code":86101,"message":"未扫描"}}"#;
        let env: ApiEnvelope<QrPollResp> = serde_json::from_str(poll).unwrap();
        let d = env.data.unwrap();
        assert_eq!(d.code, 86101);
        let params = parse_query_params(&d.url);
        assert!(params.iter().any(|(k, v)| k == "SESSDATA" && v == "sessdata%2Cenc"));

        let fav = r#"{"code":0,"message":"0","ttl":1,"data":{"count":2,"list":[
            {"id":555,"pid":0,"title":"喜欢的歌","media_count":42,"intro":"","attr":0},
            {"id":666,"title":"默认收藏夹","media_count":1}
        ],"has_more":false}}"#;
        let env: ApiEnvelope<FolderListResp> = serde_json::from_str(fav).unwrap();
        let d = env.data.unwrap();
        assert_eq!(d.list.len(), 2);
        assert_eq!(d.list[0].id, 555);
        assert_eq!(d.list[0].media_count, 42);

        let res = r#"{"code":0,"message":"0","ttl":1,"data":{"info":{"id":555,"title":"喜欢的歌","media_count":42},"medias":[
            {"id":1,"bvid":"BV1xx411c7mD","title":"字幕君交流场所","cover":"https://i0.hdslb.com/bfs/x.jpg","duration":2055,"upper":{"mid":2,"name":"碧诗"},"cnt_info":{"collect":1}}
        ]}}"#;
        let env: ApiEnvelope<ResourceListResp> = serde_json::from_str(res).unwrap();
        let d = env.data.unwrap();
        assert_eq!(d.info.media_count, 42);
        assert_eq!(d.medias[0].bvid, "BV1xx411c7mD");
        assert_eq!(d.medias[0].upper.name, "碧诗");
    }

    #[test]
    fn test_deserialize_folder_list_has_more_and_null_data() {
        // created/list 分页响应：list + has_more。
        let body = r#"{"code":0,"message":"0","ttl":1,"data":{"count":22,"list":[
            {"id":939227072,"title":"学习","media_count":22},
            {"id":75020272,"title":"MAD/AMV","media_count":16}
        ],"has_more":true}}"#;
        let env: ApiEnvelope<FolderListResp> = serde_json::from_str(body).unwrap();
        let d = env.data.unwrap();
        assert!(d.has_more);
        assert_eq!(d.list.len(), 2);
        assert_eq!(d.list[1].title, "MAD/AMV");

        // 响应缺 has_more 字段时按 false（单页即止）。
        let body = r#"{"code":0,"message":"0","ttl":1,"data":{"count":2,"list":[
            {"id":1,"title":"默认收藏夹","media_count":1}
        ]}}"#;
        let env: ApiEnvelope<FolderListResp> = serde_json::from_str(body).unwrap();
        let d = env.data.unwrap();
        assert!(!d.has_more);

        // data 为 null（无收藏的收藏夹）：按空页处理而非解析失败。
        let body = r#"{"code":0,"message":"OK","ttl":1,"data":null}"#;
        let env: ApiEnvelope<FolderListResp> = serde_json::from_str(body).unwrap();
        let d = env.data.unwrap_or_default();
        assert!(d.list.is_empty());
        assert!(!d.has_more);
    }

    #[test]
    fn test_deserialize_nav_resp_logged_in_and_guest() {
        // 已登录 nav 响应节选（data 只列关键通用字段，其余靠 serde 忽略）。
        let logged_in = r#"{
            "code": 0, "message": "0", "ttl": 1,
            "data": {
                "isLogin": true, "mid": 9469746, "uname": "碧诗",
                "face": "https://i0.hdslb.com/bfs/face/x.jpg",
                "wbi_img": {
                    "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                    "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
                }
            }
        }"#;
        let env: ApiEnvelope<NavResp> = serde_json::from_str(logged_in).expect("nav 解析失败");
        let d = env.data.unwrap();
        assert_eq!(d.mid, 9469746);
        assert_eq!(d.uname, "碧诗");
        assert_eq!(d.face, "https://i0.hdslb.com/bfs/face/x.jpg");
        assert_eq!(d.wbi_img.img_url.contains("7cd084941338484aae1ad9425b84077c"), true);
        // mid/uname 齐全 → nav_user 判定为已登录。
        assert!(d.mid != 0 && !d.uname.is_empty());

        // 游客 nav：code=-101，data 无 mid/uname 字段（应默认 0/空）。
        let guest = r#"{
            "code": -101, "message": "账号未登录", "ttl": 1,
            "data": {
                "isLogin": false,
                "wbi_img": {
                    "img_url": "https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png",
                    "sub_url": "https://i0.hdslb.com/bfs/wbi/4932caff0ff746eab6f01bf08b70ac45.png"
                }
            }
        }"#;
        let env: ApiEnvelope<NavResp> = serde_json::from_str(guest).expect("游客 nav 解析失败");
        let d = env.data.unwrap();
        assert_eq!(d.mid, 0);
        assert_eq!(d.uname, "");
        // mid/uname 缺失 → nav_user 判定为未登录。
        assert!(d.mid == 0 || d.uname.is_empty());
    }


    #[test]
    fn test_required_headers_content() {
        let hs = StreamUrl::build_required_headers("buvid3=XYZ");
        assert!(hs.iter().any(|(k, v)| k == "User-Agent" && v == USER_AGENT));
        assert!(hs.iter().any(|(k, v)| k == "Referer" && v == REFERER));
        assert!(hs.iter().any(|(k, v)| k == "Cookie" && v == "buvid3=XYZ"));
        // 未登录时不应出现空 Cookie 头。
        let hs2 = StreamUrl::build_required_headers("");
        assert!(!hs2.iter().any(|(k, _)| k == "Cookie"));
    }
}
