//! 标题清洗的低层文本工具：书名号提取、括号注释剥离、分隔符清理、
//! 规范化（sanitize_preserving_case）与 Levenshtein 相似度。
//!
//! 上层语义见 [`super::query`]（查询生成）与 [`super::matching`]（打分）。

pub(super) fn extract_book_core(s: &str) -> Option<String> {
    let start = s.find('《')?;
    let from = &s[start..];
    let end = from.find('》')? + start;
    let inner = s[start + '《'.len_utf8()..end].trim();
    if inner.is_empty() {
        None
    } else {
        Some(inner.to_string())
    }
}

/// 移除全部 `open…close` 成对分组（嵌套不支持，一次删最内层并循环）。
pub(super) fn strip_groups(s: &str, open: char, close: char) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        if chars[i] == open {
            let mut j = i + 1;
            while j < n && chars[j] != close {
                j += 1;
            }
            if j < n {
                i = j + 1; // 删除整组
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// 处理 `(…)`/`（…）`：注释组删掉；非注释组保留内部文字、仅去括号。
pub(super) fn strip_annotation_parens(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        if c == '(' || c == '（' {
            let close = if c == '(' { ')' } else { '）' };
            let mut j = i + 1;
            while j < n && chars[j] != close {
                j += 1;
            }
            if j < n {
                let inner: String = chars[i + 1..j].iter().collect();
                if is_annotation(&inner) {
                    i = j + 1; // 删整组
                    continue;
                }
                for &cc in &chars[i + 1..j] {
                    out.push(cc); // 保留文字
                }
                i = j + 1;
                continue;
            }
            out.push(c);
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// 判断括号内容是否为注释（关键词或全大写短标记）。
fn is_annotation(inner: &str) -> bool {
    let l = inner.trim().to_lowercase();
    if l.is_empty() {
        return true;
    }
    const KW: &[&str] = &[
        "mv", "music video", "official", "ost", "op", "ed", "tv", "tvsize", "tv size",
        "size", "1080p", "4k", "高清", "官方", "现场", "完整", "翻唱", "cover", "歌词",
        "伴奏", "preview", "预告", "teaser", "ver", "version", "live", "remix", "lyric",
        "lyrics", "karaoke", "piano", "tvas", "pv", "sp", "fllv", "字幕", "合唱",
    ];
    if KW.iter().any(|k| l.contains(k)) {
        return true;
    }
    // 全大写短标记如 "MV" "OST" "4K" "TV"。
    l.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()) && l.chars().count() <= 6
}

/// 去掉尾部 ` - 艺术家` 之类的分隔后缀（保留左侧主体）。
pub(super) fn strip_trailing_separator(s: &str) -> String {
    const SEPS: &[&str] = &[" - ", " — ", " – ", " | ", " ｜ ", " : ", " / ", " · ", " ・ "];
    let mut best: Option<usize> = None;
    for sep in SEPS {
        if let Some(pos) = s.find(sep) {
            best = Some(best.map_or(pos, |b| b.min(pos)));
        }
    }
    if let Some(pos) = best {
        let before = s[..pos].trim();
        if !before.is_empty() {
            return before.to_string();
        }
    }
    s.to_string()
}

/// 保留大小写、仅剥离注释符号的标题（生成第二种查询用）。
pub fn sanitize_preserving_case(title: &str) -> String {
    let mut t = title.trim().to_string();
    t = strip_groups(&t, '【', '】');
    t = strip_groups(&t, '[', ']');
    t = strip_annotation_parens(&t);
    t = strip_trailing_separator(&t);
    for ch in ['《', '》', '「', '」', '『', '』', '〈', '〉', '"', '\'', '“', '”'] {
        t = t.replace(ch, "");
    }
    let t = collapse_ws(&t);
    t.trim().to_string()
}

/// 只保留字母数字与空格（去掉其余标点），用于 bare 关键词查询。
pub(super) fn strip_punctuation(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
}

/// 折叠连续空白为一个空格。
pub(super) fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 字符级编辑距离（O(n·m)）。
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 归一化相似度 `0.0..=1.0`。
pub fn lev_similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let d = levenshtein(a, b);
    let max = a.chars().count().max(b.chars().count());
    1.0 - (d as f64 / max as f64)
}

// ===========================================================================
// 测试
// ===========================================================================


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_levenshtein_similarity() {
        assert!(lev_similarity("晴天", "晴天") > 0.99);
        // 两字之差 1，相似度较低但不为 0。
        let s = lev_similarity("晴天", "阴天");
        assert!(s > 0.4 && s < 0.6, "got {s}");
        assert_eq!(lev_similarity("", ""), 1.0);
        assert_eq!(lev_similarity("", "abc"), 0.0);
    }
}
