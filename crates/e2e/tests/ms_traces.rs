//! 用微软 MMSys 2024 带宽估计挑战赛的仿真通话（真实带宽和丢包随时间变化）驱动端到端仿真。
//! 轨迹由 `scripts/ms-traces.py` 生成，在 `crates/e2e/traces/`（数据 CC BY 4.0）。
//! 数据里没有真实的传播延迟，统一用单向 25 ms ± 3 ms。
#![allow(
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::indexing_slicing,
    clippy::cast_precision_loss
)]

use peerdesk_core::adaptive::Profile;
use peerdesk_core::sim::Link;
use peerdesk_e2e::{
    run, Action, ContentPolicy, EstimatorKind, FeatureRow, Outcome, Phase, ProbeStats, Scenario,
    StateShare,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

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

/// 一次运行里量尺要的数字（整份 [`Outcome`] 带着每个阶段的截图，一次一百多兆，多次运行不能都留着）。
struct Sample {
    freezes: u32,
    max_wait_ms: f64,
    first_ms: f64,
    idle_psnr: f64,
    unsharp: bool,
    states: StateShare,
    probes: ProbeStats,
    sent_bytes: u64,
    /// 表格的一行、明细、事件记录（打开 `PEERDESK_EVENTS` 时）。
    row: String,
    detail: String,
    trace: Vec<String>,
}

/// 用哪个估计器：测量时可以用 `PEERDESK_ESTIMATOR`（legacy、v1、gcc）换估计器对比。
fn estimator() -> EstimatorKind {
    std::env::var("PEERDESK_ESTIMATOR")
        .ok()
        .and_then(|n| EstimatorKind::parse(&n))
        .unwrap_or(EstimatorKind::ClassicV1)
}

fn one(name: &str, csv: &str, seed: u64, estimator: EstimatorKind) -> Sample {
    let o = run(&Scenario {
        network: network(csv),
        profile: Profile::WIRELESS,
        estimator,
        start_bps: ContentPolicy::OFFICE.start_bps,
        seed,
        ..Scenario::new(name, workload())
    })
    .unwrap();
    if std::env::var_os("PEERDESK_SAVE").is_some() {
        peerdesk_e2e::report::save(std::path::Path::new("../../target/e2e-ms"), &o).unwrap();
    }
    // 逐行对比用（例如核对“跟踪状态和基线逐字节相同”）：只存特征表，不存截图
    if let Some(dir) = std::env::var_os("PEERDESK_FEATURES_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut csv = String::from(FeatureRow::HEADER);
        for f in &o.features {
            csv.push('\n');
            csv.push_str(&f.csv());
        }
        std::fs::write(dir.join(format!("{name}-{seed}.csv")), csv).unwrap();
    }
    let row = row(&format!("{name}#{seed}"), &o);
    Sample {
        freezes: o.freezes,
        max_wait_ms: o.max_wait_ms,
        first_ms: o.first_picture_ms.unwrap_or(f64::INFINITY),
        idle_psnr: idle_psnr(&o),
        unsharp: row.contains("未清晰"),
        states: o.states,
        probes: o.probes,
        sent_bytes: o.sent_bytes,
        row,
        detail: format!(
            "  网络丢 {}，排队丢 {}，跳过 {}，坏帧 {}，恢复 {}；{}；{}",
            o.net.lost,
            o.net.queue_dropped,
            o.skipped,
            o.bad,
            o.recoveries,
            o.states.line(),
            o.probes.line(o.sent_bytes)
        ),
        trace: o.trace,
    }
}

/// 同时跑几次运行：默认是核数，可用 `PEERDESK_JOBS` 改。覆盖率插桩的构建里只用 1 个线程：插桩的计数器是全局共享的，
/// 几个线程同时写互相抢缓存行，实测 4 个线程时每次运行要 148 秒 CPU（单线程 20 秒），并行反而慢 2 倍。
fn workers(jobs: usize) -> usize {
    let instrumented = std::env::var_os("LLVM_PROFILE_FILE").is_some();
    std::env::var("PEERDESK_JOBS")
        .ok()
        .and_then(|n| n.parse::<usize>().ok())
        .or_else(|| instrumented.then_some(1))
        .or_else(|| std::thread::available_parallelism().ok().map(usize::from))
        .unwrap_or(1)
        .clamp(1, jobs.max(1))
}

/// 把每条轨迹的每个种子各跑一遍，按（轨迹, 种子）的顺序返回。各次运行互不相干、各自确定，所以分到几个线程上
/// 并行跑，结果和串行逐字节相同（线程数见 [`workers`]）。
fn measure(traces: &[(&str, &str)], seeds: u64, estimator: EstimatorKind) -> Vec<Vec<Sample>> {
    let jobs: Vec<(usize, u64)> = (0..traces.len())
        .flat_map(|t| (1..=seeds).map(move |seed| (t, seed)))
        .collect();
    let workers = workers(jobs.len());
    let next = AtomicUsize::new(0);
    let done: Mutex<Vec<(usize, Sample)>> = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((t, seed)) = jobs.get(i).copied() else {
                    break;
                };
                let (name, csv) = traces[t];
                let sample = one(name, csv, seed, estimator);
                done.lock().unwrap().push((i, sample));
            });
        }
    });
    let mut done = done.into_inner().unwrap();
    done.sort_by_key(|(i, _)| *i);
    let mut out: Vec<Vec<Sample>> = traces.iter().map(|_| Vec::new()).collect();
    for (i, sample) in done {
        out[jobs[i].0].push(sample);
    }
    out
}

/// 均值和样本标准差（除以 n − 1；只有一个样本时标准差为 0）。
fn mean_sd(values: &[f64]) -> (f64, f64) {
    let n = values.len().max(1) as f64;
    let mean = values.iter().sum::<f64>() / n;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    (mean, var.sqrt())
}

#[test]
#[ignore = "测量用，手动运行：PEERDESK_SEEDS=12 cargo test -p peerdesk-e2e --release --test ms_traces microsoft_traces -- --ignored --exact --nocapture"]
fn microsoft_traces() {
    let only = std::env::var("PEERDESK_TRACES").unwrap_or_default();
    // 每条轨迹用几个随机种子各跑一遍（种子决定抖动、丢包和桌面内容）：单次结果对时序很敏感，
    // 改动前后的比较看每条轨迹多个种子的均值和标准差（PEERDESK_SEEDS，默认 1；版本对比用 12）
    let seeds = std::env::var("PEERDESK_SEEDS")
        .ok()
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or(1)
        .max(1);
    let traces: Vec<(&str, &str)> = TRACES
        .into_iter()
        .filter(|(name, _)| only.is_empty() || only.split(',').any(|t| t == *name))
        .collect();
    let started = std::time::Instant::now();
    let results = measure(&traces, seeds, estimator());
    println!("| 轨迹 | 冻结（>200 ms） | 最长等待 ms | 平均延迟 ms | 停下后最慢变清晰 ms | 停下后最差 PSNR | 首帧 ms | 关键帧 |");
    println!("|---|---|---|---|---|---|---|---|");
    let detail = std::env::var_os("PEERDESK_DETAIL").is_some();
    for sample in results.iter().flatten() {
        for line in &sample.trace {
            eprintln!("{line}");
        }
        println!("{}", sample.row);
        if detail {
            println!("{}", sample.detail);
        }
    }
    // 每条轨迹：多个种子的均值 ± 标准差（标准差说明单次结果有多不可靠；均值的标准误 = 标准差 ÷ √种子数）
    println!("\n| 轨迹 | 次数 | 冻结 均值 ± 标准差 | 冻结合计 | 最长等待 均值 ± 标准差 ms | 最长等待 最大 ms | 首帧最慢 ms | 没变清晰 |");
    println!("|---|---|---|---|---|---|---|---|");
    let (mut runs, mut freezes, mut unsharp, mut wait_sum, mut wait_max, mut psnr_sum) =
        (0u32, 0u32, 0u32, 0.0, 0.0f64, 0.0f64);
    let mut states = StateShare::default();
    let (mut probes, mut sent_bytes) = (ProbeStats::default(), 0u64);
    for ((name, _), samples) in traces.iter().zip(&results) {
        let f: Vec<f64> = samples.iter().map(|s| f64::from(s.freezes)).collect();
        let w: Vec<f64> = samples.iter().map(|s| s.max_wait_ms).collect();
        let ((fm, fs), (wm, ws)) = (mean_sd(&f), mean_sd(&w));
        let total: u32 = samples.iter().map(|s| s.freezes).sum();
        let worst = w.iter().copied().fold(0.0, f64::max);
        let first = samples.iter().map(|s| s.first_ms).fold(0.0, f64::max);
        let blurry = samples.iter().filter(|s| s.unsharp).count();
        println!(
            "| {name} | {} | {fm:.2} ± {fs:.2} | {total} | {wm:.0} ± {ws:.0} | {worst:.0} | {first:.0} | {blurry} |",
            samples.len()
        );
        for s in samples {
            runs += 1;
            freezes += s.freezes;
            wait_sum += s.max_wait_ms;
            wait_max = wait_max.max(s.max_wait_ms);
            psnr_sum += s.idle_psnr;
            unsharp += u32::from(s.unsharp);
            states.merge(&s.states);
            probes.merge(&s.probes);
            sent_bytes += s.sent_bytes;
        }
    }
    println!(
        "合计 {runs} 次：冻结 {freezes} 次，最长等待平均 {:.0} ms、最大 {wait_max:.0} ms，停下后没变清晰 {unsharp} 次，\
         停下后最差 PSNR 平均 {:.1} dB",
        wait_sum / f64::from(runs.max(1)),
        psnr_sum / f64::from(runs.max(1))
    );
    println!("{}", states.line());
    println!("{}", probes.line(sent_bytes));
    println!("用时 {:.0} 秒", started.elapsed().as_secs_f64());
}

/// 基线是用几个种子量的（种子 1 到这个数）。
const BASE_SEEDS: u64 = 12;
/// 回归门槛每条轨迹用几个种子（种子 1 到这个数）。发布构建 12 个，是真正把关的那一道（CI 的端到端任务，并行跑，
/// 4 核约 100 秒，和以前 9 次串行一样）。调试构建只用 3 个：它在覆盖率任务里跑，插桩之后每次运行 20 秒而且只能
/// 串行（见 [`workers`]），12 个种子要 12 分钟（以前 3 分钟）；调试构建这一道只拦明显的退步，细的交给发布构建。
const GATE_SEEDS: u64 = if cfg!(debug_assertions) { 3 } else { 12 };
/// 门槛 = 基线均值 + 这么多倍“两个均值之差的标准误”。2 倍：改动其实不好不坏时，一项指标误报的概率约 2.3%。
const GATE_Z: f64 = 2.0;

/// 一条轨迹的基线：[`BASE_SEEDS`] 个种子上量出的（均值, 样本标准差），和首帧上限。
struct Baseline {
    name: &'static str,
    /// 每次运行的冻结次数。
    freezes: (f64, f64),
    /// 每次运行的最长等待（毫秒）。
    wait_ms: (f64, f64),
    /// 首帧最慢不超过多少毫秒（硬上限，不带统计余量：首帧时间几乎不随种子变）。
    first_ms: f64,
}

/// 新版本 `n` 个种子的均值不许超过的线：基线均值 + [`GATE_Z`] × 标准差 × √(1/基线种子数 + 1/n)。
///
/// 依据：换个种子，单次结果差很多（1 Mbps、20% 丢包的 ms-05729 冻结 6.5 ± 2.6 次）；任何改动只要动了时序，
/// 每个种子的结果就像重新抽了一次。基线是 [`BASE_SEEDS`] 次的均值，标准误 σ/√12；新版本是 n 次的均值，标准误 σ/√n；
/// 两者之差的标准误是 σ·√(1/12 + 1/n)。“新均值 − 基线均值”超过它的 2 倍才算真的变差。
/// n = 12 时余量是 0.82 个标准差，n = 3 时是 1.29 个。
///
/// 冻结是计数，标准差至少按泊松（√均值）算，基线全是 0 时按“12 次里出 1 次”算：12 次都是 0 的轨迹量出来标准差是 0，
/// 不设下限的话以后任何一个种子出一次刚过 200 ms 的等待就不过。
fn limit((mean, sd): (f64, f64), n: u64, count: bool) -> f64 {
    let (base, n) = (BASE_SEEDS as f64, n.max(1) as f64);
    let sd = if count {
        sd.max(mean.max(1.0 / base).sqrt())
    } else {
        sd
    };
    mean + GATE_Z * sd * (1.0 / base + 1.0 / n).sqrt()
}

/// 回归门槛（每次 CI 都跑）：四条有代表性的真实轨迹，每条 [`GATE_SEEDS`] 个随机种子，按均值和 [`limit`] 判断。
///
/// 2026-11-26 以前是 3 个种子的合计加两成余量，这把尺子分不出好坏：同一份代码 12 个种子里任取 3 个，ms-05729 的
/// 冻结合计在 17–36 之间，而门槛是 19（按种子 1–3 的 17 次定的，低于 12 个种子的均值 6.5 × 3 = 19.5）——
/// 一个不好不坏、只是动了时序的改动有一半的机会不过。当天的五次尝试里“22 对 19、7 对 6、5 对 4”都在噪声里。
/// 现在按 [`limit`]：误报约 2.3%（每项）；发布构建上差出 0.8 个标准差以上（ms-05729 冻结 +33%）就不过。
/// 详见 `docs/research/L-framework.md` 6.6。
///
/// 规则（只升不降）：
/// - 改进之后把基线换成新的实测（均值和标准差一起换）；按新实测算出的线比旧线宽的那一项留旧的，线只收紧。
/// - 没过时先跑完整的 12 条 × 12 个种子（上面的 `microsoft_traces`）和基线比。确实变差就改代码；144 次里看不出
///   变差（合计的差在两倍标准误之内）才可以按新实测重定这里的基线，并在提交信息里写上 144 次的数据。
/// - 防基线漂移：144 次的比较一律对照固定的文件 `docs/research/data/ms-traces-baseline-2026-11-28.md`
///   （或者以后正式改进之后存下的新基线文件），不对照上一次“重定”的数字——否则一连串“看不出变差”的改动
///   每次把线往上推一点，累计起来就退步了。
///
/// 用时见 [`GATE_SEEDS`]：2026-11-28 加了第四条轨迹，两种构建都比以前多三分之一。
#[test]
fn microsoft_traces_do_not_regress() {
    // 发布构建（CI 的端到端任务，720p）和调试构建（覆盖率任务，画面 480×272，数字不一样）各有一套基线。
    // 2026-11-26 在 ui-bugs 的码率控制上量的（种子 1–12，见 docs/research/L-framework.md 6.6）。
    // 2026-11-28 在 #71（码率控制、画质优先的帧率调度）上重量，只收紧：新的均值和标准差算出的线比旧的紧才换，
    // 否则留旧的（注释里写了新实测）；首帧上限换成新实测的最慢值（向上取整到毫秒）；加 ms-02651（见 docs/research/P-fps-bitrate.md 第 10 节）。
    let baselines = if cfg!(debug_assertions) {
        [
            Baseline {
                name: "ms-09401", // 0.5–1.5 Mbps 变化
                freezes: (0.0, 0.0),
                wait_ms: (135.3, 17.2), // 11-28；11-26 是 (127.1, 27.6)，线 163 → 158
                first_ms: 83.0,         // 11-28 实测 82.14；以前 98
            },
            Baseline {
                name: "ms-02560",        // 8 Mbps、12% 丢包
                freezes: (0.833, 1.030), // 11-26（11-28 实测 1.17 ± 1.12，按它定线会放宽）
                wait_ms: (226.2, 124.6), // 11-26（11-28 实测 237 ± 117，同上）
                first_ms: 35.0,          // 11-28 实测 34.20；以前 41
            },
            Baseline {
                name: "ms-05729",        // 1 Mbps、20% 丢包
                freezes: (3.917, 1.730), // 11-28；11-26 是 (4.0, 2.132)，线 6.75 → 6.47
                wait_ms: (335.3, 96.8),  // 11-28；11-26 是 (327.7, 106.8)，线 466 → 460
                first_ms: 85.0,          // 11-28 实测 84.27；以前 101
            },
            Baseline {
                name: "ms-02651", // 0.9–1.3 Mbps 稳定、不丢包
                freezes: (0.0, 0.0),
                wait_ms: (109.8, 19.5),
                first_ms: 90.0, // 11-28 实测 89.58
            },
        ]
    } else {
        [
            Baseline {
                name: "ms-09401",
                freezes: (1.167, 1.193), // 11-28；11-26 是 (1.583, 0.996)，线 2.61 → 2.14
                wait_ms: (320.9, 130.4), // 11-26（11-28 实测 318 ± 163，按它定线会放宽）
                first_ms: 110.0,         // 11-28 实测 109.34；以前 128
            },
            Baseline {
                name: "ms-02560",
                freezes: (0.167, 0.389), // 11-28；11-26 是 (0.5, 0.798)，线 1.15 → 0.50
                wait_ms: (182.0, 65.0),  // 11-28；11-26 是 (188.8, 63.7)，线 241 → 235
                first_ms: 39.0,          // 11-28 实测 38.36；以前 44
            },
            Baseline {
                name: "ms-05729",
                freezes: (6.5, 2.646), // 11-26（11-28 实测 7.00 ± 2.37，按它定线会放宽）
                wait_ms: (411.2, 136.4), // 11-26（11-28 实测 429 ± 163，同上）
                first_ms: 113.0,       // 11-28 实测 112.53；以前 132
            },
            Baseline {
                name: "ms-02651", // 0.9–1.3 Mbps 稳定、不丢包（#71 修帧率码率不稳时的主要对象）
                freezes: (1.083, 1.0),
                wait_ms: (265.0, 123.0),
                first_ms: 121.0, // 11-28 实测 120.50
            },
        ]
    };
    let traces: Vec<(&str, &str)> = baselines
        .iter()
        .map(|b| *TRACES.iter().find(|(n, _)| *n == b.name).unwrap())
        .collect();
    let results = measure(&traces, GATE_SEEDS, EstimatorKind::ClassicV1);
    let mut failed = Vec::new();
    let mut check = |what: String, got: f64, base: f64, limit: f64| {
        let ok = got <= limit;
        println!(
            "{what}：实测 {got:.2}，基线 {base:.2}，门槛 {limit:.2}{}",
            if ok { "" } else { "  ← 没过" }
        );
        if !ok {
            failed.push(what);
        }
    };
    // 几条轨迹合在一起再看一遍（均值相加，方差相加）：各条都差一点、单看都没过线的全面退步，合起来看得出
    let (mut all_freezes, mut all_wait) = ((0.0, 0.0, 0.0), (0.0, 0.0, 0.0));
    let mut states = StateShare::default();
    for (b, samples) in baselines.iter().zip(&results) {
        for s in samples {
            println!("{}", s.row);
            states.merge(&s.states);
        }
        let f: Vec<f64> = samples.iter().map(|s| f64::from(s.freezes)).collect();
        let w: Vec<f64> = samples.iter().map(|s| s.max_wait_ms).collect();
        let (freezes, wait) = (mean_sd(&f).0, mean_sd(&w).0);
        let first = samples.iter().map(|s| s.first_ms).fold(0.0, f64::max);
        let name = b.name;
        let n = GATE_SEEDS;
        let freeze_limit = limit(b.freezes, n, true);
        let wait_limit = limit(b.wait_ms, n, false);
        check(
            format!("{name} 冻结（{n} 次均值）"),
            freezes,
            b.freezes.0,
            freeze_limit,
        );
        check(
            format!("{name} 最长等待 ms（{n} 次均值）"),
            wait,
            b.wait_ms.0,
            wait_limit,
        );
        check(format!("{name} 首帧最慢 ms"), first, b.first_ms, b.first_ms);
        // 合计的余量：各条余量的平方和再开方（方差相加）
        all_freezes.0 += freezes;
        all_freezes.1 += b.freezes.0;
        all_freezes.2 += (freeze_limit - b.freezes.0).powi(2);
        all_wait.0 += wait;
        all_wait.1 += b.wait_ms.0;
        all_wait.2 += (wait_limit - b.wait_ms.0).powi(2);
    }
    check(
        "合计 冻结".to_owned(),
        all_freezes.0,
        all_freezes.1,
        all_freezes.1 + all_freezes.2.sqrt(),
    );
    check(
        "合计 最长等待 ms".to_owned(),
        all_wait.0,
        all_wait.1,
        all_wait.1 + all_wait.2.sqrt(),
    );
    println!("{}", states.line());
    assert!(failed.is_empty(), "回归门槛没过：{}", failed.join("；"));
}

#[test]
fn gate_limits_follow_the_standard_error_of_a_difference() {
    // 12 个种子：余量 = 2 × 标准差 × √(1/12 + 1/12) = 0.8165 × 标准差
    assert!((limit((6.5, 2.646), 12, true) - 8.660).abs() < 0.001);
    assert!((limit((411.2, 136.4), 12, false) - 522.57).abs() < 0.01);
    // 3 个种子：余量 = 2 × 标准差 × √(1/12 + 1/3) = 1.291 × 标准差
    assert!((limit((4.0, 2.132), 3, true) - 6.752).abs() < 0.001);
    // 计数的标准差至少按泊松：均值 1.583 时 √1.583 = 1.258 > 0.996
    assert!((limit((1.583, 0.996), 12, true) - 2.610).abs() < 0.001);
    // 基线全是 0：按“12 次里出 1 次”。12 个种子容得下 2 次（均值 0.167）、容不下 3 次（0.25）；
    // 3 个种子容得下 1 次（0.333）、容不下 2 次
    let zero = limit((0.0, 0.0), 12, true);
    assert!(zero > 2.0 / 12.0 && zero < 3.0 / 12.0, "{zero}");
    let zero = limit((0.0, 0.0), 3, true);
    assert!(zero > 1.0 / 3.0 && zero < 2.0 / 3.0, "{zero}");
    // 不是计数的不设下限；种子越多线越紧
    assert!((limit((100.0, 0.0), 12, false) - 100.0).abs() < 1e-9);
    assert!(limit((6.5, 2.646), 48, true) < limit((6.5, 2.646), 12, true));
    assert!(limit((1.0, 1.0), 0, false).is_finite());
    let (mean, sd) = mean_sd(&[1.0, 2.0, 3.0]);
    assert!((mean - 2.0).abs() < 1e-9 && (sd - 1.0).abs() < 1e-9);
    assert!((mean_sd(&[5.0]).1).abs() < 1e-9);
    assert!((mean_sd(&[]).0).abs() < 1e-9);
    // 线程数不超过运行次数，至少 1
    assert_eq!(workers(1), 1);
    assert_eq!(workers(0), 1);
}
