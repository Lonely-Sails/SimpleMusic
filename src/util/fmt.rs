//! 时长 / 字节数格式化。

/// 秒数格式化为 `MM:SS`（超 1 小时也显示 `MM:SS`，如 `60:30`）。
pub fn format_secs(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

/// 字节数格式化为带单位的字符串（B / KB / MB / GB）。
pub fn format_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
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

    #[test]
    fn format_bytes_rounds() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}