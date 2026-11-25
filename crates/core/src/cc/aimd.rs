//! 码率的加性增、乘性减（AIMD）和链路容量估计。
//!
//! 照 WebRTC `modules/remote_bitrate_estimator/aimd_rate_control.cc`、`link_capacity_estimator.cc`：
//! - 状态机：延迟判“正常”→ 升；“过载”→ 降；“低载”→ 保持（排队在消，先别动）；
//! - 降：降到 **已确认速率 × 0.85**（再减 5 kbps），只在比当前低时才降；降完保持；
//!   降的时候记一次链路容量（已确认速率的平滑值和方差）；
//! - 升：知道链路容量时按“每个往返加约一个包”慢慢逼近（加性），不知道时每秒 ×1.08（乘性）；
//!   **升不超过 已确认速率 × 1.5 + 10 kbps**（发送端自己少发时，估计不会凭空涨上天）；
//! - 两次降之间至少隔一个往返（10–200 ms），或者已确认速率掉到估计的一半以下。
//!
//! 只在延迟判过载时降——不因为“送达速率低”就降（K-pipeline.md P3：发送端只有两三帧时，
//! 原来的估计器按送达速率降到 100 kbps）。

use crate::time::Time;

/// 降的系数（千分比）。
pub const BETA_PERMILLE: u64 = 850;
/// 乘性增：每秒涨 8%（按经过的时间折算，最多按 1 秒算）。
const INCREASE_PER_SECOND_PERMILLE: u64 = 77; // ln(1.08) ≈ 0.077
/// 升的上限：已确认速率的 1.5 倍再加 10 kbps。
const INCREASE_LIMIT_PERMILLE: u64 = 1500;
const INCREASE_LIMIT_EXTRA_BPS: u64 = 10_000;
/// 加性增至少每秒这么多。
const MIN_ADDITIVE_BPS_PER_S: u64 = 4_000;
/// 往返时间不知道时按这个算（微秒）。
const DEFAULT_RTT_US: u64 = 200_000;

/// 延迟检测的结果（见 [`super::delay::Usage`]）。
pub use super::delay::Usage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Hold,
    Increase,
    Decrease,
}

/// 链路容量：每次过载时的已确认速率（kbps）的平滑值和归一化方差（千分比，0.4–2.5）。
/// 上下界是 估计 ± 3 × √(方差 × 估计)（WebRTC 的 `LinkCapacityEstimator`，平滑系数 0.05）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinkCapacity {
    estimate_kbps: Option<u64>,
    deviation_permille: u64,
}

impl Default for LinkCapacity {
    fn default() -> Self {
        Self {
            estimate_kbps: None,
            deviation_permille: 400,
        }
    }
}

impl LinkCapacity {
    fn on_overuse(&mut self, acked_bps: u64) {
        let sample = acked_bps / 1000;
        let estimate = self.estimate_kbps.map_or(sample, |e| {
            (e.saturating_mul(95) + sample.saturating_mul(5)) / 100
        });
        let error = estimate.abs_diff(sample);
        let norm = estimate.max(1);
        let term = error.saturating_mul(error).saturating_mul(1000) / norm;
        self.deviation_permille =
            ((self.deviation_permille.saturating_mul(950) + term.saturating_mul(50)) / 1000)
                .clamp(400, 2500);
        self.estimate_kbps = Some(estimate);
    }

    /// 估计 ± 3 × √(方差 × 估计)，比特每秒。
    fn spread_kbps(&self, e: u64) -> u64 {
        3 * self
            .deviation_permille
            .saturating_mul(e)
            .saturating_mul(1000)
            .isqrt()
            / 1000
    }

    fn upper_bps(&self) -> Option<u64> {
        self.estimate_kbps
            .map(|e| e.saturating_add(self.spread_kbps(e)).saturating_mul(1000))
    }

    fn lower_bps(&self) -> Option<u64> {
        self.estimate_kbps
            .map(|e| e.saturating_sub(self.spread_kbps(e)).saturating_mul(1000))
    }

    fn estimate_bps(&self) -> Option<u64> {
        self.estimate_kbps.map(|e| e.saturating_mul(1000))
    }
}

/// AIMD 码率控制。
#[derive(Debug, Clone)]
pub struct Aimd {
    rate_bps: u64,
    min_bps: u64,
    max_bps: u64,
    state: State,
    capacity: LinkCapacity,
    rtt_us: u64,
    last_change: Option<Time>,
    /// 最近一次的已确认速率。
    throughput_bps: Option<u64>,
    /// 最近一次降的原因（诊断日志用）。
    last_decrease: Option<(u64, u64)>,
}

impl Aimd {
    /// 从 `start_bps` 开始，夹在 `[min_bps, max_bps]`。
    #[must_use]
    pub fn new(start_bps: u64, min_bps: u64, max_bps: u64) -> Self {
        let min_bps = min_bps.max(1);
        let max_bps = max_bps.max(min_bps);
        Self {
            rate_bps: start_bps.clamp(min_bps, max_bps),
            min_bps,
            max_bps,
            state: State::Hold,
            capacity: LinkCapacity::default(),
            rtt_us: DEFAULT_RTT_US,
            last_change: None,
            throughput_bps: None,
            last_decrease: None,
        }
    }

    /// 当前码率。
    #[must_use]
    pub const fn rate_bps(&self) -> u64 {
        self.rate_bps
    }

    /// 链路容量估计（比特每秒），过载过至少一次才有。
    #[must_use]
    pub fn capacity_bps(&self) -> Option<u64> {
        self.capacity.estimate_bps()
    }

    /// 更新往返时间。
    pub fn set_rtt(&mut self, rtt_us: u64) {
        if rtt_us > 0 {
            self.rtt_us = rtt_us;
        }
    }

    /// 直接设码率（探测结果、按丢包限制之后）。
    pub fn set_rate(&mut self, now: Time, bps: u64) {
        self.rate_bps = bps.clamp(self.min_bps, self.max_bps);
        self.last_change = Some(now);
    }

    /// 取走最近一次降：（降前, 降后）。
    pub fn take_decrease(&mut self) -> Option<(u64, u64)> {
        self.last_decrease.take()
    }

    /// 过载时现在能不能再降（`DelayBasedBwe::MaybeUpdateEstimate` 里的 `TimeToReduceFurther`）。
    #[must_use]
    pub fn time_to_reduce(&self, now: Time, throughput_bps: u64) -> bool {
        let interval = self.rtt_us.clamp(10_000, 200_000);
        let waited = self.last_change.is_none_or(|t| now.since(t) >= interval);
        waited || throughput_bps < self.rate_bps / 2
    }

    /// 处理一次延迟判断。`throughput_bps` 是已确认速率（不知道时 `None`）。
    pub fn update(&mut self, now: Time, usage: Usage, throughput_bps: Option<u64>) -> u64 {
        if let Some(t) = throughput_bps {
            self.throughput_bps = Some(t);
        }
        let throughput = self.throughput_bps;
        // 状态机
        self.state = match (usage, self.state) {
            (Usage::Normal, State::Hold) => {
                self.last_change = Some(now);
                State::Increase
            }
            (Usage::Normal, s) => s,
            (Usage::Over, State::Increase | State::Hold | State::Decrease) => State::Decrease,
            (Usage::Under, State::Increase | State::Hold | State::Decrease) => State::Hold,
        };
        let mut new_rate = self.rate_bps;
        match self.state {
            State::Hold => {}
            State::Increase => {
                if throughput.is_some_and(|t| self.capacity.upper_bps().is_some_and(|u| t > u)) {
                    self.capacity = LinkCapacity::default();
                }
                let elapsed = self.last_change.map_or(0, |t| now.since(t)).min(1_000_000);
                let step = if self.capacity.estimate_kbps.is_some() {
                    self.additive_bps_per_s().saturating_mul(elapsed) / 1_000_000
                } else {
                    (self
                        .rate_bps
                        .saturating_mul(INCREASE_PER_SECOND_PERMILLE)
                        .saturating_mul(elapsed)
                        / 1_000_000_000)
                        .max(1_000)
                };
                new_rate = new_rate.saturating_add(step);
                self.last_change = Some(now);
            }
            State::Decrease => {
                let base = throughput.unwrap_or(self.rate_bps);
                let mut decreased = base.saturating_mul(BETA_PERMILLE) / 1000;
                if decreased > 5_000 {
                    decreased -= 5_000;
                }
                if decreased > self.rate_bps {
                    if let Some(c) = self.capacity.estimate_bps() {
                        decreased = c.saturating_mul(BETA_PERMILLE) / 1000;
                    }
                }
                if decreased < self.rate_bps {
                    self.last_decrease = Some((self.rate_bps, decreased));
                    new_rate = decreased;
                }
                if throughput.is_some_and(|t| self.capacity.lower_bps().is_some_and(|l| t < l)) {
                    self.capacity = LinkCapacity::default();
                }
                self.capacity.on_overuse(base);
                self.state = State::Hold;
                self.last_change = Some(now);
            }
        }
        // 升不超过已确认速率的 1.5 倍加 10 kbps（没有已确认速率时不限）
        if new_rate > self.rate_bps {
            if let Some(t) = throughput {
                let limit =
                    t.saturating_mul(INCREASE_LIMIT_PERMILLE) / 1000 + INCREASE_LIMIT_EXTRA_BPS;
                new_rate = new_rate.min(limit.max(self.rate_bps));
            }
        }
        self.rate_bps = new_rate.clamp(self.min_bps, self.max_bps);
        self.rate_bps
    }

    /// 接近容量时每秒加多少：每个“往返 + 100 ms”加约一个包（按 30 帧、1200 字节一个包算平均包大小）。
    fn additive_bps_per_s(&self) -> u64 {
        let frame_bytes = self.rate_bps / 8 / 30;
        let packets = frame_bytes.div_ceil(1200).max(1);
        let avg_packet_bits = frame_bytes / packets * 8;
        let response_us = self.rtt_us.saturating_add(100_000);
        (avg_packet_bits.saturating_mul(1_000_000) / response_us).max(MIN_ADDITIVE_BPS_PER_S)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000;

    #[test]
    fn increases_multiplicatively_until_overuse_then_backs_off_to_acked() {
        let mut a = Aimd::new(1_000_000, 100_000, 30_000_000);
        let mut t = Time(0);
        a.update(t, Usage::Normal, None);
        for _ in 0..10 {
            t = t.plus(100 * MS);
            a.update(t, Usage::Normal, None);
        }
        // 一秒约涨 8%
        assert!(
            (1_070_000..=1_090_000).contains(&a.rate_bps()),
            "{}",
            a.rate_bps()
        );
        // 过载：降到已确认速率的 85% 减 5 kbps
        t = t.plus(100 * MS);
        let before = a.rate_bps();
        a.update(t, Usage::Over, Some(900_000));
        assert_eq!(a.rate_bps(), 900_000 * 85 / 100 - 5_000);
        assert_eq!(a.take_decrease(), Some((before, a.rate_bps())));
        assert_eq!(a.take_decrease(), None);
        assert_eq!(a.capacity_bps(), Some(900_000));
    }

    #[test]
    fn acked_rate_caps_increases_and_only_overuse_decreases() {
        let mut a = Aimd::new(2_000_000, 100_000, 30_000_000);
        let mut t = Time(0);
        // 发送端自己只发 200 kbps（应用受限）：估计不往上涨过 1.5 倍 + 10 kbps，但也不往下掉
        for _ in 0..50 {
            t = t.plus(100 * MS);
            a.update(t, Usage::Normal, Some(200_000));
        }
        assert_eq!(a.rate_bps(), 2_000_000, "不升也不降");
        // 低载：保持
        a.update(t.plus(MS), Usage::Under, Some(200_000));
        assert_eq!(a.rate_bps(), 2_000_000);
        // 已确认速率高：可以升，但不超过 1.5 倍 + 10 kbps
        let mut b = Aimd::new(1_000_000, 100_000, 30_000_000);
        let mut t = Time(0);
        b.update(t, Usage::Normal, Some(1_000_000));
        for _ in 0..100 {
            t = t.plus(100 * MS);
            b.update(t, Usage::Normal, Some(1_000_000));
        }
        assert_eq!(b.rate_bps(), 1_510_000);
    }

    #[test]
    fn near_capacity_increases_additively() {
        let mut a = Aimd::new(5_000_000, 100_000, 30_000_000);
        a.set_rtt(50 * MS);
        let mut t = Time(0);
        a.update(t, Usage::Over, Some(5_000_000));
        let after = a.rate_bps();
        assert!(after < 5_000_000);
        // 知道容量：每秒只加几十 kbps
        a.update(t, Usage::Normal, Some(5_000_000));
        for _ in 0..10 {
            t = t.plus(100 * MS);
            a.update(t, Usage::Normal, Some(5_000_000));
        }
        let grown = a.rate_bps() - after;
        assert!((10_000..=200_000).contains(&grown), "{grown}");
        // 已确认速率远超容量上界：容量作废，回到乘性
        a.update(t.plus(100 * MS), Usage::Normal, Some(20_000_000));
        assert!(a.capacity_bps().is_none());
    }

    #[test]
    fn reduce_timing_and_limits() {
        let mut a = Aimd::new(4_000_000, 100_000, 30_000_000);
        a.set_rtt(0);
        a.set_rtt(50 * MS);
        assert!(a.time_to_reduce(Time(0), 4_000_000), "还没变过");
        a.update(Time(0), Usage::Over, Some(3_000_000));
        assert!(
            !a.time_to_reduce(Time(10 * MS), 3_000_000),
            "一个往返内不再降"
        );
        assert!(a.time_to_reduce(Time(60 * MS), 3_000_000));
        assert!(a.time_to_reduce(Time(10 * MS), 100_000), "送达掉到一半以下");
        // 降不到下限以下；探测结果直接设
        a.update(Time(100 * MS), Usage::Over, Some(1_000));
        assert_eq!(a.rate_bps(), 100_000);
        a.set_rate(Time(200 * MS), 99_000_000);
        assert_eq!(a.rate_bps(), 30_000_000);
        // 已确认速率比现在高时过载：按容量的 85%
        let mut c = Aimd::new(1_000_000, 100_000, 30_000_000);
        c.update(Time(0), Usage::Over, Some(1_000_000));
        let r = c.rate_bps();
        c.update(Time(300 * MS), Usage::Over, Some(5_000_000));
        assert!(c.rate_bps() <= r, "不会因为过载反而升");
    }
}
