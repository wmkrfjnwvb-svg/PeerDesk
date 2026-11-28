//! 仿真主循环：一对或几对端点共享一个网络（`docs/research/Q-redesign/a2-plan.md` 第 5 节“A2-4 的设计”第 1 条）。
//!
//! # 一对端点
//!
//! [`Pair`] 是一次会话的全部状态：被控端（发送端、节奏器、画面）、控制端（接收端）、统计。它的方法对应主循环里的各段：
//! 阶段开始、帧开始（渲染、编码、发出）、醒来（定时任务、发出、反馈）、收到一个包、帧末、阶段结束、收尾。
//! 每一对用自己的时钟：本地时间 = 全局时间 − 这一对的开始时刻。只有一对、从 0 开始时，本地时间就是全局时间，
//! 每一步的调用顺序和拆分之前的单个会话逐行相同（结果指纹逐字节不变，这是这一步的门槛）。
//!
//! # 全局循环
//!
//! 每一轮取最早的一件事：网络的送达或事件、某一对的定时器（反馈、网络控制器的定时任务、节奏器）、某一对的帧边界
//! （帧末，或者还没开始的一对的开始时刻）。
//! - 先给每一对记账（码率控制状态的时间、码率停留的时间），再把网络推进到这个时刻；
//! - 有帧边界在这个时刻：按对的顺序做帧末（和阶段结束、下一阶段开始）、下一帧的开始，这一轮就到这里；
//!   同一时刻的定时器和送达在下一轮做（和拆分之前一样：一帧的定时器只看早于帧末的，帧末那一刻的留给下一帧）；
//! - 否则是醒来：发出“路径变了”事件，每一对做定时任务、发出、反馈，再把到了的包按对号交给各对。
//!
//! 网络按时间表换参数（场景的 `network`）在帧开始时做（任何一对的帧开始；只有一对时和以前一样）。
//!
//! # 几对共享一个网络
//!
//! 数据报外面带对号（[`Packet`]）。每一对的被控端在节点 0 还是 1 由 [`PairSpec::reverse`] 定（RFC 8867 5.3 的双向媒体）；
//! [`PairSpec::extra_delay_us`] 是这一对自己的额外单程延迟（5.5 的 RTT 公平），两个方向都加，加在共享网络之后
//! （固定延迟不改同一对的包的先后）。共享网络的计数（`Outcome::net`）是所有对合在一起的。

use std::collections::BTreeMap;

use super::*;
use peerdesk_core::sim::Delivery;

/// 共享网络里的一个数据报：属于哪一对，内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Packet {
    /// 对号。
    pub pair: usize,
    /// 数据报。
    pub bytes: Vec<u8>,
}

/// 几对共享的网络中的一对。
#[derive(Debug, Clone)]
pub struct PairSpec {
    /// 这一对的会话：阶段、编码、码控、种子……它的 `network` 不用（网络是共享的，见 [`MultiScenario::network`]）。
    pub scenario: Scenario,
    /// 什么时候开始（微秒，全局时间）。
    pub start_us: u64,
    /// 被控端在节点 1（画面走反向）。
    pub reverse: bool,
    /// 这一对额外的单程延迟（微秒），两个方向都加。
    pub extra_delay_us: u64,
}

/// 几对端点共享一个网络的场景。
#[derive(Debug, Clone)]
pub struct MultiScenario {
    /// 共享的网络（交叉流、换路都在这里）。
    pub network: NetworkPlan,
    /// 网络的种子。
    pub seed: u64,
    /// 各对。
    pub pairs: Vec<PairSpec>,
}

/// 跑几对共享一个网络的会话，按 [`MultiScenario::pairs`] 的顺序返回每一对的结果。
///
/// # Errors
/// 某一对的帧率或阶段为空、编码器建不起来，或会话里出错。
pub fn run_multi(m: &MultiScenario) -> Result<Vec<Outcome>, Error> {
    let specs: Vec<Spec<'_>> = m
        .pairs
        .iter()
        .map(|p| Spec {
            s: &p.scenario,
            start: Time(p.start_us),
            reverse: p.reverse,
            extra: p.extra_delay_us,
        })
        .collect();
    let out = with_network(&m.network, m.seed, &specs, false)?;
    // 每一对一行指纹（`PEERDESK_E2E_FINGERPRINTS` 设了才写）：身份用这一对自己的场景，名字要各不相同
    for (p, f) in m.pairs.iter().zip(&out) {
        note_fingerprint(&p.scenario, &f.outcome);
    }
    Ok(out.into_iter().map(|f| f.outcome).collect())
}

/// 一对跑完的结果（内部用）：结果、会话记录、墙上耗时、仿真结束的本地时刻。
pub(crate) struct Finished {
    pub(crate) outcome: Outcome,
    pub(crate) recorded: Option<Vec<u8>>,
    pub(crate) wall: std::time::Duration,
    pub(crate) end: Time,
}

/// 一对的参数（内部用）。
pub(crate) struct Spec<'a> {
    pub(crate) s: &'a Scenario,
    pub(crate) start: Time,
    pub(crate) reverse: bool,
    pub(crate) extra: u64,
}

/// 按网络的种类建网络，跑完全部对。
pub(crate) fn with_network(
    plan: &NetworkPlan,
    seed: u64,
    specs: &[Spec<'_>],
    record: bool,
) -> Result<Vec<Finished>, Error> {
    let extra: Vec<u64> = specs.iter().map(|p| p.extra).collect();
    match plan {
        NetworkPlan::Layout1(links) => drive(
            Shared::new(Net::new(seed), extra),
            links.clone(),
            specs,
            record,
        ),
        NetworkPlan::Layout2(plan) => {
            let mut path = Path::new(seed, plan.start.clone());
            for change in &plan.routes {
                path.schedule_route_change(change.clone());
            }
            for (dir, spec) in &plan.cross {
                let _ = path.add_cross(*dir, spec.clone());
            }
            drive(Shared::new(path, extra), plan.steps.clone(), specs, record)
        }
        NetworkPlan::Segmented(plan) => {
            let mut net = Segmented::new(seed, plan.start.clone());
            for change in &plan.routes {
                net.schedule_route_change(change.clone());
            }
            let mut ids = Vec::new();
            for (segment, dir, spec) in &plan.cross {
                ids.push((*segment, net.add_cross(*segment, *dir, spec.clone())));
            }
            drive(
                Shared::new(SegmentedWire { net, cross: ids }, extra),
                plan.steps.clone(),
                specs,
                record,
            )
        }
    }
}

/// 共享网络：里面的网络加每一对的额外延迟。额外延迟都是 0 时，送达和里面的网络完全一样。
pub(crate) struct Shared<N> {
    inner: N,
    extra: Vec<u64>,
    /// 加了额外延迟、还没到的包：（到达时刻，序号）。
    delayed: BTreeMap<(Time, u64), Delivery<Packet>>,
    next_id: u64,
}

impl<N: Wire> Shared<N> {
    fn new(inner: N, extra: Vec<u64>) -> Self {
        Self {
            inner,
            extra,
            delayed: BTreeMap::new(),
            next_id: 0,
        }
    }

    fn delayed_head(&self) -> Option<Time> {
        self.delayed.keys().next().map(|(t, _)| *t)
    }
}

impl<N: Wire> Network<Packet> for Shared<N> {
    fn send(&mut self, from: usize, to: usize, size_bytes: usize, payload: Packet) {
        self.inner.send(from, to, size_bytes, payload);
    }
    fn peek_time(&self) -> Option<Time> {
        match (self.inner.peek_time(), self.delayed_head()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        }
    }
    /// 取出不晚于进来时 `peek_time` 的那一个送达。从里面的网络取出的包要加额外延迟时，挪进延迟队列；
    /// 延迟队列的队首晚于这个时刻就返回 `None`（调用方等到新的 `peek_time` 再来取），决不提前交出去
    /// （2026-11-28 主会话审查发现：以前会把加了延迟的包当场交出去，稀疏的反馈方向上额外延迟等于没加）。
    fn pop(&mut self) -> Option<Delivery<Packet>> {
        let limit = self.peek_time()?;
        loop {
            let inner = self.inner.peek_time();
            let held = self.delayed_head();
            // 两边都只取不晚于 limit 的：挪进延迟队列之后，里面的网络的下一个包也可能已经晚于 limit
            let inner = inner.filter(|t| *t <= limit);
            let held = held.filter(|t| *t <= limit);
            let take_inner = match (inner, held) {
                (Some(a), Some(b)) => a <= b,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => return None,
            };
            if !take_inner {
                return self.delayed.pop_first().map(|(_, d)| d);
            }
            let d = self.inner.pop()?;
            let extra = self.extra.get(d.payload.pair).copied().unwrap_or(0);
            if extra == 0 {
                return Some(d);
            }
            let at = d.at.plus(extra);
            let id = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
            self.delayed.insert((at, id), Delivery { at, ..d });
        }
    }
    fn advance_to(&mut self, t: Time) {
        self.inner.advance_to(t);
    }
    fn in_flight(&self) -> usize {
        self.inner.in_flight() + self.delayed.len()
    }
    fn backlog_us(&self, from: usize, to: usize) -> u64 {
        self.inner.backlog_us(from, to)
    }
    fn stats(&self) -> NetStats {
        self.inner.stats()
    }
    fn path_stats(&self) -> Option<PathStats> {
        self.inner.path_stats()
    }
    fn next_event_time(&self) -> Option<Time> {
        self.inner.next_event_time()
    }
    fn pop_event(&mut self) -> Option<peerdesk_core::path::PathEvent> {
        self.inner.pop_event()
    }
}

/// 一对的帧时钟在哪儿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clock {
    /// 还没开始：在这个全局时刻开始。
    Before(Time),
    /// 在一帧里：这一帧在这个本地时刻结束。
    InFrame(Time),
    /// 全部阶段做完了。
    Done,
}

/// 一对端点的全部状态。字段和拆分之前的单个会话的局部变量一一对应。
struct Pair<'a> {
    s: &'a Scenario,
    index: usize,
    host: usize,
    viewer: usize,
    start: Time,
    extra: u64,
    interval: u64,
    profile: Stopwatch,
    stage: Box<dyn Stage>,
    tx: MediaSender,
    rx: MediaReceiver,
    wall: std::time::Instant,
    route_changes: Vec<u64>,
    seg_queue: Vec<(u128, u64)>,
    captured: Vec<Time>,
    results: Vec<PhaseResult>,
    seq: u32,
    fb_seq: u32,
    overall_wait: u64,
    features: Vec<FeatureRow>,
    trace_events: bool,
    trace: Vec<String>,
    gate: (u32, u32, u32),
    waiting_since: Option<Time>,
    first_picture: Option<u64>,
    first: bool,
    pending: bool,
    overall_freezes: u32,
    states: StateShare,
    counter: Counter,
    journal: Option<Journal>,
    /// 本地时间。
    now: Time,
    next_feedback: Time,
    video_since_feedback: u32,
    reported_floor: u32,
    last_feedback: Time,
    next_tick: Time,
    pacer: peerdesk_core::pacer::Pacer,
    pacing: bool,
    clock: Clock,
    /// 第几个阶段、这个阶段还剩几帧。
    phase: usize,
    frames_left: u32,
    acc: Option<PhaseAcc>,
    outcome: Option<Finished>,
    received_bytes: u64,
    first_received: Option<Time>,
    flow: flow::FlowLog,
}

impl<'a> Pair<'a> {
    fn new(spec: &Spec<'a>, index: usize, record: bool) -> Result<Self, Error> {
        let s = spec.s;
        if s.fps == 0 || s.phases.is_empty() {
            return Err(Error::InvalidArgument("帧率和阶段都不能为空"));
        }
        let interval = 1_000_000 / u64::from(s.fps);
        let mut profile = Stopwatch::from_env();
        let tag = Arc::new(AtomicU64::new(0));
        let feed = ChangeFeed::default();
        let (encoder, decoder) = s.codec.make(s, &tag, &feed)?;
        // L1 在收包的当场解码；L0.5、L0 走控制端用的延后解码（解码器交给量尺里的 DecodeWorker）
        let (stage, inline): (Box<dyn Stage>, Option<Box<dyn VideoDecoder>>) = match s.codec {
            CodecKind::H264L0 => {
                // 导出时间线要画一遍像素（有缓存）；它不算进会话的计时，单独报
                let preparing = profile.start();
                let timeline = timeline::Timeline::cached(s.width, s.height, s.seed, &s.phases)?;
                profile.prepared = preparing.map(|t| t.elapsed()).unwrap_or_default();
                (
                    Box::new(TimelineStage {
                        timeline,
                        next: 0,
                        content: None,
                        feed,
                        worker: DecodeWorker::new(decoder),
                        shown: None,
                    }),
                    None,
                )
            }
            CodecKind::H264L05 => (
                Box::new(DescribedStage {
                    desk: RealisticDesktop::new(s.width, s.height, s.seed)?,
                    feed,
                    version: 0,
                    tag,
                    worker: DecodeWorker::new(decoder),
                    shown: None,
                }),
                None,
            ),
            CodecKind::Tile | CodecKind::H264 => (
                Box::new(PixelStage {
                    desk: RealisticDesktop::new(s.width, s.height, s.seed)?,
                    shown: None,
                }),
                Some(decoder),
            ),
        };
        let wall = std::time::Instant::now();
        let cfg = SenderConfig {
            fps: s.fps,
            start_bps: s.start_bps,
            max_bps: s.max_bps,
            profile: s.profile,
            estimator: s.estimator,
            policy: s.policy,
            ..SenderConfig::default()
        };
        let mut tx = MediaSender::new(cfg, encoder);
        let mut rx = match inline {
            Some(decoder) => MediaReceiver::new(Time::ZERO, cfg.deadline_us, decoder),
            None => MediaReceiver::detached(Time::ZERO, cfg.deadline_us),
        };
        rx.set_keyframe_grace(s.codec.is_h264());
        rx.set_fec(s.fec);
        tx.set_fec(s.fec);
        // 网络控制器要到达报告时两端都打开（产品里由握手的 TRANSPORT_CC 决定）
        tx.set_reports(true);
        rx.set_reports(tx.reports());
        // 静止时探测（产品里由握手的 PROBE_TRAIN 决定；新版本之间都打开）
        tx.set_idle_probe(true);
        tx.set_start_probe(s.start_probe);
        tx.set_probe_ids(s.probe_ids);
        rx.set_probe_ids(s.probe_ids);
        let journal = record.then(|| Journal::new(s, tx.estimator()));
        let (host, viewer) = if spec.reverse { (1, 0) } else { (0, 1) };
        Ok(Self {
            s,
            index,
            host,
            viewer,
            start: spec.start,
            extra: spec.extra,
            interval,
            profile,
            stage,
            tx,
            rx,
            wall,
            route_changes: Vec::new(),
            // 分段路径每段正向排队：（时间加权的和 微秒·微秒，最大 微秒），按帧末采样
            seg_queue: Vec::new(),
            // 每一帧的采集时间，按帧号。
            captured: vec![Time::ZERO],
            results: Vec::new(),
            seq: 0,
            fb_seq: 0,
            overall_wait: 0,
            features: Vec::new(),
            trace_events: s.trace,
            trace: Vec::new(),
            // 调试：每秒里因为哪个闸门没编帧（等关键帧、在途满、节奏器忙）
            gate: (0, 0, 0),
            // 源画面变了、控制端还没看到新画面：从哪个时刻开始等的。
            waiting_since: None,
            // 会话开始到看到第一幅画面用了多久。
            first_picture: None,
            first: true,
            // 画面变了但还没编（在途数据满了）。
            pending: false,
            overall_freezes: 0,
            states: StateShare::default(),
            counter: Counter::default(),
            journal,
            now: Time::ZERO,
            next_feedback: Time(FEEDBACK_US),
            // 上次反馈以来收到的画面包数、上次报告的 floor、上次反馈的时间（反馈节奏和产品一样，见 IDLE_FEEDBACK_US）
            video_since_feedback: 0,
            reported_floor: 0,
            last_feedback: Time::ZERO,
            next_tick: Time::ZERO,
            pacer: peerdesk_core::pacer::Pacer::new(Time(0)),
            pacing: s.pacing,
            clock: Clock::Before(spec.start),
            phase: 0,
            frames_left: 0,
            acc: None,
            outcome: None,
            received_bytes: 0,
            first_received: None,
            flow: flow::FlowLog::default(),
        })
    }

    /// 全局时间 → 本地时间。
    fn local(&self, t: Time) -> Time {
        Time(t.micros().saturating_sub(self.start.micros()))
    }

    /// 本地时间 → 全局时间。
    fn global(&self, t: Time) -> Time {
        t.plus(self.start.micros())
    }

    const fn active(&self) -> bool {
        matches!(self.clock, Clock::InFrame(_))
    }

    /// 下一个帧边界（全局时刻）。
    fn boundary(&self) -> Option<Time> {
        match self.clock {
            Clock::Before(t) => Some(t),
            Clock::InFrame(end) => Some(self.global(end)),
            Clock::Done => None,
        }
    }

    /// 这一帧里最早的定时器（全局时刻）；只算早于帧末的（帧末那一刻的留给下一帧）。
    fn next_timer(&self) -> Option<Time> {
        let Clock::InFrame(end) = self.clock else {
            return None;
        };
        [
            Some(self.next_feedback),
            Some(self.next_tick),
            self.pacer
                .next_due(self.now, self.tx.pacing_bps(self.pacing)),
        ]
        .into_iter()
        .flatten()
        .min()
        .filter(|t| *t < end)
        .map(|t| self.global(t))
    }

    /// 记账到全局时刻 `when`（推进网络之前调用：真实排队用的是推进之前的值，和拆分之前一样）。
    fn account(&mut self, net: &impl Network<Packet>, when: Time) {
        if !self.active() {
            return;
        }
        let when = self.local(when);
        self.states.add(
            self.tx.rate_state(),
            net.backlog_us(self.host, self.viewer),
            when.since(self.now),
        );
        self.counter.held(self.tx.rate_bps(), when.since(self.now));
    }

    /// 网络推进到全局时刻 `when` 以后，本地时钟跟上。
    fn arrive_at(&mut self, when: Time) {
        if self.active() {
            self.now = self.local(when);
        }
    }

    /// 一个阶段开始；空的阶段（0 帧）当场结束，直到有帧的阶段或全部做完。
    fn begin_phase(&mut self) {
        loop {
            let Some(phase) = self.s.phases.get(self.phase).copied() else {
                self.clock = Clock::Done;
                return;
            };
            let mut acc = PhaseAcc::new(self.now);
            // 阶段开始时画面已经清晰（例如上一阶段停下来之后补清晰了）。
            let gauging = self.profile.start();
            if self.stage.sharp() {
                acc.sharp_at = Some(self.now);
            }
            self.profile.stop(Part::Gauge, gauging);
            self.acc = Some(acc);
            self.frames_left = phase.frames;
            if phase.frames > 0 {
                return;
            }
            self.end_phase();
            self.phase += 1;
        }
    }

    /// 帧开始：渲染、决定编不编、编码、交给节奏器发出。`steps` 是网络按时间表换参数（在帧开始时做）。
    fn begin_frame<N: Wire>(
        &mut self,
        net: &mut Shared<N>,
        steps: &mut Steps<N::Step>,
    ) -> Result<(), Error> {
        let Some(phase) = self.s.phases.get(self.phase).copied() else {
            self.clock = Clock::Done;
            return Ok(());
        };
        let now = self.now;
        self.clock = Clock::InFrame(now.plus(self.interval));
        self.frames_left = self.frames_left.saturating_sub(1);
        // 被控端：这一帧。
        steps.apply_due(&mut net.inner, self.global(now));
        // 第一帧总要发：控制端还什么都没有。
        let rendering = self.profile.start();
        let moved = self.stage.step(phase.action)?;
        self.profile.stop(Part::Render, rendering);
        let changed = moved || std::mem::take(&mut self.first);
        if changed && self.waiting_since.is_none() {
            self.waiting_since = Some(now);
        }
        self.pending |= changed;
        // 和产品一样：画面变了就告诉发送端（编不编这一帧另说），判断画面是不是停下了
        if changed {
            self.tx.screen_changed(now);
        }
        // 在途数据超过上限，或上一帧还没发完（发送队列最多放一帧）：这一帧不编，
        // 变化攒着，等发完、确认之后编最新的画面。
        // 和产品一样每一轮都检查迟迟没有结论的帧（整个关键帧都被瓶颈丢掉时，接收端不知道有这一帧，
        // 只能靠发送端超时判丢；只在编帧时检查的话，等关键帧时不编帧，就永远等下去）。
        self.tx.expire_unacked(now);
        let frame_us = 1_000_000 / u64::from(self.s.fps.max(1));
        if self.trace_events && (self.pending || self.tx.wants_frame()) && self.tx.due(now) {
            if self.tx.key_pending() {
                self.gate.0 += 1;
            } else if self.tx.in_flight_full(now) {
                self.gate.1 += 1;
            } else if self.pacer.busy(self.tx.pacing_bps(self.pacing), frame_us) {
                self.gate.2 += 1;
            }
        }
        let wanted = self.pending || self.tx.wants_frame();
        let pacer_busy = self.pacer.busy(self.tx.pacing_bps(self.pacing), frame_us);
        if self.tx.frame_gate(now, wanted, pacer_busy) {
            let rtt = net
                .inner
                .nominal_rtt_us()
                .saturating_add(self.extra.saturating_mul(2));
            let before = self.tx.stats();
            let changed = std::mem::take(&mut self.pending);
            let encoding = self.profile.start();
            let out = self.stage.encode(&mut self.tx, now, changed, rtt)?;
            self.counter.after(self.tx.rate_bps(), self.tx.fps());
            self.profile.stop(Part::Encode, encoding);
            if let Some(j) = self.journal.as_mut() {
                j.frame(now, &self.tx, before, changed);
            }
            let before = before.frames;
            if self.tx.stats().frames != before {
                self.captured.push(now);
                if self.trace_events {
                    let bytes: usize = out.iter().map(|o| o.body.len()).sum();
                    if bytes > 4_000 {
                        self.trace.push(format!(
                            "[{:>6} ms] 大帧 {bytes} B（关键帧累计 {}），码率 {} kbps",
                            now.micros() / 1000,
                            self.tx.stats().keyframes,
                            self.tx.rate_bps() / 1000
                        ));
                    }
                }
            }
            // 和产品一样经发送节奏控制发出（见 peerdesk_core::pacer）
            self.pacer.push(out);
            self.send(net)?;
        }
        Ok(())
    }

    /// 节奏器里到时间的包发出去，记进这个阶段的字节。
    fn send(&mut self, net: &mut impl Network<Packet>) -> Result<(), Error> {
        let sending = self.profile.start();
        let mut tagged = Tagged {
            net,
            pair: self.index,
            host: self.host,
            viewer: self.viewer,
            packets: 0,
        };
        let wire = send_ready(
            &mut self.pacer,
            &mut self.tx,
            self.pacing,
            &mut tagged,
            self.now,
            &mut self.seq,
        )?;
        if tagged.packets > 0 {
            self.flow.sent(self.now, tagged.packets, wire);
        }
        if let Some(acc) = self.acc.as_mut() {
            acc.bytes = acc.bytes.saturating_add(wire);
        }
        self.counter.sent(self.now, wire);
        self.profile.stop(Part::Send, sending);
        Ok(())
    }

    /// “路径变了”事件。
    fn route_event(&mut self, at: Time) {
        if !self.active() {
            return;
        }
        self.route_changes.push(at.micros());
        route_changed(&mut self.tx, &mut self.rx, at);
    }

    /// 醒来：网络控制器的定时任务、发出、该发反馈就发。
    fn wake(&mut self, net: &mut impl Network<Packet>) -> Result<(), Error> {
        if !self.active() {
            return Ok(());
        }
        let now = self.now;
        let controlling = self.profile.start();
        if self.next_tick <= now {
            self.tx.on_tick(now);
            self.counter.after(self.tx.rate_bps(), self.tx.fps());
            for cluster in self.tx.take_probes() {
                self.pacer.push_probe(cluster);
            }
            self.next_tick = now.plus(CC_TICK_US);
            if self.trace_events && now.micros() % 1_000_000 < CC_TICK_US {
                let line = self
                    .tx
                    .controller_line()
                    .unwrap_or_else(|| format!("码率 {} kbps", self.tx.rate_bps() / 1000));
                self.trace.push(format!(
                    "[{:>6} ms] {line}；节奏器排队 {} B，在途 {} B；这一秒不编：等关键帧 {}、在途满 {}、节奏器忙 {}",
                    now.micros() / 1000,
                    self.pacer.queued_bytes(),
                    self.tx.in_flight(),
                    self.gate.0,
                    self.gate.1,
                    self.gate.2
                ));
                self.gate = (0, 0, 0);
                let st = self.tx.stats();
                self.trace.push(format!(
                    "           发送端累计：帧 {}，关键帧 {}，判丢 {}，重传 {}（重传请求 {}，没到时间跳过 {}，原包没发 {}，超额没发 {}）",
                    st.frames, st.keyframes, st.lost, st.retransmitted, st.nack_requests, st.resend_skipped, st.resend_unsent, st.resend_capped
                ));
            }
        }
        self.profile.stop(Part::Control, controlling);
        self.send(net)?;
        let controlling = self.profile.start();
        let feedback_due = self.next_feedback <= now && {
            self.next_feedback = now.plus(FEEDBACK_US);
            let (floor, _) = self.rx.settle(now);
            let news = self.video_since_feedback > 0
                || floor != self.reported_floor
                || self.rx.wants_nack(now);
            let due = news || now.since(self.last_feedback) >= IDLE_FEEDBACK_US;
            if due {
                self.video_since_feedback = 0;
                self.reported_floor = floor;
                self.last_feedback = now;
            }
            due
        };
        if feedback_due {
            let fb = self.rx.feedback_datagram(now);
            let header = Header {
                channel: Channel::Control,
                flags: 0,
                conn_id: 0,
                seq: self.fb_seq,
                send_time: now,
            };
            self.fb_seq = self.fb_seq.wrapping_add(1);
            let datagram = encode_datagram(&header, &fb)?;
            self.flow
                .feedback(u64::try_from(datagram.len()).unwrap_or(u64::MAX));
            net.send(
                self.viewer,
                self.host,
                datagram.len(),
                Packet {
                    pair: self.index,
                    bytes: datagram,
                },
            );
        }
        self.profile.stop(Part::Control, controlling);
        Ok(())
    }

    /// 收到一个包（已经解出数据报头）。
    fn deliver<N: Wire>(
        &mut self,
        net: &mut Shared<N>,
        to: usize,
        size: usize,
        header: &Header,
        body: &[u8],
    ) -> Result<(), Error> {
        if !self.active() {
            return Ok(());
        }
        let now = self.now;
        if to == self.host {
            // 反馈；顺便按请求重传丢了的分片。
            let controlling = self.profile.start();
            let again = self.tx.on_feedback_datagram(now, body).unwrap_or_default();
            self.counter.after(self.tx.rate_bps(), self.tx.fps());
            let tx = &self.tx;
            self.pacer.retain(|o| tx.wanted(o));
            self.pacer.push_front(again);
            self.profile.stop(Part::Control, controlling);
            self.send(net)?;
            if self.trace_events {
                for e in self.tx.take_events() {
                    self.trace
                        .push(format!("[{:>6} ms] {e}", now.micros() / 1000));
                }
            }
            if let Some(obs) = self.tx.last_observation() {
                if let Some(j) = self.journal.as_mut() {
                    j.feedback(now, &self.tx, &obs);
                }
                self.features.push(FeatureRow {
                    t_ms: now.micros() / 1000,
                    rate_bps: self.tx.rate_bps(),
                    queue_us: obs.feedback.queue_delay_us,
                    trend: obs.ext.trend_tenth_ms,
                    train: obs.ext.train_bytes_per_s,
                    loss: obs.feedback.loss_permille,
                    recv: obs.feedback.recv_bytes_per_s,
                    in_flight: obs.in_flight,
                    true_rate_bps: net.inner.true_rate_bps(),
                    true_queue_packets: net.in_flight(),
                    true_queue_us: net.backlog_us(self.host, self.viewer),
                    state: self.tx.rate_state(),
                });
            }
            return Ok(());
        }
        self.video_since_feedback = self.video_since_feedback.saturating_add(1);
        let size = u64::try_from(size).unwrap_or(u64::MAX);
        self.received_bytes = self.received_bytes.saturating_add(size);
        self.first_received.get_or_insert(now);
        // 单程延迟：两端用的都是这一对的本地时钟
        self.flow.received(now, size, now.since(header.send_time));
        let receiving = self.profile.start();
        let before = self.rx.stats();
        let got = self.stage.receive(&mut self.rx, now, header, body);
        // 这个包让几帧收齐了：每帧到齐要多久（收齐的帧数和用时之和的增量；一次收齐几帧时取平均）
        let after = self.rx.stats();
        let frames = after.assembled.saturating_sub(before.assembled);
        if frames > 0 {
            let us = after.assembly_us_sum.saturating_sub(before.assembly_us_sum);
            for _ in 0..frames {
                self.flow.assembled(us / u64::from(frames));
            }
        }
        self.profile.stop(
            if got.is_some() {
                Part::Decode
            } else {
                Part::Reassemble
            },
            receiving,
        );
        let Some(frame_id) = got else {
            return Ok(());
        };
        let at = self
            .captured
            .get(usize::try_from(frame_id).unwrap_or(usize::MAX))
            .copied()
            .unwrap_or(now);
        let Some(acc) = self.acc.as_mut() else {
            return Ok(());
        };
        acc.latencies.push(now.since(at));
        acc.shown += 1;
        // 等待到“包含这次变化的画面”显示出来才算结束：采集时间早于变化的帧（例如变化之前编的补清晰帧
        // 还在路上）显示出来时，用户看到的还是旧画面。以前任何一帧显示就算结束，切换窗口时一个 100 KB 的帧
        // 要传 700 ms，前面一个补清晰帧先到，就只记了 169 ms（2026-11 微软轨迹 ms-09401 实测）。
        let done = self.waiting_since.filter(|since| at >= *since);
        if done.is_some() {
            self.waiting_since = None;
        }
        if let Some(since) = done {
            let wait = now.since(since);
            // 第一幅画面单独记（会话开始时的等待），不算卡顿。
            if self.first_picture.is_none() {
                self.first_picture = Some(wait);
            } else {
                acc.max_wait = acc.max_wait.max(wait);
                if wait > FREEZE_US {
                    acc.freezes += 1;
                    if self.trace_events {
                        self.trace.push(format!(
                            "[{:>6} ms] 冻结：画面在 {} ms 变了，等了 {} ms 才显示出来",
                            now.micros() / 1000,
                            since.micros() / 1000,
                            wait / 1000
                        ));
                    }
                }
            }
        }
        let gauging = self.profile.start();
        if acc.sharp_at.is_none() && self.stage.sharp() {
            acc.sharp_at = Some(now);
        }
        self.profile.stop(Part::Gauge, gauging);
        Ok(())
    }

    /// 帧边界到了：还没开始的一对开始；在一帧里的做帧末（和阶段结束），再开始下一帧。
    fn on_boundary<N: Wire>(
        &mut self,
        net: &mut Shared<N>,
        steps: &mut Steps<N::Step>,
    ) -> Result<(), Error> {
        match self.clock {
            Clock::Before(_) => {
                self.clock = Clock::InFrame(Time::ZERO);
                self.now = Time::ZERO;
                self.begin_phase();
            }
            Clock::InFrame(_) => {
                self.end_frame(net);
                if self.frames_left == 0 {
                    self.end_phase();
                    self.phase += 1;
                    self.begin_phase();
                }
            }
            Clock::Done => return Ok(()),
        }
        if self.clock == Clock::Done {
            self.finish(net);
            return Ok(());
        }
        self.begin_frame(net, steps)
    }

    /// 帧末：每段排队采样、记录仪的秒记录（时钟已经推进到帧末）。
    fn end_frame<N: Wire>(&mut self, net: &Shared<N>) {
        self.flow.sample(
            self.now,
            net.inner.true_rate_bps(),
            net.backlog_us(self.host, self.viewer),
        );
        let queues = net.inner.segment_queues_us();
        self.seg_queue.resize(queues.len(), (0, 0));
        for ((sum, max), q) in self.seg_queue.iter_mut().zip(queues) {
            *sum = sum.saturating_add(u128::from(q) * u128::from(self.interval));
            *max = (*max).max(q);
        }
        if let Some(j) = self.journal.as_mut() {
            j.second(self.now, self.tx.stats());
        }
    }

    /// 阶段结束：这一阶段的结果。
    fn end_phase(&mut self) {
        let Some(phase) = self.s.phases.get(self.phase).copied() else {
            return;
        };
        let Some(mut acc) = self.acc.take() else {
            return;
        };
        let now = self.now;
        // 阶段结束时还在等（第一幅画面已经出来之后）：等到现在的时间也算进去。
        if let Some(since) = self.waiting_since.filter(|_| self.first_picture.is_some()) {
            let wait = now.since(since);
            acc.max_wait = acc.max_wait.max(wait);
            if wait > FREEZE_US && acc.max_wait == wait {
                acc.freezes += 1;
            }
        }
        let gauging = self.profile.start();
        let (end_psnr, snapshot) = self.stage.snapshot();
        self.profile.stop(Part::Gauge, gauging);
        let duration = now.since(acc.start).max(1);
        let mut sorted = acc.latencies.clone();
        sorted.sort_unstable();
        self.overall_wait = self.overall_wait.max(acc.max_wait);
        self.overall_freezes += acc.freezes;
        self.results.push(PhaseResult {
            action: phase.action,
            bytes: acc.bytes,
            kbps: acc.bytes.saturating_mul(8_000) / duration,
            frames_shown: acc.shown,
            latency_ms: (ms(percentile(&sorted, 50)), ms(percentile(&sorted, 95))),
            max_wait_ms: ms(acc.max_wait),
            freezes: acc.freezes,
            end_psnr,
            sharp_ms: acc
                .sharp_at
                .filter(|_| phase.action == Action::Idle)
                .map(|t| ms(t.since(acc.start))),
            snapshot,
        });
    }

    /// 全部阶段做完：结果。
    fn finish<N: Wire>(&mut self, net: &Shared<N>) {
        let s = self.s;
        let now = self.now;
        let (tx_stats, rx_stats) = (self.tx.stats(), self.rx.stats());
        let results = std::mem::take(&mut self.results);
        let sent_bytes = results.iter().map(|p| p.bytes).sum();
        let recorded = self.journal.take().map(|j| j.finish(now, tx_stats));
        let outcome = Outcome {
            name: s.name.clone(),
            codec: s.codec,
            phases: results,
            frames: tx_stats.frames,
            keyframes: tx_stats.keyframes,
            recoveries: tx_stats.lost,
            skipped: rx_stats.skipped,
            bad: rx_stats.bad,
            net: net.stats(),
            max_wait_ms: ms(self.overall_wait),
            first_picture_ms: self.first_picture.map(ms),
            freezes: self.overall_freezes,
            retransmitted: tx_stats.retransmitted,
            states: self.states,
            probes: tx_stats.probes,
            sent_bytes,
            estimator: self.tx.estimator(),
            features: std::mem::take(&mut self.features),
            tally: std::mem::take(&mut self.counter).finish(now),
            path: net.path_stats(),
            route_changes: std::mem::take(&mut self.route_changes),
            cross: net.inner.cross(),
            segments: net
                .inner
                .segment_stats()
                .into_iter()
                .zip(&self.seg_queue)
                .map(|(stats, (sum, max))| SegmentTruth {
                    stats,
                    // 和 ms() 同一个口径：微秒 → 毫秒
                    mean_queue_ms: *sum as f64 / now.micros().max(1) as f64 / 1000.0,
                    max_queue_ms: ms(*max),
                })
                .collect(),
            trace: std::mem::take(&mut self.trace),
            received_bytes: self.received_bytes,
            first_received_us: self.first_received.map(Time::micros),
            flow: std::mem::take(&mut self.flow),
        };
        if self.profile.on {
            use std::io::Write as _;
            // 测量输出走标准错误，一次写一整行（并行的测试不互相打断）
            let line = self.profile.line(&s.name, self.wall.elapsed(), now);
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        }
        self.outcome = Some(Finished {
            outcome,
            recorded,
            wall: self.wall.elapsed(),
            end: now,
        });
    }
}

/// 网络按时间表换参数。
pub(crate) struct Steps<S> {
    list: Vec<(u64, S)>,
    next: usize,
}

impl<S> Steps<S> {
    fn new(mut list: Vec<(u64, S)>) -> Self {
        list.sort_by_key(|(t, _)| *t);
        Self { list, next: 0 }
    }

    /// 把时刻不晚于 `now` 的都换上。
    fn apply_due<N: Wire<Step = S>>(&mut self, net: &mut N, now: Time) {
        while self
            .list
            .get(self.next)
            .is_some_and(|(t, _)| Time(*t) <= now)
        {
            if let Some((_, step)) = self.list.get(self.next) {
                net.apply(step);
            }
            self.next += 1;
        }
    }
}

/// 一对往共享网络里发包：数据报包上对号，从这一对的被控端发往控制端。
pub(crate) struct Tagged<'n, N> {
    net: &'n mut N,
    pair: usize,
    host: usize,
    viewer: usize,
    /// 这一次发了几个包。
    packets: u64,
}

impl<N: Network<Packet>> Tagged<'_, N> {
    pub(crate) fn send(&mut self, size_bytes: usize, bytes: Vec<u8>) {
        self.packets += 1;
        self.net.send(
            self.host,
            self.viewer,
            size_bytes,
            Packet {
                pair: self.pair,
                bytes,
            },
        );
    }
}

/// 跑完全部对。
fn drive<N: Wire>(
    mut net: Shared<N>,
    steps: Vec<(u64, N::Step)>,
    specs: &[Spec<'_>],
    record: bool,
) -> Result<Vec<Finished>, Error> {
    let mut steps = Steps::new(steps);
    let mut pairs = specs
        .iter()
        .enumerate()
        .map(|(i, spec)| Pair::new(spec, i, record))
        .collect::<Result<Vec<_>, _>>()?;
    let mut now = Time::ZERO;
    loop {
        let boundary = pairs.iter().filter_map(Pair::boundary).min();
        let other = [net.peek_time(), net.next_event_time()]
            .into_iter()
            .chain(pairs.iter().map(Pair::next_timer))
            .flatten()
            .min();
        let when = match (boundary, other) {
            (Some(b), Some(o)) => b.min(o),
            (Some(t), None) => t,
            // 全部对都做完了；网络里剩下的包没人收了
            (None, _) => break,
        };
        for p in &mut pairs {
            p.account(&net, when);
        }
        net.advance_to(when);
        now = now.max(when);
        for p in &mut pairs {
            p.arrive_at(now);
        }
        if boundary == Some(when) {
            // 帧边界先做（按对的顺序）；同一时刻的定时器和送达留给下一轮
            for p in &mut pairs {
                if p.boundary() == Some(when) {
                    p.on_boundary(&mut net, &mut steps)?;
                }
            }
            continue;
        }
        while let Some(event) = net.pop_event() {
            for p in &mut pairs {
                p.route_event(p.local(event.at()));
            }
        }
        for p in &mut pairs {
            p.wake(&mut net)?;
        }
        while net.peek_time().is_some_and(|t| t <= now) {
            let Some(d) = net.pop() else {
                break;
            };
            debug_assert!(d.at <= now, "送达 {:?} 晚于现在 {now:?}", d.at);
            let size = d.payload.bytes.len();
            let Ok((header, body)) = decode_datagram(&d.payload.bytes) else {
                continue;
            };
            if let Some(p) = pairs.get_mut(d.payload.pair) {
                p.deliver(&mut net, d.to, size, &header, body)?;
            }
        }
    }
    Ok(pairs
        .into_iter()
        .filter_map(|mut p| p.outcome.take())
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use peerdesk_core::path::{BottleneckConfig, Capacity, Delay, QueueLimit};

    fn link(rate_bps: u64, delay_us: u64) -> DirConfig {
        DirConfig {
            bottleneck: BottleneckConfig {
                capacity: Capacity::Constant { rate_bps },
                limit: QueueLimit::delay(200_000),
                ..BottleneckConfig::default()
            },
            delay: Delay::Fixed(delay_us),
            ..DirConfig::default()
        }
    }

    /// 额外延迟只加在那一对的包上，两个方向都加；额外延迟为 0 的那一对和里面的网络一样。
    #[test]
    fn extra_delay_is_per_pair() {
        // 不限速：送达时刻只有传播延迟，好核对
        let path: Path<Packet> = Path::new(
            1,
            PathConfig::symmetric(DirConfig {
                delay: Delay::Fixed(1_000),
                ..DirConfig::default()
            }),
        );
        let mut net = Shared::new(path, vec![0, 7_000]);
        let p = |pair| Packet {
            pair,
            bytes: vec![1],
        };
        net.send(0, 1, 100, p(0));
        net.send(0, 1, 100, p(1));
        net.send(1, 0, 100, p(1));
        net.send(0, 1, 100, p(9)); // 没有这一对：不加
        assert_eq!(net.in_flight(), 4);
        let mut got = Vec::new();
        while let Some(d) = net.pop() {
            got.push((d.payload.pair, d.to, d.at.micros()));
        }
        assert_eq!(got.len(), 4);
        assert!(got.contains(&(0, 1, 1_000)));
        assert!(got.contains(&(9, 1, 1_000)));
        assert!(got.contains(&(1, 1, 8_000)));
        assert!(got.contains(&(1, 0, 8_000)));
        assert!(
            got.windows(2).all(|w| w[0].2 <= w[1].2),
            "按到达先后：{got:?}"
        );
        assert_eq!(net.in_flight(), 0);
        assert!(net.pop().is_none() && net.peek_time().is_none());
        assert_eq!(net.stats().sent, 4);
    }

    /// 主会话审查发现的问题（2026-11-28）：加了额外延迟的包，在里面的网络下一个包更晚时，以前会被当场交出去。
    /// 现在 `pop` 决不交出晚于进来时 `peek_time` 的包，加了延迟的包在到达时刻（发出 + 传播 + 额外）才交出去。
    #[test]
    fn a_delayed_packet_is_not_delivered_early() {
        let path: Path<Packet> = Path::new(
            1,
            PathConfig::symmetric(DirConfig {
                delay: Delay::Fixed(10_000),
                ..DirConfig::default()
            }),
        );
        let mut net = Shared::new(path, vec![0, 8_000]);
        net.send(
            0,
            1,
            100,
            Packet {
                pair: 1,
                bytes: vec![1],
            },
        ); // 10 ms 出里面的网络，18 ms 到
        net.advance_to(Time(15_000));
        net.send(
            0,
            1,
            100,
            Packet {
                pair: 0,
                bytes: vec![2],
            },
        ); // 25 ms 到
        let mut now = Time(15_000);
        let mut got = Vec::new();
        // 和全局循环一样：推进到 peek_time，只取不晚于现在的
        while let Some(t) = net.peek_time() {
            now = now.max(t);
            net.advance_to(now);
            while net.peek_time().is_some_and(|t| t <= now) {
                let Some(d) = net.pop() else { break };
                assert!(d.at <= now, "{:?} 晚于 {now:?}", d.at);
                got.push((d.payload.pair, d.at.micros(), now.micros()));
            }
        }
        assert_eq!(got, vec![(1, 18_000, 18_000), (0, 25_000, 25_000)]);
    }

    fn pair(name: &str, frames: u32, start_us: u64, reverse: bool, extra: u64) -> PairSpec {
        let mut s = Scenario::fixed(
            name,
            vec![Phase {
                action: Action::Video,
                frames,
            }],
        );
        s.codec = CodecKind::Tile;
        s.seed = 3 + start_us;
        PairSpec {
            scenario: s,
            start_us,
            reverse,
            extra_delay_us: extra,
        }
    }

    /// 两对共享一个 3 Mbps 的瓶颈，第二对晚 3 秒开始：两对都出画面、解码不出错，送达的字节之和不超过容量，
    /// 第二对开始之前什么都没收到；反过来的一对（被控端在节点 1）也能跑。
    #[test]
    fn two_pairs_share_one_bottleneck() {
        let m = MultiScenario {
            network: NetworkPlan::Layout2(Box::new(Layout2 {
                start: PathConfig::symmetric(link(3_000_000, 20_000)),
                ..Layout2::default()
            })),
            seed: 5,
            pairs: vec![
                pair("first", 180, 0, false, 0),
                pair("second", 90, 3_000_000, false, 10_000),
            ],
        };
        let out = run_multi(&m).unwrap();
        assert_eq!(out.len(), 2);
        let (a, b) = (&out[0], &out[1]);
        for o in [a, b] {
            assert!(o.frames > 10 && o.first_picture_ms.is_some(), "{}", o.name);
            assert_eq!(o.bad, 0, "{}：别人的包进来会解坏", o.name);
            assert!(o.received_bytes > 0);
        }
        // 第二对的本地时间从它开始算：第一个包在开始之后才到（往返 20 ms + 额外 10 ms 起）
        assert!(
            b.first_received_us.unwrap() >= 30_000,
            "{:?}",
            b.first_received_us
        );
        // 6 秒 3 Mbps 最多 2.25 MB（含反馈方向，反馈走另一个方向不占这里）
        let total = a.received_bytes + b.received_bytes;
        assert!(total <= 3_000_000 / 8 * 6, "{total}");
        assert_eq!(a.net, b.net, "共享网络的计数是合在一起的");

        // 一对反过来：画面走反向
        let rev = MultiScenario {
            pairs: vec![pair("rev", 60, 0, true, 0)],
            ..m
        };
        let r = run_multi(&rev).unwrap();
        assert!(r[0].frames > 0 && r[0].first_picture_ms.is_some());
        let st = r[0].path.unwrap();
        assert!(st.sent > 0);
    }

    #[test]
    fn empty_or_zero_frame_phases() {
        let mut s = Scenario::fixed(
            "zero",
            vec![
                Phase {
                    action: Action::Idle,
                    frames: 0,
                },
                Phase {
                    action: Action::Typing,
                    frames: 3,
                },
                Phase {
                    action: Action::Idle,
                    frames: 0,
                },
            ],
        );
        s.codec = CodecKind::Tile;
        let o = run(&s).unwrap();
        assert_eq!(o.phases.len(), 3);
        assert_eq!(o.phases[0].frames_shown, 0);
        let mut bad = s.clone();
        bad.phases.clear();
        assert!(run(&bad).is_err());
    }
}
