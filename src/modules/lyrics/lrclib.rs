//! LRCLIB 数据源（blocking）：`LyricsProvider::fetch` 是阻塞调用（UI 请丢后台线程）。
//!
//! 拉取策略：每条查询先试 vkeys（见 [`super::vkeys`]），再回退 LRCLIB
//! `search` 与精确 `get`，按得分挑选。

use std::time::Duration;

use super::model::{Lyrics, LrcSearchResult, SongHint};
use super::matching::best_match_if_acceptable;
use super::query::clean_title;
use super::query::{search_queries_with_hint, usable_uploader};
use super::vkeys::{vkeys_source_fetch, VkSource};
use super::{LRCLIB_GET, LRCLIB_SEARCH, LRCLIB_UA, MIN_ACCEPT_SCORE};

// ===========================================================================
// LRCLIB 获取（blocking）
// ===========================================================================

/// LRCLIB 数据源：`LyricsProvider::fetch` 是阻塞调用（UI 请丢后台线程）。
pub struct LyricsProvider;

impl LyricsProvider {
    /// 拉取歌词：先查 vkeys.cn 聚合源（QQ 音乐优先 → 网易云），再回退 LRCLIB。
    ///
    /// 等价于 [`fetch_all_with_hint`](LyricsProvider::fetch_all_with_hint)（`hint=None`）
    /// 的第一个候选（最优先命中）。全链路失败返回 `None`（网络错误、无命中、无歌词）。
    pub fn fetch(title: &str, uploader: &str) -> Option<Lyrics> {
        Self::fetch_all_with_hint(title, uploader, None)
            .into_iter()
            .next()
    }

    /// 拉取**全部**歌词候选（供「歌词选择」弹窗使用）。
    ///
    /// 收集顺序与候选优先级一致：
    /// 1. 每条查询：vkeys QQ 音乐最佳命中 → 网易云最佳命中；
    /// 2. 每条查询：LRCLIB 搜索的最佳命中（得分达标）；
    /// 3. LRCLIB 精确 GET（歌名 + 艺术家）。
    ///
    /// 按歌词内容去重（不同来源命中同一份歌词时只保留第一个），
    /// 无任何命中返回空数组。
    ///
    /// `hint` 来自 B 站「识别音乐」+ 稿件时长（见 [`SongHint`]），用于生成更准的
    /// 查询词并校准打分（查询与打分见 [`search_queries_with_hint`] /
    /// [`match_score_with_hint`]）；`None` 时按 title/uploader 原样搜索。
    pub fn fetch_all_with_hint(
        title: &str,
        uploader: &str,
        hint: Option<&SongHint>,
    ) -> Vec<Lyrics> {
        let client = http_client();
        let queries = search_queries_with_hint(title, uploader, hint);
        let mut out: Vec<Lyrics> = Vec::new();

        // 1) vkeys.cn 聚合源（QQ 音乐 priority=1, 网易云 priority=0）
        for q in &queries {
            let q = q.trim();
            if q.is_empty() {
                continue;
            }
            if let Some(ly) = vkeys_source_fetch(&client, VkSource::Qq, q, title, uploader, hint) {
                push_unique_lyrics(&mut out, ly);
            }
            if let Some(ly) = vkeys_source_fetch(&client, VkSource::Netease, q, title, uploader, hint)
            {
                push_unique_lyrics(&mut out, ly);
            }
        }

        // 2) LRCLIB 回退：搜索 + 精确 GET。
        for q in &queries {
            let q = q.trim();
            if q.is_empty() {
                continue;
            }
            if let Some(results) = search(&client, q) {
                if let Some((_, best)) =
                    best_match_if_acceptable(&results, title, uploader, hint, MIN_ACCEPT_SCORE)
                {
                    push_unique_lyrics(&mut out, lyrics_from(best));
                }
            }
        }
        // 3) LRCLIB 精确 GET：优先用提示里的官方词（识别音乐时曲名/歌手最标准），
        //    没有提示再退回视频标题清洗结果。
        let (artist, track) = match hint {
            Some(h) if h.has_query() => (
                usable_uploader(&h.artist).unwrap_or("").to_string(),
                clean_title(&h.title),
            ),
            _ => (
                usable_uploader(uploader).unwrap_or("").to_string(),
                clean_title(title),
            ),
        };
        if !track.is_empty() {
            if let Some(res) = get(&client, &artist, &track) {
                push_unique_lyrics(&mut out, lyrics_from(&res));
            }
        }
        out
    }
}

/// 把 `ly` 追加到候选列表末尾；若已有内容相同的候选（比较 LRC 原文，
/// 无 LRC 时比较纯文本）则跳过。
fn push_unique_lyrics(out: &mut Vec<Lyrics>, ly: Lyrics) {
    if out.iter().any(|x| lyrics_same_content(x, &ly)) {
        return;
    }
    out.push(ly);
}

/// 两份歌词是否内容相同（有 LRC 比 LRC，否则比纯文本）。
fn lyrics_same_content(a: &Lyrics, b: &Lyrics) -> bool {
    match (a.lrc.as_deref(), b.lrc.as_deref()) {
        (Some(x), Some(y)) => x.trim() == y.trim(),
        _ => a.plain.trim() == b.plain.trim(),
    }
}

/// 把 LRCLIB 结果打包成 [`Lyrics`]。
fn lyrics_from(res: &LrcSearchResult) -> Lyrics {
    Lyrics {
        lrc: if res.synced_lyrics.is_empty() {
            None
        } else {
            Some(res.synced_lyrics.clone())
        },
        plain: res.plain_lyrics.clone(),
        source: Some(res.clone()),
    }
}

/// 构建带 UA、连接/总超时的 blocking 客户端。
fn http_client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent(LRCLIB_UA)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .expect("构建 LRCLIB HTTP 客户端失败")
}

/// LRCLIB 搜索：`GET /api/search?q=…`，命中为空或失败返回 `None`。
fn search(client: &reqwest::blocking::Client, query: &str) -> Option<Vec<LrcSearchResult>> {
    let resp = client
        .get(LRCLIB_SEARCH)
        .query(&[("q", query)])
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<Vec<LrcSearchResult>>().ok()
}

/// LRCLIB 精确 `GET /api/get?artist_name=..&track_name=..`，未命中/失败返回 `None`。
fn get(client: &reqwest::blocking::Client, artist: &str, track: &str) -> Option<LrcSearchResult> {
    let resp = client
        .get(LRCLIB_GET)
        .query(&[("artist_name", artist), ("track_name", track)])
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<LrcSearchResult>().ok()
}

// ===========================================================================
// vkeys.cn 聚合源（QQ 音乐 / 网易云音乐）
