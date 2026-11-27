//! 被控端会话循环里的记录点（M1 的 R3，设计 2.1 节）：只做不用改核心库、直接取得到的那部分——
//! 反馈、帧（含类别）、每秒的重传汇总、传输、输入、元数据、开始和结束。
//! 决定的时间和种类代码、没编的拍的原因、QP、变化面积、帧发完、采集数字统计、探测时长要改核心库，属于 R5。
//!
//! 这里只读会话状态、把它们变成记录交给记录线程；不影响会话怎么走。每个记录的构造都是纯函数，单独测。
//! 时间是从会话开始（握手完成、循环开始）算起的微秒。

use std::borrow::Cow;

use peerdesk_core::media::{FrameJob, MediaSender, SenderStats};
use peerdesk_core::proto::PeerOs;
use peerdesk_core::record::{
    EndReason, Feedback, Frame, FrameClass, InputLevel, Meta, Record, Retransmit, Role, Timing,
    Transport,
};
use peerdesk_core::time::Time;
use peerdesk_core::video::{EncodeTiming, EncodedFrame};

use crate::node::QuicStats;
use crate::recorder::{Finishing, RecordSetup, Recorder};

/// 会话开始时已知的、写进文件头的参数。
#[derive(Debug)]
pub(crate) struct Params<'a> {
    /// 握手协商出的共同能力位。
    pub caps: u32,
    /// 编码格式。
    pub codec: &'a str,
    /// 码率估计器的名字。
    pub estimator: &'a str,
    /// 内容策略的名字。
    pub policy: &'a str,
    /// 帧率设定。
    pub fps: u32,
    /// 起步码率。
    pub start_bps: u64,
    /// 码率上限。
    pub max_bps: u64,
    /// 会话开始时的墙上时间（Unix 微秒）。
    pub wall_start_us: u64,
}

/// 被控端的一份会话记录和它的每秒汇总基线。
#[derive(Debug)]
pub(crate) struct HostJournal {
    rec: Recorder,
    t0: Time,
    last_sender: SenderStats,
    last_quic: QuicStats,
    last_input: u64,
}

impl HostJournal {
    /// 开始记录。打不开（目录建不出来、没有权限、磁盘满）时写日志说明原因，返回 `None`，会话不记录继续。
    pub(crate) fn open(
        setup: &RecordSetup,
        params: &Params<'_>,
        now: Time,
        quic: QuicStats,
    ) -> Option<Self> {
        let mut meta = Meta::new(Role::Host, params.wall_start_us);
        meta.program_version.clone_from(&setup.program_version);
        meta.commit.clone_from(&setup.commit);
        meta.os = Some(PeerOs::current());
        meta.codec = Some(params.codec.to_owned());
        meta.estimator = Some(params.estimator.to_owned());
        meta.policy = Some(params.policy.to_owned());
        meta.fps = Some(params.fps);
        meta.start_bps = Some(params.start_bps);
        meta.max_bps = Some(params.max_bps);
        meta.caps = Some(params.caps);
        match Recorder::start_in_dir(&setup.dir, &meta, setup.retention) {
            Ok(rec) => Some(Self {
                rec,
                t0: now,
                last_sender: SenderStats::default(),
                last_quic: quic,
                last_input: 0,
            }),
            Err(e) => {
                crate::note!(
                    warn,
                    "会话记录没有打开，这次会话不记录（目录 {}）：{e}",
                    setup.dir.display()
                );
                None
            }
        }
    }

    fn at(&self, now: Time) -> u64 {
        now.since(self.t0)
    }

    /// 处理完一份反馈：发送端看到的和算出的。发送端还没有观测时不记。
    pub(crate) fn feedback(&self, now: Time, media: &MediaSender) {
        if let Some(obs) = media.last_observation() {
            let f = Feedback::from_observation(&obs, media.rate_bps(), media.rate_state());
            self.rec.record(self.at(now), Record::Feedback(f));
        }
    }

    /// 一个编码任务结束。
    pub(crate) fn frame(&self, now: Time, frame: Frame) {
        self.rec.record(self.at(now), Record::Frame(frame));
    }

    /// 每秒汇总（也在结束时调用，把最后不足一秒的写进去）：重传、传输、输入。
    pub(crate) fn second(
        &mut self,
        now: Time,
        sender: SenderStats,
        quic: QuicStats,
        input_total: u64,
    ) {
        let at = self.at(now);
        let last_sender = std::mem::replace(&mut self.last_sender, sender);
        let last_quic = std::mem::replace(&mut self.last_quic, quic);
        let input =
            input_total.saturating_sub(std::mem::replace(&mut self.last_input, input_total));
        self.rec.record(
            at,
            Record::Retransmit(retransmit_delta(&last_sender, &sender)),
        );
        self.rec
            .record(at, Record::Transport(transport_delta(&last_quic, &quic)));
        self.rec.record(at, Record::Input(InputLevel::of(input)));
    }

    /// 会话结束：把最后不足一秒的汇总写进去，交代结束原因，关闭队列。不等记录线程写完。
    pub(crate) fn finish(
        mut self,
        now: Time,
        sender: SenderStats,
        quic: QuicStats,
        input_total: u64,
        reason: EndReason,
    ) -> Finishing {
        self.second(now, sender, quic, input_total);
        let at = self.at(now);
        self.rec.finish(at, reason)
    }
}

/// 这一秒里发送端的重传计数（两次累计值的差）。
pub(crate) fn retransmit_delta(last: &SenderStats, now: &SenderStats) -> Retransmit {
    Retransmit {
        requested: now.nack_requests.saturating_sub(last.nack_requests),
        resent: now.retransmitted.saturating_sub(last.retransmitted),
        capped: now.resend_capped.saturating_sub(last.resend_capped),
        lost_frames: now.lost.saturating_sub(last.lost),
    }
}

/// 这一秒里 QUIC 的计数（两次累计值的差；拥塞窗口和 MTU 是当前值）。
pub(crate) fn transport_delta(last: &QuicStats, now: &QuicStats) -> Transport {
    Transport {
        app_sent: now.app_sent.saturating_sub(last.app_sent),
        frames_sent: now
            .datagram_frames_sent
            .saturating_sub(last.datagram_frames_sent),
        too_large: now.app_too_large.saturating_sub(last.app_too_large),
        lost_packets: now.lost_packets.saturating_sub(last.lost_packets),
        congestion_events: now.congestion_events.saturating_sub(last.congestion_events),
        cwnd_bytes: now.cwnd,
        mtu: now.mtu,
    }
}

/// 编码结果里出了帧的话，是（码流字节数，是不是关键帧）；没出帧或编码出错为 `None`。
pub(crate) fn encoded_info(
    result: &Result<(Option<EncodedFrame>, bool), peerdesk_core::Error>,
) -> Option<(usize, bool)> {
    let (encoded, _) = result.as_ref().ok()?;
    encoded.as_ref().map(|e| (e.payload.len(), e.key))
}

/// 一个编码任务结束时的帧记录。`encoded` 见 [`encoded_info`]；`changed` 是交任务时画面变没变；
/// `captured_ago_us` 是采集时间在现在之前多少微秒。
pub(crate) fn frame_record(
    job: &FrameJob,
    took_us: u64,
    timing: &EncodeTiming,
    encoded: Option<(usize, bool)>,
    changed: bool,
    captured_ago_us: u64,
) -> Frame {
    let (bytes, key) = encoded.unwrap_or((0, false));
    Frame {
        frame_id: job.frame_id,
        captured_ago_us: Some(captured_ago_us),
        class: FrameClass::of(key, changed),
        key_reason: Cow::Borrowed(timing.key_reason),
        budget_bytes: u32::try_from(job.budget).ok(),
        bytes: u32::try_from(bytes).unwrap_or(u32::MAX),
        scale_permille: Some(job.scale_permille),
        size: (timing.width > 0 && timing.height > 0).then_some((timing.width, timing.height)),
        timing: Some(Timing {
            took_us,
            scale_us: timing.scale_us,
            convert_us: timing.convert_us,
            wait_us: timing.wait_us,
            encode_us: timing.encode_us,
        }),
        hardware: timing.hardware,
        dropped: timing.dropped,
    }
}

/// 会话为什么结束。`failed` 是本端自己出的错（编码线程没了、编码或分片出错）；
/// 其余按连接的状态分：对端退出程序、网络断了、否则是正常结束。
pub(crate) const fn end_reason(failed: bool, peer_quit: bool, link_lost: bool) -> EndReason {
    if failed {
        EndReason::Failed
    } else if peer_quit {
        EndReason::PeerQuit
    } else if link_lost {
        EndReason::LinkLost
    } else {
        EndReason::Normal
    }
}

/// 文件头里要用的墙上时间。
pub(crate) fn wall_now_us() -> u64 {
    crate::framelog::unix_us()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn job() -> FrameJob {
        FrameJob {
            frame_id: 7,
            budget: 40_000,
            lost: Vec::new(),
            feedback: None,
            scale_permille: 750,
            view: None,
            rate_stale: false,
        }
    }

    #[test]
    fn counters_are_deltas_and_current_values() {
        let last = SenderStats {
            nack_requests: 10,
            retransmitted: 8,
            resend_capped: 1,
            lost: 2,
            ..SenderStats::default()
        };
        let now = SenderStats {
            nack_requests: 15,
            retransmitted: 12,
            resend_capped: 1,
            lost: 3,
            ..SenderStats::default()
        };
        let r = retransmit_delta(&last, &now);
        assert_eq!(
            (r.requested, r.resent, r.capped, r.lost_frames),
            (5, 4, 0, 1)
        );
        // 计数倒退（不该发生）：按 0，不溢出
        assert_eq!(retransmit_delta(&now, &last).requested, 0);

        let q0 = QuicStats {
            app_sent: 100,
            datagram_frames_sent: 90,
            app_too_large: 1,
            lost_packets: 2,
            congestion_events: 1,
            cwnd: 10_000,
            mtu: 1200,
            ..QuicStats::default()
        };
        let q1 = QuicStats {
            app_sent: 160,
            datagram_frames_sent: 148,
            app_too_large: 1,
            lost_packets: 5,
            congestion_events: 2,
            cwnd: 30_000,
            mtu: 1452,
            ..QuicStats::default()
        };
        let t = transport_delta(&q0, &q1);
        assert_eq!((t.app_sent, t.frames_sent, t.too_large), (60, 58, 0));
        assert_eq!((t.lost_packets, t.congestion_events), (3, 1));
        assert_eq!((t.cwnd_bytes, t.mtu), (30_000, 1452));
    }

    #[test]
    fn frame_records_carry_what_the_loop_knows() {
        let timing = EncodeTiming {
            scale_us: 1_000,
            convert_us: 3_000,
            encode_us: 9_000,
            wait_us: 500,
            hardware: true,
            dropped: false,
            width: 1280,
            height: 720,
            key_reason: "有帧丢了（接收端解不了后面的帧）",
            ..EncodeTiming::default()
        };
        let f = frame_record(&job(), 14_000, &timing, Some((38_000, true)), true, 4_200);
        assert_eq!((f.frame_id, f.bytes, f.class), (7, 38_000, FrameClass::Key));
        assert_eq!(f.key_reason, "有帧丢了（接收端解不了后面的帧）");
        assert_eq!(
            (f.budget_bytes, f.scale_permille),
            (Some(40_000), Some(750))
        );
        assert_eq!(
            (f.size, f.captured_ago_us),
            (Some((1280, 720)), Some(4_200))
        );
        assert_eq!(f.timing.unwrap().wait_us, 500);
        assert!(f.hardware && !f.dropped);
        // 没出帧：字节为 0；没变化也没出关键帧是补清晰；尺寸不知道
        let none = frame_record(&job(), 1_000, &EncodeTiming::default(), None, false, 0);
        assert_eq!(
            (none.bytes, none.class, none.size),
            (0, FrameClass::Refine, None)
        );
        let content = frame_record(
            &job(),
            1,
            &EncodeTiming::default(),
            Some((10, false)),
            true,
            0,
        );
        assert_eq!(content.class, FrameClass::Content);
        // 预算大得装不进 u32：不知道
        let mut huge = job();
        huge.budget = usize::MAX;
        assert_eq!(
            frame_record(&huge, 1, &EncodeTiming::default(), None, false, 0).budget_bytes,
            None
        );
    }

    #[test]
    fn encoded_info_reads_the_result() {
        let ok: Result<(Option<EncodedFrame>, bool), peerdesk_core::Error> = Ok((
            Some(EncodedFrame {
                payload: vec![0; 12],
                key: true,
            }),
            false,
        ));
        assert_eq!(encoded_info(&ok), Some((12, true)));
        assert_eq!(encoded_info(&Ok((None, true))), None);
        assert_eq!(
            encoded_info(&Err(peerdesk_core::Error::InvalidArgument("x"))),
            None
        );
    }

    #[test]
    fn the_end_reason_follows_the_cause() {
        assert_eq!(end_reason(true, true, true), EndReason::Failed);
        assert_eq!(end_reason(false, true, true), EndReason::PeerQuit);
        assert_eq!(end_reason(false, false, true), EndReason::LinkLost);
        assert_eq!(end_reason(false, false, false), EndReason::Normal);
    }

    #[test]
    fn a_journal_that_cannot_open_says_so_and_returns_none() {
        let dir = std::env::temp_dir().join(format!("peerdesk-hostrec-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        let setup = RecordSetup {
            dir: blocker.join("records"),
            retention: crate::recorder::Retention::default(),
            program_version: None,
            commit: None,
        };
        let params = Params {
            caps: 0,
            codec: "h264",
            estimator: "gcc-v1",
            policy: "office",
            fps: 60,
            start_bps: 1,
            max_bps: 2,
            wall_start_us: 0,
        };
        assert!(HostJournal::open(&setup, &params, Time::ZERO, QuicStats::default()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
