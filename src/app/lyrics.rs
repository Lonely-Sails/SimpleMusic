//! 歌词同步：把当前播放进度映射为「当前句 / 下一句」。

use crate::modules::lyrics::{self, LrcLine, Lyrics};

use super::MusicApp;

/// 歌词时间偏移的钳制范围（秒）：±60 秒足够覆盖常见的整曲时间差，
/// 同时避免误操作（或旧配置里的脏值）把时间轴推到离谱的位置。
pub const LYRICS_OFFSET_LIMIT_SECS: f64 = 60.0;

/// 把设置里的歌词偏移应用到 LRC 时间轴（纯函数）。
///
/// 语义：`offset` 是「歌词整体延迟量」，正 = 歌词更晚出现。
/// 因此把每行时间戳加上 `offset` 即可——同步时 `pos_secs >= time_secs + offset`
/// 才切到该行，正偏移自然表现为延后。
///
/// `offset == 0.0` 时原样返回（不克隆、不排序），避免无谓开销。
/// 负数把时间戳推到 0 以下时钳制为 0（与 `lrc::parse` 对 `[offset:]` 的处理一致）。
pub fn shift_lines(lines: &[LrcLine], offset: f64) -> Vec<LrcLine> {
    if offset == 0.0 || lines.is_empty() {
        return lines.to_vec();
    }
    let mut out = lines.to_vec();
    for line in &mut out {
        line.time_secs = (line.time_secs + offset).max(0.0);
    }
    // 平移是单调变换，顺序不会变，但负偏移钳制到 0 后可能产生并列时间戳，
    // 稳定排序保证同时间保持原相对顺序（与 parse 的约定一致）。
    out.sort_by(|a, b| {
        a.time_secs
            .partial_cmp(&b.time_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

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
            let next = self.lyrics_plain.get(idx + 1).cloned().unwrap_or_default();
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
        // 显示文本净化：剔除内嵌字体渲染不出的字符（emoji/PUA/零宽等），
        // 桌面歌词浮窗不再「?」满天飞（浮窗侧显示边界还有一道同样过滤，幂等）。
        self.state.current_lrc_line = crate::fonts::sanitize_text(&current_line);
        self.lyrics_next_line = crate::fonts::sanitize_text(&(if prelude { cur } else { next }));
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
            // 缓存表更新 + 落盘都在后台（磁盘 IO 不进 UI 线程）。
            crate::net::spawn(async move {
                crate::net::spawn_blocking(move || {
                    if let Ok(mut m) = cache.lock() {
                        lyrics::cache_update_selected(&mut m, &bvid, ly);
                        let _ = crate::modules::storage::save_lyrics_cache(&m);
                    }
                })
                .await
                .ok();
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
        // 时间轴按设置里的全局偏移平移后再参与同步（歌词原文不动）。
        self.lyrics_lines = shift_lines(&li.lrc_lines(), self.settings.lyrics_offset_secs);
        self.lyrics_plain = li
            .plain
            .lines()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self.update_lyrics_line();
    }

    /// 调节歌词时间偏移：`delta` 秒（正 = 歌词延后出现，负 = 提前）。
    ///
    /// 按 [`LYRICS_OFFSET_LIMIT_SECS`] 钳制后写进设置（随设置的每 5 秒兜底 +
    /// 退出保存落盘），并立即用新偏移重算时间轴与当前句——**不重新抓取歌词**，
    /// 只平移已有的时间轴，所以点击后当帧就能看到效果。
    ///
    /// 返回调整后的偏移量，供调用方拼提示文案。
    pub(crate) fn adjust_lyrics_offset(&mut self, delta: f64) -> f64 {
        let next = (self.settings.lyrics_offset_secs + delta)
            .clamp(-LYRICS_OFFSET_LIMIT_SECS, LYRICS_OFFSET_LIMIT_SECS);
        self.settings.lyrics_offset_secs = next;
        if let Some(li) = self.current_lyrics.clone() {
            self.lyrics_lines = shift_lines(&li.lrc_lines(), next);
        }
        self.update_lyrics_line();
        next
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
            .map(|(i, t)| LrcLine {
                time_secs: *t,
                text: format!("line{i}"),
            })
            .collect()
    }

    /// 偏移平移：正偏移把每行推后（切行更晚），负偏移提前，0 原样。
    #[test]
    fn shift_lines_moves_timeline() {
        let lines = lrc(&[10.0, 20.0]);
        assert_eq!(shift_lines(&lines, 0.0), lines);
        let later = shift_lines(&lines, 1.5);
        assert_eq!(later[0].time_secs, 11.5);
        assert_eq!(later[1].time_secs, 21.5);
        let earlier = shift_lines(&lines, -1.0);
        assert_eq!(earlier[0].time_secs, 9.0);
        assert_eq!(earlier[1].time_secs, 19.0);
        // 文本保持不变（只平移时间轴，不改歌词原文）。
        assert_eq!(earlier[0].text, "line0");
    }

    /// 负偏移把时间戳推到 0 以下时钳制为 0，并保持升序（同时间戳相对顺序不变）。
    #[test]
    fn shift_lines_clamps_to_zero_and_keeps_order() {
        let lines = lrc(&[0.5, 2.0, 3.0]);
        let out = shift_lines(&lines, -1.0);
        assert_eq!(
            out.iter().map(|l| l.time_secs).collect::<Vec<_>>(),
            vec![0.0, 1.0, 2.0]
        );
        let clamped = shift_lines(&lines, -10.0);
        assert_eq!(
            clamped.iter().map(|l| l.time_secs).collect::<Vec<_>>(),
            vec![0.0, 0.0, 0.0]
        );
        // 钳制后文本顺序仍是原顺序（稳定排序）。
        assert_eq!(
            clamped.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(),
            vec!["line0", "line1", "line2"]
        );
    }

    /// 空时间轴不 panic，任意偏移都返回空。
    #[test]
    fn shift_lines_empty() {
        assert!(shift_lines(&[], 1.0).is_empty());
        assert!(shift_lines(&[], 0.0).is_empty());
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
        assert_eq!(
            next_switch_delay_secs(&[], plain_len, 5.0, 100.0),
            Some(20.0)
        );
        assert_eq!(
            next_switch_delay_secs(&[], plain_len, 30.0, 100.0),
            Some(20.0)
        );
        assert_eq!(
            next_switch_delay_secs(&[], plain_len, 100.0, 100.0),
            Some(0.0)
        );
        assert_eq!(next_switch_delay_secs(&[], 0, 5.0, 100.0), None);
        assert_eq!(next_switch_delay_secs(&[], plain_len, 5.0, 0.0), None);
    }
}
