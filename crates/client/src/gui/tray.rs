//! 系统托盘（菜单栏）图标的接线：只调用系统托盘库，状态逻辑在 `tray_logic.rs`。
//! 要有真实的桌面才能运行，所以和 `app.rs` 一样不计入覆盖率（见 `scripts/coverage.sh`），
//! 由三个系统上的实机测试验证。
//!
//! 库的选择（许可证都已核对，见 `THIRD_PARTY_NOTICES.md`）：
//! - Windows、macOS：`tray-icon`（MIT OR Apache-2.0），Tauri 项目维护，没有系统库依赖。
//!   菜单事件在回调里转成 [`TrayCommand`] 发给主窗口，并唤醒界面线程。
//! - Linux：`ksni`（Unlicense，即公有领域），纯 Rust 的 StatusNotifierItem（D-Bus），不需要任何系统库。
//!   没有用 `tray-icon` 的 Linux 实现：它依赖 GTK3 和 libayatana-appindicator（LGPL 系统库），
//!   还要在单独的线程里跑 GTK 主循环，和 winit 打架。
//!   代价：桌面要支持 StatusNotifierItem。KDE、XFCE、Cinnamon、MATE 自带；
//!   GNOME 要装 AppIndicator 扩展（Ubuntu 默认带）。不支持时 [`Tray::create`] 返回原因，不假装有托盘。

use std::sync::mpsc::Sender;
use std::time::Duration;

use eframe::egui;

use super::i18n::Strings;
use super::tray_logic::{self, IconKind, NotifyOs, TrayCommand};

/// 点“退出”后，如果界面线程一直不响应（窗口隐藏时有的系统不再重绘），多久后强制以退出码 0 结束。
/// 用户明确点了退出，不能因为界面卡住就退不掉；退出码 0 保证守护不会再把它拉起。
const QUIT_WATCHDOG: Duration = Duration::from_secs(5);

/// 唤醒界面线程：窗口隐藏时也要处理托盘命令。
fn wake(ctx: &egui::Context, command: TrayCommand) {
    if command == TrayCommand::Open {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
    if command == TrayCommand::Quit {
        let _ = std::thread::Builder::new()
            .name("quit-watchdog".to_owned())
            .spawn(|| {
                std::thread::sleep(QUIT_WATCHDOG);
                log::warn!("界面没有响应退出命令，强制退出");
                std::process::exit(i32::from(crate::EXIT_OK));
            });
    }
    ctx.request_repaint();
}

/// 第二次启动程序时会留下显示请求（见 `crate::instance`）：后台线程看到就当成“打开主窗口”。
pub fn watch_show_requests(ctx: &egui::Context, dir: &std::path::Path, tx: &Sender<TrayCommand>) {
    let (ctx, dir, tx) = (ctx.clone(), dir.to_owned(), tx.clone());
    let spawned = std::thread::Builder::new()
        .name("show-request".to_owned())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_millis(400));
            if crate::instance::take_show_request(&dir) {
                if tx.send(TrayCommand::Open).is_err() {
                    return;
                }
                wake(&ctx, TrayCommand::Open);
            }
        });
    if let Err(e) = spawned {
        log::warn!("无法监听第二次启动的显示请求：{e}");
    }
}

/// 发一条系统通知（第一次关闭窗口时的提示）。返回通知有没有发出去：
/// 命令启动不了，或者很快就以失败退出（例如没有通知服务），都算没发出去。
/// 窗口马上要隐藏，窗口里的提示没人看得到，所以必须是系统通知。
pub fn notify(title: &str, body: &str) -> bool {
    let command = tray_logic::notify_command(NotifyOs::current(), title, body);
    let Some((program, args)) = command.split_first() else {
        return false;
    };
    let mut process = std::process::Command::new(program);
    let _ = process
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW：不闪出 PowerShell 的控制台窗口
        let _ = process.creation_flags(0x0800_0000);
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(e) => {
            log::warn!("发不出系统通知（{program}）：{e}");
            return false;
        }
    };
    // 等一小会儿看它是不是马上失败；PowerShell 的气泡要显示几秒，还在运行就当成功
    for _ in 0..15 {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => return false,
        }
    }
    let _ = std::thread::Builder::new()
        .name("notify-reaper".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
    true
}

#[cfg(any(windows, target_os = "macos"))]
pub use native::Tray;

#[cfg(all(unix, not(target_os = "macos")))]
pub use sni::Tray;

#[cfg(any(windows, target_os = "macos"))]
mod native {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::{egui, tray_logic, IconKind, Sender, Strings, TrayCommand};

    /// 托盘图标。
    pub struct Tray {
        icon: TrayIcon,
        connected: bool,
        s: &'static Strings,
    }

    impl std::fmt::Debug for Tray {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Tray")
                .field("connected", &self.connected)
                .finish_non_exhaustive()
        }
    }

    fn icon_for(kind: IconKind) -> Result<(Icon, bool), String> {
        // macOS 没人连时用模板图（系统按深浅色着色）；有人连着时模板图画不出青绿色，用彩色图。
        let (bytes, template) = if cfg!(target_os = "macos") && kind == IconKind::Idle {
            (tray_logic::MAC_TEMPLATE_PNG, true)
        } else {
            (tray_logic::icon_png(kind), false)
        };
        let (rgba, w, h) = tray_logic::decode_rgba(bytes)?;
        let icon = Icon::from_rgba(rgba, w, h).map_err(|e| e.to_string())?;
        Ok((icon, template))
    }

    fn build_menu(connected: bool, s: &Strings) -> Result<Menu, String> {
        let menu = Menu::new();
        for entry in tray_logic::menu(connected, s) {
            let item = MenuItem::with_id(entry.command.id(), entry.label, true, None);
            menu.append(&item).map_err(|e| e.to_string())?;
        }
        Ok(menu)
    }

    impl Tray {
        /// 创建托盘图标。必须在主线程（界面创建时）调用。失败时返回原因。
        pub fn create(
            ctx: &egui::Context,
            s: &'static Strings,
            tx: &Sender<TrayCommand>,
        ) -> Result<Self, String> {
            let (icon, template) = icon_for(IconKind::Idle)?;
            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(build_menu(false, s)?))
                .with_tooltip(tray_logic::tooltip(false, s))
                .with_icon(icon)
                .with_icon_as_template(template)
                // Windows：左键直接打开窗口，右键出菜单（Tailscale、RustDesk 一样）。
                // macOS：菜单栏图标的习惯是左键出菜单。
                .with_menu_on_left_click(cfg!(target_os = "macos"))
                .build()
                .map_err(|e| e.to_string())?;
            let (menu_ctx, menu_tx) = (ctx.clone(), tx.clone());
            MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
                if let Some(command) = TrayCommand::from_id(event.id.as_ref()) {
                    if menu_tx.send(command).is_ok() {
                        super::wake(&menu_ctx, command);
                    }
                }
            }));
            let (click_ctx, click_tx) = (ctx.clone(), tx.clone());
            TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
                if let TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                } = event
                {
                    if cfg!(windows) && click_tx.send(TrayCommand::Open).is_ok() {
                        super::wake(&click_ctx, TrayCommand::Open);
                    }
                }
            }));
            Ok(Self {
                icon: tray,
                connected: false,
                s,
            })
        }

        /// 有没有人连着变了：换图标、提示文字和菜单（有人连着时多一项“断开连接”）。
        pub fn set_connected(&mut self, connected: bool) {
            if connected == self.connected {
                return;
            }
            self.connected = connected;
            match icon_for(tray_logic::icon_kind(connected)) {
                Ok((icon, template)) => {
                    if let Err(e) = self.icon.set_icon_with_as_template(Some(icon), template) {
                        log::warn!("换托盘图标失败：{e}");
                    }
                }
                Err(e) => log::warn!("托盘图标解码失败：{e}"),
            }
            if let Err(e) = self
                .icon
                .set_tooltip(Some(tray_logic::tooltip(connected, self.s)))
            {
                log::warn!("换托盘提示失败：{e}");
            }
            match build_menu(connected, self.s) {
                Ok(menu) => self.icon.set_menu(Some(Box::new(menu))),
                Err(e) => log::warn!("换托盘菜单失败：{e}"),
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod sni {
    use ksni::blocking::{Handle, TrayMethods};
    use ksni::menu::StandardItem;

    use super::{egui, tray_logic, IconKind, Sender, Strings, TrayCommand};

    /// StatusNotifierItem 的内容。
    struct Item {
        connected: bool,
        s: &'static Strings,
        tx: Sender<TrayCommand>,
        ctx: egui::Context,
        idle: ksni::Icon,
        active: ksni::Icon,
    }

    impl std::fmt::Debug for Item {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Item")
                .field("connected", &self.connected)
                .finish_non_exhaustive()
        }
    }

    impl Item {
        fn send(&self, command: TrayCommand) {
            if self.tx.send(command).is_ok() {
                super::wake(&self.ctx, command);
            }
        }
    }

    impl ksni::Tray for Item {
        fn id(&self) -> String {
            "peerdesk".to_owned()
        }

        fn title(&self) -> String {
            "PeerDesk".to_owned()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![if self.connected {
                self.active.clone()
            } else {
                self.idle.clone()
            }]
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: tray_logic::tooltip(self.connected, self.s).to_owned(),
                ..ksni::ToolTip::default()
            }
        }

        /// 左键点图标：打开窗口。
        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayCommand::Open);
        }

        fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
            tray_logic::menu(self.connected, self.s)
                .into_iter()
                .map(|entry| {
                    let command = entry.command;
                    StandardItem {
                        label: entry.label.to_owned(),
                        activate: Box::new(move |this: &mut Self| this.send(command)),
                        ..StandardItem::default()
                    }
                    .into()
                })
                .collect()
        }
    }

    fn icon(kind: IconKind) -> Result<ksni::Icon, String> {
        let (rgba, w, h) = tray_logic::decode_rgba(tray_logic::icon_png(kind))?;
        Ok(ksni::Icon {
            width: i32::try_from(w).map_err(|e| e.to_string())?,
            height: i32::try_from(h).map_err(|e| e.to_string())?,
            data: tray_logic::rgba_to_argb(&rgba),
        })
    }

    /// 托盘图标。
    pub struct Tray {
        handle: Handle<Item>,
        connected: bool,
    }

    impl std::fmt::Debug for Tray {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Tray")
                .field("connected", &self.connected)
                .finish_non_exhaustive()
        }
    }

    impl Tray {
        /// 创建托盘图标。桌面不支持 StatusNotifierItem（没有 StatusNotifierWatcher 或没有托盘宿主）时返回原因。
        pub fn create(
            ctx: &egui::Context,
            s: &'static Strings,
            tx: &Sender<TrayCommand>,
        ) -> Result<Self, String> {
            let item = Item {
                connected: false,
                s,
                tx: tx.clone(),
                ctx: ctx.clone(),
                idle: icon(IconKind::Idle)?,
                active: icon(IconKind::Active)?,
            };
            let handle = item.spawn().map_err(|e| e.to_string())?;
            Ok(Self {
                handle,
                connected: false,
            })
        }

        /// 有没有人连着变了：换图标、提示文字和菜单。
        pub fn set_connected(&mut self, connected: bool) {
            if connected == self.connected {
                return;
            }
            self.connected = connected;
            if self
                .handle
                .update(|item| item.connected = connected)
                .is_none()
            {
                log::warn!("托盘服务已经停止，图标不再更新");
            }
        }
    }
}
