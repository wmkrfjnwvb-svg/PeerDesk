//! 诊断（只在本地跑）：RFC 8867 多流用例里窄段一路被饿死，逐格看两路各自发生了什么（a3-plan 第 11 节）。
//!
//! 用法：`starve [用例名] [种子] [起 s] [止 s]`（默认 `5.2-trace 1 20 55`）。
//! 每路按 100 ms 一格打印：码率目标（这一格最后一份反馈后的）、送达速率、发出和收到的线上码率、
//! 接收端报的排队和延迟趋势（这一格最大）、丢包（千分比，最大）、真实瓶颈排队、码率状态；
//! 再打印这段时间里码率控制写下的决定和原因，以及每秒的累计重传请求。只读记录，不改会话。

use std::collections::BTreeMap;
use std::io::Write as _;

use peerdesk_e2e::encode_time::{EncodeTime, MAC_MEDIAN_US};
use peerdesk_e2e::rfc8867::{cases, truncated};
use peerdesk_e2e::{run_multi, Action, FeatureRow, Phase};

/// 办公负载（和 `nackspread` 同一套，一轮 495 帧），重复到 `frames` 帧。
fn office_phases(frames: u32) -> Vec<Phase> {
    let p = |action, frames| Phase { action, frames };
    let cycle = [
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
    let mut out = Vec::new();
    let mut left = frames;
    for ph in cycle.iter().cycle() {
        if left == 0 {
            break;
        }
        let n = ph.frames.min(left);
        out.push(Phase {
            action: ph.action,
            frames: n,
        });
        left -= n;
    }
    out
}

/// 一格的汇总。
#[derive(Default)]
struct Bin {
    rate_kbps: u64,
    recv_kbps: u64,
    queue_ms: u32,
    trend: i32,
    loss: u16,
    true_queue_ms: u64,
    state: String,
    reports: u32,
}

fn bins(rows: &[FeatureRow], t0: u64, t1: u64) -> BTreeMap<u64, Bin> {
    let mut out: BTreeMap<u64, Bin> = BTreeMap::new();
    for r in rows.iter().filter(|r| r.t_ms >= t0 && r.t_ms < t1) {
        let b = out.entry(r.t_ms / 100).or_default();
        b.rate_kbps = r.rate_bps / 1000;
        b.recv_kbps = u64::from(r.recv) * 8 / 1000;
        b.queue_ms = b.queue_ms.max(r.queue_us / 1000);
        b.trend = b.trend.max(r.trend);
        b.loss = b.loss.max(r.loss);
        b.true_queue_ms = b.true_queue_ms.max(r.true_queue_us / 1000);
        b.state = format!("{:?}", r.state);
        b.reports += 1;
    }
    out
}

/// 调试记录里的时刻（`[ 12345 ms]` 开头的）。
fn line_ms(l: &str) -> Option<u64> {
    let inner = l.strip_prefix('[')?.split(" ms]").next()?;
    inner.trim().parse().ok()
}

/// 一路在 [t0, t1) 毫秒里的平均接收码率（kbps）。
fn mean_recv_kbps(f: &peerdesk_e2e::flow::FlowLog, t0: u64, t1: u64) -> u64 {
    let (a, b) = (
        usize::try_from(t0 / 100).unwrap_or(0),
        usize::try_from(t1 / 100).unwrap_or(0),
    );
    let bytes: u64 = f.recv_bytes.iter().skip(a).take(b.saturating_sub(a)).sum();
    bytes * 8 / (t1 - t0).max(1)
}

/// 相位扫描：第 2 路的帧时钟比第 1 路晚 d 毫秒（d = 0, 3, …, 30），其余不变，看 [t0, t1) 里两路各收到多少。
/// 公平只随相位变，就是两路严格同频、相位固定带来的假象（小流的包总落在大流一帧突发的同一个位置）。
fn sweep(case: peerdesk_e2e::rfc8867::Case, t0: u64, t1: u64) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(
        out,
        "# {} 相位扫描，{} – {} s 的平均接收 kbps",
        case.id,
        t0 / 1000,
        t1 / 1000
    );
    let mut base = truncated(case, t1 / 1000 + 1);
    let enc = std::env::var_os("STARVE_ENC").is_some();
    let office = std::env::var_os("STARVE_OFFICE").is_some();
    let _ = writeln!(
        out,
        "编码耗时模型 {}，内容 {}",
        if enc {
            "openh264 办公形状、中位 7 ms"
        } else {
            "按 0 算"
        },
        if office {
            "办公（只在画面变了时编码，各路内容种子不同）"
        } else {
            "一直在播的视频"
        }
    );
    // 视频通话的帧内排开倍数（千分比）换成 STARVE_PACE（0 是整帧同时发）
    if let Some(pace) = std::env::var("STARVE_PACE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
    {
        if let peerdesk_e2e::NetworkPlan::Layout2(plan) = &mut base.multi.network {
            for (_, c) in &mut plan.cross {
                if let peerdesk_core::path::CrossSpec::Frames { pace_permille, .. } = c {
                    *pace_permille = pace;
                }
            }
        }
        let _ = writeln!(out, "视频通话帧内排开 {pace}‰");
    }
    for (i, p) in base.multi.pairs.iter_mut().enumerate() {
        if enc {
            p.scenario.encode_time = EncodeTime::Openh264Office {
                median_us: MAC_MEDIAN_US,
            };
        }
        if office {
            let frames: u32 = p.scenario.phases.iter().map(|ph| ph.frames).sum();
            p.scenario.phases = office_phases(frames);
            p.scenario.seed += 1000 * u64::try_from(i).unwrap_or(0);
        }
    }
    let start = base.multi.pairs.first().map_or(0, |p| p.start_us);
    for d in (0..=30).step_by(3) {
        let mut c = base.clone();
        // 两路时挪第 2 路；只有一路时（例如 pd-video-call）挪它自己，相对的是交叉流的帧时钟
        let shifted = usize::from(c.multi.pairs.len() > 1);
        for (i, p) in c.multi.pairs.iter_mut().enumerate() {
            p.start_us = start + if i == shifted { d * 1000 } else { 0 };
        }
        match run_multi(&c.multi) {
            Ok(o) => {
                let r: Vec<u64> = o.iter().map(|o| mean_recv_kbps(&o.flow, t0, t1)).collect();
                let _ = writeln!(out, "相差 {d:>2} ms：{r:?}");
            }
            Err(e) => {
                let _ = writeln!(out, "相差 {d:>2} ms：运行失败 {e:?}");
            }
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let id = args.next().unwrap_or_else(|| "5.2-trace".into());
    let seed: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(1);
    let t0: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(20) * 1000;
    let t1: u64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(55) * 1000;
    let mut out = std::io::stdout().lock();
    let Some(mut case) = cases(seed).into_iter().find(|c| c.id == id) else {
        let _ = writeln!(out, "没有用例 {id}");
        return;
    };
    if std::env::var_os("STARVE_SWEEP").is_some() {
        sweep(case, t0, t1);
        return;
    }
    for p in &mut case.multi.pairs {
        p.scenario.trace = true;
    }
    let outcomes = match run_multi(&case.multi) {
        Ok(o) => o,
        Err(e) => {
            let _ = writeln!(out, "运行失败：{e:?}");
            return;
        }
    };
    let _ = writeln!(out, "# {id} 种子 {seed}，{} – {} s", t0 / 1000, t1 / 1000);
    let per: Vec<BTreeMap<u64, Bin>> = outcomes.iter().map(|o| bins(&o.features, t0, t1)).collect();
    let _ = writeln!(
        out,
        "\n格(s) | 路: 目标 kbps / 送达 kbps / 发 kbps / 收 kbps / 排队 ms / 趋势 0.1ms / 丢‰ / 真排队 ms / 状态 / 反馈份数"
    );
    for k in (t0 / 100)..(t1 / 100) {
        let mut line = format!("{:>5.1}", k as f64 / 10.0);
        for (i, (o, b)) in outcomes.iter().zip(&per).enumerate() {
            let idx = usize::try_from(k).unwrap_or(usize::MAX);
            let sent = o.flow.sent_bytes.get(idx).copied().unwrap_or(0) * 8 / 100;
            let recv = o.flow.recv_bytes.get(idx).copied().unwrap_or(0) * 8 / 100;
            match b.get(&k) {
                Some(b) => line.push_str(&format!(
                    " | {}: {:>4} / {:>4} / {:>4} / {:>4} / {:>3} / {:>4} / {:>3} / {:>3} / {:<9} / {}",
                    i + 1,
                    b.rate_kbps,
                    b.recv_kbps,
                    sent,
                    recv,
                    b.queue_ms,
                    b.trend,
                    b.loss,
                    b.true_queue_ms,
                    b.state,
                    b.reports
                )),
                None => line.push_str(&format!(
                    " | {}: (无反馈) 发 {sent} 收 {recv}",
                    i + 1
                )),
            }
        }
        let _ = writeln!(out, "{line}");
    }
    for (i, o) in outcomes.iter().enumerate() {
        let _ = writeln!(out, "\n## 第 {} 路的决定和每秒累计", i + 1);
        // 没有时刻的续行（每秒的累计）跟着上一行的时刻走
        let mut in_range = false;
        for l in &o.trace {
            if let Some(t) = line_ms(l) {
                in_range = t >= t0 && t < t1;
            }
            if in_range && !l.contains("大帧") {
                let _ = writeln!(out, "{l}");
            }
        }
    }
}
