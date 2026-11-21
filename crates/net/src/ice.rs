//! 并行探测所有候选路径，测出往返时间，选最好的一条（核心逻辑在 `peerdesk_core::paths`）。
//!
//! 两端同时进行：每 [`PING_EVERY`] 向对方的每个候选地址发一个 ping；对方的端口自动回 pong
//! （见 [`crate::sidechannel`]）。控制端根据 pong 算往返时间并选路；被控端只负责发 ping
//! （让自己的 NAT 为控制端开门）。从对方没报告过的地址来的 ping/pong 说明对方的 NAT 换了端口，
//! 也当作一个候选（“对方换了端口”）加进去。

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use peerdesk_core::paths::{decide, Candidate, Decision, Limits, Measured, PathKind, Ping};

use crate::node::Subscription;
use crate::sidechannel::canonical;
use crate::{Fingerprint, Node};

/// 每隔多久向每个候选地址发一个 ping。
pub const PING_EVERY: Duration = Duration::from_millis(50);

/// 一次探测的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Race {
    /// 选中的路径；`None` 表示到时限一条都没通。
    pub picked: Option<Measured>,
    /// 测到的所有路径（写日志、给用户看）。
    pub measured: Vec<Measured>,
    /// 探测的所有候选（包括探测中发现的）。
    pub candidates: Vec<Candidate>,
    /// 用时。
    pub elapsed: Duration,
}

/// 探测进行中的状态（不做 IO，便于测试）。
#[derive(Debug, Clone, Default)]
struct Board {
    candidates: Vec<Candidate>,
    measured: Vec<Measured>,
    /// 是否接受探测中发现的新地址（只走中继时不接受）。
    reflexive: bool,
}

impl Board {
    fn new(candidates: Vec<Candidate>, reflexive: bool) -> Self {
        let mut board = Self {
            reflexive,
            ..Self::default()
        };
        for c in candidates {
            board.add(c);
        }
        board
    }

    /// 加一个候选（同一地址不重复）；返回它的种类。
    fn add(&mut self, c: Candidate) -> PathKind {
        let addr = canonical(c.addr);
        if let Some(known) = self.candidates.iter().find(|k| k.addr == addr) {
            return known.kind;
        }
        self.candidates.push(Candidate { addr, kind: c.kind });
        c.kind
    }

    /// 探测中发现的新地址；不接受时返回 `None`。
    fn discovered(&mut self, from: SocketAddr) -> Option<PathKind> {
        let from = canonical(from);
        let known = self
            .candidates
            .iter()
            .find(|k| k.addr == from)
            .map(|k| k.kind);
        if known.is_some() || !self.reflexive {
            return known;
        }
        Some(self.add(Candidate {
            addr: from,
            kind: PathKind::PeerReflexive,
        }))
    }

    /// 收到 `from` 来的 pong，往返时间 `rtt_us`，在探测开始后 `at_us`。
    fn pong(&mut self, from: SocketAddr, rtt_us: u64, at_us: u64) {
        let Some(kind) = self.discovered(from) else {
            return;
        };
        let from = canonical(from);
        if let Some(m) = self.measured.iter_mut().find(|m| m.candidate.addr == from) {
            m.rtt_us = m.rtt_us.min(rtt_us);
            return;
        }
        self.measured.push(Measured {
            candidate: Candidate { addr: from, kind },
            rtt_us,
            first_us: at_us,
        });
    }
}

/// 向所有候选发一轮 ping。
pub(crate) fn ping_all(node: &Node, nonce: [u8; 16], seq: u32, candidates: &[Candidate]) {
    let ping = Ping {
        nonce,
        pong: false,
        seq,
        sent_us: node.now_us(),
    }
    .encode();
    for c in candidates {
        node.send_raw(c.addr, &ping);
    }
}

/// 控制端：并行探测 `candidates`，按 `limits` 选路。`reflexive` 为假时（只走中继）
/// 不接受探测中才发现的对方地址。
pub async fn race(
    node: &Node,
    sub: &mut Subscription,
    nonce: [u8; 16],
    candidates: Vec<Candidate>,
    limits: Limits,
    reflexive: bool,
) -> Race {
    let started = Instant::now();
    let since = |at: Instant| {
        u64::try_from(at.saturating_duration_since(started).as_micros()).unwrap_or(u64::MAX)
    };
    let mut board = Board::new(candidates, reflexive);
    let mut tick = tokio::time::interval(PING_EVERY);
    let mut seq = 0u32;
    loop {
        let decision = decide(
            &board.measured,
            board.candidates.len(),
            since(Instant::now()),
            limits,
        );
        match decision {
            Decision::Wait => {}
            Decision::Pick(m) => return finish(board, Some(m), started),
            Decision::GiveUp => return finish(board, None, started),
        }
        tokio::select! {
            _ = tick.tick() => {
                ping_all(node, nonce, seq, &board.candidates);
                seq = seq.wrapping_add(1);
            }
            heard = sub.recv() => {
                let Some(heard) = heard else { return finish(board, None, started) };
                if heard.ping.pong {
                    let rtt = node.at_us(heard.at).saturating_sub(heard.ping.sent_us);
                    board.pong(heard.from, rtt, since(heard.at));
                } else {
                    // 对方从一个我们不知道的地址发来 ping：它的 NAT 换了端口，把这个地址也探测一下。
                    let _ = board.discovered(heard.from);
                }
            }
        }
    }
}

fn finish(board: Board, picked: Option<Measured>, started: Instant) -> Race {
    Race {
        picked,
        measured: board.measured,
        candidates: board.candidates,
        elapsed: started.elapsed(),
    }
}

/// 被控端发第一个 ping 之前等多久：让控制端先向这边发包，它那边的路由器先为这边开好门。
/// 有些路由器不丢外网主动发来的包，而是记一笔；被控端的 ping 抢先到达时，路由器会给控制端
/// 换一个公网端口，这边就再也找不到它了（NAT 仿真里一侧这样的路由器 3 次有 1 次失败）。
pub const HOST_PING_DELAY: Duration = Duration::from_millis(150);

/// 被控端：等 [`HOST_PING_DELAY`]，然后在 `within` 内不停向控制端的候选地址发 ping
/// （让本端 NAT 开门），对方从新地址发来的 ping 也加进去。已经接受了 `peer` 的连接就提前停。
pub async fn keep_pinging(
    node: &Node,
    mut sub: Subscription,
    nonce: [u8; 16],
    candidates: Vec<Candidate>,
    peer: Fingerprint,
    within: Duration,
) {
    tokio::time::sleep(HOST_PING_DELAY).await;
    let mut board = Board::new(candidates, true);
    let deadline = tokio::time::sleep(within);
    tokio::pin!(deadline);
    let mut tick = tokio::time::interval(PING_EVERY);
    let mut seq = 0u32;
    loop {
        tokio::select! {
            () = &mut deadline => return,
            _ = tick.tick() => {
                if node.accepted_recently(&peer, within) {
                    return;
                }
                ping_all(node, nonce, seq, &board.candidates);
                seq = seq.wrapping_add(1);
            }
            heard = sub.recv() => {
                let Some(heard) = heard else { return };
                let _ = board.discovered(heard.from);
            }
        }
    }
}

/// 一次探测结果的一行说明（写日志）。
#[must_use]
pub fn describe(race: &Race) -> String {
    let mut parts: Vec<String> = race
        .measured
        .iter()
        .map(|m| {
            format!(
                "{} {} {:.1} ms",
                m.candidate.kind.label(),
                m.candidate.addr,
                to_ms(m.rtt_us)
            )
        })
        .collect();
    let silent = race
        .candidates
        .iter()
        .filter(|c| !race.measured.iter().any(|m| m.candidate.addr == c.addr))
        .map(|c| format!("{} {} 不通", c.kind.label(), c.addr));
    parts.extend(silent);
    let picked = race.picked.map_or_else(
        || "没有可用路径".to_owned(),
        |m| format!("选 {} {}", m.candidate.kind.label(), m.candidate.addr),
    );
    format!("探测 {:?}：{}；{picked}", race.elapsed, parts.join("，"))
}

/// 微秒换成毫秒（日志用）。
fn to_ms(us: u64) -> f64 {
    // 往返时间远小于 2^52 微秒，换成浮点数不丢精度。
    #[allow(clippy::cast_precision_loss)]
    let ms = us as f64 / 1000.0;
    ms
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn c(addr: &str, kind: PathKind) -> Candidate {
        Candidate {
            addr: addr.parse().unwrap(),
            kind,
        }
    }

    #[test]
    fn board_tracks_minimum_rtt_and_reflexive_addresses() {
        let mut only_relay = Board::new(vec![c("127.0.0.1:7", PathKind::Relay)], false);
        only_relay.pong("1.2.3.4:5".parse().unwrap(), 1, 1);
        assert!(only_relay.measured.is_empty(), "只走中继时不接受新地址");
        only_relay.pong("127.0.0.1:7".parse().unwrap(), 1, 1);
        assert_eq!(only_relay.measured.len(), 1);
        let mut b = Board::new(
            vec![
                c("10.0.0.2:5", PathKind::Lan),
                c("10.0.0.2:5", PathKind::Lan),
                c("[::ffff:1.2.3.4]:5", PathKind::Public),
            ],
            true,
        );
        assert_eq!(
            b.candidates.len(),
            2,
            "重复的地址只留一个，映射地址还原成 IPv4"
        );
        b.pong("1.2.3.4:5".parse().unwrap(), 30_000, 30_000);
        b.pong("1.2.3.4:5".parse().unwrap(), 20_000, 60_000);
        assert_eq!(b.measured.len(), 1);
        assert_eq!(b.measured[0].rtt_us, 20_000);
        assert_eq!(b.measured[0].first_us, 30_000);
        assert_eq!(b.measured[0].candidate.kind, PathKind::Public);
        b.pong("1.2.3.4:6".parse().unwrap(), 25_000, 70_000);
        assert_eq!(b.candidates.len(), 3);
        assert_eq!(b.measured[1].candidate.kind, PathKind::PeerReflexive);
        let race = Race {
            picked: b.measured.first().copied(),
            measured: b.measured.clone(),
            candidates: b.candidates.clone(),
            elapsed: Duration::from_millis(80),
        };
        let line = describe(&race);
        assert!(
            line.contains("20.0 ms") && line.contains("局域网直连 10.0.0.2:5 不通"),
            "{line}"
        );
        assert!(line.contains("选 公网直连"), "{line}");
        let none = Race {
            picked: None,
            ..race
        };
        assert!(describe(&none).contains("没有可用路径"));
    }
}
