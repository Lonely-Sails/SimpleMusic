//! 纯函数工具模块。
//!
//! 只放「无 egui / 无 IO / 无网络」的纯函数，全部可离线单测；
//! 不依赖本 crate 的业务类型，供 `app` 与 `modules` 复用。
//!
//! - `fmt`：时长 / 字节数格式化。
//! - `rand`：极简随机数（Xorshift，不引入 rand crate）。
//! - `filter`：歌曲搜索过滤。

pub mod filter;
pub mod fmt;
pub mod rand;
