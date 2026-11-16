//! 中继转发的状态机（不做 IO）。
//!
//! 直连失败时，两端各自把数据包发到中继，中继原样转给另一端。数据在两端之间是端到端加密的，
//! 中继只看到密文，只做三件事：配对、转发、计数。
//!
//! 包格式：`会话号(16 字节) | 方向(1 字节) | 载荷`。会话号由信令服务器随机生成，
//! 通过加密的信令连接分别交给两端，知道它才能使用这条中继通道。
//!
//! 防滥用：
//! - 不认识的会话号一律丢弃，中继从不主动回包，不能被当作放大器。
//! - 每一端的地址在它发来第一个包时锁定，之后只有这个地址能以这一端的身份发包。
//! - 对端还没出现时不转发；空闲超时后会话作废。

use std::collections::BTreeMap;
use std::net::SocketAddr;

use crate::Time;

/// 会话号。
pub type SessionId = [u8; 16];
/// 中继包头的长度。
pub const RELAY_HEADER: usize = 17;

/// 给载荷加上中继包头。`side` 是发送方的方向（0 或 1）。
#[must_use]
pub fn wrap(session: &SessionId, side: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(RELAY_HEADER + payload.len());
    out.extend_from_slice(session);
    out.push(side);
    out.extend_from_slice(payload);
    out
}

/// 拆开中继包：返回会话号、发送方的方向、载荷。格式不对时返回 `None`。
#[must_use]
pub fn unwrap(packet: &[u8]) -> Option<(SessionId, u8, &[u8])> {
    let (head, payload) = packet.split_at_checked(RELAY_HEADER)?;
    let (id, side) = head.split_at(16);
    let side = *side.first()?;
    if side > 1 {
        return None;
    }
    Some((id.try_into().ok()?, side, payload))
}

/// 一个会话的用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    /// 转发的字节数（两个方向合计，含中继包头）。
    pub bytes: u64,
    /// 转发的包数。
    pub packets: u64,
}

#[derive(Debug, Clone)]
struct Session {
    addrs: [Option<SocketAddr>; 2],
    usage: Usage,
    last_active: Time,
    /// 这个会话记在谁的账上（发起连接的设备指纹）。
    account: [u8; 32],
}

/// 一个已结束会话的账单记录。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// 会话号。
    pub session: SessionId,
    /// 记在谁的账上。
    pub account: [u8; 32],
    /// 用量。
    pub usage: Usage,
}

/// 中继。
#[derive(Debug, Clone)]
pub struct Relay {
    sessions: BTreeMap<SessionId, Session>,
    idle_us: u64,
    max_sessions: usize,
    total: Usage,
    dropped: u64,
}

impl Relay {
    /// 创建。会话空闲 `idle_us` 微秒后作废；最多同时 `max_sessions` 个会话。
    #[must_use]
    pub const fn new(idle_us: u64, max_sessions: usize) -> Self {
        Self {
            sessions: BTreeMap::new(),
            idle_us,
            max_sessions,
            total: Usage {
                bytes: 0,
                packets: 0,
            },
            dropped: 0,
        }
    }

    /// 开一个会话。已满或会话号重复时返回 false。
    pub fn allocate(&mut self, now: Time, session: SessionId, account: [u8; 32]) -> bool {
        if self.sessions.len() >= self.max_sessions || self.sessions.contains_key(&session) {
            return false;
        }
        self.sessions.insert(
            session,
            Session {
                addrs: [None, None],
                usage: Usage::default(),
                last_active: now,
                account,
            },
        );
        true
    }

    /// 处理收到的一个包。需要转发时返回目的地址；包本身原样转发，不改动。
    pub fn on_packet(&mut self, now: Time, from: SocketAddr, packet: &[u8]) -> Option<SocketAddr> {
        let forward = unwrap(packet).and_then(|(id, side, _)| {
            let session = self.sessions.get_mut(&id)?;
            let me = usize::from(side);
            let slot = session.addrs.get_mut(me)?;
            match slot {
                // 这一端的地址已经锁定，别的地址不能冒充它。
                Some(locked) if *locked != from => return None,
                Some(_) => {}
                None => *slot = Some(from),
            }
            session.last_active = now;
            let to = (*session.addrs.get(1 - me)?)?;
            let bytes = u64::try_from(packet.len()).unwrap_or(u64::MAX);
            session.usage.bytes = session.usage.bytes.saturating_add(bytes);
            session.usage.packets = session.usage.packets.saturating_add(1);
            self.total.bytes = self.total.bytes.saturating_add(bytes);
            self.total.packets = self.total.packets.saturating_add(1);
            Some(to)
        });
        if forward.is_none() {
            self.dropped = self.dropped.saturating_add(1);
        }
        forward
    }

    /// 作废空闲超时的会话，返回它们的账单记录。
    pub fn expire(&mut self, now: Time) -> Vec<Record> {
        let idle = self.idle_us;
        let dead: Vec<SessionId> = self
            .sessions
            .iter()
            .filter(|(_, s)| now.since(s.last_active) >= idle)
            .map(|(id, _)| *id)
            .collect();
        dead.into_iter()
            .filter_map(|id| {
                self.sessions.remove(&id).map(|s| Record {
                    session: id,
                    account: s.account,
                    usage: s.usage,
                })
            })
            .collect()
    }

    /// 某个会话到目前为止的用量。
    #[must_use]
    pub fn usage(&self, session: &SessionId) -> Option<Usage> {
        self.sessions.get(session).map(|s| s.usage)
    }

    /// 当前的会话数。
    #[must_use]
    pub fn sessions(&self) -> usize {
        self.sessions.len()
    }

    /// 启动以来转发的总量。
    #[must_use]
    pub const fn total(&self) -> Usage {
        self.total
    }

    /// 启动以来丢弃的包数（会话不存在、地址不符、对端未到、格式不对）。
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: SessionId = [7; 16];
    const ACCOUNT: [u8; 32] = [9; 32];

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn wrap_unwrap() {
        let packet = wrap(&ID, 1, b"hello");
        assert_eq!(packet.len(), RELAY_HEADER + 5);
        assert_eq!(unwrap(&packet), Some((ID, 1, &b"hello"[..])));
        assert_eq!(unwrap(&wrap(&ID, 0, b"")), Some((ID, 0, &b""[..])));
        assert_eq!(unwrap(&packet[..16]), None, "太短");
        assert_eq!(unwrap(&wrap(&ID, 2, b"x")), None, "方向只能是 0 或 1");
    }

    #[test]
    fn pairs_forwards_and_counts() {
        let (a, b) = (addr("1.1.1.1:1000"), addr("2.2.2.2:2000"));
        let mut relay = Relay::new(30_000_000, 10);
        assert!(relay.allocate(Time(0), ID, ACCOUNT));
        let from_a = wrap(&ID, 0, b"ping");
        let from_b = wrap(&ID, 1, b"pong!");

        // 对端还没出现：不转发
        assert_eq!(relay.on_packet(Time(1), a, &from_a), None);
        // 对端出现后双向转发
        assert_eq!(relay.on_packet(Time(2), b, &from_b), Some(a));
        assert_eq!(relay.on_packet(Time(3), a, &from_a), Some(b));
        let usage = relay.usage(&ID).unwrap();
        assert_eq!(usage.packets, 2);
        assert_eq!(usage.bytes, (from_a.len() + from_b.len()) as u64);
        assert_eq!(relay.total(), usage);
        assert_eq!(relay.dropped(), 1);
        assert_eq!(relay.sessions(), 1);
    }

    #[test]
    fn rejects_unknown_sessions_and_address_hijacking() {
        let (a, b, evil) = (addr("1.1.1.1:1"), addr("2.2.2.2:2"), addr("6.6.6.6:6"));
        let mut relay = Relay::new(30_000_000, 10);
        relay.allocate(Time(0), ID, ACCOUNT);
        relay.on_packet(Time(1), a, &wrap(&ID, 0, b"x"));
        relay.on_packet(Time(1), b, &wrap(&ID, 1, b"x"));

        // 不认识的会话号、格式不对的包
        assert_eq!(
            relay.on_packet(Time(2), evil, &wrap(&[1; 16], 0, b"x")),
            None
        );
        assert_eq!(relay.on_packet(Time(2), evil, b"short"), None);
        assert_eq!(relay.on_packet(Time(2), evil, &[]), None);
        // 知道会话号的第三方冒充某一端：地址不符，丢弃
        assert_eq!(relay.on_packet(Time(2), evil, &wrap(&ID, 0, b"x")), None);
        assert_eq!(relay.on_packet(Time(2), evil, &wrap(&ID, 1, b"x")), None);
        // 原来的两端不受影响
        assert_eq!(relay.on_packet(Time(3), a, &wrap(&ID, 0, b"x")), Some(b));
        assert_eq!(relay.dropped(), 6);
    }

    #[test]
    fn limits_and_expiry_produce_billing_records() {
        let mut relay = Relay::new(1000, 2);
        assert!(relay.allocate(Time(0), [1; 16], ACCOUNT));
        assert!(!relay.allocate(Time(0), [1; 16], ACCOUNT), "会话号重复");
        assert!(relay.allocate(Time(0), [2; 16], [3; 32]));
        assert!(!relay.allocate(Time(0), [3; 16], ACCOUNT), "已满");

        let (a, b) = (addr("1.1.1.1:1"), addr("2.2.2.2:2"));
        relay.on_packet(Time(500), a, &wrap(&[1; 16], 0, b"data"));
        relay.on_packet(Time(600), b, &wrap(&[1; 16], 1, b"data"));
        // 会话 2 一直空闲，先到期；它没有流量
        let records = relay.expire(Time(1000));
        assert_eq!(
            records,
            vec![Record {
                session: [2; 16],
                account: [3; 32],
                usage: Usage::default()
            }]
        );
        assert!(relay.expire(Time(1599)).is_empty());
        let records = relay.expire(Time(1600));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].account, ACCOUNT);
        assert_eq!(records[0].usage.packets, 1);
        assert_eq!(relay.sessions(), 0);
        assert_eq!(relay.usage(&[1; 16]), None);
        // 到期后同一个会话号的包被丢弃
        assert_eq!(
            relay.on_packet(Time(1700), a, &wrap(&[1; 16], 0, b"x")),
            None
        );
        assert!(relay.allocate(Time(1700), [3; 16], ACCOUNT), "腾出了位置");
    }
}
