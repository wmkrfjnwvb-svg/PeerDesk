//! 连接路径的探测和选择（不做 IO）。
//!
//! 两端交换所有候选地址（局域网、IPv6、公网、路由器映射的端口、中继），然后同时向对方的每个
//! 候选地址发探测包（ping），对方从同一个端口回 pong。这一步本身就是打洞：两端同时发包，
//! 双方的 NAT 都会为对方的地址开门。pong 带回发送时间，得到每条路径的往返时间，
//! 选最好的一条再建 QUIC 连接，整个过程限定在几秒内。
//!
//! 选路规则：直连优先；几条直连都通时选往返时间最短的；中继要比最好的直连快
//! [`RELAY_PENALTY_US`] 以上才选中继（中继占服务器带宽，是成本）。

use std::net::SocketAddr;

/// 探测包的开头。
pub const PING_MAGIC: &[u8; 8] = b"PDPING01";
/// 探测包长度：开头 8 + 暗号 16 + 类型 1 + 序号 4 + 时间 8。
pub const PING_LEN: usize = 37;
/// 中继的额外代价（微秒）：直连只要不比中继慢这么多，就用直连。
pub const RELAY_PENALTY_US: u64 = 30_000;
/// 探测阶段的时间安排（微秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// 第一条直连回应之后，再等这么久看有没有更好的。
    pub settle_us: u64,
    /// 只有中继通时，最多等直连这么久。
    pub direct_wait_us: u64,
    /// 总时限：到时还没有任何回应就放弃。
    pub budget_us: u64,
}

impl Limits {
    /// 一般情况：普通打洞一般几十到几百毫秒就通。
    pub const NORMAL: Self = Self {
        settle_us: 300_000,
        direct_wait_us: 1_500_000,
        budget_us: 2_000_000,
    };
    /// 有一端是难打洞的 NAT：公网地址基本不会通，只等局域网、IPv6、端口映射这些快路径，
    /// 早点交给多端口打洞（整个连接仍在 5 秒内）。
    pub const HARD: Self = Self {
        settle_us: 200_000,
        direct_wait_us: 600_000,
        budget_us: 800_000,
    };
}

/// 路径的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathKind {
    /// 同一个局域网。
    Lan,
    /// IPv6 直连（没有 NAT）。
    Ipv6,
    /// 路由器为对方映射出来的端口（UPnP、NAT-PMP、PCP）。
    Mapped,
    /// 对方的公网地址（打洞）。
    Public,
    /// 探测中才发现的对方地址（对方的 NAT 换了端口）。
    PeerReflexive,
    /// 经服务器中继。
    Relay,
}

impl PathKind {
    /// 给用户看的名字。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            PathKind::Lan => "局域网直连",
            PathKind::Ipv6 => "IPv6 直连",
            PathKind::Mapped => "端口映射直连",
            PathKind::Public => "公网直连（打洞）",
            PathKind::PeerReflexive => "公网直连（打洞，对方换了端口）",
            PathKind::Relay => "中继",
        }
    }

    /// 是否是直连。
    #[must_use]
    pub const fn direct(self) -> bool {
        match self {
            PathKind::Relay => false,
            PathKind::Lan
            | PathKind::Ipv6
            | PathKind::Mapped
            | PathKind::Public
            | PathKind::PeerReflexive => true,
        }
    }

    const fn code(self) -> u8 {
        match self {
            PathKind::Lan => 0,
            PathKind::Ipv6 => 1,
            PathKind::Mapped => 2,
            PathKind::Public => 3,
            PathKind::PeerReflexive => 4,
            PathKind::Relay => 5,
        }
    }

    /// 从编码还原。
    #[must_use]
    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(PathKind::Lan),
            1 => Some(PathKind::Ipv6),
            2 => Some(PathKind::Mapped),
            3 => Some(PathKind::Public),
            4 => Some(PathKind::PeerReflexive),
            5 => Some(PathKind::Relay),
            _ => None,
        }
    }

    /// 编码（写进信令）。
    #[must_use]
    pub const fn to_code(self) -> u8 {
        self.code()
    }
}

/// 根据地址判断一个本机地址是哪种候选：全局 IPv6 是 IPv6 直连，其余是局域网。
#[must_use]
pub fn local_kind(addr: &SocketAddr) -> PathKind {
    match addr {
        SocketAddr::V6(v6) => {
            let first = v6.ip().segments().first().copied().unwrap_or(0);
            // 2000::/3 是全局单播地址；fc00::/7（ULA）只在局域网里有效。
            if first & 0xe000 == 0x2000 {
                PathKind::Ipv6
            } else {
                PathKind::Lan
            }
        }
        SocketAddr::V4(_) => PathKind::Lan,
    }
}

/// 一个候选地址。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    /// 地址。
    pub addr: SocketAddr,
    /// 种类。
    pub kind: PathKind,
}

/// 探测包。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    /// 这次连接的暗号。
    pub nonce: [u8; 16],
    /// 是回应（pong）还是探测（ping）。
    pub pong: bool,
    /// 序号。
    pub seq: u32,
    /// 发送 ping 的一端的发送时间（微秒，发送方自己的时钟）；pong 原样带回。
    pub sent_us: u64,
}

impl Ping {
    /// 编码。
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(PING_LEN);
        p.extend_from_slice(PING_MAGIC);
        p.extend_from_slice(&self.nonce);
        p.push(u8::from(self.pong));
        p.extend_from_slice(&self.seq.to_be_bytes());
        p.extend_from_slice(&self.sent_us.to_be_bytes());
        p
    }

    /// 解码；不是探测包时返回 `None`。
    #[must_use]
    pub fn decode(packet: &[u8]) -> Option<Self> {
        let rest = packet.strip_prefix(PING_MAGIC.as_slice())?;
        if rest.len() != PING_LEN - PING_MAGIC.len() {
            return None;
        }
        let (nonce, rest) = rest.split_at_checked(16)?;
        let (kind, rest) = rest.split_first()?;
        let (seq, sent) = rest.split_at_checked(4)?;
        let pong = match kind {
            0 => false,
            1 => true,
            _ => return None,
        };
        Some(Self {
            nonce: <[u8; 16]>::try_from(nonce).ok()?,
            pong,
            seq: u32::from_be_bytes(<[u8; 4]>::try_from(seq).ok()?),
            sent_us: u64::from_be_bytes(<[u8; 8]>::try_from(sent).ok()?),
        })
    }

    /// 对这个 ping 的回应。
    #[must_use]
    pub const fn reply(&self) -> Self {
        Self {
            pong: true,
            ..*self
        }
    }
}

/// 一条测到了往返时间的路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measured {
    /// 候选地址。
    pub candidate: Candidate,
    /// 最小往返时间（微秒）。
    pub rtt_us: u64,
    /// 第一次收到回应的时刻（探测开始后的微秒数）。
    pub first_us: u64,
}

/// 路径的综合代价：往返时间，中继再加上 [`RELAY_PENALTY_US`]；相同时按种类排（局域网优先）。
fn score(m: &Measured) -> (u64, PathKind) {
    let penalty = if m.candidate.kind.direct() {
        0
    } else {
        RELAY_PENALTY_US
    };
    (m.rtt_us.saturating_add(penalty), m.candidate.kind)
}

/// 已测到的路径里最好的一条。
#[must_use]
pub fn best(measured: &[Measured]) -> Option<Measured> {
    measured.iter().min_by_key(|m| score(m)).copied()
}

/// 探测进行到 `elapsed_us` 时该怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 继续等。
    Wait,
    /// 就用这一条。
    Pick(Measured),
    /// 时间到了，一条都不通。
    GiveUp,
}

/// 根据已测到的路径决定：有直连回应后再等 `settle_us` 看有没有更好的；只有中继通时最多等
/// `direct_wait_us`；所有候选都回应了就不必再等；到 `budget_us` 还没有回应就放弃。
#[must_use]
pub fn decide(
    measured: &[Measured],
    candidates: usize,
    elapsed_us: u64,
    limits: Limits,
) -> Decision {
    let Some(top) = best(measured) else {
        return if elapsed_us >= limits.budget_us {
            Decision::GiveUp
        } else {
            Decision::Wait
        };
    };
    let first_direct = measured
        .iter()
        .filter(|m| m.candidate.kind.direct())
        .map(|m| m.first_us)
        .min();
    let all_in = measured.len() >= candidates;
    let ready = match first_direct {
        Some(t) => elapsed_us >= t.saturating_add(limits.settle_us),
        None => elapsed_us >= limits.direct_wait_us,
    };
    if all_in || ready || elapsed_us >= limits.budget_us {
        Decision::Pick(top)
    } else {
        Decision::Wait
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn m(kind: PathKind, rtt_ms: u64, first_ms: u64) -> Measured {
        Measured {
            candidate: Candidate {
                addr: addr("1.2.3.4:5"),
                kind,
            },
            rtt_us: rtt_ms * 1000,
            first_us: first_ms * 1000,
        }
    }

    #[test]
    fn ping_roundtrip_and_rejects() {
        let p = Ping {
            nonce: [3; 16],
            pong: false,
            seq: 7,
            sent_us: 123_456_789,
        };
        let bytes = p.encode();
        assert_eq!(bytes.len(), PING_LEN);
        assert_eq!(Ping::decode(&bytes), Some(p));
        assert_eq!(
            Ping::decode(&p.reply().encode()).map(|r| r.pong),
            Some(true)
        );
        assert_eq!(Ping::decode(&bytes[..PING_LEN - 1]), None);
        assert_eq!(Ping::decode(b"other"), None);
        let mut bad = bytes.clone();
        *bad.get_mut(24).unwrap() = 9;
        assert_eq!(Ping::decode(&bad), None, "类型只能是 0 或 1");
    }

    #[test]
    fn kinds_codes_labels() {
        for k in [
            PathKind::Lan,
            PathKind::Ipv6,
            PathKind::Mapped,
            PathKind::Public,
            PathKind::PeerReflexive,
            PathKind::Relay,
        ] {
            assert_eq!(PathKind::from_code(k.to_code()), Some(k));
            assert!(!k.label().is_empty());
            assert_eq!(k.direct(), k != PathKind::Relay);
        }
        assert_eq!(PathKind::from_code(9), None);
        assert_eq!(local_kind(&addr("192.168.1.2:1")), PathKind::Lan);
        assert_eq!(local_kind(&addr("[2408:8000::1]:1")), PathKind::Ipv6);
        assert_eq!(local_kind(&addr("[fd00::1]:1")), PathKind::Lan);
    }

    #[test]
    fn direct_wins_unless_relay_is_much_faster() {
        assert_eq!(best(&[]), None);
        let pick = best(&[m(PathKind::Relay, 30, 0), m(PathKind::Public, 50, 0)]).unwrap();
        assert_eq!(
            pick.candidate.kind,
            PathKind::Public,
            "直连只慢 20 ms，用直连"
        );
        let pick = best(&[m(PathKind::Relay, 30, 0), m(PathKind::Public, 90, 0)]).unwrap();
        assert_eq!(pick.candidate.kind, PathKind::Relay, "直连慢 60 ms，用中继");
        let pick = best(&[m(PathKind::Public, 5, 0), m(PathKind::Lan, 5, 0)]).unwrap();
        assert_eq!(pick.candidate.kind, PathKind::Lan, "一样快时局域网优先");
        let pick = best(&[m(PathKind::Public, 20, 0), m(PathKind::Ipv6, 12, 0)]).unwrap();
        assert_eq!(pick.candidate.kind, PathKind::Ipv6);
    }

    #[test]
    fn decisions_over_time() {
        let n = Limits::NORMAL;
        // 什么都没有：等到时限再放弃
        assert_eq!(decide(&[], 3, 100_000, n), Decision::Wait);
        assert_eq!(decide(&[], 3, n.budget_us, n), Decision::GiveUp);
        assert_eq!(
            decide(&[], 3, Limits::HARD.budget_us, Limits::HARD),
            Decision::GiveUp
        );
        // 第一条直连在 40 ms 到：再等 300 ms
        let one = [m(PathKind::Public, 40, 40)];
        assert_eq!(decide(&one, 3, 200_000, n), Decision::Wait);
        assert!(matches!(decide(&one, 3, 340_000, n), Decision::Pick(_)));
        // 所有候选都回应了：不用等
        assert!(matches!(decide(&one, 1, 50_000, n), Decision::Pick(_)));
        // 只有中继：等直连到 1.5 秒（难打洞时 0.6 秒）
        let relay = [m(PathKind::Relay, 30, 30)];
        assert_eq!(decide(&relay, 3, 1_000_000, n), Decision::Wait);
        assert!(matches!(
            decide(&relay, 3, n.direct_wait_us, n),
            Decision::Pick(_)
        ));
        assert!(matches!(
            decide(&relay, 3, 600_000, Limits::HARD),
            Decision::Pick(_)
        ));
        // 时限到了，有什么用什么
        let late = [m(PathKind::Public, 40, 1_900)];
        assert!(matches!(
            decide(&late, 3, n.budget_us, n),
            Decision::Pick(_)
        ));
    }
}
