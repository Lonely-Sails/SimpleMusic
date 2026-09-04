//! 时长格式化。

/// 秒数格式化为 `MM:SS`（超 1 小时也显示 `MM:SS`，如 `60:30`）。
pub fn format_secs(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_secs_pads() {
        assert_eq!(format_secs(0.0), "00:00");
        assert_eq!(format_secs(65.4), "01:05");
        assert_eq!(format_secs(3630.0), "60:30");
    }
}
