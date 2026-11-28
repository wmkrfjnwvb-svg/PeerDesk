//! 限速器（traffic policing）：令牌桶，超过的包直接丢，不排队。对应 Linux `tc police`、微软仿真集里的 traffic policing。
//!
//! 令牌以“比特 × 10^6”为单位记（每微秒增加 `rate_bps`），全是整数，没有舍入误差。桶开始时是满的。
//! 包比桶深大时永远过不去（和真实的 policer 一样）。过不去的包不扣令牌。

use crate::Time;

/// 限速器参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policer {
    /// 放行速率（比特每秒）。
    pub rate_bps: u64,
    /// 桶深（字节）：桶满时一阵突发最多能过去这么多。
    pub burst_bytes: u64,
}

/// 限速器状态。
#[derive(Debug, Clone, Copy)]
pub struct PolicerState {
    /// 现有令牌，单位：比特 × 10^6。
    tokens: u128,
    last: Time,
}

impl PolicerState {
    /// 新桶，满的；`origin` 是开始计时的时刻。
    #[must_use]
    pub fn new(policer: &Policer, origin: Time) -> Self {
        Self {
            tokens: Self::capacity(policer),
            last: origin,
        }
    }

    fn capacity(policer: &Policer) -> u128 {
        u128::from(policer.burst_bytes) * 8 * 1_000_000
    }

    /// 时刻 `now` 到了一个 `size_bytes` 字节的包：放行返回 true，超出返回 false。
    pub fn admit(&mut self, policer: &Policer, now: Time, size_bytes: u64) -> bool {
        let dt = now.since(self.last);
        self.last = self.last.max(now);
        self.tokens = (self.tokens + u128::from(dt) * u128::from(policer.rate_bps))
            .min(Self::capacity(policer));
        let cost = u128::from(size_bytes) * 8 * 1_000_000;
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Policer = Policer {
        rate_bps: 800_000, // 100 kB/s
        burst_bytes: 5_000,
    };

    /// 桶满时一阵突发能过去桶深那么多，超出的全丢；歇一会儿令牌按速率补回来。
    #[test]
    fn a_full_bucket_passes_a_burst_then_drops_the_excess() {
        let mut s = PolicerState::new(&P, Time(0));
        // 1000 字节的包一次来 8 个：前 5 个过（5000 字节），后 3 个丢
        let passed: Vec<bool> = (0..8).map(|_| s.admit(&P, Time(0), 1000)).collect();
        assert_eq!(passed, [true, true, true, true, true, false, false, false]);
        // 丢掉的包不扣令牌：10 ms 补回 1000 字节，正好再过一个
        assert!(s.admit(&P, Time(10_000), 1000));
        assert!(!s.admit(&P, Time(10_000), 1000));
        // 桶满之后不会攒超过桶深：歇 10 秒也只能过 5 个
        let n = (0..8)
            .filter(|_| s.admit(&P, Time(10_000_000), 1000))
            .count();
        assert_eq!(n, 5);
        // 比桶深大的包永远过不去
        let mut s = PolicerState::new(&P, Time(0));
        assert!(!s.admit(&P, Time(100_000_000), 5_001));
        // 时钟倒退（不应发生）不补令牌也不 panic
        assert!(s.admit(&P, Time(0), 5_000));
    }

    /// 长期吞吐 ≤ 速率 + 桶深/时长：持续以 3 倍速率发，过去的字节数 = 速率 × 时长 + 桶深（误差一个包）。
    #[test]
    fn long_run_throughput_is_rate_plus_bucket() {
        let mut s = PolicerState::new(&P, Time(0));
        let mut passed = 0u64;
        // 300 kB/s 的恒速流（1000 字节一个，每 3333 微秒），约 10 秒
        let n = 3_000u64;
        for i in 0..n {
            if s.admit(&P, Time(i * 3_333), 1000) {
                passed += 1000;
            }
        }
        // 令牌总数 = 初始的桶深 + 速率 × 最后一个包的时刻
        let want = 100_000 * (n - 1) * 3_333 / 1_000_000 + 5_000;
        assert!(passed <= want, "过了 {passed} > {want}");
        assert!(passed + 1000 >= want, "过了 {passed} 远低于 {want}");
        // 低于速率的流不丢
        let mut s = PolicerState::new(&P, Time(0));
        assert!((0..1000).all(|i| s.admit(&P, Time(i * 20_000), 1000)));
    }
}
