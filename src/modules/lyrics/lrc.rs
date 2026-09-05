//! LRC 文本解析与按播放位置的同步查找（纯函数）。

use super::LrcLine;

/// 解析 LRC 文本成按 `time_secs` 升序排列的时间轴。
///
/// 支持：
/// - 时间标签 `[mm:ss.xx]` / `[mm:ss.xxx]`（十进制分隔符 `.` 或 `,`，秒可 1~2 位）
/// - 一行多个时间标签（该句在其中每个时刻各出现一次）
/// - BOM（`\u{feff}`）与 CRLF（`\r\n`）
/// - 元数据标签 `[ti:][ar:][al:][by:][au:][offset:]` 等：不报错、不进入正文
/// - `[offset:±N]`（毫秒，正负）作用到所有时间戳（正号 = 时间后移，句更晚出现）
/// - 无时间标签的行忽略
///
/// 若 `offset` 把某个时间戳推到负值，则钳制为 0。
pub fn parse(lrc: &str) -> Vec<LrcLine> {
    let lrc = lrc.trim_start_matches('\u{feff}');
    let offset_ms = find_offset(lrc);
    let mut out: Vec<LrcLine> = Vec::new();
    for raw in lrc.split('\n') {
        let line = raw.trim_end_matches('\r');
        let (times, text) = parse_lrc_line(line);
        if times.is_empty() {
            continue; // 无时间标签的行忽略
        }
        for t in times {
            let shifted = (t + offset_ms as f64 / 1000.0).max(0.0);
            out.push(LrcLine {
                time_secs: shifted,
                text: text.clone(),
            });
        }
    }
    // 源 LRC 可能乱序；稳定排序保证时间轴单调（同时间保持相对顺序）。
    out.sort_by(|a, b| {
        a.time_secs
            .partial_cmp(&b.time_secs)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// 当前播到哪一句：返回最后一个时间 `<= pos_secs` 的行的下标（二分查找）。
///
/// 语义约定：
/// - 空输入 → `None`。
/// - `pos_secs` 早于第一句时间（前奏）→ 返回 `Some(0)`（UI 可借此做"前奏"判断，
///   见模块文档）。这是**钳制**行为，不是"没有当前句"。
/// - 其余情况返回最后一个满足 `time_secs <= pos_secs` 的下标。
///
/// 要求传入已按 `time_secs` 升序排序的切片（`lrc::parse` 的输出即满足）。
pub fn current_line_index(lines: &[LrcLine], pos_secs: f64) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    let idx = lines.partition_point(|l| l.time_secs <= pos_secs);
    if idx == 0 {
        // pos 早于第一句（或恰好等于第一句时间）。
        Some(0)
    } else {
        Some(idx - 1)
    }
}

/// 当前句的引用（未越过任何句/为空时返回 `None`；前奏时返回第一句，见
/// [`current_line_index`]）。
pub fn current_line(lines: &[LrcLine], pos_secs: f64) -> Option<&LrcLine> {
    current_line_index(lines, pos_secs).and_then(|i| lines.get(i))
}

/// 下一句（尚未播放的）：返回第一个时间 **严格大于** `pos_secs` 的行。
///
/// - 此后无行 → `None`。
/// - 前奏/越过所有行前的场景返回下一句，供"下一句预览"。
/// - 若 `pos_secs` 恰好等于某句时间，则返回那一句的**下**一句（该句已算当前）。
pub fn next_line(lines: &[LrcLine], pos_secs: f64) -> Option<&LrcLine> {
    let idx = lines.partition_point(|l| l.time_secs <= pos_secs);
    lines.get(idx)
}

/// 提取一行的正文（去掉 `[mm:ss]` 时间/元数据标签），用于生成纯文本歌词。
pub fn plain_line(line: &str) -> String {
    let (_, text) = parse_lrc_line(line);
    text
}

/// 取整段 LRC 的全局 `[offset:±N]`（毫秒）；无则 0。
fn find_offset(lrc: &str) -> i64 {
    for raw in lrc.split('\n') {
        let line = raw.trim();
        if line.starts_with('[') {
            if let Some(close) = line.find(']') {
                let inner = &line[1..close];
                if let Some(off) = parse_offset_tag(inner) {
                    return off;
                }
            }
        }
    }
    0
}

/// 解析单个时间标签内容（不含方括号），如 `"02:15.30"`、`"2:05,5"`。
fn parse_time_tag(s: &str) -> Option<f64> {
    let s = s.trim();
    let colon = s.find(':')?;
    if colon == 0 || colon + 1 >= s.len() {
        return None;
    }
    let mins: f64 = s[..colon].trim().parse().ok()?;
    let rest = &s[colon + 1..];
    let (sec_part, frac_part) = if let Some(dot) = rest.find('.') {
        (&rest[..dot], &rest[dot + 1..])
    } else if let Some(com) = rest.find(',') {
        (&rest[..com], &rest[com + 1..])
    } else {
        (rest, "")
    };
    let secs: f64 = sec_part.trim().parse().ok()?;
    let frac = if frac_part.is_empty() {
        0.0
    } else {
        frac_part.trim().parse::<f64>().ok()? / 10f64.powi(frac_part.len() as i32)
    };
    Some(mins * 60.0 + secs + frac)
}

/// 解析 `offset:±N`（内容不含方括号）。
fn parse_offset_tag(s: &str) -> Option<i64> {
    let low = s.trim().to_lowercase();
    if !low.starts_with("offset") {
        return None;
    }
    let rest = low[6..].strip_prefix(':')?;
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    let (sign, digits) = if let Some(d) = rest.strip_prefix('-') {
        (-1i64, d.trim())
    } else if let Some(d) = rest.strip_prefix('+') {
        (1i64, d.trim())
    } else {
        (1i64, rest)
    };
    Some(sign * digits.parse::<i64>().ok()?)
}

/// 该方括号内容是否为已知元数据标签 `key:...`。
fn is_metadata_tag(inner: &str) -> bool {
    let low = inner.trim().to_lowercase();
    const KEYS: &[&str] = &["ti", "ar", "al", "by", "au", "length", "re", "ve", "tool", "offset"];
    KEYS.iter()
        .any(|k| low.strip_prefix(k).map_or(false, |r| r.starts_with(':')))
}

/// 解析一行：返回 (所有时间标签, 剥离标签后的正文)。
/// 未识别的方括号（既非时间也非元数据）视作正文保留。
fn parse_lrc_line(line: &str) -> (Vec<f64>, String) {
    let mut times: Vec<f64> = Vec::new();
    let mut rest = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i] == '[' {
            let mut j = i + 1;
            while j < n && chars[j] != ']' {
                j += 1;
            }
            if j < n {
                let inner: String = chars[i + 1..j].iter().collect();
                if let Some(t) = parse_time_tag(&inner) {
                    times.push(t);
                    i = j + 1;
                    continue;
                }
                if parse_offset_tag(&inner).is_some() || is_metadata_tag(&inner) {
                    i = j + 1; // 剥离元数据/offset（全局 offset 已由 find_offset 处理）
                    continue;
                }
                // 未识别标签：当作正文的 '['，回退 1 字符。
                rest.push(chars[i]);
                i += 1;
            } else {
                rest.push(chars[i]);
                i += 1;
            }
        } else {
            rest.push(chars[i]);
            i += 1;
        }
    }
    (times, rest.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;


    #[test]
    fn parse_multi_timestamp_one_line() {
        let lrc = "[00:10.00][00:20.00]重复的句";
        let lines = parse(lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_secs, 10.0);
        assert_eq!(lines[1].time_secs, 20.0);
        assert_eq!(lines[0].text, "重复的句");
        assert_eq!(lines[1].text, "重复的句");
    }

    #[test]
    fn parse_bom_and_crlf() {
        let lrc = "\u{feff}[00:01.00]第一行\r\n[00:02.00]第二行\r\n";
        let lines = parse(lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_secs, 1.0);
        assert_eq!(lines[0].text, "第一行");
        assert_eq!(lines[1].time_secs, 2.0);
        assert_eq!(lines[1].text, "第二行");
    }

    #[test]
    fn parse_offset_positive_shifts_later() {
        let lrc = "[offset:+500]\n[00:10.00]a";
        let lines = parse(lrc);
        assert_eq!(lines.len(), 1);
        assert!((lines[0].time_secs - 10.5).abs() < 1e-9, "got {}", lines[0].time_secs);
    }

    #[test]
    fn parse_offset_negative_shifts_earlier_and_clamps() {
        // 负偏移把 10s 推到 9.8s。
        let lrc = "[offset:-200]\n[00:10.00]a";
        let lines = parse(lrc);
        assert!((lines[0].time_secs - 9.8).abs() < 1e-9);
        // 大负偏移把 10s 推到负值 → 钳制为 0。
        let lrc2 = "[offset:-11000]\n[00:10.00]a";
        let lines2 = parse(lrc2);
        assert_eq!(lines2[0].time_secs, 0.0);
    }

    #[test]
    fn parse_metadata_tags_ignored() {
        let lrc = "[ti:标题][ar:歌手][al:专辑][by:制作]\n[offset:0]\n[00:03.00]歌词";
        let lines = parse(lrc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "歌词");
        assert_eq!(lines[0].time_secs, 3.0);
    }

    #[test]
    fn parse_plain_text_without_timestamps_ignored() {
        let lrc = "这是一行没有时间标签的歌词\n[00:05.00]有标签的";
        let lines = parse(lrc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "有标签的");
    }

    #[test]
    fn parse_fraction_dot_and_comma() {
        let lrc = "[00:01.5]a\n[00:02,50]b\n[00:02.120]c";
        let lines = parse(lrc);
        assert_eq!(lines[0].time_secs, 1.5);
        assert_eq!(lines[0].text, "a");
        // 排序后：[00:01.5] < [00:02.120] < [00:02,50]。
        assert!((lines[1].time_secs - 2.120).abs() < 1e-9);
        assert_eq!(lines[1].text, "c");
        assert_eq!(lines[2].time_secs, 2.5);
        assert_eq!(lines[2].text, "b");
    }

    // ---- 同步引擎 ----

    #[test]
    fn current_line_breakpoints() {
        let lines = parse("[00:01.00]a\n[00:03.00]b\n[00:05.00]c\n[00:07.00]d");
        // pos 在 3 与 5 之间 → 上一句是 3(b)，下标 1。
        assert_eq!(current_line_index(&lines, 4.0), Some(1));
        // 恰好等于某句 → 那一句。
        assert_eq!(current_line_index(&lines, 3.0), Some(1));
        // 越过最后一句 → 最后一句。
        assert_eq!(current_line_index(&lines, 100.0), Some(3));
        // 等于第一句。
        assert_eq!(current_line_index(&lines, 1.0), Some(0));
    }

    #[test]
    fn current_line_prelude_returns_first() {
        let lines = parse("[00:03.00]a\n[00:05.00]b");
        // pos 早于第一句（前奏）→ 钳制为 0。
        assert_eq!(current_line_index(&lines, 0.5), Some(0));
        assert_eq!(current_line(&lines, 0.5).map(|l| l.text.as_str()), Some("a"));
    }

    #[test]
    fn next_line_and_empty() {
        let lines = parse("[00:01.00]a\n[00:03.00]b\n[00:05.00]c");
        // pos=4 → 下一句是 5(c)，下标 2。
        assert_eq!(next_line(&lines, 4.0).map(|l| l.text.as_str()), Some("c"));
        // 前奏 → 第一句是下一句。
        assert_eq!(next_line(&lines, 0.0).map(|l| l.text.as_str()), Some("a"));
        // 恰等于 b(3) → 下一句是 c(5)。
        assert_eq!(next_line(&lines, 3.0).map(|l| l.text.as_str()), Some("c"));
        // 越过最后一句 → None。
        assert_eq!(next_line(&lines, 100.0), None);
    }

    #[test]
    fn sync_engine_empty_input() {
        let empty: Vec<LrcLine> = Vec::new();
        assert_eq!(current_line_index(&empty, 5.0), None);
        assert_eq!(current_line(&empty, 5.0), None);
        assert_eq!(next_line(&empty, 5.0), None);
    }
}
