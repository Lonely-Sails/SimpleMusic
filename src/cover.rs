//! 封面缩略图系统：异步下载 B 站视频封面 → 解码 → 小尺寸纹理缓存。
//!
//! - 后台线程用 reqwest blocking 下载（带 UA；上限 2MB），结果经 mpsc 回主线程；
//! - 主线程每帧 [`CoverCache::poll`] 排空 channel，`image` 解码 + 居中方形裁剪 +
//!   缩略到 96px，然后（lazy）注册为 egui 纹理；
//! - 失败缓存 30 分钟不重试；内存条目上限 400，超出按最久未访问清理 100 条。
//!
//! 本模块不依赖项目的主题色板（不 import crate::theme），保持可独立测试。

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
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
/// B 站图床也校验 UA（防盗链）。
const COVER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// 单个条目的缓存态。
type CoverImage = Arc<ColorImage>;

/// 封面缓存（UI 线程持有；`request` 可随时调用，内部自行去重）。
pub struct CoverCache {
    ctx: egui::Context,
    tx: Sender<(String, Result<Vec<u8>, String>)>,
    rx: Receiver<(String, Result<Vec<u8>, String>)>,
    /// key(bvid) -> (解码图, 延迟注册的纹理, 最近访问时间)。
    images: HashMap<String, (CoverImage, Option<TextureHandle>, Instant)>,
    /// key -> 最近失败时间。
    failed: HashMap<String, Instant>,
    /// 正在下载中的 key。
    in_flight: HashSet<String>,
}

impl CoverCache {
    /// 用 egui context 创建缓存。context 仅用于延迟注册纹理。
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            ctx,
            tx,
            rx,
            images: HashMap::new(),
            failed: HashMap::new(),
            in_flight: HashSet::new(),
        }
    }

    /// 请求加载封面。key 一般用 bvid；url 为空/已缓存/已在下载/失败未过期则跳过。
    pub fn request(&mut self, key: &str, url: &str) {
        if url.trim().is_empty() || key.is_empty() {
            return;
        }
        if self.images.contains_key(key) || self.in_flight.contains(key) {
            return;
        }
        let now = Instant::now();
        if let Some(failed_at) = self.failed.get(key) {
            if now.saturating_duration_since(*failed_at) < FAILED_RETRY_AFTER {
                return;
            }
            self.failed.remove(key); // 过期失败：允许重试
        }
        self.in_flight.insert(key.to_string());
        let tx = self.tx.clone();
        let key = key.to_string();
        let url = url.to_string();
        std::thread::spawn(move || {
            let result = download_cover(&key, &url);
            let _ = tx.send((key, result));
        });
    }

    /// 每帧调用：排空下载结果，成功入缓存，失败记入失败表。
    pub fn poll(&mut self) {
        while let Ok((key, result)) = self.rx.try_recv() {
            self.in_flight.remove(&key);
            match result {
                Ok(bytes) => match decode_cover(&bytes) {
                    Some(img) => {
                        self.images.insert(
                            key,
                            (Arc::new(img), None, Instant::now()),
                        );
                        prune_oldest(
                            &mut self.images,
                            MAX_ENTRIES,
                            PRUNE_KEEP,
                        );
                    }
                    None => {
                        self.failed.insert(key, Instant::now());
                    }
                },
                Err(_) => {
                    self.failed.insert(key, Instant::now());
                }
            }
        }
    }

    /// 取解码后的图像（无则 None，供绘制占位判断）。会刷新最近访问时间。
    pub fn image(&mut self, key: &str) -> Option<&ColorImage> {
        self.images.get_mut(key).map(|e| {
            e.2 = Instant::now();
            &*e.0
        })
    }

    /// 获取（或延迟创建）egui 纹理 id。
    pub fn texture(&mut self, key: &str) -> Option<egui::TextureId> {
        let entry = self.images.get_mut(key)?;
        entry.2 = Instant::now();
        if entry.1.is_none() {
            let handle = self.ctx.load_texture(
                format!("simple-music-cover:{key}"),
                (*entry.0).clone(),
                TextureOptions::LINEAR,
            );
            entry.1 = Some(handle);
        }
        entry.1.as_ref().map(|h| h.id())
    }
}

/// 下载封面（后台线程调用）。带 UA；超过上限放弃。
fn download_cover(key: &str, url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(COVER_UA)
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("http 客户端构建失败: {e}"))?;
    let resp = client
        .get(url)
        .send()
        .map_err(|e| format!("下载封面失败({key}): {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("下载封面失败({key}): HTTP {}", resp.status()));
    }
    let bytes = resp
        .bytes()
        .map_err(|e| format!("读取封面失败({key}): {e}"))?;
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
        map.insert("old".to_string(), (1u8, None, now - Duration::from_secs(100)));
        map.insert("mid".to_string(), (2u8, None, now - Duration::from_secs(50)));
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
        // request 不立刻网络请求（线程异步），只要不 panic 且 in_flight 正确标记即可。
        // 空 url 不应入 in_flight；直接验证逻辑分支。
        let ctx = egui::Context::default();
        let mut cc = CoverCache::new(ctx);
        cc.request("BV1", "");
        assert!(cc.in_flight.is_empty());
    }

    /// 真实网络验证：B 站公开视频 → view 接口封面 URL → 下载 → 解码。
    /// 需要网络；`cargo test -- --ignored network_cover_decode` 手动运行。
    #[test]
    #[ignore]
    fn network_cover_decode_real_bilibili_cover() {
        let client = reqwest::blocking::Client::builder()
            .user_agent(COVER_UA)
            .build()
            .unwrap();
        let resp = client
            .get("https://api.bilibili.com/x/web-interface/view?bvid=BV1xx411c7mD")
            .send()
            .expect("view 请求失败");
        let body: serde_json::Value = resp.json().expect("json 解析失败");
        let cover = body["data"]["pic"]
            .as_str()
            .expect("无 pic 字段")
            .to_string();
        eprintln!("[cover] cover_url = {cover}");
        let bytes = download_cover("BV1xx411c7mD", &cover).expect("封面下载失败");
        eprintln!("[cover] downloaded {} bytes", bytes.len());
        let img = decode_cover(&bytes).expect("封面解码失败");
        eprintln!("[cover] decoded {}x{}", img.size[0], img.size[1]);
        let mid = img.pixels[img.pixels.len() / 2];
        eprintln!("[cover] center pixel rgba = {:?}", mid);
        assert_eq!(img.size, [96, 96]);
    }
}
