//! 界面状态机：会话线程的事件怎样改变界面上显示的状态和状态栏。

use std::net::SocketAddr;

use peerdesk_core::signal::format_code;

use super::form::explain;
use super::i18n::Strings;
use super::session::{HostEvent, Summary, ViewEvent};
use peerdesk_net::rendezvous::Path as Route;

/// 状态栏的一条消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// 文字。
    pub text: String,
    /// 是否是错误。
    pub error: bool,
}

impl Status {
    /// 普通消息。
    #[must_use]
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
        }
    }

    /// 错误。
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: true,
        }
    }
}

/// 被控端处在哪个阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostPhase {
    /// 没有开启。
    Off,
    /// 正在开启。
    Starting,
    /// 在等连接。
    Listening {
        /// 端口。
        port: u16,
        /// IPv6 是否可用。
        ipv6: bool,
    },
    /// 有控制端连上来，正在验证，或者已经在控制。
    Busy {
        /// 端口。
        port: u16,
        /// IPv6 是否可用。
        ipv6: bool,
        /// 对端地址。
        addr: SocketAddr,
        /// 对端指纹。
        peer: String,
        /// 是否已经通过验证。
        established: bool,
    },
    /// 正在关闭。
    Stopping,
}

impl HostPhase {
    /// 开关应当显示为“开”。
    #[must_use]
    pub const fn is_on(&self) -> bool {
        match self {
            HostPhase::Off | HostPhase::Stopping => false,
            HostPhase::Starting | HostPhase::Listening { .. } | HostPhase::Busy { .. } => true,
        }
    }

    /// 正在监听的端口。
    #[must_use]
    pub const fn port(&self) -> Option<(u16, bool)> {
        match self {
            HostPhase::Listening { port, ipv6 } | HostPhase::Busy { port, ipv6, .. } => {
                Some((*port, *ipv6))
            }
            HostPhase::Off | HostPhase::Starting | HostPhase::Stopping => None,
        }
    }

    /// 处理一个事件，返回新状态和要显示的消息。
    #[must_use]
    pub fn apply(&self, event: &HostEvent, s: &Strings) -> (Self, Option<Status>) {
        let listening = || match self.port() {
            Some((port, ipv6)) => HostPhase::Listening { port, ipv6 },
            None => self.clone(),
        };
        match event {
            HostEvent::Listening { port, ipv6 } => (
                HostPhase::Listening {
                    port: *port,
                    ipv6: *ipv6,
                },
                Some(Status::info(format!("{} {}", s.host_listening, port))),
            ),
            HostEvent::Network(_) => (self.clone(), None),
            HostEvent::Registered { observed, code } => (
                self.clone(),
                Some(Status::info(format!(
                    "{} {}（{observed}）",
                    s.registered,
                    format_code(*code)
                ))),
            ),
            HostEvent::RegisterFailed(e) | HostEvent::HandshakeFailed(e) => {
                (self.clone(), Some(Status::error(explain(e, s))))
            }
            HostEvent::Peer { addr, fingerprint } => match self.port() {
                Some((port, ipv6)) => (
                    HostPhase::Busy {
                        port,
                        ipv6,
                        addr: *addr,
                        peer: fingerprint.clone(),
                        established: false,
                    },
                    None,
                ),
                None => (self.clone(), None),
            },
            HostEvent::Established => match self {
                HostPhase::Busy {
                    port,
                    ipv6,
                    addr,
                    peer,
                    ..
                } => (
                    HostPhase::Busy {
                        port: *port,
                        ipv6: *ipv6,
                        addr: *addr,
                        peer: peer.clone(),
                        established: true,
                    },
                    Some(Status::info(format!("{} {addr}", s.host_controlled))),
                ),
                HostPhase::Off
                | HostPhase::Starting
                | HostPhase::Listening { .. }
                | HostPhase::Stopping => (self.clone(), None),
            },
            HostEvent::Unavailable(why) => (
                HostPhase::Off,
                Some(Status::error(format!("{}（{why}）", s.host_unavailable))),
            ),
            HostEvent::SessionEnded(Ok(end)) => (
                listening(),
                Some(Status::info(format!("{}：{end}", s.session_ended))),
            ),
            HostEvent::SessionEnded(Err(e)) => (listening(), Some(Status::error(explain(e, s)))),
            // 已经关了（例如因为没有权限）：保留之前的说明，不用“已关闭”盖掉它。
            HostEvent::Stopped(None) => (
                HostPhase::Off,
                (*self != HostPhase::Off).then(|| Status::info(s.host_off)),
            ),
            HostEvent::Stopped(Some(e)) => (HostPhase::Off, Some(Status::error(explain(e, s)))),
        }
    }
}

/// 观看会话处在哪个阶段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewPhase {
    /// 正在连接。
    Connecting,
    /// 已连接。
    Connected {
        /// 对端指纹。
        peer: String,
        /// 对端地址。
        remote: SocketAddr,
        /// 直连还是中继。
        route: Route,
        /// 路径的具体说明（例如“公网直连（打洞） 23.4 ms”），直接连地址时为空。
        detail: String,
    },
    /// 网络断了，正在换通道重连。
    Reconnecting {
        /// 第几次重连。
        attempt: u32,
        /// 断开的原因。
        why: String,
    },
    /// 结束了。
    Ended {
        /// 给用户看的结束原因。
        status: Status,
        /// 会话总结（失败时没有）。
        summary: Option<Summary>,
        /// 结束前连到过的路径。
        route: Option<Route>,
        /// 服务器的通知（例如免费额度用完），界面弹框显示，附升级会员的入口。
        notice: Option<String>,
    },
}

/// 剪贴板事件要给用户看的话；不需要提示的返回 `None`。
#[must_use]
pub fn clip_message(note: &peerdesk_net::clipsync::ClipNote, s: &Strings) -> Option<String> {
    use peerdesk_net::clipsync::ClipNote;
    let mb = |size: u64| format!("{:.1}", size as f64 / 1_048_576.0);
    match note {
        ClipNote::LocalTooLarge { size, .. } => {
            Some(s.clip_local_too_large.replace("{mb}", &mb(*size)))
        }
        ClipNote::RemoteTooLarge { size, .. } => {
            Some(s.clip_remote_too_large.replace("{mb}", &mb(*size)))
        }
        ClipNote::Received { .. } => None,
    }
}

impl ViewPhase {
    /// 当前路径。
    #[must_use]
    pub const fn route(&self) -> Option<Route> {
        match self {
            ViewPhase::Connected { route, .. } => Some(*route),
            ViewPhase::Ended { route, .. } => *route,
            ViewPhase::Connecting | ViewPhase::Reconnecting { .. } => None,
        }
    }

    /// 路径的具体说明（没有时为空）。
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            ViewPhase::Connected { detail, .. } => detail,
            ViewPhase::Ended { .. } | ViewPhase::Connecting | ViewPhase::Reconnecting { .. } => "",
        }
    }

    /// 处理一个事件，返回新状态。
    #[must_use]
    pub fn apply(&self, event: &ViewEvent, s: &Strings) -> Self {
        match event {
            ViewEvent::Connected {
                peer,
                remote,
                route,
                detail,
            } => ViewPhase::Connected {
                peer: peer.clone(),
                remote: *remote,
                route: *route,
                detail: detail.clone(),
            },
            ViewEvent::Ended(result) => {
                let status = match result {
                    Ok(summary) if summary.cancelled => Status::info(s.you_disconnected),
                    Ok(summary) => Status::info(format!("{}：{}", s.session_ended, summary.end)),
                    Err(e) => Status::error(explain(e, s)),
                };
                ViewPhase::Ended {
                    status,
                    summary: result.as_ref().ok().cloned(),
                    route: self.route(),
                    notice: None,
                }
            }
            ViewEvent::Reconnecting { attempt, why } => ViewPhase::Reconnecting {
                attempt: *attempt,
                why: why.clone(),
            },
            ViewEvent::Clipboard(_) => self.clone(),
            ViewEvent::Notice(text) => ViewPhase::Ended {
                status: Status::error(text.clone()),
                summary: None,
                route: self.route(),
                notice: Some(text.clone()),
            },
        }
    }

    /// 状态栏里显示的一句话（连接中、重连中、结束原因）。
    #[must_use]
    pub fn headline(&self, s: &Strings) -> String {
        match self {
            ViewPhase::Connecting => s.connecting.to_owned(),
            ViewPhase::Connected { .. } => String::new(),
            ViewPhase::Reconnecting { attempt, why } => {
                format!(
                    "{}（{attempt}/{}）：{why}",
                    s.reconnecting,
                    crate::gui::session::RECONNECT_TRIES
                )
            }
            ViewPhase::Ended { status, .. } => status.text.clone(),
        }
    }

    /// 是否已经结束。
    #[must_use]
    pub const fn is_ended(&self) -> bool {
        matches!(self, ViewPhase::Ended { .. })
    }
}

/// 路径的名字。
#[must_use]
pub const fn route_name(route: Option<Route>, s: &Strings) -> &'static str {
    match route {
        Some(Route::Direct) => s.direct,
        Some(Route::Relay) => s.relay,
        None => s.not_available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_notes() {
        use peerdesk_core::clipboard::ClipKind;
        use peerdesk_net::clipsync::ClipNote;
        let s = &crate::gui::i18n::ZH;
        let big = ClipNote::LocalTooLarge {
            kind: ClipKind::Png,
            size: 20 * 1_048_576,
        };
        assert!(clip_message(&big, s).unwrap().contains("20.0 MB"));
        let remote = ClipNote::RemoteTooLarge {
            kind: ClipKind::Text,
            size: 5 * 1_048_576,
        };
        assert!(clip_message(&remote, s).unwrap().contains("对方"));
        let ok = ClipNote::Received {
            kind: ClipKind::Text,
            size: 1,
        };
        assert_eq!(clip_message(&ok, s), None);
        // 剪贴板事件不改变会话状态
        let phase = ViewPhase::Connecting;
        assert_eq!(phase.apply(&ViewEvent::Clipboard(ok), s), phase);
    }
    use crate::gui::i18n::EN;
    use peerdesk_net::NetError;

    fn addr() -> SocketAddr {
        "10.0.0.5:5000".parse().unwrap()
    }

    #[test]
    fn host_lifecycle() {
        let s = &EN;
        let off = HostPhase::Off;
        assert!(!off.is_on());
        assert!(HostPhase::Starting.is_on());
        assert!(!HostPhase::Stopping.is_on());
        // 没在监听时来的连接事件不改变状态
        assert_eq!(
            off.apply(
                &HostEvent::Peer {
                    addr: addr(),
                    fingerprint: "ab".into()
                },
                s
            )
            .0,
            off
        );
        assert_eq!(off.apply(&HostEvent::Established, s).0, off);

        let (listening, status) = HostPhase::Starting.apply(
            &HostEvent::Listening {
                port: 4433,
                ipv6: true,
            },
            s,
        );
        assert_eq!(
            listening,
            HostPhase::Listening {
                port: 4433,
                ipv6: true
            }
        );
        assert!(listening.is_on());
        assert_eq!(status.unwrap().text, "Waiting for a connection 4433");
        assert_eq!(listening.port(), Some((4433, true)));

        let (busy, status) = listening.apply(
            &HostEvent::Peer {
                addr: addr(),
                fingerprint: "ab".into(),
            },
            s,
        );
        assert!(status.is_none());
        assert!(matches!(
            busy,
            HostPhase::Busy {
                established: false,
                ..
            }
        ));
        let (busy, status) = busy.apply(&HostEvent::Established, s);
        assert!(matches!(
            busy,
            HostPhase::Busy {
                established: true,
                ..
            }
        ));
        assert!(status.unwrap().text.contains("10.0.0.5:5000"));

        let (back, status) = busy.apply(&HostEvent::SessionEnded(Ok("bye".into())), s);
        assert_eq!(back, listening);
        assert!(!status.unwrap().error);
        let (back, status) = busy.apply(
            &HostEvent::SessionEnded(Err(NetError::Connect("密码错误".into()))),
            s,
        );
        assert_eq!(back, listening);
        assert!(status.unwrap().text.starts_with(EN.err_wrong_password));
        // 关闭中收到会话结束：保持关闭中
        assert_eq!(
            HostPhase::Stopping
                .apply(&HostEvent::SessionEnded(Ok(String::new())), s)
                .0,
            HostPhase::Stopping
        );

        let (same, status) = listening.apply(
            &HostEvent::Registered {
                observed: addr(),
                code: 123_456_789,
            },
            s,
        );
        assert_eq!(same, listening);
        assert!(!status.unwrap().error);
        for e in [
            HostEvent::RegisterFailed(NetError::Timeout("x")),
            HostEvent::HandshakeFailed(NetError::Timeout("x")),
        ] {
            let (same, status) = listening.apply(&e, s);
            assert_eq!(same, listening);
            assert!(status.unwrap().error);
        }
        let (off, status) = listening.apply(&HostEvent::Stopped(None), s);
        assert_eq!(off, HostPhase::Off);
        assert!(!status.unwrap().error);
        let (off, status) = HostPhase::Starting.apply(
            &HostEvent::Stopped(Some(NetError::Io("绑定 x 失败".into()))),
            s,
        );
        assert_eq!(off, HostPhase::Off);
        assert!(status.unwrap().text.starts_with(EN.err_port_in_use));
    }

    #[test]
    fn no_permission_means_no_remote() {
        let s = &EN;
        let listening = HostPhase::Listening {
            port: 1,
            ipv6: false,
        };
        let (phase, status) =
            listening.apply(&HostEvent::Unavailable("screen recording".into()), s);
        assert_eq!(phase, HostPhase::Off, "没有权限：被控关闭");
        let status = status.unwrap();
        assert!(status.text.contains("screen recording") && status.text.contains("Accessibility"));
        // 之后的“已停止”不盖掉这条说明
        assert_eq!(
            phase.apply(&HostEvent::Stopped(None), s),
            (HostPhase::Off, None)
        );
    }

    #[test]
    fn view_lifecycle() {
        let s = &EN;
        let connecting = ViewPhase::Connecting;
        assert_eq!(connecting.route(), None);
        assert!(!connecting.is_ended());
        let connected = connecting.apply(
            &ViewEvent::Connected {
                peer: "ab".into(),
                remote: addr(),
                route: Route::Relay,
                detail: "中继 30.0 ms".into(),
            },
            s,
        );
        assert_eq!(connected.route(), Some(Route::Relay));
        assert_eq!(connected.detail(), "中继 30.0 ms");
        assert_eq!(ViewPhase::Connecting.detail(), "");
        let summary = Summary {
            end: "peer left".into(),
            ..Summary::default()
        };
        let ended = connected.apply(&ViewEvent::Ended(Ok(summary.clone())), s);
        assert!(ended.is_ended());
        assert_eq!(ended.route(), Some(Route::Relay));
        assert!(
            matches!(ended, ViewPhase::Ended { ref status, summary: Some(_), .. } if status.text.contains("peer left"))
        );
        let cancelled = connected.apply(
            &ViewEvent::Ended(Ok(Summary {
                cancelled: true,
                ..summary
            })),
            s,
        );
        assert!(
            matches!(cancelled, ViewPhase::Ended { ref status, .. } if status.text == EN.you_disconnected)
        );
        let failed = connecting.apply(&ViewEvent::Ended(Err(NetError::Timeout("握手"))), s);
        assert!(
            matches!(failed, ViewPhase::Ended { ref status, summary: None, route: None, notice: None } if status.error)
        );
        // 网络断了：重连中；服务器通知：结束并带通知
        let again = connecting.apply(
            &ViewEvent::Reconnecting {
                attempt: 2,
                why: "timed out".into(),
            },
            s,
        );
        assert!(again.headline(s).contains("(2/3)") || again.headline(s).contains("（2/3）"));
        assert_eq!(again.route(), None);
        assert_eq!(again.detail(), "");
        assert!(!again.is_ended());
        let busy = again.apply(&ViewEvent::Notice("免费服务器拥挤".into()), s);
        assert!(
            matches!(busy, ViewPhase::Ended { notice: Some(ref t), .. } if t == "免费服务器拥挤")
        );
        assert_eq!(busy.headline(s), "免费服务器拥挤");
        assert_eq!(ViewPhase::Connecting.headline(s), EN.connecting);

        assert_eq!(route_name(Some(Route::Direct), s), "direct");
        assert_eq!(route_name(Some(Route::Relay), s), "relay");
        assert_eq!(route_name(None, s), "n/a");
        assert_eq!(
            Status::info("x"),
            Status {
                text: "x".into(),
                error: false
            }
        );
    }
}
