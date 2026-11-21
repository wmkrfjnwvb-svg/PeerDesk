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
    /// 服务器 → 设备：登记成功，这是我看到的你的公网地址和你的设备码。
    Registered {
        /// 服务器看到的设备地址。
        observed: SocketAddr,
        /// 服务器给这台设备分配的 9 位设备码，同一台设备每次登记都一样。
        code: u32,
    },
    /// 控制端 → 服务器：我要连这台设备。
    Connect {
        /// 目标设备的指纹。
        target: DeviceId,
    },
    /// 控制端 → 服务器：我要连设备码是 `code` 的设备。
    ConnectCode {
        /// 设备码。
        code: u32,
    },
    /// 服务器 → 控制端：对方的身份和地址，以及备用的中继通道。
    Peer {
        /// 被控端的指纹。控制端用它核对连上的是不是这台设备。
        device: DeviceId,
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
            SignalMsg::ConnectCode { .. } => "ConnectCode",
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
            SignalMsg::Registered { observed, code } => {
                w.u8(2);
                put_addr(&mut w, observed);
                w.u32(*code);
            }
            SignalMsg::Connect { target } => {
                w.u8(3).bytes(target);
            }
            SignalMsg::Peer {
                device,
                addr,
                relay,
            } => {
                w.u8(4).bytes(device);
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
            SignalMsg::ConnectCode { code } => {
                w.u8(7).u32(*code);
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
                code: r.u32()?,
            },
            3 => SignalMsg::Connect {
                target: get_id(&mut r)?,
            },
            4 => SignalMsg::Peer {
                device: get_id(&mut r)?,
                addr: get_addr(&mut r)?,
                relay: get_grant(&mut r)?,
            },
            5 => SignalMsg::Incoming {
                peer: get_id(&mut r)?,
                addr: get_addr(&mut r)?,
                relay: get_grant(&mut r)?,
            },
            6 => SignalMsg::NotFound,
            7 => SignalMsg::ConnectCode { code: r.u32()? },
            other => return Err(Error::UnknownMessage(other)),
        };
        r.finish()?;
        Ok(msg)
    }
}

/// 设备码的范围：9 位数，首位不为 0。
pub const CODE_MIN: u32 = 100_000_000;
/// 设备码的上限（含）。
pub const CODE_MAX: u32 = 999_999_999;

/// 把设备码显示成 `123 456 789`。
#[must_use]
pub fn format_code(code: u32) -> String {
    format!(
        "{:03} {:03} {:03}",
        code / 1_000_000,
        code / 1000 % 1000,
        code % 1000
    )
}

/// 解析用户输入的设备码：忽略空格和短横线，必须正好是 9 位数。
#[must_use]
pub fn parse_code(text: &str) -> Option<u32> {
    let digits: String = text.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    if digits.len() != 9 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits
        .parse()
        .ok()
        .filter(|c| (CODE_MIN..=CODE_MAX).contains(c))
}

/// 服务器端的在线设备表和设备码簿（不做 IO）。连接用一个由 IO 层分配的编号标识。
///
/// 设备码一经分配就固定在这台设备（指纹）上，码簿由 IO 层存盘，服务器重启后不变。
#[derive(Debug, Clone, Default)]
pub struct Registry {
    devices: BTreeMap<DeviceId, (u64, SocketAddr)>,
    codes: BTreeMap<DeviceId, u32>,
    owners: BTreeMap<u32, DeviceId>,
}

/// [`Registry::connect`] 的结果：要发给谁什么消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Introduction {
    /// 回给控制端的消息。
    pub to_controller: SignalMsg,
    /// 被控端的连接编号和要发给它的消息；目标不在线时为 `None`。
    pub to_host: Option<(u64, SignalMsg)>,
}

/// [`Registry::register`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    /// 回给设备的消息。
    pub reply: SignalMsg,
    /// 设备码。
    pub code: u32,
    /// 这次登记新分配了设备码，码簿需要存盘。
    pub new_code: bool,
}

impl Registry {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            codes: BTreeMap::new(),
            owners: BTreeMap::new(),
        }
    }

    /// 从存盘的码簿恢复（每行 `设备码 指纹十六进制`）。返回读不懂的行数，这些行被跳过。
    pub fn load_codes(&mut self, text: &str) -> usize {
        let mut bad = 0;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut parts = line.split_whitespace();
            let code = parts.next().and_then(|c| c.parse::<u32>().ok());
            let id = parts.next().and_then(parse_id);
            match (code, id, parts.next()) {
                (Some(code), Some(id), None)
                    if (CODE_MIN..=CODE_MAX).contains(&code)
                        && !self.owners.contains_key(&code)
                        && !self.codes.contains_key(&id) =>
                {
                    self.codes.insert(id, code);
                    self.owners.insert(code, id);
                }
                _ => bad += 1,
            }
        }
        bad
    }

    /// 码簿的存盘文本，格式见 [`Registry::load_codes`]。
    #[must_use]
    pub fn codes_text(&self) -> String {
        let mut out = String::new();
        for (code, id) in &self.owners {
            out.push_str(&format!("{code} {}\n", hex(id)));
        }
        out
    }

    /// 已分配的设备码数。
    #[must_use]
    pub fn known(&self) -> usize {
        self.codes.len()
    }

    /// 登记设备在线。同一台设备重新登记时以最新的连接为准。
    ///
    /// 设备还没有设备码时，从 `random` 取随机数分配一个不冲突的；取 64 次都冲突
    /// （码簿接近装满，实际不会发生）时返回 `None`。
    pub fn register(
        &mut self,
        conn: u64,
        device: DeviceId,
        observed: SocketAddr,
        random: &mut dyn FnMut() -> u32,
    ) -> Option<Registered> {
        let (code, new_code) = match self.codes.get(&device) {
            Some(code) => (*code, false),
            None => {
                let span = CODE_MAX - CODE_MIN + 1;
                let code = (0..64)
                    .map(|_| CODE_MIN + random() % span)
                    .find(|c| !self.owners.contains_key(c))?;
                self.codes.insert(device, code);
                self.owners.insert(code, device);
                (code, true)
            }
        };
        self.devices.insert(device, (conn, observed));
        Some(Registered {
            reply: SignalMsg::Registered { observed, code },
            code,
            new_code,
        })
    }

    /// 设备码对应的设备。
    #[must_use]
    pub fn device_of(&self, code: u32) -> Option<DeviceId> {
        self.owners.get(&code).copied()
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
                device: *target,
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

fn hex(id: &DeviceId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_id(text: &str) -> Option<DeviceId> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut id = [0u8; 32];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(id)
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
                code: 123_456_789,
            },
            SignalMsg::Connect { target: [1; 32] },
            SignalMsg::ConnectCode { code: 987_654_321 },
            SignalMsg::Peer {
                device: [3; 32],
                addr: addr("[2001:db8::2]:1"),
                relay: Some(grant(0)),
            },
            SignalMsg::Peer {
                device: [4; 32],
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
                "ConnectCode",
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
            device: [4; 32],
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
            device: [4; 32],
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
        let mut next = || 5;
        assert_eq!(
            reg.register(10, host, addr("198.51.100.1:5000"), &mut next),
            Some(Registered {
                reply: SignalMsg::Registered {
                    observed: addr("198.51.100.1:5000"),
                    code: CODE_MIN + 5,
                },
                code: CODE_MIN + 5,
                new_code: true,
            })
        );
        assert_eq!(reg.online(), 1);

        let relay = Some((addr("203.0.113.1:3478"), [8; 16]));
        let intro = reg.connect(ctrl, addr("192.0.2.7:6000"), &host, relay);
        match (&intro.to_controller, &intro.to_host) {
            (
                SignalMsg::Peer {
                    device,
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
                assert_eq!(*device, host);
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
        let again = reg
            .register(11, host, addr("198.51.100.1:5001"), &mut next)
            .unwrap();
        assert_eq!(
            (again.code, again.new_code),
            (CODE_MIN + 5, false),
            "设备码不变"
        );
        reg.disconnect(10);
        assert_eq!(reg.online(), 1);
        reg.disconnect(11);
        assert_eq!(reg.online(), 0);
    }

    #[test]
    fn codes_are_unique_and_survive_a_restart() {
        let mut reg = Registry::new();
        // 随机数先给一个会冲突的值，再给新值
        let mut seq = [7u32, 7, 7, 9].into_iter();
        let mut next = || seq.next().unwrap_or(0);
        let a = reg
            .register(1, [1; 32], addr("1.1.1.1:1"), &mut next)
            .unwrap();
        let b = reg
            .register(2, [2; 32], addr("1.1.1.1:2"), &mut next)
            .unwrap();
        assert_eq!((a.code, b.code), (CODE_MIN + 7, CODE_MIN + 9));
        assert_eq!(reg.device_of(b.code), Some([2; 32]));
        assert_eq!(reg.device_of(CODE_MIN), None);
        // 码簿装不下（随机数一直冲突）时拒绝登记
        let mut stuck = || 7;
        assert_eq!(
            reg.register(3, [3; 32], addr("1.1.1.1:3"), &mut stuck),
            None
        );

        let text = reg.codes_text();
        let mut restarted = Registry::new();
        let junk = format!(
            "{text}\nnot a line\n1 {}\n{} zz\n{text}",
            "00".repeat(32),
            CODE_MIN + 1
        );
        assert_eq!(restarted.load_codes(&junk), 5, "坏行、越界、重复的都跳过");
        assert_eq!(restarted.known(), 2);
        assert_eq!(restarted.online(), 0);
        let again = restarted
            .register(5, [1; 32], addr("2.2.2.2:2"), &mut stuck)
            .unwrap();
        assert_eq!((again.code, again.new_code), (CODE_MIN + 7, false));
        assert_eq!(
            restarted.load_codes(&format!("{} {} extra", CODE_MIN + 2, "ab".repeat(32))),
            1
        );
        assert_eq!(parse_id("é".repeat(32).as_str()), None);
    }

    #[test]
    fn codes_are_shown_and_typed_like_phone_numbers() {
        assert_eq!(format_code(123_045_009), "123 045 009");
        assert_eq!(parse_code(" 123 045-009 "), Some(123_045_009));
        assert_eq!(parse_code("12345678"), None);
        assert_eq!(parse_code("1234567890"), None);
        assert_eq!(parse_code("12345678x"), None);
        assert_eq!(parse_code("012345678"), None, "首位不能是 0");
    }
}
