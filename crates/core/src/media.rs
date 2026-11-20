//! 画面的发送端和接收端：不做 IO 的状态机。
//!
//! 被控端、控制端和端到端仿真都用这一份代码，所以仿真测到的就是产品的行为。
//! 调用方负责收发数据报和提供时间；这里负责：
//! - 发送端：每帧按码率控制给编码器预算，切成分片，加校验分片；按接收端的反馈调码率，
//!   判定丢失并让编码器恢复。
//! - 接收端：重组分片，只把解得了的帧交给解码器（缺了参考帧就等关键帧，不显示花屏），
//!   生成反馈。

use std::collections::VecDeque;

use crate::adaptive::{DelayTracker, DeliveryLog, Feedback, Profile, RateController};
use crate::error::Error;
use crate::proto::{VideoHeader, MAX_DATAGRAM};
use crate::screen::Screen;
use crate::time::Time;
use crate::transport::{fragment, parity_for, Reassembler, VideoPacket, PARITY_PREFIX};
use crate::video::{Codec, FrameHeader, VideoDecoder, VideoEncoder};
use crate::wire::{Reader, Writer};

pub use crate::desktop_sim::FLAG_PARITY;
use crate::proto::{HEADER_LEN, VIDEO_HEADER_LEN};

/// 发送端的参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SenderConfig {
    /// 帧率。
    pub fps: u32,
    /// 起始码率。
    pub start_bps: u64,
    /// 码率下限。
    pub min_bps: u64,
    /// 码率上限。
    pub max_bps: u64,
    /// 网络画像。
    pub profile: Profile,
    /// 一帧从发出到必须收齐的时间（微秒）。
    pub deadline_us: u64,
    /// 一个数据报能装的最大载荷（不含数据报头）。
    pub max_payload: usize,
}

impl Default for SenderConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            start_bps: 4_000_000,
            min_bps: 100_000,
            max_bps: 30_000_000,
            profile: Profile::STABLE,
            deadline_us: 100_000,
            max_payload: MAX_DATAGRAM - HEADER_LEN,
        }
    }
}

/// 一个要发出的视频数据报（不含数据报头）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// 数据报标志位（校验分片带 [`FLAG_PARITY`]）。
    pub flags: u8,
    /// 载荷：分片头加分片数据。
    pub body: Vec<u8>,
}

/// 发送端的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SenderStats {
    /// 发出的帧数。
    pub frames: u32,
    /// 其中的关键帧数。
    pub keyframes: u32,
    /// 编码后的字节数（不含分片头和校验分片）。
    pub video_bytes: u64,
    /// 发出的数据报字节数（含校验分片，不含数据报头）。
    pub wire_bytes: u64,
    /// 判定丢失、让编码器恢复的帧数。
    pub lost: u32,
}

/// 画面发送端。
pub struct MediaSender {
    cfg: SenderConfig,
    encoder: Box<dyn VideoEncoder>,
    rate: RateController,
    /// 已发出、还没有定论的帧：(帧号, 发出时间)。
    unacked: VecDeque<(u32, Time)>,
    last_frame: u32,
    last_key: u32,
    stats: SenderStats,
}

impl std::fmt::Debug for MediaSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaSender")
            .field("codec", &self.encoder.codec())
            .field("rate_bps", &self.rate.rate_bps())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl MediaSender {
    /// 创建。
    #[must_use]
    pub fn new(cfg: SenderConfig, encoder: Box<dyn VideoEncoder>) -> Self {
        Self {
            rate: RateController::new(cfg.profile, cfg.start_bps, cfg.min_bps, cfg.max_bps),
            cfg,
            encoder,
            unacked: VecDeque::new(),
            last_frame: 0,
            last_key: 0,
            stats: SenderStats::default(),
        }
    }

    /// 编码格式。
    #[must_use]
    pub fn codec(&self) -> Codec {
        self.encoder.codec()
    }

    /// 当前码率。
    #[must_use]
    pub const fn rate_bps(&self) -> u64 {
        self.rate.rate_bps()
    }

    /// 计数。
    #[must_use]
    pub const fn stats(&self) -> SenderStats {
        self.stats
    }

    /// 画面没变时还要不要按时交来（编码器在补清晰或要恢复）。
    #[must_use]
    pub fn wants_frame(&self) -> bool {
        self.encoder.wants_refine()
    }

    /// 改网络画像（例如测出了更大的往返时间）。
    pub const fn set_profile(&mut self, profile: Profile) {
        self.rate.set_profile(profile);
    }

    fn lost(&mut self, frame_id: u32) {
        // 最近一个关键帧之前的帧丢了没关系：接收端从那个关键帧开始就能解。
        if frame_id >= self.last_key {
            self.encoder.on_lost(frame_id);
            self.stats.lost = self.stats.lost.saturating_add(1);
        }
    }

    /// 交来一帧画面。`changed` 表示画面和上一次交来的不同；`rtt_us` 是当前的往返时间估计。
    /// 返回要发出的数据报（可能为空：画面没变、清晰度也补完了）。
    pub fn on_frame(
        &mut self,
        now: Time,
        screen: &Screen,
        changed: bool,
        rtt_us: u64,
    ) -> Result<Vec<Outgoing>, Error> {
        // 迟迟等不到反馈的帧按丢失处理。
        let ack_timeout = self
            .cfg
            .deadline_us
            .saturating_add(rtt_us.saturating_mul(2))
            .saturating_add(60_000);
        while let Some((id, at)) = self.unacked.front().copied() {
            if now.since(at) < ack_timeout {
                break;
            }
            self.unacked.pop_front();
            self.lost(id);
        }
        let frame_id = self.last_frame.wrapping_add(1).max(1);
        let budget = self.rate.frame_budget(self.cfg.fps);
        let Some(encoded) = self.encoder.encode(frame_id, screen, changed, budget)? else {
            return Ok(Vec::new());
        };
        // 关键帧参考自己；其余参考上一帧（视频编码的增量帧依赖它）。
        let ref_frame = if encoded.key || self.encoder.codec() == Codec::Tile {
            frame_id
        } else {
            self.last_frame
        };
        let max = self
            .cfg
            .max_payload
            .saturating_sub(VIDEO_HEADER_LEN)
            .saturating_sub(PARITY_PREFIX);
        let mut packets = fragment(frame_id, ref_frame, &encoded.payload, max)?;
        let parity = parity_for(&packets, self.rate.profile().fec_permille);
        packets.extend(parity);
        let out: Vec<Outgoing> = packets
            .into_iter()
            .map(|p| {
                let mut body = Writer::new();
                p.header.encode(&mut body);
                body.bytes(&p.data);
                Outgoing {
                    flags: if p.parity { FLAG_PARITY } else { 0 },
                    body: body.into_vec(),
                }
            })
            .collect();
        self.last_frame = frame_id;
        if encoded.key {
            self.last_key = frame_id;
            self.stats.keyframes = self.stats.keyframes.saturating_add(1);
        }
        self.unacked.push_back((frame_id, now));
        self.stats.frames = self.stats.frames.saturating_add(1);
        self.stats.video_bytes = self
            .stats
            .video_bytes
            .saturating_add(u64::try_from(encoded.payload.len()).unwrap_or(u64::MAX));
        let wire: usize = out.iter().map(|o| o.body.len()).sum();
        self.stats.wire_bytes = self
            .stats
            .wire_bytes
            .saturating_add(u64::try_from(wire).unwrap_or(u64::MAX));
        Ok(out)
    }

    /// 接收端的反馈。
    pub fn on_feedback(&mut self, now: Time, feedback: &Feedback) {
        self.rate.on_feedback(now, feedback);
        self.encoder.on_feedback(feedback.floor, feedback.delivered);
        while let Some((id, _)) = self.unacked.front().copied() {
            if id > feedback.floor {
                break;
            }
            self.unacked.pop_front();
            let age = feedback.floor.wrapping_sub(id);
            let delivered = age < 32 && feedback.delivered >> age & 1 == 1;
            if !delivered {
                self.lost(id);
            }
        }
    }
}

/// 接收端的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReceiverStats {
    /// 解出并显示的帧数。
    pub decoded: u32,
    /// 因为缺参考帧而没解的帧数（等关键帧期间）。
    pub skipped: u32,
    /// 码流有问题、解码失败的帧数。
    pub bad: u32,
    /// 收齐的帧的字节数。
    pub video_bytes: u64,
}

/// 接收端交出的一帧画面。
#[derive(Debug)]
pub struct Shown<'a> {
    /// 帧号。
    pub frame_id: u32,
    /// 是否关键帧。
    pub key: bool,
    /// 画面。
    pub screen: &'a Screen,
}

/// 画面接收端。
pub struct MediaReceiver {
    decoder: Box<dyn VideoDecoder>,
    reassembler: Reassembler,
    tracker: DelayTracker,
    log: DeliveryLog,
    /// 最后交给解码器的帧号；`None` 表示要等关键帧。
    decoded: Option<u32>,
    stats: ReceiverStats,
}

impl std::fmt::Debug for MediaReceiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaReceiver")
            .field("decoded", &self.decoded)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl MediaReceiver {
    /// 创建。`deadline_us` 是一帧从第一个分片到收齐允许的时间。
    #[must_use]
    pub fn new(now: Time, deadline_us: u64, decoder: Box<dyn VideoDecoder>) -> Self {
        Self {
            decoder,
            reassembler: Reassembler::new(deadline_us, 16),
            tracker: DelayTracker::new(now),
            log: DeliveryLog::new(),
            decoded: None,
            stats: ReceiverStats::default(),
        }
    }

    /// 计数。
    #[must_use]
    pub const fn stats(&self) -> ReceiverStats {
        self.stats
    }

    /// 收到一个视频数据报。`send_time`、`seq`、`flags` 来自数据报头。
    /// 收齐并解出一帧时返回它。分片本身不合法时返回错误（调用方计为坏数据报）。
    pub fn on_datagram(
        &mut self,
        now: Time,
        send_time: Time,
        seq: u32,
        flags: u8,
        body: &[u8],
    ) -> Result<Option<Shown<'_>>, Error> {
        self.tracker.on_packet(now, send_time, seq, body.len());
        let mut r = Reader::new(body);
        let header = VideoHeader::decode(&mut r)?;
        let packet = VideoPacket {
            header,
            parity: flags & FLAG_PARITY != 0,
            data: r.rest().to_vec(),
        };
        let Some(frame) = self.reassembler.push(now, packet)? else {
            return Ok(None);
        };
        self.stats.video_bytes = self
            .stats
            .video_bytes
            .saturating_add(u64::try_from(frame.data.len()).unwrap_or(u64::MAX));
        let Ok((fh, data)) = FrameHeader::parse(&frame.data) else {
            self.stats.bad = self.stats.bad.saturating_add(1);
            self.log.advance(frame.frame_id);
            return Ok(None);
        };
        // 视频编码的增量帧只能接在它参考的那一帧后面解；块编码的每一帧都能直接应用。
        let decodable = fh.key
            || fh.codec == Codec::Tile
            || self.decoded == Some(frame.ref_frame) && frame.ref_frame != frame.frame_id;
        if !decodable {
            // 没交给解码器：不算送达，发送端据此补关键帧。
            self.decoded = None;
            self.stats.skipped = self.stats.skipped.saturating_add(1);
            self.log.advance(frame.frame_id);
            return Ok(None);
        }
        match self.decoder.decode(&fh, data) {
            Ok(Some(screen)) => {
                self.decoded = Some(frame.frame_id);
                self.log.delivered(frame.frame_id);
                self.stats.decoded = self.stats.decoded.saturating_add(1);
                Ok(Some(Shown {
                    frame_id: frame.frame_id,
                    key: fh.key,
                    screen,
                }))
            }
            Ok(None) => {
                self.decoded = Some(frame.frame_id);
                self.log.delivered(frame.frame_id);
                Ok(None)
            }
            Err(_) => {
                self.decoded = None;
                self.stats.bad = self.stats.bad.saturating_add(1);
                self.log.advance(frame.frame_id);
                Ok(None)
            }
        }
    }

    /// 生成一条反馈，同时让超时的帧作废。
    pub fn feedback(&mut self, now: Time) -> Feedback {
        self.reassembler.expire(now);
        if let Some(floor) = self.reassembler.floor() {
            self.log.advance(floor);
        }
        let sample = self.tracker.report(now);
        let (floor, delivered) = self.log.snapshot();
        Feedback {
            floor,
            delivered,
            queue_delay_us: sample.queue_delay_us,
            loss_permille: sample.loss_permille,
            recv_bytes_per_s: sample.recv_bytes_per_s,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::video::{EncodedFrame, TileDecoder, TileEncoder};

    /// 模拟视频编码的依赖关系：增量帧只记录和上一帧相比的“版本号”，
    /// 解码器发现版本接不上就报错。这样不需要真的 H.264 也能测接收端的等关键帧逻辑。
    #[derive(Debug, Default)]
    struct FakeEncoder {
        version: u32,
        force_key: bool,
        started: bool,
        lost: Vec<u32>,
    }

    impl VideoEncoder for FakeEncoder {
        fn codec(&self) -> Codec {
            Codec::H264
        }
        fn encode(
            &mut self,
            _id: u32,
            s: &Screen,
            changed: bool,
            budget: usize,
        ) -> Result<Option<EncodedFrame>, Error> {
            if !changed && !self.force_key && self.started {
                return Ok(None);
            }
            let key = self.force_key || !self.started;
            self.started = true;
            self.force_key = false;
            self.version += 1;
            let mut body = Writer::new();
            body.u32(self.version).u32(s.pixels()[0]);
            // 关键帧大一些，跨好几个分片
            let pad = if key { 3000 } else { 10 };
            body.bytes(&vec![0u8; pad.min(budget)]);
            let header = FrameHeader {
                codec: Codec::H264,
                key,
                width: s.width(),
                height: s.height(),
            };
            Ok(Some(EncodedFrame {
                payload: header.frame(&body.into_vec()),
                key,
            }))
        }
        fn on_feedback(&mut self, _floor: u32, _mask: u32) {}
        fn on_lost(&mut self, frame_id: u32) {
            self.lost.push(frame_id);
            self.force_key = true;
        }
        fn wants_refine(&self) -> bool {
            self.force_key
        }
    }

    #[derive(Debug, Default)]
    struct FakeDecoder {
        version: Option<u32>,
        screen: Option<Screen>,
    }

    impl VideoDecoder for FakeDecoder {
        fn decode(&mut self, h: &FrameHeader, data: &[u8]) -> Result<Option<&Screen>, Error> {
            let mut r = Reader::new(data);
            let (version, color) = (r.u32()?, r.u32()?);
            // 接收端保证只交来接得上的帧；这里只检查一下，并用一个特殊颜色模拟解码失败。
            assert!(
                h.key || self.version.map(|v| v + 1) == Some(version),
                "参考帧接不上"
            );
            if color == 0x00de_ad00 {
                return Err(Error::Inconsistent("模拟解码失败"));
            }
            self.version = Some(version);
            if color == 0x00ab_cdef {
                return Ok(None); // 模拟解码器暂时没有输出
            }
            self.screen = Some(Screen::new(h.width, h.height, color));
            Ok(self.screen.as_ref())
        }
    }

    fn cfg() -> SenderConfig {
        SenderConfig {
            max_payload: 400,
            ..SenderConfig::default()
        }
    }

    /// 把发送端的输出交给接收端，`drop` 决定丢哪些数据报。返回显示出的帧号。
    fn deliver(
        rx: &mut MediaReceiver,
        now: Time,
        out: &[Outgoing],
        drop: impl Fn(usize) -> bool,
    ) -> Vec<(u32, u32)> {
        let mut shown = Vec::new();
        for (i, o) in out.iter().enumerate() {
            if drop(i) {
                continue;
            }
            if let Some(s) = rx
                .on_datagram(now, now, u32::try_from(i).unwrap(), o.flags, &o.body)
                .unwrap()
            {
                shown.push((s.frame_id, s.screen.pixels()[0]));
            }
        }
        shown
    }

    #[test]
    fn tile_codec_round_trip() {
        let mut tx = MediaSender::new(cfg(), Box::new(TileEncoder::new()));
        let mut rx = MediaReceiver::new(Time::ZERO, 100_000, Box::new(TileDecoder::new()));
        assert_eq!(tx.codec(), Codec::Tile);
        assert!(tx.rate_bps() > 0);
        let mut screen = Screen::new(64, 48, 0x0011_2233);
        let mut t = Time::ZERO;
        for i in 0..3u32 {
            screen.fill(0, 0, 8, 8, i);
            let out = tx.on_frame(t, &screen, true, 10_000).unwrap();
            let shown = deliver(&mut rx, t, &out, |_| false);
            assert_eq!(shown.len(), 1);
            let fb = rx.feedback(t);
            tx.on_feedback(t, &fb);
            t = t.plus(33_000);
        }
        assert_eq!(rx.stats().decoded, 3);
        assert_eq!(tx.stats().frames, 3);
        assert!(tx.stats().wire_bytes > tx.stats().video_bytes);
        assert!(format!("{tx:?} {rx:?}").contains("Tile"));
    }

    #[test]
    fn lost_frame_waits_for_a_keyframe_and_never_shows_garbage() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let mut rx = MediaReceiver::new(Time::ZERO, 100_000, Box::new(FakeDecoder::default()));
        tx.set_profile(Profile {
            fec_permille: 0,
            ..Profile::STABLE
        });
        let mut t = Time::ZERO;
        let mut step =
            |tx: &mut MediaSender, rx: &mut MediaReceiver, color: u32, drop_all: bool| {
                let screen = Screen::new(32, 32, color);
                let out = tx.on_frame(t, &screen, true, 10_000).unwrap();
                let shown = deliver(rx, t, &out, |_| drop_all);
                let fb = rx.feedback(t.plus(1));
                tx.on_feedback(t.plus(1), &fb);
                t = t.plus(33_000);
                shown
            };
        assert_eq!(
            step(&mut tx, &mut rx, 1, false),
            vec![(1, 1)],
            "第一帧是关键帧"
        );
        assert_eq!(step(&mut tx, &mut rx, 2, false), vec![(2, 2)]);
        assert!(
            step(&mut tx, &mut rx, 3, true).is_empty(),
            "第 3 帧整帧丢了"
        );
        // 第 4 帧到了，但它参考的第 3 帧没有：不解，不显示
        assert!(step(&mut tx, &mut rx, 4, false).is_empty());
        assert_eq!(rx.stats().skipped, 1);
        // 发送端已经知道丢了：下一帧是关键帧，接收端恢复
        assert!(tx.wants_frame());
        assert_eq!(step(&mut tx, &mut rx, 5, false), vec![(5, 5)]);
        assert_eq!(tx.stats().keyframes, 2);
        assert_eq!(rx.stats().bad, 0, "从不把接不上的帧交给解码器");
        assert!(tx.stats().lost >= 1);
    }

    #[test]
    fn silence_counts_as_loss_and_old_losses_do_not_cause_extra_keyframes() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let screen = Screen::new(32, 32, 7);
        let _ = tx.on_frame(Time::ZERO, &screen, true, 10_000).unwrap(); // 1 关键帧
        let _ = tx.on_frame(Time(10), &screen, true, 10_000).unwrap(); // 2
                                                                       // 很久没有反馈：1、2 都判丢；2 在关键帧之后，要恢复
        let out = tx
            .on_frame(Time(1_000_000), &screen, false, 10_000)
            .unwrap();
        assert!(!out.is_empty(), "恢复用的关键帧");
        assert_eq!(tx.stats().keyframes, 2);
        // 关键帧 3 之前的帧再报丢：不再补关键帧
        tx.on_feedback(
            Time(1_000_100),
            &Feedback {
                floor: 2,
                delivered: 0,
                queue_delay_us: 0,
                loss_permille: 0,
                recv_bytes_per_s: 0,
            },
        );
        assert!(!tx.wants_frame());
        // 画面没变也没有要补的：不发
        assert!(tx
            .on_frame(Time(1_000_200), &screen, false, 10_000)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn bad_input_is_rejected_or_counted() {
        let mut rx = MediaReceiver::new(Time::ZERO, 100_000, Box::new(FakeDecoder::default()));
        assert!(
            rx.on_datagram(Time::ZERO, Time::ZERO, 0, 0, &[1, 2])
                .is_err(),
            "截断的分片头"
        );
        // 帧头不合法的整帧
        let packets = fragment(1, 1, &[9, 9, 9], 100).unwrap();
        let mut body = Writer::new();
        packets[0].header.encode(&mut body);
        body.bytes(&packets[0].data);
        assert!(rx
            .on_datagram(Time::ZERO, Time::ZERO, 1, 0, &body.into_vec())
            .unwrap()
            .is_none());
        assert_eq!(rx.stats().bad, 1);
        let mut rx = MediaReceiver::new(Time::ZERO, 100_000, Box::new(FakeDecoder::default()));
        // 解码器出错（版本接不上的增量帧冒充成关键帧之后的帧）
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let k = tx
            .on_frame(Time::ZERO, &Screen::new(8, 8, 1), true, 0)
            .unwrap();
        deliver(&mut rx, Time::ZERO, &k, |_| false);
        // 解码器暂时没有输出的帧也算送达
        let quiet = tx
            .on_frame(Time(1), &Screen::new(8, 8, 0x00ab_cdef), true, 0)
            .unwrap();
        assert!(deliver(&mut rx, Time(1), &quiet, |_| false).is_empty());
        let fb = rx.feedback(Time(2));
        assert_eq!(fb.floor, 2);
        assert_eq!(fb.delivered & 0b11, 0b11, "第 1、2 帧都算送达");
        let other = FakeEncoder {
            version: 41,
            started: true,
            ..FakeEncoder::default()
        };
        let mut tx2 = MediaSender::new(cfg(), Box::new(other));
        // 帧号从 1 开始，和接收端的参考帧对不上：按缺参考帧处理
        let p = tx2
            .on_frame(Time(3), &Screen::new(8, 8, 5), true, 0)
            .unwrap();
        assert!(deliver(&mut rx, Time(3), &p, |_| false).is_empty());
        // 解码器出错：不显示，之后等关键帧
        let k = tx
            .on_frame(Time(4), &Screen::new(8, 8, 0x00de_ad00), true, 0)
            .unwrap();
        assert!(deliver(&mut rx, Time(4), &k, |_| false).is_empty());
        assert_eq!(rx.stats().bad, 1);
    }
}
