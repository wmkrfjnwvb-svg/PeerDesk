//! 诊断用：跑一个世界文件，打开事件记录，把会话的时间线写出来，供两个版本对着看（为什么这里冻结了）。
//!
//! 用法：`cargo run --release -p peerdesk-e2e --example trace -- <世界文件> <输出目录>`
//!
//! 输出目录里：
//! - `trace.txt`：事件（降码率、状态切换、探测、冻结……），每行带仿真时刻；
//! - `features.csv`：每份反馈一行（码率、排队、送达速率、状态，以及仿真知道的真实容量和真实排队）；
//! - `phases.txt`：每个阶段的开始时刻、操作、冻结次数、最长等待。
//!
//! 只读，不改会话：和批量运行器跑同一个世界，结果逐字节相同（指纹一样）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use peerdesk_e2e::world::World;
use peerdesk_e2e::{fingerprint, FeatureRow};

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::File::create(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| format!("{}：{e}", path.display()))
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(world), Some(out)) = (args.first(), args.get(1)) else {
        let _ = writeln!(
            std::io::stderr(),
            "用法：trace <世界文件> <输出目录>"
        );
        return ExitCode::from(2);
    };
    match run(Path::new(world), &PathBuf::from(out)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "{e}");
            ExitCode::FAILURE
        }
    }
}

fn run(world: &Path, out: &Path) -> Result<(), String> {
    let mut w = World::load(world).map_err(|e| e.to_string())?;
    w.scenario.trace = true;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let o = peerdesk_e2e::run(&w.scenario).map_err(|e| e.to_string())?;
    write(&out.join("trace.txt"), &(o.trace.join("\n") + "\n"))?;
    let mut csv = String::from(FeatureRow::HEADER);
    csv.push('\n');
    for f in &o.features {
        csv.push_str(&f.csv());
        csv.push('\n');
    }
    write(&out.join("features.csv"), &csv)?;
    let interval_us = 1_000_000 / u64::from(w.scenario.fps.max(1));
    let mut start_us = 0u64;
    let mut phases = String::from("开始ms\t操作\t冻结\t最长等待ms\t千比特每秒\n");
    for (p, r) in w.scenario.phases.iter().zip(&o.phases) {
        phases.push_str(&format!(
            "{}\t{:?}\t{}\t{:.0}\t{}\n",
            start_us / 1000,
            r.action,
            r.freezes,
            r.max_wait_ms,
            r.kbps
        ));
        start_us = start_us.saturating_add(u64::from(p.frames).saturating_mul(interval_us));
    }
    phases.push_str(&format!(
        "合计冻结 {}，最长等待 {:.0} ms，指纹 {:016x}\n",
        o.freezes,
        o.max_wait_ms,
        fingerprint(&o)
    ));
    write(&out.join("phases.txt"), &phases)?;
    Ok(())
}
