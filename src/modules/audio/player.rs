//! 专用播放线程：命令串行处理 + 每 100ms 轮询进度/曲终 + 输出设备管理。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::Sink;

use super::cache::cache_path_in;
use super::control::{Command, PlaybackStatus, PlayRequest};
use super::decode::{MediaInput, SourceShared, SymphoniaSource};
use super::download::{fetch_to_cache, FetchErr};

/// load_and_play 的失败类型。
pub(super) enum LoadErr {
    /// 下载被新命令打断，不展示错误。
    Aborted,
    Failed(String),
}

/// 一个活跃的播放会话（输出流必须保活）。
struct PlayerSession {
    _stream: rodio::OutputStream,
    sink: Sink,
    shared: Arc<SourceShared>,
    sample_rate: u32,
}

pub(super) fn set_status(status: &Mutex<PlaybackStatus>, f: impl FnOnce(&mut PlaybackStatus)) {
    if let Ok(mut s) = status.lock() {
        f(&mut s);
    }
}

fn open_output() -> Result<(rodio::OutputStream, Sink), String> {
    let (stream, handle) = rodio::OutputStream::try_default()
        .map_err(|e| format!("无法打开音频输出设备: {e}"))?;
    let sink = Sink::try_new(&handle).map_err(|e| format!("无法打开音频输出设备: {e}"))?;
    Ok((stream, sink))
}

/// 专用播放线程主循环：串行处理命令 + 每 100ms 轮询进度/曲终。
pub(super) fn worker_loop(
    rx: Receiver<Command>,
    status: Arc<Mutex<PlaybackStatus>>,
    cache_dir: PathBuf,
    initial_volume: f32,
) {
    let mut session: Option<PlayerSession> = None;
    let mut volume = initial_volume;
    set_status(&status, |s| s.volume = volume);

    // HTTP 客户端只建一次；失败则所有下载报错（不影响本地文件播放）。
    // 总超时兜底：连接后若长期无数据（CDN 挂起/网络黑洞），blocking read 会永久阻塞，
    // 导致 st.loading 永远为 true、UI 一直转圈。给整个请求设上限，超时即报错退出。
    let http = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .ok();

    loop {
        // 有活跃会话时短超时轮询；空闲时阻塞等待命令。
        let cmd = if session.is_some() {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(c) => Some(c),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match rx.recv() {
                Ok(c) => Some(c),
                Err(_) => break,
            }
        };

        match cmd {
            Some(Command::Play(req)) => {
                // 丢弃旧会话，重置状态（保留音量）。
                session = None;
                set_status(&status, |s| {
                    *s = PlaybackStatus {
                        volume,
                        ..Default::default()
                    };
                    s.loading = true;
                });
                match load_and_play(&req, &status, &cache_dir, &http, &rx, volume) {
                    Ok((stream, sink, shared, sample_rate, duration)) => {
                        set_status(&status, |s| {
                            s.loading = false;
                            s.playing = true;
                            s.position_secs = 0.0;
                            s.duration_secs = duration;
                            s.error = None;
                        });
                        session = Some(PlayerSession {
                            _stream: stream,
                            sink,
                            shared,
                            sample_rate,
                        });
                    }
                    Err(LoadErr::Aborted) => {
                        // 被新命令打断：不加错误，交给下一条命令。
                        set_status(&status, |s| s.loading = false);
                    }
                    Err(LoadErr::Failed(e)) => {
                        set_status(&status, |s| {
                            s.loading = false;
                            s.playing = false;
                            s.error = Some(e);
                        });
                    }
                }
            }
            Some(Command::Pause) => {
                if let Some(sess) = &session {
                    sess.sink.pause();
                    set_status(&status, |s| s.playing = false);
                }
            }
            Some(Command::Resume) => {
                if let Some(sess) = &session {
                    sess.sink.play();
                    set_status(&status, |s| s.playing = true);
                }
            }
            Some(Command::Stop) => {
                session = None;
                set_status(&status, |s| {
                    s.playing = false;
                    s.loading = false;
                    s.finished = false;
                    s.position_secs = 0.0;
                    s.cache_hit = false;
                });
            }
            Some(Command::Seek(t)) => {
                let loading = status.lock().map(|s| s.loading).unwrap_or(true);
                let dur = status.lock().map(|s| s.duration_secs).unwrap_or(0.0);
                let target = if dur > 0.0 { t.clamp(0.0, dur) } else { t.max(0.0) };
                if let Some(sess) = &session {
                    if !loading {
                        sess.shared.base_ms.store((target * 1000.0) as u64, Ordering::Relaxed);
                        if let Ok(mut g) = sess.shared.seek.lock() {
                            *g = Some(target);
                        }
                        set_status(&status, |s| s.position_secs = target);
                    }
                } else if !loading {
                    // 无会话：仅更新记录的位置（下次 play 不继承）。
                    set_status(&status, |s| s.position_secs = target);
                }
            }
            Some(Command::Volume(v)) => {
                let v = v.clamp(0.0, 1.0);
                volume = v;
                if let Some(sess) = &session {
                    sess.sink.set_volume(v);
                }
                set_status(&status, |s| s.volume = v);
            }
            Some(Command::Shutdown) => {
                break;
            }
            None => {
                // 轮询：更新进度 + 曲终检测。
                let Some(sess) = session.as_ref() else { continue };
                let base_ms = sess.shared.base_ms.load(Ordering::Relaxed);
                let frames = sess.shared.emitted.load(Ordering::Relaxed);
                let pos = base_ms as f64 / 1000.0 + frames as f64 / sess.sample_rate as f64;
                set_status(&status, |s| {
                    if !s.playing {
                        return; // 暂停/已停：位置冻结，不做曲终判定
                    }
                    s.position_secs = pos;
                    if sess.sink.empty() {
                        s.playing = false;
                        s.finished = true;
                        if s.duration_secs > 0.0 {
                            s.position_secs = s.duration_secs;
                        }
                    }
                });
            }
        }
    }
    // 退出前由作用域结束 drop PlayerSession（关 Sink/OutputStream）。
}

/// Play 命令的执行体：取媒体 → 打开解码器 → 打开输出设备。
/// 成功返回输出流组件；`LoadErr::Aborted` 表示下载被新命令打断（不进错误状态）。
#[allow(clippy::type_complexity)]
fn load_and_play(
    req: &PlayRequest,
    status: &Mutex<PlaybackStatus>,
    cache_dir: &Path,
    http: &Option<reqwest::blocking::Client>,
    rx: &Receiver<Command>,
    volume: f32,
) -> Result<(rodio::OutputStream, Sink, Arc<SourceShared>, u32, f64), LoadErr> {
    // 1. 取得媒体数据（本地文件 or 下载缓存）。
    let (mut input, was_cached) = if let Some(p) = &req.local_file {
        (MediaInput::File(p.clone()), false)
    } else {
        match fetch_to_cache(req, status, cache_dir, http, rx) {
            Ok((m, hit)) => (m, hit),
            Err(FetchErr::Aborted) => return Err(LoadErr::Aborted),
            Err(FetchErr::Failed(e)) => return Err(LoadErr::Failed(e)),
        }
    };

    // 2. 解码器。缓存命中的文件若解码失败，可能是缓存损坏：删除后重下载一次。
    let source = match SymphoniaSource::new(input.clone()) {
        Ok(s) => s,
        Err(e) => {
            if !was_cached || req.local_file.is_some() {
                return Err(LoadErr::Failed(e));
            }
            let cached = cache_path_in(cache_dir, &req.cache_key);
            let _ = fs::remove_file(&cached);
            match fetch_to_cache(req, status, cache_dir, http, rx) {
                Ok((m, _)) => {
                    set_status(status, |s| s.cache_hit = false);
                    input = m;
                }
                Err(FetchErr::Aborted) => return Err(LoadErr::Aborted),
                Err(FetchErr::Failed(e2)) => {
                    return Err(LoadErr::Failed(format!("{e}；缓存重建下载也失败: {e2}")))
                }
            }
            match SymphoniaSource::new(input) {
                Ok(s) => s,
                Err(e2) => return Err(LoadErr::Failed(format!("{e}；缓存重建后仍解码失败: {e2}"))),
            }
        }
    };

    // 3. 时长：容器里读不到时按 size/bandwidth 估算。
    let duration = source
        .duration_secs
        .or_else(|| match (req.expected_size, req.bandwidth) {
            (Some(size), Some(bw)) if bw > 0 => Some(size as f64 * 8.0 / bw as f64),
            _ => None,
        })
        .unwrap_or(0.0);

    // 4. 输出设备。
    let shared = source.shared.clone();
    let sample_rate = source.sample_rate;
    let (stream, sink) = open_output().map_err(LoadErr::Failed)?;
    sink.set_volume(volume);
    sink.append(source);
    Ok((stream, sink, shared, sample_rate, duration))
}
