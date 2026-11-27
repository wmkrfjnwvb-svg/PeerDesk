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
/// Windows 不靠它：Windows 的退出走窗口消息，立即生效（见 `peerdesk_platform::winshell`）。
#[cfg(not(windows))]
const QUIT_WATCHDOG: Duration = Duration::from_secs(5);

/// 托盘点击、托盘菜单、第二次启动、`peerdesk window ...` 请求都汇合到这里：唤醒界面处理命令。
///
/// Windows：窗口隐藏后 `update` 不再运行（没有 `WM_PAINT`，见 `winshell.rs`），所以“显示”和“退出”
/// 直接给主窗口发消息，由界面线程的窗口过程立即处理；命令同时进队列，窗口显示、`update` 恢复后
/// 再按原有的逻辑处理一遍（同步 winit 记的“可见”状态，才能第二次正常隐藏）。
/// 其他系统：隐藏的窗口 `update` 还在跑，走 eframe 的视口命令。
fn wake(ctx: &egui::Context, command: TrayCommand) {
    #[cfg(windows)]
    {
        use peerdesk_platform::winshell;
        // 断开连接要 `update` 里才有会话的句柄：先把窗口显示出来，命令随后被处理
        let posted = match command {
            TrayCommand::Open | TrayCommand::Disconnect => winshell::request_show(),
            TrayCommand::Quit => winshell::request_quit(),
        };
        if !posted {
            log::warn!("主窗口没有登记，托盘命令只能等界面线程处理：{command:?}");
        }
    }
    if command == TrayCommand::Open {
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
    }
    #[cfg(not(windows))]
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

/// 像用户点窗口的关闭按钮一样关窗口（`peerdesk window close`）。
fn wake_close(ctx: &egui::Context) {
    #[cfg(windows)]
    if peerdesk_platform::winshell::request_close() {
        return;
    }
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    ctx.request_repaint();
}

/// Windows：在界面创建时调用（界面线程），登记主窗口句柄，之后托盘命令可以不经过 `update` 直接显示、退出。
/// 其他系统什么也不做。
pub fn register_window(cc: &eframe::CreationContext<'_>) {
    #[cfg(windows)]
    native_window::register(cc);
    #[cfg(not(windows))]
    let _ = cc;
}

/// macOS：接住点程序坞图标的“重新打开”事件，当成“打开”命令。其他系统什么也不做。
pub fn attach_dock(ctx: &egui::Context, tx: &Sender<TrayCommand>) {
    #[cfg(target_os = "macos")]
    dock::attach(ctx, tx);
    #[cfg(not(target_os = "macos"))]
    let _ = (ctx, tx);
}

#[cfg(windows)]
mod native_window {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    pub fn register(cc: &eframe::CreationContext<'_>) {
        let hwnd = match cc.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => handle.hwnd.get(),
            other => {
                log::warn!("取不到主窗口句柄：{other:?}");
                return;
            }
        };
        if !peerdesk_platform::winshell::install(hwnd, super::native::quit_now) {
            log::warn!("登记主窗口失败，托盘命令要等界面刷新才生效");
        }
    }
}

#[cfg(target_os = "macos")]
mod dock {
    use std::sync::mpsc::Sender;
    use std::sync::OnceLock;

    use eframe::egui;

    use super::TrayCommand;

    static TARGET: OnceLock<(egui::Context, Sender<TrayCommand>)> = OnceLock::new();

    fn on_reopen() {
        log::info!("程序坞图标被点击（重新打开），显示窗口");
        if let Some((ctx, tx)) = TARGET.get() {
            if tx.send(TrayCommand::Open).is_ok() {
                super::wake(ctx, TrayCommand::Open);
            }
        }
    }

    pub fn attach(ctx: &egui::Context, tx: &Sender<TrayCommand>) {
        let _ = TARGET.set((ctx.clone(), tx.clone()));
        if let Err(why) = peerdesk_platform::macshell::install_reopen_handler(on_reopen) {
            log::warn!("接不住程序坞图标的点击：{why}");
        }
    }
}

/// 后台线程的轮询间隔。
const REQUEST_POLL: Duration = Duration::from_millis(200);

/// 其他进程留下的请求（第二次启动、`peerdesk window ...`，见 `crate::instance`）：后台线程看到就照办。
pub fn watch_requests(ctx: &egui::Context, dir: &std::path::Path, tx: &Sender<TrayCommand>) {
    let (ctx, dir, tx) = (ctx.clone(), dir.to_owned(), tx.clone());
    let spawned = std::thread::Builder::new()
        .name("window-request".to_owned())
        .spawn(move || loop {
            std::thread::sleep(REQUEST_POLL);
            let Some(request) = crate::instance::take_request(&dir) else {
                continue;
            };
            log::info!("收到窗口请求：{}", request.word());
            let command = match request {
                crate::instance::Request::Show => TrayCommand::Open,
                crate::instance::Request::Quit => TrayCommand::Quit,
                crate::instance::Request::Close => {
                    wake_close(&ctx);
                    continue;
                }
            };
            if tx.send(command).is_err() {
                return;
            }
            wake(&ctx, command);
        });
    if let Err(e) = spawned {
        log::warn!("无法监听窗口请求：{e}");
    }
}

/// 发一条系统通知（第一次关闭窗口时的提示）。返回通知有没有发出去：
/// 命令启动不了，或者很快就以失败退出（例如没有通知服务），都算没发出去。
/// 窗口马上要隐藏，窗口里的提示没人看得到，所以必须是系统通知。
pub fn notify(title: &str, body: &str) -> bool {
    let command = tray_logic::notify_command(NotifyOs::current(), title, body);
    let Some((program, args)) = command.split_first() else {
        // 没有命令：Windows，直接弹提示框（不起外部程序）
        #[cfg(windows)]
        return peerdesk_platform::winshell::message_box_async(title, body);
        #[cfg(not(windows))]
        return false;
    };
    let mut process = std::process::Command::new(program);
    let _ = process
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _ = crate::process::quiet(&mut process);
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
    use std::cell::RefCell;

    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder, TrayIconEvent};

    use super::tray_logic::TrayClick;
    use super::{egui, tray_logic, IconKind, Sender, Strings, TrayCommand};

    thread_local! {
        /// 托盘图标放在创建它的（界面）线程里：退出时要在这个线程把它摘掉，否则 Windows 的通知区域会留下
        /// 一个死图标，要鼠标划过才消失。
        static ICON: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
    }

    /// 托盘图标的句柄（图标本身在线程局部变量里）。
    pub struct Tray {
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

    /// 在界面线程里立即退出：摘掉托盘图标，以退出码 0 结束（守护不会拉起）。
    /// Windows 的“退出”走这里：窗口隐藏时 `update` 不运行，不能等它。代价：不经过 eframe 的正常收尾，
    /// 对端已在上面收到带原因的关闭；单实例锁和网络连接由系统回收。
    #[cfg(windows)]
    pub fn quit_now() {
        // 先告诉所有会话的对端“我退出了”（最多等 300 毫秒让关闭帧发出去），再立即退出
        let told = crate::gui::session::say_goodbye();
        log::info!("退出（托盘菜单或窗口请求），已通知 {told} 个会话的对端");
        ICON.with(|icon| drop(icon.borrow_mut().take()));
        std::process::exit(i32::from(crate::EXIT_OK));
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
                // Windows：左键单击、双击直接打开窗口，右键出菜单（Tailscale、RustDesk 一样）。
                // macOS：菜单栏图标的习惯是左键出菜单。
                .with_menu_on_left_click(cfg!(target_os = "macos"))
                .build()
                .map_err(|e| e.to_string())?;
            ICON.with(|icon| *icon.borrow_mut() = Some(tray));
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
                // TrayIconEvent 是外部库的非穷尽枚举，必须有通配分支
                #[allow(clippy::wildcard_enum_match_arm)]
                let click = match event {
                    TrayIconEvent::Click {
                        button: tray_icon::MouseButton::Left,
                        button_state: tray_icon::MouseButtonState::Up,
                        ..
                    } => TrayClick::Left,
                    TrayIconEvent::DoubleClick {
                        button: tray_icon::MouseButton::Left,
                        ..
                    } => TrayClick::LeftDouble,
                    _ => TrayClick::Other,
                };
                if let Some(command) = tray_logic::click_command(click, cfg!(windows)) {
                    if click_tx.send(command).is_ok() {
                        super::wake(&click_ctx, command);
                    }
                }
            }));
            Ok(Self {
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
            ICON.with(|slot| {
                let slot = slot.borrow();
                let Some(tray) = slot.as_ref() else {
                    return;
                };
                match icon_for(tray_logic::icon_kind(connected)) {
                    Ok((icon, template)) => {
                        if let Err(e) = tray.set_icon_with_as_template(Some(icon), template) {
                            log::warn!("换托盘图标失败：{e}");
                        }
                    }
                    Err(e) => log::warn!("托盘图标解码失败：{e}"),
                }
                if let Err(e) = tray.set_tooltip(Some(tray_logic::tooltip(connected, self.s))) {
                    log::warn!("换托盘提示失败：{e}");
                }
                match build_menu(connected, self.s) {
                    Ok(menu) => tray.set_menu(Some(Box::new(menu))),
                    Err(e) => log::warn!("换托盘菜单失败：{e}"),
                }
            });
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
