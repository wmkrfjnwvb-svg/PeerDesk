//! 全模型仿真（L0）：没有像素，画面是操作时间线里的内容描述。
//!
//! 验收（`docs/research/Q-redesign/a1-plan.md` S3）：同一个场景，L0.5（真像素交给假后端）和 L0（内容描述交给假后端）
//! 的会话必须一模一样——每份反馈的特征行、每个计数、每个阶段的流量和等待都相同。这比“交给假后端的（面积, 细节, QP）
//! 逐帧相同”更强：那些数只要有一帧不同，后面的字节数、码率、反馈就全都不同。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout)]

use peerdesk_core::sim::Link;
use peerdesk_e2e::{behaviour_fingerprint, run_probed, Action, CodecKind, Phase, Scenario};

fn phase(action: Action, frames: u32) -> Phase {
    Phase { action, frames }
}

/// 各种操作都有：静止、指针、打字、滚动、拖窗口、切窗口、看视频。
fn mixed() -> Vec<Phase> {
    vec![
        phase(Action::Idle, 15),
        phase(Action::Pointer, 30),
        phase(Action::Typing, 30),
        phase(Action::Idle, 20),
        phase(Action::Scrolling, 30),
        phase(Action::Dragging, 20),
        phase(Action::Switching, 10),
        phase(Action::Idle, 30),
        phase(Action::Video, 30),
        phase(Action::Idle, 15),
    ]
}

fn scenarios() -> Vec<Scenario> {
    let narrow = Link {
        rate_bps: Some(600_000),
        delay_us: 40_000,
        jitter_us: 3_000,
        ..Link::lan()
    };
    let lossy = Link {
        rate_bps: Some(8_000_000),
        delay_us: 20_000,
        loss_permille: 50,
        ..Link::lan()
    };
    vec![
        Scenario::new("l0-lan", mixed()),
        // 窄带：会降分辨率（半尺寸的亮度抽样）、丢帧重发关键帧、帧率降档
        Scenario {
            network: vec![(0, narrow)],
            ..Scenario::new("l0-narrow", mixed())
        },
        Scenario {
            network: vec![(0, Link::cellular())],
            seed: 7,
            ..Scenario::new("l0-cellular", mixed())
        },
        // 丢包：重传、纠错、关键帧恢复
        Scenario {
            network: vec![(0, lossy)],
            seed: 3,
            fps: 60,
            ..Scenario::new("l0-lossy", mixed())
        },
        // 带宽中途减到五分之一再恢复
        Scenario {
            network: vec![
                (
                    0,
                    Link {
                        rate_bps: Some(10_000_000),
                        delay_us: 15_000,
                        ..Link::lan()
                    },
                ),
                (
                    3_000_000,
                    Link {
                        rate_bps: Some(2_000_000),
                        delay_us: 15_000,
                        ..Link::lan()
                    },
                ),
                (
                    6_000_000,
                    Link {
                        rate_bps: Some(10_000_000),
                        delay_us: 15_000,
                        ..Link::lan()
                    },
                ),
            ],
            ..Scenario::new("l0-step", mixed())
        },
    ]
}

#[test]
fn l0_sessions_are_identical_to_l05_sessions() {
    for base in scenarios() {
        let (l05, inputs05) = run_probed(&Scenario {
            codec: CodecKind::H264L05,
            ..base.clone()
        })
        .unwrap();
        let (l0, inputs0) = run_probed(&Scenario {
            codec: CodecKind::H264L0,
            ..base.clone()
        })
        .unwrap();
        // 区域类输入（脏像素数、移动像素数、整屏移动、脏区域细节、静止宏块的画质差分布、repeat）逐帧相同：
        // 字节模型现在不看它们，会话一样说明不了什么，要直接比交给假后端的数
        assert!(inputs05.len() > 10, "{}", base.name);
        assert_eq!(
            inputs05, inputs0,
            "{}：交给假后端的输入有一帧不同",
            base.name
        );
        let any = |f: &dyn Fn(&peerdesk_media::fake::Regions) -> bool| {
            inputs0.iter().any(|m| f(&m.regions))
        };
        assert!(
            any(&|r| r.dirty_px > 0 && r.dirty_px < 100_000),
            "有小块的脏区域"
        );
        assert!(any(&|r| r.moved_px > 0), "有移动区域（滚动、拖窗口）");
        assert!(any(&|r| r.static_gap.iter().any(|g| *g > 0)), "有补清晰");
        assert!(any(&|r| r.dirty_detail_permille > 0), "脏区域里有细节");
        println!(
            "{}：帧 {} / {}，关键帧 {} / {}，字节 {} / {}，反馈 {} / {}",
            base.name,
            l05.frames,
            l0.frames,
            l05.keyframes,
            l0.keyframes,
            l05.sent_bytes,
            l0.sent_bytes,
            l05.features.len(),
            l0.features.len()
        );
        assert!(l0.frames > 10 && l0.bad == 0, "{}", base.name);
        assert_eq!(l05.features, l0.features, "{}：反馈的特征行不同", base.name);
        assert_eq!(
            behaviour_fingerprint(&l05),
            behaviour_fingerprint(&l0),
            "{}：L0 和 L0.5 的会话不一样",
            base.name
        );
        assert_eq!(l0.codec.name(), "h264-l0");
    }
}
