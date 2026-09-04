//! `BiliClient` 基座：HTTP 客户端构建、会话管理、信封解包与 WBI key 缓存。
//!
//! 登录/收藏夹/解析各方法组分别在 [`super::login`] / [`super::fav`] /
//! [`super::resolve`]，以 `impl BiliClient` 的形式挂到本结构体上。

use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use super::error::{BiliError, BiliResult};
use super::{ORIGIN, REFERER, USER_AGENT};
use super::models::*;
use super::wbi::{encode_uri_component, WbiKeys};
use crate::modules::storage::{self, BiliSession};

// ---------------------------------------------------------------------------
// 客户端
// ---------------------------------------------------------------------------

/// B 站 API 客户端（blocking）。
///
/// UI 集成建议：在 `std::thread` 中持有 `BiliClient`，用 channel 把结果发回 GUI 线程，
/// 避免 blocking IO 阻塞渲染。
pub struct BiliClient {
    pub(super) http: reqwest::blocking::Client,
    /// 会话（cookies + buvid），与磁盘 session.json 同步。
    pub(super) session: BiliSession,
    /// WBI key 缓存（约 30 分钟刷新一次）。
    pub(super) wbi_cache: Mutex<Option<(WbiKeys, Instant)>>,
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

    /// 当前会话快照（诊断/探针用：查看 buvid 等设备指纹；不要打印 cookie 值）。
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
}

/// 解析 URL query 片段（仅 path 后的 k=v&…；value 不解码，B 站登录 url 里
/// SESSDATA 已是可直接入 Cookie 的编码值）。poll_login 的 data.url 兜底与
/// parse_bvid 共用。
pub(super) fn parse_query_params(url: &str) -> Vec<(String, String)> {
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
