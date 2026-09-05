//! SimpleMusic 库目标：应用逻辑与数据模块的宿主。
//!
//! 二进制入口在 `main.rs`（薄壳：命令行解析 + eframe 启动 + `--smoke` 自检）；
//! examples/ 的诊断探针（bili_probe / audio_probe / lyrics_probe）与本 crate
//! 的集成测试都从这里 `use simple_music::…`，不再用 `#[path]` 复制源码。
//!
//! 模块地图（自上而下 = 依赖方向）：
//! - `modules/`：纯后端能力，不依赖 egui —— `bilibili`（B 站 HTTP 客户端）、
//!   `lyrics`（歌词多源搜索/LRC 解析/缓存）、`audio`（下载缓存 + symphonia 解码
//!   + rodio 输出）、`storage`（config/session/歌单/歌词缓存的 JSON 持久化）。
//! - `state.rs`：跨层数据模型（Settings/QueueItem/Playlist/PlaybackState…）。
//! - `app/`：UI 状态机 —— `MusicApp`（eframe::App）+ 每帧 `logic` + 按区域拆分的
//!   `app::ui`；后台任务经 `app::messages::AsyncMsg` 单通道回主线程。
//! - 其余为 UI 支撑：`theme`（语义色板）、`icons`（自绘 Phosphor 图标）、
//!   `fonts`（字体安装/系统扫描）、`text_shadow`（真·模糊文字阴影纹理）、
//!   `cover`（封面缩略图缓存）、`tray`（系统托盘）。

pub mod app;
pub mod cover;
pub mod fonts;
pub mod icons;
pub mod modules;
pub mod state;
pub mod text_shadow;
pub mod theme;
pub mod tray;
pub mod util;
