//! 分层的网络路径模型（随机数布局 2）。老的 `sim::Net`（随机数布局 1）冻结不动，新功能只加在这里。
//! 方案和依据：`docs/research/Q-redesign/a2-plan.md`（第 2、3 节）、`industry-network-sim.md`。
//!
//! # 一条路径、两个方向、七层
//!
//! [`Path`] 是节点 0 和节点 1 之间的一条路径。两个方向（[`Dir`]：正向 0→1，反向 1→0）各有一组完整的层参数
//! （[`DirConfig`]），互相没有任何联系：改反向的参数不会影响正向的任何一个数。
//!
//! 一个方向上，包按下面的顺序经过各层，每层都可以关（默认全关，等于理想链路）。每层只看比它早到的包，
//! 所以包的送达时刻在发送时就能算出来（和 `Net` 一样，事件推进，不逐毫秒步进）。
//!
//! | 次序 | 层 | 参数 | 做什么 |
//! |---|---|---|---|
//! | 1 | 中断和换路 | [`RouteChange`]、[`Path::set_up`] | 中断期间全丢；恢复时换参数并发出“路径变了”事件 |
//! | 2 | 限速器 | [`Policer`] | 令牌桶，超过的包直接丢，不排队 |
//! | 3 | 瓶颈 | [`BottleneckConfig`] | 容量（匀速、泊松放行、轨迹），队列（按时间、按字节、丢尾、CoDel） |
//! | 4 | 丢包 | [`Loss`] | 独立丢包或两态突发丢包；在瓶颈之后，丢掉的包已经占过容量 |
//! | 5 | 传播延迟 | [`Delay`] | 固定、按计划阶跃、周期性阶跃（Starlink 式）；阶跃不改变包的先后 |
//! | 6 | 延迟噪声 | [`Noise`] | 均匀抖动（乱序或不乱序）、尖峰、省电唤醒 |
//! | 7 | 接收端 | [`Receiver`] | 批量收、接收端忙 |
//!
//! 第 6、7 层的语义照搬 `Net`（顺序也和 `Net` 一样：批量收在接收端忙之前），所以 `Link` 的各个字段都能一一搬过来。
//!
//! # 接口
//!
//! 形状和 `Net` 一样，A2-3 定公共的 `Network` 接口时可以直接把两者套进去：
//! `send(from, to, size, payload)`、`peek_time`、`pop`（取出并把时钟推到送达时刻）、`advance_to`、`in_flight`、
//! `backlog_us(from, to)`、`stats`。送达仍是 [`crate::sim::Delivery`]。节点编号：`(0, 1)` 是正向，`(1, 0)` 是反向，
//! 别的组合算 `unroutable`。和 `Net` 不同的地方：
//! - 参数用 [`PathConfig`]（两个方向各一份）；`set_dir` 立即换一个方向的参数（对应 `Net::set_one_way`），
//!   `schedule_route_change` 按计划换（带中断）；
//! - 每个包的结局分得更细（[`PathStats`]）：中断、限速器、丢尾、CoDel、丢包层各自计数；
//! - “路径变了”事件：`next_event_time` 给出下一个事件的时刻，`pop_event` 取出（到时刻才取得到）。
//!   主循环取 `min(peek_time, next_event_time)` 往前推，再把事件交给两端（怎么交是 A2-3 的事）。
//!
//! # 路径变了事件
//!
//! [`RouteChange`] 说：从 `at` 起中断 `down_us` 微秒，恢复的时刻（`at + down_us`）把两个方向的参数换成 `new`，
//! 同时发出一次 [`PathEvent::RouteChanged`]，事件的时刻就是恢复的时刻，只发一次。`new` 为 `None` 是纯中断
//! （例如电梯里断一会儿），不换参数，也不发事件。`down_us` 为 0 就是不中断、按计划换一次参数。
//! 换参数时：随机数流接着抽，不重新播种；已经算好的送达不改；瓶颈里排着的包和链路空出来的时刻不变；
//! 按时间的状态（尖峰和接收端忙的时间表、限速器的桶、周期性延迟）从恢复时刻重新开始。
//! 仿真不模拟 QUIC 连接本身：切网后的连接迁移、重连在真二进制（L2）里测，这里只给“中断加路径变了”。
//!
//! # 随机数：每层一条流
//!
//! 每一层的流由（世界种子，方向，层名）导出：blake3 哈希取前 8 个字节当 SplitMix64 的种子（见 [`rng`]）。
//! 为了让“只多了这一层”的比较干净，还有两条规矩：
//! 1. 按包抽数的层（丢包、抖动、省电）每个包固定抽固定个数，包在前面的层被丢了也照抽；
//! 2. 按时间走的层（泊松放行机会、尖峰、接收端忙、周期性延迟）的时间表从起点起只由自己的流决定，
//!    第 k 个窗、第 k 次机会、第 k 个周期永远抽到同一个数，和哪些包在什么时候来无关。
//!
//! 所以关掉任意一层、改任意一层的参数，其他层的数列（按包抽的）逐个相同，（按时间走的）互为前缀。
//! 测试里用 `stream_log` 逐个核对。
//!
//! 和 `Net` 的差别（A2-3 对账时要知道）：丢包在瓶颈之后（`Net` 在之前，丢掉的包不占容量）；
//! 容量误差用纳秒记，不是每个包向下取整到微秒；尖峰和接收端忙的时间表从 0 起算。
//!
//! # 交叉流量
//!
//! 交叉流量只在瓶颈这一层注入：瓶颈之前只有限速器，它不改变包的先后，所以每个包到瓶颈时，比它早到的包都已经处理过了。
//! 两个入口：
//! - [`Path::add_cross`] 挂一个源（[`CrossSpec`]：定速背景流、TCP 长连接、RFC 8868 的一阵一阵的短连接），
//!   它的事件在 `send`、`advance_to`、`pop` 开头随时钟按时间顺序处理掉（细节和随机数见 `cross.rs` 的模块文档）。
//!   `peek_time` 不需要处理：交叉流量不产生主流量的送达。
//! - [`Path::inject_cross`] 让外部驱动的一个包在 `at` 时刻到达瓶颈（只经过瓶颈）；要求 `at` 不早于上一个到达瓶颈的包、
//!   不晚于 `now`。
//!
//! 交叉流量的包占队列、占容量、受丢尾和 CoDel 约束，和主流量完全一样；有接收端的（TCP）还要过丢包层和传播延迟，
//! 确认走反方向，占反向的容量，也会丢。限速器、延迟噪声、接收端三层不经过。
//! 每个源一条随机数流 `cross/<序号>`，每条 TCP 连接每个方向一条车道，都不动主流量的流。

mod bottleneck;
mod codel;
mod cross;
mod delay;
mod dir;
mod loss;
mod policer;
pub mod rng;
pub mod segments;
pub mod tcp;

#[cfg(test)]
mod cross_tests;
#[cfg(test)]
mod segment_tests;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};

use crate::sim::{Delivery, NetStats, Network};
use crate::Time;

pub use bottleneck::{Aqm, Bottleneck, BottleneckConfig, Capacity, QueueLimit, Trace, Verdict};
pub use codel::Codel;
pub use cross::{BurstRecord, CrossSpec, CrossStats};
pub use delay::{Delay, DelayState, Noise, Receiver, Windows};
pub use dir::{DirConfig, DirState, Lane, Outcome, Transit};
pub use loss::{burst_probabilities, Loss, LossState};
pub use policer::{Policer, PolicerState};
pub use rng::{layer, stream_seed, Dir, LayerRng};
pub use tcp::{TcpConfig, TcpReceiver, TcpSender, TcpStats};

/// 两个方向的参数。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathConfig {
    /// 正向（节点 0 到节点 1）。
    pub forward: DirConfig,
    /// 反向（节点 1 到节点 0）。
    pub reverse: DirConfig,
}

impl PathConfig {
    /// 两个方向一样（参数相同，各自的随机数流仍然各是各的）。
    #[must_use]
    pub fn symmetric(cfg: DirConfig) -> Self {
        Self {
            forward: cfg.clone(),
            reverse: cfg,
        }
    }

    /// 取一个方向的参数。
    #[must_use]
    pub const fn dir(&self, dir: Dir) -> &DirConfig {
        match dir {
            Dir::Forward => &self.forward,
            Dir::Reverse => &self.reverse,
        }
    }
}

/// 按计划的中断和换路，见模块文档。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteChange {
    /// 中断开始的时刻。
    pub at: Time,
    /// 中断多久（微秒），0 表示不中断。
    pub down_us: u64,
    /// 恢复时换成的参数；`None` 是纯中断，不发事件。
    pub new: Option<PathConfig>,
}

/// 路径模型发给两端的事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathEvent {
    /// 路径变了（恢复的时刻，两个方向的参数已经换成新的）。
    RouteChanged {
        /// 恢复的时刻。
        at: Time,
    },
}

impl PathEvent {
    /// 事件发生的时刻。
    #[must_use]
    pub const fn at(&self) -> Time {
        match self {
            PathEvent::RouteChanged { at } => *at,
        }
    }
}

/// 一个方向上的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PathStats {
    /// 进入的包数（主流量）。
    pub sent: u64,
    /// 已送达的包数。
    pub delivered: u64,
    /// 中断期间丢的。
    pub outage_dropped: u64,
    /// 被限速器丢的。
    pub policed: u64,
    /// 瓶颈丢尾丢的。
    pub queue_dropped: u64,
    /// CoDel 丢的。
    pub aqm_dropped: u64,
    /// 丢包层丢的。
    pub lost: u64,
    /// 没有对应方向的包数（只在总计里）。
    pub unroutable: u64,
    /// 经 [`Path::inject_cross`] 注入瓶颈的交叉流量包数（`add_cross` 挂的源各自计数，见 [`Path::cross_stats`]）。
    pub cross_sent: u64,
    /// 其中被丢尾或 CoDel 丢掉的。
    pub cross_dropped: u64,
}

impl PathStats {
    fn plus(self, o: PathStats) -> PathStats {
        PathStats {
            sent: self.sent + o.sent,
            delivered: self.delivered + o.delivered,
            outage_dropped: self.outage_dropped + o.outage_dropped,
            policed: self.policed + o.policed,
            queue_dropped: self.queue_dropped + o.queue_dropped,
            aqm_dropped: self.aqm_dropped + o.aqm_dropped,
            lost: self.lost + o.lost,
            unroutable: self.unroutable + o.unroutable,
            cross_sent: self.cross_sent + o.cross_sent,
            cross_dropped: self.cross_dropped + o.cross_dropped,
        }
    }
}

/// 注入交叉流量的错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossError {
    /// `at` 比上一个到达瓶颈的包早，前面已经算好的包没法重算。
    Late {
        /// 上一个到达瓶颈的包的时刻。
        last: Time,
    },
    /// `at` 比现在的时钟晚：先 `advance_to`。
    Future,
}

/// 网络路径。节点用 0 和 1 标识。
#[derive(Debug, Clone)]
pub struct Path<T> {
    now: Time,
    seed: u64,
    dirs: [DirState; 2],
    stats: [PathStats; 2],
    unroutable: u64,
    manual_up: bool,
    /// 按计划的中断时段 `[开始, 结束)`。
    outages: Vec<(Time, Time)>,
    /// 还没到恢复时刻的换路（按恢复时刻排序，只含带新参数的）。
    pending: Vec<(Time, PathConfig)>,
    events: VecDeque<PathEvent>,
    queue: BTreeMap<(Time, u64), Delivery<T>>,
    next_id: u64,
    /// 交叉流量源，和在途的交叉包。
    sources: Vec<cross::Source>,
    cross_queue: BTreeMap<(Time, u64), cross::Item>,
    cross_next_id: u64,
}

impl<T> Path<T> {
    /// 由世界种子和参数创建。
    #[must_use]
    pub fn new(seed: u64, cfg: PathConfig) -> Self {
        Self {
            now: Time::ZERO,
            seed,
            dirs: [
                DirState::new(seed, Dir::Forward, cfg.forward),
                DirState::new(seed, Dir::Reverse, cfg.reverse),
            ],
            stats: [PathStats::default(); 2],
            unroutable: 0,
            manual_up: true,
            outages: Vec::new(),
            pending: Vec::new(),
            events: VecDeque::new(),
            queue: BTreeMap::new(),
            next_id: 0,
            sources: Vec::new(),
            cross_queue: BTreeMap::new(),
            cross_next_id: 0,
        }
    }

    /// 世界种子。
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// 当前虚拟时间。
    #[must_use]
    pub const fn now(&self) -> Time {
        self.now
    }

    /// 两个方向的总计数。
    #[must_use]
    pub fn stats(&self) -> PathStats {
        let [f, r] = self.stats;
        let mut total = f.plus(r);
        total.unroutable = self.unroutable;
        total
    }

    /// 一个方向的计数。
    #[must_use]
    pub fn dir_stats(&self, dir: Dir) -> PathStats {
        self.stats.get(dir.index()).copied().unwrap_or_default()
    }

    /// 一个方向的状态（参数、瓶颈、各层抽数的个数），诊断用。
    #[must_use]
    pub fn dir(&self, dir: Dir) -> &DirState {
        self.dirs.get(dir.index()).unwrap_or(&self.dirs[0])
    }

    fn dir_mut(&mut self, dir: Dir) -> (&mut DirState, &mut PathStats) {
        let [df, dr] = &mut self.dirs;
        let [sf, sr] = &mut self.stats;
        match dir {
            Dir::Forward => (df, sf),
            Dir::Reverse => (dr, sr),
        }
    }

    /// 立即换一个方向的参数（对应 `Net::set_one_way`）。
    pub fn set_dir(&mut self, dir: Dir, cfg: DirConfig) {
        let now = self.now;
        self.dir_mut(dir).0.reconfigure(cfg, now);
    }

    /// 手动接通或切断链路（两个方向一起，对应 `Net::set_up`）。
    pub fn set_up(&mut self, up: bool) {
        self.manual_up = up;
    }

    /// 安排一次按计划的中断和换路。
    pub fn schedule_route_change(&mut self, change: RouteChange) {
        let restore = change.at.plus(change.down_us);
        if change.down_us > 0 {
            self.outages.push((change.at, restore));
        }
        if let Some(new) = change.new {
            let pos = self.pending.partition_point(|(t, _)| *t <= restore);
            self.pending.insert(pos, (restore, new));
        }
    }

    fn is_up(&self, t: Time) -> bool {
        self.manual_up && !self.outages.iter().any(|(s, e)| *s <= t && t < *e)
    }

    /// 把到了恢复时刻的换路做掉，发出事件。
    fn apply_due(&mut self) {
        while self.pending.first().is_some_and(|(t, _)| *t <= self.now) {
            let (at, cfg) = self.pending.remove(0);
            for dir in Dir::BOTH {
                self.dir_mut(dir).0.reconfigure(cfg.dir(dir).clone(), at);
            }
            self.events.push_back(PathEvent::RouteChanged { at });
        }
    }

    /// 下一个“路径变了”事件的时刻（包括还没到时刻的计划）。
    #[must_use]
    pub fn next_event_time(&self) -> Option<Time> {
        let queued = self.events.front().map(PathEvent::at);
        let planned = self.pending.first().map(|(t, _)| *t);
        match (queued, planned) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }

    /// 取出一个时刻已经到了（不晚于 `now`）的事件。
    pub fn pop_event(&mut self) -> Option<PathEvent> {
        self.apply_due();
        self.events.pop_front()
    }

    /// 发送一个 `size_bytes` 字节的包。发送方无法得知它是否会丢。
    pub fn send(&mut self, from: usize, to: usize, size_bytes: usize, payload: T) {
        let Some(dir) = Dir::from_nodes(from, to) else {
            self.unroutable = self.unroutable.saturating_add(1);
            return;
        };
        self.apply_due();
        self.drive_cross();
        let now = self.now;
        let up = self.is_up(now);
        let size = u64::try_from(size_bytes).unwrap_or(u64::MAX);
        let (d, stats) = self.dir_mut(dir);
        stats.sent = stats.sent.saturating_add(1);
        match d.process(now, size, up) {
            Outcome::Outage => stats.outage_dropped = stats.outage_dropped.saturating_add(1),
            Outcome::Policed => stats.policed = stats.policed.saturating_add(1),
            Outcome::TailDropped => stats.queue_dropped = stats.queue_dropped.saturating_add(1),
            Outcome::AqmDropped => stats.aqm_dropped = stats.aqm_dropped.saturating_add(1),
            Outcome::Lost => stats.lost = stats.lost.saturating_add(1),
            Outcome::Delivered(at) => {
                let id = self.next_id;
                self.next_id = self.next_id.saturating_add(1);
                self.queue.insert(
                    (at, id),
                    Delivery {
                        at,
                        from,
                        to,
                        payload,
                    },
                );
            }
        }
    }

    /// 让一个交叉流量包在 `at` 时刻到达 `dir` 方向的瓶颈（只经过瓶颈），见模块文档。
    /// 返回它离开瓶颈的时刻；被丢尾或 CoDel 丢了返回 `Ok(None)`。
    ///
    /// # Errors
    /// `at` 早于上一个到达瓶颈的包，或晚于现在的时钟。
    pub fn inject_cross(
        &mut self,
        dir: Dir,
        at: Time,
        size_bytes: usize,
    ) -> Result<Option<Time>, CrossError> {
        if at > self.now {
            return Err(CrossError::Future);
        }
        self.drive_cross();
        let size = u64::try_from(size_bytes).unwrap_or(u64::MAX);
        let (d, stats) = self.dir_mut(dir);
        let last = d.bottleneck().last_arrival();
        if at < last {
            return Err(CrossError::Late { last });
        }
        stats.cross_sent = stats.cross_sent.saturating_add(1);
        match d.cross_arrive(at, size) {
            Verdict::Departs { done, .. } => Ok(Some(done)),
            Verdict::TailDropped | Verdict::AqmDropped => {
                stats.cross_dropped = stats.cross_dropped.saturating_add(1);
                Ok(None)
            }
        }
    }

    /// 在 `dir` 方向的瓶颈上挂一个交叉流量源，返回它的序号。`start` 早于现在的按现在算。
    /// 源的事件在 `send`、`advance_to`、`pop` 里随时钟往前处理（见 [`cross`] 模块文档）。
    pub fn add_cross(&mut self, dir: Dir, spec: CrossSpec) -> usize {
        let index = self.sources.len();
        self.sources
            .push(cross::Source::new(self.seed, index, dir, spec, self.now));
        index
    }

    /// 挂了几个交叉流量源（序号是 `0..这个数`）。
    #[must_use]
    pub fn cross_sources(&self) -> usize {
        self.sources.len()
    }

    /// 交叉流量源的计数（序号不存在时全 0）。
    #[must_use]
    pub fn cross_stats(&self, id: usize) -> CrossStats {
        self.sources
            .get(id)
            .map(cross::Source::stats)
            .unwrap_or_default()
    }

    /// 短连接源的记录（每一阵的空闲时长、开始、结束、每条连接的字节数）。
    #[must_use]
    pub fn cross_bursts(&self, id: usize) -> &[BurstRecord] {
        self.sources.get(id).map_or(&[], cross::Source::bursts)
    }

    /// 交叉 TCP 源第一条连接的发送端（看窗口、RTO）。
    #[must_use]
    pub fn cross_sender(&self, id: usize) -> Option<&TcpSender> {
        self.sources.get(id).and_then(cross::Source::sender)
    }

    /// 把所有 `≤ now` 的交叉流量事件按时间顺序处理掉；同一时刻先处理包的送达，再处理定时器。
    fn drive_cross(&mut self) {
        if self.sources.is_empty() {
            return;
        }
        loop {
            let timer = self
                .sources
                .iter()
                .enumerate()
                .filter_map(|(i, s)| s.next_time().map(|t| (t, i)))
                .min();
            let delivery = self.cross_queue.keys().next().copied();
            let mut ctx = cross::Ctx {
                dirs: &mut self.dirs,
                queue: &mut self.cross_queue,
                next_id: &mut self.cross_next_id,
                seed: self.seed,
            };
            match (delivery, timer) {
                (Some((td, _)), tm) if td <= self.now && tm.is_none_or(|(t, _)| td <= t) => {
                    if let Some((_, item)) = ctx.queue.pop_first() {
                        if let Some(src) = self.sources.get_mut(item.src()) {
                            src.deliver(&mut ctx, item.src(), td, item);
                        }
                    }
                }
                (_, Some((t, i))) if t <= self.now => {
                    if let Some(src) = self.sources.get_mut(i) {
                        src.fire(&mut ctx, i, t);
                    }
                }
                (Some(_) | None, Some(_) | None) => break,
            }
        }
    }

    /// 下一次送达的时间；队列为空时返回 `None`。
    #[must_use]
    pub fn peek_time(&self) -> Option<Time> {
        self.queue.keys().next().map(|(at, _)| *at)
    }

    /// 取出下一次送达，并把时钟推进到它的到达时间。
    pub fn pop(&mut self) -> Option<Delivery<T>> {
        let (_, delivery) = self.queue.pop_first()?;
        self.now = self.now.max(delivery.at);
        self.apply_due();
        self.drive_cross();
        let i = Dir::from_nodes(delivery.from, delivery.to).map_or(0, Dir::index);
        if let Some(s) = self.stats.get_mut(i) {
            s.delivered = s.delivered.saturating_add(1);
        }
        Some(delivery)
    }

    /// 把时钟推进到 `t`；时钟不会倒退。
    pub fn advance_to(&mut self, t: Time) {
        self.now = self.now.max(t);
        self.apply_due();
        self.drive_cross();
    }

    /// 还在途中的包数。
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.queue.len()
    }

    /// `from → to` 方向瓶颈里现在真实排着多久的数据（微秒），含义和 `Net::backlog_us` 一样。
    /// 不限速或不是这两个方向时为 0。
    #[must_use]
    pub fn backlog_us(&self, from: usize, to: usize) -> u64 {
        Dir::from_nodes(from, to).map_or(0, |dir| self.dir(dir).bottleneck().backlog_us(self.now))
    }
}

impl PathStats {
    /// 折算成 `Net` 的口径：`lost` 是中断、限速器、丢包层三样之和，`queue_dropped` 是丢尾加 CoDel。
    /// 交叉流量不算（`Net` 没有交叉流量）。
    #[must_use]
    pub const fn net(&self) -> NetStats {
        NetStats {
            sent: self.sent,
            delivered: self.delivered,
            lost: self
                .outage_dropped
                .saturating_add(self.policed)
                .saturating_add(self.lost),
            queue_dropped: self.queue_dropped.saturating_add(self.aqm_dropped),
            unroutable: self.unroutable,
        }
    }
}

impl<T> Network<T> for Path<T> {
    fn send(&mut self, from: usize, to: usize, size_bytes: usize, payload: T) {
        Path::send(self, from, to, size_bytes, payload);
    }
    fn peek_time(&self) -> Option<Time> {
        Path::peek_time(self)
    }
    fn pop(&mut self) -> Option<Delivery<T>> {
        Path::pop(self)
    }
    fn advance_to(&mut self, t: Time) {
        Path::advance_to(self, t);
    }
    fn in_flight(&self) -> usize {
        Path::in_flight(self)
    }
    fn backlog_us(&self, from: usize, to: usize) -> u64 {
        Path::backlog_us(self, from, to)
    }
    fn stats(&self) -> NetStats {
        Path::stats(self).net()
    }
    fn path_stats(&self) -> Option<PathStats> {
        Some(Path::stats(self))
    }
    fn next_event_time(&self) -> Option<Time> {
        Path::next_event_time(self)
    }
    fn pop_event(&mut self) -> Option<PathEvent> {
        Path::pop_event(self)
    }
}
