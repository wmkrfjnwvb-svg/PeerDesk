//! 画面的发送端和接收端：不做 IO 的状态机。
//!
//! 被控端、控制端和端到端仿真都用这一份代码，所以仿真测到的就是产品的行为。
//! 调用方负责收发数据报和提供时间；这里负责：
//! - 发送端：每帧按码率控制给编码器预算，切成分片，加校验分片；按接收端的反馈调码率，
//!   判定丢失并让编码器恢复。
//! - 接收端：重组分片，只把解得了的帧交给解码器（缺了参考帧就等关键帧，不显示花屏），
//!   生成反馈。

use std::collections::{BTreeMap, VecDeque};

use crate::adaptive::{DelayTracker, DeliveryLog, Feedback, Profile};
use crate::error::Error;
use crate::estimator::{EstimatorKind, Extension, Observation, RateEstimator, ReceiverMetrics};
use crate::governor::FpsGovernor;
use crate::proto::{VideoHeader, MAX_DATAGRAM};
use crate::screen::Screen;
use crate::time::Time;
use crate::transport::{fragment, parity_for, Reassembler, VideoPacket, PARITY_PREFIX};
use crate::video::{Codec, EncodedFrame, FrameHeader, VideoDecoder, VideoEncoder};
use crate::wire::{Reader, Writer};

pub use crate::desktop_sim::FLAG_PARITY;
use crate::proto::{HEADER_LEN, VIDEO_HEADER_LEN};

/// 一轮重传最多补发多长时间的量（按当前码率，微秒）。
const RESEND_WINDOW_US: u64 = 40_000;

/// 一帧从发出到送达的目标时间（微秒）：排队延迟按它扣减每帧预算。
const QUEUE_TARGET_US: u64 = 50_000;

/// 在途上限在往返时间之外多留的时间（微秒）。
const IN_FLIGHT_EXTRA_US: u64 = 50_000;
/// 发出多久之内的帧算“在途”（往返时间之外，微秒）。
const IN_FLIGHT_RECENT_US: u64 = 100_000;

/// 一条反馈最多带多少个重传请求。
pub const MAX_NACKS: usize = 48;
/// 分片缺了多久才请求重传（微秒）：短于这个时间可能只是乱序。
const NACK_AFTER_US: u64 = 10_000;

/// 反馈数据报：[`Feedback`]、扩展测量（[`Extension`]），后面跟着重传请求（个数 + 每个的帧号和分片序号）。
#[must_use]
pub fn encode_feedback(feedback: &Feedback, ext: &Extension, nacks: &[(u32, u16)]) -> Vec<u8> {
    let mut w = Writer::new();
    w.bytes(&feedback.encode());
    ext.encode(&mut w);
    let nacks = nacks.get(..MAX_NACKS.min(nacks.len())).unwrap_or_default();
    if !nacks.is_empty() {
        w.u8(u8::try_from(nacks.len()).unwrap_or(u8::MAX));
        for (frame, frag) in nacks {
            w.u32(*frame).u16(*frag);
        }
    }
    w.into_vec()
}

/// 解析出的反馈数据报：基本反馈、扩展测量、重传请求。
pub type DecodedFeedback = (Feedback, Extension, Vec<(u32, u16)>);

/// 解析反馈数据报。
pub fn decode_feedback(buf: &[u8]) -> Result<DecodedFeedback, Error> {
    let (head, rest) = buf
        .split_at_checked(Feedback::LEN)
        .ok_or(Error::Truncated {
            need: Feedback::LEN,
            have: buf.len(),
        })?;
    let feedback = Feedback::decode(head)?;
    let mut r = Reader::new(rest);
    let ext = Extension::decode(&mut r)?;
    if r.remaining() == 0 {
        return Ok((feedback, ext, Vec::new()));
    }
    let count = usize::from(r.u8()?);
    if count == 0 || count > MAX_NACKS {
        return Err(Error::TooManyItems {
            len: count,
            max: MAX_NACKS,
        });
    }
    let nacks = (0..count)
        .map(|_| Ok((r.u32()?, r.u16()?)))
        .collect::<Result<Vec<_>, Error>>()?;
    r.finish()?;
    Ok((feedback, ext, nacks))
}

/// 估计一帧传完要多久时用的最低速率，防止速率估计异常低时一帧永远不判丢失。
const MIN_DRAIN_BPS: u64 = 50_000;

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
    /// 带宽估计器。
    pub estimator: EstimatorKind,
}

impl Default for SenderConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            start_bps: 4_000_000,
            min_bps: 100_000,
            max_bps: 30_000_000,
            profile: Profile::STABLE,
            // 要留够时间给丢失的分片重传一次（大约一个往返）。
            deadline_us: 250_000,
            max_payload: MAX_DATAGRAM - HEADER_LEN,
            estimator: EstimatorKind::PRODUCT,
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
    /// 按接收端的请求重传的分片数。
    pub retransmitted: u32,
}

/// 交给编码器的一帧任务。编码可以在别的线程上做：任务里带着上次之后发送端判定的丢失和收到的确认，
/// 编码器先处理它们再编码（见 [`encode_job`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameJob {
    /// 帧号。
    pub frame_id: u32,
    /// 字节预算。
    pub budget: usize,
    /// 上次之后判定丢失的帧。
    pub lost: Vec<u32>,
    /// 上次之后最新的确认 `(floor, mask)`。
    pub feedback: Option<(u32, u32)>,
}

/// 在编码器所在的地方执行一个任务。返回编码结果，以及之后画面不变时还要不要继续交画面（补清晰、恢复）。
pub fn encode_job(
    encoder: &mut dyn VideoEncoder,
    job: &FrameJob,
    screen: &Screen,
    changed: bool,
) -> Result<(Option<EncodedFrame>, bool), Error> {
    for id in &job.lost {
        encoder.on_lost(*id);
    }
    if let Some((floor, mask)) = job.feedback {
        encoder.on_feedback(floor, mask);
    }
    let encoded = encoder.encode(job.frame_id, screen, changed, job.budget)?;
    Ok((encoded, encoder.wants_refine()))
}

/// 画面发送端。
///
/// 两种用法：
/// - 编码器交给它（[`MediaSender::new`]），每帧调用 [`MediaSender::on_frame`]，编码就在调用的地方做（仿真用这种）；
/// - 编码器放在别的线程（[`MediaSender::detached`]），每帧先 [`MediaSender::start_frame`] 得到任务，
///   编码器线程用 [`encode_job`] 执行，结果交回 [`MediaSender::finish_frame`]。编码期间会话循环照常处理输入。
pub struct MediaSender {
    cfg: SenderConfig,
    /// 编码器在这里时为 `Some`；放在别的线程时为 `None`，对它的通知攒在 `notes` 里随下一个任务带过去。
    encoder: Option<Box<dyn VideoEncoder>>,
    codec: Codec,
    notes: (Vec<u32>, Option<(u32, u32)>),
    /// 编码器上次报告的“还要不要继续交画面”（编码器在别的线程时用）。
    refine: bool,
    rate: Box<dyn RateEstimator>,
    /// 已发出、还没有定论的帧：(帧号, 发出时间, 线上字节数)。
    unacked: VecDeque<(u32, Time, u64)>,
    last_frame: u32,
    last_key: u32,
    stats: SenderStats,
    /// 最近发出的帧的数据分片，留着重传：(帧号, 发出时间, 数据分片)。
    recent: VecDeque<(u32, Time, Vec<Outgoing>)>,
    /// 每个分片上次重传的时间：一个往返之内不重复重传。
    resent: BTreeMap<(u32, u16), Time>,
    rtt_us: u64,
    last_obs: Option<Observation>,
    governor: FpsGovernor,
    /// 下一次 [`Self::finish_frame`] 用的编码时间（微秒），由编码线程报告。
    encode_us: u64,
}

impl std::fmt::Debug for MediaSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaSender")
            .field("codec", &self.codec)
            .field("rate_bps", &self.rate.rate_bps())
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl MediaSender {
    /// 创建。
    #[must_use]
    pub fn new(cfg: SenderConfig, encoder: Box<dyn VideoEncoder>) -> Self {
        let codec = encoder.codec();
        Self {
            encoder: Some(encoder),
            ..Self::detached(cfg, codec)
        }
    }

    /// 创建，编码器放在别的地方（见类型说明）。
    #[must_use]
    pub fn detached(cfg: SenderConfig, codec: Codec) -> Self {
        Self {
            rate: cfg
                .estimator
                .build(cfg.profile, cfg.start_bps, cfg.min_bps, cfg.max_bps),
            cfg,
            encoder: None,
            codec,
            notes: (Vec::new(), None),
            refine: false,
            unacked: VecDeque::new(),
            last_frame: 0,
            last_key: 0,
            stats: SenderStats::default(),
            recent: VecDeque::new(),
            resent: BTreeMap::new(),
            rtt_us: 0,
            last_obs: None,
            governor: FpsGovernor::new(cfg.fps),
            encode_us: 0,
        }
    }

    /// 编码格式。
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// 当前码率。
    #[must_use]
    pub fn rate_bps(&self) -> u64 {
        self.rate.rate_bps()
    }

    /// 计数。
    #[must_use]
    pub const fn stats(&self) -> SenderStats {
        self.stats
    }

    /// 当前帧率（CPU 跟不上或内容装不进预算时会低于设定值）。
    #[must_use]
    pub const fn fps(&self) -> u32 {
        self.governor.fps()
    }

    /// 按当前帧率，到时间编下一帧了吗。
    #[must_use]
    pub fn due(&self, now: Time) -> bool {
        self.governor.due(now)
    }

    /// 编码线程报告这一帧编了多久（微秒），在 [`Self::finish_frame`] 之前调用。
    pub const fn set_encode_time(&mut self, us: u64) {
        self.encode_us = us;
    }

    /// 在途数据有没有超过上限。超过时不要编下一帧：画面的变化先攒着，等在途数据被确认之后
    /// 编最新的那一幅（跳到最新，不排队）。上限约等于码率 ×（往返 + 50 ms），至少两帧的预算。
    ///
    /// 一次大面积变化（切窗口）编出来的帧往往是预算的十几倍，这是编码器决定的、免不了；
    /// 要避免的是它之后的帧接着往里塞，在瓶颈处排出长队，让之后的每一帧都晚到。
    ///
    /// 只算最近一个往返加 100 ms 之内发出的帧：更早的要么已经到了（确认还在路上），要么正在靠
    /// 重传补，再拦着新帧只会让画面停住（例如丢包网络上一个大帧要好几轮重传才收齐）。
    #[must_use]
    pub fn in_flight_full(&self, now: Time) -> bool {
        let window_us = self.rtt_us.saturating_add(IN_FLIGHT_EXTRA_US);
        let by_rate = self.rate.rate_bps().saturating_mul(window_us) / 8_000_000;
        let floor = u64::try_from(self.frame_budget())
            .unwrap_or(u64::MAX)
            .saturating_mul(2);
        let recent_us = self.rtt_us.saturating_add(IN_FLIGHT_RECENT_US);
        let recent: u64 = self
            .unacked
            .iter()
            .filter(|(_, at, _)| now.since(*at) < recent_us)
            .map(|(_, _, bytes)| *bytes)
            .sum();
        // 最近的关键帧还没确认：之后的帧都依赖它，发了也解不了，先等它送到。
        let key_pending = self.unacked.iter().any(|(id, _, _)| *id == self.last_key);
        recent > by_rate.max(floor) || key_pending
    }

    /// 画面没变时还要不要按时交来（编码器在补清晰或要恢复）。
    #[must_use]
    pub fn wants_frame(&self) -> bool {
        let encoder = self
            .encoder
            .as_ref()
            .map_or(self.refine, |e| e.wants_refine());
        encoder || !self.notes.0.is_empty()
    }

    /// 网络画像。
    #[must_use]
    pub fn profile(&self) -> &Profile {
        self.rate.profile()
    }

    /// 改网络画像（例如测出了更大的往返时间）。
    /// 更新一个数据报能放的字节数（路径 MTU 会变：丢包严重时 QUIC 会把它调小）。0 忽略。
    pub fn set_max_payload(&mut self, max_payload: usize) {
        if max_payload > 0 {
            self.cfg.max_payload = max_payload;
        }
    }

    pub fn set_profile(&mut self, profile: Profile) {
        self.rate.set_profile(profile);
    }

    fn lost(&mut self, frame_id: u32) {
        // 最近一个关键帧之前的帧丢了没关系：接收端从那个关键帧开始就能解。
        if frame_id >= self.last_key {
            match self.encoder.as_mut() {
                Some(encoder) => encoder.on_lost(frame_id),
                None => self.notes.0.push(frame_id),
            }
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
        let job = self.start_frame(now, rtt_us);
        let mut encoder = self.encoder.take().ok_or(Error::InvalidArgument(
            "编码器不在这里：用 start_frame / finish_frame",
        ))?;
        let result = encode_job(encoder.as_mut(), &job, screen, changed);
        self.encoder = Some(encoder);
        let (encoded, refine) = result?;
        self.finish_frame(now, &job, encoded, refine)
    }

    /// 开始一帧：判定超时丢失，分配帧号和字节预算，带上要交给编码器的通知。
    pub fn start_frame(&mut self, now: Time, rtt_us: u64) -> FrameJob {
        self.rtt_us = rtt_us;
        // 过了截止时间的帧不再重传。
        while self
            .recent
            .front()
            .is_some_and(|(_, at, _)| now.since(*at) > self.cfg.deadline_us)
        {
            if let Some((id, _, _)) = self.recent.pop_front() {
                self.resent.retain(|(frame, _), _| *frame != id);
            }
        }
        // 迟迟等不到反馈的帧按丢失处理。时限里加上这一帧按接收端实际收的速度传完要多久：
        // 窄带宽上一个大关键帧要传一两秒，按固定时限判丢会重发新的关键帧，越堵越多，画面始终出不来。
        let ack_timeout = self
            .cfg
            .deadline_us
            .saturating_add(rtt_us.saturating_mul(2))
            .saturating_add(60_000);
        let drain_bps = self
            .last_obs
            .map(|o| u64::from(o.feedback.recv_bytes_per_s).saturating_mul(8))
            .filter(|bps| *bps > 0)
            .unwrap_or_else(|| self.rate.rate_bps())
            .max(MIN_DRAIN_BPS);
        while let Some((id, at, bytes)) = self.unacked.front().copied() {
            let transfer_us = bytes.saturating_mul(8_000_000) / drain_bps;
            if now.since(at) < ack_timeout.saturating_add(transfer_us) {
                break;
            }
            self.unacked.pop_front();
            self.lost(id);
        }
        let (lost, feedback) = std::mem::take(&mut self.notes);
        FrameJob {
            frame_id: self.last_frame.wrapping_add(1).max(1),
            budget: self.frame_budget(),
            lost,
            feedback,
        }
    }

    /// 完成一帧：把编码结果切成分片、加校验分片，返回要发出的数据报。
    /// `refine` 是编码器报告的“画面不变时还要不要继续交画面”。
    pub fn finish_frame(
        &mut self,
        now: Time,
        job: &FrameJob,
        encoded: Option<EncodedFrame>,
        refine: bool,
    ) -> Result<Vec<Outgoing>, Error> {
        self.refine = refine;
        let encode_us = std::mem::take(&mut self.encode_us);
        let Some(encoded) = encoded else {
            return Ok(Vec::new());
        };
        self.governor
            .on_frame(now, encode_us, encoded.payload.len(), job.budget);
        let frame_id = job.frame_id;
        // 关键帧参考自己；其余参考上一帧（视频编码的增量帧依赖它）。
        let ref_frame = if encoded.key || self.codec == Codec::Tile {
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
        let data_count = packets.len();
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
        self.recent.push_back((
            frame_id,
            now,
            out.get(..data_count).unwrap_or_default().to_vec(),
        ));
        self.last_frame = frame_id;
        if encoded.key {
            self.last_key = frame_id;
            self.stats.keyframes = self.stats.keyframes.saturating_add(1);
        }
        let wire_total: usize = out.iter().map(|o| o.body.len()).sum();
        self.unacked
            .push_back((frame_id, now, u64::try_from(wire_total).unwrap_or(u64::MAX)));
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

    /// 收到一个反馈数据报。返回要重传的分片。
    pub fn on_feedback_datagram(&mut self, now: Time, buf: &[u8]) -> Result<Vec<Outgoing>, Error> {
        let (feedback, ext, nacks) = decode_feedback(buf)?;
        self.observe(now, &feedback, ext);
        // 接收端还在为这些帧要重传：它们在路上、在进展，不能按超时判丢（判丢就要重发关键帧）。
        for (id, at, _) in &mut self.unacked {
            if nacks.iter().any(|(frame, _)| frame == id) {
                *at = now;
            }
        }
        let mut out = Vec::new();
        // 一轮重传不超过约两个反馈间隔能送完的量：一次全部补发会再次挤爆瓶颈的缓冲区，
        // 又丢一大半（窄带宽上大关键帧就是这样反复丢、半天收不齐）。没补到的下一轮还会再要。
        let allowance = self.rate.rate_bps().saturating_mul(RESEND_WINDOW_US) / 8_000_000;
        let mut resent_bytes = 0u64;
        for (frame, frag) in nacks {
            if resent_bytes >= allowance.max(1) {
                break;
            }
            let again = self
                .resent
                .get(&(frame, frag))
                .is_none_or(|at| now.since(*at) >= self.rtt_us.max(1));
            let packet = self
                .recent
                .iter()
                .find(|(id, _, _)| *id == frame)
                .and_then(|(_, _, packets)| packets.get(usize::from(frag)));
            if let Some(packet) = packet.filter(|_| again) {
                resent_bytes = resent_bytes
                    .saturating_add(u64::try_from(packet.body.len()).unwrap_or(u64::MAX));
                out.push(packet.clone());
                self.resent.insert((frame, frag), now);
            }
        }
        self.stats.retransmitted = self
            .stats
            .retransmitted
            .saturating_add(u32::try_from(out.len()).unwrap_or(u32::MAX));
        let wire: usize = out.iter().map(|o| o.body.len()).sum();
        self.stats.wire_bytes = self
            .stats
            .wire_bytes
            .saturating_add(u64::try_from(wire).unwrap_or(u64::MAX));
        Ok(out)
    }

    /// 还没确认的字节数。
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.unacked.iter().map(|(_, _, bytes)| *bytes).sum()
    }

    /// 带宽估计器的名字。
    #[must_use]
    pub fn estimator(&self) -> &'static str {
        self.rate.name()
    }

    /// 最近一次交给估计器的观测（特征记录用）。
    #[must_use]
    pub const fn last_observation(&self) -> Option<Observation> {
        self.last_obs
    }

    /// 每帧字节预算：码率摊到每帧，打一个折扣给重传和控制消息留余量。
    ///
    /// 排队延迟越接近目标（[`QUEUE_TARGET_US`]），预算越小：一帧要在目标时间内送到，已经排着的
    /// 时间要扣掉。这样排队刚开始涨，下一帧就先变小，不用等码率调下来。最少保留四分之一。
    fn frame_budget(&self) -> usize {
        let per_frame = self.rate.rate_bps() / 8 / u64::from(self.governor.fps().max(1));
        let budget =
            per_frame.saturating_mul(u64::from(self.rate.profile().safety_permille)) / 1000;
        let queue = u64::from(self.last_obs.map_or(0, |o| o.feedback.queue_delay_us));
        let left = QUEUE_TARGET_US
            .saturating_sub(queue)
            .max(QUEUE_TARGET_US / 4);
        usize::try_from(budget.saturating_mul(left) / QUEUE_TARGET_US).unwrap_or(usize::MAX)
    }

    fn observe(&mut self, now: Time, feedback: &Feedback, ext: Extension) {
        let obs = Observation {
            feedback: *feedback,
            ext,
            rtt_us: self.rtt_us,
            in_flight: self.in_flight(),
        };
        self.rate.on_observation(now, &obs);
        self.last_obs = Some(obs);
        self.acknowledge(feedback);
    }

    /// 接收端的反馈（没有扩展测量时用）。
    pub fn on_feedback(&mut self, now: Time, feedback: &Feedback) {
        self.observe(now, feedback, Extension::default());
    }

    fn acknowledge(&mut self, feedback: &Feedback) {
        match self.encoder.as_mut() {
            Some(encoder) => encoder.on_feedback(feedback.floor, feedback.delivered),
            None => self.notes.1 = Some((feedback.floor, feedback.delivered)),
        }
        while let Some((id, _, _)) = self.unacked.front().copied() {
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

/// 交给解码线程的一帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeJob {
    /// 帧号。
    pub frame_id: u32,
    /// 帧头。
    pub header: FrameHeader,
    /// 码流。
    pub data: Vec<u8>,
}

/// 解码线程上的解码器。自己也记着有没有失败过：失败之后、关键帧到来之前的帧都不解，
/// 免得把接不上的帧交给解码器（接收端知道失败之前可能已经派出了几帧）。
pub struct DecodeWorker {
    decoder: Box<dyn VideoDecoder>,
    broken: bool,
}

impl std::fmt::Debug for DecodeWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodeWorker")
            .field("broken", &self.broken)
            .finish_non_exhaustive()
    }
}

impl DecodeWorker {
    /// 创建。
    #[must_use]
    pub fn new(decoder: Box<dyn VideoDecoder>) -> Self {
        Self {
            decoder,
            broken: false,
        }
    }

    /// 解一帧。返回解出的画面（`None` 表示这一帧没有新画面）；失败或因为等关键帧而没解时返回错误。
    pub fn decode(&mut self, job: &DecodeJob) -> Result<Option<Screen>, Error> {
        let independent = job.header.key || job.header.codec == Codec::Tile;
        if self.broken && !independent {
            return Err(Error::Inconsistent("之前的帧解码失败，等关键帧"));
        }
        match self.decoder.decode(&job.header, &job.data) {
            Ok(screen) => {
                self.broken = false;
                Ok(screen.cloned())
            }
            Err(e) => {
                self.broken = true;
                Err(e)
            }
        }
    }
}

/// 画面接收端。
///
/// 两种用法：解码器交给它（[`MediaReceiver::new`]），[`MediaReceiver::on_datagram`] 直接解出画面（仿真用）；
/// 或者解码器放在别的线程（[`MediaReceiver::detached`]），[`MediaReceiver::on_datagram_deferred`]
/// 交出解码任务，由 [`DecodeWorker`] 在解码线程上执行。
pub struct MediaReceiver {
    decoder: Option<Box<dyn VideoDecoder>>,
    reassembler: Reassembler,
    tracker: DelayTracker,
    metrics: ReceiverMetrics,
    log: DeliveryLog,
    /// 最后交给解码器的帧号；`None` 表示要等关键帧。
    decoded: Option<u32>,
    stats: ReceiverStats,
    last_feedback: Option<Feedback>,
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
            decoder: Some(decoder),
            ..Self::detached(now, deadline_us)
        }
    }

    /// 创建，解码器放在别的线程（见类型说明）。
    #[must_use]
    pub fn detached(now: Time, deadline_us: u64) -> Self {
        Self {
            decoder: None,
            reassembler: Reassembler::new(deadline_us, 16),
            tracker: DelayTracker::new(now),
            metrics: ReceiverMetrics::new(),
            log: DeliveryLog::new(),
            decoded: None,
            stats: ReceiverStats::default(),
            last_feedback: None,
        }
    }

    /// 计数。
    #[must_use]
    pub const fn stats(&self) -> ReceiverStats {
        self.stats
    }

    /// 最近显示的画面。
    #[must_use]
    pub fn screen(&self) -> Option<&Screen> {
        self.decoder.as_ref().and_then(|d| d.screen())
    }

    /// 打开或关闭关键帧宽限（解码器是 H.264 时打开，见 [`crate::transport::Reassembler::set_keyframe_grace`]）。
    pub fn set_keyframe_grace(&mut self, on: bool) {
        self.reassembler.set_keyframe_grace(on);
    }

    /// 分片重组的计数（例如超时作废了多少帧）。
    #[must_use]
    pub const fn reassembly(&self) -> crate::transport::ReassemblyStats {
        self.reassembler.stats()
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
        let Some(job) = self.assemble(now, send_time, seq, flags, body)? else {
            return Ok(None);
        };
        let decoder = self.decoder.as_mut().ok_or(Error::InvalidArgument(
            "解码器不在这里：用 on_datagram_deferred",
        ))?;
        match decoder.decode(&job.header, &job.data) {
            Ok(Some(screen)) => {
                self.decoded = Some(job.frame_id);
                self.log.delivered(job.frame_id);
                self.stats.decoded = self.stats.decoded.saturating_add(1);
                Ok(Some(Shown {
                    frame_id: job.frame_id,
                    key: job.header.key,
                    screen,
                }))
            }
            Ok(None) => {
                self.decoded = Some(job.frame_id);
                self.log.delivered(job.frame_id);
                Ok(None)
            }
            Err(_) => {
                self.decoded = None;
                self.stats.bad = self.stats.bad.saturating_add(1);
                self.log.advance(job.frame_id);
                Ok(None)
            }
        }
    }

    /// 解码器放在别的线程时用：收齐一帧、而且它接得上时，返回交给解码线程的任务。
    /// 这一帧先算作送达；解码线程报告失败时调用 [`Self::decode_failed`]。
    pub fn on_datagram_deferred(
        &mut self,
        now: Time,
        send_time: Time,
        seq: u32,
        flags: u8,
        body: &[u8],
    ) -> Result<Option<DecodeJob>, Error> {
        let job = self.assemble(now, send_time, seq, flags, body)?;
        if let Some(job) = &job {
            self.decoded = Some(job.frame_id);
            self.log.delivered(job.frame_id);
        }
        Ok(job)
    }

    /// 解码线程解出了一帧。
    pub const fn decode_succeeded(&mut self) {
        self.stats.decoded = self.stats.decoded.saturating_add(1);
    }

    /// 解码线程解失败了（或者因为之前失败过而没解）：之后等关键帧。
    pub const fn decode_failed(&mut self) {
        self.decoded = None;
        self.stats.bad = self.stats.bad.saturating_add(1);
    }

    /// 重组分片；收齐一帧并且它接得上时返回解码任务。
    fn assemble(
        &mut self,
        now: Time,
        send_time: Time,
        seq: u32,
        flags: u8,
        body: &[u8],
    ) -> Result<Option<DecodeJob>, Error> {
        self.tracker.on_packet(now, send_time, seq, body.len());
        let mut r = Reader::new(body);
        let header = VideoHeader::decode(&mut r)?;
        self.metrics
            .on_packet(now, send_time, header.frame_id, body.len());
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
        Ok(Some(DecodeJob {
            frame_id: frame.frame_id,
            header: fh,
            data: data.to_vec(),
        }))
    }

    /// 让超时的帧作废。返回反馈里的 `floor`（据此判断有没有新情况要报告）和作废的帧号。
    pub fn settle(&mut self, now: Time) -> (u32, Vec<u32>) {
        let expired = self.reassembler.expire(now);
        if let Some(floor) = self.reassembler.floor() {
            self.log.advance(floor);
        }
        (self.log.snapshot().0, expired)
    }

    /// 有没有缺了一阵子的分片要请求重传。
    #[must_use]
    pub fn wants_nack(&self, now: Time) -> bool {
        !self.reassembler.missing(now, NACK_AFTER_US, 1).is_empty()
    }

    /// 生成一个反馈数据报：反馈加上缺失分片的重传请求。
    pub fn feedback_datagram(&mut self, now: Time) -> Vec<u8> {
        let feedback = self.feedback(now);
        let nacks = self.reassembler.missing(now, NACK_AFTER_US, MAX_NACKS);
        encode_feedback(&feedback, &self.metrics.report(), &nacks)
    }

    /// 生成一条反馈，同时让超时的帧作废。
    pub fn feedback(&mut self, now: Time) -> Feedback {
        self.settle(now);
        let sample = self.tracker.report(now);
        let (floor, delivered) = self.log.snapshot();
        let feedback = Feedback {
            floor,
            delivered,
            queue_delay_us: sample.queue_delay_us,
            loss_permille: sample.loss_permille,
            recv_bytes_per_s: sample.recv_bytes_per_s,
        };
        self.last_feedback = Some(feedback);
        feedback
    }

    /// 最近一次反馈（丢包率、排队延迟等，界面显示网络质量用）。
    #[must_use]
    pub const fn last_feedback(&self) -> Option<Feedback> {
        self.last_feedback
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
        fn screen(&self) -> Option<&Screen> {
            self.screen.as_ref()
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

    #[test]
    fn feedback_datagram_round_trip_and_rejects_garbage() {
        let fb = Feedback {
            floor: 7,
            delivered: 3,
            queue_delay_us: 1,
            loss_permille: 2,
            recv_bytes_per_s: 9,
        };
        let ext = Extension {
            trend_tenth_ms: -5,
            groups: 3,
            train_bytes_per_s: 1,
        };
        assert_eq!(
            decode_feedback(&encode_feedback(&fb, &ext, &[])).unwrap(),
            (fb, ext, vec![])
        );
        let nacks: Vec<(u32, u16)> = (0..60).map(|i| (5, i)).collect();
        let (back, _, got) = decode_feedback(&encode_feedback(&fb, &ext, &nacks)).unwrap();
        assert_eq!((back, got.len()), (fb, MAX_NACKS), "最多带 {MAX_NACKS} 个");
        assert!(decode_feedback(&[0; 5]).is_err(), "截断");
        assert!(decode_feedback(&fb.encode()).is_err(), "缺扩展测量");
        let with_ext = encode_feedback(&fb, &ext, &[]);
        let mut zero = with_ext.clone();
        zero.push(0);
        assert!(decode_feedback(&zero).is_err(), "个数为 0");
        let mut short = with_ext;
        short.extend([2, 0, 0, 0, 1, 0, 1]);
        assert!(decode_feedback(&short).is_err(), "说有 2 个，只有 1 个");
    }

    #[test]
    fn lost_fragments_are_retransmitted_once_per_rtt() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let mut rx = MediaReceiver::new(Time::ZERO, 250_000, Box::new(FakeDecoder::default()));
        tx.set_profile(Profile {
            fec_permille: 0,
            ..Profile::STABLE
        });
        let out = tx
            .on_frame(Time::ZERO, &Screen::new(16, 16, 3), true, 40_000)
            .unwrap();
        assert!(out.len() > 3, "关键帧跨好几个分片");
        // 丢第 2 片：收不齐
        assert!(deliver(&mut rx, Time::ZERO, &out, |i| i == 1).is_empty());
        assert!(!rx.wants_nack(Time(1_000)), "刚缺：可能只是乱序");
        assert!(rx.wants_nack(Time(20_000)));
        let fb = rx.feedback_datagram(Time(20_000));
        let again = tx.on_feedback_datagram(Time(20_100), &fb).unwrap();
        assert_eq!(again, vec![out[1].clone()], "只重传缺的那一片");
        // 一个往返之内再要：不重复发
        assert!(tx
            .on_feedback_datagram(Time(30_000), &fb)
            .unwrap()
            .is_empty());
        assert_eq!(tx.stats().retransmitted, 1);
        // 重传到了：整帧收齐、显示
        let shown = deliver(&mut rx, Time(60_000), &again, |_| false);
        assert_eq!(shown, vec![(1, 3)]);
        // 过了一个往返还缺（假设重传也丢了）：可以再发
        assert_eq!(tx.on_feedback_datagram(Time(70_000), &fb).unwrap().len(), 1);
        // 过了截止时间：不再保留，不重传
        let _ = tx
            .on_frame(Time(400_000), &Screen::new(16, 16, 4), true, 40_000)
            .unwrap();
        assert!(tx
            .on_feedback_datagram(Time(400_100), &fb)
            .unwrap()
            .is_empty());
        assert!(tx.on_feedback_datagram(Time(400_200), &[1]).is_err());
    }

    #[test]
    fn detached_encoder_gives_the_same_packets_and_gets_the_notes() {
        let mut local = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let mut remote = MediaSender::detached(cfg(), Codec::H264);
        let mut encoder = FakeEncoder::default();
        assert_eq!(remote.codec(), Codec::H264);
        assert!(
            remote
                .on_frame(Time::ZERO, &Screen::new(8, 8, 1), true, 0)
                .is_err(),
            "编码器不在这里"
        );
        for (i, color) in [1u32, 2, 3].into_iter().enumerate() {
            let t = Time(u64::try_from(i).unwrap() * 33_000);
            let screen = Screen::new(16, 16, color);
            let a = local.on_frame(t, &screen, true, 10_000).unwrap();
            let job = remote.start_frame(t, 10_000);
            let (encoded, refine) = encode_job(&mut encoder, &job, &screen, true).unwrap();
            let b = remote.finish_frame(t, &job, encoded, refine).unwrap();
            assert_eq!(a, b, "两种用法发出的数据报一样");
        }
        // 判定丢失、收到确认：攒着，随下一个任务交给编码器
        let fb = Feedback {
            floor: 3,
            delivered: 0b110,
            queue_delay_us: 0,
            loss_permille: 0,
            recv_bytes_per_s: 0,
        };
        remote.on_feedback(Time(100_000), &fb);
        assert!(remote.wants_frame(), "有丢失要恢复");
        let job = remote.start_frame(Time(110_000), 10_000);
        assert_eq!(
            (job.lost.clone(), job.feedback),
            (vec![3], Some((3, 0b110)))
        );
        let (encoded, refine) =
            encode_job(&mut encoder, &job, &Screen::new(16, 16, 3), false).unwrap();
        assert!(encoded.unwrap().key, "编码器收到了丢失通知，出关键帧");
        assert!(remote
            .finish_frame(Time(110_000), &job, None, refine)
            .unwrap()
            .is_empty());
        assert!(!remote.wants_frame());
    }

    #[test]
    fn deferred_decoding_waits_for_a_keyframe_after_a_failure() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let mut rx = MediaReceiver::detached(Time::ZERO, 250_000);
        let mut worker = DecodeWorker::new(Box::new(FakeDecoder::default()));
        assert!(format!("{worker:?}").contains("broken"));
        assert!(rx.screen().is_none());
        let step = |tx: &mut MediaSender, rx: &mut MediaReceiver, color: u32, t: u64| {
            let out = tx
                .on_frame(Time(t), &Screen::new(8, 8, color), true, 0)
                .unwrap();
            let mut jobs = Vec::new();
            for (i, o) in out.iter().enumerate() {
                let seq = u32::try_from(i).unwrap();
                if let Some(job) = rx
                    .on_datagram_deferred(Time(t), Time(t), seq, o.flags, &o.body)
                    .unwrap()
                {
                    jobs.push(job);
                }
            }
            jobs
        };
        // 关键帧和一个增量帧都派出去了；解码器先解关键帧，再“解失败”增量帧
        let key = step(&mut tx, &mut rx, 1, 0);
        let bad = step(&mut tx, &mut rx, 0x00de_ad00, 1);
        let next = step(&mut tx, &mut rx, 3, 2);
        assert_eq!(worker.decode(&key[0]).unwrap().unwrap().pixels()[0], 1);
        rx.decode_succeeded();
        assert!(worker.decode(&bad[0]).is_err());
        rx.decode_failed();
        // 接收端知道失败之前已经派出的下一帧：解码线程自己拒绝
        assert!(worker.decode(&next[0]).is_err());
        // 之后的增量帧：接收端不再派
        assert!(step(&mut tx, &mut rx, 4, 3).is_empty());
        assert_eq!((rx.stats().decoded, rx.stats().bad), (1, 1));
        // 一步式接口在没有解码器时报错
        let mut lone = MediaReceiver::detached(Time::ZERO, 250_000);
        let mut fresh = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let out = fresh
            .on_frame(Time::ZERO, &Screen::new(8, 8, 5), true, 0)
            .unwrap();
        let results: Vec<bool> = out
            .iter()
            .map(|o| {
                lone.on_datagram(Time::ZERO, Time::ZERO, 0, o.flags, &o.body)
                    .is_err()
            })
            .collect();
        assert_eq!(results.last(), Some(&true), "收齐时没有解码器可用");
        // 发送端知道丢失之后出关键帧，解码线程恢复
        let fb = rx.feedback(Time(5));
        tx.on_feedback(Time(5), &fb);
        let key = step(&mut tx, &mut rx, 6, 6);
        assert_eq!(worker.decode(&key[0]).unwrap().unwrap().pixels()[0], 6);
    }

    #[test]
    fn budget_shrinks_as_the_queue_grows() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let full = tx.frame_budget();
        assert!(full > 0);
        let fb = |q| Feedback {
            floor: 0,
            delivered: 0,
            queue_delay_us: q,
            loss_permille: 0,
            recv_bytes_per_s: 0,
        };
        tx.on_feedback(Time(1), &fb(25_000));
        let half = tx.frame_budget();
        assert!(half < full && half > full / 3, "{full} {half}");
        tx.on_feedback(Time(2), &fb(200_000));
        assert!(
            tx.frame_budget() >= tx.frame_budget().min(full / 4),
            "最少保留四分之一"
        );
        assert!(tx.frame_budget() < half);
    }
}
