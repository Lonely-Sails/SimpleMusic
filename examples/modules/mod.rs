//! 示例的模块桥：把主 crate 的 storage/bilibili/audio 模块按原 crate 路径引入
//! 本示例，使 `crate::modules::…` 引用照常解析（不改主 crate 结构）。
#[path = "../../src/modules/storage.rs"]
pub mod storage;
#[path = "../../src/modules/bilibili.rs"]
pub mod bilibili;
#[path = "../../src/modules/audio.rs"]
pub mod audio;
#[path = "../../src/modules/lyrics.rs"]
pub mod lyrics;
