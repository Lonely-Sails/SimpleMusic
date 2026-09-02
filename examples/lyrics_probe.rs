//! lyrics_probe —— LRCLIB 歌词模块实测探针（无需 GUI、无账号）。
//!
//! 运行：`cargo run --example lyrics_probe [title] [uploader]`
//! 默认：title="晴天"，uploader="周杰伦"。
//! 流程：
//! 1. 打印候选查询（`search_queries`）。
//! 2. `LyricsProvider::fetch` 拉取歌词（先搜索、后回退精确 GET）。
//! 3. 命中后打印来源元信息、同步歌词前 5 行、纯文本歌词前 2 行。
//! 失败（网络/无命中）也如实打印错误信息。

#[path = "../src/state.rs"]
#[allow(dead_code)]
pub mod state;

// 桥接模块复用主 crate 的完整实现，示例只用其中一部分，容忍 dead_code。
#[allow(dead_code)]
mod modules;

use modules::lyrics::LyricsProvider;

fn main() {
    let title = std::env::args().nth(1).unwrap_or_else(|| "晴天".to_string());
    let uploader = std::env::args().nth(2).unwrap_or_else(|| "周杰伦".to_string());

    println!("=== SimpleMusic lyrics_probe ===");
    println!("UA: {}", modules::lyrics::LRCLIB_UA);
    println!("[fetch] title={title:?} uploader={uploader:?}");
    println!("[query] 候选查询(按尝试顺序): {:?}", modules::lyrics::search_queries(&title, &uploader));

    match LyricsProvider::fetch(&title, &uploader) {
        Some(lyrics) => {
            if let Some(src) = &lyrics.source {
                println!(
                    "[fetch] 来源: id={} artist=\"{}\" track=\"{}\" album=\"{}\" duration={}s instrumental={}",
                    src.id, src.artist_name, src.track_name, src.album_name, src.duration, src.instrumental
                );
            }
            println!("[fetch] 有同步歌词: {}", lyrics.has_synced());
            if let Some(lrc) = &lyrics.lrc {
                let lines = modules::lyrics::lrc::parse(lrc);
                println!("[synced] 解析到 {} 行，前 5 行：", lines.len());
                for (i, l) in lines.iter().take(5).enumerate() {
                    println!("  #{i} [{:8.3}s] {}", l.time_secs, l.text);
                }
            }
            println!("[plain] 纯文本，前 2 行：");
            for (i, line) in lyrics.plain.lines().take(2).enumerate() {
                println!("  #{i} {line}");
            }
        }
        None => {
            println!("[fetch] 未获取到歌词（网络错误 or 无命中；详见上方查询）。");
        }
    }
    println!("=== probe 完成 ===");
}
