//! 无人值守（Windows 系统服务）：锁屏、登录界面、UAC 提示时也能远程。
//!
//! 结构（设计见 `docs/research/G-unattended.md` 第 1.2–1.4 节）：
//! - `peerdesk service run`：由服务管理器以 SYSTEM 身份启动，在 Session 0 里。它看不到桌面，
//!   只负责监管：每 200 ms 看一次活动控制台会话，在那个会话里拉起 `peerdesk agent`，
//!   会话换了（切换用户、远程登录）就结束旧代理、在新会话重拉；代理退出了就重拉。
//!   看到代理写的 `sas.request` 就删掉并代发 Ctrl+Alt+Del（只有服务调用才有效）。
//! - `peerdesk agent`：SYSTEM 身份、在用户的会话里，采集和注入线程跟着“当前输入桌面”走，
//!   所以锁屏和 UAC 安全桌面也能看到、能操作。被控逻辑和图形界面的被控完全一样。
//! - `peerdesk service install|uninstall`：管理员运行（安装包调用），用系统自带的 `sc`、`reg`、
//!   `icacls` 注册服务、允许服务模拟 SAS、把代理的数据目录收紧到只有 SYSTEM 和管理员能读
//!   （里面有设备身份和无人值守密码）。
//!
//! 状态写在 `%ProgramData%\PeerDesk\status.txt`（普通用户可读，不含密码），界面靠它显示服务的设备码。
//!
//! 系统接口（服务管理器回调、跨会话拉起进程）在平台层的 `winservice.rs`、`winsession.rs` 里；
//! 这里是不依赖系统的逻辑，用假的会话和进程测试。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::gui::session::{start_host, HostConfig, HostEvent};
use crate::gui::settings::ServerSettings;

/// 服务名。
pub const SERVICE: &str = "PeerDesk";
/// 状态文件（在 [`base_dir`] 里）。
pub const STATUS_FILE: &str = "status.txt";
/// 代理请求服务代发 Ctrl+Alt+Del 时创建的文件（在 [`agent_dir`] 里：普通用户写不进去）。
pub const SAS_FILE: &str = "sas.request";
/// 无人值守密码（在 [`agent_dir`] 里，只有 SYSTEM 和管理员能读）。
pub const PASSWORD_FILE: &str = "unattended-password.txt";
/// 无人值守密码最短长度：这台电脑任何时候都能被连，密码要够长。
pub const MIN_PASSWORD: usize = 8;
/// 监管周期。
pub const TICK: Duration = Duration::from_millis(200);

/// 服务和代理共用的目录：`%ProgramData%\PeerDesk`。没有这个环境变量（非 Windows）时
/// 用普通数据目录下的 `service`。
#[must_use]
pub fn base_dir() -> PathBuf {
    base_dir_from(std::env::var_os("ProgramData"), &crate::data_dir())
}

fn base_dir_from(program_data: Option<std::ffi::OsString>, data_dir: &Path) -> PathBuf {
    program_data.filter(|v| !v.is_empty()).map_or_else(
        || data_dir.join("service"),
        |p| PathBuf::from(p).join("PeerDesk"),
    )
}

/// 代理的数据目录（设备身份、服务器设置、密码）。
#[must_use]
pub fn agent_dir(base: &Path) -> PathBuf {
    base.join("agent")
}

/// 一条要运行的系统命令：程序名和参数。
pub type Command = Vec<String>;

fn cmd(parts: &[&str]) -> Command {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// 安装服务要运行的命令（按顺序）。`exe` 是 peerdesk.exe 的完整路径。
#[must_use]
pub fn install_commands(exe: &str, agent: &Path) -> Vec<Command> {
    let bin = format!("\"{exe}\" service run");
    let agent = agent.display().to_string();
    vec![
        // 已经装过时先停掉（失败无所谓），create 会因为已存在而失败，于是改用 config
        cmd(&["sc", "stop", SERVICE]),
        cmd(&[
            "sc",
            "create",
            SERVICE,
            "binPath=",
            &bin,
            "start=",
            "auto",
            "obj=",
            "LocalSystem",
            "DisplayName=",
            "PeerDesk",
        ]),
        cmd(&["sc", "config", SERVICE, "binPath=", &bin, "start=", "auto"]),
        cmd(&[
            "sc",
            "description",
            SERVICE,
            "PeerDesk 远程桌面：锁屏和登录界面也能远程",
        ]),
        // 异常退出后 5 秒重启，一天后清零计数
        cmd(&[
            "sc",
            "failure",
            SERVICE,
            "reset=",
            "86400",
            "actions=",
            "restart/5000/restart/5000/restart/5000",
        ]),
        // 允许服务模拟 Ctrl+Alt+Del（1 = 服务可以，2 = 辅助功能程序，3 = 两者）
        cmd(&[
            "reg",
            "add",
            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System",
            "/v",
            "SoftwareSASGeneration",
            "/t",
            "REG_DWORD",
            "/d",
            "3",
            "/f",
        ]),
        // 代理目录只给 SYSTEM（S-1-5-18）和管理员组（S-1-5-32-544）：用 SID，中文系统上组名不同
        cmd(&[
            "icacls",
            &agent,
            "/inheritance:r",
            "/grant:r",
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
        ]),
        cmd(&["sc", "start", SERVICE]),
    ]
}

/// 卸载服务要运行的命令。代理目录（设备身份）保留，重装后设备码不变。
#[must_use]
pub fn uninstall_commands() -> Vec<Command> {
    vec![
        cmd(&["sc", "stop", SERVICE]),
        cmd(&["sc", "delete", SERVICE]),
    ]
}

/// 安装时哪些命令失败了也没关系。
fn may_fail(command: &[String]) -> bool {
    let words: Vec<&str> = command.iter().map(String::as_str).collect();
    // 第一次装时没有可停的服务；重装时 create 因为已存在而失败（随后 config 更新路径）
    matches!(words.get(..2), Some(["sc", "stop" | "create"]))
}

/// 监管这一步该做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 什么也不做。
    Wait,
    /// 在这个会话里拉起代理。
    Spawn(u32),
    /// 结束当前代理（会话变了或者没人在控制台）。
    Kill,
}

/// 根据活动控制台会话和当前代理（所在会话、是否还在运行）决定下一步。
#[must_use]
pub fn decide(active: Option<u32>, child: Option<(u32, bool)>) -> Action {
    match (active, child) {
        (Some(session), None | Some((_, false))) => Action::Spawn(session),
        (Some(session), Some((at, true))) if at != session => Action::Kill,
        (None, Some((_, true))) => Action::Kill,
        (Some(_), Some((_, true))) | (None, None | Some((_, false))) => Action::Wait,
    }
}

/// 服务用到的系统能力（Windows 上是真的，测试里是假的）。
pub trait System {
    /// 正在运行的代理。
    type Child: Running;
    /// 活动控制台会话。
    fn active_session(&mut self) -> Option<u32>;
    /// 在会话里拉起代理。
    ///
    /// # Errors
    /// 拉不起来时给出原因。
    fn spawn(&mut self, session: u32) -> Result<Self::Child, String>;
    /// 代发 Ctrl+Alt+Del。
    fn send_sas(&mut self);
}

/// 代理进程。
pub trait Running {
    /// 还在运行。
    fn running(&self) -> bool;
    /// 结束。
    fn kill(&self);
}

/// 两次拉起代理之间至少隔这么久（代理一启动就崩时不要每 200 ms 拉一次）。
const RESPAWN_GAP: u32 = 10;

/// 服务的监管循环：直到 `stop` 置位。结束时把代理也结束掉。
pub fn supervise<S: System>(sys: &mut S, base: &Path, stop: &AtomicBool) {
    let mut child: Option<(u32, S::Child)> = None;
    let mut cooldown = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let state = child.as_ref().map(|(s, c)| (*s, c.running()));
        match decide(sys.active_session(), state) {
            Action::Spawn(_) if cooldown > 0 => cooldown -= 1,
            Action::Spawn(session) => {
                cooldown = RESPAWN_GAP;
                child = match sys.spawn(session) {
                    Ok(c) => {
                        log::info!("无人值守：在会话 {session} 里启动了代理");
                        Some((session, c))
                    }
                    Err(e) => {
                        log::warn!("无人值守：在会话 {session} 里启动代理失败：{e}");
                        let _ = write_status(base, &Status::failed(&format!("启动代理失败：{e}")));
                        None
                    }
                };
            }
            Action::Kill => {
                cooldown = 0;
                child.take().into_iter().for_each(|(session, c)| {
                    log::info!("无人值守：会话 {session} 已不是活动会话，结束代理");
                    c.kill();
                });
            }
            Action::Wait => {}
        }
        if std::fs::remove_file(agent_dir(base).join(SAS_FILE)).is_ok() {
            log::info!("无人值守：代发 Ctrl+Alt+Del");
            sys.send_sas();
        }
        std::thread::sleep(TICK);
    }
    if let Some((_, c)) = child {
        c.kill();
    }
}

/// 无人值守的状态，写在 [`STATUS_FILE`] 里给界面看。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// 正在运行（能被连）。
    pub running: bool,
    /// 服务器分配的设备码。
    pub code: Option<u32>,
    /// 说明（出错原因、当前在做什么）。
    pub detail: String,
}

impl Status {
    fn failed(why: &str) -> Self {
        Self {
            running: false,
            code: None,
            detail: why.to_owned(),
        }
    }
}

/// 写状态文件：三行，运行与否、设备码（没有为空）、说明。
///
/// # Errors
/// 写不了文件时。
pub fn write_status(base: &Path, status: &Status) -> std::io::Result<()> {
    std::fs::create_dir_all(base)?;
    let code = status.code.map(|c| c.to_string()).unwrap_or_default();
    let detail = status.detail.replace('\n', " ");
    std::fs::write(
        base.join(STATUS_FILE),
        format!("{}\n{code}\n{detail}\n", u8::from(status.running)),
    )
}

/// 读状态文件；没有装服务（没有文件）时为 `None`。
#[must_use]
pub fn read_status(base: &Path) -> Option<Status> {
    let text = std::fs::read_to_string(base.join(STATUS_FILE)).ok()?;
    let mut lines = text.lines();
    Some(Status {
        running: lines.next() == Some("1"),
        code: lines.next().and_then(|c| c.trim().parse().ok()),
        detail: lines.next().unwrap_or_default().to_owned(),
    })
}

/// 认出 Ctrl+Alt+Delete（USB HID 用途码）。控制端的工具条按钮发的就是这三个键。
#[derive(Debug, Default)]
pub struct SasDetector {
    ctrl: u8,
    alt: u8,
}

impl SasDetector {
    /// 送进一个按键；按下 Delete 时 Ctrl 和 Alt 都按着就返回 true。
    pub fn key(&mut self, code: u16, down: bool) -> bool {
        let bit = |left: u16| u8::from(code == left) | (u8::from(code == left + 4) << 1);
        let set = |held: &mut u8, mask: u8| {
            if down {
                *held |= mask;
            } else {
                *held &= !mask;
            }
        };
        set(&mut self.ctrl, bit(0xE0));
        set(&mut self.alt, bit(0xE2));
        down && code == 0x4C && self.ctrl != 0 && self.alt != 0
    }
}

/// 读无人值守密码；没设或太短时给出原因。
///
/// # Errors
/// 没有设置密码。
pub fn load_password(agent: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(agent.join(PASSWORD_FILE)).unwrap_or_default();
    let password = text.trim().to_owned();
    if password.chars().count() < MIN_PASSWORD {
        return Err(format!(
            "还没有设置无人值守密码（至少 {MIN_PASSWORD} 位）：以管理员身份运行 peerdesk service password 新密码"
        ));
    }
    Ok(password)
}

/// 保存无人值守密码。
///
/// # Errors
/// 太短，或写不了文件。
pub fn save_password(agent: &Path, password: &str) -> Result<(), String> {
    if password.chars().count() < MIN_PASSWORD {
        return Err(format!("密码至少 {MIN_PASSWORD} 位"));
    }
    std::fs::create_dir_all(agent).map_err(|e| format!("建目录失败：{e}"))?;
    std::fs::write(agent.join(PASSWORD_FILE), password)
        .map_err(|e| format!("写密码失败（需要管理员身份）：{e}"))
}

/// 被控端的一个事件对状态的影响。被控停止时返回原因。
fn apply(status: &mut Status, event: HostEvent) -> Result<(), String> {
    match event {
        HostEvent::Listening { port, .. } => {
            status.running = true;
            status.detail = format!("等待连接（端口 {port}）");
        }
        HostEvent::Registered { code, .. } => status.code = Some(code),
        HostEvent::RegisterFailed(e) => status.detail = format!("登记失败：{e}"),
        HostEvent::Established => status.detail = "正在被远程".into(),
        HostEvent::SessionEnded(_) => status.detail = "等待连接".into(),
        HostEvent::Unavailable(why) | HostEvent::Refused(why) => {
            log::warn!("无人值守：{why}");
            status.detail = why;
        }
        HostEvent::Stopped(why) => {
            return Err(why.map_or_else(|| "被控已停止".to_owned(), |e| e.to_string()));
        }
        HostEvent::Network(_) | HostEvent::Peer { .. } | HostEvent::HandshakeFailed(_) => {}
    }
    Ok(())
}

/// 代理：用 `agent` 目录里的身份、服务器设置和密码当被控端，状态写到 `base`。
/// 直到 `stop` 置位或被控停止。返回停止的原因。
pub fn run_agent(base: &Path, agent: &Path, port: u16, stop: &AtomicBool) -> Result<(), String> {
    let report = |status: &Status| {
        if let Err(e) = write_status(base, status) {
            log::warn!("无人值守：写状态失败：{e}");
        }
    };
    let password = load_password(agent).inspect_err(|e| report(&Status::failed(e)))?;
    let server = ServerSettings::load(agent)
        .server(&crate::gui::i18n::ZH)
        .ok()
        .flatten()
        .unwrap_or_else(crate::crashreport::default_server);
    let config = HostConfig {
        port,
        data_dir: agent.to_path_buf(),
        server: Some(server),
        fps: 60,
    };
    let host = start_host(config, &password, Arc::new(|| {}));
    let mut status = Status {
        running: false,
        code: None,
        detail: "正在启动".into(),
    };
    report(&status);
    let mut ticks = 0u32;
    let mut current = password;
    while !stop.load(Ordering::Relaxed) {
        // 管理员改了密码：之后的连接用新密码（每 2 秒看一次）
        ticks = ticks.wrapping_add(1);
        if ticks % 10 == 0 {
            match load_password(agent) {
                Ok(p) if p != current => {
                    log::info!("无人值守：密码已更新");
                    host.set_password(&p);
                    current = p;
                }
                Ok(_) | Err(_) => {}
            }
        }
        for event in host.poll() {
            if let Err(why) = apply(&mut status, event) {
                report(&Status::failed(&why));
                return Err(why);
            }
            report(&status);
        }
        std::thread::sleep(TICK);
    }
    host.stop();
    report(&Status::failed("代理已退出"));
    Ok(())
}

/// 运行一条命令，返回是否成功和输出。
type Runner<'a> = dyn FnMut(&Command) -> Result<(bool, String), String> + 'a;

/// 依次运行命令，输出每条的结果。`runner` 运行一条命令，返回是否成功和输出。
fn run_all(commands: &[Command], out: &mut dyn Write, runner: &mut Runner) -> Result<(), String> {
    for command in commands {
        let (ok, text) = runner(command)?;
        let _ = writeln!(out, "{} {}", if ok { "✓" } else { "✗" }, command.join(" "));
        if !ok && !may_fail(command) {
            return Err(format!("{} 失败：{}", command.join(" "), text.trim()));
        }
    }
    Ok(())
}

/// 真的运行一条系统命令。
fn system_runner(command: &Command) -> Result<(bool, String), String> {
    let (program, args) = command.split_first().ok_or("空命令")?;
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("运行 {program} 失败：{e}"))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    Ok((output.status.success(), text))
}

/// `peerdesk service ...`。
pub fn cmd_service(args: &[String], out: &mut dyn Write) -> Result<u8, String> {
    service_with(
        args,
        out,
        &base_dir(),
        &crate::data_dir(),
        &mut system_runner,
    )
}

fn service_with(
    args: &[String],
    out: &mut dyn Write,
    base: &Path,
    user_dir: &Path,
    runner: &mut Runner<'_>,
) -> Result<u8, String> {
    let agent = agent_dir(base);
    match args.first().map(String::as_str) {
        Some("install") => {
            std::fs::create_dir_all(&agent)
                .map_err(|e| format!("建目录失败（需要管理员身份）：{e}"))?;
            // 沿用安装者的服务器设置（没有时代理用默认服务器）
            let theirs = ServerSettings::load(user_dir);
            if theirs.is_set() && !ServerSettings::load(&agent).is_set() {
                theirs
                    .save(&agent)
                    .map_err(|e| format!("保存服务器设置失败：{e}"))?;
            }
            let exe = std::env::current_exe().map_err(|e| format!("找不到本程序：{e}"))?;
            run_all(
                &install_commands(&exe.display().to_string(), &agent),
                out,
                runner,
            )?;
            if let Err(e) = load_password(&agent) {
                let _ = writeln!(out, "注意：{e}");
            }
            Ok(crate::EXIT_OK)
        }
        Some("uninstall") => {
            run_all(&uninstall_commands(), out, runner)?;
            let _ = std::fs::remove_file(base.join(STATUS_FILE));
            Ok(crate::EXIT_OK)
        }
        Some("password") => {
            let password = args
                .get(1)
                .ok_or("用法：peerdesk service password 新密码")?;
            save_password(&agent, password)?;
            let _ = writeln!(out, "已保存，几秒后生效。");
            Ok(crate::EXIT_OK)
        }
        Some("status") => {
            match read_status(base) {
                Some(s) => {
                    let code = s
                        .code
                        .map_or_else(|| "（没有）".into(), peerdesk_core::signal::format_code);
                    let _ = writeln!(
                        out,
                        "{}  设备码 {code}  {}",
                        if s.running { "运行中" } else { "未运行" },
                        s.detail
                    );
                }
                None => {
                    let _ = writeln!(out, "没有安装无人值守服务");
                }
            }
            Ok(crate::EXIT_OK)
        }
        Some("run") => run_service(base),
        _ => Err("用法：peerdesk service install|uninstall|password 新密码|status".into()),
    }
}

#[cfg(windows)]
fn run_service(base: &Path) -> Result<u8, String> {
    let base = base.to_path_buf();
    peerdesk_platform::winservice::run(SERVICE, move |stop| {
        let mut sys = WindowsSystem;
        supervise(&mut sys, &base, stop);
    })?;
    Ok(crate::EXIT_OK)
}

#[cfg(not(windows))]
fn run_service(_: &Path) -> Result<u8, String> {
    Err("系统服务目前只支持 Windows".into())
}

/// Windows 上真实的会话和进程。
#[cfg(windows)]
struct WindowsSystem;

#[cfg(windows)]
impl Running for peerdesk_platform::winsession::Child {
    fn running(&self) -> bool {
        Self::running(self)
    }
    fn kill(&self) {
        Self::kill(self);
    }
}

#[cfg(windows)]
impl System for WindowsSystem {
    type Child = peerdesk_platform::winsession::Child;
    fn active_session(&mut self) -> Option<u32> {
        peerdesk_platform::winsession::active_console_session()
    }
    fn spawn(&mut self, session: u32) -> Result<Self::Child, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        peerdesk_platform::winsession::spawn_in_session(
            session,
            &format!("\"{}\" agent", exe.display()),
        )
    }
    fn send_sas(&mut self) {
        peerdesk_platform::winsession::send_sas();
    }
}

/// 代理的端口：和图形界面一样。服务在跑时图形界面不再自己当被控端，不会冲突。
const AGENT_PORT: u16 = crate::gui::form::DEFAULT_PORT;

/// `peerdesk agent`：由服务在用户会话里拉起。
pub fn cmd_agent(args: &[String], _: &mut dyn Write) -> Result<u8, String> {
    let base = match args {
        [] => base_dir(),
        [flag, dir] if flag == "--dir" => PathBuf::from(dir),
        _ => return Err("用法：peerdesk agent [--dir 目录]（由无人值守服务启动）".into()),
    };
    #[cfg(windows)]
    peerdesk_platform::winsession::set_follow_input_desktop(true);
    crate::local::set_sas_hook(sas_requester(agent_dir(&base).join(SAS_FILE)));
    let stop = AtomicBool::new(false);
    run_agent(&base, &agent_dir(&base), AGENT_PORT, &stop).map(|()| crate::EXIT_OK)
}

/// 收到 Ctrl+Alt+Del 时在代理目录里写请求文件，服务看到后代发。
fn sas_requester(file: PathBuf) -> Box<dyn Fn() + Send + Sync> {
    Box::new(move || {
        if let Err(e) = std::fs::write(&file, b"") {
            log::warn!("无人值守：请求 Ctrl+Alt+Del 失败：{e}");
        }
    })
}

#[cfg(test)]
mod tests;
