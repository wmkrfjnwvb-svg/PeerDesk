//! 用微软 MMSys 2024 带宽估计挑战赛的仿真通话（真实带宽和丢包随时间变化）驱动端到端仿真。
//! 轨迹由 `scripts/ms-traces.py` 生成，在 `crates/e2e/traces/`（数据 CC BY 4.0）。
//! 数据里没有真实的传播延迟，统一用单向 25 ms ± 3 ms。
#![allow(clippy::unwrap_used, clippy::print_stdout, clippy::indexing_slicing)]

use peerdesk_core::adaptive::Profile;
use peerdesk_core::sim::Link;
use peerdesk_e2e::{run, Action, ContentPolicy, EstimatorKind, Outcome, Phase, Scenario};

const TRACES: [(&str, &str); 12] = [
    ("ms-09401", include_str!("../traces/ms-09401.csv")),
    ("ms-02626", include_str!("../traces/ms-02626.csv")),
    ("ms-02605", include_str!("../traces/ms-02605.csv")),
    ("ms-02683", include_str!("../traces/ms-02683.csv")),
    ("ms-09235", include_str!("../traces/ms-09235.csv")),
    ("ms-05729", include_str!("../traces/ms-05729.csv")),
    ("ms-02560", include_str!("../traces/ms-02560.csv")),
    ("ms-09261", include_str!("../traces/ms-09261.csv")),
    ("ms-02651", include_str!("../traces/ms-02651.csv")),
    ("ms-05719", include_str!("../traces/ms-05719.csv")),
    ("ms-09217", include_str!("../traces/ms-09217.csv")),
    ("ms-02703", include_str!("../traces/ms-02703.csv")),
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

/// 停下（第一段之后的空闲段）结束时最差的画质。
fn idle_psnr(o: &Outcome) -> f64 {
    o.phases
        .iter()
        .filter(|ph| ph.action == Action::Idle)
        .skip(1)
        .map(|ph| ph.end_psnr)
        .fold(f64::INFINITY, f64::min)
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
    let psnr = idle_psnr(o);
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
    // 每条轨迹用几个随机种子各跑一遍（种子决定抖动、丢包和桌面内容）：单次结果对时序很敏感，
    // 改动前后的比较看全部轨迹、全部种子的合计（PEERDESK_SEEDS，默认 1）
    let seeds = std::env::var("PEERDESK_SEEDS")
        .ok()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(1)
        .max(1);
    let (mut runs, mut freezes, mut unsharp, mut wait_sum, mut wait_max, mut psnr_sum) =
        (0u32, 0u32, 0u32, 0.0, 0.0f64, 0.0f64);
    for (name, csv) in TRACES {
        if !only.is_empty() && !only.split(',').any(|t| t == name) {
            continue;
        }
        for seed in 1..=seeds {
            // 测量时可以用 PEERDESK_ESTIMATOR（legacy、v1、gcc）换估计器对比
            let estimator = std::env::var("PEERDESK_ESTIMATOR")
                .ok()
                .and_then(|n| EstimatorKind::parse(&n))
                .unwrap_or(EstimatorKind::ClassicV1);
            let o = run(&Scenario {
                network: network(csv),
                profile: Profile::WIRELESS,
                estimator,
                start_bps: ContentPolicy::OFFICE.start_bps,
                seed,
                ..Scenario::new(name, workload())
            })
            .unwrap();
            runs += 1;
            freezes += o.freezes;
            wait_sum += o.max_wait_ms;
            wait_max = wait_max.max(o.max_wait_ms);
            psnr_sum += idle_psnr(&o);
            unsharp += u32::from(row(name, &o).contains("未清晰"));
            let name = if seeds > 1 {
                format!("{name}#{seed}")
            } else {
                name.to_owned()
            };
            let name = name.as_str();
            for line in &o.trace {
                eprintln!("{line}");
            }
            println!("{}", row(name, &o));
            if std::env::var_os("PEERDESK_SAVE").is_some() {
                peerdesk_e2e::report::save(std::path::Path::new("../../target/e2e-ms"), &o)
                    .unwrap();
            }
            if std::env::var_os("PEERDESK_DETAIL").is_some() {
                println!(
                    "  网络丢 {}，排队丢 {}，跳过 {}，坏帧 {}，恢复 {}",
                    o.net.lost, o.net.queue_dropped, o.skipped, o.bad, o.recoveries
                );
            }
        }
    }
    println!(
        "合计 {runs} 次：冻结 {freezes} 次，最长等待平均 {:.0} ms、最大 {wait_max:.0} ms，停下后没变清晰 {unsharp} 次，\
         停下后最差 PSNR 平均 {:.1} dB",
        wait_sum / f64::from(runs.max(1)),
        psnr_sum / f64::from(runs.max(1))
    );
}

/// 回归门槛（每次 CI 都跑）：三条有代表性的真实轨迹，每条用 3 个随机种子各跑一遍，按合计判断
/// （单个种子的结果对时序很敏感，同一条轨迹换个种子卡顿能差几倍，见 `docs/research/L-framework.md` 6.2）。
/// 指标不许比收紧时的结果差（留两成余量）。数字只升不降：改进之后把门槛收紧。
#[test]
fn microsoft_traces_do_not_regress() {
    // (轨迹, 3 次冻结合计上限, 3 次最长等待的平均上限 ms, 首帧上限 ms)。发布构建（CI 的端到端任务，720p）和
    // 调试构建（覆盖率任务，画面 480×272，数字不一样）各有一套。改进之后收紧：新门槛取旧门槛和“实测加两成
    // （次数很少时至少加 2）”里严的那个。
    // 2026-11-25：按改正后的等待统计重定；之后变化面积按细节加权、整帧重传，再收紧。
    let gates = if cfg!(debug_assertions) {
        [
            ("ms-09401", 2, 203.0, 99.0),   // 实测 0 次 / 169 ms / 82 ms
            ("ms-02560", 4, 226.0, 41.0),   // 实测 2 次 / 188 ms / 34 ms
            ("ms-05729", 15, 462.0, 101.0), // 实测 12 次 / 385 ms / 84 ms
        ]
    } else {
        [
            ("ms-09401", 6, 547.0, 128.0), // 0.5–1.5 Mbps 变化：实测 4 次 / 456 ms / 107 ms
            // 8 Mbps、12% 丢包：实测 3 次 / 198 ms / 37 ms。冻结次数门槛从 2 放到 3（唯一放宽的一处）：变化面积按细节
            // 加权之后，会话开头码率还在从 1 Mbps 往上爬时多了一两次刚过线的卡顿（213–225 ms），36 次评估集上总的
            // 冻结 145 → 87 次（docs/research/L-framework.md 6.2）；最长等待平均从 214 收紧到 198 ms
            ("ms-02560", 3, 237.0, 45.0),
            ("ms-05729", 23, 602.0, 132.0), // 1 Mbps、20% 丢包：实测 19 次 / 502 ms / 110 ms（之前 52 次 / 905 ms）
        ]
    };
    for (name, freezes, mean_wait, first) in gates {
        let csv = TRACES.iter().find(|(n, _)| *n == name).unwrap().1;
        let (mut total_freezes, mut wait_sum, mut first_max) = (0u32, 0.0f64, 0.0f64);
        for seed in 1..=3 {
            let o = run(&Scenario {
                network: network(csv),
                profile: Profile::WIRELESS,
                estimator: EstimatorKind::ClassicV1,
                start_bps: ContentPolicy::OFFICE.start_bps,
                seed,
                ..Scenario::new(name, workload())
            })
            .unwrap();
            println!("{}", row(&format!("{name}#{seed}"), &o));
            total_freezes += o.freezes;
            wait_sum += o.max_wait_ms;
            first_max = first_max.max(o.first_picture_ms.unwrap_or(f64::INFINITY));
        }
        let mean = wait_sum / 3.0;
        assert!(
            total_freezes <= freezes,
            "{name}：3 次冻结合计 {total_freezes} 次，门槛 {freezes}"
        );
        assert!(
            mean <= mean_wait,
            "{name}：3 次最长等待平均 {mean:.0} ms，门槛 {mean_wait}"
        );
        assert!(
            first_max <= first,
            "{name}：首帧最慢 {first_max:.0} ms，门槛 {first}"
        );
    }
}
