//! 中继转发的状态机（不做 IO）。
//!
//! 直连失败时，两端各自把数据包发到中继，中继原样转给另一端。数据在两端之间是端到端加密的，
//! 中继只看到密文，只做三件事：配对、转发、计数。
//!
//! 包格式：`会话号(16 字节) | 方向(1 字节) | 载荷`。会话号由信令服务器随机生成，
//! 通过加密的信令连接分别交给两端，知道它才能使用这条中继通道。
//!
//! 票据：信令节点给每条中继通道签一张票据（会话号、记账账户、会员还是免费、设备、过期时间），
//! 两端的转发器把票据放在第一个包里交给中继。中继节点用集群共享的密钥验证票据后才开通道，
//! 所以信令节点可以把中继分到集群里任何一台，节点之间不用互相通知。
//!
//! 免费额度：免费通道按设备和按公网 IP 各自统计一个时间窗口（例如 10 分钟）里的流量，
//! 超过额度就停止转发，并给两端发一个通知包，客户端据此提示“免费服务器拥挤，请稍后重试”。
//!
//! 防滥用：
//! - 不认识的会话号一律丢弃，中继从不主动回包，不能被当作放大器。
//! - 每一端的地址在它发来第一个包时锁定，之后只有这个地址能以这一端的身份发包。
//! - 对端还没出现时不转发；空闲超时后会话作废。

use std::collections::BTreeMap;
use std::net::SocketAddr;

use crate::billing::AccountId;
use crate::Time;

/// 会话号。
pub type SessionId = [u8; 16];
/// 中继包头的长度。
pub const RELAY_HEADER: usize = 17;

/// 票据的开头。
pub const TICKET_MAGIC: &[u8; 8] = b"PDTKT001";
/// 票据长度：开头 8 + 会话号 16 + 账户 8 + 类别 1 + 设备 8 + 过期时间 8 + 签名 16。
pub const TICKET_LEN: usize = 65;
/// 通知包的开头（中继发给转发器）。
pub const NOTICE_MAGIC: &[u8; 8] = b"PDNOTE01";
/// 通知：免费额度用完，过一会儿再试。
pub const NOTICE_FREE_LIMIT: u8 = 1;

/// 一张中继票据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ticket {
    /// 会话号。
    pub session: SessionId,
    /// 记账账户（免费用户是公共账户）。
    pub account: AccountId,
    /// 会员通道（不受免费额度限制）。
    pub vip: bool,
    /// 发起连接的设备（指纹的前 8 字节），免费额度按它统计。
    pub subject: [u8; 8],
    /// 过期时间（Unix 毫秒）：过期的票据不能再开新通道。
    pub expires_ms: u64,
}

fn ticket_mac(secret: &[u8], body: &[u8]) -> [u8; 16] {
    use hmac::{Hmac, Mac};
    let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(secret).unwrap_or_else(|_| {
        <Hmac<sha2::Sha256> as hmac::digest::KeyInit>::new(&Default::default())
    });
    mac.update(body);
    let full = mac.finalize().into_bytes();
    let mut out = [0u8; 16];
    for (d, s) in out.iter_mut().zip(full.iter()) {
        *d = *s;
    }
    out
}

impl Ticket {
    /// 用集群密钥签名并编码。
    #[must_use]
    pub fn seal(&self, secret: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(TICKET_LEN);
        out.extend_from_slice(TICKET_MAGIC);
        out.extend_from_slice(&self.session);
        out.extend_from_slice(&self.account);
        out.push(u8::from(self.vip));
        out.extend_from_slice(&self.subject);
        out.extend_from_slice(&self.expires_ms.to_be_bytes());
        let mac = ticket_mac(secret, &out);
        out.extend_from_slice(&mac);
        out
    }

    /// 验证并解码：签名不对、过期或格式不对时返回 `None`。
    #[must_use]
    pub fn open(secret: &[u8], bytes: &[u8], now_ms: u64) -> Option<Self> {
        if bytes.len() != TICKET_LEN || !bytes.starts_with(TICKET_MAGIC) {
            return None;
        }
        let (body, mac) = bytes.split_at(TICKET_LEN - 16);
        let expected = ticket_mac(secret, body);
        let diff = expected
            .iter()
            .zip(mac.iter())
            .fold(0u8, |a, (x, y)| a | (x ^ y));
        if diff != 0 {
            return None;
        }
        let rest = body.get(8..)?;
        let (session, rest) = rest.split_at_checked(16)?;
        let (account, rest) = rest.split_at_checked(8)?;
        let (vip, rest) = rest.split_first()?;
        let (subject, expires) = rest.split_at_checked(8)?;
        let ticket = Self {
            session: session.try_into().ok()?,
            account: account.try_into().ok()?,
            vip: *vip == 1,
            subject: subject.try_into().ok()?,
            expires_ms: u64::from_be_bytes(expires.try_into().ok()?),
        };
        (ticket.expires_ms >= now_ms).then_some(ticket)
    }
}

/// 通知包的内容：`开头 | 类别(1) | 建议多少秒后重试(4)`。
#[must_use]
pub fn notice(kind: u8, retry_secs: u32) -> Vec<u8> {
    let mut out = NOTICE_MAGIC.to_vec();
    out.push(kind);
    out.extend_from_slice(&retry_secs.to_be_bytes());
    out
}

/// 解析通知包的内容：（类别，建议多少秒后重试）。
#[must_use]
pub fn parse_notice(payload: &[u8]) -> Option<(u8, u32)> {
    let rest = payload.strip_prefix(NOTICE_MAGIC.as_slice())?;
    let (kind, secs) = rest.split_first()?;
    Some((*kind, u32::from_be_bytes(secs.try_into().ok()?)))
}

/// 免费通道的额度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreePolicy {
    /// 统计窗口（微秒）。
    pub window_us: u64,
    /// 每台设备每个窗口最多转发的字节数；0 表示不限。
    pub device_bytes: u64,
    /// 每个公网 IP 每个窗口最多转发的字节数（防止重新生成设备身份来绕过）；0 表示不限。
    pub ip_bytes: u64,
}

impl FreePolicy {
    /// 不限。
    pub const UNLIMITED: Self = Self {
        window_us: 600_000_000,
        device_bytes: 0,
        ip_bytes: 0,
    };
}

/// 一个统计窗口里的用量。
#[derive(Debug, Clone, Copy, Default)]
struct Window {
    index: u64,
    bytes: u64,
}

impl Window {
    /// 在第 `index` 个窗口里加 `bytes`，返回加完之后的量。
    fn add(&mut self, index: u64, bytes: u64) -> u64 {
        if self.index != index {
            *self = Self { index, bytes: 0 };
        }
        self.bytes = self.bytes.saturating_add(bytes);
        self.bytes
    }

    fn current(&self, index: u64) -> u64 {
        if self.index == index {
            self.bytes
        } else {
            0
        }
    }
}

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
    /// 这个会话记在谁的账上（发起连接一端的账户）。
    account: AccountId,
    /// 已经记过账的用量。
    billed: Usage,
    /// 会员通道。
    vip: bool,
    /// 发起连接的设备（免费额度按它统计）。
    subject: [u8; 8],
    /// 免费额度用完，停止转发。
    blocked: bool,
}

/// 一笔账：某个会话自上次记账以来新增的用量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// 会话号。
    pub session: SessionId,
    /// 记在谁的账上。
    pub account: AccountId,
    /// 新增的用量。
    pub usage: Usage,
    /// 会话已结束（这是它的最后一笔）。
    pub last: bool,
}

fn delta(now: Usage, before: Usage) -> Usage {
    Usage {
        bytes: now.bytes.saturating_sub(before.bytes),
        packets: now.packets.saturating_sub(before.packets),
    }
}

/// 中继。
#[derive(Debug, Clone)]
pub struct Relay {
    sessions: BTreeMap<SessionId, Session>,
    idle_us: u64,
    max_sessions: usize,
    total: Usage,
    dropped: u64,
    free: FreePolicy,
    by_device: BTreeMap<[u8; 8], Window>,
    by_ip: BTreeMap<std::net::IpAddr, Window>,
    notices: Vec<(SocketAddr, Vec<u8>)>,
    limited: u64,
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
            free: FreePolicy::UNLIMITED,
            by_device: BTreeMap::new(),
            by_ip: BTreeMap::new(),
            notices: Vec::new(),
            limited: 0,
        }
    }

    /// 设置免费通道的额度。
    #[must_use]
    pub const fn with_free_policy(mut self, free: FreePolicy) -> Self {
        self.free = free;
        self
    }

    /// 是否已有这个会话。
    #[must_use]
    pub fn knows(&self, session: &SessionId) -> bool {
        self.sessions.contains_key(session)
    }

    /// 凭票据开通道（票据已由调用方验证）。已满或会话号重复时返回 false。
    pub fn admit(&mut self, now: Time, ticket: &Ticket) -> bool {
        if !self.allocate(now, ticket.session, ticket.account) {
            return false;
        }
        if let Some(s) = self.sessions.get_mut(&ticket.session) {
            s.vip = ticket.vip;
            s.subject = ticket.subject;
        }
        true
    }

    /// 取走要发给两端的通知包（目的地址，完整的包）。
    pub fn take_notices(&mut self) -> Vec<(SocketAddr, Vec<u8>)> {
        std::mem::take(&mut self.notices)
    }

    /// 因为免费额度用完而停掉的通道数（启动以来）。
    #[must_use]
    pub const fn limited(&self) -> u64 {
        self.limited
    }

    /// 现在被限流的设备数（当前窗口里用完了额度的）。
    #[must_use]
    pub fn limited_now(&self, now: Time) -> usize {
        let index = now.0 / self.free.window_us.max(1);
        let limit = self.free.device_bytes;
        if limit == 0 {
            return 0;
        }
        self.by_device
            .values()
            .filter(|w| w.current(index) >= limit)
            .count()
    }

    /// 免费通道转发 `bytes` 字节之前检查额度：超了就停掉这个会话并通知两端。
    fn charge_free(&mut self, now: Time, id: SessionId, from: SocketAddr, bytes: u64) -> bool {
        let index = now.0 / self.free.window_us.max(1);
        let Some(session) = self.sessions.get_mut(&id) else {
            return false;
        };
        if session.vip {
            return true;
        }
        if session.blocked {
            return false;
        }
        let device = self
            .by_device
            .entry(session.subject)
            .or_default()
            .add(index, bytes);
        let ip = self.by_ip.entry(from.ip()).or_default().add(index, bytes);
        let over = |used: u64, limit: u64| limit != 0 && used > limit;
        if !over(device, self.free.device_bytes) && !over(ip, self.free.ip_bytes) {
            return true;
        }
        session.blocked = true;
        self.limited = self.limited.saturating_add(1);
        // 建议重试时间：到这个窗口结束。
        let window = self.free.window_us.max(1);
        let left_us = window - now.0 % window;
        let retry = u32::try_from(left_us.div_ceil(1_000_000)).unwrap_or(u32::MAX);
        for (side, addr) in session.addrs.iter().enumerate() {
            if let Some(addr) = addr {
                // 方向写成“对端”：转发器只收对端方向的包。
                let other = u8::from(side == 0);
                self.notices
                    .push((*addr, wrap(&id, other, &notice(NOTICE_FREE_LIMIT, retry))));
            }
        }
        false
    }

    /// 开一个会话。已满或会话号重复时返回 false。
    pub fn allocate(&mut self, now: Time, session: SessionId, account: AccountId) -> bool {
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
                billed: Usage::default(),
                vip: true,
                subject: [0; 8],
                blocked: false,
            },
        );
        true
    }

    /// 处理收到的一个包。需要转发时返回目的地址；包本身原样转发，不改动。
    pub fn on_packet(&mut self, now: Time, from: SocketAddr, packet: &[u8]) -> Option<SocketAddr> {
        let bytes = u64::try_from(packet.len()).unwrap_or(u64::MAX);
        let route = self.route(now, from, packet);
        let to = route.filter(|(id, _)| self.charge_free(now, *id, from, bytes));
        let Some((id, to)) = to else {
            self.dropped = self.dropped.saturating_add(1);
            return None;
        };
        if let Some(session) = self.sessions.get_mut(&id) {
            session.usage.bytes = session.usage.bytes.saturating_add(bytes);
            session.usage.packets = session.usage.packets.saturating_add(1);
        }
        self.total.bytes = self.total.bytes.saturating_add(bytes);
        self.total.packets = self.total.packets.saturating_add(1);
        Some(to)
    }

    /// 登记发送方的地址（票据、保活包），不转发、不计费。
    pub fn touch(&mut self, now: Time, from: SocketAddr, packet: &[u8]) {
        let _ = self.route(now, from, packet);
    }

    /// 找到这个包要转给谁：会话号、方向、地址锁定都对，对端也已出现。
    fn route(
        &mut self,
        now: Time,
        from: SocketAddr,
        packet: &[u8],
    ) -> Option<(SessionId, SocketAddr)> {
        let (id, side, _) = unwrap(packet)?;
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
        Some((id, to))
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
                    usage: delta(s.usage, s.billed),
                    last: true,
                })
            })
            .collect()
    }

    /// 记账：每个进行中的会话自上次记账以来新增的用量（没有新增的不记）。
    /// 定期调用，进程崩溃时最多丢掉最近一次记账以后的用量。
    pub fn bill(&mut self) -> Vec<Record> {
        self.sessions
            .iter_mut()
            .filter(|(_, s)| s.usage != s.billed)
            .map(|(id, s)| {
                let usage = delta(s.usage, s.billed);
                s.billed = s.usage;
                Record {
                    session: *id,
                    account: s.account,
                    usage,
                    last: false,
                }
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
    const ACCOUNT: AccountId = [9; 8];

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
        assert!(relay.allocate(Time(0), [2; 16], [3; 8]));
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
                account: [3; 8],
                usage: Usage::default(),
                last: true,
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

    /// 定期记账：只记新增的部分；会话结束时的最后一笔是上次记账以后的余数。
    #[test]
    fn billing_reports_deltas() {
        let mut relay = Relay::new(1000, 10);
        assert!(relay.allocate(Time(0), ID, ACCOUNT));
        let (a, b) = (addr("10.0.0.1:1"), addr("10.0.0.2:2"));
        relay.on_packet(Time(1), a, &wrap(&ID, 0, b"hello"));
        relay.on_packet(Time(2), b, &wrap(&ID, 1, b"hi"));
        relay.on_packet(Time(3), a, &wrap(&ID, 0, b"hello"));
        assert!(!relay.bill().is_empty());
        assert!(relay.bill().is_empty(), "没有新增就不记");
        relay.on_packet(Time(4), b, &wrap(&ID, 1, b"bye"));
        let records = relay.bill();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].usage.packets, 1);
        assert!(!records[0].last);
        relay.on_packet(Time(5), a, &wrap(&ID, 0, b"x"));
        let ended = relay.expire(Time(2000));
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].usage.packets, 1);
        assert!(ended[0].last);
        assert_eq!(ended[0].account, ACCOUNT);
    }

    #[test]
    fn tickets_verify_with_the_cluster_key_and_expire() {
        let t = Ticket {
            session: [1; 16],
            account: [2; 8],
            vip: true,
            subject: [3; 8],
            expires_ms: 1000,
        };
        let bytes = t.seal(b"key");
        assert_eq!(bytes.len(), TICKET_LEN);
        assert_eq!(Ticket::open(b"key", &bytes, 999), Some(t));
        assert_eq!(Ticket::open(b"key", &bytes, 1001), None, "过期");
        assert_eq!(Ticket::open(b"other", &bytes, 0), None, "别的密钥");
        let mut forged = bytes.clone();
        forged[30] ^= 1;
        assert_eq!(Ticket::open(b"key", &forged, 0), None);
        assert_eq!(Ticket::open(b"key", &bytes[..TICKET_LEN - 1], 0), None);
        let free = Ticket { vip: false, ..t };
        assert_eq!(
            Ticket::open(b"key", &free.seal(b"key"), 0).map(|t| t.vip),
            Some(false)
        );
        assert_eq!(
            parse_notice(&notice(NOTICE_FREE_LIMIT, 300)),
            Some((NOTICE_FREE_LIMIT, 300))
        );
        assert_eq!(parse_notice(b"PDNOTE01"), None);
        assert_eq!(parse_notice(b"other"), None);
    }

    /// 免费通道：一台设备在一个窗口里超过额度后停止转发，两端各收到一个通知；
    /// 换个会话也不行，下一个窗口恢复；会员通道不受限制；同一个 IP 上换设备身份也受 IP 额度限制。
    #[test]
    fn free_channels_stop_at_the_window_limit_and_notify() {
        let policy = FreePolicy {
            window_us: 1_000_000,
            device_bytes: 100,
            ip_bytes: 150,
        };
        let mut relay = Relay::new(10_000_000, 100).with_free_policy(policy);
        let (a, b) = (addr("10.0.0.1:1"), addr("10.0.0.2:2"));
        let ticket = |session: u8, subject: u8, vip| Ticket {
            session: [session; 16],
            account: [0; 8],
            vip,
            subject: [subject; 8],
            expires_ms: u64::MAX,
        };
        assert!(relay.admit(Time(0), &ticket(1, 1, false)));
        assert!(relay.knows(&[1; 16]) && !relay.knows(&[2; 16]));
        let id = [1u8; 16];
        relay.on_packet(Time(1), b, &wrap(&id, 1, b""));
        let payload = [0u8; 23]; // 包长 40 字节
        assert!(relay
            .on_packet(Time(2), a, &wrap(&id, 0, &payload))
            .is_some());
        assert!(relay
            .on_packet(Time(3), a, &wrap(&id, 0, &payload))
            .is_some());
        assert!(
            relay
                .on_packet(Time(4), a, &wrap(&id, 0, &payload))
                .is_none(),
            "第三个包超过 100 字节"
        );
        let notices = relay.take_notices();
        assert_eq!(notices.len(), 2, "两端各一个通知");
        let (to, packet) = &notices[0];
        assert_eq!(*to, a);
        let (_, side, body) = unwrap(packet).unwrap();
        assert_eq!(side, 1, "方向写成对端");
        assert_eq!(parse_notice(body), Some((NOTICE_FREE_LIMIT, 1)));
        assert!(relay.take_notices().is_empty());
        assert!(
            relay
                .on_packet(Time(5), b, &wrap(&id, 1, &payload))
                .is_none(),
            "停掉后两个方向都不转"
        );
        assert_eq!(relay.limited(), 1);
        assert_eq!(relay.limited_now(Time(6)), 1);
        // 同一台设备开新会话：还在这个窗口里，第一个包就停
        assert!(relay.admit(Time(7), &ticket(2, 1, false)));
        relay.on_packet(Time(8), b, &wrap(&[2; 16], 1, b""));
        assert!(relay
            .on_packet(Time(9), a, &wrap(&[2; 16], 0, &payload))
            .is_none());
        // 下一个窗口：新会话恢复
        assert!(relay.admit(Time(1_000_000), &ticket(3, 1, false)));
        relay.on_packet(Time(1_000_001), b, &wrap(&[3; 16], 1, b""));
        assert!(relay
            .on_packet(Time(1_000_002), a, &wrap(&[3; 16], 0, &payload))
            .is_some());
        assert_eq!(relay.limited_now(Time(1_000_003)), 0);
        // 同一 IP 换设备身份：IP 额度 150 也会用完
        assert!(relay.admit(Time(1_000_010), &ticket(4, 9, false)));
        relay.on_packet(Time(1_000_011), b, &wrap(&[4; 16], 1, b""));
        assert!(relay
            .on_packet(Time(1_000_012), a, &wrap(&[4; 16], 0, &payload))
            .is_some());
        assert!(relay
            .on_packet(Time(1_000_013), a, &wrap(&[4; 16], 0, &payload))
            .is_some());
        assert!(
            relay
                .on_packet(Time(1_000_014), a, &wrap(&[4; 16], 0, &payload))
                .is_none(),
            "IP 超过 150"
        );
        // 会员通道不受限制
        assert!(relay.admit(Time(1_000_020), &ticket(5, 1, true)));
        relay.on_packet(Time(1_000_021), b, &wrap(&[5; 16], 1, b""));
        for t in 0..10 {
            assert!(relay
                .on_packet(Time(1_000_022 + t), a, &wrap(&[5; 16], 0, &payload))
                .is_some());
        }
        // 票据和保活包只登记地址，不转发
        let mut quiet = Relay::new(10_000_000, 10);
        assert!(quiet.admit(Time(0), &ticket(9, 1, true)));
        quiet.touch(Time(1), a, &wrap(&[9; 16], 0, b"ticket"));
        quiet.touch(Time(2), b, &wrap(&[9; 16], 1, b"ticket"));
        assert_eq!(
            quiet.on_packet(Time(3), a, &wrap(&[9; 16], 0, b"x")),
            Some(b)
        );
        assert_eq!(quiet.total().packets, 1);
        // 已满时开不了
        let mut full = Relay::new(10, 1);
        assert!(full.admit(Time(0), &ticket(1, 1, true)));
        assert!(!full.admit(Time(0), &ticket(2, 1, true)));
        assert_eq!(full.limited_now(Time(0)), 0, "不限额时没有被限流的");
    }
}
