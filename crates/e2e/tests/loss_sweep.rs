//! 丢包扫描（测量用，不是门槛）：往返 50 ms、20 Mbps，随机丢包 0–30%，办公操作。
#![allow(clippy::unwrap_used, clippy::print_stdout, clippy::indexing_slicing)]

use peerdesk_core::adaptive::Profile;
use peerdesk_core::sim::Link;
use peerdesk_e2e::{run, Action, EstimatorKind, Phase, Scenario};

#[test]
#[ignore = "测量用，手动运行：cargo test -p peerdesk-e2e --test loss_sweep -- --ignored --nocapture"]
fn loss_sweep() {
    let p = |action, frames| Phase { action, frames };
    println!("| 丢包 | 估计器 | 冻结次数 | 最长等待 ms | 平均延迟 ms | 打字 kbps | 停下后 PSNR | 变清晰 ms | 关键帧 | 恢复 |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for loss in [0u16, 30, 50, 100, 200, 300] {
        {
            let estimator = EstimatorKind::ClassicV1;
            let o = run(&Scenario {
                network: vec![(
                    0,
                    Link {
                        delay_us: 25_000,
                        jitter_us: 2_000,
                        loss_permille: loss,
                        rate_bps: Some(20_000_000),
                        ..Link::lan()
                    },
                )],
                // 真实会话里往返短时选的是“稳定”画像（不带纠错），实测如此
                profile: std::env::var("PEERDESK_PROFILE").map_or(Profile::STABLE, |v| {
                    if v == "wireless" {
                        Profile::WIRELESS
                    } else {
                        Profile::STABLE
                    }
                }),
                estimator,
                ..Scenario::new(
                    "loss-sweep",
                    vec![
                        p(Action::Idle, 30),
                        p(Action::Typing, 90),
                        p(Action::Scrolling, 60),
                        p(Action::Dragging, 60),
                        p(Action::Idle, 45),
                    ],
                )
            })
            .unwrap();
            let shown: Vec<_> = o.phases.iter().filter(|ph| ph.frames_shown > 0).collect();
            let lat =
                shown.iter().map(|ph| ph.latency_ms.0).sum::<f64>() / shown.len().max(1) as f64;
            let last = o.phases.last().unwrap();
            println!(
                "| {}% | {} | {} | {:.0} | {:.0} | {} | {:.1} | {} | {} | {} |",
                f64::from(loss) / 10.0,
                o.estimator,
                o.freezes,
                o.max_wait_ms,
                lat,
                o.phases[1].kbps,
                last.end_psnr,
                last.sharp_ms.map_or("未清晰".into(), |v| format!("{v:.0}")),
                o.keyframes,
                o.recoveries
            );
        }
    }
}
