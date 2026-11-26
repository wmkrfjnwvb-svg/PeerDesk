//! 音画同步和播放缓冲的测量（见 `docs/research/N-av-sync.md`）：带抖动的链路上放视频加声音，
//! 播放缓冲关着和打开时，画面匀不匀、声音和画面差多少、各自多大延迟。
//!
//! 用 release 跑：`cargo test --release -p peerdesk-e2e --test avsync -- --nocapture`。
#![allow(clippy::unwrap_used, clippy::print_stdout)]

use peerdesk_core::sim::Link;
use peerdesk_e2e::avsync::{run_av, AvOutcome, AvScenario};

/// 抖动 30 ms 的链路（隔着墙的 Wi-Fi、拥挤的宽带）。
fn jittery() -> Link {
    Link {
        delay_us: 20_000,
        jitter_us: 30_000,
        rate_bps: Some(20_000_000),
        ..Link::ideal()
    }
}

/// 一次测量：release 里 20 秒；调试构建（覆盖率）里编码慢得多，放 8 秒。
fn scenario(link: Link, playout_ms: u32) -> AvScenario {
    AvScenario {
        frames: if cfg!(debug_assertions) { 240 } else { 600 },
        ..AvScenario::new(link, playout_ms)
    }
}

fn row(name: &str, playout_ms: u32, o: &AvOutcome) -> String {
    format!(
        "| {name} | {} | {} | {:.1} / {:.1} / {:.1} | {} | {:.0} / {:.0} | {:.0} / {:.0} | {:+.0} / {:+.0} / {:+.0} | {} | {} / {} / {} |",
        if playout_ms == 0 { "关".to_owned() } else { format!("{playout_ms} ms") },
        o.shown,
        o.interval_error_ms.0,
        o.interval_error_ms.1,
        o.interval_error_ms.2,
        o.late,
        o.video_latency_ms.0,
        o.video_latency_ms.1,
        o.audio_latency_ms.0,
        o.audio_latency_ms.1,
        o.av_diff_ms.0,
        o.av_diff_ms.1,
        o.av_diff_ms.2,
        o.out_of_sync,
        o.audio.concealed,
        o.audio.accelerated,
        o.audio.decelerated,
    )
}

const HEADER: &str = "| 链路 | 播放延迟 | 显示帧数 | 显示间隔偏差 ms（平均 / p95 / 最大） | 迟到帧 | 画面延迟 ms（p50 / p95） | 声音延迟 ms（p50 / p95） | 音画差 ms（最小 / 中位 / 最大，正数是声音早） | 能察觉的帧 | 声音 补偿 / 加速 / 减速 |\n|---|---|---|---|---|---|---|---|---|---|";

#[test]
fn playout_buffer_on_jittery_links() {
    println!("\n{HEADER}");
    let links = [
        ("抖动 30 ms", jittery()),
        ("Wi-Fi（抖动 8 ms，丢包 1%）", Link::wifi()),
        ("蜂窝（抖动 20 ms，丢包 2%）", Link::cellular()),
    ];
    for (name, link) in links {
        let off = run_av(&scenario(link, 0)).unwrap();
        println!("{}", row(name, 0, &off));
        for playout_ms in [50, 150, 250] {
            let on = run_av(&scenario(link, playout_ms)).unwrap();
            println!("{}", row(name, playout_ms, &on));
            // 门槛（150 ms 是建议的默认值）：音画差察觉不到；没有丢包的链路上画面原样重现节奏。
            // 有丢包的链路上等重传的帧会迟到（马上显示），只要求是少数。
            if playout_ms >= 150 {
                assert_eq!(on.out_of_sync, 0, "{name} {playout_ms} ms：{on:?}");
                assert!(
                    on.av_diff_ms.1.abs() <= 10.0,
                    "{name} {playout_ms} ms：{on:?}"
                );
                assert!(
                    on.late * 50 <= u64::from(on.shown),
                    "{name} {playout_ms} ms：{on:?}"
                );
                assert!(on.interval_error_ms.0 < off.interval_error_ms.0);
                if link.loss_permille == 0 {
                    assert_eq!(on.late, 0, "{name} {playout_ms} ms：{on:?}");
                    assert!(
                        on.interval_error_ms.2 < 2.0
                            && on.av_diff_ms.0 >= -10.0
                            && on.av_diff_ms.2 <= 10.0,
                        "{name} {playout_ms} ms：{on:?}"
                    );
                }
            }
        }
    }
}
