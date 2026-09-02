//! audio_probe —— 音频引擎集成实测探针（无需 GUI、无账号）。
//!
//! 运行：`cargo run --example audio_probe [BV号]`（默认 BV1xx411c7mD）。
//! 流程：
//! 1. BiliClient.resolve_stream 解析音频直链（含备用 CDN 与必需请求头）。
//! 2. reqwest blocking + Range 下载前 2MB 到临时文件。
//! 3. symphonia 探测：codec / 采样率 / 声道 / 时长估计；并解码第一个包验证解码器。
//! 4. AudioEngine.play_file 端到端播放该片段，打印引擎状态（沙箱无声卡时展示
//!    「无法打开音频输出设备」错误路径）。
//! 5. 不打印任何 Cookie 值（凭据脱敏）。

#[path = "../src/state.rs"]
#[allow(dead_code)]
pub mod state;

// 桥接模块复用主 crate 的完整实现，示例只用其中一部分，容忍 dead_code。
#[allow(dead_code)]
mod modules;

use std::io::Read;
use std::time::{Duration, Instant};

use modules::audio::AudioEngine;
use modules::bilibili::{BiliClient, BiliError};


use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

fn main() {
    let input = std::env::args().nth(1).unwrap_or_else(|| "BV1xx411c7mD".to_string());
    println!("=== SimpleMusic audio_probe ===");

    // ---- 1. 解析音频直链 ----
    let mut client = match BiliClient::with_session() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("创建客户端失败: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = client.ensure_buvid() {
        eprintln!("[buvid] 获取失败: {e}");
    }
    println!("[login] logged_in={}", client.logged_in());

    let bvid = BiliClient::parse_bvid_direct(&input).unwrap_or_else(|| input.trim().to_string());
    println!("[stream] 目标 BV: {bvid}");
    let stream = match client.resolve_stream(&bvid, state::AudioQuality::High) {
        Ok(s) => s,
        Err(BiliError::Api { code, message }) if code == -404 => {
            println!("[stream] API code=-404 message=\"{message}\"，回退公开测试视频 BV1GJ411x7h7");
            let fallback = "BV1GJ411x7h7";
            match client.resolve_stream(fallback, state::AudioQuality::High) {
                Ok(s) => s,
                Err(e) => {
                    println!("[stream] resolve_stream 失败: {e}");
                    println!("=== probe 完成（解析失败）===");
                    return;
                }
            }
        }
        Err(e) => {
            println!("[stream] resolve_stream 失败: {e}");
            println!("=== probe 完成（解析失败）===");
            return;
        }
    };
    println!(
        "[stream] resolved codec={:?} bandwidth={:?}bps size={:?} ttl={}s backup={}条 signed_wbi={}",
        stream.audio_codec,
        stream.bandwidth,
        stream.size_bytes,
        stream.ttl_secs,
        stream.audio_backup_urls.len(),
        stream.signed_with_wbi
    );

    // ---- 2. 下载前 2MB ----
    let partial = std::env::temp_dir().join("simple-music-audio-probe-partial.m4s");
    match download_partial(&stream.audio_url, &stream.required_headers, 2 * 1024 * 1024, &partial) {
        Ok(n) => println!("[download] Range 0-{} -> {} bytes -> {}", 2 * 1024 * 1024 - 1, n, partial.display()),
        Err(e) => {
            println!("[download] 失败: {e}");
            println!("=== probe 完成（下载失败）===");
            return;
        }
    }

    // 文件头 hexdump（识别 B 站 m4s 魔数，确认是标准 fMP4）。
    let mut head = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open(&partial) {
        let _ = f.read_exact(&mut head);
    }
    println!(
        "[download] 文件头 32 字节: {}",
        head.iter().map(|b| format!("{b:02X}")).collect::<String>()
    );
    let ascii: String = head
        .iter()
        .map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' })
        .collect();
    println!("[download] ASCII 视图: {ascii}");

    // ---- 3. symphonia 探测 + 首包解码 ----
    // B 站 m4s 的 moov box 在文件末尾（非 faststart），2MB 头部片段无法探测；
    // 完整下载（34MB 级）后探测。沙箱里 ~/.cache 不可写（引擎会内存降级），
    // 故这里直接落到 TMPDIR 供 symphonia 打开。
    let full = std::env::temp_dir().join("simple-music-audio-probe-full.m4s");
    if std::fs::metadata(&full).map(|m| m.len() > 1024 * 1024).unwrap_or(false) {
        println!("[download] 复用已存在的完整下载: {}", full.display());
    } else {
        match download_partial(&stream.audio_url, &stream.required_headers, usize::MAX, &full) {
            Ok(n) => println!("[download] 完整下载 -> {n} bytes -> {}", full.display()),
            Err(e) => {
                println!("[download] 完整下载失败: {e}");
                println!("=== probe 完成（下载失败）===");
                return;
            }
        }
    }
    match probe_media(&full, "完整文件") {
        Ok(info) => println!("{info}"),
        Err(e) => {
            println!("[probe] 失败: {e}");
            println!("=== probe 完成（探测失败）===");
            return;
        }
    }
    // fMP4 无总帧数（n_frames=0），用 size/bandwidth 估算时长（与引擎回退策略一致）。
    if let (Some(size), Some(bw)) = (std::fs::metadata(&full).ok().map(|m| m.len()), stream.bandwidth) {
        if bw > 0 {
            println!(
                "[probe] 按 size/bandwidth 估算时长: {:.1}s（≈{}分{}秒）",
                size as f64 * 8.0 / bw as f64,
                (size as f64 * 8.0 / bw as f64) as u64 / 60,
                (size as f64 * 8.0 / bw as f64) as u64 % 60
            );
        }
    }

    // ---- 4. 引擎端到端（沙箱无声卡 → 预期进入设备错误状态） ----
    println!("[engine] AudioEngine::play_file 真实 B 站音频（沙箱预期设备错误）:");
    let engine = AudioEngine::new();
    engine.play_file(&full);
    // debug 版 AAC 全量解码 34 分钟音频较慢，放宽到 180s。
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let st = engine.status();
        if !st.loading && (st.error.is_some() || st.playing || st.finished) {
            println!(
                "[engine] status: loading={} playing={} finished={} error={:?} duration={:.2}s position={:.2}s cache_hit={}",
                st.loading, st.playing, st.finished, st.error, st.duration_secs, st.position_secs, st.cache_hit
            );
            break;
        }
        if Instant::now() > deadline {
            println!("[engine] 15s 内未达稳定状态");
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("=== probe 完成 ===");
}

/// 用 StreamUrl 的必需请求头做（可限长的）下载，返回实际写入字节数。
/// `max_bytes` 为 usize::MAX 表示完整下载（不带 Range 头）。
fn download_partial(
    url: &str,
    headers: &[(String, String)],
    max_bytes: usize,
    out: &std::path::Path,
) -> Result<usize, String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| format!("HTTP 客户端构建失败: {e}"))?;
    let mut req = client.get(url);
    if max_bytes != usize::MAX {
        req = req.header(reqwest::header::RANGE, format!("bytes=0-{}", max_bytes - 1));
    }
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let mut resp = req.send().map_err(|e| format!("请求失败: {e}"))?;
    let status = resp.status().as_u16();
    if !resp.status().is_success() {
        return Err(format!("HTTP {status}（CDN 拒绝；备用地址数见上方 resolve 输出）"));
    }
    println!("[download] HTTP {status} content-length={:?} content-range={:?}",
        resp.headers().get(reqwest::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()),
        resp.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok()));
    let mut file = std::fs::File::create(out).map_err(|e| format!("创建临时文件失败: {e}"))?;
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    loop {
        let n = resp.read(&mut buf).map_err(|e| format!("读取流失败: {e}"))?;
        if n == 0 {
            break;
        }
        total += n;
        std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| format!("写盘失败: {e}"))?;
        if total >= max_bytes {
            break;
        }
    }
    Ok(total)
}

/// symphonia 探测媒体信息 + 解码第一个包，返回人类可读报告。
fn probe_media(path: &std::path::Path, tag: &str) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("打开文件失败: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    hint.with_extension("mp4"); // m4s 本质是 fMP4
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("格式探测失败: {e}"))?;
    let mut format = probed.format;
    // 先把需要的轨道参数拷贝出来，避免 track 借用与 next_packet 的可变借用冲突。
    let decoder: Option<Box<dyn symphonia::core::codecs::Decoder>>;
    let track_id;
    let codec_name;
    let sample_rate;
    let channels;
    let time_base;
    let n_frames;
    let max_fpp;
    let duration_est;
    {
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or("没有可解码的音频轨道")?;
        let cp = &track.codec_params;
        track_id = track.id;
        sample_rate = cp.sample_rate;
        channels = cp.channels.map(|c| c.count());
        time_base = cp.time_base;
        n_frames = cp.n_frames;
        max_fpp = cp.max_frames_per_packet;
        duration_est = cp
            .n_frames
            .zip(cp.time_base)
            .map(|(n, tb)| {
                let t = tb.calc_time(n);
                format!("{:.3}s", t.seconds as f64 + t.frac)
            })
            .unwrap_or_else(|| "未知".to_string());
        codec_name = if cp.codec == symphonia::core::codecs::CODEC_TYPE_AAC {
            "AAC-LC (mp4a.40.2)".to_string()
        } else if cp.codec == symphonia::core::codecs::CODEC_TYPE_MP3 {
            "MP3".to_string()
        } else {
            format!("{}", cp.codec)
        };
        // 创建解码器并解一个包，验证解码链路真实可用。
        decoder = Some(
            symphonia::default::get_codecs()
                .make(cp, &DecoderOptions::default())
                .map_err(|e| format!("创建解码器失败: {e}"))?,
        );
    }
    let mut decoder = decoder.unwrap();

    let mut report = format!(
        "[probe:{tag}] track_id={track_id} codec={codec_name} sample_rate={sample_rate:?} channels={channels:?} time_base={:?} n_frames={n_frames:?} 时长估计={duration_est} max_frames_per_packet={max_fpp:?}",
        time_base.map(|tb| format!("{}/{}", tb.numer, tb.denom)),
    );

    let mut packets = 0u32;
    let mut first_frames = 0u64;
    loop {
        match format.next_packet() {
            Ok(packet) => {
                if packet.track_id() != track_id {
                    continue;
                }
                match decoder.decode(&packet) {
                    Ok(decoded) => {
                        packets += 1;
                        if packets == 1 {
                            first_frames = decoded.frames() as u64;
                        }
                        if packets >= 16 {
                            break; // 验证 16 个包即止（文件只有 2MB 片段）
                        }
                    }
                    Err(symphonia::core::errors::Error::DecodeError(e)) => {
                        report.push_str(&format!("\n[probe] 跳过坏包: {e}"));
                        continue;
                    }
                    Err(e) => {
                        report.push_str(&format!("\n[probe] 解码中止: {e}（2MB 片段在中途截断属正常）"));
                        break;
                    }
                }
            }
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                report.push_str("\n[probe] 到达片段末尾（2MB 截断，属正常）");
                break;
            }
            Err(e) => {
                report.push_str(&format!("\n[probe] demux 中止: {e}"));
                break;
            }
        }
    }
    report.push_str(&format!(
        "\n[probe] 实际解码验证: 成功解码 {packets} 个包，首包帧数={first_frames}"
    ));
    Ok(report)
}
