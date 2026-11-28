//! 世界文件里布局 2 的网络（`rng_layout 2`，见 `a2-plan.md` 第 6、8 节）。
//!
//! 键（`<cfg>` 是一个方向的全部层参数，一行，见 [`dir_fields`]）：
//! - `path.fwd <cfg>`、`path.rev <cfg>`：起始的参数，正向（被控端 → 控制端，画面走的方向）和反向；
//! - `steps N`，每一项 `step.<i> at=<微秒> up=0|1`，加 `step.<i>.fwd <cfg>`、`step.<i>.rev <cfg>`：
//!   从那一刻起立即换参数（和布局 1 的换链路一样，不发事件）；
//! - `routes N`，每一项 `route.<i> at=<微秒> down_us=<微秒> new=0|1`，`new=1` 时加 `route.<i>.fwd`、`route.<i>.rev`：
//!   按计划中断和换路，带新参数的在恢复时刻发一次“路径变了”；
//! - `cross N`，每一项 `cross.<i> dir=fwd|rev kind=cbr|tcp|bursts ...`：交叉流量源。
//!
//! 分段路径（`peerdesk_core::path::segments`，五段，每段的正向是 A → B）用 `seg` 开头的另一套键，和上面互斥：
//! - `seg.<段名>.fwd <cfg>`、`seg.<段名>.rev <cfg>`：五段起始的参数（段名 `a-lan`、`a-egress`、`public`、`b-egress`、`b-lan`）；
//! - `seg_steps N`，每一项 `seg_step.<i> at=<微秒> seg=<段名> up=0|1`，加 `seg_step.<i>.fwd`、`seg_step.<i>.rev`；
//! - `seg_routes N`，每一项 `seg_route.<i> at=<微秒> down_us=<微秒> down=<段名,…|-> new=<段名,…|->`，
//!   `new` 里的每段加 `seg_route.<i>.<段名>.fwd`、`seg_route.<i>.<段名>.rev`；
//! - `seg_cross N`，每一项 `seg_cross.<i> seg=<段名> dir=fwd|rev kind=…`（其余字段和 `cross.<i>` 一样）。
//!
//! 写的时候每个结构都整个解构：以后加了字段，这里编译不过，逼着把它写进文件。
//! 容量轨迹原样内联（`时长@速率`，逗号隔开），不在这里合并：合并相邻同速率的段在语义上等价，但瓶颈按段算发送时间，
//! 合并可能差 1 纳秒，同一个世界就有了两种写法和两种结果。要合并在生成轨迹的地方做。

use super::{bad, opt_fields, parse_opt_fields, WorldError};
use crate::{Layout2, PathStep, SegmentStep, SegmentedPlan};
use peerdesk_core::kv::{join_fields, Kv, KvWriter};
use peerdesk_core::path::segments::{Segment, SegmentRoute, SegmentedConfig};
use peerdesk_core::path::{
    Aqm, BottleneckConfig, Capacity, CrossSpec, Delay, Dir, DirConfig, Loss, Noise, PathConfig,
    Policer, QueueLimit, Receiver, RouteChange, TcpConfig, Trace,
};
use peerdesk_core::sim::{Busy, Doze, Spikes};
use peerdesk_core::time::Time;

/// 布局 2 才有的键（出现在布局 1 的文件里要报错）。
pub(super) const KEYS: [&str; 9] = [
    "path.fwd",
    "path.rev",
    "steps",
    "routes",
    "cross",
    "seg.a-lan.fwd",
    "seg_steps",
    "seg_routes",
    "seg_cross",
];

/// 分段路径的键（出现了就按分段读）。
pub(super) const SEGMENTED_KEY: &str = "seg.a-lan.fwd";

const SEGMENT_NAMES: [(&str, Segment); 5] = [
    ("a-lan", Segment::ALan),
    ("a-egress", Segment::AEgress),
    ("public", Segment::Public),
    ("b-egress", Segment::BEgress),
    ("b-lan", Segment::BLan),
];

fn segment_list(v: &[Segment]) -> String {
    if v.is_empty() {
        return "-".to_owned();
    }
    v.iter().map(|s| s.name()).collect::<Vec<_>>().join(",")
}

fn parse_segment(name: &str, ctx: &str) -> Result<Segment, WorldError> {
    SEGMENT_NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| *s)
        .map_or_else(
            || {
                bad(format!(
                    "{ctx}：段名 {name:?} 不认识（a-lan、a-egress、public、b-egress、b-lan）"
                ))
            },
            Ok,
        )
}

fn parse_segment_list(text: &str, ctx: &str) -> Result<Vec<Segment>, WorldError> {
    if text == "-" {
        return Ok(Vec::new());
    }
    text.split(',').map(|n| parse_segment(n, ctx)).collect()
}

fn flag(b: bool) -> String {
    if b { "1" } else { "0" }.to_owned()
}

fn opt(v: Option<u64>) -> String {
    v.map_or("-".to_owned(), |n| n.to_string())
}

/// 冒号隔开的 `n` 个非负整数。
fn nums(text: &str, n: usize, ctx: &str, what: &str) -> Result<Vec<u64>, WorldError> {
    let v: Result<Vec<u64>, _> = text.split(':').map(str::parse::<u64>).collect();
    match v {
        Ok(v) if v.len() == n => Ok(v),
        _ => bad(format!(
            "{ctx}：{what} 的值 {text:?} 不对，应当是 {n} 个用冒号隔开的非负整数"
        )),
    }
}

/// `a@b,a@b,...`，`-` 是空的。
fn pairs(text: &str, ctx: &str, what: &str) -> Result<Vec<(u64, u64)>, WorldError> {
    if text == "-" {
        return Ok(Vec::new());
    }
    text.split(',')
        .map(|p| {
            let parsed = p
                .split_once('@')
                .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)));
            parsed.map_or_else(
                || {
                    bad(format!(
                        "{ctx}：{what} 里的 {p:?} 不对，应当是 非负整数@非负整数"
                    ))
                },
                Ok,
            )
        })
        .collect()
}

fn write_pairs(v: &[(u64, u64)]) -> String {
    if v.is_empty() {
        return "-".to_owned();
    }
    v.iter()
        .map(|(a, b)| format!("{a}@{b}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// 一组数按顺序取出来（个数已经由 [`nums`] 核对过）。
fn take(it: &mut impl Iterator<Item = u64>) -> u64 {
    it.next().unwrap_or(0)
}

fn capacity_text(c: &Capacity) -> String {
    match c {
        Capacity::Unlimited => "unlimited".to_owned(),
        Capacity::Constant { rate_bps } => format!("const:{rate_bps}"),
        Capacity::Poisson {
            rate_bps,
            opportunity_bytes,
        } => format!("poisson:{rate_bps}:{opportunity_bytes}"),
        Capacity::Trace(Trace { segments, repeat }) => format!(
            "trace:{}:{}",
            if *repeat { "repeat" } else { "once" },
            write_pairs(segments)
        ),
    }
}

fn parse_capacity(text: &str, ctx: &str) -> Result<Capacity, WorldError> {
    let (kind, rest) = text.split_once(':').unwrap_or((text, ""));
    match kind {
        "unlimited" if rest.is_empty() => Ok(Capacity::Unlimited),
        "const" => {
            let v = nums(rest, 1, ctx, "capacity")?;
            Ok(Capacity::Constant {
                rate_bps: take(&mut v.into_iter()),
            })
        }
        "poisson" => {
            let mut it = nums(rest, 2, ctx, "capacity")?.into_iter();
            Ok(Capacity::Poisson {
                rate_bps: take(&mut it),
                opportunity_bytes: take(&mut it),
            })
        }
        "trace" => {
            let (mode, segs) = rest.split_once(':').unwrap_or((rest, ""));
            let repeat = match mode {
                "repeat" => true,
                "once" => false,
                _ => {
                    return bad(format!(
                        "{ctx}：capacity 的轨迹要写 trace:once:… 或 trace:repeat:…，不是 {text:?}"
                    ))
                }
            };
            Ok(Capacity::Trace(Trace {
                segments: pairs(segs, ctx, "capacity 的轨迹")?,
                repeat,
            }))
        }
        _ => bad(format!(
            "{ctx}：capacity 的值 {text:?} 不认识（unlimited、const:速率、poisson:速率:字节、trace:once|repeat:时长@速率,…）"
        )),
    }
}

fn aqm_text(a: Aqm) -> String {
    match a {
        Aqm::TailDrop => "tail".to_owned(),
        Aqm::CoDel {
            target_us,
            interval_us,
        } => format!("codel:{target_us}:{interval_us}"),
    }
}

fn parse_aqm(text: &str, ctx: &str) -> Result<Aqm, WorldError> {
    match text.split_once(':') {
        None if text == "tail" => Ok(Aqm::TailDrop),
        Some(("codel", rest)) => {
            let mut it = nums(rest, 2, ctx, "aqm")?.into_iter();
            Ok(Aqm::CoDel {
                target_us: take(&mut it),
                interval_us: take(&mut it),
            })
        }
        _ => bad(format!(
            "{ctx}：aqm 的值 {text:?} 不认识（tail 或 codel:目标微秒:窗口微秒）"
        )),
    }
}

fn loss_text(l: Loss) -> String {
    match l {
        Loss::None => "none".to_owned(),
        Loss::Independent { ppm } => format!("ind:{ppm}"),
        Loss::Burst {
            loss_ppm,
            avg_burst_milli,
        } => format!("burst:{loss_ppm}:{avg_burst_milli}"),
    }
}

fn u32_of(v: u64, ctx: &str, what: &str) -> Result<u32, WorldError> {
    u32::try_from(v).or_else(|_| bad(format!("{ctx}：{what} 的值 {v} 超出范围")))
}

fn parse_loss(text: &str, ctx: &str) -> Result<Loss, WorldError> {
    match text.split_once(':') {
        None if text == "none" => Ok(Loss::None),
        Some(("ind", rest)) => {
            let v = nums(rest, 1, ctx, "loss")?;
            Ok(Loss::Independent {
                ppm: u32_of(take(&mut v.into_iter()), ctx, "loss")?,
            })
        }
        Some(("burst", rest)) => {
            let mut it = nums(rest, 2, ctx, "loss")?.into_iter();
            Ok(Loss::Burst {
                loss_ppm: u32_of(take(&mut it), ctx, "loss")?,
                avg_burst_milli: u32_of(take(&mut it), ctx, "loss")?,
            })
        }
        _ => bad(format!(
            "{ctx}：loss 的值 {text:?} 不认识（none、ind:百万分比、burst:百万分比:千分之一包）"
        )),
    }
}

fn delay_text(d: &Delay) -> String {
    match d {
        Delay::Fixed(us) => format!("fixed:{us}"),
        Delay::Steps { initial_us, steps } => {
            let v: Vec<(u64, u64)> = steps.iter().map(|(t, us)| (t.micros(), *us)).collect();
            format!("steps:{initial_us}:{}", write_pairs(&v))
        }
        Delay::Periodic {
            initial_us,
            period_us,
            phase_us,
            low_us,
            high_us,
        } => format!("periodic:{initial_us}:{period_us}:{phase_us}:{low_us}:{high_us}"),
    }
}

fn parse_delay(text: &str, ctx: &str) -> Result<Delay, WorldError> {
    let (kind, rest) = text.split_once(':').unwrap_or((text, ""));
    match kind {
        "fixed" => {
            let v = nums(rest, 1, ctx, "delay")?;
            Ok(Delay::Fixed(take(&mut v.into_iter())))
        }
        "steps" => {
            let (initial, steps) = rest.split_once(':').unwrap_or((rest, ""));
            let initial_us = nums(initial, 1, ctx, "delay")?;
            Ok(Delay::Steps {
                initial_us: take(&mut initial_us.into_iter()),
                steps: pairs(steps, ctx, "delay 的阶跃")?
                    .into_iter()
                    .map(|(t, us)| (Time(t), us))
                    .collect(),
            })
        }
        "periodic" => {
            let mut it = nums(rest, 5, ctx, "delay")?.into_iter();
            Ok(Delay::Periodic {
                initial_us: take(&mut it),
                period_us: take(&mut it),
                phase_us: take(&mut it),
                low_us: take(&mut it),
                high_us: take(&mut it),
            })
        }
        _ => bad(format!(
            "{ctx}：delay 的值 {text:?} 不认识（fixed:微秒、steps:起始微秒:时刻@微秒,…、periodic:起始:周期:相位:下限:上限）"
        )),
    }
}

/// 一个方向的全部层参数写成一行。
fn dir_fields(c: &DirConfig) -> Result<String, WorldError> {
    let DirConfig {
        policer,
        bottleneck,
        loss,
        delay,
        noise,
        receiver,
        loss_before_bottleneck,
    } = c;
    let BottleneckConfig {
        capacity,
        limit,
        aqm,
    } = bottleneck;
    let QueueLimit {
        max_delay_us,
        max_bytes,
    } = limit;
    let Noise {
        jitter_us,
        fifo_jitter,
        spikes,
        doze,
    } = noise;
    let Receiver { busy, batch_us } = receiver;
    Ok(join_fields(&[
        (
            "policer",
            policer.map_or("-".to_owned(), |p| opt_fields(&[p.rate_bps, p.burst_bytes])),
        ),
        ("capacity", capacity_text(capacity)),
        ("queue_delay_us", opt(*max_delay_us)),
        ("queue_bytes", opt(*max_bytes)),
        ("aqm", aqm_text(*aqm)),
        ("loss", loss_text(*loss)),
        ("loss_first", flag(*loss_before_bottleneck)),
        ("delay", delay_text(delay)),
        ("jitter_us", jitter_us.to_string()),
        ("fifo_jitter", flag(*fifo_jitter)),
        (
            "spikes",
            spikes.map_or("-".to_owned(), |s| {
                opt_fields(&[s.every_us, s.length_us, s.max_us])
            }),
        ),
        (
            "doze",
            doze.map_or("-".to_owned(), |d| opt_fields(&[d.idle_us, d.wake_us])),
        ),
        (
            "busy",
            busy.map_or("-".to_owned(), |b| opt_fields(&[b.every_us, b.length_us])),
        ),
        ("batch_us", batch_us.to_string()),
    ])?)
}

fn parse_dir(text: &str, ctx: &str) -> Result<DirConfig, WorldError> {
    let mut f = Kv::parse_fields(text, ctx)?;
    let policer = parse_opt_fields(&mut f, "policer", 2, ctx)?.map(|v| {
        let mut it = v.into_iter();
        Policer {
            rate_bps: take(&mut it),
            burst_bytes: take(&mut it),
        }
    });
    let capacity = parse_capacity(&f.take("capacity")?, ctx)?;
    let max_delay_us = f.take_opt_u64("queue_delay_us")?;
    let max_bytes = f.take_opt_u64("queue_bytes")?;
    let aqm = parse_aqm(&f.take("aqm")?, ctx)?;
    let loss = parse_loss(&f.take("loss")?, ctx)?;
    let loss_before_bottleneck = f.take_bool("loss_first")?;
    let delay = parse_delay(&f.take("delay")?, ctx)?;
    let jitter_us = f.take_u64("jitter_us")?;
    let fifo_jitter = f.take_bool("fifo_jitter")?;
    let spikes = parse_opt_fields(&mut f, "spikes", 3, ctx)?.map(|v| {
        let mut it = v.into_iter();
        Spikes {
            every_us: take(&mut it),
            length_us: take(&mut it),
            max_us: take(&mut it),
        }
    });
    let doze = parse_opt_fields(&mut f, "doze", 2, ctx)?.map(|v| {
        let mut it = v.into_iter();
        Doze {
            idle_us: take(&mut it),
            wake_us: take(&mut it),
        }
    });
    let busy = parse_opt_fields(&mut f, "busy", 2, ctx)?.map(|v| {
        let mut it = v.into_iter();
        Busy {
            every_us: take(&mut it),
            length_us: take(&mut it),
        }
    });
    let batch_us = f.take_u64("batch_us")?;
    f.finish()?;
    Ok(DirConfig {
        policer,
        bottleneck: BottleneckConfig {
            capacity,
            limit: QueueLimit {
                max_delay_us,
                max_bytes,
            },
            aqm,
        },
        loss,
        delay,
        noise: Noise {
            jitter_us,
            fifo_jitter,
            spikes,
            doze,
        },
        receiver: Receiver { busy, batch_us },
        loss_before_bottleneck,
    })
}

fn put_config(w: &mut KvWriter, prefix: &str, c: &PathConfig) -> Result<(), WorldError> {
    let PathConfig { forward, reverse } = c;
    w.put(&format!("{prefix}.fwd"), &dir_fields(forward)?)?;
    w.put(&format!("{prefix}.rev"), &dir_fields(reverse)?)?;
    Ok(())
}

fn take_config(kv: &mut Kv, prefix: &str) -> Result<PathConfig, WorldError> {
    let fwd = format!("{prefix}.fwd");
    let rev = format!("{prefix}.rev");
    Ok(PathConfig {
        forward: parse_dir(&kv.take(&fwd)?, &fwd)?,
        reverse: parse_dir(&kv.take(&rev)?, &rev)?,
    })
}

const DIRS: [(&str, Dir); 2] = [("fwd", Dir::Forward), ("rev", Dir::Reverse)];

fn dir_name(d: Dir) -> String {
    match d {
        Dir::Forward => "fwd",
        Dir::Reverse => "rev",
    }
    .to_owned()
}

fn tcp_fields(c: &TcpConfig) -> Vec<(&'static str, String)> {
    let TcpConfig {
        mss_bytes,
        ack_bytes,
        init_cwnd_segments,
        min_rto_us,
        max_rto_us,
        initial_rto_us,
        granularity_us,
    } = c;
    vec![
        ("mss_bytes", mss_bytes.to_string()),
        ("ack_bytes", ack_bytes.to_string()),
        ("init_cwnd", init_cwnd_segments.to_string()),
        ("min_rto_us", min_rto_us.to_string()),
        ("max_rto_us", max_rto_us.to_string()),
        ("initial_rto_us", initial_rto_us.to_string()),
        ("granularity_us", granularity_us.to_string()),
    ]
}

fn parse_tcp(f: &mut Kv) -> Result<TcpConfig, WorldError> {
    Ok(TcpConfig {
        mss_bytes: f.take_parsed("mss_bytes", "非负整数")?,
        ack_bytes: f.take_parsed("ack_bytes", "非负整数")?,
        init_cwnd_segments: f.take_u64("init_cwnd")?,
        min_rto_us: f.take_u64("min_rto_us")?,
        max_rto_us: f.take_u64("max_rto_us")?,
        initial_rto_us: f.take_u64("initial_rto_us")?,
        granularity_us: f.take_u64("granularity_us")?,
    })
}

fn cross_fields(dir: Dir, spec: &CrossSpec) -> Result<String, WorldError> {
    let mut fields: Vec<(&str, String)> = vec![("dir", dir_name(dir))];
    match spec {
        CrossSpec::Cbr {
            rate_bps,
            packet_bytes,
            start,
            stop,
        } => {
            fields.push(("kind", "cbr".to_owned()));
            fields.push(("rate_bps", rate_bps.to_string()));
            fields.push(("packet_bytes", packet_bytes.to_string()));
            fields.push(("start", start.micros().to_string()));
            fields.push(("stop", opt(stop.map(Time::micros))));
        }
        CrossSpec::Tcp { cfg, start, bytes } => {
            fields.push(("kind", "tcp".to_owned()));
            fields.push(("start", start.micros().to_string()));
            fields.push(("bytes", opt(*bytes)));
            fields.extend(tcp_fields(cfg));
        }
        CrossSpec::Bursts {
            cfg,
            connections,
            min_bytes,
            max_bytes,
            mean_off_us,
            start_on,
            start,
        } => {
            fields.push(("kind", "bursts".to_owned()));
            fields.push(("connections", connections.to_string()));
            fields.push(("min_bytes", min_bytes.to_string()));
            fields.push(("max_bytes", max_bytes.to_string()));
            fields.push(("mean_off_us", mean_off_us.to_string()));
            fields.push(("start_on", flag(*start_on)));
            fields.push(("start", start.micros().to_string()));
            fields.extend(tcp_fields(cfg));
        }
    }
    Ok(join_fields(&fields)?)
}

fn parse_cross(text: &str, ctx: &str) -> Result<(Dir, CrossSpec), WorldError> {
    let mut f = Kv::parse_fields(text, ctx)?;
    let got = parse_cross_fields(&mut f)?;
    f.finish()?;
    Ok(got)
}

fn parse_cross_fields(f: &mut Kv) -> Result<(Dir, CrossSpec), WorldError> {
    const KINDS: [(&str, u8); 3] = [("cbr", 0), ("tcp", 1), ("bursts", 2)];
    let dir = f.take_choice("dir", &DIRS)?;
    let spec = match f.take_choice("kind", &KINDS)? {
        0 => CrossSpec::Cbr {
            rate_bps: f.take_u64("rate_bps")?,
            packet_bytes: f.take_parsed("packet_bytes", "非负整数")?,
            start: Time(f.take_u64("start")?),
            stop: f.take_opt_u64("stop")?.map(Time),
        },
        1 => CrossSpec::Tcp {
            start: Time(f.take_u64("start")?),
            bytes: f.take_opt_u64("bytes")?,
            cfg: parse_tcp(f)?,
        },
        _ => CrossSpec::Bursts {
            connections: f.take_u32("connections")?,
            min_bytes: f.take_u64("min_bytes")?,
            max_bytes: f.take_u64("max_bytes")?,
            mean_off_us: f.take_u64("mean_off_us")?,
            start_on: f.take_bool("start_on")?,
            start: Time(f.take_u64("start")?),
            cfg: parse_tcp(f)?,
        },
    };
    Ok((dir, spec))
}

/// 写布局 2 的网络（键的顺序固定）。
pub(super) fn write(w: &mut KvWriter, plan: &Layout2) -> Result<(), WorldError> {
    let Layout2 {
        start,
        steps,
        routes,
        cross,
    } = plan;
    put_config(w, "path", start)?;
    w.put_display("steps", steps.len())?;
    for (i, (at, PathStep { config, up })) in steps.iter().enumerate() {
        let key = format!("step.{i}");
        w.put(
            &key,
            &join_fields(&[("at", at.to_string()), ("up", flag(*up))])?,
        )?;
        put_config(w, &key, config)?;
    }
    w.put_display("routes", routes.len())?;
    for (i, RouteChange { at, down_us, new }) in routes.iter().enumerate() {
        let key = format!("route.{i}");
        w.put(
            &key,
            &join_fields(&[
                ("at", at.micros().to_string()),
                ("down_us", down_us.to_string()),
                ("new", flag(new.is_some())),
            ])?,
        )?;
        if let Some(cfg) = new {
            put_config(w, &key, cfg)?;
        }
    }
    w.put_display("cross", cross.len())?;
    for (i, (dir, spec)) in cross.iter().enumerate() {
        w.put(&format!("cross.{i}"), &cross_fields(*dir, spec)?)?;
    }
    Ok(())
}

/// 读布局 2 的网络。
pub(super) fn read(kv: &mut Kv) -> Result<Layout2, WorldError> {
    let start = take_config(kv, "path")?;
    let mut steps = Vec::new();
    for i in 0..kv.take_u32("steps")? {
        let key = format!("step.{i}");
        let mut f = Kv::parse_fields(&kv.take(&key)?, &key)?;
        let at = f.take_u64("at")?;
        let up = f.take_bool("up")?;
        f.finish()?;
        steps.push((
            at,
            PathStep {
                config: take_config(kv, &key)?,
                up,
            },
        ));
    }
    let mut routes = Vec::new();
    for i in 0..kv.take_u32("routes")? {
        let key = format!("route.{i}");
        let mut f = Kv::parse_fields(&kv.take(&key)?, &key)?;
        let at = Time(f.take_u64("at")?);
        let down_us = f.take_u64("down_us")?;
        let new = f.take_bool("new")?;
        f.finish()?;
        let new = if new {
            Some(take_config(kv, &key)?)
        } else {
            None
        };
        routes.push(RouteChange { at, down_us, new });
    }
    let mut cross = Vec::new();
    for i in 0..kv.take_u32("cross")? {
        let key = format!("cross.{i}");
        cross.push(parse_cross(&kv.take(&key)?, &key)?);
    }
    Ok(Layout2 {
        start,
        steps,
        routes,
        cross,
    })
}

/// 写分段路径（键的顺序固定）。
pub(super) fn write_segmented(w: &mut KvWriter, plan: &SegmentedPlan) -> Result<(), WorldError> {
    let SegmentedPlan {
        start,
        steps,
        routes,
        cross,
    } = plan;
    let SegmentedConfig { segments } = start;
    for (s, cfg) in Segment::ALL.iter().zip(segments) {
        put_config(w, &format!("seg.{}", s.name()), cfg)?;
    }
    w.put_display("seg_steps", steps.len())?;
    for (
        i,
        (
            at,
            SegmentStep {
                segment,
                config,
                up,
            },
        ),
    ) in steps.iter().enumerate()
    {
        let key = format!("seg_step.{i}");
        w.put(
            &key,
            &join_fields(&[
                ("at", at.to_string()),
                ("seg", segment.name().to_owned()),
                ("up", flag(*up)),
            ])?,
        )?;
        put_config(w, &key, config)?;
    }
    w.put_display("seg_routes", routes.len())?;
    for (
        i,
        SegmentRoute {
            at,
            down_us,
            down,
            new,
        },
    ) in routes.iter().enumerate()
    {
        let key = format!("seg_route.{i}");
        let names: Vec<Segment> = new.iter().map(|(s, _)| *s).collect();
        w.put(
            &key,
            &join_fields(&[
                ("at", at.micros().to_string()),
                ("down_us", down_us.to_string()),
                ("down", segment_list(down)),
                ("new", segment_list(&names)),
            ])?,
        )?;
        for (s, cfg) in new {
            put_config(w, &format!("{key}.{}", s.name()), cfg)?;
        }
    }
    w.put_display("seg_cross", cross.len())?;
    for (i, (segment, dir, spec)) in cross.iter().enumerate() {
        let fields = cross_fields(*dir, spec)?;
        w.put(
            &format!("seg_cross.{i}"),
            &format!("seg={} {fields}", segment.name()),
        )?;
    }
    Ok(())
}

/// 读分段路径。
pub(super) fn read_segmented(kv: &mut Kv) -> Result<SegmentedPlan, WorldError> {
    let mut start = SegmentedConfig::default();
    for s in Segment::ALL {
        *start.segment_mut(s) = take_config(kv, &format!("seg.{}", s.name()))?;
    }
    let mut steps = Vec::new();
    for i in 0..kv.take_u32("seg_steps")? {
        let key = format!("seg_step.{i}");
        let mut f = Kv::parse_fields(&kv.take(&key)?, &key)?;
        let at = f.take_u64("at")?;
        let segment = parse_segment(&f.take("seg")?, &key)?;
        let up = f.take_bool("up")?;
        f.finish()?;
        steps.push((
            at,
            SegmentStep {
                segment,
                config: take_config(kv, &key)?,
                up,
            },
        ));
    }
    let mut routes = Vec::new();
    for i in 0..kv.take_u32("seg_routes")? {
        let key = format!("seg_route.{i}");
        let mut f = Kv::parse_fields(&kv.take(&key)?, &key)?;
        let at = Time(f.take_u64("at")?);
        let down_us = f.take_u64("down_us")?;
        let down = parse_segment_list(&f.take("down")?, &key)?;
        let names = parse_segment_list(&f.take("new")?, &key)?;
        f.finish()?;
        let mut new = Vec::new();
        for s in names {
            new.push((s, take_config(kv, &format!("{key}.{}", s.name()))?));
        }
        routes.push(SegmentRoute {
            at,
            down_us,
            down,
            new,
        });
    }
    let mut cross = Vec::new();
    for i in 0..kv.take_u32("seg_cross")? {
        let key = format!("seg_cross.{i}");
        let mut f = Kv::parse_fields(&kv.take(&key)?, &key)?;
        let segment = parse_segment(&f.take("seg")?, &key)?;
        let (dir, spec) = parse_cross_fields(&mut f)?;
        f.finish()?;
        cross.push((segment, dir, spec));
    }
    Ok(SegmentedPlan {
        start,
        steps,
        routes,
        cross,
    })
}
