//! Linux 图形界面启动前检查运行库：图形库（winit、glutin）在运行时才加载 X11、xkbcommon、OpenGL，
//! 缺了就直接崩溃，只留一句英文。这里先查系统的库缓存（`ldconfig -p`），缺哪个、装哪个包说清楚。

/// 一个需要的库：任一 soname 存在即可，以及 Debian/Ubuntu 上提供它的包。
struct Need {
    sonames: &'static [&'static str],
    package: &'static str,
}

/// 两种会话都要的。
const COMMON: &[Need] = &[
    Need {
        sonames: &["libxkbcommon.so.0"],
        package: "libxkbcommon0",
    },
    Need {
        sonames: &["libGL.so.1", "libEGL.so.1"],
        package: "libgl1 libegl1",
    },
];

/// X11 会话要的。
const X11: &[Need] = &[
    Need {
        sonames: &["libxkbcommon-x11.so.0"],
        package: "libxkbcommon-x11-0",
    },
    Need {
        sonames: &["libX11.so.6"],
        package: "libx11-6",
    },
    Need {
        sonames: &["libX11-xcb.so.1"],
        package: "libx11-xcb1",
    },
    Need {
        sonames: &["libXcursor.so.1"],
        package: "libxcursor1",
    },
    Need {
        sonames: &["libXrandr.so.2"],
        package: "libxrandr2",
    },
    Need {
        sonames: &["libXi.so.6"],
        package: "libxi6",
    },
];

/// Wayland 会话要的。
const WAYLAND: &[Need] = &[Need {
    sonames: &["libwayland-client.so.0"],
    package: "libwayland-client0",
}];

/// 从 `ldconfig -p` 的输出里找缺的库，返回要装的包（去重、按出现顺序）。
#[must_use]
pub fn missing_packages(ldconfig: &str, wayland: bool) -> Vec<&'static str> {
    let has = |so: &str| ldconfig.lines().any(|l| l.trim_start().starts_with(so));
    let session = if wayland { WAYLAND } else { X11 };
    let mut out: Vec<&'static str> = Vec::new();
    for need in COMMON.iter().chain(session) {
        if !need.sonames.iter().any(|so| has(so)) && !out.contains(&need.package) {
            out.push(need.package);
        }
    }
    out
}

/// 缺库时给用户看的话。
#[must_use]
pub fn advice(packages: &[&str]) -> String {
    format!(
        "缺少图形界面需要的系统库，PeerDesk 无法打开窗口。\n\
         Debian / Ubuntu 上运行：sudo apt install {}\n\
         （用 .deb 安装包安装会自动装好：sudo apt install ./peerdesk_*.deb）\n\
         Missing system libraries for the GUI. Install them with the command above.",
        packages.join(" ")
    )
}

/// 检查本机。查不了（没有 ldconfig）时不拦，交给图形库自己报错。
#[cfg(target_os = "linux")]
#[must_use]
pub fn check() -> Option<String> {
    let output = ["/sbin/ldconfig", "/usr/sbin/ldconfig", "ldconfig"]
        .iter()
        .find_map(|cmd| std::process::Command::new(cmd).arg("-p").output().ok())
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&output.stdout);
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    let missing = missing_packages(&text, wayland);
    (!missing.is_empty()).then(|| advice(&missing))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str =
        "	libxkbcommon.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libxkbcommon.so.0
	libxkbcommon-x11.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libxkbcommon-x11.so.0
	libX11.so.6 (libc6,x86-64) => /lib/x86_64-linux-gnu/libX11.so.6
	libX11-xcb.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libX11-xcb.so.1
	libXcursor.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libXcursor.so.1
	libXrandr.so.2 (libc6,x86-64) => /lib/x86_64-linux-gnu/libXrandr.so.2
	libXi.so.6 (libc6,x86-64) => /lib/x86_64-linux-gnu/libXi.so.6
	libEGL.so.1 (libc6,x86-64) => /lib/x86_64-linux-gnu/libEGL.so.1
	libwayland-client.so.0 (libc6,x86-64) => /lib/x86_64-linux-gnu/libwayland-client.so.0";

    #[test]
    fn finds_what_is_missing() {
        assert!(missing_packages(FULL, false).is_empty());
        assert!(missing_packages(FULL, true).is_empty());
        // 用户机器上的情况：缺 xkbcommon-x11
        let without: String = FULL
            .lines()
            .filter(|l| !l.contains("xkbcommon-x11"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(missing_packages(&without, false), ["libxkbcommon-x11-0"]);
        // Wayland 会话不需要它
        assert!(missing_packages(&without, true).is_empty());
        // 什么都没有：每个包只列一次
        let all = missing_packages("", false);
        assert_eq!(all.len(), COMMON.len() + X11.len());
        assert_eq!(
            missing_packages("", true),
            ["libxkbcommon0", "libgl1 libegl1", "libwayland-client0"]
        );
        assert!(advice(&all).contains("sudo apt install libxkbcommon0"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_machine_can_be_checked() {
        // 只要求不出错；CI 机器上可能缺图形库，结果不断言
        let _ = check();
    }
}
