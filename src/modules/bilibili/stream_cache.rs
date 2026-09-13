//! 已解析音频直链缓存（B 站 playurl 直链 + TTL）。
//!
//! B 站 playurl 返回的直链带签名，**会过期**（CDN 侧通常 2 小时内失效，也可能提前），
//! 所以直链缓存必须有 TTL：这里取保守的 [`STREAM_CACHE_TTL`]（10 分钟）。
//!
//! - TTL 内重复播放同一首歌（切歌来回、单曲循环、重播）直接复用直链，省一次
//!   `playurl` 往返（含被风控时的 WBI 签名重试），出声更快；
//! - 超过 TTL 一律重新解析，**绝不拿过期直链去下载**（否则 403）。
//!
//! 条目里同时存 [`QueueItem`] 元数据：命中缓存时标题/时长/封面/cid 一并复用，
//! 免一次 `video_info`，也避免「命中缓存后界面没有歌曲信息」。
//!
//! 与音频磁盘缓存（`modules::audio::cache`，`<md5(bvid)>.m4s`）互相独立：磁盘缓存
//! 命中时压根不需要直链，本缓存只在「需要下载」时才被用到。
//!
//! 键为 `(bvid, 音质)`：同一首歌换音质偏好必须重新解析，否则会拿到旧音质的直链。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::models::StreamUrl;
use crate::state::{AudioQuality, QueueItem};

/// 直链缓存有效期：超过即视为过期，重新解析。
pub const STREAM_CACHE_TTL: Duration = Duration::from_secs(10 * 60);

/// 条目上限（超限按写入时间淘汰最旧一条；正常听歌远达不到）。
const MAX_ENTRIES: usize = 256;

/// 缓存键：bvid + 音质偏好。
type Key = (String, AudioQuality);

struct Entry {
    item: QueueItem,
    stream: StreamUrl,
    at: Instant,
}

/// 已解析直链缓存。UI 线程持有；后台解析线程经 `Arc<Mutex<…>>` 共享读写。
#[derive(Default)]
pub struct StreamCache {
    map: HashMap<Key, Entry>,
}

impl StreamCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// 取未过期的 `(QueueItem, StreamUrl)`；过期条目顺手删除。
    /// 返回 `None` 时调用方应重新解析。
    pub fn get(&mut self, bvid: &str, quality: AudioQuality) -> Option<(QueueItem, StreamUrl)> {
        let key = (bvid.to_string(), quality);
        let fresh = self
            .map
            .get(&key)
            .map(|e| is_fresh(e.at.elapsed()))
            .unwrap_or(false);
        if fresh {
            self.map
                .get(&key)
                .map(|e| (e.item.clone(), e.stream.clone()))
        } else {
            self.map.remove(&key);
            None
        }
    }

    /// 写入（或覆盖）一条；达到上限时先淘汰最旧的一条。
    pub fn put(&mut self, bvid: &str, quality: AudioQuality, item: QueueItem, stream: StreamUrl) {
        let key = (bvid.to_string(), quality);
        if self.map.len() >= MAX_ENTRIES && !self.map.contains_key(&key) {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.at)
                .map(|(k, _)| k.clone());
            if let Some(k) = oldest {
                self.map.remove(&k);
            }
        }
        self.map.insert(
            key,
            Entry {
                item,
                stream,
                at: Instant::now(),
            },
        );
    }

    /// 丢弃某首歌的直链。下载报 403/410（疑似直链过期）时调用，
    /// 使下一次解析强制走网络而不是复用坏直链。
    pub fn remove(&mut self, bvid: &str, quality: AudioQuality) {
        self.map.remove(&(bvid.to_string(), quality));
    }

    /// 清空全部。登出时调用：直链带 Cookie 签名，换账号必须重新解析。
    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// 当前条目数（诊断/测试用）。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// 是否为空（诊断/测试用）。
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// TTL 判定（纯函数）：距写入未超过 [`STREAM_CACHE_TTL`] 才算新鲜。
pub fn is_fresh(elapsed: Duration) -> bool {
    elapsed < STREAM_CACHE_TTL
}

/// 判断一次下载失败是否属于「直链已过期」——B 站 CDN 对失效签名直链回 403/410，
/// 少数情况回 404。命中时上层应重新解析后重试（仅一次）。
///
/// 注意：需与真正的网络故障区分（超时/连接失败重试没意义），所以只认状态码。
pub fn is_stream_expired_error(err: &str) -> bool {
    // 失败文案形如「HTTP 403」「流地址返回 HTTP 410」或聚合文案「…: HTTP 403；HTTP 403」。
    ["HTTP 403", "HTTP 410", "HTTP 404"]
        .iter()
        .any(|code| err.contains(code))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(url: &str) -> StreamUrl {
        StreamUrl {
            audio_url: url.to_string(),
            video_url: None,
            ttl_secs: 0,
            audio_id: Some(30280),
            audio_codec: None,
            bandwidth: None,
            size_bytes: None,
            audio_backup_urls: Vec::new(),
            required_headers: Vec::new(),
            signed_with_wbi: false,
        }
    }

    fn item(bvid: &str) -> QueueItem {
        QueueItem::new(bvid, "标题", "UP主", 123.0)
    }

    #[test]
    fn ttl_boundary() {
        assert!(is_fresh(Duration::from_secs(0)));
        assert!(is_fresh(STREAM_CACHE_TTL - Duration::from_secs(1)));
        assert!(!is_fresh(STREAM_CACHE_TTL));
        assert!(!is_fresh(STREAM_CACHE_TTL + Duration::from_secs(1)));
        assert_eq!(
            STREAM_CACHE_TTL,
            Duration::from_secs(600),
            "TTL 应为 10 分钟"
        );
    }

    #[test]
    fn put_then_get_hits_with_metadata_and_misses_on_other_key() {
        let mut c = StreamCache::new();
        c.put(
            "BV1",
            AudioQuality::High,
            item("BV1"),
            stream("https://x/a.m4s"),
        );
        let (it, st) = c.get("BV1", AudioQuality::High).expect("应命中");
        assert_eq!(st.audio_url, "https://x/a.m4s");
        assert_eq!(it.title, "标题", "命中时元数据不应丢");
        assert_eq!(it.duration_secs, 123.0);
        // 不同 bvid / 不同音质都是独立键。
        assert!(c.get("BV2", AudioQuality::High).is_none());
        assert!(c.get("BV1", AudioQuality::Low).is_none());
    }

    #[test]
    fn expired_entry_is_dropped() {
        let mut c = StreamCache::new();
        c.put(
            "BV1",
            AudioQuality::High,
            item("BV1"),
            stream("https://x/a.m4s"),
        );
        // 手工把写入时刻拨回 TTL 之前（模拟 10 分钟后再播）。
        c.map
            .get_mut(&("BV1".to_string(), AudioQuality::High))
            .unwrap()
            .at = Instant::now() - STREAM_CACHE_TTL - Duration::from_secs(1);
        assert!(c.get("BV1", AudioQuality::High).is_none(), "过期应未命中");
        assert!(c.is_empty(), "过期条目应被顺手删除");
    }

    #[test]
    fn put_overwrites_and_remove_clears_one_key() {
        let mut c = StreamCache::new();
        c.put(
            "BV1",
            AudioQuality::High,
            item("BV1"),
            stream("https://x/old.m4s"),
        );
        c.put(
            "BV1",
            AudioQuality::High,
            item("BV1"),
            stream("https://x/new.m4s"),
        );
        assert_eq!(c.len(), 1, "同键覆盖不新增条目");
        assert_eq!(
            c.get("BV1", AudioQuality::High).unwrap().1.audio_url,
            "https://x/new.m4s"
        );
        c.put(
            "BV2",
            AudioQuality::High,
            item("BV2"),
            stream("https://x/b.m4s"),
        );
        c.remove("BV1", AudioQuality::High);
        assert!(c.get("BV1", AudioQuality::High).is_none());
        assert!(c.get("BV2", AudioQuality::High).is_some(), "只删指定键");
        c.clear();
        assert!(c.is_empty(), "clear 应清空全部");
    }

    #[test]
    fn evicts_oldest_when_full() {
        let mut c = StreamCache::new();
        for i in 0..MAX_ENTRIES {
            let bv = format!("BV{i}");
            c.put(
                &bv,
                AudioQuality::High,
                item(&bv),
                stream("https://x/a.m4s"),
            );
        }
        // 把第一条拨到最旧，再插一条新的触发淘汰。
        c.map
            .get_mut(&("BV0".to_string(), AudioQuality::High))
            .unwrap()
            .at = Instant::now() - Duration::from_secs(3600);
        c.put(
            "BVNEW",
            AudioQuality::High,
            item("BVNEW"),
            stream("https://x/new.m4s"),
        );
        assert_eq!(c.len(), MAX_ENTRIES, "容量不超上限");
        assert!(
            c.get("BV0", AudioQuality::High).is_none(),
            "最旧条目应被淘汰"
        );
        assert!(c.get("BVNEW", AudioQuality::High).is_some(), "新条目应保留");
    }

    #[test]
    fn stream_expired_error_detects_cdn_status_codes() {
        assert!(is_stream_expired_error("HTTP 403"));
        assert!(is_stream_expired_error("流地址返回 HTTP 410，换备用地址"));
        assert!(is_stream_expired_error(
            "音频下载失败（已尝试 2 个地址）: HTTP 403；HTTP 404"
        ));
        // 网络故障不算「直链过期」：重试同一直链也没用，应由用户重试。
        assert!(!is_stream_expired_error("请求失败: connection timed out"));
        assert!(!is_stream_expired_error("没有可用的音频流地址"));
        assert!(!is_stream_expired_error("HTTP 500"));
    }
}
