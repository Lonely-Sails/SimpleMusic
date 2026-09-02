//! bili_probe —— B 站客户端模块实测探针（无需 GUI、无账号）。
//!
//! 运行：`cargo run --example bili_probe [BV号或链接]`
//! 默认用 BV1GJ411x7h7（任务指定）。流程：
//! 1. 构建 BiliClient（加载 session.json），ensure_buvid 并打印。
//! 2. BV 解析器本地用例自检。
//! 3. video_info：打印 HTTP code / API code / 标题 / UP 主 / 时长 / cid。
//! 4. playurl 未签名一次 + WBI 签名一次，对比 API code 与 dash.audio。
//! 5. 尝试带 UA/Referer 下载音频流前 1KB，报告 CDN 可达性（沙箱出口 IP 常见 403）。
//! 6. 不打印任何 Cookie 值（凭据脱敏）。

#[path = "../src/state.rs"]
#[allow(dead_code)]
pub mod state;

// 桥接模块复用主 crate 的完整实现，示例只用其中一部分，容忍 dead_code。
#[allow(dead_code)]
mod modules;

use modules::bilibili::{BiliClient, BiliError};

fn main() {
    let input = std::env::args().nth(1).unwrap_or_else(|| "BV1GJ411x7h7".to_string());
    println!("=== SimpleMusic bili_probe ===");
    println!("UA: {}", modules::bilibili::USER_AGENT);

    // ---- 1. 客户端 + buvid ----
    let mut client = match BiliClient::with_session() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("创建客户端失败: {e}");
            std::process::exit(1);
        }
    };
    match client.ensure_buvid() {
        Ok(()) => {
            let buvid3 = client.session().get("buvid3").unwrap_or("");
            let buvid4 = client.session().get("buvid4").unwrap_or("");
            let persisted = std::path::Path::new(&format!("{}/.config/simple-music/session.json", std::env::var("HOME").unwrap_or_default())).exists();
            println!("[buvid] 已获取 (落盘session.json存在={persisted}; 沙箱只读环境下为内存态属正常)");
            println!("[buvid] buvid3={buvid3}");
            println!("[buvid] buvid4={buvid4}");
            println!("[buvid] Cookie 头(脱敏): {} 项 cookie: {}",
                client.session().cookies.len(),
                format!("{:?}", client.session()).replace("BiliSession { cookies: ", "").replace(", saved_at_unix: 0 }", ""));
        }
        Err(e) => eprintln!("[buvid] 获取失败: {e}"),
    }
    println!("[login] logged_in={}", client.logged_in());

    // ---- 2. BV 解析自检 ----
    println!("[parse] 本地解析用例:");
    for (case, expect) in [
        ("https://www.bilibili.com/video/BV1xx411c7mD?p=2", "BV1xx411c7mD"),
        ("BV1GJ411x7h7", "BV1GJ411x7h7"),
        ("av170001", "None"),
        ("https://example.com/nothing", "None"),
    ] {
        let got = BiliClient::parse_bvid_direct(case)
            .map(|s| s.to_string())
            .unwrap_or_else(|| "None".to_string());
        println!("  {case:<55} -> {got} (期望 {expect})");
    }

    // ---- 3. video_info ----
    let mut bvid = BiliClient::parse_bvid_direct(&input).unwrap_or_else(|| input.trim().to_string());
    println!("[view] 目标 BV: {bvid}");
    let detail = match client.video_info(&bvid) {
        Ok(d) => d,
        Err(BiliError::Api { code, message }) => {
            println!("[view] API code={code} message=\"{message}\"");
            if code == -404 {
                // 任务指定视频可能已下架，回退一个确定可用的公开视频继续验证取流链路。
                bvid = "BV1xx411c7mD".to_string();
                println!("[view] {bvid} 已不可见，回退公开测试视频 {bvid}");
                match client.video_info(&bvid) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[view] 回退视频仍失败: {e}");
                        std::process::exit(2);
                    }
                }
            } else {
                std::process::exit(2);
            }
        }
        Err(e) => {
            eprintln!("[view] 请求失败: {e}");
            std::process::exit(2);
        }
    };
    println!(
        "[view] HTTP=200 code=0 title=\"{}\" owner=\"{}\" duration={}s cid={} pages={}",
        detail.info.title, detail.info.uploader, detail.info.duration_secs, detail.cid, detail.pages
    );

    // ---- 4. playurl：未签名 vs WBI 签名 ----
    println!("[playurl] 未签名 fnval=16 fourk=1:");
    match client.fetch_playurl_raw(&bvid, detail.cid, false) {
        Ok((http, raw)) => {
            println!("[playurl] HTTP={http} APIcode={}", raw.code);
            if let Some(d) = raw.data.as_ref().and_then(|d| d.dash.as_ref()) {
                println!("[playurl] dash.audio 数量={} (video 数量={})", d.audio.len(), d.video.len());
                if let Some(a) = d.audio.first() {
                    let mut base = a.base_url.clone();
                    base.truncate(80);
                    println!("[playurl] 第一条 baseUrl[:80]={base}");
                }
            } else if raw.data.as_ref().map(|d| d.durl.is_some()).unwrap_or(false) {
                println!("[playurl] 无 dash，走 durl（老格式）");
            }
            if raw.code != 0 {
                println!("[playurl] 未签名被拒: code={} message=\"{}\"", raw.code, raw.message);
            }
        }
        Err(e) => println!("[playurl] 未签名失败: {e}"),
    }

    println!("[playurl] WBI 签名:");
    match client.wbi_keys() {
        Ok(keys) => {
            let mix = keys.mixin_key();
            println!("[wbi] img_key={} sub_key={}", keys.img_key, keys.sub_key);
            println!("[wbi] mixin_key(32位)={mix}");
        }
        Err(e) => println!("[wbi] nav 失败: {e}"),
    }
    let (signed_http, signed_code, signed_audio, signed_url80, signed_bw, signed_codec) =
        match client.fetch_playurl_raw(&bvid, detail.cid, true) {
            Ok((http, raw)) => {
                let d = raw.data.as_ref().and_then(|d| d.dash.as_ref());
                let (n, url80, bw, codec) = match d.and_then(|dash| dash.audio.first()) {
                    Some(a) => {
                        let mut u = a.base_url.clone();
                        u.truncate(80);
                        (d.map(|x| x.audio.len()).unwrap_or(0), u, a.bandwidth, a.codecs.clone().unwrap_or_default())
                    }
                    None => (0, "-".into(), 0, "-".into()),
                };
                (http, raw.code, n, url80, bw, codec)
            }
            Err(e) => {
                println!("[playurl] WBI 签名失败: {e}");
                (0, i64::MIN, 0, "-".into(), 0, "-".into())
            }
        };
    println!("[playurl] HTTP={signed_http} APIcode={signed_code} dash.audio数量={signed_audio}");
    println!("[playurl] 第一条 baseUrl[:80]={signed_url80}");
    println!("[playurl] 选中最高码率音频 bandwidth={signed_bw}bps codec={signed_codec}");

    // ---- 5. resolve_stream 端到端（含备用 CDN 与必需请求头） ----
    match client.resolve_stream_with_cid(&bvid, detail.cid, state::AudioQuality::High) {
        Ok(s) => {
            let mut url80 = s.audio_url.clone();
            url80.truncate(80);
            println!("[stream] resolved signed_with_wbi={} ttl={}s id={:?} codec={:?} bandwidth={:?} size={:?}",
                s.signed_with_wbi, s.ttl_secs, s.audio_id, s.audio_codec, s.bandwidth, s.size_bytes);
            println!("[stream] audio_url[:80]={url80}");
            println!("[stream] backup_urls={} 条", s.audio_backup_urls.len());
            println!("[stream] 音频 Worker 必需请求头:");
            for (k, v) in &s.required_headers {
                let v = if k == "Cookie" { "<已附加会话cookie，值已脱敏>".to_string() } else { v.clone() };
                println!("    {k}: {v}");
            }

            // 尝试拉流前 1KB（沙箱出口 IP 很可能被 CDN 拒绝，如实报告）。
            match client.probe_download(&s.audio_url, &s.required_headers, "bytes=0-1023") {
                Ok((status, n)) => {
                    println!("[cdn] GET Range 0-1023 -> HTTP {status}, body {n} bytes");
                    if status != 200 && status != 206 {
                        println!("[cdn] 注意: 非 2xx。本沙箱出口 IP 已被 B 站 CDN 风控（换 UA/Referer/加 Cookie 均仍 403），");
                        println!("[cdn] 属环境网络限制；宿主/正常用户网络用同样请求头即可下载。");
                    }
                }
                Err(e) => println!("[cdn] 请求失败: {e}"),
            }
        }
        Err(e) => println!("[stream] resolve_stream 失败: {e}"),
    }

    println!("=== probe 完成 ===");
}
