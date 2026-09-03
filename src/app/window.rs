//! 窗口关闭/隐藏与系统托盘事件轮询。

use eframe::egui;

use super::MusicApp;

impl MusicApp {
    /// 点击关闭按钮：托盘可用时最小化到托盘，否则直接退出。
    pub(crate) fn request_close(&mut self, ctx: &egui::Context) {
        if self.tray.is_enabled() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.window_hidden = true;
        } else {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// 轮询系统托盘事件（窗口隐藏时也会被 `logic` 调用）。
    ///
    /// 交互约定：**左键单击 = 显示/聚焦主窗口**；**右键 = 托盘菜单**（由系统弹出，
    /// 事件经 `MenuEvent` 回来）。Linux/libappindicator 不上报图标点击事件（点击由
    /// 系统面板打开菜单），属平台限制。
    #[cfg(feature = "tray")]
    pub(crate) fn poll_tray_events(&mut self, ctx: &egui::Context) {
        use tray_icon::menu::MenuEvent;
        use tray_icon::TrayIconEvent;

        // 图标点击：左键（松开为准，避免与按下/双击重复触发）直接显示主窗口。
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            let show = match event {
                TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                } => true,
                // 双击（Windows）同样打开主窗口。
                TrayIconEvent::DoubleClick { .. } => true,
                _ => false,
            };
            if show && self.window_hidden {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                self.window_hidden = false;
            }
        }

        // 菜单事件
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let id = &event.id;
            if id == crate::tray::MENU_TOGGLE {
                // 切换窗口可见性
                if self.window_hidden {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    self.window_hidden = false;
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                    self.window_hidden = true;
                }
            } else if id == crate::tray::MENU_QUIT {
                self.force_quit = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    /// 无托盘编译时的桩方法。
    #[cfg(not(feature = "tray"))]
    pub(crate) fn poll_tray_events(&mut self, _ctx: &egui::Context) {}
}