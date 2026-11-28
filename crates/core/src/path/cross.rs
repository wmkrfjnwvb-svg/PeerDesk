//! 交叉流量：挂在一个方向的瓶颈上，和主流量抢同一个队列和容量（`a2-plan.md` 第 4 节）。
//!
//! 三种源（[`CrossSpec`]）：
//! - **定速背景流**：固定速率的 UDP，可以设开始、结束时刻和包长。只经过瓶颈，没有接收端（RFC 8867 5.1 用它制造容量变化）；
//! - **TCP 长连接**：一条包一级的 NewReno（[`super::tcp`]），发不完（RFC 8867 5.6、5.7；WebRTC `IsFairToTCP`）；
//! - **一阵一阵的短连接**：RFC 8868 第 5.1 节的短 TCP 模型。
//!   原文：“Each short TCP flow is modeled as a sequence of file downloads interleaved with idle periods”；
//!   “30 connections start simultaneously fetching small (30-50 KB) amounts of data, evenly distributed”；
//!   “The idle period between bursts of starting a group of TCP flows is typically derived from an exponential distribution
//!   with the mean value of 10 seconds”。所以：**“30”是一阵里同时开始的连接数**，每条连接取 30–50 KB（均匀分布），
//!   两阵之间的空闲时间服从均值 10 s 的指数分布。原文没有说空闲时间从哪里起算；这里从一阵里最后一条连接
//!   完成的时刻起算（开关交替，不会两阵叠在一起）。原文还说“有的短流从开的状态起步，有的从关的状态起步”：
//!   用 `start_on` 选；从关起步时第一次空闲也是指数分布（指数分布无记忆，等于从任意时刻起步）。
//!   RFC 里的“多条短流”做成多个源（每个源一组连接）。
//!
//! # 和路径的接法
//! 源挂在 [`super::Path`] 的一个方向 `dir` 上（TCP 的确认走 `dir.other()`，占反向的容量，也会丢）。
//! 路径在 `send`、`advance_to`、`pop`（时钟往前走的地方）开头把所有 `≤ now` 的源事件按时间顺序处理掉
//! （定时器、包的送达归并，同一时刻先处理送达），包到达瓶颈的先后因此就是时间的先后，满足瓶颈对到达顺序的要求
//! （`Bottleneck::arrive`）。`peek_time` 不需要：交叉流量不产生主流量的送达，主流量的送达在 `send` 时就定了，
//! 排在它后面到达瓶颈的交叉包影响不了它。
//!
//! # 随机数
//! 每个源一条流 `cross/<序号>`（方向是它挂的方向）：短连接的空闲时间和文件大小从这里抽。每条 TCP 连接的两个方向各有一条
//! 车道（[`super::Lane`]，名字 `cross/<序号>/<第几阵>/<第几条>`），丢包和周期性延迟的随机数从车道的流里抽，不动主流量的流。
//! 交叉包不经过限速器、延迟噪声、接收端三层。
//!
//! # 更像真实的交叉流（[`CrossOptions`]，A3-1）
//! A2-4b 里两条同 RTT 的 TCP 吞吐比收敛到 0.9995–1.0005：丢尾队列让两条流同时丢包，AIMD 在同步丢包下窗口收敛到相等。
//! 真实的 TCP 随机得多（开始时刻、确认的时机、往返各不相同；主会话审 A2-4b 时提出）。三项，默认都是 0（和以前完全一样）：
//! - `start_jitter_us`：开始时刻在 [start, start + 这么久] 里均匀抽（随机数流 `cross/<序号>/start`，只抽一次）；
//! - `ack_jitter_us`：每个确认多一个 [0, 这么久] 的均匀延迟，同一条连接的确认不乱序（每条连接一条流
//!   `cross/<序号>/<第几阵>/<第几条>/ack-jitter`）；
//! - `extra_one_way_us`：这条流在共享瓶颈之外多走的单程延迟（到服务器的那一段），数据和确认各加一次。补 A2-3 第二步记下的限制：
//!   以前交叉流的往返只有挂的那一段自己的传播延迟，比真实的短、比真实的“凶”。
//!
//! 默认值不抽任何随机数，所以不用这些选项的源，抽到的数和以前逐个相同。

use std::collections::BTreeMap;

use crate::Time;

use super::dir::{DirState, Lane, Transit};
use super::rng::{Dir, LayerRng};
use super::tcp::{SackBlocks, TcpConfig, TcpReceiver, TcpSender, TcpStats};
use super::Verdict;

/// 交叉流更像真实的几项，见模块文档。默认都是 0：和以前完全一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CrossOptions {
    /// 开始时刻的随机推迟上限（微秒）。
    pub start_jitter_us: u64,
    /// 每个确认的随机延迟上限（微秒），同一条连接不乱序。
    pub ack_jitter_us: u64,
    /// 共享瓶颈之外多走的单程延迟（微秒），数据和确认各加一次。
    pub extra_one_way_us: u64,
}

/// 交叉流量的种类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrossSpec {
    /// 定速背景流。
    Cbr {
        /// 速率（比特每秒），0 是不发。
        rate_bps: u64,
        /// 包长（字节）。
        packet_bytes: usize,
        /// 开始的时刻。
        start: Time,
        /// 结束的时刻（不含），`None` 是一直发。
        stop: Option<Time>,
    },
    /// 一条 TCP 连接。
    Tcp {
        /// TCP 参数。
        cfg: TcpConfig,
        /// 开始的时刻。
        start: Time,
        /// 要发多少字节；`None` 是发不完。
        bytes: Option<u64>,
    },
    /// 一阵一阵的短连接（RFC 8868 第 5.1 节）。
    Bursts {
        /// TCP 参数。
        cfg: TcpConfig,
        /// 一阵里同时开始的连接数（RFC 8868：30）。
        connections: u32,
        /// 每条连接取的字节数下限（RFC 8868：30 KB）。
        min_bytes: u64,
        /// 上限（RFC 8868：50 KB）。
        max_bytes: u64,
        /// 两阵之间空闲时间的均值（微秒，RFC 8868：10 s），指数分布。
        mean_off_us: u64,
        /// 一开始就是开的状态（第一阵在 `start` 开始）；否则先空闲一段指数分布的时间。
        start_on: bool,
        /// 开始的时刻。
        start: Time,
    },
    /// 按帧发的定速流：视频通话（Zoom、Teams 一类，不是我们的码率控制）。每 `1/fps` 秒一帧，
    /// 一帧 `rate_bps / fps` 的数据切成 `packet_bytes` 的包，按 `rate_bps × pace_permille / 1000` 的速率排开发出
    /// （WebRTC 节奏器默认是媒体码率的 2.5 倍，即 2500；0 是整帧同时进瓶颈）。和 [`Self::Cbr`] 的平均速率相同，
    /// 区别只在帧周期的突发：多路共享窄瓶颈时，别人的周期性突发会被我们的排队判断采到（a3-plan 第 11 节）。
    Frames {
        /// 平均速率（比特每秒），0 是不发。
        rate_bps: u64,
        /// 帧率。
        fps: u32,
        /// 包长（字节）。
        packet_bytes: usize,
        /// 一帧内排开的速率是平均速率的多少千分比；0 是不排开。
        pace_permille: u32,
        /// 开始的时刻（第一帧）。
        start: Time,
        /// 结束的时刻（不含），`None` 是一直发。
        stop: Option<Time>,
    },
}

/// 一个源的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CrossStats {
    /// 注入的包数（数据包，TCP 的确认不算）。
    pub sent_packets: u64,
    /// 注入的字节数。
    pub sent_bytes: u64,
    /// 其中被瓶颈丢掉的（丢尾或 CoDel）。
    pub queue_dropped: u64,
    /// 其中被丢包层丢掉的。
    pub lost: u64,
    /// 这些包在瓶颈里等待的时间之和（微秒，只算没被瓶颈丢掉的）。
    pub wait_sum_us: u64,
    /// 上面那个和对应的包数。
    pub wait_count: u64,
    /// TCP：接收端按顺序收到的字节数（goodput）。
    pub delivered_bytes: u64,
    /// TCP 确认包被瓶颈丢掉的个数（反向）。
    pub ack_queue_dropped: u64,
    /// TCP 确认包被丢包层丢掉的个数。
    pub ack_lost: u64,
    /// TCP 发送端的计数（所有连接合计）。
    pub tcp: TcpStats,
}

impl CrossStats {
    /// 平均在瓶颈里等了多久（微秒）。
    #[must_use]
    pub fn mean_wait_us(&self) -> f64 {
        self.wait_sum_us as f64 / self.wait_count.max(1) as f64
    }
}

/// 一阵短连接的记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BurstRecord {
    /// 这一阵之前空闲了多久（微秒）；第一阵从开的状态起步时为 0。
    pub off_us: u64,
    /// 开始的时刻。
    pub start: Time,
    /// 最后一条连接完成的时刻；还没完成为 `None`。
    pub end: Option<Time>,
    /// 每条连接取的字节数。
    pub sizes: Vec<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    Data(u64),
    /// 累计确认和 SACK 块。
    Ack(u64, SackBlocks),
}

/// 在途的交叉包（已经算好什么时候到）。
#[derive(Debug, Clone, Copy)]
pub struct Item {
    src: usize,
    generation: u64,
    flow: usize,
    kind: ItemKind,
}

/// 处理事件时需要的路径内部状态。
pub struct Ctx<'a> {
    pub dirs: &'a mut [DirState; 2],
    pub queue: &'a mut BTreeMap<(Time, u64), Item>,
    pub next_id: &'a mut u64,
    pub seed: u64,
}

impl Ctx<'_> {
    fn dir(&mut self, dir: Dir) -> &mut DirState {
        let [f, r] = &mut *self.dirs;
        match dir {
            Dir::Forward => f,
            Dir::Reverse => r,
        }
    }

    fn push(&mut self, at: Time, item: Item) {
        let id = *self.next_id;
        *self.next_id = id.saturating_add(1);
        self.queue.insert((at, id), item);
    }
}

#[derive(Debug, Clone)]
struct Flow {
    snd: TcpSender,
    rcv: TcpReceiver,
    data_lane: Lane,
    ack_lane: Lane,
    finished: bool,
    /// 确认抖动：自己的随机数流和上一个确认的到达时刻（`ack_jitter_us` 为 0 时没有）。
    ack_jitter: Option<(LayerRng, Time)>,
}

#[derive(Debug, Clone)]
enum Kind {
    Cbr {
        interval_ns: u64,
        packet_bytes: u64,
        next_ns: Option<u64>,
        stop_ns: Option<u64>,
    },
    Frames {
        frame_ns: u64,
        /// 一帧几个包（最后一个是余下的字节）。
        per_frame: u64,
        /// 一帧多少字节。
        frame_bytes: u64,
        /// 一帧里相邻两个包相隔多久（不排开时为 0）。
        gap_ns: u64,
        packet_bytes: u64,
        /// 这一帧开始的时刻。
        frame_ns_at: u64,
        /// 这一帧已经发了几个包。
        sent: u64,
        next_ns: Option<u64>,
        stop_ns: Option<u64>,
    },
    Tcp {
        cfg: TcpConfig,
        bytes: Option<u64>,
        /// 还没开始时的开始时刻。
        start: Option<Time>,
        flows: Vec<Flow>,
    },
    Bursts {
        cfg: TcpConfig,
        connections: u32,
        min_bytes: u64,
        max_bytes: u64,
        mean_off_ns: u64,
        /// 下一阵开始的时刻（这一阵进行中时为 `None`）。
        next_burst: Option<Time>,
        /// 下一阵之前的空闲时长（记录用）。
        pending_off_us: u64,
        flows: Vec<Flow>,
        log: Vec<BurstRecord>,
    },
}

/// 一个交叉流量源。
#[derive(Debug, Clone)]
pub struct Source {
    dir: Dir,
    opts: CrossOptions,
    rng: LayerRng,
    stats: CrossStats,
    /// 已经结束的连接的 TCP 计数。
    tcp_done: TcpStats,
    generation: u64,
    kind: Kind,
}

fn add_tcp(a: TcpStats, b: TcpStats) -> TcpStats {
    TcpStats {
        sent: a.sent + b.sent,
        retransmits: a.retransmits + b.retransmits,
        fast_retransmits: a.fast_retransmits + b.fast_retransmits,
        timeouts: a.timeouts + b.timeouts,
    }
}

impl Source {
    /// 新源。`index` 是它在路径里的序号；`now` 之前的开始时刻按 `now` 算；`opts` 见 [`CrossOptions`]。
    #[must_use]
    pub fn with_options(
        seed: u64,
        index: usize,
        dir: Dir,
        spec: CrossSpec,
        opts: CrossOptions,
        now: Time,
    ) -> Self {
        let mut rng = LayerRng::new(seed, dir, &format!("cross/{index}"));
        // 开始时刻的随机推迟：自己一条流，不动 `rng`（没有推迟时一个数也不抽）
        let delay = if opts.start_jitter_us > 0 {
            LayerRng::new(seed, dir, &format!("cross/{index}/start"))
                .below(opts.start_jitter_us.saturating_add(1))
        } else {
            0
        };
        let spec = match spec {
            CrossSpec::Cbr {
                rate_bps,
                packet_bytes,
                start,
                stop,
            } => CrossSpec::Cbr {
                rate_bps,
                packet_bytes,
                start: start.plus(delay),
                stop,
            },
            CrossSpec::Frames {
                rate_bps,
                fps,
                packet_bytes,
                pace_permille,
                start,
                stop,
            } => CrossSpec::Frames {
                rate_bps,
                fps,
                packet_bytes,
                pace_permille,
                start: start.plus(delay),
                stop,
            },
            CrossSpec::Tcp { cfg, start, bytes } => CrossSpec::Tcp {
                cfg,
                start: start.plus(delay),
                bytes,
            },
            CrossSpec::Bursts {
                cfg,
                connections,
                min_bytes,
                max_bytes,
                mean_off_us,
                start_on,
                start,
            } => CrossSpec::Bursts {
                cfg,
                connections,
                min_bytes,
                max_bytes,
                mean_off_us,
                start_on,
                start: start.plus(delay),
            },
        };
        let kind = match spec {
            CrossSpec::Cbr {
                rate_bps,
                packet_bytes,
                start,
                stop,
            } => {
                let packet_bytes = u64::try_from(packet_bytes).unwrap_or(u64::MAX);
                let interval_ns = if rate_bps == 0 {
                    0
                } else {
                    (u128::from(packet_bytes) * 8 * 1_000_000_000 / u128::from(rate_bps))
                        .try_into()
                        .unwrap_or(u64::MAX)
                        .max(1)
                };
                let start_ns = start.max(now).micros().saturating_mul(1000);
                Kind::Cbr {
                    interval_ns,
                    packet_bytes,
                    next_ns: (rate_bps > 0).then_some(start_ns),
                    stop_ns: stop.map(|t| t.micros().saturating_mul(1000)),
                }
            }
            CrossSpec::Frames {
                rate_bps,
                fps,
                packet_bytes,
                pace_permille,
                start,
                stop,
            } => {
                let packet = u64::try_from(packet_bytes).unwrap_or(u64::MAX).max(1);
                let frame_ns = 1_000_000_000 / u64::from(fps.max(1));
                // 一帧的字节 = 速率 × 帧周期 / 8（至少 1 字节），切成整包加余下的一个
                let frame_bytes =
                    u64::try_from(u128::from(rate_bps) * u128::from(frame_ns) / 8_000_000_000)
                        .unwrap_or(u64::MAX)
                        .max(1);
                let per_frame = frame_bytes.div_ceil(packet);
                let pace_bps = u128::from(rate_bps) * u128::from(pace_permille) / 1000;
                // 不排开（速率 0）时间隔为 0
                let gap_ns = (u128::from(packet) * 8_000_000_000)
                    .checked_div(pace_bps)
                    .map_or(0, |g| u64::try_from(g).unwrap_or(u64::MAX));
                let start_ns = start.max(now).micros().saturating_mul(1000);
                Kind::Frames {
                    frame_ns,
                    per_frame,
                    frame_bytes,
                    gap_ns,
                    packet_bytes: packet,
                    frame_ns_at: start_ns,
                    sent: 0,
                    next_ns: (rate_bps > 0).then_some(start_ns),
                    stop_ns: stop.map(|t| t.micros().saturating_mul(1000)),
                }
            }
            CrossSpec::Tcp { cfg, start, bytes } => Kind::Tcp {
                cfg,
                bytes,
                start: Some(start.max(now)),
                flows: Vec::new(),
            },
            CrossSpec::Bursts {
                cfg,
                connections,
                min_bytes,
                max_bytes,
                mean_off_us,
                start_on,
                start,
            } => {
                let mean_off_ns = mean_off_us.saturating_mul(1000);
                let start = start.max(now);
                let (first, off_us) = if start_on {
                    (start, 0)
                } else {
                    let off = rng.exp_ns(mean_off_ns) / 1000;
                    (start.plus(off), off)
                };
                Kind::Bursts {
                    cfg,
                    connections,
                    min_bytes,
                    max_bytes: max_bytes.max(min_bytes),
                    mean_off_ns,
                    next_burst: Some(first),
                    pending_off_us: off_us,
                    flows: Vec::new(),
                    log: Vec::new(),
                }
            }
        };
        Self {
            dir,
            opts,
            rng,
            stats: CrossStats::default(),
            tcp_done: TcpStats::default(),
            generation: 0,
            kind,
        }
    }

    /// 计数（TCP 的计数含正在跑的连接）。
    #[must_use]
    pub fn stats(&self) -> CrossStats {
        let mut s = self.stats;
        let live: &[Flow] = match &self.kind {
            Kind::Cbr { .. } | Kind::Frames { .. } => &[],
            Kind::Tcp { flows, .. } | Kind::Bursts { flows, .. } => flows,
        };
        s.tcp = live
            .iter()
            .fold(self.tcp_done, |acc, f| add_tcp(acc, f.snd.stats()));
        s
    }

    /// 短连接的记录（别的种类为空）。
    #[must_use]
    pub fn bursts(&self) -> &[BurstRecord] {
        match &self.kind {
            Kind::Bursts { log, .. } => log,
            Kind::Cbr { .. } | Kind::Frames { .. } | Kind::Tcp { .. } => &[],
        }
    }

    /// 第一条（单条 TCP 时就是它）连接的发送端，看窗口、RTO 用。
    #[must_use]
    pub fn sender(&self) -> Option<&TcpSender> {
        match &self.kind {
            Kind::Tcp { flows, .. } | Kind::Bursts { flows, .. } => flows.first().map(|f| &f.snd),
            Kind::Cbr { .. } | Kind::Frames { .. } => None,
        }
    }

    /// 下一个定时事件的时刻。
    #[must_use]
    pub fn next_time(&self) -> Option<Time> {
        match &self.kind {
            Kind::Cbr { next_ns, .. } | Kind::Frames { next_ns, .. } => {
                next_ns.map(|n| Time(n / 1000))
            }
            Kind::Tcp { start, flows, .. } => {
                start.or_else(|| flows.iter().filter_map(|f| f.snd.next_timer()).min())
            }
            Kind::Bursts {
                next_burst, flows, ..
            } => next_burst.or_else(|| flows.iter().filter_map(|f| f.snd.next_timer()).min()),
        }
    }

    /// 处理 `t` 时刻到期的定时事件。
    pub fn fire(&mut self, ctx: &mut Ctx<'_>, index: usize, t: Time) {
        let dir = self.dir;
        match &mut self.kind {
            Kind::Cbr {
                interval_ns,
                packet_bytes,
                next_ns,
                stop_ns,
            } => {
                let Some(n) = *next_ns else { return };
                let verdict = ctx.dir(dir).cross_arrive(Time(n / 1000), *packet_bytes);
                self.stats.sent_packets += 1;
                self.stats.sent_bytes += *packet_bytes;
                match verdict {
                    Verdict::Departs { start, .. } => {
                        self.stats.wait_sum_us += start.since(Time(n / 1000));
                        self.stats.wait_count += 1;
                    }
                    Verdict::TailDropped | Verdict::AqmDropped => self.stats.queue_dropped += 1,
                }
                let next = n.saturating_add(*interval_ns);
                *next_ns = stop_ns.is_none_or(|s| next < s).then_some(next);
            }
            Kind::Frames {
                frame_ns,
                per_frame,
                frame_bytes,
                gap_ns,
                packet_bytes,
                frame_ns_at,
                sent,
                next_ns,
                stop_ns,
            } => {
                let Some(n) = *next_ns else { return };
                let size = (*packet_bytes).min(frame_bytes.saturating_sub(*sent * *packet_bytes));
                let verdict = ctx.dir(dir).cross_arrive(Time(n / 1000), size);
                self.stats.sent_packets += 1;
                self.stats.sent_bytes += size;
                match verdict {
                    Verdict::Departs { start, .. } => {
                        self.stats.wait_sum_us += start.since(Time(n / 1000));
                        self.stats.wait_count += 1;
                    }
                    Verdict::TailDropped | Verdict::AqmDropped => self.stats.queue_dropped += 1,
                }
                *sent += 1;
                // 这一帧还有包：隔 gap 再发（排开的速率不够在一帧里发完时，下一帧照样按帧时钟开始）；
                // 发完了：下一帧按帧时钟开始
                let next_frame = frame_ns_at.saturating_add(*frame_ns);
                let next = if *sent < *per_frame && n.saturating_add(*gap_ns) < next_frame {
                    n.saturating_add(*gap_ns)
                } else {
                    *frame_ns_at = next_frame;
                    *sent = 0;
                    next_frame
                };
                *next_ns = stop_ns.is_none_or(|s| next < s).then_some(next);
            }
            Kind::Tcp {
                cfg,
                bytes,
                start,
                flows,
            } => {
                if start.take().is_some() {
                    let segs = bytes.map(|b| b.div_ceil(cfg.mss_bytes as u64).max(1));
                    let mut flow = new_flow(
                        ctx.seed,
                        dir,
                        index,
                        self.generation,
                        0,
                        *cfg,
                        segs,
                        self.opts.ack_jitter_us > 0,
                    );
                    let mut out = Vec::new();
                    flow.snd.start(t, &mut out);
                    send_data(
                        ctx,
                        dir,
                        &mut self.stats,
                        &mut flow,
                        index,
                        self.generation,
                        0,
                        t,
                        &out,
                        self.opts.extra_one_way_us,
                    );
                    flows.push(flow);
                } else {
                    timeouts(
                        ctx,
                        dir,
                        &mut self.stats,
                        flows,
                        index,
                        self.generation,
                        t,
                        self.opts.extra_one_way_us,
                    );
                }
            }
            Kind::Bursts {
                cfg,
                connections,
                min_bytes,
                max_bytes,
                next_burst,
                pending_off_us,
                flows,
                log,
                ..
            } => {
                if next_burst.take().is_some() {
                    self.tcp_done = flows
                        .iter()
                        .fold(self.tcp_done, |acc, f| add_tcp(acc, f.snd.stats()));
                    flows.clear();
                    self.generation += 1;
                    let mut sizes = Vec::new();
                    for k in 0..*connections {
                        let size = *min_bytes + self.rng.below(*max_bytes - *min_bytes + 1);
                        sizes.push(size);
                        let segs = Some(size.div_ceil(cfg.mss_bytes as u64).max(1));
                        let mut flow = new_flow(
                            ctx.seed,
                            dir,
                            index,
                            self.generation,
                            k,
                            *cfg,
                            segs,
                            self.opts.ack_jitter_us > 0,
                        );
                        let mut out = Vec::new();
                        flow.snd.start(t, &mut out);
                        let ki = usize::try_from(k).unwrap_or(usize::MAX);
                        send_data(
                            ctx,
                            dir,
                            &mut self.stats,
                            &mut flow,
                            index,
                            self.generation,
                            ki,
                            t,
                            &out,
                            self.opts.extra_one_way_us,
                        );
                        flows.push(flow);
                    }
                    log.push(BurstRecord {
                        off_us: *pending_off_us,
                        start: t,
                        end: None,
                        sizes,
                    });
                } else {
                    timeouts(
                        ctx,
                        dir,
                        &mut self.stats,
                        flows,
                        index,
                        self.generation,
                        t,
                        self.opts.extra_one_way_us,
                    );
                }
            }
        }
    }

    /// 处理 `t` 时刻送达的交叉包。
    pub fn deliver(&mut self, ctx: &mut Ctx<'_>, index: usize, t: Time, item: Item) {
        let dir = self.dir;
        let (cfg, flows) = match &mut self.kind {
            Kind::Cbr { .. } | Kind::Frames { .. } => return,
            Kind::Tcp { cfg, flows, .. } | Kind::Bursts { cfg, flows, .. } => (*cfg, flows),
        };
        if item.generation != self.generation {
            return; // 上一阵的连接，已经结束了
        }
        let Some(flow) = flows.get_mut(item.flow) else {
            return;
        };
        match item.kind {
            ItemKind::Data(seq) => {
                let before = flow.rcv.delivered();
                let ack = flow.rcv.on_segment(seq);
                let blocks = flow.rcv.sack_blocks(seq);
                self.stats.delivered_bytes +=
                    (flow.rcv.delivered() - before) * cfg.mss_bytes as u64;
                let reverse = dir.other();
                match ctx
                    .dir(reverse)
                    .cross_transit(&mut flow.ack_lane, t, cfg.ack_bytes as u64)
                {
                    Transit::Arrives { at, .. } => {
                        let mut at = at.plus(self.opts.extra_one_way_us);
                        if let Some((rng, last)) = flow.ack_jitter.as_mut() {
                            at = at.plus(rng.below(self.opts.ack_jitter_us.saturating_add(1)));
                            at = at.max(*last);
                            *last = at;
                        }
                        let it = Item {
                            src: item.src,
                            generation: item.generation,
                            flow: item.flow,
                            kind: ItemKind::Ack(ack, blocks),
                        };
                        ctx.push(at, it);
                    }
                    Transit::QueueDropped => self.stats.ack_queue_dropped += 1,
                    Transit::Lost => self.stats.ack_lost += 1,
                }
            }
            ItemKind::Ack(ack, blocks) => {
                let mut out = Vec::new();
                flow.snd.on_ack_sack(t, ack, blocks, &mut out);
                send_data(
                    ctx,
                    dir,
                    &mut self.stats,
                    flow,
                    index,
                    self.generation,
                    item.flow,
                    t,
                    &out,
                    self.opts.extra_one_way_us,
                );
                if flow.snd.is_done() && !flow.finished {
                    flow.finished = true;
                    if let Kind::Bursts {
                        flows,
                        log,
                        next_burst,
                        pending_off_us,
                        mean_off_ns,
                        ..
                    } = &mut self.kind
                    {
                        if flows.iter().all(|f| f.finished) {
                            if let Some(rec) = log.last_mut() {
                                rec.end = Some(t);
                            }
                            let off = self.rng.exp_ns(*mean_off_ns) / 1000;
                            *pending_off_us = off;
                            *next_burst = Some(t.plus(off));
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn new_flow(
    seed: u64,
    dir: Dir,
    src: usize,
    generation: u64,
    k: u32,
    cfg: TcpConfig,
    segs: Option<u64>,
    ack_jitter: bool,
) -> Flow {
    let name = format!("cross/{src}/{generation}/{k}");
    Flow {
        snd: TcpSender::new(cfg, segs),
        rcv: TcpReceiver::new(),
        data_lane: Lane::new(seed, dir, &name),
        ack_lane: Lane::new(seed, dir.other(), &name),
        finished: false,
        ack_jitter: ack_jitter.then(|| {
            (
                LayerRng::new(seed, dir.other(), &format!("{name}/ack-jitter")),
                Time::ZERO,
            )
        }),
    }
}

/// 超时的连接（定时器到期的那些）处理超时，重发。
#[allow(clippy::too_many_arguments)]
fn timeouts(
    ctx: &mut Ctx<'_>,
    dir: Dir,
    stats: &mut CrossStats,
    flows: &mut [Flow],
    src: usize,
    generation: u64,
    t: Time,
    extra_one_way_us: u64,
) {
    for (k, flow) in flows.iter_mut().enumerate() {
        if flow.snd.next_timer().is_some_and(|d| d <= t) {
            let mut out = Vec::new();
            flow.snd.on_timeout(t, &mut out);
            send_data(
                ctx,
                dir,
                stats,
                flow,
                src,
                generation,
                k,
                t,
                &out,
                extra_one_way_us,
            );
        }
    }
}

/// 把发送端要发的包送进 `dir` 方向。
#[allow(clippy::too_many_arguments)]
fn send_data(
    ctx: &mut Ctx<'_>,
    dir: Dir,
    stats: &mut CrossStats,
    flow: &mut Flow,
    src: usize,
    generation: u64,
    k: usize,
    t: Time,
    out: &[u64],
    extra_one_way_us: u64,
) {
    let size = flow.snd_mss();
    for seq in out {
        stats.sent_packets += 1;
        stats.sent_bytes += size;
        match ctx.dir(dir).cross_transit(&mut flow.data_lane, t, size) {
            Transit::Arrives { at, wait_us } => {
                stats.wait_sum_us += wait_us;
                stats.wait_count += 1;
                ctx.push(
                    at.plus(extra_one_way_us),
                    Item {
                        src,
                        generation,
                        flow: k,
                        kind: ItemKind::Data(*seq),
                    },
                );
            }
            Transit::QueueDropped => stats.queue_dropped += 1,
            Transit::Lost => stats.lost += 1,
        }
    }
}

impl Flow {
    fn snd_mss(&self) -> u64 {
        self.snd.mss_bytes()
    }
}

impl Item {
    /// 属于哪个源。
    #[must_use]
    pub const fn src(&self) -> usize {
        self.src
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::DirConfig;

    fn dirs() -> [DirState; 2] {
        [
            DirState::new(1, Dir::Forward, DirConfig::default()),
            DirState::new(1, Dir::Reverse, DirConfig::default()),
        ]
    }

    /// 不属于当前这一阵的包、不存在的连接、定速源收到的包，都被忽略，不动任何状态。
    #[test]
    fn stray_deliveries_are_ignored() {
        let mut dirs = dirs();
        let mut queue = BTreeMap::new();
        let mut next_id = 0u64;
        let mut ctx = Ctx {
            dirs: &mut dirs,
            queue: &mut queue,
            next_id: &mut next_id,
            seed: 1,
        };
        let spec = CrossSpec::Tcp {
            cfg: TcpConfig::default(),
            start: Time(0),
            bytes: None,
        };
        let mut s =
            Source::with_options(1, 0, Dir::Forward, spec, CrossOptions::default(), Time(0));
        s.fire(&mut ctx, 0, Time(0));
        let sent = s.stats().sent_packets;
        assert_eq!(sent, 10, "初始窗口 10 个包");
        let pending = ctx.queue.len();
        let stale = Item {
            src: 0,
            generation: 99,
            flow: 0,
            kind: ItemKind::Ack(5, [None; 3]),
        };
        let missing = Item {
            src: 0,
            generation: 0,
            flow: 7,
            kind: ItemKind::Data(0),
        };
        s.deliver(&mut ctx, 0, Time(50), stale);
        s.deliver(&mut ctx, 0, Time(50), missing);
        assert_eq!((ctx.queue.len(), s.stats().sent_packets), (pending, sent));
        assert_eq!(stale.src(), 0);
        let cbr = CrossSpec::Cbr {
            rate_bps: 1_000_000,
            packet_bytes: 100,
            start: Time(0),
            stop: None,
        };
        let mut c = Source::with_options(1, 1, Dir::Forward, cbr, CrossOptions::default(), Time(0));
        c.deliver(&mut ctx, 1, Time(50), stale);
        assert_eq!(c.stats(), CrossStats::default());
    }
}
