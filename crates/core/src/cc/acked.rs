//! 已确认速率：最近一段时间里实际送达的速率（发送速率和到达速率取小）。
//!
//! 照 WebRTC `modules/congestion_controller/goog_cc/robust_throughput_estimator.cc`（默认打开）：
//! 窗口至少 20 个包、至少 1 秒、最多 5 秒 / 500 个包，至少 10 个包才给值；
//! 到达速率扣掉第一个到达的包、发送速率扣掉最后一个发出的包（各自区间的边界包不算在区间里）；
//! 到达时间里最大的一个间隔换成第二大的（延迟尖峰不把速率算低）；
//! 丢了的包算进发送量（`unacked_weight` 1.0）。
//!
//! 它只回答“最近实际送达了多少”，不代表链路能力：发送端自己少发（应用受限）时它也小，
//! 所以只在延迟判过载时拿来定降到多少，平时只限制升得多快（见 [`super::aimd`]）。

use std::collections::VecDeque;

use super::report::PacketResult;
use crate::time::Time;

const WINDOW_PACKETS: usize = 20;
const MAX_WINDOW_PACKETS: usize = 500;
const MIN_WINDOW_US: u64 = 1_000_000;
const MAX_WINDOW_US: u64 = 5_000_000;
const REQUIRED_PACKETS: usize = 10;

/// 已确认速率的估计。
#[derive(Debug, Clone, Default)]
pub struct AckedRate {
    /// 窗口里的包：(发送时间, 到达时间（没收到为 None）, 大小)。按到达顺序。
    window: VecDeque<(Time, Option<Time>, u64)>,
}

impl AckedRate {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            window: VecDeque::new(),
        }
    }

    /// 加入一份报告里的包。
    pub fn on_packets(&mut self, packets: &[PacketResult]) {
        for p in packets {
            self.window
                .push_back((p.sent.send_time, p.recv_time, u64::from(p.sent.size)));
        }
        // 按到达时间去掉最老的：保留至少 20 个包、至少 1 秒；最多 500 个、5 秒
        let newest = self.window.iter().filter_map(|(_, r, _)| *r).max();
        while self.window.len() > WINDOW_PACKETS {
            let Some(oldest) = self.window.front().and_then(|(_, r, _)| *r).or(newest) else {
                break;
            };
            let span = newest.map_or(0, |n| n.since(oldest));
            let too_many = self.window.len() > MAX_WINDOW_PACKETS;
            if span > MIN_WINDOW_US && (span > MAX_WINDOW_US || too_many || span > MIN_WINDOW_US)
                || too_many
            {
                self.window.pop_front();
            } else {
                break;
            }
        }
    }

    /// 当前估计（比特每秒）；包太少时为 `None`。
    #[must_use]
    pub fn bitrate_bps(&self) -> Option<u64> {
        let received: Vec<(Time, Time, u64)> = self
            .window
            .iter()
            .filter_map(|(s, r, b)| r.map(|r| (*s, r, *b)))
            .collect();
        if received.len() < REQUIRED_PACKETS {
            return None;
        }
        // 发送：所有包（含丢的），扣掉最后发出的那个
        let first_send = self.window.iter().map(|(s, _, _)| *s).min()?;
        let (last_send, last_size) = self
            .window
            .iter()
            .map(|(s, _, b)| (*s, *b))
            .max_by_key(|(s, _)| *s)?;
        let send_bytes = self
            .window
            .iter()
            .map(|(_, _, b)| *b)
            .sum::<u64>()
            .saturating_sub(last_size);
        // 到达：收到的包，扣掉第一个到达的；最大间隔换成第二大
        let mut arrivals: Vec<(Time, u64)> = received.iter().map(|(_, r, b)| (*r, *b)).collect();
        arrivals.sort_by_key(|(r, _)| *r);
        let first_size = arrivals.first().map_or(0, |(_, b)| *b);
        let recv_bytes = arrivals
            .iter()
            .map(|(_, b)| *b)
            .sum::<u64>()
            .saturating_sub(first_size);
        let mut gaps: Vec<u64> = arrivals
            .windows(2)
            .filter_map(|w| match w {
                [a, b] => Some(b.0.since(a.0)),
                _ => None,
            })
            .collect();
        gaps.sort_unstable();
        let largest = gaps.pop().unwrap_or(0);
        let second = gaps.last().copied().unwrap_or(0);
        let recv_span = arrivals
            .last()
            .zip(arrivals.first())
            .map_or(0, |(l, f)| l.0.since(f.0))
            .saturating_sub(largest)
            .saturating_add(second);
        let send_span = last_send.since(first_send);
        let rate = |bytes: u64, span: u64| bytes.saturating_mul(8_000_000) / span.max(1_000);
        Some(rate(send_bytes, send_span).min(rate(recv_bytes, recv_span)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc::report::{PacketKind, SentPacket};

    /// 按 `bps` 匀速发 `n` 个 1200 字节的包，单程 20 ms；`lost(i)` 为真的包没收到。
    fn paced(start_seq: u32, n: u32, bps: u64, lost: impl Fn(u32) -> bool) -> Vec<PacketResult> {
        let gap = 1200 * 8 * 1_000_000 / bps;
        (start_seq..start_seq + n)
            .map(|i| {
                let send = Time(u64::from(i) * gap);
                PacketResult {
                    sent: SentPacket {
                        seq: i,
                        send_time: send,
                        size: 1200,
                        kind: PacketKind::Media,
                    },
                    recv_time: (!lost(i)).then(|| send.plus(20_000)),
                }
            })
            .collect()
    }

    #[test]
    fn measures_what_was_delivered() {
        let mut a = AckedRate::new();
        a.on_packets(&paced(0, 5, 2_000_000, |_| false));
        assert_eq!(a.bitrate_bps(), None, "包太少");
        a.on_packets(&paced(5, 400, 2_000_000, |_| false));
        let r = a.bitrate_bps().unwrap();
        assert!((1_900_000..=2_100_000).contains(&r), "{r}");
        // 只留最近 1 秒左右
        assert!(a.window.len() <= 220, "{}", a.window.len());
    }

    #[test]
    fn one_delay_spike_does_not_drag_the_rate_down() {
        let mut a = AckedRate::new();
        let mut packets = paced(0, 200, 4_000_000, |_| false);
        // 中间一个包晚到 300 ms，之后的包跟着它一起到
        for p in packets.iter_mut().skip(100) {
            p.recv_time = p.recv_time.map(|t| t.plus(300_000));
        }
        a.on_packets(&packets);
        let r = a.bitrate_bps().unwrap();
        assert!(r > 3_000_000, "{r}");
    }

    #[test]
    fn lost_packets_count_as_sent_only() {
        let mut a = AckedRate::new();
        // 一半丢了：到达速率只有一半
        a.on_packets(&paced(0, 400, 2_000_000, |i| i % 2 == 0));
        let r = a.bitrate_bps().unwrap();
        assert!((900_000..=1_100_000).contains(&r), "{r}");
    }
}
