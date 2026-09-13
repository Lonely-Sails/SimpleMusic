# AGENT.md — SimpleMusic 开发指南（给后续 AI/开发者）

本项目是 **SimpleMusic**：一个极简桌面音乐播放器（Rust 2024 + eframe/egui 0.36，纯原生 GUI 无 WebView）。音源来自 B 站视频，歌词来自 vkeys.cn 聚合源（QQ 音乐/网易云）在线搜索（LRCLIB 兜底），带桌面歌词悬浮窗。

> 目标：让接手的人/Agent 用最少的时间搞清楚「项目怎么跑、代码怎么组织、哪些约定必须遵守、改哪里能加什么功能」。

---

## 1. 构建与测试（macOS）

```sh
cargo check                       # 编译检查（默认 tray feature）
cargo test                        # 单测（离线用例 + 2 个 #[ignore] 网络用例）
cargo run -- --smoke              # 无窗口模块自检，打印 SMOKE_OK 退出
cargo run                         # 真实 GUI 启动
cargo fmt                         # 格式化
```

- 开发环境为 **macOS**，cargo 已在 PATH，直接运行即可。
- **系统托盘 feature**：默认启用 `tray`，跨平台（见 `src/tray.rs` 模块注释）：
  **macOS/Windows 用系统原生托盘（NSStatusItem / Shell_NotifyIcon），无需 GTK、无额外线程**，
  图标由 `MusicApp::new` 在主线程创建（macOS 要求事件循环运行中创建）；
  Linux 走独立 GTK 线程 + libappindicator（需系统装 GTK3）。
- **lib/bin 双 target**：`src/lib.rs`（库目标，crate 名 `simple_music`）+ `src/main.rs`
  （薄壳：命令行解析 + `--smoke` + eframe 启动）。业务代码全在 lib 里，`examples/` 探针
  直接 `use simple_music::…`，**不要再用 `#[path]` 桥接复制源码树**。
- **图标字体** `assets/Phosphor.ttf`（约 0.5MB，MIT）编译期 `include_bytes!` 进二进制，恒定注册；
  **主界面文字字体恒内嵌** Noto Sans SC（跨机器观感一致，不再探测系统字体）；
  **桌面歌词字体**由设置 `Settings::lyrics_font`（`LyricsFont` 枚举）决定：
  `FollowUi`（跟随界面 = 内嵌，兼容旧 `auto`）/ `Embedded` 内嵌 Noto Sans SC、
  `Specific(路径)` 用户挑选的系统字体。歌词文字走**专用 named family**
  `fonts::lyrics_family()`（浮窗与主窗口共享 Context 的 FontDefinitions，无法按窗口
  选字体，named family 是唯一解耦手段）；系统字体加载前用 skrifa 校验可解析
  （`font_file_is_loadable`）——egui 对解析失败的字体直接 panic；文件失效回退内嵌。
  **缺字净化**：内嵌字体没有 emoji/PUA 字形，缺字会渲染成「?」占位——动态文本
  显示入口统一走 `fonts::sanitize_text`（必删字符类 + 内嵌 Noto cmap 覆盖判定，
  见 `util/text.rs` 模块文档）；新增显示网络文本的地方必须接净化。
  环境变量：`SIMPLEMUSIC_EMBEDDED_FONTS=1` 强制全内嵌、
  `SIMPLEMUSIC_FONT=/path/to.ttf` 手动指定。测试统一用
  `fonts::install_fonts(&ctx, &LyricsFont::Embedded)`（度量不随宿主系统字体漂移）。
- 已有 git 仓库（分支 `main`）：改动用增量编辑，提交信息用中文、说明动机；`SimpleMusic.zip`
  手动备份包与 `target/` 均已在 `.gitignore` 中排除。

---

## 2. 架构总览

代码按「分层 + 按职责拆文件」组织，**业务全在 lib 目标**：

- `app/` 应用层（UI + 状态 + 异步调度，全是 `impl MusicApp` 块）；
- `modules/` 领域模块（B 站客户端 / 音频引擎 / 歌词 / 持久化，无 UI 依赖）——
  三个超过千行的模块已拆成**目录 + 子模块**，每个子模块内聚一个职责，
  `mod.rs` 只放模块文档、子模块声明、跨子模块常量与公共 API re-export；
- `util/` 纯函数工具（无 egui 依赖，全部带单测）；
- 顶层文件是主题/图标/字体/封面/托盘等基础能力。

```
src/
├── lib.rs            库目标：模块声明 + 模块地图文档（main.rs/examples 都从这里引用）
├── net.rs            全局异步网络层：唯一 tokio runtime(2 worker) + 共享 HTTP 客户端 + spawn/block_on 派发
├── main.rs           薄壳：解析 --width/--height/--smoke；run_smoke 自检；eframe 启动
├── app/              应用层（均为 `impl MusicApp` 块）
│   ├── mod.rs        MusicApp 结构 + new() + 跨模块小工具 + eframe::App 实现（ui/logic/on_exit）
│   ├── messages.rs   异步网络任务 AsyncMsg + spawn_* 派发 + handle_msg
│   ├── player.rs     播放控制（上下曲/seek/音量/移除）+ 快捷键 + playback_songs() 快照
│   ├── playlists.rs  歌单管理（切换/删除/重命名/添加到歌单/在线歌单定位）
│   ├── lyrics.rs     歌词同步（update_lyrics_line + pick_plain_line_index + next_switch_delay_secs）
│   ├── window.rs     窗口关闭/隐藏 + 托盘事件轮询
│   └── ui/           主界面组件，按区域一文件（mod/widgets/title_bar/status_bar/
│                     playlist_bar/song_list/import/player_bar/settings/login/lyrics_viewport）
├── modules/
│   ├── bilibili/     B 站客户端（原单文件拆 8 个子模块）
│   │   ├── mod.rs    模块地图 + 常量(USER_AGENT/REFERER/ORIGIN) + pub use（对外路径 modules::bilibili::* 不变）
│   │   ├── error.rs  BiliError/BiliResult
│   │   ├── models.rs 对外数据模型(VideoInfo/StreamUrl/FavFolder/MusicHint…) + API 响应结构体(pub(super))
│   │   ├── wbi.rs    WBI 签名（mixin_key/md5_hex/wbi_sign_params/WbiKeys）
│   │   ├── client.rs BiliClient 基座：HTTP 构建/会话/信封解包/get_json/get_data
│   │   ├── login.rs  扫码登录方法组（generate_qrcode/qrcode_matrix/poll_login）
│   │   ├── fav.rs    收藏夹方法组（list_favorite_folders/list_favorite_resources）
│   │   ├── resolve.rs BV 解析/video_info/resolve_stream(DASH 优先)/detect_music 识别音乐
│   │   ├── stream_cache.rs 已解析直链缓存（键 (bvid,音质)，TTL 10min）+ 直链过期判定
│   │   └── util.rs   纯函数（pick_dash_audio/scan_bv_token/parse_set_cookie/dedup_folders）
│   ├── audio/        音频引擎（原单文件拆 8 个子模块）
│   │   ├── mod.rs    架构文档 + re-export（default_cache_dir/cache_path_in/PlaybackStatus/PlayRequest/AudioEngine）
│   │   ├── control.rs 协议层：PlaybackStatus(UI 只读)/Command(mpsc)/PlayRequest
│   │   ├── cache.rs  缓存路径规则(<dir>/<md5(key)>.m4s)与命中判定
│   │   ├── decode.rs symphonia 解码源 SymphoniaSource（文件/内存输入、seek、position 推算）
│   │   ├── normalize.rs 响度均衡：整曲预分析(RMS dBFS) → 固定增益 + 硬限幅
│   │   ├── download.rs fetch_to_cache 异步流式下载(.part 原子重命名)/CDN 备援/降级内存
│   │   ├── player.rs worker_loop 播放线程主循环 + load_and_play + LoadErr
│   │   └── engine.rs AudioEngine UI 句柄（play/pause/resume/seek/stop/volume/normalize/status）
│   ├── lyrics/       歌词（原单文件拆 8 个子模块）
│   │   ├── mod.rs    模块地图 + 端点常量 + re-export（对外路径 modules::lyrics::* 不变）
│   │   ├── model.rs  SongHint/LrcLine/LrcSearchResult/Lyrics 数据模型
│   │   ├── lrc.rs    LRC 解析 + 按播放位置同步引擎（pub mod，纯函数）
│   │   ├── query.rs  标题清洗 clean_title 与查询词生成 search_queries*
│   │   ├── matching.rs 打分 match_score*/best_match*（含 SongHint 校准 + 阈值判定）
│   │   ├── cache.rs  本地歌词缓存条目语义（cache_key/lookup/update_selected/store_fetch）
│   │   ├── lrclib.rs LyricsProvider + LRCLIB HTTP（search/get）
│   │   ├── vkeys.rs  vkeys.cn 聚合源（QQ 音乐/网易云）搜索/歌词/翻译合并
│   │   └── text.rs   文本低层工具（书名号/括号注释/分隔符/Levenshtein）
│   └── storage.rs    配置/会话/歌单 JSON 持久化（write_json_at 统一落盘；BiliSession Debug 已脱敏）
├── state.rs          数据模型：PlaybackState / QueueItem / Playlist / PlayMode / AudioQuality / Settings
├── text_shadow.rs    文字真·模糊柔影：skrifa 轮廓 → vello_cpu 光栅化 → 盒滤波高斯 → egui 纹理
├── theme.rs          主题色板 + 按钮/样式辅助（BG_*/TEXT_*/ACCENT 等语义常量）
├── icons.rs          图标：内嵌 Phosphor 图标字体（PUA 码点），不依赖 emoji/系统字形
├── cover.rs          封面缩略图：异步下载 + 解码（不在主线程，最多 6 个并发任务）→ egui 纹理缓存（失败 30 分钟冷却；高/低双优先级队列：可视区域与头像插队）
├── fonts.rs          字体：主界面恒内嵌 Noto Sans SC + Phosphor；桌面歌词专用 family（设置可选系统字体）+ 缺字净化
├── util/             fmt.rs(format_secs) / rand.rs(rand_idx) / filter.rs(song_matches_query) / text.rs(sanitize_ui_text 缺字过滤) / log.rs(分级日志)
└── tray.rs           系统托盘（feature=tray）：Linux=GTK 线程；macOS/Win=原生；无 feature 时 no-op 桩
```

> **模块拆分约定**：任何文件超过 ~500 行就按职责拆目录。`BiliClient`/`AudioEngine` 等
> 结构体跨子模块用多个 `impl` 块挂方法；跨子模块可见性用 `pub(super)`（不要无脑 `pub`），
> 测试跟随被测代码放同文件 `#[cfg(test)] mod tests`；`mod.rs` 负责「地图 + re-export」，
> 不放逻辑。examples/ 下的探针是手工网络诊断工具，依赖 lib 目标的 pub API。

### 线程模型（最重要的一条约定）
- **所有网络/IO 都是异步的，统一跑在 `net` 模块里的单个 tokio runtime**（`src/net.rs`，`new_multi_thread().worker_threads(2)`，线程名 `simple-music-net`）。**不要**再新建 `reqwest::blocking::Client`——它每实例起一条 `reqwest-internal-sync-runtime` OS 线程，会造成线程数无界增长（历史 bug：歌词抓取时线程从 12 涨到 25）。共享客户端走 `net::http_client()`（`OnceLock`，连接池 + 10s connect timeout），逐请求用 `.timeout(..)`/`.header(..)` 叠加。
- **派发 API**：`net::spawn(future)`（`F: Future + Send + 'static`）、`net::spawn_blocking(f)`（CPU/阻塞活，如字体扫描、封面解码）、`net::block_on(future)`（非 async 上下文桥接，如 `main.rs` smoke、examples、播放线程；基于 `Handle::block_on`，**不要求 Future: Send**）。
- **结果经单个 `mpsc` 通道** `AsyncMsg` 发回主线程；`MusicApp::logic` 每帧 `try_recv` 排空并更新状态。
- **`BiliClient` 用 `Arc<tokio::sync::Mutex<..>>`**（不是 `std::sync::Mutex`）：其 guard 是 `Send`，能跨 `.await` 持有；`std::sync::MutexGuard` 不是 `Send`，跨 await 会让 `tokio::spawn` 拒绝编译。锁跨 await 处务必短临界区（见 `resolve.rs::wbi_keys` 的三段式：查缓存 → 放锁请求 → 回写）。UI 线程读它用 `try_lock()` 避免卡帧。
- **播放仍在专用 OS 线程**（`simple-music-audio`，`audio/player.rs::worker_loop`）：rodio `OutputStream` 是 `!Send`，只能待在专属线程。该线程需要下载时用 `net::block_on(fetch_to_cache(..))` 桥进 runtime。`AudioEngine` 仅在 UI 线程持有，命令经 mpsc 发往播放线程，状态经 `Arc<Mutex<PlaybackStatus>>` 轮询。
- **重绘保活线程**（`app/mod.rs`，线程名 `render-keepalive`，200ms 一拍）：后台线程持续
  `ctx.request_repaint()`，规避 eframe/winit「最小化后恢复界面卡死」的上游缺陷
  （egui #8246：macOS 上 `ViewportCommand::Minimized(true)` 把 `info.minimized` 锁死；
  egui #5136 / PR #8414：合成器扣留 frame callback）。配套 `logic` 里检测「egui 认为最小化
  但窗口已恢复」的矛盾态并补发 `Minimized(false)` 清锁。**改最小化/托盘隐藏相关代码前先读
  这两个 issue**；`on_exit` 里必须置位 `keepalive_stop`。经验：这类「只在最小化/恢复后出现
  的冻结」不是死锁，是事件循环饿死——应用层保活 + 恢复补绘兜底。
- **预期线程数 ≈ 9**：main + NSEventThread + render-keepalive + simple-music-audio + 2× simple-music-net + 1 匿名 + macOS GCD dispatch。**不应出现 `reqwest-internal-sync-runtime`**（出现即说明某处又建了 blocking 客户端）。
- **桌面歌词浮窗**通过 `egui::Context::show_viewport_deferred`（延迟模式）渲染，**不与主窗口共享
  重绘节奏**：浮窗只在内容指纹（当前句/下一句/字号/锁定）变化或输入事件时重绘。切歌过渡动画的
  过渡状态存在共享 `Context` data 槽（`TRANSITION_SLOT`），动画期间 `request_repaint()` 连续唤醒
  浮窗自身——呈现节奏交给 vsync/合成器对齐（egui 内建动画同款）；固定 1/60s 定时器会与 vblank
  相位漂移（帧距 16.7/33.4ms 交替），观感一顿一顿，**不要改回去**。跨 viewport 通信一律走
  data 槽（`IdTypeMap`，读后即删），deferred 闭包是 `Fn + 'static`，不能借用 `&mut self`。
- **主窗口播放时按 5Hz 节流自醒**（`PLAY_REPAINT_INTERVAL`，`app/mod.rs::logic`）：醒来时刻取
  「节流间隔」与「下一个歌词切换点 − 20ms 提前量」的较早者（`app/lyrics.rs::next_switch_delay_secs`），
  进度条平滑且切行动画不迟到。**不要恢复 `playing ⇒ request_repaint()` 的全速连续重绘**——
  浮窗动画期间主窗口全速重绘会在 winit 全局重绘队列里互相踩踏，是浮窗掉帧主因。
- UI 闭包里禁止直接做网络请求；需要结果就 `spawn_*` 一个异步任务（`net::spawn`）+ 发消息。
- **egui `Context` 是一把大写锁**（`RwLock<ContextImpl>`，memory/data、viewports、input、
  fonts 全在里面；epaint RwLock **不可重入**）。**绝不能在 `ctx.data_mut`/`data`/`input`/
  `fonts` 等闭包里再调任何 `Context` API**（如 `load_texture`、`pixels_per_point`）——
  同线程递归加锁 = 死锁，debug 构建 10s 后 `DEBUG PANIC: Failed to acquire RwLock`、
  release 永久挂死（macOS 上 panic 发生在 `draw_rect` 里无法 unwind，直接 abort）。
  正确写法：拆成「查（短临界区）→ 锁外做事（光栅化/load_texture）→ 写（短临界区）」，
  见 `text_shadow.rs::ShadowCache` 的 get/insert 两段式设计。

### 播放列表语义（改播放相关代码前必读）
- **当前选中的歌单就是播放列表，没有独立的播放队列**。`MusicApp` 只有
  `current_bvid: Option<String>`（按 bvid 记住正在播哪首），**绝不在播放时把歌写进歌单**。
- 播放列表快照由 `app/player.rs::playback_songs()` 按需构建：本地歌单取 `songs`，
  在线歌单取已加载的收藏夹条目 `fav_items`（在线歌单的 `songs` 永远为空，只是收藏夹引用）。
  上下曲/随机/曲终自动切歌/列表高亮全部基于该快照，**按 bvid 定位，不按下标**。
- 入单只有两条显式路径：右键「添加到歌单」和链接导入「添加并播放」（静默去重；在线歌单只读不入单）。
- 启动时会自动清空在线歌单 `songs` 的历史残留（旧版隐式入列的脏数据，别删这段清理）。

### 播放链路
点播 → **直链缓存查命中**（`modules/bilibili/stream_cache.rs`，键 `(bvid, 音质)`，
TTL `STREAM_CACHE_TTL` = 10 分钟）→ 命中则跳过 `video_info + playurl` 直接播放，
未命中走 `resolve_stream`（DASH 音频流，未签名被风控自动补 WBI 重试）→ 带 `required_headers`
(UA/Referer/Cookie) 流式下载到缓存 `~/.cache/simple-music/audio/<md5(bvid)>.m4s`
（二次秒开；损坏缓存解码失败自动删除重下）→ symphonia(AAC/MP4) 解码 → rodio 输出；
CDN 403/410 自动换备用地址；写盘失败降级内存缓冲；无输出设备绝不 panic，进错误状态。

**音量均衡（响度归一化，默认关闭）**：开启后（设置页「播放」）在**开始出声前**由播放线程
同步扫一遍整曲解码流（`modules/audio/normalize.rs::analyze`，累计样本平方和得 RMS dBFS），
推导固定增益（目标 -18 dBFS，钳在 ±12 dB）后包一层 `NormalizeSource` 乘增益并硬限幅；
播放期间增益恒定（无抽气效应）。分析可被新命令打断（复用 `download::poll_abort`），
期间 `PlaybackStatus.normalizing=true`；分析失败降级为原音量播放，不阻断出声。
开关**只对后续播放生效**（`Command::SetNormalize`），避免播到一半响度跳变。

**直链缓存为什么要 TTL**：B 站 playurl 直链带签名，CDN 侧会过期，缓存过期直链必然 403。
所以：① TTL 内（10 分钟）才复用，超时重新解析；② 下载报 403/410/404 时
（`is_stream_expired_error`）丢弃该曲直链并**强制重新解析重试一次**
（`stream_retried` 集合保证每首歌只自动重试一次，防死循环；用户重新点播会重置该标记）；
③ 登出清空（直链带 Cookie 签名，换账号不能复用）。
条目里连 `QueueItem` 一起存，命中时标题/时长/封面/cid 都不缺，也免一次 `video_info`。

### 歌词链路
切歌 → 后台 `spawn_lyrics_fetch`（歌词线程，绝不阻塞播放解析）→ **先查本地歌词缓存**
（`~/.cache/simple-music/lyrics.json`，按 bvid 的 md5 键控；命中即零网络回放，**用户手选的
歌词优先于自动结果**）→ 未命中再调 `BiliClient::detect_music(bvid, cid)` 问
**B 站「识别音乐」**（`/x/player/v2` 的 `bgm_info` → `tag_type=bgm` TAG → 官方曲库
曲名/歌手，全程无需登录；失败返回 `None`，纯增强不阻塞）→ 把官方词作为 `SongHint` 传给
`LyricsProvider::fetch_all_with_hint` → 按候选查询（提示词最优先，标题清洗词兜底，去重 ≤5 条）
依次尝试 **vkeys.cn 聚合源**（QQ 音乐 `mid` 优先 → 网易云 `id`，翻译按时间戳并入同行）→
全部未命中再回退 **LRCLIB**（搜索 + 精确 GET；打分阈值 `MIN_ACCEPT_SCORE=40`，命中判定统一走
`matching::best_match_if_acceptable`）→ 抓取成功写回缓存并当场落盘（异步任务）；
`LyricsFetched{key, candidates, selected}` 按 bvid 回主线程；**用户手选**走 `apply_lyrics`
（应用 + 写缓存 + 落盘）；同步歌词用二分定位当前句，无同步时按进度近似取纯文本行。

**歌词时间校准（全局偏移）**：设置项 `Settings::lyrics_offset_secs`（正 = 歌词延后出现，
默认 0，钳在 ±60s），入口在播放条「T」歌词弹窗底部的「-1s / +1s / 重置」。
`app/lyrics.rs::shift_lines` 在 `apply_lyrics_inner` 里把解析出的时间轴整体平移后再参与同步；
`adjust_lyrics_offset` 只平移已有时间轴并立即重算当前句，**不重新抓取、不改歌词原文**
（缓存里仍是原始 LRC），因此可随时调节、可归零，且是全局设置对所有曲目生效。
弹窗用 `PopupCloseBehavior::IgnoreClicks` 以便连续点按（选歌词项仍显式 `ui.close()`）。

---

## 3. 数据与持久化（macOS 路径）

```
~/.config/simple-music/config.json      设置（桌面歌词开关/锁定/字号/位置/歌词字体/歌词时间偏移/音量/音量均衡/音质/播放模式）
~/.config/simple-music/session.json     B 站登录态 Cookie（权限 0600，Debug 已脱敏）
~/.config/simple-music/playlists.json   所有歌单（本地 + 在线引用）
~/.config/simple-music/playlist.json    旧版单队列文件（读取时自动迁移，随后删除）
~/.cache/simple-music/audio/            音频缓存（按 bvid 的 md5，损坏自动重下）
~/.cache/simple-music/lyrics.json       歌词缓存（按 bvid 的 md5：上次生效歌词 + 全部候选）
```

- 设置每 5 秒兜底保存 + 退出保存；歌单变更 `queue_dirty` 后 2 秒防抖保存。
- `QueueItem`/`Settings` 等带 `#[serde(default)]` 保证旧 JSON 兼容。
- 落盘统一走 `storage.rs::write_json_at`（自动建目录；会话文件传 0600 权限）。

---

## 4. 核心约定 / 改代码前必看

1. **借用手法**：`app/` 下各 UI 文件里大量「先 clone 数据（`rows`/`fav_items`/`snapshot`），再进闭包操作 `self`」来绕开借用检查。新增 UI 逻辑时沿用此模式，不要在闭包内同时 hold 两个 `&mut self` 借用。
2. **窗口/弹窗开关**：用 `let mut open = self.xxx; ... .open(&mut open).show(...); self.xxx = open;` 模式。**闭包内不能改 `open`**——需要「操作后关窗」用外部 `let mut close_after = false;` 捕获进闭包，`show` 之后再改 `open`。
3. **egui 自动 id 漂移与输入法（踩过的大坑）**：`TextEdit` 不给 `id_salt` 时用同 Ui 内自增的盐，id 随前面控件数量变化——条件渲染的按钮一插入，输入框 id 漂移导致失焦，中文输入法组合被打断（egui-winit 检测到无焦点即关 IME）。**输入框一律显式 `id_salt`**；条件出现/消失的相邻控件要常驻占位（分配空间不绘制），避免布局跳动连带 id 漂移。无头 egui 测试：必须先 `fonts::install_fonts(&ctx, &LyricsFont::Embedded)`（默认字体表为空，光标定位会坍缩）；模拟打字时 Key press/release 与 `Event::Text` 必须同帧投递（复刻 egui-winit 行为）。
4. **键盘快捷键**在 `app/player.rs::handle_shortcuts`（`logic` 里调用），用 `ctx.memory(|m| m.focused().is_none())` 判断无输入焦点才生效。新增快捷键加在这里，别散落到 UI 闭包里。
5. **主题**：一律用 `theme::` 语义色常量，不要写魔法色值；按钮样式用 `theme::primary_button`/`theme::small_button`。
6. **图标**：所有界面图标用 `icons::*`（内嵌 Phosphor，PUA 码点渲染到 rect 中心），不要依赖 emoji 或媒体控制码点（跨平台字形缺失会显示 "?"）。
7. **错误处理**：音频错误不 panic，写 `PlaybackStatus.error` 由 UI 展示；网络错误经 `AsyncMsg` 回 `ui_error`（红色）或 `notice`（金色轻提示，4 秒）。
8. **文本宽度**：动态文案先 `truncate_label`/`fit_text` 再 `painter.text`。
9. **单测**：纯函数（解析/打分/格式化/过滤）放同文件 `#[cfg(test)] mod tests`，离线跑；真实网络用 `#[ignore]` 标注（如 `detect_music_live`）。新增纯逻辑尽量带测试。测试数 221 + 2 ignored。
10. **UI 状态与数据解耦（稳定标识模式）**：凡是「UI 里选中的东西」跨帧/跨列表操作要记住时，**存稳定标识（如 bvid），不要存列表下标**——下标在过滤/删歌/刷新后静默漂移出 bug，标识找不到时按 `None` 处理即可自然降级。
11. **不要让「执行动作」顺手改数据**：副作用（入单/落盘/置 dirty）必须由用户的显式操作触发；新功能如果发现自己「顺手」改了用户数据，几乎一定是设计错了。
12. **行为不变量改动要写迁移/清理**：改持久化语义时在启动路径加一次性数据清理，并考虑旧文件兼容（`#[serde(default)]`）。
13. **死代码零容忍**：不加 crate 级 `#![allow(dead_code)]`；公开但无调用方的 API 直接删（git 里永远找得回来）。清理时连同其单测一起删。
14. **提交规范**：每完成一个功能/修复后 **立即提交**（`git add -A && git commit`），不要攒。提交消息格式：前缀 `修复：`/`优化：`/`重构：`/`更新：`/`新增：`/`移除：` + 中文正文（改了什么 + 为什么改），多步重构按步骤分提交。

---

## 5. 功能清单（现状）

- **B 站扫码登录**：二维码 + 轮询（86101/86090/86038），Cookie 持久化、日志脱敏。
- **音源（仅两种入口，刻意无搜索）**：① 收藏夹 → 在线歌单（分页加载，只读）；② 链接导入（BV 号 / `/video/BV..` / `b23.tv` 短链）。
- **播放**：播放/暂停、上下曲、进度条 seek、音量、曲终自动下一首、加载进度、三种切歌模式（顺序循环/单曲循环/随机）、音质偏好（64/128/320k/无损）、音量均衡（响度归一化，默认关闭，整曲预分析 + 固定增益）。
- **播放列表语义**：**当前选中的歌单就是播放列表**，没有独立队列；播放时不隐式写歌进歌单。
- **封面**：列表/播放条圆角缩略图，异步 + 内存缓存 + 占位图。调度分高/低两级：
  头像、当前播放曲目、**当前可视区域内**的列表行走高优先级队列（滚动到哪先出哪，
  已在低优先级队列里的任务被命中时就地提升），其余后台预取走低优先级。
- **桌面歌词**：透明置顶无边框悬浮窗，当前句+下一句预览（带 skrifa+vello_cpu 离屏光栅化的
  真·模糊柔影，见 `text_shadow.rs`，等价 CSS text-shadow；纹理按文本缓存，过渡动画期间复用），
  可拖动/锁定(鼠标穿透)/调字号，位置随浮窗重绘实时记录进设置并持久化（仅 X11），重启后自动恢复；
  切歌淡入淡出过渡（vsync 对齐，见线程模型）。歌词文字用**专用字体 family**
  （`fonts::lyrics_family()`），设置页可单独选「桌面歌词字体」（跟随界面/内嵌/系统字体），
  切换后需 `clear_shadow_cache` 失效柔影缓存并唤醒浮窗重绘（柔影缓存键不含字体维度）。
  浮窗**所有**文本（含「等待播放…」占位）都必须走 `fonts::lyrics_font_id`——
  占位曾是唯一常显文本却用了主界面字体，导致换字体看似无效（已修复，勿回退）。
- **歌词**：B 站「识别音乐」生成优先查询词 + 视频时长校准打分；vkeys.cn 聚合源自动搜索 + LRC 时间轴同步，翻译并入；LRCLIB 兜底；本地缓存 + 手选持久化；**时间校准**（全局 ±1 秒微调，见播放条「T」弹窗）。
- **LRC 空行不变量**：`lrc::parse` 丢弃「只有时间标签、正文为空」的行（如 `[00:03.38]`）——QQ 音乐用它们做换行节奏填充。保留会让同步引擎切到空句，而浮窗把空文本当「未播放」画出「等待播放…」占位，与正常歌词交替闪烁。旧缓存里的原始 LRC 无需迁移（在读取时丢弃）。
- **歌单**：本地歌单增删改（管理窗口）；在线歌单（B 站收藏夹引用，可删）。
- **歌单内搜索**：标题/UP 主实时过滤（本地与在线列表都有）。
- **键盘快捷键**：`空格` 播放/暂停，`←/→` 快退/快进 5s，`↑/↓` 音量 ±5%，`N/P` 上下曲。
- **右键菜单**：歌曲项复制 BV 号、添加到/收藏到其他本地歌单。
- **歌词选择**：播放条「T」按钮弹出多源候选（vkeys/LRCLIB），点选切换；弹窗底部带歌词时间校准（-1s / +1s / 重置）。
- **系统托盘**：显示/隐藏/退出菜单；关闭按钮隐藏到托盘（托盘可用时）。

---

## 6. 已知限制 / 可能的下一步

- 多 P 视频只取 P1（`video_info` 的 `pages` 未逐 P 展开）。
- 不支持 av 号导入（`parse_bvid` 明确只认 BV）。
- 切换歌单会停止当前播放（播放列表 = 选中歌单的直接推论）；没有跨歌单的播放队列。
- 在线歌单只显示已加载页，搜索也只过滤已加载页。
- 桌面歌词位置仅 X11 会话下记录/恢复；原生 Wayland 由合成器决定窗口位置，跳过记录以防写进占位坐标。
- 无全局媒体快捷键（如系统级播放/暂停）、无音量静音键。
- 本地歌单内歌曲不可拖拽排序。
- 无播放历史/最近播放记录。
- 若要加功能：UI 增量放 `app/ui/` 对应 `show_*` 文件；跨线程新数据用 `AsyncMsg` 变体 +
  `messages.rs::handle_msg` 分支；新增纯逻辑放对应模块 + 单测；领域逻辑超过 ~500 行
  按「模块目录」约定拆分。

---

## 7. 快速定位表

| 想改什么 | 去哪个文件/函数 |
|---|---|
| 主界面布局/顶部栏 | `app/ui/mod.rs::show_main` |
| 自定义标题栏/窗口控制 | `app/ui/title_bar.rs::show_custom_title_bar` / `show_resize_grip` |
| 窗口关闭/隐藏、托盘事件 | `app/window.rs::request_close` / `poll_tray_events`；`tray.rs` |
| 播放条（进度/音量/切歌模式） | `app/ui/player_bar.rs::show_player_bar` |
| 歌单选择 + 管理 | `app/ui/playlist_bar.rs::show_playlist_selector` / `show_playlist_manage_window` |
| 本地歌曲列表 | `app/ui/song_list.rs::show_local_songs` |
| 在线收藏夹列表 | `app/ui/song_list.rs::show_online_songs` |
| 设置窗口 | `app/ui/settings.rs::show_settings_window`（左侧分类导航 `SettingsTab` + 右侧滚动；歌词字体选择器 `lyrics_font_picker`） |
| 桌面歌词悬浮窗 | `app/ui/lyrics_viewport.rs::show_lyrics_viewport` |
| 缺字净化（emoji/PUA 过滤） | `util/text.rs::sanitize_ui_text` + `fonts.rs::sanitize_text`（动态文本显示入口统一调用） |
| 扫码登录弹窗 | `app/ui/login.rs::show_login_window` |
| 快捷键 | `app/player.rs::handle_shortcuts` |
| 播放控制（上下曲/seek/移除） | `app/player.rs` |
| 播放列表快照/当前曲目定位 | `app/player.rs::playback_songs` / `current_bvid`（`app/mod.rs`） |
| 歌单增删改/切换 | `app/playlists.rs` |
| 歌词同步（当前句/下一句） | `app/lyrics.rs` |
| 歌词时间校准（全局偏移） | `app/lyrics.rs::shift_lines` / `adjust_lyrics_offset`；入口 `app/ui/player_bar.rs` 的「T」弹窗 |
| 异步消息类型与分发 | `app/messages.rs::AsyncMsg` + `handle_msg` |
| B 站登录/收藏夹 | `modules/bilibili/{login,fav}.rs` |
| B 站取流/识别音乐 | `modules/bilibili/resolve.rs` |
| 直链缓存（TTL 10min）/ 过期判定 | `modules/bilibili/stream_cache.rs` |
| WBI 签名 | `modules/bilibili/wbi.rs` |
| 音频引擎对外接口 | `modules/audio/engine.rs`（`AudioEngine`） |
| 音频下载/缓存 | `modules/audio/{download,cache}.rs` |
| 音频解码/播放线程 | `modules/audio/{decode,player}.rs`（下载经 `net::block_on` 桥入 runtime） |
| 音量均衡（响度归一化） | `modules/audio/normalize.rs`（设置项 `Settings::volume_normalize`，默认关闭） |
| 歌词搜索/打分 | `modules/lyrics/{lrclib,vkeys,matching}.rs` |
| LRC 解析/同步 | `modules/lyrics/lrc.rs`（`parse` 丢弃无正文的行，勿回退） |
| 歌词缓存 | `modules/lyrics/cache.rs` + `modules/storage.rs` |
| 持久化 | `modules/storage.rs` |
| 数据模型/设置 | `state.rs` |
| 格式化/随机数/搜索过滤 | `util/fmt.rs` / `util/rand.rs` / `util/filter.rs` |
| 主题色板/按钮样式 | `theme.rs` |
| 图标（Phosphor） | `icons.rs` |
| 封面下载/缓存 | `cover.rs` |
| 网络诊断探针 | `examples/{bili_probe,audio_probe,lyrics_probe,lyrics_vkeys_probe}.rs`（`cargo run --example …`） |
