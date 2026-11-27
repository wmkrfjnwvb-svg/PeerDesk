//! 会话记录（M1 的 R3）在仿真里的两条硬门槛：
//! 1. 开着记录和关着记录，仿真的结果逐字节相同（记录只读发送端的状态，不改变会话）；
//! 2. 仿真导出的记录能完整读回，条数和仿真里实际发生的次数对得上。
//!
//! 场景和 `e2e.rs` 里同名的场景一致（办公操作、带宽阶梯、突然掉带宽、蜂窝网络、随机丢包），用产品默认的估计器。
//! 用 release 跑：`cargo test --release -p peerdesk-e2e --test record -- --nocapture`。

#![allow(clippy::unwrap_used, clippy::print_stdout, clippy::indexing_slicing)]

use peerdesk_core::adaptive::Profile;
use peerdesk_core::record::{BlockWriter, EndReason, FrameClass, Kind, Recording};
use peerdesk_core::screen::Screen;
use peerdesk_core::sim::Link;
use peerdesk_e2e::{run, run_recorded, Action, Outcome, Phase, Scenario};

fn phase(action: Action, frames: u32) -> Phase {
    Phase { action, frames }
}

fn office() -> Vec<Phase> {
    vec![
        phase(Action::Idle, 30),
        phase(Action::Typing, 90),
        phase(Action::Idle, 30),
        phase(Action::Scrolling, 60),
        phase(Action::Idle, 30),
        phase(Action::Dragging, 60),
        phase(Action::Idle, 30),
        phase(Action::Switching, 15),
        phase(Action::Idle, 30),
        phase(Action::Video, 90),
        phase(Action::Idle, 30),
    ]
}

fn scenarios() -> Vec<Scenario> {
    let steady = |mbps: u64| Link {
        rate_bps: Some(mbps * 1_000_000),
        ..Link::lan()
    };
    let full = Link {
        rate_bps: Some(20_000_000),
        ..Link::wifi()
    };
    vec![
        Scenario::new("office-lan", office()),
        Scenario {
            network: vec![
                (0, steady(20)),
                (4_000_000, steady(3)),
                (8_000_000, steady(20)),
            ],
            ..Scenario::new(
                "bandwidth-steps",
                vec![
                    phase(Action::Scrolling, 120),
                    phase(Action::Scrolling, 120),
                    phase(Action::Scrolling, 120),
                    phase(Action::Idle, 30),
                ],
            )
        },
        Scenario {
            network: vec![
                (0, full),
                (
                    3_000_000,
                    Link {
                        rate_bps: Some(2_000_000),
                        ..full
                    },
                ),
                (5_000_000, full),
            ],
            profile: Profile::WIRELESS,
            ..Scenario::new(
                "sudden-drop",
                vec![phase(Action::Dragging, 240), phase(Action::Idle, 45)],
            )
        },
        Scenario {
            network: vec![(0, Link::cellular())],
            profile: Profile::CELLULAR,
            start_bps: 2_000_000,
            ..Scenario::new("office-cellular", office())
        },
        Scenario {
            network: vec![(
                0,
                Link {
                    delay_us: 3_000,
                    jitter_us: 2_000,
                    loss_permille: 30,
                    rate_bps: Some(30_000_000),
                    ..Link::lan()
                },
            )],
            profile: Profile::WIRELESS,
            ..Scenario::new(
                "random-loss",
                vec![phase(Action::Dragging, 150), phase(Action::Idle, 30)],
            )
        },
    ]
}

/// 文本部分，和每个阶段两幅截图的（宽，高，像素）。
type Fingerprint = (String, Vec<(u16, u16, Vec<u32>)>);

/// 一次运行的全部结果，逐字节可比：画面单独按像素比，其余按 `Debug` 文本比
/// （浮点数的 `Debug` 是最短的、能原样还原的写法，文本相同就是每一位都相同）。
fn fingerprint(o: &Outcome) -> Fingerprint {
    let mut bare = o.clone();
    let mut screens = Vec::new();
    for p in &mut bare.phases {
        let (a, b) = std::mem::replace(
            &mut p.snapshot,
            (Screen::new(0, 0, 0), Screen::new(0, 0, 0)),
        );
        for screen in [a, b] {
            screens.push((screen.width(), screen.height(), screen.pixels().to_vec()));
        }
    }
    (format!("{bare:?}"), screens)
}

#[test]
fn recording_does_not_change_the_session() {
    for s in scenarios() {
        let plain = run(&s).unwrap();
        let again = run(&s).unwrap();
        let (recorded, bytes) = run_recorded(&s).unwrap();
        // 对照：不开记录跑两次本来就一样（否则下面的比较说明不了什么）
        assert_eq!(
            fingerprint(&plain),
            fingerprint(&again),
            "{}：不开记录两次就不同",
            s.name
        );
        let (text_a, screens_a) = fingerprint(&plain);
        let (text_b, screens_b) = fingerprint(&recorded);
        assert_eq!(text_a, text_b, "{}：开着记录结果变了", s.name);
        assert_eq!(screens_a, screens_b, "{}：开着记录画面变了", s.name);
        assert!(!bytes.is_empty());
        println!(
            "{:<16} 结果相同（{} 字节的 Debug 文本，{} 个阶段的截图像素）；记录 {} 字节",
            s.name,
            text_a.len(),
            screens_a.len(),
            bytes.len()
        );
    }
}

#[test]
fn the_recording_reads_back_and_matches_what_happened() {
    println!(
        "{:<16} {:>8} {:>8} {:>6} {:>6} {:>7} {:>9} {:>9}",
        "场景", "时长 s", "字节", "反馈", "帧", "重传", "字节/秒", "反馈条数=特征行"
    );
    for s in scenarios() {
        let (o, bytes) = run_recorded(&s).unwrap();
        let rec = Recording::parse(&bytes).unwrap();
        let st = rec.stats;
        assert_eq!(
            (
                st.bad_blocks,
                st.bad_records,
                st.unknown_records,
                st.skipped_bytes
            ),
            (0, 0, 0, 0),
            "{}",
            s.name
        );
        assert!(!st.truncated_tail && st.blocks > 0, "{}", s.name);
        assert_eq!(rec.meta.codec.as_deref(), Some("h264"));
        assert_eq!(rec.meta.estimator, Some(o.estimator.to_owned()));
        assert_eq!(rec.meta.fps, Some(s.fps));

        // 结束记录：正常结束，时长等于场景时长
        let end = rec.end().unwrap();
        assert_eq!(end.reason, EndReason::Normal);
        let total_frames: u64 = s.phases.iter().map(|p| u64::from(p.frames)).sum();
        let interval = 1_000_000 / u64::from(s.fps);
        assert_eq!(end.duration_us, total_frames * interval, "{}", s.name);

        // 反馈：条数等于特征行数，每一项和特征行一致
        let feedback: Vec<_> = rec.feedbacks().collect();
        assert_eq!(feedback.len(), o.features.len(), "{}：反馈条数", s.name);
        for ((at, f), row) in feedback.iter().zip(&o.features) {
            assert_eq!(at / 1000, row.t_ms);
            assert_eq!(f.rate_bps, row.rate_bps);
            assert_eq!(f.queue_delay_us, row.queue_us);
            assert_eq!(f.trend_tenth_ms, row.trend);
            assert_eq!(f.train_bytes_per_s, row.train);
            assert_eq!(f.loss_permille, row.loss);
            assert_eq!(f.recv_bytes_per_s, row.recv);
            assert_eq!(f.in_flight_bytes, row.in_flight);
            assert_eq!(f.state, row.state);
        }

        // 帧：出了帧的任务数等于发出的帧数，帧号 1.. 连续；关键帧数对得上
        let frames: Vec<_> = rec.frames().map(|(_, f)| f).collect();
        let sent: Vec<_> = frames.iter().filter(|f| f.sent_bytes().is_some()).collect();
        assert_eq!(
            sent.len(),
            usize::try_from(o.frames).unwrap(),
            "{}：帧数",
            s.name
        );
        for (i, f) in sent.iter().enumerate() {
            assert_eq!(usize::try_from(f.frame_id).unwrap(), i + 1);
        }
        let keys = sent.iter().filter(|f| f.class == FrameClass::Key).count();
        assert_eq!(
            keys,
            usize::try_from(o.keyframes).unwrap(),
            "{}：关键帧数",
            s.name
        );
        // 没出帧的任务不是关键帧
        assert!(frames
            .iter()
            .filter(|f| f.sent_bytes().is_none())
            .all(|f| f.class != FrameClass::Key));

        // 重传：每秒汇总加起来等于全程的计数
        let (mut resent, mut lost) = (0u64, 0u64);
        let mut windows = 0u64;
        for (_, r) in rec.retransmits() {
            resent += u64::from(r.resent);
            lost += u64::from(r.lost_frames);
            windows += 1;
        }
        assert_eq!(resent, u64::from(o.retransmitted), "{}：重传分片数", s.name);
        assert_eq!(lost, u64::from(o.recoveries), "{}：判定丢帧数", s.name);
        // 每满一秒一条，加最后不足一秒的一条
        let seconds = end.duration_us / 1_000_000;
        assert_eq!(windows, seconds + 1, "{}：重传汇总条数", s.name);

        // 结束记录里的账和读回的条数一致，没有丢
        for (kind, count) in [
            (Kind::Feedback, feedback.len()),
            (Kind::Frame, frames.len()),
            (Kind::Retransmit, usize::try_from(windows).unwrap()),
        ] {
            let tally = end.tallies.iter().find(|t| t.kind == kind.code()).unwrap();
            assert_eq!(usize::try_from(tally.written).unwrap(), count);
            assert_eq!(tally.dropped, 0);
        }
        assert_eq!(rec.count(Kind::Gap), 0);

        // 每个种类单独占多少字节（把这个种类的记录单独再写一遍，块头和块起始时间摊在里面）
        let size_of = |kind: Kind| {
            let mut w = BlockWriter::new();
            let mut out = Vec::new();
            for e in rec.entries.iter().filter(|e| e.record.kind() == kind) {
                w.push(e.at_us, &e.record, &mut out);
            }
            w.flush(&mut out);
            out.len()
        };
        println!(
            "  每条平均字节：反馈 {:.1}，帧 {:.1}，重传汇总 {:.1}",
            size_of(Kind::Feedback) as f64 / feedback.len() as f64,
            size_of(Kind::Frame) as f64 / frames.len() as f64,
            size_of(Kind::Retransmit) as f64 / windows as f64,
        );
        let secs = end.duration_us as f64 / 1e6;
        println!(
            "{:<16} {:>8.1} {:>8} {:>6} {:>6} {:>7} {:>9.0} {:>9}",
            s.name,
            secs,
            bytes.len(),
            feedback.len(),
            sent.len(),
            windows,
            bytes.len() as f64 / secs,
            feedback.len() == o.features.len()
        );
    }
}
