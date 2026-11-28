//! 瓶颈：容量过程加队列。第 3 层，交叉流量（A2-2）在这里注入。
//!
//! - 容量：不限、匀速、泊松放行机会（Mahimahi / Pantheon 式）、轨迹（分段匀速，可重复）；
//! - 队列：按时间和（或）按字节计上限，丢尾；可选 CoDel（RFC 8289）；
//! - 内部时间用纳秒，避免每个包向下取整到微秒带来的容量误差；对外是微秒。
//!
//! “先到先服务”，每个包到达时就能算出它什么时候开始发、什么时候发完（`arrive`），不用逐毫秒推进。
//! 包到达的时刻必须不倒退（主流量用发送时刻，交叉流量见 [`Bottleneck::arrive`]）。

use std::collections::VecDeque;

use crate::Time;

use super::codel::Codel;
use super::rng::LayerRng;

const NS: u64 = 1_000;

/// 轨迹：分段匀速的容量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trace {
    /// （时长微秒，速率比特每秒）。速率 0 是一段时间完全不放行；时长 0 的段被跳过。
    pub segments: Vec<(u64, u64)>,
    /// 走完之后是否从头再来。不重复时，最后一段的速率一直保持下去
    /// （最后一段速率为 0 就是链路永远断了）。
    pub repeat: bool,
}

/// 容量过程。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Capacity {
    /// 不限速，不排队。
    #[default]
    Unlimited,
    /// 匀速：一个包要发 `字节×8/速率` 秒。速率 0 是一个包也放不出去（和轨迹里的速率 0 一样）；不限速用 `Unlimited`。
    Constant {
        /// 比特每秒。
        rate_bps: u64,
    },
    /// 泊松放行：放行机会（每次 `opportunity_bytes` 字节）的间隔服从指数分布，平均间隔使长期速率等于 `rate_bps`。
    /// 一个机会可以被几个小包分着用，用不完的部分在队列空时作废（Mahimahi 的语义）。速率 0 是一个包也放不出去；不限速用 `Unlimited`。
    Poisson {
        /// 比特每秒。
        rate_bps: u64,
        /// 每次机会放行的字节数，常用 1500（至少取 1）。
        opportunity_bytes: u64,
    },
    /// 轨迹。
    Trace(Trace),
}

/// 队列上限。两个都设就是任何一个超了都丢。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLimit {
    /// 一个包开始发之前要等的时间上限（微秒）：超过就丢（`Net` 的 `max_queue_us`）。
    pub max_delay_us: Option<u64>,
    /// 队列里的字节数上限（含正在发的包）：加上新包超过就丢。
    pub max_bytes: Option<u64>,
}

impl QueueLimit {
    /// 只按时间限制。
    #[must_use]
    pub const fn delay(us: u64) -> Self {
        Self {
            max_delay_us: Some(us),
            max_bytes: None,
        }
    }
    /// 只按字节限制。
    #[must_use]
    pub const fn bytes(n: u64) -> Self {
        Self {
            max_delay_us: None,
            max_bytes: Some(n),
        }
    }
}

/// 队列满之前的主动丢包策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Aqm {
    /// 不主动丢，只丢尾。
    #[default]
    TailDrop,
    /// CoDel。
    CoDel {
        /// 目标排队时长（微秒），RFC 默认 5 ms。
        target_us: u64,
        /// 窗口（微秒），RFC 默认 100 ms。
        interval_us: u64,
    },
}

impl Aqm {
    /// RFC 8289 默认参数的 CoDel。
    #[must_use]
    pub const fn codel() -> Self {
        Aqm::CoDel {
            target_us: Codel::DEFAULT_TARGET_US,
            interval_us: Codel::DEFAULT_INTERVAL_US,
        }
    }
}

/// 瓶颈的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BottleneckConfig {
    /// 容量过程。
    pub capacity: Capacity,
    /// 队列上限。
    pub limit: QueueLimit,
    /// 主动丢包策略。
    pub aqm: Aqm,
}

impl Default for BottleneckConfig {
    /// 不限速；排队上限 200 ms（和 `Link::ideal` 一样）。
    fn default() -> Self {
        Self {
            capacity: Capacity::Unlimited,
            limit: QueueLimit::delay(200_000),
            aqm: Aqm::TailDrop,
        }
    }
}

/// 一个包到瓶颈的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// 发出去了：开始发的时刻、发完（离开瓶颈）的时刻。
    Departs {
        /// 开始发。
        start: Time,
        /// 发完。
        done: Time,
    },
    /// 队列超过上限，丢尾。
    TailDropped,
    /// CoDel 在队首丢掉。
    AqmDropped,
}

/// 泊松放行的当前机会。
#[derive(Debug, Clone, Copy)]
struct Opportunity {
    at_ns: u64,
    left_bytes: u64,
}

/// 瓶颈的状态。
#[derive(Debug, Clone)]
pub struct Bottleneck {
    cfg: BottleneckConfig,
    /// 轨迹各段结束的累计时刻（纳秒，不含重复）。
    trace_ends_ns: Vec<u64>,
    /// 泊松放行计时的起点。
    origin_ns: u64,
    opp: Option<Opportunity>,
    /// 链路空出来的时刻：下一个包最早从这里开始发。
    cursor_ns: u64,
    /// 队列里的包：（离开队列的时刻，字节数）。发出去的包离开队列是发完的时候，被 CoDel 丢的是丢它的时候。
    queue: VecDeque<(u64, u64)>,
    queued_bytes: u64,
    codel: Option<Codel>,
    last_arrival_ns: u64,
}

/// 把 `bits` 比特按 `rate_bps` 发完要多少纳秒（向上取整）。
fn tx_ns(bits: u128, rate_bps: u64) -> u64 {
    let ns = (bits * 1_000_000_000).div_ceil(u128::from(rate_bps.max(1)));
    u64::try_from(ns).unwrap_or(u64::MAX)
}

impl Bottleneck {
    /// 新瓶颈，`origin` 是泊松放行开始计时的时刻。
    #[must_use]
    pub fn new(cfg: BottleneckConfig, origin: Time) -> Self {
        let mut b = Self {
            cfg: BottleneckConfig::default(),
            trace_ends_ns: Vec::new(),
            origin_ns: 0,
            opp: None,
            cursor_ns: 0,
            queue: VecDeque::new(),
            queued_bytes: 0,
            codel: None,
            last_arrival_ns: 0,
        };
        b.reconfigure(cfg, origin);
        b
    }

    /// 换参数（换路）：容量过程、队列上限、CoDel 都换成新的；
    /// 队里已经有的包和链路什么时候空出来不变（已经算好的送达不改）。
    pub fn reconfigure(&mut self, cfg: BottleneckConfig, origin: Time) {
        let mut acc = 0u64;
        self.trace_ends_ns = match &cfg.capacity {
            Capacity::Trace(t) => t
                .segments
                .iter()
                .map(|(us, _)| {
                    acc = acc.saturating_add(us.saturating_mul(NS));
                    acc
                })
                .collect(),
            Capacity::Unlimited | Capacity::Constant { .. } | Capacity::Poisson { .. } => {
                Vec::new()
            }
        };
        self.codel = match cfg.aqm {
            Aqm::TailDrop => None,
            Aqm::CoDel {
                target_us,
                interval_us,
            } => Some(Codel::new(target_us, interval_us)),
        };
        self.origin_ns = origin.micros().saturating_mul(NS);
        self.opp = None;
        self.cfg = cfg;
    }

    /// 现在的参数。
    #[must_use]
    pub const fn config(&self) -> &BottleneckConfig {
        &self.cfg
    }

    /// CoDel 的状态（没开 CoDel 时为 `None`），测试和诊断用。
    #[must_use]
    pub const fn codel(&self) -> Option<&Codel> {
        self.codel.as_ref()
    }

    /// 现在新来一个包，要等多久才轮到它（微秒）。
    #[must_use]
    pub fn backlog_us(&self, now: Time) -> u64 {
        self.cursor_ns
            .saturating_sub(now.micros().saturating_mul(NS))
            / NS
    }

    /// 队列里现在有多少字节（含正在发的）。
    #[must_use]
    pub const fn queued_bytes(&self) -> u64 {
        self.queued_bytes
    }

    /// 最后一个包到达的时刻（下一个包不能比它早）。
    #[must_use]
    pub const fn last_arrival(&self) -> Time {
        Time(self.last_arrival_ns / NS)
    }

    /// 一个 `size_bytes` 字节的包在 `at` 时刻到达瓶颈。
    ///
    /// 主流量由 `Path::send` 用发送时刻调用；交叉流量（A2-2）通过 `Path::inject_cross` 调用，
    /// 它占队列、占容量、受丢尾和 CoDel 约束，和主流量完全一样，只是不再经过后面的层。
    /// `at` 不能早于上一个包的到达时刻（早了，前面算好的包就得重算）。
    pub fn arrive(&mut self, rng: &mut LayerRng, at: Time, size_bytes: u64) -> Verdict {
        let at_ns = at.micros().saturating_mul(NS).max(self.last_arrival_ns);
        self.last_arrival_ns = at_ns;
        if matches!(self.cfg.capacity, Capacity::Unlimited) {
            let t = Time(at_ns / NS);
            return Verdict::Departs { start: t, done: t };
        }
        // 先把已经离开队列的包清掉
        while let Some((leave, size)) = self.queue.front().copied() {
            if leave > at_ns {
                break;
            }
            self.queued_bytes = self.queued_bytes.saturating_sub(size);
            let _ = self.queue.pop_front();
        }
        let start_ns = self.cursor_ns.max(at_ns);
        let wait_ns = start_ns - at_ns;
        let too_long = self
            .cfg
            .limit
            .max_delay_us
            .is_some_and(|m| wait_ns > m.saturating_mul(NS));
        let too_big = self
            .cfg
            .limit
            .max_bytes
            .is_some_and(|m| self.queued_bytes.saturating_add(size_bytes) > m);
        if too_long || too_big {
            return Verdict::TailDropped;
        }
        if let Some(codel) = self.codel.as_mut() {
            if codel.dequeue(start_ns / NS, wait_ns / NS) {
                self.queue.push_back((start_ns, size_bytes));
                self.queued_bytes = self.queued_bytes.saturating_add(size_bytes);
                return Verdict::AqmDropped;
            }
        }
        let done_ns = self.serve(rng, at_ns, start_ns, size_bytes);
        self.cursor_ns = done_ns;
        self.queue.push_back((done_ns, size_bytes));
        self.queued_bytes = self.queued_bytes.saturating_add(size_bytes);
        Verdict::Departs {
            start: Time(start_ns / NS),
            done: Time(done_ns / NS),
        }
    }

    /// 这个包发完的时刻（纳秒）。
    fn serve(&mut self, rng: &mut LayerRng, at_ns: u64, start_ns: u64, size_bytes: u64) -> u64 {
        let bits = u128::from(size_bytes) * 8;
        match &self.cfg.capacity {
            Capacity::Unlimited => start_ns,
            // 速率 0：永远发不完（送达时刻是饱和值），排队上限会把后面的包丢掉
            Capacity::Constant { rate_bps: 0 } | Capacity::Poisson { rate_bps: 0, .. } => u64::MAX,
            Capacity::Constant { rate_bps } => start_ns.saturating_add(tx_ns(bits, *rate_bps)),
            Capacity::Poisson {
                rate_bps,
                opportunity_bytes,
            } => {
                let opp_bytes = (*opportunity_bytes).max(1);
                let mean_ns = tx_ns(u128::from(opp_bytes) * 8, *rate_bps);
                self.serve_poisson(rng, at_ns, size_bytes, opp_bytes, mean_ns)
            }
            Capacity::Trace(trace) => self.serve_trace(trace, start_ns, bits),
        }
    }

    fn serve_poisson(
        &mut self,
        rng: &mut LayerRng,
        at_ns: u64,
        size: u64,
        opp_bytes: u64,
        mean_ns: u64,
    ) -> u64 {
        let origin = self.origin_ns;
        let mut opp = self.opp.unwrap_or_else(|| Opportunity {
            at_ns: origin.saturating_add(rng.exp_ns(mean_ns)),
            left_bytes: opp_bytes,
        });
        // 包到达之前已经过去的机会，队列是空的，作废
        while opp.at_ns < at_ns {
            opp = Opportunity {
                at_ns: opp.at_ns.saturating_add(rng.exp_ns(mean_ns)),
                left_bytes: opp_bytes,
            };
        }
        let mut need = size;
        loop {
            let take = opp.left_bytes.min(need);
            opp.left_bytes -= take;
            need -= take;
            if need == 0 {
                break;
            }
            opp = Opportunity {
                at_ns: opp.at_ns.saturating_add(rng.exp_ns(mean_ns)),
                left_bytes: opp_bytes,
            };
        }
        self.opp = Some(opp);
        opp.at_ns
    }

    /// 轨迹：从 `start_ns` 起按各段的速率发 `bits` 比特，返回发完的时刻。
    fn serve_trace(&self, trace: &Trace, start_ns: u64, bits: u128) -> u64 {
        let total = self.trace_ends_ns.last().copied().unwrap_or(0);
        if total == 0 {
            return start_ns; // 空轨迹：不限速
        }
        // 需要的“比特·纳秒”：一段里放行的量 = 速率 × 时长（纳秒）÷ 10^9 比特
        let mut need = bits * 1_000_000_000;
        let mut t = start_ns;
        loop {
            let (cycle_base, local) = if trace.repeat {
                (t - t % total, t % total)
            } else {
                (0, t)
            };
            let idx = self.trace_ends_ns.partition_point(|end| *end <= local);
            let (rate, seg_end) = match (trace.segments.get(idx), self.trace_ends_ns.get(idx)) {
                (Some((_, rate)), Some(end)) => (*rate, cycle_base.saturating_add(*end)),
                // 不重复且已经走完：最后一段的速率一直保持
                (Some(_) | None, Some(_) | None) => {
                    (trace.segments.last().map_or(0, |s| s.1), u64::MAX)
                }
            };
            if rate == 0 {
                if seg_end == u64::MAX {
                    return u64::MAX; // 永远断了
                }
                t = seg_end;
                continue;
            }
            let can = u128::from(rate) * u128::from(seg_end.saturating_sub(t));
            if can >= need {
                let dt = need.div_ceil(u128::from(rate));
                return t.saturating_add(u64::try_from(dt).unwrap_or(u64::MAX));
            }
            need -= can;
            t = seg_end;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::rng::{layer, Dir};

    /// `serve` 的 `Unlimited` 分支（`arrive` 在前面就拦下了不限速的情形，这里直接验证它的语义：不占时间）。
    #[test]
    fn serving_without_a_capacity_takes_no_time() {
        let mut b = Bottleneck::new(BottleneckConfig::default(), Time::ZERO);
        let mut rng = LayerRng::new(1, Dir::Forward, layer::BOTTLENECK);
        assert_eq!(b.serve(&mut rng, 5, 7, 100), 7);
        assert_eq!(
            b.arrive(&mut rng, Time(9), 100),
            Verdict::Departs {
                start: Time(9),
                done: Time(9)
            }
        );
    }
}
