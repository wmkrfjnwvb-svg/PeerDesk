//! 控制端会话循环里的记录点（M1 的 R4，设计 2.2 节）：只做不用改核心库、直接取得到的那部分——
//! 每一帧的解码和交给界面、每秒的传输和输入、操作到画面的延迟、元数据（含会话号、访问号、上一份）。
//! 到达和收齐时间、纠错和重传次数、等关键帧跳过、时钟基准要改核心库（R5），画到屏幕要界面线程配合（R6）。
//!
//! 时间是从会话开始（握手完成、循环开始）算起的微秒。

use peerdesk_core::proto::PeerOs;
use peerdesk_core::record::{
    EndReason, InputLevel, Meta, Reaction, Record, Role, Transport, ViewFrame, ViewResult,
};
use peerdesk_core::time::Time;

use crate::host_record::transport_delta;
use crate::node::QuicStats;
use crate::recorder::{Finishing, Recorder, ViewRecordSetup};

/// 会话开始时已知的、写进文件头的参数。
#[derive(Debug)]
pub(crate) struct ViewParams<'a> {
    /// 会话号（见 `Link::session_id`）；导不出来时为 `None`。
    pub session_id: Option<[u8; 16]>,
    /// 握手协商出的共同能力位。
    pub caps: u32,
    /// 编码格式。
    pub codec: &'a str,
    /// 会话开始时的墙上时间（Unix 微秒）。
    pub wall_start_us: u64,
}

/// 控制端的一份会话记录和它的每秒汇总基线。
#[derive(Debug)]
pub(crate) struct ViewJournal {
    rec: Recorder,
    t0: Time,
    last_quic: QuicStats,
    last_input: u64,
}

impl ViewJournal {
    /// 开始记录。打不开时写日志说明原因，返回 `None`，会话不记录继续。
    pub(crate) fn open(
        setup: &ViewRecordSetup,
        params: &ViewParams<'_>,
        now: Time,
        quic: QuicStats,
    ) -> Option<Self> {
        let mut meta = Meta::new(Role::Controller, params.wall_start_us);
        meta.program_version
            .clone_from(&setup.setup.program_version);
        meta.commit.clone_from(&setup.setup.commit);
        meta.session_id = params.session_id;
        meta.visit_id = Some(setup.visit_id);
        meta.previous = setup.previous;
        meta.os = Some(PeerOs::current());
        meta.codec = Some(params.codec.to_owned());
        meta.caps = Some(params.caps);
        match Recorder::start_in_dir(&setup.setup.dir, &meta, setup.setup.retention) {
            Ok(rec) => Some(Self {
                rec,
                t0: now,
                last_quic: quic,
                last_input: 0,
            }),
            Err(e) => {
                crate::note!(
                    warn,
                    "会话记录没有打开，这次会话不记录（目录 {}）：{e}",
                    setup.setup.dir.display()
                );
                None
            }
        }
    }

    fn at(&self, now: Time) -> u64 {
        now.since(self.t0)
    }

    /// 解码线程交回一帧。
    pub(crate) fn frame(&self, now: Time, frame: ViewFrame) {
        self.rec.record(self.at(now), Record::ViewFrame(frame));
    }

    /// 每秒汇总（也在结束时调用）：传输、输入、操作到画面。`reactions` 是这一秒的各次延迟（微秒）。
    pub(crate) fn second(
        &mut self,
        now: Time,
        quic: QuicStats,
        input_total: u64,
        reactions: &[u64],
    ) {
        let at = self.at(now);
        let last_quic = std::mem::replace(&mut self.last_quic, quic);
        let input =
            input_total.saturating_sub(std::mem::replace(&mut self.last_input, input_total));
        let t: Transport = transport_delta(&last_quic, &quic);
        self.rec.record(at, Record::Transport(t));
        self.rec.record(at, Record::Input(InputLevel::of(input)));
        if let Some(r) = reaction_record(reactions) {
            self.rec.record(at, Record::Reaction(r));
        }
    }

    /// 会话结束：最后不足一秒的汇总和结束记录。不等记录线程写完。
    pub(crate) fn finish(
        mut self,
        now: Time,
        quic: QuicStats,
        input_total: u64,
        reactions: &[u64],
        reason: EndReason,
    ) -> Finishing {
        self.second(now, quic, input_total, reactions);
        let at = self.at(now);
        self.rec.finish(at, reason)
    }
}

/// 一组操作延迟（微秒）的次数、中位数、最大值；没有时为 `None`。
pub(crate) fn reaction_record(v: &[u64]) -> Option<Reaction> {
    let mut sorted = v.to_vec();
    sorted.sort_unstable();
    let max_us = *sorted.last()?;
    Some(Reaction {
        count: u32::try_from(sorted.len()).unwrap_or(u32::MAX),
        median_us: sorted.get(sorted.len() / 2).copied().unwrap_or(0),
        max_us,
    })
}

/// 解码线程交回的结果对应的记录。`present_us` 是交给界面用的时间，没交给界面时为 0。
pub(crate) const fn view_frame(
    frame_id: u32,
    result: ViewResult,
    decode_us: u64,
    present_us: u64,
) -> ViewFrame {
    ViewFrame {
        frame_id,
        result,
        decode_us,
        present_us,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn reactions_are_summarised_in_microseconds() {
        assert_eq!(reaction_record(&[]), None);
        let r = reaction_record(&[90_000, 10_000, 40_000]).unwrap();
        assert_eq!((r.count, r.median_us, r.max_us), (3, 40_000, 90_000));
        assert_eq!(
            view_frame(4, ViewResult::NoPicture, 7, 0),
            ViewFrame {
                frame_id: 4,
                result: ViewResult::NoPicture,
                decode_us: 7,
                present_us: 0
            }
        );
    }
}
