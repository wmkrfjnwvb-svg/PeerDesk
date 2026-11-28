//! 按帧的类别拆字节（S5-3 诊断，只在本地跑，不进 CI）：每个会话用 `run_recorded` 跑一遍，
//! 从会话记录里读出被控端每个编码任务的类别和字节，每个会话输出一行 CSV。
//!
//! 用法：`byclass [--jobs N] [--out 文件.csv] [--codec 名字] [--byte-model 名字] [--byte-noise 名字] <世界文件或目录>...`
//! 参数和 `batch` 相同（换档、目录展开、输出顺序）。
//!
//! 列：
//! - 身份：世界哈希（换档之后的）、会话身份、名字、编码档、字节模型、扰动；
//! - 每个类别（`content` 变化、`refine` 补清晰、`key` 关键帧）：出了帧的个数 `_n`、字节合计 `_bytes`；
//!   `idle_` 开头的是落在“停下”阶段里的那部分（阶段的帧数换成仿真时间，用帧记录的时刻归阶段）；
//! - 没出帧的任务：`dropped`（编码器因预算丢）、`nooutput`（没有要发的）、`unknown`；
//! - 补清晰链：一个“出了帧的非补清晰帧”之后，连续出了帧的补清晰帧算一条链（没出帧的任务跳过，不断链）。
//!   `chains` 条数，`chain_len_p50/p90` 每条链的帧数，`chain_bytes_p50/p90` 每条链的字节，
//!   `step{k}_n`、`step{k}_bytes`：链上第 k 步（k=1..5）、第 6 步及以后（`step6p`）的帧数和字节合计。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use peerdesk_core::record::{FrameClass, FrameOutcome, Recording};
use peerdesk_e2e::world::{World, EXTENSION};
use peerdesk_e2e::{run_recorded, Action, ByteModelKind, ByteNoise, CodecKind};

const STEPS: usize = 6;

fn header() -> String {
    let mut cols: Vec<String> = [
        "world_hash",
        "session",
        "name",
        "codec",
        "byte_model",
        "byte_noise",
        "content_n",
        "content_bytes",
        "refine_n",
        "refine_bytes",
        "key_n",
        "key_bytes",
        "idle_content_n",
        "idle_content_bytes",
        "idle_refine_n",
        "idle_refine_bytes",
        "idle_key_n",
        "idle_key_bytes",
        "dropped",
        "nooutput",
        "unknown",
        "chains",
        "chain_len_p50",
        "chain_len_p90",
        "chain_bytes_p50",
        "chain_bytes_p90",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    for k in 1..=STEPS {
        let tag = if k == STEPS {
            format!("step{k}p")
        } else {
            format!("step{k}")
        };
        cols.push(format!("{tag}_n"));
        cols.push(format!("{tag}_bytes"));
    }
    cols.push("error".to_owned());
    cols.join(",")
}

fn field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

fn pct(sorted: &[u64], permille: usize) -> u64 {
    sorted
        .get(sorted.len().saturating_mul(permille) / 1000)
        .copied()
        .unwrap_or(0)
}

#[derive(Default)]
struct Counts {
    n: [u64; 3],
    bytes: [u64; 3],
    idle_n: [u64; 3],
    idle_bytes: [u64; 3],
    dropped: u64,
    nooutput: u64,
    unknown: u64,
    chain_lens: Vec<u64>,
    chain_bytes: Vec<u64>,
    step_n: [u64; STEPS],
    step_bytes: [u64; STEPS],
}

const fn slot(c: FrameClass) -> usize {
    match c {
        FrameClass::Content => 0,
        FrameClass::Refine => 1,
        FrameClass::Key => 2,
    }
}

/// 阶段的结束时刻（微秒）和是不是“停下”阶段。
fn phase_ends(world: &World) -> Vec<(u64, bool)> {
    let step = 1_000_000 / u64::from(world.scenario.fps.max(1));
    let mut end = 0u64;
    world
        .scenario
        .phases
        .iter()
        .map(|p| {
            end = end.saturating_add(u64::from(p.frames).saturating_mul(step));
            (end, p.action == Action::Idle)
        })
        .collect()
}

fn close(c: &mut Counts, chain: &mut Option<(u64, u64)>) {
    if let Some((n, b)) = chain.take() {
        if n > 0 {
            c.chain_lens.push(n);
            c.chain_bytes.push(b);
        }
    }
}

fn count(world: &World, rec: &Recording) -> Counts {
    let ends = phase_ends(world);
    let mut c = Counts::default();
    // 当前链：(已有的补清晰帧数, 字节)
    let mut chain: Option<(u64, u64)> = None;
    for (at, f) in rec.frames() {
        match f.outcome {
            FrameOutcome::Sent { bytes } => {
                let i = slot(f.class);
                let b = u64::from(bytes);
                let idle = ends
                    .iter()
                    .find(|(end, _)| at <= *end)
                    .is_some_and(|(_, idle)| *idle);
                if let (Some(n), Some(s)) = (c.n.get_mut(i), c.bytes.get_mut(i)) {
                    *n += 1;
                    *s += b;
                }
                if idle {
                    if let (Some(n), Some(s)) = (c.idle_n.get_mut(i), c.idle_bytes.get_mut(i)) {
                        *n += 1;
                        *s += b;
                    }
                }
                if f.class == FrameClass::Refine {
                    let (n, sum) = chain.get_or_insert((0, 0));
                    *n += 1;
                    *sum += b;
                    let k = usize::try_from(*n).unwrap_or(STEPS).clamp(1, STEPS) - 1;
                    if let (Some(sn), Some(sb)) = (c.step_n.get_mut(k), c.step_bytes.get_mut(k)) {
                        *sn += 1;
                        *sb += b;
                    }
                } else {
                    close(&mut c, &mut chain);
                    chain = Some((0, 0));
                }
            }
            FrameOutcome::DroppedByEncoder => c.dropped += 1,
            FrameOutcome::NoOutput => c.nooutput += 1,
            FrameOutcome::NoFrameUnknown => c.unknown += 1,
        }
    }
    close(&mut c, &mut chain);
    c
}

fn row(world: &World, c: &mut Counts) -> String {
    c.chain_lens.sort_unstable();
    c.chain_bytes.sort_unstable();
    let mut cols = vec![
        world.hash.clone(),
        peerdesk_e2e::world::session_key(&world.scenario).unwrap_or_default(),
        field(&world.scenario.name),
        world.scenario.codec.name().to_owned(),
        world.scenario.byte_model.name().to_owned(),
        world.scenario.byte_noise.name().to_owned(),
    ];
    for i in 0..3 {
        cols.push(c.n.get(i).copied().unwrap_or(0).to_string());
        cols.push(c.bytes.get(i).copied().unwrap_or(0).to_string());
    }
    for i in 0..3 {
        cols.push(c.idle_n.get(i).copied().unwrap_or(0).to_string());
        cols.push(c.idle_bytes.get(i).copied().unwrap_or(0).to_string());
    }
    cols.extend([
        c.dropped.to_string(),
        c.nooutput.to_string(),
        c.unknown.to_string(),
        c.chain_lens.len().to_string(),
        pct(&c.chain_lens, 500).to_string(),
        pct(&c.chain_lens, 900).to_string(),
        pct(&c.chain_bytes, 500).to_string(),
        pct(&c.chain_bytes, 900).to_string(),
    ]);
    for k in 0..STEPS {
        cols.push(c.step_n.get(k).copied().unwrap_or(0).to_string());
        cols.push(c.step_bytes.get(k).copied().unwrap_or(0).to_string());
    }
    cols.push(String::new());
    cols.join(",")
}

fn error_row(label: &str, why: &str) -> String {
    let blanks = header().matches(',').count().saturating_sub(1);
    format!(
        "{},{}{}",
        field(label),
        ",".repeat(blanks),
        field(&why.replace('\n', " "))
    )
}

#[derive(Clone, Copy, Default)]
struct Tier {
    codec: Option<CodecKind>,
    byte_model: Option<ByteModelKind>,
    byte_noise: Option<ByteNoise>,
}

fn run_one(path: &Path, tier: &Tier) -> Result<String, String> {
    let world = World::load(path).map_err(|e| e.to_string())?;
    let world = world
        .retiered(tier.codec, tier.byte_model, tier.byte_noise)
        .map_err(|e| format!("{}：换档失败：{e}", world.hash))?;
    if let Some(why) = peerdesk_e2e::world::env_conflict(&world.scenario) {
        return Err(why);
    }
    let (_, bytes) = run_recorded(&world.scenario).map_err(|e| format!("{}：{e}", world.hash))?;
    let rec = Recording::parse(&bytes).map_err(|e| format!("{}：记录读不出：{e:?}", world.hash))?;
    let mut c = count(&world, &rec);
    Ok(row(&world, &mut c))
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

struct Ordered {
    slots: Vec<Option<String>>,
    printed: usize,
    sink: Box<dyn Write + Send>,
}

impl Ordered {
    fn put(&mut self, i: usize, line: String) {
        if let Some(s) = self.slots.get_mut(i) {
            *s = Some(line);
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
                "用法：byclass [--jobs N] [--out 文件.csv] [--codec 名字] [--byte-model 名字] [--byte-noise 名字] <世界文件或目录>..."
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
    let next = AtomicUsize::new(0);
    let ordered = Mutex::new(Ordered {
        slots: vec![None; files.len()],
        printed: 0,
        sink,
    });
    let failed = AtomicUsize::new(0);
    let tier = opts.tier;
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
    if let Ok(mut g) = ordered.lock() {
        let _ = g.sink.flush();
    }
    let failed = failed.load(Ordering::Relaxed);
    let _ = writeln!(std::io::stderr(), "{} 个世界，{failed} 个失败", files.len());
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
