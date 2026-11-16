//! 通过信令服务器找到对方并建立连接：先试直连（打洞），不通再走中继。
//!
//! 标准的 QUIC 库不能给一条已有的连接换对端地址，所以直连和中继是两条独立的连接尝试，
//! 这里选能用的那条。会话建立之后从中继切换到直连在应用层做（另建连接后把会话搬过去）。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::relay::{unwrap, wrap, SessionId, RELAY_HEADER};
use peerdesk_core::signal::{RelayGrant, SignalMsg};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::stream::{read_signal, write_kind, write_signal, StreamKind};
use crate::{Fingerprint, Link, NetError, Node};

/// 直连握手最多等这么久，超时就改走中继。
const DIRECT_TIMEOUT: Duration = Duration::from_millis(2500);
/// 打洞包发几轮、每轮间隔多久。
const PUNCH_ROUNDS: u32 = 5;
const PUNCH_GAP: Duration = Duration::from_millis(100);
/// 中继通道的保活间隔：让中继和沿途的 NAT 记住本端的地址。
const SHIM_KEEPALIVE: Duration = Duration::from_secs(5);

/// 信令服务器的位置和身份。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Server {
    /// 地址。
    pub addr: SocketAddr,
    /// 服务器的证书指纹；给了就必须一致。
    pub pin: Option<Fingerprint>,
}

/// 连接走的是哪条路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// 两端直连，不经过服务器。
    Direct,
    /// 经中继转发。
    Relay,
}

/// 允许走哪些路。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathPolicy {
    /// 先直连，不通再中继。
    Auto,
    /// 只允许直连。
    DirectOnly,
    /// 只走中继（用于测试，或者不想暴露自己地址的场合）。
    RelayOnly,
}

/// 向 `addr` 发几轮打洞包。
pub async fn punch(node: &Node, addr: SocketAddr) {
    for _ in 0..PUNCH_ROUNDS {
        node.punch(addr);
        tokio::time::sleep(PUNCH_GAP).await;
    }
}

/// 本机地址：`loopback` 为真给回环地址，否则给“所有地址”。
fn local_ip(v6: bool, loopback: bool) -> std::net::IpAddr {
    use std::net::{Ipv4Addr, Ipv6Addr};
    match (v6, loopback) {
        (false, false) => Ipv4Addr::UNSPECIFIED.into(),
        (false, true) => Ipv4Addr::LOCALHOST.into(),
        (true, false) => Ipv6Addr::UNSPECIFIED.into(),
        (true, true) => Ipv6Addr::LOCALHOST.into(),
    }
}

/// 本机上的一个小转发器：把 QUIC 端点的包加上中继包头发给中继，把中继来的包去掉包头交给端点。
/// 这样 QUIC 库完全不用知道中继的存在。句柄被丢弃时转发器停止。
#[derive(Debug)]
pub struct Shim {
    local: SocketAddr,
    task: JoinHandle<()>,
}

impl Shim {
    /// 启动。`endpoint` 是本机 QUIC 端点的地址：被控端要给（中继来的包交给它），
    /// 控制端传 `None`（由第一个从本机发来的包确定）。
    pub async fn start(grant: RelayGrant, endpoint: Option<SocketAddr>) -> Result<Self, NetError> {
        let io = |e: std::io::Error| NetError::Io(format!("启动中继转发失败：{e}"));
        let v6 = grant.addr.is_ipv6();
        let socket = UdpSocket::bind((local_ip(v6, false), 0))
            .await
            .map_err(io)?;
        let port = socket.local_addr().map_err(io)?.port();
        let local = SocketAddr::new(local_ip(v6, true), port);
        let task = tokio::spawn(run_shim(socket, grant, endpoint));
        Ok(Self { local, task })
    }

    /// 本机 QUIC 端点应当把这个地址当作“对端”。
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run_shim(socket: UdpSocket, grant: RelayGrant, mut endpoint: Option<SocketAddr>) {
    let hello = wrap(&grant.session, grant.side, &[]);
    let mut keepalive = tokio::time::interval(SHIM_KEEPALIVE);
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            // 空载荷的包只用来让中继记住本端的地址。
            _ = keepalive.tick() => { let _ = socket.send_to(&hello, grant.addr).await; }
            received = socket.recv_from(&mut buf) => {
                let Ok((len, from)) = received else { continue };
                let packet = buf.get(..len).unwrap_or_default();
                if from == grant.addr {
                    // 中继来的：只接受本会话、对方方向的包。
                    let Some((session, side, payload)) = unwrap(packet) else { continue };
                    if session != grant.session || side == grant.side || payload.is_empty() {
                        continue;
                    }
                    if let Some(to) = endpoint {
                        let _ = socket.send_to(payload, to).await;
                    }
                } else if from.ip().is_loopback() || endpoint == Some(from) {
                    // 本机端点发来的。
                    endpoint.get_or_insert(from);
                    if len + RELAY_HEADER <= buf.len() {
                        let _ = socket.send_to(&wrap(&grant.session, grant.side, packet), grant.addr).await;
                    }
                }
            }
        }
    }
}

/// 被控端收到的一次连接请求。
#[derive(Debug)]
pub struct Incoming {
    /// 控制端的指纹。
    pub peer: Fingerprint,
    /// 控制端的公网地址。
    pub addr: SocketAddr,
    /// 备用的中继通道。
    pub relay: Option<RelayGrant>,
}

/// 被控端在服务器上的登记。保持它存活，设备才算在线。
#[derive(Debug)]
pub struct Registration {
    /// 服务器看到的本机公网地址。
    pub observed: SocketAddr,
    /// 连接请求从这里来。服务器断开时通道关闭。
    pub incoming: mpsc::Receiver<Incoming>,
    _link: Arc<Link>,
    task: JoinHandle<()>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// 被控端：向服务器登记在线。
pub async fn register(node: &Node, server: &Server) -> Result<Registration, NetError> {
    let link = Arc::new(node.connect(server.addr, server.pin.as_ref()).await?);
    let (mut send, mut recv) = link.open_stream().await?;
    write_kind(&mut send, StreamKind::Signal).await?;
    write_signal(&mut send, &SignalMsg::Register).await?;
    let SignalMsg::Registered { observed } = read_signal(&mut recv).await? else {
        return Err(NetError::Protocol("服务器没有确认登记"));
    };
    let (tx, incoming) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        // send 留在这里：流的发送端被丢弃会让服务器认为我们下线了。
        let _keep = send;
        while let Ok(msg) = read_signal(&mut recv).await {
            if let SignalMsg::Incoming { peer, addr, relay } = msg {
                if tx.send(Incoming { peer, addr, relay }).await.is_err() {
                    break;
                }
            }
        }
    });
    Ok(Registration {
        observed,
        incoming,
        _link: link,
        task,
    })
}

/// 被控端：响应一次连接请求——向对方打洞，并接上中继通道。返回的转发器要保持到会话结束。
pub async fn answer(node: &Node, incoming: &Incoming) -> Result<Option<Shim>, NetError> {
    let shim = match incoming.relay {
        Some(grant) => {
            let mut endpoint = node.local_addr()?;
            if endpoint.ip().is_unspecified() {
                endpoint.set_ip(local_ip(endpoint.is_ipv6(), true));
            }
            Some(Shim::start(grant, Some(endpoint)).await?)
        }
        None => None,
    };
    punch(node, incoming.addr).await;
    Ok(shim)
}

/// 一条已经连到目标设备的连接。
#[derive(Debug)]
pub struct Dialed {
    /// 连接。
    pub link: Link,
    /// 走的是哪条路。
    pub path: Path,
    /// 走中继时的会话号，可以用来对账。
    pub relay_session: Option<SessionId>,
    /// 走中继时的本机转发器，要保持到会话结束。
    pub shim: Option<Shim>,
}

/// 控制端：通过服务器连接 `target`。
pub async fn dial(
    node: &Node,
    server: &Server,
    target: &Fingerprint,
    policy: PathPolicy,
) -> Result<Dialed, NetError> {
    let signal = node.connect(server.addr, server.pin.as_ref()).await?;
    let (mut send, mut recv) = signal.open_stream().await?;
    write_kind(&mut send, StreamKind::Signal).await?;
    write_signal(&mut send, &SignalMsg::Connect { target: *target }).await?;
    let (addr, relay) = match read_signal(&mut recv).await? {
        SignalMsg::Peer { addr, relay } => (addr, relay),
        SignalMsg::NotFound => return Err(NetError::Connect("设备不在线".into())),
        SignalMsg::Register
        | SignalMsg::Registered { .. }
        | SignalMsg::Connect { .. }
        | SignalMsg::Incoming { .. } => return Err(NetError::Protocol("服务器的应答不是地址")),
    };
    signal.close("done");

    let try_direct = !matches!(policy, PathPolicy::RelayOnly);
    let direct_error = if try_direct {
        // 对方同时也在向我们打洞。
        node.punch(addr);
        match node
            .connect_within(addr, Some(target), DIRECT_TIMEOUT)
            .await
        {
            Ok(link) => {
                return Ok(Dialed {
                    link,
                    path: Path::Direct,
                    relay_session: None,
                    shim: None,
                });
            }
            // 指纹不符说明连到的不是目标设备：不能退到中继去掩盖它。
            Err(e @ NetError::PinMismatch { .. }) => return Err(e),
            Err(e) => Some(e),
        }
    } else {
        None
    };
    let grant = match (policy, relay) {
        (PathPolicy::DirectOnly, _) | (_, None) => {
            return Err(
                direct_error.unwrap_or_else(|| NetError::Connect("服务器没有提供中继".into()))
            );
        }
        (PathPolicy::Auto | PathPolicy::RelayOnly, Some(grant)) => grant,
    };
    let shim = Shim::start(grant, None).await?;
    let link = node.connect(shim.local_addr(), Some(target)).await?;
    Ok(Dialed {
        link,
        path: Path::Relay,
        relay_session: Some(grant.session),
        shim: Some(shim),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn local_ip_covers_both_families() {
        assert!(local_ip(false, false).is_unspecified() && local_ip(false, false).is_ipv4());
        assert!(local_ip(false, true).is_loopback() && local_ip(false, true).is_ipv4());
        assert!(local_ip(true, false).is_unspecified() && local_ip(true, false).is_ipv6());
        assert!(local_ip(true, true).is_loopback() && local_ip(true, true).is_ipv6());
    }

    /// 控制端的转发器在本机端点开口之前不知道把包交给谁，只能丢弃；过大的包也丢弃。
    #[tokio::test]
    async fn shim_drops_what_it_cannot_forward() {
        let relay = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let grant = RelayGrant {
            addr: relay.local_addr().unwrap(),
            session: [3; 16],
            side: 0,
        };
        let shim = Shim::start(grant, None).await.unwrap();
        let mut buf = [0u8; 4096];
        // 第一个包是登记用的空包
        let (len, shim_addr) = relay.recv_from(&mut buf).await.unwrap();
        assert_eq!(len, RELAY_HEADER);
        // 端点还没出现：中继来的包没处可交
        relay
            .send_to(&wrap(&[3; 16], 1, b"early"), shim_addr)
            .await
            .unwrap();
        // 端点发一个过大的包（被丢弃），再发一个正常的
        let endpoint = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        endpoint
            .send_to(&[1u8; 2040], shim.local_addr())
            .await
            .unwrap();
        endpoint.send_to(b"hello", shim.local_addr()).await.unwrap();
        let (len, _) = relay.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], wrap(&[3; 16], 0, b"hello").as_slice());
        // 现在中继来的包能交到端点；别的会话、自己方向、空载荷的都不交
        relay
            .send_to(&wrap(&[9; 16], 1, b"other"), shim_addr)
            .await
            .unwrap();
        relay
            .send_to(&wrap(&[3; 16], 0, b"echo"), shim_addr)
            .await
            .unwrap();
        relay.send_to(b"junk", shim_addr).await.unwrap();
        relay
            .send_to(&wrap(&[3; 16], 1, b"reply"), shim_addr)
            .await
            .unwrap();
        let (len, _) = endpoint.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..len], b"reply");
    }
}
