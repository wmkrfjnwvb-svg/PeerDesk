//! 区域感知的节点选择。客户端选服务器、服务端选集群节点都用同一套规则。
//!
//! 顺序：未过载的节点优先；内网优先；同区域优先；然后按配置的区域回退顺序；
//! 同一层内比延迟，再比负载。过载的节点排到最后，流量自然溢出到下一层。

use std::net::IpAddr;

/// 节点标识。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

/// 负载达到这个千分比视为过载。
pub const OVERLOAD_PERMILLE: u16 = 900;

/// 一个候选节点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// 节点标识。
    pub id: NodeId,
    /// 节点所在区域。
    pub region: String,
    /// 是否与请求方在同一内网。
    pub lan: bool,
    /// 实测往返延迟（毫秒）；未测为 `None`。
    pub rtt_ms: Option<u32>,
    /// 负载（千分比）。
    pub load_permille: u16,
    /// 是否健康。不健康的节点不参与选择。
    pub healthy: bool,
}

impl Candidate {
    /// 是否过载。
    #[must_use]
    pub const fn overloaded(&self) -> bool {
        self.load_permille >= OVERLOAD_PERMILLE
    }
}

/// 候选节点相对请求方的层级，越小越优先。
///
/// 0 是内网，1 是同区域，之后按 `fallback` 里的顺序，其余区域排最后。
#[must_use]
pub fn tier(client_region: &str, fallback: &[String], candidate: &Candidate) -> usize {
    if candidate.lan {
        return 0;
    }
    if candidate.region == client_region {
        return 1;
    }
    fallback
        .iter()
        .position(|r| *r == candidate.region)
        .map_or(usize::MAX, |i| i.saturating_add(2))
}

/// 把健康的候选节点按优先顺序排好。结果是确定的：完全相同时按节点标识排。
#[must_use]
pub fn rank(client_region: &str, fallback: &[String], candidates: &[Candidate]) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = candidates.iter().filter(|c| c.healthy).cloned().collect();
    out.sort_by_key(|c| {
        (
            c.overloaded(),
            tier(client_region, fallback, c),
            c.rtt_ms.is_none(),
            c.rtt_ms.unwrap_or(u32::MAX),
            c.load_permille,
            c.id,
        )
    });
    out
}

/// 选出最优的候选节点；没有健康节点时返回 `None`。
#[must_use]
pub fn pick(
    client_region: &str,
    fallback: &[String],
    candidates: &[Candidate],
) -> Option<Candidate> {
    rank(client_region, fallback, candidates).into_iter().next()
}

/// 地址是否属于内网（私有、回环、链路本地、IPv6 唯一本地地址）。
#[must_use]
pub const fn is_lan_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: u64, region: &str) -> Candidate {
        Candidate {
            id: NodeId(id),
            region: region.into(),
            lan: false,
            rtt_ms: None,
            load_permille: 0,
            healthy: true,
        }
    }

    fn ids(list: &[Candidate]) -> Vec<u64> {
        list.iter().map(|c| c.id.0).collect()
    }

    #[test]
    fn lan_then_same_region_then_fallback_order() {
        let fallback = vec!["eu".to_owned(), "ap".to_owned()];
        let cands = vec![
            cand(1, "sa"),
            cand(2, "ap"),
            cand(3, "eu"),
            cand(4, "us"),
            Candidate {
                lan: true,
                ..cand(5, "sa")
            },
        ];
        assert_eq!(ids(&rank("us", &fallback, &cands)), vec![5, 4, 3, 2, 1]);
        assert_eq!(pick("us", &fallback, &cands).unwrap().id, NodeId(5));
        assert_eq!(tier("us", &fallback, &cands[0]), usize::MAX);
        assert_eq!(tier("us", &fallback, &cands[1]), 3);
    }

    #[test]
    fn unhealthy_nodes_are_skipped() {
        let cands = vec![
            Candidate {
                healthy: false,
                ..cand(1, "us")
            },
            cand(2, "eu"),
        ];
        assert_eq!(ids(&rank("us", &[], &cands)), vec![2]);
        assert!(pick("us", &[], &[cands[0].clone()]).is_none());
        assert!(pick("us", &[], &[]).is_none());
    }

    #[test]
    fn overload_spills_to_next_tier() {
        let cands = vec![
            Candidate {
                load_permille: OVERLOAD_PERMILLE,
                ..cand(1, "us")
            },
            cand(2, "eu"),
        ];
        assert_eq!(ids(&rank("us", &[], &cands)), vec![2, 1]);
        // 刚好低于阈值不算过载
        let cands = vec![
            Candidate {
                load_permille: OVERLOAD_PERMILLE - 1,
                ..cand(1, "us")
            },
            cand(2, "eu"),
        ];
        assert_eq!(ids(&rank("us", &[], &cands)), vec![1, 2]);
        // 全部过载时仍然给出结果
        let all: Vec<_> = (1..=2)
            .map(|i| Candidate {
                load_permille: 1000,
                ..cand(i, "us")
            })
            .collect();
        assert_eq!(ids(&rank("us", &[], &all)), vec![1, 2]);
    }

    #[test]
    fn within_a_tier_rtt_then_load_then_id() {
        let cands = vec![
            cand(1, "us"), // 未测延迟排在已测之后
            Candidate {
                rtt_ms: Some(30),
                ..cand(2, "us")
            },
            Candidate {
                rtt_ms: Some(10),
                load_permille: 500,
                ..cand(3, "us")
            },
            Candidate {
                rtt_ms: Some(10),
                load_permille: 100,
                ..cand(4, "us")
            },
            Candidate {
                rtt_ms: Some(10),
                load_permille: 100,
                ..cand(0, "us")
            },
        ];
        assert_eq!(ids(&rank("us", &[], &cands)), vec![0, 4, 3, 2, 1]);
    }

    #[test]
    fn lan_addresses() {
        for s in [
            "10.0.0.1",
            "172.16.5.5",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
            "::1",
            "fd00::1",
            "fc00::1",
            "fe80::1",
        ] {
            assert!(is_lan_ip(s.parse().unwrap()), "{s}");
        }
        for s in ["8.8.8.8", "172.32.0.1", "2001:db8::1", "fec0::1", "fe00::1"] {
            assert!(!is_lan_ip(s.parse().unwrap()), "{s}");
        }
    }
}
