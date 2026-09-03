//! 歌曲搜索过滤（标题 / UP 主，不区分大小写）。

/// 歌曲标题/UP 主匹配查询（不区分大小写）。空查询恒匹配。
pub fn song_matches_query(title: &str, uploader: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    title.to_lowercase().contains(&query) || uploader.to_lowercase().contains(&query)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_matches_query_case_insensitive() {
        // 标题命中
        assert!(song_matches_query("晴天", "周杰伦", "晴"));
        assert!(song_matches_query("晴天", "周杰伦", "晴天"));
        // UP 主命中
        assert!(song_matches_query("晴天", "周杰伦", "杰伦"));
        // 大小写不敏感（英文）
        assert!(song_matches_query("Hello World", "Someone", "hello"));
        assert!(song_matches_query("Hello World", "Someone", "WORLD"));
        // 空查询恒匹配
        assert!(song_matches_query("任何", "标题", ""));
        // 不匹配
        assert!(!song_matches_query("晴天", "周杰伦", "阴天"));
        assert!(!song_matches_query("A", "B", "C"));
    }
}