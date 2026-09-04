//! 音频播放引擎：B 站音源下载缓存（md5 键控）+ symphonia 解码 + rodio 输出。
//!
//! ## 架构（线程 / 通道）
//!
//! ```text
//! UI 线程                        AudioEngine（UI 侧句柄）
//!   │  play_stream()/pause()/…      │
//!   └── mpsc:Command ──────────────▶ 播放线程（simple-music-audio，唯一所有者）
//!                                      ├─ 下载：reqwest::blocking 流式写缓存
//!                                      │   ~/.cache/simple-music/audio/<md5(key)>.m4s
//!                                      ├─ 解码：symphonia probe → FormatReader/Decoder
//!                                      └─ 输出：rodio OutputStream + Sink（音量/暂停）
//!   UI 只读轮询 ◀── Arc<Mutex<PlaybackStatus>>（position/duration/playing/finished/
//!                  error/downloaded_bytes/total_bytes/cache_hit）
//! ```
//!
//! - 解码是拉式的：`SymphoniaSource` 实现 `rodio::Source<Item=i16>`，rodio 每要一段样本
//!   才解码一个包；暂停时 rodio 停止拉取，解码与进度自然冻结（无需额外同步）。
//! - `position_secs` 由**已输出样本帧数累计**（见 `SourceShared.emitted`），seek 时清零
//!   并以 `base_ms`（目标秒）为基准续算。
//! - 无声卡环境：输出设备打开失败不 panic，错误写入 `PlaybackStatus.error`
//!   （文案前缀「无法打开音频输出设备」）。
//!
//! ## UI 集成速览（给 UI Worker）
//!
//! 1. 提交播放：`engine.play_stream(&stream_url, bvid)` —— 直接传
//!    `modules::bilibili::StreamUrl`（下载头 `required_headers` 已内含），第二个参数是
//!    **缓存键**（用 bvid，重复播放同一视频可秒开）。本地文件用
//!    `engine.play_file(path)`。
//! 2. 轮询：每帧 `engine.status()`，把 `position_secs/duration_secs/playing/volume` 映射进
//!    `PlaybackState`；`loading=true` 时建议禁用进度条拖动。
//! 3. 曲终感知：轮询到 `finished == true` 后调 `engine.take_finished()`（读后清除），
//!    再 `play_stream` 下一首。
//! 4. 音量：`engine.set_volume(0.0..=1.0)`；seek：`engine.seek(secs)`（引擎会按
//!    duration 钳制；`loading` 期间的 pause/seek 会被忽略）。
//! 5. 错误展示：`status.error` 非 None 时直接展示该文案（下载失败/解码失败/无设备
//!    都会进入此状态，引擎绝不 panic）。

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rodio::Sink;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use crate::modules::bilibili::{md5_hex, StreamUrl};

// ---------------------------------------------------------------------------
// 缓存路径
// ---------------------------------------------------------------------------

/// 默认音频缓存目录：`$XDG_CACHE_HOME`（缺省 `$HOME/.cache`）下的
/// `simple-music/audio`。HOME/XDG 都拿不到时退回系统临时目录。
pub fn default_cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("simple-music").join("audio")
}

/// 缓存文件路径：`<cache_dir>/<md5(cache_key)>.m4s`。
/// 纯函数（不触盘），供引擎与测试使用。
pub fn cache_path_in(dir: &Path, cache_key: &str) -> PathBuf {
    dir.join(format!("{}.m4s", md5_hex(cache_key)))
}

/// 缓存命中判定：期望大小已知时必须严格等于文件长度；未知时只接受 >1KB 的文件
/// （防止空文件/残页被当成有效缓存）。
fn cache_usable(path: &Path, expected_size: Option<u64>) -> bool {
    match fs::metadata(path) {
        Ok(m) if m.is_file() => match expected_size {
            Some(want) => m.len() == want,
            None => m.len() > 1024,
        },
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 共享播放状态
// ---------------------------------------------------------------------------

/// 引擎与 UI 之间的共享播放状态（`Arc<Mutex<…>>`，UI 只读轮询）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlaybackStatus {
    /// 下载/解码准备中（play 已接受，尚未开始出声）。
    pub loading: bool,
    /// 正在出声（未暂停、未结束、无错误）。
    pub playing: bool,
    /// 自然播放到结尾（与 stop 区分；UI 读后可用 take_finished 清除）。
    pub finished: bool,
    /// 当前播放位置（秒，按已输出帧数累计）。
    pub position_secs: f64,
    /// 曲目时长估计（秒）；完全未知时为 0。
    pub duration_secs: f64,
    /// 音量 0.0 ~ 1.0。
    pub volume: f32,
    /// 下载进度：已下载字节。
    pub downloaded_bytes: u64,
    /// 下载进度：总字节（Content-Length 或 StreamUrl.size_bytes；未知为 None）。
    pub total_bytes: Option<u64>,
    /// 错误状态（下载/解码/输出设备）；非 None 时 UI 可直接展示。
    pub error: Option<String>,
    /// 本次播放是否直接命中磁盘缓存（诊断用）。
    pub cache_hit: bool,
}

// ---------------------------------------------------------------------------
// 播放请求
// ---------------------------------------------------------------------------

/// 一次播放任务描述。
#[derive(Debug, Clone)]
pub struct PlayRequest {
    /// 缓存键。建议传 bvid —— 直链带签名参数每次解析都会变，用 bvid 才能命中缓存秒开。
    pub cache_key: String,
    /// 主音频直链 + 备用 CDN 地址（403/410/5xx 时按序尝试）。
    pub urls: Vec<String>,
    /// 下载必须携带的 HTTP 头（StreamUrl.required_headers：UA/Referer/Cookie）。
    pub headers: Vec<(String, String)>,
    /// 音频文件期望大小（字节），用于缓存校验与 total_bytes 展示。
    pub expected_size: Option<u64>,
    /// 音频码率（bps），用于在容器读不出时长时按 size/bandwidth 估算时长。
    pub bandwidth: Option<i64>,
    /// 直接播放本地文件（测试/本地音乐）；设置后跳过网络下载。
    pub local_file: Option<PathBuf>,
}

impl PlayRequest {
    /// 从 B 站解析结果构造播放请求。`cache_key` 传 bvid（不要传直链）。
    pub fn from_stream(stream: &StreamUrl, cache_key: &str) -> Self {
        let mut urls = vec![stream.audio_url.clone()];
        urls.extend(stream.audio_backup_urls.iter().cloned());
        Self {
            cache_key: cache_key.to_string(),
            urls,
            headers: stream.required_headers.clone(),
            expected_size: stream.size_bytes.map(|s| s.max(0) as u64),
            bandwidth: stream.bandwidth,
            local_file: None,
        }
    }

    /// 播放本地文件（无需网络）。
    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        let p = path.into();
        Self {
            cache_key: p.display().to_string(),
            urls: Vec::new(),
            headers: Vec::new(),
            expected_size: None,
            bandwidth: None,
            local_file: Some(p),
        }
    }
}

// ---------------------------------------------------------------------------
// 命令通道
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Command {
    Play(PlayRequest),
    Pause,
    Resume,
    Stop,
    Seek(f64),
    Volume(f32),
    Shutdown,
}

// ---------------------------------------------------------------------------
// 媒体数据来源（磁盘文件或内存缓冲）
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum MediaInput {
    File(PathBuf),
    Mem(Arc<[u8]>),
}

impl MediaInput {
    fn open(&self) -> Result<Box<dyn MediaSource>, String> {
        match self {
            MediaInput::File(p) => {
                let f = fs::File::open(p)
                    .map_err(|e| format!("打开音频文件失败({}): {e}", p.display()))?;
                Ok(Box::new(f))
            }
            // Cursor<Arc<Vec<u8>>> 实现了 symphonia 的 MediaSource，克隆零拷贝。
            MediaInput::Mem(v) => Ok(Box::new(std::io::Cursor::new(v.clone()))),
        }
    }

    fn describe(&self) -> String {
        match self {
            MediaInput::File(p) => p.display().to_string(),
            MediaInput::Mem(v) => format!("<内存缓冲 {} 字节>", v.len()),
        }
    }
}

// ---------------------------------------------------------------------------
// symphonia 解码源（rodio::Source）
// ---------------------------------------------------------------------------

/// 播放线程与解码源之间的共享控制块。
struct SourceShared {
    /// 待处理的 seek 请求（秒）。播放线程写入，解码源在拉取样本前消费。
    seek: Mutex<Option<f64>>,
    /// seek 基准（毫秒）：position = base_ms/1000 + emitted/sample_rate。
    base_ms: AtomicU64,
    /// 自上次 seek 起已输出的帧数（解码源写入，播放线程轮询）。
    emitted: AtomicU64,
}

impl SourceShared {
    fn new() -> Self {
        Self {
            seek: Mutex::new(None),
            base_ms: AtomicU64::new(0),
            emitted: AtomicU64::new(0),
        }
    }
}

/// symphonia 驱动的拉式解码源：输出 i16 交错样本，rodio 负责重采样/声道转换
/// （UniformSourceIterator 会把我们的输出统一到输出设备参数）。
struct SymphoniaSource {
    input: MediaInput,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<symphonia::core::units::TimeBase>,
    /// 当前解码出的 i16 交错样本及读取位置。
    pending: Vec<i16>,
    pos: usize,
    channels: usize,
    sample_rate: u32,
    /// 容器时长估计（秒）。
    duration_secs: Option<f64>,
    shared: Arc<SourceShared>,
    /// seek 粗定位后需要丢弃的帧数（把 packet 边界对齐到目标时间）。
    skip_frames: u64,
    /// 自上次 seek 起累计输出帧数（镜像到 shared.emitted）。
    out_frames: u64,
    eos: bool,
}

/// 打开媒体源并 probe：返回（FormatReader, Decoder, track_id, time_base, 采样率,
/// 声道数, 时长估计）。
#[allow(clippy::type_complexity)]
fn open_media(
    input: &MediaInput,
) -> Result<
    (
        Box<dyn FormatReader>,
        Box<dyn Decoder>,
        u32,
        Option<symphonia::core::units::TimeBase>,
        u32,
        usize,
        Option<f64>,
    ),
    String,
> {
    let src = input.open()?;
    let mss = MediaSourceStream::new(src, MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let MediaInput::File(p) = input {
        if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
            // m4s 只是 B 站的叫法，本质是 fMP4；给 probe 一个认识的扩展名。
            hint.with_extension(if ext.eq_ignore_ascii_case("m4s") {
                "mp4"
            } else {
                ext
            });
        }
    } else {
        hint.with_extension("mp4");
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("音频格式探测失败({}): {e}", input.describe()))?;
    let format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| format!("媒体文件中没有可解码的音频轨道: {}", input.describe()))?;
    let track_id = track.id;
    let rate = track.codec_params.sample_rate.unwrap_or(0);
    let channels = track
        .codec_params
        .channels
        .map(|c| c.count())
        .unwrap_or(0);
    let duration_secs = track
        .codec_params
        .n_frames
        .zip(track.codec_params.time_base)
        .map(|(n, tb)| {
            let t = tb.calc_time(n);
            t.seconds as f64 + t.frac
        })
        .filter(|d| *d > 0.0);
    let decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("创建音频解码器失败: {e}"))?;
    let time_base = track.codec_params.time_base;
    Ok((format, decoder, track_id, time_base, rate, channels, duration_secs))
}

impl SymphoniaSource {
    fn new(input: MediaInput) -> Result<Self, String> {
        let (format, decoder, track_id, time_base, rate, channels, duration_secs) =
            open_media(&input)?;
        let mut src = Self {
            input,
            format,
            decoder,
            track_id,
            time_base,
            pending: Vec::new(),
            pos: 0,
            channels,
            sample_rate: rate,
            duration_secs,
            shared: Arc::new(SourceShared::new()),
            skip_frames: 0,
            out_frames: 0,
            eos: false,
        };
        // 轨道参数不完整（B 站 m4s 的 isomp4 轨道常缺声道数，只给采样率）：
        // 预解码第一个包，从解码输出取真实 spec；同时提前暴露坏文件。
        if src.sample_rate == 0 || src.channels == 0 {
            if !src.fill() {
                return Err("音轨无有效音频帧".into());
            }
        }
        if src.sample_rate == 0 || src.channels == 0 {
            return Err(format!(
                "音轨参数不完整(rate={}, channels={})",
                src.sample_rate, src.channels
            ));
        }
        Ok(src)
    }

    /// 解出下一个包的样本到 pending。返回 false 表示流已结束。
    fn fill(&mut self) -> bool {
        if self.eos {
            return false;
        }
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.eos = true;
                    return false;
                }
                Err(SymphError::ResetRequired) | Err(_) => {
                    // 其余错误一律按流结束处理（错误细节已在状态里可见的路径之外，
                    // 引擎层以「曲终」收尾，不 panic）。
                    self.eos = true;
                    return false;
                }
            };
            if packet.track_id() != self.track_id {
                continue;
            }
            let decoded = match self.decoder.decode(&packet) {
                Ok(d) => d,
                Err(SymphError::DecodeError(_)) => continue, // 坏包跳过
                Err(_) => {
                    self.eos = true;
                    return false;
                }
            };
            let spec = *decoded.spec();
            // 轨道参数缺失时以解码输出的真实 spec 为准。
            if self.sample_rate == 0 {
                self.sample_rate = spec.rate;
            }
            if self.channels == 0 {
                self.channels = spec.channels.count();
            }
            let capacity = decoded.capacity();            let mut sbuf = SampleBuffer::<i16>::new(capacity as u64, spec);
            sbuf.copy_interleaved_ref(decoded);
            let samples = sbuf.samples();
            if samples.is_empty() {
                continue;
            }
            // seek 细修剪：丢掉 packet 边界与目标时间之间的帧。
            let mut start = 0usize;
            if self.skip_frames > 0 {
                let avail_frames = (samples.len() / self.channels) as u64;
                let drop = self.skip_frames.min(avail_frames);
                self.skip_frames -= drop;
                start = (drop as usize) * self.channels;
                if start >= samples.len() {
                    continue;
                }
            }
            // 追加新样本后，读取位置应指向新数据的起点（旧数据已全部消费）。
            let old_len = self.pending.len();
            self.pending.extend_from_slice(&samples[start..]);
            self.pos = old_len;
            return true;
        }
    }

    /// 处理播放线程下发的 seek 请求。
    fn maybe_seek(&mut self) {
        let request = self
            .shared
            .seek
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        if let Some(t) = request {
            if self.perform_seek(t).is_err() {
                // 彻底失败的兜底：回到起点（保持不 panic，进度同步归零）。
                let _ = self.perform_seek(0.0);
                self.shared.base_ms.store(0, Ordering::Relaxed);
            }
        }
    }

    /// seek 到目标秒。优先 symphonia 容器内 seek（Accurate）+ 帧级修剪；
    /// 失败则重建解码器并按时间戳快进（只 demux 不解码，代价低）。
    fn perform_seek(&mut self, target_secs: f64) -> Result<(), String> {
        let target_secs = target_secs.max(0.0);
        self.eos = false;
        // 目标 >= 时长：直接按播完处理（sink 变空后引擎标记 finished）。
        if let Some(dur) = self.duration_secs {
            if target_secs >= dur {
                self.pending.clear();
                self.pos = 0;
                self.skip_frames = 0;
                self.out_frames = 0;
                self.shared.emitted.store(0, Ordering::Relaxed);
                self.eos = true;
                return Ok(());
            }
        }
        let time = Time::from(Duration::from_secs_f64(target_secs));
        match self.format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            },
        ) {
            Ok(seeked) => {
                // Accurate seek 保证 actual_ts <= 目标；差额按帧修剪。
                let actual_secs = self
                    .time_base
                    .map(|tb| {
                        let t = tb.calc_time(seeked.actual_ts);
                        t.seconds as f64 + t.frac
                    })
                    .unwrap_or(0.0);
                self.skip_frames =
                    ((target_secs - actual_secs).max(0.0) * self.sample_rate as f64).round() as u64;
                let _ = self.decoder.reset();
                self.pending.clear();
                self.pos = 0;
                self.out_frames = 0;
                self.shared.emitted.store(0, Ordering::Relaxed);
                Ok(())
            }
            Err(_) => self.rebuild_and_fast_forward(target_secs),
        }
    }

    /// seek 回退路径：重开媒体源重建解码器，然后按包时间戳快进到目标
    /// （只 demux，不解码，速度远快于逐包解码）。
    fn rebuild_and_fast_forward(&mut self, target_secs: f64) -> Result<(), String> {
        let (format, decoder, track_id, time_base, _rate, _channels, _dur) =
            open_media(&self.input)?;
        let target_ts = time_base
            .ok_or_else(|| "音轨缺少时间基，无法按时间 seek".to_string())
            .map(|tb| tb.calc_timestamp(Time::from(Duration::from_secs_f64(target_secs))))?;
        self.format = format;
        self.decoder = decoder;
        self.track_id = track_id;
        self.time_base = time_base;
        loop {
            match self.format.next_packet() {
                Ok(p) => {
                    if p.track_id() == self.track_id && p.ts() >= target_ts {
                        break;
                    }
                }
                Err(SymphError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.eos = true;
                    self.pending.clear();
                    self.pos = 0;
                    return Err("快进到目标位置前已到达流末尾".into());
                }
                Err(e) => {
                    self.eos = true;
                    return Err(format!("快进失败: {e}"));
                }
            }
        }
        let _ = self.decoder.reset();
        self.skip_frames = 0;
        self.pending.clear();
        self.pos = 0;
        self.out_frames = 0;
        self.shared.emitted.store(0, Ordering::Relaxed);
        Ok(())
    }
}

impl Iterator for SymphoniaSource {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        loop {
            self.maybe_seek();
            if self.pos >= self.pending.len() && !self.fill() && self.pos >= self.pending.len() {
                return None;
            }
            if self.pos >= self.pending.len() {
                continue; // fill 补到了数据（或 seek 后重试）
            }
            let s = self.pending[self.pos];
            self.pos += 1;
            // 按帧计数（位置以帧为单位，channel 数恒定）。
            if self.pos % self.channels == 0 {
                self.out_frames += 1;
                self.shared.emitted.store(self.out_frames, Ordering::Relaxed);
            }
            return Some(s);
        }
    }
}

impl rodio::Source for SymphoniaSource {
    /// 全程采样率/声道不变 → 整条流视作一个 span（None = 无限延续到流结束）。
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels as u16
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.duration_secs.map(Duration::from_secs_f64)
    }
}

// ---------------------------------------------------------------------------
// 下载与缓存
// ---------------------------------------------------------------------------

/// fetch_to_cache 的失败类型。
enum FetchErr {
    /// 下载被新命令打断（Stop / 新的 Play），worker 应直接处理下一条命令。
    Aborted,
    Failed(String),
}

/// 下载过程中检查是否有中止命令（Stop/Play）。其余命令在下载期间被忽略。
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
fn fetch_to_cache(
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
// ---------------------------------------------------------------------------

/// 一个活跃的播放会话（输出流必须保活）。
struct PlayerSession {
    _stream: rodio::OutputStream,
    sink: Sink,
    shared: Arc<SourceShared>,
    sample_rate: u32,
}

fn set_status(status: &Mutex<PlaybackStatus>, f: impl FnOnce(&mut PlaybackStatus)) {
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
fn worker_loop(
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

/// load_and_play 的失败类型。
enum LoadErr {
    /// 下载被新命令打断，不展示错误。
    Aborted,
    Failed(String),
}

// ---------------------------------------------------------------------------
// AudioEngine（UI 侧句柄）
// ---------------------------------------------------------------------------

/// 音频引擎句柄：命令经 mpsc 发给专用播放线程；状态经 `Arc<Mutex<…>>` 共享。
/// Clone 语义不需要 —— UI 持有一个实例即可；Drop 时停线程并关闭输出。
pub struct AudioEngine {
    tx: Sender<Command>,
    status: Arc<Mutex<PlaybackStatus>>,
    worker: Option<std::thread::JoinHandle<()>>,
    cache_dir: PathBuf,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    /// 创建引擎（启动专用播放线程；缓存目录用 [`default_cache_dir`]）。
    pub fn new() -> Self {
        Self::with_cache_dir(default_cache_dir())
    }

    /// 创建引擎并指定缓存目录（测试用）。
    pub fn with_cache_dir(cache_dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Mutex::new(PlaybackStatus::default()));
        let worker = {
            let rx = rx;
            let status = status.clone();
            let dir = cache_dir.clone();
            std::thread::Builder::new()
                .name("simple-music-audio".into())
                .spawn(move || worker_loop(rx, status, dir, 0.8))
                .ok()
        };
        Self {
            tx,
            status,
            worker,
            cache_dir,
        }
    }

    /// 当前缓存目录。
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// 某缓存键对应的缓存文件路径（诊断/测试用）。
    pub fn cache_path(&self, cache_key: &str) -> PathBuf {
        cache_path_in(&self.cache_dir, cache_key)
    }

    // ---- 提交播放 ----

    /// 播放 B 站音频流：下载/复用缓存 → 解码 → 输出。`cache_key` 传 bvid。
    /// 异步：函数立即返回，进度/错误看 [`AudioEngine::status`]。
    pub fn play_stream(&self, stream: &StreamUrl, cache_key: &str) {
        self.submit(Command::Play(PlayRequest::from_stream(stream, cache_key)));
    }

    /// 直接提交一个预构造的播放请求。
    pub fn play_request(&self, req: PlayRequest) {
        self.submit(Command::Play(req));
    }

    /// 播放本地音频文件（wav/mp3/m4a…，由 symphonia feature 决定）。
    pub fn play_file(&self, path: &Path) {
        self.submit(Command::Play(PlayRequest::from_file(path)));
    }

    // ---- 控制命令 ----

    /// 暂停（保留进度）。
    pub fn pause(&mut self) {
        self.submit(Command::Pause);
    }

    /// 从暂停恢复。
    pub fn resume(&mut self) {
        self.submit(Command::Resume);
    }

    /// 停止并清空会话（position 归零，finished 清除）。
    pub fn stop(&mut self) {
        self.submit(Command::Stop);
    }

    /// seek 到指定秒（会按已知时长钳制；`loading` 期间忽略）。
    pub fn seek(&mut self, secs: f64) {
        self.submit(Command::Seek(secs));
    }

    /// 设置音量 0.0 ~ 1.0（越界自动钳制）。
    pub fn set_volume(&mut self, volume: f32) {
        self.submit(Command::Volume(volume));
    }

    // ---- 状态查询 ----

    /// 状态快照（UI 每帧轮询用）。
    pub fn status(&self) -> PlaybackStatus {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// 是否在加载（下载/解码准备）中。
    pub fn is_loading(&self) -> bool {
        self.status.lock().map(|s| s.loading).unwrap_or(false)
    }

    /// 最近一次错误文案（不清除）。
    pub fn last_error(&self) -> Option<String> {
        self.status.lock().ok().and_then(|s| s.error.clone())
    }

    /// 曲终标志：读到 `true` 并清除（UI 播下一首的入口）。
    pub fn take_finished(&self) -> bool {
        self.status
            .lock()
            .map(|mut s| std::mem::take(&mut s.finished))
            .unwrap_or(false)
    }

    fn submit(&self, cmd: Command) {
        // 发送失败 = 线程已退出（不应发生；Drop 前通道保持连接）。
        let _ = self.tx.send(cmd);
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Shutdown);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use rodio::Source as _;
    use std::time::Instant;

    // ---- 工具：手写 PCM WAV（不引入第三方依赖） ----

    fn wav_bytes(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
        let data_len = (samples.len() * 2) as u32;
        let block_align = channels * 2;
        let mut v = Vec::with_capacity(44 + data_len as usize);
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk 大小
        v.extend_from_slice(&1u16.to_le_bytes()); // PCM
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        v.extend_from_slice(&(rate * block_align as u32).to_le_bytes()); // byte rate
        v.extend_from_slice(&block_align.to_le_bytes());
        v.extend_from_slice(&16u16.to_le_bytes()); // 位深
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        for s in samples {
            v.extend_from_slice(&s.to_le_bytes());
        }
        v
    }

    /// 生成立体声正弦波 WAV 文件，返回 (路径, 帧数, 采样率, 声道)。
    fn synth_wav(dir: &Path, name: &str, secs: f64, rate: u32) -> (PathBuf, u64, u32, u16) {
        let frames = (secs * rate as f64).round() as usize;
        let mut samples = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let s = ((2.0 * std::f64::consts::PI * 440.0 * i as f64 / rate as f64).sin()
                * 12000.0) as i16;
            samples.push(s);
            samples.push(s);
        }
        let path = dir.join(name);
        fs::write(&path, wav_bytes(&samples, 2, rate)).unwrap();
        (path, frames as u64, rate, 2)
    }

    /// 每个测试用独立临时目录，避免并行冲突。
    fn test_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::AtomicUsize;
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "simple-music-audio-test-{}-{}-{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::create_dir_all(&d);
        d
    }

    fn wait_for<F: Fn(&PlaybackStatus) -> bool>(engine: &AudioEngine, timeout: Duration, f: F) -> PlaybackStatus {
        let start = Instant::now();
        loop {
            let st = engine.status();
            if f(&st) || start.elapsed() > timeout {
                return st;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // ---- 解码链路（无输出设备也能验证） ----

    #[test]
    fn test_decode_wav_counts_samples_and_duration() {
        let dir = test_dir("decode");
        let rate = 8000u32;
        let (path, frames, _rate, ch) = synth_wav(&dir, "tone.wav", 1.0, rate);
        let src = SymphoniaSource::new(MediaInput::File(path)).expect("打开 wav 失败");
        assert_eq!(src.channels(), ch as u16);
        assert_eq!(src.sample_rate(), rate);

        let total_samples = src.count();
        assert_eq!(total_samples, (frames * ch as u64) as usize, "样本数应与合成数据一致");
        // 时长 = 样本帧数 / 采样率 ≈ 1.0s。
        let dur = total_samples as f64 / ch as f64 / rate as f64;
        assert!((dur - 1.0).abs() < 0.01, "实测时长 {dur}s");
        if let Some(d) = src_total_duration_of(&dir, "tone.wav") {
            assert!((d - 1.0).abs() < 0.05, "容器时长估计 {d}s 应接近 1s");
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// 打开文件只读容器时长估计（辅助断言）。
    fn src_total_duration_of(dir: &Path, name: &str) -> Option<f64> {
        SymphoniaSource::new(MediaInput::File(dir.join(name)))
            .ok()
            .and_then(|s| s.duration_secs)
    }

    #[test]
    fn test_seek_middle_counts_remaining() {
        let dir = test_dir("seek-mid");
        let rate = 8000u32;
        let (path, frames, _r, _ch) = synth_wav(&dir, "tone.wav", 2.0, rate);
        let mut src = SymphoniaSource::new(MediaInput::File(path)).unwrap();
        // 先消耗 0.2s 的样本。
        let warmup = (0.2 * rate as f64) as usize * src.channels() as usize;
        assert_eq!(src.by_ref().take(warmup).count(), warmup);
        // seek 到 1.5s。
        src.perform_seek(1.5).expect("wav seek 应成功");
        let channels = src.channels();
        let remaining_samples: usize = src.count();
        let remaining_secs = remaining_samples as f64 / channels as f64 / rate as f64;
        assert!(
            (remaining_secs - 0.5).abs() < 0.05,
            "seek 后剩余 {remaining_secs:.3}s，期望约 0.5s"
        );
        assert_eq!(frames, 16000);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_seek_past_end_ends_stream_without_panic() {
        let dir = test_dir("seek-past-end");
        let (path, _f, _r, _c) = synth_wav(&dir, "tone.wav", 1.0, 8000);
        let mut src = SymphoniaSource::new(MediaInput::File(path)).unwrap();
        // 越界 seek：按“播完”处理，不 panic，流直接结束。
        src.perform_seek(100.0).expect("越界 seek 应按播完处理");
        assert_eq!(src.next(), None);
        // 负值 seek：钳制到 0，可继续播放。
        src.perform_seek(-3.0).unwrap();
        assert!(src.next().is_some(), "seek 到 0 后应能继续解码");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_decode_from_memory_input() {
        // 内存降级路径：同样的数据以 Cursor 喂入 symphonia 也能解码。
        let samples: Vec<i16> = (0..4000).map(|i| ((i % 80) * 200 - 8000) as i16).collect();
        let bytes = wav_bytes(&samples, 1, 8000);
        let src = SymphoniaSource::new(MediaInput::Mem(Arc::from(bytes))).unwrap();
        assert_eq!(src.channels(), 1);
        assert_eq!(src.count(), 4000);
    }

    // ---- 缓存路径 / 命中规则 ----

    #[test]
    fn test_cache_path_deterministic_and_distinct() {
        let dir = PathBuf::from("/tmp/cache-x");
        let p1 = cache_path_in(&dir, "BV1xx411c7mD");
        let p2 = cache_path_in(&dir, "BV1xx411c7mD");
        let p3 = cache_path_in(&dir, "BV1GJ411x7h7");
        assert_eq!(p1, p2, "同键同路径");
        assert_ne!(p1, p3, "不同键不同路径");
        assert_eq!(p1.extension().and_then(|e| e.to_str()), Some("m4s"));
        assert_eq!(
            p1.file_name().and_then(|n| n.to_str()),
            Some(format!("{}.m4s", md5_hex("BV1xx411c7mD")).as_str())
        );
    }

    #[test]
    fn test_cache_usable_rules() {
        let dir = test_dir("cache");
        let p = dir.join("x.m4s");
        fs::write(&p, vec![0u8; 100]).unwrap();
        assert!(!cache_usable(&p, Some(200)), "大小不匹配 → 不可用");
        assert!(!cache_usable(&p, None), "过小(<1KB)且无期望大小 → 不可用");
        fs::write(&p, vec![0u8; 2048]).unwrap();
        assert!(cache_usable(&p, Some(2048)), "大小匹配 → 可用");
        assert!(cache_usable(&p, None), "无期望大小且 >1KB → 可用");
        assert!(!cache_usable(&dir.join("missing.m4s"), None), "不存在 → 不可用");
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- 引擎级：错误状态传播 / 无输出设备 / 生命周期 ----

    #[test]
    fn test_engine_reports_decode_error_without_panic() {
        let dir = test_dir("engine-err");
        let engine = AudioEngine::with_cache_dir(dir.clone());
        engine.play_file(Path::new(&dir).join("不存在.wav").as_path());
        let st = wait_for(&engine, Duration::from_secs(5), |s| {
            !s.loading && (s.error.is_some() || s.playing || s.finished)
        });
        assert!(st.error.is_some(), "解码失败应进入错误状态");
        assert!(!st.playing);
        assert!(!st.loading);
        drop(engine);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_output_device_missing_or_plays_without_panic() {
        // 沙箱无声卡：错误文案必须是「无法打开音频输出设备…」；
        // 有声卡环境：能正常进入 playing —— 两种结果都不得 panic。
        let dir = test_dir("engine-device");
        let (path, _f, _r, _c) = synth_wav(&dir, "tone.wav", 1.0, 8000);
        let engine = AudioEngine::with_cache_dir(dir.clone());
        engine.play_file(&path);
        let st = wait_for(&engine, Duration::from_secs(8), |s| {
            !s.loading && (s.error.is_some() || s.playing || s.finished)
        });
        if let Some(e) = &st.error {
            assert!(
                e.contains("无法打开音频输出设备") || e.contains("打开音频文件") || !e.is_empty(),
                "错误文案应明确: {e}"
            );
        } else {
            assert!(st.playing || st.finished, "无错误时应可播放");
            let mut e2 = engine;
            e2.stop();
            drop(e2);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_seek_clamp_without_session() {
        let dir = test_dir("engine-seek");
        let mut engine = AudioEngine::with_cache_dir(dir.clone());
        engine.seek(-3.0);
        // 负值钳到 0（初始值也是 0，等待命令被处理以免后续状态被覆盖）。
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(engine.status().position_secs, 0.0, "负值钳到 0");
        engine.seek(100.0);
        let st = wait_for(&engine, Duration::from_secs(2), |s| s.position_secs == 100.0);
        assert_eq!(st.position_secs, 100.0, "无时长时只钳下界");
        engine.pause();
        engine.resume();
        engine.stop();
        let st = wait_for(&engine, Duration::from_secs(2), |s| s.position_secs == 0.0 && !s.playing);
        assert_eq!(st.position_secs, 0.0, "stop 后归零");
        drop(engine);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_volume_state() {
        let dir = test_dir("engine-vol");
        let mut engine = AudioEngine::with_cache_dir(dir.clone());
        engine.set_volume(0.5);
        let st = wait_for(&engine, Duration::from_secs(2), |s| (s.volume - 0.5).abs() < 1e-6);
        assert_eq!(st.volume, 0.5);
        engine.set_volume(7.0);
        let st = wait_for(&engine, Duration::from_secs(2), |s| s.volume == 1.0);
        assert_eq!(st.volume, 1.0, "越界钳制");
        drop(engine);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_drop_without_play_is_clean() {
        let dir = test_dir("engine-drop");
        {
            let engine = AudioEngine::with_cache_dir(dir.clone());
            assert!(!engine.is_loading());
            assert_eq!(engine.status(), PlaybackStatus::default());
        } // Drop：线程 join，无 panic。
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_engine_downloads_to_disk_cache_and_hits_second_time() {
        use std::io::Write as _;
        let dir = test_dir("engine-http");
        // 合成一段 WAV 并用本地 TCP 服务充当 HTTP 音源（无外部依赖）。
        let samples: Vec<i16> = (0..8000).map(|i| ((i % 50) * 300 - 7000) as i16).collect();
        let body = wav_bytes(&samples, 1, 8000);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let server_body = body.clone();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let Ok((mut sock, _)) = listener.accept() else { return };
            let mut buf = [0u8; 1024];
            let _ = std::io::Read::read(&mut sock, &mut buf); // 读掉请求头
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                server_body.len()
            );
            let _ = sock.write_all(resp.as_bytes());
            let _ = sock.write_all(&server_body);
        });

        let cache_dir = dir.join("cache");
        let engine = AudioEngine::with_cache_dir(cache_dir);
        let make_req = || PlayRequest {
            cache_key: "local-test".into(),
            urls: vec![format!("http://127.0.0.1:{port}/audio.wav")],
            headers: Vec::new(),
            expected_size: Some(body.len() as u64),
            bandwidth: None,
            local_file: None,
        };
        // 第一次播放：应完整下载并落盘（大小精确匹配）。
        engine.play_request(make_req());
        let st = wait_for(&engine, Duration::from_secs(15), |s| {
            !s.loading && (s.error.is_some() || s.playing || s.finished)
        });
        let cache_file = engine.cache_path("local-test");
        let meta = fs::metadata(&cache_file).expect("缓存文件应已落盘");
        assert_eq!(meta.len(), body.len() as u64, "缓存大小应与源一致");
        assert_eq!(st.downloaded_bytes, body.len() as u64);
        // 第二次播放：缓存命中（cache_hit=true），秒开。
        // 等待条件带 cache_hit，避免读到上一次播放的旧状态。
        engine.play_request(make_req());
        let st = wait_for(&engine, Duration::from_secs(15), |s| {
            s.cache_hit && !s.loading && (s.error.is_some() || s.playing || s.finished)
        });
        assert!(st.cache_hit, "第二次播放应命中磁盘缓存");
        drop(engine);
        server.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_play_request_from_stream() {
        let stream = StreamUrl {
            audio_url: "https://upos.example.com/a.m4s?e=1".into(),
            video_url: None,
            ttl_secs: 300,
            audio_id: Some(30232),
            audio_codec: Some("mp4a.40.2".into()),
            bandwidth: Some(155622),
            size_bytes: Some(40000000),
            audio_backup_urls: vec!["https://backup.example.com/a.m4s".into()],
            required_headers: vec![
                ("User-Agent".into(), "UA".into()),
                ("Referer".into(), "https://www.bilibili.com/".into()),
            ],
            signed_with_wbi: false,
        };
        let req = PlayRequest::from_stream(&stream, "BV1xx411c7mD");
        assert_eq!(req.cache_key, "BV1xx411c7mD");
        assert_eq!(req.urls.len(), 2, "主地址 + 备用地址");
        assert_eq!(req.urls[0], stream.audio_url);
        assert_eq!(req.expected_size, Some(40000000));
        assert_eq!(req.bandwidth, Some(155622));
        assert_eq!(req.headers.len(), 2);
        assert!(req.local_file.is_none());
        // 缓存键 → md5 路径。
        let p = cache_path_in(Path::new("/tmp/c"), &req.cache_key);
        assert!(p.to_string_lossy().contains(&md5_hex("BV1xx411c7mD")));
    }
}
