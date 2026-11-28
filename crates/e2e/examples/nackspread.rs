//! 诊断（只在本地跑）：没有丢包、只有一个固定容量的瓶颈时，接收端“首片之后 10 ms 还没到就请求重传”会请求多少分片。
//! 检验 a3-plan 第 11 节的假设 S：节奏器按远高于瓶颈的速率把一帧发出去，帧在瓶颈排队、按瓶颈速率放出，
//! 帧越大摊得越开，后半截过了 10 ms 的线就被请求（它们没丢，还在路上）。
//!
//! 用法：`nackspread [种子数]`（默认 3）。每个容量、每个种子跑一遍办公负载（L1 H.264），按两秒一个窗口，
//! 用调试记录里每秒一行的累计“重传请求”和 `tally.window_bytes` 算每个窗口的线上码率、每秒请求的分片数、请求 ÷ 发出，
//! 再按线上码率分档汇总，和真机的表（`docs/research/data/real-4g-mac-2026-11-28-per-second.txt`）对照。
//! 网络不丢包，所以每一个请求都是虚假的。发出的包数用“字节 ÷ 1100”估（真机上每个 UDP 包平均约 1100 字节）。

use std::io::Write as _;

use peerdesk_core::adaptive::Profile;
use peerdesk_core::sim::Link;
use peerdesk_e2e::{run, Action, EstimatorKind, NetworkPlan, Phase, Scenario, WINDOW_US};

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

/// 调试记录里每秒一行“发送端累计：…重传请求 N…”，取出 N。
fn cumulative_requests(trace: &[String]) -> Vec<u64> {
    trace
        .iter()
        .filter(|l| l.contains("发送端累计"))
        .filter_map(|l| {
            let after = l.split("重传请求 ").nth(1)?;
            after
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .collect()
}

fn main() {
    let seeds: u64 = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let buckets = [
        (0u64, 1000u64),
        (1000, 2500),
        (2500, 3500),
        (3500, u64::MAX),
    ];
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "容量 | 线上码率档 | 窗口数 | 每秒请求 | 请求 ÷ 发出");
    for rate_mbps in [10u64, 16, 20, 30] {
        // （窗口数，请求数，估计的发出包数，秒数）
        let mut acc = vec![(0u64, 0u64, 0f64, 0f64); buckets.len()];
        for seed in 1..=seeds {
            let mut s = Scenario::fixed("nackspread", workload());
            s.network = NetworkPlan::Layout1(vec![(
                0,
                Link {
                    delay_us: 15_000,
                    jitter_us: 0,
                    rate_bps: Some(rate_mbps * 1_000_000),
                    max_queue_us: 200_000,
                    ..Link::ideal()
                },
            )]);
            s.profile = Profile::WIRELESS;
            s.estimator = EstimatorKind::ClassicV1;
            s.seed = seed;
            s.trace = true;
            let o = match run(&s) {
                Ok(o) => o,
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "{rate_mbps} Mbps 种子 {seed}：{e}");
                    continue;
                }
            };
            let cum = cumulative_requests(&o.trace);
            // 第 k 秒末的累计在 cum[k]（调试记录从第 0 秒起每秒一行）；两秒一窗：窗口 w 是第 2w、2w+1 秒
            let secs_per_window = WINDOW_US / 1_000_000;
            for (w, bytes) in o.tally.window_bytes.iter().enumerate() {
                let w = w as u64;
                let end = usize::try_from((w + 1) * secs_per_window).unwrap_or(usize::MAX);
                let start = usize::try_from(w * secs_per_window).unwrap_or(usize::MAX);
                let (Some(&b), Some(&a)) = (
                    cum.get(end.min(cum.len().saturating_sub(1))),
                    cum.get(start),
                ) else {
                    continue;
                };
                let req = b.saturating_sub(a);
                let kbps = bytes * 8 / 1000 / secs_per_window;
                let packets = *bytes as f64 / 1100.0;
                if let Some(i) = buckets
                    .iter()
                    .position(|(lo, hi)| kbps >= *lo && kbps < *hi)
                {
                    if let Some(x) = acc.get_mut(i) {
                        x.0 += 1;
                        x.1 += req;
                        x.2 += packets;
                        x.3 += secs_per_window as f64;
                    }
                }
            }
            let _ = writeln!(
                std::io::stderr(),
                "{rate_mbps} Mbps 种子 {seed}：网络丢 {}，排队丢 {}，重传 {}",
                o.net.lost,
                o.net.queue_dropped,
                o.retransmitted
            );
        }
        for ((lo, hi), (n, req, packets, secs)) in buckets.iter().zip(&acc) {
            if *n == 0 {
                continue;
            }
            let hi = if *hi == u64::MAX {
                "以上".to_owned()
            } else {
                format!("–{hi}")
            };
            let _ = writeln!(
                out,
                "{rate_mbps} Mbps | {lo}{hi} kbps | {n} | {:.0} | {:.3}",
                *req as f64 / secs,
                *req as f64 / packets.max(1.0)
            );
        }
    }
}
