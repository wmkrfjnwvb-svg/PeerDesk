//! 图形界面常驻后台的实机回归测试：在 Windows、macOS 的真实桌面上启动编译出的 `peerdesk`，
//! 验证产品负责人真机试用时发现的几处问题不再出现（见各测试的说明）。
//!
//! 都标了 `#[ignore]`：只在 CI 的“platform live”任务里跑（提交信息带 `[win]`、`[mac]` 或 `[full]`），
//! 命令是 `cargo test -p peerdesk --test gui_live -- --ignored --test-threads 1 --nocapture`。
//! 只发窗口消息、请求文件和启动进程，不碰键鼠（CLAUDE.md：测试构建里系统输入注入一律不打开）。
//! 托盘图标本身的点击在 CI 里点不了：托盘单击、双击、菜单“打开”、第二次启动、`peerdesk window ...` 请求
//! 都汇合到 `tray::wake` 这一个函数，测试通过其中的请求文件和第二次启动两条入口走到它；
//! 点击到命令的映射在 `tray_logic` 的单元测试里。
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    dead_code
)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_peerdesk");

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-live-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 数据目录里的日志。
fn log_text(data: &Path) -> String {
    std::fs::read_to_string(data.join("logs").join("peerdesk.log")).unwrap_or_default()
}

/// 轮询直到条件成立，超时就带着日志失败。
fn wait_until(
    what: &str,
    data: &Path,
    timeout: Duration,
    mut ready: impl FnMut() -> bool,
) -> Duration {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if ready() {
            return start.elapsed();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!(
        "{timeout:?} 内没有等到：{what}\n--- 日志 ---\n{}",
        log_text(data)
    );
}

/// 测试里启动的进程：失败时也要收拾掉，不留下界面进程。
struct Guard(Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn peerdesk(data: &Path) -> Command {
    let mut command = Command::new(BIN);
    let _ = command
        .env("PEERDESK_DATA_DIR", data)
        .env("PEERDESK_LANG", "en")
        // 开发构建也真的写开机启动项：要验证启动时不起外部程序
        .env("PEERDESK_AUTOSTART", "force");
    command
}

/// 已经提示过“还在后台运行”：第一次关闭不弹提示框，直接隐藏。
fn mark_hint_seen(data: &Path) {
    std::fs::write(data.join("tray-hint-shown"), b"").unwrap();
}

#[cfg(windows)]
mod windows_tests {
    use super::*;
    use peerdesk_platform::winshell::{self, WindowInfo};

    /// 启动后不能出现的外部程序：它们（或它们带出的 conhost）都会让用户看到闪一下的控制台窗口。
    const FORBIDDEN: [&str; 5] = [
        "reg.exe",
        "powershell.exe",
        "pwsh.exe",
        "cmd.exe",
        "conhost.exe",
    ];

    /// 主窗口：进程自己的、有标题的顶层窗口里最大的一个（托盘、winit 的隐藏辅助窗口没有标题）。
    fn main_window(pid: u32) -> Option<WindowInfo> {
        winshell::top_windows_of_process(pid)
            .into_iter()
            .find(|w| !w.title.is_empty() && w.title.contains("PeerDesk"))
    }

    fn visible_on_taskbar(pid: u32) -> bool {
        main_window(pid).is_some_and(|w| w.visible && !w.tool_window)
    }

    fn hidden(pid: u32) -> bool {
        main_window(pid).is_some_and(|w| !w.visible)
    }

    fn run_registry_value() -> Option<String> {
        let out = Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "PeerDesk",
            ])
            .output()
            .unwrap();
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// 真机反馈 1：启动时客户端背后“闪了一下，像打开了什么”（启动后台确认自启时运行 reg.exe）。
    /// 启动后到窗口出现、再多等 3 秒，每 5 毫秒看一次本进程有没有子进程；同时验证自启项真的写进了注册表。
    #[test]
    #[ignore = "需要 Windows 桌面，只在 [win] 的实机任务里跑"]
    fn startup_starts_no_external_programs_and_still_registers_autostart() {
        let data = temp_dir("win-startup");
        mark_hint_seen(&data);
        let child = peerdesk(&data).spawn().unwrap();
        let pid = child.id();
        let _guard = Guard(child);
        let mut seen: Vec<String> = Vec::new();
        let begin = Instant::now();
        let mut shown_at = None;
        while begin.elapsed() < Duration::from_secs(40) {
            seen.extend(winshell::child_process_names(pid));
            if shown_at.is_none() && visible_on_taskbar(pid) {
                shown_at = Some(Instant::now());
            }
            if shown_at.is_some_and(|t| t.elapsed() > Duration::from_secs(3)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(shown_at.is_some(), "窗口没有出现\n{}", log_text(&data));
        for name in &seen {
            assert!(
                !FORBIDDEN.iter().any(|f| name.eq_ignore_ascii_case(f)),
                "启动时运行了外部程序 {name}（用户会看到闪一下）：{seen:?}"
            );
        }
        // 自启项是用注册表接口写的
        let value = run_registry_value();
        assert!(
            value.is_some_and(|v| v.contains("guard")),
            "注册表里没有开机启动项"
        );
        let off = peerdesk(&data).args(["autostart", "off"]).status().unwrap();
        assert!(off.success());
        assert!(run_registry_value().is_none(), "关闭自启后注册表项还在");
    }

    /// 真机反馈 2、3、4：关闭窗口后，单击托盘图标/双击/菜单“打开”（同一个函数 `tray::wake`，
    /// 这里用第二次启动和 `peerdesk window show` 走到它）窗口不出现、任务栏也没有；“退出”要等几秒。
    /// 流程：向窗口发 `WM_CLOSE`（和点关闭按钮一样）→ 隐藏、进程还在 → 第二次启动 → 窗口可见、在任务栏、
    /// 重复一遍（验证第二次还能隐藏，winit 记的可见状态没有失步）→ `window show` → `window quit` 1 秒内以 0 退出。
    #[test]
    #[ignore = "需要 Windows 桌面，只在 [win] 的实机任务里跑"]
    fn close_hides_then_show_requests_bring_it_back_and_quit_is_immediate() {
        let data = temp_dir("win-lifecycle");
        mark_hint_seen(&data);
        let child = peerdesk(&data).spawn().unwrap();
        let pid = child.id();
        let mut guard = Guard(child);
        wait_until("主窗口出现", &data, Duration::from_secs(40), || {
            visible_on_taskbar(pid)
        });
        wait_until(
            "后台常驻就绪（托盘已建立）",
            &data,
            Duration::from_secs(10),
            || log_text(&data).contains("托盘已建立"),
        );
        for round in 1..=2 {
            let window = main_window(pid).unwrap();
            assert!(winshell::post_close(window.hwnd));
            wait_until(
                &format!("第 {round} 次关闭后窗口隐藏"),
                &data,
                Duration::from_secs(5),
                || hidden(pid),
            );
            assert!(
                guard.0.try_wait().unwrap().is_none(),
                "关闭窗口后进程不该退出"
            );

            // 第二次启动（单实例）：自己以 0 退出，已在运行的那个把窗口显示出来
            let second = peerdesk(&data).status().unwrap();
            assert_eq!(second.code(), Some(0), "第二次启动应以退出码 0 退出");
            let took = wait_until(
                &format!("第 {round} 次第二次启动后窗口可见且在任务栏"),
                &data,
                Duration::from_secs(5),
                || visible_on_taskbar(pid),
            );
            println!("第二次启动后 {took:?} 窗口出现");
            assert_eq!(
                main_window(pid).unwrap().hwnd,
                window.hwnd,
                "应是同一个窗口"
            );
        }
        // `peerdesk window show`：和托盘“打开”同一条路
        let window = main_window(pid).unwrap();
        assert!(winshell::post_close(window.hwnd));
        wait_until("再次隐藏", &data, Duration::from_secs(5), || {
            hidden(pid)
        });
        assert!(peerdesk(&data)
            .args(["window", "show"])
            .status()
            .unwrap()
            .success());
        wait_until(
            "window show 后可见",
            &data,
            Duration::from_secs(5),
            || visible_on_taskbar(pid),
        );

        // 窗口隐藏时退出也要立即生效（真机上曾靠 5 秒看门狗）
        assert!(winshell::post_close(main_window(pid).unwrap().hwnd));
        wait_until("退出前先隐藏", &data, Duration::from_secs(5), || {
            hidden(pid)
        });
        assert!(peerdesk(&data)
            .args(["window", "quit"])
            .status()
            .unwrap()
            .success());
        let asked = Instant::now();
        let status = loop {
            if let Some(status) = guard.0.try_wait().unwrap() {
                break status;
            }
            assert!(
                asked.elapsed() < Duration::from_secs(1),
                "退出请求 1 秒内没有退出\n{}",
                log_text(&data)
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            status.code(),
            Some(0),
            "用户退出的退出码必须是 0（守护不重启）"
        );
        let _ = peerdesk(&data).args(["autostart", "off"]).status();
    }
}

#[cfg(target_os = "macos")]
mod mac_tests {
    use super::*;

    /// 在临时目录里装一个最小的 .app（二进制 + Info.plist），让系统把它当成真正的应用：
    /// 只有这样 `open -a` 才会像点程序坞图标那样给它发“重新打开”事件。
    fn install_bundle(dir: &Path) -> PathBuf {
        let app = dir.join("PeerDesk.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::copy(BIN, macos.join("peerdesk")).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>org.peerdesk.livetest</string>
<key>CFBundleName</key><string>PeerDesk</string>
<key>CFBundleExecutable</key><string>peerdesk</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
"#,
        )
        .unwrap();
        app
    }

    fn open(app: &Path, data: &Path, extra: &[&str]) -> bool {
        Command::new("open")
            .args(["-a"])
            .arg(app)
            .args(extra)
            .args(["--env", &format!("PEERDESK_DATA_DIR={}", data.display())])
            .args(["--env", "PEERDESK_LANG=en"])
            .status()
            .unwrap()
            .success()
    }

    /// 真机反馈 7：点关闭后再点程序坞图标没反应（winit 没把 applicationShouldHandleReopen 转出来）。
    /// 流程：用 `open -a` 启动 → 等后台就绪 → `peerdesk window close`（和点关闭按钮同一路径）→ 等“窗口已隐藏”
    /// → 再 `open -a` 同一个应用：系统发的就是点程序坞图标的“重新打开”事件 → 日志里必须出现重新打开和显示窗口。
    /// CI 机器没有辅助功能权限，不能用 System Events，所以用程序自己的日志判断。
    #[test]
    #[ignore = "需要 macOS 桌面，只在 [mac] 的实机任务里跑"]
    fn clicking_the_dock_icon_reopens_the_hidden_window() {
        let dir = temp_dir("mac-reopen");
        let data = dir.join("data");
        std::fs::create_dir_all(&data).unwrap();
        mark_hint_seen(&data);
        let app = install_bundle(&dir);
        assert!(open(&app, &data, &[]), "open 启动失败");
        wait_until(
            "后台常驻就绪（托盘已建立）",
            &data,
            Duration::from_secs(60),
            || log_text(&data).contains("托盘已建立"),
        );
        let window = |what: &str| {
            assert!(
                peerdesk(&data)
                    .args(["window", what])
                    .status()
                    .unwrap()
                    .success(),
                "peerdesk window {what} 失败"
            );
        };
        window("close");
        wait_until("窗口隐藏", &data, Duration::from_secs(10), || {
            log_text(&data).contains("窗口已隐藏到托盘")
        });
        let before = log_text(&data).matches("显示主窗口").count();
        assert!(open(&app, &data, &[]), "再次 open 失败");
        wait_until(
            "收到重新打开事件",
            &data,
            Duration::from_secs(10),
            || log_text(&data).contains("程序坞图标被点击"),
        );
        wait_until(
            "重新打开后显示了窗口",
            &data,
            Duration::from_secs(10),
            || log_text(&data).matches("显示主窗口").count() > before,
        );
        window("quit");
        wait_until("退出", &data, Duration::from_secs(10), || {
            log_text(&data).contains("收到窗口请求：quit")
        });
    }
}
