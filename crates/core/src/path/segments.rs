//! 分段路径：两端之间串起五段，每段一个 [`Path`]（`a2-plan.md` 第 8 节“分段路径”，CLAUDE.md“分段测量”）。
//!
//! # 五段
//!
//! 画面从 A（节点 0，被控端）走向 B（节点 1，控制端）。一个方向经过的五段依次是：
//!
//! | 序号 | 段 | 放什么 |
//! |---|---|---|
//! | 0 | A 局域网 | A 的 Wi-Fi：尖峰、省电、随机丢包 |
//! | 1 | A 家庭出口 | A 的宽带：限速器、瓶颈和排队（有没有 AQM），A 家其他设备的上传 |
//! | 2 | 公网 | 传播延迟、延迟阶跃、少量公网交叉流量 |
//! | 3 | B 家庭出口 | B 的宽带：B 家其他设备的下载（看视频） |
//! | 4 | B 局域网 | B 的 Wi-Fi、接收端 |
//!
//! 每段是一个两个方向的 [`Path`]：它的正向是“A → B 方向经过这一段”，反向是“B → A”。所以 A 出口段的正向是 A 的上行，
//! 反向是 A 的下行；B 出口段的正向是 B 的下行。主流量 A → B 依次过 0..4 段的正向，B → A 依次过 4..0 段的反向。
//! 每段的层、交叉流、换路都是 `Path` 现成的，这里不改。
//!
//! # 段与段之间按事件推进
//!
//! 单个 `Path` 在发送时就能算出送达，因为每层只看比它早到的包。五段串起来以后不行：一个包算出“将来某刻到第 3 段”，
//! 之后才处理到同一个瓶颈、时刻更早的交叉流包，瓶颈就不是按到达先后处理了。所以：
//! - 一个包离开第 k 段时（第 k 段的送达），在那个时刻送进下一段：先把下一段推进到这个时刻（它的交叉流事件按时间先处理掉），
//!   再收包；
//! - 全局按时间顺序处理这些“段间转交”。每段看到的到达都不早于前一个，局域网段的乱序如实带进下一段，不用限制抖动的种类；
//! - 同一时刻的转交按段号从小到大，同一段里按 `Path` 自己的（时刻，进入的先后），所以逐字节可复现。
//!
//! # 探测
//!
//! [`Segmented::send_probe`] 让一个包只走到某一段再折返：从 A 发、深度 1 是到网关（第 0 段正向再第 0 段反向），
//! 深度 2 是到运营商第一跳（第 0、1 段正向，再第 1、0 段反向）。从 B 发对称。送达的 `from` 和 `to` 都是发探测的一端。
//!
//! # 随机数
//!
//! 每段自己的世界种子 = [`stream_seed`]（世界种子，正向，`segment/<段名>`）；段里每层再按 `Path` 的规矩各一条流。
//! 五段互不相关；同一段的参数不变时，别的段怎么改都不影响它抽到的数。
//!
//! # 计数
//!
//! [`Network::stats`] 只数主流量（`send` 进来的包）：进入算一次、送达算一次，丢包和排队丢包算在丢它的那一段，五段相加。
//! 探测包另数（[`Segmented::probe_stats`]）。每段的真值（排队、各层丢了多少、参数）用 [`Segmented::segment`] 取。

use std::collections::BTreeMap;

use crate::sim::{Delivery, NetStats, Network};
use crate::Time;

use super::rng::stream_seed;
use super::{CrossSpec, Dir, Path, PathConfig, PathEvent, PathStats, RouteChange};

/// 段数。
pub const SEGMENTS: usize = 5;

/// 一段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Segment {
    /// A 局域网。
    ALan,
    /// A 家庭出口。
    AEgress,
    /// 公网。
    Public,
    /// B 家庭出口。
    BEgress,
    /// B 局域网。
    BLan,
}

impl Segment {
    /// 按 A → B 的顺序。
    pub const ALL: [Segment; SEGMENTS] = [
        Segment::ALan,
        Segment::AEgress,
        Segment::Public,
        Segment::BEgress,
        Segment::BLan,
    ];

    /// 序号（A → B 方向的第几段）。
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Segment::ALan => 0,
            Segment::AEgress => 1,
            Segment::Public => 2,
            Segment::BEgress => 3,
            Segment::BLan => 4,
        }
    }

    /// 名字（随机数流和世界文件用）。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Segment::ALan => "a-lan",
            Segment::AEgress => "a-egress",
            Segment::Public => "public",
            Segment::BEgress => "b-egress",
            Segment::BLan => "b-lan",
        }
    }
}

/// 五段的参数，按 [`Segment::ALL`] 的顺序；每段的正向是 A → B 方向。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SegmentedConfig {
    /// 每段的参数。
    pub segments: [PathConfig; SEGMENTS],
}

impl SegmentedConfig {
    /// 取一段的参数。
    #[must_use]
    pub fn segment(&self, s: Segment) -> &PathConfig {
        let [a, b, c, d, e] = &self.segments;
        match s {
            Segment::ALan => a,
            Segment::AEgress => b,
            Segment::Public => c,
            Segment::BEgress => d,
            Segment::BLan => e,
        }
    }

    /// 改一段的参数。
    pub fn segment_mut(&mut self, s: Segment) -> &mut PathConfig {
        let [a, b, c, d, e] = &mut self.segments;
        match s {
            Segment::ALan => a,
            Segment::AEgress => b,
            Segment::Public => c,
            Segment::BEgress => d,
            Segment::BLan => e,
        }
    }
}

/// 一次按计划的中断和换路，可以同时涉及几段（例如 Wi-Fi 换成蜂窝：A 局域网段和 A 出口段一起换）。
/// `down` 里的段从 `at` 起中断 `down_us`；`new` 里的段在恢复时刻（`at + down_us`）换成新参数。
/// `new` 不空时，在恢复时刻发**一次**“路径变了”（不管换了几段）；`new` 为空是纯中断，不发事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRoute {
    /// 中断开始的时刻。
    pub at: Time,
    /// 中断多久（微秒），0 是不中断。
    pub down_us: u64,
    /// 中断的段。
    pub down: Vec<Segment>,
    /// 恢复时换参数的段。
    pub new: Vec<(Segment, PathConfig)>,
}

/// 包走的路线。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    /// 主流量：一个方向走完五段。
    Main(Dir),
    /// 探测：从一端走 `depth` 段再折返。
    Probe {
        /// 从哪一端发（正向是从 A 发）。
        from: Dir,
        /// 走几段。
        depth: usize,
    },
}

impl Route {
    /// 第 `step` 跳经过哪一段、哪个方向；走完了是 `None`。
    fn hop(self, step: usize) -> Option<(usize, Dir)> {
        let n = SEGMENTS;
        match self {
            Route::Main(Dir::Forward) => (step < n).then_some((step, Dir::Forward)),
            Route::Main(Dir::Reverse) => (step < n).then(|| (n - 1 - step, Dir::Reverse)),
            Route::Probe { from, depth } => {
                if step >= 2 * depth {
                    return None;
                }
                let out = step < depth;
                // 出去时第 step 段，回来时第 2·depth−1−step 段（都从发的那一端数）
                let k = if out { step } else { 2 * depth - 1 - step };
                Some(match (from, out) {
                    (Dir::Forward, true) => (k, Dir::Forward),
                    (Dir::Forward, false) => (k, Dir::Reverse),
                    (Dir::Reverse, true) => (n - 1 - k, Dir::Reverse),
                    (Dir::Reverse, false) => (n - 1 - k, Dir::Forward),
                })
            }
        }
    }

    /// 送达时的（发送节点，接收节点）。
    const fn ends(self) -> (usize, usize) {
        match self {
            Route::Main(Dir::Forward) => (0, 1),
            Route::Main(Dir::Reverse) => (1, 0),
            Route::Probe { from, .. } => match from {
                Dir::Forward => (0, 0),
                Dir::Reverse => (1, 1),
            },
        }
    }
}

/// 在段里走着的包。
#[derive(Debug, Clone)]
pub struct Hop<T> {
    payload: T,
    size: usize,
    route: Route,
    step: usize,
}

const fn nodes(dir: Dir) -> (usize, usize) {
    match dir {
        Dir::Forward => (0, 1),
        Dir::Reverse => (1, 0),
    }
}

/// 一类流量的计数（主流量或探测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlowStats {
    /// 进入的包数。
    pub sent: u64,
    /// 送达的包数。
    pub delivered: u64,
    /// 丢包层、中断、限速器丢的（`Net` 口径的 `lost`）。
    pub lost: u64,
    /// 丢尾和 CoDel 丢的。
    pub queue_dropped: u64,
}

/// 分段路径。节点 0 是 A，1 是 B。
#[derive(Debug, Clone)]
pub struct Segmented<T> {
    now: Time,
    segments: [Path<Hop<T>>; SEGMENTS],
    /// 走完了的包：（送达时刻，序号）。
    out: BTreeMap<(Time, u64), Delivery<T>>,
    next_out: u64,
    main: FlowStats,
    probes: FlowStats,
    unroutable: u64,
    /// 还没发出的“路径变了”事件的时刻（恢复时刻，已去重、排序）。
    pending_events: Vec<Time>,
}

impl<T> Segmented<T> {
    /// 由世界种子和五段的参数创建。
    #[must_use]
    pub fn new(seed: u64, cfg: SegmentedConfig) -> Self {
        let make = |s: Segment, c: PathConfig| {
            let name = format!("segment/{}", s.name());
            Path::new(stream_seed(seed, Dir::Forward, &name), c)
        };
        let [a, b, c, d, e] = cfg.segments;
        let segments = [
            make(Segment::ALan, a),
            make(Segment::AEgress, b),
            make(Segment::Public, c),
            make(Segment::BEgress, d),
            make(Segment::BLan, e),
        ];
        Self {
            now: Time::ZERO,
            segments,
            out: BTreeMap::new(),
            next_out: 0,
            main: FlowStats::default(),
            probes: FlowStats::default(),
            unroutable: 0,
            pending_events: Vec::new(),
        }
    }

    /// 当前虚拟时间。
    #[must_use]
    pub const fn now(&self) -> Time {
        self.now
    }

    /// 一段（看参数、各层计数、排队等真值）。
    #[must_use]
    pub const fn segment(&self, s: Segment) -> &Path<Hop<T>> {
        let [a, b, c, d, e] = &self.segments;
        match s {
            Segment::ALan => a,
            Segment::AEgress => b,
            Segment::Public => c,
            Segment::BEgress => d,
            Segment::BLan => e,
        }
    }

    /// 第 `i` 段（`i` 来自 [`Segment::index`] 或路线，总在 0..5 里；大于 4 的按最后一段算）。
    fn seg_mut(&mut self, i: usize) -> &mut Path<Hop<T>> {
        let [a, b, c, d, e] = &mut self.segments;
        match i {
            0 => a,
            1 => b,
            2 => c,
            3 => d,
            _ => e,
        }
    }

    /// 每段的计数（按 [`Segment::ALL`] 的顺序），主流量和探测都算在里面。
    #[must_use]
    pub fn segment_stats(&self) -> Vec<PathStats> {
        self.segments.iter().map(Path::stats).collect()
    }

    /// 主流量的计数。
    #[must_use]
    pub const fn main_stats(&self) -> FlowStats {
        self.main
    }

    /// 探测包的计数。
    #[must_use]
    pub const fn probe_stats(&self) -> FlowStats {
        self.probes
    }

    /// 立即换一段一个方向的参数。
    pub fn set_dir(&mut self, s: Segment, dir: Dir, cfg: super::DirConfig) {
        self.seg_mut(s.index()).set_dir(dir, cfg);
    }

    /// 手动接通或切断一段（两个方向）。
    pub fn set_up(&mut self, s: Segment, up: bool) {
        self.seg_mut(s.index()).set_up(up);
    }

    /// 安排一次按计划的中断和换路，见 [`SegmentRoute`]。
    pub fn schedule_route_change(&mut self, change: SegmentRoute) {
        let restore = change.at.plus(change.down_us);
        for s in Segment::ALL {
            let down = change.down.contains(&s);
            let new = change
                .new
                .iter()
                .rev()
                .find(|(x, _)| *x == s)
                .map(|(_, c)| c.clone());
            if !down && new.is_none() {
                continue;
            }
            self.seg_mut(s.index()).schedule_route_change(RouteChange {
                at: change.at,
                down_us: if down { change.down_us } else { 0 },
                new,
            });
        }
        if !change.new.is_empty() {
            let pos = self.pending_events.partition_point(|t| *t < restore);
            if self.pending_events.get(pos) != Some(&restore) {
                self.pending_events.insert(pos, restore);
            }
        }
    }

    /// 在一段一个方向的瓶颈上挂交叉流量源，返回它在那一段里的序号。
    ///
    /// 注意：交叉 TCP 的往返只有这一段自己的传播延迟（确认走这一段的反向），不含其他段。
    pub fn add_cross(&mut self, s: Segment, dir: Dir, spec: CrossSpec) -> usize {
        self.seg_mut(s.index()).add_cross(dir, spec)
    }

    /// 从一端发一个探测包：走 `depth` 段（1 到 5）再折返回来。`from` 是 0（A）或 1（B）；不对时算 `unroutable`。
    pub fn send_probe(&mut self, from: usize, depth: usize, size_bytes: usize, payload: T) {
        let dir = match from {
            0 => Dir::Forward,
            1 => Dir::Reverse,
            _ => {
                self.unroutable = self.unroutable.saturating_add(1);
                return;
            }
        };
        if depth == 0 || depth > SEGMENTS {
            self.unroutable = self.unroutable.saturating_add(1);
            return;
        }
        self.probes.sent = self.probes.sent.saturating_add(1);
        self.enter(
            Hop {
                payload,
                size: size_bytes,
                route: Route::Probe { from: dir, depth },
                step: 0,
            },
            self.now,
        );
        // 不花时间的段马上转交完：发完以后看到的排队真值已经算上这个包
        self.run_until(self.now);
    }

    /// 包在 `at` 时刻进入它这一跳的段（或者走完了，进送达队列）。
    fn enter(&mut self, hop: Hop<T>, at: Time) {
        let Some((i, dir)) = hop.route.hop(hop.step) else {
            let (from, to) = hop.route.ends();
            let flow = self.flow_mut(hop.route);
            flow.delivered = flow.delivered.saturating_add(1);
            let id = self.next_out;
            self.next_out = self.next_out.saturating_add(1);
            self.out.insert(
                (at, id),
                Delivery {
                    at,
                    from,
                    to,
                    payload: hop.payload,
                },
            );
            return;
        };
        let route = hop.route;
        let seg = self.seg_mut(i);
        seg.advance_to(at);
        let before = seg.dir_stats(dir);
        let (from, to) = nodes(dir);
        let size = hop.size;
        seg.send(from, to, size, hop);
        let after = seg.dir_stats(dir);
        let lost = (after.outage_dropped + after.policed + after.lost)
            .saturating_sub(before.outage_dropped + before.policed + before.lost);
        let queued = (after.queue_dropped + after.aqm_dropped)
            .saturating_sub(before.queue_dropped + before.aqm_dropped);
        let flow = self.flow_mut(route);
        flow.lost = flow.lost.saturating_add(lost);
        flow.queue_dropped = flow.queue_dropped.saturating_add(queued);
    }

    fn flow_mut(&mut self, route: Route) -> &mut FlowStats {
        match route {
            Route::Main(_) => &mut self.main,
            Route::Probe { .. } => &mut self.probes,
        }
    }

    /// 下一个段间转交：（时刻，段号）。同一时刻段号小的先。
    fn next_transfer(&self) -> Option<(Time, usize)> {
        self.segments
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.peek_time().map(|t| (t, i)))
            .min()
    }

    /// 做掉一个段间转交。
    fn transfer(&mut self, i: usize) {
        // `i` 来自 next_transfer，那一段一定有包
        if let Some(d) = self.seg_mut(i).pop() {
            let mut hop = d.payload;
            hop.step = hop.step.saturating_add(1);
            self.enter(hop, d.at);
        }
    }

    /// 把 `≤ t` 的段间转交按时间顺序做掉。
    fn run_until(&mut self, t: Time) {
        while let Some((_, i)) = self.next_transfer().filter(|(at, _)| *at <= t) {
            self.transfer(i);
        }
    }
}

impl<T> Network<T> for Segmented<T> {
    fn send(&mut self, from: usize, to: usize, size_bytes: usize, payload: T) {
        let dir = match (from, to) {
            (0, 1) => Dir::Forward,
            (1, 0) => Dir::Reverse,
            _ => {
                self.unroutable = self.unroutable.saturating_add(1);
                return;
            }
        };
        self.run_until(self.now);
        self.main.sent = self.main.sent.saturating_add(1);
        self.enter(
            Hop {
                payload,
                size: size_bytes,
                route: Route::Main(dir),
                step: 0,
            },
            self.now,
        );
        self.run_until(self.now);
    }

    /// 下一次要处理的时刻：最早的送达，或者更早的段间转交（推进到那里不一定有送达）。
    fn peek_time(&self) -> Option<Time> {
        let out = self.out.keys().next().map(|(t, _)| *t);
        let transfer = self.next_transfer().map(|(t, _)| t);
        match (out, transfer) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }

    fn pop(&mut self) -> Option<Delivery<T>> {
        // 比最早的送达还早（或同时）的转交可能产生更早的送达，先做掉
        while let Some((t, i)) = self.next_transfer() {
            if self.out.keys().next().is_some_and(|(h, _)| *h < t) {
                break;
            }
            self.transfer(i);
        }
        let (_, d) = self.out.pop_first()?;
        self.now = self.now.max(d.at);
        Some(d)
    }

    fn advance_to(&mut self, t: Time) {
        let t = self.now.max(t);
        self.run_until(t);
        for p in &mut self.segments {
            p.advance_to(t);
        }
        self.now = t;
    }

    fn in_flight(&self) -> usize {
        self.out.len() + self.segments.iter().map(Path::in_flight).sum::<usize>()
    }

    /// 一个方向五段瓶颈的排队之和：现在发一个包，大约要在各段排这么久。
    fn backlog_us(&self, from: usize, to: usize) -> u64 {
        if !matches!((from, to), (0, 1) | (1, 0)) {
            return 0;
        }
        self.segments
            .iter()
            .map(|p| p.backlog_us(from, to))
            .fold(0u64, u64::saturating_add)
    }

    fn stats(&self) -> NetStats {
        NetStats {
            sent: self.main.sent,
            delivered: self.main.delivered,
            lost: self.main.lost,
            queue_dropped: self.main.queue_dropped,
            unroutable: self.unroutable,
        }
    }

    fn next_event_time(&self) -> Option<Time> {
        self.pending_events.first().copied()
    }

    fn pop_event(&mut self) -> Option<PathEvent> {
        // 各段自己的事件不往外发（同一次换路几段各发一个），统一在这里按恢复时刻发一次
        for p in &mut self.segments {
            while p.pop_event().is_some() {}
        }
        let at = *self.pending_events.first().filter(|t| **t <= self.now)?;
        let _ = self.pending_events.remove(0);
        Some(PathEvent::RouteChanged { at })
    }
}
