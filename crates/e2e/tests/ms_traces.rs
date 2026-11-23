//! 用微软 MMSys 2024 带宽估计挑战赛的仿真通话（真实带宽和丢包随时间变化）驱动端到端仿真。
//! 轨迹由 `scripts/ms-traces.py` 生成，在 `crates/e2e/traces/`（数据 CC BY 4.0）。
//! 数据里没有真实的传播延迟，统一用单向 25 ms ± 3 ms。
#![allow(clippy::unwrap_used, clippy::print_stdout, clippy::indexing_slicing)]

use peerdesk_core::adaptive::Profile;
use peerdesk_core::sim::Link;
use peerdesk_e2e::{run, Action, EstimatorKind, Outcome, Phase, Scenario};

const TRACES: [(&str, &str); 8] = [
    ("ms-09401", include_str!("../traces/ms-09401.csv")),
    ("ms-02626", include_str!("../traces/ms-02626.csv")),
    ("ms-02605", include_str!("../traces/ms-02605.csv")),
    ("ms-02683", include_str!("../traces/ms-02683.csv")),
    ("ms-09235", include_str!("../traces/ms-09235.csv")),
    ("ms-05729", include_str!("../traces/ms-05729.csv")),
    ("ms-02560", include_str!("../traces/ms-02560.csv")),
    ("ms-09261", include_str!("../traces/ms-09261.csv")),
];

/// 轨迹文件 → 仿真网络：每一行“毫秒,带宽 bps,丢包千分比”是从这一刻起的链路。
/// 瓶颈的排队上限（毫秒）：数据里没有，默认按产品仿真的 200 ms，可用 `PEERDESK_QUEUE_MS` 改。
fn queue_us() -> u64 {
    std::env::var("PEERDESK_QUEUE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(200_000, |ms| ms * 1000)
}

fn network(csv: &str) -> Vec<(u64, Link)> {
    let max_queue_us = queue_us();
    csv.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<u64> = l.split(',').map(|x| x.trim().parse().unwrap()).collect();
            let link = Link {
                delay_us: 25_000,
                jitter_us: 3_000,
                loss_permille: u16::try_from(f[2]).unwrap(),
                rate_bps: Some(f[1]),
                max_queue_us,
                ..Link::lan()
            };
            (f[0] * 1000, link)
        })
        .collect()
}

fn workload() -> Vec<Phase> {
    let p = |action, frames| Phase { action, frames };
    let office = [
        p(Action::Idle, 30),
        p(Action::Typing, 90),
        p(Action::Idle, 30),
        p(Action::Scrolling, 60),
        p(Action::Idle, 30),
        p(Action::Dragging, 60),
        p(Action::Idle, 30),
        p(Action::Switching, 15),
        p(Action::Idle, 30),
        p(Action::Video, 90),
        p(Action::Idle, 30),
    ];
    office.iter().chain(office.iter()).copied().collect()
}

fn row(name: &str, o: &Outcome) -> String {
    let shown: Vec<_> = o.phases.iter().filter(|ph| ph.frames_shown > 0).collect();
    let lat = shown.iter().map(|ph| ph.latency_ms.0).sum::<f64>() / shown.len().max(1) as f64;
    // 停下后最慢多久变清晰；有一次没变清晰就记“未清晰”
    let idle: Vec<_> = o
        .phases
        .iter()
        .filter(|ph| ph.action == Action::Idle)
        .skip(1)
        .collect();
    let sharp = if idle.iter().any(|ph| ph.sharp_ms.is_none()) {
        "未清晰".to_owned()
    } else {
        format!(
            "{:.0}",
            idle.iter().filter_map(|ph| ph.sharp_ms).fold(0.0, f64::max)
        )
    };
    let psnr = idle
        .iter()
        .map(|ph| ph.end_psnr)
        .fold(f64::INFINITY, f64::min);
    format!(
        "| {name} | {} | {:.0} | {:.0} | {} | {psnr:.1} | {} | {} |",
        o.freezes,
        o.max_wait_ms,
        lat,
        sharp,
        o.first_picture_ms
            .map_or("无".into(), |v| format!("{v:.0}")),
        o.keyframes,
    )
}

#[test]
#[ignore = "测量用，手动运行：cargo test -p peerdesk-e2e --release --test ms_traces -- --ignored --nocapture"]
fn microsoft_traces() {
    println!("| 轨迹 | 冻结（>200 ms） | 最长等待 ms | 平均延迟 ms | 停下后最慢变清晰 ms | 停下后最差 PSNR | 首帧 ms | 关键帧 |");
    println!("|---|---|---|---|---|---|---|---|");
    let only = std::env::var("PEERDESK_TRACES").unwrap_or_default();
    for (name, csv) in TRACES {
        if !only.is_empty() && !only.split(',').any(|t| t == name) {
            continue;
        }
        let o = run(&Scenario {
            network: network(csv),
            profile: Profile::WIRELESS,
            estimator: EstimatorKind::ClassicV1,
            start_bps: 1_000_000,
            ..Scenario::new(name, workload())
        })
        .unwrap();
        println!("{}", row(name, &o));
        if std::env::var_os("PEERDESK_DETAIL").is_some() {
            println!(
                "  网络丢 {}，排队丢 {}，跳过 {}，坏帧 {}，恢复 {}",
                o.net.lost, o.net.queue_dropped, o.skipped, o.bad, o.recoveries
            );
        }
    }
}
