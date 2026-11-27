//! 通过信令服务器找到对方并建立连接：先试直连（打洞），不通再走中继。
//!
//! 标准的 QUIC 库不能给一条已有的连接换对端地址，所以直连和中继是两条独立的连接尝试，
//! 这里选能用的那条。会话建立之后从中继切换到直连在应用层做（另建连接后把会话搬过去）。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use peerdesk_core::paths::{Candidate, Limits, PathKind, NAT_PENALTY_US};
use peerdesk_core::relay::{parse_notice, unwrap, wrap, SessionId, RELAY_HEADER};
use peerdesk_core::signal::{
    format_code, observe_packet, Features, PunchInfo, PunchPlan, RelayGrant, SignalMsg,
};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::stream::{read_signal, write_kind, write_signal, StreamKind};
use crate::{Fingerprint, Link, NetError, Node};

/// 被控端向控制端的候选地址发 ping 的时长（控制端探测加多端口打洞都在这之内）。
const PINGING_TIME: Duration = Duration::from_millis(5000);
/// 是否优先 IPv6 直连（见 [`peerdesk_core::paths::NAT_PENALTY_US`]）。默认是（海外优先）。
static PREFER_IPV6: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 设置是否优先 IPv6 直连。客户端按界面语言设置：英文（海外）优先 IPv6；中文只比延迟
/// （国内 IPv6 跨运营商时延迟和丢包常常比 IPv4 差）。
pub fn set_prefer_ipv6(on: bool) {
    PREFER_IPV6.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// 当前是否优先 IPv6 直连。
#[must_use]
pub fn prefer_ipv6() -> bool {
    PREFER_IPV6.load(std::sync::atomic::Ordering::Relaxed)
}

/// 一端难打洞时，先让快路径试这么久再开始多端口打洞（和 `Limits::HARD` 的等待一致）。
const HARD_HEAD_START: Duration = Duration::from_millis(1000);
/// 普通打洞时，等这么久控制端还没连上来，被控端就开始多端口打洞（和 `Limits::NORMAL` 的等待一致）。
const PLAIN_FALLBACK: Duration = Duration::from_millis(1500);
/// 多端口打洞（生日攻击）最多用这么久。
const BIRTHDAY_TIMEOUT: Duration = Duration::from_millis(3500);
/// NAT 探测包发几份。
const OBSERVE_COPIES: usize = 3;
/// 直连握手最多等这么久，超时就改走中继。
const DIRECT_TIMEOUT: Duration = Duration::from_millis(2500);
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
    /// 先走中继（马上出画面），会话中在后台试直连，通了就切过去。
    RelayFirst,
}

impl PathPolicy {
    /// 这一次拨号只走中继（[`Self::RelayOnly`]，以及 [`Self::RelayFirst`] 的第一次）。
    #[must_use]
    pub const fn relay_only(self) -> bool {
        matches!(self, PathPolicy::RelayOnly | PathPolicy::RelayFirst)
    }

    /// 走了中继之后要不要在会话中试着升级成直连（用户没要求只走中继时都要：中继是成本，直连更快）。
    #[must_use]
    pub const fn upgrades(self) -> bool {
        matches!(self, PathPolicy::Auto | PathPolicy::RelayFirst)
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
    notice: Arc<std::sync::Mutex<Option<(u8, u32)>>>,
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
        let notice = Arc::new(std::sync::Mutex::new(None));
        let task = tokio::spawn(run_shim(socket, grant, endpoint, Arc::clone(&notice)));
        Ok(Self {
            local,
            task,
            notice,
        })
    }

    /// 中继发来的通知（类别、建议多少秒后重试），例如免费额度用完；没有时为 `None`。
    #[must_use]
    pub fn notice(&self) -> Option<(u8, u32)> {
        *self
            .notice
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

async fn run_shim(
    socket: UdpSocket,
    grant: RelayGrant,
    mut endpoint: Option<SocketAddr>,
    notice: Arc<std::sync::Mutex<Option<(u8, u32)>>>,
) {
    // 登记和保活的包里带着票据：中继凭它开通道（只认集群签发的票据）。
    let hello = wrap(&grant.session, grant.side, &grant.ticket);
    let mut keepalive = tokio::time::interval(SHIM_KEEPALIVE);
    let mut buf = vec![0u8; 2048];
    loop {
        tokio::select! {
            // 带票据的包只用来开通道、让中继记住本端的地址，中继不转发。
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
                    if let Some(n) = parse_notice(payload) {
                        crate::note!(warn, "中继通知：类别 {}，建议 {} 秒后重试", n.0, n.1);
                        *notice.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(n);
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
    /// 控制端的候选地址。
    pub locals: Vec<Candidate>,
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
    /// 服务器会哪些后来加的功能（老服务器是 [`Features::NONE`]）。只有服务器会的消息才能在这条登记上发：
    /// 发了它不认识的，它会关掉这条流，登记就断了。
    pub features: Features,
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
/// 把崩溃报告传给服务器，等它确认保存。
pub async fn upload_crash(
    node: &Node,
    server: &Server,
    report: &peerdesk_core::crash::Report,
) -> Result<(), NetError> {
    let link = node.connect(server.addr, server.pin.as_ref()).await?;
    let (mut send, mut recv) = link.open_stream().await?;
    write_kind(&mut send, StreamKind::Crash).await?;
    let result = crate::stream::write_crash(&mut send, &mut recv, report).await;
    link.close("done");
    result
}

pub async fn register(node: &Node, server: &Server) -> Result<Registration, NetError> {
    register_with(node, server, Vec::new()).await
}

/// 同 [`register`]，另外报告 `extra` 候选地址（例如路由器为本机映射出来的端口）。
pub async fn register_with(
    node: &Node,
    server: &Server,
    extra: Vec<Candidate>,
) -> Result<Registration, NetError> {
    let link = Arc::new(node.connect(server.addr, server.pin.as_ref()).await?);
    let (mut send, mut recv) = link.open_stream().await?;
    write_kind(&mut send, StreamKind::Signal).await?;
    let mut locals = extra;
    locals.extend(crate::candidates::local_candidates(
        node.local_addr()?,
        &crate::candidates::lan_addresses(),
    ));
    write_signal(&mut send, &SignalMsg::Register { locals }).await?;
    let SignalMsg::Registered {
        observed,
        code,
        probe,
        features,
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
                locals,
                punch,
                relay,
            } = msg
            {
                let incoming = Incoming {
                    peer,
                    addr,
                    locals,
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
        features,
        incoming,
        _link: link,
        task,
    })
}

/// 问服务器会哪些后来加的功能（不登记的一方用，例如控制端）。老服务器回答的是 [`Features::NONE`]。
///
/// 用的是体检消息（[`SignalMsg::Check`]），所有版本的服务器都认识。注意体检有一个副作用：服务器会忘掉这台设备
/// 上一次 NAT 探测的结果，所以别在“探测”和“发起连接”之间调用它；会话开始前单独问一次，结果可以一直用。
pub async fn server_features(node: &Node, server: &Server) -> Result<Features, NetError> {
    let link = node.connect(server.addr, server.pin.as_ref()).await?;
    let (mut send, mut recv) = link.open_stream().await?;
    write_kind(&mut send, StreamKind::Signal).await?;
    write_signal(&mut send, &SignalMsg::Check).await?;
    let reply = read_signal(&mut recv).await;
    link.close("done");
    if let SignalMsg::Checked { features, .. } = reply? {
        return Ok(features);
    }
    Err(NetError::Protocol("服务器的体检应答不对"))
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
    // 两端同时互相探测：向控制端的每个候选地址不停发 ping，让本机的 NAT 为它开门；
    // 控制端发来的 ping 由端口自动回 pong，控制端据此测每条路径并选路。
    let nonce = incoming.punch.nonce;
    let sub = node.subscribe(nonce);
    let mut remotes = incoming.locals.clone();
    remotes.push(Candidate {
        addr: incoming.addr,
        kind: PathKind::Public,
    });
    let (pinger, peer) = (Arc::clone(node), incoming.peer);
    tokio::spawn(async move {
        crate::ice::keep_pinging(&pinger, sub, nonce, remotes, peer, PINGING_TIME).await;
    });
    // 一端难打洞时，控制端先用很短的时间试快路径（局域网、IPv6、端口映射），都不通才多端口打洞；
    // 这边等同样的时间再开始，期间控制端已经连上来的话就不必了。
    let (node, addr) = (Arc::clone(node), incoming.addr);
    match plan {
        PunchPlan::Skip => {}
        // 普通打洞没通（例如本端路由器把外网先到的包记了一笔，给本机换了端口，对控制端来说
        // 本端就成了“难打洞”的一端）：和控制端同时退到多端口打洞。
        PunchPlan::Plain => {
            tokio::spawn(host_sockets(node, addr, peer, nonce, PLAIN_FALLBACK));
        }
        PunchPlan::Spray => {
            tokio::spawn(async move {
                tokio::time::sleep(HARD_HEAD_START).await;
                if !node.accepted_recently(&peer, PINGING_TIME) {
                    crate::punch::spray(&node, addr.ip(), addr, &nonce).await;
                }
            });
        }
        PunchPlan::Sockets => {
            tokio::spawn(host_sockets(node, addr, peer, nonce, HARD_HEAD_START));
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
    /// 具体是哪种路径（局域网、IPv6、打洞、中继……）。
    pub route: PathKind,
    /// 探测时测到的这条路径的往返时间（微秒）。
    pub rtt_us: Option<u64>,
    /// 探测结果的说明：每条路径通不通、多快。
    pub report: String,
    /// 服务器对中继的说明：会员通道、免费通道，或者为什么没有中继。
    pub relay_note: String,
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
/// 通过服务器建立连接时各阶段的耗时。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DialTiming {
    /// 连上服务器。
    pub server_connect: Duration,
    /// 向服务器查询对方地址。
    pub lookup: Duration,
    /// 并行探测所有候选路径。
    pub probe: Duration,
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
    dial_via(node, &[*server], lookup, policy, "").await
}

/// 连信令服务器最多等这么久，超时就换集群里的下一个节点。
const SERVER_TIMEOUT: Duration = Duration::from_secs(3);

/// 依次试 `servers`（集群里的节点），用第一个连得上的建立连接。某个节点连不上（崩溃、网络不通）
/// 就换下一个；连上了之后的结果（例如“设备不在线”）不再换节点重试。`account` 是会员令牌
/// （空表示免费用户），走中继时记在这个账户上。
pub async fn dial_via(
    node: &Node,
    servers: &[Server],
    lookup: &Lookup,
    policy: PathPolicy,
    account: &str,
) -> Result<Dialed, NetError> {
    let started = Instant::now();
    let mut last = NetError::Connect("没有配置服务器".into());
    for server in servers {
        match node
            .connect_within(server.addr, server.pin.as_ref(), SERVER_TIMEOUT)
            .await
        {
            Ok(signal) => {
                let timing = DialTiming {
                    server_connect: started.elapsed(),
                    ..DialTiming::default()
                };
                return dial_on(node, server, signal, timing, lookup, policy, account).await;
            }
            Err(e) => {
                crate::note!(warn, "服务器 {} 连不上，换下一个：{e}", server.addr);
                last = e;
            }
        }
    }
    Err(last)
}

/// 问一个服务器集群里有哪些节点（用于故障切换）。节点的指纹来自这个已经核对过身份的服务器。
pub async fn fetch_nodes(node: &Node, server: &Server) -> Result<Vec<Server>, NetError> {
    let link = node
        .connect_within(server.addr, server.pin.as_ref(), SERVER_TIMEOUT)
        .await?;
    let (mut send, mut recv) = link.open_stream().await?;
    write_kind(&mut send, StreamKind::Signal).await?;
    write_signal(&mut send, &SignalMsg::Nodes).await?;
    let reply = read_signal(&mut recv).await?;
    link.close("done");
    let SignalMsg::NodeList { nodes } = reply else {
        return Err(NetError::Protocol("服务器的节点列表应答不对"));
    };
    Ok(nodes
        .into_iter()
        .map(|(addr, pin, _)| Server {
            addr,
            pin: Some(pin),
        })
        .collect())
}

/// 在已经连上的信令连接上完成一次连接。
async fn dial_on(
    node: &Node,
    server: &Server,
    signal: Link,
    mut timing: DialTiming,
    lookup: &Lookup,
    policy: PathPolicy,
    account: &str,
) -> Result<Dialed, NetError> {
    let (mut send, mut recv) = signal.open_stream().await?;
    write_kind(&mut send, StreamKind::Signal).await?;
    let mine = crate::candidates::local_candidates(
        node.local_addr()?,
        &crate::candidates::lan_addresses(),
    );
    let ask = match lookup {
        Lookup::Device(target) => SignalMsg::Connect {
            target: *target,
            locals: mine,
            account: account.to_owned(),
        },
        Lookup::Code(code) => SignalMsg::ConnectCode {
            code: *code,
            locals: mine,
            account: account.to_owned(),
        },
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
    let (device, addr, locals, you, punch_info, relay, relay_note) = match reply {
        SignalMsg::Peer {
            device,
            addr,
            locals,
            you,
            punch,
            relay,
            note,
        } => (device, addr, locals, you, punch, relay, note),
        SignalMsg::NotFound => {
            return Err(NetError::Connect(match lookup {
                Lookup::Device(_) => "设备不在线".into(),
                Lookup::Code(code) => format!("设备码 {} 不存在或设备不在线", format_code(*code)),
            }));
        }
        SignalMsg::Register { .. }
        | SignalMsg::Probe { .. }
        | SignalMsg::Check
        | SignalMsg::Nodes
        | SignalMsg::NodeList { .. }
        | SignalMsg::Checked { .. }
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
        "服务器 {}：连上 {:?}，查到对方 {addr}（{:?}），本机公网地址 {you}，打洞方式 {plan:?}（本机 NAT {:?}，对方 {:?}），中继{}",
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
    let nonce = punch_info.nonce;
    let mut sub = node.subscribe(nonce);

    // 候选：对方报告的局域网 / IPv6 / 端口映射地址，服务器看到的公网地址，还有中继。
    let mut remotes: Vec<Candidate> = Vec::new();
    if !policy.relay_only() {
        remotes.extend(locals.iter().copied());
        remotes.push(Candidate {
            addr,
            kind: PathKind::Public,
        });
    }
    if policy.relay_only() && relay.is_none() {
        return Err(NetError::Connect("服务器没有提供中继".into()));
    }
    // 拿到对方地址立刻先发一轮 ping（不等中继转发器准备好）：本端路由器先为对方开门，
    // 对方的包到达时就不会被本端路由器当成陌生来客（见 `ice::HOST_PING_DELAY`）。
    crate::ice::ping_all(node, nonce, u32::MAX, &remotes);
    let shim = match (policy, relay) {
        (PathPolicy::DirectOnly, _) | (_, None) => None,
        (PathPolicy::Auto | PathPolicy::RelayOnly | PathPolicy::RelayFirst, Some(grant)) => {
            Some(Shim::start(grant, None).await?)
        }
    };
    if let Some(shim) = &shim {
        // 中继也作为一条路径参加探测：ping 经两端的转发器过去，能和直连比快慢。
        remotes.push(Candidate {
            addr: shim.local_addr(),
            kind: PathKind::Relay,
        });
    }
    // 有一端难打洞（包括两端都难）时，公网地址基本不会通，只等快路径。
    let base = if plan == PunchPlan::Plain {
        Limits::NORMAL
    } else {
        Limits::HARD
    };
    let limits = Limits {
        nat_penalty_us: if prefer_ipv6() { NAT_PENALTY_US } else { 0 },
        ..base
    };
    let probing = Instant::now();
    let direct_allowed = !policy.relay_only();
    let race = crate::ice::race(node, &mut sub, nonce, remotes, limits, direct_allowed).await;
    timing.probe = probing.elapsed();
    let report = crate::ice::describe(&race);
    crate::note!(info, "{report}");
    let any_direct = race.measured.iter().any(|m| m.candidate.kind.direct());

    let mut direct_error = None;
    if let Some(m) = race.picked.filter(|m| m.candidate.kind.direct()) {
        let tried = Instant::now();
        let link = node
            .connect_within(m.candidate.addr, Some(target), DIRECT_TIMEOUT)
            .await;
        timing.direct = tried.elapsed();
        match link {
            Ok(link) => {
                return Ok(Dialed {
                    link,
                    path: Path::Direct,
                    route: m.candidate.kind,
                    rtt_us: Some(m.rtt_us),
                    report,
                    relay_note: relay_note.clone(),
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
            Err(e) => direct_error = Some(e),
        }
    } else if plan != PunchPlan::Skip && !any_direct && !policy.relay_only() {
        // 快路径都不通：多端口打洞（生日攻击）。普通打洞没通时，多半是对方的路由器给对方换了
        // 端口（对方成了难打洞的一端），所以本端撒包、对方开很多端口。
        let tried = Instant::now();
        // 本端难时本端开很多端口；否则（对方难，或者普通打洞没通）本端撒包。
        let found = birthday(node, plan == PunchPlan::Sockets, addr, nonce, target).await;
        timing.direct = tried.elapsed();
        match found {
            Ok(link) => {
                crate::note!(
                    info,
                    "多端口打洞直连 {} 成功，用时 {:?}",
                    link.remote_addr(),
                    timing.direct
                );
                return Ok(Dialed {
                    link,
                    path: Path::Direct,
                    route: PathKind::PeerReflexive,
                    rtt_us: None,
                    report,
                    relay_note: relay_note.clone(),
                    device,
                    plan,
                    timing,
                    direct_error: None,
                    relay_session: None,
                    shim: None,
                });
            }
            Err(e @ NetError::PinMismatch { .. }) => return Err(e),
            Err(e) => direct_error = Some(e),
        }
    } else if !any_direct && !policy.relay_only() {
        direct_error = Some(NetError::Connect(
            "两端的 NAT 都是对称型（换目的地就换端口），打洞几乎不可能成功，直接走中继".into(),
        ));
    }

    let (Some(shim), Some(grant)) = (shim, relay) else {
        return Err(direct_error.unwrap_or_else(|| NetError::Connect("服务器没有提供中继".into())));
    };
    let tried = Instant::now();
    let link = node.connect(shim.local_addr(), Some(target)).await?;
    timing.relay = tried.elapsed();
    let why = direct_error
        .as_ref()
        .map_or_else(|| "中继更快".to_owned(), ToString::to_string);
    crate::note!(
        info,
        "经中继 {} 连上，用时 {:?}；不走直连的原因：{why}",
        grant.addr,
        timing.relay
    );
    let rtt_us = race
        .measured
        .iter()
        .find(|m| m.candidate.kind == PathKind::Relay)
        .map(|m| m.rtt_us);
    Ok(Dialed {
        link,
        path: Path::Relay,
        route: PathKind::Relay,
        rtt_us,
        report,
        relay_note: relay_note.clone(),
        device,
        plan,
        timing,
        direct_error: Some(why),
        relay_session: Some(grant.session),
        shim: Some(shim),
    })
}

/// 被控端的多端口打洞：等 `after`（控制端先试快路径），控制端还没连上来就开很多端口找通路，
/// 撞上后主动连控制端，这条连接从 [`Node::accept`] 出来。
async fn host_sockets(
    node: Arc<Node>,
    addr: SocketAddr,
    peer: Fingerprint,
    nonce: [u8; 16],
    after: Duration,
) {
    tokio::time::sleep(after).await;
    if node.accepted_recently(&peer, PINGING_TIME) {
        return;
    }
    let found = crate::punch::sockets(addr, nonce, BIRTHDAY_TIMEOUT).await;
    // 撞上之前控制端可能已经从别的路连上了：不要再建第二条连接。
    let socket = found.filter(|_| !node.accepted_recently(&peer, PINGING_TIME));
    let linked = match socket {
        Some(socket) => {
            node.connect_via(socket, addr, Some(&peer), DIRECT_TIMEOUT)
                .await
        }
        None => Err(NetError::Timeout("多端口打洞没有撞上或已经不需要")),
    };
    match linked {
        Ok(link) => {
            crate::note!(info, "被控：多端口打洞成功，主动连上 {addr}");
            node.offer(link).await;
        }
        Err(e) => crate::note!(info, "被控：多端口打洞没有用上 {addr}：{e}"),
    }
}

/// 多端口打洞：本端难时开很多端口；对方难时向它撒探测包，等它从撞上的端口连过来。
async fn birthday(
    node: &Node,
    open_sockets: bool,
    addr: SocketAddr,
    nonce: [u8; 16],
    target: &Fingerprint,
) -> Result<Link, NetError> {
    if open_sockets {
        let found = crate::punch::sockets(addr, nonce, BIRTHDAY_TIMEOUT).await;
        let socket = found.ok_or(NetError::Timeout("多端口打洞没有撞上"))?;
        return node
            .connect_via(socket, addr, Some(target), DIRECT_TIMEOUT)
            .await;
    }
    let spraying = crate::punch::spray(node, addr.ip(), addr, &nonce);
    let accepting = accept_from(node, target);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn ipv6_preference_defaults_on() {
        // 其他测试并行运行，这里只写默认值，不改变它们看到的行为。
        assert!(prefer_ipv6());
        set_prefer_ipv6(true);
        assert!(prefer_ipv6());
    }

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
            ticket: [5; peerdesk_core::relay::TICKET_LEN],
        };
        let shim = Shim::start(grant, None).await.unwrap();
        let mut buf = [0u8; 4096];
        // 第一个包是登记用的，带着票据
        let (len, shim_addr) = relay.recv_from(&mut buf).await.unwrap();
        assert_eq!(len, RELAY_HEADER + peerdesk_core::relay::TICKET_LEN);
        assert_eq!(
            &buf[RELAY_HEADER..len],
            &[5; peerdesk_core::relay::TICKET_LEN]
        );
        // 中继的通知（免费额度用完）：记下，不交给端点
        assert_eq!(shim.notice(), None);
        relay
            .send_to(
                &wrap(&[3; 16], 1, &peerdesk_core::relay::notice(1, 300)),
                shim_addr,
            )
            .await
            .unwrap();
        for _ in 0..100 {
            if shim.notice().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(shim.notice(), Some((1, 300)));
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
