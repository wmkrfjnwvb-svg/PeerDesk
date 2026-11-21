//! 权限向导：站在用户的角度，按系统和系统版本一步一步告诉他怎么授权，每一步配一个按钮。
//!
//! 这里只决定“显示哪几步、每步配什么按钮”，不做 IO，可以在任何系统上测试。
//! 按钮真正做的事（打开系统设置、弹出授权对话框、重新打开程序）在 [`crate::perform`] 里。
//!
//! 各系统的差别：
//! - macOS 13 起叫“系统设置 → 隐私与安全性”，12 及更早叫“系统偏好设置 → 安全性与隐私 → 隐私”，
//!   要先点左下角的锁解锁。15 起“屏幕录制”改名为“录屏与系统录音”，并且系统会定期（约每月）
//!   再确认一次。授予屏幕录制后，程序要退出重开才生效。
//! - Windows 不需要授权；但要控制“以管理员身份运行”的窗口，PeerDesk 自己也要以管理员身份运行。
//! - Linux 这一版只支持 X11（Xorg）会话；Wayland 会话下无法采集和控制，要在登录界面换成 Xorg。

use crate::PermissionStatus;

/// 运行的系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// macOS，主版本号和次版本号（例如 14.5 是 `(14, 5)`）；读不到版本时按最新系统处理。
    MacOs {
        /// 主版本号。
        major: u32,
        /// 次版本号。
        minor: u32,
    },
    /// Windows。
    Windows,
    /// Linux 的 X11 会话。
    LinuxX11,
    /// Linux 的 Wayland 会话。
    LinuxWayland,
    /// 其他系统。
    Other,
}

/// 一步里的按钮做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// 打开系统设置的“屏幕录制”页面。
    OpenScreenSettings,
    /// 打开系统设置的“辅助功能”页面。
    OpenInputSettings,
    /// 请系统弹出“屏幕录制”授权对话框（同时把本程序加进设置里的列表）。
    RequestScreen,
    /// 请系统弹出“辅助功能”授权对话框（同时把本程序加进设置里的列表）。
    RequestInput,
    /// 退出并重新打开本程序。
    Relaunch,
}

/// 向导里的一步。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    /// 说明文字。
    pub text: String,
    /// 按钮：按钮上的字和要做的事。
    pub button: Option<(&'static str, Action)>,
    /// 这一步已经完成（显示为打勾）。
    pub done: bool,
}

/// 整个向导。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guide {
    /// 标题。
    pub title: String,
    /// 步骤。
    pub steps: Vec<Step>,
    /// 所有条件都满足，可以被远程控制。
    pub ready: bool,
}

/// 从 `sw_vers -productVersion` 的输出（例如 `14.5` 或 `15.0.1`）读出主、次版本号。
#[must_use]
pub fn parse_mac_version(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().map_or(Some(0), |m| m.parse().ok())?;
    Some((major, minor))
}

/// 根据 `XDG_SESSION_TYPE` 判断 Linux 会话类型。没有这个变量时按 X11 处理（例如 SSH 转发、Xvfb）。
#[must_use]
pub fn linux_session(xdg_session_type: Option<&str>) -> Os {
    match xdg_session_type.map(str::trim) {
        Some(t) if t.eq_ignore_ascii_case("wayland") => Os::LinuxWayland,
        Some(_) | None => Os::LinuxX11,
    }
}

fn step(text: impl Into<String>, button: Option<(&'static str, Action)>, done: bool) -> Step {
    Step {
        text: text.into(),
        button,
        done,
    }
}

/// macOS 上两项权限在设置里的位置，按版本给出。
fn mac_places(major: u32) -> (String, String) {
    // macOS 11 起主版本号是 11、12……；10.15 及更早主版本号是 10。
    let (root, screen) = match major {
        0..=12 => (
            "“系统偏好设置” → “安全性与隐私” → “隐私”标签",
            "左侧的“屏幕录制”",
        ),
        13 | 14 => ("“系统设置” → “隐私与安全性”", "“屏幕录制”"),
        _ => ("“系统设置” → “隐私与安全性”", "“录屏与系统录音”"),
    };
    let input = if major <= 12 {
        "左侧的“辅助功能”"
    } else {
        "“辅助功能”"
    };
    (format!("{root} → {screen}"), format!("{root} → {input}"))
}

/// 向导：在 `os` 上，权限现状是 `status`，这台设备怎样才能被远程控制。
#[must_use]
pub fn guide(os: Os, status: PermissionStatus) -> Guide {
    match os {
        Os::MacOs { major, minor } => mac_guide(major, minor, status),
        Os::Windows => Guide {
            title: "Windows：不需要额外授权".into(),
            steps: vec![
                step(
                    "第一次开启时，Windows 防火墙可能弹窗询问，请勾选“专用网络”和“公用网络”并点“允许访问”，否则别人连不进来。",
                    None,
                    true,
                ),
                step(
                    "要控制“以管理员身份运行”的程序（包括 UAC 确认框），PeerDesk 自己也要右键“以管理员身份运行”。",
                    None,
                    true,
                ),
            ],
            ready: status.all_granted(),
        },
        Os::LinuxX11 | Os::Other => Guide {
            title: "Linux（X11）：不需要额外授权".into(),
            steps: Vec::new(),
            ready: status.all_granted(),
        },
        Os::LinuxWayland => Guide {
            title: "Linux：当前是 Wayland 会话，这一版还不能被远程控制".into(),
            steps: vec![
                step("注销当前用户，回到登录界面。", None, false),
                step(
                    "选中你的用户后，点右下角（或密码框旁边）的齿轮图标，选“Ubuntu on Xorg”或“GNOME on Xorg”，再登录。",
                    None,
                    false,
                ),
                step(
                    "重新打开 PeerDesk 开启被控。以后版本会直接支持 Wayland（通过系统的屏幕共享授权）。",
                    None,
                    false,
                ),
            ],
            ready: false,
        },
    }
}

fn mac_guide(major: u32, minor: u32, status: PermissionStatus) -> Guide {
    let (screen_place, input_place) = mac_places(major);
    let unlock = if major <= 12 {
        "先点窗口左下角的锁，输入开机密码解锁，"
    } else {
        ""
    };
    let mut steps = Vec::new();
    // 第一项：屏幕录制。
    steps.push(step(
        "① 允许“屏幕录制”：对方才能看到你的屏幕。点右边的按钮，系统会弹窗；选“打开系统设置”。",
        (!status.screen_capture).then_some(("请求屏幕录制权限", Action::RequestScreen)),
        status.screen_capture,
    ));
    if !status.screen_capture {
        steps.push(step(
            format!(
                "   在 {screen_place} 里，{unlock}找到 PeerDesk 并打开开关。列表里没有 PeerDesk 时，点“+”，到“应用程序”里选 PeerDesk。"
            ),
            Some(("打开屏幕录制设置", Action::OpenScreenSettings)),
            false,
        ));
        steps.push(step(
            "   打开开关后系统会提示“退出并重新打开”，点它；没有提示就点右边的按钮。",
            Some(("重新打开 PeerDesk", Action::Relaunch)),
            false,
        ));
    }
    // 第二项：辅助功能。
    steps.push(step(
        "② 允许“辅助功能”：对方才能操作你的键盘和鼠标。",
        (!status.input_injection).then_some(("请求辅助功能权限", Action::RequestInput)),
        status.input_injection,
    ));
    if !status.input_injection {
        steps.push(step(
            format!(
                "   在 {input_place} 里，{unlock}找到 PeerDesk 并打开开关（没有就点“+”添加）。这一项打开后立即生效，不用重开。"
            ),
            Some(("打开辅助功能设置", Action::OpenInputSettings)),
            false,
        ));
    }
    if !status.all_granted() {
        steps.push(step(
            "开关已经打开却仍显示未授权：在列表里选中 PeerDesk 点“−”删掉，再点“+”重新添加，然后重新打开 PeerDesk。更新版本后常见这种情况。",
            None,
            false,
        ));
    }
    if major >= 15 {
        steps.push(step(
            "macOS 15 起，系统会定期（大约每月）再问一次是否继续允许录屏，选“继续允许”即可，否则远程会中断。",
            None,
            status.all_granted(),
        ));
    }
    let version = if minor == 0 {
        format!("{major}")
    } else {
        format!("{major}.{minor}")
    };
    let title = if status.all_granted() {
        format!("macOS {version}：权限已就绪")
    } else {
        format!("macOS {version}：还差几步才能被远程控制")
    };
    Guide {
        title,
        steps,
        ready: status.all_granted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: PermissionStatus = PermissionStatus {
        screen_capture: false,
        input_injection: false,
    };
    const ALL: PermissionStatus = PermissionStatus {
        screen_capture: true,
        input_injection: true,
    };

    fn texts(g: &Guide) -> String {
        g.steps
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn actions(g: &Guide) -> Vec<Action> {
        g.steps
            .iter()
            .filter_map(|s| s.button.map(|b| b.1))
            .collect()
    }

    #[test]
    fn versions_and_sessions_are_parsed() {
        assert_eq!(parse_mac_version("14.5\n"), Some((14, 5)));
        assert_eq!(parse_mac_version("15.0.1"), Some((15, 0)));
        assert_eq!(parse_mac_version("26"), Some((26, 0)));
        assert_eq!(parse_mac_version("10.15.7"), Some((10, 15)));
        assert_eq!(parse_mac_version(""), None);
        assert_eq!(parse_mac_version("x.1"), None);
        assert_eq!(parse_mac_version("14.x"), None);
        assert_eq!(linux_session(Some("wayland")), Os::LinuxWayland);
        assert_eq!(linux_session(Some(" Wayland ")), Os::LinuxWayland);
        assert_eq!(linux_session(Some("x11")), Os::LinuxX11);
        assert_eq!(linux_session(None), Os::LinuxX11);
    }

    #[test]
    fn mac_wording_follows_the_version() {
        let old = guide(
            Os::MacOs {
                major: 12,
                minor: 7,
            },
            NONE,
        );
        assert!(old.title.contains("macOS 12.7") && !old.ready);
        assert!(texts(&old).contains("系统偏好设置"));
        assert!(texts(&old).contains("点窗口左下角的锁"));
        assert!(texts(&old).contains("屏幕录制"));

        let mid = guide(
            Os::MacOs {
                major: 14,
                minor: 0,
            },
            NONE,
        );
        assert!(mid.title.contains("macOS 14："));
        assert!(texts(&mid).contains("“系统设置” → “隐私与安全性” → “屏幕录制”"));
        assert!(!texts(&mid).contains("锁"));
        assert!(!texts(&mid).contains("每月"));

        let new = guide(
            Os::MacOs {
                major: 15,
                minor: 1,
            },
            NONE,
        );
        assert!(texts(&new).contains("录屏与系统录音"));
        assert!(texts(&new).contains("每月"));
        assert_eq!(
            actions(&new),
            vec![
                Action::RequestScreen,
                Action::OpenScreenSettings,
                Action::Relaunch,
                Action::RequestInput,
                Action::OpenInputSettings
            ]
        );
    }

    #[test]
    fn mac_steps_shrink_as_permissions_arrive() {
        let os = Os::MacOs {
            major: 15,
            minor: 0,
        };
        let screen_only = PermissionStatus {
            screen_capture: true,
            input_injection: false,
        };
        let g = guide(os, screen_only);
        assert_eq!(
            actions(&g),
            vec![Action::RequestInput, Action::OpenInputSettings]
        );
        assert!(g.steps.first().is_some_and(|s| s.done));
        let done = guide(os, ALL);
        assert!(done.ready && done.title.contains("已就绪"));
        assert!(actions(&done).is_empty());
        assert!(done.steps.iter().all(|s| s.done));
    }

    #[test]
    fn other_systems() {
        let w = guide(Os::Windows, ALL);
        assert!(w.ready && texts(&w).contains("防火墙") && texts(&w).contains("管理员"));
        assert!(guide(Os::LinuxX11, ALL).ready);
        assert!(guide(Os::Other, ALL).steps.is_empty());
        let way = guide(Os::LinuxWayland, ALL);
        assert!(!way.ready && texts(&way).contains("Xorg"));
    }
}
