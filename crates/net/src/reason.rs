//! 连接失败、会话结束、断线重连的原因：一个小枚举，桌面端和手机端共用。
//!
//! 为什么要有它：核心报出的原因是写给开发者的原文（常常夹着英文的系统错误，例如 QUIC 的 `timed out`），
//! 直接给用户看，既看不懂也不知道该怎么办。界面按 [`Reason`] 用本地语言说一句原因、一句办法，
//! 最多两个按钮（[`Reason::actions`]）；原文留作“详情”，可以复制给开发者。
//!
//! 原因从哪来：
//! - 能从结构上判断的直接判断：错误的种类（超时、指纹不符、连不上服务器）、重连时是网络断了还是重拨失败
//!   （见 `viewer::drive`）。
//! - 其余的从本仓库代码自己写的说明文字里认关键词：被控端拒绝、密码错、设备不在线这些说明都出自这里的代码
//!   （[`reject_text`] 等），老版本的对端发来的也是同样的文字。关键词只在这里列一次，`viewer::network_lost`
//!   也用它判断要不要重连。测试用真实的连接（`tests/auth.rs`、`tests/misbehave.rs` 等）核对每种说明认得出来。
//! - 认不出来的是 [`Reason::Other`]：界面说“出错了”，原文放进详情，不猜。

use peerdesk_core::proto::RejectReason;

use crate::NetError;

/// 一次连接、一次会话为什么失败或结束。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    /// 找不到通往对方的路：连接超时，打洞不通又没有中继。
    NoRoute,
    /// 对方不在线，或者设备码不存在（服务器上没有这台设备）。
    DeviceOffline,
    /// 连不上服务器（本机断网、服务器停了、防火墙挡住了，或者服务器地址填错了）。
    ServerUnreachable,
    /// 密码不对（或者对方要求密码而这边没填）。
    WrongPassword,
    /// 对方因为多次密码错误，暂时不接受连接。
    Locked,
    /// 对方正在被别的设备控制。
    Busy,
    /// 对方拒绝了这次连接（没有更具体的原因）。
    Declined,
    /// 对方现在打不开屏幕或键鼠：缺系统权限（屏幕录制、辅助功能），或者在锁屏、登录界面。
    HostPermission,
    /// 双方的 PeerDesk 版本不兼容。
    Version,
    /// 对方的身份（证书指纹）和预期的不一致：可能连错了设备，也可能有人冒充。
    IdentityMismatch,
    /// 对方结束了会话。
    PeerClosed,
    /// 对方退出了 PeerDesk。
    PeerQuit,
    /// 网络断了（换了网络、Wi-Fi 断开），正在或曾经自动重连。
    NetworkLost,
    /// 免费中继的流量额度暂时用完了（服务器的通知）。
    RelayQuota,
    /// 本机端口被占用（例如另一个 PeerDesk 正在运行）。
    PortInUse,
    /// 本机用户自己断开的（不是问题）。
    Cancelled,
    /// 其他原因：原文在详情里。
    Other,
}

/// 失败提示里可以放的按钮。界面显示其中自己支持的（桌面端没有“导出诊断记录”，换成“复制详情”）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// 再试一次。
    Retry,
    /// 去看服务器设置（地址、指纹）。
    ServerSettings,
    /// 把详情交给开发者：手机端导出诊断记录，桌面端复制详情。
    Diagnostics,
}

impl Action {
    /// 稳定的代号（手机端的事件里用）。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::ServerSettings => "server_settings",
            Self::Diagnostics => "diagnostics",
        }
    }
}

/// 被控端拒绝连接时的说明（控制端、被控端的日志和详情里都用这一句，[`Reason::of_text`] 认得出来）。
#[must_use]
pub const fn reject_text(reason: RejectReason) -> &'static str {
    match reason {
        RejectReason::Unauthorized => "密码错误，或被控端需要密码",
        RejectReason::Locked => "被控端因多次认证失败暂时锁定",
        RejectReason::Busy => "被控端正在被其他设备控制",
        RejectReason::Version => "对方的 PeerDesk 版本太旧，不能连接，请先升级对方",
        RejectReason::NoCommonSuite => {
            "被控端拒绝：双方没有共同支持的加密套件，版本可能不兼容（NoCommonSuite）"
        }
    }
}

/// 服务器连不上时错误原文的开头（见 [`NetError::Unreachable`]）。
pub const SERVER_UNREACHABLE: &str = "连不上服务器";
/// 免费额度用完的通知原文的开头（见 `viewer::free_limit_text`）。
pub const FREE_LIMIT: &str = "免费服务器拥挤";
/// 对端发来 Bye（正常结束会话）时，会话报告里的结束原因。
pub const PEER_ENDED: &str = "对端结束会话";

/// 原文里出现这些词，说明是网络断了（应当换通道重连），而不是对方或自己结束的。
const NETWORK_LOST: [&str; 5] = [
    "timed out",
    "超时",
    "connection lost",
    "reset",
    "closed by peer: 0",
];

impl Reason {
    /// 全部原因（测试、界面文字表逐个核对用）。
    pub const ALL: [Self; 17] = [
        Self::NoRoute,
        Self::DeviceOffline,
        Self::ServerUnreachable,
        Self::WrongPassword,
        Self::Locked,
        Self::Busy,
        Self::Declined,
        Self::HostPermission,
        Self::Version,
        Self::IdentityMismatch,
        Self::PeerClosed,
        Self::PeerQuit,
        Self::NetworkLost,
        Self::RelayQuota,
        Self::PortInUse,
        Self::Cancelled,
        Self::Other,
    ];

    /// 稳定的代号：手机端事件 JSON 的 `reason` 字段，界面按它取文字。只增不改。
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::NoRoute => "no_route",
            Self::DeviceOffline => "device_offline",
            Self::ServerUnreachable => "server_unreachable",
            Self::WrongPassword => "wrong_password",
            Self::Locked => "locked",
            Self::Busy => "busy",
            Self::Declined => "declined",
            Self::HostPermission => "host_permission",
            Self::Version => "version",
            Self::IdentityMismatch => "identity_mismatch",
            Self::PeerClosed => "peer_closed",
            Self::PeerQuit => "peer_quit",
            Self::NetworkLost => "network_lost",
            Self::RelayQuota => "relay_quota",
            Self::PortInUse => "port_in_use",
            Self::Cancelled => "cancelled",
            Self::Other => "other",
        }
    }

    /// 代号换回原因；不认识的代号（更新的版本加的）为 `None`。
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.code() == code)
    }

    /// 提示里的按钮（最多两个，主要的在前）。
    #[must_use]
    pub const fn actions(self) -> &'static [Action] {
        match self {
            // 网络、路径的问题：先重试；还不行就把详情交给开发者
            Self::NoRoute | Self::NetworkLost | Self::Other => {
                &[Action::Retry, Action::Diagnostics]
            }
            // 服务器连不上：可能是地址填错了
            Self::ServerUnreachable => &[Action::Retry, Action::ServerSettings],
            // 改了密码、对方处理好了、等一会儿，再试
            Self::DeviceOffline
            | Self::WrongPassword
            | Self::Locked
            | Self::Busy
            | Self::Declined
            | Self::HostPermission
            | Self::PeerClosed
            | Self::PeerQuit
            | Self::RelayQuota => &[Action::Retry],
            // 指纹不符：不鼓励重试，交给开发者看详情
            Self::IdentityMismatch => &[Action::Diagnostics],
            // 要对方升级、换端口：重试没用
            Self::Version | Self::PortInUse | Self::Cancelled => &[],
        }
    }

    /// 网络层错误的原因：先看错误的种类，再看原文。
    #[must_use]
    pub fn of_error(e: &NetError) -> Self {
        match e {
            NetError::Timeout(_) => Self::NoRoute,
            NetError::Unreachable(_) => Self::ServerUnreachable,
            NetError::PinMismatch { .. } => Self::IdentityMismatch,
            NetError::Core(
                peerdesk_core::Error::UnsupportedVersion(_)
                | peerdesk_core::Error::NoCommonSuite
                | peerdesk_core::Error::UnknownSuite(_),
            ) => Self::Version,
            // 还没连上就失败：原文里的“超时”说的是连不过去，不是“网络断了”
            NetError::Connect(why) => {
                let reason = Self::of_text(why);
                if reason == Self::NetworkLost {
                    Self::NoRoute
                } else {
                    reason
                }
            }
            NetError::Closed(why) => Self::of_text(why),
            NetError::Io(why) if why.contains("绑定") => Self::PortInUse,
            NetError::Io(_) | NetError::Tls(_) | NetError::Core(_) | NetError::Protocol(_) => {
                Self::Other
            }
        }
    }

    /// 从原文认原因（会话的结束原因、重连的原因、被控端拒绝的说明）。认不出来是 [`Self::Other`]。
    #[must_use]
    pub fn of_text(text: &str) -> Self {
        let has = |words: &[&str]| words.iter().any(|w| text.contains(w));
        if has(&[crate::node::END_PEER_QUIT, crate::node::QUIT_REASON]) {
            Self::PeerQuit
        } else if text.starts_with(FREE_LIMIT) {
            Self::RelayQuota
        } else if has(&[SERVER_UNREACHABLE, "找不到服务器", "没有配置服务器"]) {
            Self::ServerUnreachable
        } else if has(&[PEER_ENDED, "对端在握手阶段结束了会话"]) {
            Self::PeerClosed
        } else if has(&NETWORK_LOST) {
            Self::NetworkLost
        } else if has(&["密码"]) {
            Self::WrongPassword
        } else if has(&["锁定"]) {
            Self::Locked
        } else if has(&["其他设备"]) {
            Self::Busy
        } else if has(&["版本"]) {
            Self::Version
        } else if has(&["权限", "permission", "屏幕", "screen", "键鼠"]) {
            Self::HostPermission
        } else if has(&["拒绝"]) {
            Self::Declined
        } else if has(&["不在线", "不存在"]) {
            Self::DeviceOffline
        } else if has(&["中继", "打洞", "NAT", "no route"]) {
            Self::NoRoute
        } else {
            Self::Other
        }
    }
}

/// 结束原因是不是网络断了（应当换通道重连），而不是对方或自己结束的。
#[must_use]
pub fn network_lost(text: &str) -> bool {
    Reason::of_text(text) == Reason::NetworkLost
}

/// 一次失败或结束：原因，加上核心的原文（界面放进“详情”，可以复制）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    /// 原因。
    pub reason: Reason,
    /// 原文。
    pub details: String,
}

impl Failure {
    /// 网络层错误。
    #[must_use]
    pub fn of_error(e: &NetError) -> Self {
        Self {
            reason: Reason::of_error(e),
            details: e.to_string(),
        }
    }

    /// 原文。
    #[must_use]
    pub fn of_text(text: &str) -> Self {
        Self {
            reason: Reason::of_text(text),
            details: text.to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_round_trip() {
        for r in Reason::ALL {
            assert_eq!(Reason::from_code(r.code()), Some(r));
            assert_eq!(
                Reason::ALL.iter().filter(|o| o.code() == r.code()).count(),
                1,
                "{r:?}"
            );
            assert!(r.actions().len() <= 2, "{r:?}：最多两个按钮");
        }
        assert_eq!(Reason::from_code("from_the_future"), None);
        let codes: Vec<_> = [Action::Retry, Action::ServerSettings, Action::Diagnostics]
            .iter()
            .map(|a| a.code())
            .collect();
        assert_eq!(codes, ["retry", "server_settings", "diagnostics"]);
    }

    #[test]
    fn reject_texts_are_recognized() {
        let want = [
            (RejectReason::Unauthorized, Reason::WrongPassword),
            (RejectReason::Locked, Reason::Locked),
            (RejectReason::Busy, Reason::Busy),
            (RejectReason::Version, Reason::Version),
            (RejectReason::NoCommonSuite, Reason::Version),
        ];
        for (reject, reason) in want {
            assert_eq!(
                Reason::of_error(&NetError::Connect(reject_text(reject).into())),
                reason,
                "{reject:?}"
            );
        }
        assert_eq!(want.len(), RejectReason::ALL.len());
    }

    #[test]
    fn texts_from_the_core_are_recognized() {
        let cases = [
            (crate::node::END_PEER_QUIT, Reason::PeerQuit),
            ("closed by peer: 3 (peerdesk-quit)", Reason::PeerQuit),
            ("免费服务器拥挤，请约 2 分钟后重试", Reason::RelayQuota),
            ("连不上服务器：超时：握手", Reason::ServerUnreachable),
            ("找不到服务器 x.invalid：dns", Reason::ServerUnreachable),
            (PEER_ENDED, Reason::PeerClosed),
            ("对端结束会话：timed out", Reason::PeerClosed),
            ("对端在握手阶段结束了会话", Reason::PeerClosed),
            ("timed out", Reason::NetworkLost),
            (
                "connection lost：10 秒没有收到对端的数据",
                Reason::NetworkLost,
            ),
            ("connection reset", Reason::NetworkLost),
            ("closed by peer: 0", Reason::NetworkLost),
            (
                "对方拒绝了连接：缺少权限：没有“屏幕录制”权限",
                Reason::HostPermission,
            ),
            (
                "对方拒绝了连接：host cannot share its screen",
                Reason::HostPermission,
            ),
            (
                "对方拒绝了连接：无法控制本机键鼠：x",
                Reason::HostPermission,
            ),
            ("对方拒绝了连接：别的原因", Reason::Declined),
            (
                "设备码 123 456 789 不存在或设备不在线",
                Reason::DeviceOffline,
            ),
            ("设备不在线", Reason::DeviceOffline),
            ("服务器没有提供中继", Reason::NoRoute),
            ("两端的 NAT 都是对称型（换目的地就换端口）", Reason::NoRoute),
            ("多端口打洞没有撞上", Reason::NoRoute),
            ("something odd", Reason::Other),
        ];
        for (text, reason) in cases {
            assert_eq!(Reason::of_text(text), reason, "{text}");
            assert_eq!(Failure::of_text(text).details, text);
        }
    }

    #[test]
    fn errors_are_classified_by_kind_first() {
        let cases = [
            (NetError::Timeout("握手"), Reason::NoRoute),
            (
                NetError::Unreachable("超时：握手".into()),
                Reason::ServerUnreachable,
            ),
            (
                NetError::PinMismatch {
                    expected: "a".into(),
                    actual: "b".into(),
                },
                Reason::IdentityMismatch,
            ),
            (
                NetError::Core(peerdesk_core::Error::UnsupportedVersion(9)),
                Reason::Version,
            ),
            (
                NetError::Core(peerdesk_core::Error::NoCommonSuite),
                Reason::Version,
            ),
            (
                NetError::Core(peerdesk_core::Error::UnknownSuite(7)),
                Reason::Version,
            ),
            (NetError::Core(peerdesk_core::Error::BadUtf8), Reason::Other),
            // 连接阶段的“timed out”是连不过去
            (NetError::Connect("timed out".into()), Reason::NoRoute),
            (
                NetError::Connect("设备不在线".into()),
                Reason::DeviceOffline,
            ),
            // 会话中断开、重连用完：网络断了
            (NetError::Closed("timed out".into()), Reason::NetworkLost),
            (NetError::Closed(PEER_ENDED.into()), Reason::PeerClosed),
            (
                NetError::Io("绑定 0.0.0.0:1 失败：in use".into()),
                Reason::PortInUse,
            ),
            (NetError::Io("disk".into()), Reason::Other),
            (NetError::Tls("x".into()), Reason::Other),
            (NetError::Protocol("x"), Reason::Other),
        ];
        for (e, reason) in cases {
            assert_eq!(Reason::of_error(&e), reason, "{e:?}");
            let f = Failure::of_error(&e);
            assert_eq!((f.reason, f.details), (reason, e.to_string()));
        }
    }

    #[test]
    fn network_lost_keeps_its_meaning() {
        assert!(network_lost("timed out") && network_lost("connection lost"));
        assert!(network_lost("超时") && network_lost("reset by peer"));
        assert!(!network_lost("对端结束会话：timed out") && !network_lost("用户结束"));
        assert!(!network_lost(crate::node::END_PEER_QUIT));
    }
}
