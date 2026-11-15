//! 桌面会话仿真：合成桌面 → 编码 → 分片与纠错 → 仿真网络 → 重组 → 解码，外加反馈和码率控制。
//!
//! 除了网络是仿真的，其余都是真实代码。用来回答这类问题：
//! 打字时用多少流量、丢包后多久画面恢复一致、带宽突然减半时会不会卡住。
//! 同一组参数的结果完全相同。

use std::collections::VecDeque;

use crate::adaptive::{DelayTracker, DeliveryLog, Feedback, Profile, RateController};
use crate::codec::{Decoder, Encoder};
use crate::proto::{
    decode_datagram, encode_datagram, Channel, Header, VideoHeader, HEADER_LEN, MAX_DATAGRAM,
    VIDEO_HEADER_LEN,
};
use crate::screen::{SyntheticDesktop, Workload};
use crate::sim::{Link, Net, NetStats};
use crate::transport::{fragment, parity_for, Reassembler, VideoPacket, PARITY_PREFIX};
use crate::wire::{Reader, Writer};
use crate::{Error, Time};

const HOST: usize = 0;
const VIEWER: usize = 1;
/// 包头标志位：这是一个校验分片。
pub const FLAG_PARITY: u8 = 1;

/// 一段操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phase {
    /// 桌面上在发生什么。
    pub workload: Workload,
    /// 持续多少帧。
    pub frames: u32,
}

/// 码率怎么定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateMode {
    /// 每帧固定预算（字节），不看网络状况。
    Fixed(usize),
    /// 根据反馈自适应。
    Adaptive {
        /// 参数。
        profile: Profile,
        /// 起始码率。
        start_bps: u64,
        /// 码率上限。
        max_bps: u64,
    },
}

/// 仿真参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    /// 随机种子。
    pub seed: u64,
    /// 画面宽度。
    pub width: u16,
    /// 画面高度。
    pub height: u16,
    /// 帧率。
    pub fps: u32,
    /// 链路。
    pub link: Link,
    /// 在第几帧把链路换成另一种（模拟带宽突变）。
    pub link_change: Option<(u32, Link)>,
    /// 依次进行的操作。
    pub phases: Vec<Phase>,
    /// 操作结束后再空转多少帧，让画面收敛。
    pub settle_frames: u32,
    /// 码率模式。
    pub rate: RateMode,
    /// 纠错冗余比例（千分比）；自适应模式下用参数里的值。
    pub fec_permille: u16,
    /// 一帧从首个分片到收齐允许的最长时间（微秒）。
    pub deadline_us: u64,
    /// 是否启用丢帧修复（接收端反馈和发送端超时）。关掉用来做对照。
    pub repair: bool,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            seed: 1,
            width: 640,
            height: 360,
            fps: 30,
            link: Link::lan(),
            link_change: None,
            phases: vec![Phase {
                workload: Workload::Typing,
                frames: 60,
            }],
            settle_frames: 30,
            rate: RateMode::Adaptive {
                profile: Profile::STABLE,
                start_bps: 2_000_000,
                max_bps: 20_000_000,
            },
            fec_permille: 0,
            deadline_us: 100_000,
            repair: true,
        }
    }
}

/// 仿真结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// 编码并发出的帧数。
    pub frames_sent: u32,
    /// 完整送达并应用的帧数。
    pub frames_delivered: u32,
    /// 发出的字节数（含包头和纠错）。
    pub bytes_sent: u64,
    /// 每一段操作期间发出的字节数；最后一项是收尾阶段。
    pub phase_bytes: Vec<u64>,
    /// 结束时观看端的画面是否与源完全一致。
    pub converged: bool,
    /// 结束时不一致的像素数。
    pub final_diff: usize,
    /// 源画面最后一次变化之后，观看端多久变得完全一致（微秒）；没收敛时为 `None`。
    pub converge_us: Option<u64>,
    /// 帧延迟中位数（微秒，从发出到应用）。
    pub latency_p50_us: u64,
    /// 帧延迟 95 分位（微秒）。
    pub latency_p95_us: u64,
    /// 码率的最小值、最大值、结束时的值（自适应模式）。
    pub rate_bps: (u64, u64, u64),
    /// 靠纠错恢复的分片数。
    pub recovered: u64,
    /// 被判定丢失而重新发送的帧数。
    pub frames_repaired: u32,
    /// 无法解析或应用的数据报与帧数。
    pub bad_input: u64,
    /// 网络计数。
    pub net: NetStats,
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let index = sorted.len().saturating_sub(1).saturating_mul(pct) / 100;
    sorted.get(index).copied().unwrap_or(0)
}

/// 运行仿真。
#[allow(clippy::too_many_lines)]
pub fn run(p: &Params) -> Result<Report, Error> {
    if p.fps == 0 {
        return Err(Error::InvalidArgument("fps 不能为 0"));
    }
    let interval = 1_000_000 / u64::from(p.fps);
    let mut net: Net<Vec<u8>> = Net::new(p.seed);
    net.set_link(HOST, VIEWER, p.link);

    let mut desktop = SyntheticDesktop::new(p.width, p.height, p.seed);
    let (width, height) = (desktop.screen().width(), desktop.screen().height());
    let mut encoder = Encoder::new(width, height);
    let mut decoder = Decoder::new(width, height);
    let mut reassembler = Reassembler::new(p.deadline_us, 16);
    let mut tracker = DelayTracker::new(Time::ZERO);
    let mut log = DeliveryLog::new();
    let (mut controller, fec) = match p.rate {
        RateMode::Fixed(_) => (None, p.fec_permille),
        RateMode::Adaptive {
            profile,
            start_bps,
            max_bps,
        } => (
            Some(RateController::new(profile, start_bps, 100_000, max_bps)),
            profile.fec_permille,
        ),
    };
    let max_payload = MAX_DATAGRAM - HEADER_LEN - VIDEO_HEADER_LEN - PARITY_PREFIX;
    // 等不到反馈就判定丢失的时间：截止时间加上一个来回，再留一点余量。
    let ack_timeout = p
        .deadline_us
        .saturating_add(p.link.delay_us.saturating_mul(2))
        .saturating_add(p.link.jitter_us.saturating_mul(2))
        .saturating_add(interval.saturating_mul(2));

    let schedule: Vec<Workload> = p
        .phases
        .iter()
        .flat_map(|ph| std::iter::repeat_n(ph.workload, usize::try_from(ph.frames).unwrap_or(0)))
        .collect();
    let total_ticks = schedule
        .len()
        .saturating_add(usize::try_from(p.settle_frames).unwrap_or(0));
    let mut phase_ends: Vec<usize> = Vec::new();
    let mut acc = 0usize;
    for ph in &p.phases {
        acc = acc.saturating_add(usize::try_from(ph.frames).unwrap_or(0));
        phase_ends.push(acc);
    }
    let mut phase_bytes = vec![0u64; p.phases.len().saturating_add(1)];

    let (mut tick, mut frame_id, mut video_seq, mut feedback_seq) = (0usize, 0u32, 0u32, 0u32);
    let mut unacked: VecDeque<(u32, Time)> = VecDeque::new();
    let mut acked_floor = 0u32;
    let mut sent_at: Vec<Time> = vec![Time::ZERO];
    let mut latencies: Vec<u64> = Vec::new();
    let (mut bytes_sent, mut bad_input, mut frames_repaired) = (0u64, 0u64, 0u32);
    let mut frames_delivered = 0u32;
    let mut last_change = Time::ZERO;
    let mut converged_at: Option<Time> = None;
    let mut rates = (u64::MAX, 0u64, 0u64);
    let mut next_tick = Some(Time::ZERO);
    let mut next_feedback = Some(Time(interval / 2));
    // 所有帧都发完之后，再等这么久让在途的数据和反馈走完。
    let drain = ack_timeout.saturating_mul(3);
    let mut end_at: Option<Time> = None;

    loop {
        let next = [net.peek_time(), next_tick, next_feedback]
            .into_iter()
            .flatten()
            .min();
        let Some(when) = next else { break };
        if end_at.is_some_and(|end| when > end) {
            break;
        }
        net.advance_to(when);
        let now = net.now();

        // 被控端：到了一帧的时间。
        if next_tick.is_some_and(|t| t <= now) {
            if let Some((at, link)) = p.link_change {
                if usize::try_from(at).unwrap_or(usize::MAX) == tick {
                    net.set_link(HOST, VIEWER, link);
                }
            }
            let workload = schedule.get(tick).copied().unwrap_or(Workload::Idle);
            let before = desktop.screen().clone();
            desktop.step(workload);
            if desktop.screen() != &before {
                last_change = now;
                converged_at = None;
            }
            // 太久没有反馈的帧：按丢失处理。
            if p.repair {
                while let Some((id, at)) = unacked.front().copied() {
                    if id <= acked_floor {
                        unacked.pop_front();
                    } else if now.since(at) >= ack_timeout {
                        unacked.pop_front();
                        encoder.mark_lost(id);
                        frames_repaired = frames_repaired.saturating_add(1);
                    } else {
                        break;
                    }
                }
            }
            let budget = match (&controller, p.rate) {
                (Some(rc), _) => rc.frame_budget(p.fps),
                (None, RateMode::Fixed(bytes)) => bytes,
                (None, RateMode::Adaptive { .. }) => usize::MAX,
            };
            if let Some(encoded) =
                encoder.encode(frame_id.saturating_add(1), desktop.screen(), budget)
            {
                frame_id = frame_id.saturating_add(1);
                let mut packets = fragment(frame_id, frame_id, &encoded.data, max_payload)?;
                let parity = parity_for(&packets, fec);
                packets.extend(parity);
                let phase = phase_ends
                    .iter()
                    .position(|end| tick < *end)
                    .unwrap_or(p.phases.len());
                for packet in packets {
                    let header = Header {
                        channel: Channel::Video,
                        flags: if packet.parity { FLAG_PARITY } else { 0 },
                        conn_id: 0,
                        seq: video_seq,
                        send_time: now,
                    };
                    video_seq = video_seq.wrapping_add(1);
                    let mut body = Writer::new();
                    packet.header.encode(&mut body);
                    body.bytes(&packet.data);
                    let datagram = encode_datagram(&header, &body.into_vec())?;
                    let size = u64::try_from(datagram.len()).unwrap_or(u64::MAX);
                    bytes_sent = bytes_sent.saturating_add(size);
                    if let Some(slot) = phase_bytes.get_mut(phase) {
                        *slot = slot.saturating_add(size);
                    }
                    net.send(HOST, VIEWER, datagram.len(), datagram);
                }
                sent_at.push(now);
                unacked.push_back((frame_id, now));
            }
            tick = tick.saturating_add(1);
            next_tick = if tick < total_ticks {
                Some(now.plus(interval))
            } else {
                end_at = Some(now.plus(drain));
                None
            };
        }

        // 观看端：定期发反馈。
        if next_feedback.is_some_and(|t| t <= now) {
            reassembler.expire(now);
            if let Some(floor) = reassembler.floor() {
                log.advance(floor);
            }
            let sample = tracker.report(now);
            let (floor, delivered) = log.snapshot();
            let feedback = Feedback {
                floor,
                delivered,
                queue_delay_us: sample.queue_delay_us,
                loss_permille: sample.loss_permille,
                recv_bytes_per_s: sample.recv_bytes_per_s,
            };
            let header = Header {
                channel: Channel::Control,
                flags: 0,
                conn_id: 0,
                seq: feedback_seq,
                send_time: now,
            };
            feedback_seq = feedback_seq.wrapping_add(1);
            let datagram = encode_datagram(&header, &feedback.encode())?;
            net.send(VIEWER, HOST, datagram.len(), datagram);
            next_feedback = Some(now.plus(interval));
        }

        while net.peek_time().is_some_and(|t| t <= now) {
            let Some(delivery) = net.pop() else { break };
            let Ok((header, payload)) = decode_datagram(&delivery.payload) else {
                bad_input = bad_input.saturating_add(1);
                continue;
            };
            match (delivery.to, header.channel) {
                (VIEWER, Channel::Video) => {
                    tracker.on_packet(now, header.send_time, header.seq, delivery.payload.len());
                    let mut r = Reader::new(payload);
                    let packet = VideoHeader::decode(&mut r).map(|video| VideoPacket {
                        header: video,
                        parity: header.flags & FLAG_PARITY != 0,
                        data: r.rest().to_vec(),
                    });
                    match packet.and_then(|pk| reassembler.push(now, pk)) {
                        Ok(Some(frame)) => {
                            if decoder.apply(&frame.data).is_ok() {
                                frames_delivered = frames_delivered.saturating_add(1);
                                log.delivered(frame.frame_id);
                                let index = usize::try_from(frame.frame_id).unwrap_or(usize::MAX);
                                if let Some(sent) = sent_at.get(index) {
                                    latencies.push(now.since(*sent));
                                }
                                if converged_at.is_none() && decoder.screen() == desktop.screen() {
                                    converged_at = Some(now);
                                }
                            } else {
                                bad_input = bad_input.saturating_add(1);
                            }
                        }
                        Ok(None) => {}
                        Err(_) => bad_input = bad_input.saturating_add(1),
                    }
                }
                (HOST, Channel::Control) => match Feedback::decode(payload) {
                    Ok(feedback) => {
                        if p.repair {
                            encoder.on_feedback(feedback.floor, feedback.delivered);
                            acked_floor = acked_floor.max(feedback.floor);
                        }
                        if let Some(rc) = controller.as_mut() {
                            let rate = rc.on_feedback(now, &feedback);
                            rates = (rates.0.min(rate), rates.1.max(rate), rate);
                        }
                    }
                    Err(_) => bad_input = bad_input.saturating_add(1),
                },
                (_, _) => bad_input = bad_input.saturating_add(1),
            }
        }
    }

    latencies.sort_unstable();
    let final_diff = decoder.screen().diff(desktop.screen());
    if rates.0 == u64::MAX {
        rates = (0, 0, 0);
    }
    Ok(Report {
        frames_sent: frame_id,
        frames_delivered,
        bytes_sent,
        phase_bytes,
        converged: final_diff == 0,
        final_diff,
        converge_us: converged_at
            .filter(|_| final_diff == 0)
            .map(|at| at.since(last_change)),
        latency_p50_us: percentile(&latencies, 50),
        latency_p95_us: percentile(&latencies, 95),
        rate_bps: rates,
        recovered: reassembler.stats().recovered,
        frames_repaired,
        bad_input,
        net: net.stats(),
    })
}
