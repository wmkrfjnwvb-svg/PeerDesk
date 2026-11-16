//! 信令：设备向服务器登记自己在线，控制端通过服务器找到被控端。
//!
//! 服务器从每条信令连接上看到设备的公网地址（设备用同一个 UDP 端口做信令和点对点连接，
//! 所以这个地址就是它在 NAT 上的映射），在两端之间交换地址，并为它们开一条中继通道备用。
//! 设备的身份是它的证书指纹，由加密连接本身证明，消息里不需要再声明。

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::relay::SessionId;
use crate::wire::{Reader, Writer};
use crate::Error;

/// 设备指纹。
pub type DeviceId = [u8; 32];

/// 一条中继通道的使用凭据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayGrant {
    /// 中继的地址。
    pub addr: SocketAddr,
    /// 会话号。
    pub session: SessionId,
    /// 本端在这条通道里的方向（0 或 1）。
    pub side: u8,
}

/// 信令消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalMsg {
    /// 设备 → 服务器：我在线，可以被连接。
    Register,
    /// 服务器 → 设备：登记成功，这是我看到的你的公网地址。
    Registered {
        /// 服务器看到的设备地址。
        observed: SocketAddr,
    },
    /// 控制端 → 服务器：我要连这台设备。
    Connect {
        /// 目标设备的指纹。
        target: DeviceId,
    },
    /// 服务器 → 控制端：对方的地址，以及备用的中继通道。
    Peer {
        /// 被控端的公网地址。
        addr: SocketAddr,
        /// 中继通道。
        relay: Option<RelayGrant>,
    },
    /// 服务器 → 被控端：有人要连你，请向这个地址打洞。
    Incoming {
        /// 控制端的指纹。
        peer: DeviceId,
        /// 控制端的公网地址。
        addr: SocketAddr,
        /// 中继通道。
        relay: Option<RelayGrant>,
    },
    /// 服务器 → 控制端：目标设备不在线。
    NotFound,
}

fn put_addr(w: &mut Writer, addr: &SocketAddr) {
    match addr.ip() {
        IpAddr::V4(ip) => w.u8(4).bytes(&ip.octets()),
        IpAddr::V6(ip) => w.u8(6).bytes(&ip.octets()),
    };
    w.u16(addr.port());
}

fn get_addr(r: &mut Reader<'_>) -> Result<SocketAddr, Error> {
    let ip = match r.u8()? {
        4 => {
            let mut b = [0u8; 4];
            b.copy_from_slice(r.take(4)?);
            IpAddr::V4(Ipv4Addr::from(b))
        }
        6 => {
            let mut b = [0u8; 16];
            b.copy_from_slice(r.take(16)?);
            IpAddr::V6(Ipv6Addr::from(b))
        }
        _ => return Err(Error::Inconsistent("未知的地址族")),
    };
    Ok(SocketAddr::new(ip, r.u16()?))
}

fn put_grant(w: &mut Writer, grant: Option<&RelayGrant>) {
    match grant {
        None => {
            w.u8(0);
        }
        Some(g) => {
            w.u8(1);
            put_addr(w, &g.addr);
            w.bytes(&g.session).u8(g.side);
        }
    }
}

fn get_grant(r: &mut Reader<'_>) -> Result<Option<RelayGrant>, Error> {
    match r.u8()? {
        0 => Ok(None),
        1 => {
            let addr = get_addr(r)?;
            let mut session = [0u8; 16];
            session.copy_from_slice(r.take(16)?);
            let side = r.u8()?;
            if side > 1 {
                return Err(Error::Inconsistent("中继方向只能是 0 或 1"));
            }
            Ok(Some(RelayGrant {
                addr,
                session,
                side,
            }))
        }
        _ => Err(Error::Inconsistent("中继凭据标志不合法")),
    }
}

fn get_id(r: &mut Reader<'_>) -> Result<DeviceId, Error> {
    let mut id = [0u8; 32];
    id.copy_from_slice(r.take(32)?);
    Ok(id)
}

impl SignalMsg {
    /// 消息名。
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            SignalMsg::Register => "Register",
            SignalMsg::Registered { .. } => "Registered",
            SignalMsg::Connect { .. } => "Connect",
            SignalMsg::Peer { .. } => "Peer",
            SignalMsg::Incoming { .. } => "Incoming",
            SignalMsg::NotFound => "NotFound",
        }
    }

    /// 编码。
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            SignalMsg::Register => {
                w.u8(1);
            }
            SignalMsg::Registered { observed } => {
                w.u8(2);
                put_addr(&mut w, observed);
            }
            SignalMsg::Connect { target } => {
                w.u8(3).bytes(target);
            }
            SignalMsg::Peer { addr, relay } => {
                w.u8(4);
                put_addr(&mut w, addr);
                put_grant(&mut w, relay.as_ref());
            }
            SignalMsg::Incoming { peer, addr, relay } => {
                w.u8(5).bytes(peer);
                put_addr(&mut w, addr);
                put_grant(&mut w, relay.as_ref());
            }
            SignalMsg::NotFound => {
                w.u8(6);
            }
        }
        w.into_vec()
    }

    /// 解码。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let msg = match r.u8()? {
            1 => SignalMsg::Register,
            2 => SignalMsg::Registered {
                observed: get_addr(&mut r)?,
            },
            3 => SignalMsg::Connect {
                target: get_id(&mut r)?,
            },
            4 => SignalMsg::Peer {
                addr: get_addr(&mut r)?,
                relay: get_grant(&mut r)?,
            },
            5 => SignalMsg::Incoming {
                peer: get_id(&mut r)?,
                addr: get_addr(&mut r)?,
                relay: get_grant(&mut r)?,
            },
            6 => SignalMsg::NotFound,
            other => return Err(Error::UnknownMessage(other)),
        };
        r.finish()?;
        Ok(msg)
    }
}

/// 服务器端的在线设备表（不做 IO）。连接用一个由 IO 层分配的编号标识。
#[derive(Debug, Clone, Default)]
pub struct Registry {
    devices: BTreeMap<DeviceId, (u64, SocketAddr)>,
}

/// [`Registry::connect`] 的结果：要发给谁什么消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Introduction {
    /// 回给控制端的消息。
    pub to_controller: SignalMsg,
    /// 被控端的连接编号和要发给它的消息；目标不在线时为 `None`。
    pub to_host: Option<(u64, SignalMsg)>,
}

impl Registry {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
        }
    }

    /// 登记设备在线。同一台设备重新登记时以最新的连接为准。
    pub fn register(&mut self, conn: u64, device: DeviceId, observed: SocketAddr) -> SignalMsg {
        self.devices.insert(device, (conn, observed));
        SignalMsg::Registered { observed }
    }

    /// 一条连接断开：如果它是某台设备当前的登记连接，把设备标为离线。
    pub fn disconnect(&mut self, conn: u64) {
        self.devices.retain(|_, (c, _)| *c != conn);
    }

    /// 在线设备数。
    #[must_use]
    pub fn online(&self) -> usize {
        self.devices.len()
    }

    /// 控制端请求连接 `target`。`relay` 是为这次连接准备的中继地址和会话号（没有中继时为 `None`）。
    #[must_use]
    pub fn connect(
        &self,
        controller: DeviceId,
        controller_addr: SocketAddr,
        target: &DeviceId,
        relay: Option<(SocketAddr, SessionId)>,
    ) -> Introduction {
        let Some((conn, host_addr)) = self.devices.get(target) else {
            return Introduction {
                to_controller: SignalMsg::NotFound,
                to_host: None,
            };
        };
        let grant = |side| {
            relay.map(|(addr, session)| RelayGrant {
                addr,
                session,
                side,
            })
        };
        Introduction {
            to_controller: SignalMsg::Peer {
                addr: *host_addr,
                relay: grant(0),
            },
            to_host: Some((
                *conn,
                SignalMsg::Incoming {
                    peer: controller,
                    addr: controller_addr,
                    relay: grant(1),
                },
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn grant(side: u8) -> RelayGrant {
        RelayGrant {
            addr: addr("[2001:db8::1]:3478"),
            session: [5; 16],
            side,
        }
    }

    #[test]
    fn messages_roundtrip() {
        let all = [
            SignalMsg::Register,
            SignalMsg::Registered {
                observed: addr("203.0.113.9:40000"),
            },
            SignalMsg::Connect { target: [1; 32] },
            SignalMsg::Peer {
                addr: addr("[2001:db8::2]:1"),
                relay: Some(grant(0)),
            },
            SignalMsg::Peer {
                addr: addr("1.2.3.4:5"),
                relay: None,
            },
            SignalMsg::Incoming {
                peer: [2; 32],
                addr: addr("1.2.3.4:5"),
                relay: Some(grant(1)),
            },
            SignalMsg::NotFound,
        ];
        let mut names = Vec::new();
        for msg in all {
            assert_eq!(SignalMsg::decode(&msg.encode()).unwrap(), msg);
            names.push(msg.name());
        }
        assert_eq!(
            names,
            [
                "Register",
                "Registered",
                "Connect",
                "Peer",
                "Peer",
                "Incoming",
                "NotFound"
            ]
        );
    }

    #[test]
    fn malformed_messages_are_rejected() {
        assert_eq!(SignalMsg::decode(&[0]), Err(Error::UnknownMessage(0)));
        assert_eq!(SignalMsg::decode(&[1, 0]), Err(Error::TrailingBytes(1)));
        assert_eq!(
            SignalMsg::decode(&[2, 9]),
            Err(Error::Inconsistent("未知的地址族"))
        );
        assert!(matches!(
            SignalMsg::decode(&[2, 4, 1, 2]),
            Err(Error::Truncated { .. })
        ));
        assert!(matches!(
            SignalMsg::decode(&[3, 1, 2]),
            Err(Error::Truncated { .. })
        ));
        let mut bad_flag = SignalMsg::Peer {
            addr: addr("1.2.3.4:5"),
            relay: None,
        }
        .encode();
        *bad_flag.last_mut().unwrap() = 7;
        assert_eq!(
            SignalMsg::decode(&bad_flag),
            Err(Error::Inconsistent("中继凭据标志不合法"))
        );
        let mut bad_side = SignalMsg::Peer {
            addr: addr("1.2.3.4:5"),
            relay: Some(grant(0)),
        }
        .encode();
        *bad_side.last_mut().unwrap() = 2;
        assert_eq!(
            SignalMsg::decode(&bad_side),
            Err(Error::Inconsistent("中继方向只能是 0 或 1"))
        );
        let good = SignalMsg::Incoming {
            peer: [2; 32],
            addr: addr("1.2.3.4:5"),
            relay: Some(grant(1)),
        }
        .encode();
        for len in 0..good.len() {
            assert!(SignalMsg::decode(&good[..len]).is_err(), "{len}");
        }
    }

    #[test]
    fn registry_introduces_peers_with_matching_relay_sides() {
        let mut reg = Registry::new();
        let (host, ctrl) = ([1; 32], [2; 32]);
        assert_eq!(
            reg.register(10, host, addr("198.51.100.1:5000")),
            SignalMsg::Registered {
                observed: addr("198.51.100.1:5000")
            }
        );
        assert_eq!(reg.online(), 1);

        let relay = Some((addr("203.0.113.1:3478"), [8; 16]));
        let intro = reg.connect(ctrl, addr("192.0.2.7:6000"), &host, relay);
        match (&intro.to_controller, &intro.to_host) {
            (
                SignalMsg::Peer {
                    addr: host_addr,
                    relay: Some(c),
                },
                Some((
                    10,
                    SignalMsg::Incoming {
                        peer,
                        addr: ctrl_addr,
                        relay: Some(h),
                    },
                )),
            ) => {
                assert_eq!(*host_addr, addr("198.51.100.1:5000"));
                assert_eq!(*ctrl_addr, addr("192.0.2.7:6000"));
                assert_eq!(*peer, ctrl);
                assert_eq!((c.session, h.session), ([8; 16], [8; 16]));
                assert_eq!((c.side, h.side), (0, 1), "两端拿到同一条通道的两个方向");
            }
            other => panic!("{other:?}"),
        }
        // 没有中继可用
        let intro = reg.connect(ctrl, addr("192.0.2.7:6000"), &host, None);
        assert!(matches!(
            intro.to_controller,
            SignalMsg::Peer { relay: None, .. }
        ));

        // 目标不在线
        let intro = reg.connect(ctrl, addr("192.0.2.7:6000"), &[9; 32], relay);
        assert_eq!(
            intro,
            Introduction {
                to_controller: SignalMsg::NotFound,
                to_host: None
            }
        );

        // 重新登记：以新连接为准；旧连接断开不影响
        reg.register(11, host, addr("198.51.100.1:5001"));
        reg.disconnect(10);
        assert_eq!(reg.online(), 1);
        reg.disconnect(11);
        assert_eq!(reg.online(), 0);
    }
}
