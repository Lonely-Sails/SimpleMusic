//! 歌词同步：把当前播放进度映射为「当前句 / 下一句」。

use crate::modules::lyrics;

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pick_plain_line_index_clamped() {
        let plain = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert_eq!(pick_plain_line_index(&plain, 0.0), 0);
        assert_eq!(pick_plain_line_index(&plain, 0.9), 2);
        assert_eq!(pick_plain_line_index(&plain, 1.5), 2);
        assert_eq!(pick_plain_line_index(&plain, -1.0), 0);
        assert_eq!(pick_plain_line_index(&[], 0.5), 0);
    }
}