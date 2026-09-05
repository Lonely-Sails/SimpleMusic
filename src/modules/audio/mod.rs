//! 音频播放模块：专用播放线程 + rodio/symphonia 解码 + 磁盘缓存 + 备用 CDN 下载。
//!
//! 架构（单命令通道 + 共享状态，UI 永不阻塞）：
//! 1. UI 线程持 [`AudioEngine`]，`play/pause/resume/seek/stop/volume` 全部是
//!    非阻塞 mpsc 发送（[`control::Command`]）；
//! 2. 专用播放线程（[`player::worker_loop`]）串行处理命令：取媒体
//!    （[`download::fetch_to_cache`]，缓存优先、CDN 备援）→ 解码
//!    （[`decode::SymphoniaSource`]，内存或文件）→ rodio 输出；
//! 3. 播放状态写 [`control::PlaybackStatus`]（`Arc<Mutex>`），UI 每帧只读轮询：
//!    `loading/playing/finished/position_secs/duration_secs/error/cache_hit`；
//! 4. 时长来源三层兜底：容器元数据 → `size/bandwidth` 估算 →
//!    duration 钳制；`loading` 期间的 pause/seek 会被忽略；
//! 5. 错误展示：`status.error` 非 None 时直接展示该文案（下载失败/解码失败/无设备
//!    都会进入此状态，引擎绝不 panic）。
//!
//! 子模块地图：
//! - [`control`]：`PlaybackStatus` / `Command` / `PlayRequest`（协议层）；
//! - [`cache`]：缓存路径规则与命中判定；
//! - [`decode`]：symphonia 解码源（seek/position/内存输入）；
//! - [`download`]：流式下载 + 缓存复用 + CDN 备援 + 降级内存；
//! - [`player`]：播放线程主循环 + load_and_play + 输出设备；
//! - [`engine`]：`AudioEngine` 句柄（UI 唯一入口）。

pub mod cache;
pub mod control;
pub mod decode;
pub mod download;
pub mod engine;
pub mod player;

pub use cache::{cache_path_in, default_cache_dir};
pub use control::{PlaybackStatus, PlayRequest};
pub use engine::AudioEngine;
