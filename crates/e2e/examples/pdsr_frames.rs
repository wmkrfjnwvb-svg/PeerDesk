//! 从被控端会话记录（`.pdsr`，写到一半的 `.pdsr.part` 也行）读回逐帧的编码耗时和出发时刻，给编码耗时模型
//! （`peerdesk_e2e::encode_time`）校准用。只在本地跑。
//!
//! 用法：`pdsr_frames <记录文件>... [--csv 输出.csv]`。
//! 每个文件打印：元数据（角色、编码、帧率、是不是硬件编码）、编码任务的耗时分布（`took`，整个任务；`encode`，编码器本身），
//! 出发间隔（相邻两次“编完交给节奏器”的间隔）和出发抖动（|间隔 − 帧周期|，只算连着编的帧），采集间隔，采集到出发的耗时。
//! `--csv` 再写一份逐帧表：`file,at_us,frame_id,class,took_us,encode_us,convert_us,wait_us,captured_ago_us,bytes,hardware`。
//!
//! 时刻的含义（`crates/net/src/host.rs`）：帧记录的时间是编码任务交回会话循环、紧接着交给节奏器的那一刻，
//! 所以它就是这一帧的出发时刻；`took` 是编码线程里整个任务的耗时；采集时刻 = 时间 − `captured_ago_us`。

use std::io::Write as _;

use peerdesk_core::record::{FrameClass, Record, Recording};

fn quantiles(mut v: Vec<u64>) -> String {
    if v.is_empty() {
        return "（没有）".to_owned();
    }
    v.sort_unstable();
    let q = |p: usize| v.get(p * (v.len() - 1) / 100).copied().unwrap_or(0);
    format!(
        "{} 个：p10 {:.1} / p50 {:.1} / p90 {:.1} / p99 {:.1} / 最大 {:.1} ms",
        v.len(),
        q(10) as f64 / 1e3,
        q(50) as f64 / 1e3,
        q(90) as f64 / 1e3,
        q(99) as f64 / 1e3,
        v.last().copied().unwrap_or(0) as f64 / 1e3
    )
}

fn class_name(c: FrameClass) -> &'static str {
    match c {
        FrameClass::Content => "content",
        FrameClass::Refine => "refine",
        FrameClass::Key => "key",
    }
}

fn main() {
    let mut files = Vec::new();
    let mut csv: Option<std::fs::File> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--csv" {
            csv = args.next().and_then(|p| std::fs::File::create(p).ok());
            if let Some(f) = csv.as_mut() {
                let _ = writeln!(
                    f,
                    "file,at_us,frame_id,class,took_us,encode_us,convert_us,wait_us,captured_ago_us,bytes,hardware"
                );
            }
        } else {
            files.push(a);
        }
    }
    let mut out = std::io::stdout().lock();
    if files.is_empty() {
        let _ = writeln!(out, "用法：pdsr_frames <记录文件>... [--csv 输出.csv]");
        return;
    }
    for path in &files {
        let rec = match std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|b| Recording::parse(&b).map_err(|e| format!("{e:?}")))
        {
            Ok(r) => r,
            Err(e) => {
                let _ = writeln!(out, "{path}：读不出来：{e}");
                continue;
            }
        };
        let fps = rec.meta.fps.unwrap_or(30).max(1);
        let period = 1_000_000 / u64::from(fps);
        let mut frames: Vec<(u64, &peerdesk_core::record::Frame)> = rec
            .entries
            .iter()
            .filter_map(|e| match &e.record {
                Record::Frame(f) => Some((e.at_us, f)),
                _ => None,
            })
            .collect();
        frames.sort_by_key(|(t, _)| *t);
        let hardware = frames.iter().filter(|(_, f)| f.hardware).count();
        let _ = writeln!(
            out,
            "# {path}\n角色 {:?}，编码 {}，帧率 {fps}，系统 {:?}；帧记录 {} 条（硬件编码 {} 条），损坏 {:?}",
            rec.meta.role,
            rec.meta.codec.as_deref().unwrap_or("?"),
            rec.meta.os,
            frames.len(),
            hardware,
            rec.stats
        );
        let took: Vec<u64> = frames
            .iter()
            .filter_map(|(_, f)| f.timing.map(|t| t.took_us))
            .collect();
        let enc: Vec<u64> = frames
            .iter()
            .filter_map(|(_, f)| f.timing.map(|t| t.encode_us))
            .collect();
        let _ = writeln!(out, "编码任务耗时 took：{}", quantiles(took));
        let _ = writeln!(out, "编码器本身 encode：{}", quantiles(enc));
        for class in [FrameClass::Content, FrameClass::Refine, FrameClass::Key] {
            let v: Vec<u64> = frames
                .iter()
                .filter(|(_, f)| f.class == class)
                .filter_map(|(_, f)| f.timing.map(|t| t.took_us))
                .collect();
            let _ = writeln!(out, "  {} 的 took：{}", class_name(class), quantiles(v));
        }
        // 出发间隔；连着编的帧（间隔不超过 1.5 个周期）的 |间隔 − 周期|（逐帧的出发抖动，不累积时钟漂移）
        let gaps: Vec<u64> = frames
            .windows(2)
            .map(|w| w[1].0.saturating_sub(w[0].0))
            .collect();
        let _ = writeln!(out, "出发间隔：{}", quantiles(gaps.clone()));
        let jitter: Vec<u64> = gaps
            .iter()
            .filter(|g| **g <= period * 3 / 2)
            .map(|g| g.abs_diff(period))
            .collect();
        let _ = writeln!(
            out,
            "出发抖动 |间隔 − 周期|（连着编的帧）：{}",
            quantiles(jitter)
        );
        // 采集时刻 = 记录时刻 − captured_ago；采集间隔和“采集到出发”
        let captured: Vec<u64> = frames
            .iter()
            .filter_map(|(t, f)| f.captured_ago_us.map(|a| t.saturating_sub(a)))
            .collect();
        let cgaps: Vec<u64> = captured
            .windows(2)
            .map(|w| w[1].saturating_sub(w[0]))
            .collect();
        let _ = writeln!(out, "采集间隔：{}", quantiles(cgaps));
        let ago: Vec<u64> = frames
            .iter()
            .filter_map(|(_, f)| f.captured_ago_us)
            .collect();
        let _ = writeln!(out, "采集到出发：{}", quantiles(ago));
        if let Some(f) = csv.as_mut() {
            for (t, fr) in &frames {
                let tm = fr.timing;
                let _ = writeln!(
                    f,
                    "{path},{t},{},{},{},{},{},{},{},{},{}",
                    fr.frame_id,
                    class_name(fr.class),
                    tm.map_or(0, |x| x.took_us),
                    tm.map_or(0, |x| x.encode_us),
                    tm.map_or(0, |x| x.convert_us),
                    tm.map_or(0, |x| x.wait_us),
                    fr.captured_ago_us.unwrap_or(0),
                    fr.sent_bytes().unwrap_or(0),
                    u8::from(fr.hardware)
                );
            }
        }
    }
}
