//! RFC 8867 的 8 个基础用例的冒烟（A2-4c）：每个用例只跑前 10 秒，保证能跑、指标有数。
//! 完整的用例（12 个种子、按均值加两倍标准误）用例子程序 `rfc8867` 在本地跑，不进 CI。
//! 各用例互不相干，几个线程分着跑（单线程约 150 秒）。
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use peerdesk_e2e::rfc8867::{cases, report, truncated, Case};
use peerdesk_e2e::run_multi;

fn check(c: &Case) {
    let out = run_multi(&c.multi).unwrap();
    assert_eq!(out.len(), c.media.len(), "{}", c.id);
    let r = report(c, &out);
    for (i, f) in r.flows.iter().enumerate() {
        assert!(out[i].frames > 0, "{} 第 {} 路没有发帧", c.id, i + 1);
        assert!(f.mean_recv_bps > 0.0, "{} 第 {} 路没收到", c.id, i + 1);
        assert!(f.delay_ms.0.is_some(), "{}", c.id);
        assert!((0.0..=1.0).contains(&f.loss), "{}", c.id);
        assert!(f.feedback_overhead > 0.0, "{}", c.id);
    }
    assert!(!r.spans.is_empty(), "{}", c.id);
    assert!(r.utilization > 0.0, "{}", c.id);
    assert_eq!(r.tcp.is_some(), !c.tcp.is_empty(), "{}", c.id);
}

#[test]
fn every_rfc_8867_case_runs_and_has_metrics() {
    let all: Vec<Case> = cases(1).into_iter().map(|c| truncated(c, 10)).collect();
    let next = AtomicUsize::new(0);
    let failures = Mutex::new(Vec::new());
    let jobs = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(all.len());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            let _ = scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(c) = all.get(i) else { break };
                if std::panic::catch_unwind(|| check(c)).is_err() {
                    failures.lock().unwrap().push(c.id.clone());
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(failures.is_empty(), "没过的用例：{failures:?}");
}
