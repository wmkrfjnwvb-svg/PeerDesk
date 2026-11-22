//! 主窗口（eframe 接线）：左侧导航（远程控制、设备、设置），右侧页面；首页是“本设备”和
//! “远程控制设备”两张卡片加最近连接。
//! 只画控件、把操作交给会话线程；逻辑在同目录的其他模块里。

use std::io::Write;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, TextEdit};
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
/// 启动时打开哪一页（home、devices、settings、permissions），截图用。
const PAGE_ENV: &str = "PEERDESK_GUI_PAGE";
/// 预览 macOS 授权引导的环境变量（值是 macOS 主版本号）。
const PREVIEW_MAC_ENV: &str = "PEERDESK_GUI_PREVIEW_MAC";
/// 权限向导多久重新检查一次权限。
const GUIDE_REFRESH: Duration = Duration::from_secs(1);

/// 打开图形界面，直到窗口关闭。返回进程退出码。
pub fn run() -> u8 {
    let lang = effective_lang(&default_data_dir());
    peerdesk_net::rendezvous::set_prefer_ipv6(lang.prefers_ipv6());
    let s = strings(lang);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(s.app_title)
            .with_app_id("peerdesk")
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
    serial: u64,
    status: Status,
    selftest: bool,
    selftest_connect: bool,
    repaint: Repaint,
    page: Page,
    show_password: bool,
    perms: peerdesk_platform::PermissionStatus,
    /// 预览 macOS 引导（在别的系统上截图用）：权限一律当作没有。
    preview_mac: bool,
    appearance: Appearance,
}

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
        let (password, status) = match password::generate() {
            Ok(p) => (p, Status::info(s.ready)),
            Err(e) => (String::new(), Status::error(e)),
        };
        let ctx2 = ctx.clone();
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
            perms,
            preview_mac,
            appearance,
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
            Err(e) => self.status = Status::error(e),
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
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
        self.status = Status::info(format!("{} {shown}", self.s.copied));
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

    /// “本设备”卡片：开关、设备码、密码、谁在控制。
    fn this_device_card(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        let mut on = self.phase.is_on();
        // 权限不全时不能开启被控（关闭总是可以）。
        let enabled = !matches!(self.phase, HostPhase::Stopping | HostPhase::Starting)
            && (self.guide.ready || self.phase.is_on());
        let (state, color) = self.host_state(p);
        let mut toggled = false;
        card_title(ui, p, icon::DESKTOP, s.this_device, |ui| {
            toggled = toggle(ui, p, &mut on, enabled)
                .on_hover_text(s.allow_control)
                .changed();
            let _ = pill(ui, state, color);
        });
        if toggled {
            if on {
                self.start_hosting();
            } else {
                self.stop_hosting();
            }
        }
        ui.add_space(4.0);
        if !self.phase.is_on() {
            let _ = muted(ui, p, s.host_off_hint);
            ui.add_space(8.0);
        }

        // 设备码（登记到服务器后才有）
        let _ = muted(ui, p, s.device_code);
        ui.horizontal(|ui| match self.device_code {
            Some(code) => {
                let shown = format_code(code);
                ui.label(RichText::new(&shown).monospace().size(30.0).strong());
                if icon_button(ui, icon::COPY, s.copy).clicked() {
                    self.copy(ui, &code.to_string(), &shown);
                }
            }
            None => {
                ui.label(RichText::new("— — —").monospace().size(30.0).color(p.muted));
            }
        });
        if self.device_code.is_none() && !self.server.is_set() {
            // 没设服务器就没有设备码：引导去设置（局域网里仍可以用地址直连）
            let link = egui::Button::new(
                RichText::new(format!("{}  {}", s.server_unset, icon::ARROW_RIGHT))
                    .size(12.5)
                    .color(p.accent),
            )
            .frame(false);
            if ui.add(link).clicked() {
                self.page = Page::Settings;
            }
        }
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
                match password::generate() {
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
                        ui.label(format!(
                            "{}  {addr}  {}",
                            s.controller,
                            short_fingerprint(&peer)
                        ));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
    }

    /// 最近连接过的设备（小卡片，点一下填进连接表单）。
    fn recent_strip(&mut self, ui: &mut egui::Ui, p: &Palette, limit: usize) {
        let s = self.s;
        let items: Vec<_> = self.recent.items().iter().take(limit).cloned().collect();
        if items.is_empty() {
            let _ = muted(ui, p, s.no_recent);
            return;
        }
        let mut pick = None;
        ui.horizontal_wrapped(|ui| {
            for item in items {
                let fp = if item.pin.is_empty() {
                    String::new()
                } else {
                    short_fingerprint(&item.pin)
                };
                let response = egui::Frame::new()
                    .fill(p.card)
                    .stroke(egui::Stroke::new(1.0_f32, p.border))
                    .corner_radius(10)
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.set_width(190.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(icon::DESKTOP_TOWER)
                                    .size(22.0)
                                    .color(p.accent),
                            );
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&item.addr).strong());
                                if !fp.is_empty() {
                                    let _ = muted(ui, p, &fp);
                                }
                            });
                        });
                    })
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if response.clicked() {
                    pick = Some(item.clone());
                }
            }
        });
        if let Some(item) = pick {
            self.form.addr = item.addr;
            self.form.pin = item.pin;
            self.page = Page::Home;
        }
    }

    fn home_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.label(RichText::new(s.nav_home).size(22.0).strong());
        ui.add_space(14.0);
        self.permission_guide(ui, p);
        ui.columns(2, |cols| {
            if let [left, right] = cols {
                card(left, p, |ui| self.this_device_card(ui, p));
                card(right, p, |ui| self.remote_card(ui, p));
            }
        });
        ui.add_space(18.0);
        ui.label(RichText::new(s.recent).size(15.0).strong());
        ui.add_space(6.0);
        self.recent_strip(ui, p, 6);
    }

    fn devices_page(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let s = self.s;
        ui.label(RichText::new(s.nav_devices).size(22.0).strong());
        ui.add_space(14.0);
        ui.label(RichText::new(s.recent).size(15.0).strong());
        ui.add_space(6.0);
        self.recent_strip(ui, p, usize::MAX);
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
                        Ok(()) => Status::info(s.saved),
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
            ui.horizontal(|ui| {
                ui.label(format!("{}:", s.port));
                let editable = self.phase == HostPhase::Off;
                let _ = ui.add_enabled(
                    editable,
                    TextEdit::singleline(&mut self.port).desired_width(80.0),
                );
            });
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

    fn perm_button(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        text: &str,
        action: Action,
        primary: bool,
    ) {
        let clicked = if primary {
            primary_button(ui, p, text, 150.0, true).clicked()
        } else {
            ui.add(egui::Button::new(text).min_size(egui::vec2(0.0, 40.0)))
                .clicked()
        };
        if clicked {
            if let Err(e) = peerdesk_platform::perform(action) {
                self.status = Status::error(e);
            }
            // 授权对话框关掉后马上重新检查
            self.guide_checked = Instant::now() - GUIDE_REFRESH;
        }
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
        ui.columns(2, |cols| {
            for (col, (name, what, ok, glyph)) in cols.iter_mut().zip(items) {
                card(col, p, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(glyph).size(22.0).color(p.accent));
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(name).strong());
                                if ok {
                                    let _ = pill(ui, s.perm_granted, p.success);
                                } else {
                                    let _ = pill(ui, s.perm_missing, p.warning);
                                }
                            });
                            let _ = muted(ui, p, what);
                        });
                    });
                });
            }
        });
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
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if screen {
                    self.perm_button(ui, p, s.perm_request, Action::RequestScreen, true);
                    self.perm_button(
                        ui,
                        p,
                        s.perm_open_settings,
                        Action::OpenScreenSettings,
                        false,
                    );
                    self.perm_button(ui, p, s.perm_relaunch, Action::Relaunch, false);
                } else {
                    self.perm_button(ui, p, s.perm_request, Action::RequestInput, true);
                    self.perm_button(
                        ui,
                        p,
                        s.perm_open_settings,
                        Action::OpenInputSettings,
                        false,
                    );
                }
            });
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
        ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(14.0);
            let (text, color) = if !self.server.is_set() {
                (s.server_unset, p.muted)
            } else if self.device_code.is_some() {
                (s.server_online, p.success)
            } else {
                (s.server_offline, p.warning)
            };
            ui.horizontal(|ui| {
                ui.add_space(6.0);
                let _ = pill(ui, text, color);
            });
        });
    }

    /// 内容区底部的状态（最近一次操作的结果）。
    fn status_line(&self, ui: &mut egui::Ui, p: &Palette) {
        if self.status.text.is_empty() {
            return;
        }
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
fn open_folder(dir: &std::path::Path) -> Result<(), String> {
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
            .show(ctx, |ui| self.status_line(ui, &p));
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

        let s = self.s;
        let mut remembered = Vec::new();
        self.views.retain_mut(|(viewer, remember)| {
            let outcome = viewer.show(ctx, s);
            if outcome.connected {
                remembered.extend(remember.take());
            }
            outcome.open
        });
        for (addr, pin) in remembered {
            if let Err(e) = self.recent.remember(&addr, &pin) {
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
