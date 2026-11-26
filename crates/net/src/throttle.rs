//! 后台传输（剪贴板里的文件、图片）的限速：不能挤占画面和输入。
//!
//! 控制端往被控端传剪贴板内容，走的是控制端的上行。上行平时只有输入和反馈（几十 kbps），
//! 没有现成的带宽估计：媒体层的估计在被控端，量的是画面那个方向；QUIC 的窗口
//! （[`crate::safety`]）平时量到的是这几十 kbps，不是容量。所以这里自己探：
//!
//! - 从 [`START`] 开始，每 [`STEP`] 翻一倍（传输开始后约 1 秒到上限 [`MAX`]）；
//! - 往返时间比见过的最小值高出一截（[`inflated`]）说明瓶颈在排队，当前速率就是对容量的估计：
//!   **上限定为它的一半**，之后不再超过；还在排队就继续减半，直到 [`MIN`]；
//! - 估计 [`ESTIMATE_TTL`] 后过期（换了网络、别的流量走了），重新探。
//!
//! 做法取自 LEDBAT（RFC 6817）：后台流量看排队延迟，一涨就让路；这里更保守，不追着容量跑，
//! 只取一半。拿不到往返时间时（[`Throttle::on_rtt`] 没被调用）速率停在 [`START`]：
//! 1 Mbps 在常见的最慢上行（ADSL、弱 4G，1–2 Mbps）上也留出一半给输入和反馈，
//! 代价是 20 MB 要 160 秒。
//!
//! 写出去的量按速率摊开：一次最多写 [`Throttle::quota`]（约 20 ms 的量），不成批灌进去。
//!
//! 这些常数是按上面的道理定的，没有在真实的慢链路上测过（见 docs/research/clipboard-lazy.md）。

use std::time::Duration;

/// 起步速率（字节每秒）：1 Mbps。
pub const START: u64 = 125_000;
/// 速率上限（字节每秒）：32 Mbps，20 MB 约 5 秒；再快对用户没有差别，对画面的风险变大。
pub const MAX: u64 = 4_000_000;
/// 速率下限（字节每秒）：128 kbps，再慢传不完。
pub const MIN: u64 = 16_000;
/// 多久调整一次速率（往返时间很长时按两个往返）。
pub const STEP: Duration = Duration::from_millis(200);
/// 对容量的估计多久过期。
pub const ESTIMATE_TTL: Duration = Duration::from_secs(60);
/// 往返时间至少高出这么多才算排队（小于它的抖动不算）。
const QUEUE_MARGIN: Duration = Duration::from_millis(40);

/// 往返时间 `rtt` 相对最小值 `base` 是不是说明链路在排队：高出一倍且至少 40 ms。
#[must_use]
pub fn inflated(base: Duration, rtt: Duration) -> bool {
    rtt > base.saturating_add(base.max(QUEUE_MARGIN))
}

/// 限速器。时间用“从某个起点算起过了多久”表示（不依赖时钟，便于测试）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Throttle {
    /// 当前速率（字节每秒）。
    rate: u64,
    /// 速率不能超过它（估计容量的一半；没有估计时是 [`MAX`]）和估计的时刻。
    ceiling: u64,
    estimated_at: Option<Duration>,
    /// 这个时刻以前不能再写。
    next: Duration,
    /// 见过的最小往返时间。
    base: Option<Duration>,
    /// 上次调整速率的时刻，和那以后有没有写过。
    stepped: Duration,
    busy: bool,
}

impl Default for Throttle {
    fn default() -> Self {
        Self::new()
    }
}

impl Throttle {
    /// 创建：速率 [`START`]。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            rate: START,
            ceiling: MAX,
            estimated_at: None,
            next: Duration::ZERO,
            base: None,
            stepped: Duration::ZERO,
            busy: false,
        }
    }

    /// 当前速率（字节每秒）。
    #[must_use]
    pub const fn rate(&self) -> u64 {
        self.rate
    }

    /// 还要等多久才能写（可以写时为零）。
    #[must_use]
    pub const fn wait(&self, now: Duration) -> Duration {
        self.next.saturating_sub(now)
    }

    /// 一次最多写多少字节：约 20 ms 的量，至少一个包。
    #[must_use]
    pub fn quota(&self) -> usize {
        usize::try_from(self.rate / 50)
            .unwrap_or(usize::MAX)
            .max(1200)
    }

    /// 写出去了 `bytes` 字节。
    pub fn sent(&mut self, now: Duration, bytes: usize) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        let micros = bytes.saturating_mul(1_000_000) / self.rate.max(1);
        // 空了很久以后不补发（不攒额度）：从现在算起
        self.next = self.next.max(now) + Duration::from_micros(micros);
        self.busy = true;
    }

    /// 定时告诉它现在的往返时间（没有在传时也可以调，只更新最小值）。
    pub fn on_rtt(&mut self, now: Duration, rtt: Duration) {
        self.base = Some(self.base.map_or(rtt, |b| b.min(rtt)));
        let base = self.base.unwrap_or(rtt);
        if self
            .estimated_at
            .is_some_and(|at| now.saturating_sub(at) > ESTIMATE_TTL)
        {
            self.estimated_at = None;
            self.ceiling = MAX;
        }
        if !self.busy {
            // 没在传：下一次传输从头探（不超过还有效的估计）
            self.rate = START.min(self.ceiling);
            self.stepped = now;
            return;
        }
        if now.saturating_sub(self.stepped) < STEP.max(rtt.saturating_mul(2)) {
            return;
        }
        self.stepped = now;
        self.busy = false;
        if inflated(base, rtt) {
            self.ceiling = (self.rate / 2).max(MIN);
            self.estimated_at = Some(now);
            self.rate = self.ceiling;
        } else {
            self.rate = self.rate.saturating_mul(2).min(self.ceiling);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn writes_are_spread_out_at_the_current_rate() {
        let mut t = Throttle::default();
        assert_eq!(t.rate(), START);
        assert_eq!(t.quota(), 2500);
        assert_eq!(t.wait(ms(0)), Duration::ZERO);
        // 1 Mbps 下写 2500 字节：20 ms 以后才能再写
        t.sent(ms(0), 2500);
        assert_eq!(t.wait(ms(0)), ms(20));
        assert_eq!(t.wait(ms(5)), ms(15));
        assert_eq!(t.wait(ms(20)), Duration::ZERO);
        // 空了很久不攒额度：下一次写完照样要等
        t.sent(ms(5000), 2500);
        assert_eq!(t.wait(ms(5000)), ms(20));
    }

    #[test]
    fn the_rate_doubles_until_the_cap_while_the_path_is_clear() {
        let mut t = Throttle::new();
        let mut now = ms(0);
        let mut rates = Vec::new();
        for _ in 0..8 {
            t.sent(now, 1000);
            now += STEP;
            // 还不到调整的时候：不变
            t.on_rtt(now - ms(1), ms(10));
            t.on_rtt(now, ms(10));
            rates.push(t.rate());
        }
        assert_eq!(
            rates,
            vec![250_000, 500_000, 1_000_000, 2_000_000, MAX, MAX, MAX, MAX]
        );
        assert_eq!(t.quota(), 80_000);
        // 传完了（一个周期没写）：下次从头探
        t.on_rtt(now + STEP, ms(10));
        assert_eq!(t.rate(), START);
    }

    #[test]
    fn queueing_halves_the_rate_and_caps_it_at_half_the_estimate() {
        assert!(!inflated(ms(10), ms(45)));
        assert!(inflated(ms(10), ms(51)));
        assert!(!inflated(ms(100), ms(200)));
        assert!(inflated(ms(100), ms(201)));
        let mut t = Throttle::new();
        let mut now = ms(0);
        let step = |t: &mut Throttle, now: &mut Duration, rtt: Duration| {
            t.sent(*now, 1000);
            *now += STEP.max(rtt * 2);
            t.on_rtt(*now, rtt);
            t.rate()
        };
        assert_eq!(step(&mut t, &mut now, ms(30)), 250_000);
        assert_eq!(step(&mut t, &mut now, ms(30)), 500_000);
        // 500 kB/s 时开始排队：容量大约就是这么多，上限定在一半
        assert_eq!(step(&mut t, &mut now, ms(120)), 250_000);
        // 不排队了也不再超过一半
        assert_eq!(step(&mut t, &mut now, ms(30)), 250_000);
        assert_eq!(step(&mut t, &mut now, ms(30)), 250_000);
        // 还在排队：继续减半，直到下限
        assert_eq!(step(&mut t, &mut now, ms(150)), 125_000);
        for _ in 0..6 {
            step(&mut t, &mut now, ms(400));
        }
        assert_eq!(t.rate(), MIN);
        // 估计过期以后重新探
        now += ESTIMATE_TTL + ms(1);
        t.on_rtt(now, ms(30));
        assert_eq!(t.rate(), START);
        assert_eq!(step(&mut t, &mut now, ms(30)), 250_000);
        assert_eq!(step(&mut t, &mut now, ms(30)), 500_000);
        assert_eq!(step(&mut t, &mut now, ms(30)), 1_000_000);
    }
}
