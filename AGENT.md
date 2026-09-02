# AGENT.md — SimpleMusic 开发指南（给后续 AI/开发者）

本项目是 **SimpleMusic**：一个极简桌面音乐播放器（Rust 2024 + eframe/egui 0.36，纯原生 GUI 无 WebView）。音源来自 B 站视频，歌词来自 LRCLIB 在线搜索，带桌面歌词悬浮窗。

> 目标：让接手的人/Agent 用最少的时间搞清楚「项目怎么跑、代码怎么组织、哪些约定必须遵守、改哪里能加什么功能」。

---

## 1. 构建与测试（沙箱环境必读）

> ⚠️ **沙箱里 cargo 不在 PATH**，直接 `cargo` 会报 `command not found`；直接 `rustup` 会因 HOME 无写权限失败。**必须先 source 工具链环境**：

```sh
cd /data/dsh/home/SimpleMusic
source .toolchain/env.sh          # 设置 RUSTUP_HOME / CARGO_HOME / PATH / CC / 链接器 等
cargo check                       # 编译检查
cargo test                        # 单测（当前 81+ 个，全部离线）
cargo run -- --smoke              # 无窗口模块自检，打印 SMOKE_OK 退出（会走少量网络，失败不阻断）
cargo run                         # 真实 GUI 启动（需要显示环境）
```

- 工具链/依赖全部离线缓存于 `.toolchain/`；`.sysroot/` 是构建系统根（gcc/alsa/x11 库）。
- **系统托盘 feature**：默认启用 `tray`，跨平台（见 `src/tray.rs` 模块注释）：
  Linux 走独立 GTK 线程 + libappindicator（需系统装 GTK3；沙箱无 GTK 库，改用
  `cargo build --no-default-features` 跳过托盘，GUI 其余功能不受影响）；
  **macOS/Windows 用系统原生托盘（NSStatusItem / Shell_NotifyIcon），无需 GTK、无额外线程**，
  图标由 `MusicApp::new` 在主线程创建（macOS 要求事件循环运行中创建）。
- 内嵌字体 `assets/NotoSansSC-Regular.otf`（约 16MB）编译期 `include_bytes!` 进二进制。
- 无 git 仓库（当前目录没有 .git），改动请以「读代码 + 增量编辑」为准。

---

## 2. 架构总览

```
main.rs       启动入口：解析 --width/--height/--smoke；注册字体、主题；创建 MusicApp
app.rs        ★核心：主窗口 UI + 桌面歌词悬浮窗 + 异步任务调度（约 2700+ 行）
tray.rs       系统托盘（feature=tray）：Linux=独立 GTK 线程+libappindicator；macOS/Win=主线程原生托盘；无 feature 时是 no-op 桩
              托盘点击约定：左键单击=显示/聚焦主窗口；右键=托盘菜单（显示/隐藏、退出）。
              Linux/libappindicator 不上报图标点击事件（点击由系统面板打开菜单），属平台限制；
              macOS/Windows 用 `with_menu_on_left_click(false)` 关掉左键弹菜单后监听 `TrayIconEvent::Click`。
state.rs      数据模型：PlaybackState / QueueItem / Playlist / PlayMode / AudioQuality / Settings
theme.rs      主题色板 + 按钮/样式辅助（BG_*/TEXT_*/ACCENT 等语义常量）
icons.rs      painter 手绘矢量图标（play/pause/prev/next/volume/cross/窗口控制），不依赖字体字形
cover.rs      封面缩略图：后台线程下载 → image 解码 → egui 纹理缓存（LRU，失败 30 分钟冷却）
fonts.rs      CJK 字体：内嵌 Noto Sans SC 优先，失败探测系统字体
modules/
  bilibili.rs B 站客户端：扫码登录/收藏夹/BV 解析/playurl DASH 音频流（含 WBI 签名）
  audio.rs    音频引擎：下载缓存(md5 键控) + symphonia 解码 + rodio 输出（专用线程）
  lyrics.rs   LRCLIB 搜索/清洗/打分 + LRC 解析 + 时间轴同步
  storage.rs  配置/会话/歌单 JSON 持久化（BiliSession Debug 已脱敏）
```

### 线程模型（最重要的一条约定）
- **所有阻塞网络/IO 都放后台 `std::thread`**，结果经**单个 `mpsc` 通道** `AsyncMsg` 发回主线程；`MusicApp::logic` 每帧 `try_recv` 排空并更新状态。
- `BiliClient` 以 `Arc<Mutex<..>>` 跨线程共享（有锁中毒保护）；`AudioEngine` 仅在 UI 线程持有，命令经 mpsc 发往专用播放线程，状态经 `Arc<Mutex<PlaybackStatus>>` 轮询。
- UI 闭包里禁止直接做网络请求；需要结果就 `spawn_*` 一个后台线程 + 发消息。

### 播放链路
`resolve_stream`（DASH 音频流，未签名被风控自动补 WBI 重试）→ 带 `required_headers`(UA/Referer/Cookie) 流式下载到缓存 `~/.cache/simple-music/audio/<md5(bvid)>.m4s`（二次秒开）→ symphonia(AAC/MP4) 解码 → rodio 输出；CDN 403 自动换备用地址；无输出设备绝不 panic，进错误状态。

### 歌词链路
切歌 → 后台 `LyricsProvider::fetch(title, uploader)` → LRCLIB 多候选查询 + 相似度打分(阈值 40) → `Lyrics{lrc, plain}` 按 bvid 回主线程；同步歌词用二分定位当前句，无同步时按进度近似取纯文本行。

---

## 3. 数据与持久化（Linux 路径）

```
~/.config/simple-music/config.json      设置（桌面歌词开关/锁定/字号/音量/音质/播放模式）
~/.config/simple-music/session.json     B 站登录态 Cookie（权限 0600，Debug 已脱敏）
~/.config/simple-music/playlists.json   所有歌单（本地 + 在线引用）
~/.config/simple-music/playlist.json    旧版单队列文件（读取时自动迁移，随后删除）
~/.cache/simple-music/audio/            音频缓存（按 bvid 的 md5，损坏自动重下）
```

- 设置每 5 秒兜底保存 + 退出保存；歌单变更 `queue_dirty` 后 2 秒防抖保存。
- `QueueItem`/`Settings` 等带 `#[serde(default)]` 保证旧 JSON 兼容。

---

## 4. 核心约定 / 改代码前必看

1. **借用手法**：`app.rs` 里大量「先 clone 数据（`rows`/`fav_items`/`snapshot`），再进闭包操作 `self`」来绕开借用检查。新增 UI 逻辑时沿用此模式，不要在闭包内同时 hold 两个 `&mut self` 借用。
2. **窗口/弹窗开关**：用 `let mut open = self.xxx; ... .open(&mut open).show(...); self.xxx = open;` 模式。注意：**闭包内不能改 `open`**（会与 `.open(&mut open)` 冲突）——需要「操作后关窗」用外部 `let mut close_after = false;` 捕获进闭包，`show` 之后再改 `open`。
3. **借用手法细节**：`egui::Popup`/`Response::context_menu` 的闭包是 `FnOnce`，但每帧新建，所以闭包内可以直接 `self.method()`（参考 `show_playlist_selector` 的 + 按钮 popup）。
4. **键盘快捷键**在 `handle_shortcuts`（`logic` 里调用），用 `ctx.memory(|m| m.focused().is_none())` 判断无输入焦点才生效，避免抢空格/方向键。新增快捷键加在这里，别散落到 UI 闭包里。
5. **文本输入框**（导入/搜索/重命名）：聚焦时 `focused()` 返回 Some，快捷键自动停用，这是预期行为。
6. **主题**：一律用 `theme::` 语义色常量（`BG_CARD`/`TEXT_PRIMARY`/`ACCENT`/`GOLD`…），不要写魔法色值；按钮样式用 `theme::primary_button`/`theme::small_button`。
7. **图标**：媒体控制图标用 `icons::*`（painter 绘制），不要依赖 emoji/字体码点（跨平台字形缺失）。
8. **错误处理**：音频错误不 panic，写 `PlaybackStatus.error` 由 UI 展示；网络错误经 `AsyncMsg` 回 `ui_error`（红色）或 `notice`（金色轻提示，4 秒）。
9. **文本宽度**：动态文案先 `truncate_label`/`fit_text` 再 `painter.text`。
10. **单测**：纯函数（解析/打分/格式化/过滤）放 `#[cfg(test)] mod tests`，用 `cargo test` 离线跑；真实网络用 `#[ignore]` 标注。新增纯逻辑尽量带测试。

---

## 5. 功能清单（现状）

- **B 站扫码登录**：二维码 + 轮询（86101/86090/86038），Cookie 持久化、日志脱敏。
- **音源（仅两种入口，刻意无搜索）**：① 收藏夹 → 在线歌单（分页加载，只读）；② 链接导入（BV 号 / `/video/BV..` / `b23.tv` 短链）。
- **播放**：播放/暂停、上下曲、进度条 seek、音量、曲终自动下一首、加载进度、三种切歌模式（顺序循环/单曲循环/随机）、音质偏好（64/128/320k/无损）。
- **封面**：列表/播放条圆角缩略图，异步 + 内存缓存 + 占位图。
- **桌面歌词**：透明置顶无边框悬浮窗，当前句+下一句预览，可拖动/锁定(鼠标穿透)/调字号，位置持久化（仅 X11）。
- **歌词**：LRCLIB 自动搜索 + LRC 时间轴同步。
- **歌单**：本地歌单增删改（管理窗口：重命名/删除，至少留一个）；在线歌单（B 站收藏夹引用，可删）。
- **歌单内搜索**：标题/UP 主实时过滤（本地与在线列表都有）。
- **键盘快捷键**：`空格` 播放/暂停，`←/→` 快退/快进 5s，`↑/↓` 音量 ±5%，`N/P` 上下曲。
- **右键菜单**：歌曲项复制 BV 号、添加到/收藏到其他本地歌单。
- **播放位置提示**：播放栏「第 N/M 首」。

---

## 6. 近期改动（本轮已实现）

上一轮在 `src/app.rs` 新增/改动（均有单测或 smoke 验证，81+ 测试通过）：

- `MusicApp` 新字段：`search_text`、`playlist_mgmt_open`、`renaming_idx`、`rename_text`、`last_notice`。
- 新辅助方法：`notice` / `switch_active_playlist`（切歌单重置 `current_track` 并清搜索） / `stop_current` / `add_song_to_local_playlist` / `delete_playlist` / `rename_playlist` / `change_volume` / `handle_shortcuts`。
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
- **状态栏**：`show_status_bar` 合并当前曲目 + 登录状态 + 设置按钮；登录后显示用户昵称
  （nav 接口后台拉取，`MusicApp.uname`，未取到前回退显示 `UID <mid>`）。

---

## 7. 已知限制 / 可能的下一步

- 多 P 视频只取 P1（`video_info` 的 `pages` 未逐 P 展开）。
- 不支持 av 号导入（`parse_bvid` 明确只认 BV）；队列是循环模式，无「顺序不循环」选项。
- 桌面歌词位置仅 X11 可持久化，Wayland 保持居中。
- 无全局媒体快捷键（如系统级播放/暂停）、无音量静音键。
- 本地播放列表项不可拖拽排序；歌单内歌曲不可移动/复制到其他歌单（目前只有右键「添加到其他歌单」= 复制式）。
- 无播放历史/最近播放记录。
- 在线歌单分页加载，搜索只过滤已加载页。
- 若要加功能，优先在 `app.rs` 对应 `show_*` 方法内做增量；涉及跨线程新数据用 `AsyncMsg` 变体 + `handle_msg` 分支。

---

## 8. 快速定位表

| 想改什么 | 去哪个文件/函数 |
|---|---|
| 主界面布局/顶部栏 | `app.rs::show_main` |
| 自定义标题栏/窗口控制 | `app.rs::show_custom_title_bar` / `show_resize_grip` |
| 系统托盘 | `tray.rs` + `app.rs::poll_tray_events` |
| 播放条（进度/音量/切歌模式） | `app.rs::show_player_bar` |
| 歌单选择 + 管理 | `app.rs::show_playlist_selector` / `show_playlist_manage_window` |
| 本地歌曲列表 | `app.rs::show_local_songs` |
| 在线收藏夹列表 | `app.rs::show_online_songs` |
| 设置窗口 | `app.rs::show_settings_window` |
| 桌面歌词悬浮窗 | `app.rs::show_lyrics_viewport` |
| 快捷键 | `app.rs::handle_shortcuts` |
| 异步消息类型 | `app.rs::AsyncMsg` + `handle_msg` |
| B 站接口/取流 | `modules/bilibili.rs` |
| 音频/缓存 | `modules/audio.rs` |
| 歌词/LRC | `modules/lyrics.rs` |
| 持久化 | `modules/storage.rs` |
| 数据模型/设置 | `state.rs` |
