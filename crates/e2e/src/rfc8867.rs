//! RFC 8867 的 8 个基础用例和 RFC 8868 的指标（A2-4c，`docs/research/Q-redesign/a2-plan.md` 第 5 节“A2-4 的设计”）。
//!
//! # 用例
//!
//! 参数照原文（RFC 8867 第 4、5 节，rfc-editor.org 的 txt 版本），各用例的差别写在 [`cases`] 里每一个构造函数上。共同的：
//! - 路径（第 4.2 节）：单程 50 ms、丢尾、队列 300 ms、路径丢包 0%、抖动用 RFC 8868 的 NR-BPDV（std 5 ms、±3σ；
//!   8867 写的“最大 30 ms”和 8868 的公式对不上，照 8868，见 `Pdv`）；
//! - 媒体（第 4.3 节）：150 kbps 起步、1.5 Mbps 封顶（产品默认起步是 1 Mbps，这里为了对齐 RFC 改成 150 kbps），
//!   内容是一直播的 `Video` 段（远程桌面里码率最高、最接近“有足够多的数据要发”的真实内容）；
//! - 画面 640×360（[`MEDIA_SIZE`]，主会话 2026-11-28 定）：原文第 4.3 节给的分辨率范围是 144p–720p，对应
//!   150 kbps–1.5 Mbps；这些用例测的是竞争、公平和收敛，不是画质；1.5 Mbps 封顶时 720p 本来就糊；L1 在 720p 上
//!   一路只有 0.5–1 倍实时，12 个种子要 6 小时。分辨率会改变帧大小和分片，可能影响码控，所以 5.1 另在 720p 上跑一遍
//!   对照（例子程序的 `--size 1280x720`），结论写在报告里；
//! - 音频（第 4.3 节“CBR 20 Kbps”）：我们的仿真没有音频编码，每一路音频是一条 20 kbps 的定速交叉流（每秒 50 个 50 字节的包），
//!   挂在共享瓶颈上，不受码控；
//! - 5.8 的“暂停”：远程桌面没有“停发媒体流”，暂停那 20 秒改成 `Idle`（画面静止，产品照常发反馈和静止探测）。
//!
//! 变容量的两种做法（第 4.2 节最后一段）都做：直接改链路容量（[`Capacity::Trace`]），或者 4 Mbps 的物理容量加上
//! (4 − x) Mbps 的 UDP 背景流。
//!
//! # 指标
//!
//! 都按每一对在**接收端**、200 ms 一格算（主会话 2026-11-28：`Outcome::net` 是几对合在一起的，不拿来算公平和收敛）。
//! - 段：容量变化、某一对开始、暂停、恢复、结束，都把时间切开；
//! - 公平份额：这一段画面方向的容量，减去这一段的音频，除以这一段在场的媒体流加长 TCP 的条数，再不超过 1.5 Mbps（媒体的上限）；
//! - 稳定值：这一段最后 10 秒接收速率的均值；
//! - 收敛时间：从段开始，到接收速率（1 秒滑动平均）进入并保持在稳定值 ±20% 以内至少 5 秒的时刻。
//!   - 一直没进入的记“未收敛”，值取这一段的时长；
//!   - 段短于 15 秒（不够 10 秒稳定值加 5 秒保持）记“段太短”。
//!   - 用 1 秒滑动平均而不是 200 ms 一格：画面一帧一帧地发，200 ms 一格只有 6 帧，本身就跳 ±50% 以上（原文第 4.3 节
//!     也是“bit rate variation should be calculated over a one (1) second period”）；
//! - 稳定值 ÷ 公平份额：避免“收敛到一个错误的低值也算快”；
//! - 振荡：500 ms 一格，在高水位（公平份额 × 1.25）和低水位（× 0.5）之间来回一次记一次（RFC 8868 的例子是 500 ms 的窗）；
//! - 公平：同一段里同方向的各对，稳定值的最小 ÷ 最大（RFC 8868 要求同 RTT 的流在 0.333–3 之间，即最小 ÷ 最大不低于 0.333）；
//! - 利用率：200 ms 一格，各对发出之和 ÷ 这一刻的容量，取全程均值；
//! - 包的单程延迟（中位、95%、99%）、接收端丢包、反馈开销（反馈字节 ÷ 画面字节）、冻结、最长等待；
//! - 5.6、5.7 加上 TCP 的吞吐和丢包。

use peerdesk_core::path::{
    Aqm, BottleneckConfig, Capacity, CrossSpec, CrossStats, Delay, Dir, DirConfig, Noise,
    PathConfig, Pdv, QueueLimit, TcpConfig, Trace,
};
use peerdesk_core::time::Time;

use crate::flow::{FlowLog, BIN_US};
use crate::session::{MultiScenario, PairSpec};
use crate::{Action, CodecKind, Layout2, NetworkPlan, Outcome, Phase, Scenario};

/// 媒体码率的上限（RFC 8867 第 4.3 节：150 Kbps – 1.5 Mbps）。
pub const MEDIA_MAX_BPS: u64 = 1_500_000;
/// 媒体起步码率。
pub const MEDIA_START_BPS: u64 = 150_000;
/// 音频（CBR 20 Kbps）。
pub const AUDIO_BPS: u64 = 20_000;
/// 帧率。
const FPS: u32 = 30;
/// 画面尺寸（见模块文档）。
pub const MEDIA_SIZE: (u16, u16) = (640, 360);

/// 一路媒体的时间线（全局时间，微秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaFlow {
    /// 开始。
    pub start_us: u64,
    /// 结束。
    pub end_us: u64,
    /// 暂停的时段 [开始, 结束)。
    pub pause_us: Option<(u64, u64)>,
    /// 画面走反向（5.3 的双向媒体）。
    pub reverse: bool,
    /// 这一路带一路音频。
    pub audio: bool,
}

impl MediaFlow {
    /// `t` 时刻在发媒体。
    #[must_use]
    pub fn active(&self, t: u64) -> bool {
        t >= self.start_us && t < self.end_us && self.pause_us.is_none_or(|(a, b)| t < a || t >= b)
    }
}

/// 一个用例。
#[derive(Debug, Clone)]
pub struct Case {
    /// 名字，例如 `5.1-trace-50ms`。
    pub id: String,
    /// 说明（原文的标题和这一档的参数）。
    pub title: String,
    /// 时长（微秒）。
    pub duration_us: u64,
    /// 共享网络和各对。
    pub multi: MultiScenario,
    /// 各路媒体，和 `multi.pairs` 一一对应。
    pub media: Vec<MediaFlow>,
    /// 正向（节点 0 → 1）对媒体可用的容量随时间的变化：（从这一刻起，比特每秒），已经扣掉 UDP 背景流。
    pub forward_capacity: Vec<(u64, u64)>,
    /// 反向的，同上。
    pub reverse_capacity: Vec<(u64, u64)>,
    /// 交叉流里长 TCP 的序号（在 `Layout2::cross` 里），算公平份额时算一条竞争流。
    pub long_tcp: Vec<usize>,
    /// 交叉流里所有 TCP（长的和短的）的序号，报告 TCP 的吞吐和丢包。
    pub tcp: Vec<usize>,
}

fn secs(s: u64) -> u64 {
    s * 1_000_000
}

/// 一个方向的路径（第 4.2 节）：容量过程、单程 50 ms（或 `delay_us`）、丢尾、队列 `queue_us`、NR-BPDV。
fn dir(capacity: Capacity, delay_us: u64, queue_us: u64) -> DirConfig {
    DirConfig {
        bottleneck: BottleneckConfig {
            capacity,
            limit: QueueLimit::delay(queue_us),
            aqm: Aqm::TailDrop,
        },
        delay: Delay::Fixed(delay_us),
        noise: Noise {
            pdv: Some(Pdv::RFC8868),
            ..Noise::default()
        },
        ..DirConfig::default()
    }
}

/// 容量表（（秒，比例））→ 轨迹，参考容量 `reference_bps`，到 `end_s` 为止。
fn trace(table: &[(u64, u64)], reference_bps: u64, end_s: u64) -> Capacity {
    let mut segments = Vec::new();
    for (i, (t, ratio)) in table.iter().enumerate() {
        let next = table.get(i + 1).map_or(end_s, |(n, _)| *n);
        segments.push((secs(next.saturating_sub(*t)), rate(reference_bps, *ratio)));
    }
    Capacity::Trace(Trace {
        segments,
        repeat: false,
    })
}

/// 参考容量 × 比例（千分数，原文表里的 2.5 写成 2500）。
fn rate(reference_bps: u64, permille: u64) -> u64 {
    reference_bps.saturating_mul(permille) / 1_000
}

/// 容量表 → 对媒体可用的容量随时间的变化。
fn timeline(table: &[(u64, u64)], reference_bps: u64) -> Vec<(u64, u64)> {
    table
        .iter()
        .map(|(t, ratio)| (secs(*t), rate(reference_bps, *ratio)))
        .collect()
}

/// UDP 背景流的做法：物理容量 4 Mbps，背景流的速率是 (4 − x) Mbps，每一段一个定速源。
fn background(
    table: &[(u64, u64)],
    reference_bps: u64,
    end_s: u64,
    dir: Dir,
) -> Vec<(Dir, CrossSpec)> {
    const PHYSICAL: u64 = 4_000_000;
    table
        .iter()
        .enumerate()
        .filter_map(|(i, (t, ratio))| {
            let next = table.get(i + 1).map_or(end_s, |(n, _)| *n);
            let udp = PHYSICAL.saturating_sub(rate(reference_bps, *ratio));
            (udp > 0).then_some((
                dir,
                CrossSpec::Cbr {
                    rate_bps: udp,
                    packet_bytes: 1_200,
                    start: Time(secs(*t)),
                    stop: Some(Time(secs(next))),
                },
            ))
        })
        .collect()
}

/// 一路音频：和媒体同时开始、结束（暂停时音频照常，原文没说音频暂停）。
fn audio(flow: &MediaFlow) -> (Dir, CrossSpec) {
    (
        if flow.reverse {
            Dir::Reverse
        } else {
            Dir::Forward
        },
        CrossSpec::Cbr {
            rate_bps: AUDIO_BPS,
            packet_bytes: 50,
            start: Time(flow.start_us),
            stop: Some(Time(flow.end_us)),
        },
    )
}

/// 一路媒体的会话：一直播 `Video`，暂停的时段是 `Idle`。
fn media_scenario(name: &str, flow: &MediaFlow, seed: u64, network: &NetworkPlan) -> Scenario {
    let frames = |us: u64| u32::try_from(us * u64::from(FPS) / 1_000_000).unwrap_or(u32::MAX);
    let mut phases = Vec::new();
    match flow.pause_us {
        Some((a, b)) => {
            phases.push(Phase {
                action: Action::Video,
                frames: frames(a.saturating_sub(flow.start_us)),
            });
            phases.push(Phase {
                action: Action::Idle,
                frames: frames(b.saturating_sub(a)),
            });
            phases.push(Phase {
                action: Action::Video,
                frames: frames(flow.end_us.saturating_sub(b)),
            });
        }
        None => phases.push(Phase {
            action: Action::Video,
            frames: frames(flow.end_us.saturating_sub(flow.start_us)),
        }),
    }
    let mut s = Scenario::fixed(name, phases);
    (s.width, s.height) = MEDIA_SIZE;
    s.codec = CodecKind::H264;
    s.fps = FPS;
    s.start_bps = MEDIA_START_BPS;
    s.max_bps = MEDIA_MAX_BPS;
    s.seed = seed;
    // 网络是共享的（`MultiScenario::network`）；这里放同一份，只为指纹行的身份里写明布局
    s.network = network.clone();
    s
}

/// 拼出一个用例：共享网络（两个方向、交叉流）和各对。
#[allow(clippy::too_many_arguments)]
fn build(
    id: String,
    title: String,
    duration_s: u64,
    path: PathConfig,
    mut cross: Vec<(Dir, CrossSpec)>,
    media: Vec<MediaFlow>,
    extra_delay_us: &[u64],
    forward_capacity: Vec<(u64, u64)>,
    reverse_capacity: Vec<(u64, u64)>,
    seed: u64,
) -> Case {
    // 交叉流的顺序：先是用例自己的（背景流、TCP），再是各路音频
    let tcp: Vec<usize> = cross
        .iter()
        .enumerate()
        .filter(|(_, (_, c))| matches!(c, CrossSpec::Tcp { .. } | CrossSpec::Bursts { .. }))
        .map(|(i, _)| i)
        .collect();
    let long_tcp: Vec<usize> = cross
        .iter()
        .enumerate()
        .filter(|(_, (_, c))| matches!(c, CrossSpec::Tcp { bytes: None, .. }))
        .map(|(i, _)| i)
        .collect();
    for f in media.iter().filter(|f| f.audio) {
        cross.push(audio(f));
    }
    let network = NetworkPlan::Layout2(Box::new(Layout2 {
        start: path,
        steps: Vec::new(),
        routes: Vec::new(),
        cross,
    }));
    let pairs = media
        .iter()
        .enumerate()
        .map(|(i, f)| PairSpec {
            scenario: media_scenario(
                &format!("rfc8867-{id}/{}", i + 1),
                f,
                seed * 16 + i as u64,
                &network,
            ),
            start_us: f.start_us + frame_phase(seed, i),
            reverse: f.reverse,
            extra_delay_us: extra_delay_us.get(i).copied().unwrap_or(0),
        })
        .collect();
    Case {
        id,
        title,
        duration_us: secs(duration_s),
        multi: MultiScenario {
            network,
            seed,
            pairs,
        },
        media,
        forward_capacity,
        reverse_capacity,
        long_tcp,
        tcp,
    }
}

/// 每一路帧时钟的相位：开始时刻加上 [0, 一帧间隔) 里的一个确定的偏移（由种子和第几路定）。
///
/// 没有它时，各路都是 30 帧每秒、开始时刻差整数秒，帧时钟完全对齐；全局循环在同一时刻总是先让前面的那一路编码、发出，
/// 后面那一路的每一帧都排在它后面，看到的排队系统性更高（3 个种子的开发那一轮里，5.2 两路同时开始，每次都是第 1 路赢，
/// 第 2 路的单程延迟中位总是多 8 ms）。真实里两路的帧不会对齐到同一微秒。
fn frame_phase(seed: u64, i: usize) -> u64 {
    // SplitMix64 的一步，足够把（种子，路）打散
    let mut z = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(
        u64::try_from(i)
            .unwrap_or(0)
            .wrapping_add(1)
            .wrapping_mul(0xBF58_476D_1CE4_E5B9),
    );
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    z % (1_000_000 / u64::from(FPS))
}

fn flow(start_s: u64, end_s: u64) -> MediaFlow {
    MediaFlow {
        start_us: secs(start_s),
        end_us: secs(end_s),
        pause_us: None,
        reverse: false,
        audio: true,
    }
}

/// 变容量的做法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// 直接改链路容量。
    Trace,
    /// 4 Mbps 物理容量加 (4 − x) Mbps 的 UDP 背景流。
    Udp,
}

impl Mechanism {
    const fn name(self) -> &'static str {
        match self {
            Mechanism::Trace => "trace",
            Mechanism::Udp => "udp",
        }
    }
}

/// 一个方向按容量表变：两种做法之一。返回（容量过程，背景流）。
fn varying(
    table: &[(u64, u64)],
    reference_bps: u64,
    end_s: u64,
    m: Mechanism,
    d: Dir,
) -> (Capacity, Vec<(Dir, CrossSpec)>) {
    match m {
        Mechanism::Trace => (trace(table, reference_bps, end_s), Vec::new()),
        Mechanism::Udp => (
            Capacity::Constant {
                rate_bps: 4_000_000,
            },
            background(table, reference_bps, end_s, d),
        ),
    }
}

/// 5.1 单流变速：100 s；1 Mbps × 1.0（0 s）→ 2.5（40 s）→ 0.6（60 s）→ 1.0（80 s）；单程 50 或 100 ms。
#[must_use]
pub fn case_5_1(m: Mechanism, one_way_ms: u64, seed: u64) -> Case {
    let table = [(0, 1000), (40, 2500), (60, 600), (80, 1000)];
    let (cap, cross) = varying(&table, 1_000_000, 100, m, Dir::Forward);
    let d = one_way_ms * 1_000;
    let path = PathConfig {
        forward: dir(cap, d, 300_000),
        reverse: dir(
            Capacity::Constant {
                rate_bps: 4_000_000,
            },
            d,
            300_000,
        ),
    };
    build(
        format!("5.1-{}-{one_way_ms}ms", m.name()),
        format!("5.1 单流变速（{}，单程 {one_way_ms} ms）", m.name()),
        100,
        path,
        cross,
        vec![flow(0, 100)],
        &[],
        timeline(&table, 1_000_000),
        vec![(0, 4_000_000)],
        seed,
    )
}

/// 5.2 多流变速：125 s；两路；2 Mbps × 2.0 → 1.0（25 s）→ 1.75（50 s）→ 0.5（75 s）→ 1.0（100 s）。
#[must_use]
pub fn case_5_2(m: Mechanism, seed: u64) -> Case {
    let table = [(0, 2000), (25, 1000), (50, 1750), (75, 500), (100, 1000)];
    let (cap, cross) = varying(&table, 2_000_000, 125, m, Dir::Forward);
    let path = PathConfig {
        forward: dir(cap, 50_000, 300_000),
        reverse: dir(
            Capacity::Constant {
                rate_bps: 4_000_000,
            },
            50_000,
            300_000,
        ),
    };
    build(
        format!("5.2-{}", m.name()),
        format!("5.2 多流变速（{}）", m.name()),
        125,
        path,
        cross,
        vec![flow(0, 125), flow(0, 125)],
        &[],
        timeline(&table, 2_000_000),
        vec![(0, 4_000_000)],
        seed,
    )
}

/// 5.3 反向拥塞、双向媒体：100 s；1 Mbps；正向 2.0 → 1.0（20 s）→ 0.5（40 s）→ 2.0（60 s），
/// 反向 2.0 → 0.8（35 s）→ 2.0（70 s）。第 2 路的画面走反向。
#[must_use]
pub fn case_5_3(m: Mechanism, seed: u64) -> Case {
    let fwd = [(0, 2000), (20, 1000), (40, 500), (60, 2000)];
    let rev = [(0, 2000), (35, 800), (70, 2000)];
    let (fcap, mut cross) = varying(&fwd, 1_000_000, 100, m, Dir::Forward);
    let (rcap, rcross) = varying(&rev, 1_000_000, 100, m, Dir::Reverse);
    cross.extend(rcross);
    let path = PathConfig {
        forward: dir(fcap, 50_000, 300_000),
        reverse: dir(rcap, 50_000, 300_000),
    };
    build(
        format!("5.3-{}", m.name()),
        format!("5.3 反向拥塞、双向媒体（{}）", m.name()),
        100,
        path,
        cross,
        vec![
            flow(0, 100),
            MediaFlow {
                reverse: true,
                ..flow(0, 100)
            },
        ],
        &[],
        timeline(&fwd, 1_000_000),
        timeline(&rev, 1_000_000),
        seed,
    )
}

/// 5.4 同算法竞争：120 s；3.5 Mbps；三路在 0、20、40 s 加入，各带一路音频。
#[must_use]
pub fn case_5_4(seed: u64) -> Case {
    let path = PathConfig::symmetric(dir(
        Capacity::Constant {
            rate_bps: 3_500_000,
        },
        50_000,
        300_000,
    ));
    build(
        "5.4".to_owned(),
        "5.4 同算法竞争".to_owned(),
        120,
        path,
        Vec::new(),
        vec![flow(0, 120), flow(20, 120), flow(40, 120)],
        &[],
        vec![(0, 3_500_000)],
        vec![(0, 3_500_000)],
        seed,
    )
}

/// 5.5 RTT 公平：300 s；4 Mbps；五路，单程 10、25、50、100、150 ms，在 0、10、20、30、40 s 加入。
/// 共享路径本身不加延迟，每一路的单程延迟用 `PairSpec::extra_delay_us`（两个方向都加）。
#[must_use]
pub fn case_5_5(seed: u64) -> Case {
    let path = PathConfig::symmetric(dir(
        Capacity::Constant {
            rate_bps: 4_000_000,
        },
        0,
        300_000,
    ));
    build(
        "5.5".to_owned(),
        "5.5 RTT 公平（单程 10/25/50/100/150 ms）".to_owned(),
        300,
        path,
        Vec::new(),
        (0..5).map(|i| flow(i * 10, 300)).collect(),
        &[10_000, 25_000, 50_000, 100_000, 150_000],
        vec![(0, 4_000_000)],
        vec![(0, 4_000_000)],
        seed,
    )
}

/// 5.6 对抗长 TCP：120 s；2 Mbps；队列 300 或 1000 ms；TCP 0 s 起（默认的 TCP，即 NewReno 加 SACK），媒体 5 s 起。
#[must_use]
pub fn case_5_6(queue_ms: u64, seed: u64) -> Case {
    let path = PathConfig::symmetric(dir(
        Capacity::Constant {
            rate_bps: 2_000_000,
        },
        50_000,
        queue_ms * 1_000,
    ));
    let tcp = vec![(
        Dir::Forward,
        CrossSpec::Tcp {
            cfg: TcpConfig::default(),
            start: Time::ZERO,
            bytes: None,
        },
    )];
    build(
        format!("5.6-{queue_ms}ms"),
        format!("5.6 对抗长 TCP（队列 {queue_ms} ms）"),
        120,
        path,
        tcp,
        vec![flow(5, 120)],
        &[],
        vec![(0, 2_000_000)],
        vec![(0, 2_000_000)],
        seed,
    )
}

/// 不在 RFC 8867 里（PeerDesk 自己加的，名字以 `pd-` 开头）：同一条 2 Mbps 窄上行上有一个视频通话
/// （Zoom、Teams 一类：1 Mbps、30 fps 按帧突发，帧内按 2.5 倍媒体码率排开，WebRTC 节奏器的默认；不是我们的码率控制，
/// 不会让），我们一路 5 s 起。60 s；单程 50 ms；队列 300 ms。
/// 为什么要它（a3-plan 第 11 节，主会话 2026-11-28）：5.2 窄段一路被饿死，是常驻队列判断采到了别的流的周期性突发；
/// 真实里更常见的“别的周期性突发”是同一条窄上行上的视频通话，而不是两路 PeerDesk。
/// 公平份额按容量减去通话的 1 Mbps 算（和 UDP 背景流一样从容量里扣）。
#[must_use]
pub fn case_video_call(seed: u64) -> Case {
    let path = PathConfig::symmetric(dir(
        Capacity::Constant {
            rate_bps: 2_000_000,
        },
        50_000,
        300_000,
    ));
    let call = vec![(
        Dir::Forward,
        CrossSpec::Frames {
            rate_bps: 1_000_000,
            fps: 30,
            packet_bytes: 1_200,
            pace_permille: 2_500,
            start: Time::ZERO,
            stop: None,
        },
    )];
    build(
        "pd-video-call".to_owned(),
        "PeerDesk 加项：窄上行上同时有视频通话（1 Mbps、30 fps 按帧突发）".to_owned(),
        60,
        path,
        call,
        vec![flow(5, 60)],
        &[],
        vec![(0, 1_000_000)],
        vec![(0, 2_000_000)],
        seed,
    )
}

/// 5.7 对抗短 TCP：300 s；2 Mbps；两路媒体 5 s 起；十条短 TCP 流，2 条开始时是开、8 条是关。
///
/// 两种读法（主会话 2026-11-28 定：主用例照 8867 字面，30 个连接的作为压力变体另跑，报告里两种都标明）：
/// - **主用例**（`connections = 1`）：RFC 8867 第 5.7 节原文“Number and types of sources: ten (10), short-lived TCP flows”，
///   “Each short TCP flow is modeled as a sequence of file downloads interleaved with idle periods”。十条流，每条一个连接，
///   下载和空闲交替；文件大小和空闲时间照它引的 RFC 8868 第 5.1 节（30–50 KB，空闲均值 10 秒的指数分布）；
/// - **压力变体**（`connections = 30`）：RFC 8868 第 5.1 节“30 connections start simultaneously fetching small (30-50 KB)
///   amounts of data”，理解成每条流一阵 30 个连接，合计最多 300 个连接抢 2 Mbps。
#[must_use]
pub fn case_5_7(seed: u64) -> Case {
    case_5_7_with(1, seed)
}

/// 5.7 的压力变体：每条短流一阵 30 个连接（见 [`case_5_7`]）。
#[must_use]
pub fn case_5_7_stress(seed: u64) -> Case {
    case_5_7_with(30, seed)
}

fn case_5_7_with(connections: u32, seed: u64) -> Case {
    let path = PathConfig::symmetric(dir(
        Capacity::Constant {
            rate_bps: 2_000_000,
        },
        50_000,
        300_000,
    ));
    let tcp = (0..10)
        .map(|i| {
            (
                Dir::Forward,
                CrossSpec::Bursts {
                    cfg: TcpConfig::default(),
                    connections,
                    min_bytes: 30_000,
                    max_bytes: 50_000,
                    mean_off_us: 10_000_000,
                    start_on: i < 2,
                    start: Time::ZERO,
                },
            )
        })
        .collect();
    let (id, title) = if connections == 1 {
        (
            "5.7".to_owned(),
            "5.7 对抗短 TCP（十条，每条一个连接，2 开 8 关；照 RFC 8867 字面）".to_owned(),
        )
    } else {
        (
            format!("5.7-stress{connections}"),
            format!("5.7 压力变体（十条，每条一阵 {connections} 个连接；照 RFC 8868 第 5.1 节的另一种读法）"),
        )
    };
    build(
        id,
        title,
        300,
        path,
        tcp,
        vec![flow(5, 300), flow(5, 300)],
        &[],
        vec![(0, 2_000_000)],
        vec![(0, 2_000_000)],
        seed,
    )
}

/// 5.8 暂停和恢复：参数同 5.4，三路都 0 s 起，第 2 路 40 s 暂停、60 s 恢复（暂停做成 `Idle`，见模块文档）。
#[must_use]
pub fn case_5_8(seed: u64) -> Case {
    let path = PathConfig::symmetric(dir(
        Capacity::Constant {
            rate_bps: 3_500_000,
        },
        50_000,
        300_000,
    ));
    build(
        "5.8".to_owned(),
        "5.8 暂停和恢复（第 2 路 40–60 s 静止）".to_owned(),
        120,
        path,
        Vec::new(),
        vec![
            flow(0, 120),
            MediaFlow {
                pause_us: Some((secs(40), secs(60))),
                ..flow(0, 120)
            },
            flow(0, 120),
        ],
        &[],
        vec![(0, 3_500_000)],
        vec![(0, 3_500_000)],
        seed,
    )
}

/// 量内容自己的上限：一路、不限速、单程 50 ms、60 秒（主会话 2026-11-28）。它的平均接收速率就是这份内容在 1.5 Mbps
/// 封顶下最多能出多少；报告里的“稳定值 ÷ min(公平份额, 内容上限)”用它，内容出满了不会被看成不公平。
#[must_use]
pub fn ceiling_case(seed: u64) -> Case {
    let path = PathConfig::symmetric(dir(Capacity::Unlimited, 50_000, 300_000));
    build(
        "ceiling".to_owned(),
        "内容自己的上限（不限速）".to_owned(),
        60,
        path,
        Vec::new(),
        vec![MediaFlow {
            audio: false,
            ..flow(0, 60)
        }],
        &[],
        vec![(0, 0)],
        vec![(0, 0)],
        seed,
    )
}

/// 全部用例（5.1 两种做法 × 两档延迟，5.2、5.3 两种做法，5.6 两档队列）。
#[must_use]
pub fn cases(seed: u64) -> Vec<Case> {
    let mut v = Vec::new();
    for m in [Mechanism::Trace, Mechanism::Udp] {
        for ms in [50, 100] {
            v.push(case_5_1(m, ms, seed));
        }
    }
    for m in [Mechanism::Trace, Mechanism::Udp] {
        v.push(case_5_2(m, seed));
    }
    for m in [Mechanism::Trace, Mechanism::Udp] {
        v.push(case_5_3(m, seed));
    }
    v.push(case_5_4(seed));
    v.push(case_5_5(seed));
    for q in [300, 1_000] {
        v.push(case_5_6(q, seed));
    }
    v.push(case_5_7(seed));
    v.push(case_5_7_stress(seed));
    v.push(case_5_8(seed));
    v.push(case_video_call(seed));
    v
}

/// 换画面尺寸（720p 对照用）。名字里加上尺寸，指纹行的身份分得开。
#[must_use]
pub fn with_size(mut c: Case, width: u16, height: u16) -> Case {
    if (width, height) == MEDIA_SIZE {
        return c;
    }
    for p in &mut c.multi.pairs {
        p.scenario.width = width;
        p.scenario.height = height;
        p.scenario.name = format!("{}@{width}x{height}", p.scenario.name);
    }
    c.id = format!("{}@{width}x{height}", c.id);
    c
}

/// 把用例截短到前 `secs` 秒（CI 的冒烟：保证能跑、指标有数）。各路的阶段按比例截，开始晚于截止的去掉。
#[must_use]
pub fn truncated(mut c: Case, limit_s: u64) -> Case {
    let limit = secs(limit_s);
    if c.duration_us <= limit {
        return c;
    }
    c.duration_us = limit;
    let keep: Vec<bool> = c.media.iter().map(|f| f.start_us < limit).collect();
    let mut media = Vec::new();
    let mut pairs = Vec::new();
    for ((f, p), k) in c.media.iter().zip(c.multi.pairs.drain(..)).zip(&keep) {
        if !*k {
            continue;
        }
        let mut f = *f;
        f.end_us = f.end_us.min(limit);
        let mut p = p;
        // 阶段按时长截：从开始起累计，超过的砍掉
        let mut left =
            u32::try_from((f.end_us - f.start_us) * u64::from(FPS) / 1_000_000).unwrap_or(0);
        let mut phases = Vec::new();
        for ph in &p.scenario.phases {
            if left == 0 {
                break;
            }
            let frames = ph.frames.min(left);
            left -= frames;
            phases.push(Phase { frames, ..*ph });
        }
        p.scenario.phases = phases;
        media.push(f);
        pairs.push(p);
    }
    c.media = media;
    c.multi.pairs = pairs;
    c
}

// ---------------------------------------------------------------- 指标

/// 一段（两个切点之间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// 开始（微秒，全局）。
    pub start_us: u64,
    /// 结束。
    pub end_us: u64,
}

/// 一段里一路的收敛结论。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Convergence {
    /// 收敛了，用了多久（秒）。
    Converged(f64),
    /// 一直没进入范围，值取这一段的时长（秒）。
    NotConverged(f64),
    /// 段短于 15 秒。
    TooShort,
}

/// 一段里一路的指标。
#[derive(Debug, Clone, PartialEq)]
pub struct SpanFlow {
    /// 第几路（从 0 起）。
    pub flow: usize,
    /// 公平份额（比特每秒）。
    pub fair_bps: f64,
    /// 稳定值（最后 10 秒的接收速率均值；段太短时是整段的均值）。
    pub stable_bps: f64,
    /// min(公平份额, 内容自己的上限)；没给内容上限时等于公平份额。
    pub capped_fair_bps: f64,
    /// 收敛。
    pub convergence: Convergence,
    /// 振荡次数。
    pub oscillations: u32,
}

/// 一段的指标。
#[derive(Debug, Clone, PartialEq)]
pub struct SpanReport {
    /// 时段。
    pub span: Span,
    /// 在场的各路。
    pub flows: Vec<SpanFlow>,
    /// 同方向各路稳定值的最小 ÷ 最大（只有一路时为 `None`）。
    pub fairness: Option<f64>,
}

/// 一路的全程指标。
#[derive(Debug, Clone, PartialEq)]
pub struct FlowReport {
    /// 平均接收速率（在场期间，比特每秒）。
    pub mean_recv_bps: f64,
    /// 单程延迟中位、95%、99%（毫秒）。
    pub delay_ms: (Option<u64>, Option<u64>, Option<u64>),
    /// 接收端丢包率（1 − 收到 ÷ 发出）。
    pub loss: f64,
    /// 反馈开销（反馈字节 ÷ 画面字节）。
    pub feedback_overhead: f64,
    /// 每帧到齐要多久（首片到收齐）的中位、95%、99%（毫秒）。
    pub assembly_ms: (Option<u64>, Option<u64>, Option<u64>),
    /// 冻结次数。
    pub freezes: u32,
    /// 最长等待（毫秒）。
    pub max_wait_ms: f64,
}

/// 一个用例跑一遍的指标。
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// 用例名。
    pub id: String,
    /// 各路。
    pub flows: Vec<FlowReport>,
    /// 各段。
    pub spans: Vec<SpanReport>,
    /// 画面方向的利用率（全程均值）。
    pub utilization: f64,
    /// 真实排队（毫秒）：（均值，最大），取第 1 路画面方向的。
    pub queue_ms: (f64, f64),
    /// TCP 的吞吐（比特每秒）和丢包率（被丢的数据包 ÷ 发出的数据包）。
    pub tcp: Option<(f64, f64)>,
}

/// 一路的接收速率，全局时间上 200 ms 一格（比特每秒）；这一路开始之前的格子是 0。
fn recv_200ms(o: &Outcome, start_us: u64) -> Vec<f64> {
    let offset = usize::try_from(start_us / BIN_US).unwrap_or(0);
    let mut bins = vec![0u64; offset];
    bins.extend(&o.flow.recv_bytes);
    FlowLog::windows(&bins, 2)
        .into_iter()
        .map(|b| b as f64 * 8.0 / 0.2)
        .collect()
}

/// 一路的发送速率，同上。
fn send_200ms(o: &Outcome, start_us: u64) -> Vec<f64> {
    let offset = usize::try_from(start_us / BIN_US).unwrap_or(0);
    let mut bins = vec![0u64; offset];
    bins.extend(&o.flow.sent_bytes);
    FlowLog::windows(&bins, 2)
        .into_iter()
        .map(|b| b as f64 * 8.0 / 0.2)
        .collect()
}

/// `t` 时刻的容量（按时间表）。
fn capacity_at(table: &[(u64, u64)], t: u64) -> u64 {
    table
        .iter()
        .rfind(|(at, _)| *at <= t)
        .map_or(0, |(_, r)| *r)
}

/// 切段：容量变化、各路开始、暂停、恢复、结束。
#[must_use]
pub fn spans(c: &Case) -> Vec<Span> {
    let mut cuts = vec![0, c.duration_us];
    cuts.extend(c.forward_capacity.iter().map(|(t, _)| *t));
    cuts.extend(c.reverse_capacity.iter().map(|(t, _)| *t));
    for f in &c.media {
        cuts.push(f.start_us);
        cuts.push(f.end_us);
        if let Some((a, b)) = f.pause_us {
            cuts.push(a);
            cuts.push(b);
        }
    }
    cuts.retain(|t| *t <= c.duration_us);
    cuts.sort_unstable();
    cuts.dedup();
    cuts.windows(2)
        .filter_map(|w| match w {
            [a, b] if b > a => Some(Span {
                start_us: *a,
                end_us: *b,
            }),
            _ => None,
        })
        .collect()
}

/// 公平份额：这一段这个方向的容量减去在场的音频，除以在场的媒体加长 TCP 的条数，不超过媒体的上限。
fn fair_share(c: &Case, span: Span, reverse: bool) -> f64 {
    let mid = span.start_us;
    let table = if reverse {
        &c.reverse_capacity
    } else {
        &c.forward_capacity
    };
    let cap = capacity_at(table, mid) as f64;
    let same: Vec<&MediaFlow> = c
        .media
        .iter()
        .filter(|f| f.reverse == reverse && mid >= f.start_us && mid < f.end_us)
        .collect();
    let audio = same.iter().filter(|f| f.audio).count() as f64 * AUDIO_BPS as f64;
    let media = same.iter().filter(|f| f.active(mid)).count();
    // 长 TCP 都挂在正向
    let tcp = if reverse { 0 } else { c.long_tcp.len() };
    let n = (media + tcp).max(1) as f64;
    ((cap - audio).max(0.0) / n).min(MEDIA_MAX_BPS as f64)
}

/// 1 秒滑动平均（5 格 200 ms）。
fn smooth(v: &[f64]) -> Vec<f64> {
    (0..v.len())
        .map(|i| {
            let lo = i.saturating_sub(4);
            let w = v.get(lo..=i).unwrap_or(&[]);
            w.iter().sum::<f64>() / w.len().max(1) as f64
        })
        .collect()
}

/// 一段里一路的收敛：见模块文档。`rate` 是全局时间上 200 ms 一格的接收速率。
#[must_use]
pub fn convergence(rate: &[f64], span: Span) -> (f64, Convergence) {
    let lo = usize::try_from(span.start_us / 200_000).unwrap_or(0);
    let hi = usize::try_from(span.end_us / 200_000)
        .unwrap_or(0)
        .min(rate.len());
    let seg: Vec<f64> = rate.get(lo..hi).unwrap_or(&[]).to_vec();
    let len_s = (span.end_us - span.start_us) as f64 / 1e6;
    if len_s < 15.0 || seg.len() < 75 {
        let mean = seg.iter().sum::<f64>() / seg.len().max(1) as f64;
        return (mean, Convergence::TooShort);
    }
    let tail = seg.get(seg.len() - 50..).unwrap_or(&[]);
    let stable = tail.iter().sum::<f64>() / tail.len().max(1) as f64;
    let smooth = smooth(&seg);
    let inside = |x: f64| stable > 0.0 && (x - stable).abs() <= 0.2 * stable;
    // 最早的 i：从 i 起连续 25 格（5 秒）都在范围里
    let mut run = 0usize;
    for (i, x) in smooth.iter().enumerate() {
        if inside(*x) {
            run += 1;
            if run >= 25 {
                let start = i + 1 - 25;
                return (stable, Convergence::Converged(start as f64 * 0.2));
            }
        } else {
            run = 0;
        }
    }
    (stable, Convergence::NotConverged(len_s))
}

/// 一段里一路的振荡次数：500 ms 一格，高水位 fair × 1.25、低水位 fair × 0.5 之间来回一次记一次。
#[must_use]
pub fn oscillations(
    recv_bytes_100ms: &[u64],
    offset_bins: usize,
    span: Span,
    fair_bps: f64,
) -> u32 {
    let lo = usize::try_from(span.start_us / BIN_US).unwrap_or(0);
    let hi = usize::try_from(span.end_us / BIN_US).unwrap_or(0);
    let mut bins = vec![0u64; offset_bins];
    bins.extend(recv_bytes_100ms);
    let seg = bins.get(lo..hi.min(bins.len())).unwrap_or(&[]);
    let (high, low) = (fair_bps * 1.25, fair_bps * 0.5);
    let mut state: Option<bool> = None; // Some(true) 在高水位之上
    let mut n = 0u32;
    for w in seg.chunks(5) {
        if w.len() < 5 {
            break;
        }
        let r = w.iter().sum::<u64>() as f64 * 8.0 / 0.5;
        let now = if r > high {
            Some(true)
        } else if r < low {
            Some(false)
        } else {
            None
        };
        if let (Some(a), Some(b)) = (state, now) {
            if a != b {
                n += 1;
            }
        }
        if now.is_some() {
            state = now;
        }
    }
    n
}

/// 算一个用例跑一遍的指标。`outcomes` 和 `c.multi.pairs` 一一对应。`ceiling_bps` 是内容自己的上限（[`ceiling_case`] 量出来的），
/// 没有就给 `None`。
#[must_use]
pub fn report(c: &Case, outcomes: &[Outcome], ceiling_bps: Option<f64>) -> Report {
    let rates: Vec<Vec<f64>> = c
        .media
        .iter()
        .zip(outcomes)
        .map(|(f, o)| recv_200ms(o, f.start_us))
        .collect();
    let flows = c
        .media
        .iter()
        .zip(outcomes)
        .zip(&rates)
        .map(|((f, o), r)| {
            let lo = usize::try_from(f.start_us / 200_000).unwrap_or(0);
            let hi = usize::try_from(f.end_us / 200_000)
                .unwrap_or(0)
                .min(r.len());
            let active = r.get(lo..hi).unwrap_or(&[]);
            let (sent, recv) = o.flow.packets();
            let media_bytes: u64 = o.flow.sent_bytes.iter().sum();
            FlowReport {
                mean_recv_bps: active.iter().sum::<f64>() / active.len().max(1) as f64,
                delay_ms: (
                    o.flow.delay_percentile_ms(50),
                    o.flow.delay_percentile_ms(95),
                    o.flow.delay_percentile_ms(99),
                ),
                loss: if sent == 0 {
                    0.0
                } else {
                    1.0 - recv as f64 / sent as f64
                },
                feedback_overhead: o.flow.feedback_bytes as f64 / media_bytes.max(1) as f64,
                assembly_ms: (
                    o.flow.assembly_percentile_ms(50),
                    o.flow.assembly_percentile_ms(95),
                    o.flow.assembly_percentile_ms(99),
                ),
                freezes: o.freezes,
                max_wait_ms: o.max_wait_ms,
            }
        })
        .collect();
    let spans = spans(c)
        .into_iter()
        .map(|span| {
            let mut sf = Vec::new();
            for (i, ((f, o), r)) in c.media.iter().zip(outcomes).zip(&rates).enumerate() {
                if !f.active(span.start_us) {
                    continue;
                }
                let fair = fair_share(c, span, f.reverse);
                let (stable, conv) = convergence(r, span);
                let offset = usize::try_from(f.start_us / BIN_US).unwrap_or(0);
                sf.push(SpanFlow {
                    flow: i,
                    fair_bps: fair,
                    stable_bps: stable,
                    capped_fair_bps: ceiling_bps.map_or(fair, |c| fair.min(c)),
                    convergence: conv,
                    oscillations: oscillations(&o.flow.recv_bytes, offset, span, fair),
                });
            }
            // 同方向的各路比公平（5.3 两路方向不同，不比）
            let fairness = [false, true]
                .iter()
                .filter_map(|rev| {
                    let v: Vec<f64> = sf
                        .iter()
                        .filter(|x| c.media.get(x.flow).is_some_and(|f| f.reverse == *rev))
                        .map(|x| x.stable_bps)
                        .collect();
                    (v.len() >= 2).then(|| {
                        let max = v.iter().copied().fold(0.0, f64::max);
                        let min = v.iter().copied().fold(f64::INFINITY, f64::min);
                        if max > 0.0 {
                            min / max
                        } else {
                            0.0
                        }
                    })
                })
                .reduce(f64::min);
            SpanReport {
                span,
                flows: sf,
                fairness,
            }
        })
        .collect();
    // 利用率：正向（画面）各路发出之和 ÷ 容量，200 ms 一格
    let sends: Vec<Vec<f64>> = c
        .media
        .iter()
        .zip(outcomes)
        .filter(|(f, _)| !f.reverse)
        .map(|(f, o)| send_200ms(o, f.start_us))
        .collect();
    let windows = usize::try_from(c.duration_us / 200_000).unwrap_or(0);
    let mut util = Vec::new();
    for w in 0..windows {
        let t = w as u64 * 200_000;
        let cap = capacity_at(&c.forward_capacity, t) as f64;
        let any = c.media.iter().any(|f| !f.reverse && f.active(t));
        if cap <= 0.0 || !any {
            continue;
        }
        let s: f64 = sends.iter().map(|v| v.get(w).copied().unwrap_or(0.0)).sum();
        util.push(s / cap);
    }
    let queue = outcomes.first().map_or((0.0, 0.0), |o| {
        let v = &o.flow.queue_us;
        let mean = v.iter().sum::<u64>() as f64 / v.len().max(1) as f64 / 1000.0;
        let max = v.iter().copied().max().unwrap_or(0) as f64 / 1000.0;
        (mean, max)
    });
    let tcp = (!c.tcp.is_empty())
        .then(|| outcomes.first())
        .flatten()
        .map(|o| {
            let st: Vec<CrossStats> = c
                .tcp
                .iter()
                .filter_map(|i| o.cross.get(*i).copied())
                .collect();
            let bytes: u64 = st.iter().map(|s| s.delivered_bytes).sum();
            let sent: u64 = st.iter().map(|s| s.sent_packets).sum();
            let dropped: u64 = st.iter().map(|s| s.queue_dropped + s.lost).sum();
            (
                bytes as f64 * 8.0 / (c.duration_us as f64 / 1e6),
                dropped as f64 / sent.max(1) as f64,
            )
        });
    Report {
        id: c.id.clone(),
        flows,
        spans,
        utilization: util.iter().sum::<f64>() / util.len().max(1) as f64,
        queue_ms: queue,
        tcp,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn the_cases_follow_the_rfc_tables() {
        let all = cases(1);
        assert_eq!(all.len(), 16);
        // 加项：视频通话按帧发，公平份额扣掉它的 1 Mbps
        let vc = all.last().unwrap();
        assert_eq!(vc.id, "pd-video-call");
        assert_eq!(vc.forward_capacity, vec![(0, 1_000_000)]);
        let NetworkPlan::Layout2(p) = &vc.multi.network else {
            panic!()
        };
        // 交叉流里还有音频（定速）；按帧发的只有通话一条
        let frames: Vec<_> = p
            .cross
            .iter()
            .filter(|(_, c)| matches!(c, CrossSpec::Frames { .. }))
            .collect();
        assert!(matches!(
            frames[..],
            [(
                Dir::Forward,
                CrossSpec::Frames {
                    rate_bps: 1_000_000,
                    fps: 30,
                    pace_permille: 2_500,
                    ..
                }
            )]
        ));
        let c = case_5_1(Mechanism::Trace, 50, 1);
        assert_eq!(c.duration_us, 100_000_000);
        assert_eq!(
            c.forward_capacity,
            vec![
                (0, 1_000_000),
                (40_000_000, 2_500_000),
                (60_000_000, 600_000),
                (80_000_000, 1_000_000)
            ]
        );
        let NetworkPlan::Layout2(p) = &c.multi.network else {
            panic!()
        };
        assert_eq!(
            p.start.forward.bottleneck.capacity,
            Capacity::Trace(Trace {
                segments: vec![
                    (40_000_000, 1_000_000),
                    (20_000_000, 2_500_000),
                    (20_000_000, 600_000),
                    (20_000_000, 1_000_000)
                ],
                repeat: false,
            })
        );
        assert_eq!(p.start.forward.noise.pdv, Some(Pdv::RFC8868));
        assert_eq!(p.start.forward.delay, Delay::Fixed(50_000));
        assert_eq!(p.cross.len(), 1, "一路音频");
        let s = &c.multi.pairs[0].scenario;
        assert_eq!((s.start_bps, s.max_bps), (150_000, 1_500_000));
        assert_eq!((s.width, s.height), (640, 360));
        let big = with_size(c.clone(), 1280, 720);
        assert_eq!(big.id, "5.1-trace-50ms@1280x720");
        assert_eq!(big.multi.pairs[0].scenario.width, 1280);
        assert!(big.multi.pairs[0].scenario.name.ends_with("@1280x720"));
        assert_eq!(with_size(c.clone(), 640, 360).id, c.id);
        assert_eq!(s.phases.iter().map(|p| p.frames).sum::<u32>(), 3_000);
        // UDP 背景流：4 Mbps 物理容量，背景流 3、1.5、3.4、3 Mbps
        let u = case_5_1(Mechanism::Udp, 100, 1);
        let NetworkPlan::Layout2(p) = &u.multi.network else {
            panic!()
        };
        let rates: Vec<u64> = p
            .cross
            .iter()
            .filter_map(|(_, c)| match c {
                CrossSpec::Cbr {
                    rate_bps,
                    packet_bytes: 1_200,
                    ..
                } => Some(*rate_bps),
                CrossSpec::Cbr { .. }
                | CrossSpec::Frames { .. }
                | CrossSpec::Tcp { .. }
                | CrossSpec::Bursts { .. } => None,
            })
            .collect();
        assert_eq!(rates, vec![3_000_000, 1_500_000, 3_400_000, 3_000_000]);
        assert_eq!(p.start.forward.delay, Delay::Fixed(100_000));
        // 5.2 第一段 2 × 2 = 4 Mbps：不需要背景流
        let NetworkPlan::Layout2(p) = &case_5_2(Mechanism::Udp, 1).multi.network else {
            panic!()
        };
        assert_eq!(
            p.cross
                .iter()
                .filter(|(_, c)| matches!(
                    c,
                    CrossSpec::Cbr {
                        packet_bytes: 1_200,
                        ..
                    }
                ))
                .count(),
            4
        );
        // 5.3：第 2 路反向；反向也有背景流
        let c3 = case_5_3(Mechanism::Udp, 1);
        assert!(c3.multi.pairs[1].reverse && !c3.multi.pairs[0].reverse);
        let NetworkPlan::Layout2(p) = &c3.multi.network else {
            panic!()
        };
        assert!(p.cross.iter().any(|(d, c)| *d == Dir::Reverse
            && matches!(
                c,
                CrossSpec::Cbr {
                    packet_bytes: 1_200,
                    ..
                }
            )));
        // 5.5：额外延迟
        let c5 = case_5_5(1);
        let extra: Vec<u64> = c5.multi.pairs.iter().map(|p| p.extra_delay_us).collect();
        assert_eq!(extra, vec![10_000, 25_000, 50_000, 100_000, 150_000]);
        // 开始时刻加了 [0, 一帧) 的相位
        let start = c5.multi.pairs[4].start_us;
        assert!((40_000_000..40_033_333).contains(&start), "{start}");
        let phases: Vec<u64> = c5
            .multi
            .pairs
            .iter()
            .map(|p| p.start_us % 1_000_000)
            .collect();
        assert!(
            phases.windows(2).any(|w| w[0] != w[1]),
            "各路相位不同：{phases:?}"
        );
        assert_eq!(frame_phase(3, 1), frame_phase(3, 1));
        // 5.6：长 TCP 一条、队列两档
        let c6 = case_5_6(1_000, 1);
        assert_eq!((c6.long_tcp.clone(), c6.tcp.clone()), (vec![0], vec![0]));
        let NetworkPlan::Layout2(p) = &c6.multi.network else {
            panic!()
        };
        assert_eq!(
            p.start.forward.bottleneck.limit,
            QueueLimit::delay(1_000_000)
        );
        // 5.7：十条短 TCP，2 开 8 关
        let c7 = case_5_7(1);
        assert_eq!(c7.tcp.len(), 10);
        assert!(c7.long_tcp.is_empty());
        let NetworkPlan::Layout2(p) = &c7.multi.network else {
            panic!()
        };
        let NetworkPlan::Layout2(ps) = &case_5_7_stress(1).multi.network else {
            panic!()
        };
        assert!(ps.cross.iter().all(
            |(_, c)| !matches!(c, CrossSpec::Bursts { connections, .. } if *connections != 30)
        ));
        assert!(p.cross.iter().all(
            |(_, c)| !matches!(c, CrossSpec::Bursts { connections, .. } if *connections != 1)
        ));
        assert_eq!(case_5_7_stress(1).id, "5.7-stress30");
        let ceil = ceiling_case(1);
        assert_eq!((ceil.media.len(), ceil.media[0].audio), (1, false));
        let on = p
            .cross
            .iter()
            .filter(|(_, c)| matches!(c, CrossSpec::Bursts { start_on: true, .. }))
            .count();
        assert_eq!(on, 2);
        // 5.8：第 2 路暂停
        let c8 = case_5_8(1);
        let ph: Vec<(Action, u32)> = c8.multi.pairs[1]
            .scenario
            .phases
            .iter()
            .map(|p| (p.action, p.frames))
            .collect();
        assert_eq!(
            ph,
            vec![
                (Action::Video, 1_200),
                (Action::Idle, 600),
                (Action::Video, 1_800)
            ]
        );
        // 每一对的名字不同（指纹行的身份）
        let mut names: Vec<String> = all
            .iter()
            .flat_map(|c| c.multi.pairs.iter().map(|p| p.scenario.name.clone()))
            .collect();
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
    }

    #[test]
    fn spans_fair_shares_and_truncation() {
        let c = case_5_4(1);
        let s = spans(&c);
        assert_eq!(
            s.iter()
                .map(|x| (x.start_us / 1_000_000, x.end_us / 1_000_000))
                .collect::<Vec<_>>(),
            vec![(0, 20), (20, 40), (40, 120)]
        );
        // 3.5 Mbps，三路在场：(3.5 − 3 × 0.02) / 3
        let fair = fair_share(&c, s[2], false);
        assert!((fair - (3_500_000.0 - 60_000.0) / 3.0).abs() < 1.0);
        // 一路时封顶 1.5 Mbps
        assert!((fair_share(&c, s[0], false) - 1_500_000.0).abs() < 1.0);
        // 5.6：长 TCP 算一条
        let c6 = case_5_6(300, 1);
        let s6 = spans(&c6);
        assert!((fair_share(&c6, s6[1], false) - (2_000_000.0 - 20_000.0) / 2.0).abs() < 1.0);
        // 5.8：暂停的那段只有两路在发
        let c8 = case_5_8(1);
        let s8 = spans(&c8);
        assert_eq!(s8.len(), 3);
        // 两路在发：(3.5 − 0.06) / 2 = 1.72 Mbps，封顶 1.5 Mbps
        assert!((fair_share(&c8, s8[1], false) - 1_500_000.0).abs() < 1.0);
        assert!(!c8.media[1].active(45_000_000) && c8.media[1].active(60_000_000));
        // 截到前 10 秒：5.4 只剩第 1 路，帧数 300
        let t = truncated(case_5_4(1), 10);
        assert_eq!(t.multi.pairs.len(), 1);
        assert_eq!(t.duration_us, 10_000_000);
        assert_eq!(
            t.multi.pairs[0]
                .scenario
                .phases
                .iter()
                .map(|p| p.frames)
                .sum::<u32>(),
            300
        );
        let t8 = truncated(case_5_8(1), 50);
        assert_eq!(
            t8.multi.pairs[1]
                .scenario
                .phases
                .iter()
                .map(|p| p.frames)
                .sum::<u32>(),
            1_500
        );
        let same = truncated(case_5_4(1), 500);
        assert_eq!(same.duration_us, 120_000_000);
        assert_eq!(capacity_at(&[], 5), 0);
    }

    #[test]
    fn convergence_and_oscillation_rules() {
        // 前 3 秒 0，之后稳定在 1 Mbps：3 秒收敛
        let mut r = vec![0.0; 15];
        r.extend(vec![1_000_000.0; 100]);
        let span = Span {
            start_us: 0,
            end_us: 23_000_000,
        };
        let (stable, c) = convergence(&r, span);
        assert!((stable - 1_000_000.0).abs() < 1.0);
        assert!(
            matches!(c, Convergence::Converged(t) if (t - 3.6).abs() < 0.01),
            "{c:?}"
        );
        // 一直来回跳：未收敛，值是段长
        let osc: Vec<f64> = (0..100)
            .map(|i| if (i / 10) % 2 == 0 { 2e6 } else { 0.1e6 })
            .collect();
        let (_, c) = convergence(
            &osc,
            Span {
                start_us: 0,
                end_us: 20_000_000,
            },
        );
        assert_eq!(c, Convergence::NotConverged(20.0));
        // 段太短
        let (_, c) = convergence(
            &r,
            Span {
                start_us: 0,
                end_us: 10_000_000,
            },
        );
        assert_eq!(c, Convergence::TooShort);
        // 振荡：500 ms 一格高低交替 4 次 → 3 次来回
        let fair = 1_000_000.0;
        let hi = 25_000; // 100 ms 里 2 Mbps 的字节
        let lo = 1_250;
        let mut bytes = Vec::new();
        for k in 0..4 {
            bytes.extend(vec![if k % 2 == 0 { hi } else { lo }; 5]);
        }
        let n = oscillations(
            &bytes,
            0,
            Span {
                start_us: 0,
                end_us: 2_000_000,
            },
            fair,
        );
        assert_eq!(n, 3);
        // 中间的值不算一次
        let mid = 12_500;
        let mut b2 = vec![hi; 5];
        b2.extend(vec![mid; 5]);
        b2.extend(vec![hi; 5]);
        assert_eq!(
            oscillations(
                &b2,
                0,
                Span {
                    start_us: 0,
                    end_us: 1_500_000
                },
                fair
            ),
            0
        );
    }
}
