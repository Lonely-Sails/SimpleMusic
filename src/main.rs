//! SimpleMusic —— 基于 Rust + eframe/egui 的原生桌面音乐播放器（无 WebView）。
//!
//! 本文件是薄壳：命令行解析、`--smoke` 模块自检与 eframe 启动；
//! 全部应用逻辑在库目标 `simple_music`（见 `lib.rs` 的模块地图）。

use simple_music::{app, fonts, modules, state, theme, tray};
use state::Settings;

/// 简易命令行/环境变量解析（保持依赖少，不引入 clap）。
struct LaunchOptions {
    width: f32,
    height: f32,
    /// `--smoke`：不创建窗口；执行模块初始化后打印 SMOKE_OK 并退出。
    smoke: bool,
}

impl LaunchOptions {
    /// 解析优先级：命令行 flag > 环境变量 > 默认值。
    ///
    /// 用法：
    /// - `simple-music --width 1024 --height 640`
    /// - `simple-music --smoke`（无窗口自检）
    fn parse() -> Self {
        let mut width = 520.0_f32;
        let mut height = 820.0_f32;
        let mut smoke = false;

        let args: Vec<String> = std::env::args().collect();
        let mut i = 1;
        while i < args.len() {
            let flag = args[i].as_str();
            let value = args.get(i + 1).and_then(|v| v.parse::<f32>().ok());
            match flag {
                "--width" | "-w" => {
                    if let Some(v) = value {
                        width = v;
                        i += 1;
                    }
                }
                "--height" | "-h" => {
                    if let Some(v) = value {
                        height = v;
                        i += 1;
                    }
                }
                "--smoke" => {
                    smoke = true;
                }
                "--help" => {
                    println!(
                        "用法: simple-music [--width N] [--height N] [--smoke]\n也可用环境变量 SIMPLE_MUSIC_WIDTH / SIMPLE_MUSIC_HEIGHT"
                    );
                    std::process::exit(0);
                }
                _ => {}
            }
            i += 1;
        }

        if let Ok(v) = std::env::var("SIMPLE_MUSIC_WIDTH") {
            if let Ok(n) = v.parse::<f32>() {
                width = n;
            }
        }
        if let Ok(v) = std::env::var("SIMPLE_MUSIC_HEIGHT") {
            if let Ok(n) = v.parse::<f32>() {
                height = n;
            }
        }

        LaunchOptions {
            width: width.max(320.0),
            height: height.max(240.0),
            smoke,
        }
    }
}

/// `--smoke`：无窗口执行模块初始化自检。
///
/// 依次：
/// 1. `BiliClient::new`（建客户端）→ `ensure_buvid`（**走网络**，finger/spi 取设备指纹）；
/// 2. `Settings::load`；
/// 3. `AudioEngine::new`（构建、**不播**）；
/// 4. `LyricsProvider` 空探针（走一次 LRCLIB 网络查询，预期 None）。
///
/// 网络异常不阻断：照常打印诊断与 `SMOKE_OK` 后以 0 退出；仅客户端/引擎构建失败才返回非 0。
fn run_smoke() -> i32 {
    println!("[smoke] SimpleMusic 模块自检");

    // 1. BiliClient（建客户端 + 网络取 buvid）。
    match modules::bilibili::BiliClient::new() {
        Ok(mut c) => {
            println!("[smoke] BiliClient::new: OK");
            match c.ensure_buvid() {
                Ok(_) => println!("[smoke] ensure_buvid (network): OK"),
                Err(e) => println!("[smoke] ensure_buvid (network): {e}（不影响后续自检）"),
            }
            println!("[smoke] logged_in={} mid={:?}", c.logged_in(), c.mid());
        }
        Err(e) => {
            println!("[smoke] BiliClient::new: FAILED {e}");
            return 1;
        }
    }

    // 2. Settings。
    match Settings::load() {
        Some(s) => println!("[smoke] Settings::load: OK {s:?}"),
        None => println!("[smoke] Settings::load: OK（无配置，使用默认）"),
    }

    // 3. 歌单。
    let playlists = modules::storage::load_playlists();
    println!("[smoke] Playlists::load: OK（{} 个歌单）", playlists.len());

    // 4. AudioEngine（构建、不播）。
    let engine = modules::audio::AudioEngine::new();
    println!(
        "[smoke] AudioEngine::new: OK（未播放；cache_dir={:?}）",
        engine.cache_dir()
    );
    drop(engine);

    // 5. LyricsProvider 空探针（一次网络查询，预期 None）。
    let probe = modules::lyrics::LyricsProvider::fetch("", "");
    println!(
        "[smoke] LyricsProvider empty probe: {}",
        if probe.is_some() {
            "unexpected 命中（可能网络异常）"
        } else {
            "None（符合预期）"
        }
    );

    println!("[smoke] SMOKE_OK");
    0
}

fn main() -> eframe::Result<()> {
    let opts = LaunchOptions::parse();

    if opts.smoke {
        let code = run_smoke();
        std::process::exit(code);
    }

    // 系统托盘：独立 GTK 线程，best-effort（无显示环境时自动禁用）。
    let tray = tray::Tray::start();

    // 浮窗形态：去掉系统标题栏（自绘）+ 透明圆角卡片 + 紧凑尺寸。
    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title("SimpleMusic")
        .with_inner_size([opts.width, opts.height])
        .with_min_inner_size([380.0, 560.0])
        .with_decorations(false)
        .with_titlebar_shown(false)
        .with_transparent(true)
        .with_resizable(true);

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "SimpleMusic",
        native_options,
        Box::new(move |cc| {
            // 先读设置（可能指定了界面字体），再注册字体：选择在首帧渲染前生效。
            let settings = Settings::load().unwrap_or_default();
            // 注册字体：设置选择优先，Auto 时文字优先系统字体（图标恒用内嵌 Phosphor）。
            let font_choice = fonts::install_fonts(
                &cc.egui_ctx,
                settings.ui_font.path().map(std::path::Path::new),
            );
            match font_choice {
                fonts::FontChoice::System(p) => {
                    println!("[font] 界面字体: {}（图标用内嵌 Phosphor）", p.display());
                }
                fonts::FontChoice::Embedded => {
                    println!("[font] 界面字体: 内嵌 Noto Sans SC（未探测到系统字体）+ 内嵌 Phosphor 图标");
                }
            }
            // 应用深色淡雅主题。
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(app::MusicApp::new(cc, settings, tray)))
        }),
    )
}
