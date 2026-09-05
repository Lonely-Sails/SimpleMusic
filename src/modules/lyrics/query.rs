//! 标题清洗与查询词生成（纯函数）：把 B 站视频标题/UP 主名转成歌词站查询词。

use super::model::SongHint;
use super::text::{
    collapse_ws, extract_book_core, sanitize_preserving_case, strip_annotation_parens, strip_groups,
    strip_trailing_separator,
};
use super::text::strip_punctuation;
/// 去掉 B 站标题常见噪音并统一为规范化形式（去括号注释、去书名号、去多余空白、
/// 统一小写），**用于查询生成与相似度比较**。
///
/// 策略（保守，尽量不误伤主标题）：
/// - 若标题含《…》，优先取其书名号内内容（B 站音乐标题常把歌名放在《》里）。
/// - 整体去掉 `【…】`、`[…]`。
/// - `(…)`/`（…）` 仅当内容是注释（MV/OST/OP/ED/官方/现场/翻唱等关键词或全大写短标记）时移除，
///   否则保留其中的文字、只去掉括号本身。
/// - 去掉尾部 ` - 艺术家` 之类的分隔后缀。
/// - 去掉书名号/引号符号，折叠空白，转小写。
pub fn clean_title(title: &str) -> String {
    let mut t = title.trim().to_string();
    if let Some(core) = extract_book_core(&t) {
        t = core;
    }
    t = strip_groups(&t, '【', '】');
    t = strip_groups(&t, '[', ']');
    t = strip_annotation_parens(&t);
    t = strip_trailing_separator(&t);
    for ch in ['《', '》', '「', '」', '『', '』', '〈', '〉', '"', '\'', '“', '”'] {
        t = t.replace(ch, "");
    }
    let t = collapse_ws(&t);
    t.trim().to_lowercase()
}

/// 生成对 vkeys/LRCLIB 依次尝试的有序候选查询（最多 5 个），从最精确到最宽松。
///
/// 顺序：
/// 1. `<uploader> <clean_title>`（若 uploader 像是艺术家名）
/// 2. `<clean_title>`
/// 3. 保留大小写、剥离注释后的标题
/// 4. 去掉所有标点的 bare 关键词
/// 5. uploader 单独（作为艺术家名兜底）
pub fn search_queries(title: &str, uploader: &str) -> Vec<String> {
    search_queries_with_hint(title, uploader, None)
}

/// 带歌曲提示的查询生成（[`search_queries`] 的增强版）。
///
/// 有 `hint`（B 站「识别音乐」）时把官方词插到最前——官方曲名/歌手远比 B 站标题干净：
/// - `<hint.artist> <hint.title>`（官方歌手 + 官方曲名，最精确）
/// - `<hint.title>`（官方曲名）
/// 其余视频标题派生的查询作为兜底（识别偶有偏差：识别的是 BGM 而非主曲、
/// 或标注的是二创所用原曲）。
pub fn search_queries_with_hint(
    title: &str,
    uploader: &str,
    hint: Option<&SongHint>,
) -> Vec<String> {
    let mut qs: Vec<String> = Vec::new();
    if let Some(h) = hint {
        let ht = clean_title(&h.title);
        let ha = clean_title(&h.artist);
        if !ht.is_empty() {
            if !ha.is_empty() {
                qs.push(format!("{ha} {ht}"));
            }
            qs.push(ht);
        }
    }

    let cleaned = clean_title(title);
    if let Some(u) = usable_uploader(uploader) {
        let cand = format!("{u} {cleaned}").trim().to_string();
        if !cand.is_empty() {
            qs.push(cand);
        }
    }
    if !cleaned.is_empty() {
        qs.push(cleaned.clone());
    }

    let preserved = sanitize_preserving_case(title);
    if !preserved.is_empty() && !qs.iter().any(|x| x.eq_ignore_ascii_case(&preserved)) {
        qs.push(preserved);
    }
    let bare = collapse_ws(&strip_punctuation(&cleaned));
    if !bare.is_empty() && !qs.iter().any(|x| x.eq_ignore_ascii_case(&bare)) {
        qs.push(bare);
    }
    if let Some(u) = usable_uploader(uploader) {
        if !qs.iter().any(|x| x.eq_ignore_ascii_case(u)) {
            qs.push(u.to_string());
        }
    }

    // 去重（提示词与标题派生词可能相同）+ 截断到 5 条。
    let mut qs = dedup_queries(qs, 5);
    if qs.is_empty() {
        qs.push(cleaned.trim().to_string());
    }
    qs
}

/// 按原始顺序去重查询词（大小写不敏感比较）；已满 `max` 则截断。
///
/// 提示词与视频标题相同（如视频就叫《晴天》）时，两边会生成同一查询，
/// 去重避免对搜索源发起重复请求。
fn dedup_queries(mut qs: Vec<String>, max: usize) -> Vec<String> {
    let mut seen: Vec<String> = Vec::with_capacity(qs.len());
    qs.retain(|q| {
        let key = q.trim().to_lowercase();
        if key.is_empty() || seen.contains(&key) {
            return false;
        }
        seen.push(key);
        true
    });
    qs.truncate(max);
    qs
}

/// 是否为「可当作艺术家名」的 uploader（B 站频道）：
/// 过长、空、或含明显非艺术家标记（官方/频道/字幕组/音乐平台词）时返回 `None`。
pub fn usable_uploader(uploader: &str) -> Option<&str> {
    let u = uploader.trim();
    if u.is_empty() || u.chars().count() > 40 {
        return None;
    }
    let lower = u.to_lowercase();
    const MARKERS: &[&str] = &[
        "官方", "官方频道", "频道", "official", "电视台", "字幕组", "搬运", "资源",
        "music zone", "music", "studio", "records", "center", "group", "video", "live",
        "歌迷会", "后援会", "粉丝", "musicclub", "音乐台",
    ];
    if MARKERS.iter().any(|m| lower.contains(m)) {
        return None;
    }
    Some(u)
}

/// 相似度打分：候选结果相对 (title, uploader) 的匹配质量，越大越好。
///
/// 组成：
/// - 标题（clean_title 后）：完全相等 +100；互为子串 +65；否则按编辑距离相似度比例 +≤55。
/// - 艺术家（uploader 与 candidate.artist_name 的 clean 比较）：相等 +30；子串 +18；否则 ≤+22。
/// - 结果带同步歌词 +8；`instrumental` 结果 -25（我们要有歌词的版本）。
/// - 时长（卢——无目标时长，仅做合理性：90~600s 属于典型歌曲 +5；<10s 视为异常 -8）。
///
/// 注：任务提到「duration 接近加分」，但该签名无目标时长参考；做相对比较需调用方把目标

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_title_prefers_book_core_and_strips_noise() {
        assert_eq!(clean_title("【4K】周杰伦《晴天》MV (Official)"), "晴天");
        assert_eq!(clean_title("我的地盘《七里香》"), "七里香");
    }

    #[test]
    fn clean_title_strips_separator_and_annotation_parens() {
        assert_eq!(clean_title("晴天 - 周杰伦"), "晴天");
        assert_eq!(clean_title("Hello (Live)"), "hello");
        assert_eq!(clean_title("Hello (Official)"), "hello");
        // 非注释括号：保留文字、去括号。
        assert_eq!(clean_title("Love (You)"), "love you");
    }

    #[test]
    fn search_queries_produces_ordered_candidates() {
        let qs = search_queries("晴天", "周杰伦");
        assert_eq!(qs[0], "周杰伦 晴天"); // artist + title 在前
        assert!(qs.contains(&"晴天".to_string())); // 裸标题
        assert!(qs.len() >= 2 && qs.len() <= 5);
    }

    #[test]
    fn search_queries_filters_channel_uploader() {
        // 明显是频道的 uploader 不作为 artist 前缀。
        let qs = search_queries("晴天", "某某官方频道");
        assert!(!qs[0].contains("某某官方频道 "));
    }

    #[test]
    fn hint_queries_lead_with_official_words() {
        let hint = SongHint {
            title: "Unwelcome School".into(),
            artist: "ミツキヨ".into(),
            duration_secs: 0.0,
        };
        let qs = search_queries_with_hint(
            "【4K修复】【碧蓝档案】Unwelcome School 燃剪",
            "某搬运频道",
            Some(&hint),
        );
        // 官方词在最前，且视频标题的查询仍作兜底。
        assert_eq!(qs[0], "ミツキヨ unwelcome school");
        assert_eq!(qs[1], "unwelcome school");
        assert!(qs.iter().any(|q| q.contains("燃剪")));
    }

    #[test]
    fn hint_queries_dedup_and_fallback_without_hint() {
        // 无提示 = 旧行为。
        let plain = search_queries("晴天", "周杰伦");
        let with_none = search_queries_with_hint("晴天", "周杰伦", None);
        assert_eq!(plain, with_none);
        // 提示与标题相同时不产生重复查询。
        let hint = SongHint {
            title: "晴天".into(),
            artist: "周杰伦".into(),
            duration_secs: 0.0,
        };
        let qs = search_queries_with_hint("晴天", "周杰伦", Some(&hint));
        assert_eq!(qs[0], "周杰伦 晴天");
        let uniq: std::collections::HashSet<_> = qs.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(uniq.len(), qs.len(), "查询有重复: {qs:?}");
    }

}
