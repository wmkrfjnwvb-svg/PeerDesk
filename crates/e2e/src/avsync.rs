//! 音画同步和播放缓冲的仿真测量（见 `docs/research/N-av-sync.md`）。
//!
//! 和 [`crate::run`] 一样用产品的发送端、接收端和真实的编码器，画面是桌面上放视频；另外加一路声音
//! （每 10 ms 一个包，大小和 96 kbps 的 Opus 一样），和画面走同一条仿真链路。控制端这一侧和产品的会话循环
//! 一样接：收齐的帧和声音包记时钟差（[`AvSync`]），解出的画面进播放缓冲（[`FrameQueue`]），声音每 10 ms
//! 取一帧（打开时 [`JitterBuffer::pop_synced`]，关着时 [`JitterBuffer::pop`]）。
//!
//! 量三件事：
//! - **画面匀不匀**：相邻两帧的显示间隔和它们的采集间隔差多少（差是 0 就是原样重现）；
//! - **音画差**：同一时刻，画面的延迟（显示 − 采集）减声音的延迟（播放 − 采集）。正数是声音早于画面；
//! - **代价**：画面和声音各自的延迟。
//!
//! 没有模拟的：解码用时（仿真里解码不占时间）、声卡的输出延迟、显示器的刷新时刻。

use peerdesk_core::adaptive::Profile;
use peerdesk_core::audio::{AudioPacket, AudioStats, JitterBuffer, Play};
use peerdesk_core::avsync::{AvSync, CaptureClock, FrameQueue};
use peerdesk_core::error::Error;
use peerdesk_core::media::{DecodeWorker, MediaReceiver, MediaSender, SenderConfig};
use peerdesk_core::proto::{decode_datagram, encode_datagram, Channel, Header};
use peerdesk_core::sim::{Link, Net};
use peerdesk_core::time::Time;

use crate::{
    ms, percentile, send_ready, Action, CodecKind, ContentPolicy, EstimatorKind, RealisticDesktop,
    CC_TICK_US, DEFAULT_SIZE, FEEDBACK_US, HOST, VIEWER,
};

/// 声音一帧多长（微秒）。
const AUDIO_FRAME_US: u64 = 10_000;
/// 一个声音包的负载字节数：96 kbps 的 Opus 一帧 10 ms 是 120 字节。
const AUDIO_FRAME_BYTES: usize = 120;
/// 控制端的声音节拍比被控端的采集节拍错开多少（微秒）：两端的节拍本来就不对齐。
const PLAY_PHASE_US: u64 = 3_000;
/// 声音早于画面超过这么多能察觉（微秒，ITU-R BT.1359）。
pub const SOUND_EARLY_US: i64 = 45_000;
/// 声音晚于画面超过这么多能察觉（微秒，ITU-R BT.1359）。
pub const SOUND_LATE_US: i64 = 125_000;

/// 一次测量的条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AvScenario {
    /// 画面尺寸。
    pub width: u16,
    /// 画面高度。
    pub height: u16,
    /// 帧率。
    pub fps: u32,
    /// 放多少帧视频。
    pub frames: u32,
    /// 链路（两个方向一样）。
    pub link: Link,
    /// 播放延迟（毫秒）；0 表示关：画面收到就显示，声音按水位。
    pub playout_ms: u32,
    /// 开头这么久不计入结果（微秒）：码率爬升、缓冲起步。
    pub warmup_us: u64,
    /// 随机种子。
    pub seed: u64,
}

impl AvScenario {
    /// 默认：30 帧、20 秒、局域网、播放缓冲关。
    #[must_use]
    pub const fn new(link: Link, playout_ms: u32) -> Self {
        Self {
            width: DEFAULT_SIZE.0,
            height: DEFAULT_SIZE.1,
            fps: 30,
            frames: 600,
            link,
            playout_ms,
            warmup_us: 3_000_000,
            seed: 1,
        }
    }
}

/// 一次测量的结果（时间都是毫秒，只算开头之后的）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AvOutcome {
    /// 显示的画面帧数。
    pub shown: u32,
    /// 画面延迟（采集到显示）p50 / p95。
    pub video_latency_ms: (f64, f64),
    /// 显示间隔和采集间隔的差（绝对值）：平均 / p95 / 最大。0 表示节奏原样重现。
    pub interval_error_ms: (f64, f64, f64),
    /// 到的时候已经过了播放时刻的画面帧数（播放延迟装不下）。
    pub late: u64,
    /// 声音延迟（采集到播放）p50 / p95。
    pub audio_latency_ms: (f64, f64),
    /// 音画差（画面延迟 − 声音延迟；正数是声音早于画面）：最小 / 中位 / 最大。
    pub av_diff_ms: (f64, f64, f64),
    /// 音画差能察觉的画面帧数（声音早超过 45 ms 或晚超过 125 ms）。
    pub out_of_sync: u32,
    /// 声音抖动缓冲的计数（全程）。
    pub audio: AudioStats,
}

/// 有符号的微秒换成毫秒。
fn signed_ms(us: i64) -> f64 {
    let magnitude = ms(us.unsigned_abs());
    if us < 0 {
        -magnitude
    } else {
        magnitude
    }
}

fn signed(us: u64) -> i64 {
    i64::try_from(us).unwrap_or(i64::MAX)
}

/// 控制端这一侧：和产品的会话循环（`peerdesk_net::controller`）一样的接法。
struct Viewer {
    rx: MediaReceiver,
    worker: DecodeWorker,
    /// 时钟差一直记；`on` 时才按它排画面和声音。
    sync: Option<AvSync>,
    on: bool,
    /// 等播放时刻的画面：采集时刻。
    held: FrameQueue<u64>,
    jitter: JitterBuffer,
    /// 显示的画面：（显示时刻，采集时刻）。
    pictures: Vec<(u64, u64)>,
    /// 播放的声音帧：（播放时刻，采集时刻）。
    sounds: Vec<(u64, u64)>,
}

impl Viewer {
    fn on_video(&mut self, now: Time, header: &Header, body: &[u8]) {
        // 分片不合法的数据报不理会（仿真链路不会改包）
        let jobs = self
            .rx
            .on_datagram_deferred(now, header.send_time, header.seq, header.flags, body)
            .unwrap_or_default();
        for job in jobs {
            // 被控端每一帧都带采集时刻（见 run_av）
            let capture = job.capture_us.unwrap_or(now.micros());
            if let Some(sync) = self.sync.as_mut() {
                sync.observe_picture(now.micros(), capture);
            }
            match self.worker.decode(&job) {
                Ok(Some(_)) => {
                    self.rx.decode_succeeded();
                    let due = self
                        .sync
                        .as_ref()
                        .filter(|_| self.on)
                        .and_then(|s| s.due_us(capture));
                    self.held.push(now.micros(), due, capture);
                }
                Ok(None) => {}
                Err(_) => self.rx.decode_failed(),
            }
        }
        self.show_due(now);
    }

    fn show_due(&mut self, now: Time) {
        while let Some(capture) = self.held.pop_due(now.micros()) {
            self.pictures.push((now.micros(), capture));
        }
    }

    fn on_audio(&mut self, now: Time, body: &[u8]) {
        if let Ok(packet) = AudioPacket::decode(body) {
            if let Some(sync) = self.sync.as_mut() {
                sync.observe_sound(now.micros(), packet.capture_us);
            }
            self.jitter.push(now.micros(), packet);
        }
    }

    /// 每 10 ms 一拍。`captures` 是每个声音帧的采集时刻（按序号）。
    fn audio_tick(&mut self, now: Time, captures: &[u64]) {
        let lead = self
            .sync
            .as_ref()
            .filter(|_| self.on)
            .zip(self.jitter.next_capture_us())
            .and_then(|(s, capture)| s.lead_us(now.micros(), capture));
        let play = match lead {
            Some(lead) => self.jitter.pop_synced(lead),
            None => self.jitter.pop(),
        };
        // 加速时两帧压成一帧，这一拍结束时播到的是第二帧
        let frame = match play {
            Play::Normal(frame) | Play::Accelerate(_, frame) => frame,
            Play::Expand | Play::Wait => return,
        };
        let seq = frame
            .first_chunk::<4>()
            .map_or(u32::MAX, |b| u32::from_be_bytes(*b));
        if let Some(capture) = usize::try_from(seq).ok().and_then(|i| captures.get(i)) {
            self.sounds.push((now.micros(), *capture));
        }
    }
}

/// 把逐帧记录算成结果。
fn summarize(viewer: &Viewer, warmup_us: u64) -> AvOutcome {
    let pictures: Vec<(u64, u64)> = viewer
        .pictures
        .iter()
        .copied()
        .filter(|(at, _)| *at >= warmup_us)
        .collect();
    let mut video: Vec<u64> = pictures
        .iter()
        .map(|(at, c)| at.saturating_sub(*c))
        .collect();
    video.sort_unstable();
    let mut errors: Vec<u64> = pictures
        .iter()
        .zip(pictures.iter().skip(1))
        .map(|((d0, c0), (d1, c1))| {
            signed(d1.saturating_sub(*d0))
                .saturating_sub(signed(c1.saturating_sub(*c0)))
                .unsigned_abs()
        })
        .collect();
    errors.sort_unstable();
    let count = u64::try_from(errors.len()).unwrap_or(u64::MAX).max(1);
    let mean_error = errors.iter().sum::<u64>() / count;
    let mut audio: Vec<u64> = viewer
        .sounds
        .iter()
        .filter(|(at, _)| *at >= warmup_us)
        .map(|(at, c)| at.saturating_sub(*c))
        .collect();
    audio.sort_unstable();
    // 每个画面帧显示的那一刻，正在播的声音帧的延迟
    let mut diffs: Vec<i64> = pictures
        .iter()
        .filter_map(|(at, c)| {
            let playing = viewer.sounds.partition_point(|(t, _)| t <= at);
            let (t, sound) = viewer.sounds.get(playing.checked_sub(1)?)?;
            Some(signed(at.saturating_sub(*c)).saturating_sub(signed(t.saturating_sub(*sound))))
        })
        .collect();
    diffs.sort_unstable();
    let out_of_sync = diffs
        .iter()
        .filter(|d| **d > SOUND_EARLY_US || **d < -SOUND_LATE_US)
        .count();
    AvOutcome {
        shown: u32::try_from(pictures.len()).unwrap_or(u32::MAX),
        video_latency_ms: (ms(percentile(&video, 50)), ms(percentile(&video, 95))),
        interval_error_ms: (
            ms(mean_error),
            ms(percentile(&errors, 95)),
            ms(errors.last().copied().unwrap_or(0)),
        ),
        late: viewer.held.stats().late,
        audio_latency_ms: (ms(percentile(&audio, 50)), ms(percentile(&audio, 95))),
        av_diff_ms: (
            signed_ms(diffs.first().copied().unwrap_or(0)),
            signed_ms(diffs.get(diffs.len() / 2).copied().unwrap_or(0)),
            signed_ms(diffs.last().copied().unwrap_or(0)),
        ),
        out_of_sync: u32::try_from(out_of_sync).unwrap_or(u32::MAX),
        audio: viewer.jitter.stats(),
    }
}

/// 跑一次：桌面上放视频，同时有声音。
#[allow(clippy::too_many_lines)]
pub fn run_av(s: &AvScenario) -> Result<AvOutcome, Error> {
    if s.fps == 0 || s.frames == 0 {
        return Err(Error::InvalidArgument("帧率和帧数都不能为 0"));
    }
    let interval = 1_000_000 / u64::from(s.fps);
    let policy = ContentPolicy::OFFICE;
    let max_bps = 30_000_000;
    let mut desk = RealisticDesktop::new(s.width, s.height, s.seed)?;
    let (encoder, decoder) = CodecKind::H264.make(s.fps, max_bps, &policy)?;
    let cfg = SenderConfig {
        fps: s.fps,
        start_bps: policy.start_bps,
        max_bps,
        profile: Profile::STABLE,
        estimator: EstimatorKind::PRODUCT,
        policy,
        ..SenderConfig::default()
    };
    let fec = peerdesk_core::transport::Fec::Rs;
    let mut tx = MediaSender::new(cfg, encoder);
    tx.set_fec(fec);
    tx.set_reports(true);
    tx.set_idle_probe(true);
    // 产品里双方都有 AV_SYNC 能力位就带采集时刻（控制端开不开播放缓冲都带）
    tx.set_capture_stamps(true);
    let mut rx = MediaReceiver::detached(Time::ZERO, cfg.deadline_us);
    rx.set_keyframe_grace(true);
    rx.set_fec(fec);
    // 估计器不用到达报告时两端都不开（和 crate::run 一样）
    rx.set_reports(tx.reports());
    let mut viewer = Viewer {
        rx,
        worker: DecodeWorker::new(decoder),
        sync: AvSync::new(s.playout_ms.max(1)),
        on: s.playout_ms > 0,
        held: FrameQueue::default(),
        jitter: JitterBuffer::new(),
        pictures: Vec::new(),
        sounds: Vec::new(),
    };
    let mut net: Net<Vec<u8>> = Net::new(s.seed);
    net.set_link(HOST, VIEWER, s.link);
    let mut pacer = peerdesk_core::pacer::Pacer::new(Time(0));
    let pacing = peerdesk_core::pacer::enabled();
    let mut audio_clock = CaptureClock::default();
    let mut audio_captures: Vec<u64> = Vec::new();

    let (mut seq, mut fb_seq, mut audio_seq) = (0u32, 0u32, 0u32);
    let mut bytes = 0u64;
    let mut first = true;
    let mut pending = false;
    let mut now = Time::ZERO;
    let mut next_feedback = Time(FEEDBACK_US);
    let mut next_tick = Time::ZERO;
    // 声卡录完一帧才交出来：第 n 帧在 (n + 1) × 10 ms 交来
    let mut next_capture = Time(AUDIO_FRAME_US);
    let mut next_play = Time(PLAY_PHASE_US);
    for _ in 0..s.frames {
        let tick_end = now.plus(interval);
        let changed = desk.step(Action::Video)? || std::mem::take(&mut first);
        pending |= changed;
        if changed {
            tx.screen_changed(now);
        }
        tx.expire_unacked(now);
        if (pending || tx.wants_frame())
            && !tx.in_flight_full(now)
            && !pacer.busy(tx.pacing_bps(pacing), interval)
            && tx.due(now)
        {
            let rtt = s.link.delay_us.saturating_mul(2).saturating_add(1_000);
            let out = tx.on_frame(now, desk.screen(), std::mem::take(&mut pending), rtt)?;
            pacer.push(out);
            send_ready(
                &mut pacer, &mut tx, pacing, &mut net, now, &mut seq, &mut bytes,
            )?;
        }
        loop {
            let next = [
                net.peek_time(),
                Some(next_feedback),
                Some(next_tick),
                Some(next_capture),
                Some(next_play),
                viewer.held.next_due().map(Time),
                pacer.next_due(now, tx.pacing_bps(pacing)),
            ]
            .into_iter()
            .flatten()
            .min()
            .filter(|t| *t < tick_end);
            let Some(when) = next else { break };
            // 播放缓冲里已经到时间的帧：时刻不往回走
            let when = when.max(now);
            net.advance_to(when);
            now = when;
            if next_tick <= now {
                tx.on_tick(now);
                for cluster in tx.take_probes() {
                    pacer.push_probe(cluster);
                }
                next_tick = now.plus(CC_TICK_US);
            }
            send_ready(
                &mut pacer, &mut tx, pacing, &mut net, now, &mut seq, &mut bytes,
            )?;
            if next_capture <= now {
                // 被控端：一帧声音，采集时刻和画面帧同一个时钟
                let index = u64::from(audio_seq);
                let capture_us = audio_clock.stamp(now.micros(), index, AUDIO_FRAME_US);
                audio_captures.push(capture_us);
                let mut frame = audio_seq.to_be_bytes().to_vec();
                frame.resize(AUDIO_FRAME_BYTES, 0);
                let packet = AudioPacket {
                    seq: audio_seq,
                    capture_us,
                    frame,
                    redundant: Vec::new(),
                };
                let header = Header {
                    channel: Channel::Audio,
                    flags: 0,
                    conn_id: 0,
                    seq: audio_seq,
                    send_time: now,
                };
                audio_seq = audio_seq.wrapping_add(1);
                let datagram = encode_datagram(&header, &packet.encode()?)?;
                net.send(HOST, VIEWER, datagram.len(), datagram);
                next_capture = next_capture.plus(AUDIO_FRAME_US);
            }
            if next_feedback <= now {
                let fb = viewer.rx.feedback_datagram(now);
                let header = Header {
                    channel: Channel::Control,
                    flags: 0,
                    conn_id: 0,
                    seq: fb_seq,
                    send_time: now,
                };
                fb_seq = fb_seq.wrapping_add(1);
                let datagram = encode_datagram(&header, &fb)?;
                net.send(VIEWER, HOST, datagram.len(), datagram);
                next_feedback = now.plus(FEEDBACK_US);
            }
            while net.peek_time().is_some_and(|t| t <= now) {
                let Some(d) = net.pop() else { break };
                let Ok((header, body)) = decode_datagram(&d.payload) else {
                    continue;
                };
                if d.to == HOST {
                    let again = tx.on_feedback_datagram(now, body).unwrap_or_default();
                    pacer.retain(|o| tx.wanted(o));
                    pacer.push_front(again);
                    send_ready(
                        &mut pacer, &mut tx, pacing, &mut net, now, &mut seq, &mut bytes,
                    )?;
                } else if header.channel == Channel::Audio {
                    viewer.on_audio(now, body);
                } else {
                    viewer.on_video(now, &header, body);
                }
            }
            viewer.show_due(now);
            if next_play <= now {
                viewer.audio_tick(now, &audio_captures);
                next_play = next_play.plus(AUDIO_FRAME_US);
            }
        }
        net.advance_to(tick_end);
        now = tick_end;
    }
    Ok(summarize(&viewer, s.warmup_us))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 抖动 30 ms 的链路（家里隔着墙的 Wi-Fi）。
    fn jittery() -> Link {
        Link {
            delay_us: 20_000,
            jitter_us: 30_000,
            rate_bps: Some(20_000_000),
            ..Link::ideal()
        }
    }

    fn short(playout_ms: u32) -> AvScenario {
        AvScenario {
            frames: 150,
            warmup_us: 1_500_000,
            ..AvScenario::new(jittery(), playout_ms)
        }
    }

    #[test]
    fn playout_buffer_evens_out_the_picture_and_lines_up_the_sound() {
        let off = run_av(&short(0)).unwrap();
        let on = run_av(&short(150)).unwrap();
        // 关着：显示间隔跟着网络抖动走，画面和声音各走各的
        assert!(off.interval_error_ms.1 > 5.0, "{off:?}");
        assert_eq!(off.late, 0, "{off:?}");
        // 打开：显示间隔和采集间隔一样，音画差在声音一帧之内
        assert!(on.shown >= 60, "{on:?}");
        assert!(on.interval_error_ms.1 < 1.0, "{on:?}");
        assert_eq!((on.late, on.out_of_sync), (0, 0), "{on:?}");
        assert!(
            on.av_diff_ms.0 >= -15.0 && on.av_diff_ms.2 <= 15.0,
            "{on:?}"
        );
        assert_eq!(on.audio.concealed, 0, "{on:?}");
        // 代价：画面多等了播放延迟
        assert!(on.video_latency_ms.0 > off.video_latency_ms.0 + 50.0);
        // 同一个种子结果一样
        assert_eq!(run_av(&short(150)).unwrap(), on);
    }

    #[test]
    fn delay_too_small_for_the_jitter_shows_up_as_late_frames() {
        // 播放延迟 10 ms 装不下 30 ms 的抖动：帧到的时候已经过了时刻，马上显示并计数
        let tight = run_av(&short(10)).unwrap();
        assert!(tight.late > 0, "{tight:?}");
        assert!(tight.shown >= 60, "{tight:?}");
    }

    #[test]
    fn bad_input_and_helpers() {
        let mut s = short(0);
        s.fps = 0;
        assert!(run_av(&s).is_err());
        let mut s = short(0);
        s.frames = 0;
        assert!(run_av(&s).is_err());
        assert!((signed_ms(-1500) + 1.5).abs() < 1e-9);
        assert!((signed_ms(2500) - 2.5).abs() < 1e-9);
    }
}
