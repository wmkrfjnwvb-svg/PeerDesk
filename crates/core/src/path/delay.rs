//! 传播延迟（含按计划的阶跃）、延迟噪声（抖动、尖峰、省电唤醒）、接收端（忙、批量收）。
//!
//! 尖峰、省电、接收端忙、批量收的语义照搬 `sim.rs` 里的 `Net`（那边的文档写了各自对应的用户日志）。
//! 和 `Net` 的差别：
//! - 随机数每层一条流，每个包固定抽固定个数（包被前面的层丢了也照抽），所以别的层开关不影响这里；
//! - 尖峰和接收端忙的“时间表”从时刻 0 起算，只由自己的流决定，和哪些包在什么时候来无关
//!   （`Net` 是从第一个包到达时起算）。

use crate::sim::{Busy, Doze, Spikes};
use crate::Time;

use super::rng::LayerRng;

/// 传播延迟。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delay {
    /// 固定单程延迟（微秒）。
    Fixed(u64),
    /// 按计划阶跃：`initial_us` 之后，到了 `steps` 里每一项的时刻就换成那一项的延迟。
    /// 新包用新延迟；已经在途的包不变；延迟变小时后发的包不会超过先发的（排在它后面到）。
    Steps {
        /// 第一个阶跃之前的延迟（微秒）。
        initial_us: u64,
        /// （生效时刻，延迟微秒）；不要求排序，同一时刻有多项时以后面的为准。
        steps: Vec<(Time, u64)>,
    },
    /// 周期性阶跃（Starlink 式的全球同步重配置）：从 `phase_us` 起每隔 `period_us` 换一次，
    /// 每个周期的延迟在 `[low_us, high_us]` 里均匀随机（用 `delay` 流，每个周期抽一次）。
    /// 第一次重配置之前用 `initial_us`。Starlink 的周期是 15 秒；相位要读到一手来源再定（`industry-network-sim.md` 第 7 条）。
    Periodic {
        /// 第一次重配置之前的延迟（微秒）。
        initial_us: u64,
        /// 周期（微秒），为 0 时不阶跃。
        period_us: u64,
        /// 第一次重配置的时刻（微秒）。
        phase_us: u64,
        /// 每个周期延迟的下限（微秒）。
        low_us: u64,
        /// 每个周期延迟的上限（微秒）。
        high_us: u64,
    },
}

impl Default for Delay {
    fn default() -> Self {
        Delay::Fixed(0)
    }
}

impl Delay {
    /// `t` 时刻名义上的单程延迟（微秒），不抽随机数：固定的就是它，按计划阶跃的是 `t` 时生效的那一项，
    /// 周期性的在第一次重配置之前是 `initial_us`、之后是上下限的中点（每个周期实际抽到多少只有包碰上才知道）。
    /// 给仿真主循环当“名义往返”的依据（和 `Net` 里取 `Link::delay_us` 一样），不影响任何一个包。
    #[must_use]
    pub fn nominal_us(&self, t: Time) -> u64 {
        match self {
            Delay::Fixed(d) => *d,
            Delay::Steps { initial_us, steps } => {
                steps
                    .iter()
                    .filter(|(at, _)| *at <= t)
                    .fold((None, *initial_us), |(best, cur), (at, d)| match best {
                        Some(b) if b > *at => (best, cur),
                        Some(_) | None => (Some(*at), *d),
                    })
                    .1
            }
            Delay::Periodic {
                initial_us,
                period_us,
                phase_us,
                low_us,
                high_us,
            } => {
                if *period_us == 0 || t.micros() < *phase_us {
                    *initial_us
                } else {
                    low_us.midpoint(*high_us)
                }
            }
        }
    }

    /// 会不会改变包的先后（需要保序）。
    const fn steps_over_time(&self) -> bool {
        match self {
            Delay::Fixed(_) => false,
            Delay::Steps { .. } | Delay::Periodic { .. } => true,
        }
    }
}

/// 传播延迟的状态：周期性阶跃已经抽到第几个周期，和上一个包的到达时刻（保序用）。
#[derive(Debug, Clone, Default)]
pub struct DelayState {
    /// 已经抽过的最后一个周期的编号和它的延迟。
    periodic: Option<(u64, u64)>,
    last_arrival: Time,
}

impl DelayState {
    /// `t` 时刻离开瓶颈的包的传播延迟（微秒），已经保序：返回的到达时刻不会早于上一个包。
    pub fn arrival(&mut self, delay: &Delay, rng: &mut LayerRng, t: Time) -> Time {
        let d = match delay {
            Delay::Fixed(_) | Delay::Steps { .. } => delay.nominal_us(t),
            Delay::Periodic {
                initial_us,
                period_us,
                phase_us,
                low_us,
                high_us,
            } => {
                if *period_us == 0 || t.micros() < *phase_us {
                    *initial_us
                } else {
                    let k = (t.micros() - *phase_us) / *period_us;
                    self.periodic_delay(rng, k, *low_us, *high_us)
                }
            }
        };
        let mut at = t.plus(d);
        if delay.steps_over_time() {
            at = at.max(self.last_arrival);
            self.last_arrival = at;
        }
        at
    }

    /// 第 `k` 个周期的延迟；中间跳过的周期也要抽（每个周期固定抽一个数，不依赖有没有包碰上它）。
    fn periodic_delay(&mut self, rng: &mut LayerRng, k: u64, low: u64, high: u64) -> u64 {
        let (mut done, mut cur) = match self.periodic {
            Some((n, d)) => (Some(n), d),
            None => (None, low),
        };
        let first = done.map_or(0, |n| n.saturating_add(1));
        let span = high.saturating_sub(low).saturating_add(1);
        for i in first..=k {
            cur = low.saturating_add(rng.below(span));
            done = Some(i);
        }
        self.periodic = done.map(|n| (n, cur));
        cur
    }
}

/// 延迟噪声（第 6 层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Noise {
    /// 均匀抖动的上限（微秒），0 表示没有。
    pub jitter_us: u64,
    /// 抖动不乱序（Wi-Fi 式，见 `Link::fifo_jitter`）。
    pub fifo_jitter: bool,
    /// 延迟尖峰。
    pub spikes: Option<Spikes>,
    /// 省电唤醒。
    pub doze: Option<Doze>,
    /// RFC 8868 的不乱序有界延迟变化（NR-BPDV，见 [`Pdv`]）。默认关。
    pub pdv: Option<Pdv>,
}

/// RFC 8868 第 4.5.2、4.5.3 节的“Approximately Random Subject to No-Reordering Bounded PDV”（NR-BPDV），
/// RFC 8867 第 4.2 节要求评估时用它。
///
/// - 每个包的额外延迟 z(n) = |max(min(N(0, std²), N_STD·std), −N_STD·std)|，原文推荐 std = 5 ms、N_STD = 3；
/// - 不乱序：j(n) 早于 j(n−1) + serial(n−1) 时改成 j(n−1) + serial(n−1)，serial 是上一个包按瓶颈速率的串行时间
///   （瓶颈不限速时为 0）。
///
/// 正态分布用整数表查（`pdv_table.rs`，N_STD 固定为 3），不用浮点函数，跨平台逐字节相同。表由
/// `statistics.NormalDist().inv_cdf((1 + k / 1024) / 2)` 算出，取 1025 个点，相邻两点之间线性插值。
///
/// 原文的一处矛盾：RFC 8867 第 4.2 节写“Maximum end-to-end jitter: 30 ms”，而 8868 的公式在推荐参数下最大是
/// 3 × 5 = 15 ms。这里照 8868 的公式（8867 自己说“Jitter models are described in [RFC8868]”）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pdv {
    /// 正态分布的标准差（微秒），原文推荐 5000。
    pub std_us: u64,
}

impl Pdv {
    /// RFC 8868 推荐的参数：std = 5 ms，N_STD = 3。
    pub const RFC8868: Pdv = Pdv { std_us: 5_000 };

    /// 由一个随机数得到这个包的额外延迟（微秒）。
    #[must_use]
    pub fn sample(&self, draw: u64) -> u64 {
        use super::pdv_table::{HALF_NORMAL_MICRO, STEPS};
        // 高 10 位选格，接下来 32 位在格里线性插值
        let cell = draw >> 54;
        let frac = (draw >> 22) & 0xFFFF_FFFF;
        let i = usize::try_from(cell.min(STEPS - 1)).unwrap_or(0);
        let lo = u64::from(HALF_NORMAL_MICRO.get(i).copied().unwrap_or(0));
        let hi = u64::from(HALF_NORMAL_MICRO.get(i + 1).copied().unwrap_or(0));
        let micro = lo + (((hi - lo) * frac) >> 32);
        let z = u128::from(micro) * u128::from(self.std_us) / 1_000_000;
        u64::try_from(z).unwrap_or(u64::MAX)
    }
}

/// 接收端（第 7 层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Receiver {
    /// 接收端偶尔忙一阵。
    pub busy: Option<Busy>,
    /// 一批一批地收（微秒，0 表示随到随收）。
    pub batch_us: u64,
}

/// 一串“时间窗”：平均每 `every_us` 来一次（间隔在 0 到 2 倍之间均匀），每次持续 `length_us`，
/// 可选地每次带一个“额外延迟”（在 `max/2` 到 `max` 之间）。尖峰和接收端忙都是它。
#[derive(Debug, Clone)]
pub struct Windows {
    every_us: u64,
    length_us: u64,
    extra_max_us: Option<u64>,
    /// 还没有抽过第一个间隔。
    fresh: bool,
    /// 下一个窗的开始。
    next: Time,
    start: Time,
    end: Time,
    extra: u64,
}

impl Windows {
    /// 新的时间表，从 `origin` 起算。`every_us` 为 0 时没有任何窗。
    #[must_use]
    pub const fn new(
        origin: Time,
        every_us: u64,
        length_us: u64,
        extra_max_us: Option<u64>,
    ) -> Self {
        Self {
            every_us,
            length_us,
            extra_max_us,
            fresh: true,
            next: origin,
            start: Time::ZERO,
            end: Time::ZERO,
            extra: 0,
        }
    }

    /// 时刻 `t` 落在窗里就返回这个窗的额外延迟（没有额外延迟的窗返回 0），不在窗里返回 `None`。
    /// `t` 大体上要往后走（早于当前窗开始的时刻按“不在窗里”处理）。
    pub fn at(&mut self, rng: &mut LayerRng, t: Time) -> Option<u64> {
        if self.every_us == 0 {
            return None;
        }
        if self.fresh {
            self.fresh = false;
            self.next = self.next.plus(rng.below(self.every_us.saturating_mul(2)));
        }
        while self.next <= t {
            self.start = self.next;
            self.end = self.start.plus(self.length_us);
            if let Some(max) = self.extra_max_us {
                self.extra = max / 2 + rng.below(max / 2 + 1);
            }
            self.next = self
                .next
                .plus(rng.below(self.every_us.saturating_mul(2)).max(1));
        }
        (t >= self.start && t < self.end).then_some(self.extra)
    }

    /// 当前（最近一次碰到的）窗的结束时刻。
    #[must_use]
    pub const fn end(&self) -> Time {
        self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::rng::{layer, Dir};

    fn rng(layer: &str) -> LayerRng {
        LayerRng::new(5, Dir::Forward, layer)
    }

    #[test]
    fn fixed_delay_adds_a_constant() {
        let mut s = DelayState::default();
        let mut r = rng(layer::DELAY);
        assert_eq!(s.arrival(&Delay::default(), &mut r, Time(10)), Time(10));
        assert_eq!(s.arrival(&Delay::Fixed(500), &mut r, Time(10)), Time(510));
        assert_eq!(r.drawn(), 0, "固定延迟不抽随机数");
    }

    /// 阶跃发生在计划的时刻；之前、之后的延迟各是各的值；同一时刻有多项以后面的为准；不要求排序。
    #[test]
    fn steps_take_effect_exactly_at_the_scheduled_time() {
        let delay = Delay::Steps {
            initial_us: 20_000,
            steps: vec![
                (Time(3_000_000), 90_000),
                (Time(1_000_000), 50_000),
                (Time(1_000_000), 60_000),
            ],
        };
        let mut s = DelayState::default();
        let mut r = rng(layer::DELAY);
        let at = |s: &mut DelayState, r: &mut LayerRng, t: u64| {
            s.arrival(&delay, r, Time(t)).micros() - t
        };
        assert_eq!(at(&mut s, &mut r, 0), 20_000);
        assert_eq!(at(&mut s, &mut r, 999_999), 20_000);
        assert_eq!(
            at(&mut s, &mut r, 1_000_000),
            60_000,
            "恰好在 1 s 生效，同一时刻以后面的为准"
        );
        assert_eq!(at(&mut s, &mut r, 2_999_999), 60_000);
        assert_eq!(at(&mut s, &mut r, 3_000_000), 90_000);
        assert_eq!(at(&mut s, &mut r, 9_000_000), 90_000);
    }

    /// 延迟变小：后发的包不会超过先发的，排在它后面到；变大则各自按新延迟。
    #[test]
    fn a_step_down_does_not_reorder() {
        let delay = Delay::Steps {
            initial_us: 50_000,
            steps: vec![(Time(10_000), 5_000)],
        };
        let mut s = DelayState::default();
        let mut r = rng(layer::DELAY);
        let arrivals: Vec<Time> = (0..20u64)
            .map(|i| s.arrival(&delay, &mut r, Time(i * 1_000)))
            .collect();
        assert!(arrivals.windows(2).all(|w| w[0] <= w[1]), "{arrivals:?}");
        assert_eq!(arrivals[9], Time(59_000));
        // 10 ms 之后的包本来 15 ms 就到，被挡到前一个包之后
        assert_eq!(arrivals[10], Time(59_000));
        assert_eq!(arrivals[19], Time(59_000));
    }

    /// 周期性阶跃：第一次重配置之前是初始值；之后每个周期换一次、周期内不变，边界精确；取值在范围内；
    /// 每个周期固定抽一次，跳过的周期也抽（所以结果和有没有包碰上它无关）。
    #[test]
    fn periodic_steps_change_once_per_period_on_the_boundary() {
        let delay = Delay::Periodic {
            initial_us: 30_000,
            period_us: 15_000_000,
            phase_us: 12_000_000,
            low_us: 25_000,
            high_us: 60_000,
        };
        let mut s = DelayState::default();
        let mut r = rng(layer::DELAY);
        // 每 100 ms 一个包，100 秒
        let ds: Vec<u64> = (0..1000u64)
            .map(|i| s.arrival(&delay, &mut r, Time(i * 100_000)).micros() - i * 100_000)
            .collect();
        assert!(ds[..120].iter().all(|d| *d == 30_000), "12 s 之前用初始值");
        // 从 12 s 起，每 150 个包（15 s）一个值
        let mut values = Vec::new();
        for k in 0..6usize {
            let lo = 120 + k * 150;
            let seg = &ds[lo..(lo + 150).min(1000)];
            assert!(seg.iter().all(|d| *d == seg[0]), "周期 {k} 内延迟要不变");
            assert!((25_000..=60_000).contains(&seg[0]));
            values.push(seg[0]);
        }
        assert!(
            values.windows(2).any(|w| w[0] != w[1]),
            "每个周期换一个值：{values:?}"
        );
        assert_eq!(r.drawn(), 6, "周期 0..=5 各抽一次");
        // 稀疏的包（每 40 s 一个）：跳过的周期也抽，所以同一个周期的值和密集查询时一样
        let mut s2 = DelayState::default();
        let mut r2 = rng(layer::DELAY);
        let sparse: Vec<u64> = [20u64, 60, 99]
            .iter()
            .map(|sec| {
                s2.arrival(&delay, &mut r2, Time(sec * 1_000_000)).micros() - sec * 1_000_000
            })
            .collect();
        assert_eq!(sparse, vec![ds[200], ds[600], ds[990]]);
        assert_eq!(r2.drawn(), 6, "跳过的周期也抽了");
        // 周期为 0 不阶跃
        let flat = Delay::Periodic {
            initial_us: 7,
            period_us: 0,
            phase_us: 0,
            low_us: 1,
            high_us: 2,
        };
        assert_eq!(
            DelayState::default().arrival(&flat, &mut rng(layer::DELAY), Time(100)),
            Time(107)
        );
    }

    fn windows_of(w: &mut Windows, r: &mut LayerRng, until: u64, step: u64) -> Vec<(u64, u64)> {
        // 返回每个窗（首次碰到的时刻，额外延迟）
        let mut out: Vec<(u64, u64)> = Vec::new();
        let mut prev = false;
        let mut t = 0;
        while t < until {
            let hit = w.at(r, Time(t));
            if let Some(e) = hit {
                if !prev {
                    out.push((t, e));
                }
            }
            prev = hit.is_some();
            t += step;
        }
        out
    }

    /// 时间窗：平均间隔 every，每个窗长 length，额外延迟在 max/2..max 之间；时间表只由流决定，和查询的粒度无关。
    #[test]
    fn windows_open_every_so_often_with_bounded_extra() {
        let mut w = Windows::new(Time::ZERO, 500_000, 40_000, Some(25_000));
        let mut r = rng(layer::SPIKES);
        let wins = windows_of(&mut w, &mut r, 60_000_000, 1_000);
        // 60 秒、平均 0.5 秒一次：约 120 个窗（平均间隔 0.5 s，标准差约 0.29 s，120 个间隔的总和标准差约 3.2 s）
        assert!((100..=140).contains(&wins.len()), "{}", wins.len());
        assert!(wins.iter().all(|(_, e)| (12_500..=25_000).contains(e)));
        // 换一个查询粒度，窗的位置和额外延迟都一样（只差粒度内的误差）
        let mut w2 = Windows::new(Time::ZERO, 500_000, 40_000, Some(25_000));
        let mut r2 = rng(layer::SPIKES);
        let wins2 = windows_of(&mut w2, &mut r2, 60_000_000, 7_000);
        assert!(wins2.len() <= wins.len());
        for (t, e) in &wins2 {
            assert!(
                wins.iter()
                    .any(|(t1, e1)| e1 == e && t1 <= t && t - t1 < 40_000),
                "{t} {e}"
            );
        }
        // 没有额外延迟的窗（接收端忙）：返回 0；every 为 0 时没有窗，也不抽数
        let mut busy = Windows::new(Time::ZERO, 300_000, 20_000, None);
        let mut rb = rng(layer::BUSY);
        assert!(windows_of(&mut busy, &mut rb, 5_000_000, 1_000)
            .iter()
            .all(|(_, e)| *e == 0));
        let mut none = Windows::new(Time::ZERO, 0, 20_000, None);
        let mut rn = rng(layer::BUSY);
        assert_eq!(none.at(&mut rn, Time(1_000_000)), None);
        assert_eq!(rn.drawn(), 0);
        // 早于当前窗开始的时刻不在窗里
        assert_eq!(busy.at(&mut rb, Time(0)), None);
    }
}
