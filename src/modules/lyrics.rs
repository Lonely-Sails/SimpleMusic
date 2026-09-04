//! 歌词模块：多源搜索（vkeys.cn 聚合 + LRCLIB 回退）+ LRC 解析 + 按播放位置的时间轴同步。
//!
//! 数据来源：
//! 1. **vkeys.cn 聚合源**（优先，中文歌曲覆盖率高）：
//!    - QQ 音乐：`GET /v2/music/tencent/search/song?word=..&page=1&num=8` →
//!      `data[]`（`mid` 为歌曲 id）；歌词 `GET /v2/music/tencent/lyric?mid=..` →
//!      `data.lrc`（LRC 文本）/ `data.trans`（翻译）。
//!    - 网易云：`GET /v2/music/netease?word=..&page=1&num=8` → `data[]`（`id`）；
//!      歌词 `GET /v2/music/netease/lyric?id=..` → `data.lrc` / `data.tlyric.lyric`。
//!    - 翻译歌词存在时按时间戳与主歌词合并成「主句 + 翻译」两行。
//! 2. **LRCLIB 回退**（<https://lrclib.net>，免费、无需鉴权）：
//!    - 搜索：`GET /api/search?q=<查询>` → 命中数组
//!    - 精确：`GET /api/get?artist_name=<..>&track_name=<..>` → 单对象
//!    每个结果含 `id, trackName, artistName, albumName, duration, instrumental,
//!    plainLyrics, syncedLyrics`，其中 `syncedLyrics` 为 LRC 格式文本。
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

use std::collections::BTreeMap;
use std::time::Duration;

/// LRCLIB 应用的 User-Agent（LRCLIB 要求标识应用，禁止默认 curl UA）。
pub const LRCLIB_UA: &str = "SimpleMusic/0.1 (Rust desktop player; lyrics fetched from LRCLIB)";

const LRCLIB_SEARCH: &str = "https://lrclib.net/api/search";
const LRCLIB_GET: &str = "https://lrclib.net/api/get";

/// vkeys.cn 聚合源（QQ 音乐 / 网易云音乐歌词，覆盖中文歌曲）。
const VKEYS_QQ_SEARCH: &str = "https://api.vkeys.cn/v2/music/tencent/search/song";
const VKEYS_QQ_LYRIC: &str = "https://api.vkeys.cn/v2/music/tencent/lyric";
const VKEYS_NETEASE_SEARCH: &str = "https://api.vkeys.cn/v2/music/netease";
const VKEYS_NETEASE_LYRIC: &str = "https://api.vkeys.cn/v2/music/netease/lyric";

/// fetch 接受的候选相似度下限：低于它认为没命中（转为尝试下一条查询 / 回退 GET）。
const MIN_ACCEPT_SCORE: i64 = 40;

/// 已知歌曲提示：来自 B 站「识别音乐」（官方曲库标注）等外部信号，用于
/// ① 生成比视频标题更准的查询词；② 校准候选打分（标题/歌手/时长匹配度）。
///
/// 全字段可选式：`None` 提示时行为与旧版完全一致（按 title/uploader 搜索）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SongHint {
    /// 官方曲名（如 "Unwelcome School"），来自曲库而非视频标题。
    pub title: String,
    /// 官方歌手名。
    pub artist: String,
    /// 视频实际时长（秒，来自 B 站稿件信息）：官方歌曲时长与视频时长接近时
    /// 强烈暗示候选正确（整曲/原曲向视频），差得远则可能是二创混剪。
    pub duration_secs: f64,
}

impl SongHint {
    /// 是否可用于生成查询（至少要有曲名）。
    pub fn has_query(&self) -> bool {
        !self.title.trim().is_empty()
    }
}

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
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
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

/// 带提示的最优候选（[`best_match`] 的打分换成 [`match_score_with_hint`]）。
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

// ===========================================================================
// 本地歌词缓存（条目语义；磁盘读写见 modules/storage.rs）
// ===========================================================================

/// 一条歌词缓存：上次生效的歌词 + 抓取到的全部候选。
///
/// - `selected` = 当前生效歌词（自动抓取结果或用户在「歌词选择」弹窗的手选），
///   下次播放同曲直接应用，**零网络请求**；
/// - `candidates` 存全部候选原文，重启后「歌词选择」弹窗仍可切换；
/// - `saved_at_unix` 仅供排查，不参与过期判断。
///
/// 缓存按 bvid 的 md5 键控（与音频缓存同方案），整表序列化为
/// `~/.cache/simple-music/lyrics.json`。坏文件静默降级为缓存未命中。
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LyricsCacheEntry {
    /// 当前生效（上次使用）的歌词。
    #[serde(default)]
    pub selected: Option<Lyrics>,
    /// 全部歌词候选（供「歌词选择」弹窗；与 selected 一起落盘）。
    #[serde(default)]
    pub candidates: Vec<Lyrics>,
    /// 落盘时间（Unix 秒）。
    #[serde(default)]
    pub saved_at_unix: u64,
}

/// 用 bvid 生成缓存键（复用音频缓存的 md5 键控方案）。
pub fn cache_key(bvid: &str) -> String {
    crate::modules::bilibili::md5_hex(bvid)
}

/// 单曲读写接口：按 bvid 读缓存（无则 `None`）。
pub fn cache_lookup<'a>(
    cache: &'a BTreeMap<String, LyricsCacheEntry>,
    bvid: &str,
) -> Option<&'a LyricsCacheEntry> {
    cache.get(&cache_key(bvid))
}

/// 单曲写入接口：更新 `selected`（当前生效歌词），返回新 entry 供调用方存表。
pub fn cache_update_selected<'a>(
    cache: &'a mut BTreeMap<String, LyricsCacheEntry>,
    bvid: &str,
    selected: Lyrics,
) -> &'a mut LyricsCacheEntry {
    let key = cache_key(bvid);
    let entry = cache.entry(key).or_default();
    entry.selected = Some(selected);
    entry.saved_at_unix = now_unix();
    entry
}

/// 单曲写入接口：记录一次完整抓取结果（selected + candidates）。
pub fn cache_store_fetch(
    cache: &mut BTreeMap<String, LyricsCacheEntry>,
    bvid: &str,
    selected: Option<Lyrics>,
    candidates: Vec<Lyrics>,
) {
    let key = cache_key(bvid);
    let entry = cache.entry(key).or_default();
    entry.selected = selected;
    entry.candidates = candidates;
    entry.saved_at_unix = now_unix();
}

/// 当前 Unix 秒。
fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ===========================================================================
// LRCLIB 获取（blocking）
// ===========================================================================

/// LRCLIB 数据源：`LyricsProvider::fetch` 是阻塞调用（UI 请丢后台线程）。
pub struct LyricsProvider;

impl LyricsProvider {
    /// 拉取歌词：先查 vkeys.cn 聚合源（QQ 音乐优先 → 网易云），再回退 LRCLIB。
    ///
    /// 等价于 [`fetch_all`](LyricsProvider::fetch_all) 的第一个候选（最优先命中）。
    /// 全链路失败返回 `None`（网络错误、无命中、无歌词）。
    pub fn fetch(title: &str, uploader: &str) -> Option<Lyrics> {
        Self::fetch_all(title, uploader).into_iter().next()
    }

    /// 带歌曲提示的歌词拉取（推荐）：`hint` 来自 B 站「识别音乐」+ 稿件时长，
    /// 用于生成更准的查询词并校准打分；`None` 时与 [`fetch`](Self::fetch) 等价。
    pub fn fetch_with_hint(title: &str, uploader: &str, hint: Option<&SongHint>) -> Option<Lyrics> {
        Self::fetch_all_with_hint(title, uploader, hint)
            .into_iter()
            .next()
    }

    /// 拉取**全部**歌词候选（供「歌词选择」弹窗使用）。
    ///
    /// 收集顺序与 [`fetch`](LyricsProvider::fetch) 的优先级一致：
    /// 1. 每条查询：vkeys QQ 音乐最佳命中 → 网易云最佳命中；
    /// 2. 每条查询：LRCLIB 搜索的最佳命中（得分达标）；
    /// 3. LRCLIB 精确 GET（歌名 + 艺术家）。
    ///
    /// 按歌词内容去重（不同来源命中同一份歌词时只保留第一个），
    /// 无任何命中返回空数组。
    pub fn fetch_all(title: &str, uploader: &str) -> Vec<Lyrics> {
        Self::fetch_all_with_hint(title, uploader, None)
    }

    /// [`fetch_all`](Self::fetch_all) 的带提示版本（查询与打分见
    /// [`search_queries_with_hint`] / [`match_score_with_hint`]）。
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
                if let Some(best) = best_match_with_hint(&results, title, uploader, hint) {
                    if match_score_with_hint(best, title, uploader, hint) >= MIN_ACCEPT_SCORE {
                        push_unique_lyrics(&mut out, lyrics_from(best));
                    }
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
// ===========================================================================

/// vkeys 搜索响应：`data` 可能是数组（QQ）、单对象（网易）或 null。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct VkeySearchResp {
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// vkeys 歌词响应：`data` 内含 `lrc` / `trans`（QQ）/ `tlyric`（网易）。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct VkeyLyricResp {
    #[serde(default)]
    pub data: Option<VkeyLyricData>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct VkeyLyricData {
    /// 主歌词（可能是字符串，也可能是 `{"lyric": ".."}` 对象）。
    #[serde(default)]
    pub lrc: Option<LyricText>,
    /// QQ 翻译。
    #[serde(default)]
    pub trans: Option<LyricText>,
    /// 网易翻译（`{"lyric": ".."}`）。
    #[serde(default)]
    pub tlyric: Option<LyricText>,
}

/// 歌词文本字段：兼容字符串与 `{"lyric": ".."}` 两种形态。
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(untagged)]
pub enum LyricText {
    Str(String),
    Obj {
        #[serde(default)]
        lyric: Option<String>,
    },
}

impl LyricText {
    fn text(&self) -> String {
        match self {
            LyricText::Str(s) => s.trim().to_string(),
            LyricText::Obj { lyric } => lyric.as_deref().unwrap_or("").trim().to_string(),
        }
    }
}

/// 数据源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VkSource {
    /// QQ 音乐（priority 1）。
    Qq,
    /// 网易云音乐（priority 0）。
    Netease,
}

impl VkSource {
    fn search_url(&self) -> &'static str {
        match self {
            VkSource::Qq => VKEYS_QQ_SEARCH,
            VkSource::Netease => VKEYS_NETEASE_SEARCH,
        }
    }

    fn lyric_url(&self) -> &'static str {
        match self {
            VkSource::Qq => VKEYS_QQ_LYRIC,
            VkSource::Netease => VKEYS_NETEASE_LYRIC,
        }
    }

    /// 取歌词时用的 id 参数名：QQ 用 `mid`，网易用 `id`。
    fn id_param(&self) -> &'static str {
        match self {
            VkSource::Qq => "mid",
            VkSource::Netease => "id",
        }
    }
}

/// 从 vkeys 单个源搜索并取回歌词；未命中/无歌词返回 `None`。
#[allow(clippy::too_many_arguments)]
fn vkeys_source_fetch(
    client: &reqwest::blocking::Client,
    src: VkSource,
    query: &str,
    title: &str,
    uploader: &str,
    hint: Option<&SongHint>,
) -> Option<Lyrics> {
    let resp = client
        .get(src.search_url())
        .query(&[("word", query), ("page", "1"), ("num", "8")])
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let items = vkeys_extract_items(&resp.json::<VkeySearchResp>().ok()?);
    if items.is_empty() {
        return None;
    }
    let candidates: Vec<LrcSearchResult> = items
        .iter()
        .filter_map(|it| vkey_item_to_candidate(src, it))
        .collect();
    if candidates.is_empty() {
        return None;
    }
    let best = best_match_with_hint(&candidates, title, uploader, hint)?;
    if match_score_with_hint(best, title, uploader, hint) < MIN_ACCEPT_SCORE {
        return None;
    }
    let best_idx = candidates.iter().position(|c| std::ptr::eq(c, best))?;
    let best_id = vkey_item_id(src, &items[best_idx])?;
    let lyric = vkeys_lyric_fetch(client, src, &best_id)?;
    let mut ly = build_vkey_lyrics(lyric)?;
    // 带上候选元信息（用于「歌词选择」弹窗显示曲名/歌手）。
    let mut meta = best.clone();
    if meta.album_name.is_empty() {
        meta.album_name = match src {
            VkSource::Qq => "QQ音乐".to_string(),
            VkSource::Netease => "网易云".to_string(),
        };
    }
    ly.source = Some(meta);
    Some(ly)
}

/// vkeys 搜索响应 → 歌曲条目数组（`data` 数组 / 单对象 / 空）。
fn vkeys_extract_items(resp: &VkeySearchResp) -> Vec<serde_json::Value> {
    match &resp.data {
        Some(serde_json::Value::Array(arr)) => arr.clone(),
        Some(v @ serde_json::Value::Object(_)) => vec![v.clone()],
        _ => Vec::new(),
    }
}

/// 取歌曲条目 id：QQ 用 `mid`（字符串），网易用 `id`（数字或字符串）。
fn vkey_item_id(src: VkSource, item: &serde_json::Value) -> Option<String> {
    let key = match src {
        VkSource::Qq => "mid",
        VkSource::Netease => "id",
    };
    match item.get(key) {
        Some(v) if v.is_string() => v.as_str().map(|s| s.to_string()),
        Some(v) if v.is_number() => v.as_i64().map(|n| n.to_string()),
        _ => None,
    }
}

/// 取歌曲标题：按常见字段名依次探测（vkeys 实际返回 `song`）。
fn vkey_item_title(item: &serde_json::Value) -> String {
    for k in ["song", "name", "title", "songname", "songName"] {
        if let Some(s) = item.get(k).and_then(|v| v.as_str()) {
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    String::new()
}

/// 取歌手：按常见字段名依次探测（字符串或数组；QQ 返回 `singer` 字符串
/// 且带 `singer_list` 数组，网易返回 `singer` 字符串）。
fn vkey_item_artist(item: &serde_json::Value) -> String {
    for k in ["singer", "singers", "singer_list", "singerList", "artist", "artists"] {
        if let Some(v) = item.get(k) {
            let s = flatten_names(v);
            if !s.is_empty() {
                return s;
            }
        }
    }
    String::new()
}

/// 把歌手字段压平为 "A / B"：字符串直接用；数组取每项 `name` 或字符串元素。
fn flatten_names(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.trim().to_string(),
        serde_json::Value::Array(arr) => {
            let names: Vec<String> = arr
                .iter()
                .filter_map(|it| {
                    if let Some(s) = it.as_str() {
                        Some(s.trim().to_string())
                    } else if let Some(s) = it.get("name").and_then(|n| n.as_str()) {
                        Some(s.trim().to_string())
                    } else {
                        None
                    }
                })
                .filter(|s| !s.is_empty())
                .collect();
            names.join(" / ")
        }
        _ => String::new(),
    }
}

/// 取时长（秒）。支持：
/// - 数字毫秒（`duration`/`dt`，>1000 时自动÷1000）
/// - 中文 interval 如 `"4分29秒"`（QQ 音乐返回格式）
fn vkey_item_duration_secs(item: &serde_json::Value) -> f64 {
    for k in ["duration", "dt"] {
        let secs = match item.get(k) {
            Some(serde_json::Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(serde_json::Value::String(s)) => s.parse::<f64>().unwrap_or(0.0),
            _ => 0.0,
        };
        if secs > 0.0 {
            return if secs > 1000.0 { secs / 1000.0 } else { secs };
        }
    }
    // QQ 音乐用中文 interval 如 "4分29秒" 或 "3分" 或 "45秒"
    if let Some(interval) = item.get("interval").and_then(|v| v.as_str()) {
        if let Some(secs) = parse_cn_interval(interval) {
            return secs;
        }
    }
    0.0
}

/// 解析中文时长格式（"4分29秒" / "3分" / "45秒"）。
fn parse_cn_interval(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0f64;
    if let Some(pos) = s.find('分') {
        let mins: f64 = s[..pos].trim().parse().ok()?;
        total += mins * 60.0;
        let rest = &s[(pos + '分'.len_utf8())..];
        if let Some(s2) = rest.find('秒') {
            let secs: f64 = rest[..s2].trim().parse().ok()?;
            total += secs;
        }
        return Some(total);
    }
    if let Some(pos) = s.find('秒') {
        let secs: f64 = s[..pos].trim().parse().ok()?;
        return Some(secs);
    }
    None
}

/// vkeys 条目 → 候选（复用标题/歌手匹配打分）。
fn vkey_item_to_candidate(src: VkSource, item: &serde_json::Value) -> Option<LrcSearchResult> {
    let id = vkey_item_id(src, item)?;
    let title = vkey_item_title(item);
    if title.is_empty() {
        return None;
    }
    Some(LrcSearchResult {
        id: id.parse().unwrap_or(0),
        track_name: title,
        artist_name: vkey_item_artist(item),
        album_name: String::new(),
        duration: vkey_item_duration_secs(item),
        instrumental: false,
        plain_lyrics: String::new(),
        synced_lyrics: String::new(),
    })
}

/// 拉取歌词文本（`mid` / `id`）。
fn vkeys_lyric_fetch(
    client: &reqwest::blocking::Client,
    src: VkSource,
    id: &str,
) -> Option<VkeyLyricData> {
    let resp = client
        .get(src.lyric_url())
        .query(&[(src.id_param(), id)])
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.json::<VkeyLyricResp>().ok()?.data
}

/// 把 vkeys 歌词数据打包成 [`Lyrics`]（合并翻译歌词）。
fn build_vkey_lyrics(data: VkeyLyricData) -> Option<Lyrics> {
    let lrc = data.lrc.as_ref().map(LyricText::text).unwrap_or_default();
    let trans = data
        .trans
        .as_ref()
        .map(LyricText::text)
        .or_else(|| data.tlyric.as_ref().map(LyricText::text))
        .unwrap_or_default();
    let (merged_lrc, plain) = merge_lrc_translation(&lrc, &trans);
    if merged_lrc.is_empty() && plain.is_empty() {
        return None;
    }
    Some(Lyrics {
        lrc: if merged_lrc.is_empty() { None } else { Some(merged_lrc) },
        plain,
        source: None,
    })
}

/// 秒 → `[mm:ss.xx]` LRC 时间标签。
fn fmt_lrc_time(secs: f64) -> String {
    let m = (secs / 60.0).floor() as u64;
    let s = secs - m as f64 * 60.0;
    format!("[{:02}:{:05.2}]", m, s)
}

/// 把翻译歌词按时间戳并入主歌词：同一句时间相差 ≤0.5s 视为对应，
/// 输出「主句\n翻译」同行（桌面歌词可整句显示）。返回 (合并 LRC, 纯文本)。
///
/// 主歌词为空 → 全部为空；翻译无时间标签（纯文本）→ 不合并，仅保留主歌词。
fn merge_lrc_translation(lrc: &str, trans: &str) -> (String, String) {
    let main = lrc::parse(lrc);
    if main.is_empty() {
        return (String::new(), String::new());
    }
    let tr = lrc::parse(trans);
    let merged_lrc: Vec<String> = main
        .iter()
        .map(|l| {
            let tr_text = tr
                .iter()
                .filter(|t| (t.time_secs - l.time_secs).abs() <= 0.5)
                .map(|t| t.text.trim())
                .find(|t| !t.is_empty() && *t != l.text.trim())
                .unwrap_or("");
            let text = if tr_text.is_empty() {
                l.text.clone()
            } else {
                format!("{}\n{}", l.text, tr_text)
            };
            format!("{}{}", fmt_lrc_time(l.time_secs), text)
        })
        .collect();
    let merged = merged_lrc.join("\n");
    let plain = merged
        .lines()
        .map(lrc::plain_line)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (merged, plain)
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

    // ---- 歌曲提示（识别音乐）参与查询生成与打分 ----

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

    // ---- vkeys.cn 解析 ----

    #[test]
    fn vkey_lyric_text_untagged_string() {
        let v: LyricText = serde_json::from_str(r#""[00:01.00]故事的小黄花""#).unwrap();
        assert_eq!(v.text(), "[00:01.00]故事的小黄花");
    }

    #[test]
    fn vkey_lyric_text_untagged_object() {
        let v: LyricText = serde_json::from_str(r#"{"lyric":"[00:01.00]故事的小黄花"}"#).unwrap();
        assert_eq!(v.text(), "[00:01.00]故事的小黄花");
    }

    #[test]
    fn vkey_search_qq_parse_array() {
        let json = r#"{"data": [
            {"mid":"003a1uRx2cRwY1","name":"晴天","singer":[{"id":4558,"mid":"...","name":"周杰伦"}],"duration":269000}
        ]}"#;
        let resp: VkeySearchResp = serde_json::from_str(json).unwrap();
        let items = vkeys_extract_items(&resp);
        assert_eq!(items.len(), 1);
        let id = vkey_item_id(VkSource::Qq, &items[0]).unwrap();
        assert_eq!(id, "003a1uRx2cRwY1");
        let title = vkey_item_title(&items[0]);
        assert_eq!(title, "晴天");
        let artist = vkey_item_artist(&items[0]);
        assert_eq!(artist, "周杰伦");
        let dur = vkey_item_duration_secs(&items[0]);
        assert!((dur - 269.0).abs() < 1.0, "got {dur}");
    }

    #[test]
    fn vkey_search_netease_parse_array() {
        let json = r#"{"data": [
            {"id":186016,"name":"晴天","artists":[{"id":6452,"name":"周杰伦"}],"duration":269000}
        ]}"#;
        let resp: VkeySearchResp = serde_json::from_str(json).unwrap();
        let items = vkeys_extract_items(&resp);
        assert_eq!(items.len(), 1);
        let id = vkey_item_id(VkSource::Netease, &items[0]).unwrap();
        assert_eq!(id, "186016");
        assert_eq!(vkey_item_title(&items[0]), "晴天");
        assert_eq!(vkey_item_artist(&items[0]), "周杰伦");
    }

    #[test]
    fn vkey_search_netease_single_object() {
        let json = r#"{"data": {"id":7,"name":"夜曲","duration":200000}}"#;
        let resp: VkeySearchResp = serde_json::from_str(json).unwrap();
        let items = vkeys_extract_items(&resp);
        assert_eq!(items.len(), 1);
        assert_eq!(vkey_item_title(&items[0]), "夜曲");
    }

    #[test]
    fn vkey_item_artist_string_singer() {
        let item: serde_json::Value = serde_json::from_str(r#"{"id":1,"singer":"周杰伦"}"#).unwrap();
        assert_eq!(vkey_item_artist(&item), "周杰伦");
    }

    #[test]
    fn vkey_item_artist_empty_when_missing() {
        let item: serde_json::Value = serde_json::from_str(r#"{"id":1}"#).unwrap();
        assert_eq!(vkey_item_artist(&item), "");
    }

    #[test]
    fn vkey_item_duration_millis_converted() {
        let item: serde_json::Value = serde_json::from_str(r#"{"duration":269000}"#).unwrap();
        let dur = vkey_item_duration_secs(&item);
        assert!((dur - 269.0).abs() < 1.0, "got {dur}");
    }

    #[test]
    fn vkey_item_duration_seconds_kept() {
        let item: serde_json::Value = serde_json::from_str(r#"{"duration":240.0}"#).unwrap();
        let dur = vkey_item_duration_secs(&item);
        assert!((dur - 240.0).abs() < 0.1, "got {dur}");
    }

    #[test]
    fn vkey_item_to_candidate_qq() {
        let item: serde_json::Value = serde_json::from_str(
            r#"{"mid":"abc","name":"晴天","singer":[{"name":"周杰伦"}],"duration":269000}"#,
        )
        .unwrap();
        let cand = vkey_item_to_candidate(VkSource::Qq, &item).unwrap();
        assert_eq!(cand.track_name, "晴天");
        assert_eq!(cand.artist_name, "周杰伦");
        assert!((cand.duration - 269.0).abs() < 1.0);
    }

    #[test]
    fn vkey_merge_lrc_translation_aligns() {
        let lrc = "[00:01.00]故事的小黄花\n[00:03.00]从出生那年就飘着";
        let trans = "[00:01.00]The yellow flower\n[00:03.00]Floating since birth";
        let (merged, plain) = merge_lrc_translation(lrc, trans);
        // 合并后的 LRC 应该包含翻译文本
        assert!(merged.contains("The yellow flower"), "got: {merged}");
        assert!(merged.contains("Floating since birth"), "got: {merged}");
        // 纯文本也应包含两行
        assert!(plain.contains("故事的小黄花"));
        assert!(plain.contains("The yellow flower"));
    }

    #[test]
    fn vkey_merge_lrc_translation_empty_trans() {
        let lrc = "[00:01.00]a\n[00:02.00]b";
        let (merged, plain) = merge_lrc_translation(lrc, "");
        assert!(merged.contains("[00:01.00]a"));
        assert_eq!(plain, "a\nb");
    }

    #[test]
    fn vkey_merge_lrc_translation_empty_lrc() {
        let (merged, plain) = merge_lrc_translation("", "[00:01.00]trans");
        assert!(merged.is_empty());
        assert!(plain.is_empty());
    }

    #[test]
    fn flatten_names_string() {
        let v: serde_json::Value = serde_json::from_str(r#""周杰伦""#).unwrap();
        assert_eq!(flatten_names(&v), "周杰伦");
    }

    #[test]
    fn flatten_names_array() {
        let v: serde_json::Value =
            serde_json::from_str(r#"[{"name":"周杰伦"},{"name":"方文山"}]"#).unwrap();
        assert_eq!(flatten_names(&v), "周杰伦 / 方文山");
    }

    #[test]
    fn flatten_names_array_of_strings() {
        let v: serde_json::Value = serde_json::from_str(r#"["周杰伦","方文山"]"#).unwrap();
        assert_eq!(flatten_names(&v), "周杰伦 / 方文山");
    }

    #[test]
    fn fmt_lrc_time_formats_correctly() {
        assert_eq!(fmt_lrc_time(0.0), "[00:00.00]");
        assert_eq!(fmt_lrc_time(61.5), "[01:01.50]");
        assert_eq!(fmt_lrc_time(3661.0), "[61:01.00]");
    }

    #[test]
    fn vkey_lyric_data_parse_without_lrc_fallback() {
        // 只有 tlyric 没有 lrc 的场景
        let json = r#"{"data":{"tlyric":{"lyric":"翻译歌词"}}}"#;
        let resp: VkeyLyricResp = serde_json::from_str(json).unwrap();
        let data = resp.data.unwrap();
        assert!(data.lrc.is_none());
        assert!(data.trans.is_none());
        assert!(data.tlyric.is_some());
        assert_eq!(data.tlyric.unwrap().text(), "翻译歌词");
    }

    #[test]
    fn lrc_plain_line_strips_tags() {
        assert_eq!(lrc::plain_line("[00:01.00]hello"), "hello");
        assert_eq!(lrc::plain_line("[00:01.00][00:03.00]aaa"), "aaa");
        assert_eq!(lrc::plain_line("[ti:title]"), "");
    }

    #[test]
    fn parse_cn_interval_min_sec() {
        assert!((parse_cn_interval("4分29秒").unwrap() - 269.0).abs() < 1.0);
    }

    #[test]
    fn parse_cn_interval_only_min() {
        assert!((parse_cn_interval("3分").unwrap() - 180.0).abs() < 0.1);
    }

    #[test]
    fn parse_cn_interval_only_sec() {
        assert!((parse_cn_interval("45秒").unwrap() - 45.0).abs() < 0.1);
    }

    #[test]
    fn parse_cn_interval_empty() {
        assert!(parse_cn_interval("").is_none());
    }

    #[test]
    fn vkey_item_title_extracts_song_field() {
        let item: serde_json::Value = serde_json::from_str(r#"{"song":"晴天","id":1}"#).unwrap();
        assert_eq!(vkey_item_title(&item), "晴天");
    }

    #[test]
    fn vkey_item_duration_parses_cn_interval() {
        let item: serde_json::Value =
            serde_json::from_str(r#"{"interval":"4分29秒","id":1}"#).unwrap();
        let dur = vkey_item_duration_secs(&item);
        assert!((dur - 269.0).abs() < 1.0, "got {dur}");
    }

    // ---- 本地歌词缓存 ----

    fn sample_lyrics(tag: &str) -> Lyrics {
        Lyrics {
            lrc: Some(format!("[00:01.00]第一句{tag}\n[00:03.00]第二句{tag}")),
            plain: format!("第一句{tag}\n第二句{tag}"),
            source: Some(LrcSearchResult {
                id: 1,
                track_name: format!("晴天{tag}"),
                artist_name: "周杰伦".into(),
                album_name: "叶惠美".into(),
                duration: 269.0,
                instrumental: false,
                plain_lyrics: String::new(),
                synced_lyrics: String::new(),
            }),
        }
    }

    #[test]
    fn lyrics_cache_key_is_stable_md5_of_bvid() {
        let k = cache_key("BV1GJ411x7h7");
        assert_eq!(k.len(), 32);
        assert_eq!(k, cache_key("BV1GJ411x7h7"));
        assert_ne!(k, cache_key("BV1xx411c7mD"));
    }

    #[test]
    fn lyrics_json_roundtrip_preserves_candidates() {
        let entry = LyricsCacheEntry {
            selected: Some(sample_lyrics("A")),
            candidates: vec![sample_lyrics("A"), sample_lyrics("B")],
            saved_at_unix: 1_700_000_000,
        };
        let text = serde_json::to_string(&entry).unwrap();
        let back: LyricsCacheEntry = serde_json::from_str(&text).unwrap();
        assert_eq!(back, entry);
        // 旧文件缺字段也能反序列化（serde default）。
        let bare: LyricsCacheEntry = serde_json::from_str("{}").unwrap();
        assert_eq!(bare, LyricsCacheEntry::default());
    }
}
