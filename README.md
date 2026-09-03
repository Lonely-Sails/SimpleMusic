# SimpleMusic

> 工作区位置：`/data/dsh/home/SimpleMusic`（DeepSeek Harness 工作区，沙箱内构建）

极简桌面音乐播放器 —— **纯原生 GUI（无 WebView）**，音源来自 B 站视频，歌词来自 LRCLIB 在线搜索，带**桌面歌词悬浮窗**。

技术栈：**Rust 2024 + eframe/egui 0.36（glow 渲染）** · reqwest(rustls) · rodio + symphonia（音频解码）· serde。

跨平台：Windows / macOS / Linux（X11 与 Wayland 均可）。

## 功能一览

- **B 站扫码登录**：二维码登录（手机 B 站 App 扫码），Cookie 持久化到 `~/.config/simple-music/session.json`（日志中自动脱敏）。
- **音源获取（仅两种入口，无搜索）**：
  - **收藏夹**：登录后展示收藏夹文件夹列表 → 点选文件夹加载视频列表 → 单击即播（分页加载）。
  - **链接导入**：粘贴 `BV 号` / `www.bilibili.com/video/BV..` / `b23.tv` 短链，添加并播放。
- **播放**：播放/暂停、上一首/下一首、进度条拖动 seek、音量、曲终自动下一首、加载进度显示；音频自动下载到本地缓存（`~/.cache/simple-music/audio/`，按 bvid 缓存，二次秒开），CDN 403 自动切换备用地址。播放栏显示当前歌曲位置「第 N/M 首」。
- **封面**：队列/收藏列表/播放条显示 B 站封面缩略图（异步下载、内存缓存、圆角渲染，无图时显示音符占位）。
- **桌面歌词**：透明置顶无边框悬浮窗，当前句大字 + 下一句半透明预览，随播放进度同步滚动；可拖动、可锁定（锁定即鼠标穿透），字号可调，位置与设置持久化。
- **歌词搜索**：切歌后自动按「上传者 + 标题」搜索同步歌词，**vkeys.cn 聚合源优先**（QQ 音乐 → 网易云，中文歌曲覆盖率高，翻译歌词按时间戳并入同行），未命中回退 **LRCLIB**（免费、无鉴权）；无同步歌词时回退纯文本按播放进度近似高亮。
- **歌单管理**：歌单栏「+」创建本地歌单，可同步 B 站收藏夹为只读在线歌单；「管理」支持重命名（本地）与删除；切换歌单时播放上下文自动跟随。
- **歌单内搜索**：歌曲列表顶部搜索框，按标题 / UP 主实时过滤（含无匹配提示与一键清空）。
- **键盘快捷键**：`空格` 播放/暂停 · `←/→` 快退/快进 5 秒 · `↑/↓` 音量 ±5% · `N/P` 下一首/上一首（文本输入聚焦时自动禁用，避免冲突）。
- **歌曲右键菜单**：列表项右键可「复制 BV 号」、收藏/添加到其他本地歌单。
- **极简原则**：无音乐搜索、无推荐、无评论、无广告。就一个播放器。

## 构建与运行

普通桌面机器（已装 Rust stable）：

```sh
cargo build --release
./target/release/simple-music
# 可选启动参数
./target/release/simple-music --width 1280 --height 800
SIMPLE_MUSIC_WIDTH=1440 SIMPLE_MUSIC_HEIGHT=900 ./target/release/simple-music
```

Linux 构建前需要系统包（依发行版而异）：

```sh
# Debian/Ubuntu
sudo apt install build-essential pkg-config libasound2-dev libxkbcommon-dev libwayland-dev libx11-dev libgl1-mesa-dev
```

开发调试：`cargo run`；测试：`cargo test`（64 个单测）；模块自检（无头环境）：`cargo run -- --smoke`，打印 `SMOKE_OK` 并以 0 退出。

> 注：bin 名 `simple-music` 用蛇形连字符；`cargo run` 与 `cargo test` 均可直接用。

## 使用方法

1. 打开应用 → 顶部「扫码登录」→ 手机 B 站 App 扫码，从“扫一扫”进入并确认。
2. 登录后歌单栏「+」→「同步B站收藏夹」，选一个收藏夹即成为在线歌单，点选即加载歌曲列表，单击播放。
3. 或者直接在底部「导入」框粘贴 B 站视频链接/`BV 号`，添加并播放。
4. 歌单栏「+」可创建本地歌单；「管理」可重命名或删除歌单（至少保留一个）。
5. 歌单歌曲列表顶部搜索框可实时按标题/UP 主过滤；列表项**右键**可复制 BV 号、收藏/添加到其他本地歌单。
6. 勾选顶部「桌面歌词」，桌面出现透明歌词浮窗；右键/锁定按钮控制穿透与拖动。
7. **键盘快捷键**：`空格` 播放/暂停，`←/→` 快退/快进 5 秒，`↑/↓` 音量 ±5%，`N/P` 下一首/上一首（在输入框打字时自动停用）。
8. 顶栏右侧可退出登录；队列与设置在退出时自动保存。

## 目录与数据

```
src/
├── main.rs            启动入口（--width/--height/--smoke）
├── app/               应用层：MusicApp 结构 + 主界面 + 异步调度（按职责拆文件）
│   ├── mod.rs         结构定义 + eframe 生命周期（ui/logic/on_exit）
│   ├── messages.rs    后台线程消息（AsyncMsg + spawn_* + handle_msg）
│   ├── player.rs      播放控制 + 键盘快捷键
│   ├── playlists.rs   歌单管理
│   ├── lyrics.rs      歌词同步
│   ├── window.rs      窗口控制 + 系统托盘事件
│   └── ui/            界面组件（标题栏/状态栏/歌单/歌曲列表/播放条/设置/登录/桌面歌词…）
├── util/              纯函数工具（格式化/随机数/搜索过滤，带单测）
├── fonts.rs           CJK 字体加载（内嵌 Noto Sans SC + 系统字体兜底）
├── state.rs           PlaybackState / Settings / 歌单模型
├── cover.rs           封面异步下载 + 缩略图缓存
├── theme.rs           主题色板与按钮样式
├── icons.rs           Phosphor 图标
├── tray.rs            系统托盘（feature=tray，可选 GTK）
└── modules/
    ├── bilibili.rs    B 站客户端：扫码登录/收藏夹/BV 解析/playurl DASH 音流（含 WBI 支持）
    ├── audio.rs       音频引擎：下载缓存 + symphonia 解码 + rodio 输出（专用线程 + 命令通道）
    ├── lyrics.rs      vkeys.cn 聚合（QQ/网易）+ LRCLIB 搜索/清洗/匹配 + LRC 解析 + 时间轴同步
    └── storage.rs     配置/会话/队列 JSON 持久化
```

用户数据（跨平台由 `dirs` 语义决定，Linux 如下）：

```
~/.config/simple-music/config.json      设置（桌面歌词开关/锁定/字号等）
~/.config/simple-music/session.json     B 站登录态（Cookie，脱敏）
~/.config/simple-music/playlist.json    播放队列
~/.cache/simple-music/audio/            音频缓存（按 bvid，md5 校验）
```

## 实现要点

- **桌面歌词悬浮窗**：egui 子 viewport（`show_viewport_immediate`），`with_transparent(true) + with_decorations(false) + with_always_on_top()`，固定 800×64；锁定状态通过 `ViewportCommand::MousePassthrough` 运行期切换鼠标穿透（egui 0.36 支持），未锁定时用 `ViewportCommand::StartDrag` 系统级拖动；大号歌词文本用多次偏移重绘近似描边阴影。
- **音频链路**：`BiliClient::resolve_stream` 取 dash 音频流（优先最高码率，未签名请求失败自动补 WBI 签名重试）→ 按 `required_headers`（UA/Referer/Cookie）流式下载 → symphonia (AAC/MP4) 解码 → rodio 输出；播放线程与 UI 线程用 mpsc 命令通道解耦，进度/错误经共享状态轮询；无输出设备绝不 panic，进入错误状态展示。
- **歌词匹配**：`LyricsProvider::fetch` 生成多组候选查询（"上传者+标题"/"标题"），对 LRCLIB 结果按标题相似度 + 上传者命中 + 时长接近打分取最优（阈值 40 分），全失败再走精确 `/get`；LRC 解析支持多时间标签/BOM/CRLF/`[offset:]`，同步引擎二分定位当前句。
- **异步模型**：所有阻塞网络/IO 在后台 `std::thread` 执行，结果经单个 `mpsc` 回主线程；`BiliClient` 以 `Arc<Mutex<..>>` 共享，`AudioEngine` 仅在 UI 线程持有。
- **中文字体**：编译期内嵌 Noto Sans SC Regular（约 16MB）并注册进 egui 字体族；若资产被移除，运行期自动探测系统 CJK 字体（Windows msyh/simhei、macOS PingFang、Linux noto/wqy）。

## 已知限制

- 桌面歌词位置仅在 X11 下可回读并持久化；Wayland 保持居中。
- 收藏夹/登录需真实账号，沙箱 CI 未做端到端验证（接口按官方 v3 文档建模并离线测试解析）。
- 队列为循环模式（末曲后回到第一首）；av 号导入未支持（仅 BV/链接）。
- 仅取视频的音频流轨，视频画面与评论等一律不拉取。
