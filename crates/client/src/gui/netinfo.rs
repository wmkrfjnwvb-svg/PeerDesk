//! 本机的局域网地址，显示给用户告诉控制端。

use std::net::IpAddr;

/// 值得显示的地址：去掉回环、未指定和链路本地地址（后者需要网卡序号，不方便告诉别人），
/// IPv4 在前，去重。
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
