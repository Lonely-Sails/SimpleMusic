//! lyrics_vkeys_probe —— vkeys.cn + LRCLIB 双源歌词实测探针（无需 GUI、无账号）。
//!
//! 运行：`cargo run --no-default-features --example lyrics_vkeys_probe [title] [uploader]`
//! 默认：title="晴天"，uploader="周杰伦"。
//! 流程：`LyricsProvider::fetch` 先查 vkeys（QQ 音乐 → 网易云），再回退 LRCLIB，
//! 命中后打印同步歌词行数与前 5 行。


#[allow(dead_code)]

use simple_music::modules::lyrics::LyricsProvider;

fn main() {
    let title = std::env::args().nth(1).unwrap_or_else(|| "晴天".to_string());
    let uploader = std::env::args().nth(2).unwrap_or_else(|| "周杰伦".to_string());

    println!("=== SimpleMusic lyrics_vkeys_probe ===");
    println!("[query] 候选查询: {:?}", simple_music::modules::lyrics::search_queries(&title, &uploader));

    match LyricsProvider::fetch(&title, &uploader) {
        Some(lyrics) => {
            if let Some(src) = &lyrics.source {
                println!(
                    "[fetch] 来源(LRCLIB): id={} artist=\"{}\" track=\"{}\"",
                    src.id, src.artist_name, src.track_name
                );
            } else {
                println!("[fetch] 来源: vkeys.cn 聚合源");
            }
            println!("[fetch] 有同步歌词: {}", lyrics.has_synced());
            if let Some(lrc) = &lyrics.lrc {
                let lines = simple_music::modules::lyrics::lrc::parse(lrc);
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
            println!("[fetch] 未获取到歌词（网络错误 or 无命中）。");
        }
    }
    println!("=== probe 完成 ===");
}
