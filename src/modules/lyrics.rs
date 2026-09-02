//! 歌词模块：LRCLIB 搜索 + LRC 解析 + 按播放位置的时间轴同步。
//!
//! 数据来源固定为 LRCLIB（<https://lrclib.net>，免费、无需鉴权，返回 JSON）。
//! 两条 HTTP 通道：
//! - 搜索：`GET /api/search?q=<查询>` → 命中数组
//! - 精确：`GET /api/get?artist_name=<..>&track_name=<..>` → 单对象
//! 每个结果含 `id, trackName, artistName, albumName, duration, instrumental,
//! plainLyrics, syncedLyrics`，其中 `syncedLyrics` 为 LRC 格式文本。
//!
//! 本模块为纯逻辑 + blocking 网络（调用方把 `LyricsProvider::fetch` 丢到后台线程即可），
//! 不依赖 UI，不写 `state`，因此可独立单测与探针集成。
//!
//! # 给 UI 接线工人的接口速览
//! - `LyricsProvider::fetch(title, uploader) -> Option<Lyrics>`：阻塞在调用线程；
//!   拿到 `Lyrics` 后把它放进后台状态即可。
//! - 同步歌词可用性：`lyrics.lrc.is_some()`；解析为时间轴：
//!   `let lines = lrc::parse(lyrics.lrc.as_deref().unwrap_or(""));`
//! - 拿当前行：`lrc::current_line(&lines, pos_secs)` 返回当前句（`Option<&LrcLine>`）；
//!   `lrc::next_line(&lines, pos_secs)` 返回下一句（供"下一句预览"）。
//! - **无同步歌词时**：`lyrics.plain` 仍可整段显示（桌面歌词/滚动列表）但没有高亮；
//!   需要近似高亮时可按 `pos_secs / duration_secs` 的播放比例在 plain 行数上近似取行
//!   （`plain.lines().nth((progress * lines).floor())`）。
//! - **前奏**：`current_line_index` 在 `pos_secs < 第一句时间` 时返回 0，UI 若想显示
//!   "前奏" 可比较 `pos_secs < lines[0].time_secs` 并单独渲染，否则直接显示第 0 句。

use std::time::Duration;

/// LRCLIB 应用的 User-Agent（LRCLIB 要求标识应用，禁止默认 curl UA）。
pub const LRCLIB_UA: &str = "SimpleMusic/0.1 (Rust desktop player; lyrics fetched from LRCLIB)";

const LRCLIB_SEARCH: &str = "https://lrclib.net/api/search";
const LRCLIB_GET: &str = "https://lrclib.net/api/get";

/// fetch 接受的候选相似度下限：低于它认为没命中（转为尝试下一条查询 / 回退 GET）。
const MIN_ACCEPT_SCORE: i64 = 40;

// ===========================================================================
// 数据模型
// ===========================================================================

/// 一行带时间轴的歌词。
#[derive(Debug, Clone, PartialEq)]
pub struct LrcLine {
    /// 这句开始的时间（秒）。
    pub time_secs: f64,
    /// 歌词文本。
    pub text: String,
}

/// LRCLIB 返回的一条歌词结果（搜索数组元素与 GET 单对象同构）。
///
/// 用宽松反序列化：缺失字段取默认值，避免结果只缺 `syncedLyrics` 时整条失败。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LrcSearchResult {
    #[serde(default)]
    pub id: u64,
    #[serde(rename = "trackName", default)]
    pub track_name: String,
    #[serde(rename = "artistName", default)]
    pub artist_name: String,
    #[serde(rename = "albumName", default)]
    pub album_name: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub instrumental: bool,
    #[serde(rename = "plainLyrics", default)]
    pub plain_lyrics: String,
    #[serde(rename = "syncedLyrics", default)]
    pub synced_lyrics: String,
}

/// `fetch` 的最终产物。
#[derive(Debug, Clone, PartialEq)]
pub struct Lyrics {
    /// 同步 LRC 原文（无同步时为 `None`）。
    pub lrc: Option<String>,
    /// 纯文本歌词（无时间标签），无同步歌词时的兜底展示。
    pub plain: String,
    /// 命中的来源元信息（LRCLIB 结果），用于展示所用专辑/艺术家等。
    pub source: Option<LrcSearchResult>,
}

impl Lyrics {
    /// 是否有同步（时间轴）歌词。
    pub fn has_synced(&self) -> bool {
        self.lrc.is_some()
    }

    /// 把同步歌词解析成时间轴行；无同步歌词时返回空。
    pub fn lrc_lines(&self) -> Vec<LrcLine> {
        match self.lrc.as_deref() {
            Some(l) => lrc::parse(l),
            None => Vec::new(),
        }
    }
}

// ===========================================================================
// LRC 解析 + 同步引擎（纯函数，独立子模块 `lrc`）
// ===========================================================================

/// LRC 文本的解析与按播放位置的同步查找。
pub mod lrc {
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
}

// ===========================================================================
// 标题清洗 / 查询生成（纯函数）
// ===========================================================================

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

/// 生成对 LRCLIB 依次尝试的有序候选查询（2~5 个），从最精确到最宽松。
///
/// 顺序：
/// 1. `<uploader> <clean_title>`（若 uploader 像是艺术家名）
/// 2. `<clean_title>`
/// 3. 保留大小写、剥离注释后的标题
/// 4. 去掉所有标点的 bare 关键词
/// 5. uploader 单独（作为艺术家名兜底）
pub fn search_queries(title: &str, uploader: &str) -> Vec<String> {
    let cleaned = clean_title(title);
    let mut qs: Vec<String> = Vec::new();

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

    qs.truncate(5);
    if qs.is_empty() {
        qs.push(cleaned.trim().to_string());
    }
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

/// 从候选里按 [`match_score`] 选出最优（首个并列者胜）；候选为空返回 `None`。
pub fn best_match<'a>(
    candidates: &'a [LrcSearchResult],
    title: &str,
    uploader: &str,
) -> Option<&'a LrcSearchResult> {
    let mut best: Option<&LrcSearchResult> = None;
    let mut best_score = i64::MIN;
    for c in candidates {
        let sc = match_score(c, title, uploader);
        if sc > best_score {
            best_score = sc;
            best = Some(c);
        }
    }
    best
}

// ===========================================================================
// LRCLIB 获取（blocking）
// ===========================================================================

/// LRCLIB 数据源：`LyricsProvider::fetch` 是阻塞调用（UI 请丢后台线程）。
pub struct LyricsProvider;

impl LyricsProvider {
    /// 拉取歌词：先按 [`search_queries`] 依次搜索并 `best_match`；都不满意时回退
    /// LRCLIB 精确 GET（用 clean 后的 uploader/artist 与 title/track）。
    ///
    /// 全链路失败返回 `None`（网络错误、无命中、无歌词）。
    pub fn fetch(title: &str, uploader: &str) -> Option<Lyrics> {
        let client = http_client();

        for q in search_queries(title, uploader) {
            let q = q.trim();
            if q.is_empty() {
                continue;
            }
            if let Some(results) = search(&client, q) {
                if let Some(best) = best_match(&results, title, uploader) {
                    if match_score(best, title, uploader) >= MIN_ACCEPT_SCORE {
                        return Some(lyrics_from(best));
                    }
                }
            }
        }

        // 回退精确 GET。
        let artist = usable_uploader(uploader).unwrap_or("");
        let track = clean_title(title);
        if !track.is_empty() {
            if let Some(res) = get(&client, artist, &track) {
                return Some(lyrics_from(&res));
            }
        }
        None
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
// 小工具
// ===========================================================================

/// 提取书名号《…》内文本（取第一个）；无则 `None`。
fn extract_book_core(s: &str) -> Option<String> {
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
fn strip_groups(s: &str, open: char, close: char) -> String {
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
fn strip_annotation_parens(s: &str) -> String {
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
fn strip_trailing_separator(s: &str) -> String {
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
fn sanitize_preserving_case(title: &str) -> String {
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
fn strip_punctuation(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
}

/// 折叠连续空白为一个空格。
fn collapse_ws(s: &str) -> String {
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
fn lev_similarity(a: &str, b: &str) -> f64 {
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

    // ---- LRC 解析 ----

    #[test]
    fn parse_multi_timestamp_one_line() {
        let lrc = "[00:10.00][00:20.00]重复的句";
        let lines = lrc::parse(lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_secs, 10.0);
        assert_eq!(lines[1].time_secs, 20.0);
        assert_eq!(lines[0].text, "重复的句");
        assert_eq!(lines[1].text, "重复的句");
    }

    #[test]
    fn parse_bom_and_crlf() {
        let lrc = "\u{feff}[00:01.00]第一行\r\n[00:02.00]第二行\r\n";
        let lines = lrc::parse(lrc);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].time_secs, 1.0);
        assert_eq!(lines[0].text, "第一行");
        assert_eq!(lines[1].time_secs, 2.0);
        assert_eq!(lines[1].text, "第二行");
    }

    #[test]
    fn parse_offset_positive_shifts_later() {
        let lrc = "[offset:+500]\n[00:10.00]a";
        let lines = lrc::parse(lrc);
        assert_eq!(lines.len(), 1);
        assert!((lines[0].time_secs - 10.5).abs() < 1e-9, "got {}", lines[0].time_secs);
    }

    #[test]
    fn parse_offset_negative_shifts_earlier_and_clamps() {
        // 负偏移把 10s 推到 9.8s。
        let lrc = "[offset:-200]\n[00:10.00]a";
        let lines = lrc::parse(lrc);
        assert!((lines[0].time_secs - 9.8).abs() < 1e-9);
        // 大负偏移把 10s 推到负值 → 钳制为 0。
        let lrc2 = "[offset:-11000]\n[00:10.00]a";
        let lines2 = lrc::parse(lrc2);
        assert_eq!(lines2[0].time_secs, 0.0);
    }

    #[test]
    fn parse_metadata_tags_ignored() {
        let lrc = "[ti:标题][ar:歌手][al:专辑][by:制作]\n[offset:0]\n[00:03.00]歌词";
        let lines = lrc::parse(lrc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "歌词");
        assert_eq!(lines[0].time_secs, 3.0);
    }

    #[test]
    fn parse_plain_text_without_timestamps_ignored() {
        let lrc = "这是一行没有时间标签的歌词\n[00:05.00]有标签的";
        let lines = lrc::parse(lrc);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "有标签的");
    }

    #[test]
    fn parse_fraction_dot_and_comma() {
        let lrc = "[00:01.5]a\n[00:02,50]b\n[00:02.120]c";
        let lines = lrc::parse(lrc);
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
        let lines = lrc::parse("[00:01.00]a\n[00:03.00]b\n[00:05.00]c\n[00:07.00]d");
        // pos 在 3 与 5 之间 → 上一句是 3(b)，下标 1。
        assert_eq!(lrc::current_line_index(&lines, 4.0), Some(1));
        // 恰好等于某句 → 那一句。
        assert_eq!(lrc::current_line_index(&lines, 3.0), Some(1));
        // 越过最后一句 → 最后一句。
        assert_eq!(lrc::current_line_index(&lines, 100.0), Some(3));
        // 等于第一句。
        assert_eq!(lrc::current_line_index(&lines, 1.0), Some(0));
    }

    #[test]
    fn current_line_prelude_returns_first() {
        let lines = lrc::parse("[00:03.00]a\n[00:05.00]b");
        // pos 早于第一句（前奏）→ 钳制为 0。
        assert_eq!(lrc::current_line_index(&lines, 0.5), Some(0));
        assert_eq!(lrc::current_line(&lines, 0.5).map(|l| l.text.as_str()), Some("a"));
    }

    #[test]
    fn next_line_and_empty() {
        let lines = lrc::parse("[00:01.00]a\n[00:03.00]b\n[00:05.00]c");
        // pos=4 → 下一句是 5(c)，下标 2。
        assert_eq!(lrc::next_line(&lines, 4.0).map(|l| l.text.as_str()), Some("c"));
        // 前奏 → 第一句是下一句。
        assert_eq!(lrc::next_line(&lines, 0.0).map(|l| l.text.as_str()), Some("a"));
        // 恰等于 b(3) → 下一句是 c(5)。
        assert_eq!(lrc::next_line(&lines, 3.0).map(|l| l.text.as_str()), Some("c"));
        // 越过最后一句 → None。
        assert_eq!(lrc::next_line(&lines, 100.0), None);
    }

    #[test]
    fn sync_engine_empty_input() {
        let empty: Vec<LrcLine> = Vec::new();
        assert_eq!(lrc::current_line_index(&empty, 5.0), None);
        assert_eq!(lrc::current_line(&empty, 5.0), None);
        assert_eq!(lrc::next_line(&empty, 5.0), None);
    }

    // ---- 标题清洗 / 查询 / 匹配 ----

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
        let best = best_match(&candidates, "晴天", "周杰伦").unwrap();
        assert_eq!(best.id, 1);
    }

    // ---- JSON 反序列化（LRCLIB 真实响应样例） ----

    #[test]
    fn json_deserialize_search_array() {
        let json = r#"[
          {"id":11,"trackName":"晴天","artistName":"周杰伦","albumName":"叶惠美",
           "duration":269,"instrumental":false,
           "plainLyrics":"故事的小黄花\n从出生那年就飘着",
           "syncedLyrics":"[00:01.00]故事的小黄花\n[00:03.00]从出生那年就飘着"}
        ]"#;
        let v: Vec<LrcSearchResult> = serde_json::from_str(json).expect("反序列化失败");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].id, 11);
        assert_eq!(v[0].track_name, "晴天");
        assert_eq!(v[0].artist_name, "周杰伦");
        assert_eq!(v[0].duration, 269.0);
        assert!(!v[0].instrumental);
        assert!(v[0].synced_lyrics.contains("故事的小黄花"));
    }

    #[test]
    fn json_deserialize_get_missing_optional_fields() {
        // GET 单对象，syncedLyrics 缺失（只有纯文本）：仍应解析成功，synced_lyrics 为空。
        let json = r#"{"id":22,"trackName":"晴天","artistName":"周杰伦",
            "albumName":"叶惠美","duration":269,"instrumental":false,
            "plainLyrics":"纯文本歌词"}"#;
        let v: LrcSearchResult = serde_json::from_str(json).expect("反序列化失败");
        assert_eq!(v.track_name, "晴天");
        assert_eq!(v.synced_lyrics, "");
        assert_eq!(v.plain_lyrics, "纯文本歌词");
    }

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
