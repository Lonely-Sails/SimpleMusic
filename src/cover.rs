//! 封面缩略图系统：异步下载 B 站视频封面 → 解码 → 小尺寸纹理缓存。
//!
//! - 下载走全局 tokio runtime（[`crate::net`]，不再占用专用 OS 线程），解码（CPU 密集）
//!   在 `spawn_blocking` 上执行，结果经 mpsc 回主线程；
//! - 主线程每帧 [`CoverCache::poll`] 排空 channel，存入缓存并注册（lazy）egui 纹理；
//! - **并发有界**：同时最多 [`MAX_IN_FLIGHT`] 张在飞（大歌单启动预取上百张时
//!   不会一次性发出上百个请求）；
//! - **优先级调度**：可视区域内的封面、头像、当前播放曲目走 [`Prio::High`]
//!   队列，先于后台预取（[`Prio::Low`]）派发；已在低优先级队列里的任务被
//!   可视请求命中时就地提升，因此「滚到哪、先出哪」；
//! - **共享 HTTP 客户端**：复用 [`crate::net::http_client`]（不再自建客户端与线程）；
//! - 失败缓存 30 分钟不重试；内存条目上限 400，超出按最久未访问清理 100 条。
//!
//! 本模块不依赖项目的主题色板（不 import crate::theme），保持可独立测试。

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

/// 缩略图边长（方形裁剪）。
const THUMB_SIZE: u32 = 96;
/// 下载上限：超过即放弃（防超大图/恶意响应）。
const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
/// 下载失败后重试间隔。
const FAILED_RETRY_AFTER: Duration = Duration::from_secs(30 * 60);
/// 内存缓存上限与清理后保留数。
const MAX_ENTRIES: usize = 400;
const PRUNE_KEEP: usize = 300;
/// 失败冷却表的整理阈值：超过这么多条失败记录时顺手清掉已过冷却期的。
const FAILED_PRUNE_THRESHOLD: usize = 64;
/// 同时在飞的封面下载上限。
///
/// 封面是小文件（≤2MB）、顺序不重要；限制在飞数量可以避免大歌单启动时
/// 一次性排队上百个请求，也让共享 runtime 的 worker 留给解析/歌词等关键任务。
const MAX_IN_FLIGHT: usize = 6;
/// B 站图床也校验 UA（防盗链）。
const COVER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 单个条目的缓存态。
type CoverImage = Arc<ColorImage>;

/// 下载优先级。
///
/// 大歌单/收藏夹会在启动或翻页时一次性预取上百张封面，若不分优先级，
/// 用户当前看到的几张可能要排在这些后台任务后面。可视区域内的封面、
/// 用户头像、当前播放曲目一律走 [`Prio::High`]，其余预取走 [`Prio::Low`]。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prio {
    /// 可视区域内的封面、头像、当前播放曲目。
    High,
    /// 后台预取（当前看不到的条目）。
    Low,
}

/// 一个待下载任务（`request` 投递，调度器消费）。
struct CoverJob {
    key: String,
    url: String,
}

/// 封面缓存（UI 线程持有；`request` 可随时调用，内部自行去重）。
pub struct CoverCache {
    ctx: egui::Context,
    rx: Receiver<(String, Result<CoverImage, String>)>,
    /// 结果回传端（派发的异步任务各持一份）。
    tx: Sender<(String, Result<CoverImage, String>)>,
    /// 高优先级待下载队列（可视封面/头像/当前曲目），先于 `pending_low` 派发。
    pending_high: VecDeque<CoverJob>,
    /// 低优先级待下载队列（后台预取）。
    pending_low: VecDeque<CoverJob>,
    /// key(bvid) -> (解码图(注册纹理后释放), 延迟注册的纹理, 最近访问时间)。
    images: HashMap<String, (Option<CoverImage>, Option<TextureHandle>, Instant)>,
    /// key -> 最近失败时间。
    failed: HashMap<String, Instant>,
    /// 已入队或已派发、尚未出结果的 key -> 其优先级（去重 + 提升用，
    /// **不是**在飞计数）。
    in_flight: HashMap<String, Prio>,
    /// 已派发到 runtime、尚未回结果的**实际**在飞任务数（限流用）。
    ///
    /// 必须与 `in_flight.len()` 区分开：`request` 会把整批 key 一次性塞进队列，
    /// 若用集合长度限流，队列一长就永远达不到派发条件（封面全都不加载）。
    active: usize,
}

impl CoverCache {
    /// 用 egui context 创建缓存。
    ///
    /// 不启动任何专用线程：下载任务在 [`Self::poll`]（每帧调用）里限流派发到全局
    /// runtime（[`crate::net::spawn`]），解码在 `spawn_blocking` 上执行。
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ctx,
            rx,
            tx,
            pending_high: VecDeque::new(),
            pending_low: VecDeque::new(),
            images: HashMap::new(),
            failed: HashMap::new(),
            in_flight: HashMap::new(),
            active: 0,
        }
    }

    /// 把待下载队列限流派发到全局 runtime（在飞数不超过 [`MAX_IN_FLIGHT`]）。
    ///
    /// 先取空高优先级队列再取低优先级，保证可视封面/头像优先于后台预取。
    fn dispatch_pending(&mut self) {
        while self.active < MAX_IN_FLIGHT {
            let job = self
                .pending_high
                .pop_front()
                .or_else(|| self.pending_low.pop_front());
            let Some(job) = job else {
                break;
            };
            self.active += 1;
            let tx = self.tx.clone();
            crate::net::spawn(async move {
                let result = match fetch_cover_bytes(&job.url).await {
                    Ok(bytes) => crate::net::spawn_blocking(move || {
                        decode_cover(&bytes)
                            .map(Arc::new)
                            .ok_or_else(|| "封面解码失败".to_string())
                    })
                    .await
                    .unwrap_or_else(|e| Err(format!("解码任务失败: {e}"))),
                    Err(e) => Err(e),
                };
                let _ = tx.send((job.key, result));
            });
        }
    }

    /// 请求加载封面（**低优先级**，后台预取用）。key 一般用 bvid。
    pub fn request(&mut self, key: &str, url: &str) {
        self.request_with_priority(key, url, Prio::Low);
    }

    /// 请求加载封面（**高优先级**：可视区域内的封面、头像、当前播放曲目）。
    pub fn request_visible(&mut self, key: &str, url: &str) {
        self.request_with_priority(key, url, Prio::High);
    }

    /// 请求加载封面。url 为空/已缓存/失败未过期则跳过。
    ///
    /// 已在队列中的任务被更高优先级请求命中时**就地提升**（低 → 高），
    /// 这样「用户滚到哪，哪张封面先下」，而不必等后台预取排完。
    pub fn request_with_priority(&mut self, key: &str, url: &str, prio: Prio) {
        if url.trim().is_empty() || key.is_empty() {
            return;
        }
        if self.images.contains_key(key) {
            return;
        }
        if let Some(&existing) = self.in_flight.get(key) {
            if prio == Prio::High && existing == Prio::Low {
                self.in_flight.insert(key.to_string(), Prio::High);
                if let Some(pos) = self.pending_low.iter().position(|j| j.key == key) {
                    if let Some(job) = self.pending_low.remove(pos) {
                        self.pending_high.push_back(job);
                    }
                }
            }
            return;
        }
        let now = Instant::now();
        if let Some(failed_at) = self.failed.get(key) {
            if is_failed_active(*failed_at, now) {
                return;
            }
            self.failed.remove(key); // 过期失败：允许重试
        }
        self.in_flight.insert(key.to_string(), prio);
        // 入待下载队列；实际派发在 `poll`（限流，不一次性发上百个请求）。
        let job = CoverJob {
            key: key.to_string(),
            url: url.to_string(),
        };
        match prio {
            Prio::High => self.pending_high.push_back(job),
            Prio::Low => self.pending_low.push_back(job),
        }
    }

    /// 每帧调用：限流派发待下载任务 + 排空下载结果，成功入缓存，失败记入失败表。
    pub fn poll(&mut self) {
        self.dispatch_pending();
        while let Ok((key, result)) = self.rx.try_recv() {
            self.active = self.active.saturating_sub(1);
            self.in_flight.remove(&key);
            match result {
                Ok(img) => {
                    self.images.insert(key, (Some(img), None, Instant::now()));
                    prune_oldest(&mut self.images, MAX_ENTRIES, PRUNE_KEEP);
                }
                Err(e) => {
                    crate::util::log::warn("cover", &format!("封面加载失败 {key}: {e}"));
                    self.failed.insert(key, Instant::now());
                }
            }
        }
        // 失败冷却表攒多了顺手整理一次（已过冷却期的条目没有保留价值）。
        if self.failed.len() > FAILED_PRUNE_THRESHOLD {
            let now = Instant::now();
            self.failed.retain(|_, t| is_failed_active(*t, now));
        }
    }

    /// 获取（或延迟创建）egui 纹理 id。纹理注册后立即释放解码缓冲——
    /// egui 的纹理管理器自留一份 CPU 拷贝，我们再留一份纯属双倍内存。
    pub fn texture(&mut self, key: &str) -> Option<egui::TextureId> {
        let entry = self.images.get_mut(key)?;
        entry.2 = Instant::now();
        if entry.1.is_none() {
            let Some(img) = entry.0.take() else {
                return None;
            };
            let handle = self.ctx.load_texture(
                format!("simple-music-cover:{key}"),
                (*img).clone(),
                TextureOptions::LINEAR,
            );
            entry.1 = Some(handle);
        }
        entry.1.as_ref().map(|h| h.id())
    }
}

/// 异步下载封面字节（工作线程调用）。用全局共享客户端；超过上限放弃。
async fn fetch_cover_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = crate::net::http_client();
    let resp = client
        .get(url)
        .header(reqwest::header::USER_AGENT, COVER_UA)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("下载封面失败: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载封面失败: HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("读取封面失败: {e}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("封面过大({} bytes)，放弃", bytes.len()));
    }
    Ok(bytes.to_vec())
}

/// 解码 + 居中方形裁剪 + 缩略。任何一步失败返回 None。
pub fn decode_cover(raw: &[u8]) -> Option<ColorImage> {
    let img = image::load_from_memory(raw).ok()?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    // 居中裁成正方形（B 站封面 16:9，裁掉左右）。
    let side = w.min(h);
    let x = (w - side) / 2;
    let y = (h - side) / 2;
    let cropped = img.crop_imm(x, y, side, side).to_rgba8();
    let thumb = image::imageops::thumbnail(&cropped, THUMB_SIZE, THUMB_SIZE);
    let size = [thumb.width() as usize, thumb.height() as usize];
    Some(ColorImage::from_rgba_unmultiplied(size, thumb.as_raw()))
}

/// 失败重试是否仍处于冷却期。
pub fn is_failed_active(failed_at: Instant, now: Instant) -> bool {
    now.saturating_duration_since(failed_at) < FAILED_RETRY_AFTER
}

/// 条目总数超过 `max` 时，按最近访问时间清理最旧的，保留 `keep` 条。
fn prune_oldest<T>(
    map: &mut HashMap<String, (T, Option<TextureHandle>, Instant)>,
    _max: usize,
    keep: usize,
) {
    if map.len() <= keep {
        return;
    }
    let mut keys: Vec<String> = map.keys().cloned().collect();
    // 最近访问的排在后面，drop 最旧的前面部分（保留 keep 条）。
    keys.sort_by(|a, b| {
        let ta = map[a].2;
        let tb = map[b].2;
        // 升序：最旧在前。drop 取前面的部分 = 清最旧。
        ta.cmp(&tb).then(a.cmp(b))
    });
    let drop = keys.len().saturating_sub(keep);
    for k in keys.into_iter().take(drop) {
        map.remove(&k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PNG: &[u8] = include_bytes!("../tests/fixtures/px1.png");

    #[test]
    fn decode_cover_handles_real_png() {
        let img = decode_cover(TEST_PNG).expect("1x1 png 应能解码");
        // 缩略(96×96)会等比上采样小图，1x1 -> 96x96。
        assert_eq!(img.size, [96, 96]);
        let px = img.pixels.first().unwrap();
        assert_ne!(px[3], 0, "像素应非全透明");
    }

    #[test]
    fn decode_cover_rejects_garbage() {
        assert!(decode_cover(b"not an image at all").is_none());
        assert!(decode_cover(&[]).is_none());
    }

    #[test]
    fn prune_keeps_most_recent() {
        let mut map = HashMap::new();
        let now = Instant::now();
        map.insert(
            "old".to_string(),
            (1u8, None, now - Duration::from_secs(100)),
        );
        map.insert(
            "mid".to_string(),
            (2u8, None, now - Duration::from_secs(50)),
        );
        map.insert("new".to_string(), (3u8, None, now));
        prune_oldest(&mut map, 3, 2);
        assert!(!map.contains_key("old"));
        assert!(map.contains_key("mid"));
        assert!(map.contains_key("new"));
    }

    #[test]
    fn failed_retry_cooldown() {
        let now = Instant::now();
        assert!(is_failed_active(now - Duration::from_secs(60), now));
        assert!(!is_failed_active(now - Duration::from_secs(31 * 60), now));
    }

    #[test]
    fn request_skips_bad_input() {
        // request 不立刻网络请求（任务进队列由工作线程消费），
        // 空 url 不应入队；直接验证逻辑分支。
        let ctx = egui::Context::default();
        let mut cc = CoverCache::new(ctx);
        cc.request("BV1", "");
        assert!(cc.in_flight.is_empty());
    }

    #[test]
    fn request_dedups_and_enqueues() {
        let ctx = egui::Context::default();
        let mut cc = CoverCache::new(ctx);
        cc.request("BV1", "https://example.com/a.jpg");
        assert!(cc.in_flight.contains_key("BV1"), "首次请求应入队");
        // 已在下载中：不重复入队。
        cc.request("BV1", "https://example.com/a.jpg");
        // 已缓存/失败分支在 poll 侧，这里只验证 in_flight 去重不 panic。
        assert_eq!(cc.in_flight.len(), 1);
        assert_eq!(cc.pending_low.len(), 1, "只应入队一次");
    }

    /// 回归：一次性入队超过 [`MAX_IN_FLIGHT`] 的任务时，`poll` 仍必须派发
    /// （曾用 `in_flight.len()` 限流，队列一长就永远派发不出去 → 封面全不显示）。
    #[test]
    fn dispatch_starts_even_when_queue_exceeds_limit() {
        let ctx = egui::Context::default();
        let mut cc = CoverCache::new(ctx);
        for i in 0..(MAX_IN_FLIGHT * 5) {
            cc.request(&format!("BV{i}"), &format!("https://example.com/{i}.jpg"));
        }
        assert_eq!(cc.in_flight.len(), MAX_IN_FLIGHT * 5, "全部应已入队去重表");
        cc.poll();
        assert_eq!(cc.active, MAX_IN_FLIGHT, "应立刻派发满在飞上限");
        assert_eq!(
            cc.pending_low.len(),
            MAX_IN_FLIGHT * 4,
            "其余任务留在队列等待"
        );
    }

    /// 高优先级（可视封面/头像）先于低优先级（后台预取）派发。
    #[test]
    fn high_priority_dispatches_before_low() {
        let ctx = egui::Context::default();
        let mut cc = CoverCache::new(ctx);
        // 先用后台预取塞满队列。
        for i in 0..(MAX_IN_FLIGHT * 2) {
            cc.request(&format!("low{i}"), &format!("https://example.com/l{i}.jpg"));
        }
        // 再来一张可视封面。
        cc.request_visible("vis", "https://example.com/v.jpg");
        cc.poll();
        assert_eq!(cc.active, MAX_IN_FLIGHT);
        // 可视任务必须已被派发（不在任何待队列里）。
        assert!(cc.pending_high.is_empty(), "高优先级队列应已取空");
        assert_eq!(cc.pending_low.len(), MAX_IN_FLIGHT * 2 - MAX_IN_FLIGHT + 1);
    }

    /// 已在低优先级队列里的任务被可视请求命中时就地提升到高优先级。
    #[test]
    fn visible_request_promotes_queued_job() {
        let ctx = egui::Context::default();
        let mut cc = CoverCache::new(ctx);
        cc.request("BV1", "https://example.com/a.jpg");
        assert_eq!(cc.pending_low.len(), 1);
        assert_eq!(cc.pending_high.len(), 0);
        cc.request_visible("BV1", "https://example.com/a.jpg");
        assert_eq!(cc.pending_low.len(), 0, "应从低优先级队列移出");
        assert_eq!(cc.pending_high.len(), 1, "应提升到高优先级队列");
        assert_eq!(cc.in_flight.get("BV1"), Some(&Prio::High));
        assert_eq!(cc.in_flight.len(), 1, "提升不应产生重复条目");
    }

    /// 已派发（在飞）的任务无法提升：不 panic、不重复入队。
    #[test]
    fn visible_request_on_inflight_job_is_noop() {
        let ctx = egui::Context::default();
        let mut cc = CoverCache::new(ctx);
        cc.request("BV1", "https://example.com/a.jpg");
        cc.poll();
        assert_eq!(cc.active, 1, "应已派发");
        cc.request_visible("BV1", "https://example.com/a.jpg");
        assert_eq!(cc.active, 1);
        assert_eq!(cc.in_flight.len(), 1);
        assert!(cc.pending_high.is_empty());
        assert!(cc.pending_low.is_empty());
    }

    /// 真实网络验证：B 站公开视频 → view 接口封面 URL → 下载 → 解码。
    /// 需要网络；`cargo test -- --ignored network_cover_decode` 手动运行。
    #[test]
    #[ignore]
    fn network_cover_decode_real_bilibili_cover() {
        let client = crate::net::http_client();
        let resp = crate::net::block_on(
            client
                .get("https://api.bilibili.com/x/web-interface/view?bvid=BV1xx411c7mD")
                .header(reqwest::header::USER_AGENT, COVER_UA)
                .send(),
        )
        .expect("view 请求失败");
        let body: serde_json::Value = crate::net::block_on(resp.json()).expect("json 解析失败");
        let cover = body["data"]["pic"]
            .as_str()
            .expect("无 pic 字段")
            .to_string();
        eprintln!("[cover] cover_url = {cover}");
        let bytes = crate::net::block_on(fetch_cover_bytes(&cover)).expect("封面下载失败");
        eprintln!("[cover] downloaded {} bytes", bytes.len());
        let img = decode_cover(&bytes).expect("封面解码失败");
        eprintln!("[cover] decoded {}x{}", img.size[0], img.size[1]);
        let mid = img.pixels[img.pixels.len() / 2];
        eprintln!("[cover] center pixel rgba = {:?}", mid);
        assert_eq!(img.size, [96, 96]);
    }
}
