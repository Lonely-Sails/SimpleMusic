//! symphonia 解码源（rodio::Source）：`SymphoniaSource` 从磁盘文件或内存缓冲
//! 拉取样本，支持 seek（基准 ms + 已输出帧数推算 position）与流结束标记。

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

#[derive(Clone, Debug)]
pub(super) enum MediaInput {
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
pub(super) struct SourceShared {
    /// 待处理的 seek 请求（秒）。播放线程写入，解码源在拉取样本前消费。
    pub(super) seek: Mutex<Option<f64>>,
    /// seek 基准（毫秒）：position = base_ms/1000 + emitted/sample_rate。
    pub(super) base_ms: AtomicU64,
    /// 自上次 seek 起已输出的帧数（解码源写入，播放线程轮询）。
    pub(super) emitted: AtomicU64,
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
pub(super) struct SymphoniaSource {
    input: MediaInput,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    time_base: Option<symphonia::core::units::TimeBase>,
    /// 当前解码出的 i16 交错样本及读取位置。
    pending: Vec<i16>,
    pos: usize,
    channels: usize,
    pub(super) sample_rate: u32,
    /// 容器时长估计（秒）。
    pub(super) duration_secs: Option<f64>,
    pub(super) shared: Arc<SourceShared>,
    /// seek 粗定位后需要丢弃的帧数（把 packet 边界对齐到目标时间）。
    skip_frames: u64,
    /// 自上次 seek 起累计输出帧数（镜像到 shared.emitted）。
    out_frames: u64,
    eos: bool,
}

/// 打开媒体源并 probe：返回（FormatReader, Decoder, track_id, time_base, 采样率,
/// 声道数, 时长估计）。
#[allow(clippy::type_complexity)]
pub(super) fn open_media(
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
    pub(super) fn new(input: MediaInput) -> Result<Self, String> {
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;
    use rodio::Source as _;

    // ---- 工具：手写 PCM WAV（不引入第三方依赖） ----

    pub(crate) fn wav_bytes(samples: &[i16], channels: u16, rate: u32) -> Vec<u8> {
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
    pub(crate) fn synth_wav(dir: &Path, name: &str, secs: f64, rate: u32) -> (PathBuf, u64, u32, u16) {
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
    pub(crate) fn test_dir(tag: &str) -> PathBuf {
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

}
