//! 传输基准：同一台机器的回环地址上，对比裸 UDP 和 QUIC 数据报的单向延迟。
//!
//! 用来回答技术方案里的问题：QUIC 数据报相对裸 UDP 多出多少延迟。
//! 回环测不出真实网络上的表现，但能测出协议栈本身的开销。

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use peerdesk_core::crypto::Policy;
use peerdesk_core::proto::Channel;
use tokio::net::UdpSocket;

use crate::{Identity, NetError, Node};

/// 一种传输方式的延迟统计（微秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Latency {
    /// 收到的包数。
    pub received: usize,
    /// 中位数。
    pub p50_us: u64,
    /// 99 分位。
    pub p99_us: u64,
    /// 最大值。
    pub max_us: u64,
}

/// 对比结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Comparison {
    /// 发送的包数。
    pub packets: usize,
    /// 每个包的载荷字节数。
    pub payload_bytes: usize,
    /// 裸 UDP。
    pub udp: Latency,
    /// QUIC 数据报。
    pub quic: Latency,
}

/// 每批发几个包（模拟一帧的分片）。
const BURST: usize = 6;
/// 批与批之间的间隔。
const GAP: Duration = Duration::from_millis(2);
/// 等最后几个包的时间上限。
const TAIL: Duration = Duration::from_secs(2);

fn summarize(mut samples: Vec<u64>) -> Latency {
    samples.sort_unstable();
    let at = |pct: usize| {
        let index = samples.len().saturating_sub(1).saturating_mul(pct) / 100;
        samples.get(index).copied().unwrap_or(0)
    };
    Latency {
        received: samples.len(),
        p50_us: at(50),
        p99_us: at(99),
        max_us: at(100),
    }
}

fn stamp(start: Instant, size: usize) -> Vec<u8> {
    let mut payload = vec![0u8; size.max(8)];
    let nanos = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    if let Some(head) = payload.get_mut(..8) {
        head.copy_from_slice(&nanos.to_be_bytes());
    }
    payload
}

fn elapsed_us(start: Instant, payload: &[u8]) -> Option<u64> {
    let sent = u64::from_be_bytes(payload.get(..8)?.try_into().ok()?);
    let now = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
    Some(now.saturating_sub(sent) / 1000)
}

async fn bench_udp(packets: usize, size: usize) -> Result<Latency, NetError> {
    let any: SocketAddr = ([127, 0, 0, 1], 0).into();
    let rx = UdpSocket::bind(any)
        .await
        .map_err(|e| NetError::Io(e.to_string()))?;
    let tx = UdpSocket::bind(any)
        .await
        .map_err(|e| NetError::Io(e.to_string()))?;
    tx.connect(rx.local_addr().map_err(|e| NetError::Io(e.to_string()))?)
        .await
        .map_err(|e| NetError::Io(e.to_string()))?;
    let start = Instant::now();
    let receiver = tokio::spawn(async move {
        let mut samples = Vec::with_capacity(packets);
        let mut buf = vec![0u8; 2048];
        while samples.len() < packets {
            match tokio::time::timeout(TAIL, rx.recv(&mut buf)).await {
                Ok(Ok(n)) => samples.extend(elapsed_us(start, buf.get(..n).unwrap_or_default())),
                Ok(Err(_)) | Err(_) => break,
            }
        }
        samples
    });
    for i in 0..packets {
        tx.send(&stamp(start, size))
            .await
            .map_err(|e| NetError::Io(e.to_string()))?;
        if i % BURST == BURST - 1 {
            tokio::time::sleep(GAP).await;
        }
    }
    let samples = receiver.await.map_err(|e| NetError::Io(e.to_string()))?;
    Ok(summarize(samples))
}

async fn bench_quic(packets: usize, size: usize) -> Result<Latency, NetError> {
    let any: SocketAddr = ([127, 0, 0, 1], 0).into();
    let policy = Policy::recommended();
    let a = Node::bind(any, &Identity::generate()?, &policy)?;
    let b = Node::bind(any, &Identity::generate()?, &policy)?;
    let addr = a.local_addr()?;
    let (tx, rx) = tokio::join!(b.connect(addr, None), a.accept());
    let tx = tx?;
    let rx = rx.ok_or_else(|| NetError::Closed("端点已关闭".into()))??;
    let size = size.min(tx.max_payload());

    let start = Instant::now();
    let receiver = tokio::spawn(async move {
        let mut samples = Vec::with_capacity(packets);
        while samples.len() < packets {
            match tokio::time::timeout(TAIL, rx.recv()).await {
                Ok(Ok((_, payload))) => samples.extend(elapsed_us(start, &payload)),
                Ok(Err(_)) | Err(_) => break,
            }
        }
        samples
    });
    for i in 0..packets {
        tx.send(Channel::Video, &stamp(start, size))?;
        if i % BURST == BURST - 1 {
            tokio::time::sleep(GAP).await;
        }
    }
    let samples = receiver.await.map_err(|e| NetError::Io(e.to_string()))?;
    tx.close("done");
    a.close();
    b.close();
    Ok(summarize(samples))
}

/// 运行对比。`packets` 是每种方式发送的包数，`payload_bytes` 是每个包的载荷大小。
pub async fn compare(packets: usize, payload_bytes: usize) -> Result<Comparison, NetError> {
    let udp = bench_udp(packets, payload_bytes).await?;
    let quic = bench_quic(packets, payload_bytes).await?;
    Ok(Comparison {
        packets,
        payload_bytes,
        udp,
        quic,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_statistics() {
        assert_eq!(
            summarize(vec![]),
            Latency {
                received: 0,
                p50_us: 0,
                p99_us: 0,
                max_us: 0
            }
        );
        let s = summarize((1..=100).rev().collect());
        assert_eq!(
            (s.received, s.p50_us, s.p99_us, s.max_us),
            (100, 50, 99, 100)
        );
    }

    #[test]
    fn stamps_roundtrip() {
        let start = Instant::now();
        let payload = stamp(start, 3); // 小于时间戳长度时自动加长
        assert_eq!(payload.len(), 8);
        assert!(elapsed_us(start, &payload).unwrap() < 1_000_000);
        assert_eq!(elapsed_us(start, &[1, 2, 3]), None);
    }
}
