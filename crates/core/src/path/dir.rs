//! 一个方向上的全部层：参数 [`DirConfig`] 和状态 [`DirState`]，以及一个包穿过各层的 [`DirState::process`]。

use crate::sim::Link;
use crate::Time;

use super::bottleneck::{Aqm, Bottleneck, BottleneckConfig, Capacity, QueueLimit, Verdict};
use super::delay::{Delay, DelayState, Noise, Receiver, Windows};
use super::loss::{Loss, LossState};
use super::policer::{Policer, PolicerState};
use super::rng::{layer, Dir, LayerRng};

/// 一个方向的层参数。每层默认关闭，默认值是“理想链路”：没有延迟、不丢、不限速。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirConfig {
    /// 第 2 层：限速器。
    pub policer: Option<Policer>,
    /// 第 3 层：瓶颈（容量、队列、CoDel）。
    pub bottleneck: BottleneckConfig,
    /// 第 4 层：丢包。
    pub loss: Loss,
    /// 第 5 层：传播延迟。
    pub delay: Delay,
    /// 第 6 层：延迟噪声。
    pub noise: Noise,
    /// 第 7 层：接收端。
    pub receiver: Receiver,
}

impl DirConfig {
    /// 把 `sim::Link` 的各个字段搬到对应的层上（A2-3 用它把布局 1 的参数转成布局 2）。
    /// 差别：丢包在瓶颈之后；`Link::up` 不搬（中断用 `Path::set_up` 或 `RouteChange`）；没有 CoDel、字节上限、限速器。
    #[must_use]
    pub fn from_link(link: &Link) -> Self {
        Self {
            policer: None,
            bottleneck: BottleneckConfig {
                // `Link` 的速率 `None` 和 `Some(0)` 都是不限速（`Net` 的约定）；这里显式转成 `Unlimited`。
                // 在布局 2 里速率 0 的意思是“一个包也放不出去”，和轨迹一致。
                capacity: match link.rate_bps {
                    Some(rate_bps) if rate_bps > 0 => Capacity::Constant { rate_bps },
                    Some(_) | None => Capacity::Unlimited,
                },
                limit: QueueLimit::delay(link.max_queue_us),
                aqm: Aqm::TailDrop,
            },
            loss: if link.loss_permille > 0 {
                Loss::Independent {
                    ppm: u32::from(link.loss_permille) * 1000,
                }
            } else {
                Loss::None
            },
            delay: Delay::Fixed(link.delay_us),
            noise: Noise {
                jitter_us: link.jitter_us,
                fifo_jitter: link.fifo_jitter,
                spikes: link.spikes,
                doze: link.doze,
            },
            receiver: Receiver {
                busy: link.busy,
                batch_us: link.batch_us,
            },
        }
    }
}

/// 一个包的结局。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 在这个时刻送达。
    Delivered(Time),
    /// 中断期间丢了。
    Outage,
    /// 被限速器丢了。
    Policed,
    /// 瓶颈队列超过上限，丢尾。
    TailDropped,
    /// CoDel 丢了。
    AqmDropped,
    /// 丢包层丢了（占过容量）。
    Lost,
}

/// 一个方向的状态。
#[derive(Debug, Clone)]
pub struct DirState {
    cfg: DirConfig,
    policer: Option<PolicerState>,
    bottleneck: Bottleneck,
    loss: LossState,
    delay: DelayState,
    spikes: Option<Windows>,
    busy: Option<Windows>,
    /// 同一批里已经有几个包：（批的时刻，个数）。批量收和接收端忙共用。
    batch: Option<(Time, u64)>,
    last_depart: Option<Time>,
    last_arrival: Option<Time>,
    // 每层一条随机数流
    rng_bottleneck: LayerRng,
    rng_loss: LayerRng,
    rng_delay: LayerRng,
    rng_jitter: LayerRng,
    rng_spikes: LayerRng,
    rng_doze: LayerRng,
    rng_busy: LayerRng,
}

impl DirState {
    /// 新状态。
    #[must_use]
    pub fn new(world_seed: u64, dir: Dir, cfg: DirConfig) -> Self {
        let stream = |name: &str| LayerRng::new(world_seed, dir, name);
        let mut s = Self {
            cfg: DirConfig::default(),
            policer: None,
            bottleneck: Bottleneck::new(BottleneckConfig::default(), Time::ZERO),
            loss: LossState::default(),
            delay: DelayState::default(),
            spikes: None,
            busy: None,
            batch: None,
            last_depart: None,
            last_arrival: None,
            rng_bottleneck: stream(layer::BOTTLENECK),
            rng_loss: stream(layer::LOSS),
            rng_delay: stream(layer::DELAY),
            rng_jitter: stream(layer::JITTER),
            rng_spikes: stream(layer::SPIKES),
            rng_doze: stream(layer::DOZE),
            rng_busy: stream(layer::BUSY),
        };
        s.reconfigure(cfg, Time::ZERO);
        s
    }

    /// 换参数（换路，或按计划换）：随机数流接着往下抽，不重新播种；
    /// 瓶颈里已经排着的包和链路什么时候空出来不变；
    /// 按时间的状态（尖峰和接收端忙的时间表、限速器的桶、周期性延迟）从 `origin` 重新开始。
    pub fn reconfigure(&mut self, cfg: DirConfig, origin: Time) {
        self.policer = cfg.policer.as_ref().map(|p| PolicerState::new(p, origin));
        self.bottleneck.reconfigure(cfg.bottleneck.clone(), origin);
        self.loss = LossState::default();
        self.delay = DelayState::default();
        self.spikes = cfg
            .noise
            .spikes
            .map(|s| Windows::new(origin, s.every_us, s.length_us, Some(s.max_us)));
        self.busy = cfg
            .receiver
            .busy
            .map(|b| Windows::new(origin, b.every_us, b.length_us, None));
        self.cfg = cfg;
    }

    /// 现在的参数。
    #[must_use]
    pub const fn config(&self) -> &DirConfig {
        &self.cfg
    }

    /// 瓶颈。
    #[must_use]
    pub const fn bottleneck(&self) -> &Bottleneck {
        &self.bottleneck
    }

    /// 交叉流量注入的入口：只经过瓶颈这一层（见 `Path::inject_cross`）。
    pub fn cross_arrive(&mut self, at: Time, size_bytes: u64) -> Verdict {
        self.bottleneck
            .arrive(&mut self.rng_bottleneck, at, size_bytes)
    }

    /// 测试用：某一层的随机数流。
    #[cfg(test)]
    pub(crate) fn stream_log(&self, name: &str) -> &[u64] {
        match name {
            layer::BOTTLENECK => &self.rng_bottleneck.log,
            layer::LOSS => &self.rng_loss.log,
            layer::DELAY => &self.rng_delay.log,
            layer::JITTER => &self.rng_jitter.log,
            layer::SPIKES => &self.rng_spikes.log,
            layer::DOZE => &self.rng_doze.log,
            layer::BUSY => &self.rng_busy.log,
            _ => &[],
        }
    }

    /// 各层随机数流各抽了多少个数：（层名，个数）。
    #[must_use]
    pub fn draws(&self) -> [(&'static str, u64); 7] {
        [
            (layer::BOTTLENECK, self.rng_bottleneck.drawn()),
            (layer::LOSS, self.rng_loss.drawn()),
            (layer::DELAY, self.rng_delay.drawn()),
            (layer::JITTER, self.rng_jitter.drawn()),
            (layer::SPIKES, self.rng_spikes.drawn()),
            (layer::DOZE, self.rng_doze.drawn()),
            (layer::BUSY, self.rng_busy.drawn()),
        ]
    }

    /// 时刻 `now` 发出一个 `size_bytes` 字节的包，`up` 表示这时候链路是通的。
    ///
    /// 每个包到每个开着的“按包抽数”的层（丢包、抖动、省电）各抽固定个数，包在前面的层被丢了也抽，
    /// 所以前面的层怎么改，后面的层抽到的数列都不变。
    pub fn process(&mut self, now: Time, size_bytes: u64, up: bool) -> Outcome {
        let loss_draw = self.cfg.loss.enabled().then(|| self.rng_loss.next_u64());
        let jitter = (self.cfg.noise.jitter_us > 0).then(|| {
            self.rng_jitter
                .below(self.cfg.noise.jitter_us.saturating_add(1))
        });
        let doze_wake = self
            .cfg
            .noise
            .doze
            .map(|d| d.wake_us / 2 + self.rng_doze.below(d.wake_us / 2 + 1));

        // 1 中断
        if !up {
            return Outcome::Outage;
        }
        // 2 限速器
        if let (Some(p), Some(state)) = (self.cfg.policer.as_ref(), self.policer.as_mut()) {
            if !state.admit(p, now, size_bytes) {
                return Outcome::Policed;
            }
        }
        // 3 瓶颈
        let depart = match self
            .bottleneck
            .arrive(&mut self.rng_bottleneck, now, size_bytes)
        {
            Verdict::Departs { done, .. } => done,
            Verdict::TailDropped => return Outcome::TailDropped,
            Verdict::AqmDropped => return Outcome::AqmDropped,
        };
        // 4 丢包（丢掉的包已经占过容量）
        if let Some(draw) = loss_draw {
            if self.loss.lost(self.cfg.loss, draw) {
                return Outcome::Lost;
            }
        }
        // 5 传播延迟
        let mut at = self
            .delay
            .arrival(&self.cfg.delay, &mut self.rng_delay, depart);
        // 6 延迟噪声：抖动、尖峰、省电
        at = at.plus(jitter.unwrap_or(0));
        if let Some(spikes) = self.spikes.as_mut() {
            at = at.plus(spikes.at(&mut self.rng_spikes, depart).unwrap_or(0));
        }
        if let (Some(doze), Some(wake)) = (self.cfg.noise.doze, doze_wake) {
            let asleep = self
                .last_depart
                .is_none_or(|t| depart.since(t) >= doze.idle_us);
            if asleep {
                at = at.plus(wake);
            }
            self.last_depart = Some(depart);
        }
        // 7 接收端：批量收、接收端忙
        if self.cfg.receiver.batch_us > 0 {
            // 同一批里的包被程序逐个读走，时间戳相差 1 微秒（见 `Link::batch_us`）
            let b = self.cfg.receiver.batch_us;
            let boundary = Time(at.micros().div_ceil(b).saturating_mul(b));
            at = self.in_batch(boundary);
        }
        if let Some(busy) = self.busy.as_mut() {
            if let Some(end) = busy_end(busy, &mut self.rng_busy, at) {
                at = self.in_batch(end);
            }
        }
        let c = &self.cfg;
        if c.noise.fifo_jitter
            || c.noise.spikes.is_some()
            || c.noise.doze.is_some()
            || c.receiver.busy.is_some()
        {
            if let Some(last) = self.last_arrival {
                at = at.max(last);
            }
            self.last_arrival = Some(at);
        }
        Outcome::Delivered(at)
    }

    /// 在 `boundary` 这一批里排下一个位置：每个包晚 1 微秒。
    fn in_batch(&mut self, boundary: Time) -> Time {
        let index = match self.batch {
            Some((b, n)) if b == boundary => n.saturating_add(1),
            Some(_) | None => 0,
        };
        self.batch = Some((boundary, index));
        boundary.plus(index)
    }
}

/// 接收端忙：`at` 落在忙的时段里就返回这段忙完的时刻。
fn busy_end(busy: &mut Windows, rng: &mut LayerRng, at: Time) -> Option<Time> {
    busy.at(rng, at).map(|_| busy.end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每层的流各自记录抽到的数；包过一层抽一个数（丢包、抖动、省电按包抽）。
    #[test]
    fn each_layer_logs_its_own_draws() {
        let cfg = DirConfig {
            loss: Loss::Independent { ppm: 1_000 },
            noise: Noise {
                jitter_us: 10,
                ..Noise::default()
            },
            ..DirConfig::default()
        };
        let mut d = DirState::new(1, Dir::Forward, cfg);
        for i in 0..4 {
            let _ = d.process(Time(i), 100, true);
        }
        assert_eq!(d.stream_log(layer::LOSS).len(), 4);
        assert_eq!(d.stream_log(layer::JITTER).len(), 4);
        for name in [
            layer::BOTTLENECK,
            layer::DELAY,
            layer::SPIKES,
            layer::DOZE,
            layer::BUSY,
            "没有这一层",
        ] {
            assert!(d.stream_log(name).is_empty(), "{name}");
        }
        assert_eq!(d.draws()[1], (layer::LOSS, 4));
    }
}
