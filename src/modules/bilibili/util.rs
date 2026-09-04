//! 模块内纯函数工具：音质选择、BV token 扫描、Set-Cookie 解析、
//! 收藏夹去重、URL query 解析（不触网络，全部带单测或被单测覆盖）。

use super::models::{DashStream, FavFolder};
use crate::state::AudioQuality;


/// 按音质偏好从 DASH 音频流中选择。
///
/// 优先精确匹配偏好 id；未命中时低/中档取最接近目标码率的流，高档取最高码率。
/// 无损偏好（Lossless）依次尝试 FLAC (30255) → Dolby (30250/30251) → 最高码率。
pub fn pick_dash_audio<'a>(audio: &'a [DashStream], quality: AudioQuality) -> Option<&'a DashStream> {
    if audio.is_empty() {
        return None;
    }
    let preferred_ids: Vec<i64> = match quality {
        AudioQuality::Low => vec![30216],
        AudioQuality::Medium => vec![30232],
        AudioQuality::High => vec![30280],
        AudioQuality::Lossless => vec![30255, 30250, 30251],
    };
    for id in preferred_ids {
        if let Some(s) = audio.iter().find(|s| s.id == id) {
            return Some(s);
        }
    }
    // 未命中偏好：低/中档取最接近目标码率的流，其余取最高码率。
    let target_bandwidth = match quality {
        AudioQuality::Low => 64_000,
        AudioQuality::Medium => 128_000,
        _ => i64::MAX,
    };
    audio.iter().min_by_key(|s| (s.bandwidth - target_bandwidth).abs())
}

/// 在文本里扫描 `BV + 10 位 [0-9A-Za-z]`，返回第一个匹配。
pub(super) fn scan_bv_token(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i + 12 <= n {
        if bytes[i] == b'B' && bytes[i + 1] == b'V' {
            let candidate = &s[i + 2..i + 12];
            if candidate
                .bytes()
                .all(|b| b.is_ascii_alphanumeric())
            {
                // BV 号第 1 位（总第 3 位）按现行规范是 1~7 之间的数字，用于排除
                // 恰好拼成 "BVxxx" 的普通单词（如 "BVDIRECTORY" 这类长词截断误判）。
                let c = candidate.as_bytes()[0];
                if (b'1'..=b'7').contains(&c) {
                    return Some(format!("BV{candidate}"));
                }
            }
        }
        i += 1;
    }
    None
}

/// 从 Set-Cookie 值里提取第一对 k=v。
pub(super) fn parse_set_cookie(set_cookie: &str) -> Option<(String, String)> {
    let first = set_cookie.split(';').next()?;
    let (k, v) = first.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), v.trim().to_string()))
}

/// 按 id 去重收藏夹（保留首个出现者）：created/collected 两路合并时的防御性去重。
pub(super) fn dedup_folders(folders: Vec<FavFolder>) -> Vec<FavFolder> {
    let mut seen = std::collections::HashSet::new();
    folders
        .into_iter()
        .filter(|f| seen.insert(f.id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dedup_folders_keeps_first() {
        let folders = vec![
            FavFolder { id: 555, title: "a".into(), media_count: 1 },
            FavFolder { id: 666, title: "b".into(), media_count: 2 },
            FavFolder { id: 555, title: "a2".into(), media_count: 9 },
        ];
        let out = dedup_folders(folders);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, 555);
        assert_eq!(out[0].title, "a", "应保留首个出现者");
        assert_eq!(out[1].id, 666);
    }

    #[test]
    fn test_parse_set_cookie() {
        let (k, v) = parse_set_cookie(
            "SESSDATA=abc%2Cdef; Path=/; Domain=.bilibili.com; Secure; HttpOnly; SameSite=None",
        )
        .unwrap();
        assert_eq!(k, "SESSDATA");
        assert_eq!(v, "abc%2Cdef");
        assert!(parse_set_cookie("invalid").is_none());
    }
}
