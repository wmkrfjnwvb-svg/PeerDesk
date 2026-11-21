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
    },
    /// 结束了。
    Ended {
        /// 给用户看的结束原因。
        status: Status,
        /// 会话总结（失败时没有）。
        summary: Option<Summary>,
        /// 结束前连到过的路径。
        route: Option<Route>,
    },
}

impl ViewPhase {
    /// 当前路径。
    #[must_use]
    pub const fn route(&self) -> Option<Route> {
        match self {
            ViewPhase::Connected { route, .. } => Some(*route),
            ViewPhase::Ended { route, .. } => *route,
            ViewPhase::Connecting => None,
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
            } => ViewPhase::Connected {
                peer: peer.clone(),
                remote: *remote,
                route: *route,
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
                }
            }
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
            },
            s,
        );
        assert_eq!(connected.route(), Some(Route::Relay));
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
            matches!(failed, ViewPhase::Ended { ref status, summary: None, route: None } if status.error)
        );

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
