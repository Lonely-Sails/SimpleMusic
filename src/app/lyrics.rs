//! 歌词同步：把当前播放进度映射为「当前句 / 下一句」。

use crate::modules::lyrics::{self, LrcLine, Lyrics};

use super::MusicApp;

/// 无同步歌词时按播放进度近似取行：返回 `plain` 的下标（非空时必在界内）。
pub fn pick_plain_line_index(plain: &[String], progress: f64) -> usize {
    if plain.is_empty() {
        return 0;
    }
    let p = progress.clamp(0.0, 1.0);
    let idx = (p * plain.len() as f64) as usize;
    idx.min(plain.len() - 1)
}

/// 距下一次歌词行切换的秒数：LRC 用下一个时间戳，纯文本按进度均分推算。
/// 用于主窗口「定时在切换点醒来」——节流重绘的同时保证切行不延迟。
/// 无歌词或已到最后一行之后返回 `None`。
pub fn next_switch_delay_secs(
    lines: &[LrcLine],
    plain_len: usize,
    pos_secs: f64,
    duration_secs: f64,
) -> Option<f64> {
    if !lines.is_empty() {
        // LRC 行按时间升序（解析后已排序）；取第一个晚于当前进度的行。
        let idx = lines.partition_point(|l| l.time_secs <= pos_secs);
        let dt = lines.get(idx)?.time_secs - pos_secs;
        Some(dt.max(0.0))
    } else if plain_len > 0 && duration_secs > 0.0 {
        let idx = pick_plain_line_index(&[], 0.0).min(plain_len - 1);
        let _ = idx; // 切换点只取决于进度比例，无需当前下标
        let p = (pos_secs / duration_secs).clamp(0.0, 1.0);
        let idx = (p * plain_len as f64) as usize;
        let next_pos = (idx + 1).min(plain_len) as f64 / plain_len as f64 * duration_secs;
        Some((next_pos - pos_secs).max(0.0))
    } else {
        None
    }
}

impl MusicApp {
    /// 根据当前进度更新 `state.current_lrc_line` 与 `lyrics_next_line`。
    pub(crate) fn update_lyrics_line(&mut self) {
        let pos = self.state.position_secs;
        let dur = self.state.duration_secs;
        let (cur, next) = if !self.lyrics_lines.is_empty() {
            let cur = lyrics::lrc::current_line(&self.lyrics_lines, pos)
                .map(|l| l.text.clone())
                .unwrap_or_default();
            let next = lyrics::lrc::next_line(&self.lyrics_lines, pos)
                .map(|l| l.text.clone())
                .unwrap_or_default();
            (cur, next)
        } else if !self.lyrics_plain.is_empty() {
            let progress = if dur > 0.0 { pos / dur } else { 0.0 };
            let idx = pick_plain_line_index(&self.lyrics_plain, progress);
            let cur = self.lyrics_plain.get(idx).cloned().unwrap_or_default();
            let next = self
                .lyrics_plain
                .get(idx + 1)
                .cloned()
                .unwrap_or_default();
            (cur, next)
        } else {
            (self.state.title.clone(), String::new())
        };
        let prelude = !self.lyrics_lines.is_empty() && self.lyrics_lines[0].time_secs > pos;
        let current_line = if prelude {
            "前奏…".to_string()
        } else {
            cur.clone()
        };
        self.state.current_lrc_line = current_line;
        self.lyrics_next_line = if prelude { cur } else { next };
    }

    /// 应用一份歌词候选（歌词选择弹窗点选时调用）：重设当前歌词与时间轴/纯文本行。
    ///
    /// **用户显式手选是持久化时机**：把该候选写进歌词缓存的 `selected`
    /// （按当前曲 bvid 键控），下次播放同曲零网络直接生效；落盘在后台线程。
    pub(crate) fn apply_lyrics(&mut self, li: &Lyrics) {
        self.apply_lyrics_inner(li);
        if let Some(bvid) = self.current_bvid().map(|b| b.to_string()) {
            let cache = self.lyrics_cache.clone();
            let ly = li.clone();
            // 缓存表更新 + 落盘都在后台线程（磁盘 IO 不进 UI 线程）。
            std::thread::spawn(move || {
                if let Ok(mut m) = cache.lock() {
                    lyrics::cache_update_selected(&mut m, &bvid, ly);
                    let _ = crate::modules::storage::save_lyrics_cache(&m);
                }
            });
        }
    }

    /// 仅应用歌词（自动抓取回放路径用）：抓取线程已把结果写进缓存，
    /// 这里只更新 UI 状态，不再落盘（避免重复 IO）。
    pub(crate) fn apply_lyrics_only(&mut self, li: &Lyrics) {
        self.apply_lyrics_inner(li);
    }

    /// 应用歌词的公共部分：更新当前歌词与时间轴/纯文本行。
    fn apply_lyrics_inner(&mut self, li: &Lyrics) {
        self.current_lyrics = Some(li.clone());
        self.lyrics_lines = li.lrc_lines();
        self.lyrics_plain = li
            .plain
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self.update_lyrics_line();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::lyrics::LrcLine;

    #[test]
    fn pick_plain_line_index_clamped() {
        let plain = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(pick_plain_line_index(&plain, 0.0), 0);
        assert_eq!(pick_plain_line_index(&plain, 0.9), 2);
        assert_eq!(pick_plain_line_index(&plain, 1.5), 2);
        assert_eq!(pick_plain_line_index(&plain, -1.0), 0);
        assert_eq!(pick_plain_line_index(&[], 0.5), 0);
    }

    fn lrc(times: &[f64]) -> Vec<LrcLine> {
        times
            .iter()
            .enumerate()
            .map(|(i, t)| LrcLine { time_secs: *t, text: format!("line{i}") })
            .collect()
    }

    /// LRC：切换点 = 下一个时间戳；前奏（pos < 首行时间）覆盖首行切换；
    /// 最后一行之后无切换点。
    #[test]
    fn next_switch_delay_lrc() {
        let lines = lrc(&[10.0, 20.0, 30.0]);
        assert_eq!(next_switch_delay_secs(&lines, 0, 5.0, 100.0), Some(5.0));
        assert_eq!(next_switch_delay_secs(&lines, 0, 0.0, 100.0), Some(10.0));
        assert_eq!(next_switch_delay_secs(&lines, 0, 21.5, 100.0), Some(8.5));
        assert_eq!(next_switch_delay_secs(&lines, 0, 31.0, 100.0), None);
        // 越界进度（时钟略超时间戳）不产生负延迟。
        assert_eq!(next_switch_delay_secs(&lines, 0, 10.5, 100.0), Some(9.5));
    }

    /// 纯文本：按进度均分推算切换点；无歌词/无时长时 None。
    #[test]
    fn next_switch_delay_plain() {
        let plain_len = 4;
        // 4 行均分：切换点在 25/50/75/100%。
        assert_eq!(next_switch_delay_secs(&[], plain_len, 5.0, 100.0), Some(20.0));
        assert_eq!(next_switch_delay_secs(&[], plain_len, 30.0, 100.0), Some(20.0));
        assert_eq!(next_switch_delay_secs(&[], plain_len, 100.0, 100.0), Some(0.0));
        assert_eq!(next_switch_delay_secs(&[], 0, 5.0, 100.0), None);
        assert_eq!(next_switch_delay_secs(&[], plain_len, 5.0, 0.0), None);
    }
}