//! 文本「真·模糊阴影」：skrifa 取字形轮廓 → vello_cpu 离屏光栅化 → 盒滤波近似
//! 高斯模糊 → egui 纹理，垫在文字下方实现 CSS `text-shadow` 式向四周晕开的柔影。
//!
//! ## 为什么不用「复制几层文字垫在下面」
//!
//! egui 没有文本模糊滤镜，多层不同偏移/透明度的文字副本只能凑出「同心硬边叠影」，
//! 晕不开。真正的晕开感来自高斯模糊，而模糊只能作用在位图上——所以把整行文本的
//! 字形轮廓离屏光栅化成一张 alpha 位图，对它做模糊，再以单张纹理垫在文字下层。
//!
//! ## 性能约定
//!
//! 纹理以 `(文本, 像素字号, σ, 强度)` 为键缓存在 `ShadowCache` 里（挂在共享
//! `egui::Context` 的 data 槽上，跨帧复用）。歌词过渡动画期间同一文本逐帧命中
//! 缓存，每帧只多一次四边形绘制；只有切行那一帧才光栅化 + 模糊（一次 <1ms 级）。
//!
//! **锁纪律（重要）**：`ShadowCache` 存在 `Context::data_mut`（写锁）里，但
//! [`ShadowCache::get`] / [`ShadowCache::insert`] 必须是两个独立临界区——光栅化
//! 与 `ctx.load_texture` 绝不能在 `data_mut` 闭包内执行。`load_texture` 内部会经
//! `Context::input`/`tex_manager` 再次获取同一把 `ContextImpl` 写锁，egui 的
//! `epaint::RwLock` 不可重入，嵌套调用 = 同线程死锁（debug 构建 10s 后 panic，
//! 见 epaint `mutex.rs` 的 DEADLOCK_DURATION）。
//!
//! 光栅化后端用 vello_cpu（epaint 0.36 同款 skrifa/vello 栈），字形渲染观感与
//! UI 文字一致；skrifa 是本 crate 已有依赖，vello_cpu 本就在 epaint 的依赖树里。

use eframe::egui;
use eframe::egui::TextureHandle;
use skrifa::MetadataProvider as _;
use skrifa::outline::OutlinePen;
use skrifa::prelude::{LocationRef, Size};
use std::collections::{HashMap, VecDeque};
use vello_cpu::kurbo;

/// 阴影位图四周留白 = 3σ + 1px：高斯光可见范围约 ±3σ，留白不足会把晕开的光
/// 截成方形边缘。
const PAD_SIGMAS: f32 = 3.0;
/// 位图上限（像素数）：超过则放弃（防御异常超长文本，正常一行 ~800×60）。
const MAX_PIXELS: usize = 2_000_000;

/// 阴影外观参数。
#[derive(Clone, Copy, PartialEq)]
pub struct ShadowStyle {
    /// 高斯标准差 σ（**像素**，含 DPI 换算）；可见晕开范围约 ±3σ。
    pub sigma: f32,
    /// 阴影强度 [0,1]，乘在模糊后的 alpha 上。
    pub strength: f32,
}

/// 缓存键（不透明：只能经 [`ShadowKey::new`] 构造）。f32 参数乘 100 取整后参与
/// 哈希（f32 不宜直接做键）。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ShadowKey {
    text: String,
    px_centi: u32,
    sigma_centi: u32,
    strength_centi: u32,
}

impl ShadowKey {
    pub fn new(text: &str, px: f32, style: ShadowStyle) -> Self {
        Self {
            text: text.to_owned(),
            px_centi: (px * 100.0).round() as u32,
            sigma_centi: (style.sigma * 100.0).round() as u32,
            strength_centi: (style.strength * 100.0).round() as u32,
        }
    }
}

/// 阴影纹理缓存。`None` 缓存「光栅化失败」（纯空白/无字形），避免每帧重试。
/// `Clone` 是 egui IdTypeMap data 槽的要求（槽值需要 Clone + Send + Sync）。
#[derive(Clone, Default)]
pub struct ShadowCache {
    map: HashMap<ShadowKey, Option<egui::TextureHandle>>,
    /// 插入顺序，超出容量时淘汰最旧。
    order: VecDeque<ShadowKey>,
}

impl ShadowCache {
    /// 取（或生成）`text` 的柔影纹理。
    ///
    /// `font_px` 为像素字号（pt × pixels_per_point）；`style.sigma` 亦是像素单位。
    /// 返回 `None` = 该文本没有可渲染的字形。
    /// 查缓存（短临界区，闭包内只做 HashMap 查找）。命中返回纹理克隆；
    /// `Some(None)` 表示「已知光栅化失败」（纯空白/无字形），调用方直接放弃；
    /// `None` 表示未缓存，调用方应在**锁外**光栅化后调 [`Self::insert`]。
    pub fn get(&mut self, text: &str, font_px: f32, style: ShadowStyle) -> CachedShadow {
        let key = ShadowKey::new(text, font_px, style);
        match self.map.get(&key) {
            Some(Some(tex)) => CachedShadow::Ready(tex.clone()),
            Some(None) => CachedShadow::Failed,
            None => CachedShadow::Miss(key),
        }
    }

    /// 写缓存（短临界区，闭包内只做 HashMap 插入 + LRU 淘汰）。
    /// `texture` 为 `None` 时缓存「光栅化失败」，避免每帧重试。
    pub fn insert(&mut self, key: ShadowKey, texture: Option<egui::TextureHandle>) {
        if self.map.contains_key(&key) {
            return; // 同帧竞态下另一处已插入，保旧（LRU 不动）
        }
        self.map.insert(key.clone(), texture);
        self.order.push_back(key);
        while self.order.len() > 8 {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old); // 释放 TextureHandle，纹理随后被 egui 回收
            }
        }
    }
}

/// [`ShadowCache::get`] 的查询结果。
#[must_use]
pub enum CachedShadow {
    /// 缓存命中，可直接绘制。
    Ready(TextureHandle),
    /// 已知失败（无字形/字体不可解析），不要重试。
    Failed,
    /// 未缓存；携带预构造的键，锁外光栅化完成后连同结果调 [`ShadowCache::insert`]。
    Miss(ShadowKey),
}

/// 手动实现：egui 的 `TextureHandle` 没有实现 `Debug`，这里只打印判别名。
impl std::fmt::Debug for CachedShadow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(_) => f.write_str("Ready(TextureHandle)"),
            Self::Failed => f.write_str("Failed"),
            Self::Miss(key) => f.debug_tuple("Miss").field(key).finish(),
        }
    }
}

/// 光栅化 + 模糊 + 上传。字形全空（纯空白文本）或字体不可解析时返回 `None`。
///
/// 供浮窗侧在 `data_mut` 锁外调用（锁内不可再碰 `ctx.*`，见模块文档锁纪律）。
pub(crate) fn rasterize_shadow(
    ctx: &egui::Context,
    font_bytes: &[u8],
    font_index: u32,
    text: &str,
    font_px: f32,
    style: ShadowStyle,
) -> Option<egui::TextureHandle> {
    let bmp = shadow_bitmap(font_bytes, font_index, text, font_px, style)?;
    Some(ctx.load_texture(
        "lyrics_text_shadow",
        egui::ColorImage::from_rgba_premultiplied([bmp.width, bmp.height], &bmp.rgba),
        egui::TextureOptions::LINEAR,
    ))
}

/// 一张已光栅化 + 模糊的阴影位图（预乘黑：rgb=0，alpha=模糊覆盖度 × 强度）。
#[derive(Debug)]
struct ShadowBitmap {
    rgba: Vec<u8>,
    width: usize,
    height: usize,
}

/// 纯 CPU 部分：skrifa 轮廓 → vello_cpu 光栅化 → 盒滤波模糊。
fn shadow_bitmap(
    font_bytes: &[u8],
    font_index: u32,
    text: &str,
    font_px: f32,
    style: ShadowStyle,
) -> Option<ShadowBitmap> {
    let font = skrifa::FontRef::from_index(font_bytes, font_index).ok()?;
    let size = Size::new(font_px);
    let charmap = font.charmap();
    let metrics = font.glyph_metrics(size, LocationRef::default());
    let outlines = font.outline_glyphs();

    // ── 1. 逐字取轮廓 → kurbo 路径，x 按步进平移；收集整体控制点包围盒。
    // skrifa 轮廓是 y-up（基线 y=0），位图是 y-down：先 FLIP_Y 再平移到步进位。
    let mut bounds: Option<kurbo::Rect> = None;
    let mut paths: Vec<kurbo::BezPath> = Vec::new();
    let mut pen_x = 0.0_f64;
    for ch in text.chars() {
        if let Some(gid) = charmap.map(ch) {
            if let Some(glyph) = outlines.get(gid) {
                let mut path = kurbo::BezPath::new();
                glyph
                    .draw(
                        skrifa::outline::DrawSettings::unhinted(size, LocationRef::default()),
                        &mut FlipPen { path: &mut path, dx: pen_x },
                    )
                    .ok();
                if !path.is_empty() {
                    let b = path.control_box();
                    bounds = Some(match bounds {
                        Some(acc) => acc.union(b),
                        None => b,
                    });
                    paths.push(path);
                }
            }
            pen_x += metrics.advance_width(gid).unwrap_or(0.0) as f64;
        } else {
            // 字体缺字：跳过（阴影缺一个字形 vs 整体不画，前者伤害小得多）。
            // 不补 advance——egui 会用 replacement 字形渲染，此处宽度差异被模糊吞掉。
        }
    }
    let bounds = bounds?;
    if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
        return None;
    }

    // ── 2. 画布 = 包围盒 + pad；scale 至位图坐标。
    let pad = (PAD_SIGMAS * style.sigma) as f64 + 1.0;
    let width = ((bounds.width() + 2.0 * pad).ceil() as usize).clamp(1, 8192);
    let height = ((bounds.height() + 2.0 * pad).ceil() as usize).clamp(1, 8192);
    if width * height > MAX_PIXELS {
        return None;
    }

    // ── 3. vello_cpu 离屏渲染白色字形 → 取 alpha 通道。
    let (w, h) = (width as u16, height as u16);
    let mut rc = vello_cpu::RenderContext::new(w, h);
    rc.set_transform(kurbo::Affine::translate((
        pad - bounds.x0,
        pad - bounds.y0,
    )));
    rc.set_paint(vello_cpu::color::palette::css::WHITE);
    for path in &paths {
        rc.fill_path(path);
    }
    rc.flush();
    let mut pixmap = vello_cpu::Pixmap::new(w, h);
    rc.render(&mut pixmap, &mut vello_cpu::Resources::new());

    let mut alpha: Vec<f32> = pixmap.data().iter().map(|p| p.a as f32 / 255.0).collect();

    // ── 4. 高斯模糊（三次盒滤波近似，O(1)/像素）。
    gaussian_blur(&mut alpha, width, height, style.sigma);

    // ── 5. 打包纹理：预乘黑（rgb=0），alpha = 模糊覆盖度 × 强度。
    // 预乘黑阴影无论 egui 走直线过滤还是预乘合成，颜色都恒为黑，只有 alpha 生效。
    let strength = style.strength.clamp(0.0, 1.0);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for a in alpha {
        let a = (a * strength * 255.0).round().clamp(0.0, 255.0) as u8;
        rgba.extend_from_slice(&[0, 0, 0, a]);
    }
    Some(ShadowBitmap { rgba, width, height })
}

/// skrifa OutlinePen 适配：y 翻转（y-up → y-down）+ x 平移到字形步进位。
struct FlipPen<'a> {
    path: &'a mut kurbo::BezPath,
    dx: f64,
}

impl OutlinePen for FlipPen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.move_to((self.dx + x as f64, -(y as f64)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.path.line_to((self.dx + x as f64, -(y as f64)));
    }
    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.path.quad_to(
            (self.dx + cx0 as f64, -(cy0 as f64)),
            (self.dx + x as f64, -(y as f64)),
        );
    }
    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.path.curve_to(
            (self.dx + cx0 as f64, -(cy0 as f64)),
            (self.dx + cx1 as f64, -(cy1 as f64)),
            (self.dx + x as f64, -(y as f64)),
        );
    }
    fn close(&mut self) {
        self.path.close_path();
    }
}

/// 高斯模糊（盒滤波近似），`alpha` 原地更新。
///
/// 三次盒滤波等效 σ：σ_box² = 3×(r²-1)/12 → 盒半径 r = √(4σ²+1)/2。
fn gaussian_blur(alpha: &mut [f32], width: usize, height: usize, sigma: f32) {
    if sigma <= 0.01 || width == 0 || height == 0 || alpha.len() != width * height {
        return;
    }
    let ideal = (12.0 * sigma * sigma / 3.0 + 1.0).sqrt();
    let r = ((ideal / 2.0).floor() as usize).max(1);
    let mut tmp = vec![0.0_f32; alpha.len()];
    for _ in 0..3 {
        for y in 0..height {
            let row = &alpha[y * width..(y + 1) * width];
            let out = &mut tmp[y * width..(y + 1) * width];
            box_filter_1d(row, out, r);
        }
        // 纵向按列处理（缓存不友好但位图宽有限，且一次模糊只跑三轮）。
        for x in 0..width {
            let mut col = Vec::with_capacity(height);
            for y in 0..height {
                col.push(tmp[y * width + x]);
            }
            let mut out = vec![0.0_f32; height];
            box_filter_1d(&col, &mut out, r);
            for y in 0..height {
                alpha[y * width + x] = out[y];
            }
        }
    }
}

/// 一维盒滤波（滑动窗口和，截断窗口按实际数量归一），`dst` 与 `src` 等长。
///
/// 边界取截断语义（窗口越界部分不计入、分母用实际数量）：总能量守恒，
/// 阴影光晕在位图边缘平滑趋零，不会出现 clamp 式的边缘增亮。
fn box_filter_1d(src: &[f32], dst: &mut [f32], r: usize) {
    let n = src.len();
    if n == 0 {
        return;
    }
    let r = r.min(n.saturating_sub(1));
    // 初始窗口 i=0：j∈[0, min(r, n-1)]。
    let hi0 = r.min(n - 1);
    let mut acc: f32 = src[..=hi0].iter().sum();
    let mut count = hi0 + 1;
    for i in 0..n {
        dst[i] = acc / count as f32;
        // 迭代到 i+1：左侧移出 j=i-r（若有效），右侧移入 j=i+r+1（若有效）。
        if i >= r {
            acc -= src[i - r];
            count -= 1;
        }
        let in_idx = i + r + 1;
        if in_idx < n {
            acc += src[in_idx];
            count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_filter_on_constant_input_is_constant() {
        let src = vec![1.0_f32; 16];
        let mut dst = vec![0.0_f32; 16];
        box_filter_1d(&src, &mut dst, 2);
        for v in &dst {
            assert!((*v - 1.0).abs() < 1e-3, "全 1 输入的盒滤波应仍为 1: {v}");
        }
    }

    #[test]
    fn box_filter_spreads_impulse_and_conserves_mass() {
        // 冲激输入：能量摊到 2r+1 宽度，总和守恒（远端边界外）。
        let mut src = vec![0.0_f32; 64];
        src[32] = 63.0;
        let mut dst = vec![0.0_f32; 64];
        box_filter_1d(&src, &mut dst, 3);
        let total: f32 = dst[32 - 3..=32 + 3].iter().sum();
        assert!((total - 63.0).abs() < 1e-3, "盒滤波总能量守恒: {total}");
        assert!((dst[32] - 63.0 / 7.0).abs() < 1e-3, "峰值应摊薄 1/7: {}", dst[32]);
    }

    #[test]
    fn gaussian_blur_zero_sigma_is_noop() {
        let mut a = vec![0.5_f32; 100];
        let before = a.clone();
        gaussian_blur(&mut a, 10, 10, 0.0);
        assert_eq!(a, before, "σ=0 不应改变输入");
    }

    #[test]
    fn gaussian_blur_spreads_and_preserves_energy() {
        let mut a = vec![0.0_f32; 64 * 64];
        a[32 * 64 + 32] = 1.0;
        gaussian_blur(&mut a, 64, 64, 3.0);
        let total: f32 = a.iter().sum();
        assert!((total - 1.0).abs() < 0.05, "模糊后总能量近似守恒: {total}");
        assert!(a[32 * 64 + 32] < 0.2, "冲激峰值应显著摊薄: {}", a[32 * 64 + 32]);
    }

    /// 内嵌 Noto Sans SC 端到端：产出非空预乘黑位图，中心 alpha 高于角落（晕开），
    /// 位置贴图由调用方完成——这里只验证位图本身。
    #[test]
    fn box_filter_matches_bruteforce_reference() {
        let src: Vec<f32> = (0..24).map(|i| (i as f32 * 0.37).sin().abs() * 0.8 + 0.1).collect();
        let r = 3;
        let mut fast = vec![0.0_f32; src.len()];
        box_filter_1d(&src, &mut fast, r);
        for (i, v) in fast.iter().enumerate() {
            // 暴力参考实现：窗口 [i-r, i+r] 截断到有效下标，按实际数量归一。
            let lo = i.saturating_sub(r);
            let hi = (i + r).min(src.len() - 1);
            let acc: f32 = src[lo..=hi].iter().sum();
            let want = acc / (hi - lo + 1) as f32;
            assert!(
                (*v - want).abs() < 1e-4,
                "滑动窗口与暴力实现不一致 at {i}: {} vs {want}",
                *v
            );
        }
    }

    /// 内嵌 Noto Sans SC 端到端：光栅化 + 模糊产出非空预乘黑位图，且「晕开」——
    /// 笔画处 alpha 显著高于空白角落，角落仍有非零光晕。
    #[test]
    fn shadow_bitmap_end_to_end() {
        let style = ShadowStyle { sigma: 5.0, strength: 0.6 };
        let Some(bmp) = shadow_bitmap(
            crate::fonts::NOTO_SC_BYTES_FOR_TEST,
            0,
            "测试 Lyrics",
            26.0,
            style,
        ) else {
            panic!("内嵌 CJK 字体应能光栅化出阴影位图");
        };
        assert!(bmp.width > 10 && bmp.height > 10, "位图尺寸异常: {bmp:?}");
        let alpha_at = |x: usize, y: usize| bmp.rgba[(y * bmp.width + x) * 4 + 3];
        // 位图中心附近（字形墨迹区）与非角落区域。
        let cx = bmp.width / 2;
        let cy = bmp.height / 2;
        let center_a = alpha_at(cx, cy);
        let corner_a = alpha_at(0, 0);
        assert!(center_a > 0, "中心必须有可见阴影");
        assert!(
            center_a > corner_a + 8,
            "中心 alpha({center_a}) 应明显高于角落({corner_a})——晕开效果"
        );
        // 预乘黑：rgb 恒 0。
        assert!(bmp.rgba.chunks_exact(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0));
    }

    /// 空白文本：无字形 → None（不生成全黑矩形纹理）。
    #[test]
    fn shadow_bitmap_blank_text_is_none() {
        let style = ShadowStyle { sigma: 5.0, strength: 0.6 };
        assert!(shadow_bitmap(crate::fonts::NOTO_SC_BYTES_FOR_TEST, 0, "  ", 26.0, style).is_none());
        assert!(shadow_bitmap(crate::fonts::NOTO_SC_BYTES_FOR_TEST, 0, "", 26.0, style).is_none());
    }

    /// 垃圾字体字节：安全返回 None（不 panic）。
    #[test]
    fn shadow_bitmap_bad_font_is_none() {
        let style = ShadowStyle { sigma: 5.0, strength: 0.6 };
        assert!(shadow_bitmap(b"not a font", 0, "测试", 26.0, style).is_none());
    }

    /// 缓存语义：Miss → 光栅化 → insert 后变 Ready/Failed；失败也要缓存；
    /// 同键重复 insert 不覆盖；容量 8 FIFO 淘汰。纹理用 None（缓存失败）+ 一张
    /// 真实纹理（无头 Context 上传 2x2）验证两条路径。
    #[test]
    fn shadow_cache_get_insert_semantics() {
        let style = ShadowStyle { sigma: 5.0, strength: 0.6 };
        let mut cache = ShadowCache::default();

        // 未缓存 → Miss（携带键）。
        let key = match cache.get("测试", 26.0, style) {
            CachedShadow::Miss(k) => k,
            other => panic!("首次查询应为 Miss，得到 {other:?}"),
        };

        // 失败也缓存：insert(None) → Failed（避免每帧重试光栅化）。
        cache.insert(key.clone(), None);
        assert!(matches!(cache.get("测试", 26.0, style), CachedShadow::Failed));

        // 同键重复 insert 不覆盖（保旧）。
        cache.insert(key, None);
        assert!(matches!(cache.get("测试", 26.0, style), CachedShadow::Failed));

        // 命中路径：无头 Context 上传一张 2x2 纹理。
        let ctx = egui::Context::default();
        let tex = ctx.load_texture(
            "shadow-cache-test",
            egui::ColorImage::from_rgba_premultiplied([2, 2], &[0, 0, 0, 128, 0, 0, 0, 128, 0, 0, 0, 128, 0, 0, 0, 128]),
            egui::TextureOptions::LINEAR,
        );
        cache.insert(ShadowKey::new("命中", 26.0, style), Some(tex.clone()));
        match cache.get("命中", 26.0, style) {
            CachedShadow::Ready(t) => assert_eq!(t.id(), tex.id()),
            other => panic!("insert 后应 Ready，得到 {other:?}"),
        }

        // 容量 8 FIFO：再灌 8 个新键（共 9+），最早插入的「测试」应被淘汰。
        for i in 0..8 {
            let k = ShadowKey::new(&format!("fill{i}"), 26.0, style);
            cache.insert(k, None);
        }
        assert!(matches!(cache.get("测试", 26.0, style), CachedShadow::Miss(_)));
    }
}
