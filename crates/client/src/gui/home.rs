//! 主窗口背后的逻辑：把状态变成界面上要说的话（发生了什么、怎么办）和连接状态的颜色。
//! 只算不画，`app.rs` 拿结果去画。

use peerdesk_core::proto::PeerOs;
use peerdesk_core::signal::Nat;
use peerdesk_net::netcheck::NetReport;
use peerdesk_net::reason::Action;

use super::book::{BookError, MAX_DEPTH, MAX_NAME};
use super::i18n::Strings;
use super::state::{HostPhase, ViewPhase};
use super::theme::Signal;

/// 设备列表上面的切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    /// 最近连过的。
    Recent,
    /// 收藏的。
    Favorites,
    /// 全部设备，按分组排成树。
    Groups,
    /// 同一网络里的电脑。
    Network,
}

impl Tab {
    /// 全部切换项，按显示顺序。
    pub const ALL: [Self; 4] = [Self::Recent, Self::Favorites, Self::Groups, Self::Network];

    /// 切换项上的字。
    #[must_use]
    pub const fn label(self, s: &Strings) -> &'static str {
        match self {
            Self::Recent => s.tab_recent,
            Self::Favorites => s.tab_favorites,
            Self::Groups => s.tab_groups,
            Self::Network => s.tab_network,
        }
    }

    /// 这一栏空着时说什么（原因和下一步）。
    #[must_use]
    pub const fn empty(self, s: &Strings) -> &'static str {
        match self {
            Self::Recent => s.no_recent,
            Self::Favorites => s.empty_favorites,
            Self::Groups => s.empty_groups,
            Self::Network => s.network_unavailable,
        }
    }
}

/// “这台电脑”现在的情况：状态标签、颜色、下面那句说明。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBadge {
    /// 状态标签上的字。
    pub label: &'static str,
    /// 状态的颜色。
    pub signal: Signal,
    /// 标签下面的说明：现在是什么情况、接下来怎么做。
    pub hint: &'static str,
}

/// 被控端各阶段在界面上怎么说。`has_server` 是有没有设置服务器（没有就没有设备码），
/// `permitted` 是系统权限齐不齐（不齐不能开启）。
#[must_use]
pub const fn host_badge(
    phase: &HostPhase,
    has_server: bool,
    permitted: bool,
    s: &Strings,
) -> HostBadge {
    let (label, signal, hint) = match phase {
        HostPhase::Off if !permitted => (s.host_off, Signal::Relay, s.hint_need_permission),
        HostPhase::Off => (s.host_off, Signal::Idle, s.hint_host_off),
        HostPhase::Starting => (s.host_starting, Signal::Idle, s.hint_starting),
        HostPhase::Listening { .. } if !has_server => {
            (s.badge_waiting, Signal::Direct, s.hint_bad_server)
        }
        HostPhase::Listening { .. } => (s.badge_waiting, Signal::Direct, s.hint_waiting),
        HostPhase::Busy {
            established: false, ..
        } => (s.host_handshaking, Signal::Idle, s.hint_handshaking),
        HostPhase::Busy {
            established: true, ..
        } => (s.host_controlled, Signal::Direct, s.hint_controlled),
        HostPhase::Stopping => (s.host_stopping, Signal::Idle, s.hint_host_off),
    };
    HostBadge {
        label,
        signal,
        hint,
    }
}

/// 侧边栏底部的服务器状态：一句话和颜色。
#[must_use]
pub const fn server_badge(
    has_server: bool,
    registered: bool,
    s: &Strings,
) -> (&'static str, Signal) {
    if !has_server {
        (s.server_invalid, Signal::Idle)
    } else if registered {
        (s.server_online, Signal::Direct)
    } else {
        (s.server_offline, Signal::Relay)
    }
}

/// 侧边栏底部第二行：本机网络好不好直连（体检结果里的 NAT 类型）。没有结论时不说。
#[must_use]
pub fn nat_hint(report: Option<&NetReport>, s: &Strings) -> Option<&'static str> {
    match report?.nat? {
        Nat::Easy => Some(s.nat_easy),
        Nat::Hard => Some(s.nat_hard),
        Nat::Unknown => None,
    }
}

/// 连接表单下面那一行：正在做什么，或者为什么没连上、怎么办。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectLine {
    /// 一句话（失败时是原因）。
    pub text: String,
    /// 怎么办（一句）；进行中为空。
    pub fix: String,
    /// 核心的原文，放在可展开的“详情”里、可以复制；没有时为空。
    pub details: String,
    /// 颜色：进行中是灰的，走中继重连是琥珀，失败是红的。
    pub signal: Signal,
    /// 按钮（最多两个，主要的在前）。
    pub actions: Vec<LineAction>,
}

/// 连接状态那一行的按钮。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineAction {
    /// 取消正在进行的连接。
    Cancel,
    /// 失败了，再试一次。
    Retry,
    /// 去设置页看服务器。
    ServerSettings,
    /// 把详情复制下来（发给开发者）。
    CopyDetails,
}

impl LineAction {
    /// 失败原因建议的按钮换成这一行的按钮。
    #[must_use]
    pub const fn of(action: Action) -> Self {
        match action {
            Action::Retry => Self::Retry,
            Action::ServerSettings => Self::ServerSettings,
            Action::Diagnostics => Self::CopyDetails,
        }
    }

    /// 按钮上的字。
    #[must_use]
    pub const fn label(self, s: &Strings) -> &'static str {
        match self {
            Self::Cancel => s.cancel,
            Self::Retry => s.retry,
            Self::ServerSettings => s.server_settings,
            Self::CopyDetails => s.copy_details,
        }
    }
}

/// 一次连接在主窗口里要不要说、怎么说。连上以后不说（远程窗口自己显示）；
/// 用户自己断开的、正常结束的也不说；只有“正在连”“在重连”“没连上”要说。
#[must_use]
pub fn connect_line(phase: &ViewPhase, target: &str, s: &Strings) -> Option<ConnectLine> {
    match phase {
        ViewPhase::Connecting => Some(ConnectLine {
            text: s.connecting_to.replace("{target}", target),
            fix: String::new(),
            details: String::new(),
            signal: Signal::Idle,
            actions: vec![LineAction::Cancel],
        }),
        ViewPhase::Reconnecting { why, .. } => Some(ConnectLine {
            text: format!("{target}：{}", phase.headline(s)),
            fix: String::new(),
            details: why.clone(),
            signal: Signal::Relay,
            actions: vec![LineAction::Cancel],
        }),
        ViewPhase::Ended { status, .. } if status.error => Some(ConnectLine {
            text: format!(
                "{}{}",
                s.connect_failed.replace("{target}", target),
                status.text
            ),
            fix: status.fix(s).unwrap_or_default().to_owned(),
            details: status.details().to_owned(),
            signal: Signal::Problem,
            actions: status.failure.as_ref().map_or_else(
                || vec![LineAction::Retry],
                |f| {
                    f.reason
                        .actions()
                        .iter()
                        .map(|a| LineAction::of(*a))
                        .collect()
                },
            ),
        }),
        ViewPhase::Connected { .. } | ViewPhase::Ended { .. } => None,
    }
}

/// 多久以前（“刚刚”“5 分钟前”“3 天前”）。`then` 为 0 表示没连过。
/// 只说相对时间：算“今天 09:12”要知道本地时区，客户端没有带时区库。
#[must_use]
pub fn ago(now: u64, then: u64, s: &Strings) -> String {
    if then == 0 {
        return s.ago_never.to_owned();
    }
    let secs = now.saturating_sub(then);
    let (n, unit) = match secs {
        0..60 => return s.ago_now.to_owned(),
        60..3600 => (secs / 60, s.ago_minutes),
        3600..86_400 => (secs / 3600, s.ago_hours),
        _ => (secs / 86_400, s.ago_days),
    };
    unit.replace("{n}", &n.to_string())
}

/// 现在的 Unix 秒。
#[must_use]
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// 系统的名字（设备名下面那行小字）；不知道时为空。
#[must_use]
pub const fn os_name(os: PeerOs) -> &'static str {
    match os {
        PeerOs::Windows => "Windows",
        PeerOs::Mac => "macOS",
        PeerOs::Linux => "Linux",
        PeerOs::Unknown => "",
    }
}

/// 设备簿的操作失败时给用户看的话：原因和办法。
#[must_use]
pub fn book_error(e: &BookError, s: &Strings) -> String {
    match e {
        BookError::EmptyName => s.err_group_empty.to_owned(),
        BookError::NameTooLong => s.err_name_long.replace("{n}", &MAX_NAME.to_string()),
        BookError::DuplicateName => s.err_group_duplicate.to_owned(),
        BookError::TooDeep => s.err_group_deep.replace("{n}", &MAX_DEPTH.to_string()),
        BookError::NotFound => s.err_gone.to_owned(),
        BookError::Io(why) => format!("{}{why}", s.err_book_save),
    }
}

/// 表格里正在就地编辑的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Editing {
    /// 新建分组；`parent` 是上级分组。
    NewGroup {
        /// 上级分组；`None` 建在顶层。
        parent: Option<super::book::Id>,
    },
    /// 给分组改名。
    RenameGroup(super::book::Id),
    /// 给设备起名字。
    RenameDevice(super::book::Id),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::i18n::{EN, ZH};
    use crate::gui::state::Status;
    use peerdesk_net::reason::{Failure, Reason};
    use peerdesk_net::NetError;
    use std::net::SocketAddr;

    #[test]
    fn tabs_have_labels_and_empty_states() {
        for s in [&ZH, &EN] {
            for tab in Tab::ALL {
                assert!(!tab.label(s).is_empty());
                assert!(tab.empty(s).chars().count() > tab.label(s).chars().count());
            }
        }
        assert_eq!(Tab::Groups.label(&ZH), "分组");
        assert_eq!(Tab::ALL[0], Tab::Recent);
    }

    #[test]
    fn host_badge_says_what_is_happening_and_what_to_do() {
        let s = &ZH;
        let addr: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let listening = HostPhase::Listening {
            port: 4433,
            ipv6: false,
        };
        let busy = |established| HostPhase::Busy {
            port: 4433,
            ipv6: false,
            addr,
            peer: String::new(),
            established,
        };
        let cases = [
            (
                HostPhase::Off,
                true,
                true,
                s.host_off,
                Signal::Idle,
                s.hint_host_off,
            ),
            (
                HostPhase::Off,
                true,
                false,
                s.host_off,
                Signal::Relay,
                s.hint_need_permission,
            ),
            (
                HostPhase::Starting,
                true,
                true,
                s.host_starting,
                Signal::Idle,
                s.hint_starting,
            ),
            (
                listening.clone(),
                true,
                true,
                s.badge_waiting,
                Signal::Direct,
                s.hint_waiting,
            ),
            (
                listening,
                false,
                true,
                s.badge_waiting,
                Signal::Direct,
                s.hint_bad_server,
            ),
            (
                busy(false),
                true,
                true,
                s.host_handshaking,
                Signal::Idle,
                s.hint_handshaking,
            ),
            (
                busy(true),
                true,
                true,
                s.host_controlled,
                Signal::Direct,
                s.hint_controlled,
            ),
            (
                HostPhase::Stopping,
                true,
                true,
                s.host_stopping,
                Signal::Idle,
                s.hint_host_off,
            ),
        ];
        for (phase, has_server, permitted, label, signal, hint) in cases {
            let badge = host_badge(&phase, has_server, permitted, s);
            assert_eq!(
                badge,
                HostBadge {
                    label,
                    signal,
                    hint
                },
                "{phase:?}"
            );
            assert!(!badge.hint.is_empty());
        }
    }

    #[test]
    fn server_and_network_lines() {
        let s = &ZH;
        assert_eq!(
            server_badge(false, false, s),
            (s.server_invalid, Signal::Idle)
        );
        assert_eq!(
            server_badge(true, true, s),
            (s.server_online, Signal::Direct)
        );
        assert_eq!(
            server_badge(true, false, s),
            (s.server_offline, Signal::Relay)
        );
        assert_eq!(nat_hint(None, s), None);
        let mut report = NetReport::default();
        assert_eq!(nat_hint(Some(&report), s), None);
        report.nat = Some(Nat::Easy);
        assert_eq!(nat_hint(Some(&report), s), Some(s.nat_easy));
        report.nat = Some(Nat::Hard);
        assert_eq!(nat_hint(Some(&report), s), Some(s.nat_hard));
        report.nat = Some(Nat::Unknown);
        assert_eq!(nat_hint(Some(&report), s), None);
    }

    #[test]
    fn connect_line_reports_progress_and_failures_only() {
        let s = &ZH;
        let line = connect_line(&ViewPhase::Connecting, "731 054 268", s).unwrap();
        assert_eq!(line.text, "正在连接 731 054 268");
        assert_eq!(
            (line.signal, line.actions),
            (Signal::Idle, vec![LineAction::Cancel])
        );
        let again = ViewPhase::Reconnecting {
            attempt: 2,
            why: "timed out".into(),
            reason: Reason::NetworkLost,
        };
        let line = connect_line(&again, "办公室 PC", s).unwrap();
        assert!(line.text.starts_with("办公室 PC：") && line.text.contains("2/3"));
        assert!(!line.text.contains("timed out"), "原文只在详情里");
        assert_eq!(line.details, "timed out");
        assert_eq!(
            (line.signal, line.actions),
            (Signal::Relay, vec![LineAction::Cancel])
        );
        let ended = |status| ViewPhase::Ended {
            status,
            summary: None,
            route: None,
            notice: None,
        };
        let line = connect_line(&ended(Status::error("密码不对")), "731 054 268", s).unwrap();
        assert_eq!(line.text, "没连上 731 054 268：密码不对");
        assert_eq!(
            (line.signal, line.actions),
            (Signal::Problem, vec![LineAction::Retry])
        );
        // 有结构化原因：原因、办法、详情和建议的按钮
        let unreachable = Failure::of_error(&NetError::Unreachable("超时：握手".into()));
        let line = connect_line(&ended(Status::failed(unreachable, s)), "731 054 268", s).unwrap();
        assert_eq!(line.text, "没连上 731 054 268：连不上服务器。");
        assert_eq!(
            line.fix,
            "检查本机的网络；用自己的服务器时，核对服务器地址。"
        );
        assert_eq!(line.details, "连不上服务器：超时：握手");
        assert_eq!(
            line.actions,
            vec![LineAction::Retry, LineAction::ServerSettings]
        );
        let lost = Failure::of_error(&NetError::Timeout("握手"));
        let line = connect_line(&ended(Status::failed(lost, &EN)), "x", &EN).unwrap();
        assert_eq!(
            line.actions,
            vec![LineAction::Retry, LineAction::CopyDetails]
        );
        let labels: Vec<_> = [
            LineAction::Cancel,
            LineAction::Retry,
            LineAction::ServerSettings,
            LineAction::CopyDetails,
        ]
        .iter()
        .map(|a| a.label(&EN))
        .collect();
        assert_eq!(
            labels,
            [EN.cancel, EN.retry, EN.server_settings, EN.copy_details]
        );
        // 自己断开、正常结束、已经连上：主窗口不说
        assert_eq!(connect_line(&ended(Status::info("已断开")), "x", s), None);
        let connected = ViewPhase::Connected {
            peer: String::new(),
            remote: "127.0.0.1:1".parse().unwrap(),
            route: peerdesk_net::rendezvous::Path::Direct,
            detail: String::new(),
        };
        assert_eq!(connect_line(&connected, "x", s), None);
    }

    #[test]
    fn relative_times() {
        let s = &ZH;
        let now = 1_000_000;
        assert_eq!(ago(now, 0, s), "没连过");
        assert_eq!(ago(now, now, s), "刚刚");
        assert_eq!(ago(now, now + 50, s), "刚刚", "对方的钟快了也不出负数");
        assert_eq!(ago(now, now - 59, s), "刚刚");
        assert_eq!(ago(now, now - 60, s), "1 分钟前");
        assert_eq!(ago(now, now - 3599, s), "59 分钟前");
        assert_eq!(ago(now, now - 3600, s), "1 小时前");
        assert_eq!(ago(now, now - 86_399, s), "23 小时前");
        assert_eq!(ago(now, now - 3 * 86_400, s), "3 天前");
        assert_eq!(ago(now, now - 3 * 86_400, &EN), "3 d ago");
        assert!(unix_now() > 1_700_000_000);
    }

    #[test]
    fn os_names_and_book_errors() {
        assert_eq!(os_name(PeerOs::Windows), "Windows");
        assert_eq!(os_name(PeerOs::Mac), "macOS");
        assert_eq!(os_name(PeerOs::Linux), "Linux");
        assert_eq!(os_name(PeerOs::Unknown), "");
        let s = &ZH;
        assert_eq!(book_error(&BookError::EmptyName, s), s.err_group_empty);
        assert!(book_error(&BookError::NameTooLong, s).contains("40"));
        assert_eq!(
            book_error(&BookError::DuplicateName, s),
            s.err_group_duplicate
        );
        assert!(book_error(&BookError::TooDeep, s).contains('4'));
        assert_eq!(book_error(&BookError::NotFound, s), s.err_gone);
        assert!(book_error(&BookError::Io("磁盘满了".into()), s).ends_with("磁盘满了"));
        let id = fake_id();
        assert_ne!(Editing::RenameGroup(id), Editing::RenameDevice(id));
        assert_ne!(Editing::NewGroup { parent: None }, Editing::RenameGroup(id));
    }

    fn fake_id() -> crate::gui::book::Id {
        let dir = std::env::temp_dir().join(format!("peerdesk-home-{}", std::process::id()));
        let mut book = crate::gui::book::Book::load(&dir, 0);
        let id = book.add_group(None, "x", 0).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        id
    }
}
