//! QUIC 端点与连接。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use peerdesk_core::crypto::Policy;
use peerdesk_core::proto::{
    decode_datagram, encode_datagram, Channel, Header, HEADER_LEN, MAX_DATAGRAM,
};
use peerdesk_core::Time;
use quinn::{RecvStream, SendStream, VarInt};

use crate::identity::{fingerprint_of, to_hex};
use crate::{Fingerprint, Identity, NetError};

/// 握手的时间上限。
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(8);

/// 一个 QUIC 端点。同一个端点既能发起连接也能接受连接，所以打洞后任一端都可以拨号。
#[derive(Debug)]
pub struct Node {
    endpoint: quinn::Endpoint,
    fingerprint: Fingerprint,
}

impl Node {
    /// 绑定本地地址。必须在 tokio 运行时内调用。
    pub fn bind(addr: SocketAddr, identity: &Identity, policy: &Policy) -> Result<Self, NetError> {
        let (server, client) = crate::tls::configs(identity, policy)?;
        let mut endpoint = quinn::Endpoint::server(server, addr)
            .map_err(|e| NetError::Io(format!("绑定 {addr} 失败：{e}")))?;
        endpoint.set_default_client_config(client);
        Ok(Self {
            endpoint,
            fingerprint: identity.fingerprint(),
        })
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

    /// 连接对端。给了 `pin` 时，对端指纹必须与之相同，否则立刻断开并报错。
    pub async fn connect(
        &self,
        addr: SocketAddr,
        pin: Option<&Fingerprint>,
    ) -> Result<Link, NetError> {
        let connecting = self
            .endpoint
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
                return Err(NetError::PinMismatch {
                    expected: to_hex(expected),
                    actual: to_hex(&link.peer),
                });
            }
        }
        Ok(link)
    }

    /// 等待并接受一个连接。端点关闭后返回 `None`；握手失败的连接返回 `Some(Err)`。
    pub async fn accept(&self) -> Option<Result<Link, NetError>> {
        let incoming = self.endpoint.accept().await?;
        let result = match tokio::time::timeout(HANDSHAKE_TIMEOUT, incoming).await {
            Err(_) => Err(NetError::Timeout("握手")),
            Ok(Err(e)) => Err(NetError::Connect(e.to_string())),
            Ok(Ok(conn)) => Link::new(conn),
        };
        Some(result)
    }

    /// 关闭端点和它的所有连接。
    pub fn close(&self) {
        self.endpoint.close(VarInt::from_u32(0), b"closed");
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
    seq: [AtomicU32; 6],
    started: Instant,
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
        self.conn.remote_address()
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
        self.conn
            .send_datagram(Bytes::from(datagram))
            .map_err(|e| match e {
                quinn::SendDatagramError::ConnectionLost(why) => NetError::Closed(why.to_string()),
                quinn::SendDatagramError::UnsupportedByPeer
                | quinn::SendDatagramError::Disabled
                | quinn::SendDatagramError::TooLarge => NetError::Protocol("数据报无法发送"),
            })
    }

    /// 接收一个数据报。连接关闭时返回 [`NetError::Closed`]。
    pub async fn recv(&self) -> Result<(Header, Vec<u8>), NetError> {
        let bytes = self
            .conn
            .read_datagram()
            .await
            .map_err(|e| NetError::Closed(e.to_string()))?;
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

    /// 等待连接关闭，返回原因。
    pub async fn closed(&self) -> String {
        self.conn.closed().await.to_string()
    }
}
