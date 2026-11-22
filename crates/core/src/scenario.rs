//! 端到端仿真场景：握手，然后被控端按帧率向控制端发送视频帧。
//!
//! 走的是真实的编解码、加密、分片、重组代码，只有网络是仿真的。
//! 测试和命令行（`peerdesk sim`）共用这一个入口，同一组参数结果完全相同。

use crate::auth::Auth;
use crate::crypto::{Policy, Sealer, Suite};
use crate::proto::{
    decode_datagram, encode_datagram, Caps, Channel, Control, Header, RejectReason, VideoHeader,
    HEADER_LEN, MAX_DATAGRAM, VIDEO_HEADER_LEN,
};
use crate::session::{Session, SessionConfig, State};
use crate::sim::{Link, Net, NetStats};
use crate::trace::{Stage, TraceId, Tracer};
use crate::transport::{fragment, Reassembler, VideoPacket};
use crate::wire::{Reader, Writer};
use crate::{Error, Time};

const CONTROLLER: usize = 0;
const HOST: usize = 1;
/// Hello 的重发间隔（微秒）。
const HELLO_RETRY_US: u64 = 200_000;
/// Hello 最多发送次数。
const HELLO_ATTEMPTS: u32 = 20;
/// 发送方向标识：被控端 → 控制端。
const DIR_HOST_TO_CONTROLLER: u32 = 1;

/// 场景参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    /// 随机种子。
    pub seed: u64,
    /// 两端之间的链路。
    pub link: Link,
    /// 发送的帧数。
    pub frames: u32,
    /// 每帧字节数。
    pub frame_bytes: usize,
    /// 帧率。
    pub fps: u32,
    /// 一帧从首个分片到收齐允许的最长时间（微秒）。
    pub deadline_us: u64,
    /// 控制端的加密策略。
    pub controller_policy: Policy,
    /// 被控端的加密策略。
    pub host_policy: Policy,
    /// 数据报在途中被翻转一个比特的概率（千分比），模拟损坏或篡改。
    pub corrupt_permille: u16,
    /// 控制端持有的密码。
    pub controller_auth: Auth,
    /// 被控端要求的密码。
    pub host_auth: Auth,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            seed: 1,
            link: Link::lan(),
            frames: 120,
            frame_bytes: 6_000,
            fps: 60,
            deadline_us: 50_000,
            controller_policy: Policy::recommended(),
            host_policy: Policy::recommended(),
            corrupt_permille: 0,
            controller_auth: Auth::Open,
            host_auth: Auth::Open,
        }
    }
}

/// 场景结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// 会话是否建立。
    pub established: bool,
    /// 选中的加密套件。
    pub suite: Option<Suite>,
    /// 被拒绝时的原因。
    pub rejected: Option<RejectReason>,
    /// 从首次发出 Hello 到控制端确认建立的耗时（微秒）。
    pub handshake_us: u64,
    /// Hello 发送次数。
    pub hello_attempts: u32,
    /// 发出的帧数。
    pub frames_sent: u32,
    /// 完整送达的帧数。
    pub frames_delivered: u32,
    /// 送达但内容与发出时不一致的帧数。启用加密时必须为 0。
    pub frames_corrupted: u32,
    /// 帧延迟的中位数（微秒，从发出到收齐）。
    pub latency_p50_us: u64,
    /// 帧延迟的 95 分位（微秒）。
    pub latency_p95_us: u64,
    /// 无法解析或认证失败而被丢弃的数据报数。
    pub bad_datagrams: u64,
    /// 网络计数。
    pub net: NetStats,
    /// 控制端一侧的业务路径（去掉逐帧事件）。
    pub controller_path: Vec<Stage>,
    /// 被控端一侧的业务路径（去掉逐帧事件）。
    pub host_path: Vec<Stage>,
    /// 完整的追踪文本。
    pub trace_dump: String,
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = (sorted.len().saturating_sub(1)).saturating_mul(pct) / 100;
    sorted.get(index).copied().unwrap_or(0)
}

fn without_frames(path: Vec<Stage>) -> Vec<Stage> {
    path.into_iter()
        .filter(|s| match s {
            Stage::FrameSent | Stage::FrameDelivered | Stage::FrameExpired => false,
            Stage::HelloSent
            | Stage::HelloReceived
            | Stage::SuiteNegotiated
            | Stage::AuthRequested
            | Stage::AuthPassed
            | Stage::AuthFailed
            | Stage::AcceptSent
            | Stage::AcceptReceived
            | Stage::Established
            | Stage::Rejected
            | Stage::Closed => true,
        })
        .collect()
}

/// 第 `frame_id` 帧的内容：整帧填同一个由帧号决定的字节，接收端据此校验。
fn frame_fill(frame_id: u32) -> u8 {
    u8::try_from(frame_id % 251).unwrap_or(0)
}

/// 以千分之 `permille` 的概率翻转 `bytes` 里的一个随机比特。
fn maybe_corrupt(rng: &mut crate::sim::Rng, permille: u16, bytes: &mut [u8]) {
    if permille == 0 || !rng.chance(permille) {
        return;
    }
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let index = usize::try_from(rng.below(len)).unwrap_or(0);
    let bit = u32::try_from(rng.below(8)).unwrap_or(0);
    if let Some(byte) = bytes.get_mut(index) {
        *byte ^= 1u8 << bit;
    }
}

/// 占位密钥：仿真里两端直接共享。真实连接的密钥来自 QUIC 的 TLS 1.3 握手。
fn sim_key(suite: Suite, seed: u64) -> Vec<u8> {
    let mut rng = crate::sim::Rng::new(seed ^ 0x5eed_5eed_5eed_5eed);
    let mut key = Vec::with_capacity(suite.key_len());
    while key.len() < suite.key_len() {
        key.extend_from_slice(&rng.next_u64().to_be_bytes());
    }
    key.truncate(suite.key_len());
    key
}

fn decode_control_batch(payload: &[u8]) -> Result<Vec<Control>, Error> {
    let mut r = Reader::new(payload);
    let mut out = Vec::new();
    while r.remaining() > 0 {
        let len = usize::from(r.u16()?);
        out.push(Control::decode(r.take(len)?)?);
    }
    Ok(out)
}

struct Endpoint {
    session: Session,
    seq: u32,
}

impl Endpoint {
    fn send_control(
        &mut self,
        net: &mut Net<Vec<u8>>,
        from: usize,
        to: usize,
        msgs: &[Control],
    ) -> Result<(), Error> {
        // 真实连接里控制消息走有序的可靠流。仿真用数据报承载，
        // 所以把同一批消息放进一个数据报，保证它们一起到、顺序不变。
        let mut body = Writer::new();
        for msg in msgs {
            let encoded = msg.encode()?;
            body.u16(u16::try_from(encoded.len()).unwrap_or(u16::MAX));
            body.bytes(&encoded);
        }
        let header = Header {
            channel: Channel::Control,
            flags: 0,
            conn_id: 0,
            seq: self.seq,
            send_time: net.now(),
        };
        self.seq = self.seq.wrapping_add(1);
        let datagram = encode_datagram(&header, &body.into_vec())?;
        net.send(from, to, datagram.len(), datagram);
        Ok(())
    }
}

/// 运行场景。
#[allow(clippy::too_many_lines)]
pub fn run(p: &Params) -> Result<Report, Error> {
    if p.fps == 0 {
        return Err(Error::InvalidArgument("fps 不能为 0"));
    }
    let frame_interval_us = 1_000_000 / u64::from(p.fps);
    let mut net: Net<Vec<u8>> = Net::new(p.seed);
    net.set_link(CONTROLLER, HOST, p.link);

    let mut tracer = Tracer::new(1 << 16);
    let (trace_c, trace_h) = (TraceId(p.seed), TraceId(p.seed ^ u64::MAX));
    let mut controller = Endpoint {
        session: Session::controller(
            SessionConfig {
                device_id: 0xc0,
                region: "sim".into(),
                caps: Caps::H264.with(Caps::AV1),
                policy: p.controller_policy.clone(),
                auth: p.controller_auth.clone(),
                binding: b"sim".to_vec(),
            },
            trace_c,
        ),
        seq: 0,
    };
    let mut host = Endpoint {
        session: Session::host(
            SessionConfig {
                device_id: 0xa0,
                region: "sim".into(),
                caps: Caps::H264,
                policy: p.host_policy.clone(),
                auth: p.host_auth.clone(),
                binding: b"sim".to_vec(),
            },
            trace_h,
        ),
        seq: 0,
    };

    let mut hello_attempts = 0u32;
    let mut next_hello = Some(Time::ZERO);
    let mut established_at: Option<Time> = None;
    let mut next_frame: Option<Time> = None;
    let mut frames_sent = 0u32;
    let mut sealer_tx: Option<Sealer> = None;
    let mut sealer_rx: Option<Sealer> = None;
    let mut sent_at: Vec<Time> = Vec::new();
    let mut latencies: Vec<u64> = Vec::new();
    let mut reassembler = Reassembler::new(p.deadline_us, 8);
    let mut bad_datagrams = 0u64;
    let mut frames_corrupted = 0u32;
    let max_payload = MAX_DATAGRAM
        .saturating_sub(HEADER_LEN)
        .saturating_sub(VIDEO_HEADER_LEN)
        .saturating_sub(16);

    loop {
        // 下一个要处理的时刻：网络送达、Hello 重发、发下一帧，取最早的。
        let next = [net.peek_time(), next_hello, next_frame]
            .into_iter()
            .flatten()
            .min();
        let Some(when) = next else { break };
        net.advance_to(when);
        let now = net.now();

        if next_hello.is_some_and(|t| t <= now) {
            match controller.session.state() {
                State::Idle | State::HelloSent if hello_attempts < HELLO_ATTEMPTS => {
                    let hello = controller.session.hello(now, &mut tracer)?;
                    controller.send_control(&mut net, CONTROLLER, HOST, &[hello])?;
                    hello_attempts = hello_attempts.saturating_add(1);
                    next_hello = Some(now.plus(HELLO_RETRY_US));
                }
                State::Idle
                | State::HelloSent
                | State::AwaitAuth
                | State::Established { .. }
                | State::Rejected(_)
                | State::Closed => next_hello = None,
            }
        }

        if next_frame.is_some_and(|t| t <= now) {
            if let Some(sealer) = &sealer_tx {
                let frame_id = frames_sent.saturating_add(1);
                let data = vec![frame_fill(frame_id); p.frame_bytes];
                for packet in fragment(frame_id, frame_id.saturating_sub(1), &data, max_payload)? {
                    let header = Header {
                        channel: Channel::Video,
                        flags: 0,
                        conn_id: 0,
                        seq: host.seq,
                        send_time: now,
                    };
                    host.seq = host.seq.wrapping_add(1);
                    let mut aad = Writer::new();
                    header.encode(&mut aad);
                    packet.header.encode(&mut aad);
                    let aad = aad.into_vec();
                    let sealed = sealer.seal(u64::from(header.seq), &aad, &packet.data)?;
                    let mut body = Writer::new();
                    packet.header.encode(&mut body);
                    body.bytes(&sealed);
                    let datagram = encode_datagram(&header, &body.into_vec())?;
                    net.send(HOST, CONTROLLER, datagram.len(), datagram);
                }
                tracer.record(now, trace_h, Stage::FrameSent, u64::from(frame_id));
                sent_at.push(now);
                frames_sent = frame_id;
            }
            next_frame = if frames_sent < p.frames {
                Some(now.plus(frame_interval_us))
            } else {
                None
            };
        }

        while net.peek_time().is_some_and(|t| t <= now) {
            let Some(mut delivery) = net.pop() else { break };
            maybe_corrupt(net.rng(), p.corrupt_permille, &mut delivery.payload);
            let Ok((header, payload)) = decode_datagram(&delivery.payload) else {
                bad_datagrams = bad_datagrams.saturating_add(1);
                continue;
            };
            match header.channel {
                Channel::Control => {
                    let Ok(msgs) = decode_control_batch(payload) else {
                        bad_datagrams = bad_datagrams.saturating_add(1);
                        continue;
                    };
                    let (endpoint, me, peer) = if delivery.to == HOST {
                        (&mut host, HOST, CONTROLLER)
                    } else {
                        (&mut controller, CONTROLLER, HOST)
                    };
                    // 状态机拒绝的消息（例如迟到的重复应答）只计数，不中断仿真。
                    let mut replies = Vec::new();
                    for msg in &msgs {
                        match endpoint.session.on_control(now, msg, &mut tracer) {
                            Ok(more) => replies.extend(more),
                            Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                        }
                    }
                    if !replies.is_empty() {
                        endpoint.send_control(&mut net, me, peer, &replies)?;
                    }
                    if established_at.is_none() {
                        if let Some(suite) = controller.session.suite() {
                            established_at = Some(now);
                            next_hello = None;
                            let key = sim_key(suite, p.seed);
                            sealer_rx = Some(Sealer::new(suite, &key, DIR_HOST_TO_CONTROLLER)?);
                            sealer_tx = Some(Sealer::new(suite, &key, DIR_HOST_TO_CONTROLLER)?);
                            if p.frames > 0 {
                                next_frame = Some(now);
                            }
                        }
                    }
                }
                Channel::Video => {
                    let Some(sealer) = &sealer_rx else {
                        bad_datagrams = bad_datagrams.saturating_add(1);
                        continue;
                    };
                    let mut r = Reader::new(payload);
                    let Ok(video) = VideoHeader::decode(&mut r) else {
                        bad_datagrams = bad_datagrams.saturating_add(1);
                        continue;
                    };
                    let mut aad = Writer::new();
                    header.encode(&mut aad);
                    video.encode(&mut aad);
                    let Ok(data) = sealer.open(u64::from(header.seq), &aad.into_vec(), r.rest())
                    else {
                        bad_datagrams = bad_datagrams.saturating_add(1);
                        continue;
                    };
                    let packet = VideoPacket {
                        header: video,
                        parity: false,
                        data,
                    };
                    // 没有加密时，损坏的分片头可能自相矛盾：丢弃该分片，不中断会话。
                    let Ok(pushed) = reassembler.push(now, packet) else {
                        bad_datagrams = bad_datagrams.saturating_add(1);
                        continue;
                    };
                    if let Some(frame) = pushed {
                        if frame.data.iter().any(|b| *b != frame_fill(frame.frame_id)) {
                            frames_corrupted = frames_corrupted.saturating_add(1);
                        }
                        tracer.record(
                            now,
                            trace_c,
                            Stage::FrameDelivered,
                            u64::from(frame.frame_id),
                        );
                        let index = usize::try_from(frame.frame_id.saturating_sub(1)).unwrap_or(0);
                        if let Some(sent) = sent_at.get(index) {
                            latencies.push(now.since(*sent));
                        }
                    }
                }
                Channel::Input
                | Channel::Cursor
                | Channel::Clipboard
                | Channel::File
                | Channel::Audio => {
                    bad_datagrams = bad_datagrams.saturating_add(1);
                }
            }
        }

        for frame_id in reassembler.expire(now) {
            tracer.record(now, trace_c, Stage::FrameExpired, u64::from(frame_id));
        }
    }

    latencies.sort_unstable();
    let rejected = match controller.session.state() {
        State::Rejected(reason) => Some(*reason),
        State::Idle
        | State::HelloSent
        | State::AwaitAuth
        | State::Established { .. }
        | State::Closed => None,
    };
    Ok(Report {
        established: established_at.is_some(),
        suite: controller.session.suite(),
        rejected,
        handshake_us: established_at.map_or(0, Time::micros),
        hello_attempts,
        frames_sent,
        frames_delivered: u32::try_from(latencies.len()).unwrap_or(u32::MAX),
        frames_corrupted,
        latency_p50_us: percentile(&latencies, 50),
        latency_p95_us: percentile(&latencies, 95),
        bad_datagrams,
        net: net.stats(),
        controller_path: without_frames(tracer.path(trace_c)),
        host_path: without_frames(tracer.path(trace_h)),
        trace_dump: tracer.dump(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_edges() {
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[7], 95), 7);
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 50), 3);
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 100), 5);
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 0), 1);
    }

    #[test]
    fn corruption_flips_exactly_one_bit_or_nothing() {
        let mut rng = crate::sim::Rng::new(1);
        let original = [0u8; 16];
        let mut never = original;
        maybe_corrupt(&mut rng, 0, &mut never);
        assert_eq!(never, original);
        let mut always = original;
        maybe_corrupt(&mut rng, 1000, &mut always);
        assert_eq!(always.iter().map(|b| b.count_ones()).sum::<u32>(), 1);
        // 空数据报不会越界
        maybe_corrupt(&mut rng, 1000, &mut []);
        assert_eq!(frame_fill(252), 1);
    }

    #[test]
    fn sim_keys_match_suite_length() {
        for suite in Suite::ALL {
            assert_eq!(sim_key(suite, 9).len(), suite.key_len());
        }
        assert_ne!(sim_key(Suite::Aes256Gcm, 1), sim_key(Suite::Aes256Gcm, 2));
    }
}
