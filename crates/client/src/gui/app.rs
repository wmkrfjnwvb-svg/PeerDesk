//! 主窗口（eframe 接线）：左侧导航（远程控制、设备、设置），右侧页面；首页是“本设备”和
//! “远程控制设备”两张卡片加最近连接。
//! 只画控件、把操作交给会话线程；逻辑在同目录的其他模块里。

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align, Color32, Layout, RichText, TextEdit};
use peerdesk_core::proto::PeerOs;
use peerdesk_core::signal::format_code;
use peerdesk_net::identity::to_hex;
use peerdesk_net::netcheck::NetReport;
use peerdesk_net::rendezvous::Lookup;
use peerdesk_net::Identity;
use peerdesk_platform::guide::{guide, mac_settings_names, Action, Guide, Os};

use super::form::{parse_pin, parse_port, ConnectForm, Target, DEFAULT_PORT};
use super::i18n::{effective_lang, save_lang, strings, Lang, Strings};
use super::netinfo::lan_addresses;
use super::recent::RecentStore;
use super::session::{start_host, start_view, HostConfig, HostEvent, HostHandle, Repaint};
use super::settings::ServerSettings;
use super::state::{HostPhase, Status};
use super::stats::short_fingerprint;
use super::theme::{
    big_input, card, card_title, icon, icon_button, muted, nav_item, pill, primary_button, toggle,
    Appearance, Palette,
};
use super::viewer::Viewer;
use super::{fonts, guide_art, password, theme};
use crate::remote::default_data_dir;
use crate::{EXIT_FAILED, EXIT_OK};

/// 自测模式：启动后自动开启被控、再连接本机。值为 `host` 时只开启被控。CI 用它截图。
const SELFTEST_ENV: &str = "PEERDESK_GUI_SELFTEST";
/// 窗口图标的边长（像素）。
const ICON_SIZE: u32 = 128;
/// 首页两张卡片的最小高度（两张一样高）。
const CARD_HEIGHT: f32 = 250.0;
/// 复制等操作的轻提示显示多久。
/// 首页卡片并排时每张的最小宽度。
const CARD_MIN_WIDTH: f32 = 340.0;
const TOAST: Duration = Duration::from_millis(1500);
/// 普通的状态信息显示多久（错误一直显示，直到下一条）。
const STATUS_FADE: Duration = Duration::from_secs(6);
/// 启动时打开哪一页（home、devices、settings、permissions），截图用。
const PAGE_ENV: &str = "PEERDESK_GUI_PAGE";
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
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(s.app_title)
            .with_app_id("peerdesk")
            .with_icon(egui::IconData {
                rgba: theme::app_icon(ICON_SIZE),
                width: ICON_SIZE,
                height: ICON_SIZE,
            })
            .with_inner_size([1000.0, 680.0])
            .with_min_inner_size([820.0, 560.0]),
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
    recent: RecentStore,
    views: Vec<(Viewer, Option<(String, String)>)>,
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
}

/// 用户手动关闭被控后留下的标记文件。
const HOST_OFF_FILE: &str = "host-off";

/// 主窗口的页面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Home,
    Devices,
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
        let (defs, _font) = fonts::definitions();
        ctx.set_fonts(defs);
        theme::install(ctx);
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
            recent: RecentStore::load(&data_dir),
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
                Ok("devices") => Page::Devices,
                Ok("settings") => Page::Settings,
                Ok("permissions") => Page::Permissions,
                Ok(_) | Err(_) => Page::Home,
            },
            show_password: false,
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

    fn connect(&mut self, target: Target, remember: Option<(String, String)>) {
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
        self.views
            .push((Viewer::new(self.serial, title, handle), remember));
        self.status = Status::info(self.s.connecting);
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
                let remember = Some((self.form.remembered(), pin));
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
            egui::Frame::new()
                .fill(p.warning.gamma_multiply(0.12))
                .stroke(egui::Stroke::new(1.0_f32, p.warning.gamma_multiply(0.5)))
                .corner_radius(10)
                .inner_margin(14)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icon::SHIELD_WARNING)
                                .size(20.0)
                                .color(p.warning),
                        );
                        ui.label(RichText::new(self.s.perm_banner).strong());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if primary_button(ui, p, self.s.perm_start, 110.0, true).clicked() {
                                self.page = Page::Permissions;
                            }
                        });
                    });
                });
            ui.add_space(12.0);
            return;
        }
        egui::Frame::new()
            .fill(p.warning.gamma_multiply(0.12))
            .stroke(egui::Stroke::new(1.0_f32, p.warning.gamma_multiply(0.5)))
            .corner_radius(10)
            .inner_margin(14)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icon::SHIELD_WARNING)
                            .size(18.0)
                            .color(p.warning),
                    );
                    ui.label(RichText::new(&self.guide.title).strong().color(p.warning));
                });
                for step in self.guide.steps.clone() {
                    ui.horizontal_wrapped(|ui| {
                        let (mark, color) = if step.done {
                            (icon::CHECK_CIRCLE, p.success)
                        } else {
                            (icon::WARNING, p.warning)
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
        ui.add_space(12.0);
    }

    fn copy(&mut self, ui: &egui::Ui, text: &str, shown: &str) {
        ui.ctx().copy_text(text.to_owned());
        let _ = shown;
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
                    .fill(if p.dark {
                        Color32::from_rgb(0x2A, 0x31, 0x3D)
                    } else {
                        Color32::from_rgb(0x1B, 0x23, 0x30)
                    })
                    .corner_radius(18)
                    .inner_margin(egui::Margin::symmetric(16, 8))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(format!("{}  {text}", icon::CHECK_CIRCLE))
                                .color(Color32::WHITE),
                        );
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn host_state(&self, p: &Palette) -> (&'static str, Color32) {
        let s = self.s;
        match &self.phase {
            HostPhase::Off => (s.host_off, p.muted),
            HostPhase::Starting => (s.host_starting, p.warning),
            HostPhase::Listening { .. } => (s.host_listening, p.success),
            HostPhase::Busy {
                established: false, ..
            } => (s.host_handshaking, p.warning),
            HostPhase::Busy {
                established: true, ..
            } => (s.host_controlled, p.accent),
            HostPhase::Stopping => (s.host_stopping, p.muted),
        }
    }

    /// 无人值守服务在运行时的“本设备”卡片：服务的设备码，本程序不自己当被控端。
    fn service_card(&mut self, ui: &mut egui::Ui, p: &Palette, code: Option<u32>) {
        let s = self.s;
        let title = if self.name.is_empty() {
            s.this_device.to_owned()
        } else {
            format!("{} · {}", s.this_device, self.name)
        };
        card_title(ui, p, os_icon(PeerOs::current()), &title, |ui| {
            let _ = pill(ui, s.unattended, p.success);
        });
        ui.add_space(4.0);
        let _ = muted(ui, p, s.unattended_hint);
        ui.add_space(8.0);
        let _ = muted(ui, p, s.device_code);
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            if let Some(code) = code {
                let shown = format_code(code);
                ui.label(RichText::new(&shown).monospace().size(30.0).strong());
                if icon_button(ui, icon::COPY, s.copy).clicked() {
                    self.copy(ui, &code.to_string(), &shown);
                }
            } else {
                ui.add(egui::Spinner::new().size(16.0).color(p.muted));
                let _ = muted(ui, p, s.code_pending);
            }
        });
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

    /// “本设备”卡片：开关、设备码、密码、谁在控制。
    fn this_device_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        if let Some(code) = self
            .service
            .as_ref()
            .filter(|st| st.running)
            .map(|st| st.code)
        {
            return self.service_card(ui, p, code);
        }
        let mut on = self.phase.is_on();
        // 权限不全时不能开启被控（关闭总是可以）。
        let enabled = !matches!(self.phase, HostPhase::Stopping | HostPhase::Starting)
            && (self.guide.ready || self.phase.is_on());
        let (state, color) = self.host_state(p);
        let mut toggled = false;
        let title = if self.name.is_empty() {
            s.this_device.to_owned()
        } else {
            format!("{} · {}", s.this_device, self.name)
        };
        card_title(ui, p, os_icon(PeerOs::current()), &title, |ui| {
            toggled = toggle(ui, p, &mut on, enabled)
                .on_hover_text(s.allow_control)
                .changed();
            let _ = pill(ui, state, color);
        });
        if toggled {
            // 记住用户的选择：手动关了，下次打开不再自动开启
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
        ui.add_space(4.0);
        if !self.phase.is_on() {
            let _ = muted(ui, p, s.host_off_hint);
            ui.add_space(8.0);
        }

        // 设备码（登记到服务器后才有）
        let _ = muted(ui, p, s.device_code);
        ui.horizontal(|ui| {
            ui.set_min_height(40.0);
            match self.device_code {
                Some(code) => {
                    let shown = format_code(code);
                    ui.label(RichText::new(&shown).monospace().size(30.0).strong());
                    if icon_button(ui, icon::COPY, s.copy).clicked() {
                        self.copy(ui, &code.to_string(), &shown);
                    }
                }
                // 没设服务器就没有设备码：引导去设置（局域网里仍可以用地址直连）
                None if !self.server.is_set() => {
                    let link = egui::Button::new(
                        RichText::new(format!("{}  {}", s.server_unset, icon::ARROW_RIGHT))
                            .size(14.0)
                            .color(p.accent),
                    )
                    .frame(false);
                    if ui.add(link).clicked() {
                        self.page = Page::Settings;
                    }
                }
                None if self.phase.is_on() => {
                    ui.add(egui::Spinner::new().size(16.0).color(p.muted));
                    let _ = muted(ui, p, s.code_pending);
                }
                None => {
                    ui.label(RichText::new(s.host_off).size(16.0).color(p.muted));
                }
            }
        });
        ui.add_space(10.0);

        // 密码
        let _ = muted(ui, p, s.password);
        let password = self.password.clone();
        ui.horizontal(|ui| {
            let shown = if self.show_password {
                password.clone()
            } else {
                "•".repeat(password.chars().count())
            };
            ui.label(RichText::new(shown).monospace().size(22.0).strong());
            let (eye, tip) = if self.show_password {
                (icon::EYE_SLASH, s.hide_password)
            } else {
                (icon::EYE, s.show_password)
            };
            if icon_button(ui, eye, tip).clicked() {
                self.show_password = !self.show_password;
            }
            if icon_button(ui, icon::COPY, s.copy).clicked() {
                self.copy(ui, &password, s.password);
            }
            if icon_button(ui, icon::ARROWS_CLOCKWISE, s.new_password).clicked() {
                // 用户主动换：生成并保存（重启后还是这个）
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
        });
        ui.add_space(8.0);
        let _ = muted(ui, p, s.this_device_hint);

        // 正在被控制：谁、断开
        if let HostPhase::Busy { addr, peer, .. } = &self.phase {
            let (addr, peer) = (*addr, peer.clone());
            ui.add_space(10.0);
            egui::Frame::new()
                .fill(p.accent_soft)
                .corner_radius(8)
                .inner_margin(10)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(icon::HAND_EYE).color(p.accent));
                        // 先放右侧的断开按钮，地址和指纹用剩下的宽度（放不下截断）
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let stop = egui::Button::new(
                                RichText::new(format!(
                                    "{}  {}",
                                    icon::PHONE_DISCONNECT,
                                    s.disconnect
                                ))
                                .color(Color32::WHITE),
                            )
                            .fill(p.danger);
                            if ui.add(stop).clicked() {
                                if let Some(host) = &self.host {
                                    host.kick();
                                }
                            }
                            let who =
                                format!("{}  {addr}  {}", s.controller, short_fingerprint(&peer));
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(&who).truncate())
                                    .on_hover_text(&who);
                            });
                        });
                    });
                });
        }
    }

    /// “远程控制设备”卡片：设备码、密码、连接。
    fn remote_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        card_title(ui, p, icon::LINK, s.remote_card, |_| {});
        ui.add_space(4.0);
        let mut go = false;
        let r = big_input(ui, p, &mut self.form.addr, s.code_hint, false);
        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if r.changed() {
            self.form_error = None;
        }
        ui.add_space(4.0);
        let r = big_input(ui, p, &mut self.form_password, s.password_input_hint, true);
        go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        egui::CollapsingHeader::new(RichText::new(s.advanced).size(12.5).color(p.muted))
            .id_salt("advanced")
            .show(ui, |ui| {
                let _ = muted(ui, p, s.pin);
                let r = big_input(ui, p, &mut self.form.pin, "", false);
                go |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            });
        ui.add_space(6.0);
        let ready = !self.form.addr.trim().is_empty();
        let width = ui.available_width();
        if primary_button(
            ui,
            p,
            &format!("{}  {}", s.connect_this, icon::ARROW_RIGHT),
            width,
            ready,
        )
        .clicked()
            || (go && ready)
        {
            self.connect_from_form();
        }
        if let Some(e) = &self.form_error {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon::WARNING).color(p.danger));
                ui.label(RichText::new(e).size(12.5).color(p.danger));
            });
        }
    }

    /// 最近连接过的设备（小卡片）：单击填进连接表单，双击直接连接。
    fn recent_strip(&mut self, ui: &mut egui::Ui, p: &Palette, limit: usize) {
        let s = self.s;
        let items: Vec<_> = self.recent.items().iter().take(limit).cloned().collect();
        if items.is_empty() {
            empty_state(ui, p, icon::CLOCK_COUNTER_CLOCKWISE, s.no_recent);
            return;
        }
        let mut pick = None;
        let mut go = false;
        // 顶端对齐、放不下就换行
        let layout = Layout::left_to_right(Align::TOP).with_main_wrap(true);
        ui.with_layout(layout, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(12.0, 12.0);
            for item in items {
                let (title, kind) = item.heading(s.device_code, s.ip_direct);
                let id = ui.id().with(("recent", &item.addr));
                let hovered = ui
                    .ctx()
                    .memory(|m| m.data.get_temp::<bool>(id))
                    .unwrap_or(false);
                let response = egui::Frame::new()
                    .fill(p.card)
                    .stroke(egui::Stroke::new(
                        1.0_f32,
                        if hovered { p.accent } else { p.border },
                    ))
                    .corner_radius(10)
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.set_width(196.0);
                        ui.set_height(40.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(os_icon(item.os)).size(24.0).color(p.accent));
                            ui.vertical(|ui| {
                                ui.add(egui::Label::new(RichText::new(&title).strong()).truncate());
                                let _ = muted(ui, p, &kind);
                            });
                        });
                    })
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(s.recent_hint);
                ui.ctx()
                    .memory_mut(|m| m.data.insert_temp(id, response.hovered()));
                if response.clicked() {
                    pick = Some(item.clone());
                }
                if response.double_clicked() {
                    pick = Some(item.clone());
                    go = true;
                }
            }
        });
        if let Some(item) = pick {
            self.form.addr = item.addr;
            self.form.pin = item.pin;
            self.page = Page::Home;
            if go {
                self.connect_from_form();
            }
        }
    }

    fn home_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.label(RichText::new(s.nav_home).size(22.0).strong());
        ui.add_space(14.0);
        self.permission_guide(ui, p);
        // 窗口够宽时两张卡片并排，否则上下排：并排时每张卡片至少要 CARD_MIN_WIDTH，
        // 不够时内容会溢出到旁边那张卡片上（Windows 高缩放比例下窗口逻辑宽度更小）。
        if ui.available_width() >= 2.0 * CARD_MIN_WIDTH + 8.0 {
            ui.columns(2, |cols| {
                if let [left, right] = cols {
                    card(left, p, |ui| {
                        ui.set_min_height(CARD_HEIGHT);
                        self.this_device_card(ui, p);
                    });
                    card(right, p, |ui| {
                        ui.set_min_height(CARD_HEIGHT);
                        self.remote_card(ui, p);
                    });
                }
            });
        } else {
            card(ui, p, |ui| self.this_device_card(ui, p));
            ui.add_space(12.0);
            card(ui, p, |ui| self.remote_card(ui, p));
        }
        ui.add_space(18.0);
        ui.label(RichText::new(s.recent).size(15.0).strong());
        ui.add_space(6.0);
        self.recent_strip(ui, p, 6);
    }

    fn devices_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.label(RichText::new(s.nav_devices).size(22.0).strong());
        ui.add_space(4.0);
        let _ = muted(ui, p, s.devices_hint);
        ui.add_space(14.0);
        let items: Vec<_> = self.recent.items().to_vec();
        if items.is_empty() {
            empty_state(ui, p, icon::CLOCK_COUNTER_CLOCKWISE, s.no_recent);
            return;
        }
        let mut connect = None;
        let mut forget = None;
        let mut files = None;
        card(ui, p, |ui| {
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    ui.separator();
                }
                let (title, kind) = item.heading(s.device_code, s.ip_direct);
                ui.horizontal(|ui| {
                    ui.set_min_height(44.0);
                    ui.label(RichText::new(os_icon(item.os)).size(24.0).color(p.accent));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&title).strong());
                        let detail = if item.pin.is_empty() {
                            kind.to_owned()
                        } else {
                            format!("{kind}  ·  {}", short_fingerprint(&item.pin))
                        };
                        let _ = muted(ui, p, &detail);
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if icon_button(ui, icon::TRASH, s.forget).clicked() {
                            forget = Some(item.addr.clone());
                        }
                        // 文件传输：先连上这台设备，连上后自动打开文件管理
                        if icon_button(ui, icon::FOLDER_OPEN, s.file_transfer).clicked() {
                            files = Some(item.clone());
                        }
                        if primary_button(ui, p, s.connect_this, 88.0, true).clicked() {
                            connect = Some(item.clone());
                        }
                    });
                });
            }
        });
        if let Some(addr) = forget {
            if let Err(e) = self.recent.forget(&addr) {
                self.status = Status::error(e.to_string());
            }
        }
        if let Some(item) = files {
            self.files_after = Some(item.addr.clone());
            self.status = Status::info(s.files_after_connect);
            connect = Some(item);
        }
        if let Some(item) = connect {
            // 回到首页、填好表单：可能要输入密码
            self.form.addr = item.addr;
            self.form.pin = item.pin;
            self.page = Page::Home;
        }
    }

    fn settings_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.label(RichText::new(s.nav_settings).size(22.0).strong());
        ui.add_space(14.0);

        card(ui, p, |ui| {
            card_title(ui, p, icon::PLUG, s.section_server, |_| {});
            let _ = muted(ui, p, s.via_server);
            ui.add_space(6.0);
            let mut save = false;
            for (label, value, secret) in [
                (s.server_addr, &mut self.server.addr, false),
                (s.server_pin, &mut self.server.pin, false),
                (s.account_token, &mut self.server.account, true),
            ] {
                ui.add_space(4.0);
                let _ = muted(ui, p, label);
                let r = big_input(ui, p, value, "", secret);
                save |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }
            ui.add_space(8.0);
            save |= primary_button(ui, p, s.save, 120.0, true).clicked();
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
        ui.add_space(12.0);

        card(ui, p, |ui| {
            card_title(ui, p, icon::WIFI_HIGH, s.section_network, |ui| {
                if icon_button(ui, icon::ARROWS_CLOCKWISE, s.refresh).clicked() {
                    self.addrs = lan_addresses();
                }
            });
            // 和其他输入框同一种样式：小字标签在上，输入框在下
            let _ = muted(ui, p, s.port);
            let editable = self.phase == HostPhase::Off;
            let field = ui.add_enabled(
                editable,
                TextEdit::singleline(&mut self.port)
                    .margin(egui::Margin::symmetric(10, 8))
                    .desired_width(120.0),
            );
            if !editable {
                let _ = field.on_disabled_hover_text(s.port_locked);
            }
            ui.add_space(6.0);
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
                    if icon_button(ui, icon::COPY, s.copy).clicked() {
                        self.copy(ui, &addr, &addr);
                    }
                });
            }
            if let Some(report) = self.net.clone() {
                ui.add_space(6.0);
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
        ui.add_space(12.0);

        card(ui, p, |ui| {
            card_title(ui, p, icon::GEAR_SIX, s.section_general, |_| {});
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
        ui.add_space(12.0);

        card(ui, p, |ui| {
            card_title(ui, p, icon::INFO, s.section_about, |_| {});
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
                        if icon_button(ui, icon::COPY, s.copy).clicked() {
                            self.copy(ui, &fp, s.fingerprint);
                        }
                    });
                }
                Err(e) => {
                    ui.colored_label(p.danger, e);
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
        ui.label(RichText::new(s.perm_title).size(22.0).strong());
        ui.add_space(4.0);
        let _ = muted(ui, p, s.perm_subtitle);
        ui.add_space(14.0);

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
                card(col, p, |ui| {
                    ui.set_min_height(120.0);
                    // 标题一行，状态单独一行：英文标题很长，放同一行会和状态标签重叠
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(glyph).size(22.0).color(p.accent));
                        ui.add(egui::Label::new(RichText::new(name).size(15.0).strong()).wrap());
                    });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ok {
                            let _ = pill(ui, s.perm_granted, p.success);
                        } else {
                            let _ = pill(ui, s.perm_missing, p.warning);
                        }
                    });
                    ui.add_space(6.0);
                    let _ = muted(ui, p, what);
                    if ok {
                        return;
                    }
                    ui.add_space(10.0);
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
                            primary_button(ui, p, s.perm_request, 120.0, true)
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
        ui.add_space(14.0);

        if self.perms.screen_capture && self.perms.input_injection {
            card(ui, p, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icon::CHECK_CIRCLE)
                            .size(36.0)
                            .color(p.success),
                    );
                    ui.vertical(|ui| {
                        ui.label(RichText::new(s.perm_done_title).size(18.0).strong());
                        let _ = muted(ui, p, s.perm_done);
                    });
                });
            });
            return;
        }
        // 先做屏幕录制，再做辅助功能
        let screen = !self.perms.screen_capture;
        let item = if screen { names.screen } else { names.input };
        card(ui, p, |ui| {
            let step_no = if screen { 1 } else { 2 };
            ui.label(
                RichText::new(format!(
                    "{}  ·  {item}",
                    s.perm_step.replace("{n}", &step_no.to_string())
                ))
                .size(16.0)
                .strong(),
            );
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                guide_art::mac_settings(ui, p, &names, item, false, s.perm_callout, s.perm_unlock);
            });
            ui.add_space(12.0);
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
                        .fill(p.accent)
                        .corner_radius(11)
                        .inner_margin(egui::Margin::symmetric(7, 1))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&n).strong().color(Color32::WHITE));
                        });
                    ui.add(egui::Label::new(text).wrap());
                });
                ui.add_space(4.0);
            }
            if screen {
                // 屏幕录制授权后要重新打开程序才生效
                ui.add_space(6.0);
                if ui
                    .add(egui::Button::new(s.perm_relaunch).min_size(egui::vec2(0.0, 40.0)))
                    .clicked()
                {
                    self.perform(Action::Relaunch);
                }
            }
        });
    }

    /// 左侧导航栏：品牌、页面、服务器状态。
    fn sidebar(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);
            ui.label(RichText::new(icon::MONITOR).size(24.0).color(p.accent));
            ui.label(RichText::new("PeerDesk").size(19.0).strong());
        });
        ui.add_space(22.0);
        for (page, glyph, text) in [
            (Page::Home, icon::DESKTOP, s.nav_home),
            (Page::Devices, icon::CLOCK_COUNTER_CLOCKWISE, s.nav_devices),
            (Page::Settings, icon::GEAR_SIX, s.nav_settings),
        ] {
            if nav_item(ui, p, glyph, text, self.page == page).clicked() {
                self.page = page;
            }
            ui.add_space(2.0);
        }
        ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
            ui.add_space(14.0);
            let (text, color) = if !self.server.is_set() {
                (s.server_unset, p.muted)
            } else if self.device_code.is_some() {
                (s.server_online, p.success)
            } else {
                (s.server_offline, p.warning)
            };
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let _ = muted(
                    ui,
                    p,
                    &format!(
                        "v{} · {}",
                        env!("CARGO_PKG_VERSION"),
                        env!("PEERDESK_COMMIT")
                    ),
                );
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                let _ = pill(ui, text, color);
            });
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
            (icon::WARNING, p.danger)
        } else {
            (icon::INFO, p.muted)
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(glyph).color(color));
            ui.label(RichText::new(&self.status.text).size(12.5).color(color));
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
            .exact_width(200.0)
            .frame(
                egui::Frame::new()
                    .fill(p.sidebar)
                    .inner_margin(egui::Margin::symmetric(12, 0)),
            )
            .show(ctx, |ui| {
                // 右侧一条细分隔线（画在面板里面，不留缝）
                let r = ui.max_rect();
                ui.painter().vline(
                    r.right() + 11.5,
                    r.y_range(),
                    egui::Stroke::new(1.0_f32, p.border),
                );
                self.sidebar(ui, &p);
            });
        egui::TopBottomPanel::bottom("status")
            .show_separator_line(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(28, 8)),
            )
            .show(ctx, |ui| {
                ui.set_min_height(20.0);
                self.status_line(ui, &p);
            });
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(28, 22)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| match self.page {
                    Page::Home => self.home_page(ui, &p),
                    Page::Devices => self.devices_page(ui, &p),
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
                    .is_some_and(|(addr, _)| files_after.as_deref() == Some(addr.as_str()))
                {
                    open_files = Some(viewer.title().to_owned());
                    files_after = None;
                }
                if let Some((addr, pin)) = remember.take() {
                    remembered.push((addr, pin, outcome.peer.0, outcome.peer.1));
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
        for (addr, pin, os, name) in remembered {
            if let Err(e) = self.recent.remember(&addr, &pin, os, &name) {
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

/// 空列表时的提示：大图标 + 一句话，居中。
fn empty_state(ui: &mut egui::Ui, p: &Palette, glyph: &str, text: &str) {
    card(ui, p, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(12.0);
            ui.label(
                RichText::new(glyph)
                    .size(36.0)
                    .color(p.muted.gamma_multiply(0.7)),
            );
            ui.add_space(6.0);
            let _ = muted(ui, p, text);
            ui.add_space(12.0);
        });
    });
}
