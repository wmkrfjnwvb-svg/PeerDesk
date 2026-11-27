//! 常驻后台的接线：把托盘、关闭请求、单实例、开机自启接到主窗口上。
//! `app.rs` 只在几处调用这里（启动、每帧、设置页），其余都在这个文件和 `tray.rs`、`tray_logic.rs` 里。
//! 要有真实的桌面和托盘宿主才能运行，和 `tray.rs` 一样不计入覆盖率（见 `scripts/coverage.sh`）；
//! 决定“关闭是隐藏还是退出”的逻辑在 `tray_logic.rs`，自启在 `crate::autostart`，单实例在 `crate::instance`。

use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;

use eframe::egui;

use super::i18n::Strings;
use super::state::Status;
use super::theme::{muted, panel, section_title, toggle, Palette};
use super::tray::{self, Tray};
use super::tray_logic::{self, CloseAction, Lifecycle, TrayCommand, WindowAction};
use crate::{autostart, instance};

/// 自测模式（CI 截图）的环境变量，和 `app.rs` 里的同一个。
const SELFTEST_ENV: &str = "PEERDESK_GUI_SELFTEST";

/// 启动时抢单实例锁的结果。
#[derive(Debug)]
pub enum Startup {
    /// 已经有一个在跑：已请它显示窗口，本进程应以退出码 0 退出（守护不会当成崩溃）。
    AlreadyRunning,
    /// 继续启动；文件是单实例锁（要拿着直到进程结束，系统在进程消失时释放）。
    Proceed(Option<std::fs::File>),
}

/// 单实例检查。自测模式不检查。
pub fn startup(data_dir: &Path) -> Startup {
    if std::env::var_os(SELFTEST_ENV).is_some() {
        return Startup::Proceed(None);
    }
    match instance::acquire(data_dir) {
        Ok(instance::Instance::First(file)) => Startup::Proceed(Some(file)),
        Ok(instance::Instance::Running) => {
            if let Err(e) = instance::request_show(data_dir) {
                log::warn!("无法通知已在运行的 PeerDesk：{e}");
            }
            Startup::AlreadyRunning
        }
        Err(e) => {
            // 锁文件打不开：照常启动（宁可可能多开，也不能因此不能用），原因写进日志
            log::warn!("单实例检查失败，继续启动：{e}");
            Startup::Proceed(None)
        }
    }
}

/// 托盘、关闭、自启的状态。
pub struct Background {
    life: Lifecycle,
    tray: Option<Tray>,
    commands: Receiver<TrayCommand>,
    autostart: bool,
    data_dir: PathBuf,
    _lock: Option<std::fs::File>,
}

impl std::fmt::Debug for Background {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Background")
            .field("life", &self.life)
            .field("autostart", &self.autostart)
            .finish_non_exhaustive()
    }
}

impl Background {
    /// 建托盘、开始监听第二次启动的请求、确认开机自启。返回的状态文字非空时要显示给用户。
    pub fn new(
        ctx: &egui::Context,
        s: &'static Strings,
        data_dir: &Path,
        lock: Option<std::fs::File>,
    ) -> (Self, Option<Status>) {
        let selftest = std::env::var_os(SELFTEST_ENV).is_some();
        let (tx, commands) = std::sync::mpsc::channel();
        tray::watch_requests(ctx, data_dir, &tx);
        tray::attach_dock(ctx, &tx);
        // 自测（CI 截图，没有桌面托盘）和 PEERDESK_NO_TRAY 不建托盘。建不了时不静默：告诉用户，
        // 此时关闭窗口就是退出，否则程序藏起来了用户却找不到。
        let (tray, status) = if selftest || std::env::var_os("PEERDESK_NO_TRAY").is_some() {
            (None, None)
        } else {
            match Tray::create(ctx, s, &tx) {
                Ok(tray) => (Some(tray), None),
                Err(why) => {
                    log::warn!("系统托盘不可用：{why}");
                    (None, Some(Status::error(s.tray_unavailable)))
                }
            }
        };
        let life = Lifecycle::new(tray.is_some(), tray_logic::hint_seen(data_dir));
        // 开机自启和自动恢复：默认开，用户在设置里关过就不再装。开发构建（cargo run）不自动装，
        // 否则会把 target 目录里的程序注册成开机项。
        let autostart = !autostart::user_disabled(data_dir);
        // PEERDESK_AUTOSTART=force：实机测试用，开发构建也真的写启动项（测试要验证启动时不起外部程序、不闪窗口）
        let forced = std::env::var_os("PEERDESK_AUTOSTART").is_some_and(|v| v == "force");
        if autostart && !selftest && (forced || !cfg!(debug_assertions)) {
            ensure_autostart();
        }
        log::info!(
            "后台常驻就绪：托盘{}",
            if tray.is_some() {
                "已建立"
            } else {
                "没有"
            }
        );
        let bg = Self {
            life,
            tray,
            commands,
            autostart,
            data_dir: data_dir.to_owned(),
            _lock: lock,
        };
        (bg, status)
    }

    /// 每帧开头调用：处理托盘命令和窗口关闭请求。`connected`：现在有没有人连着；`kick`：断开当前控制端。
    /// 返回要显示的状态文字（如果有）。
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        s: &'static Strings,
        connected: bool,
        kick: impl Fn(),
    ) -> Option<Status> {
        let mut status = None;
        let commands: Vec<TrayCommand> = self.commands.try_iter().collect();
        for command in commands {
            match self.life.on_command(command, connected) {
                WindowAction::Show => {
                    log::info!("显示主窗口");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                WindowAction::Kick => kick(),
                // 关闭请求下一帧到来时 `life` 已经是退出状态，会放行
                WindowAction::Exit => {
                    // 退出前先告诉对端“我退出了”，对端不用等超时
                    let told = super::session::say_goodbye();
                    log::info!("退出，已通知 {told} 个会话的对端");
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                WindowAction::Nothing => {}
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            let dir = &self.data_dir;
            let action = self.life.on_close_requested(|| {
                // 窗口马上隐藏，窗口里的提示没人看得到：发系统通知
                let delivered = tray::notify("PeerDesk", s.tray_hint);
                if let Err(e) = tray_logic::mark_hint_seen(dir) {
                    log::warn!("记录已提示失败：{e}");
                }
                delivered
            });
            match action {
                CloseAction::Hide => {
                    log::info!("窗口已隐藏到托盘，程序继续在后台运行");
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                }
                CloseAction::Stay => {
                    // 通知发不出去（没有通知服务）：这次不隐藏，在窗口里说明，下次关闭才隐藏
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    status = Some(Status::info(s.tray_hint));
                }
                CloseAction::Quit => {
                    // 没有托盘时关窗口就是退出：同样先通知对端
                    let told = super::session::say_goodbye();
                    log::info!("关闭窗口即退出，已通知 {told} 个会话的对端");
                }
            }
        }
        if let Some(tray) = &mut self.tray {
            tray.set_connected(connected);
        }
        status
    }

    /// 设置页的“开机自启”卡片。用户拨了开关时返回结果文字。
    pub fn settings_card(
        &mut self,
        ui: &mut egui::Ui,
        p: &Palette,
        s: &'static Strings,
    ) -> Option<Status> {
        let mut on = self.autostart;
        panel(ui, p, |ui| {
            section_title(ui, s.autostart_label, |ui| {
                let _ = toggle(ui, p, &mut on, s.autostart_label, true);
            });
            let _ = muted(ui, p, s.autostart_hint);
        });
        (on != self.autostart).then(|| self.set_autostart(on, s))
    }

    /// 开启或关闭开机自启和自动恢复，并记住用户的选择。
    fn set_autostart(&mut self, on: bool, s: &'static Strings) -> Status {
        let result = autostart::Env::detect().and_then(|env| {
            if on {
                autostart::enable(&env, &mut autostart::system_runner).map(Some)
            } else {
                autostart::disable(&env, &mut autostart::system_runner).map(|()| None)
            }
        });
        match result {
            Ok(outcome) => {
                self.autostart = on;
                if let Err(e) = autostart::remember_choice(&self.data_dir, on) {
                    log::warn!("记住开机自启的选择失败：{e}");
                }
                Status::info(match outcome {
                    None => s.autostart_off,
                    Some(autostart::Outcome::Fallback) => s.autostart_fallback,
                    Some(autostart::Outcome::System | autostart::Outcome::Guard) => s.autostart_on,
                })
            }
            Err(why) => {
                log::warn!("设置开机自启失败：{why}");
                Status::error(format!("{}：{why}", s.autostart_failed))
            }
        }
    }
}

/// 后台开启开机自启和自动恢复（每次启动都确认一遍：程序挪了位置、升级后路径变了会跟着更新）。
/// 要运行系统命令（systemctl、reg），放到线程里，不卡界面启动；结果只写日志。
fn ensure_autostart() {
    let spawned = std::thread::Builder::new()
        .name("autostart".to_owned())
        .spawn(|| {
            let result = autostart::Env::detect()
                .and_then(|env| autostart::enable(&env, &mut autostart::system_runner));
            match result {
                Ok(outcome) => log::info!("开机自启已确认：{outcome:?}"),
                Err(why) => log::warn!("开机自启设置失败：{why}"),
            }
        });
    if let Err(e) = spawned {
        log::warn!("无法启动自启设置线程：{e}");
    }
}
