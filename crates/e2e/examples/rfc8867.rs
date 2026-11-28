//! RFC 8867 的 8 个基础用例（A2-4c）：每个用例跑 N 个种子，按用例出一份 markdown 报告（RFC 8868 的指标，见
//! `peerdesk_e2e::rfc8867` 的模块文档）。只在本地跑，不进 CI（CI 里是 `tests/rfc8867.rs` 的 10 秒冒烟）。
//!
//! 用法：`rfc8867 [--seeds N] [--jobs N] [--out 报告.md] [--only 用例名前缀] [--size 宽x高]`
//! - `--size`：换画面尺寸（默认 640×360，见 `rfc8867::MEDIA_SIZE`）；5.1 的 720p 对照用 `--only 5.1 --size 1280x720`；
//! - 默认 3 个种子（开发时迭代）；最终报告用 12 个（主会话 2026-11-28 定），按均值加减两倍标准误；
//! - 一次运行 = 一个用例 × 一个种子，几个线程分着跑；
//! - 报告里每个数都能追到用例名和种子（种子 1..=N）。

use std::io::Write as _;
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use peerdesk_e2e::rfc8867::{cases, report, Case, Convergence, Report};
use peerdesk_e2e::run_multi;

struct Options {
    seeds: u64,
    jobs: usize,
    out: Option<String>,
    only: Option<String>,
    size: Option<(u16, u16)>,
}

fn parse() -> Result<Options, String> {
    let mut o = Options {
        seeds: 3,
        jobs: std::thread::available_parallelism().map_or(1, usize::from),
        out: None,
        only: None,
        size: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |what: &str| args.next().ok_or(format!("{what} 后面要跟值"));
        match a.as_str() {
            "--seeds" => {
                o.seeds = value("--seeds")?
                    .parse()
                    .map_err(|_| "--seeds 要是正整数".to_owned())?;
            }
            "--jobs" => {
                o.jobs = value("--jobs")?
                    .parse()
                    .map_err(|_| "--jobs 要是正整数".to_owned())?;
            }
            "--out" => o.out = Some(value("--out")?),
            "--only" => o.only = Some(value("--only")?),
            "--size" => {
                let v = value("--size")?;
                let parsed = v
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
                o.size = Some(parsed.ok_or(format!("--size {v:?} 要写成 宽x高"))?);
            }
            other => return Err(format!("不认识的参数 {other}")),
        }
    }
    if o.seeds == 0 || o.jobs == 0 {
        return Err("--seeds、--jobs 都要大于 0".to_owned());
    }
    Ok(o)
}

/// 均值和两倍标准误。
fn mean_se2(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    if v.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    let m = v.iter().sum::<f64>() / n;
    if v.len() < 2 {
        return (m, f64::NAN);
    }
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
    (m, 2.0 * (var / n).sqrt())
}

fn fmt(v: &[f64], scale: f64, digits: usize) -> String {
    let (m, s) = mean_se2(v);
    if s.is_nan() {
        format!("{:.*}", digits, m * scale)
    } else {
        format!("{:.*} ± {:.*}", digits, m * scale, digits, s * scale)
    }
}

/// 一个用例 N 个种子的报告。
fn section(id: &str, title: &str, runs: &[Report]) -> String {
    let mut s = format!("## {id}：{title}（{} 个种子）\n\n", runs.len());
    let Some(first) = runs.first() else {
        return s;
    };
    s.push_str("| 路 | 接收速率 kbps | 单程延迟中位 / 95% / 99% ms | 接收端丢包 | 反馈开销 | 冻结 | 最长等待 ms |\n");
    s.push_str("|---|---|---|---|---|---|---|\n");
    for i in 0..first.flows.len() {
        let col = |f: &dyn Fn(&Report) -> Option<f64>| -> Vec<f64> {
            runs.iter().filter_map(f).collect()
        };
        let get = |r: &Report| r.flows.get(i).cloned();
        let rate = col(&|r| get(r).map(|f| f.mean_recv_bps));
        let p50 = col(&|r| get(r).and_then(|f| f.delay_ms.0).map(|x| x as f64));
        let p95 = col(&|r| get(r).and_then(|f| f.delay_ms.1).map(|x| x as f64));
        let p99 = col(&|r| get(r).and_then(|f| f.delay_ms.2).map(|x| x as f64));
        let loss = col(&|r| get(r).map(|f| f.loss));
        let fb = col(&|r| get(r).map(|f| f.feedback_overhead));
        let fr = col(&|r| get(r).map(|f| f64::from(f.freezes)));
        let mw = col(&|r| get(r).map(|f| f.max_wait_ms));
        s.push_str(&format!(
            "| {} | {} | {} / {} / {} | {}% | {}% | {} | {} |\n",
            i + 1,
            fmt(&rate, 1e-3, 0),
            fmt(&p50, 1.0, 0),
            fmt(&p95, 1.0, 0),
            fmt(&p99, 1.0, 0),
            fmt(&loss, 100.0, 2),
            fmt(&fb, 100.0, 1),
            fmt(&fr, 1.0, 1),
            fmt(&mw, 1.0, 0),
        ));
    }
    s.push_str("\n| 段 | 路 | 公平份额 kbps | 稳定值 ÷ 公平份额 | 收敛（收敛 / 未收敛 / 段太短） | 收敛时间 s（收敛的） | 振荡 | 同方向公平（最小 ÷ 最大） |\n");
    s.push_str("|---|---|---|---|---|---|---|---|\n");
    for (k, span) in first.spans.iter().enumerate() {
        let label = format!(
            "{}–{} s",
            span.span.start_us / 1_000_000,
            span.span.end_us / 1_000_000
        );
        let fair_col: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.spans.get(k).and_then(|x| x.fairness))
            .collect();
        for (j, f) in span.flows.iter().enumerate() {
            let at = |r: &Report| r.spans.get(k).and_then(|x| x.flows.get(j)).cloned();
            let ratio: Vec<f64> = runs
                .iter()
                .filter_map(&at)
                .filter(|x| x.fair_bps > 0.0)
                .map(|x| x.stable_bps / x.fair_bps)
                .collect();
            let (mut c, mut n, mut t) = (0, 0, 0);
            let mut times = Vec::new();
            for x in runs.iter().filter_map(&at) {
                match x.convergence {
                    Convergence::Converged(v) => {
                        c += 1;
                        times.push(v);
                    }
                    Convergence::NotConverged(_) => n += 1,
                    Convergence::TooShort => t += 1,
                }
            }
            let osc: Vec<f64> = runs
                .iter()
                .filter_map(&at)
                .map(|x| f64::from(x.oscillations))
                .collect();
            let fairness = if j == 0 && !fair_col.is_empty() {
                fmt(&fair_col, 1.0, 2)
            } else {
                String::new()
            };
            s.push_str(&format!(
                "| {label} | {} | {:.0} | {} | {c} / {n} / {t} | {} | {} | {fairness} |\n",
                f.flow + 1,
                f.fair_bps / 1e3,
                fmt(&ratio, 1.0, 2),
                if times.is_empty() {
                    "—".to_owned()
                } else {
                    fmt(&times, 1.0, 1)
                },
                fmt(&osc, 1.0, 1),
            ));
        }
    }
    let util: Vec<f64> = runs.iter().map(|r| r.utilization).collect();
    let qm: Vec<f64> = runs.iter().map(|r| r.queue_ms.0).collect();
    let qx: Vec<f64> = runs.iter().map(|r| r.queue_ms.1).collect();
    s.push_str(&format!(
        "\n利用率（画面方向各路发出之和 ÷ 容量）{}%；真实排队均值 {} ms、最大 {} ms。",
        fmt(&util, 100.0, 1),
        fmt(&qm, 1.0, 1),
        fmt(&qx, 1.0, 0)
    ));
    let tcp: Vec<(f64, f64)> = runs.iter().filter_map(|r| r.tcp).collect();
    if !tcp.is_empty() {
        let thr: Vec<f64> = tcp.iter().map(|t| t.0).collect();
        let loss: Vec<f64> = tcp.iter().map(|t| t.1).collect();
        s.push_str(&format!(
            "TCP 吞吐 {} kbps、数据包丢失 {}%。",
            fmt(&thr, 1e-3, 0),
            fmt(&loss, 100.0, 2)
        ));
    }
    s.push_str("\n\n");
    s
}

fn main() -> ExitCode {
    let o = match parse() {
        Ok(o) => o,
        Err(e) => {
            let _ = writeln!(
                std::io::stderr(),
                "{e}\n用法：rfc8867 [--seeds N] [--jobs N] [--out 报告.md] [--only 用例名前缀]"
            );
            return ExitCode::from(2);
        }
    };
    // 每个种子一套用例（种子进了世界的随机数）；一次运行 = (用例序号, 种子)
    let all: Vec<Vec<Case>> = (1..=o.seeds)
        .map(|seed| {
            cases(seed)
                .into_iter()
                .filter(|c| o.only.as_ref().is_none_or(|p| c.id.starts_with(p.as_str())))
                .map(|c| match o.size {
                    Some((w, h)) => peerdesk_e2e::rfc8867::with_size(c, w, h),
                    None => c,
                })
                .collect()
        })
        .collect();
    let n_cases = all.first().map_or(0, Vec::len);
    let jobs_list: Vec<(usize, usize)> = (0..n_cases)
        .flat_map(|c| (0..all.len()).map(move |s| (c, s)))
        .collect();
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Vec<Option<Report>>>> = Mutex::new(vec![vec![None; all.len()]; n_cases]);
    let failed = AtomicUsize::new(0);
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..o.jobs.min(jobs_list.len().max(1)) {
            let _ = scope.spawn(|| loop {
                let k = next.fetch_add(1, Ordering::Relaxed);
                let Some(&(ci, si)) = jobs_list.get(k) else {
                    break;
                };
                let Some(case) = all.get(si).and_then(|v| v.get(ci)) else {
                    continue;
                };
                let t0 = std::time::Instant::now();
                match run_multi(&case.multi) {
                    Ok(out) => {
                        let r = report(case, &out);
                        let _ = writeln!(
                            std::io::stderr(),
                            "{} 种子 {}：{:.0} 秒",
                            case.id,
                            si + 1,
                            t0.elapsed().as_secs_f64()
                        );
                        if let Ok(mut g) = results.lock() {
                            if let Some(slot) = g.get_mut(ci).and_then(|v| v.get_mut(si)) {
                                *slot = Some(r);
                            }
                        }
                    }
                    Err(e) => {
                        let _ = failed.fetch_add(1, Ordering::Relaxed);
                        let _ = writeln!(std::io::stderr(), "{} 种子 {}：{e}", case.id, si + 1);
                    }
                }
            });
        }
    });
    let results = results.into_inner().unwrap_or_default();
    let mut md = format!(
        "# RFC 8867 基础用例的结果（{} 个种子，{} 次运行，{:.0} 分钟）\n\n由 `crates/e2e/examples/rfc8867.rs` 生成；指标的定义见 `crates/e2e/src/rfc8867.rs` 的模块文档。\n\n",
        o.seeds,
        jobs_list.len(),
        started.elapsed().as_secs_f64() / 60.0
    );
    for (ci, runs) in results.iter().enumerate() {
        let Some(case) = all.first().and_then(|v| v.get(ci)) else {
            continue;
        };
        let runs: Vec<Report> = runs.iter().flatten().cloned().collect();
        md.push_str(&section(&case.id, &case.title, &runs));
    }
    match &o.out {
        Some(p) => {
            if let Err(e) = std::fs::write(p, &md) {
                let _ = writeln!(std::io::stderr(), "写 {p} 失败：{e}");
                return ExitCode::FAILURE;
            }
        }
        None => {
            let _ = write!(std::io::stdout(), "{md}");
        }
    }
    if failed.load(Ordering::Relaxed) > 0 {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
