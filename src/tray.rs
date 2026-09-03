//! 系统托盘图标（可选 feature `tray`）。
//!
//! 平台策略：
//! - **Linux**：独立 GTK 线程 + libappindicator 托盘图标（tray-icon 的 Linux 后端要求
//!   一个 GTK 事件循环），菜单事件经全局跨线程通道 (`MenuEvent::receiver`) 送至主线程轮询。
//! - **macOS / Windows**：直接用系统原生托盘（macOS = NSStatusItem，Windows =
//!   Shell_NotifyIcon），无需额外线程、无需 GTK。macOS 要求托盘图标必须在**主线程**且
//!   事件循环运行中创建，因此 `start()` 仅占位，由 `MusicApp::new`（主线程）调用
//!   [`Tray::init_on_main_thread`] 真正创建。
//! - 未启用 `tray` feature：no-op 桩（`Tray::is_enabled` 恒为 `false`），主程序照常运行。
//!
//! 点击交互约定（见 `app/window.rs::poll_tray_events`）：
//! - **左键单击**：直接显示/聚焦主窗口（macOS/Windows 上报 `TrayIconEvent`；Linux 上
//!   libappindicator 不上报点击事件，点击由系统面板打开菜单——平台限制）。
//! - **右键**：弹出托盘菜单（显示/隐藏主窗口、退出）。

#[cfg(feature = "tray")]
mod inner {
    use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
    use tray_icon::Icon;

    // -----------------------------------------------------------------------
    // 托盘菜单项 ID（主线程通过 `MenuEvent::receiver()` 匹配）
    // -----------------------------------------------------------------------

    /// 托盘菜单：显示/隐藏主窗口
    pub const MENU_TOGGLE: &str = "simple_music:toggle";
    /// 托盘菜单：退出应用
    pub const MENU_QUIT: &str = "simple_music:quit";

    pub use self::platform::*;

    // -----------------------------------------------------------------------
    // 跨平台共享：菜单构建
    // -----------------------------------------------------------------------

    fn build_menu() -> Menu {
        let menu = Menu::new();

        let toggle = MenuItem::with_id(MENU_TOGGLE, "显示/隐藏主窗口", true, None);
        let _ = menu.append(&toggle);

        let _ = menu.append(&PredefinedMenuItem::separator());

        let quit = MenuItem::with_id(MENU_QUIT, "退出", true, None);
        let _ = menu.append(&quit);

        menu
    }

    // -----------------------------------------------------------------------
    // 跨平台共享：图标生成（32×32 雾青蓝圆底 + 白色双音符 ♫）
    // -----------------------------------------------------------------------

    fn make_icon() -> Result<Icon, tray_icon::BadIcon> {
        const S: usize = 32;
        let mut buf = vec![0u8; S * S * 4];
        let cx = 15.5;
        let cy = 15.5;
        let radius = 15.0;

        // 背景圆：雾青蓝渐变（上浅下深）
        for y in 0..S {
            for x in 0..S {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = (dx * dx + dy * dy).sqrt();
                if d <= radius {
                    let t = (y as f32 / S as f32).clamp(0.0, 1.0);
                    let idx = (y * S + x) * 4;
                    buf[idx] = lerp(0x8F, 0x6E, t); // R
                    buf[idx + 1] = lerp(0xB8, 0x9B, t); // G
                    buf[idx + 2] = lerp(0xD0, 0xB8, t); // B
                    buf[idx + 3] = 255;
                }
            }
        }

        // 白色音符 ♫（双符头 + 双符干 + 横梁）
        let white = [255u8, 255, 255, 255];

        // 左符头
        fill_circle(&mut buf, S, 10.5, 21.0, 3.2, white);
        // 右符头
        fill_circle(&mut buf, S, 18.5, 21.0, 3.2, white);
        // 左符干
        fill_rect(&mut buf, S, 13.4, 9.5, 15.0, 21.5, white);
        // 右符干
        fill_rect(&mut buf, S, 21.4, 9.5, 23.0, 21.5, white);
        // 横梁
        fill_rect(&mut buf, S, 13.4, 9.5, 23.0, 11.2, white);

        Icon::from_rgba(buf, S as u32, S as u32)
    }

    // -----------------------------------------------------------------------
    // 像素绘图辅助
    // -----------------------------------------------------------------------

    fn lerp(a: u8, b: u8, t: f32) -> u8 {
        (a as f32 + (b as f32 - a as f32) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    }

    fn fill_circle(buf: &mut [u8], s: usize, cxc: f32, cyc: f32, r: f32, color: [u8; 4]) {
        let x0 = (cxc - r).floor().max(0.0) as usize;
        let x1 = (cxc + r).ceil().min(s as f32 - 1.0) as usize;
        let y0 = (cyc - r).floor().max(0.0) as usize;
        let y1 = (cyc + r).ceil().min(s as f32 - 1.0) as usize;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = x as f32 + 0.5 - cxc;
                let dy = y as f32 + 0.5 - cyc;
                if dx * dx + dy * dy <= r * r {
                    let idx = (y * s + x) * 4;
                    buf[idx..idx + 4].copy_from_slice(&color);
                }
            }
        }
    }

    fn fill_rect(buf: &mut [u8], s: usize, x0: f32, y0: f32, x1: f32, y1: f32, color: [u8; 4]) {
        let xi0 = x0.floor().max(0.0) as usize;
        let xi1 = x1.ceil().min(s as f32) as usize;
        let yi0 = y0.floor().max(0.0) as usize;
        let yi1 = y1.ceil().min(s as f32) as usize;
        for y in yi0..yi1 {
            for x in xi0..xi1 {
                let idx = (y * s + x) * 4;
                buf[idx..idx + 4].copy_from_slice(&color);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Linux：独立 GTK 线程实现
    // -----------------------------------------------------------------------

    #[cfg(target_os = "linux")]
    mod platform {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        use tray_icon::TrayIconBuilder;

        use super::{build_menu, make_icon};

        pub struct Tray {
            shutdown: Arc<AtomicBool>,
            handle: Option<thread::JoinHandle<()>>,
            /// 托盘是否成功启用（GTK 初始化 + 图标创建 ok）。
            enabled: Arc<AtomicBool>,
        }

        impl Tray {
            /// 启动托盘线程（best-effort，失败不 panic）。
            pub fn start() -> Self {
                let shutdown = Arc::new(AtomicBool::new(false));
                let enabled = Arc::new(AtomicBool::new(false));
                let (tx, rx) = std::sync::mpsc::channel();

                let sd = shutdown.clone();
                let en = enabled.clone();
                let handle = thread::Builder::new()
                    .name("simple-music-tray".into())
                    .spawn(move || tray_main(sd, en, tx))
                    .ok();

                // 等待线程报告初始化结果（最多 1.5 s）。
                let _ = rx.recv_timeout(Duration::from_millis(1500));

                Self {
                    shutdown,
                    handle,
                    enabled,
                }
            }

            /// Linux 上图标已在线程内创建，无需主线程初始化（no-op）。
            pub fn init_on_main_thread(&mut self) {}

            /// 托盘是否可用。
            pub fn is_enabled(&self) -> bool {
                self.enabled.load(Ordering::Relaxed)
            }

            /// 请求托盘线程退出（应在 `on_exit` 中调用）。
            pub fn stop(&mut self) {
                self.shutdown.store(true, Ordering::Relaxed);
                if let Some(h) = self.handle.take() {
                    let _ = h.join();
                }
            }
        }

        impl Drop for Tray {
            fn drop(&mut self) {
                self.stop();
            }
        }

        fn tray_main(
            shutdown: Arc<AtomicBool>,
            enabled: Arc<AtomicBool>,
            ready: std::sync::mpsc::Sender<()>,
        ) {
            if gtk::init().is_err() {
                eprintln!("[tray] GTK 初始化失败（无显示环境？），托盘不可用");
                let _ = ready.send(());
                return;
            }

            let icon = match make_icon() {
                Ok(i) => i,
                Err(e) => {
                    eprintln!("[tray] 生成图标失败: {e}");
                    let _ = ready.send(());
                    return;
                }
            };

            let menu = build_menu();
            let tray = TrayIconBuilder::new()
                .with_tooltip("SimpleMusic 音乐播放器")
                .with_icon(icon)
                // 左键不弹菜单（左键=显示主窗口，由主程序监听 TrayIconEvent 处理）。
                // Linux/libappindicator 不支持该开关（菜单由系统面板接管），无副作用。
                .with_menu_on_left_click(false)
                .with_menu(Box::new(menu))
                .build();

            match tray {
                Ok(_tray_icon) => {
                    enabled.store(true, Ordering::Relaxed);
                    let _ = ready.send(());
                    eprintln!("[tray] 托盘图标已就绪");

                    // GTK 主循环迭代（非阻塞，配合 shutdown 信号退出）。
                    while !shutdown.load(Ordering::Relaxed) {
                        gtk::main_iteration_do(false);
                        thread::sleep(Duration::from_millis(50));
                    }
                    eprintln!("[tray] 托盘线程退出");
                }
                Err(e) => {
                    eprintln!("[tray] 创建托盘失败: {e}");
                    let _ = ready.send(());
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // macOS / Windows：主线程原生托盘（无 GTK、无额外线程）
    // -----------------------------------------------------------------------

    #[cfg(not(target_os = "linux"))]
    mod platform {
        use tray_icon::TrayIconBuilder;

        use super::{build_menu, make_icon};

        /// 持有原生托盘图标；图标存活期间一直显示，drop 时自动从系统托盘移除。
        pub struct Tray {
            icon: Option<tray_icon::TrayIcon>,
        }

        impl Tray {
            /// 占位：macOS 要求在主线程事件循环运行中创建图标，
            /// 因此真正创建推迟到 [`Tray::init_on_main_thread`]。
            pub fn start() -> Self {
                Self { icon: None }
            }

            /// 在主线程（应用启动后）创建托盘图标；由 `MusicApp::new` 调用。
            pub fn init_on_main_thread(&mut self) {
                if self.icon.is_some() {
                    return;
                }
                let icon = match make_icon() {
                    Ok(i) => i,
                    Err(e) => {
                        eprintln!("[tray] 生成图标失败: {e}");
                        return;
                    }
                };
                let menu = build_menu();
                match TrayIconBuilder::new()
                    .with_tooltip("SimpleMusic 音乐播放器")
                    .with_icon(icon)
                    // 左键不弹菜单（左键=显示主窗口，由主程序监听 TrayIconEvent 处理）。
                    .with_menu_on_left_click(false)
                    .with_menu(Box::new(menu))
                    .build()
                {
                    Ok(tray) => {
                        eprintln!("[tray] 托盘图标已就绪");
                        self.icon = Some(tray);
                    }
                    Err(e) => {
                        eprintln!("[tray] 创建托盘失败: {e}");
                    }
                }
            }

            /// 托盘是否可用。
            pub fn is_enabled(&self) -> bool {
                self.icon.is_some()
            }

            /// 移除托盘图标（Drop 时也会自动调用）。
            pub fn stop(&mut self) {
                self.icon.take();
            }
        }

        impl Drop for Tray {
            fn drop(&mut self) {
                self.stop();
            }
        }
    }
}

#[cfg(not(feature = "tray"))]
mod inner {
    /// 无托盘编译的 no-op 桩：保持与启用时相同的 API 面。
    pub struct Tray;

    pub const MENU_TOGGLE: &str = "simple_music:toggle";
    pub const MENU_QUIT: &str = "simple_music:quit";

    impl Tray {
        pub fn start() -> Self {
            Self
        }
        pub fn init_on_main_thread(&mut self) {}
        pub fn is_enabled(&self) -> bool {
            false
        }
        pub fn stop(&mut self) {}
    }

    impl Drop for Tray {
        fn drop(&mut self) {}
    }
}

pub use inner::*;
