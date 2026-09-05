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
//! # 子模块地图（`BiliClient` 式拆分：数据模型 / 解析 / 查询 / 打分 / 缓存 / 双数据源）
//! - [`model`]：`SongHint` / `LrcLine` / `LrcSearchResult` / `Lyrics` 数据模型；
//! - [`lrc`]：LRC 文本解析 + 同步引擎（纯函数）；
//! - [`query`]：标题清洗与查询词生成（含 `SongHint` 提示词）；
//! - [`matching`]：候选打分与最佳命中（含 `SongHint` 校准）；
//! - [`cache`]：本地歌词缓存条目语义；
//! - [`lrclib`]：`LyricsProvider` + LRCLIB HTTP；
//! - [`vkeys`]：vkeys.cn 聚合源（QQ/网易）解析与歌词拉取；
//! - [`text`]：文本清洗低层工具（书名号/括号/分隔符/Levenshtein）。
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
//!   （或只用 `LrcLine.plain`，不要自行写标签解析）。
//! - **前奏**：`current_line_index` 在 `pos_secs < 第一句时间` 时返回 0，UI 若想显示
//!   "前奏" 可比较 `pos_secs < lines[0].time_secs` 并单独渲染，否则直接显示第 0 句。

mod cache;
pub mod lrc;
mod lrclib;
mod matching;
mod model;
mod query;
mod text;
mod vkeys;


pub use cache::{cache_key, cache_lookup, cache_store_fetch, cache_update_selected, LyricsCacheEntry};
pub use model::LrcLine;
pub use lrclib::LyricsProvider;
pub use matching::{best_match_with_hint, match_score, match_score_with_hint};
pub use model::{Lyrics, LrcSearchResult, SongHint};
pub use query::{clean_title, search_queries, search_queries_with_hint, usable_uploader};
pub use text::{lev_similarity, sanitize_preserving_case};

/// LRCLIB 应用的 User-Agent（LRCLIB 要求标识应用，禁止默认 curl UA）。
/// 同时用作 vkeys 请求 UA——两站都只是要求可识别的应用标识。
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
