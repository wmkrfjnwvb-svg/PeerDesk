//! 主窗口（eframe 接线）：“本机”和“连接”两个面板，加一行状态。
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
use peerdesk_platform::guide::{guide, Guide, Os};

use super::form::{parse_pin, parse_port, ConnectForm, Target, DEFAULT_PORT};
use super::i18n::{strings, system_lang, Strings};
use super::netinfo::lan_addresses;
use super::recent::RecentStore;
use super::session::{start_host, start_view, HostConfig, HostEvent, HostHandle, Repaint};
use super::settings::ServerSettings;
use super::state::{HostPhase, Status};
use super::stats::short_fingerprint;
use super::viewer::Viewer;
use super::{fonts, password};
use crate::remote::default_data_dir;
use crate::{EXIT_FAILED, EXIT_OK};

/// 自测模式：启动后自动开启被控、再连接本机。值为 `host` 时只开启被控。CI 用它截图。
const SELFTEST_ENV: &str = "PEERDESK_GUI_SELFTEST";
/// 权限向导多久重新检查一次权限。
const GUIDE_REFRESH: Duration = Duration::from_secs(1);

/// 打开图形界面，直到窗口关闭。返回进程退出码。
pub fn run() -> u8 {
    let s = strings(system_lang());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(s.app_title)
            .with_app_id("peerdesk")
            .with_inner_size([560.0, 760.0])
            .with_min_inner_size([420.0, 480.0]),
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
}

impl App {
    fn new(ctx: &egui::Context, s: &'static Strings) -> Self {
        let (defs, _font) = fonts::definitions();
        ctx.set_fonts(defs);
        let data_dir = default_data_dir();
        let fingerprint = Identity::load_or_create(&data_dir)
            .map(|id| to_hex(&id.fingerprint()))
            .map_err(|e| format!("{}：{e}", s.err_identity));
        let (password, status) = match password::generate() {
            Ok(p) => (p, Status::info(s.ready)),
            Err(e) => (String::new(), Status::error(e)),
        };
        let ctx2 = ctx.clone();
        let os = peerdesk_platform::current_os();
        Self {
            s,
            recent: RecentStore::load(&data_dir),
            server: ServerSettings::load(&data_dir),
            device_code: None,
            net: None,
            os,
            guide: guide(os, peerdesk_platform::check_permissions()),
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
            Ok(target) => {
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

    fn copy_row(&mut self, ui: &mut egui::Ui, label: &str, shown: &str, copied: &str) {
        ui.horizontal(|ui| {
            ui.label(format!("{label}:"));
            ui.monospace(shown);
            if ui.small_button(self.s.copy).clicked() {
                ui.ctx().copy_text(copied.to_owned());
                self.status = Status::info(format!("{} {copied}", self.s.copied));
            }
        });
    }

    /// 权限向导：每秒重新检查一次权限，用户在系统设置里打开开关后这里立刻变成打勾。
    fn permission_guide(&mut self, ui: &mut egui::Ui) {
        if self.guide_checked.elapsed() >= GUIDE_REFRESH {
            self.guide = guide(self.os, peerdesk_platform::check_permissions());
            self.guide_checked = Instant::now();
        }
        ui.ctx().request_repaint_after(GUIDE_REFRESH);
        if self.guide.steps.is_empty() {
            return;
        }
        let color = if self.guide.ready {
            Color32::from_rgb(60, 160, 90)
        } else {
            Color32::from_rgb(220, 140, 40)
        };
        let title = self.guide.title.clone();
        egui::CollapsingHeader::new(RichText::new(title).color(color).strong())
            .id_salt("permission-guide")
            .default_open(!self.guide.ready)
            .show(ui, |ui| {
                for step in self.guide.steps.clone() {
                    ui.horizontal_wrapped(|ui| {
                        let mark = if step.done { "✔" } else { "○" };
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
    }

    fn this_device(&mut self, ui: &mut egui::Ui) {
        let s = self.s;
        ui.heading(s.this_device);
        self.permission_guide(ui);
        let mut on = self.phase.is_on();
        // 权限不全时不能开启被控（关闭总是可以）。
        let enabled = !matches!(self.phase, HostPhase::Stopping | HostPhase::Starting)
            && (self.guide.ready || self.phase.is_on());
        ui.horizontal(|ui| {
            if ui
                .add_enabled(enabled, egui::Checkbox::new(&mut on, s.allow_control))
                .changed()
            {
                if on {
                    self.start_hosting();
                } else {
                    self.stop_hosting();
                }
            }
            let state = match &self.phase {
                HostPhase::Off => s.host_off,
                HostPhase::Starting => s.host_starting,
                HostPhase::Listening { .. } => s.host_listening,
                HostPhase::Busy {
                    established: false, ..
                } => s.host_handshaking,
                HostPhase::Busy {
                    established: true, ..
                } => s.host_controlled,
                HostPhase::Stopping => s.host_stopping,
            };
            ui.label(RichText::new(state).strong());
        });
        ui.horizontal(|ui| {
            ui.label(format!("{}:", s.port));
            let editable = self.phase == HostPhase::Off;
            let _ = ui.add_enabled(
                editable,
                TextEdit::singleline(&mut self.port).desired_width(70.0),
            );
        });

        let port = self.phase.port();
        let shown_port = port.map_or_else(
            || parse_port(&self.port, s).unwrap_or(DEFAULT_PORT),
            |p| p.0,
        );
        let ipv6 = port.is_none_or(|p| p.1);
        ui.horizontal(|ui| {
            ui.label(format!("{}:", s.addresses));
            if ui.small_button(s.refresh).clicked() {
                self.addrs = lan_addresses();
            }
        });
        let addrs: Vec<String> = self
            .addrs
            .iter()
            .filter(|ip| ip.is_ipv4() || ipv6)
            .map(|ip| SocketAddr::new(*ip, shown_port).to_string())
            .collect();
        if addrs.is_empty() {
            ui.label(s.no_addresses);
        }
        for addr in addrs {
            ui.horizontal(|ui| {
                ui.add_space(12.0);
                ui.monospace(&addr);
                if ui.small_button(s.copy).clicked() {
                    ui.ctx().copy_text(addr.clone());
                    self.status = Status::info(format!("{} {addr}", s.copied));
                }
            });
        }

        if let Some(code) = self.device_code {
            let shown = format_code(code);
            ui.horizontal(|ui| {
                ui.label(format!("{}:", s.device_code));
                ui.label(RichText::new(&shown).monospace().size(22.0).strong());
                if ui.small_button(s.copy).clicked() {
                    ui.ctx().copy_text(code.to_string());
                    self.status = Status::info(format!("{} {shown}", s.copied));
                }
            });
        }
        if let Some(report) = &self.net {
            // 网络体检：开启被控时做一次，连接时直接用；出问题时让用户一眼看到卡在哪一段。
            egui::CollapsingHeader::new(s.network_check)
                .id_salt("network-check")
                .show(ui, |ui| {
                    egui::Grid::new("network-check-grid")
                        .num_columns(2)
                        .show(ui, |ui| {
                            for (item, value) in report.lines() {
                                ui.label(item);
                                ui.label(value);
                                ui.end_row();
                            }
                        });
                });
        }
        match self.fingerprint.clone() {
            Ok(fp) => self.copy_row(ui, s.fingerprint, &short_fingerprint(&fp), &fp),
            Err(e) => {
                ui.colored_label(Color32::LIGHT_RED, e);
            }
        }
        let password = self.password.clone();
        ui.horizontal(|ui| {
            ui.label(format!("{}:", s.password));
            ui.label(RichText::new(&password).monospace().size(20.0).strong());
            if ui.small_button(s.copy).clicked() {
                ui.ctx().copy_text(password.clone());
                self.status = Status::info(s.copied);
            }
            if ui.small_button(s.new_password).clicked() {
                match password::generate() {
                    Ok(p) => {
                        if let Some(host) = &self.host {
                            host.set_password(&p);
                        }
                        self.password = p;
                    }
                    Err(e) => self.status = Status::error(e),
                }
            }
        });
        ui.small(s.password_hint);

        if let HostPhase::Busy { addr, peer, .. } = &self.phase {
            let (addr, peer) = (*addr, peer.clone());
            ui.horizontal(|ui| {
                ui.label(format!(
                    "{}: {addr}  {}",
                    s.controller,
                    short_fingerprint(&peer)
                ));
                if ui.button(s.disconnect).clicked() {
                    if let Some(host) = &self.host {
                        host.kick();
                    }
                }
            });
        }
        if let Some(host) = &self.host {
            let tap = host.tap();
            if tap.count() > 0 {
                let last = tap.last().map(|e| format!("{e:?}")).unwrap_or_default();
                ui.small(format!("{}: {}  {last}", s.inputs_received, tap.count()));
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
    }

    fn server_panel(&mut self, ui: &mut egui::Ui) {
        let s = self.s;
        egui::CollapsingHeader::new(s.register_server)
            .default_open(!self.server.is_set())
            .show(ui, |ui| {
                ui.small(s.via_server);
                let mut save = labeled(ui, s.server_addr, &mut self.server.addr, false);
                save |= labeled(ui, s.server_pin, &mut self.server.pin, false);
                ui.horizontal(|ui| {
                    save |= ui.button(s.save).clicked();
                    if ui.button(s.open_logs).clicked() {
                        self.status = match open_folder(&self.data_dir.join("logs")) {
                            Ok(()) => {
                                Status::info(self.data_dir.join("logs").display().to_string())
                            }
                            Err(e) => Status::error(e),
                        };
                    }
                });
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
    }

    fn connect_panel(&mut self, ui: &mut egui::Ui) {
        let s = self.s;
        ui.heading(s.connect_title);
        let mut go = labeled(ui, s.remote_addr, &mut self.form.addr, false);
        go |= labeled(ui, s.password, &mut self.form_password, true);
        egui::CollapsingHeader::new(s.pin).show(ui, |ui| {
            go |= labeled(ui, s.pin, &mut self.form.pin, false);
        });
        if ui.button(RichText::new(s.connect).strong()).clicked() || go {
            self.connect_from_form();
        }
        if !self.recent.items().is_empty() {
            ui.label(format!("{}:", s.recent));
            let mut pick = None;
            for item in self.recent.items() {
                let text = if item.pin.is_empty() {
                    item.addr.clone()
                } else {
                    format!("{}  {}", item.addr, short_fingerprint(&item.pin))
                };
                if ui.small_button(text).clicked() {
                    pick = Some(item.clone());
                }
            }
            if let Some(item) = pick {
                self.form.addr = item.addr;
                self.form.pin = item.pin;
            }
        }
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

/// 一行“标签 + 输入框”。在输入框里按回车返回 `true`。
fn labeled(ui: &mut egui::Ui, label: &str, value: &mut String, secret: bool) -> bool {
    ui.horizontal(|ui| {
        ui.label(format!("{label}:"));
        let response = ui.add(
            TextEdit::singleline(value)
                .password(secret)
                .desired_width(f32::INFINITY),
        );
        response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
    })
    .inner
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.selftest && self.host.is_none() && self.phase == HostPhase::Off {
            self.start_hosting();
        }
        self.poll_host();

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            let color = if self.status.error {
                Color32::LIGHT_RED
            } else {
                ui.visuals().text_color()
            };
            ui.colored_label(color, &self.status.text);
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    self.this_device(ui);
                });
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    self.connect_panel(ui);
                });
                ui.add_space(8.0);
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    self.server_panel(ui);
                });
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
