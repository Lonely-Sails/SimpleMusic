//! 音频磁盘缓存：路径规则（`<dir>/<md5(key)>.m4s`）与命中判定。

use std::fs;
use std::path::{Path, PathBuf};

use crate::modules::bilibili::md5_hex;

pub fn default_cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join("simple-music").join("audio")
}

/// 缓存文件路径：`<cache_dir>/<md5(cache_key)>.m4s`。
/// 纯函数（不触盘），供引擎与测试使用。
pub fn cache_path_in(dir: &Path, cache_key: &str) -> PathBuf {
    dir.join(format!("{}.m4s", md5_hex(cache_key)))
}

/// 缓存命中判定：期望大小已知时必须严格等于文件长度；未知时只接受 >1KB 的文件
/// （防止空文件/残页被当成有效缓存）。
pub(super) fn cache_usable(path: &Path, expected_size: Option<u64>) -> bool {
    match fs::metadata(path) {
        Ok(m) if m.is_file() => match expected_size {
            Some(want) => m.len() == want,
            None => m.len() > 1024,
        },
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 共享播放状态
// ---------------------------------------------------------------------------


#[cfg(test)]
mod tests {
    use super::*;
    use super::super::decode::tests::test_dir;

    #[test]
    fn test_cache_path_deterministic_and_distinct() {
        let dir = PathBuf::from("/tmp/cache-x");
        let p1 = cache_path_in(&dir, "BV1xx411c7mD");
        let p2 = cache_path_in(&dir, "BV1xx411c7mD");
        let p3 = cache_path_in(&dir, "BV1GJ411x7h7");
        assert_eq!(p1, p2, "同键同路径");
        assert_ne!(p1, p3, "不同键不同路径");
        assert_eq!(p1.extension().and_then(|e| e.to_str()), Some("m4s"));
        assert_eq!(
            p1.file_name().and_then(|n| n.to_str()),
            Some(format!("{}.m4s", md5_hex("BV1xx411c7mD")).as_str())
        );
    }

    #[test]
    fn test_cache_usable_rules() {
        let dir = test_dir("cache");
        let p = dir.join("x.m4s");
        fs::write(&p, vec![0u8; 100]).unwrap();
        assert!(!cache_usable(&p, Some(200)), "大小不匹配 → 不可用");
        assert!(!cache_usable(&p, None), "过小(<1KB)且无期望大小 → 不可用");
        fs::write(&p, vec![0u8; 2048]).unwrap();
        assert!(cache_usable(&p, Some(2048)), "大小匹配 → 可用");
        assert!(cache_usable(&p, None), "无期望大小且 >1KB → 可用");
        assert!(!cache_usable(&dir.join("missing.m4s"), None), "不存在 → 不可用");
        let _ = fs::remove_dir_all(&dir);
    }
}
