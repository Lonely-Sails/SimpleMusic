# AGENT.md — SimpleMusic 开发指南（给后续 AI/开发者）

本项目是 **SimpleMusic**：一个极简桌面音乐播放器（Rust 2024 + eframe/egui 0.36，纯原生 GUI 无 WebView）。音源来自 B 站视频，歌词来自 vkeys.cn 聚合源（QQ 音乐/网易云）在线搜索（LRCLIB 兜底），带桌面歌词悬浮窗。

> 目标：让接手的人/Agent 用最少的时间搞清楚「项目怎么跑、代码怎么组织、哪些约定必须遵守、改哪里能加什么功能」。

---

## 1. 构建与测试（沙箱环境必读）

> ⚠️ **沙箱里 cargo 不在 PATH**，直接 `cargo` 会报 `command not found`；直接 `rustup` 会因 HOME 无写权限失败。**必须先 source 工具链环境**：

```sh
cd /data/dsh/home/SimpleMusic
source .toolchain/env.sh          # 设置 RUSTUP_HOME / CARGO_HOME / PATH / CC / 链接器 等
cargo check                       # 编译检查（默认 tray feature）
cargo test --no-default-features  # 单测（当前 163 个，全部离线；为什么加 flag 见下）
cargo run -- --smoke              # 无窗口模块自检，打印 SMOKE_OK 退出（会走少量网络，失败不阻断）
cargo run                         # 真实 GUI 启动（需要显示环境）
```

- 工具链/依赖全部离线缓存于 `.toolchain/`；`.sysroot/` 是构建系统根（gcc/alsa/x11 库）。
- **沙箱里跑测试必须 `--no-default-features`**：默认 `tray` feature 要链接 GTK3/libxdo，
  沙箱没有这些库，`cargo test` 会在**链接期**报 `unable to find library -lgtk-3`——
  注意 `cargo check` 是能过的，别被「check 绿了」骗去跑 test 再白白踩一次。
  反过来，提交前 `cargo check`（默认 feature）也要跑：托盘相关代码只在默认 feature 下编译。
- **系统托盘 feature**：默认启用 `tray`，跨平台（见 `src/tray.rs` 模块注释）：
  Linux 走独立 GTK 线程 + libappindicator（需系统装 GTK3；沙箱无 GTK 库，改用
  `cargo build --no-default-features` 跳过托盘，GUI 其余功能不受影响）；
  **macOS/Windows 用系统原生托盘（NSStatusItem / Shell_NotifyIcon），无需 GTK、无额外线程**，
  图标由 `MusicApp::new` 在主线程创建（macOS 要求事件循环运行中创建）。
- **图标字体** `assets/Phosphor.ttf`（约 0.5MB，MIT）编译期 `include_bytes!` 进二进制，恒定注册；
  **文字字体**运行时优先加载系统字体（Windows 微软雅黑 / macOS 苹方 / Linux Noto CJK、
  文泉驿等），加载前用 skrifa（epaint 同款解析器）校验「可解析 + 覆盖拉丁/汉字」——egui
  对解析失败的字体直接 panic，必须前置校验；探测失败回退内嵌 `assets/NotoSansSC-Regular.otf`
  （仍保留作 CJK 兜底）。环境变量：`SIMPLEMUSIC_EMBEDDED_FONTS=1` 强制全内嵌（旧行为）、
  `SIMPLEMUSIC_FONT=/path/to.ttf` 手动指定字体文件。无头测试一律用
  `fonts::install_embedded_fonts`（度量不随宿主系统字体漂移）。
- 已有 git 仓库（分支 `main`）：改动用增量编辑，提交信息用中文、说明动机；`SimpleMusic.zip`
  手动备份包与 `.toolchain/`、`.sysroot/`、`target/` 均已在 `.gitignore` 中排除。

---

## 2. 架构总览

代码按「分层 + 按职责拆文件」组织：`app/` 是应用层（UI + 状态 + 异步调度），
`modules/` 是领域模块（B 站客户端 / 音频引擎 / 歌词 / 持久化，无 UI 依赖），
`util/` 是纯函数工具（无 egui 依赖，可独立单测），顶层文件是主题/图标/字体/封面/托盘等基础能力。

```
src/
├── main.rs       启动入口：解析 --width/--height/--smoke；注册字体、主题；创建 MusicApp
├── app/          应用层（原 app.rs 按职责拆分，均为 `impl MusicApp` 块）
│   ├── mod.rs    MusicApp 结构 + new() + 跨模块小工具 + eframe::App 实现（ui/logic/on_exit）
│   ├── messages.rs 后台线程消息 AsyncMsg + spawn_* 派发 + handle_msg
│   ├── player.rs 播放控制（上下曲/seek/音量/移除）+ 快捷键 handle_shortcuts + 播放列表快照
│   │              playback_songs() + clamp_seek/enqueue_dedup
│   ├── playlists.rs 歌单管理（切换/删除/重命名/添加到歌单/在线歌单定位）
│   ├── lyrics.rs 歌词同步（update_lyrics_line + pick_plain_line_index）
│   ├── window.rs 窗口关闭/隐藏（request_close）+ 系统托盘事件轮询 poll_tray_events
│   └── ui/       主界面组件，按区域一文件
│       ├── mod.rs            show_main 主窗口组装
│       ├── widgets.rs        跨区域复用的 egui 小组件（transport_button/icon_button/spinner_arc/
│       │                      封面占位/二维码/文本截断）
│       ├── title_bar.rs      自定义标题栏 + 窗口控制按钮 + 缩放把手
│       ├── status_bar.rs     状态栏（用户头像/昵称 + 登录态 + 设置按钮）
│       ├── playlist_bar.rs   歌单选择栏 + 收藏夹选择弹窗 + 歌单管理窗口
│       ├── song_list.rs      本地/在线歌曲列表（含右键菜单、搜索过滤）
│       ├── import.rs         导入 B 站歌曲输入栏
│       ├── player_bar.rs     底部播放条（控制/进度/音量/切歌模式/桌面歌词开关）
│       ├── settings.rs       设置窗口
│       ├── login.rs          扫码登录弹窗
│       └── lyrics_viewport.rs 桌面歌词悬浮窗（独立 viewport）
├── util/         纯函数工具（无 egui 依赖，全部带单测）
│   ├── fmt.rs    format_secs / format_bytes
│   ├── rand.rs   rand_idx（Xorshift）
│   └── filter.rs song_matches_query
├── modules/
│   ├── bilibili.rs B 站客户端：扫码登录/收藏夹/BV 解析/playurl DASH 音频流（含 WBI 签名）/识别音乐 detect_music
│   ├── audio.rs    音频引擎：下载缓存(md5 键控) + symphonia 解码 + rodio 输出（专用线程）
│   ├── lyrics.rs   vkeys.cn 聚合（QQ 音乐/网易云，中文覆盖高）+ LRCLIB 搜索/清洗/打分 + LRC 解析 + 时间轴同步
│   └── storage.rs  配置/会话/歌单 JSON 持久化（BiliSession Debug 已脱敏）
├── state.rs      数据模型：PlaybackState / QueueItem / Playlist / PlayMode / AudioQuality / Settings
├── theme.rs      主题色板 + 按钮/样式辅助（BG_*/TEXT_*/ACCENT 等语义常量）
├── icons.rs      界面图标：内嵌 Phosphor 图标字体（PUA 码点，画到 rect 中心），不依赖 emoji/系统字形
├── cover.rs      封面缩略图：后台线程下载 + image 解码（不在主线程解码）→ egui 纹理缓存（LRU，失败 30 分钟冷却）
├── fonts.rs      字体：文字优先系统字体（运行时探测 + skrifa 校验），内嵌 Noto Sans SC 兜底；图标恒用内嵌 Phosphor
└── tray.rs       系统托盘（feature=tray）：Linux=独立 GTK 线程+libappindicator；macOS/Win=主线程原生托盘；无 feature 时是 no-op 桩
```

> **重构说明（2025-09）**：原 3000+ 行的 `app.rs` 已按「消息调度 / 播放控制 / 歌单 / 歌词 /
> 窗口 / UI 组件」拆成 `app/` 目录。`MusicApp` 仍是单一结构体，各文件是它的 `impl` 块，
> 跨文件调用的方法标 `pub(crate)`；逻辑与测试未改动，仅物理拆分 + 清理未用导入。

### 线程模型（最重要的一条约定）
- **所有阻塞网络/IO 都放后台 `std::thread`**，结果经**单个 `mpsc` 通道** `AsyncMsg` 发回主线程；`MusicApp::logic` 每帧 `try_recv` 排空并更新状态。
- **重绘保活线程**（`app/mod.rs`，线程名 `render-keepalive`，200ms 一拍）：后台线程持续
  `ctx.request_repaint()`，规避 eframe/winit「最小化后恢复界面卡死」的上游缺陷
  （egui #8246：macOS 上 `ViewportCommand::Minimized(true)` 把 `info.minimized` 锁死为
  `Some(true)`、eframe 永远跳过绘制；egui #5136 / PR #8414：合成器扣留 frame callback
  后重绘门不再打开）。配套在 `MusicApp::logic` 里检测「egui 认为最小化但窗口已恢复
  （有焦点或 inner_rect 存在）」的矛盾态并补发 `Minimized(false)` 清锁。**改最小化/托盘
  隐藏相关代码前先读懂这两个 issue**；`on_exit` 里必须置位 `keepalive_stop`。
- `BiliClient` 以 `Arc<Mutex<..>>` 跨线程共享（有锁中毒保护）；`AudioEngine` 仅在 UI 线程持有，命令经 mpsc 发往专用播放线程，状态经 `Arc<Mutex<PlaybackStatus>>` 轮询。
- **桌面歌词浮窗**通过 `egui::Context::show_viewport_deferred`（延迟模式）渲染，**不与主窗口共享重绘节奏**：浮窗只在歌词文本变化或被 `request_repaint_of` 显式唤醒时才重绘，主窗口播放动画时不会连带浮窗——彻底解决多 viewport 卡顿。
  歌词**切换过渡动画**也遵守该约定：过渡状态（旧文本 + 起始时刻）存在共享 `Context`
  的 data 槽（`TRANSITION_SLOT`），动画期间浮窗闭包内 `request_repaint_after(1/60s)`
  只唤醒浮窗自身，0.4s 过渡结束自动停止；无头测试无法驱动 viewport 动画，过渡状态机
  `LineFade` 是纯函数可单测（见 `lyrics_viewport.rs` tests）。
- UI 闭包里禁止直接做网络请求；需要结果就 `spawn_*` 一个后台线程 + 发消息。

### 播放列表语义（改播放相关代码前必读）
- **当前选中的歌单就是播放列表，没有独立的播放队列**。`MusicApp` 只有
  `current_bvid: Option<String>`（按 bvid 记住正在播哪首），**绝不在播放时把歌写进歌单**。
- 播放列表快照由 `app/player.rs::playback_songs()` 按需构建：本地歌单取 `songs`，
  在线歌单取已加载的收藏夹条目 `fav_items`（在线歌单的 `songs` 永远为空，只是收藏夹引用）。
  上下曲/随机/曲终自动切歌/列表高亮全部基于该快照，**按 bvid 定位，不按下标**——
  下标在搜索过滤、删歌、列表刷新后会漂移，bvid 不会。
- 入单只有两条显式路径：右键「添加到歌单」（`add_song_to_local_playlist`）和
  链接导入「添加并播放」（`messages.rs::PlayReady` 分支，静默去重；在线歌单只读不入单）。
- 启动时会自动清空在线歌单 `songs` 的历史残留（旧版隐式入列的脏数据，别删这段清理）。

### 播放链路
`resolve_stream`（DASH 音频流，未签名被风控自动补 WBI 重试）→ 带 `required_headers`(UA/Referer/Cookie) 流式下载到缓存 `~/.cache/simple-music/audio/<md5(bvid)>.m4s`（二次秒开）→ symphonia(AAC/MP4) 解码 → rodio 输出；CDN 403 自动换备用地址；无输出设备绝不 panic，进错误状态。

### 歌词链路
切歌 → 后台 `spawn_lyrics_fetch`（歌词线程，绝不阻塞播放解析）→ **先查本地歌词缓存**
（`~/.cache/simple-music/lyrics.json`，按 bvid 的 md5 键控；命中 `selected`/`candidates`
即零网络回放，**用户手选的歌词优先于自动结果**）→ 未命中再调 `BiliClient::detect_music(bvid, cid)` 问
**B 站「识别音乐」**（`/x/player/v2` 的 `bgm_info` → `/x/web-interface/view/detail/tag` 的 `tag_type=bgm` TAG，
拿 `MA…` music_id 后换 `/x/copyright-music-publicity/bgm/detail` 的官方曲名/歌手，全程无需登录）→
把官方词作为 `SongHint`（附视频时长）传给 `LyricsProvider::fetch_all_with_hint` →
按候选查询（提示词最优先，视频标题清洗词兜底，去重后 ≤5 条）依次尝试 **vkeys.cn 聚合源**
（QQ 音乐 `mid` 优先 → 网易云 `id`，取回 LRC 原文，翻译 `trans`/`tlyric` 按时间戳并入同行）→
全部未命中再回退 **LRCLIB**（搜索 + 精确 GET + 相似度打分阈值 40；有提示时精确 GET 也用官方词）→
打分用 `match_score_with_hint`（提示曲名/歌手匹配加分 + **视频时长 vs 候选时长接近加分**）→
抓取成功（非空）写回歌词缓存并当场落盘（后台线程）；`LyricsFetched{key, candidates, selected}` 按 bvid 回主线程
（`selected` = 缓存的当前生效歌词或第一候选），主线程只 `apply_lyrics_only` 更新 UI、不再落盘；
**用户在「T」弹窗手选**走 `apply_lyrics`（应用 + 写缓存 `selected` + 后台落盘），下次播放同曲直接生效；
同步歌词用二分定位当前句，无同步时按进度近似取纯文本行。
识别音乐是纯增强：`detect_music` 失败返回 `None`，歌词链路照旧走标题搜索。

---

## 3. 数据与持久化（Linux 路径）

```
~/.config/simple-music/config.json      设置（桌面歌词开关/锁定/字号/位置/音量/音质/播放模式）
~/.config/simple-music/session.json     B 站登录态 Cookie（权限 0600，Debug 已脱敏）
~/.config/simple-music/playlists.json   所有歌单（本地 + 在线引用）
~/.config/simple-music/playlist.json    旧版单队列文件（读取时自动迁移，随后删除）
~/.cache/simple-music/audio/            音频缓存（按 bvid 的 md5，损坏自动重下）
~/.cache/simple-music/lyrics.json       歌词缓存（按 bvid 的 md5：上次生效歌词 + 全部候选）
```

- 设置每 5 秒兜底保存 + 退出保存；歌单变更 `queue_dirty` 后 2 秒防抖保存。
- `QueueItem`/`Settings` 等带 `#[serde(default)]` 保证旧 JSON 兼容。

---

## 4. 核心约定 / 改代码前必看

1. **借用手法**：`app/` 下各 UI 文件里大量「先 clone 数据（`rows`/`fav_items`/`snapshot`），再进闭包操作 `self`」来绕开借用检查。新增 UI 逻辑时沿用此模式，不要在闭包内同时 hold 两个 `&mut self` 借用。
2. **窗口/弹窗开关**：用 `let mut open = self.xxx; ... .open(&mut open).show(...); self.xxx = open;` 模式。注意：**闭包内不能改 `open`**（会与 `.open(&mut open)` 冲突）——需要「操作后关窗」用外部 `let mut close_after = false;` 捕获进闭包，`show` 之后再改 `open`。
3. **借用手法细节**：`egui::Popup`/`Response::context_menu` 的闭包是 `FnOnce`，但每帧新建，所以闭包内可以直接 `self.method()`（参考 `ui/playlist_bar.rs` 的 + 按钮 popup）。
4. **键盘快捷键**在 `app/player.rs::handle_shortcuts`（`logic` 里调用），用 `ctx.memory(|m| m.focused().is_none())` 判断无输入焦点才生效，避免抢空格/方向键。新增快捷键加在这里，别散落到 UI 闭包里。
5. **文本输入框**（导入/搜索/重命名）：聚焦时 `focused()` 返回 Some，快捷键自动停用，这是预期行为。
6. **主题**：一律用 `theme::` 语义色常量（`BG_CARD`/`TEXT_PRIMARY`/`ACCENT`/`GOLD`…），不要写魔法色值；按钮样式用 `theme::primary_button`/`theme::small_button`。
7. **图标**：所有界面图标用 `icons::*`（内嵌 Phosphor 图标字体，PUA 码点渲染到 rect 中心），不要依赖 emoji 或媒体控制码点（跨平台字形缺失会显示 "?"）。
8. **错误处理**：音频错误不 panic，写 `PlaybackStatus.error` 由 UI 展示；网络错误经 `AsyncMsg` 回 `ui_error`（红色）或 `notice`（金色轻提示，4 秒）。
9. **文本宽度**：动态文案先 `truncate_label`/`fit_text` 再 `painter.text`。
10. **单测**：纯函数（解析/打分/格式化/过滤）放 `#[cfg(test)] mod tests`，用 `cargo test` 离线跑；真实网络用 `#[ignore]` 标注。新增纯逻辑尽量带测试。
11. **UI 状态与数据解耦（当前曲目模式）**：凡是「UI 里选中的东西」跨帧/跨列表操作要记住时，**存稳定标识（如 bvid），不要存列表下标**——下标在过滤/删歌/刷新后静默漂移出 bug，标识找不到时按 `Option::None` 处理即可自然降级（如高亮消失）。
12. **不要让「执行动作」顺手改数据**：旧版播放歌曲时隐式把歌写进歌单并落盘，造成在线歌单累积脏数据。副作用（入单/落盘/置 dirty）必须由用户的显式操作触发；新功能如果发现自己「顺手」改了用户数据，几乎一定是设计错了。
13. **行为不变量改动要写迁移/清理**：改持久化语义时（如在线歌单不再存歌），在启动路径加一次性数据清理（见 `app/mod.rs::new` 对在线歌单 `songs` 的清空），并考虑旧文件兼容（`#[serde(default)]`）。
14. **提交规范**：每完成一个功能/修复后 **立即提交**（`git add -A && git commit`），不要攒多个改动再一次性提交。提交消息格式：
    - 前缀：`修复：` / `优化：` / `重构：` / `更新：` / `新增：` / `移除：` 等，后接详细描述。
    - 示例：`修复：封面下载超时后无限转圈的问题` / `优化：标题栏外边距与默认窗口大小` / `重构：封面解码移出主线程` / `更新：AGENT.md 提交规范说明`。
    - 消息用中文，说明「改了什么 + 为什么改」，避免笼统的"更新代码"或"fix bug"。

---

## 5. 功能清单（现状）

- **B 站扫码登录**：二维码 + 轮询（86101/86090/86038），Cookie 持久化、日志脱敏。
- **音源（仅两种入口，刻意无搜索）**：① 收藏夹 → 在线歌单（分页加载，只读）；② 链接导入（BV 号 / `/video/BV..` / `b23.tv` 短链）。
- **播放**：播放/暂停、上下曲、进度条 seek、音量、曲终自动下一首、加载进度、三种切歌模式（顺序循环/单曲循环/随机）、音质偏好（64/128/320k/无损）。
- **播放列表语义**：**当前选中的歌单就是播放列表**，没有独立队列；播放时不隐式写歌进歌单，本地歌单只有显式「导入/添加/删除」才变化，在线歌单内容始终来自 B 站收藏夹接口（只读）。上下曲/随机/曲终自动切歌遍历选中歌单内容（本地取 `songs`，在线取已加载的 `fav_items`）。
- **封面**：列表/播放条圆角缩略图，异步 + 内存缓存 + 占位图。
- **桌面歌词**：透明置顶无边框悬浮窗，当前句+下一句预览，可拖动/锁定(鼠标穿透)/调字号，位置随浮窗重绘实时记录进设置并持久化（仅 X11），重启后自动恢复到关闭前的位置。
- **歌词**：B 站「识别音乐」（bgm_info / BGM TAG → 官方曲库曲名歌手）生成优先查询词 + 视频时长校准打分；vkeys.cn 聚合源（QQ 音乐/网易云）自动搜索 + LRC 时间轴同步，翻译歌词并入；LRCLIB 兜底。
- **歌单**：本地歌单增删改（管理窗口：重命名/删除，至少留一个）；在线歌单（B 站收藏夹引用，可删）。
- **歌单内搜索**：标题/UP 主实时过滤（本地与在线列表都有）。
- **键盘快捷键**：`空格` 播放/暂停，`←/→` 快退/快进 5s，`↑/↓` 音量 ±5%，`N/P` 上下曲。
- **右键菜单**：歌曲项复制 BV 号、添加到/收藏到其他本地歌单。
- **歌词选择**：播放条「T」按钮弹出多源候选（vkeys/LRCLIB），点选切换（`apply_lyrics`）。

---

## 6. 近期改动（本轮已实现）

本轮（修复：搜索框输入时布局跳动/失焦、中文输入法被打断）：

- **根因（egui 自动 id 漂移）**：`TextEdit` 不给 `id_salt` 时用 `ui.next_auto_id()`
  （同一 Ui 内自增的盐），id 随**同 Ui 里排在它前面的控件数量**变化。本地/在线歌曲
  列表的搜索框之前就是这样：输入第一个字 → 条件渲染的「清空搜索」按钮插到它前面 →
  输入框 id 整体漂移 → egui 按 Id 记忆的焦点失效 → 失焦。中文输入法组合期间预编辑串
  一进文本就走同一链条：egui-winit 检测到无焦点文本框即 `set_ime_allowed(false)`，
  组合被系统取消、拼音残留在框里（「中文输入法异常」的真凶）。
- **`widgets.rs` 新增 `song_search_field`**（本地/在线列表共用）：
  ① TextEdit 固定 `id_salt(SONG_SEARCH_ID_SALT)`，id 与前置控件增减无关；
  ② 清空按钮**常驻占位 24px**（无搜索词时分配同样空间但不绘制、不可点），按钮
  出现/消失不再横向挤动输入框——布局不跳，也避免输入框被挤动后原本点在输入框上的
  第二次点击落进突然出现的「×」误清搜索词。右到左布局，占位/按钮贴最右。
- **同型隐患顺手修**：`import.rs` 的 BVID 输入框（前面有条件出现的加载圈）与
  `playlist_bar.rs` 的歌单重命名输入框（列表项循环里）也补了显式 `id_salt`。
- **测试（`search_field_tests`，多帧无头 egui 模拟器）**：id_salt 稳定、点击聚焦后
  输入第一个字不失焦、完整 IME 序列（Preedit→更新→Commit→空 Preedit）全程聚焦且
  文本正确（"ni"→"nihao"→"你"）、按钮出现/消失输入框矩形不变、点「×」清空。
  两个无头测试的坑，后续写类似测试必看：
  1. **必须装字体**：本项目 eframe 关了 `default_fonts`，无头 `Context::default()`
     字体表是空的 → 零字形 galley → 光标定位坍缩到 0，任何依赖光标位置的断言都会
     诡异失败。测试里调 `crate::fonts::install_embedded_fonts(&ctx)`（强制内嵌字体，度量不随宿主
      机器的系统字体漂移；生产入口 `install_fonts` 优先系统字体）。
  2. **模拟打字要复刻 egui-winit 的投递**：Key press+release 与 `Event::Text` 必须
     同帧（见 egui-winit `on_keyboard_input`：push 完 Key 紧接着 push Text）；拆成
     多帧的话 release 帧的 Key 会先于 Text 消费，插入位置就错了。
- 验证：156 个离线单测全绿（新增 5 个），`cargo check`（默认 tray）通过。

上一轮（新增：桌面歌词位置随开随记，重启自动恢复）：

- **`Settings` 新增 `lyrics_pos: Option<[f32; 2]>`**（`#[serde(default)]`，旧 config.json 兼容）：
  存屏幕坐标而非 egui 的 `Pos2`——项目未启用 eframe 的 serde feature，`Pos2` 没实现 Serialize，
  数据模型层也不应反向依赖 egui。移除 `MusicApp.lyrics_pos` 内存字段，位置以设置为准。
- **浮窗每次重绘都上报当前位置**（`POS_SLOT` 槽读后即删，主线程写回 `settings.lyrics_pos`），
  随设置的「每 5 秒兜底 + 退出保存」落盘：拖动由系统处理、移动结束至少触发一次浮窗重绘，
  最终位置必被捕获；本会话内关掉再开浮窗也直接回到该位置；启动时在 viewport builder 恢复。
- **Wayland 防污染**：原生 Wayland（`WAYLAND_DISPLAY` 非空）下客户端拿不到窗口全局位置，
  上报/恢复都跳过，避免把 (0,0) 之类占位值写进配置污染跨会话记录；`wayland_session_from` 带单测。
- 验证：151 个离线单测全绿（新增 3 个），`cargo check`（默认 tray）通过，`--smoke` OK。

上一轮（新增：歌词本地缓存 + 手选歌词持久化，二次播放零网络）：

- **`lyrics.rs` 缓存条目**：`LyricsCacheEntry{selected, candidates, saved_at_unix}` +
  纯函数 `cache_key`（bvid 的 md5，与音频缓存同方案）/`cache_lookup`/`cache_store_fetch`/
  `cache_update_selected`；`Lyrics` 加 serde derive（可直接 JSON 落盘）。
  磁盘读写统一放 `storage.rs`（`load/save_lyrics_cache[_from/_to]`，
  `~/.cache/simple-music/lyrics.json`，坏文件静默降级为空缓存）。
- **歌词线程（`spawn_lyrics_fetch`）**：先查缓存——`selected` 或 `candidates` 任一存在
  即直接回放（零网络，用户上次手选优先）；未命中才走识别音乐 + 多源搜索，
  抓取成功（非空）写回缓存并当场落盘（本就是后台线程，失败静默只丢缓存不丢功能；
  空结果不缓存，源补录后还能自动命中）。
- **手选持久化（`apply_lyrics`）**：「T」弹窗点选 = 显式副作用，写缓存 `selected` +
  后台落盘；自动抓取回放走 `apply_lyrics_only`（不重复落盘）。
  `LyricsFetched` 消息新增 `selected` 字段。
- 验证：148 个离线单测全绿（新增 4 个缓存测试），`cargo check`（默认 tray）通过，
  `--smoke`（no-default-features）OK。

上一轮（优化：歌词搜索接入 B 站「识别音乐」，提升命中率）：

- **动机**：B 站视频标题噪音大（【4K】【燃剪】xxx 4K修复版…），仅靠标题清洗搜歌词
  经常命中翻唱/remix 甚至搜不到；B 站自己有「识别音乐」标注（官方曲库），直接拿来当查询词。
- **`bilibili.rs` 新增 `detect_music(bvid, cid) -> Option<MusicHint>`**：探测顺序
  ① `/x/player/v2` 的 `bgm_info`（需 cid，`QueueItem.cid` 现在由解析播放时回填，
  旧歌单条目 cid=0 自动跳过）② `/x/web-interface/view/detail/tag` 的 `tag_type=bgm` TAG
  （只要 bvid）③ 拿 `MA…` id 换音乐开放平台 `bgm/detail` 的官方曲名/歌手/专辑
  （`origin_artist` 优先，空则压平 `artists_list`；`BiliNameValue` 兼容字符串/对象两种形态）。
  任一步失败返回 `None`，**识别是纯增强，绝不阻塞歌词获取**；实测三接口均无需登录、未风控。
- **`lyrics.rs` 新增 `SongHint`**（title/artist/duration_secs）与提示版链路：
  `search_queries_with_hint`（官方词插队最前 + 与标题派生词去重 ≤5 条）、
  `match_score_with_hint`（提示曲名 +60/子串 +35，提示歌手 +30/子串 +15，
  **视频时长 vs 候选时长**：≤3s +35 / ≤8s +20 / ≤15s +8 / >45s −10 —— 原曲向视频的强信号）、
  `best_match_with_hint`、`LyricsProvider::fetch_with_hint/fetch_all_with_hint`
  （LRCLIB 精确 GET 优先用官方词）。无提示时行为与旧版完全一致。
- **接线**（`messages.rs` / `player.rs` / `state.rs`）：`QueueItem` 加 `#[serde(default)] cid`；
  `resolve_playable`/`spawn_import` 回填 cid；`spawn_lyrics_fetch` 在**歌词线程**（不拖慢出声）
  先 `detect_music` 再带提示搜索。歌词候选弹窗（`T` 按钮）无需改动，候选标签本就展示
  来源曲名/歌手/专辑。
- 验证：144 个离线单测全绿；`detect_music_live`（`#[ignore]`，`cargo test -- --ignored`）
  实测 BV1M741177Kg → Other Side — MIYAVI，vkeys 用官方词搜到雅-MIYAVI 原曲；
  `cargo check`（默认 tray feature）通过；`cargo run --no-default-features -- --smoke` OK
  （沙箱 `cargo run --` 默认 feature 仍会因缺 GTK 链接失败，属预期）。

上一轮（修复：最小化后恢复界面卡死）：

- **根因**：上游 eframe/winit 已知缺陷——最小化后部分平台不再向应用投递
  `RedrawRequested`（Windows 隐藏/最小化窗口不投递；Wayland 合成器对不可见 surface
  扣留 frame callback，重绘门不重开；macOS 上 `ViewportCommand::Minimized(true)` 把
  `info.minimized` 锁死），而 eframe 只在重绘事件里执行 `logic`/`ui`，于是恢复窗口后
  事件循环永远等不到重绘 → 整窗冻结（egui #8246 / #5136，修复 PR #8414 尚未发布）。
- **修复**（`app/mod.rs`）：① `render-keepalive` 后台线程每 200ms `ctx.request_repaint()`
  强制事件循环保持苏醒（eframe 0.34+ 对不可见窗口会在收到重绘请求时直接跑
  `run_ui_and_paint`，viewport 命令得以处理）；② `logic` 里检测「egui 认为最小化但
  窗口已恢复（有焦点或 inner_rect 存在）」矛盾态，补发 `Minimized(false)` 清掉 macOS
  的 `info.minimized` 锁存；③ `was_minimized` 记录最小化→恢复跳变，恢复瞬间补一针重绘。
  `on_exit` 置位 `keepalive_stop` 让线程退出。
- 经验：**这类「只在最小化/恢复后出现的冻结」不是死锁，是事件循环饿死**；
  优先怀疑平台不再投递重绘事件，应用层用后台线程保活 + 恢复补绘即可兜底。

上一轮（文档：沉淀播放列表重构的经验与踩坑）：

- §1 补沙箱测试要点：`cargo test` 必须 `--no-default-features`（GTK 链接坑，`check` 却能过，
  别被骗）；`cargo check`（默认 feature）也要跑以覆盖托盘代码；测试数更新为 133。
- 新增「播放列表语义」小节（§2）+ 核心约定 3 条（§4：稳定标识替代下标、
  副作用只由显式操作触发、持久化语义变更要带迁移清理）。
- 修正文档与实现不符处：状态栏早就不显示当前曲目；播放栏「第 N/M 首」已不存在，
  替换为歌词选择按钮的描述。

上一轮（重构：移除隐式播放队列，播放列表 = 当前选中歌单）：

- **移除内部播放队列**：原 `play_prepared` 在播放任何歌时都会把歌 `enqueue_dedup`
  进当前歌单的 `songs` 并落盘——点播在线收藏夹的歌会悄悄累积进收藏夹歌单（脏数据）。
  现在改为：`MusicApp.current_bvid: Option<String>` 只按 bvid 记住「正在播哪首」，
  `play_prepared` 只出声、绝不写歌单。
- **播放列表 = 当前选中歌单的内容**：`MusicApp::playback_songs()` 按需构建只读快照——
  本地歌单取其 `songs`，在线歌单取已加载的收藏夹条目 `fav_items`。
  上下曲/随机/曲终自动切歌/列表高亮全部基于该快照。
- **行为变化（刻意）**：切换歌单即切换播放列表，原歌单正在播的歌会停止
  （`switch_active_playlist` → `stop_current`）；删除正在播放的歌也是直接停止。
- **导入语义**：「添加并播放」现在显式把歌加进当前选中的**本地歌单**（静默去重）再播放；
  在线歌单只读，导入时只播放不入单。
- **启动清理**：加载歌单时自动清空在线歌单 `songs` 的历史残留（旧版隐式入列的脏数据）。
- 133 个单测全绿，`cargo check`（含默认 tray feature）通过，`--smoke` OK。

上一轮（性能修复：桌面歌词浮窗改为延迟 viewport + 按需重绘，消除主界面卡顿）：

- **根因**：浮窗原来用 `show_viewport_immediate`（立即模式），egui 文档明确说明该模式
  「父子窗口任一需要重绘，双方都重绘」= 双倍工作量。主窗口播放时每帧重绘（进度条动画），
  把透明置顶浮窗也拖进每帧渲染，导致主界面卡顿。
- **修复**：改为 `show_viewport_deferred`（延迟模式），浮窗只在自身需要重绘时执行 UI 闭包：
  - `logic()` 比较浮窗内容指纹（当前句/下一句/字号/锁定），变化才
    `request_lyrics_repaint`（`ctx.request_repaint_of`）唤醒浮窗；
  - 浮窗收到输入事件（hover/拖动/点击）由 egui 自动重绘；
  - 其余时间浮窗完全静止，与主窗口互不拖累。
- **通信**：deferred 闭包是 `Fn + 'static`，不能借用 `&mut self`，浮窗交互（关闭按钮、
  首次位置捕获）通过共享 `ctx` 的 data 槽（`IdTypeMap`：`CLOSE_SLOT`/`POS_SLOT`）回传
  主线程消费；窗口拖动由系统处理，`ViewportBuilder::patch` 只在值变化时发命令，不会拉回。
- 行为零改动：88 个单测全绿，两种 feature 配置均编译通过。

上一轮（结构重构：`app.rs` 拆分 + 目录分层）：

- **重构：`src/app.rs`（约 3000 行）按职责拆分为 `src/app/` 目录**：
  `mod.rs`（结构/生命周期）、`messages.rs`（AsyncMsg + spawn_* + handle_msg）、
  `player.rs`（播放控制 + 快捷键）、`playlists.rs`（歌单管理）、`lyrics.rs`（歌词同步）、
  `window.rs`（托盘/窗口）、`ui/`（按区域拆 11 个 UI 文件）。
  纯函数抽到新 `src/util/`（fmt/rand/filter，全部带单测）。
  行为零改动：88 个单测全绿，`cargo check`（含默认 tray feature）通过。
- **重构细节**：`MusicApp` 仍是单一结构体，各文件是其 `impl` 块；跨文件调用的方法
  标 `pub(crate)`（如 `current_bvid`/`spawn_*`/`show_*`）；闭包借用模式、`AsyncMsg`
  消息协议、持久化格式、快捷键与 UI 布局均未改变。
- **新增单测**：`util::rand` 边界测试（`max=0` / `max=1` / 界内）3 个。

上一轮（桌面歌词全透明 + 收藏夹接口修复）：

- **桌面歌词全透明**：`show_lyrics_viewport` 默认不再绘制任何背景/描边/外圈柔光，
  仅「未锁定 + 鼠标悬浮」时绘制背景卡片（`LYRIC_BG`）与描边（关闭按钮/拖动仍随 hover 出现）。
  锁定（鼠标穿透）时永远透明。
- **收藏夹接口修复（B 站已下线 `fav/folder/owned/list`，HTTP 404）**：
  `list_favorite_folders` 改用 `fav/folder/created/list` + `fav/folder/collected/list`
  两个分页接口合并（按 id 去重，`dedup_folders`）；`data:null` 按空页处理；
  `fav/resource/list` 增加 `platform=web` 参数。新增单测：分页响应解析 + 去重。
- **启动补 buvid**：GUI 启动时后台线程调 `BiliClient::ensure_buvid`
  （之前只有 `--smoke` 才调用），降低风控 412 概率。

上一轮在 `app/` 目录新增/改动（均有单测或 smoke 验证，88+ 测试通过）：

- `MusicApp` 新字段：`search_text`、`playlist_mgmt_open`、`renaming_idx`、`rename_text`、`last_notice`。
- 新辅助方法：`notice` / `switch_active_playlist`（切歌单停止播放并清搜索） / `stop_current` / `add_song_to_local_playlist` / `delete_playlist` / `rename_playlist` / `change_volume` / `handle_shortcuts`。
- 歌单选择栏新增「管理」按钮 → `show_playlist_manage_window`；在线歌单选择、ComboBox、创建歌单均改走 `switch_active_playlist`。
- 本地/在线歌曲列表加搜索框 + 过滤 + 无结果态；列表项加 `resp.context_menu`（复制 BV/添加到歌单）。
- 播放栏加「第 N/M 首」与金色 `notice` 提示；`logic` 调 `handle_shortcuts`。
- 新增纯函数 `song_matches_query`（含单测）。

本轮（浮窗 + 自定义标题栏 + 系统托盘）新增/改动：
- **自定义标题栏**：`show_custom_title_bar` 自绘窗口 chrome，含拖动区域、最小化、关闭按钮。
- **浮窗卡片**：`with_decorations(false)` + `with_transparent(true)`，圆角 `CORNER_XL` 悬浮卡片。
- **缩放把手**：`show_resize_grip` 右下角拖拽缩放（`BeginResize(SouthEast)`）。
- **系统托盘**：`src/tray.rs` 托盘图标（Linux=GTK 线程+libappindicator；macOS/Win=原生托盘），菜单「显示/隐藏 / 退出」。
- **最小化到托盘**：关闭按钮隐藏窗口（托盘可用时），托盘菜单唤醒。
- **状态栏**：`show_status_bar` 用户头像 + 登录状态 + 设置按钮；登录后显示用户昵称
  （nav 接口后台拉取，`MusicApp.uname`，未取到前回退显示 `UID <mid>`）。

---

## 7. 已知限制 / 可能的下一步

- 多 P 视频只取 P1（`video_info` 的 `pages` 未逐 P 展开）。
- 不支持 av 号导入（`parse_bvid` 明确只认 BV）。
- 切换歌单会停止当前播放（播放列表 = 选中歌单的直接推论）；没有跨歌单的播放队列。
- 在线歌单只显示已加载页，搜索也只过滤已加载页。
- 桌面歌词位置仅 X11 会话下记录/恢复；原生 Wayland（设置了 `WAYLAND_DISPLAY`）由合成器决定窗口位置，跳过记录以防写进占位坐标。
- 无全局媒体快捷键（如系统级播放/暂停）、无音量静音键。
- 本地歌单内歌曲不可拖拽排序。
- 无播放历史/最近播放记录。
- 若要加功能，优先在 `app/ui/` 对应 `show_*` 方法所在文件内做增量；涉及跨线程新数据用 `AsyncMsg` 变体 + `messages.rs::handle_msg` 分支；新增纯逻辑放 `util/` 或对应文件内 `#[cfg(test)]`。

---

## 8. 快速定位表

| 想改什么 | 去哪个文件/函数 |
|---|---|
| 主界面布局/顶部栏 | `app/ui/mod.rs::show_main` |
| 自定义标题栏/窗口控制 | `app/ui/title_bar.rs::show_custom_title_bar` / `show_resize_grip` |
| 窗口关闭/隐藏、托盘事件 | `app/window.rs::request_close` / `poll_tray_events`；`tray.rs` |
| 播放条（进度/音量/切歌模式） | `app/ui/player_bar.rs::show_player_bar` |
| 歌单选择 + 管理 | `app/ui/playlist_bar.rs::show_playlist_selector` / `show_playlist_manage_window` |
| 本地歌曲列表 | `app/ui/song_list.rs::show_local_songs` |
| 在线收藏夹列表 | `app/ui/song_list.rs::show_online_songs` |
| 设置窗口 | `app/ui/settings.rs::show_settings_window` |
| 桌面歌词悬浮窗 | `app/ui/lyrics_viewport.rs::show_lyrics_viewport` |
| 扫码登录弹窗 | `app/ui/login.rs::show_login_window` |
| 快捷键 | `app/player.rs::handle_shortcuts` |
| 播放控制（上下曲/seek/移除） | `app/player.rs` |
| 播放列表快照/当前曲目定位 | `app/player.rs::playback_songs` / `current_bvid`（`app/mod.rs`） |
| 歌单增删改/切换 | `app/playlists.rs` |
| 歌词同步（当前句/下一句） | `app/lyrics.rs` |
| 异步消息类型与分发 | `app/messages.rs::AsyncMsg` + `handle_msg` |
| B 站接口/取流 | `modules/bilibili.rs` |
| 音频/缓存 | `modules/audio.rs` |
| 歌词/LRC | `modules/lyrics.rs` |
| 持久化 | `modules/storage.rs` |
| 数据模型/设置 | `state.rs` |
| 格式化/随机数/搜索过滤 | `util/fmt.rs` / `util/rand.rs` / `util/filter.rs` |
| 主题色板/按钮样式 | `theme.rs` |
| 图标（Phosphor） | `icons.rs` |
| 封面下载/缓存 | `cover.rs` |
