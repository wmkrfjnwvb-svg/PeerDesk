//! 主窗口（eframe 接线）：左侧导航（远程控制、设备、设置），右侧页面；首页是“本设备”和
//! “远程控制设备”两张卡片加最近连接。
//! 只画控件、把操作交给会话线程；逻辑在同目录的其他模块里。

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Layout, RichText, TextEdit};
use peerdesk_core::proto::PeerOs;
use peerdesk_core::signal::format_code;
use peerdesk_net::identity::to_hex;
use peerdesk_net::netcheck::NetReport;
use peerdesk_net::rendezvous::Lookup;
use peerdesk_net::Identity;
use peerdesk_platform::guide::{guide, mac_settings_names, Action, Guide, Os};

use super::book::{Book, Device, Id, Row};
use super::form::{parse_pin, parse_port, ConnectForm, Target, DEFAULT_PORT};
use super::home::{
    ago, book_error, connect_line, host_badge, nat_hint, os_name, server_badge, unix_now, Editing,
    HostBadge, LineAction, Tab,
};
use super::i18n::{effective_lang, save_lang, strings, Lang, Strings};
use super::netinfo::lan_addresses;
use super::session::{start_host, start_view, HostConfig, HostEvent, HostHandle, Repaint};
use super::settings::ServerSettings;
use super::state::{HostPhase, Status};
use super::stats::short_fingerprint;
use super::theme::{
    big_input, brand_mark, danger_button, dot, empty_state, font, icon, icon_button, link_button,
    muted, nav_item, notice, pad, page_title, panel, pill, primary_button, radius,
    secondary_button, section_title, segmented, size, space, spaced, text, toggle, Appearance,
    ControlSize, Palette, Signal, Weight,
};
use super::viewer::Viewer;
use super::{guide_art, password, theme};
use crate::remote::default_data_dir;
use crate::{EXIT_FAILED, EXIT_OK};

/// 自测模式：启动后自动开启被控、再连接本机。值为 `host` 时只开启被控。CI 用它截图。
const SELFTEST_ENV: &str = "PEERDESK_GUI_SELFTEST";
/// 页面内容最宽这么多（再宽，一行字太长不好读）。
const PAGE_MAX_WIDTH: f32 = 1060.0;
/// “这台电脑”的面板里面够这么宽时，状态和设备码、密码排成一行；不够就放到下一行。
const THIS_PC_WIDE: f32 = 900.0;
/// 面板里面不到这么宽时，设备码用小一号的字。
const CODE_WIDE: f32 = 640.0;
/// 设备表：设备码、上次连接、操作三列的宽度（名称用剩下的）。
const COL_CODE: f32 = 150.0;
const COL_LAST: f32 = 110.0;
const COL_ACTIONS: f32 = 200.0;
/// 设备表的行高：设备（两行字）、分组（一行字）、表头。
const ROW_HEIGHT: f32 = 56.0;
const GROUP_ROW_HEIGHT: f32 = 40.0;
const HEADER_HEIGHT: f32 = 40.0;
/// 就地编辑的输入框宽度。
const EDIT_WIDTH: f32 = 260.0;
/// “最近”里最多显示这么多台。
const RECENT_SHOWN: usize = 10;
/// 远程窗口标题的开头，后面是设备码或地址。
const TITLE_PREFIX: &str = "PeerDesk — ";
/// 复制等操作的轻提示显示多久。
const TOAST: Duration = Duration::from_millis(1500);
/// 普通的状态信息显示多久（错误一直显示，直到下一条）。
const STATUS_FADE: Duration = Duration::from_secs(6);
/// 启动时打开哪一页（home、devices、settings、permissions），截图用。
const PAGE_ENV: &str = "PEERDESK_GUI_PAGE";
/// 启动时设备列表看哪一栏（recent、favorites、groups、network），截图用。
const TAB_ENV: &str = "PEERDESK_GUI_TAB";
/// 预览 macOS 授权引导的环境变量（值是 macOS 主版本号）。
const PREVIEW_MAC_ENV: &str = "PEERDESK_GUI_PREVIEW_MAC";
/// 权限向导多久重新检查一次权限。
const GUIDE_REFRESH: Duration = Duration::from_secs(1);

/// 打开图形界面，直到窗口关闭。返回进程退出码。
pub fn run() -> u8 {
    #[cfg(target_os = "linux")]
    if let Some(why) = crate::libcheck::check() {
        log::error!("{why}");
        let _ = writeln!(std::io::stderr(), "{why}");
        return EXIT_FAILED;
    }
    let lang = effective_lang(&default_data_dir());
    peerdesk_net::rendezvous::set_prefer_ipv6(lang.prefers_ipv6());
    let s = strings(lang);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(s.app_title)
        .with_app_id("peerdesk")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([860.0, 600.0]);
    // 窗口图标（品牌标志，见 packaging/icons/README.md）。解码失败时不设图标，记日志
    match eframe::icon_data::from_png_bytes(include_bytes!("../../assets/window-icon.png")) {
        Ok(icon) => viewport = viewport.with_icon(icon),
        Err(e) => log::warn!("窗口图标解码失败，不设图标：{e}"),
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let result = eframe::run_native(
        s.app_title,
        options,
        Box::new(move |cc| Ok(Box::new(App::new(&cc.egui_ctx, s)))),
    );
    match result {
        Ok(()) => EXIT_OK,
        Err(e) => {
            let _ = writeln!(
                std::io::stderr(),
                "无法打开图形界面 / cannot open the GUI: {e}"
            );
            EXIT_FAILED
        }
    }
}

/// 连上之后要记进最近连接的内容（认证通过、握手完成之后才记）。
struct Remember {
    /// 地址（设备码或 `IP:端口`）。
    addr: String,
    /// 核对过的指纹。
    pin: String,
    /// 要加密保存的密码；`None` 表示不保存（并删掉以前保存的）。
    password: Option<String>,
}

/// 设备列表里的一个操作（点按钮、选菜单项），画完这一帧再统一执行。
enum Act {
    /// 连接这台设备。
    Connect(Id),
    /// 连接后打开文件管理。
    Files(Id),
    /// 收藏或取消收藏。
    Favorite(Id, bool),
    /// 把设备移到分组（`None` 是未分组）。
    Move(Id, Option<Id>),
    /// 复制设备码或地址。
    Copy(Id),
    /// 删除设备记录。
    Forget(Id),
    /// 折叠或展开分组。
    Collapse(Id, bool),
    /// 删除分组。
    DeleteGroup(Id),
    /// 开始就地编辑（新建分组、改名）。
    Edit(Editing),
    /// 保存正在编辑的内容。
    Commit,
    /// 放弃正在编辑的内容。
    Cancel,
}

struct App {
    s: &'static Strings,
    data_dir: PathBuf,
    fingerprint: Result<String, String>,
    password: String,
    port: String,
    server: ServerSettings,
    device_code: Option<u32>,
    net: Option<NetReport>,
    os: Os,
    guide: Guide,
    guide_checked: Instant,
    host: Option<HostHandle>,
    phase: HostPhase,
    addrs: Vec<IpAddr>,
    form: ConnectForm,
    form_password: String,
    /// 连上之后把密码加密保存（见 `crate::vault`）。
    remember_password: bool,
    /// 保存的密码：用到时才打开（打开要读系统钥匙串，有的系统会弹授权框），打不开时记着原因。
    vault: Option<Result<crate::vault::Vault, crate::vault::VaultError>>,
    /// 设备簿：连接过的设备和分组。
    book: Book,
    /// 设备列表现在看哪一栏。
    tab: Tab,
    /// 正在就地编辑的东西、输入框里的字、要不要把光标放进去。
    editing: Option<(Editing, String, bool)>,
    /// 设备列表上一次操作失败的原因（显示在表格上面）。
    book_error: Option<String>,
    /// 哪一行的“更多操作”菜单开着（鼠标移开后这一行的按钮也要留着）。
    menu_open: Option<Id>,
    /// 连接表单里显示指纹核对那一行。
    show_pin: bool,
    /// 下一帧把光标放进地址框、密码框。
    focus_addr: bool,
    focus_password: bool,
    views: Vec<(Viewer, Option<Remember>)>,
    /// 文件管理窗口。
    files: super::files_window::FilesWindow,
    /// 在设备列表里点了“文件传输”的设备地址：连上之后自动打开它的文件管理。
    files_after: Option<String>,
    serial: u64,
    status: Status,
    selftest: bool,
    selftest_connect: bool,
    repaint: Repaint,
    page: Page,
    show_password: bool,
    toast: Option<(String, Instant)>,
    /// 连接表单的错误（显示在连接按钮下面，改了输入就消失）。
    form_error: Option<String>,
    status_text: String,
    status_since: Instant,
    perms: peerdesk_platform::PermissionStatus,
    /// 预览 macOS 引导（在别的系统上截图用）：权限一律当作没有。
    preview_mac: bool,
    appearance: Appearance,
    /// 打开程序后自动开启被控（默认开；用户手动关过就记住，下次不自动开）。权限齐了才真正开启。
    auto_host: bool,
    /// 无人值守服务的状态（每 2 秒读一次；没装为 `None`）。
    service: Option<crate::unattended::Status>,
    service_checked: Option<Instant>,
    /// 本机电脑名（首页“本机”卡片上显示）。
    name: String,
    /// 没找到兜底用的系统中文字体（生僻字会显示成方框）：设置页里一直提示。
    font_fallback_missing: bool,
}

/// 用户手动关闭被控后留下的标记文件。
const HOST_OFF_FILE: &str = "host-off";

/// 主窗口的页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    /// 设备：这台电脑、连接另一台电脑、设备列表。
    Home,
    /// 文件传输。
    Files,
    Settings,
    Permissions,
}

impl App {
    /// 中文 / English 切换：立即生效并记住；英文（海外）同时优先走 IPv6 直连。
    fn language_switch(&mut self, ui: &mut egui::Ui) {
        let current = if std::ptr::eq(self.s, strings(Lang::Zh)) {
            Lang::Zh
        } else {
            Lang::En
        };
        let mut chosen = current;
        // 从右往左排：先放 English
        ui.selectable_value(&mut chosen, Lang::En, "English");
        ui.selectable_value(&mut chosen, Lang::Zh, "中文");
        if chosen != current {
            self.s = strings(chosen);
            peerdesk_net::rendezvous::set_prefer_ipv6(chosen.prefers_ipv6());
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Title(self.s.app_title.to_owned()));
            if let Err(e) = save_lang(&self.data_dir, chosen) {
                self.status = Status::error(e.to_string());
            }
        }
    }

    fn new(ctx: &egui::Context, s: &'static Strings) -> Self {
        let fonts = theme::install(ctx);
        let appearance = Appearance::load(&default_data_dir());
        appearance.apply(ctx);
        let data_dir = default_data_dir();
        let fingerprint = Identity::load_or_create(&data_dir)
            .map(|id| to_hex(&id.fingerprint()))
            .map_err(|e| format!("{}：{e}", s.err_identity));
        let (password, status) = match password::load_or_create(&data_dir) {
            Ok(p) => (p, Status::info(s.ready)),
            Err(e) => (String::new(), Status::error(e)),
        };
        // 上次崩溃了：后台把日志传给服务器（配置了服务器就传给它，否则传给测试版默认服务器）
        let server = ServerSettings::load(&data_dir);
        crate::crashreport::upload_in_background(&data_dir, server.server(s).ok().flatten());
        let ctx2 = ctx.clone();
        let auto_host = !data_dir.join(HOST_OFF_FILE).exists();
        // PEERDESK_GUI_PREVIEW_MAC=15：在任何系统上预览 macOS 15 的授权引导（截图、设计评审用）
        let preview = std::env::var(PREVIEW_MAC_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<u32>().ok());
        let preview_mac = preview.is_some();
        let os = preview.map_or_else(peerdesk_platform::current_os, |major| Os::MacOs {
            major,
            minor: 0,
        });
        let perms = if preview_mac {
            peerdesk_platform::PermissionStatus {
                screen_capture: false,
                input_injection: false,
            }
        } else {
            peerdesk_platform::check_permissions()
        };
        Self {
            s,
            book: Book::load(&data_dir, unix_now()),
            tab: match std::env::var(TAB_ENV).as_deref() {
                Ok("favorites") => Tab::Favorites,
                Ok("groups") => Tab::Groups,
                Ok("network") => Tab::Network,
                Ok(_) | Err(_) => Tab::Recent,
            },
            editing: None,
            book_error: None,
            menu_open: None,
            show_pin: false,
            focus_addr: false,
            focus_password: false,
            server: ServerSettings::load(&data_dir),
            device_code: None,
            net: None,
            os,
            guide: guide(os, perms),
            guide_checked: Instant::now(),
            data_dir,
            fingerprint,
            password,
            port: DEFAULT_PORT.to_string(),
            host: None,
            phase: HostPhase::Off,
            addrs: lan_addresses(),
            form: ConnectForm::default(),
            form_password: String::new(),
            remember_password: true,
            vault: None,
            views: Vec::new(),
            files: {
                // PEERDESK_GUI_FILES=1：启动就打开文件管理（截图用）
                let mut w = super::files_window::FilesWindow::new(default_data_dir());
                w.open = std::env::var_os("PEERDESK_GUI_FILES").is_some();
                w
            },
            files_after: None,
            serial: 0,
            status,
            selftest: std::env::var_os(SELFTEST_ENV).is_some(),
            selftest_connect: std::env::var_os(SELFTEST_ENV).is_some_and(|v| v != "host"),
            repaint: Arc::new(move || ctx2.request_repaint()),
            page: match std::env::var(PAGE_ENV).as_deref() {
                Ok("files") => Page::Files,
                Ok("settings") => Page::Settings,
                Ok("permissions") => Page::Permissions,
                Ok(_) | Err(_) => Page::Home,
            },
            // 密码默认显示（要念给对方听），旁边的按钮可以藏起来
            show_password: true,
            toast: None,
            form_error: None,
            status_text: String::new(),
            status_since: Instant::now(),
            perms,
            preview_mac,
            appearance,
            auto_host,
            service: None,
            service_checked: None,
            name: crate::local::device_name(),
            font_fallback_missing: fonts.system_cjk.is_none(),
        }
    }

    fn start_hosting(&mut self) {
        let s = self.s;
        let port = match parse_port(&self.port, s) {
            Ok(port) => port,
            Err(e) => return self.status = Status::error(e),
        };
        let server = match self.server.server(s) {
            Ok(server) => server,
            Err(e) => return self.status = Status::error(e),
        };
        self.device_code = None;
        let config = HostConfig {
            port,
            data_dir: self.data_dir.clone(),
            server,
            fps: 60,
        };
        self.host = Some(start_host(
            config,
            &self.password,
            Arc::clone(&self.repaint),
        ));
        self.phase = HostPhase::Starting;
        self.status = Status::info(s.host_starting);
        self.addrs = lan_addresses();
    }

    fn stop_hosting(&mut self) {
        if let Some(host) = &self.host {
            host.stop();
            self.phase = HostPhase::Stopping;
        }
    }

    /// 保存的密码（打开一次，之后复用；打不开时返回原因）。
    fn vault(&mut self) -> Result<&mut crate::vault::Vault, crate::vault::VaultError> {
        let data_dir = self.data_dir.clone();
        self.vault
            .get_or_insert_with(|| {
                crate::vault::Vault::open(&data_dir, &crate::keychain::SystemKeyStore)
            })
            .as_mut()
            .map_err(|e| e.clone())
    }

    /// `addr` 保存的密码；没有或打不开时为空。
    fn saved_password(&mut self, addr: &str) -> String {
        match self.vault() {
            Ok(v) => v.get(addr).unwrap_or_default().to_owned(),
            Err(e) => {
                log::warn!("{e}");
                String::new()
            }
        }
    }

    fn connect(&mut self, target: Target, remember: Option<Remember>) {
        self.serial += 1;
        let title = match &target {
            Target::Addr { addr, .. } => format!("PeerDesk — {addr}"),
            Target::Device {
                device: Lookup::Device(device),
                ..
            } => format!("PeerDesk — {}", short_fingerprint(&to_hex(device))),
            Target::Device {
                device: Lookup::Code(code),
                ..
            } => format!("PeerDesk — {}", format_code(*code)),
        };
        let handle = start_view(
            target,
            &self.form_password,
            self.data_dir.clone(),
            Arc::clone(&self.repaint),
        );
        // 进度和失败原因显示在连接表单下面（见 `connect_feedback`），不再另外写一条状态
        self.form_error = None;
        self.views
            .push((Viewer::new(self.serial, title, handle), remember));
    }

    fn connect_from_form(&mut self) {
        match self.form.target(self.s, self.server.server(self.s)) {
            Ok(mut target) => {
                if let Target::Device { account, .. } = &mut target {
                    account.clone_from(&self.server.account);
                }
                let pin = match target {
                    Target::Addr { .. } => self.form.pin.clone(),
                    Target::Device { .. } => String::new(),
                };
                let remember = Some(Remember {
                    addr: self.form.remembered(),
                    pin,
                    password: self.remember_password.then(|| self.form_password.clone()),
                });
                self.connect(target, remember);
            }
            Err(e) => {
                // 表单的错误就近显示在连接按钮下面
                self.form_error = Some(e.clone());
                self.status = Status::error(e);
            }
        }
    }

    fn poll_host(&mut self) {
        let events = self.host.as_ref().map(HostHandle::poll).unwrap_or_default();
        for event in events {
            if let HostEvent::Registered { code, .. } = event {
                self.device_code = Some(code);
            }
            if let HostEvent::Network(report) = &event {
                self.net = Some(report.clone());
            }
            let (phase, status) = self.phase.apply(&event, self.s);
            self.phase = phase;
            if let Some(status) = status {
                self.status = status;
            }
            if self.phase == HostPhase::Off {
                self.host = None;
                self.device_code = None;
            }
            if self.selftest {
                if let HostPhase::Listening { port, .. } = self.phase {
                    self.selftest = false;
                    if !self.selftest_connect {
                        continue;
                    }
                    self.form_password = self.password.clone();
                    let target = Target::Addr {
                        addr: SocketAddr::from(([127, 0, 0, 1], port)),
                        pin: None,
                    };
                    self.connect(target, None);
                }
            }
        }
    }

    /// 权限向导：每秒重新检查一次权限，用户在系统设置里打开开关后这里立刻变成打勾。
    fn permission_guide(&mut self, ui: &mut egui::Ui, p: &Palette) {
        self.refresh_permissions(ui.ctx());
        if self.guide.steps.is_empty() || self.guide.ready {
            return;
        }
        // macOS：首页只放一条提示，点进专门的引导页（有示意图和分步操作）
        if matches!(self.os, Os::MacOs { .. }) {
            notice(ui, p, Signal::Relay, |ui, _| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icon::SHIELD_WARNING)
                            .size(font::ICON_LARGE)
                            .color(p.relay.text),
                    );
                    ui.label(text(self.s.perm_banner, font::BODY, Weight::Semibold));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if primary_button(ui, p, self.s.perm_start, ControlSize::Medium, true)
                            .clicked()
                        {
                            self.page = Page::Permissions;
                        }
                    });
                });
            });
            ui.add_space(space::S12);
            return;
        }
        notice(ui, p, Signal::Relay, |ui, _| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(icon::SHIELD_WARNING)
                        .size(font::ICON)
                        .color(p.relay.text),
                );
                ui.label(text(&self.guide.title, font::BODY, Weight::Semibold).color(p.relay.text));
            });
            for step in self.guide.steps.clone() {
                ui.horizontal_wrapped(|ui| {
                    let (mark, color) = if step.done {
                        (icon::CHECK_CIRCLE, p.direct.text)
                    } else {
                        (icon::WARNING, p.relay.text)
                    };
                    ui.label(RichText::new(mark).color(color));
                    ui.label(&step.text);
                    if let Some((label, action)) = step.button {
                        if ui.button(label).clicked() {
                            if let Err(e) = peerdesk_platform::perform(action) {
                                self.status = Status::error(e);
                            }
                            // 授权对话框关掉后马上重新检查。
                            self.guide_checked = Instant::now() - GUIDE_REFRESH;
                        }
                    }
                });
            }
        });
        ui.add_space(space::S12);
    }

    fn copy(&mut self, ui: &egui::Ui, text: &str) {
        ui.ctx().copy_text(text.to_owned());
        self.toast = Some((self.s.copied.to_owned(), Instant::now()));
    }

    /// 轻提示（例如“已复制”）：内容区底部居中，一会儿就消失。
    fn show_toast(&mut self, ctx: &egui::Context, p: &Palette) {
        let Some((text, at)) = &self.toast else {
            return;
        };
        if at.elapsed() > TOAST {
            self.toast = None;
            return;
        }
        let text = text.clone();
        egui::Area::new(egui::Id::new("toast"))
            .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(100.0, -48.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(p.ink)
                    .corner_radius(radius::PILL)
                    .inner_margin(pad(space::S16, space::S8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{}  {text}", icon::CHECK_CIRCLE))
                                .color(p.on_ink),
                        );
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    /// 读无人值守服务的状态。服务在跑时本程序不再自己当被控端（端口和设备都让给服务）。
    fn check_service(&mut self) {
        let due = self
            .service_checked
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
        if !due {
            return;
        }
        self.service_checked = Some(Instant::now());
        self.service = crate::unattended::read_status(&crate::unattended::base_dir());
        if self.service.as_ref().is_some_and(|st| st.running) {
            self.auto_host = false;
            if self.phase.is_on() {
                self.stop_hosting();
            }
        }
    }

    /// 用户拨了“允许别人连接”的开关：开启或关闭被控，并记住这个选择（手动关了，下次打开不再自动开启）。
    fn set_hosting(&mut self, on: bool) {
        let marker = self.data_dir.join(HOST_OFF_FILE);
        let saved = if on {
            self.start_hosting();
            std::fs::remove_file(&marker).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(e)
                }
            })
        } else {
            self.auto_host = false;
            self.stop_hosting();
            std::fs::write(&marker, b"")
        };
        if let Err(e) = saved {
            log::warn!("记住被控开关失败：{e}");
        }
    }

    /// 用户点了“换一个密码”：生成并保存（重启后还是这个），正在被控时立刻生效。
    fn renew_password(&mut self) {
        match password::generate().and_then(|pw| {
            password::save(&self.data_dir, &pw)?;
            Ok(pw)
        }) {
            Ok(pw) => {
                if let Some(host) = &self.host {
                    host.set_password(&pw);
                }
                self.password = pw;
            }
            Err(e) => self.status = Status::error(e),
        }
    }

    /// 设备码那一组：标签在上，大号的设备码和复制按钮在下。还没有设备码时说明在等什么。
    fn code_block(&mut self, ui: &mut egui::Ui, p: &Palette, code: Option<u32>, waiting: bool) {
        let s = self.s;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::S6;
            let _ = muted(ui, p, s.device_code);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S12;
                ui.set_min_height(size::CONTROL_LARGE);
                // 窗口窄的时候设备码用小一号的字，不把密码挤到下一行
                let size = if ui.available_width() < CODE_WIDE {
                    font::CODE_COMPACT
                } else {
                    font::CODE
                };
                match code {
                    Some(code) => {
                        let shown = format_code(code);
                        ui.label(spaced(
                            &shown,
                            size,
                            Weight::Semibold,
                            font::CODE_TRACKING,
                            p.ink,
                        ));
                        if icon_button(ui, p, icon::COPY, s.copy_address, ControlSize::Large)
                            .clicked()
                        {
                            self.copy(ui, &code.to_string());
                        }
                    }
                    None if waiting => {
                        ui.add(egui::Spinner::new().size(font::ICON).color(p.muted));
                        let _ = muted(ui, p, s.code_pending);
                    }
                    None => {
                        ui.label(text("—", size, Weight::Semibold).color(p.idle.solid));
                    }
                }
            });
        });
    }

    /// 密码那一组：密码、显示或隐藏、复制、换一个。
    fn password_block(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::S6;
            let _ = muted(ui, p, s.password);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S8;
                ui.set_min_height(size::CONTROL_LARGE);
                let password = self.password.clone();
                let shown = if self.show_password {
                    password.clone()
                } else {
                    "•".repeat(password.chars().count())
                };
                ui.label(spaced(
                    &shown,
                    font::SECRET,
                    Weight::Medium,
                    font::SECRET_TRACKING,
                    p.ink,
                ));
                let (eye, tip) = if self.show_password {
                    (icon::EYE_SLASH, s.hide_password)
                } else {
                    (icon::EYE, s.show_password)
                };
                if icon_button(ui, p, eye, tip, ControlSize::Large).clicked() {
                    self.show_password = !self.show_password;
                }
                if icon_button(ui, p, icon::COPY, s.copy_password, ControlSize::Large).clicked() {
                    self.copy(ui, &password);
                }
                if icon_button(
                    ui,
                    p,
                    icon::ARROWS_CLOCKWISE,
                    s.new_password,
                    ControlSize::Large,
                )
                .clicked()
                {
                    self.renew_password();
                }
            });
        });
    }

    /// 状态那一组：状态标签，下面一句说明（现在是什么情况、怎么办）；正在被控时写明是谁，可以断开。
    fn host_status_block(&mut self, ui: &mut egui::Ui, p: &Palette, badge: &HostBadge) {
        let s = self.s;
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = space::S6;
            let _ = pill(ui, p, badge.label, badge.signal);
            ui.add(
                egui::Label::new(text(badge.hint, font::SMALL, Weight::Regular).color(p.muted))
                    .wrap(),
            );
            if !self.server.is_set()
                && self.service.is_none()
                && link_button(ui, p, s.go_settings).clicked()
            {
                self.page = Page::Settings;
            }
            if let HostPhase::Busy { addr, peer, .. } = &self.phase {
                let who = format!("{}  {addr}  {}", s.controller, short_fingerprint(peer));
                ui.horizontal(|ui| {
                    if danger_button(ui, p, s.disconnect, ControlSize::Small).clicked() {
                        if let Some(host) = &self.host {
                            host.kick();
                        }
                    }
                    ui.add(egui::Label::new(text(&who, font::SMALL, Weight::Regular)).truncate())
                        .on_hover_text(&who);
                });
            }
        });
    }

    /// “这台电脑”：设备码、密码、允许别人连接的开关、状态。
    fn this_pc_section(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        // 无人值守服务在跑：设备码是服务的，本程序不自己当被控端，也没有开关和临时密码
        let service = self
            .service
            .as_ref()
            .filter(|st| st.running)
            .map(|st| st.code);
        let mut on = self.phase.is_on();
        // 权限不全时不能开启被控（关闭总是可以）。
        let enabled = !matches!(self.phase, HostPhase::Stopping | HostPhase::Starting)
            && (self.guide.ready || self.phase.is_on());
        let mut toggled = false;
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if service.is_none() {
                    ui.spacing_mut().item_spacing.x = space::S10;
                    ui.label(s.allow_incoming);
                    toggled = toggle(ui, p, &mut on, s.allow_incoming, enabled).changed();
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = space::S10;
                    let _ = page_title(ui, s.this_pc);
                    ui.add(egui::Label::new(RichText::new(&self.name).color(p.muted)).truncate());
                });
            });
        });
        if toggled {
            self.set_hosting(on);
        }
        ui.add_space(space::S6);
        let badge = match service {
            Some(_) => HostBadge {
                label: s.unattended,
                signal: Signal::Direct,
                hint: s.unattended_hint,
            },
            None => host_badge(&self.phase, self.server.is_set(), self.guide.ready, s),
        };
        let code = service.unwrap_or(self.device_code);
        let waiting = service.is_some() || (self.phase.is_on() && self.server.is_set());
        panel(ui, p, |ui| {
            let wide = ui.available_width() >= THIS_PC_WIDE;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S48;
                self.code_block(ui, p, code, waiting);
                if service.is_none() {
                    self.password_block(ui, p);
                }
                if wide {
                    self.host_status_block(ui, p, &badge);
                }
            });
            if !wide {
                ui.add_space(space::S14);
                self.host_status_block(ui, p, &badge);
            }
        });
    }

    /// 连接表单下面那一行：连接进度、没连上的原因（都在原地说，不弹窗）。
    fn connect_feedback(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        if let Some(e) = &self.form_error {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WARNING).color(p.problem.solid));
                ui.add(egui::Label::new(RichText::new(e).color(p.problem.text)).wrap());
            });
            return;
        }
        // 最近发起的那一次连接
        let Some((index, line)) = self.views.iter().enumerate().rev().find_map(|(i, v)| {
            let target = v.0.title().trim_start_matches(TITLE_PREFIX);
            connect_line(v.0.phase(), target, s).map(|line| (i, line))
        }) else {
            return;
        };
        let colors = p.signal(line.signal);
        let mut clicked = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S10;
            match line.action {
                LineAction::Cancel => {
                    ui.add(egui::Spinner::new().size(font::SUBTITLE).color(p.ink));
                    ui.label(RichText::new(&line.text).color(p.ink));
                    clicked = link_button(ui, p, s.cancel).clicked();
                }
                LineAction::Retry => {
                    ui.label(RichText::new(icon::WARNING).color(colors.solid));
                    // 先放右边的按钮，原因用剩下的宽度，放不下就换行
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        clicked = link_button(ui, p, s.retry).clicked();
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&line.text).color(colors.text))
                                    .wrap(),
                            );
                        });
                    });
                }
            }
        });
        if clicked {
            if let Some(view) = self.views.get_mut(index) {
                view.0.stop();
            }
            if line.action == LineAction::Retry {
                self.views.remove(index);
                self.connect_from_form();
            }
        }
    }

    /// “连接另一台电脑”：设备码或地址、连接、传文件；填了地址才出现密码那一行。
    fn connect_section(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.label(text(s.connect_heading, font::SUBTITLE, Weight::Semibold));
        ui.add_space(space::S4);
        let enter = |r: &egui::Response, ui: &egui::Ui| {
            r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
        };
        let ready = !self.form.addr.trim().is_empty();
        let (mut go, mut files) = (false, false);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S10;
            // 先放右边的两个按钮，输入框用剩下的宽度
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                files = secondary_button(ui, p, s.send_files, ControlSize::Large, ready)
                    .on_hover_text(s.files_after_connect)
                    .clicked();
                let label = format!("{}  {}", s.connect_this, icon::ARROW_RIGHT);
                go = primary_button(ui, p, &label, ControlSize::Large, ready).clicked();
                let r = big_input(ui, p, &mut self.form.addr, s.code_hint, false);
                go |= enter(&r, ui);
                if r.changed() {
                    self.form_error = None;
                }
                if self.focus_addr {
                    self.focus_addr = false;
                    r.request_focus();
                }
            });
        });
        if ready {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S14;
                ui.scope(|ui| {
                    ui.set_max_width(size::INPUT_WIDTH + space::S48);
                    let r = big_input(ui, p, &mut self.form_password, s.password_input_hint, true);
                    go |= enter(&r, ui);
                    if self.focus_password {
                        self.focus_password = false;
                        r.request_focus();
                    }
                });
                // 记住密码：加密保存在本机（系统钥匙串用不了时不能勾，悬停说明原因）
                let blocked = match &self.vault {
                    Some(Err(e)) => Some(e.to_string()),
                    Some(Ok(_)) | None => None,
                };
                let r = ui.add_enabled(
                    blocked.is_none(),
                    egui::Checkbox::new(&mut self.remember_password, s.remember_password),
                );
                if let Some(why) = blocked {
                    let _ = r.on_disabled_hover_text(why);
                }
                if !self.show_pin && link_button(ui, p, s.verify_fingerprint).clicked() {
                    self.show_pin = true;
                }
            });
            if self.show_pin || !self.form.pin.is_empty() {
                let _ = muted(ui, p, s.pin);
                let r = big_input(ui, p, &mut self.form.pin, "", false);
                go |= enter(&r, ui);
            }
        }
        if (go || files) && ready {
            self.files_after = files.then(|| self.form.remembered());
            self.connect_from_form();
        }
        self.connect_feedback(ui, p);
    }

    /// 从设备列表发起连接：填好表单（有保存的密码就填上）。有密码就直接连，没有就把光标放到密码框。
    fn connect_device(&mut self, id: Id, files: bool) {
        let Some(device) = self.book.device(id).cloned() else {
            return;
        };
        self.form_password = self.saved_password(&device.addr);
        self.form.addr = device.code().0;
        self.form.pin = device.pin;
        self.form_error = None;
        self.files_after = files.then(|| device.addr.clone());
        if self.form_password.is_empty() {
            self.focus_password = true;
        } else {
            self.connect_from_form();
        }
    }

    /// 执行设备列表里的一个操作（点按钮、选菜单项）。失败的原因显示在表格上面。
    fn apply(&mut self, ui: &egui::Ui, act: Act) {
        let now = unix_now();
        let s = self.s;
        let result = match act {
            Act::Connect(id) => {
                self.connect_device(id, false);
                Ok(())
            }
            Act::Files(id) => {
                self.connect_device(id, true);
                Ok(())
            }
            Act::Favorite(id, on) => self.book.set_favorite(id, on, now),
            Act::Move(id, group) => self.book.move_device(id, group, now),
            Act::Copy(id) => {
                if let Some(addr) = self.book.device(id).map(|d| d.addr.clone()) {
                    self.copy(ui, &addr);
                }
                Ok(())
            }
            Act::Forget(id) => {
                let addr = self.book.device(id).map(|d| d.addr.clone());
                let result = self.book.forget(id, now);
                // 删掉记录时一起删掉保存的密码
                if let (Ok(()), Some(addr), Ok(v)) = (&result, addr, self.vault()) {
                    if let Err(e) = v.remove(&addr) {
                        self.status = Status::error(e.to_string());
                    }
                }
                result
            }
            Act::Collapse(id, collapsed) => self.book.set_collapsed(id, collapsed),
            Act::DeleteGroup(id) => {
                let result = self.book.delete_group(id, now);
                if result.is_ok() {
                    self.toast = Some((s.group_deleted.to_owned(), Instant::now()));
                }
                result
            }
            Act::Edit(editing) => {
                let current = match &editing {
                    Editing::NewGroup { .. } => String::new(),
                    Editing::RenameGroup(id) => self
                        .book
                        .group(*id)
                        .map(|g| g.name.clone())
                        .unwrap_or_default(),
                    Editing::RenameDevice(id) => {
                        self.book.device(*id).map(Device::title).unwrap_or_default()
                    }
                };
                // 新建分组只在“分组”那一栏看得见
                if matches!(editing, Editing::NewGroup { .. }) {
                    self.tab = Tab::Groups;
                }
                self.editing = Some((editing, current, true));
                Ok(())
            }
            Act::Commit => match self.editing.take() {
                Some((editing, value, _)) => {
                    let result = match &editing {
                        Editing::NewGroup { parent } => {
                            self.book.add_group(*parent, &value, now).map(drop)
                        }
                        Editing::RenameGroup(id) => self.book.rename_group(*id, &value, now),
                        Editing::RenameDevice(id) => self.book.rename_device(*id, &value, now),
                    };
                    // 没存上：留在编辑状态，让用户改了再试
                    if result.is_err() {
                        self.editing = Some((editing, value, true));
                    }
                    result
                }
                None => Ok(()),
            },
            Act::Cancel => {
                self.editing = None;
                Ok(())
            }
        };
        self.book_error = result.err().map(|e| book_error(&e, s));
    }

    /// 就地编辑的输入框（新建分组、改名）：回车保存，Esc 或点到别处取消。
    fn edit_field(&mut self, ui: &mut egui::Ui, p: &Palette, hint: &str, acts: &mut Vec<Act>) {
        let Some((_, value, focus)) = &mut self.editing else {
            return;
        };
        let r = ui.add(
            TextEdit::singleline(value)
                .hint_text(RichText::new(hint).color(p.muted))
                .margin(pad(space::S10, space::S6))
                .desired_width(EDIT_WIDTH),
        );
        if std::mem::take(focus) {
            r.request_focus();
        }
        if r.lost_focus() {
            acts.push(if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                Act::Commit
            } else {
                Act::Cancel
            });
        }
    }

    /// 设备的“更多操作”菜单（“…”按钮和右键是同一份）。
    fn device_menu(&self, ui: &mut egui::Ui, device: &Device, acts: &mut Vec<Act>) {
        let s = self.s;
        let id = device.id;
        let favorite = if device.favorite {
            s.remove_favorite
        } else {
            s.add_favorite
        };
        if ui.button(favorite).clicked() {
            acts.push(Act::Favorite(id, !device.favorite));
        }
        if ui.button(s.rename).clicked() {
            acts.push(Act::Edit(Editing::RenameDevice(id)));
        }
        // 下级菜单的箭头用自己的图标：egui 默认的箭头字符我们的字体里没有
        let button = egui::Button::new(s.move_to_group).right_text(icon::CARET_RIGHT);
        egui::containers::menu::SubMenuButton::from_button(button).ui(ui, |ui| {
            let here = |on: bool, label: String| {
                if on {
                    format!("{}  {label}", icon::CHECK)
                } else {
                    label
                }
            };
            if ui
                .button(here(device.group.is_none(), s.ungrouped.to_owned()))
                .clicked()
            {
                acts.push(Act::Move(id, None));
            }
            for (group, depth, name) in self.book.group_list() {
                let label = format!("{}{name}", "    ".repeat(depth));
                if ui
                    .button(here(device.group == Some(group), label))
                    .clicked()
                {
                    acts.push(Act::Move(id, Some(group)));
                }
            }
            ui.separator();
            if ui.button(s.new_group).clicked() {
                acts.push(Act::Edit(Editing::NewGroup { parent: None }));
            }
        });
        if ui.button(s.copy_address).clicked() {
            acts.push(Act::Copy(id));
        }
        ui.separator();
        if ui.button(s.forget_device).clicked() {
            acts.push(Act::Forget(id));
        }
    }

    /// 设备表里的一行：名称（下面是系统）、设备码、上次连接；鼠标停在这一行或用键盘走到它时，右边出现操作。
    fn device_row(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        device: &Device,
        depth: usize,
        acts: &mut Vec<Act>,
    ) {
        let s = self.s;
        let id = device.id;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), ROW_HEIGHT),
            egui::Sense::click(),
        );
        let editing = matches!(&self.editing, Some((Editing::RenameDevice(e), ..)) if *e == id);
        // 两行交界的那一个像素只算下面那一行
        let active = ui.rect_contains_pointer(rect.with_max_y(rect.bottom() - 1.0))
            || response.has_focus()
            || response.context_menu_opened()
            || self.menu_open == Some(id);
        if active {
            ui.painter().rect_filled(rect, 0.0, p.row_hover);
        }
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(size::LINE, p.line_soft),
        );
        let cols = columns(rect);
        let indent = space::S16 + indent_of(depth);
        // 名称
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(
                    cols.name
                        .shrink2(egui::vec2(0.0, space::S8))
                        .with_min_x(cols.name.left() + indent),
                )
                .layout(Layout::left_to_right(Align::Center)),
        );
        cell.label(text(os_icon(device.os), font::ICON, Weight::Regular).color(p.muted));
        if editing {
            self.edit_field(&mut cell, p, s.device_name_hint, acts);
        } else {
            let title = device.title();
            let name = if device.favorite {
                format!("{title}  {}", icon::STAR)
            } else {
                title
            };
            let name = egui::Label::new(text(name, font::BODY, Weight::Medium)).truncate();
            let os = os_name(device.os);
            if os.is_empty() {
                // 只有一行字：直接放，和图标竖直居中
                cell.add(name);
            } else {
                cell.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    ui.add(name);
                    let _ = muted(ui, p, os);
                });
            }
        }
        // 设备码、上次连接
        let (code, _) = device.code();
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(cols.code)
                .layout(Layout::left_to_right(Align::Center)),
        );
        cell.add(egui::Label::new(code).truncate());
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(cols.last)
                .layout(Layout::left_to_right(Align::Center)),
        );
        cell.label(RichText::new(ago(unix_now(), device.last_used, s)).color(p.muted));
        // 操作
        if active && !editing {
            let mut cell = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(cols.actions)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            cell.spacing_mut().item_spacing.x = space::S6;
            let tip = s.more_actions.replace("{name}", &device.title());
            let more = icon_button(&mut cell, p, icon::DOTS_THREE, &tip, ControlSize::Medium);
            let open = egui::Popup::menu(&more)
                .show(|ui| self.device_menu(ui, device, acts))
                .is_some();
            self.menu_open = if open {
                Some(id)
            } else {
                self.menu_open.filter(|m| *m != id)
            };
            if secondary_button(&mut cell, p, s.row_files, ControlSize::Medium, true).clicked() {
                acts.push(Act::Files(id));
            }
            if primary_button(&mut cell, p, s.connect_this, ControlSize::Medium, true).clicked() {
                acts.push(Act::Connect(id));
            }
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, device.title())
        });
        if response.double_clicked() {
            acts.push(Act::Connect(id));
        }
        response.context_menu(|ui| self.device_menu(ui, device, acts));
    }

    /// 分组那一行：折叠箭头、名称、里面有几台；点一下折叠或展开，菜单里新建下级、改名、删除。
    fn group_row(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        group: (Id, usize, &str, usize, bool),
        acts: &mut Vec<Act>,
    ) {
        let s = self.s;
        let (id, depth, name, count, collapsed) = group;
        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), GROUP_ROW_HEIGHT),
            egui::Sense::click(),
        );
        let editing = matches!(&self.editing, Some((Editing::RenameGroup(e), ..)) if *e == id);
        // 两行交界的那一个像素只算下面那一行
        let active = ui.rect_contains_pointer(rect.with_max_y(rect.bottom() - 1.0))
            || response.has_focus()
            || response.context_menu_opened()
            || self.menu_open == Some(id);
        if active {
            ui.painter().rect_filled(rect, 0.0, p.row_hover);
        }
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(size::LINE, p.line_soft),
        );
        let menu = |ui: &mut egui::Ui, acts: &mut Vec<Act>| {
            if ui.button(s.new_subgroup).clicked() {
                acts.push(Act::Edit(Editing::NewGroup { parent: Some(id) }));
            }
            if ui.button(s.rename).clicked() {
                acts.push(Act::Edit(Editing::RenameGroup(id)));
            }
            ui.separator();
            if ui.button(s.delete_group).clicked() {
                acts.push(Act::DeleteGroup(id));
            }
        };
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(egui::vec2(space::S16, 0.0)))
                .layout(Layout::right_to_left(Align::Center)),
        );
        if active && !editing {
            let tip = s.more_actions.replace("{name}", name);
            let more = icon_button(&mut cell, p, icon::DOTS_THREE, &tip, ControlSize::Small);
            let open = egui::Popup::menu(&more).show(|ui| menu(ui, acts)).is_some();
            self.menu_open = if open {
                Some(id)
            } else {
                self.menu_open.filter(|m| *m != id)
            };
        }
        cell.with_layout(Layout::left_to_right(Align::Center), |ui| {
            ui.add_space(indent_of(depth));
            let caret = if collapsed {
                icon::CARET_RIGHT
            } else {
                icon::CARET_DOWN
            };
            ui.label(text(caret, font::BODY, Weight::Regular).color(p.muted));
            ui.label(text(icon::FOLDER, font::ICON, Weight::Regular).color(p.muted));
            if editing {
                self.edit_field(ui, p, s.group_name_hint, acts);
            } else {
                ui.add(egui::Label::new(text(name, font::BODY, Weight::Medium)).truncate());
                let _ = muted(ui, p, &s.device_count.replace("{n}", &count.to_string()));
            }
        });
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::CollapsingHeader, true, !collapsed, name)
        });
        if response.clicked() && !editing {
            acts.push(Act::Collapse(id, !collapsed));
        }
        response.context_menu(|ui| menu(ui, acts));
    }

    /// 新建分组时的那一行输入框（出现在上级分组下面；顶层的出现在最上面）。
    fn new_group_row(&mut self, ui: &mut egui::Ui, p: &Palette, depth: usize, acts: &mut Vec<Act>) {
        let hint = self.s.group_name_hint;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), GROUP_ROW_HEIGHT),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(rect, 0.0, p.row_hover);
        let mut cell = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect.shrink2(egui::vec2(space::S16, 0.0)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        cell.add_space(indent_of(depth) + space::S20);
        cell.label(text(icon::FOLDER_PLUS, font::ICON, Weight::Regular).color(p.muted));
        self.edit_field(&mut cell, p, hint, acts);
    }

    /// 表头。
    fn table_header(&self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width(), HEADER_HEIGHT),
            egui::Sense::hover(),
        );
        ui.painter().hline(
            rect.x_range(),
            rect.bottom(),
            egui::Stroke::new(size::LINE, p.line),
        );
        let cols = columns(rect);
        let name = cols.name.with_min_x(cols.name.left() + space::S16);
        for (rect, label, layout) in [
            (name, s.col_name, Layout::left_to_right(Align::Center)),
            (
                cols.code,
                s.device_code,
                Layout::left_to_right(Align::Center),
            ),
            (cols.last, s.col_last, Layout::left_to_right(Align::Center)),
            (
                cols.actions,
                s.col_actions,
                Layout::right_to_left(Align::Center),
            ),
        ] {
            let mut cell = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(layout));
            cell.label(text(label, font::SMALL, Weight::Medium).color(p.muted));
        }
    }

    /// “设备”：最近、收藏、分组（树）、同一网络。
    fn devices_section(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        let mut acts = Vec::new();
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let options = Tab::ALL.map(|tab| (tab, tab.label(s)));
                if segmented(ui, p, &mut self.tab, &options) {
                    self.editing = None;
                    self.book_error = None;
                }
                if self.tab == Tab::Groups {
                    let label = format!("{}  {}", icon::PLUS, s.new_group);
                    if secondary_button(ui, p, &label, ControlSize::Small, true).clicked() {
                        acts.push(Act::Edit(Editing::NewGroup { parent: None }));
                    }
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.label(text(s.nav_devices, font::SUBTITLE, Weight::Semibold));
                });
            });
        });
        ui.add_space(space::S4);
        if let Some(e) = self.book_error.clone() {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WARNING).color(p.problem.solid));
                ui.label(RichText::new(e).color(p.problem.text));
            });
        }
        let rows: Vec<Row> = match self.tab {
            Tab::Recent => device_rows(&self.book.recent(RECENT_SHOWN)),
            Tab::Favorites => device_rows(&self.book.favorites()),
            Tab::Groups => self.book.tree(),
            Tab::Network => Vec::new(),
        };
        let new_group = match &self.editing {
            Some((Editing::NewGroup { parent }, ..)) if self.tab == Tab::Groups => Some(*parent),
            Some(_) | None => None,
        };
        egui::Frame::new()
            .fill(p.surface)
            .stroke(egui::Stroke::new(size::LINE, p.line))
            .corner_radius(radius::PANEL)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                if rows.is_empty() && new_group.is_none() {
                    let glyph = match self.tab {
                        Tab::Recent => icon::CLOCK_COUNTER_CLOCKWISE,
                        Tab::Favorites => icon::STAR,
                        Tab::Groups => icon::FOLDER,
                        Tab::Network => icon::WIFI_HIGH,
                    };
                    empty_state(ui, p, glyph, self.tab.empty(s));
                    return;
                }
                self.table_header(ui, p);
                if new_group == Some(None) {
                    self.new_group_row(ui, p, 0, &mut acts);
                }
                for row in &rows {
                    match row {
                        Row::Group {
                            id,
                            depth,
                            name,
                            count,
                            collapsed,
                        } => {
                            self.group_row(
                                ui,
                                p,
                                (*id, *depth, name, *count, *collapsed),
                                &mut acts,
                            );
                            if new_group == Some(Some(*id)) {
                                self.new_group_row(ui, p, depth + 1, &mut acts);
                            }
                        }
                        Row::Ungrouped { count } => {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), GROUP_ROW_HEIGHT),
                                egui::Sense::hover(),
                            );
                            ui.painter().text(
                                rect.left_center() + egui::vec2(space::S16, 0.0),
                                egui::Align2::LEFT_CENTER,
                                format!(
                                    "{}  {}",
                                    s.ungrouped,
                                    s.device_count.replace("{n}", &count.to_string())
                                ),
                                theme::font_id(font::SMALL, Weight::Medium),
                                p.muted,
                            );
                            ui.painter().hline(
                                rect.x_range(),
                                rect.bottom(),
                                egui::Stroke::new(size::LINE, p.line_soft),
                            );
                        }
                        Row::Device { id, depth } => {
                            if let Some(device) = self.book.device(*id).cloned() {
                                self.device_row(ui, p, &device, *depth, &mut acts);
                            }
                        }
                    }
                }
            });
        for act in acts {
            self.apply(ui, act);
        }
    }

    /// 主页面：这台电脑、连接另一台电脑、设备。
    fn home_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        ui.set_max_width(ui.available_width().min(PAGE_MAX_WIDTH));
        self.permission_guide(ui, p);
        self.this_pc_section(ui, p);
        ui.add_space(space::S36 - space::S8);
        self.connect_section(ui, p);
        ui.add_space(space::S36 - space::S8);
        self.devices_section(ui, p);
    }

    /// “文件传输”页。文件管理要走一个远程会话的连接：没有会话时说明怎么开始，有就打开文件管理。
    fn files_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.set_max_width(ui.available_width().min(PAGE_MAX_WIDTH));
        let _ = page_title(ui, s.file_transfer);
        ui.add_space(space::S12);
        panel(ui, p, |ui| {
            if self.views.is_empty() {
                empty_state(ui, p, icon::FOLDER_OPEN, s.files_need_session);
            } else if primary_button(ui, p, s.open_files, ControlSize::Large, true).clicked() {
                self.files.open = true;
            }
        });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.label(text(s.nav_settings, font::TITLE, Weight::Semibold));
        ui.add_space(space::S14);

        panel(ui, p, |ui| {
            section_title(ui, s.section_server, |_| {});
            let _ = muted(ui, p, s.via_server);
            ui.add_space(space::S6);
            let mut save = false;
            for (label, value, secret) in [
                (s.server_addr, &mut self.server.addr, false),
                (s.server_pin, &mut self.server.pin, false),
                (s.account_token, &mut self.server.account, true),
            ] {
                ui.add_space(space::S4);
                let _ = muted(ui, p, label);
                let r = big_input(ui, p, value, "", secret);
                save |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }
            ui.add_space(space::S8);
            save |= primary_button(ui, p, s.save, ControlSize::Medium, true).clicked();
            if save {
                self.status = match self.server.server(s) {
                    Err(e) => Status::error(e),
                    Ok(_) => match self.server.save(&self.data_dir) {
                        // 本机正在被控：下次开启时用新服务器登记。
                        Ok(()) => {
                            self.toast = Some((s.saved.to_owned(), Instant::now()));
                            Status::info(s.saved)
                        }
                        Err(e) => Status::error(e.to_string()),
                    },
                };
            }
        });
        ui.add_space(space::S12);

        panel(ui, p, |ui| {
            section_title(ui, s.section_network, |ui| {
                if icon_button(ui, p, icon::ARROWS_CLOCKWISE, s.refresh, ControlSize::Small)
                    .clicked()
                {
                    self.addrs = lan_addresses();
                }
            });
            // 和其他输入框同一种样式：小字标签在上，输入框在下
            let _ = muted(ui, p, s.port);
            let editable = self.phase == HostPhase::Off;
            let field = ui.add_enabled(
                editable,
                TextEdit::singleline(&mut self.port)
                    .margin(pad(space::S10, space::S8))
                    .desired_width(120.0),
            );
            if !editable {
                let _ = field.on_disabled_hover_text(s.port_locked);
            }
            ui.add_space(space::S6);
            let port = self.phase.port();
            let shown_port = port.map_or_else(
                || parse_port(&self.port, s).unwrap_or(DEFAULT_PORT),
                |p| p.0,
            );
            let ipv6 = port.is_none_or(|p| p.1);
            let _ = muted(ui, p, s.lan_addresses);
            let addrs: Vec<String> = self
                .addrs
                .iter()
                .filter(|ip| ip.is_ipv4() || ipv6)
                .map(|ip| SocketAddr::new(*ip, shown_port).to_string())
                .collect();
            if addrs.is_empty() {
                let _ = muted(ui, p, s.no_addresses);
            }
            for addr in addrs {
                ui.horizontal(|ui| {
                    ui.monospace(&addr);
                    if icon_button(ui, p, icon::COPY, s.copy, ControlSize::Small).clicked() {
                        self.copy(ui, &addr);
                    }
                });
            }
            if let Some(report) = self.net.clone() {
                ui.add_space(space::S6);
                egui::CollapsingHeader::new(s.network_check)
                    .id_salt("network-check")
                    .show(ui, |ui| {
                        egui::Grid::new("network-check-grid")
                            .num_columns(2)
                            .spacing([16.0, 6.0])
                            .show(ui, |ui| {
                                for (item, value) in report.lines() {
                                    let _ = muted(ui, p, item);
                                    ui.label(value);
                                    ui.end_row();
                                }
                            });
                    });
            }
        });
        ui.add_space(space::S12);

        panel(ui, p, |ui| {
            section_title(ui, s.section_general, |_| {});
            if self.font_fallback_missing {
                notice(ui, p, Signal::Relay, |ui, colors| {
                    ui.add(
                        egui::Label::new(RichText::new(s.font_fallback_missing).color(colors.text))
                            .wrap(),
                    );
                });
                ui.add_space(space::S8);
            }
            ui.horizontal(|ui| {
                ui.label(format!("{}:", s.language));
                self.language_switch(ui);
            });
            ui.horizontal(|ui| {
                ui.label(format!("{}:", s.appearance));
                let mut chosen = self.appearance;
                ui.selectable_value(&mut chosen, Appearance::System, s.appearance_system);
                ui.selectable_value(&mut chosen, Appearance::Light, s.appearance_light);
                ui.selectable_value(&mut chosen, Appearance::Dark, s.appearance_dark);
                if chosen != self.appearance {
                    self.appearance = chosen;
                    chosen.apply(ui.ctx());
                    if let Err(e) = chosen.save(&self.data_dir) {
                        self.status = Status::error(e.to_string());
                    }
                }
            });
            if ui
                .button(format!("{}  {}", icon::FOLDER_OPEN, s.open_logs))
                .clicked()
            {
                self.status = match open_folder(&self.data_dir.join("logs")) {
                    Ok(()) => Status::info(self.data_dir.join("logs").display().to_string()),
                    Err(e) => Status::error(e),
                };
            }
        });
        ui.add_space(space::S12);

        panel(ui, p, |ui| {
            section_title(ui, s.section_about, |_| {});
            ui.label(format!(
                "PeerDesk {} {}",
                s.version,
                env!("CARGO_PKG_VERSION")
            ));
            match self.fingerprint.clone() {
                Ok(fp) => {
                    ui.horizontal(|ui| {
                        let _ = muted(ui, p, s.fingerprint);
                        ui.monospace(short_fingerprint(&fp));
                        if icon_button(ui, p, icon::COPY, s.copy, ControlSize::Small).clicked() {
                            self.copy(ui, &fp);
                        }
                    });
                }
                Err(e) => {
                    ui.colored_label(p.problem.text, e);
                }
            }
            if let HostPhase::Listening { port, .. } = self.phase {
                if ui.button(s.connect_self).clicked() {
                    self.form_password = self.password.clone();
                    let target = Target::Addr {
                        addr: SocketAddr::from(([127, 0, 0, 1], port)),
                        pin: self
                            .fingerprint
                            .as_ref()
                            .ok()
                            .and_then(|fp| parse_pin(fp, s).ok().flatten()),
                    };
                    self.connect(target, None);
                }
            }
        });
    }

    /// 每秒重新检查一次权限：用户在系统设置里打开开关后，这里马上变成打勾。
    fn refresh_permissions(&mut self, ctx: &egui::Context) {
        if self.guide_checked.elapsed() >= GUIDE_REFRESH {
            if !self.preview_mac {
                self.perms = peerdesk_platform::check_permissions();
            }
            self.guide = guide(self.os, self.perms);
            self.guide_checked = Instant::now();
        }
        ctx.request_repaint_after(GUIDE_REFRESH);
    }

    fn perform(&mut self, action: Action) {
        if let Err(e) = peerdesk_platform::perform(action) {
            self.status = Status::error(e);
        }
        // 授权对话框关掉后马上重新检查
        self.guide_checked = Instant::now() - GUIDE_REFRESH;
    }

    /// macOS 授权引导：两项权限的状态，当前这一项的示意图和分步操作。
    fn permissions_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        self.refresh_permissions(ui.ctx());
        let major = match self.os {
            Os::MacOs { major, .. } => major,
            Os::Windows | Os::LinuxX11 | Os::LinuxWayland | Os::Other => 15,
        };
        let english = std::ptr::eq(s, strings(Lang::En));
        let names = mac_settings_names(major, english);
        ui.label(text(s.perm_title, font::TITLE, Weight::Semibold));
        ui.add_space(space::S4);
        let _ = muted(ui, p, s.perm_subtitle);
        ui.add_space(space::S14);

        // 两项权限的状态
        let items = [
            (
                names.screen,
                s.perm_screen_what,
                self.perms.screen_capture,
                icon::MONITOR,
            ),
            (
                names.input,
                s.perm_input_what,
                self.perms.input_injection,
                icon::CURSOR_CLICK,
            ),
        ];
        // 每张卡片里直接放授权按钮：用户不用往下翻就能操作
        let screen_first = !self.perms.screen_capture;
        let mut clicked = None;
        ui.columns(2, |cols| {
            for (i, (col, (name, what, ok, glyph))) in cols.iter_mut().zip(items).enumerate() {
                let screen = i == 0;
                panel(col, p, |ui| {
                    ui.set_min_height(120.0);
                    // 标题一行，状态单独一行：英文标题很长，放同一行会和状态标签重叠
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(glyph).size(font::TITLE).color(p.ink));
                        ui.add(egui::Label::new(text(name, font::BODY, Weight::Semibold)).wrap());
                    });
                    ui.add_space(space::S6);
                    ui.horizontal(|ui| {
                        if ok {
                            let _ = pill(ui, p, s.perm_granted, Signal::Direct);
                        } else {
                            let _ = pill(ui, p, s.perm_missing, Signal::Relay);
                        }
                    });
                    ui.add_space(space::S6);
                    let _ = muted(ui, p, what);
                    if ok {
                        return;
                    }
                    ui.add_space(space::S10);
                    ui.horizontal(|ui| {
                        // 先做的那一项用主按钮，另一项用普通按钮，引导用户按顺序来
                        let request = if screen {
                            Action::RequestScreen
                        } else {
                            Action::RequestInput
                        };
                        let settings = if screen {
                            Action::OpenScreenSettings
                        } else {
                            Action::OpenInputSettings
                        };
                        let primary = screen == screen_first;
                        let r = if primary {
                            primary_button(ui, p, s.perm_request, ControlSize::Medium, true)
                        } else {
                            ui.add(
                                egui::Button::new(s.perm_request).min_size(egui::vec2(120.0, 40.0)),
                            )
                        };
                        if r.clicked() {
                            clicked = Some(request);
                        }
                        if ui
                            .add(
                                egui::Button::new(s.perm_open_settings)
                                    .min_size(egui::vec2(0.0, 40.0)),
                            )
                            .clicked()
                        {
                            clicked = Some(settings);
                        }
                    });
                });
            }
        });
        if let Some(action) = clicked {
            self.perform(action);
        }
        ui.add_space(space::S14);

        if self.perms.screen_capture && self.perms.input_injection {
            panel(ui, p, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icon::CHECK_CIRCLE)
                            .size(font::ICON_HERO)
                            .color(p.direct.text),
                    );
                    ui.vertical(|ui| {
                        ui.label(text(s.perm_done_title, font::ICON, Weight::Semibold));
                        let _ = muted(ui, p, s.perm_done);
                    });
                });
            });
            return;
        }
        // 先做屏幕录制，再做辅助功能
        let screen = !self.perms.screen_capture;
        let item = if screen { names.screen } else { names.input };
        panel(ui, p, |ui| {
            let step_no = if screen { 1 } else { 2 };
            ui.label(
                RichText::new(format!(
                    "{}  ·  {item}",
                    s.perm_step.replace("{n}", &step_no.to_string())
                ))
                .size(font::SUBTITLE)
                .strong(),
            );
            ui.add_space(space::S10);
            ui.vertical_centered(|ui| {
                guide_art::mac_settings(ui, p, &names, item, false, s.perm_callout, s.perm_unlock);
            });
            ui.add_space(space::S12);
            let mut texts = vec![s.perm_open_hint];
            if names.unlock {
                texts.push(s.perm_unlock_hint);
            }
            texts.push(s.perm_toggle_hint);
            texts.push(s.perm_add_hint);
            if screen {
                texts.push(s.perm_relaunch_hint);
            }
            for (i, text) in texts.into_iter().enumerate() {
                ui.horizontal(|ui| {
                    let n = (i + 1).to_string();
                    egui::Frame::new()
                        .fill(p.ink)
                        .corner_radius(radius::PILL)
                        .inner_margin(pad(space::S8, space::S2))
                        .show(ui, |ui| {
                            ui.label(theme::text(&n, font::BODY, Weight::Semibold).color(p.on_ink));
                        });
                    ui.add(egui::Label::new(text).wrap());
                });
                ui.add_space(space::S4);
            }
            if screen {
                // 屏幕录制授权后要重新打开程序才生效
                ui.add_space(space::S6);
                if ui
                    .add(egui::Button::new(s.perm_relaunch).min_size(egui::vec2(0.0, 40.0)))
                    .clicked()
                {
                    self.perform(Action::Relaunch);
                }
            }
        });
    }

    /// 左侧导航栏：品牌、页面、服务器和本机网络的状态。
    fn sidebar(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.add_space(space::S24);
        ui.horizontal(|ui| {
            ui.add_space(space::S8);
            ui.spacing_mut().item_spacing.x = space::S10;
            let _ = brand_mark(ui, p, font::ICON_LARGE);
            ui.label(text("PeerDesk", font::SUBTITLE, Weight::Semibold));
        });
        ui.add_space(space::S20);
        ui.spacing_mut().item_spacing.y = space::S4;
        for (page, glyph, label) in [
            (Page::Home, icon::MONITOR, s.nav_devices),
            (Page::Files, icon::FOLDER, s.file_transfer),
            (Page::Settings, icon::SLIDERS_HORIZONTAL, s.nav_settings),
        ] {
            if nav_item(ui, p, glyph, label, self.page == page).clicked() {
                self.page = page;
            }
        }
        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.spacing_mut().item_spacing.y = space::S2;
            ui.add_space(space::S14);
            let indent = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
                ui.horizontal(|ui| {
                    ui.add_space(space::S10);
                    add(ui);
                });
            };
            // 从下往上排：版本号、网络、服务器
            indent(ui, &mut |ui| {
                let version = format!(
                    "v{} · {}",
                    env!("CARGO_PKG_VERSION"),
                    env!("PEERDESK_COMMIT")
                );
                ui.label(text(version, font::CAPTION, Weight::Regular).color(p.muted));
            });
            if let Some(hint) = nat_hint(self.net.as_ref(), s) {
                indent(ui, &mut |ui| {
                    // 英文这句比侧边栏宽：换行，不把侧边栏撑宽
                    ui.add(
                        egui::Label::new(text(hint, font::CAPTION, Weight::Regular).color(p.muted))
                            .wrap(),
                    );
                });
            }
            let registered = self.device_code.is_some()
                || self
                    .service
                    .as_ref()
                    .is_some_and(|st| st.running && st.code.is_some());
            let (label, signal) = server_badge(self.server.is_set(), registered, s);
            indent(ui, &mut |ui| {
                ui.spacing_mut().item_spacing.x = space::S8;
                let _ = dot(ui, p, signal);
                ui.add(egui::Label::new(text(label, font::SMALL, Weight::Regular)).wrap());
            });
            ui.add_space(space::S10);
            ui.separator();
        });
    }

    /// 内容区底部的状态（最近一次操作的结果）。
    fn status_line(&mut self, ui: &mut egui::Ui, p: &Palette) {
        // 新的状态从现在开始计时；普通信息几秒后淡出，错误一直显示
        if self.status_text != self.status.text {
            self.status_text.clone_from(&self.status.text);
            self.status_since = Instant::now();
        }
        let faded = !self.status.error && self.status_since.elapsed() > STATUS_FADE;
        if self.status.text.is_empty() || faded {
            return;
        }
        ui.ctx().request_repaint_after(STATUS_FADE);
        let (glyph, color) = if self.status.error {
            (icon::WARNING, p.problem.text)
        } else {
            (icon::INFO, p.muted)
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).color(color));
            ui.label(
                RichText::new(&self.status.text)
                    .size(font::SMALL)
                    .color(color),
            );
        });
    }
}

/// 用系统的文件管理器打开文件夹（不存在时先建好）。
pub(super) fn open_folder(dir: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}：{e}", dir.display()))?;
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(dir)
        .spawn()
        .map(drop)
        .map_err(|e| format!("{}：{e}", dir.display()))
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.selftest && self.host.is_none() && self.phase == HostPhase::Off {
            self.start_hosting();
        }
        if !self.selftest {
            self.check_service();
        }
        // 默认本机可以被控制：权限齐了就自动开启（只自动开一次，之后听用户的）
        if self.auto_host && !self.preview_mac && self.guide.ready && self.phase == HostPhase::Off {
            self.auto_host = false;
            self.start_hosting();
        }
        self.poll_host();

        let p = Palette::of(ctx);
        egui::SidePanel::left("nav")
            .resizable(false)
            .show_separator_line(false)
            .exact_width(size::SIDEBAR - 2.0 * space::S12)
            .frame(
                egui::Frame::new()
                    .fill(p.surface)
                    .inner_margin(pad(space::S12, 0.0)),
            )
            .show(ctx, |ui| {
                // 右侧一条细分隔线（画在面板里面，不留缝）
                let r = ui.max_rect();
                ui.painter().vline(
                    r.right() + 11.5,
                    r.y_range(),
                    egui::Stroke::new(size::LINE, p.line),
                );
                self.sidebar(ui, &p);
            });
        egui::TopBottomPanel::bottom("status")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(pad(space::S44, space::S8)),
            )
            .show(ctx, |ui| {
                ui.set_min_height(20.0);
                self.status_line(ui, &p);
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(pad(space::S44, space::S36)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                    Page::Home => self.home_page(ui, &p),
                    Page::Files => self.files_page(ui, &p),
                    Page::Settings => self.settings_page(ui, &p),
                    Page::Permissions => self.permissions_page(ui, &p),
                });
            });

        self.show_toast(ctx, &p);
        let s = self.s;
        let mut remembered = Vec::new();
        let mut open_files = None;
        let mut files_after = self.files_after.take();
        self.views.retain_mut(|(viewer, remember)| {
            let outcome = viewer.show(ctx, s);
            if outcome.open_files {
                open_files = Some(viewer.title().to_owned());
            }
            if outcome.connected {
                // 从设备列表点的“文件传输”：连上了就打开这台设备的文件管理
                if remember
                    .as_ref()
                    .is_some_and(|r| files_after.as_deref() == Some(r.addr.as_str()))
                {
                    open_files = Some(viewer.title().to_owned());
                    files_after = None;
                }
            }
            // 认证通过、握手完成（对方的系统和电脑名到了）才记进最近连接：以前链路一通就记，
            // 那时名字还没到，记录里一直没有电脑名；密码输错的连接也被记了进去
            if outcome.established {
                if let Some(r) = remember.take() {
                    remembered.push((r, outcome.peer.0, outcome.peer.1));
                }
            }
            outcome.open
        });
        self.files_after = files_after;
        // 文件管理：在当前的远程会话上浏览、传文件
        if let Some(title) = open_files {
            self.files.open_for(&title);
        }
        let sessions: Vec<super::files_window::Session> = self
            .views
            .iter()
            .map(|(viewer, _)| super::files_window::Session {
                title: viewer.title().to_owned(),
                link: viewer.files_link(),
            })
            .collect();
        self.files.show(ctx, &sessions);
        for (r, os, name) in remembered {
            if let Err(e) = self.book.remember(&r.addr, &r.pin, os, &name, unix_now()) {
                self.status = Status::error(book_error(&e, s));
            }
            // 记住密码：加密保存；没勾时删掉以前保存的
            let saved = self.vault().and_then(|v| match &r.password {
                Some(pw) => v.put(&r.addr, pw),
                None => v.remove(&r.addr),
            });
            if let Err(e) = saved {
                self.status = Status::error(e.to_string());
            }
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for (viewer, _) in &mut self.views {
            viewer.stop();
        }
        self.stop_hosting();
    }
}

/// 系统的图标：设备列表里一眼分出 Windows、Mac、Linux。
const fn os_icon(os: PeerOs) -> &'static str {
    match os {
        PeerOs::Windows => icon::WINDOWS_LOGO,
        PeerOs::Mac => icon::APPLE_LOGO,
        PeerOs::Linux => icon::LINUX_LOGO,
        PeerOs::Unknown => icon::DESKTOP_TOWER,
    }
}

/// 设备表各列的位置。名称用剩下的宽度，其余几列固定。
struct Columns {
    name: egui::Rect,
    code: egui::Rect,
    last: egui::Rect,
    actions: egui::Rect,
}

/// 把一行切成几列（表头和每一行用同一个切法，才对得齐）。
fn columns(row: egui::Rect) -> Columns {
    let right = row.right() - space::S16;
    let actions_left = right - COL_ACTIONS;
    let last_left = actions_left - COL_LAST;
    let code_left = last_left - COL_CODE;
    let slice = |from: f32, to: f32| {
        egui::Rect::from_min_max(egui::pos2(from, row.top()), egui::pos2(to, row.bottom()))
    };
    Columns {
        name: slice(row.left(), code_left - space::S16),
        code: slice(code_left, last_left - space::S16),
        last: slice(last_left, actions_left),
        actions: slice(actions_left, right),
    }
}

/// 树里第 `depth` 层往右缩进多少。
fn indent_of(depth: usize) -> f32 {
    space::S24 * f32::from(u8::try_from(depth).unwrap_or(u8::MAX))
}

/// 一组设备排成平的几行（最近、收藏）。
fn device_rows(devices: &[&Device]) -> Vec<Row> {
    devices
        .iter()
        .map(|d| Row::Device { id: d.id, depth: 0 })
        .collect()
}
