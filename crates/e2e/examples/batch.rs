//! 批量运行器（只在本地跑，不进 CI、不进安装包）：输入一批世界文件（[`peerdesk_e2e::world`]），并行跑，
//! 每个会话输出一行 CSV。
//!
//! 用法：`batch [--jobs N] [--out 文件.csv] [--codec 名字] [--byte-model 名字] [--byte-noise 名字] [--layout 2|2-loss-first] <世界文件或目录>...`
//! - `--codec`、`--byte-model`、`--byte-noise`：把读进来的每个世界换一档再跑（世界文件本身不改）。三档对账时用同一批
//!   L1 的世界文件跑出 L0.5、L0 的各种配置；`session` 列是不随这三样变的会话身份，汇总脚本（`scripts/a1-reconcile.py`）靠它对上。
//! - `--layout`：把布局 1 的世界按 `NetworkPlan::to_layout2` 换成布局 2 再跑（A2-3 的对照）；`2-loss-first` 把丢包放回瓶颈之前。
//!   换了档的世界哈希会变，`source_hash` 列是读进来的文件的哈希，布局 1 和布局 2 的两份 CSV 按它配对。
//! - 目录：取其中所有 `*.world`（不递归），按文件名排序；参数里的顺序保留。
//! - 输出按输入顺序，不按完成顺序（同一批输入每次输出的行顺序相同）。默认写标准输出，`--out` 写文件。
//! - `--jobs`：并行的线程数，默认是机器的核数。
//! - 某个世界出错（读不出来、哈希对不上、仿真失败）：那一行的 `error` 列写原因，其余列空，最后退出码非 0。
//!
//! 为什么用线程而不是多进程：`run()` 没有可变的全局状态（静态变量只有三处：RS 纠错的查表，进程内只初始化一次；
//! 节奏器倍数的环境变量，只读；操作时间线的缓存，有锁，只影响速度、不影响结果），而且 `cargo test` 本来就在一个进程里
//! 并行跑着几十个会话，指纹和单个跑的一致；线程省掉进程启动和结果回传。
//!
//! 墙上耗时（`wall_ms`）是这个会话在它那条线程上用掉的时间；并行时核被别的会话抢占，会偏大。量速度用 `--jobs 1`。
//!
//! 列：见 [`HEADER`]。`fingerprint` 和 `PEERDESK_E2E_FINGERPRINTS` 里每行最后那个数相同（那个文件还会记场景的身份）。
//! `rate_p50_kbps`、`rate_p5_kbps` 按反馈的条数算（偏向有动静的时段，留着和以前的数对照）；
//! `rate_time_p50_kbps`、`rate_time_p5_kbps` 按时间加权。`window_bytes` 是每两秒窗口发出的字节，分号隔开。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use peerdesk_core::estimator::{ProbeVerdict, RateState};
use peerdesk_e2e::world::{World, EXTENSION};
use peerdesk_e2e::{fingerprint, Action, ByteModelKind, ByteNoise, CodecKind, Outcome};

fn header() -> String {
    let mut cols: Vec<String> = [
        "world_hash",
        "session",
        "name",
        "codec",
        "byte_model",
        "sim_s",
        "wall_ms",
        "frames",
        "keyframes",
        "freezes",
        "max_wait_ms",
        "first_picture_ms",
        "skipped",
        "bad",
        "recoveries",
        "retransmitted",
        "sent_bytes",
        "net_lost",
        "net_queue_dropped",
        "rate_p50_kbps",
        "rate_p5_kbps",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    cols.extend(
        RateState::ALL
            .iter()
            .map(|s| format!("state_{}_permille", s.name())),
    );
    cols.extend(
        [
            "probe_run_sent",
            "probe_verify_rounds",
            "probe_periodic_rounds",
        ]
        .map(str::to_owned),
    );
    cols.extend(
        ProbeVerdict::ALL
            .iter()
            .map(|v| format!("probe_run_{}", v.name())),
    );
    cols.extend(
        [
            "probe_idle_sent",
            "probe_bytes",
            "sharp_ms",
            // 对账用（A1 第五步加的）：场景的身份三样、Outcome::tally 的六样
            "seed",
            "estimator",
            "byte_noise",
            "rate_time_p50_kbps",
            "rate_time_p5_kbps",
            "rate_cuts",
            "rate_deep_cuts",
            "fps_changes",
            "window_bytes",
            "fingerprint",
            // A2-3 加的：随机数布局，读进来的文件的哈希（换档之前）
            "rng_layout",
            "source_hash",
            "error",
        ]
        .map(str::to_owned),
    );
    cols.join(",")
}

/// CSV 字段：含逗号、引号、换行时加引号。
fn field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// 仿真的总时长（微秒）：每个阶段每帧 `1_000_000 / fps`，和 `run()` 里的步长一样。
fn sim_us(world: &World) -> u64 {
    let frames: u64 = world
        .scenario
        .phases
        .iter()
        .map(|p| u64::from(p.frames))
        .sum();
    frames.saturating_mul(1_000_000 / u64::from(world.scenario.fps.max(1)))
}

fn rate_percentiles(o: &Outcome) -> (u64, u64) {
    let mut rates: Vec<u64> = o.features.iter().map(|f| f.rate_bps / 1000).collect();
    rates.sort_unstable();
    let at = |permille: usize| {
        rates
            .get(rates.len().saturating_mul(permille) / 1000)
            .copied()
            .unwrap_or(0)
    };
    (at(500), at(50))
}

fn row(world: &World, o: &Outcome, wall_ms: f64, source: &str) -> String {
    let (p50, p5) = rate_percentiles(o);
    let sharp: Vec<String> = o
        .phases
        .iter()
        .filter(|p| p.action == Action::Idle)
        // 每个停下阶段一项，没变清晰的写 `-`：两份 CSV 才能按阶段对齐（汇总脚本求和时本来就跳过 `-`）
        .map(|p| p.sharp_ms.map_or("-".to_owned(), |ms| format!("{ms:.0}")))
        .collect();
    let mut cols = vec![
        world.hash.clone(),
        peerdesk_e2e::world::session_key(&world.scenario).unwrap_or_default(),
        field(&world.scenario.name),
        o.codec.name().to_owned(),
        world.scenario.byte_model.name().to_owned(),
        format!("{:.3}", sim_us(world) as f64 / 1e6),
        format!("{wall_ms:.1}"),
        o.frames.to_string(),
        o.keyframes.to_string(),
        o.freezes.to_string(),
        format!("{:.1}", o.max_wait_ms),
        o.first_picture_ms
            .map_or("-".to_owned(), |v| format!("{v:.1}")),
        o.skipped.to_string(),
        o.bad.to_string(),
        o.recoveries.to_string(),
        o.retransmitted.to_string(),
        o.sent_bytes.to_string(),
        o.net.lost.to_string(),
        o.net.queue_dropped.to_string(),
        p50.to_string(),
        p5.to_string(),
    ];
    cols.extend(
        RateState::ALL
            .iter()
            .map(|s| o.states.share_permille(*s).to_string()),
    );
    cols.extend([
        o.probes.run_sent.to_string(),
        o.probes.verify_rounds.to_string(),
        o.probes.periodic_rounds.to_string(),
    ]);
    cols.extend(ProbeVerdict::ALL.iter().map(|v| {
        o.probes
            .run_verdicts
            .get(v.index())
            .copied()
            .unwrap_or(0)
            .to_string()
    }));
    cols.extend([
        o.probes.idle_sent.to_string(),
        o.probes.bytes().to_string(),
        if sharp.is_empty() {
            "-".to_owned()
        } else {
            sharp.join(";")
        },
        world.scenario.seed.to_string(),
        o.estimator.to_owned(),
        world.scenario.byte_noise.name().to_owned(),
        (o.tally.rate_time_p50_bps / 1000).to_string(),
        (o.tally.rate_time_p5_bps / 1000).to_string(),
        o.tally.rate_cuts.to_string(),
        o.tally.rate_deep_cuts.to_string(),
        o.tally.fps_changes.to_string(),
        // 每个两秒窗口发出的字节，分号隔开
        o.tally
            .window_bytes
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(";"),
        format!("{:016x}", fingerprint(o)),
        world.scenario.network.layout().to_string(),
        source.to_owned(),
        String::new(),
    ]);
    cols.join(",")
}

/// 出错的一行：哈希（读得出来的话）、名字（文件名）、错误，其余列空。
fn error_row(label: &str, why: &str) -> String {
    let blanks = header().matches(',').count().saturating_sub(1);
    format!(
        "{},{}{}",
        field(label),
        ",".repeat(blanks),
        field(&why.replace('\n', " "))
    )
}

fn run_one(path: &Path, tier: &Tier) -> Result<String, String> {
    let world = World::load(path).map_err(|e| e.to_string())?;
    let source = world.hash.clone();
    let world = if tier.is_set() {
        world
            .retiered(tier.codec, tier.byte_model, tier.byte_noise)
            .map_err(|e| format!("{}：换档失败：{e}", world.hash))?
    } else {
        world
    };
    let world = match tier.layout2 {
        Some(loss_first) => world
            .with_layout2(loss_first)
            .map_err(|e| format!("{}：换布局失败：{e}", world.hash))?,
        None => world,
    };
    let started = Instant::now();
    let outcome = world.run().map_err(|e| format!("{}：{e}", world.hash))?;
    Ok(row(
        &world,
        &outcome,
        started.elapsed().as_secs_f64() * 1000.0,
        &source,
    ))
}

fn collect(args: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for arg in args {
        if arg.is_dir() {
            let mut inside: Vec<PathBuf> = std::fs::read_dir(arg)
                .map_err(|e| format!("读目录 {} 失败：{e}", arg.display()))?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == EXTENSION))
                .collect();
            inside.sort();
            files.extend(inside);
        } else {
            files.push(arg.clone());
        }
    }
    Ok(files)
}

/// 把读进来的每个世界换一档再跑（对账用）：给了哪一样就换哪一样。
#[derive(Clone, Copy, Default)]
struct Tier {
    codec: Option<CodecKind>,
    byte_model: Option<ByteModelKind>,
    byte_noise: Option<ByteNoise>,
    /// 换成布局 2：`Some(丢包在瓶颈之前)`。
    layout2: Option<bool>,
}

impl Tier {
    const fn is_set(&self) -> bool {
        self.codec.is_some() || self.byte_model.is_some() || self.byte_noise.is_some()
    }
}

struct Options {
    jobs: usize,
    out: Option<PathBuf>,
    inputs: Vec<PathBuf>,
    tier: Tier,
}

fn parse_args() -> Result<Options, String> {
    let mut jobs = std::thread::available_parallelism().map_or(1, usize::from);
    let (mut out, mut inputs) = (None, Vec::new());
    let mut tier = Tier::default();
    let named = |what: &str, v: Option<String>| v.ok_or(format!("{what} 后面要跟名字"));
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--jobs" => {
                let v = args.next().ok_or("--jobs 后面要跟线程数")?;
                jobs = v
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or(format!("--jobs {v:?} 不对，应当是正整数"))?;
            }
            "--out" => out = Some(PathBuf::from(args.next().ok_or("--out 后面要跟文件名")?)),
            "--codec" => {
                let v = named("--codec", args.next())?;
                tier.codec = Some(peerdesk_e2e::world::codec_named(&v).ok_or(format!(
                    "--codec {v:?} 不认识（tile、h264、h264-l05、h264-l0）"
                ))?);
            }
            "--byte-model" => {
                let v = named("--byte-model", args.next())?;
                tier.byte_model = Some(
                    ByteModelKind::ALL
                        .iter()
                        .find(|(n, _)| *n == v)
                        .map(|(_, m)| *m)
                        .ok_or(format!("--byte-model {v:?} 不认识"))?,
                );
            }
            "--byte-noise" => {
                let v = named("--byte-noise", args.next())?;
                tier.byte_noise = Some(
                    ByteNoise::ALL
                        .iter()
                        .find(|(n, _)| *n == v)
                        .map(|(_, m)| *m)
                        .ok_or(format!("--byte-noise {v:?} 不认识（default、within、off）"))?,
                );
            }
            "--layout" => {
                let v = named("--layout", args.next())?;
                tier.layout2 = Some(match v.as_str() {
                    "2" => false,
                    "2-loss-first" => true,
                    _ => return Err(format!("--layout {v:?} 不认识（2、2-loss-first）")),
                });
            }
            "--help" | "-h" => return Err(String::new()),
            flag if flag.starts_with("--") => return Err(format!("不认识的参数 {flag}")),
            path => inputs.push(PathBuf::from(path)),
        }
    }
    if inputs.is_empty() {
        return Err("没有给世界文件或目录".to_owned());
    }
    Ok(Options {
        jobs,
        out,
        inputs,
        tier,
    })
}

/// 按输入顺序往外写：第 i 个会话的结果先放进格子，从头起连续到齐的行写出去。
struct Ordered {
    slots: Vec<Option<String>>,
    printed: usize,
    sink: Box<dyn Write + Send>,
}

impl Ordered {
    fn put(&mut self, i: usize, line: String) {
        if let Some(slot) = self.slots.get_mut(i) {
            *slot = Some(line);
        }
        while let Some(Some(line)) = self.slots.get(self.printed) {
            let _ = writeln!(self.sink, "{line}");
            self.printed += 1;
        }
    }
}

fn main() -> ExitCode {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(why) => {
            if !why.is_empty() {
                let _ = writeln!(std::io::stderr(), "{why}");
            }
            let _ = writeln!(
                std::io::stderr(),
                "用法：batch [--jobs N] [--out 文件.csv] [--codec 名字] [--byte-model 名字] [--byte-noise 名字] [--layout 2|2-loss-first] <世界文件或目录>...\n\
                 后四个参数把读进来的每个世界换一档再跑（对账用），世界文件本身不改"
            );
            return ExitCode::from(2);
        }
    };
    let files = match collect(&opts.inputs) {
        Ok(f) => f,
        Err(why) => {
            let _ = writeln!(std::io::stderr(), "{why}");
            return ExitCode::FAILURE;
        }
    };
    let mut sink: Box<dyn Write + Send> = match &opts.out {
        Some(p) => match std::fs::File::create(p) {
            Ok(f) => Box::new(std::io::BufWriter::new(f)),
            Err(e) => {
                let _ = writeln!(std::io::stderr(), "建文件 {} 失败：{e}", p.display());
                return ExitCode::FAILURE;
            }
        },
        None => Box::new(std::io::stdout()),
    };
    let _ = writeln!(sink, "{}", header());

    // 结果按输入顺序输出：完成的先放着，前面的都到齐了再往外写
    let next = AtomicUsize::new(0);
    let ordered = Mutex::new(Ordered {
        slots: vec![None; files.len()],
        printed: 0,
        sink,
    });
    let failed = AtomicUsize::new(0);
    let tier = opts.tier;
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..opts.jobs.min(files.len().max(1)) {
            let _ = scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(path) = files.get(i) else { break };
                let line = match run_one(path, &tier) {
                    Ok(line) => line,
                    Err(why) => {
                        let _ = failed.fetch_add(1, Ordering::Relaxed);
                        error_row(&path.display().to_string(), &why)
                    }
                };
                ordered
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .put(i, line);
            });
        }
    });
    if let Ok(mut guard) = ordered.lock() {
        let _ = guard.sink.flush();
    }
    let failed = failed.load(Ordering::Relaxed);
    let _ = writeln!(
        std::io::stderr(),
        "{} 个世界，{} 个失败，{} 个线程，墙上 {:.1} 秒",
        files.len(),
        failed,
        opts.jobs,
        started.elapsed().as_secs_f64()
    );
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
