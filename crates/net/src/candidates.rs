//! 本机地址：被控端登记时报告给服务器，同一个局域网里的控制端可以直接连过来，
//! 不用绕到公网再回来（很多家用路由器不支持这种回环）。

use std::net::{IpAddr, SocketAddr};

use peerdesk_core::paths::{local_kind, Candidate};

/// 值得用的地址：去掉回环、未指定和链路本地地址（后者需要网卡序号），IPv4 在前，去重。
#[must_use]
pub fn usable(addrs: impl IntoIterator<Item = IpAddr>) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = addrs
        .into_iter()
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_link_local(),
            IpAddr::V6(v6) => {
                (v6.segments().first().copied().unwrap_or_default() & 0xffc0) != 0xfe80
            }
        })
        .collect();
    out.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    out.dedup();
    out
}

/// 本机网卡上的地址。取不到时返回空列表。
#[must_use]
pub fn lan_addresses() -> Vec<IpAddr> {
    usable(
        if_addrs::get_if_addrs()
            .unwrap_or_default()
            .into_iter()
            .map(|i| i.ip()),
    )
}

/// 端点绑定在 `bound` 上时，别人能直接连进来的地址：绑定在“所有地址”上时是本机每个
/// 地址加上端口（IPv6 的“所有地址”是双栈，两种地址都算；IPv4 的只算 IPv4）；
/// 绑定在具体地址上时就是它自己。全局 IPv6 地址标为 IPv6 直连，其余为局域网。
#[must_use]
pub fn local_candidates(bound: SocketAddr, lan: &[IpAddr]) -> Vec<Candidate> {
    let ips: Vec<IpAddr> = if bound.ip().is_unspecified() {
        lan.iter()
            .copied()
            .filter(|ip| bound.is_ipv6() || ip.is_ipv4())
            .collect()
    } else {
        usable([bound.ip()])
    };
    ips.into_iter()
        .map(|ip| {
            let addr = SocketAddr::new(ip, bound.port());
            Candidate {
                addr,
                kind: local_kind(&addr),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use peerdesk_core::paths::PathKind;

    #[test]
    fn filters_and_orders() {
        let parse = |s: &str| s.parse::<IpAddr>().unwrap();
        let got = usable(
            [
                "fd00::5",
                "127.0.0.1",
                "192.168.1.20",
                "::1",
                "fe80::1",
                "169.254.3.4",
                "0.0.0.0",
                "10.0.0.2",
                "192.168.1.20",
            ]
            .map(parse),
        );
        assert_eq!(
            got,
            ["10.0.0.2", "192.168.1.20", "fd00::5"].map(parse).to_vec()
        );
        // 只是确认能调用
        let _ = lan_addresses();
    }

    #[test]
    fn candidates_follow_the_binding() {
        let lan: Vec<IpAddr> = ["10.0.0.2", "fd00::5", "2408:8000::5"]
            .map(|s| s.parse().unwrap())
            .to_vec();
        let addrs = |c: Vec<Candidate>| {
            c.into_iter()
                .map(|c| (c.addr.to_string(), c.kind))
                .collect::<Vec<_>>()
        };
        let any4: SocketAddr = "0.0.0.0:4433".parse().unwrap();
        let any6: SocketAddr = "[::]:4433".parse().unwrap();
        assert_eq!(
            addrs(local_candidates(any4, &lan)),
            vec![("10.0.0.2:4433".to_owned(), PathKind::Lan)]
        );
        assert_eq!(
            addrs(local_candidates(any6, &lan)),
            vec![
                ("10.0.0.2:4433".to_owned(), PathKind::Lan),
                ("[fd00::5]:4433".to_owned(), PathKind::Lan),
                ("[2408:8000::5]:4433".to_owned(), PathKind::Ipv6),
            ],
            "双栈端口两种地址都算"
        );
        let one: SocketAddr = "192.168.1.9:7".parse().unwrap();
        assert_eq!(
            addrs(local_candidates(one, &lan)),
            vec![(one.to_string(), PathKind::Lan)]
        );
        let lo: SocketAddr = "127.0.0.1:7".parse().unwrap();
        assert!(local_candidates(lo, &lan).is_empty());
    }
}
