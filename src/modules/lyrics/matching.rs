//! 候选打分与最佳命中挑选（纯函数）：`match_score*` 系列与 `best_match*`。
//!
//! 打分因子：标题 clean 相等/子串/Levenshtein 相似度、歌手 clean 相等/子串、
//! 同步歌词加成、`instrumental` 惩罚、时长合理性；`SongHint` 存在时对官方
//! 曲名/歌手/时长做强校准。

use super::model::{LrcSearchResult, SongHint};
use super::query::clean_title;
use super::query::usable_uploader;
use super::text::lev_similarity;

/// 相似度打分：候选结果相对 (title, uploader) 的匹配质量，越大越好。
///
/// 组成：
/// - 标题（clean_title 后）：完全相等 +100；互为子串 +65；否则按编辑距离相似度比例 +≤55。
/// - 艺术家（uploader 与 candidate.artist_name 的 clean 比较）：相等 +30；子串 +18；否则 ≤+22。
/// - 结果带同步歌词 +8；`instrumental` 结果 -25（我们要有歌词的版本）。
/// - 时长（卢——无目标时长，仅做合理性：90~600s 属于典型歌曲 +5；<10s 视为异常 -8）。
///
/// 注：任务提到「duration 接近加分」，但该签名无目标时长参考；做相对比较需调用方把目标
/// 时长传入，此处用「典型歌曲区间」做弱偏好，见遗留 TODO。
pub fn match_score(candidate: &LrcSearchResult, title: &str, uploader: &str) -> i64 {
    let t = clean_title(title);
    let ct = clean_title(&candidate.track_name);
    let mut s: i64 = 0;

    if !ct.is_empty() && !t.is_empty() {
        if ct == t {
            s += 100;
        } else if ct.contains(&t) || t.contains(&ct) {
            s += 65;
        } else {
            s += (lev_similarity(&t, &ct) * 55.0) as i64;
        }
    }

    if let Some(u) = usable_uploader(uploader) {
        let un = clean_title(u);
        let an = clean_title(&candidate.artist_name);
        if !un.is_empty() && !an.is_empty() {
            if un == an {
                s += 30;
            } else if un.contains(&an) || an.contains(&un) {
                s += 18;
            } else {
                s += (lev_similarity(&un, &an) * 22.0) as i64;
            }
        }
    }

    if !candidate.synced_lyrics.is_empty() {
        s += 8;
    }
    if candidate.instrumental {
        s -= 25;
    }
    if candidate.duration > 0.0 {
        if (90.0..=600.0).contains(&candidate.duration) {
            s += 5;
        } else if candidate.duration < 10.0 {
            s -= 8;
        }
    }
    s
}

/// 带歌曲提示的打分（[`match_score`] + 提示校准项）。
///
/// 提示来自 B 站「识别音乐」（官方曲库标注），命中提示的候选额外加分：
/// - 候选曲名 == 提示曲名（clean 后）+60；互为子串 +35；
/// - 候选歌手 == 提示歌手 +30；互为子串 +15；
/// - **时长接近**（视频时长 vs 候选歌曲时长）：差 ≤3s +35、≤8s +20、≤15s +8；
///   差 >45s -10（识别对了曲名但候选是remix/live/翻唱时，时长通常是明显信号）。
///
/// 标题派生打分与提示打分并行叠加：视频标题与提示词都命中的候选稳居第一。
pub fn match_score_with_hint(
    candidate: &LrcSearchResult,
    title: &str,
    uploader: &str,
    hint: Option<&SongHint>,
) -> i64 {
    let mut s = match_score(candidate, title, uploader);
    let Some(h) = hint else {
        return s;
    };
    let ht = clean_title(&h.title);
    let ct = clean_title(&candidate.track_name);
    if !ht.is_empty() && !ct.is_empty() {
        if ct == ht {
            s += 60;
        } else if ct.contains(&ht) || ht.contains(&ct) {
            s += 35;
        }
    }
    let ha = clean_title(&h.artist);
    let an = clean_title(&candidate.artist_name);
    if !ha.is_empty() && !an.is_empty() {
        if an == ha {
            s += 30;
        } else if an.contains(&ha) || ha.contains(&an) {
            s += 15;
        }
    }
    if h.duration_secs > 0.0 && candidate.duration > 0.0 {
        let diff = (h.duration_secs - candidate.duration).abs();
        if diff <= 3.0 {
            s += 35;
        } else if diff <= 8.0 {
            s += 20;
        } else if diff <= 15.0 {
            s += 8;
        } else if diff > 45.0 {
            s -= 10;
        }
    }
    s
}

/// 带提示的最优候选（打分用 [`match_score_with_hint`]，首个并列者胜；候选为空
/// 返回 `None`）。`hint = None` 时与旧 [`match_score`] 选优语义完全一致。
pub fn best_match_with_hint<'a>(
    candidates: &'a [LrcSearchResult],
    title: &str,
    uploader: &str,
    hint: Option<&SongHint>,
) -> Option<&'a LrcSearchResult> {
    let mut best: Option<&LrcSearchResult> = None;
    let mut best_score = i64::MIN;
    for c in candidates {
        let sc = match_score_with_hint(c, title, uploader, hint);
        if sc > best_score {
            best_score = sc;
            best = Some(c);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_score_boosts_official_title_artist_and_duration() {
        let hint = SongHint {
            title: "晴天".into(),
            artist: "周杰伦".into(),
            duration_secs: 269.0,
        };
        let official = LrcSearchResult {
            id: 1,
            track_name: "晴天".to_string(),
            artist_name: "周杰伦".to_string(),
            album_name: "叶惠美".to_string(),
            duration: 269.0,
            instrumental: false,
            plain_lyrics: "故事的小黄花".to_string(),
            synced_lyrics: "[00:01.00]故事的小黄花".to_string(),
        };
        // 同曲名的翻唱（歌手不同、时长差很多）：提示打分应拉开差距。
        let cover = LrcSearchResult {
            id: 2,
            track_name: "晴天".to_string(),
            artist_name: "某翻唱歌手".to_string(),
            album_name: String::new(),
            duration: 180.0,
            instrumental: false,
            plain_lyrics: "故事的小黄花".to_string(),
            synced_lyrics: "[00:01.00]故事的小黄花".to_string(),
        };
        let s_official = match_score_with_hint(&official, "【高清】晴天 周杰伦", "Music频道", Some(&hint));
        let s_cover = match_score_with_hint(&cover, "【高清】晴天 周杰伦", "Music频道", Some(&hint));
        assert!(s_official > s_cover + 30, "{s_official} vs {s_cover}");
        // 无提示时两者平手（曲名相同、同步歌词相同）。
        let s0_official = match_score(&official, "晴天", "Music频道");
        let s0_cover = match_score(&cover, "晴天", "Music频道");
        assert_eq!(s0_official, s0_cover);
    }

    #[test]
    fn hint_score_duration_tiers() {
        let mk = |dur: f64| LrcSearchResult {
            duration: dur,
            track_name: "晴天".into(),
            artist_name: "周杰伦".into(),
            ..Default::default()
        };
        let hint = |vid: f64| SongHint {
            title: "晴天".into(),
            artist: "周杰伦".into(),
            duration_secs: vid,
        };
        let h = hint(269.0);
        // 分层：≤3s > ≤8s > ≤15s。
        let a = match_score_with_hint(&mk(269.0), "晴天", "", Some(&h));
        let b = match_score_with_hint(&mk(274.0), "晴天", "", Some(&h));
        let c = match_score_with_hint(&mk(281.0), "晴天", "", Some(&h));
        assert!(a > b && b > c, "{a} {b} {c}");
        // 同曲名同歌手的两个候选（如原曲 vs remix）：时长接近者胜出。
        let close = match_score_with_hint(&mk(270.0), "晴天", "", Some(&h));
        let far = match_score_with_hint(&mk(400.0), "晴天", "", Some(&h));
        assert!(close > far, "close={close} far={far}");
    }

    #[test]
    fn best_match_with_hint_picks_official_version() {
        let hint = SongHint {
            title: "Unwelcome School".into(),
            artist: "Mitsukiyo".into(),
            duration_secs: 122.0,
        };
        let candidates = vec![
            LrcSearchResult {
                id: 2,
                track_name: "Unwelcome School (Remix)".to_string(),
                artist_name: "某Remixer".to_string(),
                album_name: String::new(),
                duration: 95.0,
                instrumental: false,
                plain_lyrics: "x".to_string(),
                synced_lyrics: String::new(),
            },
            LrcSearchResult {
                id: 1,
                track_name: "Unwelcome School".to_string(),
                artist_name: "Mitsukiyo".to_string(),
                album_name: "Blue Archive".to_string(),
                duration: 122.0,
                instrumental: false,
                plain_lyrics: "y".to_string(),
                synced_lyrics: "[00:01.00]y".to_string(),
            },
        ];
        let best = best_match_with_hint(&candidates, "碧蓝档案神曲燃剪", "搬运", Some(&hint)).unwrap();
        assert_eq!(best.id, 1);
    }

    #[test]
    fn best_match_prefers_exact_and_synced() {
        let candidates = vec![
            LrcSearchResult {
                id: 2,
                track_name: "晴天 (Live)".to_string(),
                artist_name: "周杰伦".to_string(),
                album_name: String::new(),
                duration: 270.0,
                instrumental: true, // 乐器版 → 扣分
                plain_lyrics: "x".to_string(),
                synced_lyrics: String::new(),
            },
            LrcSearchResult {
                id: 1,
                track_name: "晴天".to_string(),
                artist_name: "周杰伦".to_string(),
                album_name: "叶惠美".to_string(),
                duration: 269.0,
                instrumental: false,
                plain_lyrics: "故事的小黄花".to_string(),
                synced_lyrics: "[00:01.00]故事的小黄花".to_string(),
            },
            LrcSearchResult {
                id: 3,
                track_name: "阴天".to_string(),
                artist_name: "王力宏".to_string(),
                album_name: String::new(),
                duration: 240.0,
                instrumental: false,
                plain_lyrics: "y".to_string(),
                synced_lyrics: String::new(),
            },
        ];
        let best = best_match_with_hint(&candidates, "晴天", "周杰伦", None).unwrap();
        assert_eq!(best.id, 1);
    }

}
