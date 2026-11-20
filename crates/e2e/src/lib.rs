//! PeerDesk 端到端测试平台（第一层：进程内仿真）。
//!
//! 被控端和控制端用的是产品里同一份发送端 / 接收端（[`peerdesk_core::media`]）和真实的编码器，
//! 中间是确定性的仿真网络，画面是接近真实的桌面（[`content`]）。每个场景输出：
//! - 延迟（采集到显示）、卡顿（两次画面更新之间的最大间隔、超过 200 ms 的次数）；
//! - 画质（显示的画面和源画面的亮度 PSNR）、停下之后多久变清晰；
//! - 每种操作的流量、关键帧数、丢失恢复次数；
//! - 每个阶段结束时的并排截图（源画面 | 显示的画面 | 放大的差异）。
//!
//! 同一个场景同一个种子，结果逐字节相同，可以放进 CI 当回归门槛。

pub mod content;
pub mod report;

use peerdesk_core::adaptive::Profile;
use peerdesk_core::error::Error;
use peerdesk_core::media::{MediaReceiver, MediaSender, SenderConfig};
use peerdesk_core::proto::{decode_datagram, encode_datagram, Channel, Header};
use peerdesk_core::screen::Screen;
use peerdesk_core::sim::{Link, Net, NetStats};
use peerdesk_core::time::Time;
use peerdesk_core::video::{TileDecoder, TileEncoder, VideoDecoder, VideoEncoder};
use peerdesk_media::{H264Decoder, H264Encoder};

pub use content::{Action, RealisticDesktop};

const HOST: usize = 0;
const VIEWER: usize = 1;
/// 反馈间隔（微秒）。
const FEEDBACK_US: u64 = 20_000;
/// 亮度 PSNR 达到这个值算“清晰”（dB）。
pub const SHARP_DB: f64 = 38.0;
/// 两次画面更新间隔超过这么久算一次卡顿（微秒）。
pub const FREEZE_US: u64 = 200_000;

/// 一对编码器和解码器。
type Codecs = (Box<dyn VideoEncoder>, Box<dyn VideoDecoder>);

/// 默认画面尺寸。调试构建（覆盖率）里画面合成慢得多，用小画面，逻辑一样。
const DEFAULT_SIZE: (u16, u16) = if cfg!(debug_assertions) {
    (480, 272)
} else {
    (1280, 720)
};

/// 编码器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecKind {
    /// 块编码（对照组）。
    Tile,
    /// H.264（OpenH264）。
    H264,
}

impl CodecKind {
    /// 名字。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CodecKind::Tile => "tile",
            CodecKind::H264 => "h264",
        }
    }

    fn make(self, fps: u32, max_bps: u64) -> Result<Codecs, Error> {
        Ok(match self {
            CodecKind::Tile => (Box::new(TileEncoder::new()), Box::new(TileDecoder::new())),
            CodecKind::H264 => (
                Box::new(H264Encoder::with_max_bps(fps, max_bps)),
                Box::new(H264Decoder::new()?),
            ),
        })
    }
}

/// 一个阶段：做什么，做多少帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phase {
    /// 操作。
    pub action: Action,
    /// 帧数。
    pub frames: u32,
}

/// 一个场景。
#[derive(Debug, Clone)]
pub struct Scenario {
    /// 名字，也是截图目录名。
    pub name: String,
    /// 画面尺寸。
    pub width: u16,
    /// 画面高度。
    pub height: u16,
    /// 帧率。
    pub fps: u32,
    /// 编码器。
    pub codec: CodecKind,
    /// 依次执行的阶段。
    pub phases: Vec<Phase>,
    /// 网络：从某个时刻（微秒）起换成某条链路。第一项应当从 0 开始。
    pub network: Vec<(u64, Link)>,
    /// 网络画像。
    pub profile: Profile,
    /// 起始码率。
    pub start_bps: u64,
    /// 码率上限。
    pub max_bps: u64,
    /// 随机种子。
    pub seed: u64,
}

impl Scenario {
    /// 一个默认的场景：720p、30 帧、H.264、局域网。
    #[must_use]
    pub fn new(name: &str, phases: Vec<Phase>) -> Self {
        let (width, height) = DEFAULT_SIZE;
        Self {
            name: name.to_owned(),
            width,
            height,
            fps: 30,
            codec: CodecKind::H264,
            phases,
            network: vec![(0, Link::lan())],
            profile: Profile::STABLE,
            start_bps: 4_000_000,
            max_bps: 30_000_000,
            seed: 1,
        }
    }
}

/// 一个阶段的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseResult {
    /// 操作。
    pub action: Action,
    /// 这个阶段发出的数据报字节数。
    pub bytes: u64,
    /// 平均码率（kbps）。
    pub kbps: u64,
    /// 显示的帧数。
    pub frames_shown: u32,
    /// 采集到显示的延迟 p50 / p95（毫秒）。
    pub latency_ms: (f64, f64),
    /// 最长等待（毫秒）：源画面变了之后，过了多久控制端才看到新画面。
    /// 正常时约等于延迟；丢包、拥塞时画面停住，这个值就变大。
    pub max_wait_ms: f64,
    /// 卡顿次数（等待超过 [`FREEZE_US`]）。
    pub freezes: u32,
    /// 阶段结束时显示的画面的亮度 PSNR（dB）。
    pub end_psnr: f64,
    /// 停下来的阶段（[`Action::Idle`]）：从阶段开始到画面清晰（PSNR ≥ [`SHARP_DB`]）用了多久（毫秒）。
    /// `None` 表示阶段内没清晰，或者不是停下来的阶段。
    pub sharp_ms: Option<f64>,
    /// 阶段结束时的源画面和显示的画面，用于截图。
    pub snapshot: (Screen, Screen),
}

/// 一个场景的结果。
#[derive(Debug, Clone)]
pub struct Outcome {
    /// 场景名。
    pub name: String,
    /// 编码器。
    pub codec: CodecKind,
    /// 每个阶段。
    pub phases: Vec<PhaseResult>,
    /// 发出的关键帧数。
    pub keyframes: u32,
    /// 判定丢失、让编码器恢复的次数。
    pub recoveries: u32,
    /// 接收端因为缺参考帧没解的帧数。
    pub skipped: u32,
    /// 接收端解码失败的帧数。
    pub bad: u32,
    /// 网络计数。
    pub net: NetStats,
    /// 全程最长等待（毫秒）。
    pub max_wait_ms: f64,
    /// 全程卡顿次数。
    pub freezes: u32,
}

/// 亮度 PSNR（dB）。尺寸不同时返回 0。
#[must_use]
pub fn luma_psnr(a: &Screen, b: &Screen) -> f64 {
    if (a.width(), a.height()) != (b.width(), b.height()) || a.pixels().is_empty() {
        return 0.0;
    }
    let luma = |p: u32| {
        0.299 * f64::from((p >> 16) & 0xff)
            + 0.587 * f64::from((p >> 8) & 0xff)
            + 0.114 * f64::from(p & 0xff)
    };
    let sum: f64 = a
        .pixels()
        .iter()
        .zip(b.pixels())
        .map(|(x, y)| (luma(*x) - luma(*y)).powi(2))
        .sum();
    let count = f64::from(u32::try_from(a.pixels().len()).unwrap_or(u32::MAX));
    let mse = sum / count;
    if mse <= 1e-9 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 / mse).log10()
}

fn ms(us: u64) -> f64 {
    f64::from(u32::try_from(us).unwrap_or(u32::MAX)) / 1000.0
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let index = sorted.len().saturating_sub(1).saturating_mul(pct) / 100;
    sorted.get(index).copied().unwrap_or(0)
}

/// 正在统计的阶段。
#[derive(Debug)]
struct PhaseAcc {
    start: Time,
    bytes: u64,
    shown: u32,
    latencies: Vec<u64>,
    max_wait: u64,
    freezes: u32,
    sharp_at: Option<Time>,
}

impl PhaseAcc {
    const fn new(start: Time) -> Self {
        Self {
            start,
            bytes: 0,
            shown: 0,
            latencies: Vec::new(),
            max_wait: 0,
            freezes: 0,
            sharp_at: None,
        }
    }
}

/// 运行一个场景。
#[allow(clippy::too_many_lines)]
pub fn run(s: &Scenario) -> Result<Outcome, Error> {
    if s.fps == 0 || s.phases.is_empty() {
        return Err(Error::InvalidArgument("帧率和阶段都不能为空"));
    }
    let interval = 1_000_000 / u64::from(s.fps);
    let mut desk = RealisticDesktop::new(s.width, s.height, s.seed)?;
    let (encoder, decoder) = s.codec.make(s.fps, s.max_bps)?;
    let cfg = SenderConfig {
        fps: s.fps,
        start_bps: s.start_bps,
        max_bps: s.max_bps,
        profile: s.profile,
        ..SenderConfig::default()
    };
    let mut tx = MediaSender::new(cfg, encoder);
    let mut rx = MediaReceiver::new(Time::ZERO, cfg.deadline_us, decoder);
    let mut net: Net<Vec<u8>> = Net::new(s.seed);
    let mut network = s.network.clone();
    network.sort_by_key(|(t, _)| *t);
    let mut next_link = 0usize;
    let mut link = Link::lan();

    // 每一帧的采集时间，按帧号。
    let mut captured: Vec<Time> = vec![Time::ZERO];
    let mut shown: Option<Screen> = None;
    let mut results = Vec::new();
    let (mut seq, mut fb_seq) = (0u32, 0u32);
    let mut overall_wait = 0u64;
    // 源画面变了、控制端还没看到新画面：从哪个时刻开始等的。
    let mut waiting_since: Option<Time> = None;
    let mut first = true;
    let mut overall_freezes = 0u32;

    let mut now = Time::ZERO;
    let mut next_feedback = Time(FEEDBACK_US);
    for phase in &s.phases {
        let mut acc = PhaseAcc::new(now);
        // 阶段开始时画面已经清晰（例如上一阶段停下来之后补清晰了）。
        if shown
            .as_ref()
            .is_some_and(|d| luma_psnr(d, desk.screen()) >= SHARP_DB)
        {
            acc.sharp_at = Some(now);
        }
        for _ in 0..phase.frames {
            let tick_end = now.plus(interval);
            // 被控端：这一帧。
            while network.get(next_link).is_some_and(|(t, _)| Time(*t) <= now) {
                if let Some((_, l)) = network.get(next_link) {
                    link = *l;
                    net.set_link(HOST, VIEWER, link);
                }
                next_link += 1;
            }
            // 第一帧总要发：控制端还什么都没有。
            let changed = desk.step(phase.action)? || std::mem::take(&mut first);
            if changed && waiting_since.is_none() {
                waiting_since = Some(now);
            }
            if changed || tx.wants_frame() {
                let rtt = link.delay_us.saturating_mul(2).saturating_add(1_000);
                let before = tx.stats().frames;
                let out = tx.on_frame(now, desk.screen(), changed, rtt)?;
                if tx.stats().frames != before {
                    captured.push(now);
                }
                for o in out {
                    let header = Header {
                        channel: Channel::Video,
                        flags: o.flags,
                        conn_id: 0,
                        seq,
                        send_time: now,
                    };
                    seq = seq.wrapping_add(1);
                    let datagram = encode_datagram(&header, &o.body)?;
                    acc.bytes = acc
                        .bytes
                        .saturating_add(u64::try_from(datagram.len()).unwrap_or(u64::MAX));
                    net.send(HOST, VIEWER, datagram.len(), datagram);
                }
            }
            // 这一帧的时间内：投递数据报、发反馈。
            loop {
                let next = [net.peek_time(), Some(next_feedback)]
                    .into_iter()
                    .flatten()
                    .min()
                    .filter(|t| *t < tick_end);
                let Some(when) = next else { break };
                net.advance_to(when);
                now = when;
                if next_feedback <= now {
                    let fb = rx.feedback(now);
                    let header = Header {
                        channel: Channel::Control,
                        flags: 0,
                        conn_id: 0,
                        seq: fb_seq,
                        send_time: now,
                    };
                    fb_seq = fb_seq.wrapping_add(1);
                    let datagram = encode_datagram(&header, &fb.encode())?;
                    net.send(VIEWER, HOST, datagram.len(), datagram);
                    next_feedback = now.plus(FEEDBACK_US);
                }
                while net.peek_time().is_some_and(|t| t <= now) {
                    let Some(d) = net.pop() else { break };
                    let Ok((header, body)) = decode_datagram(&d.payload) else {
                        continue;
                    };
                    if d.to == HOST {
                        if let Ok(fb) = peerdesk_core::adaptive::Feedback::decode(body) {
                            tx.on_feedback(now, &fb);
                        }
                        continue;
                    }
                    let Ok(Some(frame)) =
                        rx.on_datagram(now, header.send_time, header.seq, header.flags, body)
                    else {
                        continue;
                    };
                    let at = captured
                        .get(usize::try_from(frame.frame_id).unwrap_or(usize::MAX))
                        .copied()
                        .unwrap_or(now);
                    acc.latencies.push(now.since(at));
                    acc.shown += 1;
                    if let Some(since) = waiting_since.take() {
                        let wait = now.since(since);
                        acc.max_wait = acc.max_wait.max(wait);
                        if wait > FREEZE_US {
                            acc.freezes += 1;
                        }
                    }
                    let screen = frame.screen.clone();
                    if acc.sharp_at.is_none() && luma_psnr(&screen, desk.screen()) >= SHARP_DB {
                        acc.sharp_at = Some(now);
                    }
                    shown = Some(screen);
                }
            }
            net.advance_to(tick_end);
            now = tick_end;
        }
        // 阶段结束时还在等：等到现在的时间也算进去。
        if let Some(since) = waiting_since {
            let wait = now.since(since);
            acc.max_wait = acc.max_wait.max(wait);
            if wait > FREEZE_US && acc.max_wait == wait {
                acc.freezes += 1;
            }
        }
        let blank = Screen::new(s.width, s.height, 0);
        let display = shown.clone().unwrap_or(blank);
        let end_psnr = luma_psnr(&display, desk.screen());
        let duration = now.since(acc.start).max(1);
        let mut sorted = acc.latencies.clone();
        sorted.sort_unstable();
        overall_wait = overall_wait.max(acc.max_wait);
        overall_freezes += acc.freezes;
        results.push(PhaseResult {
            action: phase.action,
            bytes: acc.bytes,
            kbps: acc.bytes.saturating_mul(8_000) / duration,
            frames_shown: acc.shown,
            latency_ms: (ms(percentile(&sorted, 50)), ms(percentile(&sorted, 95))),
            max_wait_ms: ms(acc.max_wait),
            freezes: acc.freezes,
            end_psnr,
            sharp_ms: acc
                .sharp_at
                .filter(|_| phase.action == Action::Idle)
                .map(|t| ms(t.since(acc.start))),
            snapshot: (desk.screen().clone(), display),
        });
    }
    let (tx_stats, rx_stats) = (tx.stats(), rx.stats());
    Ok(Outcome {
        name: s.name.clone(),
        codec: s.codec,
        phases: results,
        keyframes: tx_stats.keyframes,
        recoveries: tx_stats.lost,
        skipped: rx_stats.skipped,
        bad: rx_stats.bad,
        net: net.stats(),
        max_wait_ms: ms(overall_wait),
        freezes: overall_freezes,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 一个很小的结果，给报告的测试用。
    pub(crate) fn tiny() -> Outcome {
        run(&Scenario::new(
            "tiny",
            vec![Phase {
                action: Action::Idle,
                frames: 1,
            }],
        ))
        .unwrap()
    }

    #[test]
    fn bad_scenarios_and_edge_cases() {
        let mut s = Scenario::new("x", vec![]);
        assert!(run(&s).is_err(), "没有阶段");
        s.phases = vec![Phase {
            action: Action::Idle,
            frames: 1,
        }];
        s.fps = 0;
        assert!(run(&s).is_err(), "帧率为 0");
        assert_eq!(percentile(&[], 50), 0);
        let a = Screen::new(4, 4, 0);
        assert!((luma_psnr(&a, &a) - 99.0).abs() < 1e-9);
        assert!(luma_psnr(&a, &Screen::new(2, 2, 0)).abs() < 1e-9);
        assert_eq!(CodecKind::Tile.name(), "tile");
    }

    #[test]
    fn a_short_run_with_a_network_change_and_a_dead_link() {
        let mut s = Scenario::new(
            "short",
            vec![
                Phase {
                    action: Action::Typing,
                    frames: 6,
                },
                Phase {
                    action: Action::Idle,
                    frames: 6,
                },
            ],
        );
        // 第 100 ms 起链路断开：画面停住，阶段结束时还在等
        s.network = vec![
            (0, Link::lan()),
            (
                100_000,
                Link {
                    up: false,
                    ..Link::lan()
                },
            ),
        ];
        let o = run(&s).unwrap();
        assert_eq!(o.phases.len(), 2);
        assert!(o.phases.iter().all(|p| p.frames_shown < 10));
    }
}
