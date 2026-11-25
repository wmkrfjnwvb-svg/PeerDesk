//! 延迟趋势和过载判断：按发送时间把包分组（5 ms 一组），比较相邻两组“到达间隔 − 发送间隔”，
//! 累积后做线性回归，斜率乘增益和自适应门限比，判“过载 / 正常 / 低载”。
//!
//! 照 WebRTC（行为和常量逐一对应）：
//! - 分组：`modules/congestion_controller/goog_cc/inter_arrival_delta.cc`（`kSendTimeGroupLength` 5 ms，
//!   突发判定 `kBurstDeltaThreshold` 5 ms、`kMaxBurstDuration` 100 ms）；
//! - 趋势：`trendline_estimator.cc`（窗口 20、平滑 0.9、增益 4、`kMinNumDeltas` 60、
//!   过载持续 10 ms 且连续两次、趋势不降才判过载；门限 12.5 ms 起、6–600、k_up 0.0087、k_down 0.039、
//!   离门限超过 15 ms 的尖峰不调门限）。
//!
//! 输入是发送端对账后的包（[`super::report::PacketResult`]）：发送时间是发送端时钟，到达时间是接收端时钟，
//! 只用差值，两边时钟不用对齐。只算收到的包；丢的包交给丢包估计。

use std::collections::VecDeque;

use super::report::PacketResult;
use crate::time::Time;

/// 一组的发送时间跨度上限（微秒）。
pub const SEND_GROUP_US: u64 = 5_000;
/// 突发：到达间隔不超过它（微秒）的包算同一组。
const BURST_DELTA_US: u64 = 5_000;
/// 突发最长持续（微秒）。
const MAX_BURST_US: u64 = 100_000;
/// 回归窗口（组数）。
const WINDOW: usize = 20;
/// 平滑系数。
const SMOOTHING: f64 = 0.9;
/// 斜率增益。
const GAIN: f64 = 4.0;
/// 斜率乘的组数上限。
const MIN_NUM_DELTAS: u32 = 60;
/// 过载要持续多久（毫秒）。
const OVERUSE_TIME_MS: f64 = 10.0;
/// 门限的初值、上下限（毫秒）和调整速度。
const THRESHOLD_INIT: f64 = 12.5;
const THRESHOLD_MIN: f64 = 6.0;
const THRESHOLD_MAX: f64 = 600.0;
const K_UP: f64 = 0.0087;
const K_DOWN: f64 = 0.039;
/// 离门限超过这么多（毫秒）的尖峰不调门限。
const MAX_ADAPT_OFFSET_MS: f64 = 15.0;

/// 链路状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    /// 排队在涨：过载。
    Over,
    /// 正常。
    Normal,
    /// 排队在消：低载。
    Under,
}

#[derive(Debug, Clone, Copy)]
struct Group {
    first_send: Time,
    last_send: Time,
    first_arrival: Time,
    complete: Time,
}

/// 分组（[`InterArrival`]）+ 趋势（线性回归）+ 过载判断。
#[derive(Debug, Clone)]
pub struct DelayDetector {
    current: Option<Group>,
    previous: Option<Group>,
    num_deltas: u32,
    accumulated: f64,
    smoothed: f64,
    first_arrival_ms: Option<f64>,
    history: VecDeque<(f64, f64)>,
    prev_trend: f64,
    threshold: f64,
    last_threshold_update_ms: Option<f64>,
    time_over_using: Option<f64>,
    overuse_counter: u32,
    usage: Usage,
    /// 最近一次的修正趋势（毫秒），诊断日志用。
    modified_trend: f64,
}

impl Default for DelayDetector {
    fn default() -> Self {
        Self::new()
    }
}

fn ms(t: Time) -> f64 {
    t.micros() as f64 / 1000.0
}

fn signed_ms(a: Time, b: Time) -> f64 {
    ms(a) - ms(b)
}

impl DelayDetector {
    /// 创建。
    #[must_use]
    pub fn new() -> Self {
        Self {
            current: None,
            previous: None,
            num_deltas: 0,
            accumulated: 0.0,
            smoothed: 0.0,
            first_arrival_ms: None,
            history: VecDeque::new(),
            prev_trend: 0.0,
            threshold: THRESHOLD_INIT,
            last_threshold_update_ms: None,
            time_over_using: None,
            overuse_counter: 0,
            usage: Usage::Normal,
            modified_trend: 0.0,
        }
    }

    /// 现在的判断。
    #[must_use]
    pub const fn usage(&self) -> Usage {
        self.usage
    }

    /// 修正趋势和门限（毫秒），诊断日志用。
    #[must_use]
    pub const fn trend_and_threshold(&self) -> (f64, f64) {
        (self.modified_trend, self.threshold)
    }

    /// 处理一份报告里收到的包（按到达时间排好序）。
    pub fn on_packets(&mut self, packets: &[PacketResult]) -> Usage {
        let mut received: Vec<(Time, Time)> = packets
            .iter()
            .filter_map(|p| p.recv_time.map(|r| (p.sent.send_time, r)))
            .collect();
        received.sort_by_key(|(_, recv)| *recv);
        for (send, recv) in received {
            self.on_packet(send, recv);
        }
        self.usage
    }

    fn on_packet(&mut self, send: Time, recv: Time) {
        let Some(cur) = self.current.as_mut() else {
            self.current = Some(Group {
                first_send: send,
                last_send: send,
                first_arrival: recv,
                complete: recv,
            });
            return;
        };
        // 乱序：比这一组第一个包还早发的，丢掉
        if send < cur.first_send {
            return;
        }
        let in_group = send.since(cur.first_send) <= SEND_GROUP_US || {
            // 突发：发送间隔为 0，或到达得比发送间隔还紧（排队在消）且到达间隔很小
            let send_delta = send.since(cur.last_send);
            let arrival_delta = recv.since(cur.complete);
            let propagation = signed_ms(recv, cur.complete) - signed_ms(send, cur.last_send);
            send_delta == 0
                || (propagation < 0.0
                    && arrival_delta <= BURST_DELTA_US
                    && recv.since(cur.first_arrival) < MAX_BURST_US)
        };
        if in_group {
            cur.last_send = cur.last_send.max(send);
            cur.complete = cur.complete.max(recv);
            return;
        }
        let finished = *cur;
        self.current = Some(Group {
            first_send: send,
            last_send: send,
            first_arrival: recv,
            complete: recv,
        });
        if let Some(prev) = self.previous.replace(finished) {
            let send_delta = signed_ms(finished.last_send, prev.last_send);
            let recv_delta = signed_ms(finished.complete, prev.complete);
            self.update(recv_delta, send_delta, ms(finished.complete));
        }
    }

    /// `TrendlineEstimator::Update`。
    fn update(&mut self, recv_delta: f64, send_delta: f64, arrival_ms: f64) {
        let delta = recv_delta - send_delta;
        self.num_deltas = self.num_deltas.saturating_add(1).min(1000);
        let first = *self.first_arrival_ms.get_or_insert(arrival_ms);
        self.accumulated += delta;
        self.smoothed = SMOOTHING * self.smoothed + (1.0 - SMOOTHING) * self.accumulated;
        self.history.push_back((arrival_ms - first, self.smoothed));
        if self.history.len() > WINDOW {
            self.history.pop_front();
        }
        let trend = if self.history.len() == WINDOW {
            slope(&self.history).unwrap_or(self.prev_trend)
        } else {
            self.prev_trend
        };
        self.detect(trend, send_delta, arrival_ms);
    }

    /// `TrendlineEstimator::Detect`。
    fn detect(&mut self, trend: f64, send_delta: f64, now_ms: f64) {
        if self.num_deltas < 2 {
            self.usage = Usage::Normal;
            return;
        }
        let modified = f64::from(self.num_deltas.min(MIN_NUM_DELTAS)) * trend * GAIN;
        self.modified_trend = modified;
        if modified > self.threshold {
            let over = self
                .time_over_using
                .map_or(send_delta / 2.0, |t| t + send_delta);
            self.time_over_using = Some(over);
            self.overuse_counter = self.overuse_counter.saturating_add(1);
            if over > OVERUSE_TIME_MS && self.overuse_counter > 1 && trend >= self.prev_trend {
                self.time_over_using = Some(0.0);
                self.overuse_counter = 0;
                self.usage = Usage::Over;
            }
        } else if modified < -self.threshold {
            self.time_over_using = None;
            self.overuse_counter = 0;
            self.usage = Usage::Under;
        } else {
            self.time_over_using = None;
            self.overuse_counter = 0;
            self.usage = Usage::Normal;
        }
        self.prev_trend = trend;
        self.update_threshold(modified, now_ms);
    }

    /// `TrendlineEstimator::UpdateThreshold`。
    fn update_threshold(&mut self, modified: f64, now_ms: f64) {
        let last = *self.last_threshold_update_ms.get_or_insert(now_ms);
        if modified.abs() > self.threshold + MAX_ADAPT_OFFSET_MS {
            self.last_threshold_update_ms = Some(now_ms);
            return;
        }
        let k = if modified.abs() < self.threshold {
            K_DOWN
        } else {
            K_UP
        };
        let dt = (now_ms - last).clamp(0.0, 100.0);
        self.threshold += k * (modified.abs() - self.threshold) * dt;
        self.threshold = self.threshold.clamp(THRESHOLD_MIN, THRESHOLD_MAX);
        self.last_threshold_update_ms = Some(now_ms);
    }
}

/// 最小二乘斜率；横坐标都一样时为 `None`。
fn slope(points: &VecDeque<(f64, f64)>) -> Option<f64> {
    let n = points.len() as f64;
    let (sx, sy) = points
        .iter()
        .fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let (num, den) = points.iter().fold((0.0, 0.0), |(num, den), (x, y)| {
        (num + (x - mx) * (y - my), den + (x - mx) * (x - mx))
    });
    (den != 0.0).then(|| num / den)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc::report::{PacketKind, SentPacket};

    /// 每 `gap_us` 发一个包，单程延迟按 `delay(i)`（微秒）；返回对账后的包。
    fn stream(n: u32, gap_us: u64, delay: impl Fn(u32) -> u64) -> Vec<PacketResult> {
        (0..n)
            .map(|i| {
                let send = Time(u64::from(i) * gap_us);
                PacketResult {
                    sent: SentPacket {
                        seq: i,
                        send_time: send,
                        size: 1200,
                        kind: PacketKind::Media,
                    },
                    recv_time: Some(send.plus(delay(i))),
                }
            })
            .collect()
    }

    #[test]
    fn steady_delay_is_normal() {
        let mut d = DelayDetector::new();
        assert_eq!(d.on_packets(&stream(400, 2_000, |_| 30_000)), Usage::Normal);
        let (trend, threshold) = d.trend_and_threshold();
        assert!(trend.abs() < 1.0, "{trend}");
        assert!(
            (THRESHOLD_MIN..=THRESHOLD_INIT).contains(&threshold),
            "{threshold}"
        );
    }

    #[test]
    fn growing_queue_is_overuse_and_draining_is_underuse() {
        let mut d = DelayDetector::new();
        d.on_packets(&stream(200, 2_000, |_| 30_000));
        // 排队每个包涨 0.5 ms：持续过载
        let base: u32 = 200;
        let rising: Vec<_> = stream(300, 2_000, |i| 30_000 + u64::from(i) * 500)
            .into_iter()
            .map(|mut p| {
                p.sent.send_time = p.sent.send_time.plus(u64::from(base) * 2_000);
                p.recv_time = p.recv_time.map(|t| t.plus(u64::from(base) * 2_000));
                p
            })
            .collect();
        assert_eq!(d.on_packets(&rising), Usage::Over);
        // 排队消下去（排空时到达比发送紧，按突发并成长组，要攒够组数才看得出来）：一份一份报告地喂
        let start = 500u64 * 2_000;
        let falling: Vec<_> = stream(3000, 2_000, |i| {
            180_000u64.saturating_sub(u64::from(i) * 100).max(30_000)
        })
        .into_iter()
        .map(|mut p| {
            p.sent.send_time = p.sent.send_time.plus(start);
            p.recv_time = p.recv_time.map(|t| t.plus(start));
            p
        })
        .collect();
        let seen_under = falling
            .chunks(10)
            .any(|chunk| d.on_packets(chunk) == Usage::Under);
        assert!(seen_under, "排空时应当判低载");
        assert_ne!(d.usage(), Usage::Over, "排空后平稳");
    }

    #[test]
    fn groups_bursts_and_skips_reordered_and_lost_packets() {
        let mut d = DelayDetector::new();
        // 一帧 20 个包背靠背（发送间隔 0）：只算一组
        let burst = stream(20, 0, |i| 10_000 + u64::from(i) * 100);
        d.on_packets(&burst);
        assert_eq!(d.num_deltas, 0);
        // 丢的包不算；比当前组还早发的乱序包不算
        let mut lost = stream(3, 10_000, |_| 10_000);
        lost[1].recv_time = None;
        let mut late = lost[0];
        late.sent.send_time = Time(0);
        d.on_packets(&[late]);
        d.on_packets(&lost);
        assert!(d.num_deltas <= 2);
        // 突发：到达比发送紧（排队在消）、间隔很小的包并进同一组
        let mut b = DelayDetector::new();
        let mut packets = stream(2, 10_000, |_| 50_000);
        packets[1].recv_time = Some(Time(50_000 + 1_000));
        b.on_packets(&packets);
        assert!(b.previous.is_none(), "第二个包并进了第一组");
        assert!(slope(&VecDeque::from(vec![(1.0, 1.0), (1.0, 2.0)])).is_none());
        assert_eq!(DelayDetector::default().usage(), Usage::Normal);
    }

    #[test]
    fn spikes_do_not_move_the_threshold() {
        let mut d = DelayDetector::new();
        d.on_packets(&stream(100, 2_000, |_| 30_000));
        let before = d.threshold;
        d.update_threshold(before + MAX_ADAPT_OFFSET_MS + 1.0, 1e9);
        assert_eq!(d.threshold, before);
        d.update_threshold(before + 10.0, 1e9 + 100.0);
        assert!(d.threshold > before, "持续高于门限：门限往上走");
    }
}
