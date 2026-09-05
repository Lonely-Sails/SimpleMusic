//! 音频下载与缓存复用：流式下载到 `.part` 临时文件后原子重命名，
//! 备用 CDN 地址轮替，写盘失败降级内存缓冲，下载中可被打断。

use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::sync::Mutex;

use super::cache::{cache_path_in, cache_usable};
use super::control::{PlayRequest, PlaybackStatus};
use super::control::Command;
use super::decode::MediaInput;
use super::player::set_status;


/// fetch_to_cache 的失败类型。
pub(super) enum FetchErr {
    /// 下载被新命令打断（Stop / 新的 Play），worker 应直接处理下一条命令。
    Aborted,
    Failed(String),
}

fn poll_abort(rx: &Receiver<Command>) -> bool {
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
/// - 否则流式下载（8KB 缓冲）到 `<key>.m4s.part`，完成后原子重命名；
/// - 403/410/404/5xx 或网络错误 → 依次尝试备用 CDN 地址；
/// - 目录创建/写盘失败 → 降级为内存缓冲（不崩）；
/// - 下载期间收到 Stop/Play → `FetchErr::Aborted`。
pub(super) fn fetch_to_cache(
    req: &PlayRequest,
    status: &Mutex<PlaybackStatus>,
    cache_dir: &Path,
    http: &Option<reqwest::blocking::Client>,
    rx: &Receiver<Command>,
) -> Result<(MediaInput, bool), FetchErr> {
    if req.local_file.is_some() {
        return Err(FetchErr::Failed("内部错误：本地文件不应进入下载路径".into()));
    }
    if req.urls.is_empty() {
        return Err(FetchErr::Failed("没有可用的音频流地址".into()));
    }
    let Some(http) = http.as_ref() else {
        return Err(FetchErr::Failed("HTTP 客户端初始化失败，无法下载音频".into()));
    };
    let path = cache_path_in(cache_dir, &req.cache_key);

    // 1. 缓存命中 → 秒开。
    if cache_usable(&path, req.expected_size) {
        let len = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
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
        if poll_abort(rx) {
            return Err(FetchErr::Aborted);
        }
        let mut request = http.get(url);
        for (k, v) in &req.headers {
            if let (Ok(name), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                reqwest::header::HeaderValue::from_str(v),
            ) {
                request = request.header(name, val);
            }
        }
        let resp = match request.send() {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("请求失败: {e}"));
                continue;
            }
        };
        let code = resp.status().as_u16();
        if !resp.status().is_success() {
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
        let mut reader = resp;
        let mut buf = [0u8; 8192];
        let mut downloaded: u64 = 0;
        let mut last_report: u64 = 0;
        let mut read_err: Option<String> = None;
        loop {
            if poll_abort(rx) {
                out.discard();
                return Err(FetchErr::Aborted);
            }
            match reader.read(&mut buf) {
                Ok(0) => break, // 流结束
                Ok(n) => {
                    downloaded += n as u64;
                    if out.write_all(&buf[..n]).is_err() {
                        // 落盘失败：读回已写部分降级为内存，继续本次下载。
                        out.fallback_to_mem(&buf[..n]);
                    }
                    if downloaded - last_report >= 256 * 1024 {
                        last_report = downloaded;
                        let d = downloaded;
                        set_status(status, |s| s.downloaded_bytes = d);
                    }
                }
                Err(e) => {
                    read_err = Some(format!("读取音频流失败: {e}"));
                    break;
                }
            }
        }
        if let Some(e) = read_err {
            out.discard();
            failures.push(e);
            continue; // 换下一个地址
        }
        // 下载完成：落盘模式原子重命名；内存模式直接用。
        match out.finish(&path) {
            Ok(m) => {
                let d = downloaded;
                set_status(status, |s| {
                    s.downloaded_bytes = d;
                    s.total_bytes = Some(d);
                });
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

    fn write_all(&mut self, chunk: &[u8]) -> std::io::Result<()> {
        match &mut self.file {
            Some((f, _)) => f.write_all(chunk),
            None => {
                self.mem.extend_from_slice(chunk);
                Ok(())
            }
        }
    }

    /// 落盘失败时调用：读回已写内容转入内存模式，然后写入当前 chunk。
    fn fallback_to_mem(&mut self, chunk: &[u8]) {
        if let Some((mut f, p)) = self.file.take() {
            let _ = f.flush();
            drop(f);
            self.mem = fs::read(&p).unwrap_or_default();
            let _ = fs::remove_file(&p);
        }
        self.mem.extend_from_slice(chunk);
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
        Ok(MediaInput::Mem(Arc::from(std::mem::take(&mut self.mem))))
    }
}

// ---------------------------------------------------------------------------
// 播放线程
