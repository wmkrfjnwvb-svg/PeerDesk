//! 难打洞的 NAT（对称型：每换一个目的地就换一个随机公网端口）的打洞：“生日攻击”。
//!
//! 难的一端开 [`SOCKETS`] 个端口，都向容易的一端的公网地址发探测包，每个端口在自己的 NAT 上
//! 各得到一个随机的公网端口；容易的一端向难的一端公网 IP 的随机端口撒 [`SPRAY`] 个探测包。
//! 只要撒中其中一个，两边的 NAT 就都放行这一对地址：撒中的那个包能到达难的一端的对应端口，
//! 这个端口再发往容易的一端也能通过。公网端口约有 64512 个，撞上的概率约为
//! 1 − e^(−128 × 2400 / 64512) ≈ 99%（Tailscale 的分析见 docs/research/F-nat.md）。
//!
//! 探测包里带着这次连接的暗号（服务器分给两端的随机数），收到时核对，别人的包不会被当成对方。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use peerdesk_core::signal::{parse_probe, probe_packet};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::Node;

/// 难的一端开的端口数。macOS 默认每个进程最多 256 个文件句柄，所以不开到 256，
/// 少开的端口用多撒探测包补回来（撞上的概率取决于两者的乘积）。
pub const SOCKETS: usize = 128;
/// 容易的一端撒的探测包数（约 80 KB）。
pub const SPRAY: usize = 2400;
/// 撒包用的时间：分批发，不一下子冲进网络。
const SPRAY_TIME: Duration = Duration::from_millis(1800);
/// 每批多少个包。
const SPRAY_BATCH: usize = 80;
/// 难的一端每个端口隔多久发一次探测包。
const SOCKET_PROBE_EVERY: Duration = Duration::from_millis(250);
/// 端口号的范围：NAT 一般不分配 1024 以下的端口。
const LOW_PORT: u16 = 1024;

/// 某个地址族的“所有地址”。
pub(crate) fn unspecified(v6: bool) -> IpAddr {
    [
        IpAddr::from(Ipv4Addr::UNSPECIFIED),
        IpAddr::from(Ipv6Addr::UNSPECIFIED),
    ]
    .into_iter()
    .find(|ip| ip.is_ipv6() == v6)
    .unwrap_or(IpAddr::from(Ipv4Addr::UNSPECIFIED))
}

/// 一串随机端口（1024–65535）。
fn random_ports(count: usize) -> Vec<u16> {
    use ring::rand::SecureRandom;
    let mut bytes = vec![0u8; count * 2];
    // 取随机数失败极少见；退回到按顺序扫，效果差一些但仍然有用。
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        bytes
            .iter_mut()
            .enumerate()
            .for_each(|(i, b)| *b = u8::try_from(i % 251).unwrap_or(0));
    }
    let span = u32::from(u16::MAX - LOW_PORT) + 1;
    bytes
        .chunks_exact(2)
        .map(|c| {
            let r = u32::from(u16::from_be_bytes([
                c.first().copied().unwrap_or(0),
                c.get(1).copied().unwrap_or(0),
            ]));
            LOW_PORT + u16::try_from(r % span).unwrap_or(0)
        })
        .collect()
}

/// 容易的一端：从本端点的端口向 `ip` 的随机端口撒探测包。顺便也向 `known`（服务器看到的
/// 对方地址）发，对方的 NAT 万一保留了端口，这一个就够了。
pub async fn spray(node: &Node, ip: IpAddr, known: SocketAddr, nonce: &[u8; 16]) {
    let ports = random_ports(SPRAY);
    let batches = SPRAY.div_ceil(SPRAY_BATCH).max(1);
    let gap = SPRAY_TIME / u32::try_from(batches).unwrap_or(1);
    for batch in ports.chunks(SPRAY_BATCH) {
        node.send_raw(known, &probe_packet(nonce, known.port()));
        for port in batch {
            node.send_raw(SocketAddr::new(ip, *port), &probe_packet(nonce, *port));
        }
        tokio::time::sleep(gap).await;
    }
}

/// 难的一端：开 [`SOCKETS`] 个端口，都向 `peer` 发探测包，等对方撒来的探测包撞上其中一个。
/// 返回撞上的端口（交给 QUIC 用）；`within` 内没有撞上时返回 `None`。
pub async fn sockets(
    peer: SocketAddr,
    nonce: [u8; 16],
    within: Duration,
) -> Option<std::net::UdpSocket> {
    let any = unspecified(peer.is_ipv6());
    let (tx, mut rx) = mpsc::channel(4);
    let mut tasks = JoinSet::new();
    // 打不开的端口（例如文件句柄用完）跳过，少几个端口只是撞上的概率低一点。
    let mut opened = Vec::with_capacity(SOCKETS);
    for _ in 0..SOCKETS {
        opened.push(UdpSocket::bind((any, 0)).await);
    }
    for socket in opened.into_iter().flatten() {
        let tx = tx.clone();
        let _ = tasks.spawn(async move {
            let probe = probe_packet(&nonce, peer.port());
            let mut tick = tokio::time::interval(SOCKET_PROBE_EVERY);
            let mut buf = [0u8; 64];
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        let _ = socket.send_to(&probe, peer).await;
                    }
                    got = socket.recv_from(&mut buf) => {
                        let Ok((len, from)) = got else { continue };
                        let valid = from.ip() == peer.ip()
                            && parse_probe(buf.get(..len).unwrap_or_default(), &nonce).is_some();
                        if valid {
                            // 回一个包，让对方那边这一对地址的记录保持新鲜。
                            let _ = socket.send_to(&probe, from).await;
                            if let Ok(std) = socket.into_std() {
                                let _ = tx.send(std).await;
                            }
                            return;
                        }
                    }
                }
            }
        });
    }
    drop(tx);
    let found = tokio::time::timeout(within, rx.recv()).await.ok().flatten();
    tasks.abort_all();
    found
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn unspecified_follows_the_family() {
        assert!(unspecified(false).is_ipv4() && unspecified(false).is_unspecified());
        assert!(unspecified(true).is_ipv6() && unspecified(true).is_unspecified());
    }

    #[test]
    fn ports_are_in_range_and_spread() {
        let ports = random_ports(2000);
        assert_eq!(ports.len(), 2000);
        assert!(ports.iter().all(|p| *p >= LOW_PORT));
        let distinct: std::collections::BTreeSet<_> = ports.iter().collect();
        assert!(distinct.len() > 1900, "{}", distinct.len());
    }

    /// 回环上：一端开很多端口向对方发探测包，另一端回一个带暗号的包，撞上的端口被交出来；
    /// 暗号不对的包不算。
    #[tokio::test]
    async fn the_socket_that_hears_the_peer_wins() {
        let easy = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let easy_addr = easy.local_addr().unwrap();
        let nonce = [5u8; 16];
        let finder = tokio::spawn(sockets(easy_addr, nonce, Duration::from_secs(5)));
        let mut buf = [0u8; 64];
        let (len, from) = easy.recv_from(&mut buf).await.unwrap();
        assert_eq!(parse_probe(&buf[..len], &nonce), Some(easy_addr.port()));
        easy.send_to(&probe_packet(&[9; 16], 1), from)
            .await
            .unwrap();
        easy.send_to(&probe_packet(&nonce, from.port()), from)
            .await
            .unwrap();
        let won = finder.await.unwrap().unwrap();
        assert_eq!(won.local_addr().unwrap().port(), from.port());
        // 没人回应：超时返回 None
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        assert!(sockets(
            silent.local_addr().unwrap(),
            nonce,
            Duration::from_millis(300)
        )
        .await
        .is_none());
    }
}
