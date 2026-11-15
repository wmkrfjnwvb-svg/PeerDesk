//! 网络画像与直连方法规划。
//!
//! 输入是两端的网络画像，输出是按优先顺序排列的直连方法，最后总是中继兜底。
//! 这里只做决策，不发包：真正的探测和打洞在 IO 层，结果喂给这里。

use std::net::{IpAddr, SocketAddr};

/// NAT 行为分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NatType {
    /// 公网地址，没有 NAT。
    Open,
    /// 映射与目的地无关（各种锥型）。
    EndpointIndependent,
    /// 对称型，端口按固定步长递增。
    SymmetricSequential {
        /// 相邻两次映射的端口差。
        delta: i32,
    },
    /// 对称型，端口随机。
    SymmetricRandom,
    /// UDP 不通。
    Blocked,
}

/// 根据“同一本地端口向多个探测服务器发包后各自看到的外网地址”判断 NAT 类型。
///
/// `mapped` 按探测顺序排列；`local` 是本机地址，用来识别公网直连。
#[must_use]
pub fn classify_nat(local: &[IpAddr], mapped: &[SocketAddr]) -> NatType {
    let Some(first) = mapped.first() else {
        return NatType::Blocked;
    };
    if mapped.iter().any(|m| m.ip() != first.ip()) {
        // 出口地址都不固定：按最难的情况处理。
        return NatType::SymmetricRandom;
    }
    if mapped.iter().all(|m| m.port() == first.port()) {
        return if local.contains(&first.ip()) {
            NatType::Open
        } else {
            NatType::EndpointIndependent
        };
    }
    let deltas: Vec<i32> = mapped
        .iter()
        .zip(mapped.iter().skip(1))
        .map(|(a, b)| i32::from(b.port()) - i32::from(a.port()))
        .collect();
    match deltas.first() {
        Some(d) if *d != 0 && d.abs() <= 64 && deltas.iter().all(|x| x == d) => {
            NatType::SymmetricSequential { delta: *d }
        }
        Some(_) | None => NatType::SymmetricRandom,
    }
}

/// 一端的网络画像。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetProfile {
    /// NAT 类型。
    pub nat: NatType,
    /// 是否有可用的全局 IPv6 地址。
    pub ipv6: bool,
    /// 是否已取得 UDP 端口映射（UPnP、NAT-PMP、PCP 或手动转发）。
    pub mapped_udp: bool,
    /// 是否已取得 TCP 端口映射。
    pub mapped_tcp: bool,
}

impl NetProfile {
    /// UDP 是否完全不可用。
    #[must_use]
    pub const fn udp_blocked(&self) -> bool {
        match self.nat {
            NatType::Blocked => true,
            NatType::Open
            | NatType::EndpointIndependent
            | NatType::SymmetricSequential { .. }
            | NatType::SymmetricRandom => false,
        }
    }

    /// 是否拥有对端可以直接拨入的 UDP 入口。
    #[must_use]
    pub const fn reachable_udp(&self) -> bool {
        match self.nat {
            NatType::Open => true,
            NatType::EndpointIndependent
            | NatType::SymmetricSequential { .. }
            | NatType::SymmetricRandom => self.mapped_udp,
            NatType::Blocked => false,
        }
    }
}

/// 建立连接的方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Method {
    /// 同一局域网直连。
    Lan,
    /// IPv6 双向同时发包。
    Ipv6,
    /// 直接拨对端的 UDP 入口（公网地址或端口映射）。
    DirectUdp,
    /// UDP 打洞。
    HolePunch,
    /// 端口预测（对端是端口递增的对称 NAT）。
    PortPredict,
    /// 生日攻击（一端是端口随机的对称 NAT）。
    Birthday,
    /// 直接拨对端的 TCP 端口映射。
    DirectTcp,
    /// 中继兜底。
    Relay,
}

impl Method {
    /// 是否不经过中继。
    #[must_use]
    pub const fn is_direct(self) -> bool {
        match self {
            Method::Relay => false,
            Method::Lan
            | Method::Ipv6
            | Method::DirectUdp
            | Method::HolePunch
            | Method::PortPredict
            | Method::Birthday
            | Method::DirectTcp => true,
        }
    }
}

const fn is_symmetric(nat: NatType) -> bool {
    match nat {
        NatType::SymmetricSequential { .. } | NatType::SymmetricRandom => true,
        NatType::Open | NatType::EndpointIndependent | NatType::Blocked => false,
    }
}

const fn is_sequential(nat: NatType) -> bool {
    match nat {
        NatType::SymmetricSequential { .. } => true,
        NatType::Open
        | NatType::EndpointIndependent
        | NatType::SymmetricRandom
        | NatType::Blocked => false,
    }
}

const fn is_random(nat: NatType) -> bool {
    match nat {
        NatType::SymmetricRandom => true,
        NatType::Open
        | NatType::EndpointIndependent
        | NatType::SymmetricSequential { .. }
        | NatType::Blocked => false,
    }
}

/// 给出按优先顺序尝试的方法。结果非空，且最后一个总是 [`Method::Relay`]。
#[must_use]
pub fn plan(a: &NetProfile, b: &NetProfile, same_lan: bool) -> Vec<Method> {
    let mut out = Vec::new();
    if same_lan {
        out.push(Method::Lan);
    }
    // IPv6 走 UDP：任何一端的 UDP 被封都用不了。
    let udp_ok = !a.udp_blocked() && !b.udp_blocked();
    if udp_ok {
        if a.ipv6 && b.ipv6 {
            out.push(Method::Ipv6);
        }
        if a.reachable_udp() || b.reachable_udp() {
            out.push(Method::DirectUdp);
        }
        let (sa, sb) = (is_symmetric(a.nat), is_symmetric(b.nat));
        if !sa && !sb {
            out.push(Method::HolePunch);
        } else if sa != sb {
            // 只有一端是对称 NAT：按那一端的端口规律选技巧。
            let hard = if sa { a.nat } else { b.nat };
            if is_sequential(hard) {
                out.push(Method::PortPredict);
            } else {
                out.push(Method::Birthday);
            }
        } else if is_sequential(a.nat) && is_sequential(b.nat) {
            // 两端都是端口递增型：仍可预测。
            out.push(Method::PortPredict);
        } else if !(is_random(a.nat) && is_random(b.nat)) {
            // 一端递增一端随机：对随机端做生日攻击，成功率较低但值得一试。
            out.push(Method::Birthday);
        }
        // 两端都是随机对称型：不浪费时间。
    }
    if a.mapped_tcp || b.mapped_tcp {
        out.push(Method::DirectTcp);
    }
    out.push(Method::Relay);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sa(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    const fn profile(nat: NatType) -> NetProfile {
        NetProfile {
            nat,
            ipv6: false,
            mapped_udp: false,
            mapped_tcp: false,
        }
    }

    const SEQ: NatType = NatType::SymmetricSequential { delta: 1 };

    #[test]
    fn nat_classification() {
        let local = [ip("192.168.1.5")];
        assert_eq!(classify_nat(&local, &[]), NatType::Blocked);
        assert_eq!(
            classify_nat(&[ip("8.8.8.8")], &[sa("8.8.8.8:5000"), sa("8.8.8.8:5000")]),
            NatType::Open
        );
        assert_eq!(
            classify_nat(&local, &[sa("1.2.3.4:5000"), sa("1.2.3.4:5000")]),
            NatType::EndpointIndependent
        );
        // 只有一个样本：端口无从比较，按与目的地无关处理
        assert_eq!(
            classify_nat(&local, &[sa("1.2.3.4:5000")]),
            NatType::EndpointIndependent
        );
        assert_eq!(
            classify_nat(
                &local,
                &[sa("1.2.3.4:5000"), sa("1.2.3.4:5002"), sa("1.2.3.4:5004")]
            ),
            NatType::SymmetricSequential { delta: 2 }
        );
        assert_eq!(
            classify_nat(&local, &[sa("1.2.3.4:5004"), sa("1.2.3.4:5003")]),
            NatType::SymmetricSequential { delta: -1 }
        );
        // 步长不稳定
        assert_eq!(
            classify_nat(
                &local,
                &[sa("1.2.3.4:5000"), sa("1.2.3.4:5001"), sa("1.2.3.4:5005")]
            ),
            NatType::SymmetricRandom
        );
        // 步长过大视为随机
        assert_eq!(
            classify_nat(&local, &[sa("1.2.3.4:5000"), sa("1.2.3.4:9000")]),
            NatType::SymmetricRandom
        );
        // 步长为 0 后又变化
        assert_eq!(
            classify_nat(
                &local,
                &[sa("1.2.3.4:5000"), sa("1.2.3.4:5000"), sa("1.2.3.4:5001")]
            ),
            NatType::SymmetricRandom
        );
        // 出口地址不固定
        assert_eq!(
            classify_nat(&local, &[sa("1.2.3.4:5000"), sa("1.2.3.5:5000")]),
            NatType::SymmetricRandom
        );
    }

    #[test]
    fn plans_always_end_with_relay() {
        let nats = [
            NatType::Open,
            NatType::EndpointIndependent,
            SEQ,
            NatType::SymmetricRandom,
            NatType::Blocked,
        ];
        for a in nats {
            for b in nats {
                for lan in [false, true] {
                    let p = plan(&profile(a), &profile(b), lan);
                    assert_eq!(p.last(), Some(&Method::Relay));
                    assert_eq!(p.iter().filter(|m| !m.is_direct()).count(), 1);
                    assert_eq!(p.first() == Some(&Method::Lan), lan);
                }
            }
        }
    }

    #[test]
    fn plans_for_typical_pairs() {
        let cone = profile(NatType::EndpointIndependent);
        assert_eq!(
            plan(&cone, &cone, false),
            vec![Method::HolePunch, Method::Relay]
        );
        assert_eq!(
            plan(&profile(NatType::Open), &cone, false),
            vec![Method::DirectUdp, Method::HolePunch, Method::Relay]
        );
        assert_eq!(
            plan(&cone, &profile(SEQ), false),
            vec![Method::PortPredict, Method::Relay]
        );
        assert_eq!(
            plan(&profile(NatType::SymmetricRandom), &cone, false),
            vec![Method::Birthday, Method::Relay]
        );
        assert_eq!(
            plan(&profile(SEQ), &profile(SEQ), false),
            vec![Method::PortPredict, Method::Relay]
        );
        assert_eq!(
            plan(&profile(SEQ), &profile(NatType::SymmetricRandom), false),
            vec![Method::Birthday, Method::Relay]
        );
        // 两端都是随机对称型：只剩中继
        let rnd = profile(NatType::SymmetricRandom);
        assert_eq!(plan(&rnd, &rnd, false), vec![Method::Relay]);
    }

    #[test]
    fn one_good_side_rescues_the_pair() {
        let rnd = profile(NatType::SymmetricRandom);
        // 被控端有 UDP 端口映射：对端再差也能直拨
        let host = NetProfile {
            mapped_udp: true,
            ..rnd
        };
        assert_eq!(
            plan(&host, &rnd, false),
            vec![Method::DirectUdp, Method::Relay]
        );
        // 两端都有 IPv6
        let v6 = NetProfile { ipv6: true, ..rnd };
        assert_eq!(plan(&v6, &v6, false), vec![Method::Ipv6, Method::Relay]);
        // 只有一端有 IPv6 不够
        assert_eq!(plan(&v6, &rnd, false), vec![Method::Relay]);
        // 一端封 UDP，另一端有 TCP 端口映射：TCP 直拨
        let blocked = profile(NatType::Blocked);
        let tcp_host = NetProfile {
            mapped_tcp: true,
            mapped_udp: true,
            ipv6: true,
            ..profile(NatType::EndpointIndependent)
        };
        assert_eq!(
            plan(&blocked, &tcp_host, false),
            vec![Method::DirectTcp, Method::Relay]
        );
        // 封了 UDP 的一端即使声称有映射也不算可达
        let liar = NetProfile {
            mapped_udp: true,
            ..blocked
        };
        assert!(!liar.reachable_udp());
        assert!(liar.udp_blocked());
    }
}
