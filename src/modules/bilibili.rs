//! B 站数据源模块：HTTP 基座 / 扫码登录 / 收藏夹 / BV 解析 / playurl 音频流提取。
//!
//! 网络层：`reqwest` blocking + rustls（无 openssl 依赖）。cookies 手动管理
//! （`Cookie` 头逐请求拼接），持久化交给 [`crate::modules::storage::BiliSession`]。
//!
//! 安全约定：任何日志/Debug 输出不得包含 SESSDATA/bili_jct（storage 层 Debug 已脱敏）。
//!
//! 公共 API 速览：
//! - 登录：[`BiliClient::generate_qrcode`] / [`qrcode_matrix`] / [`BiliClient::poll_login`]
//!   / [`BiliClient::logged_in`] / [`BiliClient::logout`]
//! - 收藏夹：[`BiliClient::list_favorite_folders`] / [`BiliClient::list_favorite_resources`]
//! - BV：[`BiliClient::parse_bvid`] / [`BiliClient::video_info`] / [`BiliClient::resolve_stream`]
//! - WBI：[`WbiKeys::from_nav`] / [`wbi_sign_params`] / [`mixin_key`]
//!
//! 注意：`qrcode` crate 目前最新稳定版是 0.14.1（不存在 1.x），本模块使用 0.14.1
//! `default-features = false`（不需要 image/svg 渲染，只要 bool 矩阵）。

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::modules::storage::{self, BiliSession};
use crate::state::AudioQuality;

/// B 站接口普遍校验的桌面 Chrome UA。
pub const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
/// 音频流/接口请求的 Referer（缺失会被 CDN 拒绝 403）。
pub const REFERER: &str = "https://www.bilibili.com/";
/// Origin 头（部分接口校验）。
pub const ORIGIN: &str = "https://www.bilibili.com";

// ---------------------------------------------------------------------------
// 错误类型
// ---------------------------------------------------------------------------

/// B 站模块错误。
#[derive(Debug)]
pub enum BiliError {
    /// HTTP 层错误（连接、TLS、超时等）。
    Http(reqwest::Error),
    /// B 站业务层错误（HTTP 200 但 code != 0）。
    Api { code: i64, message: String },
    /// 本地 IO（会话读写）。
    Io(std::io::Error),
    /// 二维码编码失败等本地错误。
    Local(String),
}

impl std::fmt::Display for BiliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BiliError::Http(e) => write!(f, "网络错误: {e}"),
            BiliError::Api { code, message } => write!(f, "B站接口错误 code={code}: {message}"),
            BiliError::Io(e) => write!(f, "本地IO错误: {e}"),
            BiliError::Local(s) => write!(f, "本地错误: {s}"),
        }
    }
}

impl std::error::Error for BiliError {}

impl From<reqwest::Error> for BiliError {
    fn from(e: reqwest::Error) -> Self {
        BiliError::Http(e)
    }
}

impl From<std::io::Error> for BiliError {
    fn from(e: std::io::Error) -> Self {
        BiliError::Io(e)
    }
}

impl From<qrcode::types::QrError> for BiliError {
    fn from(e: qrcode::types::QrError) -> Self {
        BiliError::Local(format!("二维码编码失败: {e}"))
    }
}

/// 便捷别名。
pub type BiliResult<T> = Result<T, BiliError>;

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
    fn build_required_headers(cookie_header: &str) -> Vec<(String, String)> {
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

/// 播放来源：收藏列表里的本地条目，或直接粘贴的 B 站视频链接。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PlaySource {
    /// 收藏夹/历史记录里的条目（可含用户备注）。
    Favorite { bvid: String, note: Option<String> },
    /// 用户手动粘贴的视频链接。
    VideoLink { url: String },
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

// ---------------------------------------------------------------------------
// WBI 签名（官方 web 前端用的 query 签名；部分接口/风控下必需）
// ---------------------------------------------------------------------------

/// WBI 使用的 64 位置换表（来源：bilibili-API-collect，web 端 wbi 签名）。
const MIXIN_KEY_ENC_TAB: [usize; 64] = [
    46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49, 33, 9, 42, 19, 29,
    28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40, 61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25,
    54, 21, 56, 59, 6, 63, 57, 62, 11, 36, 20, 34, 44, 52,
];

/// 由 nav 接口拿到的 img/sub key（各 32 位 hex）派生 32 位 mixin key。
/// 纯函数：`mixin_key[i] = (img + sub)[TAB[i]]`。
pub fn mixin_key(img_key: &str, sub_key: &str) -> String {
    let concat: Vec<char> = format!("{img_key}{sub_key}").chars().collect();
    MIXIN_KEY_ENC_TAB
        .iter()
        .filter_map(|&i| concat.get(i))
        .take(32)
        .collect()
}

/// 从 `wbi_img.img_url / sub_url`（如 `https://i0.hdslb.com/bfs/wbi/xxxx.png`）
/// 提取文件名（去扩展名）得到 img_key / sub_key。
pub fn wbi_key_from_url(url: &str) -> String {
    let file = url.rsplit('/').next().unwrap_or("");
    let stem = file.split('.').next().unwrap_or("");
    stem.to_string()
}

/// 等价于 JS `encodeURIComponent`（WBI 签名要求该编码方式，与表单编码不同：
/// 空格 -> %20；`!'()*-._~` 等不转义）。
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'!' | b'*'
            | b'\'' | b'(' | b')' => out.push(b as char),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// 计算 WBI 签名：向 `params` 追加 `wts` 并返回 `w_rid`。
/// 约定：`params` 是 query 的键值对（顺序任意，函数内部会排序）。
/// 返回 `(wts, w_rid)`，调用方把它们追加进真正的请求 query。
pub fn wbi_sign_params(params: &mut Vec<(String, String)>, mixin_key: &str) -> (u64, String) {
    let wts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let w_rid = wbi_sign_params_with_wts(params, mixin_key, wts);
    (wts, w_rid)
}

/// 给定固定 `wts` 的签名（测试用，生产走 [`wbi_sign_params`]）。
pub fn wbi_sign_params_with_wts(
    params: &mut Vec<(String, String)>,
    mixin_key: &str,
    wts: u64,
) -> String {
    params.push(("wts".to_string(), wts.to_string()));
    // 按 key 的 ASCII 升序排序；value 过滤 WBI 特殊字符（!'()*）。
    let mut pairs: Vec<(String, String)> = params
        .iter()
        .map(|(k, v)| (k.clone(), v.chars().filter(|c| !"!'()*".contains(*c)).collect()))
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let query = pairs
        .iter()
        .map(|(k, v)| format!("{}={}", encode_uri_component(k), encode_uri_component(v)))
        .collect::<Vec<_>>()
        .join("&");
    md5_hex(format!("{query}{mixin_key}"))
}

/// 小写十六进制 MD5。
pub fn md5_hex(s: impl AsRef<[u8]>) -> String {
    let digest = md5::compute(s.as_ref());
    let mut out = String::with_capacity(32);
    for b in digest.0 {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// WBI key 集合（nav 接口获取）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WbiKeys {
    pub img_key: String,
    pub sub_key: String,
}

impl WbiKeys {
    /// 从 nav 响应构造。
    pub fn from_urls(img_url: &str, sub_url: &str) -> Self {
        Self {
            img_key: wbi_key_from_url(img_url),
            sub_key: wbi_key_from_url(sub_url),
        }
    }

    /// 派生当前 mixin key（随 B 站更新轮换，理论上最多 24h 缓存）。
    pub fn mixin_key(&self) -> String {
        mixin_key(&self.img_key, &self.sub_key)
    }
}

// ---------------------------------------------------------------------------
// API 响应模型（只取需要的字段，其余忽略）
// ---------------------------------------------------------------------------

/// B 站标准响应信封 `{code, message, data}`（公开仅为可见性，字段内部使用）。
#[derive(Debug, Deserialize)]
pub struct ApiEnvelope<T> {
    code: i64,
    #[serde(default)]
    message: String,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct ViewResp {
    bvid: String,
    #[serde(default)]
    cid: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    pic: String,
    #[serde(default)]
    duration: i64,
    #[serde(default)]
    videos: i64,
    owner: Owner,
}

#[derive(Debug, Deserialize)]
struct Owner {
    name: String,
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
    #[serde(default)]
    pub duration: i64,
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
    pub codecid: i64,
    #[serde(default)]
    pub codecs: Option<String>,
    #[serde(default)]
    pub size: Option<i64>,
}

impl DashStream {
    /// 备用地址 + 主地址兜底列表。
    pub fn all_urls(&self) -> Vec<String> {
        let mut v = self.backup_url.clone();
        v.push(self.base_url.clone());
        v
    }
}

// 实际响应同时包含 `baseUrl` 与 `base_url`（B 站双写），serde 对 rename+alias 的
// 同字段映射会报 duplicate field，所以先用 Option 原样捕获再合并。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct DashStreamRaw {
    id: i64,
    /// JSON 键 `baseUrl`（camel，现行响应）。
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    /// JSON 键 `base_url`（snake，双写/老格式）。
    #[serde(rename = "base_url")]
    base_url_snake: Option<String>,
    /// JSON 键 `backupUrl`。
    #[serde(rename = "backupUrl")]
    backup_url: Option<Vec<String>>,
    /// JSON 键 `backup_url`。
    #[serde(rename = "backup_url")]
    backup_url_snake: Option<Vec<String>>,
    bandwidth: i64,
    codecid: i64,
    codecs: Option<String>,
    size: Option<i64>,
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
            codecid: r.codecid,
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
struct QrGenerateResp {
    #[serde(default)]
    qrcode_key: String,
    #[serde(default)]
    url: String,
}

#[derive(Debug, Deserialize)]
struct QrPollResp {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    url: String,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct NavResp {
    #[serde(default)]
    wbi_img: WbiImg,
    /// 用户 mid（未登录时缺省为 0）。
    #[serde(default)]
    mid: u64,
    /// 用户昵称（未登录时缺省为空）。
    #[serde(default)]
    uname: String,
    /// 用户头像 URL。
    #[serde(default)]
    face: String,
}

/// 当前登录用户信息（nav 接口）。
#[derive(Debug, Clone, PartialEq)]
pub struct NavUser {
    pub mid: u64,
    pub uname: String,
    pub face: String,
}

#[derive(Debug, Default, Deserialize)]
struct WbiImg {
    #[serde(default)]
    img_url: String,
    #[serde(default)]
    sub_url: String,
}

/// 收藏夹分页响应（created/list 与 collected/list 同构）。
#[derive(Debug, Default, Deserialize)]
struct FolderListResp {
    #[serde(default)]
    list: Vec<FolderEntry>,
    /// 是否还有下一页（没有该字段时按 false 处理，单页即止）。
    #[serde(default)]
    has_more: bool,
}

#[derive(Debug, Default, Deserialize)]
struct FolderEntry {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    title: String,
    #[serde(rename = "media_count", alias = "mediaCount", default)]
    media_count: i64,
}

#[derive(Debug, Deserialize)]
struct ResourceListResp {
    /// 收藏夹元信息（media_count 在这里，v3 接口如此）。
    #[serde(default)]
    info: FolderEntry,
    #[serde(default)]
    medias: Vec<MediaEntry>,
}

#[derive(Debug, Deserialize)]
struct MediaEntry {
    #[serde(default)]
    bvid: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    cover: String,
    #[serde(default)]
    duration: i64,
    upper: Owner,
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

impl QrPoll {
    /// B 站 poll 接口的 code -> 状态。
    pub fn from_code(code: i64) -> Option<Self> {
        match code {
            0 => None, // Success 需要携带 cookies，由 poll_login 构造
            86038 => Some(QrPoll::Expired),
            86090 => Some(QrPoll::WaitingConfirm),
            86101 => Some(QrPoll::WaitingScan),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------

/// B 站 API 客户端（blocking）。
///
/// UI 集成建议：在 `std::thread` 中持有 `BiliClient`，用 channel 把结果发回 GUI 线程，
/// 避免 blocking IO 阻塞渲染。
pub struct BiliClient {
    http: reqwest::blocking::Client,
    /// 会话（cookies + buvid），与磁盘 session.json 同步。
    session: BiliSession,
    /// WBI key 缓存（约 30 分钟刷新一次）。
    wbi_cache: Mutex<Option<(WbiKeys, Instant)>>,
}

impl Default for BiliClient {
    fn default() -> Self {
        Self::new().expect("创建 BiliClient 失败")
    }
}

impl BiliClient {
    /// 构建客户端（不加载磁盘会话；测试或无痕模式用）。
    pub fn new() -> BiliResult<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::REFERER,
            reqwest::header::HeaderValue::from_static(REFERER),
        );
        headers.insert(
            reqwest::header::ORIGIN,
            reqwest::header::HeaderValue::from_static(ORIGIN),
        );
        headers.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            reqwest::header::HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
        );
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()?;
        Ok(Self {
            http,
            session: BiliSession::default(),
            wbi_cache: Mutex::new(None),
        })
    }

    /// 构建客户端并从 `~/.config/simple-music/session.json` 恢复 cookies/buvid。
    /// 文件不存在或损坏时静默降级为空会话。
    pub fn with_session() -> BiliResult<Self> {
        let mut client = Self::new()?;
        if let Ok(session) = storage::load_session() {
            client.session = session;
        }
        Ok(client)
    }

    // ---- 会话管理 ----

    /// 当前会话快照。
    pub fn session(&self) -> &BiliSession {
        &self.session
    }

    /// 是否已登录。
    pub fn logged_in(&self) -> bool {
        self.session.logged_in()
    }

    /// 当前用户 mid（DedeUserID）。
    pub fn mid(&self) -> Option<u64> {
        self.session.get("DedeUserID").and_then(|v| v.parse().ok())
    }

    /// 拉取当前登录用户的昵称/头像（nav 接口）。未登录返回 `None`。
    ///
    /// 阻塞网络调用，应在后台线程使用。
    pub fn nav_user(&self) -> BiliResult<Option<NavUser>> {
        // 游客访问 nav 返回 code=-101 但 data.wbi_img 照常下发（见 wbi_keys 注释），
        // 所以这里同样不能走 get_data 的严格 code==0 校验，直接看 data 字段。
        let (_http, env) = self.get_json::<NavResp>("https://api.bilibili.com/x/web-interface/nav", &[])?;
        let Some(data) = env.data else {
            return Err(BiliError::Api { code: env.code, message: env.message });
        };
        // 未登录：mid=0 / uname 为空串。
        if data.mid == 0 || data.uname.is_empty() {
            return Ok(None);
        }
        Ok(Some(NavUser {
            mid: data.mid,
            uname: data.uname,
            face: data.face,
        }))
    }

    /// 退出登录：清除登录相关 cookies，保留 buvid 指纹，并落盘。
    pub fn logout(&mut self) -> BiliResult<()> {
        for k in ["SESSDATA", "bili_jct", "DedeUserID", "DedeUserID__ckMd5"] {
            self.session.remove(k);
        }
        self.persist_session()?;
        Ok(())
    }

    /// 会话落盘（仅更新时间戳并写 session.json）。
    pub fn persist_session(&self) -> BiliResult<()> {
        let mut to_save = self.session.clone();
        to_save.saved_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        storage::save_session(&to_save)?;
        Ok(())
    }

    /// 未登录也需要 buvid3/buvid4：从 finger/spi 获取并持久化（已有时跳过）。
    pub fn ensure_buvid(&mut self) -> BiliResult<()> {
        if self
            .session
            .get("buvid3")
            .map_or(false, |v| !v.is_empty())
        {
            return Ok(());
        }
        #[derive(Deserialize)]
        struct SpiData {
            #[serde(rename = "b_3")]
            buvid3: String,
            #[serde(rename = "b_4")]
            buvid4: String,
        }
        let data: SpiData = self
            .get_data("https://api.bilibili.com/x/frontend/finger/spi", "finger/spi")?;
        self.session.set("buvid3", data.buvid3);
        self.session.set("buvid4", data.buvid4);
        // 落盘尽力而为：沙箱/只读文件系统下失败不应阻断取流（会话仍在内存）。
        let _ = self.persist_session();
        Ok(())
    }

    /// 拼接 `Cookie` 头的值。
    pub fn cookie_header(&self) -> String {
        self.session.cookie_header()
    }

    // ---- HTTP 基座 ----

    /// GET 一个 B 站 JSON 接口，自动带 UA/Referer/Cookie。
    /// 返回 `(HTTP 状态码, 反序列化后的信封)`。
    pub fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        extra_query: &[(String, String)],
    ) -> BiliResult<(u16, ApiEnvelope<T>)> {
        let mut url = url.to_string();
        if !extra_query.is_empty() {
            let sep = if url.contains('?') { '&' } else { '?' };
            let qs = extra_query
                .iter()
                .map(|(k, v)| format!("{}={}", encode_uri_component(k), encode_uri_component(v)))
                .collect::<Vec<_>>()
                .join("&");
            url = format!("{url}{sep}{qs}");
        }
        let resp = self
            .http
            .get(&url)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .send()?;
        let status = resp.status().as_u16();
        let text = resp.text()?;
        let env: ApiEnvelope<T> = serde_json::from_str(&text).map_err(|e| {
            BiliError::Local(format!("响应解析失败({url}): {e}; body[:200]={}", &text[..text.len().min(200)]))
        })?;
        Ok((status, env))
    }

    /// GET 并校验 code==0，直接返回 data。`api` 用于错误信息。
    pub fn get_data<T: for<'de> Deserialize<'de>>(&self, url: &str, api: &str) -> BiliResult<T> {
        let (http, env) = self.get_json(url, &[])?;
        Self::unwrap_api(http, env, api)
    }

    /// 校验信封 code==0 并取出 data。
    fn unwrap_api<T>(http: u16, env: ApiEnvelope<T>, api: &str) -> BiliResult<T> {
        if http >= 400 {
            return Err(BiliError::Api {
                code: http as i64,
                message: format!("{api} HTTP {http}"),
            });
        }
        if env.code != 0 {
            return Err(BiliError::Api {
                code: env.code,
                message: env.message,
            });
        }
        env.data.ok_or_else(|| BiliError::Local(format!("{api} 缺少 data")))
    }

    // ---- 扫码登录 ----

    /// 生成登录二维码。
    pub fn generate_qrcode(&self) -> BiliResult<QrLoginStart> {
        let data: QrGenerateResp = self.get_data(
            "https://passport.bilibili.com/x/passport-login/web/qrcode/generate",
            "qrcode/generate",
        )?;
        Ok(QrLoginStart {
            qrcode_key: data.qrcode_key,
            url: data.url,
        })
    }

    /// 把二维码内容编码成 bool 矩阵（true = 深色模块），行优先，边长 = 行数 = 列数。
    /// UI 渲染时请自行留出约 4 模块的静区（quiet zone）并反色（深色前景）。
    pub fn qrcode_matrix(content: &str) -> BiliResult<Vec<Vec<bool>>> {
        let code = qrcode::QrCode::with_error_correction_level(
            content.as_bytes(),
            qrcode::EcLevel::M,
        )?;
        let width = code.width();
        let colors = code.to_colors();
        Ok(colors
            .chunks(width)
            .map(|row| row.iter().map(|c| *c == qrcode::Color::Dark).collect())
            .collect())
    }

    /// 轮询扫码登录结果。成功时自动捕获 cookies 并落盘（含 buvid）。
    pub fn poll_login(&mut self, qrcode_key: &str) -> BiliResult<QrPoll> {
        let url = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
        let resp = self
            .http
            .get(url)
            .header(reqwest::header::COOKIE, self.cookie_header())
            .query(&[("qrcode_key", qrcode_key)])
            .send()?;

        // 先捕获 Set-Cookie（成功时才有 SESSDATA/bili_jct/DedeUserID）。
        let mut cookies = BTreeMap::new();
        for value in resp.headers().get_all(reqwest::header::SET_COOKIE) {
            if let Ok(sc) = value.to_str() {
                cookies.extend(parse_set_cookie(sc));
            }
        }

        let status = resp.status().as_u16();
        let text = resp.text()?;
        let env: ApiEnvelope<QrPollResp> = serde_json::from_str(&text).map_err(|e| {
            BiliError::Local(format!("poll 响应解析失败: {e}; body[:200]={}", &text[..text.len().min(200)]))
        })?;
        let data = env
            .data
            .ok_or_else(|| BiliError::Local("poll 响应缺少 data".into()))?;

        if env.code != 0 {
            return Err(BiliError::Api {
                code: env.code,
                message: env.message,
            });
        }
        if status >= 400 {
            return Err(BiliError::Api {
                code: status as i64,
                message: format!("poll HTTP {status}"),
            });
        }

        match data.code {
            0 => {
                // Set-Cookie 失败时兜底：成功响应的 data.url 里也带了全部登录参数。
                for (k, v) in parse_query_params(&data.url) {
                    cookies.entry(k).or_insert(v);
                }
                let mid: u64 = cookies
                    .get("DedeUserID")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                // 合并进会话并落盘（落盘尽力而为，不因 IO 失败判定登录失败）。
                for (k, v) in &cookies {
                    self.session.set(k.clone(), v.clone());
                }
                let _ = self.persist_session();
                Ok(QrPoll::Success { cookies, mid })
            }
            86038 => Ok(QrPoll::Expired),
            86090 => Ok(QrPoll::WaitingConfirm),
            86101 => Ok(QrPoll::WaitingScan),
            other => Err(BiliError::Api {
                code: other,
                message: data.message,
            }),
        }
    }

    // ---- 收藏夹（需 SESSDATA） ----

    /// 列出当前登录用户自己的收藏夹（未登录返回 -101）。
    ///
    /// 旧接口 `fav/folder/owned/list` 已被 B 站下线（HTTP 404，返回 HTML 错误页），
    /// 改用官方前端在用的两个分页接口合并：
    /// - `fav/folder/created/list`：用户**创建**的收藏夹（核心，失败直接报错）；
    /// - `fav/folder/collected/list`：用户**收藏**的收藏夹（best-effort，失败不阻断）。
    /// 两路结果按 id 去重；空收藏夹时 `data` 可能为 null，按空页处理。
    pub fn list_favorite_folders(&self) -> BiliResult<Vec<FavFolder>> {
        let mid = self.mid().ok_or_else(|| BiliError::Api {
            code: -101,
            message: "未登录（缺少 DedeUserID）".into(),
        })?;
        let mut folders = self.list_folder_pages("created", mid)?;
        folders.extend(self.list_folder_pages("collected", mid).unwrap_or_default());
        Ok(dedup_folders(folders))
    }

    /// 分页拉取一类收藏夹（`api` = created / collected），ps 上限 20，最多翻 50 页。
    fn list_folder_pages(&self, api: &str, mid: u64) -> BiliResult<Vec<FavFolder>> {
        let mut folders = Vec::new();
        let mut pn: u32 = 1;
        loop {
            let url = format!(
                "https://api.bilibili.com/x/v3/fav/folder/{api}/list?up_mid={mid}&pn={pn}&ps=20"
            );
            let (http, env) = self.get_json::<FolderListResp>(&url, &[])?;
            let page = if http >= 400 {
                return Err(BiliError::Api {
                    code: http as i64,
                    message: format!("fav/folder/{api}/list HTTP {http}"),
                });
            } else if env.code != 0 {
                return Err(BiliError::Api {
                    code: env.code,
                    message: env.message,
                });
            } else {
                // data 为 null（如无收藏的收藏夹）按空页处理，不当作错误。
                env.data.unwrap_or_default()
            };
            let has_more = page.has_more;
            folders.extend(page.list.into_iter().map(|f| FavFolder {
                id: f.id,
                title: f.title,
                media_count: f.media_count,
            }));
            if !has_more || pn >= 50 {
                break;
            }
            pn += 1;
        }
        Ok(folders)
    }

    /// 列出收藏夹资源（type=2 仅视频），返回 `(条目, 收藏夹总数)`。
    pub fn list_favorite_resources(&self, media_id: i64, pn: u32) -> BiliResult<(Vec<FavItem>, i64)> {
        // platform=web 为官方文档标注参数（影响内容列表类型），与 web 前端一致。
        let url = format!(
            "https://api.bilibili.com/x/v3/fav/resource/list?media_id={media_id}&pn={pn}&ps=20&order=mtime&type=2&platform=web"
        );
        let data: ResourceListResp = self.get_data(&url, "fav/resource/list")?;
        let items = data
            .medias
            .into_iter()
            .filter(|m| !m.bvid.is_empty())
            .map(|m| FavItem {
                bvid: m.bvid,
                title: m.title,
                owner: m.upper.name,
                duration_secs: m.duration.max(0) as f64,
                cover_url: Some(m.cover).filter(|c| !c.is_empty()),
            })
            .collect();
        Ok((items, data.info.media_count))
    }

    // ---- BV 链接导入 ----

    /// 从任意输入解析 BV 号：支持完整/移动端 URL（含 ?p= 分 P）、纯 BV 号、
    /// b23.tv 短链（跟随重定向后解析）。`av` 号暂不支持（返回 None）。
    pub fn parse_bvid(&self, input: &str) -> Option<String> {
        if let Some(bv) = Self::parse_bvid_direct(input) {
            return Some(bv);
        }
        // b23.tv 短链：跟随重定向拿最终 URL 再解析。
        let trimmed = input.trim();
        if trimmed.contains("b23.tv") {
            if let Ok(resp) = self.http.get(trimmed).send() {
                let final_url = resp.url().as_str();
                return Self::parse_bvid_direct(final_url);
            }
        }
        None
    }

    /// 纯本地 BV 解析（无网络）：在任何包含 `BV + 10位字母数字` 的文本里提取。
    /// URL 形态、纯 BV 号、带 ?p= 分 P 都适用；b23.tv 短链需网络，走 [`BiliClient::parse_bvid`]。
    pub fn parse_bvid_direct(input: &str) -> Option<String> {
        let s = input.trim();
        if s.is_empty() {
            return None;
        }
        // 注：av 号（av170001 / /video/avxxx）显式不支持——纯 BV 扫描天然不会匹配到它们，
        // 不做 av -> BV 转换（如需可后续接入 base58 算法）。
        scan_bv_token(s)
    }

    /// 拉取视频详情（title/owner/duration/cid/pages）。
    pub fn video_info(&self, bvid: &str) -> BiliResult<VideoDetail> {
        let url = format!("https://api.bilibili.com/x/web-interface/view?bvid={bvid}");
        let data: ViewResp = self.get_data(&url, "web-interface/view")?;
        Ok(VideoDetail {
            cid: data.cid,
            pages: data.videos.max(1) as u64,
            info: VideoInfo {
                bvid: data.bvid,
                title: data.title,
                uploader: data.owner.name,
                duration_secs: data.duration.max(0) as f64,
                cover_url: Some(data.pic).filter(|p| !p.is_empty()),
            },
        })
    }

    /// 解析音频流（自动先取 video_info 拿 cid，再走 playurl）。
    pub fn resolve_stream(&self, bvid: &str, quality: AudioQuality) -> BiliResult<StreamUrl> {
        let detail = self.video_info(bvid)?;
        self.resolve_stream_with_cid(bvid, detail.cid, quality)
    }

    /// 解析音频流（已知 cid，免一次 view 请求）。
    ///
    /// 策略：先无签名请求 playurl；若被风控拒绝（code != 0 / 无 dash）则自动补 WBI
    /// 签名重试一次。返回 [`StreamUrl`]，其中 `required_headers` 是音频 Worker
    /// 下载时必须携带的请求头。
    pub fn resolve_stream_with_cid(
        &self,
        bvid: &str,
        cid: i64,
        quality: AudioQuality,
    ) -> BiliResult<StreamUrl> {
        // 第一次：不带 WBI。
        let (http, raw) = self.fetch_playurl_raw(bvid, cid, false)?;
        let usable = raw.code == 0
            && raw
                .data
                .as_ref()
                .map(|d| d.dash.as_ref().map_or(true, |dash| !dash.audio.is_empty()) || d.durl.as_ref().map_or(false, |d| !d.is_empty()))
                .unwrap_or(false);
        if usable {
            return self.build_stream_url(http, raw, false, bvid, quality);
        }
        // 第二次：带 WBI 签名重试。
        let (http2, raw2) = self.fetch_playurl_raw(bvid, cid, true)?;
        self.build_stream_url(http2, raw2, true, bvid, quality)
    }

    /// 请求 playurl 接口，返回 `(HTTP code, 原始响应)`。`use_wbi` 控制是否加 WBI 签名。
    pub fn fetch_playurl_raw(
        &self,
        bvid: &str,
        cid: i64,
        use_wbi: bool,
    ) -> BiliResult<(u16, PlayUrlResp)> {
        let mut url = format!("https://api.bilibili.com/x/player/playurl?bvid={bvid}&cid={cid}");
        if use_wbi {
            let keys = self.wbi_keys()?;
            let mut params = vec![
                ("bvid".to_string(), bvid.to_string()),
                ("cid".to_string(), cid.to_string()),
                ("fnval".to_string(), "16".to_string()),
                ("fourk".to_string(), "1".to_string()),
            ];
            let (_wts, w_rid) = wbi_sign_params(&mut params, &keys.mixin_key());
            let qs = params
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            url = format!("https://api.bilibili.com/x/player/playurl?{qs}&w_rid={w_rid}");
        } else {
            url.push_str("&fnval=16&fourk=1");
        }
        let (http, env) = self.get_json::<PlayUrlData>(&url, &[])?;
        Ok((
            http,
            PlayUrlResp {
                code: env.code,
                message: env.message,
                data: env.data,
            },
        ))
    }

    /// 获取（并缓存）WBI key。缓存约 30 分钟。
    pub fn wbi_keys(&self) -> BiliResult<WbiKeys> {
        let mut cache = self
            .wbi_cache
            .lock()
            .map_err(|_| BiliError::Local("wbi 缓存锁中毒".into()))?;
        if let Some((keys, at)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(30 * 60) {
                return Ok(keys.clone());
            }
        }
        // 注意：游客访问 nav 返回 code=-101（账号未登录）但 data.wbi_img 照常下发，
        // 所以这里不能走 unwrap_api 的严格 code==0 校验。
        let (_http, env) = self.get_json::<NavResp>("https://api.bilibili.com/x/web-interface/nav", &[])?;
        let data = match env.data {
            Some(d) => d,
            None => return Err(BiliError::Api { code: env.code, message: env.message }),
        };
        if data.wbi_img.img_url.is_empty() || data.wbi_img.sub_url.is_empty() {
            return Err(BiliError::Local("nav 缺少 wbi_img".into()));
        }
        let keys = WbiKeys::from_urls(&data.wbi_img.img_url, &data.wbi_img.sub_url);
        *cache = Some((keys.clone(), Instant::now()));
        Ok(keys)
    }

    // ---- 内部 ----

    /// 诊断用：按给定头做一次 Range 下载探测，返回 `(HTTP 状态码, 实际收到字节数)`。
    pub fn probe_download(&self, url: &str, headers: &[(String, String)], range: &str) -> BiliResult<(u16, usize)> {
        let mut req = self.http.get(url).header(reqwest::header::RANGE, range);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req.send()?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes()?;
        Ok((status, bytes.len()))
    }

    fn build_stream_url(
        &self,
        http: u16,
        raw: PlayUrlResp,
        signed: bool,
        _bvid: &str,
        quality: AudioQuality,
    ) -> BiliResult<StreamUrl> {
        if http >= 400 {
            return Err(BiliError::Api {
                code: http as i64,
                message: format!("playurl HTTP {http}"),
            });
        }
        if raw.code != 0 {
            return Err(BiliError::Api {
                code: raw.code,
                message: raw.message,
            });
        }
        let data = raw.data.ok_or_else(|| BiliError::Local("playurl 缺少 data".into()))?;
        let ttl_secs = (data.timelength.max(0) as u64) / 1000;
        let cookie_header = self.cookie_header();

        if let Some(dash) = data.dash.filter(|d| !d.audio.is_empty()) {
            // 按音质偏好选音频流（未命中偏好则回退最高码率）。
            let best = pick_dash_audio(&dash.audio, quality)
                .expect("audio 非空已检查")
                .clone();
            // 视频流：纯听歌可不取，这里带出最高清视频 url 供后续 MV 模式用。
            let video = dash
                .video
                .iter()
                .max_by_key(|v| (v.bandwidth, v.id))
                .map(|v| v.base_url.clone());
            return Ok(StreamUrl {
                audio_url: best.base_url,
                video_url: video,
                ttl_secs,
                audio_id: Some(best.id),
                audio_codec: best.codecs,
                bandwidth: Some(best.bandwidth),
                size_bytes: best.size,
                audio_backup_urls: best.backup_url,
                required_headers: StreamUrl::build_required_headers(&cookie_header),
                signed_with_wbi: signed,
            });
        }

        // 老格式 / 降级：durl（音视频混合流，通常为 flv/mp4）。
        let first = data
            .durl
            .and_then(|mut d| if d.is_empty() { None } else { Some(d.remove(0)) })
            .ok_or_else(|| BiliError::Local("playurl 既无 dash.audio 也无 durl（可能被风控，请登录或稍后重试）".into()))?;
        Ok(StreamUrl {
            audio_url: first.url,
            video_url: None,
            ttl_secs,
            audio_id: None,
            audio_codec: None,
            bandwidth: None,
            size_bytes: Some(first.size).filter(|s| *s > 0),
            audio_backup_urls: Vec::new(),
            required_headers: StreamUrl::build_required_headers(&cookie_header),
            signed_with_wbi: signed,
        })
    }
}

// ---------------------------------------------------------------------------
// 纯函数工具
// ---------------------------------------------------------------------------

/// 按音质偏好从 DASH 音频流中选择。
///
/// 优先精确匹配偏好 id；未命中时低/中档取最接近目标码率的流，高档取最高码率。
/// 无损偏好（Lossless）依次尝试 FLAC (30255) → Dolby (30250/30251) → 最高码率。
pub fn pick_dash_audio<'a>(audio: &'a [DashStream], quality: AudioQuality) -> Option<&'a DashStream> {
    if audio.is_empty() {
        return None;
    }
    let preferred_ids: Vec<i64> = match quality {
        AudioQuality::Low => vec![30216],
        AudioQuality::Medium => vec![30232],
        AudioQuality::High => vec![30280],
        AudioQuality::Lossless => vec![30255, 30250, 30251],
    };
    for id in preferred_ids {
        if let Some(s) = audio.iter().find(|s| s.id == id) {
            return Some(s);
        }
    }
    // 未命中偏好：低/中档取最接近目标码率的流，其余取最高码率。
    let target_bandwidth = match quality {
        AudioQuality::Low => 64_000,
        AudioQuality::Medium => 128_000,
        _ => i64::MAX,
    };
    audio.iter().min_by_key(|s| (s.bandwidth - target_bandwidth).abs())
}

/// 在文本里扫描 `BV + 10 位 [0-9A-Za-z]`，返回第一个匹配。
fn scan_bv_token(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 12 <= n {
        if bytes[i] == b'B' && bytes[i + 1] == b'V' {
            let candidate = &s[i + 2..i + 12];
            if candidate
                .bytes()
                .all(|b| b.is_ascii_alphanumeric())
            {
                // BV 号第 1 位（总第 3 位）按现行规范是 1~7 之间的数字，用于排除
                // 恰好拼成 "BVxxx" 的普通单词（如 "BVDIRECTORY" 这类长词截断误判）。
                let c = candidate.as_bytes()[0];
                if (b'1'..=b'7').contains(&c) {
                    return Some(format!("BV{candidate}"));
                }
            }
        }
        i += 1;
    }
    None
}

/// 从 Set-Cookie 值里提取第一对 k=v。
fn parse_set_cookie(set_cookie: &str) -> Option<(String, String)> {
    let first = set_cookie.split(';').next()?;
    let (k, v) = first.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), v.trim().to_string()))
}

/// 按 id 去重收藏夹（保留首个出现者）：created/collected 两路合并时的防御性去重。
fn dedup_folders(folders: Vec<FavFolder>) -> Vec<FavFolder> {
    let mut seen = std::collections::HashSet::new();
    folders
        .into_iter()
        .filter(|f| seen.insert(f.id))
        .collect()
}

/// 解析 URL query 片段（仅 path 后的 k=v&...；value 不解码，B 站登录 url 里
/// SESSDATA 已是可直接入 Cookie 的编码值）。
fn parse_query_params(url: &str) -> Vec<(String, String)> {
    let Some(q) = url.split(['?', '#']).nth(1) else {
        return Vec::new();
    };
    let q = q.split(['?', '#']).next().unwrap_or(q);
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 音质选择 ----

    fn dash(id: i64, bandwidth: i64) -> DashStream {
        DashStream {
            id,
            base_url: format!("https://x/{id}"),
            backup_url: Vec::new(),
            bandwidth,
            codecid: 0,
            codecs: None,
            size: None,
        }
    }

    #[test]
    fn test_pick_dash_audio_prefers_exact_id() {
        let audio = vec![dash(30216, 64_000), dash(30280, 320_000)];
        assert_eq!(pick_dash_audio(&audio, AudioQuality::Low).unwrap().id, 30216);
        assert_eq!(pick_dash_audio(&audio, AudioQuality::High).unwrap().id, 30280);
    }

    #[test]
    fn test_pick_dash_audio_falls_back_to_closest_bandwidth() {
        // 无 30280 时，High 回退到最高码率。
        let audio = vec![dash(30216, 64_000), dash(30232, 128_000)];
        assert_eq!(pick_dash_audio(&audio, AudioQuality::High).unwrap().id, 30232);
        // Low 偏好接近 64kbps 的流。
        let audio2 = vec![dash(30232, 128_000), dash(30280, 320_000)];
        assert_eq!(pick_dash_audio(&audio2, AudioQuality::Low).unwrap().id, 30232);
    }

    #[test]
    fn test_pick_dash_audio_lossless_prefers_flac_then_dolby() {
        let audio = vec![dash(30280, 320_000), dash(30250, 512_000), dash(30255, 1_000_000)];
        assert_eq!(pick_dash_audio(&audio, AudioQuality::Lossless).unwrap().id, 30255);
        let audio2 = vec![dash(30280, 320_000), dash(30251, 512_000)];
        assert_eq!(pick_dash_audio(&audio2, AudioQuality::Lossless).unwrap().id, 30251);
    }

    #[test]
    fn test_pick_dash_audio_empty() {
        assert!(pick_dash_audio(&[], AudioQuality::High).is_none());
    }

    // ---- BV 解析 ----

    #[test]
    fn test_parse_bvid_direct_variants() {
        let bv = "BV1xx411c7mD";
        assert_eq!(BiliClient::parse_bvid_direct(bv), Some(bv.into()));
        assert_eq!(
            BiliClient::parse_bvid_direct("https://www.bilibili.com/video/BV1xx411c7mD"),
            Some(bv.into())
        );
        assert_eq!(
            BiliClient::parse_bvid_direct("https://www.bilibili.com/video/BV1xx411c7mD?p=2&spm_id_from=x"),
            Some(bv.into())
        );
        assert_eq!(
            BiliClient::parse_bvid_direct("https://www.bilibili.com/video/BV1xx411c7mD/?vd_source=abc"),
            Some(bv.into())
        );
        assert_eq!(
            BiliClient::parse_bvid_direct("https://m.bilibili.com/video/BV1uv411q7Mv"),
            Some("BV1uv411q7Mv".into())
        );
        assert_eq!(
            BiliClient::parse_bvid_direct("看看这个 BV1GJ411x7h7 挺好玩的"),
            Some("BV1GJ411x7h7".into())
        );
        assert_eq!(
            BiliClient::parse_bvid_direct("  https://b23.tv/abc123?t=1  "),
            None,
            "短链无网络时本地解析应返回 None（由 parse_bvid 走重定向）"
        );
    }

    #[test]
    fn test_parse_bvid_direct_rejects_av_and_garbage() {
        assert_eq!(BiliClient::parse_bvid_direct("av170001"), None);
        assert_eq!(BiliClient::parse_bvid_direct("https://www.bilibili.com/video/av170001"), None);
        assert_eq!(BiliClient::parse_bvid_direct(""), None);
        assert_eq!(BiliClient::parse_bvid_direct("https://example.com/BV_not_here"), None);
        // 不足 10 位 token
        assert_eq!(BiliClient::parse_bvid_direct("BV1xx411c7"), None);
        // 第 3 位不是 1-7 的长词不应误判
        assert_eq!(BiliClient::parse_bvid_direct("BVDIRECTORY"), None);
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
    fn test_dedup_folders_keeps_first() {
        let folders = vec![
            FavFolder { id: 555, title: "a".into(), media_count: 1 },
            FavFolder { id: 666, title: "b".into(), media_count: 2 },
            FavFolder { id: 555, title: "a2".into(), media_count: 9 },
        ];
        let out = dedup_folders(folders);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, 555);
        assert_eq!(out[0].title, "a", "应保留首个出现者");
        assert_eq!(out[1].id, 666);
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

    // ---- WBI ----

    /// WBI 文档通用示例：img/sub 各 32 位 hex。
    const WBI_TEST_IMG: &str = "7cd084941338484aae1ad9425b84077c";
    const WBI_TEST_SUB: &str = "4932caff0ff746eab6f01bf08b70ac45";

    #[test]
    fn test_mixin_key_properties() {
        let key = mixin_key(WBI_TEST_IMG, WBI_TEST_SUB);
        assert_eq!(key.chars().count(), 32, "mixin key 固定 32 位");
        // 确定性：重复计算一致。
        assert_eq!(key, mixin_key(WBI_TEST_IMG, WBI_TEST_SUB));
        // 只会取自 img/sub 的字符集合。
        let alphabet: String = format!("{WBI_TEST_IMG}{WBI_TEST_SUB}").chars().collect();
        assert!(key.chars().all(|c| alphabet.contains(c)));
        // 快照（独立脚本按同一置换表计算并核对；正确性另由真实 playurl 签名
        // 请求终验，见 examples/bili_probe.rs —— B 站校验失败会直接返回 -403）。
        assert_eq!(key, "ea1db124af3c7062474693fa704f4ff8");
    }

    #[test]
    fn test_wbi_key_from_url() {
        assert_eq!(
            wbi_key_from_url("https://i0.hdslb.com/bfs/wbi/7cd084941338484aae1ad9425b84077c.png"),
            WBI_TEST_IMG
        );
        assert_eq!(wbi_key_from_url("4932caff0ff746eab6f01bf08b70ac45.webp"), WBI_TEST_SUB);
    }

    #[test]
    fn test_encode_uri_component() {
        assert_eq!(encode_uri_component("a b"), "a%20b");
        assert_eq!(encode_uri_component("a+b"), "a%2Bb");
        assert_eq!(encode_uri_component("!'()*-._~"), "!'()*-._~");
        assert_eq!(encode_uri_component("中"), "%E4%B8%AD");
        assert_eq!(encode_uri_component("a=b&c"), "a%3Db%26c");
    }

    #[test]
    fn test_md5_hex_known_vectors() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("hello"), "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn test_wbi_sign_params_with_fixed_wts() {
        let key = mixin_key(WBI_TEST_IMG, WBI_TEST_SUB);
        let mut params = vec![
            ("foo".to_string(), "one two".to_string()),
            ("zoo".to_string(), "12".to_string()),
            ("bar".to_string(), "!'()*".to_string()),
        ];
        let w_rid = wbi_sign_params_with_wts(&mut params, &key, 1_700_000_000);
        assert_eq!(w_rid.len(), 32);
        assert!(w_rid.chars().all(|c| c.is_ascii_hexdigit()));
        // wts 已被追加进 params（供调用方拼 query）。
        assert!(params.iter().any(|(k, v)| k == "wts" && v == "1700000000"));
        // 同输入同输出（不含 wts 时值里的特殊字符被过滤）。
        let mut params2 = params.clone();
        params2.pop(); // 去掉 wts
        assert_eq!(wbi_sign_params_with_wts(&mut params2, &key, 1_700_000_000), w_rid);
    }

    // ---- Set-Cookie / query 解析 ----

    #[test]
    fn test_parse_set_cookie() {
        let (k, v) = parse_set_cookie(
            "SESSDATA=abc%2Cdef; Path=/; Domain=.bilibili.com; Secure; HttpOnly; SameSite=None",
        )
        .unwrap();
        assert_eq!(k, "SESSDATA");
        assert_eq!(v, "abc%2Cdef");
        assert!(parse_set_cookie("invalid").is_none());
    }

    // ---- 二维码矩阵 ----

    #[test]
    fn test_qrcode_matrix_shape() {
        let m = BiliClient::qrcode_matrix("https://passport.bilibili.com/x?qrcode_key=test123").unwrap();
        assert!(!m.is_empty());
        assert_eq!(m.len(), m[0].len(), "矩阵应为正方形");
        let dark = m.iter().flatten().filter(|b| **b).count();
        assert!(dark > 0, "二维码必须有深色模块");
        // 找回三个定位角的实心块（左上角 7x7 内应有大量深色）。
        let corner_dark = m[0][0] && m[0][6] && m[6][0] && m[3][3];
        assert!(corner_dark, "定位角应存在");
    }

    // ---- StreamUrl 头清单 ----

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
