//! 音频下载与缓存复用：流式下载到 `.part` 临时文件后原子重命名，
//! 备用 CDN 地址轮替，写盘失败降级内存缓冲，下载中可被打断。
//!
//! 下载走全局 tokio runtime（[`crate::net`]）；调用方（播放线程）用
//! [`crate::net::block_on`] 同步等待。写盘（阻塞 IO）包在 `spawn_blocking` 里，
//! 不占住 runtime 的 worker。

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::Receiver;

use super::cache::{cache_path_in, cache_usable};
use super::control::Command;
use super::control::PlayRequest;
use super::control::PlaybackStatus;
use super::decode::MediaInput;
use super::player::set_status;

/// 下载读缓冲大小（已由 `Response::chunk()` 内部缓冲代替，仅保留常量说明）。
#[allow(dead_code)]
const DOWNLOAD_BUF_SIZE: usize = 64 * 1024;

/// fetch_to_cache 的失败类型。
pub(super) enum FetchErr {
    /// 下载被新命令打断（Stop / 新的 Play），worker 应直接处理下一条命令。
    Aborted,
    Failed(String),
}

/// 非阻塞检查是否有抢占性命令（Stop / 新 Play / 退出）。
///
/// 下载与响度分析共用：期间到达的 Pause/Resume/Seek/Volume 会被丢弃
/// （这些命令对「尚未开始出声」的加载阶段没有意义），抢占性命令则立即返回 true。
pub(super) fn poll_abort(rx: &Receiver<Command>) -> bool {
    loop {
        match rx.try_recv() {
            Ok(Command::Stop) | Ok(Command::Play(_)) | Ok(Command::Shutdown) => return true,
            Ok(_) => continue, // 下载期间的 Pause/Resume/Seek/Volume 忽略
            Err(_) => return false,
        }
    }
}

/// 下载/复用缓存，返回可供 symphonia 打开的媒体数据。
///
/// - 缓存命中（存在且大小匹配）→ 直接返回文件路径；
/// - 否则流式下载（64KB 缓冲）到 `<key>.m4s.part`，完成后原子重命名；
/// - 403/410/404/5xx 或网络错误 → 依次尝试备用 CDN 地址；
/// - 目录创建/写盘失败 → 降级为内存缓冲（不崩）；
/// - 下载期间 `abort()` 返回 true（Stop/新 Play/退出）→ `FetchErr::Aborted`。
///
/// 异步函数；播放线程用 [`crate::net::block_on`] 驱动。
///
/// `abort` 只要求 `Fn() -> bool`（不要求 Send/Sync）：该 future 在播放线程上用
/// `block_on` 驱动，不跨线程调度，因此可以安全地借用非 Sync 的命令通道。
pub(super) async fn fetch_to_cache(
    req: &PlayRequest,
    status: &Mutex<PlaybackStatus>,
    cache_dir: &Path,
    abort: &dyn Fn() -> bool,
) -> Result<(MediaInput, bool), FetchErr> {
    if req.local_file.is_some() {
        return Err(FetchErr::Failed(
            "内部错误：本地文件不应进入下载路径".into(),
        ));
    }
    if req.urls.is_empty() {
        return Err(FetchErr::Failed("没有可用的音频流地址".into()));
    }
    let path = cache_path_in(cache_dir, &req.cache_key);

    // 1. 缓存命中 → 秒开。
    if cache_usable(&path, req.expected_size) {
        let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        crate::util::log::debug(
            "audio",
            &format!("缓存命中: {}（{} 字节）", path.display(), len),
        );
        set_status(status, |s| {
            s.cache_hit = true;
            s.downloaded_bytes = len;
            s.total_bytes = Some(len);
        });
        return Ok((MediaInput::File(path), true));
    }

    // 2. 准备落盘位置；失败则全程走内存。
    let dir_ok = fs::create_dir_all(cache_dir).is_ok();
    let tmp = path.with_extension("part");

    let mut failures: Vec<String> = Vec::new();
    for url in &req.urls {
        if abort() {
            return Err(FetchErr::Aborted);
        }
        let mut request = crate::net::http_client()
            .get(url)
            // 总超时兜底：连接后若长期无数据（CDN 挂起/网络黑洞），会永久阻塞；
            // 给整个请求设上限，超时即报错退出。
            .timeout(std::time::Duration::from_secs(180));
        for (k, v) in &req.headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                request = request.header(name, val);
            }
        }
        let resp = match request.send().await {
            Ok(r) => r,
            Err(e) => {
                crate::util::log::warn("audio", &format!("流地址请求失败: {e}"));
                failures.push(format!("请求失败: {e}"));
                continue;
            }
        };
        let code = resp.status().as_u16();
        if !resp.status().is_success() {
            crate::util::log::warn("audio", &format!("流地址返回 HTTP {code}，换备用地址"));
            failures.push(format!("HTTP {code}"));
            continue; // 403/410/… → 换备用地址
        }
        let total = resp.content_length().or(req.expected_size);
        set_status(status, |s| {
            s.total_bytes = total;
            s.downloaded_bytes = 0;
        });

        // 输出端：优先落盘，失败降级内存。
        let mut out = DownloadOut::new(dir_ok.then(|| tmp.clone()));
        if out.is_mem_only() {
            crate::util::log::warn("audio", "写盘失败，本次下载降级为内存缓冲");
        }
        let mut reader = resp;
        let mut downloaded: u64 = 0;
        let started = std::time::Instant::now();
        let mut last_report: u64 = 0;
        let mut read_err: Option<String> = None;
        loop {
            if abort() {
                out.discard();
                return Err(FetchErr::Aborted);
            }
            // 分块读取。`block_on` 在播放线程上轮询本 future，因此这里的写盘
            // （阻塞 IO）也只阻塞播放线程，不会占住 runtime 的 worker。
            let read = match reader.chunk().await {
                Ok(Some(chunk)) => Ok(chunk.to_vec()),
                Ok(None) => Ok(Vec::new()), // 流结束
                Err(e) => Err(format!("读取音频流失败: {e}")),
            };
            match read {
                Ok(chunk) if chunk.is_empty() => break, // 流结束
                Ok(chunk) => {
                    downloaded += chunk.len() as u64;
                    if out.write_all(&chunk).is_err() {
                        // 落盘失败：读回已写部分降级为内存，继续本次下载。
                        crate::util::log::warn("audio", "下载中途写盘失败，剩余数据转入内存缓冲");
                        out.force_mem_mode();
                        let _ = out.write_all(&chunk);
                    }
                    if downloaded - last_report >= 256 * 1024 {
                        last_report = downloaded;
                        let d = downloaded;
                        set_status(status, |s| s.downloaded_bytes = d);
                    }
                }
                Err(e) => {
                    read_err = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = read_err {
            out.discard();
            crate::util::log::warn("audio", &format!("{e}，换备用地址"));
            failures.push(e);
            continue; // 换下一个地址
        }
        // 下载完成：落盘模式原子重命名；内存模式直接用。
        let mem_only = out.is_mem_only();
        match out.finish(&path) {
            Ok(m) => {
                let d = downloaded;
                set_status(status, |s| {
                    s.downloaded_bytes = d;
                    s.total_bytes = Some(d);
                });
                crate::util::log::info(
                    "audio",
                    &format!(
                        "下载完成: {} 字节，用时 {:.2}s（{}）",
                        d,
                        started.elapsed().as_secs_f32(),
                        if mem_only {
                            "内存缓冲"
                        } else {
                            "已写缓存"
                        },
                    ),
                );
                return Ok((m, false));
            }
            Err(e) => {
                failures.push(e);
                continue;
            }
        }
    }

    Err(FetchErr::Failed(format!(
        "音频下载失败（已尝试 {} 个地址）: {}",
        req.urls.len(),
        failures.join("；")
    )))
}

/// 下载输出端：磁盘临时文件（.part）或内存缓冲，可中途降级。
struct DownloadOut {
    file: Option<(fs::File, PathBuf)>,
    mem: Vec<u8>,
}

impl DownloadOut {
    fn new(tmp: Option<PathBuf>) -> Self {
        let file = tmp.and_then(|p| fs::File::create(&p).ok().map(|f| (f, p)));
        Self {
            file,
            mem: Vec::new(),
        }
    }

    /// 是否处于纯内存模式（建临时文件失败 = 全程内存缓冲）。
    fn is_mem_only(&self) -> bool {
        self.file.is_none()
    }

    fn write_all(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        match &mut self.file {
            Some((f, _)) => f.write_all(chunk),
            None => {
                self.mem.extend_from_slice(chunk);
                Ok(())
            }
        }
    }

    /// 落盘失败时调用：把已写内容读回内存（转纯内存模式，后续 chunk 走 `mem`）。
    fn force_mem_mode(&mut self) {
        if let Some((mut f, p)) = self.file.take() {
            let _ = f.flush();
            drop(f);
            self.mem = fs::read(&p).unwrap_or_default();
            let _ = fs::remove_file(&p);
        }
    }

    fn discard(&mut self) {
        if let Some((_, p)) = self.file.take() {
            let _ = fs::remove_file(&p);
        }
        self.mem.clear();
    }

    /// 完成下载：重命名 .part → 最终路径；重命名失败则直接用 .part 路径。
    /// 内存模式返回 Err 表示数据为空（非法）。
    fn finish(mut self, final_path: &Path) -> Result<MediaInput, String> {
        if let Some((mut f, tmp)) = self.file.take() {
            let _ = f.flush();
            drop(f);
            if fs::rename(&tmp, final_path).is_ok() {
                return Ok(MediaInput::File(final_path.to_path_buf()));
            }
            // rename 失败（极少见，例如跨设备）：保留 .part 也能解码播放。
            return Ok(MediaInput::File(tmp));
        }
        if self.mem.is_empty() {
            return Err("音频流内容为空".into());
        }
        // Vec → Arc<[u8]>：把容量冗余 shrink 掉后零拷贝转共享切片，
        // 内存模式整首歌都驻留内存，容量按实际字节数对齐。
        let vec = std::mem::take(&mut self.mem);
        Ok(MediaInput::Mem(vec.into()))
    }
}

// ---------------------------------------------------------------------------
// 播放线程
