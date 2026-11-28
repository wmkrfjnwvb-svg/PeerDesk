//! 连接失败、会话结束的原因怎么说（中英）：一句原因、一句办法，最多两个按钮。
//! 原因由网络层分类（[`Reason`]），核心的原文放进“详情”，可以复制。手机端的同一套文字在
//! `android/app/src/main/res/values*/strings.xml`（`reason_<代号>_cause`、`reason_<代号>_fix`）。

use peerdesk_net::reason::{Action, Failure, Reason};

use super::i18n::{Lang, Strings};
use super::transfers::{FileProblem, TransferFailure};

/// 一个原因给用户看的话。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReasonText {
    /// 为什么（一句）。
    pub cause: &'static str,
    /// 怎么办（一句）。
    pub fix: &'static str,
}

const fn t(cause: &'static str, fix: &'static str) -> ReasonText {
    ReasonText { cause, fix }
}

/// 这个原因在这种语言里怎么说。
#[must_use]
pub const fn reason_text(reason: Reason, lang: Lang) -> ReasonText {
    match lang {
        Lang::Zh => zh(reason),
        Lang::En => en(reason),
    }
}

const fn zh(reason: Reason) -> ReasonText {
    match reason {
        Reason::NoRoute => t(
            "连不到对方：等了很久没有回应，也没有找到能走的线路。",
            "确认对方电脑开着、打开了“允许别人连接”，再重试；还不行就把详情发给我们。",
        ),
        Reason::DeviceOffline => t(
            "对方现在不在线，或者设备码不对。",
            "核对设备码；请对方打开 PeerDesk，并确认电脑联网、没有睡眠。",
        ),
        Reason::ServerUnreachable => t(
            "连不上服务器。",
            "检查本机的网络；用自己的服务器时，核对服务器地址。",
        ),
        Reason::WrongPassword => t(
            "密码不对。",
            "向对方要现在的密码（对方换过密码后旧的就失效了），重新输入后再试。",
        ),
        Reason::Locked => t(
            "密码错的次数太多，对方暂时不接受连接。",
            "过几分钟、核对密码后再试。",
        ),
        Reason::Busy => t("对方正在被别的设备控制。", "等那边的会话结束后再试。"),
        Reason::Declined => t(
            "对方拒绝了这次连接。",
            "请对方确认允许连接后再试，具体原因见详情。",
        ),
        Reason::HostPermission => t(
            "对方电脑现在打不开屏幕或键鼠：可能缺少屏幕录制、辅助功能权限，或者正在锁屏。",
            "请对方在 PeerDesk 里按提示开启权限，或者先解锁屏幕，再重试。",
        ),
        Reason::Version => t("双方的 PeerDesk 版本不兼容。", "把两边都升级到最新版本。"),
        Reason::IdentityMismatch => t(
            "对方的身份和记录的不一致：可能连错了设备，也可能有人冒充。",
            "先不要继续，和对方核对设备码或指纹。",
        ),
        Reason::PeerClosed => t("对方结束了这次会话。", "需要时可以重新连接。"),
        Reason::PeerQuit => t(
            "对方退出了 PeerDesk。",
            "请对方重新打开 PeerDesk 后再连接。",
        ),
        Reason::NetworkLost => t(
            "网络断了（可能换了网络，或者 Wi-Fi 断开）。",
            "检查网络后重试；PeerDesk 会先自动重连几次。",
        ),
        Reason::RelayQuota => t(
            "免费中继的流量额度暂时用完了。",
            "过几分钟再试；直连不受这个限制。",
        ),
        Reason::PortInUse => t(
            "本机的端口被占用了（也许另一个 PeerDesk 正在运行）。",
            "关掉另一个 PeerDesk，或者在设置里换一个端口。",
        ),
        Reason::Cancelled => t("你断开了连接。", "需要时可以重新连接。"),
        Reason::Other => t("出了意料之外的问题。", "再试一次；还不行就把详情发给我们。"),
    }
}

const fn en(reason: Reason) -> ReasonText {
    match reason {
        Reason::NoRoute => t(
            "Could not reach the other computer: no response, and no usable route.",
            "Make sure it is on and “Allow incoming connections” is turned on, then try again. If it still fails, send us the details.",
        ),
        Reason::DeviceOffline => t(
            "The other device is offline, or the device code is wrong.",
            "Check the code. Ask them to open PeerDesk and keep the computer online and awake.",
        ),
        Reason::ServerUnreachable => t(
            "Could not reach the server.",
            "Check your network. If you use your own server, check its address.",
        ),
        Reason::WrongPassword => t(
            "The password is wrong.",
            "Ask for the current password (an old one stops working once it is changed), enter it and try again.",
        ),
        Reason::Locked => t(
            "Too many wrong passwords, so the other device is not accepting connections for now.",
            "Wait a few minutes, check the password and try again.",
        ),
        Reason::Busy => t(
            "The other device is being controlled by someone else.",
            "Try again after that session ends.",
        ),
        Reason::Declined => t(
            "The other device declined the connection.",
            "Ask them to allow the connection and try again. The details say why.",
        ),
        Reason::HostPermission => t(
            "The other computer cannot share its screen or keyboard and mouse right now: a permission (screen recording, accessibility) may be missing, or it is locked.",
            "Ask them to grant the permissions in PeerDesk or unlock the screen, then try again.",
        ),
        Reason::Version => t(
            "The two PeerDesk versions are not compatible.",
            "Update PeerDesk on both devices to the latest version.",
        ),
        Reason::IdentityMismatch => t(
            "The other device's identity does not match the one on record: it may be the wrong device, or someone may be impersonating it.",
            "Do not continue. Check the device code or fingerprint with them.",
        ),
        Reason::PeerClosed => t(
            "The other side ended the session.",
            "You can connect again when needed.",
        ),
        Reason::PeerQuit => t(
            "The other side quit PeerDesk.",
            "Ask them to open PeerDesk again, then reconnect.",
        ),
        Reason::NetworkLost => t(
            "The network connection dropped (the network may have changed, or Wi-Fi disconnected).",
            "Check the network and try again. PeerDesk reconnects automatically a few times first.",
        ),
        Reason::RelayQuota => t(
            "The free relay traffic allowance is used up for now.",
            "Try again in a few minutes. Direct connections are not limited.",
        ),
        Reason::PortInUse => t(
            "The port on this computer is in use (maybe another PeerDesk is running).",
            "Close the other PeerDesk, or pick another port in Settings.",
        ),
        Reason::Cancelled => t("You disconnected.", "You can connect again when needed."),
        Reason::Other => t(
            "Something unexpected went wrong.",
            "Try again. If it keeps happening, send us the details.",
        ),
    }
}

/// 文件传输失败的原因在这种语言里怎么说。
#[must_use]
pub const fn file_problem_text(problem: FileProblem, lang: Lang) -> ReasonText {
    match lang {
        Lang::Zh => match problem {
            FileProblem::DiskFull => t(
                "接收的一方磁盘空间不足。",
                "清理出空间后点重试，会从断点接着传。",
            ),
            FileProblem::NoPermission => t(
                "没有权限读写这个文件或文件夹。",
                "换一个有权限的文件夹，或者改好权限后重试。",
            ),
            FileProblem::Checksum => t(
                "文件校验没通过（传输中数据出错，或者源文件在传输中被改动）。",
                "点重试重新传这个文件。",
            ),
            FileProblem::SourceChanged => t(
                "源文件在传输过程中被改动、移走或删除了。",
                "等文件不再变化后点重试。",
            ),
            FileProblem::AccessOff => t(
                "对方关闭了文件传输权限。",
                "请对方在 PeerDesk 里允许文件传输后重试。",
            ),
            FileProblem::Interrupted => t(
                "传输中断了（连接断开，或者对方中止了传输）。",
                "连上之后点重试，会从断点接着传。",
            ),
            FileProblem::Missing => t("文件或目标文件夹不存在。", "检查路径后重试。"),
            FileProblem::Other => t("传输出错了。", "点重试；还不行就把详情发给我们。"),
        },
        Lang::En => match problem {
            FileProblem::DiskFull => t(
                "The receiving side is out of disk space.",
                "Free up space, then retry. It resumes where it stopped.",
            ),
            FileProblem::NoPermission => t(
                "No permission to read or write this file or folder.",
                "Pick a folder you can write to, or fix the permissions, then retry.",
            ),
            FileProblem::Checksum => t(
                "The file failed its integrity check (data was damaged, or the source changed during the transfer).",
                "Retry to send the file again.",
            ),
            FileProblem::SourceChanged => t(
                "The source file was changed, moved or deleted during the transfer.",
                "Retry once the file has stopped changing.",
            ),
            FileProblem::AccessOff => t(
                "The other side has turned off file transfer.",
                "Ask them to allow file transfer in PeerDesk, then retry.",
            ),
            FileProblem::Interrupted => t(
                "The transfer was interrupted (the connection dropped, or the other side stopped it).",
                "Retry once connected. It resumes where it stopped.",
            ),
            FileProblem::Missing => t(
                "The file or the destination folder does not exist.",
                "Check the path and retry.",
            ),
            FileProblem::Other => t(
                "The transfer failed.",
                "Retry. If it keeps failing, send us the details.",
            ),
        },
    }
}

/// 传输记录里失败那一行：哪个文件、原因、办法。原文不在这里（放进悬停和“复制详情”）。
#[must_use]
pub fn transfer_failed(failure: &TransferFailure, s: &Strings) -> String {
    let text = file_problem_text(failure.problem, s.lang);
    match s.lang {
        Lang::Zh => format!("{}：{}{}", failure.file, text.cause, text.fix),
        Lang::En => format!("{}: {} {}", failure.file, text.cause, text.fix),
    }
}

/// 按钮上的字。桌面端没有“导出诊断记录”，交给开发者的是复制下来的详情。
#[must_use]
pub const fn action_label(action: Action, s: &Strings) -> &'static str {
    match action {
        Action::Retry => s.retry,
        Action::ServerSettings => s.server_settings,
        Action::Diagnostics => s.copy_details,
    }
}

/// 一次失败的原因（一句）。
#[must_use]
pub const fn cause(failure: &Failure, s: &Strings) -> &'static str {
    reason_text(failure.reason, s.lang).cause
}

/// 一次失败的办法（一句）。
#[must_use]
pub const fn fix(failure: &Failure, s: &Strings) -> &'static str {
    reason_text(failure.reason, s.lang).fix
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::i18n::{EN, ZH};

    /// 中文里允许出现的英文：产品名和通用写法。
    const ALLOWED_IN_ZH: [&str; 2] = ["PeerDesk", "Wi-Fi"];

    fn has_ascii_word(text: &str) -> bool {
        let mut rest = text.to_owned();
        for w in ALLOWED_IN_ZH {
            rest = rest.replace(w, "");
        }
        rest.chars().any(|c| c.is_ascii_alphabetic())
    }

    #[test]
    fn every_reason_has_chinese_and_english() {
        for r in Reason::ALL {
            let zh = reason_text(r, Lang::Zh);
            let en = reason_text(r, Lang::En);
            for text in [zh.cause, zh.fix, en.cause, en.fix] {
                assert!(text.chars().count() >= 4, "{r:?}：{text}");
            }
            // 中文里没有英文原文；英文里没有中文
            assert!(
                !has_ascii_word(zh.cause) && !has_ascii_word(zh.fix),
                "{r:?}"
            );
            for text in [en.cause, en.fix] {
                assert!(text.is_ascii() || text.contains('“'), "{r:?}：{text}");
                assert!(
                    !text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                    "{r:?}"
                );
            }
            // 一句原因、一句办法：原因和办法不重复
            assert_ne!(zh.cause, zh.fix);
            assert_ne!(en.cause, en.fix);
        }
        assert!(has_ascii_word("timed out") && !has_ascii_word("退出了 PeerDesk"));
    }

    #[test]
    fn every_file_problem_has_chinese_and_english() {
        for p in FileProblem::ALL {
            let zh = file_problem_text(p, Lang::Zh);
            let en = file_problem_text(p, Lang::En);
            assert!(
                !has_ascii_word(zh.cause) && !has_ascii_word(zh.fix),
                "{p:?}"
            );
            for text in [en.cause, en.fix] {
                assert!(text.is_ascii(), "{p:?}：{text}");
            }
            assert_ne!(zh.cause, zh.fix);
        }
        let f = TransferFailure::new("报告.pdf", "写入 /x：No space left on device (os error 28)");
        assert_eq!(
            transfer_failed(&f, &ZH),
            "报告.pdf：接收的一方磁盘空间不足。清理出空间后点重试，会从断点接着传。"
        );
        let en = transfer_failed(&f, &EN);
        assert!(en.starts_with("报告.pdf: The receiving side is out of disk space. Free up"));
        // 没有调试输出、没有原文
        assert!(!en.contains("os error") && !en.contains("Some("));
    }

    /// 手机端说的和桌面端一样（`android/.../strings.xml` 的 `reason_<代号>_cause`、`_fix`）。
    #[test]
    fn android_says_the_same_as_the_desktop() {
        let res = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../android/app/src/main/res");
        for (dir, lang) in [("values", Lang::En), ("values-zh-rCN", Lang::Zh)] {
            let xml =
                std::fs::read_to_string(res.join(dir).join("strings.xml")).unwrap_or_default();
            for r in Reason::ALL {
                let text = reason_text(r, lang);
                for (part, want) in [("cause", text.cause), ("fix", text.fix)] {
                    let line = format!(
                        "<string name=\"reason_{}_{part}\">{}</string>",
                        r.code(),
                        want.replace('&', "&amp;").replace('\'', "\\'")
                    );
                    assert!(xml.contains(&line), "{dir}：{line}");
                }
            }
        }
    }

    #[test]
    fn failures_and_buttons_follow_the_language() {
        let f = Failure::of_text("对端结束会话");
        assert_eq!(cause(&f, &ZH), "对方结束了这次会话。");
        assert_eq!(cause(&f, &EN), "The other side ended the session.");
        assert_eq!(fix(&f, &EN), "You can connect again when needed.");
        assert_eq!(action_label(Action::Retry, &ZH), ZH.retry);
        assert_eq!(
            action_label(Action::ServerSettings, &EN),
            EN.server_settings
        );
        assert_eq!(action_label(Action::Diagnostics, &ZH), ZH.copy_details);
    }
}
