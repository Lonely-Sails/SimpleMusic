//! 响度均衡（音量归一化）：**整曲预分析 → 固定增益**。
//!
//! 不同音源/不同视频的录音电平差异很大，切歌时音量忽大忽小。本模块在**开始播放前**
//! 完整扫一遍解码流，算出整曲平均响度（RMS dBFS），据此推导一个固定线性增益，
//! 让每首歌的输出响度收敛到同一目标；播放期间增益恒定，不做动态压缩
//! （无抽气效应，也不改动动态范围）。
//!
//! 设计要点：
//! - 分析在**播放线程**上同步执行（`player::load_and_play` 内），期间 UI 通过
//!   `PlaybackStatus.normalizing` 显示「响度分析中」；分析可被新命令打断
//!   （复用下载同款 `poll_abort`），不会卡住切歌。
//! - 增益上下限 [`MIN_GAIN_DB`] / [`MAX_GAIN_DB`]：只做温和修正，不把录音电平
//!   极低的曲子放大到爆音，也不把本来很响的曲子压得没有动态。
//! - 静音/近静音（低于 [`SILENCE_DBFS`]）判定为无有效音频，增益恒为 1.0，
//!   避免把底噪放大成刺耳噪声。
//! - 应用增益时**硬限幅**（clamp 到 i16 范围），放大导致的削波以削顶换不失真溢出。

use symphonia::core::audio::SampleBuffer;
use symphonia::core::errors::Error as SymphError;

use super::decode::{MediaInput, open_media};

/// 目标响度（RMS dBFS）。低于此值的曲子放大，高于此值的衰减。
///
/// 取 -18 dBFS：接近流行音乐母带的常规 RMS 电平，留出足够的峰值余量，
/// 放大后不易削波。
pub(super) const TARGET_RMS_DBFS: f64 = -18.0;
/// 增益上限（dB）：限制最大放大倍数（约 4 倍），防止底噪被拉爆。
pub(super) const MAX_GAIN_DB: f64 = 12.0;
/// 增益下限（dB）：限制最大衰减倍数。
pub(super) const MIN_GAIN_DB: f64 = -12.0;
/// 静音判定阈值（dBFS）：整曲响度低于此值视为无有效音频，增益为 1.0。
const SILENCE_DBFS: f64 = -60.0;

/// 响度分析失败类型。
#[derive(Debug)]
pub(super) enum AnalyzeErr {
    /// 被新命令打断（Stop / 新的 Play / 退出）。
    Aborted,
    Failed(String),
}

/// 整曲响度分析结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Loudness {
    /// 整曲平均响度（RMS dBFS）；无有效音频时为 `f64::NEG_INFINITY`。
    pub(super) rms_dbfs: f64,
}

impl Loudness {
    /// 由分析结果推导线性增益（1.0 = 不变）。
    pub(super) fn gain(&self) -> f32 {
        if !self.rms_dbfs.is_finite() || self.rms_dbfs < SILENCE_DBFS {
            return 1.0;
        }
        let gain_db = (TARGET_RMS_DBFS - self.rms_dbfs).clamp(MIN_GAIN_DB, MAX_GAIN_DB);
        10f64.powf(gain_db / 20.0) as f32
    }
}

/// 完整扫描媒体流，累计样本平方和得到整曲平均响度（RMS dBFS）。
///
/// `abort` 每解码一个包调用一次，返回 true 立即放弃分析（切歌/退出时不白等）。
pub(super) fn analyze(
    input: &MediaInput,
    abort: &dyn Fn() -> bool,
) -> Result<Loudness, AnalyzeErr> {
    let (mut format, mut decoder, track_id, _time_base, _rate, _channels, _dur) =
        open_media(input).map_err(AnalyzeErr::Failed)?;
    let mut sum_sq = 0.0f64;
    let mut frames = 0u64;
    loop {
        if abort() {
            return Err(AnalyzeErr::Aborted);
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            // 正常流结束（EOF）与其余错误一律按「分析到此为止」处理：
            // 已统计到的部分足够给出响度估计，不必因尾部坏包丢弃整次分析。
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // 坏包跳过（与解码源同策略）。
            Err(SymphError::DecodeError(_)) => continue,
            Err(_) => break,
        };
        let spec = *decoded.spec();
        let ch = spec.channels.count().max(1);
        let mut sbuf = SampleBuffer::<i16>::new(decoded.capacity() as u64, spec);
        sbuf.copy_interleaved_ref(decoded);
        let samples = sbuf.samples();
        // 逐帧累计：每帧先跨声道求均值平方，再累加——声道数不同的曲子
        // 也能得到可比的平均响度。
        for frame in samples.chunks_exact(ch) {
            let mut acc = 0.0f64;
            for &s in frame {
                let v = s as f64 / 32768.0;
                acc += v * v;
            }
            sum_sq += acc / ch as f64;
        }
        frames += (samples.len() / ch) as u64;
    }
    if frames == 0 {
        return Err(AnalyzeErr::Failed("无有效音频帧，无法分析响度".into()));
    }
    let mean_sq = sum_sq / frames as f64;
    let rms_dbfs = if mean_sq <= 0.0 {
        f64::NEG_INFINITY
    } else {
        10.0 * mean_sq.log10()
    };
    Ok(Loudness { rms_dbfs })
}

/// 固定增益包络：对 i16 样本乘增益并硬限幅（防放大后溢出成爆音）。
pub(super) struct NormalizeSource<S> {
    inner: S,
    gain: f32,
}

impl<S> NormalizeSource<S> {
    pub(super) fn new(inner: S, gain: f32) -> Self {
        Self { inner, gain }
    }
}

impl<S: rodio::Source<Item = i16>> Iterator for NormalizeSource<S> {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        self.inner.next().map(|s| {
            let v = (s as f32 * self.gain).round();
            v.clamp(i16::MIN as f32, i16::MAX as f32) as i16
        })
    }
}

impl<S: rodio::Source<Item = i16>> rodio::Source for NormalizeSource<S> {
    fn current_frame_len(&self) -> Option<usize> {
        self.inner.current_frame_len()
    }

    fn channels(&self) -> u16 {
        self.inner.channels()
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        self.inner.total_duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::audio::decode::tests::{synth_wav, test_dir};

    #[test]
    fn test_gain_from_loudness_clamps_and_targets() {
        // 响度正好等于目标 → 增益 1.0。
        let g = Loudness {
            rms_dbfs: TARGET_RMS_DBFS,
        }
        .gain();
        assert!((g - 1.0).abs() < 1e-3, "目标响度增益应为 1，实得 {g}");
        // 过小的响度（但未到静音）→ 正增益且不超上限。
        let quiet = Loudness { rms_dbfs: -80.0 }.gain();
        let max_gain = 10f64.powf(MAX_GAIN_DB / 20.0) as f32;
        assert!(quiet <= max_gain + 1e-4, "正增益不应超过上限 {max_gain}");
        // 过大的响度 → 衰减且不低于下限。
        let loud = Loudness { rms_dbfs: 0.0 }.gain();
        let min_gain = 10f64.powf(MIN_GAIN_DB / 20.0) as f32;
        assert!(loud >= min_gain - 1e-4, "衰减不应低于下限 {min_gain}");
        assert!(loud < 1.0);
    }

    #[test]
    fn test_silence_and_nonfinite_gain_is_unity() {
        // 近静音 → 不放大底噪。
        assert_eq!(Loudness { rms_dbfs: -90.0 }.gain(), 1.0);
        // 无有效音频（-inf）→ 1.0。
        assert_eq!(
            Loudness {
                rms_dbfs: f64::NEG_INFINITY
            }
            .gain(),
            1.0
        );
    }

    #[test]
    fn test_analyze_reports_finite_rms_for_tone() {
        let dir = test_dir("normalize-tone");
        // 1 秒 440Hz 正弦，幅度 12000/32768 ≈ -8.7 dBFS。
        let (path, _frames, _rate, _ch) = synth_wav(&dir, "tone.wav", 1.0, 8000);
        let input = MediaInput::File(path);
        let loud = analyze(&input, &|| false).expect("分析应成功");
        assert!(loud.rms_dbfs.is_finite(), "正弦波响度应有限");
        // 正弦波 RMS = 幅度/√2 → -8.7 - 3.0 ≈ -11.7 dBFS，给宽裕容差。
        assert!(
            (loud.rms_dbfs - (-11.7)).abs() < 1.5,
            "实测响度 {} dBFS 应接近 -11.7",
            loud.rms_dbfs
        );
        // 响度高于目标 → 应衰减。
        assert!(loud.gain() < 1.0, "高响度应衰减");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_analyze_aborts_when_requested() {
        let dir = test_dir("normalize-abort");
        let (path, _f, _r, _c) = synth_wav(&dir, "tone.wav", 1.0, 8000);
        let input = MediaInput::File(path);
        let err = analyze(&input, &|| true).err();
        assert!(
            matches!(err, Some(AnalyzeErr::Aborted)),
            "abort 应立即中断分析"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_normalize_source_applies_gain_and_clamps() {
        /// 测试用最简 rodio 源：把一段 i16 样本按固定参数播放。
        struct VecSource {
            it: std::vec::IntoIter<i16>,
        }
        impl Iterator for VecSource {
            type Item = i16;
            fn next(&mut self) -> Option<i16> {
                self.it.next()
            }
        }
        impl rodio::Source for VecSource {
            fn current_frame_len(&self) -> Option<usize> {
                None
            }
            fn channels(&self) -> u16 {
                1
            }
            fn sample_rate(&self) -> u32 {
                8000
            }
            fn total_duration(&self) -> Option<std::time::Duration> {
                None
            }
        }
        let inner = VecSource {
            it: vec![1000i16, -1000, 30000, -30000, 0].into_iter(),
        };
        let src = NormalizeSource::new(inner, 2.0);
        let out: Vec<i16> = src.collect();
        assert_eq!(out[0], 2000);
        assert_eq!(out[1], -2000);
        assert_eq!(out[2], i16::MAX, "放大后正向应限幅");
        assert_eq!(out[3], i16::MIN, "放大后负向应限幅");
        assert_eq!(out[4], 0);
    }
}
