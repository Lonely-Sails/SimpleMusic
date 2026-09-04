//! BV 解析方法组（`impl BiliClient`）：链接解析、video_info、playurl
//! 音频流提取（DASH 优先，durl 兜底）、WBI 签名与 B 站「识别音乐」。

use super::client::BiliClient;
use super::error::{BiliError, BiliResult};
use super::models::*;
use super::util::{pick_dash_audio, scan_bv_token};
use super::wbi::{wbi_sign_params, WbiKeys};
use std::time::{Duration, Instant};

use crate::state::AudioQuality;

impl BiliClient {
    /// 从任意输入解析 BV 号：支持完整/移动端 URL（含 ?p= 分 P）、纯 BV 号、
    /// b23.tv 短链（需网络跟随重定向）。
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

    /// 识别视频的背景/插播音乐（B 站官方「识别音乐」数据），用于提升歌词搜索准确率。
    ///
    /// 探测顺序（任一成功即停止；`cid` 已知时免一次 view 请求）：
    /// 1. `/x/player/v2?bvid=..&cid=..` → `data.bgm_info.music_id`；
    /// 2. `/x/web-interface/view/detail/tag?bvid=..`（cid 缺省仅整个稿件）→
    ///    `tag_type == "bgm"` 条目的 `music_id`；
    /// 3. 拿到 `MA…` music_id 后调 `api.bilibili.com/x/copyright-music-publicity/bgm/detail`
    ///    换官方曲名/歌手/专辑。
    ///
    /// 全链路失败（无音乐卡、接口挂了、被风控）返回 `None`，调用方照旧走标题搜索——
    /// 识别只是增强，绝不阻塞歌词获取。
    pub fn detect_music(&self, bvid: &str, cid: i64) -> Option<MusicHint> {
        let music_id = self
            .music_id_from_player(bvid, cid)
            .or_else(|| self.music_id_from_bgm_tag(bvid))?;
        let hint = self.music_detail(&music_id).ok()?;
        if hint.is_usable() {
            Some(hint)
        } else {
            None
        }
    }

    /// 从 `/x/player/v2` 拿 `bgm_info.music_id`（UP 主挂载的 BGM 音乐卡）。
    fn music_id_from_player(&self, bvid: &str, cid: i64) -> Option<String> {
        if cid <= 0 {
            return None;
        }
        let url = format!("https://api.bilibili.com/x/player/v2?bvid={bvid}&cid={cid}");
        let env: ApiEnvelope<PlayerInfoResp> = self.get_json(&url, &[]).ok()?.1;
        let data = env.data?;
        let bgm = data.bgm_info?;
        Self::valid_music_id(&bgm.music_id)
    }

    /// 从 `view/detail/tag` 拿 `tag_type == "bgm"` 的 TAG music_id。
    fn music_id_from_bgm_tag(&self, bvid: &str) -> Option<String> {
        let url = format!(
            "https://api.bilibili.com/x/web-interface/view/detail/tag?bvid={bvid}"
        );
        let env: ApiEnvelope<Vec<BgmTagItem>> = self.get_json(&url, &[]).ok()?.1;
        let data = env.data?;
        data.into_iter()
            .filter(|t| t.tag_type == "bgm")
            .find_map(|t| Self::valid_music_id(&t.music_id))
    }

    /// music_id 校验：非空且以 `MA` 开头（曲库 id），否则视为无效。
    pub(super) fn valid_music_id(id: &str) -> Option<String> {
        let id = id.trim();
        if id.len() >= 3 && id.starts_with("MA") {
            Some(id.to_string())
        } else {
            None
        }
    }

    /// 曲库 `MA…` id → 官方曲名/歌手/专辑（音乐开放平台接口，无需登录）。
    fn music_detail(&self, music_id: &str) -> BiliResult<MusicHint> {
        let url = format!(
            "https://api.bilibili.com/x/copyright-music-publicity/bgm/detail?music_id={music_id}"
        );
        let env: ApiEnvelope<CopyrightMusicDetail> = self.get_json(&url, &[])?.1;
        let data = env.data.ok_or_else(|| {
            BiliError::Local(format!("bgm/detail {music_id} 缺少 data"))
        })?;
        Ok(MusicHint {
            title: data.music_title.trim().to_string(),
            artist: data.artist_display(),
            album: data.album.trim().to_string(),
            music_id: music_id.to_string(),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- 识别音乐（detect_music 的解析/校验部分；网络探测用 #[ignore]）----

    #[test]
    fn valid_music_id_accepts_ma_prefix_only() {
        assert_eq!(
            BiliClient::valid_music_id("MA436038343856245020").as_deref(),
            Some("MA436038343856245020")
        );
        // 旧音频区 au id / 空 / 短串 / 小写 / 非曲库 id 一律拒绝（曲库 id 实际为大写 MA 前缀）。
        assert_eq!(BiliClient::valid_music_id("au123456"), None);
        assert_eq!(BiliClient::valid_music_id(""), None);
        assert_eq!(BiliClient::valid_music_id("MA"), None);
        assert_eq!(BiliClient::valid_music_id("ma123"), None);
        assert_eq!(
            BiliClient::valid_music_id(" MA123 ").as_deref(),
            Some("MA123")
        );
    }

    /// `cargo test -- --ignored detect_music_live`
    #[ignore = "真实网络请求，仅人工运行"]
    #[test]
    fn detect_music_live() {
        let client = BiliClient::new().expect("client");
        // BV1M741177Kg（aid=89772773）：带官方 BGM 卡（player/v2 bgm_info 实测有值），
        // 识别 → 曲库详情应得 Other Side — MIYAVI。
        let hint = client
            .detect_music("BV1M741177Kg", 153322313)
            .expect("应识别到音乐");
        println!("hint = {hint:?}");
        assert_eq!(hint.title.to_lowercase(), "other side");
        assert!(hint.artist.to_lowercase().contains("miyavi"));
    }

    // ---- 音质选择 ----

    fn dash(id: i64, bandwidth: i64) -> DashStream {
        DashStream {
            id,
            base_url: format!("https://x/{id}"),
            backup_url: Vec::new(),
            bandwidth,
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
}
