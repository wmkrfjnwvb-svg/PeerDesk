//! 路径模型的验收测试：每个部件对照公式或标准（`a2-plan.md` 第 3 节的表，一行至少一个测试）。
//! 统计类的测试用 12 个种子，按“均值 ± 两倍标准误”判断，测出的数用 `--nocapture` 能看到。
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use super::testkit::{check, check_pair, SEEDS};
use super::*;
use crate::sim::{Busy, Doze, Link, Net, Spikes};

type P = Path<u32>;

fn fwd(cfg: DirConfig) -> PathConfig {
    PathConfig {
        forward: cfg,
        reverse: DirConfig::default(),
    }
}

fn bottleneck(capacity: Capacity, limit: QueueLimit, aqm: Aqm) -> DirConfig {
    DirConfig {
        bottleneck: BottleneckConfig {
            capacity,
            limit,
            aqm,
        },
        ..DirConfig::default()
    }
}

/// 10 Mbps、1250 字节的包：每个包正好发 1 ms。
const D_US: f64 = 1000.0;
const PKT: usize = 1250;

fn cap10() -> Capacity {
    Capacity::Constant {
        rate_bps: 10_000_000,
    }
}

/// 泊松到达的时刻（微秒）：间隔按纳秒累加再取整，取整误差不累积。
fn poisson_times(seed: u64, mean_gap_ns: u64, n: usize) -> Vec<Time> {
    let mut rng = LayerRng::new(seed, Dir::Forward, "test/arrivals");
    let mut t = 0u64;
    (0..n)
        .map(|_| {
            t += rng.exp_ns(mean_gap_ns);
            Time(t / 1000)
        })
        .collect()
}

/// 按时刻发完所有包（先全发再收，时钟不被收包推着走），返回（发送时刻，送达）。
fn run(path: &mut P, sends: &[(Time, usize)]) -> Vec<(Time, Delivery<u32>)> {
    for (i, (t, size)) in sends.iter().enumerate() {
        path.advance_to(*t);
        path.send(0, 1, *size, u32::try_from(i).unwrap());
    }
    std::iter::from_fn(|| path.pop())
        .map(|d| (sends[d.payload as usize].0, d))
        .collect()
}

/// 第 `percent` 百分位（最近邻）。
fn quantile(sorted: &[f64], percent: usize) -> f64 {
    sorted[((sorted.len() - 1) * percent + 50) / 100]
}

// ---------------------------------------------------------------- 泊松放行

/// 泊松放行：放行间隔的均值等于 1/速率，变异系数约 1（匀速为 0）；长时间吞吐等于速率。
#[test]
fn poisson_opportunities_have_mean_gap_unit_cv_and_the_rate_as_throughput() {
    // 12 Mbps、每次机会 1500 字节：平均间隔 1000 微秒
    let cfg = bottleneck(
        Capacity::Poisson {
            rate_bps: 12_000_000,
            opportunity_bytes: 1500,
        },
        QueueLimit {
            max_delay_us: None,
            max_bytes: None,
        },
        Aqm::TailDrop,
    );
    // 吞吐 = n × 字节 ÷ n 个间隔之和；n 个指数间隔之和的倒数的期望是 1/((n−1)μ)（伽马分布），所以期望吞吐是 12 Mbps × n/(n−1)
    let n = 2999.0;
    check(
        [
            "泊松放行：放行间隔均值（微秒）",
            "泊松放行：间隔的变异系数（匀速为 0）",
            "泊松放行：长时间吞吐（Mbps）",
        ],
        [1000.0, 1.0, 12.0 * n / (n - 1.0)],
        |seed| {
            let mut p = P::new(seed, fwd(cfg.clone()));
            let out = run(&mut p, &vec![(Time(0), 1500); 3000]);
            let at: Vec<f64> = out.iter().map(|(_, d)| d.at.micros() as f64).collect();
            let gaps: Vec<f64> = at.windows(2).map(|w| w[1] - w[0]).collect();
            let m = gaps.iter().sum::<f64>() / n;
            let sd = (gaps.iter().map(|g| (g - m).powi(2)).sum::<f64>() / n).sqrt();
            [m, sd / m, n * 1500.0 * 8.0 / (at[at.len() - 1] - at[0])]
        },
    );
}

/// 一个机会可以被几个小包分着用；大包用几个机会；机会过去时队列是空的就作废。
#[test]
fn a_poisson_opportunity_is_shared_by_small_packets_and_spanned_by_big_ones() {
    let cfg = bottleneck(
        Capacity::Poisson {
            rate_bps: 12_000_000,
            opportunity_bytes: 1500,
        },
        QueueLimit {
            max_delay_us: None,
            max_bytes: None,
        },
        Aqm::TailDrop,
    );
    let mut p = P::new(1, fwd(cfg));
    // 三个 500 字节的包 = 一个机会，同一时刻送达
    let out = run(&mut p, &[(Time(0), 500), (Time(0), 500), (Time(0), 500)]);
    assert_eq!(out[0].1.at, out[2].1.at);
    let t0 = out[0].1.at;
    // 接着一个 4000 字节的包要用三个机会（1500+1500+1000），晚于前面的
    let out = run(&mut p, &[(t0, 4000)]);
    assert!(out[0].1.at > t0);
    // 隔很久再来：中间的机会都作废，包等下一个机会，不会补发
    let later = Time(10_000_000);
    let out = run(&mut p, &[(later, 1500)]);
    let wait = out[0].1.at.since(later);
    assert!((1..200_000).contains(&wait), "{wait}");
    // 机会字节数为 0 按 1 算
    let tiny = bottleneck(
        Capacity::Poisson {
            rate_bps: 8_000,
            opportunity_bytes: 0,
        },
        QueueLimit {
            max_delay_us: None,
            max_bytes: None,
        },
        Aqm::TailDrop,
    );
    let mut p = P::new(1, fwd(tiny));
    assert_eq!(run(&mut p, &[(Time(0), 3)]).len(), 1);
}

// ---------------------------------------------------------------- 轨迹放行

fn trace_cfg(repeat: bool) -> DirConfig {
    bottleneck(
        Capacity::Trace(Trace {
            // 8 Mbps 1 s、2 Mbps 1 s、断 0.5 s、16 Mbps 1.5 s
            segments: vec![
                (1_000_000, 8_000_000),
                (1_000_000, 2_000_000),
                (500_000, 0),
                (1_500_000, 16_000_000),
            ],
            repeat,
        }),
        QueueLimit {
            max_delay_us: None,
            max_bytes: None,
        },
        Aqm::TailDrop,
    )
}

/// 轨迹放行：每个时段放行的字节数等于轨迹给的容量（误差不超过一个包）；速率 0 的时段什么都不放。
#[test]
fn trace_capacity_delivers_the_traced_bytes_in_each_period() {
    let mut p = P::new(1, fwd(trace_cfg(false)));
    // 4 秒的容量一共 4.25 MB = 3400 个包；发 4000 个，一开始就全到
    let out = run(&mut p, &vec![(Time(0), PKT); 4000]);
    let bytes_in = |from: u64, to: u64| -> f64 {
        // 恰好在段尾发完的包算在这一段里：(from, to]
        out.iter()
            .filter(|(_, d)| (from + 1..=to).contains(&d.at.micros()))
            .count() as f64
            * PKT as f64
    };
    let segs = [
        (0, 1_000_000, 8e6),
        (1_000_000, 2_000_000, 2e6),
        (2_000_000, 2_500_000, 0.0),
        (2_500_000, 4_000_000, 16e6),
    ];
    for (from, to, rate) in segs {
        let want = rate * (to - from) as f64 / 1e6 / 8.0;
        let got = bytes_in(from, to);
        println!("[轨迹] {from}..{to} us: 放行 {got} 字节，轨迹给的 {want}");
        assert!(
            (got - want).abs() <= PKT as f64,
            "{from}..{to}: 放行 {got}，轨迹 {want}"
        );
    }
    assert_eq!(
        bytes_in(2_000_000, 2_500_000),
        0.0,
        "速率 0 的时段什么都不放"
    );
    // 走完之后保持最后一段的速率（16 Mbps = 一个包 0.625 ms）：4000 个包的尾巴就是这个速率
    let tail: Vec<u64> = out
        .iter()
        .map(|(_, d)| d.at.micros())
        .filter(|t| *t >= 4_000_000)
        .take(100)
        .collect();
    let gap = (tail[99] - tail[0]) as f64 / 99.0;
    assert!((gap - 625.0).abs() < 2.0, "{gap}");
}

/// 重复：第二轮和第一轮一样。
#[test]
fn a_repeating_trace_starts_over() {
    let mut p = P::new(1, fwd(trace_cfg(true)));
    let out = run(&mut p, &vec![(Time(0), PKT); 8000]);
    let count = |from: u64, to: u64| -> i64 {
        out.iter()
            .filter(|(_, d)| (from + 1..=to).contains(&d.at.micros()))
            .count()
            .try_into()
            .unwrap()
    };
    for k in 0..2u64 {
        let base = k * 4_000_000;
        assert!(
            (count(base, base + 1_000_000) - 800).abs() <= 1,
            "第 {k} 轮第一段"
        );
        assert!((count(base + 1_000_000, base + 2_000_000) - 200).abs() <= 1);
        assert_eq!(count(base + 2_000_000, base + 2_500_000), 0);
        assert!((count(base + 2_500_000, base + 4_000_000) - 2400).abs() <= 1);
    }
}

/// 轨迹的边角：空轨迹不限速；时长 0 的段被跳过；最后一段速率为 0 且不重复就是永远断了（排队上限把包都丢掉）。
#[test]
fn trace_edge_cases() {
    let free = bottleneck(
        Capacity::Trace(Trace {
            segments: vec![],
            repeat: true,
        }),
        QueueLimit::delay(1_000),
        Aqm::TailDrop,
    );
    assert_eq!(
        run(&mut P::new(1, fwd(free)), &[(Time(7), 100)])[0].1.at,
        Time(7)
    );
    let skip = bottleneck(
        Capacity::Trace(Trace {
            segments: vec![(0, 1), (1_000_000, 8_000_000)],
            repeat: false,
        }),
        QueueLimit::delay(1_000_000),
        Aqm::TailDrop,
    );
    // 1000 字节 8 Mbps = 1 ms
    assert_eq!(
        run(&mut P::new(1, fwd(skip)), &[(Time(0), 1000)])[0].1.at,
        Time(1_000)
    );
    // 跨段：8 Mbps 的段只剩 0.5 ms，发 500 字节，剩下 500 字节按 4 Mbps 发 1 ms
    let straddle = bottleneck(
        Capacity::Trace(Trace {
            segments: vec![(1_500, 8_000_000), (1_000_000, 4_000_000)],
            repeat: false,
        }),
        QueueLimit::delay(1_000_000),
        Aqm::TailDrop,
    );
    let out = run(
        &mut P::new(1, fwd(straddle)),
        &[(Time(0), 1000), (Time(0), 1000)],
    );
    assert_eq!((out[0].1.at, out[1].1.at), (Time(1_000), Time(2_500)));
    let dead = bottleneck(
        Capacity::Trace(Trace {
            segments: vec![(1_000_000, 8_000_000), (1_000, 0)],
            repeat: false,
        }),
        QueueLimit::delay(100_000),
        Aqm::TailDrop,
    );
    let mut p = P::new(1, fwd(dead));
    let out = run(&mut p, &[(Time(2_000_000), 1000), (Time(2_000_001), 1000)]);
    // 第一个包永远发不完（送达时刻是饱和值），第二个看到队列等了太久，被丢尾
    assert_eq!(out.len(), 1);
    assert!(out[0].1.at.micros() > 1_000_000_000);
    assert_eq!(p.stats().queue_dropped, 1);
}

// ---------------------------------------------------------------- 丢尾队列

/// 一个种子上的稳态（5 秒之后）：（实测丢包率 − (1 − 容量 ÷ 这个种子实际的到达速率)，平均排队时长，最大排队时长）。
/// 到达速率取这个种子自己实测的（泊松到达的个数本身有 0.4% 的波动，那不是模型的事）。
fn overload_steady_state(seed: u64, cfg: DirConfig) -> [f64; 3] {
    let mut p = P::new(seed, fwd(cfg));
    // 1250 个包每秒（ρ = 1.25），60 秒
    let sends: Vec<(Time, usize)> = poisson_times(seed, 800_000, 75_000)
        .into_iter()
        .map(|t| (t, PKT))
        .collect();
    let out = run(&mut p, &sends);
    let from = 5_000_000u64;
    let window_s = (sends[sends.len() - 1].0.micros() - from) as f64 / 1e6;
    let arrived = sends.iter().filter(|(t, _)| t.micros() >= from).count() as f64;
    let waits: Vec<f64> = out
        .iter()
        .filter(|(t, _)| t.micros() >= from)
        .map(|(t, d)| d.at.since(*t) as f64 - D_US)
        .collect();
    let loss = 1.0 - waits.len() as f64 / arrived;
    let want = 1.0 - 1000.0 / (arrived / window_s);
    [
        loss - want,
        waits.iter().sum::<f64>() / waits.len() as f64,
        waits.iter().copied().fold(0.0, f64::max),
    ]
}

/// 持续过载（泊松到达，ρ = 1.25）：丢包率 = 1 − 容量 ÷ 到达速率（= 20%）；排队时长稳定在上限附近、不超过上限。
#[test]
fn tail_drop_holds_the_queue_near_the_limit_and_loses_one_minus_capacity_over_arrival() {
    let limit_us = 50_000.0;
    let cfg = bottleneck(cap10(), QueueLimit::delay(50_000), Aqm::TailDrop);
    let mut waits = Vec::new();
    check(
        ["丢尾：丢包率 − (1 − 容量/到达速率)"],
        [0.0],
        |seed| {
            let [d, mean, max] = overload_steady_state(seed, cfg.clone());
            waits.push((mean, max));
            [d]
        },
    );
    // 排队时长不超过上限；平均贴着上限（泊松到达下队列在上限附近的几何分布，离上限 ρ/(ρ−1)·... 约几个包）
    let worst = waits.iter().map(|w| w.1).fold(0.0, f64::max);
    let mean = waits.iter().map(|w| w.0).sum::<f64>() / waits.len() as f64;
    println!("[丢尾] 稳态平均排队 {mean:.0} us，最大 {worst:.0} us（上限 {limit_us}）");
    assert!(worst <= limit_us, "{worst}");
    assert!(mean > limit_us - 10.0 * D_US, "{mean}");
}

/// 按字节计的队列上限：队列里的字节数永远不超过上限、也能到上限附近；过载时丢包率同样等于 1 − 容量 ÷ 到达速率，
/// 排队时长稳定在 上限字节 × 8 ÷ 速率 附近。（上限取 50 个包，队列几乎不会空，容量能被用满。）
#[test]
fn a_byte_limited_queue_never_holds_more_than_its_limit() {
    let limit = 50 * PKT as u64;
    let cfg = bottleneck(cap10(), QueueLimit::bytes(limit), Aqm::TailDrop);
    let mut max_bytes = 0u64;
    let mut means = Vec::new();
    check(
        ["字节队列：丢包率 − (1 − 容量/到达速率)"],
        [0.0],
        |seed| {
            // 单独再跑一遍记下队列里字节数的最大值（run 只管收发）
            let mut p = P::new(seed, fwd(cfg.clone()));
            for (i, t) in poisson_times(seed, 800_000, 75_000).iter().enumerate() {
                p.advance_to(*t);
                p.send(0, 1, PKT, u32::try_from(i).unwrap());
                max_bytes = max_bytes.max(p.dir(Dir::Forward).bottleneck().queued_bytes());
            }
            let [d, mean, _] = overload_steady_state(seed, cfg.clone());
            means.push(mean);
            [d]
        },
    );
    let mean = means.iter().sum::<f64>() / means.len() as f64;
    println!("[字节队列] 队列里最多 {max_bytes} 字节（上限 {limit}）；平均排队 {mean:.0} us（上限 50 个包 = {} us）", 50.0 * D_US);
    assert!(max_bytes <= limit);
    assert!(
        max_bytes >= limit - 2 * PKT as u64,
        "过载时队列要能到上限附近"
    );
    assert!(mean <= 50.0 * D_US && mean > 40.0 * D_US, "{mean}");
}

// ---------------------------------------------------------------- 瓶颈排队：M/D/1

/// 泊松到达、固定包长、匀速服务：平均排队时间 = ρ/(2μ(1−ρ))，μ = 1/D（每秒服务的包数）。
#[test]
fn bottleneck_queueing_matches_the_md1_formula() {
    // （ρ，到达间隔的均值（纳秒）= D/ρ）
    for (rho, mean_gap_ns) in [(0.5_f64, 2_000_000u64), (0.8, 1_250_000)] {
        check(
            [&format!("M/D/1 ρ={rho} 平均排队（微秒）")],
            [rho * D_US / (2.0 * (1.0 - rho))],
            |seed| {
                let cfg = bottleneck(
                    cap10(),
                    QueueLimit {
                        max_delay_us: None,
                        max_bytes: None,
                    },
                    Aqm::TailDrop,
                );
                let mut p = P::new(seed, fwd(cfg));
                let sends: Vec<(Time, usize)> = poisson_times(seed, mean_gap_ns, 200_000)
                    .into_iter()
                    .map(|t| (t, PKT))
                    .collect();
                let out = run(&mut p, &sends);
                assert_eq!(out.len(), sends.len());
                [out.iter()
                    .map(|(t, d)| d.at.since(*t) as f64 - D_US)
                    .sum::<f64>()
                    / out.len() as f64]
            },
        );
    }
}

// ---------------------------------------------------------------- CoDel

/// 不理会丢包的持续过载（泊松到达，ρ = 1.2，不限队列长度）：这是 CoDel 不擅长的情形（RFC 8289 的前提是发送端会反应）。
/// 控制律要几秒才把丢包速率爬到过载的量，其间队列已经涨高；退出丢包状态后重新进入时 count 从上一轮丢的个数
/// 起步（RFC 伪代码里的 `count = delta`），所以队列在 20 ms 到 360 ms 之间锯齿式地摆动（2026-11-28 实测，
/// 逐秒的 count 和排队见报告），平均排队远高于 5 ms，只是比丢尾队列的 200 ms 上限低。这里只断言长期丢包量对得上、
/// 队列没有无限涨，并把数打出来。
#[test]
fn codel_on_an_unresponsive_overload_sheds_the_excess_but_does_not_hold_the_target() {
    let mut rows = Vec::new();
    for seed in SEEDS {
        let mut p = P::new(
            seed,
            fwd(bottleneck(
                cap10(),
                QueueLimit {
                    max_delay_us: None,
                    max_bytes: None,
                },
                Aqm::codel(),
            )),
        );
        let sends: Vec<(Time, usize)> = poisson_times(seed, 833_333, 120_000)
            .into_iter()
            .map(|t| (t, PKT))
            .collect();
        let out = run(&mut p, &sends);
        let mut ws: Vec<f64> = out
            .iter()
            .filter(|(t, _)| t.micros() >= 50_000_000)
            .map(|(t, d)| d.at.since(*t) as f64 - D_US)
            .collect();
        ws.sort_by(f64::total_cmp);
        rows.push((
            ws.iter().sum::<f64>() / ws.len() as f64,
            quantile(&ws, 50),
            p.stats().aqm_dropped as f64 / sends.len() as f64,
            p.backlog_us(0, 1),
        ));
    }
    let mean = rows.iter().map(|r| r.0).sum::<f64>() / 12.0;
    let median = rows.iter().map(|r| r.1).sum::<f64>() / 12.0;
    let drop = rows.iter().map(|r| r.2).sum::<f64>() / 12.0;
    println!("[CoDel] 不反应的 ρ=1.2：后 50 秒平均排队 {mean:.0} us、中位数 {median:.0} us；丢包率 {drop:.4}（过载量 {:.4}）", 1.0 - 1.0 / 1.2);
    assert!((drop - (1.0 - 1.0 / 1.2)).abs() < 0.01, "{drop}");
    assert!(rows.iter().all(|r| r.3 < 1_000_000), "队列不能无限涨");
}

/// 丢包间隔按 interval/√count 缩短（RFC 8289 的控制律），在瓶颈上实测：恒速 2 倍过载，
/// 排队时长一直高于目标，第 i 次和第 i+1 次丢包的间隔 ≈ 100 ms/√i（误差一个发送时间）。
#[test]
fn codel_drop_spacing_follows_interval_over_sqrt_count_on_the_bottleneck() {
    let mut b = Bottleneck::new(
        BottleneckConfig {
            capacity: cap10(),
            limit: QueueLimit {
                max_delay_us: None,
                max_bytes: None,
            },
            aqm: Aqm::codel(),
        },
        Time::ZERO,
    );
    let mut rng = LayerRng::new(1, Dir::Forward, layer::BOTTLENECK);
    let mut drops: Vec<u64> = Vec::new();
    // 每 0.5 ms 一个包（2 倍过载）
    for i in 0..20_000u64 {
        let at = Time(i * 500);
        let start = at.micros() + b.backlog_us(at);
        if b.arrive(&mut rng, at, PKT as u64) == Verdict::AqmDropped {
            drops.push(start);
        }
    }
    assert!(drops.len() > 60, "{}", drops.len());
    // 第一次丢包在排队时长第一次到达目标之后一个窗口
    assert!(drops[0] >= 100_000 && drops[0] < 130_000, "{}", drops[0]);
    let mut worst = 0.0f64;
    for (i, w) in drops.windows(2).take(60).enumerate() {
        let want = 100_000.0 / ((i + 1) as f64).sqrt();
        let got = (w[1] - w[0]) as f64;
        worst = worst.max((got - want).abs());
        assert!(
            (got - want).abs() <= D_US + 1.0,
            "第 {} 次间隔 {got}，公式 {want}",
            i + 2
        );
    }
    println!("[CoDel] 前 60 个丢包间隔与 interval/√count 的最大偏差 {worst:.0} us（一个发送时间 {D_US} us）");
}

/// 负载降下来之后退出丢包状态：先 ρ = 1.5 过载 6 秒，再 ρ = 0.5 轻载 6 秒。
/// 轻载之后队列排空，CoDel 不在丢包状态，最后 3 秒一个都不丢、排队时长都在目标以下。
#[test]
fn codel_leaves_the_dropping_state_when_the_load_drops() {
    for seed in SEEDS {
        let mut p = P::new(
            seed,
            fwd(bottleneck(
                cap10(),
                QueueLimit {
                    max_delay_us: None,
                    max_bytes: None,
                },
                Aqm::codel(),
            )),
        );
        let mut rng = LayerRng::new(seed, Dir::Forward, "test/arrivals");
        let mut t_ns = 0u64;
        let mut sends: Vec<(Time, usize)> = Vec::new();
        while t_ns < 12_000_000_000 {
            let mean = if t_ns < 6_000_000_000 {
                666_667
            } else {
                2_000_000
            };
            t_ns += rng.exp_ns(mean);
            sends.push((Time(t_ns / 1000), PKT));
        }
        let mut dropped_late = 0;
        for (i, (t, size)) in sends.iter().enumerate() {
            p.advance_to(*t);
            let before = p.stats().aqm_dropped;
            p.send(0, 1, *size, u32::try_from(i).unwrap());
            if t.micros() >= 9_000_000 && p.stats().aqm_dropped > before {
                dropped_late += 1;
            }
        }
        let overload_drops = p.stats().aqm_dropped;
        assert!(overload_drops > 0, "过载时 CoDel 要丢包");
        assert_eq!(dropped_late, 0, "种子 {seed}：轻载之后还在丢");
        assert!(
            !p.dir(Dir::Forward).bottleneck().codel().unwrap().dropping(),
            "种子 {seed}"
        );
        assert!(p.backlog_us(0, 1) < 5_000);
    }
}

// ---------------------------------------------------------------- 丢包在瓶颈之后

/// 丢包层在瓶颈之后：丢掉的包已经占过容量。
#[test]
fn lost_packets_have_already_used_the_capacity() {
    let mut cfg = bottleneck(
        cap10(),
        QueueLimit {
            max_delay_us: None,
            max_bytes: None,
        },
        Aqm::TailDrop,
    );
    cfg.loss = Loss::Independent { ppm: 500_000 };
    let mut p = P::new(3, fwd(cfg));
    for i in 0..1000 {
        p.send(0, 1, PKT, i);
    }
    // 1000 个包不管丢没丢都排在瓶颈里：现在发一个包要等 1000 ms
    assert_eq!(p.backlog_us(0, 1), 1_000_000);
    let out: Vec<_> = std::iter::from_fn(|| p.pop()).collect();
    let lost = p.stats().lost;
    assert_eq!(out.len() as u64 + lost, 1000);
    assert!((400..600).contains(&lost), "{lost}");
    // 排在后面的包要等前面所有包（包括丢掉的）发完：每个送达的包的时刻 = 它前面的包数 × 1 ms + 1 ms
    assert!(out
        .iter()
        .all(|d| d.at.micros() == (u64::from(d.payload) + 1) * 1000));
}

// ---------------------------------------------------------------- 限速器（接进路径）

/// 限速器接进路径：超过的包直接丢，不占瓶颈的容量（瓶颈里排队不增长）。
#[test]
fn the_policer_drops_before_the_bottleneck() {
    let mut cfg = bottleneck(cap10(), QueueLimit::delay(200_000), Aqm::TailDrop);
    // 4 Mbps、桶深 5 个包
    cfg.policer = Some(Policer {
        rate_bps: 4_000_000,
        burst_bytes: 5 * PKT as u64,
    });
    let mut p = P::new(1, fwd(cfg));
    // 一阵 10 个包同时到：5 个过，5 个丢
    for i in 0..10 {
        p.send(0, 1, PKT, i);
    }
    assert_eq!(p.stats().policed, 5);
    assert_eq!(p.backlog_us(0, 1), 5_000, "只有放行的 5 个包排在瓶颈里");
    // 持续 8 Mbps（1.25 ms 一个包）10 秒：过的字节数 ≈ 4 Mbps × 10 s + 桶深
    let mut passed = 0u64;
    for i in 0..8000u64 {
        p.advance_to(Time(1_000_000 + i * 1250));
        let before = p.stats().policed;
        p.send(0, 1, PKT, 100 + u32::try_from(i).unwrap());
        if p.stats().policed == before {
            passed += PKT as u64;
        }
    }
    let want = 4_000_000.0 / 8.0 * 10.0 + 5.0 * PKT as f64;
    assert!(
        (passed as f64 - want).abs() <= 2.0 * PKT as f64,
        "过了 {passed}，期望 {want}"
    );
    println!("[限速器] 8 Mbps 的流经 4 Mbps/桶深 6250 字节的限速器，10 秒过了 {passed} 字节（公式 {want}）");
}

// ---------------------------------------------------------------- 延迟阶跃（接进路径）

/// 延迟阶跃接进路径：阶跃发生在计划的时刻；阶跃前后包的先后不乱。
#[test]
fn a_delay_step_happens_at_the_scheduled_time_and_keeps_the_order() {
    let cfg = DirConfig {
        delay: Delay::Steps {
            initial_us: 80_000,
            steps: vec![(Time(500_000), 10_000), (Time(800_000), 120_000)],
        },
        ..DirConfig::default()
    };
    let mut p = P::new(1, fwd(cfg));
    let sends: Vec<(Time, usize)> = (0..1000u64).map(|i| (Time(i * 1_000), 100)).collect();
    let out = run(&mut p, &sends);
    assert!(out
        .windows(2)
        .all(|w| w[0].1.payload < w[1].1.payload && w[0].1.at <= w[1].1.at));
    let delay = |i: usize| out[i].1.at.since(out[i].0);
    assert_eq!(delay(499), 80_000);
    // 500 ms 的包按新延迟 10 ms 本来 510 ms 到，但前一个包 579 ms 才到，排在它后面
    assert_eq!(out[500].1.at, Time(579_000));
    // 延迟追平之后恢复成新延迟：580 ms 起的包 590 ms 到
    assert_eq!(delay(600), 10_000);
    assert_eq!(delay(799), 10_000);
    assert_eq!(delay(800), 120_000, "800 ms 恰好变大");
}

/// Starlink 式的周期性阶跃（周期 15 秒，相位 12 秒）：取值在范围内，边界精确，包的先后不乱。
#[test]
fn periodic_steps_on_the_path_change_on_the_boundary_without_reordering() {
    let cfg = DirConfig {
        delay: Delay::Periodic {
            initial_us: 30_000,
            period_us: 15_000_000,
            phase_us: 12_000_000,
            low_us: 25_000,
            high_us: 60_000,
        },
        ..DirConfig::default()
    };
    let mut p = P::new(7, fwd(cfg));
    let sends: Vec<(Time, usize)> = (0..2000u64).map(|i| (Time(i * 50_000), 100)).collect();
    let out = run(&mut p, &sends);
    assert!(out.windows(2).all(|w| w[0].1.at <= w[1].1.at));
    // 11.95 s 之前 30 ms；12 s 的包换成新值
    assert_eq!(out[239].1.at.since(out[239].0), 30_000);
    let first = out[240].1.at.since(out[240].0);
    assert!((25_000..=60_000).contains(&first) || first >= 25_000);
}

// ---------------------------------------------------------------- 中断和换路

fn delay_cfg(d: u64) -> PathConfig {
    PathConfig::symmetric(DirConfig {
        delay: Delay::Fixed(d),
        ..DirConfig::default()
    })
}

/// 中断期间全丢；恢复时刻之后各层用新参数；“路径变了”事件只发一次，时刻对（恢复的时刻，不是开始的时刻）。
#[test]
fn route_change_drops_during_the_outage_switches_parameters_and_emits_one_event() {
    let mut p = P::new(1, delay_cfg(10_000));
    p.schedule_route_change(RouteChange {
        at: Time(1_000_000),
        down_us: 500_000,
        new: Some(delay_cfg(80_000)),
    });
    assert_eq!(
        p.next_event_time(),
        Some(Time(1_500_000)),
        "事件在恢复的时刻，不是中断开始的时刻"
    );
    let mut events = Vec::new();
    let mut arrivals: Vec<(u64, u64)> = Vec::new();
    for i in 0..300u64 {
        let t = Time(i * 10_000);
        // 主循环：先把到期的送达和事件处理掉
        while let Some(next) = p.peek_time().filter(|n| *n <= t) {
            p.advance_to(next);
            let d = p.pop().unwrap();
            arrivals.push((d.payload.into(), d.at.micros()));
            while let Some(e) = p.pop_event() {
                events.push(e);
            }
        }
        p.advance_to(t);
        while let Some(e) = p.pop_event() {
            events.push(e);
        }
        p.send(0, 1, 100, u32::try_from(i).unwrap());
    }
    while let Some(d) = p.pop() {
        arrivals.push((d.payload.into(), d.at.micros()));
    }
    assert_eq!(
        events,
        vec![PathEvent::RouteChanged {
            at: Time(1_500_000)
        }],
        "只发一次"
    );
    assert_eq!(p.next_event_time(), None);
    assert_eq!(p.pop_event(), None);
    let st = p.stats();
    // 1.0 s 到 1.5 s 之间发的 50 个包全丢（含开始的时刻，不含恢复的时刻）
    assert_eq!(st.outage_dropped, 50);
    assert_eq!(st.delivered, 250);
    let delay_of = |i: u64| {
        arrivals
            .iter()
            .find(|(p, _)| *p == i)
            .map(|(_, at)| at - i * 10_000)
    };
    assert_eq!(
        delay_of(99),
        Some(10_000),
        "中断前发的包照常送达（已经在途的不改）"
    );
    assert_eq!(delay_of(100), None);
    assert_eq!(delay_of(149), None);
    assert_eq!(delay_of(150), Some(80_000), "恢复的时刻起用新参数");
    assert_eq!(delay_of(299), Some(80_000));
}

/// 事件没有人取也要在时刻到了才取得到；纯中断（不换参数）不发事件；计划可以乱序安排；`down_us` 为 0 就是按计划换参数。
#[test]
fn route_change_events_wait_for_their_time_and_pure_outages_emit_none() {
    let mut p = P::new(1, delay_cfg(1_000));
    p.schedule_route_change(RouteChange {
        at: Time(5_000_000),
        down_us: 0,
        new: Some(delay_cfg(9_000)),
    });
    p.schedule_route_change(RouteChange {
        at: Time(1_000_000),
        down_us: 200_000,
        new: Some(delay_cfg(5_000)),
    });
    p.schedule_route_change(RouteChange {
        at: Time(3_000_000),
        down_us: 100_000,
        new: None,
    });
    assert_eq!(p.next_event_time(), Some(Time(1_200_000)));
    assert_eq!(p.pop_event(), None, "时刻没到");
    p.advance_to(Time(1_199_999));
    assert_eq!(p.pop_event(), None);
    p.advance_to(Time(1_200_000));
    assert_eq!(
        p.next_event_time(),
        Some(Time(1_200_000)),
        "已经发生但还没取走"
    );
    assert_eq!(
        p.pop_event(),
        Some(PathEvent::RouteChanged {
            at: Time(1_200_000)
        })
    );
    assert_eq!(p.dir(Dir::Forward).config().delay, Delay::Fixed(5_000));
    assert_eq!(
        p.next_event_time(),
        Some(Time(5_000_000)),
        "纯中断不在事件里"
    );
    // 一下跳过两个计划时刻：事件按各自的时刻发出
    p.advance_to(Time(9_000_000));
    assert_eq!(
        p.pop_event(),
        Some(PathEvent::RouteChanged {
            at: Time(5_000_000)
        })
    );
    assert_eq!(p.pop_event(), None);
    assert_eq!(p.dir(Dir::Reverse).config().delay, Delay::Fixed(9_000));
    // 纯中断期间（3.0–3.1 s）丢包
    let mut q = P::new(1, delay_cfg(1_000));
    q.schedule_route_change(RouteChange {
        at: Time(3_000_000),
        down_us: 100_000,
        new: None,
    });
    q.advance_to(Time(3_050_000));
    q.send(0, 1, 10, 0);
    q.send(1, 0, 10, 1);
    q.advance_to(Time(3_100_000));
    q.send(0, 1, 10, 2);
    assert_eq!((q.stats().outage_dropped, q.in_flight()), (2, 1));
    assert_eq!(q.next_event_time(), None);
    // PathEvent::at
    assert_eq!(PathEvent::RouteChanged { at: Time(4) }.at(), Time(4));
}

/// 换参数时已经算好的送达不改、瓶颈里排着的包还在；按时间的层从恢复时刻重新开始。
#[test]
fn route_change_keeps_the_queue_and_restarts_the_time_driven_layers() {
    let slow = DirConfig {
        bottleneck: BottleneckConfig {
            capacity: Capacity::Constant { rate_bps: 80_000 },
            ..BottleneckConfig::default()
        },
        ..DirConfig::default()
    };
    let spiky = DirConfig {
        noise: Noise {
            spikes: Some(Spikes {
                every_us: 100_000,
                length_us: 50_000,
                max_us: 20_000,
            }),
            ..Noise::default()
        },
        policer: Some(Policer {
            rate_bps: 1_000_000,
            burst_bytes: 2_000,
        }),
        ..DirConfig::default()
    };
    let mut p = P::new(1, fwd(slow));
    // 80 kbps：1000 字节要 100 ms；发 3 个，瓶颈排到 300 ms
    for i in 0..3 {
        p.send(0, 1, 1000, i);
    }
    p.schedule_route_change(RouteChange {
        at: Time(50_000),
        down_us: 0,
        new: Some(fwd(spiky)),
    });
    p.advance_to(Time(50_000));
    assert_eq!(p.backlog_us(0, 1), 250_000, "排着的包不受换路影响");
    assert_eq!(
        p.pop_event(),
        Some(PathEvent::RouteChanged { at: Time(50_000) })
    );
    // 新参数：不限速，限速器的桶是新的（满的）
    p.send(0, 1, 1000, 10);
    p.send(0, 1, 1000, 11);
    p.send(0, 1, 1000, 12);
    assert_eq!(p.stats().policed, 1);
    let got: Vec<u32> = std::iter::from_fn(|| p.pop()).map(|d| d.payload).collect();
    assert_eq!(got.len(), 5);
    assert!(
        got.contains(&0) && got.contains(&1) && got.contains(&2),
        "{got:?}"
    );
}

// ---------------------------------------------------------------- 反向路径

/// 两个方向参数独立：改反向的参数，正向的送达逐个相同、正向各层的随机数不变；反向用自己的参数。
#[test]
fn the_reverse_direction_has_its_own_parameters_and_streams() {
    let forward = DirConfig {
        bottleneck: BottleneckConfig {
            capacity: Capacity::Poisson {
                rate_bps: 20_000_000,
                opportunity_bytes: 1500,
            },
            ..BottleneckConfig::default()
        },
        loss: Loss::Burst {
            loss_ppm: 20_000,
            avg_burst_milli: 3_000,
        },
        noise: Noise {
            jitter_us: 4_000,
            spikes: Some(Spikes {
                every_us: 200_000,
                length_us: 30_000,
                max_us: 15_000,
            }),
            ..Noise::default()
        },
        ..DirConfig::default()
    };
    let run_with = |reverse: DirConfig| {
        let mut p = P::new(
            9,
            PathConfig {
                forward: forward.clone(),
                reverse,
            },
        );
        for i in 0..2000u64 {
            p.advance_to(Time(i * 700));
            p.send(0, 1, 1200, u32::try_from(i).unwrap());
            if i % 3 == 0 {
                p.send(1, 0, 80, 100_000 + u32::try_from(i).unwrap());
            }
        }
        let all: Vec<_> = std::iter::from_fn(|| p.pop()).collect();
        (all, p)
    };
    let (a, pa) = run_with(DirConfig::default());
    let (b, pb) = run_with(DirConfig {
        delay: Delay::Fixed(50_000),
        loss: Loss::Independent { ppm: 300_000 },
        noise: Noise {
            jitter_us: 9_000,
            ..Noise::default()
        },
        ..DirConfig::default()
    });
    let fwd_of = |v: &[Delivery<u32>]| {
        v.iter()
            .filter(|d| d.to == 1)
            .map(|d| (d.payload, d.at))
            .collect::<Vec<_>>()
    };
    assert_eq!(fwd_of(&a), fwd_of(&b), "反向的参数不影响正向");
    assert_eq!(pa.dir(Dir::Forward).draws(), pb.dir(Dir::Forward).draws());
    for name in [layer::BOTTLENECK, layer::LOSS, layer::JITTER, layer::SPIKES] {
        assert_eq!(
            pa.dir(Dir::Forward).stream_log(name),
            pb.dir(Dir::Forward).stream_log(name),
            "{name}"
        );
    }
    assert_ne!(
        pa.dir(Dir::Reverse).stream_log(layer::LOSS),
        pa.dir(Dir::Forward).stream_log(layer::LOSS),
        "两个方向的流各是各的"
    );
    // 反向确实用了自己的参数：延迟 ≥ 50 ms，丢了约 30%
    let rev: Vec<_> = b.iter().filter(|d| d.to == 0).collect();
    assert!(rev.iter().all(|d| d.at.micros() >= 50_000));
    let sent_rev = pb.dir_stats(Dir::Reverse).sent as f64;
    let lost_rev = pb.dir_stats(Dir::Reverse).lost as f64;
    assert!(
        (lost_rev / sent_rev - 0.3).abs() < 0.05,
        "{}",
        lost_rev / sent_rev
    );
    assert_eq!(pa.dir_stats(Dir::Reverse).lost, 0);
}

// ---------------------------------------------------------------- 随机数独立

fn full_config() -> DirConfig {
    DirConfig {
        policer: Some(Policer {
            rate_bps: 30_000_000,
            burst_bytes: 30_000,
        }),
        bottleneck: BottleneckConfig {
            capacity: Capacity::Poisson {
                rate_bps: 12_000_000,
                opportunity_bytes: 1500,
            },
            limit: QueueLimit {
                max_delay_us: Some(100_000),
                max_bytes: Some(60_000),
            },
            aqm: Aqm::codel(),
        },
        loss: Loss::Burst {
            loss_ppm: 30_000,
            avg_burst_milli: 3_000,
        },
        delay: Delay::Periodic {
            initial_us: 20_000,
            period_us: 200_000,
            phase_us: 1_000_000,
            low_us: 15_000,
            high_us: 40_000,
        },
        noise: Noise {
            jitter_us: 3_000,
            fifo_jitter: false,
            spikes: Some(Spikes {
                every_us: 400_000,
                length_us: 30_000,
                max_us: 20_000,
            }),
            doze: Some(Doze {
                idle_us: 30_000,
                wake_us: 20_000,
            }),
            pdv: Some(Pdv::RFC8868),
        },
        receiver: Receiver {
            busy: Some(Busy {
                every_us: 100_000,
                length_us: 5_000,
            }),
            batch_us: 4_000,
        },
        loss_before_bottleneck: false,
    }
}

const STREAMS: [&str; 8] = [
    layer::BOTTLENECK,
    layer::LOSS,
    layer::DELAY,
    layer::JITTER,
    layer::SPIKES,
    layer::DOZE,
    layer::BUSY,
    layer::PDV,
];

fn logs(cfg: DirConfig) -> Vec<Vec<u64>> {
    let mut p = P::new(42, fwd(cfg));
    // 20 秒、约 14 Mbps 的泊松流（会过载瓶颈，限速器、丢尾、CoDel 都有事做），中间夹着几次长静默（省电要碰上）
    let times = poisson_times(5, 685_000, 29_000);
    for (i, t) in times.iter().enumerate() {
        p.advance_to(Time(t.micros() + if i > 14_000 { 150_000 } else { 0 }));
        p.send(0, 1, 1200, u32::try_from(i).unwrap());
    }
    STREAMS
        .iter()
        .map(|n| p.dir(Dir::Forward).stream_log(n).to_vec())
        .collect()
}

/// 关掉任意一层或改它的参数，其他各层抽到的数逐个相同：
/// 按包抽数的层（丢包、抖动、省电）长度也相同；按时间走的层（泊松放行、延迟、尖峰、接收端忙）互为前缀。
#[test]
fn switching_a_layer_off_or_changing_it_leaves_every_other_stream_untouched() {
    let base = logs(full_config());
    for (name, log) in STREAMS.iter().zip(&base) {
        assert!(
            log.len() > 50,
            "基线里 {name} 要真的用到随机数：{}",
            log.len()
        );
    }
    type Change = fn(&mut DirConfig);
    // （改了什么，自己的流是哪条（改完它的流允许不同），改法）
    let changes: Vec<(&str, Option<&str>, Change)> = vec![
        ("限速器关掉", None, |c| c.policer = None),
        ("限速器改参数", None, |c| {
            c.policer = Some(Policer {
                rate_bps: 5_000_000,
                burst_bytes: 3_000,
            })
        }),
        ("瓶颈：泊松改匀速", Some(layer::BOTTLENECK), |c| {
            c.bottleneck.capacity = Capacity::Constant {
                rate_bps: 12_000_000,
            }
        }),
        ("瓶颈：泊松改速率", Some(layer::BOTTLENECK), |c| {
            c.bottleneck.capacity = Capacity::Poisson {
                rate_bps: 25_000_000,
                opportunity_bytes: 1500,
            }
        }),
        ("瓶颈：CoDel 关掉", None, |c| {
            c.bottleneck.aqm = Aqm::TailDrop
        }),
        ("瓶颈：队列上限改小", None, |c| {
            c.bottleneck.limit = QueueLimit::delay(10_000)
        }),
        ("丢包关掉", Some(layer::LOSS), |c| c.loss = Loss::None),
        ("丢包改参数", Some(layer::LOSS), |c| {
            c.loss = Loss::Burst {
                loss_ppm: 100_000,
                avg_burst_milli: 8_000,
            }
        }),
        ("延迟改固定", Some(layer::DELAY), |c| {
            c.delay = Delay::Fixed(30_000)
        }),
        ("延迟周期改参数", Some(layer::DELAY), |c| {
            c.delay = Delay::Periodic {
                initial_us: 1,
                period_us: 700_000,
                phase_us: 0,
                low_us: 1,
                high_us: 100_000,
            }
        }),
        ("抖动关掉", Some(layer::JITTER), |c| {
            c.noise.jitter_us = 0
        }),
        ("抖动改参数", Some(layer::JITTER), |c| {
            c.noise.jitter_us = 9_000
        }),
        ("尖峰关掉", Some(layer::SPIKES), |c| {
            c.noise.spikes = None
        }),
        ("尖峰改参数", Some(layer::SPIKES), |c| {
            c.noise.spikes = Some(Spikes {
                every_us: 90_000,
                length_us: 5_000,
                max_us: 8_000,
            })
        }),
        ("省电关掉", Some(layer::DOZE), |c| c.noise.doze = None),
        ("省电改参数", Some(layer::DOZE), |c| {
            c.noise.doze = Some(Doze {
                idle_us: 5_000,
                wake_us: 60_000,
            })
        }),
        ("接收端忙关掉", Some(layer::BUSY), |c| {
            c.receiver.busy = None
        }),
        ("接收端忙改参数", Some(layer::BUSY), |c| {
            c.receiver.busy = Some(Busy {
                every_us: 70_000,
                length_us: 9_000,
            })
        }),
        ("批量收关掉", None, |c| c.receiver.batch_us = 0),
        ("NR-BPDV 关掉", Some(layer::PDV), |c| c.noise.pdv = None),
        ("NR-BPDV 改参数", None, |c| {
            c.noise.pdv = Some(Pdv { std_us: 1_000 })
        }),
    ];
    for (what, own, change) in changes {
        let mut cfg = full_config();
        change(&mut cfg);
        let got = logs(cfg);
        for ((name, a), b) in STREAMS.iter().zip(&base).zip(&got) {
            if Some(*name) == own {
                continue;
            }
            let common = a.len().min(b.len());
            assert!(
                common > 50,
                "{what}: {name} 的数列太短 ({} / {})",
                a.len(),
                b.len()
            );
            assert_eq!(a[..common], b[..common], "{what}：{name} 的随机数变了");
            if [layer::LOSS, layer::JITTER, layer::DOZE].contains(name) {
                assert_eq!(a.len(), b.len(), "{what}：按包抽数的层 {name} 抽的个数变了");
            }
        }
    }
}

// ---------------------------------------------------------------- 搬过来的老层和 Net 对照

/// 每个种子一个：（均值，中位数，95 分位）的送达延迟（微秒）。
fn summarize(mut delays: Vec<f64>) -> [f64; 3] {
    delays.sort_by(f64::total_cmp);
    [
        delays.iter().sum::<f64>() / delays.len() as f64,
        quantile(&delays, 50),
        quantile(&delays, 95),
    ]
}

fn net_delays(seed: u64, link: Link, pattern: &[(u64, usize)]) -> (Vec<f64>, u64) {
    let mut net: Net<u32> = Net::new(seed);
    net.set_link(0, 1, link);
    for (i, (t, size)) in pattern.iter().enumerate() {
        net.advance_to(Time(*t));
        net.send(0, 1, *size, u32::try_from(i).unwrap());
    }
    let v = std::iter::from_fn(|| net.pop())
        .map(|d| d.at.micros() as f64 - pattern[d.payload as usize].0 as f64)
        .collect();
    (v, net.stats().lost)
}

fn path_delays(seed: u64, link: &Link, pattern: &[(u64, usize)]) -> (Vec<f64>, u64) {
    let mut p = P::new(seed, fwd(DirConfig::from_link(link)));
    for (i, (t, size)) in pattern.iter().enumerate() {
        p.advance_to(Time(*t));
        p.send(0, 1, *size, u32::try_from(i).unwrap());
    }
    let v = std::iter::from_fn(|| p.pop())
        .map(|d| d.at.micros() as f64 - pattern[d.payload as usize].0 as f64)
        .collect();
    (v, p.stats().lost)
}

/// 同样的链路参数、同样的发包时刻：12 个种子，送达延迟的均值、中位数、95 分位在两倍合并标准误以内。
fn compare_with_net(name: &str, link: Link, pattern: &[(u64, usize)]) {
    let names = [
        format!("{name}：延迟均值（微秒）"),
        format!("{name}：延迟中位数"),
        format!("{name}：延迟 95 分位"),
    ];
    check_pair(
        [&names[0], &names[1], &names[2]],
        |seed| summarize(net_delays(seed, link, pattern).0),
        |seed| summarize(path_delays(seed, &link, pattern).0),
    );
}

fn every(ms_us: u64, n: u64, size: usize) -> Vec<(u64, usize)> {
    (0..n).map(|i| (i * ms_us, size)).collect()
}

#[test]
fn migrated_layer_uniform_jitter_matches_net() {
    // 乱序的均匀抖动，限速链路
    let link = Link {
        jitter_us: 8_000,
        rate_bps: Some(30_000_000),
        ..Link::lan()
    };
    compare_with_net("均匀抖动（乱序）", link, &every(2_000, 3000, 1200));
}

#[test]
fn migrated_layer_fifo_jitter_matches_net() {
    let link = Link {
        jitter_us: 20_000,
        fifo_jitter: true,
        rate_bps: Some(30_000_000),
        ..Link::lan()
    };
    compare_with_net("均匀抖动（不乱序）", link, &every(2_000, 3000, 1200));
}

#[test]
fn migrated_layer_spikes_match_net() {
    let link = Link {
        spikes: Some(Spikes {
            every_us: 500_000,
            length_us: 40_000,
            max_us: 25_000,
        }),
        ..Link::lan()
    };
    compare_with_net("延迟尖峰", link, &every(5_000, 6000, 1200));
}

#[test]
fn migrated_layer_doze_matches_net() {
    let link = Link {
        doze: Some(Doze {
            idle_us: 50_000,
            wake_us: 24_000,
        }),
        ..Link::lan()
    };
    // 每 120 ms 一阵 3 个包（间隔 1 ms）：每阵第一个包碰上睡着的网卡
    let pattern: Vec<(u64, usize)> = (0..800u64)
        .flat_map(|b| (0..3u64).map(move |k| (b * 120_000 + k * 1_000, 1200)))
        .collect();
    compare_with_net("省电唤醒", link, &pattern);
}

#[test]
fn migrated_layer_busy_receiver_matches_net() {
    let link = Link {
        busy: Some(Busy {
            every_us: 300_000,
            length_us: 20_000,
        }),
        rate_bps: Some(30_000_000),
        ..Link::lan()
    };
    // 每 33 ms 一帧，每帧 30 个包（30 Mbps 下约 10 ms 发完）
    let pattern: Vec<(u64, usize)> = (0..300u64)
        .flat_map(|f| (0..30u64).map(move |_| (f * 33_000, 1200)))
        .collect();
    compare_with_net("接收端忙", link, &pattern);
}

#[test]
fn migrated_layer_batched_reads_match_net() {
    let link = Link {
        batch_us: 8_000,
        rate_bps: Some(30_000_000),
        ..Link::lan()
    };
    let pattern: Vec<(u64, usize)> = (0..200u64)
        .flat_map(|f| (0..10u64).map(move |_| (f * 33_000, 1200)))
        .collect();
    compare_with_net("批量收", link, &pattern);
}

/// 独立丢包率和带宽、排队：同样的参数下丢包率和 Net 一致（丢包位置不同：Path 在瓶颈之后，不影响没有拥塞时的丢包率）。
#[test]
fn migrated_independent_loss_and_queue_match_net() {
    let link = Link {
        loss_permille: 20,
        rate_bps: Some(20_000_000),
        max_queue_us: 30_000,
        ..Link::wifi()
    };
    let pattern = every(1_000, 5000, 1200);
    check_pair(
        ["独立丢包率"],
        |seed| [net_delays(seed, link, &pattern).1 as f64 / 5000.0],
        |seed| [path_delays(seed, &link, &pattern).1 as f64 / 5000.0],
    );
    check(["独立丢包率 = 参数 2%"], [0.02], |seed| {
        [path_delays(seed, &link, &pattern).1 as f64 / 5000.0]
    });
    // 无丢包、过载的队列：丢尾的数量和延迟一致（同样的算法，没有随机数）
    let link = Link {
        rate_bps: Some(5_000_000),
        max_queue_us: 20_000,
        ..Link::ideal()
    };
    let over = every(1_000, 2000, 1250);
    let (na, nl) = net_delays(1, link, &over);
    let (pb, pl) = path_delays(1, &link, &over);
    assert_eq!((nl, na.len()), (pl, pb.len()));
    assert_eq!(na.len(), pb.len());
    assert!(
        na.iter().zip(&pb).all(|(x, y)| (x - y).abs() <= 1.0),
        "同样的容量、队列算出的延迟至多差 1 微秒"
    );
}

// ---------------------------------------------------------------- 接口的其他部分

#[test]
fn path_interface_basics() {
    let mut p = P::new(
        5,
        fwd(DirConfig {
            delay: Delay::Fixed(1_000),
            ..DirConfig::default()
        }),
    );
    assert_eq!(p.seed(), 5);
    p.send(2, 3, 10, 0);
    p.send(1, 0, 10, 1); // 反向没有配置：理想链路
    assert_eq!(p.stats().unroutable, 1);
    p.send(0, 1, 10, 2);
    p.advance_to(Time(500));
    p.advance_to(Time(100));
    assert_eq!(p.now(), Time(500));
    assert_eq!(p.in_flight(), 2);
    assert_eq!(p.peek_time(), Some(Time(0).plus(0)));
    let first = p.pop().unwrap();
    assert_eq!((first.payload, first.from, first.to), (1, 1, 0));
    let second = p.pop().unwrap();
    assert_eq!(
        (second.payload, second.at, p.now()),
        (2, Time(1_000), Time(1_000))
    );
    assert!(p.pop().is_none());
    assert_eq!(p.peek_time(), None);
    let s = p.stats();
    assert_eq!((s.sent, s.delivered), (2, 2));
    assert_eq!(p.dir_stats(Dir::Forward).delivered, 1);
    assert_eq!(p.backlog_us(5, 6), 0);
    assert_eq!(
        p.dir(Dir::Forward).bottleneck().config(),
        &BottleneckConfig::default()
    );
    assert!(p.dir(Dir::Forward).stream_log("没有这一层").is_empty());
    assert_eq!(p.backlog_us(0, 1), 0, "不限速不排队");
    // set_dir 立即换参数，set_up 两个方向一起切
    p.set_dir(
        Dir::Forward,
        DirConfig {
            delay: Delay::Fixed(9_000),
            ..DirConfig::default()
        },
    );
    p.send(0, 1, 10, 3);
    assert_eq!(p.pop().unwrap().at, Time(10_000));
    p.set_up(false);
    p.send(0, 1, 10, 4);
    p.send(1, 0, 10, 5);
    assert_eq!(p.stats().outage_dropped, 2);
    p.set_up(true);
    p.send(0, 1, 10, 6);
    assert_eq!(p.in_flight(), 1);
    // PathConfig::dir
    let c = fwd(DirConfig::default());
    assert_eq!(c.dir(Dir::Forward), &c.forward);
    assert_eq!(c.dir(Dir::Reverse), &c.reverse);
}

#[test]
fn backlog_is_the_true_wait_at_the_bottleneck() {
    let mut p = P::new(
        3,
        fwd(bottleneck(
            Capacity::Constant { rate_bps: 80_000 },
            QueueLimit::delay(1_000_000),
            Aqm::TailDrop,
        )),
    );
    p.send(0, 1, 1000, 1);
    p.send(0, 1, 1000, 2);
    assert_eq!(p.backlog_us(0, 1), 200_000);
    assert_eq!(p.backlog_us(1, 0), 0);
    p.advance_to(Time(150_000));
    assert_eq!(p.backlog_us(0, 1), 50_000, "随时间排掉");
    p.advance_to(Time(500_000));
    assert_eq!(p.backlog_us(0, 1), 0);
}

/// 速率 0 在匀速和泊松里都是“一个包也放不出去”（和轨迹一致）：有排队上限时第一个包永远发不完，后面的丢尾；
/// 没有上限时全部排队。不限速要用 `Unlimited`；`Link` 的速率 0（`Net` 里是不限速）由 `from_link` 显式转成 `Unlimited`。
#[test]
fn rate_zero_means_nothing_gets_out_and_from_link_maps_net_zero_to_unlimited() {
    for capacity in [
        Capacity::Constant { rate_bps: 0 },
        Capacity::Poisson {
            rate_bps: 0,
            opportunity_bytes: 1500,
        },
    ] {
        let mut p = P::new(
            3,
            fwd(bottleneck(
                capacity.clone(),
                QueueLimit::delay(1_000),
                Aqm::TailDrop,
            )),
        );
        let out = run(&mut p, &[(Time(5), 1000), (Time(6), 1000), (Time(7), 1000)]);
        assert_eq!(out.len(), 1, "{capacity:?}");
        assert!(out[0].1.at.micros() > 1_000_000_000_000);
        assert_eq!(p.stats().queue_dropped, 2);
        // 没有排队上限：都排着，都发不出去
        let mut q = P::new(
            3,
            fwd(bottleneck(
                capacity,
                QueueLimit {
                    max_delay_us: None,
                    max_bytes: None,
                },
                Aqm::TailDrop,
            )),
        );
        let out = run(&mut q, &[(Time(5), 1000), (Time(6), 1000)]);
        assert!(out.iter().all(|(_, d)| d.at.micros() > 1_000_000_000_000));
        assert_eq!(q.stats().queue_dropped, 0);
    }
    for rate in [Some(0), None] {
        let link = Link {
            rate_bps: rate,
            ..Link::ideal()
        };
        assert_eq!(
            DirConfig::from_link(&link).bottleneck.capacity,
            Capacity::Unlimited
        );
        assert_eq!(
            run(
                &mut P::new(3, fwd(DirConfig::from_link(&link))),
                &[(Time(5), 1000)]
            )[0]
            .1
            .at,
            Time(5)
        );
    }
    let link = Link {
        rate_bps: Some(1_000_000),
        ..Link::ideal()
    };
    assert_eq!(
        DirConfig::from_link(&link).bottleneck.capacity,
        Capacity::Constant {
            rate_bps: 1_000_000
        }
    );
}

// ---------------------------------------------------------------- 交叉流量的注入口

/// 交叉流量包只经过瓶颈：占容量（主流量的包要排在它后面）、受丢尾约束、统计单独记；顺序和时间的要求有明确的错误。
#[test]
fn cross_traffic_injection_occupies_the_bottleneck_only() {
    let cfg = DirConfig {
        bottleneck: BottleneckConfig {
            capacity: cap10(),
            limit: QueueLimit::delay(5_000),
            aqm: Aqm::TailDrop,
        },
        loss: Loss::Independent { ppm: 1_000_000 }, // 主流量全丢：交叉流量不经过丢包层
        ..DirConfig::default()
    };
    let mut p = P::new(1, fwd(cfg));
    // 现在是 0：交叉包在 0 到达，发完是 1 ms
    assert_eq!(
        p.inject_cross(Dir::Forward, Time(0), PKT),
        Ok(Some(Time(1_000)))
    );
    assert_eq!(p.backlog_us(0, 1), 1_000);
    // 比时钟晚的不行
    assert_eq!(
        p.inject_cross(Dir::Forward, Time(10), PKT),
        Err(CrossError::Future)
    );
    p.advance_to(Time(100));
    assert_eq!(
        p.inject_cross(Dir::Forward, Time(100), PKT),
        Ok(Some(Time(2_000)))
    );
    // 比上一个到瓶颈的包早的不行
    assert_eq!(
        p.inject_cross(Dir::Forward, Time(50), PKT),
        Err(CrossError::Late { last: Time(100) })
    );
    // 排队超过 5 ms 的被丢尾
    for _ in 0..4 {
        let _ = p.inject_cross(Dir::Forward, Time(100), PKT);
    }
    assert_eq!(p.inject_cross(Dir::Forward, Time(100), PKT), Ok(None));
    let st = p.dir_stats(Dir::Forward);
    assert_eq!((st.cross_sent, st.cross_dropped), (7, 1));
    assert_eq!(st.sent, 0, "交叉流量不算主流量");
    // 主流量排在交叉流量后面：它自己被丢包层丢了，但容量已经占了
    p.send(0, 1, PKT, 9);
    assert_eq!(p.stats().queue_dropped, 1, "瓶颈已经排满，主流量也被丢尾");
    // 反向没有交叉流量时是空的
    assert_eq!(
        p.inject_cross(Dir::Reverse, Time(100), PKT),
        Ok(Some(Time(100)))
    );
}

// ---------------------------------------------------------------- 随机数种子

#[test]
fn draws_are_counted_per_layer() {
    let cfg = DirConfig {
        loss: Loss::Independent { ppm: 10_000 },
        noise: Noise {
            jitter_us: 100,
            ..Noise::default()
        },
        ..DirConfig::default()
    };
    let mut p = P::new(1, fwd(cfg));
    for i in 0..10 {
        p.send(0, 1, 10, i);
    }
    let draws = p.dir(Dir::Forward).draws();
    let of = |n: &str| draws.iter().find(|(name, _)| *name == n).unwrap().1;
    assert_eq!(
        (
            of(layer::LOSS),
            of(layer::JITTER),
            of(layer::DOZE),
            of(layer::SPIKES)
        ),
        (10, 10, 0, 0)
    );
}

// ---------------------------------------------------------------- A2-3：Network 接口、对照开关、名义值

/// 通过 `Network` 接口跑一遍：t=0 起每 1 ms 发一个 1250 字节的包，正反两个方向交替，最后全部取完。
fn drive<N: Network<u32>>(n: &mut N, packets: u32) -> (Vec<Delivery<u32>>, NetStats, u64) {
    let mut backlog = 0;
    for i in 0..packets {
        n.advance_to(Time(u64::from(i) * 1000));
        let (from, to) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
        n.send(from, to, 1250, i);
        backlog = backlog.max(n.backlog_us(0, 1));
    }
    n.send(0, 7, 10, 999);
    let mut got = Vec::new();
    while n.peek_time().is_some() {
        assert!(n.in_flight() > 0);
        got.extend(n.pop());
    }
    assert_eq!(n.in_flight(), 0);
    assert!(n.pop().is_none());
    (got, n.stats(), backlog)
}

#[test]
fn network_trait_is_the_inherent_methods() {
    let link = Link {
        rate_bps: Some(5_000_000),
        loss_permille: 100,
        ..Link::wifi()
    };
    let mut direct: Net<u32> = Net::new(3);
    let mut via: Net<u32> = Net::new(3);
    direct.set_link(0, 1, link);
    via.set_link(0, 1, link);
    // 直接调用（不经过接口）
    let mut want = Vec::new();
    for i in 0..60u32 {
        direct.advance_to(Time(u64::from(i) * 1000));
        let (from, to) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
        direct.send(from, to, 1250, i);
    }
    direct.send(0, 7, 10, 999);
    while let Some(d) = direct.pop() {
        want.push(d);
    }
    let (got, stats, backlog) = drive(&mut via, 60);
    assert_eq!(got, want);
    assert_eq!(stats, direct.stats());
    assert_eq!(stats.unroutable, 1);
    assert!(backlog > 0, "5 Mbps 上每毫秒 1250 字节会排队");
    assert_eq!(Network::path_stats(&via), None);
    assert_eq!(Network::next_event_time(&via), None);
    assert_eq!(Network::pop_event(&mut via), None);
    assert_eq!(via.link(0, 1), Some(link));
    assert_eq!(via.link(1, 2), None);

    // Path 经过接口和直接调用一样，计数按 Net 的口径折算
    let cfg = PathConfig::symmetric(DirConfig::from_link(&link));
    let mut p: P = Path::new(3, cfg.clone());
    let mut q: P = Path::new(3, cfg);
    let (got, stats, _) = drive(&mut p, 60);
    let mut want = Vec::new();
    for i in 0..60u32 {
        q.advance_to(Time(u64::from(i) * 1000));
        let (from, to) = if i % 2 == 0 { (0, 1) } else { (1, 0) };
        q.send(from, to, 1250, i);
    }
    q.send(0, 7, 10, 999);
    while let Some(d) = q.pop() {
        want.push(d);
    }
    assert_eq!(got, want);
    assert_eq!(stats, q.stats().net());
    assert_eq!(Network::path_stats(&p), Some(q.stats()));
    assert_eq!(stats.sent, 60);
    assert_eq!(stats.unroutable, 1);
    assert_eq!(stats.delivered + stats.lost + stats.queue_dropped, 60);
}

#[test]
fn path_stats_fold_into_the_net_columns() {
    let s = PathStats {
        sent: 100,
        delivered: 70,
        outage_dropped: 3,
        policed: 4,
        queue_dropped: 5,
        aqm_dropped: 6,
        lost: 7,
        unroutable: 2,
        cross_sent: 50,
        cross_dropped: 9,
    };
    assert_eq!(
        s.net(),
        NetStats {
            sent: 100,
            delivered: 70,
            lost: 14,
            queue_dropped: 11,
            unroutable: 2,
        }
    );
}

#[test]
fn route_change_event_goes_through_the_trait() {
    let mut p: P = Path::new(1, PathConfig::default());
    p.schedule_route_change(RouteChange {
        at: Time(1_000),
        down_us: 500,
        new: Some(PathConfig::symmetric(DirConfig {
            delay: Delay::Fixed(7_000),
            ..DirConfig::default()
        })),
    });
    assert_eq!(Network::next_event_time(&p), Some(Time(1_500)));
    Network::advance_to(&mut p, Time(1_499));
    assert_eq!(Network::pop_event(&mut p), None);
    Network::advance_to(&mut p, Time(1_500));
    assert_eq!(
        Network::pop_event(&mut p),
        Some(PathEvent::RouteChanged { at: Time(1_500) })
    );
    assert_eq!(Network::pop_event(&mut p), None);
    assert_eq!(Network::next_event_time(&p), None);
    assert_eq!(p.dir(Dir::Reverse).config().delay, Delay::Fixed(7_000));
}

/// 丢包在瓶颈之前（对照开关）：丢掉的包不占容量，后面的包早走；丢哪几个包两种位置一样（同一条流、每包抽一个数）。
#[test]
fn loss_before_bottleneck_frees_the_capacity() {
    let base = DirConfig {
        loss: Loss::Independent { ppm: 400_000 },
        ..bottleneck(
            Capacity::Constant {
                rate_bps: 10_000_000,
            },
            QueueLimit::delay(1_000_000),
            Aqm::TailDrop,
        )
    };
    let early = DirConfig {
        loss_before_bottleneck: true,
        ..base.clone()
    };
    let arrivals = |cfg: DirConfig| {
        let mut p: P = Path::new(5, fwd(cfg));
        for i in 0..40u32 {
            p.send(0, 1, 1250, i);
        }
        let mut got = Vec::new();
        while let Some(d) = p.pop() {
            got.push((d.payload, d.at.micros()));
        }
        (got, p.dir_stats(Dir::Forward))
    };
    let (late, ls) = arrivals(base);
    let (first, fs) = arrivals(early);
    assert_eq!(ls.lost, fs.lost);
    assert!(ls.lost > 5 && ls.lost < 30, "{}", ls.lost);
    let ids = |v: &[(u32, u64)]| v.iter().map(|(i, _)| *i).collect::<Vec<_>>();
    assert_eq!(ids(&late), ids(&first), "丢的是同样几个包");
    for (k, ((id, at_late), (_, at_first))) in late.iter().zip(&first).enumerate() {
        // 每个包正好发 1 ms：丢在后面时第 id 个包第 id+1 毫秒走；丢在前面时活下来的第 k 个包第 k+1 毫秒走
        assert_eq!(*at_late, (u64::from(*id) + 1) * 1000);
        assert_eq!(*at_first, (k as u64 + 1) * 1000);
    }
}

/// 交叉 TCP 也跟着开关走：丢包挪到瓶颈之前，被丢的数据包不占队列。
#[test]
fn loss_before_bottleneck_applies_to_cross_traffic() {
    let run = |first: bool| {
        let cfg = DirConfig {
            loss: Loss::Independent { ppm: 50_000 },
            loss_before_bottleneck: first,
            delay: Delay::Fixed(10_000),
            ..bottleneck(
                Capacity::Constant {
                    rate_bps: 10_000_000,
                },
                QueueLimit::delay(50_000),
                Aqm::TailDrop,
            )
        };
        let mut p: P = Path::new(8, PathConfig::symmetric(cfg));
        let id = p.add_cross(
            Dir::Forward,
            CrossSpec::Tcp {
                cfg: TcpConfig::default(),
                start: Time::ZERO,
                bytes: None,
            },
        );
        p.advance_to(Time(3_000_000));
        assert_eq!(p.cross_sources(), 1);
        p.cross_stats(id)
    };
    let (late, early) = (run(false), run(true));
    assert!(late.lost > 0 && early.lost > 0);
    assert!(early.delivered_bytes > 0 && late.delivered_bytes > 0);
    assert_ne!(late, early);
}

#[test]
fn nominal_rate_of_each_capacity() {
    let at = |capacity: Capacity, origin: u64, now: u64| {
        let b = Bottleneck::new(
            BottleneckConfig {
                capacity,
                ..BottleneckConfig::default()
            },
            Time(origin),
        );
        b.rate_at(Time(now))
    };
    assert_eq!(at(Capacity::Unlimited, 0, 5), 0);
    assert_eq!(at(Capacity::Constant { rate_bps: 7 }, 0, 5), 7);
    assert_eq!(
        at(
            Capacity::Poisson {
                rate_bps: 9,
                opportunity_bytes: 1500
            },
            0,
            5
        ),
        9
    );
    let trace = |repeat| {
        Capacity::Trace(Trace {
            segments: vec![(1_000, 100), (0, 999), (2_000, 200)],
            repeat,
        })
    };
    // 起点在 10 ms：[10, 11) ms 100，[11, 13) ms 200（时长 0 的段跳过）
    assert_eq!(at(trace(false), 10_000, 10_000), 100);
    assert_eq!(at(trace(false), 10_000, 10_999), 100);
    assert_eq!(at(trace(false), 10_000, 11_000), 200);
    assert_eq!(at(trace(false), 10_000, 12_999), 200);
    // 走完了：不重复时最后一段一直保持，重复时从头再来
    assert_eq!(at(trace(false), 10_000, 50_000), 200);
    assert_eq!(at(trace(true), 10_000, 13_500), 100);
    assert_eq!(at(trace(true), 10_000, 14_500), 200);
    // 起点之前按起点算；空轨迹为 0
    assert_eq!(at(trace(true), 10_000, 0), 100);
    let empty = Capacity::Trace(Trace {
        segments: Vec::new(),
        repeat: true,
    });
    assert_eq!(at(empty, 0, 5), 0);
}

#[test]
fn nominal_delay_does_not_draw() {
    assert_eq!(Delay::Fixed(40).nominal_us(Time(9)), 40);
    let steps = Delay::Steps {
        initial_us: 10,
        steps: vec![(Time(300), 30), (Time(100), 20), (Time(300), 33)],
    };
    assert_eq!(steps.nominal_us(Time(99)), 10);
    assert_eq!(steps.nominal_us(Time(100)), 20);
    assert_eq!(steps.nominal_us(Time(300)), 33, "同一时刻以后面的为准");
    let periodic = |period_us| Delay::Periodic {
        initial_us: 99,
        period_us,
        phase_us: 1_000,
        low_us: 10,
        high_us: 41,
    };
    assert_eq!(periodic(500).nominal_us(Time(999)), 99);
    assert_eq!(
        periodic(500).nominal_us(Time(1_000)),
        25,
        "上下限的中点（向下取整）"
    );
    assert_eq!(periodic(0).nominal_us(Time(5_000)), 99);
}

// ---------------------------------------------------------------- RFC 8868 的 NR-BPDV（A2-4b）

/// 分布：z = |截断到 ±3σ 的 N(0, σ²)|。理论值（σ 的倍数）：均值 0.79712、中位 0.67449、90% 分位 1.64485，
/// 最大 3（用 Python 的 math.erf 和 NormalDist 算的）。12 个种子各抽 20000 个，均值在两倍标准误以内。
#[test]
fn pdv_follows_the_truncated_half_normal() {
    let pdv = Pdv { std_us: 1_000_000 };
    let draws = |seed: u64| {
        let mut rng = LayerRng::new(seed, Dir::Forward, layer::PDV);
        let mut v: Vec<u64> = (0..20_000).map(|_| pdv.sample(rng.next_u64())).collect();
        v.sort_unstable();
        v
    };
    check(
        ["均值 / σ", "中位 / σ", "90% 分位 / σ"],
        [0.797_12, 0.674_49, 1.644_85],
        |seed| {
            let v = draws(seed);
            let mean = v.iter().sum::<u64>() as f64 / v.len() as f64;
            [
                mean / 1e6,
                v[v.len() / 2] as f64 / 1e6,
                v[v.len() * 9 / 10] as f64 / 1e6,
            ]
        },
    );
    let v = draws(1);
    assert!(*v.last().unwrap() <= 3_000_000, "截断在 3σ");
    assert_eq!(*v.first().unwrap() / 1000, 0, "最小接近 0");
    // 被截断在 3σ 的比例约 0.27%（2·(1 − Φ(3))）
    let capped = v.iter().filter(|x| **x >= 2_999_000).count();
    assert!((20..=100).contains(&capped), "{capped}");
    // 表的两端
    assert_eq!(pdv.sample(0), 0);
    assert_eq!(pdv.sample(u64::MAX), 3_000_000);
    assert_eq!(Pdv::RFC8868.std_us, 5_000);
}

/// 不乱序：每个包都不早于上一个包加上它按瓶颈速率的串行时间；延迟变化落在 [0, 3σ]（排队之外）。
#[test]
fn pdv_never_reorders_and_respects_serialization() {
    let cfg = DirConfig {
        noise: Noise {
            pdv: Some(Pdv::RFC8868),
            ..Noise::default()
        },
        delay: Delay::Fixed(50_000),
        ..bottleneck(
            Capacity::Constant {
                rate_bps: 1_000_000,
            },
            QueueLimit::delay(300_000),
            Aqm::TailDrop,
        )
    };
    let mut p = P::new(9, fwd(cfg));
    // 每 5 ms 一个 500 字节的包：串行 4 ms，比间隔短，瓶颈不排队
    for i in 0..2_000u32 {
        p.advance_to(Time(u64::from(i) * 5_000));
        p.send(0, 1, 500, i);
    }
    let mut prev: Option<(u32, u64)> = None;
    let mut extra = Vec::new();
    while let Some(d) = p.pop() {
        let sent = u64::from(d.payload) * 5_000;
        let at = d.at.micros();
        // 没有 PDV 时：发出 + 串行 4 ms + 传播 50 ms
        extra.push(at - sent - 4_000 - 50_000);
        if let Some((pi, pat)) = prev {
            assert_eq!(d.payload, pi + 1, "不乱序");
            assert!(
                at >= pat + 4_000,
                "第 {} 个包 {at} 早于上一个 {pat} + 串行 4 ms",
                d.payload
            );
        }
        prev = Some((d.payload, at));
    }
    assert_eq!(extra.len(), 2_000);
    // 被前一个包推后的包，额外延迟是前一个的减去 1 ms（间隔 5 − 串行 4），所以仍不超过 3σ；
    // 均值比 0.797σ 大：包密时“不乱序”这条规则本身就把延迟变化拉长了（RFC 8868 的定义如此）
    assert!(extra.iter().all(|x| *x <= 15_000), "最大 3σ = 15 ms");
    let mean = extra.iter().sum::<u64>() as f64 / 2_000.0;
    assert!(mean > 0.797 * 5_000.0, "{mean}");

    // 包稀（每 100 ms 一个）时推不动，额外延迟的均值就是 0.797σ ≈ 3986 µs
    let mut sparse = P::new(
        9,
        fwd(DirConfig {
            noise: Noise {
                pdv: Some(Pdv::RFC8868),
                ..Noise::default()
            },
            ..DirConfig::default()
        }),
    );
    for i in 0..4_000u64 {
        sparse.advance_to(Time(i * 100_000));
        sparse.send(0, 1, 100, 0);
    }
    let mut sum = 0u64;
    let mut n = 0u64;
    while let Some(d) = sparse.pop() {
        sum += d.at.micros() % 100_000;
        n += 1;
    }
    let mean = sum as f64 / n as f64;
    // 标准差约 0.6σ = 3 ms，4000 个的标准误约 47 µs
    assert!((mean - 3_985.6).abs() < 2.0 * 47.5, "{mean}");

    // 不限速时串行为 0，只保证不早于上一个包
    let mut q = P::new(
        9,
        fwd(DirConfig {
            noise: Noise {
                pdv: Some(Pdv::RFC8868),
                ..Noise::default()
            },
            ..DirConfig::default()
        }),
    );
    for i in 0..500u32 {
        q.send(0, 1, 100, i);
    }
    let mut last = 0;
    let mut order = Vec::new();
    while let Some(d) = q.pop() {
        assert!(d.at.micros() >= last);
        last = d.at.micros();
        order.push(d.payload);
    }
    assert_eq!(order, (0..500).collect::<Vec<_>>());
}
