//! 开机自启和进程守护：按平台用系统自己的机制，被杀、崩溃后自动恢复，用户主动退出后不恢复。
//!
//! | 系统 | 机制 | 重启条件 |
//! |---|---|---|
//! | macOS | `~/Library/LaunchAgents/org.peerdesk.client.plist`，`KeepAlive` 的 `SuccessfulExit=false` | 退出码非 0 或被信号杀 |
//! | Linux | systemd 用户单元 `peerdesk.service`，`Restart=on-failure` | 同上（`on-failure` 包含信号和非 0） |
//! | Linux 没有 systemd 用户实例 | XDG 自启动项，运行 `peerdesk guard` | 同上，由 [`crate::guard`] 实现 |
//! | Windows | 当前用户的 `Run` 注册表项，运行 `peerdesk guard`（不需要管理员） | 同上，由 [`crate::guard`] 实现 |
//!
//! 为什么 Windows 不用计划任务、服务：服务在 Session 0 里没有桌面，托盘图标和窗口都出不来
//! （锁屏和登录界面的远程是 `unattended` 里那个 SYSTEM 服务的事，两件事分开）；计划任务的“失败后重启”
//! 是否包含进程崩溃的退出码没有核实，用同一个二进制里的守护模式行为是确定的，也不新增第三个二进制。
//!
//! 用户从托盘点“退出”是退出码 0，三种机制都不会拉起；下次登录才会再启动。
//! 只写用户目录和当前用户的注册表，不需要管理员权限。写文件和运行命令都在这里，
//! 命令的执行用注入的函数，测试里不碰真实系统。

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{EXIT_FAILED, EXIT_NO_RETRY, EXIT_OK, EXIT_USAGE};

/// macOS 的启动项标识，同时是 plist 的文件名。和 `Info.plist` 的 `CFBundleIdentifier` 一致。
pub const LABEL: &str = "org.peerdesk.client";
/// systemd 用户单元名。
pub const UNIT: &str = "peerdesk.service";
/// Windows 的 `Run` 项名字。
pub const RUN_VALUE: &str = "PeerDesk";
/// 不是真的程序名：[`system_runner`] 看到它就直接调注册表接口，不启动任何外部程序。
/// 为什么不用 `reg.exe`：它是控制台程序，从没有控制台的图形界面进程里启动，系统会新开一个控制台窗口，
/// 用户看到启动时“闪了一下”。命令的形式（`@registry set 名字 命令行`、`@registry delete 名字`）让测试仍能检查要写什么。
pub const REGISTRY_PROGRAM: &str = "@registry";
/// 用户在设置里关掉开机自启后留下的标记（在数据目录里）：之后启动不再自动装回来。
pub const OFF_FILE: &str = "autostart-off";

/// 系统种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// macOS。
    MacOs,
    /// Linux。
    Linux,
    /// Windows。
    Windows,
}

impl Kind {
    /// 当前系统。
    #[must_use]
    pub const fn current() -> Self {
        #[cfg(target_os = "macos")]
        return Self::MacOs;
        #[cfg(windows)]
        return Self::Windows;
        #[cfg(all(not(windows), not(target_os = "macos")))]
        return Self::Linux;
    }
}

/// 要写文件、运行命令的环境。
#[derive(Debug, Clone)]
pub struct Env {
    /// 系统。
    pub kind: Kind,
    /// 本程序的完整路径。
    pub exe: PathBuf,
    /// 用户主目录。
    pub home: PathBuf,
    /// 用户配置目录（Linux 的 `XDG_CONFIG_HOME`，默认 `~/.config`）。
    pub config: PathBuf,
}

impl Env {
    /// 当前环境。找不到自己的路径或主目录时返回原因：不能往不明确的位置写自启项。
    pub fn detect() -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| format!("找不到程序自己的路径：{e}"))?;
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .ok_or("找不到用户主目录")?;
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map_or_else(|| home.join(".config"), PathBuf::from);
        Ok(Self {
            kind: Kind::current(),
            exe,
            home,
            config,
        })
    }

    fn plist(&self) -> PathBuf {
        self.home
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    fn unit(&self) -> PathBuf {
        self.config.join("systemd/user").join(UNIT)
    }

    fn desktop(&self) -> PathBuf {
        self.config.join("autostart/peerdesk.desktop")
    }
}

/// 开启的结果：用了哪种机制（界面据此说明）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 系统的守护机制（launchd、systemd）。
    System,
    /// 系统没有可用的守护机制，用了 `peerdesk guard`（Linux 没有 systemd 用户实例）。
    Fallback,
    /// Windows：`Run` 项加 `peerdesk guard`。
    Guard,
}

/// 一条系统命令：程序名和参数。
pub type Command = Vec<String>;
/// 执行命令：返回（成功与否，输出）；启动不了返回原因。
pub type Runner<'a> = &'a mut dyn FnMut(&Command) -> Result<(bool, String), String>;

fn cmd(parts: &[&str]) -> Command {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// macOS 启动项文件的内容。
#[must_use]
pub fn launch_agent(exe: &Path) -> String {
    let exe = xml_escape(&exe.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<!-- 只在非正常退出（非 0 退出码、崩溃、被杀）时重启；从菜单栏“退出”是退出码 0，不重启。 -->
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<!-- 两次启动至少隔 10 秒，崩溃循环时不会把 CPU 吃满。 -->
	<key>ThrottleInterval</key>
	<integer>10</integer>
	<!-- 只在用户的图形会话里运行（要窗口和菜单栏图标）。 -->
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#
    )
}

/// systemd 用户单元的内容。
#[must_use]
pub fn systemd_unit(exe: &Path) -> String {
    let exe = systemd_quote(&exe.display().to_string());
    format!(
        "[Unit]\n\
         Description=PeerDesk（被控端常驻，托盘图标）\n\
         After=graphical-session.target\n\
         PartOf=graphical-session.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exe}\n\
         # 退出码 0（从托盘退出）不重启；非 0、崩溃、被杀都重启。\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # 重试也没用的错误（缺系统库等，见 EXIT_NO_RETRY）不重启，原因在日志里。\n\
         RestartPreventExitStatus={EXIT_NO_RETRY}\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n"
    )
}

/// XDG 自启动项（没有 systemd 用户实例时用，运行守护模式）。
#[must_use]
pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=PeerDesk\n\
         Exec={} guard\n\
         Icon=peerdesk\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        desktop_quote(&exe.display().to_string())
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// systemd：双引号包住，里面的 `\`、`"` 转义，`%` 写成 `%%`（否则被当成说明符）。
fn systemd_quote(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

/// 桌面项规范：双引号包住，里面的 `"`、`` ` ``、`$`、`\` 前面加反斜杠。
fn desktop_quote(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Windows `Run` 项要运行的命令行。
#[must_use]
pub fn run_command_line(exe: &Path) -> String {
    format!("\"{}\" guard", exe.display())
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    let parent = path.parent().ok_or("路径没有上级目录")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}：{e}", parent.display()))?;
    std::fs::write(path, text).map_err(|e| format!("{}：{e}", path.display()))
}

fn remove_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}：{e}", path.display())),
    }
}

/// 要成功的命令：失败带上输出作为原因。
fn must(run: Runner<'_>, command: &Command) -> Result<(), String> {
    let (ok, text) = run(command)?;
    if ok {
        Ok(())
    } else {
        Err(format!("{}：{}", command.join(" "), text.trim()))
    }
}

/// 开启开机自启和进程守护。可以重复调用（每次启动都调用一次，程序挪了位置、升级后路径变了会跟着更新）。
pub fn enable(env: &Env, run: Runner<'_>) -> Result<Outcome, String> {
    match env.kind {
        Kind::MacOs => {
            // 不运行 launchctl：launchd 在下次登录时自动加载这个目录里的项目。现在就 bootstrap 会再起一个实例，
            // 它发现已有实例就退出，等于什么也没做。
            write_file(&env.plist(), &launch_agent(&env.exe))?;
            Ok(Outcome::System)
        }
        Kind::Linux => {
            write_file(&env.unit(), &systemd_unit(&env.exe))?;
            let systemd = must(run, &cmd(&["systemctl", "--user", "daemon-reload"]))
                .and_then(|()| must(run, &cmd(&["systemctl", "--user", "enable", UNIT])));
            match systemd {
                Ok(()) => {
                    remove_file(&env.desktop())?;
                    Ok(Outcome::System)
                }
                Err(why) => {
                    // 没有 systemd 用户实例（精简系统、容器、非 systemd 发行版）：不留一个不起作用的单元，改用守护模式。
                    log::warn!("systemd 用户单元用不了，改用守护模式：{why}");
                    remove_file(&env.unit())?;
                    write_file(&env.desktop(), &desktop_entry(&env.exe))?;
                    Ok(Outcome::Fallback)
                }
            }
        }
        Kind::Windows => {
            must(
                run,
                &cmd(&[
                    REGISTRY_PROGRAM,
                    "set",
                    RUN_VALUE,
                    &run_command_line(&env.exe),
                ]),
            )?;
            Ok(Outcome::Guard)
        }
    }
}

/// 关闭开机自启和进程守护。当前正在运行的进程不受影响（不去杀它）。
pub fn disable(env: &Env, run: Runner<'_>) -> Result<(), String> {
    match env.kind {
        Kind::MacOs => remove_file(&env.plist()),
        Kind::Linux => {
            // disable 在没有单元、没有 systemd 时会失败，这都是“本来就没开”，不算错
            let _ = run(&cmd(&["systemctl", "--user", "disable", UNIT]));
            remove_file(&env.unit())?;
            remove_file(&env.desktop())?;
            let _ = run(&cmd(&["systemctl", "--user", "daemon-reload"]));
            Ok(())
        }
        Kind::Windows => {
            // 项不存在不算错（注册表接口对不存在的值返回成功）；真的删不掉才报错
            must(run, &cmd(&[REGISTRY_PROGRAM, "delete", RUN_VALUE]))?;
            Ok(())
        }
    }
}

/// 真的运行一条系统命令。
pub fn system_runner(command: &Command) -> Result<(bool, String), String> {
    let (program, args) = command.split_first().ok_or("空命令")?;
    if program == REGISTRY_PROGRAM {
        return registry(args).map(|()| (true, String::new()));
    }
    let output = crate::process::quiet(std::process::Command::new(program).args(args))
        .output()
        .map_err(|e| format!("运行 {program} 失败：{e}"))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    Ok((output.status.success(), text))
}

/// 执行 [`REGISTRY_PROGRAM`] 命令：只有 Windows 有注册表。
#[cfg(windows)]
fn registry(args: &[String]) -> Result<(), String> {
    use peerdesk_platform::winshell;
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["set", name, value] => winshell::set_run_value(name, value),
        ["delete", name] => winshell::delete_run_value(name),
        _ => Err(format!("看不懂的注册表命令：{args:?}")),
    }
}

#[cfg(not(windows))]
fn registry(args: &[String]) -> Result<(), String> {
    Err(format!("这个系统没有注册表：{args:?}"))
}

/// 用户是否关掉了开机自启（数据目录里有标记）。
#[must_use]
pub fn user_disabled(data_dir: &Path) -> bool {
    data_dir.join(OFF_FILE).exists()
}

/// 记住用户的选择：`on` 为假时写标记，为真时删掉。
pub fn remember_choice(data_dir: &Path, on: bool) -> std::io::Result<()> {
    let marker = data_dir.join(OFF_FILE);
    if on {
        match std::fs::remove_file(&marker) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            Ok(()) | Err(_) => Ok(()),
        }
    } else {
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(marker, b"")
    }
}

/// `peerdesk autostart on|off`（安装包和排查用）。
pub fn cmd_autostart(args: &[String], out: &mut dyn Write) -> u8 {
    autostart_with(
        args,
        out,
        Env::detect(),
        &mut system_runner,
        &crate::data_dir(),
    )
}

fn autostart_with(
    args: &[String],
    out: &mut dyn Write,
    env: Result<Env, String>,
    run: Runner<'_>,
    data_dir: &Path,
) -> u8 {
    let on = match args.first().map(String::as_str) {
        Some("on") => true,
        Some("off") => false,
        Some(_) | None => {
            let _ = writeln!(out, "用法：peerdesk autostart on|off");
            return EXIT_USAGE;
        }
    };
    let env = match env {
        Ok(env) => env,
        Err(why) => {
            let _ = writeln!(out, "错误：{why}");
            return EXIT_FAILED;
        }
    };
    let result = if on {
        enable(&env, run).map(|outcome| match outcome {
            Outcome::System => "已开启：开机自启，被杀或崩溃后由系统自动恢复",
            Outcome::Fallback => "已开启：系统的服务管理器用不了，改用 PeerDesk 自带的守护进程",
            Outcome::Guard => "已开启：登录后由 PeerDesk 守护进程启动，被杀或崩溃后自动恢复",
        })
    } else {
        disable(&env, run).map(|()| "已关闭开机自启和自动恢复")
    };
    match result {
        Ok(text) => {
            let _ = writeln!(out, "{text}");
            let _ = remember_choice(data_dir, on);
            EXIT_OK
        }
        Err(why) => {
            let _ = writeln!(out, "错误：{why}");
            EXIT_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn env(kind: Kind, name: &str) -> Env {
        let home =
            std::env::temp_dir().join(format!("peerdesk-auto-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        Env {
            kind,
            exe: PathBuf::from("/Applications/PeerDesk.app/Contents/MacOS/peerdesk"),
            config: home.join(".config"),
            home,
        }
    }

    #[test]
    fn launch_agent_restarts_only_on_unsuccessful_exit() {
        let plist = launch_agent(Path::new("/Apps/Peer&Desk <1>/peerdesk"));
        assert!(plist.contains("<string>org.peerdesk.client</string>"));
        assert!(plist.contains("<string>/Apps/Peer&amp;Desk &lt;1&gt;/peerdesk</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
    }

    #[test]
    fn systemd_unit_restarts_on_failure_and_not_after_a_clean_quit() {
        let unit = systemd_unit(Path::new("/opt/Peer Desk/100%/peerdesk"));
        assert!(unit.contains("ExecStart=\"/opt/Peer Desk/100%%/peerdesk\"\n"));
        assert!(unit.contains("Restart=on-failure\n"));
        assert!(unit.contains("RestartPreventExitStatus=78\n"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
        assert_eq!(EXIT_NO_RETRY, 78, "单元里的 78 要和退出码一致");
        assert_eq!(systemd_quote(r#"a"b\c"#), r#""a\"b\\c""#);
    }

    #[test]
    fn desktop_entry_and_run_key_use_the_guard_mode() {
        let entry = desktop_entry(Path::new("/home/a b/$x/peerdesk"));
        assert!(entry.contains("Exec=\"/home/a b/\\$x/peerdesk\" guard\n"));
        assert_eq!(
            run_command_line(Path::new(r"C:\Program Files\PeerDesk\peerdesk.exe")),
            r#""C:\Program Files\PeerDesk\peerdesk.exe" guard"#
        );
        assert_eq!(desktop_quote("a`b\"c"), "\"a\\`b\\\"c\"");
    }

    #[test]
    fn macos_writes_the_plist_without_running_anything() {
        let env = env(Kind::MacOs, "mac");
        let outcome = enable(&env, &mut |_| panic!("macOS 不运行命令")).unwrap();
        assert_eq!(outcome, Outcome::System);
        let path = env
            .home
            .join("Library/LaunchAgents/org.peerdesk.client.plist");
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("KeepAlive"));
        disable(&env, &mut |_| Ok((true, String::new()))).unwrap();
        assert!(!path.exists());
        disable(&env, &mut |_| Ok((true, String::new()))).unwrap();
        let _ = std::fs::remove_dir_all(&env.home);
    }

    #[test]
    fn linux_uses_systemd_when_it_works() {
        let env = env(Kind::Linux, "sd");
        // 先留下一个旧的守护模式自启项，开启 systemd 后要清掉，否则开机会起两份
        write_file(&env.desktop(), "old").unwrap();
        let mut seen = Vec::new();
        let outcome = enable(&env, &mut |c| {
            seen.push(c.join(" "));
            Ok((true, String::new()))
        })
        .unwrap();
        assert_eq!(outcome, Outcome::System);
        assert_eq!(
            seen,
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable peerdesk.service"
            ]
        );
        assert!(env.unit().exists() && !env.desktop().exists());
        disable(&env, &mut |_| Ok((false, "没有这个单元".into()))).unwrap();
        assert!(!env.unit().exists());
        let _ = std::fs::remove_dir_all(&env.home);
    }

    #[test]
    fn linux_without_a_systemd_user_instance_falls_back_to_guard_mode() {
        let env = env(Kind::Linux, "nosd");
        let outcome = enable(&env, &mut |_| {
            Ok((false, "Failed to connect to bus".to_owned()))
        })
        .unwrap();
        assert_eq!(outcome, Outcome::Fallback);
        assert!(!env.unit().exists(), "不留下不起作用的单元");
        assert!(std::fs::read_to_string(env.desktop())
            .unwrap()
            .contains(" guard\n"));
        disable(&env, &mut |_| Err("没有 systemctl".into())).unwrap();
        assert!(!env.desktop().exists());
        // systemctl 都启动不了（没装）也是同样的结果
        let outcome = enable(&env, &mut |_| Err("没有 systemctl".to_owned())).unwrap();
        assert_eq!(outcome, Outcome::Fallback);
        let _ = std::fs::remove_dir_all(&env.home);
    }

    #[test]
    fn windows_writes_the_registry_directly_and_never_starts_a_program() {
        let env = env(Kind::Windows, "win");
        let mut seen = Vec::new();
        let outcome = enable(&env, &mut |c| {
            seen.push(c.clone());
            Ok((true, String::new()))
        })
        .unwrap();
        assert_eq!(outcome, Outcome::Guard);
        // 只有一条命令，而且是注册表的伪命令：不是 reg.exe（它会让启动时闪一个控制台窗口）
        assert_eq!(seen.len(), 1);
        let set = seen.first().unwrap();
        assert_eq!(set[..3], [REGISTRY_PROGRAM, "set", RUN_VALUE]);
        assert_eq!(set.get(3), Some(&run_command_line(&env.exe)));
        let failed = enable(&env, &mut |_| Ok((false, "拒绝访问".to_owned())));
        assert!(failed.unwrap_err().contains("拒绝访问"));
        let mut deleted = Vec::new();
        disable(&env, &mut |c| {
            deleted.push(c.clone());
            Ok((true, String::new()))
        })
        .unwrap();
        assert_eq!(deleted, [cmd(&[REGISTRY_PROGRAM, "delete", RUN_VALUE])]);
        assert!(disable(&env, &mut |_| Ok((false, "拒绝访问".to_owned()))).is_err());
    }

    #[test]
    fn registry_commands_only_work_on_windows() {
        let set = cmd(&[REGISTRY_PROGRAM, "set", "PeerDesk-test-delete-me", "y"]);
        let result = system_runner(&set);
        if cfg!(windows) {
            // 真的写了当前用户的注册表：清理掉
            assert_eq!(result, Ok((true, String::new())));
            let del = cmd(&[REGISTRY_PROGRAM, "delete", "PeerDesk-test-delete-me"]);
            assert_eq!(system_runner(&del), Ok((true, String::new())));
            assert_eq!(
                system_runner(&del),
                Ok((true, String::new())),
                "值不存在不算错"
            );
            assert!(system_runner(&cmd(&[REGISTRY_PROGRAM, "bogus"])).is_err());
        } else {
            assert!(result.unwrap_err().contains("没有注册表"));
        }
    }

    #[test]
    fn user_choice_is_remembered_in_a_marker_file() {
        let dir = std::env::temp_dir().join(format!("peerdesk-auto-mark-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!user_disabled(&dir));
        remember_choice(&dir, true).unwrap();
        remember_choice(&dir, false).unwrap();
        assert!(user_disabled(&dir));
        remember_choice(&dir, true).unwrap();
        assert!(!user_disabled(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn real_runner_and_detection() {
        assert_eq!(system_runner(&Vec::new()), Err("空命令".to_owned()));
        assert!(system_runner(&cmd(&["peerdesk-no-such-program"])).is_err());
        #[cfg(unix)]
        {
            assert_eq!(
                system_runner(&cmd(&["sh", "-c", "echo hi"])),
                Ok((true, "hi\n".into()))
            );
            assert!(!system_runner(&cmd(&["sh", "-c", "exit 3"])).unwrap().0);
        }
        let env = Env::detect().unwrap();
        assert!(env.exe.is_absolute());
        let _ = Kind::current();
    }

    #[test]
    fn command_line_turns_it_on_and_off_and_reports_failures() {
        let mac = env(Kind::MacOs, "cli");
        let dir = mac.home.join("data");
        let say = |args: &[&str], env: Result<Env, String>, run: Runner<'_>| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            let mut out = Vec::new();
            let code = autostart_with(&args, &mut out, env, run, &dir);
            (code, String::from_utf8(out).unwrap())
        };
        let ok = &mut |_: &Command| Ok((true, String::new()));
        let (code, text) = say(&["on"], Ok(mac.clone()), ok);
        assert_eq!((code, text.contains("已开启")), (EXIT_OK, true));
        assert!(!user_disabled(&dir));
        let (code, text) = say(&["off"], Ok(mac.clone()), ok);
        assert_eq!((code, text.contains("已关闭")), (EXIT_OK, true));
        assert!(user_disabled(&dir));
        let (code, text) = say(&["on"], Err("没有主目录".into()), ok);
        assert_eq!((code, text.contains("没有主目录")), (EXIT_FAILED, true));
        let win = Env {
            kind: Kind::Windows,
            ..mac.clone()
        };
        let (code, text) = say(&["on"], Ok(win.clone()), &mut |_| {
            Ok((false, "拒绝访问".into()))
        });
        assert_eq!((code, text.contains("拒绝访问")), (EXIT_FAILED, true));
        let (_, text) = say(&["on"], Ok(win), ok);
        assert!(text.contains("守护进程"));
        let linux = env(Kind::Linux, "cli-linux");
        let (_, text) = say(&["on"], Ok(linux.clone()), &mut |_| {
            Ok((false, String::new()))
        });
        assert!(text.contains("服务管理器用不了"));
        let (_, text) = say(&["on"], Ok(linux.clone()), ok);
        assert!(text.contains("由系统自动恢复"));
        let _ = std::fs::remove_dir_all(&mac.home);
        let _ = std::fs::remove_dir_all(&linux.home);
    }

    #[test]
    fn removing_a_directory_as_a_file_is_an_error() {
        let dir = std::env::temp_dir().join(format!("peerdesk-auto-rmdir-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(remove_file(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn command_line_usage() {
        let mut out = Vec::new();
        assert_eq!(cmd_autostart(&[], &mut out), EXIT_USAGE);
        assert_eq!(cmd_autostart(&["maybe".to_owned()], &mut out), EXIT_USAGE);
        assert!(String::from_utf8(out)
            .unwrap()
            .contains("peerdesk autostart on|off"));
    }
}
