//! 业务链路追踪。
//!
//! 每个会话有一个追踪号，关键步骤都记一条事件。测试用它断言“业务走了哪条路径”，
//! 线上用它在出问题时还原现场。缓冲区有上限，写满后丢弃最旧的事件。

use std::collections::VecDeque;
use std::fmt::Write as _;

use crate::Time;

/// 追踪号，一次会话一个。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TraceId(pub u64);

/// 业务步骤。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// 发出 Hello。
    HelloSent,
    /// 收到 Hello。
    HelloReceived,
    /// 选定加密套件。
    SuiteNegotiated,
    /// 发出 Accept。
    AcceptSent,
    /// 收到 Accept。
    AcceptReceived,
    /// 被控端要求密码。
    AuthRequested,
    /// 密码验证通过。
    AuthPassed,
    /// 密码验证失败。
    AuthFailed,
    /// 会话建立。
    Established,
    /// 会话被拒绝。
    Rejected,
    /// 一帧已发出。
    FrameSent,
    /// 一帧已完整送达。
    FrameDelivered,
    /// 一帧超过截止时间被丢弃。
    FrameExpired,
    /// 会话关闭。
    Closed,
}

impl Stage {
    /// 步骤名。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Stage::HelloSent => "hello_sent",
            Stage::HelloReceived => "hello_received",
            Stage::SuiteNegotiated => "suite_negotiated",
            Stage::AcceptSent => "accept_sent",
            Stage::AcceptReceived => "accept_received",
            Stage::AuthRequested => "auth_requested",
            Stage::AuthPassed => "auth_passed",
            Stage::AuthFailed => "auth_failed",
            Stage::Established => "established",
            Stage::Rejected => "rejected",
            Stage::FrameSent => "frame_sent",
            Stage::FrameDelivered => "frame_delivered",
            Stage::FrameExpired => "frame_expired",
            Stage::Closed => "closed",
        }
    }
}

/// 一条追踪事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Event {
    /// 发生时间。
    pub at: Time,
    /// 所属会话。
    pub trace: TraceId,
    /// 步骤。
    pub stage: Stage,
    /// 步骤相关的数值，例如帧号或套件编号。
    pub detail: u64,
}

/// 有上限的事件缓冲区。
#[derive(Debug, Clone)]
pub struct Tracer {
    capacity: usize,
    events: VecDeque<Event>,
    dropped: u64,
}

impl Tracer {
    /// 创建，最多保留 `capacity` 条事件（至少 1 条）。
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            capacity,
            events: VecDeque::with_capacity(capacity),
            dropped: 0,
        }
    }

    /// 记录一条事件。
    pub fn record(&mut self, at: Time, trace: TraceId, stage: Stage, detail: u64) {
        if self.events.len() >= self.capacity {
            self.events.pop_front();
            self.dropped = self.dropped.saturating_add(1);
        }
        self.events.push_back(Event {
            at,
            trace,
            stage,
            detail,
        });
    }

    /// 全部事件，按记录顺序。
    pub fn events(&self) -> impl Iterator<Item = &Event> {
        self.events.iter()
    }

    /// 某个会话走过的步骤序列。
    #[must_use]
    pub fn path(&self, trace: TraceId) -> Vec<Stage> {
        self.events
            .iter()
            .filter(|e| e.trace == trace)
            .map(|e| e.stage)
            .collect()
    }

    /// 某个会话里某个步骤出现的次数。
    #[must_use]
    pub fn count(&self, trace: TraceId, stage: Stage) -> usize {
        self.events
            .iter()
            .filter(|e| e.trace == trace && e.stage == stage)
            .count()
    }

    /// 因缓冲区写满而被丢弃的事件数。
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// 输出成文本，每行一条，便于贴进问题报告。
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        for e in &self.events {
            // 向 String 写入不会失败。
            let _ = writeln!(
                out,
                "{:>10}us trace={:016x} {} detail={}",
                e.at.micros(),
                e.trace.0,
                e.stage.name(),
                e.detail
            );
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Stage; 14] = [
        Stage::AuthRequested,
        Stage::AuthPassed,
        Stage::AuthFailed,
        Stage::HelloSent,
        Stage::HelloReceived,
        Stage::SuiteNegotiated,
        Stage::AcceptSent,
        Stage::AcceptReceived,
        Stage::Established,
        Stage::Rejected,
        Stage::FrameSent,
        Stage::FrameDelivered,
        Stage::FrameExpired,
        Stage::Closed,
    ];

    #[test]
    fn records_paths_per_trace() {
        let mut t = Tracer::new(64);
        let (a, b) = (TraceId(1), TraceId(2));
        t.record(Time(1), a, Stage::HelloSent, 0);
        t.record(Time(2), b, Stage::HelloReceived, 0);
        t.record(Time(3), a, Stage::Established, 5);
        assert_eq!(t.path(a), vec![Stage::HelloSent, Stage::Established]);
        assert_eq!(t.path(b), vec![Stage::HelloReceived]);
        assert_eq!(t.count(a, Stage::HelloSent), 1);
        assert_eq!(t.count(a, Stage::Closed), 0);
        assert_eq!(t.events().count(), 3);
        let dump = t.dump();
        assert_eq!(dump.lines().count(), 3);
        assert!(dump.contains("established detail=5"));
    }

    #[test]
    fn capacity_is_bounded_and_oldest_goes_first() {
        let mut t = Tracer::new(0); // 至少保留 1 条
        t.record(Time(1), TraceId(1), Stage::HelloSent, 0);
        t.record(Time(2), TraceId(1), Stage::Closed, 0);
        assert_eq!(t.path(TraceId(1)), vec![Stage::Closed]);
        assert_eq!(t.dropped(), 1);
    }

    #[test]
    fn stage_names_are_unique() {
        let mut names: Vec<_> = ALL.iter().map(|s| s.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), ALL.len());
    }
}
