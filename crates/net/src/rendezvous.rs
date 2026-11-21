//! 通过信令服务器找到对方并建立连接：先试直连（打洞），不通再走中继。
//!
//! 标准的 QUIC 库不能给一条已有的连接换对端地址，所以直连和中继是两条独立的连接尝试，
//! 这里选能用的那条。会话建立之后从中继切换到直连在应用层做（另建连接后把会话搬过去）。

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::{Duration, Instant};

use peerdesk_core::relay::{unwrap, wrap, SessionId, RELAY_HEADER};
use peerdesk_core::signal::{
    format_code, observe_packet, PunchInfo, PunchPlan, RelayGrant, SignalMsg,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::stream::{read_signal, write_kind, write_signal, StreamKind};
use crate::{Fingerprint, Link, NetError, Node};

/// 多端口打洞（生日攻击）最多用这么久。
const BIRTHDAY_TIMEOUT: Duration = Duration::from_millis(3500);
/// NAT 探测包发几份。
const OBSERVE_COPIES: usize = 3;
/// 局域网地址的握手最多等这么久：同一个局域网里往返只要几毫秒。
const LAN_TIMEOUT: Duration = Duration::from_millis(600);
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
    /// 打洞方式。
    pub punch: PunchInfo,
    /// 备用的中继通道。
    pub relay: Option<RelayGrant>,
}

/// 被控端在服务器上的登记。保持它存活，设备才算在线。
#[derive(Debug)]
pub struct Registration {
    /// 服务器看到的本机公网地址。
    pub observed: SocketAddr,
    /// 服务器分配的 9 位设备码。
    pub code: u32,
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
    let locals = crate::candidates::local_candidates(
        node.local_addr()?,
        &crate::candidates::lan_addresses(),
    );
    write_signal(&mut send, &SignalMsg::Register { locals }).await?;
    let SignalMsg::Registered {
        observed,
        code,
        probe,
    } = read_signal(&mut recv).await?
    else {
        return Err(NetError::Protocol("服务器没有确认登记"));
    };
    if let Some(probe) = probe {
        observe(node, probe);
    }
    let (tx, incoming) = mpsc::channel(8);
    let task = tokio::spawn(async move {
        // send 留在这里：流的发送端被丢弃会让服务器认为我们下线了。
        let _keep = send;
        while let Ok(msg) = read_signal(&mut recv).await {
            if let SignalMsg::Incoming {
                peer,
                addr,
                punch,
                relay,
            } = msg
            {
                let incoming = Incoming {
                    peer,
                    addr,
                    punch,
                    relay,
                };
                if tx.send(incoming).await.is_err() {
                    break;
                }
            }
        }
    });
    Ok(Registration {
        observed,
        code,
        incoming,
        _link: link,
        task,
    })
}

/// 从连接用的端口向服务器的探测地址发几个 NAT 探测包（丢一两个没关系）。
fn observe(node: &Node, probe: SocketAddr) {
    let packet = observe_packet(node.fingerprint());
    for _ in 0..OBSERVE_COPIES {
        node.send_raw(probe, &packet);
    }
}

/// 被控端：响应一次连接请求——按两端的 NAT 类型打洞，并接上中继通道。返回的转发器要保持到会话结束。
/// 本端是难打洞的一端时，在后台开很多端口找通路，找到后主动连接控制端，这条连接和普通接受到的
/// 连接一样从 [`Node::accept`] 出来。
pub async fn answer(node: &Arc<Node>, incoming: &Incoming) -> Result<Option<Shim>, NetError> {
    let shim = match incoming.relay {
        Some(grant) => {
            let mut endpoint = node.local_addr()?;
            if endpoint.ip().is_unspecified() {
                // 转发器和中继是同一地址族。双栈的 IPv6 端点也收 IPv4，所以中继是 IPv4 时
                // 用 IPv4 回环地址把包交给它。
                endpoint.set_ip(local_ip(endpoint.is_ipv6() && grant.addr.is_ipv6(), true));
            }
            Some(Shim::start(grant, Some(endpoint)).await?)
        }
        None => None,
    };
    let plan = incoming.punch.plan();
    crate::note!(
        info,
        "被控：{} 要连本机，打洞方式 {plan:?}（本机 NAT {:?}，对方 {:?}）",
        incoming.addr,
        incoming.punch.me,
        incoming.punch.peer
    );
    match plan {
        PunchPlan::Plain => punch(node, incoming.addr).await,
        PunchPlan::Skip => {}
        PunchPlan::Spray => {
            let (node, addr, nonce) = (Arc::clone(node), incoming.addr, incoming.punch.nonce);
            tokio::spawn(async move { crate::punch::spray(&node, addr.ip(), addr, &nonce).await });
        }
        PunchPlan::Sockets => {
            let (node, addr, peer, nonce) = (
                Arc::clone(node),
                incoming.addr,
                incoming.peer,
                incoming.punch.nonce,
            );
            tokio::spawn(async move {
                let Some(socket) = crate::punch::sockets(addr, nonce, BIRTHDAY_TIMEOUT).await
                else {
                    crate::note!(info, "被控：多端口打洞没有撞上 {addr}");
                    return;
                };
                match node
                    .connect_via(socket, addr, Some(&peer), DIRECT_TIMEOUT)
                    .await
                {
                    Ok(link) => {
                        crate::note!(info, "被控：多端口打洞成功，主动连上 {addr}");
                        node.offer(link).await;
                    }
                    Err(e) => crate::note!(warn, "被控：多端口打洞连接 {addr} 失败：{e}"),
                }
            });
        }
    }
    Ok(shim)
}

/// 要连哪台设备。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lookup {
    /// 按指纹。
    Device(Fingerprint),
    /// 按服务器分配的设备码。
    Code(u32),
}

/// 一条已经连到目标设备的连接。
#[derive(Debug)]
pub struct Dialed {
    /// 连接。
    pub link: Link,
    /// 走的是哪条路。
    pub path: Path,
    /// 对方设备的指纹（按设备码连接时由服务器告知，连接本身核对过）。
    pub device: Fingerprint,
    /// 按两端 NAT 类型选的打洞方式。
    pub plan: PunchPlan,
    /// 各阶段耗时，写日志和诊断用。
    pub timing: DialTiming,
    /// 直连失败的原因（走了中继时）。
    pub direct_error: Option<String>,
    /// 走中继时的会话号，可以用来对账。
    pub relay_session: Option<SessionId>,
    /// 走中继时的本机转发器，要保持到会话结束。
    pub shim: Option<Shim>,
}

/// 等目标设备从它撞上的端口连过来（对方是难打洞的一端时）。连过来的不是它就断开，继续等。
async fn accept_from(node: &Node, device: &Fingerprint) -> Result<Link, NetError> {
    while let Some(next) = node.accept().await {
        match next {
            Ok(link) if link.peer_fingerprint() == device => return Ok(link),
            // 别的设备连过来、握手失败：都不是要等的那条，继续等。
            Ok(other) => other.close("unexpected peer"),
            Err(_) => {}
        }
    }
    Err(NetError::Closed("端点已关闭".into()))
}

/// 同时进行几次连接尝试，返回第一个成功的；都失败时返回最后一个错误（指纹不符优先）。
/// 一次进行中的连接尝试。
type Attempt<'a> = Pin<Box<dyn Future<Output = Result<Link, NetError>> + Send + 'a>>;

async fn first_ok(mut attempts: Vec<Attempt<'_>>) -> Result<Link, NetError> {
    let mut error = NetError::Timeout("直连");
    std::future::poll_fn(|cx| {
        let mut pending = Vec::with_capacity(attempts.len());
        for mut attempt in attempts.drain(..) {
            match attempt.as_mut().poll(cx) {
                Poll::Ready(Ok(link)) => return Poll::Ready(Ok(link)),
                Poll::Ready(Err(e @ NetError::PinMismatch { .. })) => return Poll::Ready(Err(e)),
                Poll::Ready(Err(e)) => error = e,
                Poll::Pending => pending.push(attempt),
            }
        }
        attempts = pending;
        if attempts.is_empty() {
            Poll::Ready(Err(std::mem::replace(
                &mut error,
                NetError::Timeout("直连"),
            )))
        } else {
            Poll::Pending
        }
    })
    .await
}

/// 通过服务器建立连接时各阶段的耗时。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DialTiming {
    /// 连上服务器。
    pub server_connect: Duration,
    /// 向服务器查询对方地址。
    pub lookup: Duration,
    /// 尝试对方的局域网地址（两端不在同一个出口后面时为 0）。
    pub lan: Duration,
    /// 尝试直连（包括失败的情况）。
    pub direct: Duration,
    /// 经中继连上（没走中继时为 0）。
    pub relay: Duration,
}

/// 控制端：通过服务器连接 `lookup` 指定的设备。
pub async fn dial(
    node: &Node,
    server: &Server,
    lookup: &Lookup,
    policy: PathPolicy,
) -> Result<Dialed, NetError> {
    let started = Instant::now();
    let signal = node.connect(server.addr, server.pin.as_ref()).await?;
    let mut timing = DialTiming {
        server_connect: started.elapsed(),
        ..DialTiming::default()
    };
    let (mut send, mut recv) = signal.open_stream().await?;
    write_kind(&mut send, StreamKind::Signal).await?;
    let ask = match lookup {
        Lookup::Device(target) => SignalMsg::Connect { target: *target },
        Lookup::Code(code) => SignalMsg::ConnectCode { code: *code },
    };
    let asked = Instant::now();
    write_signal(&mut send, &ask).await?;
    let mut reply = read_signal(&mut recv).await?;
    if let SignalMsg::Probe { relay } = reply {
        // 服务器要先判断本端的 NAT 类型：探测一下，再问一次。
        observe(node, relay);
        write_signal(&mut send, &ask).await?;
        reply = read_signal(&mut recv).await?;
    }
    let (device, addr, locals, you, punch_info, relay) = match reply {
        SignalMsg::Peer {
            device,
            addr,
            locals,
            you,
            punch,
            relay,
        } => (device, addr, locals, you, punch, relay),
        SignalMsg::NotFound => {
            return Err(NetError::Connect(match lookup {
                Lookup::Device(_) => "设备不在线".into(),
                Lookup::Code(code) => format!("设备码 {} 不存在或设备不在线", format_code(*code)),
            }));
        }
        SignalMsg::Register { .. }
        | SignalMsg::Probe { .. }
        | SignalMsg::Registered { .. }
        | SignalMsg::Connect { .. }
        | SignalMsg::ConnectCode { .. }
        | SignalMsg::Incoming { .. } => return Err(NetError::Protocol("服务器的应答不是地址")),
    };
    timing.lookup = asked.elapsed();
    if let Lookup::Device(target) = lookup {
        if *target != device {
            return Err(NetError::Protocol("服务器给的设备不是要连的那台"));
        }
    }
    signal.close("done");
    let plan = punch_info.plan();
    crate::note!(
        info,
        "服务器 {}：连上 {:?}，查到对方 {addr}（{:?}），打洞方式 {plan:?}（本机 NAT {:?}，对方 {:?}），中继{}",
        server.addr,
        timing.server_connect,
        timing.lookup,
        punch_info.me,
        punch_info.peer,
        if relay.is_some() {
            "可用"
        } else {
            "未开启"
        }
    );
    let target = &device;

    let try_direct = !matches!(policy, PathPolicy::RelayOnly);
    // 两端的公网 IP 相同：在同一个出口后面，先试对方的局域网地址。
    let mine = node.local_addr()?;
    let lan: Vec<SocketAddr> = locals
        .into_iter()
        .filter(|l| l.is_ipv4() == mine.is_ipv4() && *l != addr)
        .collect();
    if try_direct && you.ip() == addr.ip() && !lan.is_empty() {
        let tried = Instant::now();
        let mut attempts: Vec<Attempt<'_>> = Vec::with_capacity(lan.len());
        for l in &lan {
            attempts.push(Box::pin(node.connect_within(
                *l,
                Some(&device),
                LAN_TIMEOUT,
            )));
        }
        let found = first_ok(attempts).await;
        timing.lan = tried.elapsed();
        match found {
            Ok(link) => {
                crate::note!(
                    info,
                    "局域网直连 {} 成功，用时 {:?}",
                    link.remote_addr(),
                    timing.lan
                );
                return Ok(Dialed {
                    link,
                    path: Path::Direct,
                    device,
                    plan,
                    timing,
                    direct_error: None,
                    relay_session: None,
                    shim: None,
                });
            }
            Err(e @ NetError::PinMismatch { .. }) => return Err(e),
            Err(e) => crate::note!(info, "局域网地址 {lan:?} 都不通（{:?}）：{e}", timing.lan),
        }
    }
    let direct_error = if try_direct {
        let tried = Instant::now();
        let direct = match plan {
            PunchPlan::Plain => {
                // 对方同时也在向我们打洞。
                node.punch(addr);
                node.connect_within(addr, Some(&device), DIRECT_TIMEOUT)
                    .await
            }
            PunchPlan::Skip => Err(NetError::Connect(
                "两端的 NAT 都是对称型（换目的地就换端口），打洞几乎不可能成功，直接走中继".into(),
            )),
            PunchPlan::Sockets => {
                match crate::punch::sockets(addr, punch_info.nonce, BIRTHDAY_TIMEOUT).await {
                    Some(socket) => {
                        node.connect_via(socket, addr, Some(&device), DIRECT_TIMEOUT)
                            .await
                    }
                    None => Err(NetError::Timeout("多端口打洞")),
                }
            }
            PunchPlan::Spray => {
                let spraying = crate::punch::spray(node, addr.ip(), addr, &punch_info.nonce);
                let accepting = accept_from(node, &device);
                tokio::pin!(spraying, accepting);
                let deadline = tokio::time::sleep(BIRTHDAY_TIMEOUT);
                tokio::pin!(deadline);
                let mut sprayed = false;
                loop {
                    tokio::select! {
                        () = &mut spraying, if !sprayed => sprayed = true,
                        link = &mut accepting => break link,
                        () = &mut deadline => break Err(NetError::Timeout("等对方从撞上的端口连过来")),
                    }
                }
            }
        };
        timing.direct = tried.elapsed();
        match direct {
            Ok(link) => {
                crate::note!(
                    info,
                    "直连 {} 成功（{plan:?}），用时 {:?}",
                    link.remote_addr(),
                    timing.direct
                );
                return Ok(Dialed {
                    link,
                    path: Path::Direct,
                    device,
                    plan,
                    timing,
                    direct_error: None,
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
    let tried = Instant::now();
    let shim = Shim::start(grant, None).await?;
    let link = node.connect(shim.local_addr(), Some(target)).await?;
    timing.relay = tried.elapsed();
    crate::note!(
        info,
        "经中继 {} 连上，用时 {:?}；直连失败：{}",
        grant.addr,
        timing.relay,
        direct_error
            .as_ref()
            .map_or_else(|| "没有尝试".to_owned(), ToString::to_string)
    );
    Ok(Dialed {
        link,
        path: Path::Relay,
        device,
        plan,
        timing,
        direct_error: direct_error.map(|e| e.to_string()),
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
