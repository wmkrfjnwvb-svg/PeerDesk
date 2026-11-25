//! QUIC 端点与连接。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use bytes::Bytes;
use peerdesk_core::crypto::Policy;
use peerdesk_core::proto::{
    decode_datagram, encode_datagram, Channel, Header, HEADER_LEN, MAX_DATAGRAM,
};
use peerdesk_core::Time;
use quinn::{RecvStream, SendStream, VarInt};
use tokio::sync::mpsc;

use crate::identity::{fingerprint_of, to_hex};
use crate::sidechannel::{canonical, Heard, Listeners, SideChannel};
use crate::{Fingerprint, Identity, NetError};

/// 握手的时间上限。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);

/// 在 `socket` 上开一个 QUIC 端点，接受到的连接在后台完成握手后放进 `tx`。
/// 一个端点和它的兄弟端点共享的东西：接受队列、探测任务登记、最近接受过的对端。
#[derive(Debug, Clone)]
struct Shared {
    tx: mpsc::Sender<Result<Link, NetError>>,
    listeners: Listeners,
    recent: Arc<Mutex<Vec<(Fingerprint, Instant)>>>,
}

fn endpoint_on(
    socket: std::net::UdpSocket,
    server: &quinn::ServerConfig,
    client: &quinn::ClientConfig,
    shared: &Shared,
) -> std::io::Result<quinn::Endpoint> {
    let runtime =
        quinn::default_runtime().ok_or_else(|| std::io::Error::other("没有可用的异步运行时"))?;
    let wrapped = runtime.wrap_udp_socket(socket)?;
    let side = Arc::new(SideChannel::new(wrapped, Arc::clone(&shared.listeners)));
    let mut endpoint = quinn::Endpoint::new_with_abstract_socket(
        quinn::EndpointConfig::default(),
        Some(server.clone()),
        side,
        runtime,
    )?;
    endpoint.set_default_client_config(client.clone());
    // 握手放在后台任务里做：调用方随时可以放弃等待，不会打断正在进行的握手。
    let acceptor = endpoint.clone();
    let shared = shared.clone();
    tokio::spawn(async move {
        while let Some(incoming) = acceptor.accept().await {
            let shared = shared.clone();
            tokio::spawn(async move {
                let result = match tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming).await {
                    Err(_) => Err(NetError::Timeout("握手")),
                    Ok(Err(e)) => Err(NetError::Connect(e.to_string())),
                    Ok(Ok(conn)) => Link::new(conn),
                };
                if let Ok(link) = &result {
                    note_accepted(&shared.recent, link.peer);
                }
                let _ = shared.tx.send(result).await;
            });
        }
    });
    Ok(endpoint)
}

/// 从 `endpoint` 连接 `addr`。给了 `pin` 时，对端指纹必须与之相同，否则立刻断开并报错。
async fn connect_on(
    endpoint: &quinn::Endpoint,
    addr: SocketAddr,
    pin: Option<&Fingerprint>,
) -> Result<Link, NetError> {
    let connecting = endpoint
        .connect(addr, "peerdesk")
        .map_err(|e| NetError::Connect(e.to_string()))?;
    let conn = tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting)
        .await
        .map_err(|_| NetError::Timeout("握手"))?
        .map_err(|e| NetError::Connect(e.to_string()))?;
    let link = Link::new(conn)?;
    if let Some(expected) = pin {
        if link.peer != *expected {
            link.close("pin mismatch");
            // 等关闭通知发出去再返回：否则调用方马上退出的话，对端要等到超时才知道连接没了。
            let _ = tokio::time::timeout(Duration::from_millis(300), link.closed_locally()).await;
            return Err(NetError::PinMismatch {
                expected: to_hex(expected),
                actual: to_hex(&link.peer),
            });
        }
    }
    Ok(link)
}

/// 记下刚接受了 `peer` 的连接（只留最近的几个）。
fn note_accepted(recent: &Mutex<Vec<(Fingerprint, Instant)>>, peer: Fingerprint) {
    let mut recent = recent.lock().unwrap_or_else(PoisonError::into_inner);
    recent.retain(|(p, at)| *p != peer && at.elapsed() < RECENT_ACCEPT);
    recent.push((peer, Instant::now()));
}

/// 最近接受过的连接记多久。
const RECENT_ACCEPT: Duration = Duration::from_secs(30);

/// 一个暗号的探测任务登记；丢弃时注销。
#[derive(Debug)]
pub struct Subscription {
    nonce: [u8; 16],
    listeners: Listeners,
    rx: mpsc::UnboundedReceiver<Heard>,
}

impl Subscription {
    /// 下一个收到的探测包。
    pub async fn recv(&mut self) -> Option<Heard> {
        self.rx.recv().await
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.nonce);
    }
}

/// 打开一个 UDP 端口。绑定 IPv6 的“所有地址”时打开双栈（同时收发 IPv4），
/// 这样一个端口就能同时走 IPv4 和 IPv6 两种直连。
fn open_socket(addr: SocketAddr) -> std::io::Result<std::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))?;
    if addr.is_ipv6() && addr.ip().is_unspecified() {
        // 系统不允许双栈时只用 IPv6。
        let _ = socket.set_only_v6(false);
    }
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// 一个 QUIC 端点。同一个端点既能发起连接也能接受连接，所以打洞后任一端都可以拨号。
#[derive(Debug)]
pub struct Node {
    endpoint: quinn::Endpoint,
    fingerprint: Fingerprint,
    /// 本端的 TLS 配置，开新端口（打洞用）时沿用。
    server_config: quinn::ServerConfig,
    client_config: quinn::ClientConfig,
    /// 接受到的连接放进这里；打洞时开的其他端口接受到的连接也放进来。只留弱引用：
    /// 端点全部关闭后队列随之关闭，[`Node::accept`] 才会返回 `None`。
    accepted_tx: mpsc::WeakSender<Result<Link, NetError>>,
    /// 打洞时开的其他端口上的端点，和本端点一起关闭。
    siblings: Mutex<Vec<quinn::Endpoint>>,
    /// 与 QUIC 端点共用同一个 UDP 端口的另一份句柄，只用来发打洞包。
    punch: std::net::UdpSocket,
    /// 后台任务完成握手后把连接放进这个队列。
    accepted: tokio::sync::Mutex<mpsc::Receiver<Result<Link, NetError>>>,
    /// 探测任务登记（端口旁路按暗号把探测包交给它们）。
    listeners: Listeners,
    /// 最近接受过连接的对端。
    recent: Arc<Mutex<Vec<(Fingerprint, Instant)>>>,
    /// 端口是不是 IPv6（双栈）的：往 IPv4 地址发包时要换成映射地址。
    v6: bool,
    /// 时钟起点（探测包里的时间）。
    epoch: Instant,
}

impl Node {
    /// 绑定本地地址。必须在 tokio 运行时内调用。
    pub fn bind(addr: SocketAddr, identity: &Identity, policy: &Policy) -> Result<Self, NetError> {
        let bind_err = |e: std::io::Error| NetError::Io(format!("绑定 {addr} 失败：{e}"));
        let socket = open_socket(addr).map_err(bind_err)?;
        Self::on_socket(socket, identity, policy)
    }

    /// 依次试着绑定 `addrs`，用第一个成功的；都失败时返回最后一个错误。
    /// 用来“优先双栈端口，系统不支持 IPv6 时退回 IPv4”。
    pub fn bind_first(
        addrs: &[SocketAddr],
        identity: &Identity,
        policy: &Policy,
    ) -> Result<Self, NetError> {
        let mut last = NetError::Io("没有可绑定的地址".into());
        for addr in addrs {
            match open_socket(*addr) {
                Ok(socket) => return Self::on_socket(socket, identity, policy),
                // 端口被占用时不换地址族重试：Windows 上双栈的 [::]:p 和 0.0.0.0:p 可以同时绑定，
                // 换成 IPv4 会“成功”，结果两个程序开在同一个端口上。只有系统不支持 IPv6 时才退回 IPv4。
                Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                    return Err(NetError::Io(format!("绑定 {addr} 失败：{e}")));
                }
                Err(e) => last = NetError::Io(format!("绑定 {addr} 失败：{e}")),
            }
        }
        Err(last)
    }

    /// 在一个已经打开的 UDP 端口上开端点（例如打洞时撞上的那个端口）。必须在 tokio 运行时内调用。
    pub fn on_socket(
        socket: std::net::UdpSocket,
        identity: &Identity,
        policy: &Policy,
    ) -> Result<Self, NetError> {
        let (server, client) = crate::tls::configs(identity, policy)?;
        let bind_err = |e: std::io::Error| NetError::Io(format!("打开端点失败：{e}"));
        socket.set_nonblocking(true).map_err(bind_err)?;
        let punch = socket.try_clone().map_err(bind_err)?;
        let v6 = socket.local_addr().map_err(bind_err)?.is_ipv6();
        let (tx, rx) = mpsc::channel(64);
        let listeners = Listeners::default();
        let recent = Arc::new(Mutex::new(Vec::new()));
        let shared = Shared {
            tx,
            listeners: Arc::clone(&listeners),
            recent: Arc::clone(&recent),
        };
        let endpoint = endpoint_on(socket, &server, &client, &shared).map_err(bind_err)?;
        Ok(Self {
            endpoint,
            fingerprint: identity.fingerprint(),
            server_config: server,
            client_config: client,
            accepted_tx: shared.tx.downgrade(),
            siblings: Mutex::new(Vec::new()),
            punch,
            accepted: tokio::sync::Mutex::new(rx),
            listeners,
            recent,
            v6,
            epoch: Instant::now(),
        })
    }

    /// 在另一个 UDP 端口上开一个端点（同一个身份），从它连接 `addr`。打洞时用：
    /// 在许多端口里，最先收到对方探测包的那个端口才是通的。这个端点接受到的连接也进
    /// [`Node::accept`]，它和本端点一起关闭。
    pub async fn connect_via(
        &self,
        socket: std::net::UdpSocket,
        addr: SocketAddr,
        pin: Option<&Fingerprint>,
        timeout: Duration,
    ) -> Result<Link, NetError> {
        let io = |e: std::io::Error| NetError::Io(format!("打洞端口：{e}"));
        let tx = self
            .accepted_tx
            .upgrade()
            .ok_or_else(|| NetError::Closed("端点已关闭".into()))?;
        let shared = Shared {
            tx,
            listeners: Arc::clone(&self.listeners),
            recent: Arc::clone(&self.recent),
        };
        let endpoint =
            endpoint_on(socket, &self.server_config, &self.client_config, &shared).map_err(io)?;
        {
            let mut siblings = self.siblings.lock().unwrap_or_else(PoisonError::into_inner);
            siblings.push(endpoint.clone());
        }
        tokio::time::timeout(timeout, connect_on(&endpoint, addr, pin))
            .await
            .map_err(|_| NetError::Timeout("直连"))?
    }

    /// 把一条本端主动建立的连接当作接受到的连接交给 [`Node::accept`]（打洞时被控端主动连控制端）。
    pub async fn offer(&self, link: Link) {
        if let Some(tx) = self.accepted_tx.upgrade() {
            let _ = tx.send(Ok(link)).await;
        }
    }

    /// 从本端点的端口向 `addr` 发一个任意内容的 UDP 包（打洞、探测用）。发不出去不是错误。
    pub fn send_raw(&self, addr: SocketAddr, payload: &[u8]) {
        let _ = self.punch.send_to(payload, self.fit(addr));
    }

    /// 双栈端口要用映射地址发往 IPv4。
    fn fit(&self, addr: SocketAddr) -> SocketAddr {
        match addr {
            SocketAddr::V4(v4) if self.v6 => {
                SocketAddr::new(std::net::IpAddr::V6(v4.ip().to_ipv6_mapped()), v4.port())
            }
            SocketAddr::V4(_) | SocketAddr::V6(_) => addr,
        }
    }

    /// 登记一个探测任务：之后收到带这个暗号的 ping/pong 都交给它（ping 由端口自动回 pong）。
    #[must_use]
    pub fn subscribe(&self, nonce: [u8; 16]) -> Subscription {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(nonce, tx);
        Subscription {
            nonce,
            listeners: Arc::clone(&self.listeners),
            rx,
        }
    }

    /// 探测包用的时钟：从端点创建开始的微秒数。
    #[must_use]
    pub fn now_us(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    /// 某个时刻换算成探测时钟。
    #[must_use]
    pub fn at_us(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.epoch).as_micros()).unwrap_or(u64::MAX)
    }

    /// 最近 `within` 内是否接受过 `peer` 的连接。
    #[must_use]
    pub fn accepted_recently(&self, peer: &Fingerprint, within: Duration) -> bool {
        self.recent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|(p, at)| p == peer && at.elapsed() < within)
    }

    /// 实际绑定的地址。
    pub fn local_addr(&self) -> Result<SocketAddr, NetError> {
        self.endpoint
            .local_addr()
            .map_err(|e| NetError::Io(format!("读取本地地址失败：{e}")))
    }

    /// 本端指纹。
    #[must_use]
    pub const fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }

    /// 同 [`Node::connect`]，但握手最多等 `timeout`。用于尝试一条可能不通的直连路径。
    pub async fn connect_within(
        &self,
        addr: SocketAddr,
        pin: Option<&Fingerprint>,
        timeout: Duration,
    ) -> Result<Link, NetError> {
        tokio::time::timeout(timeout, self.connect(addr, pin))
            .await
            .map_err(|_| NetError::Timeout("直连"))?
    }

    /// 连接对端。给了 `pin` 时，对端指纹必须与之相同，否则立刻断开并报错。
    pub async fn connect(
        &self,
        addr: SocketAddr,
        pin: Option<&Fingerprint>,
    ) -> Result<Link, NetError> {
        connect_on(&self.endpoint, addr, pin).await
    }

    /// 等待并接受一个连接。端点关闭后返回 `None`；握手失败的连接返回 `Some(Err)`。
    /// 可以随时取消（例如放在 `select!` 里），不会丢连接。
    pub async fn accept(&self) -> Option<Result<Link, NetError>> {
        self.accepted.lock().await.recv().await
    }

    /// 关闭端点和它的所有连接。
    pub fn close(&self) {
        self.endpoint.close(VarInt::from_u32(0), b"closed");
        let siblings = self.siblings.lock().unwrap_or_else(PoisonError::into_inner);
        for sibling in siblings.iter() {
            sibling.close(VarInt::from_u32(0), b"closed");
        }
    }

    /// 等所有连接把关闭通知发完。
    pub async fn wait_idle(&self) {
        self.endpoint.wait_idle().await;
    }
}

/// 一条已建立的连接。
#[derive(Debug)]
pub struct Link {
    conn: quinn::Connection,
    peer: Fingerprint,
    /// 每个通道各自的序号：接收端按通道统计丢包，不同通道的包不能共用一个序号。
    seq: [AtomicU32; 7],
    started: Instant,
    /// 应用交给 QUIC 发送的数据报数（含被 QUIC 发送缓冲挤掉的）。
    app_sent: AtomicU64,
    /// 超过路径 MTU、没发出去的数据报数。
    app_too_large: AtomicU64,
    /// 应用从 QUIC 读到的数据报数。
    app_read: AtomicU64,
}

/// 一条连接的 QUIC 层统计（累计值），诊断日志用：把“网络丢的”和“我们自己这一层丢的”分开。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuicStats {
    /// 应用交给 QUIC 发送的数据报数。
    pub app_sent: u64,
    /// 超过路径 MTU 没发出去的数据报数。
    pub app_too_large: u64,
    /// 应用读到的数据报数。
    pub app_read: u64,
    /// QUIC 实际发出的 DATAGRAM 帧数（比 `app_sent` 少：发送缓冲满时最旧的被挤掉）。
    pub datagram_frames_sent: u64,
    /// QUIC 收到的 DATAGRAM 帧数（比 `app_read` 多：应用读得慢，接收缓冲满时被丢）。
    pub datagram_frames_received: u64,
    /// UDP 层发出、收到的包数。
    pub udp_sent: u64,
    /// UDP 层收到的包数。
    pub udp_received: u64,
    /// QUIC 判定在网络上丢了的包数和字节数。
    pub lost_packets: u64,
    /// QUIC 判定丢了的字节数。
    pub lost_bytes: u64,
    /// QUIC 拥塞控制的拥塞事件次数。
    pub congestion_events: u64,
    /// QUIC 拥塞窗口（字节）。
    pub cwnd: u64,
    /// 当前路径 MTU。
    pub mtu: u16,
}

impl Link {
    fn new(conn: quinn::Connection) -> Result<Self, NetError> {
        let peer = conn
            .peer_identity()
            .and_then(|id| {
                id.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>()
                    .ok()
            })
            .and_then(|certs| certs.first().map(|c| fingerprint_of(c.as_ref())))
            .ok_or(NetError::Protocol("对端没有出示证书"))?;
        Ok(Self {
            conn,
            peer,
            seq: Default::default(),
            started: Instant::now(),
            app_sent: AtomicU64::new(0),
            app_too_large: AtomicU64::new(0),
            app_read: AtomicU64::new(0),
        })
    }

    /// 对端指纹。握手已经证明对端持有对应的私钥。
    #[must_use]
    pub const fn peer_fingerprint(&self) -> &Fingerprint {
        &self.peer
    }

    /// 对端地址。
    #[must_use]
    pub fn remote_addr(&self) -> SocketAddr {
        canonical(self.conn.remote_address())
    }

    /// 通道绑定：由这条连接的 TLS 密钥导出的一段字节，两端相同，别的连接无法得到。
    /// 密码授权用它保证“证明”不能被中间人转用到另一条连接上。
    pub fn channel_binding(&self) -> Result<Vec<u8>, NetError> {
        let mut out = vec![0u8; 32];
        self.conn
            .export_keying_material(&mut out, b"peerdesk channel binding", b"")
            .map_err(|_| NetError::Protocol("无法导出通道绑定"))?;
        Ok(out)
    }

    /// 这条连接在网络上实际发出和收到的字节数（含握手、保活、确认等一切开销）。
    #[must_use]
    pub fn wire_bytes(&self) -> (u64, u64) {
        let stats = self.conn.stats();
        (stats.udp_tx.bytes, stats.udp_rx.bytes)
    }

    /// QUIC 层统计（累计值，见 [`QuicStats`]）。
    #[must_use]
    pub fn quic_stats(&self) -> QuicStats {
        let s = self.conn.stats();
        QuicStats {
            app_sent: self.app_sent.load(Ordering::Relaxed),
            app_too_large: self.app_too_large.load(Ordering::Relaxed),
            app_read: self.app_read.load(Ordering::Relaxed),
            datagram_frames_sent: s.frame_tx.datagram,
            datagram_frames_received: s.frame_rx.datagram,
            udp_sent: s.udp_tx.datagrams,
            udp_received: s.udp_rx.datagrams,
            lost_packets: s.path.lost_packets,
            lost_bytes: s.path.lost_bytes,
            congestion_events: s.path.congestion_events,
            cwnd: s.path.cwnd,
            mtu: s.path.current_mtu,
        }
    }

    /// 当前估计的往返延迟。
    #[must_use]
    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    /// 连接建立以来的时间，作为数据报的发送时间戳。
    #[must_use]
    pub fn now(&self) -> Time {
        Time(u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX))
    }

    /// 进程级的单调时间（微秒），用于跨连接的计时，例如认证失败的锁定。
    #[must_use]
    pub fn now_wall(&self) -> Time {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let start = START.get_or_init(Instant::now);
        Time(u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX))
    }

    /// 一个数据报里除包头外还能放多少字节。随路径 MTU 探测的结果变化。
    #[must_use]
    pub fn max_payload(&self) -> usize {
        self.conn
            .max_datagram_size()
            .unwrap_or(0)
            .min(MAX_DATAGRAM)
            .saturating_sub(HEADER_LEN)
    }

    /// 在某个通道上发送一个数据报。不等待：发送缓冲满时最旧的数据报被丢弃。
    pub fn send(&self, channel: Channel, payload: &[u8]) -> Result<(), NetError> {
        self.send_flags(channel, 0, payload)
    }

    /// 同 [`Link::send`]，并设置包头里的标志位。
    pub fn send_flags(&self, channel: Channel, flags: u8, payload: &[u8]) -> Result<(), NetError> {
        let seq = self
            .seq
            .get(usize::from(channel.id()))
            .map_or(0, |counter| counter.fetch_add(1, Ordering::Relaxed));
        let header = Header {
            channel,
            flags,
            conn_id: 0,
            seq,
            send_time: self.now(),
        };
        self.send_raw(encode_datagram(&header, payload)?)
    }

    /// 原样发送一个数据报，不加包头。正常代码用 [`Link::send`]；
    /// 这个入口用来测试对端面对畸形数据报时的健壮性。
    pub fn send_raw(&self, datagram: Vec<u8>) -> Result<(), NetError> {
        self.app_sent.fetch_add(1, Ordering::Relaxed);
        self.conn
            .send_datagram(Bytes::from(datagram))
            .or_else(|e| match e {
                quinn::SendDatagramError::ConnectionLost(why) => {
                    Err(NetError::Closed(why.to_string()))
                }
                // 路径 MTU 变小了（丢包严重时 QUIC 的黑洞检测会调小）：这个包当作丢了，由重传和下一帧补；
                // 发送方每帧按新的大小切片。原来当成致命错误，窄带宽上会话直接断开。
                quinn::SendDatagramError::TooLarge => {
                    self.app_too_large.fetch_add(1, Ordering::Relaxed);
                    log::debug!("数据报超过当前路径 MTU，丢弃这一片");
                    Ok(())
                }
                quinn::SendDatagramError::UnsupportedByPeer
                | quinn::SendDatagramError::Disabled => Err(NetError::Protocol("对方不支持数据报")),
            })
    }

    /// 接收一个数据报。连接关闭时返回 [`NetError::Closed`]。
    pub async fn recv(&self) -> Result<(Header, Vec<u8>), NetError> {
        let bytes = self
            .conn
            .read_datagram()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))?;
        self.app_read.fetch_add(1, Ordering::Relaxed);
        let (header, payload) = decode_datagram(&bytes)?;
        Ok((header, payload.to_vec()))
    }

    /// 打开一条可靠的双向流。
    pub async fn open_stream(&self) -> Result<(SendStream, RecvStream), NetError> {
        self.conn
            .open_bi()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))
    }

    /// 接受对端打开的一条可靠的双向流。
    pub async fn accept_stream(&self) -> Result<(SendStream, RecvStream), NetError> {
        self.conn
            .accept_bi()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))
    }

    /// 关闭连接，`reason` 会送达对端。
    pub fn close(&self, reason: &str) {
        self.conn.close(VarInt::from_u32(0), reason.as_bytes());
    }

    /// 本端关闭连接后，等关闭通知送出。
    async fn closed_locally(&self) {
        // 连接关闭后，端点还要把关闭帧发出去；给它一点时间。
        self.conn.closed().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    /// 等待连接关闭，返回原因。
    pub async fn closed(&self) -> String {
        self.conn.closed().await.to_string()
    }

    /// 到目前为止从对端收到的 UDP 包数。
    #[must_use]
    pub fn received_packets(&self) -> u64 {
        self.conn.stats().udp_rx.datagrams
    }

    /// 对端主动关闭连接时给的原因（例如被控端屏幕暂时打不开、拒绝了这次连接）；没有原因时为 `None`。
    #[must_use]
    pub fn refused_reason(&self) -> Option<String> {
        use quinn::ConnectionError as E;
        match self.conn.close_reason()? {
            E::ApplicationClosed(close) if !close.reason.is_empty() => {
                Some(String::from_utf8_lossy(&close.reason).into_owned())
            }
            E::ApplicationClosed(_)
            | E::VersionMismatch
            | E::TransportError(_)
            | E::ConnectionClosed(_)
            | E::Reset
            | E::TimedOut
            | E::LocallyClosed
            | E::CidsExhausted => None,
        }
    }

    /// 连接是不是因为网络原因断了（超时、被重置、传输层出错），是的话返回原因。
    /// 对端或本端正常关闭、连接还开着时为 `None`。
    #[must_use]
    pub fn lost_reason(&self) -> Option<String> {
        use quinn::ConnectionError as E;
        match self.conn.close_reason()? {
            e @ (E::TimedOut | E::Reset | E::TransportError(_) | E::ConnectionClosed(_)) => {
                Some(e.to_string())
            }
            E::ApplicationClosed(_) | E::LocallyClosed | E::VersionMismatch | E::CidsExhausted => {
                None
            }
        }
    }
}
