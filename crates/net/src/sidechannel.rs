//! QUIC 端口上的旁路：在 QUIC 看到之前把我们自己的探测包（ping/pong、多端口打洞的探测包）挑出来。
//!
//! 路径探测必须和 QUIC 用同一个 UDP 端口：NAT 为这个端口开的门才是 QUIC 后来要走的门。
//! 这里包一层 quinn 的套接字：收到带暗号的 ping 立刻回 pong（暗号是这次连接登记过的才回，
//! 不给陌生人当反射器），ping 和 pong 都交给登记了这个暗号的探测任务；其余的包原样交给 QUIC。

use std::collections::HashMap;
use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Instant;

use peerdesk_core::paths::{Ping, PING_MAGIC};
use peerdesk_core::signal::PROBE_MAGIC;
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::sync::mpsc;

/// 收到的一个探测包：从哪来、内容、收到的时刻。
#[derive(Debug, Clone, Copy)]
pub struct Heard {
    /// 来源地址（IPv4 映射的 IPv6 地址已还原成 IPv4）。
    pub from: SocketAddr,
    /// 内容。
    pub ping: Ping,
    /// 收到的时刻。
    pub at: Instant,
}

/// 按暗号登记的探测任务。
pub type Listeners = Arc<Mutex<HashMap<[u8; 16], mpsc::UnboundedSender<Heard>>>>;

/// 包了一层的套接字。
pub struct SideChannel {
    inner: Arc<dyn AsyncUdpSocket>,
    listeners: Listeners,
}

impl fmt::Debug for SideChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SideChannel").finish_non_exhaustive()
    }
}

impl SideChannel {
    /// 包住 `inner`。
    pub fn new(inner: Arc<dyn AsyncUdpSocket>, listeners: Listeners) -> Self {
        Self { inner, listeners }
    }

    /// 处理一个探测包；返回 `true` 表示是我们的包（不交给 QUIC）。
    fn take(&self, from: SocketAddr, data: &[u8], dst_ip: Option<std::net::IpAddr>) -> bool {
        if data.starts_with(PROBE_MAGIC) {
            // 多端口打洞的探测包：它的作用（让 NAT 开门）已经达到，丢掉。
            return true;
        }
        if !data.starts_with(PING_MAGIC) {
            return false;
        }
        let Some(ping) = Ping::decode(data) else {
            return true;
        };
        let listener = self
            .listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&ping.nonce)
            .cloned();
        let Some(listener) = listener else {
            return true;
        };
        if !ping.pong {
            let reply = ping.reply().encode();
            let transmit = Transmit {
                destination: from,
                ecn: None,
                contents: &reply,
                segment_size: None,
                src_ip: dst_ip,
            };
            // 发不出去（缓冲区满）就算了：对方会再发 ping。
            let _ = self.inner.try_send(&transmit);
        }
        let heard = Heard {
            from: canonical(from),
            ping,
            at: Instant::now(),
        };
        let _ = listener.send(heard);
        true
    }
}

/// IPv4 映射的 IPv6 地址还原成 IPv4（双栈端口收到的 IPv4 包是这种形式）。
#[must_use]
pub fn canonical(addr: SocketAddr) -> SocketAddr {
    SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

impl AsyncUdpSocket for SideChannel {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Arc::clone(&self.inner).create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        self.inner.try_send(transmit)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        loop {
            let n = match self.inner.poll_recv(cx, bufs, meta) {
                Poll::Ready(Ok(n)) => n,
                other @ (Poll::Ready(Err(_)) | Poll::Pending) => return other,
            };
            let kept = filter(bufs, meta, n, |from, data, dst| self.take(from, data, dst));
            if kept > 0 {
                return Poll::Ready(Ok(kept));
            }
            // 这一批全是我们的包：接着收（没有包时内层会登记唤醒并返回 Pending）。
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

/// 把前 `n` 个收到的包里 `ours` 认领的挑出去，其余的往前挪紧，返回剩下的个数。
/// 合并接收的一批（多个同样长的包连在一起）不拆，整批交给 QUIC。
fn filter(
    bufs: &mut [IoSliceMut<'_>],
    meta: &mut [RecvMeta],
    n: usize,
    mut ours: impl FnMut(SocketAddr, &[u8], Option<std::net::IpAddr>) -> bool,
) -> usize {
    let mut kept = 0;
    for i in 0..n.min(meta.len()).min(bufs.len()) {
        let Some(m) = meta.get(i).copied() else {
            break;
        };
        let single = m.stride == m.len;
        let claimed = single
            && bufs
                .get(i)
                .and_then(|b| b.get(..m.len))
                .is_some_and(|data| ours(m.addr, data, m.dst_ip));
        if claimed {
            continue;
        }
        if kept != i {
            let (front, back) = bufs.split_at_mut(i);
            if let (Some(dst), Some(src)) = (front.get_mut(kept), back.first()) {
                if let (Some(d), Some(s)) = (dst.get_mut(..m.len), src.get(..m.len)) {
                    d.copy_from_slice(s);
                }
            }
            if let Some(slot) = meta.get_mut(kept) {
                *slot = m;
            }
        }
        kept += 1;
    }
    kept
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn meta(addr: &str, len: usize, stride: usize) -> RecvMeta {
        RecvMeta {
            addr: addr.parse().unwrap(),
            len,
            stride,
            ..RecvMeta::default()
        }
    }

    #[test]
    fn filter_removes_claimed_packets_and_compacts() {
        let mut a = *b"AAAA";
        let mut b = *b"OURS";
        let mut c = *b"CCCC";
        let mut d = *b"DDDD";
        let mut bufs = [
            IoSliceMut::new(&mut a),
            IoSliceMut::new(&mut b),
            IoSliceMut::new(&mut c),
            IoSliceMut::new(&mut d),
        ];
        let mut metas = [
            meta("1.1.1.1:1", 4, 4),
            meta("2.2.2.2:2", 4, 4),
            meta("3.3.3.3:3", 4, 2),
            meta("4.4.4.4:4", 4, 4),
        ];
        // 认领 OURS；第三个是合并接收的一批（stride 不等于 len），即使内容像也不拆
        let kept = filter(&mut bufs, &mut metas, 4, |_, data, _| {
            data == b"OURS" || data == b"CCCC"
        });
        assert_eq!(kept, 3);
        assert_eq!(&*bufs[0], b"AAAA");
        assert_eq!(&*bufs[1], b"CCCC");
        assert_eq!(&*bufs[2], b"DDDD");
        assert_eq!(metas[1].addr, "3.3.3.3:3".parse().unwrap());
        assert_eq!(metas[2].addr, "4.4.4.4:4".parse().unwrap());
        // 全被认领
        let kept = filter(&mut bufs, &mut metas, 1, |_, _, _| true);
        assert_eq!(kept, 0);
    }

    #[test]
    fn mapped_addresses_are_restored() {
        let mapped: SocketAddr = "[::ffff:1.2.3.4]:5".parse().unwrap();
        assert_eq!(canonical(mapped), "1.2.3.4:5".parse().unwrap());
        let v6: SocketAddr = "[2001:db8::1]:5".parse().unwrap();
        assert_eq!(canonical(v6), v6);
    }
}
