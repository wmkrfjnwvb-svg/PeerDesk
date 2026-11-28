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
use crate::cc::alr::Alr;
use crate::cc::probe::ProbeCluster;
use crate::cc::report::{ArrivalRecorder, ArrivalReport, PacketKind, SendHistory, SentPacket};
use crate::error::Error;
use crate::estimator::{
    EstimatorKind, Extension, NetCause, Observation, ProbeAsk, ProbeEnd, ProbeReport, ProbeVerdict,
    RateEstimator, ReceiverMetrics, SendLimit,
};
use crate::frame::{ContentDesc, Frame, Picture};
use crate::governor::FpsGovernor;
use crate::pacer::FLAG_PADDING;
use crate::policy::ContentPolicy;
use crate::proto::{VideoHeader, MAX_DATAGRAM};
use crate::scale::ScalePolicy;
use crate::screen::Screen;
use crate::time::Time;
use crate::transport::{
    fragment, parity_for, rs_parity_for, Fec, Reassembler, VideoPacket, PROGRESS, RS_PARITY_PREFIX,
    WHOLE_FRAME,
};
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
/// 一份反馈里最多带几帧的进度（见 [`crate::transport::PROGRESS`]）。
const PROGRESS_FRAMES: usize = 4;
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

/// 反馈数据报，探测结果带组号的格式（双方都支持 [`crate::proto::Caps::PROBE_ID`] 时用）：和 [`encode_feedback`]
/// 一样，有探测结果时在最后多 [`ProbeReport::LEN`] 字节。没有探测结果的反馈和老格式逐字节相同——反馈也走瓶颈，
/// 每份多一两个字节都会让时序和老版本不一样；绝大多数反馈没有探测结果，也省流量。
/// 有没有探测结果按长度分辨（见 [`decode_feedback_ids`]）。
#[must_use]
pub fn encode_feedback_ids(
    feedback: &Feedback,
    ext: &Extension,
    nacks: &[(u32, u16)],
    probe: Option<&ProbeReport>,
) -> Vec<u8> {
    let mut out = encode_feedback(feedback, ext, nacks);
    if let Some(p) = probe {
        let mut w = Writer::new();
        p.encode(&mut w);
        out.extend(w.into_vec());
    }
    out
}

/// 解析探测结果带组号的反馈数据报（见 [`encode_feedback_ids`]）。
///
/// 扩展测量之后剩下的长度：没有重传请求、没有探测结果是 0；只有重传请求是 1 + 6n（除以 6 余 1）；
/// 只有探测结果是 11（余 5）；两样都有是 12 + 6n（余 0）。三种余数互不相同，所以“余数不是 1”就是带探测结果。
pub fn decode_feedback_ids(buf: &[u8]) -> Result<(DecodedFeedback, Option<ProbeReport>), Error> {
    let rest = buf
        .len()
        .saturating_sub(Feedback::LEN)
        .saturating_sub(Extension::LEN);
    if rest == 0 || rest % 6 == 1 {
        return Ok((decode_feedback(buf)?, None));
    }
    let (head, tail) = buf
        .split_at_checked(buf.len().saturating_sub(ProbeReport::LEN))
        .ok_or(Error::Truncated {
            need: ProbeReport::LEN,
            have: buf.len(),
        })?;
    let mut r = Reader::new(tail);
    let probe = ProbeReport::decode(&mut r)?;
    r.finish()?;
    Ok((decode_feedback(head)?, Some(probe)))
}

/// 解析出的反馈数据报：基本反馈、扩展测量、重传请求。
pub type DecodedFeedback = (Feedback, Extension, Vec<(u32, u16)>);

/// 反馈数据报（双方都支持到达报告时，见 [`crate::proto::Caps::TRANSPORT_CC`]）：
/// [`Feedback`]、重传请求个数（可以为 0）和请求、有没有到达报告（0/1）、到达报告。
/// 接收端算的排队延迟、趋势、包串（[`Extension`]）不再发：网络判断都在发送端（见 [`crate::cc`]）。
#[must_use]
pub fn encode_feedback_v2(
    feedback: &Feedback,
    nacks: &[(u32, u16)],
    report: Option<&ArrivalReport>,
) -> Vec<u8> {
    let mut w = Writer::new();
    w.bytes(&feedback.encode());
    let nacks = nacks.get(..MAX_NACKS.min(nacks.len())).unwrap_or_default();
    w.u8(u8::try_from(nacks.len()).unwrap_or(u8::MAX));
    for (frame, frag) in nacks {
        w.u32(*frame).u16(*frag);
    }
    match report {
        Some(r) => {
            w.u8(1);
            r.encode(&mut w);
        }
        None => {
            w.u8(0);
        }
    }
    w.into_vec()
}

/// 解析出的反馈数据报（带到达报告的格式）。
pub type DecodedFeedbackV2 = (Feedback, Vec<(u32, u16)>, Option<ArrivalReport>);

/// 解析带到达报告的反馈数据报（见 [`encode_feedback_v2`]）。
pub fn decode_feedback_v2(buf: &[u8]) -> Result<DecodedFeedbackV2, Error> {
    let (head, rest) = buf
        .split_at_checked(Feedback::LEN)
        .ok_or(Error::Truncated {
            need: Feedback::LEN,
            have: buf.len(),
        })?;
    let feedback = Feedback::decode(head)?;
    let mut r = Reader::new(rest);
    let count = usize::from(r.u8()?);
    if count > MAX_NACKS {
        return Err(Error::TooManyItems {
            len: count,
            max: MAX_NACKS,
        });
    }
    let nacks = (0..count)
        .map(|_| Ok((r.u32()?, r.u16()?)))
        .collect::<Result<Vec<_>, Error>>()?;
    let report = match r.u8()? {
        0 => None,
        _ => Some(ArrivalReport::decode(&mut r)?),
    };
    r.finish()?;
    Ok((feedback, nacks, report))
}

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

/// 丢包率达到这个值（千分比）才按丢包加纠错。
const FEC_FROM_LOSS_PERMILLE: u16 = 10;
/// 纠错比例上限（千分比）：再多就是浪费带宽，丢包更严重时靠重传和降码率。
const FEC_MAX_PERMILLE: u16 = 330;

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
    /// 内容策略（帧率档位、码率过期阈值等，见 [`crate::policy`]）。
    pub policy: ContentPolicy,
}

impl Default for SenderConfig {
    fn default() -> Self {
        Self {
            fps: 30,
            start_bps: ContentPolicy::OFFICE.start_bps,
            min_bps: 100_000,
            max_bps: ContentPolicy::OFFICE.max_bps,
            profile: Profile::STABLE,
            // 要留够时间给丢失的分片重传一次（大约一个往返）。
            deadline_us: 250_000,
            max_payload: MAX_DATAGRAM - HEADER_LEN,
            estimator: EstimatorKind::PRODUCT,
            policy: ContentPolicy::OFFICE,
        }
    }
}

/// 一个要发出的视频数据报（不含数据报头）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    /// 数据报标志位（校验分片带 [`FLAG_PARITY`]，探测的填充包带 [`crate::pacer::FLAG_PADDING`]）。
    pub flags: u8,
    /// 载荷：分片头加分片数据。
    pub body: Vec<u8>,
    /// 种类（发出时记进发送历史，见 [`crate::cc::report`]）。
    pub kind: PacketKind,
    /// 属于哪一帧的哪个分片 `(帧号, 分片序号)`（校验分片是组号）；探测的填充包没有。
    /// 节奏器据此丢掉已经有定论的帧的包，重传据此记“实际发出”的时间。
    pub fragment: Option<(u32, u16)>,
}

/// 留着重传的一帧数据分片。
#[derive(Debug, Clone)]
struct Kept {
    frame: u32,
    packets: Vec<Outgoing>,
    /// 原包从节奏器实际发出的时间（`None`：还在排队）。还在排队的不重传：接收端只知道“没收到”，
    /// 不知道它还没发（窄带宽上一个大关键帧要分几秒发完）；刚发出的也不重传：它还在路上。
    /// 照 WebRTC `RtpPacketHistory`：还在等发送的包不响应重传请求，上次实际发出后一个往返之内也不响应。
    sent: Vec<Option<Time>>,
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
    /// 因为持续丢包从“稳定”换成“无线”画像的次数。
    pub profile_switches: u32,
    /// 收到的重传请求（分片数）。
    pub nack_requests: u32,
    /// 重传请求里因为“刚发过（原包或重传）、可能还在路上”而没有再发的分片数。
    pub resend_skipped: u32,
    /// 因为一轮重传额度用完而没有发的分片数。
    pub resend_capped: u32,
    /// 重传请求里这个分片（原包或上一份重传）还在节奏器里排队、所以没有再排一份的分片数。
    pub resend_unsent: u32,
    /// 探测的流量和结论。
    pub probes: ProbeStats,
}

/// 探测的流量和结论（探测流量是成本：中继按流量计费；每轮探测必有结论）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProbeStats {
    /// 发出的探测字节数（含数据报头）：起步探测、静止探测、运行中探测。
    pub start_bytes: u64,
    /// 见 [`Self::start_bytes`]。
    pub idle_bytes: u64,
    /// 见 [`Self::start_bytes`]。
    pub run_bytes: u64,
    /// 运行中探测发出的轮数（待发时就中止的不算），核实和周期各几轮。
    pub run_sent: u32,
    /// 见 [`Self::run_sent`]。
    pub verify_rounds: u32,
    /// 见 [`Self::run_sent`]。
    pub periodic_rounds: u32,
    /// 运行中探测各种结论的次数，下标见 [`ProbeVerdict::index`]（含待发时就中止的）。
    pub run_verdicts: [u32; 4],
    /// 静止探测发出的组数和各种结论的次数。
    pub idle_sent: u32,
    /// 见 [`Self::idle_sent`]。
    pub idle_verdicts: [u32; 4],
}

impl ProbeStats {
    /// 全部探测字节。
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.start_bytes
            .saturating_add(self.idle_bytes)
            .saturating_add(self.run_bytes)
    }

    /// 把另一次运行的计数加进来。
    pub fn merge(&mut self, o: &Self) {
        self.start_bytes = self.start_bytes.saturating_add(o.start_bytes);
        self.idle_bytes = self.idle_bytes.saturating_add(o.idle_bytes);
        self.run_bytes = self.run_bytes.saturating_add(o.run_bytes);
        self.run_sent = self.run_sent.saturating_add(o.run_sent);
        self.verify_rounds = self.verify_rounds.saturating_add(o.verify_rounds);
        self.periodic_rounds = self.periodic_rounds.saturating_add(o.periodic_rounds);
        self.idle_sent = self.idle_sent.saturating_add(o.idle_sent);
        for (a, b) in self.run_verdicts.iter_mut().zip(o.run_verdicts) {
            *a = a.saturating_add(b);
        }
        for (a, b) in self.idle_verdicts.iter_mut().zip(o.idle_verdicts) {
            *a = a.saturating_add(b);
        }
    }

    /// 一行报告。`total_bytes` 是这段时间发出的全部字节（算探测占比）。
    #[must_use]
    pub fn line(&self, total_bytes: u64) -> String {
        let verdicts = |v: &[u32; 4]| {
            ProbeVerdict::ALL
                .iter()
                .zip(v)
                .map(|(k, n)| format!("{} {n}", k.name()))
                .collect::<Vec<_>>()
                .join("、")
        };
        let share = self.bytes().saturating_mul(10_000) / total_bytes.max(1);
        format!(
            "探测流量 {} B（起步 {}、静止 {}、运行中 {}），占全部流量 {}.{:02}%；运行中探测发出 {} 轮（核实 {}、周期 {}），\
             结论：{}；静止探测 {} 组，结论：{}",
            self.bytes(),
            self.start_bytes,
            self.idle_bytes,
            self.run_bytes,
            share / 100,
            share % 100,
            self.run_sent,
            self.verify_rounds,
            self.periodic_rounds,
            verdicts(&self.run_verdicts),
            self.idle_sent,
            verdicts(&self.idle_verdicts),
        )
    }
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
    /// 按源画面的多少编码（千分比）。
    pub scale_permille: u16,
    /// 控制端显示画面的区域（像素），编码尺寸不超过它；`None` 表示不限。
    pub view: Option<(u16, u16)>,
    /// 码率估计过期了（见 [`VideoEncoder::set_rate_stale`]）。
    pub rate_stale: bool,
}

/// 在编码器所在的地方执行一个任务。返回编码结果，以及之后画面不变时还要不要继续交画面（补清晰、恢复）。
pub fn encode_job(
    encoder: &mut dyn VideoEncoder,
    job: &FrameJob,
    frame: &Frame,
    changed: bool,
) -> Result<(Option<EncodedFrame>, bool), Error> {
    prepare(encoder, job);
    let encoded = encoder.encode_frame(job.frame_id, frame, changed, job.budget)?;
    Ok((encoded, encoder.wants_refine()))
}

/// 编码前把任务里的丢帧、确认、比例、控制端窗口交给编码器。
fn prepare(encoder: &mut dyn VideoEncoder, job: &FrameJob) {
    for id in &job.lost {
        encoder.on_lost(*id);
    }
    if let Some((floor, mask)) = job.feedback {
        encoder.on_feedback(floor, mask);
    }
    encoder.set_scale(job.scale_permille);
    encoder.set_view(job.view);
    encoder.set_rate_stale(job.rate_stale);
}

/// 静止时的探测（见 [`MediaSender::set_idle_probe`]）：停下多久之后第一次探（微秒），之后间隔翻倍，最长这么久。
const IDLE_PROBE_FIRST_US: u64 = 2_000_000;
const IDLE_PROBE_MAX_GAP_US: u64 = 30_000_000;
/// 一次探测几个包（每个约 1200 字节）、按当前码率的几倍发（能测到这么高的容量）。
const IDLE_PROBE_PACKETS: u32 = 8;
const IDLE_PROBE_SCALE: u64 = 4;
/// 静止探测的组号从这里起（和网络控制器的探测组号分开）。
const IDLE_PROBE_ID_BASE: u32 = 0x8000_0000;

/// 第一个关键帧的送达速度（字节 ÷ 从发出到确认扣掉往返时间）至少是当时码率的这么多（千分比）才起步探测。
/// 链路只比起步码率宽一点时探测包反而添堵：先探测再出画面时微软窄带轨迹卡顿 76 → 130 次，门槛 0.8 倍时
/// 76 → 87 次（多在 1–8 Mbps 的轨迹），2 倍时和不探一样。不扣往返时间的话，小关键帧（十几 KB）测的主要是往返
/// 时间：30 Mbps、往返 30 ms 的链路只算出 1.7 Mbps。
const START_PROBE_GATE_PERMILLE: u64 = 2000;
/// 起步探测（见 [`MediaSender::set_start_probe`]）：第一组按起步码率的几倍，达到目标的多少（千分比）就按
/// 结果的几倍接着探；每组匀速发多久（微秒）；最多探多久（微秒，到时不管有没有探完都开始出画面）。
/// 每组发 30 ms 而不是 WebRTC 的 15 ms：接收端是应用读数据报时打的时间戳，程序忙一下几十个包一次读走，
/// 时间段太短测不准（2026-11-26 用户日志里帧的包串测出 Tbps）。
const START_PROBE_FIRST_SCALE: u64 = 4;
const START_PROBE_FURTHER_PERMILLE: u64 = 700;
/// 之后每组按结果的几倍：4 倍时最坏过冲约 5.7 倍（4 ÷ 0.7），瓶颈多排约 140 ms（2026-11-26 独立审查）；
/// WebRTC 第一轮之后也是 2 倍。第一组的 4 倍是对起步码率（只有十几 KB），不算过冲。
const START_PROBE_STEP: u64 = 2;
/// 会话循环多久交一次探测组给节奏器（微秒，被控端 `cc_tick`）：决定发探测组到它真的发出去最多差这么久。
const START_PROBE_TICK_US: u64 = 25_000;
const START_PROBE_SPAN_US: u64 = 30_000;
/// 起步探测每组至少几个包（速率低时 30 ms 只有几个包，延迟回归不稳）。
const START_PROBE_MIN_PACKETS: u32 = 16;
const START_PROBE_MAX_US: u64 = 1_000_000;
/// 起步探测的组号从这里起（和静止探测、网络控制器的探测组号分开）。
const START_PROBE_ID_BASE: u32 = 0x4000_0000;

/// 等第一个关键帧送到最多等这么久（微秒），到点就不做起步探测、结束“起步”。之后探测本身最多
/// [`START_PROBE_MAX_US`]，所以“起步”最长 3 秒，每个状态都走得完。
/// 2 秒的依据：144 次微软轨迹里“起步”实际持续 0.2–0.8 秒（中位 0.2 秒，最长 0.81 秒），大屏 Wi-Fi 场景 0.46 秒；
/// 2 秒也是“第一幅画面”的门槛。2 秒还没确认的关键帧算下来不可能比起步码率快 2 倍（探测的条件），本来也不会探。
const START_PROBE_WAIT_MAX_US: u64 = 2_000_000;
/// 静止探测发出之后这么久（微秒）没有结果算超时（收不齐的组，接收端要等两份没有新包的反馈才结算，
/// 静止时反馈 1 秒一份，所以最晚约 2 秒多）。
const IDLE_PROBE_TIMEOUT_US: u64 = 3_000_000;
/// 运行中探测的组号从这里起（和起步、静止探测的组号分开）。
const RUN_PROBE_ID_BASE: u32 = 0x2000_0000;
/// 运行中探测等能探的时机最多等这么久（微秒），到点中止。
const RUN_PROBE_WAIT_US: u64 = 200_000;
/// 运行中探测等结果：往返 + 一组的时长 + 这么久（微秒：交给节奏器最多等一个定时周期 25 ms、接收端一次反馈间隔
/// 20 ms、余量 20 ms）。
const RUN_PROBE_SLACK_US: u64 = 65_000;

/// 运行中探测在发送端的状态机（要不要探、结论怎么用在估计器，见 [`RateEstimator::probe_wanted`]）。
/// 每一轮必然以证实、否定、超时、中止之一结束（I4）：待发最多 [`RUN_PROBE_WAIT_US`]，已发最多
/// 往返 + 一组的时长 + [`RUN_PROBE_SLACK_US`]。
///
/// 探测期间照常出帧（和起步探测不同）：反馈里的探测结果带组号，帧的包串混不进来；应用受限时的帧很小，
/// 而每探一次停 100 多毫秒的画面本身就是卡顿。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunProbe {
    /// 空闲：每个定时周期问估计器要不要探。
    Idle,
    /// 待发：要探，等能探的时机（见 [`MediaSender::probe_blocked`]）。
    Pending { ask: ProbeAsk, since: Time },
    /// 已发：等带这个组号的结果。`partial` 是接收端收到一半时先报的结果。
    Sent {
        ask: ProbeAsk,
        id: u32,
        deadline: Time,
        partial: Option<u64>,
    },
}

/// 起步探测的进度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StartProbe {
    /// 发出第一组的时刻（还在等第一个关键帧送到时为 `None`）。
    started: Option<Time>,
    /// 现在这一组的组号、目标速率、发出的时刻，上一组的目标速率。
    id: u32,
    target_bps: u64,
    sent_at: Time,
    prev_target_bps: u64,
    /// 测到的最高速率。
    best_bps: u64,
    /// 这一组发出之后收到的最高结果（可能混着上一组收尾的结果，到时间再用它下结论）。
    round_bps: u64,
}

/// 连续这么久（微秒）没有任何网络受限的原因、预算桶也判成受限，才算应用受限（进慢）；一出现网络受限的原因
/// 马上不算（出快）。500 ms 是预算桶攒满的时间（[`crate::cc::alr`]，WebRTC 同值）。
const APP_LIMITED_AFTER_US: u64 = 500_000;
/// 编出的帧达到预算的这么多（千分比）就算编码器被码率卡着。
const NEAR_BUDGET_PERMILLE: usize = 800;

/// 帧率已经在最低档、帧还一直装不进预算这么久（微秒）就降分辨率（最后手段）。
const OVERFLOW_DOWN_US: u64 = 2_000_000;

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
    /// 还没有定论的帧的数据分片，留着重传。
    recent: VecDeque<Kept>,
    /// 每个分片的重传状态：`None` 还在节奏器里排队（不再排第二份），`Some(t)` 在 `t` 实际发出
    /// （之后一个往返加排队之内不重复重传）。照 WebRTC `RtpPacketHistory`：按实际发出的时间算，
    /// 不按交给节奏器的时间算——节奏器排一秒的时候，按交出时间算会每个往返再塞一份同样的包，
    /// 节奏器里全是重复的重传（2026-11 微软轨迹 ms-05729 实测：关键帧早送到了，节奏器里还有 164 KB 重传）。
    resent: BTreeMap<(u32, u16), Option<Time>>,
    rtt_us: u64,
    last_obs: Option<Observation>,
    /// 最近一次测量证实码率估计的时刻（见 [`Self::rate_stale`]）。
    rate_seen: Option<Time>,
    /// 连续有丢包的反馈次数（决定要不要换成“无线”画像）。
    lossy_feedbacks: u32,
    /// 纠错方式（握手协商）。
    fec: Fec,
    governor: FpsGovernor,
    /// 下一次 [`Self::finish_frame`] 用的编码时间（微秒），由编码线程报告。
    encode_us: u64,
    encode_hw: bool,
    /// 编码分辨率（只对 H.264；见 [`crate::scale`]）。
    scale: ScalePolicy,
    /// 编码尺寸要变（画面停下后升回原尺寸、控制端窗口变了）：画面不动也要编一帧。
    scale_up: bool,
    /// 最近发出的关键帧：(帧号, 发出时间)。
    key_sent: Option<(u32, Time)>,
    /// 控制端显示画面的区域（见 [`FrameJob::view`]）。
    view: Option<(u16, u16)>,
    /// 各模块的决定和原因，等会话取走写日志（见 [`Self::take_events`]）。
    events: Vec<String>,
    /// 用到达报告（双方都支持、估计器要它）。
    reports: bool,
    /// 静止时探测（见 [`Self::set_idle_probe`]）：下一次探测的时刻、到下一次的间隔、等结果的那一组。
    idle_probe: bool,
    next_probe: Option<Time>,
    probe_gap: u64,
    probing: Option<u32>,
    probes: u32,
    pending_probe: Option<ProbeCluster>,
    /// 起步探测（探完之前不出画面）。
    start_probe: Option<StartProbe>,
    /// 发出的包，等到达报告对账（见 [`crate::cc::report`]）。
    history: SendHistory,
    /// 发送量受谁限制（见 [`SendLimit`]）：预算桶（发出的量是不是明显少于码率），和最近一次网络受限的原因。
    alr: Alr,
    net_limited: Option<(Time, NetCause)>,
    /// 对端的反馈里探测结果带组号（见 [`Self::set_probe_ids`]）；运行中探测的状态机；会话开始的时刻；
    /// 静止探测发出的时刻；节奏器里上一帧是不是还没发完（最近一次 [`Self::frame_gate`] 时）。
    probe_ids: bool,
    run_probe: RunProbe,
    started: Option<Time>,
    probing_at: Time,
    probing_late: bool,
    pacer_busy: bool,
    /// 最近一次“有东西要发、被网络挡住”（该编帧时被拦、关键帧没确认、重传被截断）是什么时候。
    blocked_at: Option<Time>,
    /// 上一份反馈是什么时候处理的。
    observed_at: Option<Time>,
    /// 接收端报告的每一帧到了几个分片（见 [`crate::transport::PROGRESS`]）。
    progress: BTreeMap<u32, u16>,
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
            rate_seen: None,
            lossy_feedbacks: 0,
            fec: Fec::Xor,
            governor: FpsGovernor::with_policy(cfg.fps, &cfg.policy),
            encode_us: 0,
            encode_hw: false,
            scale_up: false,
            key_sent: None,
            view: None,
            events: Vec::new(),
            reports: false,
            idle_probe: false,
            next_probe: None,
            probe_gap: IDLE_PROBE_FIRST_US.saturating_mul(2),
            probing: None,
            probes: 0,
            pending_probe: None,
            start_probe: None,
            history: SendHistory::new(),
            alr: Alr::new(cfg.start_bps),
            net_limited: None,
            probe_ids: false,
            run_probe: RunProbe::Idle,
            started: None,
            probing_at: Time(0),
            probing_late: false,
            pacer_busy: false,
            blocked_at: None,
            observed_at: None,
            progress: BTreeMap::new(),
            scale: if codec == Codec::H264 {
                ScalePolicy::new()
            } else {
                ScalePolicy::full()
            },
        }
    }

    /// 编码格式。
    #[must_use]
    pub const fn codec(&self) -> Codec {
        self.codec
    }

    /// 打开到达报告（握手时双方都支持 [`crate::proto::Caps::TRANSPORT_CC`]）。估计器不用报告时不打开。
    pub fn set_reports(&mut self, on: bool) {
        self.reports = on && self.rate.uses_reports();
    }

    /// 用到达报告。
    #[must_use]
    pub const fn reports(&self) -> bool {
        self.reports
    }

    /// 一个数据报交给了网络（`seq` 是数据报头里的序号）。到达报告据此对账。
    pub fn on_packet_sent(&mut self, now: Time, seq: u32, out: &Outgoing) {
        // 发出的画面和重传都从“应用受限”的预算桶里扣
        let size = u64::try_from(out.body.len().saturating_add(HEADER_LEN)).unwrap_or(u64::MAX);
        // （探测包不算：它不是应用要发的量，算进去的话一组探测就把“应用受限”顶掉、接着的一轮被中止）
        if !matches!(out.kind, PacketKind::Probe(_)) {
            self.alr.on_sent(now, size);
        }
        if let PacketKind::Probe(id) = out.kind {
            let p = &mut self.stats.probes;
            let bytes = if id & IDLE_PROBE_ID_BASE != 0 {
                &mut p.idle_bytes
            } else if id & START_PROBE_ID_BASE != 0 {
                &mut p.start_bytes
            } else {
                &mut p.run_bytes
            };
            *bytes = bytes.saturating_add(size);
        }
        match (out.kind, out.fragment) {
            (PacketKind::Retransmit, Some(key)) => {
                if let Some(state) = self.resent.get_mut(&key) {
                    *state = Some(now);
                }
            }
            // 原包（数据分片；校验分片的序号是组号，不记）
            (PacketKind::Media, Some((frame, index))) if out.flags & FLAG_PARITY == 0 => {
                if let Some(sent) = self
                    .recent
                    .iter_mut()
                    .find(|k| k.frame == frame)
                    .and_then(|k| k.sent.get_mut(usize::from(index)))
                {
                    *sent = Some(now);
                }
            }
            (PacketKind::Media | PacketKind::Retransmit | PacketKind::Probe(_), _) => {}
        }
        if !self.reports {
            return;
        }
        let packet = SentPacket {
            seq,
            send_time: now,
            size: u32::try_from(out.body.len().saturating_add(HEADER_LEN)).unwrap_or(u32::MAX),
            kind: out.kind,
        };
        self.history.on_sent(packet);
        self.rate.on_sent(now, &packet);
    }

    /// 定时调用（约 25 ms）：网络控制器的定时任务（应用受限的预算、定期探测）。
    pub fn on_tick(&mut self, now: Time) {
        // 没有包发时预算桶照常累积
        self.alr.set_estimate(self.rate.rate_bps());
        self.alr.on_tick(now);
        if self.reports {
            self.rate.on_tick(now);
        }
        let started = *self.started.get_or_insert(now);
        if self.start_probe.is_some_and(|p| p.started.is_none())
            && now.since(started) >= START_PROBE_WAIT_MAX_US
        {
            // 兜底：第一个关键帧这么久还没确认，链路不可能比起步码率宽多少，不探了（“起步”有了时间上限）
            self.start_probe = None;
            self.rate.set_startup(
                false,
                "不做起步探测：第一个关键帧 2 秒还没确认，起步到时限了",
            );
        }
        if self
            .start_probe
            .and_then(|p| p.started)
            .is_some_and(|at| now.since(at) >= START_PROBE_MAX_US)
        {
            self.finish_start_probe(now);
        } else if self.start_probe.is_some_and(|p| p.started.is_some())
            && self.start_probe_settled(now)
        {
            // 这一组的结果该回来了、后面再没有新结果：按这一轮收到的判，不干等到 1 秒
            self.start_probe_decide(now);
        }
        // 静止探测到时间没有结果：超时（每轮探测必有结论）
        // （只下结论、写日志；这一组之后迟到的结果照旧认，行为和没有这个时限时完全一样，见不变量 I3）
        if self.probing.is_some()
            && !self.probing_late
            && now.since(self.probing_at) >= IDLE_PROBE_TIMEOUT_US
        {
            self.probing_late = true;
            self.idle_verdict(ProbeVerdict::TimedOut, "到时间没有结果");
        }
        self.tick_run_probe(now);
        // 运行中探测在进行时静止探测先等着（两组探测不同时发）
        if self.idle_probe
            && self.next_probe.is_some_and(|t| now >= t)
            && self.run_probe == RunProbe::Idle
        {
            self.stats.probes.idle_sent = self.stats.probes.idle_sent.saturating_add(1);
            self.probing_at = now;
            self.probing_late = false;
            self.probes = self.probes.wrapping_add(1);
            let id = IDLE_PROBE_ID_BASE | (self.probes & !IDLE_PROBE_ID_BASE);
            self.pending_probe = Some(ProbeCluster {
                id,
                target_bps: self
                    .rate
                    .rate_bps()
                    .saturating_mul(IDLE_PROBE_SCALE)
                    .clamp(self.cfg.min_bps, self.cfg.max_bps),
                packets: IDLE_PROBE_PACKETS,
            });
            self.probing = Some(id);
            self.next_probe = Some(now.plus(self.probe_gap));
            self.probe_gap = self.probe_gap.saturating_mul(2).min(IDLE_PROBE_MAX_GAP_US);
        }
    }

    /// 记一条静止探测的结论。
    fn idle_verdict(&mut self, verdict: ProbeVerdict, text: &str) {
        // 已经按超时下过结论的那一组：迟到的结果、之后的中止只写日志，不再算一次结论
        if std::mem::take(&mut self.probing_late) && verdict != ProbeVerdict::TimedOut {
            self.event(format!("静止探测（已按超时下过结论）之后：{text}"));
            return;
        }
        self.probing_late = verdict == ProbeVerdict::TimedOut;
        if let Some(n) = self.stats.probes.idle_verdicts.get_mut(verdict.index()) {
            *n = n.saturating_add(1);
        }
        self.event(format!("静止探测结论 {}：{text}", verdict.name()));
    }

    /// 对端的反馈里探测结果带组号（握手时双方都支持 [`crate::proto::Caps::PROBE_ID`]）：反馈按带组号的格式解，
    /// 并打开运行中探测。对端是老版本时不打开，反馈格式和探测行为都照旧。带到达报告的反馈格式不带组号。
    pub const fn set_probe_ids(&mut self, on: bool) {
        self.probe_ids = on;
    }

    /// 现在为什么不能发运行中探测（能发时为 `None`）。真拥塞时不探：探测是满速的流量，只会添堵。
    fn probe_blocked(&self, now: Time) -> Option<&'static str> {
        // （排队平不平静由估计器判：它知道哪些读数是断档的零星小包，不算数）
        let fb = self.last_obs.map(|o| o.feedback);
        if self.pending_probe.is_some() || self.probing.is_some() {
            Some("另一组探测还没完")
        } else if self.key_pending() {
            Some("关键帧还没确认")
        } else if self.in_flight_full(now) {
            Some("在途数据满了")
        } else if self.pacer_busy {
            Some("上一帧还没发完")
        } else if fb.is_some_and(|f| f.loss_permille >= FEC_FROM_LOSS_PERMILLE) {
            // 和起步探测一样：有丢包的链路上探测结果不准（ms-02560，8 Mbps、12% 丢包，按探测升码率后卡顿 0 → 3 次）
            Some("有丢包")
        } else {
            None
        }
    }

    /// 运行中探测的状态机走一步（每个定时周期）。
    fn tick_run_probe(&mut self, now: Time) {
        if self.run_probe == RunProbe::Idle
            && self.probe_ids
            && !self.reports
            && self.start_probe.is_none()
        {
            if let Some(ask) = self.rate.probe_wanted(now) {
                self.run_probe = RunProbe::Pending { ask, since: now };
            }
        }
        match self.run_probe {
            RunProbe::Idle => {}
            RunProbe::Pending { ask, since } => match self.probe_blocked(now) {
                None => {
                    self.probes = self.probes.wrapping_add(1);
                    let id = RUN_PROBE_ID_BASE | (self.probes & (RUN_PROBE_ID_BASE - 1));
                    let packets = crate::estimator::run_probe_packets(ask.target_bps);
                    self.pending_probe = Some(ProbeCluster {
                        id,
                        target_bps: ask.target_bps,
                        packets,
                    });
                    let span = u64::from(packets)
                        .saturating_mul(u64::from(crate::cc::probe::PROBE_PACKET_BYTES))
                        .saturating_mul(8_000_000)
                        / ask.target_bps.max(1);
                    self.run_probe = RunProbe::Sent {
                        ask,
                        id,
                        deadline: now.plus(self.rtt_us).plus(span).plus(RUN_PROBE_SLACK_US),
                        partial: None,
                    };
                    let p = &mut self.stats.probes;
                    p.run_sent = p.run_sent.saturating_add(1);
                    let rounds = match ask.kind {
                        crate::estimator::ProbeKind::Verify => &mut p.verify_rounds,
                        crate::estimator::ProbeKind::Periodic => &mut p.periodic_rounds,
                    };
                    *rounds = rounds.saturating_add(1);
                    self.event(format!(
                        "{}探测：按 {} kbps 发 {packets} 个包（码率 {} kbps）",
                        ask.kind.name(),
                        ask.target_bps / 1000,
                        self.rate.rate_bps() / 1000
                    ));
                }
                Some(why) if now.since(since) >= RUN_PROBE_WAIT_US => {
                    self.end_run_probe(now, ProbeEnd::Aborted(why));
                }
                Some(_) => {}
            },
            RunProbe::Sent {
                deadline, partial, ..
            } => {
                if now >= deadline {
                    // 没等到收完的结果：有收到一半时报的就按它判，没有就是超时
                    self.end_run_probe(now, partial.map_or(ProbeEnd::TimedOut, ProbeEnd::Result));
                }
            }
        }
    }

    /// 这一轮运行中探测结束：告诉估计器、记结论，回到空闲。
    fn end_run_probe(&mut self, now: Time, end: ProbeEnd) {
        if self.run_probe == RunProbe::Idle {
            return;
        }
        self.run_probe = RunProbe::Idle;
        let verdict = self.rate.on_probe_end(now, end);
        if let Some(n) = self.stats.probes.run_verdicts.get_mut(verdict.index()) {
            *n = n.saturating_add(1);
        }
    }

    /// 反馈里带组号的探测结果。
    fn on_probe_report(&mut self, now: Time, report: ProbeReport) {
        let RunProbe::Sent {
            ask,
            id,
            deadline,
            partial,
        } = self.run_probe
        else {
            return;
        };
        // 别的组的（上一轮收尾的、起步或静止探测的）不认
        if report.id != id {
            return;
        }
        let bps = u64::from(report.bytes_per_s).saturating_mul(8);
        if report.done && bps > 0 {
            self.end_run_probe(now, ProbeEnd::Result(bps));
        } else if report.done {
            // 收完了但测不出（只到了一两个包）：按之前收到一半时报的，没有就是超时
            self.end_run_probe(now, partial.map_or(ProbeEnd::TimedOut, ProbeEnd::Result));
        } else {
            self.run_probe = RunProbe::Sent {
                ask,
                id,
                deadline,
                partial: Some(bps),
            };
        }
    }

    /// 会话开头探测链路容量（对端支持 [`crate::proto::Caps::PROBE_TRAIN`]、估计器要时打开）。第一幅画面照常按
    /// 起步码率出；第一个关键帧送到、送达速度（扣掉往返）是码率 2 倍以上时，按码率的 4 倍匀速发一组填充包，
    /// 接收端测到的速率达到目标的七成就按结果的 2 倍接着探，直到探不上去或到上限，码率定为测得容量的九成；
    /// 最多 1 秒。探的时候不出新帧（第一幅画面已经出来了，见 [`Self::due`]）。
    /// 为什么（2026-11-26 用户实测，Mac 2880×1800 被控、30 Mbps Wi-Fi）：1 Mbps 起步、靠画面流量慢慢升，
    /// 开头几秒每帧预算只有几 KB，大屏的字糊好几秒。先探测再出第一幅画面在窄带上更差（微软轨迹卡顿 76 → 130 次：
    /// 探测包在瓶颈排几百毫秒、挤爆队列），所以按第一个关键帧的送达速度决定探不探。
    pub fn set_start_probe(&mut self, on: bool) {
        self.start_probe = (on && self.rate.wants_idle_probe()).then_some(StartProbe {
            started: None,
            id: START_PROBE_ID_BASE,
            target_bps: 0,
            sent_at: Time(0),
            prev_target_bps: 0,
            best_bps: 0,
            round_bps: 0,
        });
        if self.start_probe.is_some() {
            self.rate
                .set_startup(true, "会话开头，等第一个关键帧送到再决定探不探");
        }
    }

    /// 起步探测还没结束（在等第一个关键帧送到，或者在探）。
    #[must_use]
    pub const fn start_probing(&self) -> bool {
        self.start_probe.is_some()
    }

    /// 关键帧送到了（`bytes` 字节，从发出到确认 `took_us`）：第一个关键帧决定起步探测探不探。
    fn key_delivered(&mut self, now: Time, bytes: u64, took_us: u64) {
        if self.start_probe.is_none_or(|p| p.started.is_some()) {
            return;
        }
        let transfer_us = took_us.saturating_sub(self.rtt_us).max(1_000);
        let delivered_bps = bytes.saturating_mul(8_000_000) / transfer_us;
        let rate = self.rate.rate_bps();
        if delivered_bps.saturating_mul(1000) < rate.saturating_mul(START_PROBE_GATE_PERMILLE) {
            self.start_probe = None;
            self.rate
                .set_startup(false, "不做起步探测：链路不比起步码率宽多少");
            self.event(format!(
                "不做起步探测：第一个关键帧 {bytes} 字节送了 {} ms（扣掉往返 {} ms，约 {} kbps），链路不比起步码率 {} kbps 宽多少",
                took_us / 1000,
                self.rtt_us / 1000,
                delivered_bps / 1000,
                rate / 1000
            ));
            return;
        }
        if let Some(p) = self.start_probe.as_mut() {
            p.started = Some(now);
        }
        let target = rate
            .saturating_mul(START_PROBE_FIRST_SCALE)
            .clamp(self.cfg.min_bps, self.cfg.max_bps);
        self.start_cluster(now, target);
    }

    /// 发一组起步探测：按 `target_bps` 匀速发 [`START_PROBE_SPAN_US`]。
    fn start_cluster(&mut self, now: Time, target_bps: u64) {
        let Some(p) = self.start_probe.as_mut() else {
            return;
        };
        p.id = p.id.wrapping_add(1) | START_PROBE_ID_BASE;
        p.prev_target_bps = p.target_bps;
        p.target_bps = target_bps;
        p.sent_at = now;
        p.round_bps = 0;
        let bytes = target_bps.saturating_mul(START_PROBE_SPAN_US) / 8_000_000;
        let packets = u32::try_from(bytes / u64::from(crate::cc::probe::PROBE_PACKET_BYTES))
            .unwrap_or(u32::MAX)
            .max(START_PROBE_MIN_PACKETS);
        self.pending_probe = Some(ProbeCluster {
            id: p.id,
            target_bps,
            packets,
        });
        self.event(format!(
            "起步探测：按 {} kbps 发 {packets} 个包",
            target_bps / 1000
        ));
    }

    /// 起步探测有了一个结果（接收端测得的包串速率）。
    fn start_probe_result(&mut self, now: Time, result_bps: u64) {
        let Some(p) = self.start_probe.as_mut() else {
            return;
        };
        // 反馈里的包串速率也可能是帧的：比这一组的发送速率高的不算（匀速发的组测不出更高）
        let result_bps = result_bps.min(p.target_bps);
        p.best_bps = p.best_bps.max(result_bps);
        p.round_bps = p.round_bps.max(result_bps);
        // 反馈不带组号：上一组收尾的结果可能在这一组之后才到。比上一组的目标高的一定是这一组的，马上判；
        // 否则先记着，等这一组的结果有时间回来（见 `start_probe_settled`）再用这一轮收到的最高值判
        if result_bps <= p.prev_target_bps && !self.start_probe_settled(now) {
            self.event(format!(
                "起步探测：收到 {} kbps，可能是上一组的，等这一组的结果",
                result_bps / 1000
            ));
            return;
        }
        self.start_probe_decide(now);
    }

    /// 这一组的结果有时间回来了（往返 + 一组的时长 + 交给节奏器的等待 + 一次反馈间隔）。
    fn start_probe_settled(&self, now: Time) -> bool {
        let rtt = if self.rtt_us > 0 {
            self.rtt_us
        } else {
            100_000
        };
        self.start_probe.is_some_and(|p| {
            now.since(p.sent_at)
                >= rtt.saturating_add(START_PROBE_SPAN_US + START_PROBE_TICK_US + 20_000)
        })
    }

    /// 按这一轮收到的最高结果判：达到目标的七成就接着探，否则探完。
    fn start_probe_decide(&mut self, now: Time) {
        let Some(p) = self.start_probe else {
            return;
        };
        let further = p.round_bps.saturating_mul(1000)
            >= p.target_bps.saturating_mul(START_PROBE_FURTHER_PERMILLE);
        if further && p.target_bps < self.cfg.max_bps {
            let next = p
                .round_bps
                .saturating_mul(START_PROBE_STEP)
                .min(self.cfg.max_bps);
            self.start_cluster(now, next);
        } else {
            self.finish_start_probe(now);
        }
    }

    /// 起步探测结束：按测得的容量定码率，开始出画面。
    fn finish_start_probe(&mut self, now: Time) {
        let Some(p) = self.start_probe.take() else {
            return;
        };
        let took = p.started.map_or(0, |t| now.since(t) / 1000);
        self.rate.set_startup(
            false,
            if p.best_bps == 0 {
                "起步探测没测到结果"
            } else {
                "起步探测有结论了"
            },
        );
        if p.best_bps == 0 {
            self.event(format!(
                "起步探测 {took} ms 没测到结果，按起步码率 {} kbps 出画面",
                self.rate.rate_bps() / 1000
            ));
            return;
        }
        let from = self.rate.rate_bps();
        self.rate.on_start_probe(now, p.best_bps);
        self.event(format!(
            "起步探测 {took} ms：链路容量至少 {} kbps，码率 {} → {} kbps",
            p.best_bps / 1000,
            from / 1000,
            self.rate.rate_bps() / 1000
        ));
    }

    /// 画面静止时定期探测链路（对端支持 [`crate::proto::Caps::PROBE_TRAIN`]、估计器要时打开）：停下 2 秒后
    /// 发一组几个填充包，之后间隔翻倍（4、8、16 秒），最长 30 秒一次。接收端测包串速率随反馈带回，链路明显变差时
    /// 按它降码率。2 秒而不是 1 秒：操作之间停 1 秒很常见，那时探测只有扰动（微软轨迹 ms-05729 最长卡顿 304 → 496 ms，
    /// 一次也没触发降码率）；2 秒时 8 条轨迹和不探测逐项相同，e2e drop-while-idle 最长等待 820 → 651 ms。
    /// 为什么：经典算法只靠视频帧测容量，静止时链路变差了不知道，一动就按老码率编大帧（e2e drop-while-idle：
    /// 20 → 2 Mbps 时切窗口的帧 222 KB、等了 1.2 秒）。WebRTC 应用受限时每 5 秒探一次；这里开头密、之后退避，
    /// 一小时的静止约 1 MB（中继流量是成本）。
    pub fn set_idle_probe(&mut self, on: bool) {
        self.idle_probe = on && self.rate.wants_idle_probe();
    }

    /// 取走要发的探测组（交给节奏器）。
    pub fn take_probes(&mut self) -> Vec<ProbeCluster> {
        let mut out: Vec<ProbeCluster> = self.pending_probe.take().into_iter().collect();
        if self.reports {
            out.extend(self.rate.take_probes());
        }
        out
    }

    /// 节奏器的发送速率：网络控制器给的（目标的 1.1 倍）；老的估计器按老规则（`legacy_pacing` 是
    /// `PEERDESK_PACE` 开关，见 [`crate::pacer::rate_for`]）。
    #[must_use]
    pub fn pacing_bps(&self, legacy_pacing: bool) -> u64 {
        self.rate
            .pacing_bps()
            .filter(|_| self.reports)
            .unwrap_or_else(|| crate::pacer::rate_for(legacy_pacing, self.rate.rate_bps()))
    }

    /// 网络控制器的一行状态（诊断日志用；老的估计器没有）。
    #[must_use]
    pub fn controller_line(&self) -> Option<String> {
        self.rate.state_line().filter(|_| self.reports)
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
    pub fn fps(&self) -> u32 {
        self.governor.fps()
    }

    /// 按当前帧率，到时间编下一帧了吗。
    #[must_use]
    pub fn due(&self, now: Time) -> bool {
        // 起步探测在探的时候不出新帧：反馈里的包串速率不分探测组和帧，帧混进来会把结果顶高
        // （2026-11-26 独立审查）。第一幅画面已经出来了，探测最多 1 秒，一般两三百毫秒。
        self.start_probe.is_none_or(|p| p.started.is_none()) && self.governor.due(now)
    }

    /// 编码线程报告这一帧编了多久（微秒）、是不是硬件编码（流水线的，CPU 过载门限放宽），
    /// 在 [`Self::finish_frame`] 之前调用。只算在算的时间：等显卡读回、等锁的时间调用方要先扣掉
    /// （[`crate::video::EncodeTiming::wait_us`]），那不是 CPU 跟不上。
    pub const fn set_encode_time(&mut self, us: u64, hardware: bool) {
        self.encode_us = us;
        self.encode_hw = hardware;
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
        recent > by_rate.max(floor) || self.key_pending()
    }

    /// 这一帧现在能不能编：`wanted` 是有东西要编（画面变了，或 [`Self::wants_frame`]），`pacer_busy` 是节奏器里
    /// 上一帧还没发完。到了该编的时间、却被在途上限或节奏器拦下时，记一次“网络受限”（见 [`SendLimit`]）：
    /// 发送端有东西要发、是网络不让发，这时不能算应用受限。会话循环每一拍调用一次。
    pub fn frame_gate(&mut self, now: Time, wanted: bool, pacer_busy: bool) -> bool {
        self.pacer_busy = pacer_busy;
        if !wanted || !self.due(now) {
            return false;
        }
        let full = self.in_flight_full(now);
        if full || pacer_busy {
            self.net_event(
                now,
                if self.key_pending() {
                    NetCause::KeyPending
                } else {
                    NetCause::FrameBlocked
                },
            );
        }
        !full && !pacer_busy
    }

    /// 记一次网络受限的原因，马上告诉估计器（应用受限进慢出快，不等下一份反馈）。
    fn net_event(&mut self, now: Time, cause: NetCause) {
        self.net_limited = Some((now, cause));
        // 有东西要发、被网络挡住的那几种原因单独记时间（见 `observe` 里的 `backlog`）
        if matches!(
            cause,
            NetCause::FrameBlocked | NetCause::KeyPending | NetCause::ResendCapped
        ) {
            self.blocked_at = Some(now);
        }
        self.rate.on_net_limited(cause);
        // 探测期间出现网络受限的原因：中止，结果不用
        self.end_run_probe(now, ProbeEnd::Aborted(cause.name()));
    }

    /// 发送量现在是被谁限制的（每份反馈判一次，交给估计器）。
    fn send_limit(&mut self, now: Time) -> SendLimit {
        // 持续性的原因：只要还成立，就一直算网络受限
        if self.key_pending() {
            self.net_event(now, NetCause::KeyPending);
        } else if self.governor.fit_limited() {
            self.net_event(now, NetCause::FpsReduced);
        } else if self.scale.permille() < crate::scale::FULL_PERMILLE {
            self.net_event(now, NetCause::ScaleReduced);
        }
        let (at, cause) = *self.net_limited.get_or_insert((now, NetCause::Start));
        if now.since(at) < APP_LIMITED_AFTER_US {
            SendLimit::Network(cause)
        } else if self.alr.in_alr() {
            SendLimit::App
        } else {
            SendLimit::Network(NetCause::UsingRate)
        }
    }

    /// 编码器把这一帧丢了（硬件码控：预算放不下一帧）。这是网络受限——有画面要发、是码率不让发——
    /// 不能等 500 ms 没发东西就判成应用受限：应用受限不升码率，画面会一直停着
    /// （2026-11-27 用户日志，Mac：码率 344 kbps、编码器每秒丢 30–50 帧，停了 8 秒，`P-fps-bitrate.md` M2）。
    pub fn encoder_dropped(&mut self, now: Time) {
        self.net_event(now, NetCause::EncoderDropped);
    }

    /// 采集到的画面变了（编码器在别的线程时由会话循环调用；[`Self::on_frame`] 自己会调）。
    /// 用来判断画面是不是停下了（见 [`crate::scale`]）。
    pub fn screen_changed(&mut self, now: Time) {
        self.scale.on_change(now);
    }

    /// 原始画面尺寸为 `native` 时现在该按多大编码（采集端能在显卡上缩放时按它采，见 [`crate::scale::encode_size`]）。
    #[must_use]
    pub fn encode_size(&self, native: (u16, u16)) -> (u16, u16) {
        crate::scale::encode_size(native, self.scale.permille(), self.view)
    }

    /// 控制端报告了显示画面的区域（像素）；`(0, 0)` 或 `None` 表示不限（例如“原始大小”）。
    pub fn set_view(&mut self, view: Option<(u16, u16)>) {
        let view = view.filter(|(w, h)| *w > 0 && *h > 0);
        // 变了：画面不动也要按新尺寸编一帧（否则停着的画面一直是旧尺寸）
        if view != self.view {
            self.scale_up = true;
            self.event(format!(
                "控制端显示区域 {:?} → {view:?}（像素，None 表示不限）",
                self.view
            ));
        }
        self.view = view;
    }

    /// 最近的关键帧还没确认。
    /// 节奏器里的这个包还要不要发：它的帧已经有定论（送到了，或接收端已经放弃）就不发了，
    /// 免得节奏器一直在发没用的包、把新画面挡在后面（会话在每份反馈之后清一次节奏器）。
    #[must_use]
    pub fn wanted(&self, out: &Outgoing) -> bool {
        out.fragment
            .is_none_or(|(frame, _)| self.unacked.iter().any(|(id, _, _)| *id == frame))
    }

    /// 最近的关键帧还没确认（之后的帧都依赖它）。
    #[must_use]
    pub fn key_pending(&self) -> bool {
        self.unacked.iter().any(|(id, _, _)| *id == self.last_key)
    }

    /// 画面没变时还要不要按时交来（编码器在补清晰或要恢复）。
    #[must_use]
    pub fn wants_frame(&self) -> bool {
        let encoder = self
            .encoder
            .as_ref()
            .map_or(self.refine, |e| e.wants_refine());
        encoder || !self.notes.0.is_empty() || self.scale_up
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

    /// 设置纠错方式（握手后按双方能力：都支持 RS 时用 RS）。
    pub fn set_fec(&mut self, fec: Fec) {
        self.fec = fec;
    }

    /// 现在用的画像名（日志用）。
    #[must_use]
    pub fn profile_name(&self) -> &'static str {
        self.rate.profile().name
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
        self.frame_with(now, changed, rtt_us, |encoder, job| {
            encoder.encode(job.frame_id, screen, changed, job.budget)
        })
    }

    /// 和 [`Self::on_frame`] 一样，交来的是一帧 [`Frame`]（CPU 画面、显卡画面；全模型仿真以后在这里交内容描述）。
    /// 编码器在发送端里时用（仿真）；产品的编码器在自己的线程上，走 [`Self::start_frame`]、[`encode_job`]、
    /// [`Self::finish_frame`]。
    ///
    /// # Errors
    /// 同 [`Self::on_frame`]。
    pub fn on_frame_any(
        &mut self,
        now: Time,
        frame: &Frame,
        changed: bool,
        rtt_us: u64,
    ) -> Result<Vec<Outgoing>, Error> {
        self.frame_with(now, changed, rtt_us, |encoder, job| {
            encoder.encode_frame(job.frame_id, frame, changed, job.budget)
        })
    }

    /// 和 [`Self::on_frame`] 一样，交来的是内容描述（没有像素，全模型仿真用，见 [`ContentDesc`]）。
    ///
    /// # Errors
    /// 编码器不收内容描述；其余同 [`Self::on_frame`]。
    pub fn on_described(
        &mut self,
        now: Time,
        content: &ContentDesc,
        changed: bool,
        rtt_us: u64,
    ) -> Result<Vec<Outgoing>, Error> {
        self.frame_with(now, changed, rtt_us, |encoder, job| {
            encoder.encode_described(job.frame_id, content, changed, job.budget)
        })
    }

    /// 一帧的全过程：开始（帧号、预算）→ `encode` 编这一帧 → 切片、纠错。画面是什么形式由 `encode` 决定。
    fn frame_with(
        &mut self,
        now: Time,
        changed: bool,
        rtt_us: u64,
        encode: impl FnOnce(&mut dyn VideoEncoder, &FrameJob) -> Result<Option<EncodedFrame>, Error>,
    ) -> Result<Vec<Outgoing>, Error> {
        if changed {
            self.screen_changed(now);
        }
        let job = self.start_frame(now, rtt_us);
        let mut encoder = self.encoder.take().ok_or(Error::InvalidArgument(
            "编码器不在这里：用 start_frame / finish_frame",
        ))?;
        prepare(encoder.as_mut(), &job);
        let result =
            encode(encoder.as_mut(), &job).map(|encoded| (encoded, encoder.wants_refine()));
        self.encoder = Some(encoder);
        let (encoded, refine) = result?;
        self.finish_frame(now, &job, encoded, refine)
    }

    /// 开始一帧：判定超时丢失，分配帧号和字节预算，带上要交给编码器的通知。
    pub fn start_frame(&mut self, now: Time, rtt_us: u64) -> FrameJob {
        self.rtt_us = rtt_us;
        self.governor.on_start(now);
        self.expire_unacked(now);
        // 只给还没有定论的帧留着重传（接收端可能还在收）；已确认、已判丢的不再重传，由接收端的
        // 重传请求和“不再接受的帧号”决定什么时候停。不按“编出来多久”判断：节奏器要把一个大关键帧
        // 分一秒多发完（1 Mbps 传 120 KB），按 250 ms 截止的话关键帧尾部丢的包永远补不上，
        // 接收端等不齐只能作废，发送端再发一个同样大的关键帧（2026-11 微软轨迹 ms-05729 实测如此）。
        // WebRTC 的重传缓存也是按“实际发出之后”保留（`RtpPacketHistory`：至少 1 秒、3 个往返）。
        let oldest = self.unacked.front().map(|(id, _, _)| *id);
        while self
            .recent
            .front()
            .is_some_and(|k| oldest.is_none_or(|o| k.frame < o))
        {
            if let Some(k) = self.recent.pop_front() {
                self.resent.retain(|(frame, _), _| *frame != k.frame);
            }
        }
        let (lost, feedback) = std::mem::take(&mut self.notes);
        let key_pending = self.key_pending();
        let before = self.scale.permille();
        let scale_permille = self.scale.decide(now, self.rate.rate_bps(), key_pending);
        if scale_permille != before {
            self.event(format!(
                "编码分辨率 {before}‰ → {scale_permille}‰：原尺寸关键帧按码率 {} kbps 估计送得完",
                self.rate.rate_bps() / 1000
            ));
        }
        self.scale_up = false;
        FrameJob {
            frame_id: self.last_frame.wrapping_add(1).max(1),
            budget: self.frame_budget(),
            lost,
            feedback,
            scale_permille,
            view: self.view,
            rate_stale: self.rate_stale(now),
        }
    }

    /// 码率估计过期了：证实过、但已经超过策略的 `rate_stale_after_us` 没再证实。证实 = 接收端测到包串速率，
    /// 或者送达速率到了估计的一半（链路真的在承载这个速率）。会话开头还没证实过的不算（起步码率怎么定是
    /// 启动探测的事）。
    fn rate_stale(&self, now: Time) -> bool {
        self.rate_seen
            .is_some_and(|t| now.since(t) > self.cfg.policy.rate_stale_after_us)
    }

    /// 迟迟等不到反馈的帧按丢失处理。会话循环每一轮都要调用（不能只在编下一帧时检查）：
    /// 关键帧没确认时 [`Self::in_flight_full`] 不让编新帧，如果只在编帧时判丢，
    /// 关键帧丢了一个包而接收端又发现不了（例如丢的是最后一片），就永远等下去，画面一直是黑的。
    ///
    /// 时限里加上这一帧按接收端实际收的速度传完要多久：
    /// 窄带宽上一个大关键帧要传一两秒，按固定时限判丢会重发新的关键帧，越堵越多，画面始终出不来。
    pub fn expire_unacked(&mut self, now: Time) {
        let rtt_us = self.rtt_us;
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
        // 发了一帧：静止探测从头排（停下 2 秒后第一次），之前那一组的结果不再认（包串速率可能是这一帧的）
        self.next_probe = Some(now.plus(IDLE_PROBE_FIRST_US));
        self.probe_gap = IDLE_PROBE_FIRST_US.saturating_mul(2);
        if self.probing.take().is_some() {
            self.idle_verdict(ProbeVerdict::Aborted, "又开始发帧了，结果不用");
        }
        self.governor.on_frame(
            now,
            encode_us,
            self.encode_hw,
            encoded.payload.len(),
            job.budget,
        );
        // 这一帧用掉了预算的八成以上：编码器是被码率卡着的（网络受限）
        if encoded.payload.len().saturating_mul(1000)
            >= job.budget.saturating_mul(NEAR_BUDGET_PERMILLE)
        {
            self.net_event(now, NetCause::FrameNearBudget);
        }
        if let Some(change) = self.governor.take_change() {
            self.event(change);
        }
        // 适配的最后一步：帧率已经到底、一直装不进预算 → 降分辨率（画面停下后升回、补清晰）
        if self.governor.overflowing_for(now) >= OVERFLOW_DOWN_US && self.scale.down(now) {
            self.governor.reset_overflow();
            self.event(format!(
                "编码分辨率 1000‰ → {}‰：帧率已经降到最低（{} 帧），画面还是装不进预算 {} 秒（这帧 {} 字节，预算 {}）；\
                 降分辨率是最后手段，画面停下后升回原尺寸",
                self.scale.permille(),
                self.governor.fps(),
                OVERFLOW_DOWN_US / 1_000_000,
                encoded.payload.len(),
                job.budget
            ));
        }
        if encoded.key {
            self.key_sent = Some((job.frame_id, now));
            self.scale.on_keyframe(
                job.scale_permille,
                u64::try_from(encoded.payload.len()).unwrap_or(u64::MAX),
            );
        }
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
            // 按较长的 RS 校验分片头留出空间（异或的短 2 字节，不另算）
            .saturating_sub(RS_PARITY_PREFIX);
        let mut packets = fragment(frame_id, ref_frame, &encoded.payload, max)?;
        let data_count = packets.len();
        let parity = match self.fec {
            Fec::Xor => parity_for(&packets, self.fec_permille()),
            Fec::Rs => rs_parity_for(&packets, self.fec_permille()),
        };
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
                    kind: PacketKind::Media,
                    fragment: Some((p.header.frame_id, p.header.frag_index)),
                }
            })
            .collect();
        self.recent.push_back(Kept {
            frame: frame_id,
            packets: out.get(..data_count).unwrap_or_default().to_vec(),
            sent: vec![None; data_count],
        });
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
        let (feedback, ext, nacks, report) = if self.reports {
            let (f, n, r) = decode_feedback_v2(buf)?;
            (f, Extension::default(), n, r)
        } else if self.probe_ids {
            let ((f, e, n), probe) = decode_feedback_ids(buf)?;
            if let Some(report) = probe {
                self.on_probe_report(now, report);
            }
            (f, e, n, None)
        } else {
            let (f, e, n) = decode_feedback(buf)?;
            (f, e, n, None)
        };
        if let Some(report) = report {
            let results = self.history.resolve(&report);
            self.rate.on_report(now, &results, self.rtt_us);
        }
        self.observe(now, &feedback, ext);
        // 整帧重传（接收端一个分片都没收到这一帧）：展开成这一帧的全部数据分片，按同样的规则补发
        let nacks: Vec<(u32, u16)> = nacks
            .iter()
            .flat_map(|&(frame, frag)| {
                let count = if frag == WHOLE_FRAME {
                    self.recent
                        .iter()
                        .find(|k| k.frame == frame)
                        .map_or(0, |k| k.packets.len())
                } else {
                    0
                };
                let all = (0..count).map(move |i| (frame, u16::try_from(i).unwrap_or(u16::MAX)));
                let one = (frag != WHOLE_FRAME).then_some((frame, frag));
                one.into_iter().chain(all)
            })
            .collect();
        // 新反馈格式里的进度标记（不是重传请求）：这一帧到的分片比上次多了，说明它还在路上、在进展
        type Marks = Vec<(u32, u16)>;
        let (marks, nacks): (Marks, Marks) = nacks
            .into_iter()
            .partition(|(_, frag)| self.probe_ids && *frag != WHOLE_FRAME && frag & PROGRESS != 0);
        let mut alive: Vec<u32> = Vec::new();
        for (frame, mark) in marks {
            let arrived = mark & (PROGRESS - 1);
            if self.progress.get(&frame).is_none_or(|seen| arrived > *seen) {
                self.progress.insert(frame, arrived);
                alive.push(frame);
            }
        }
        self.progress
            .retain(|frame, _| self.unacked.iter().any(|(id, _, _)| id == frame));
        // 在进展的帧，和接收端还在要重传的帧：不能按超时判丢（判丢就要重发关键帧），也继续算在途（拦着新帧）。
        for (id, at, _) in &mut self.unacked {
            if alive.contains(id) || nacks.iter().any(|(frame, _)| frame == id) {
                *at = now;
            }
        }
        let mut out = Vec::new();
        // 一轮重传不超过约两个反馈间隔能送完的量：一次全部补发会再次挤爆瓶颈的缓冲区，
        // 又丢一大半（窄带宽上大关键帧就是这样反复丢、半天收不齐）。没补到的下一轮还会再要。
        let allowance = self.rate.rate_bps().saturating_mul(RESEND_WINDOW_US) / 8_000_000;
        let mut resent_bytes = 0u64;
        self.stats.nack_requests = self
            .stats
            .nack_requests
            .saturating_add(u32::try_from(nacks.len()).unwrap_or(u32::MAX));
        for (i, (frame, frag)) in nacks.iter().copied().enumerate() {
            if resent_bytes >= allowance.max(1) {
                // 有要补的分片这一轮发不出去：网络受限。只是“接收端在要重传”不算：要补的马上就发了，发送端没被拦着；
                // 而且现在的重传请求在一次延迟尖峰里就会发（按第一片到达计时），算上的话尖峰一来就退出应用受限、
                // 同一份反馈的高读数接着把码率打到底——实测 wifi-light 2 秒后最低 175 kbps（不算是 23.9 Mbps）
                self.net_event(now, NetCause::ResendCapped);
                self.stats.resend_capped = self
                    .stats
                    .resend_capped
                    .saturating_add(u32::try_from(nacks.len() - i).unwrap_or(u32::MAX));
                break;
            }
            // 这个分片上次实际发出的时间（原包或重传）：`None` 是还在节奏器里排队（原包没发、
            // 或上一份重传没发），不再排一份。发出之后，要等它排完队、到达、接收端再报告，才算真的
            // 又丢了：往返时间加上当前排队延迟（从实际发出算起）。
            let kept = self.recent.iter().find(|k| k.frame == frame);
            let last_sent = match self.resent.get(&(frame, frag)) {
                Some(state) => *state,
                None => kept.and_then(|k| k.sent.get(usize::from(frag)).copied().flatten()),
            };
            let packet = kept.and_then(|k| k.packets.get(usize::from(frag)));
            let again = match last_sent {
                None => {
                    self.stats.resend_unsent = self.stats.resend_unsent.saturating_add(1);
                    false
                }
                Some(at) if now.since(at) < self.resend_after_us() => {
                    self.stats.resend_skipped = self.stats.resend_skipped.saturating_add(1);
                    false
                }
                Some(_) => true,
            };
            if let Some(packet) = packet.filter(|_| again) {
                resent_bytes = resent_bytes
                    .saturating_add(u64::try_from(packet.body.len()).unwrap_or(u64::MAX));
                out.push(Outgoing {
                    kind: PacketKind::Retransmit,
                    ..packet.clone()
                });
                self.resent.insert((frame, frag), None);
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

    /// 记一条决定（最多攒 64 条，会话每秒取走）。
    fn event(&mut self, text: String) {
        if self.events.len() < 64 {
            self.events.push(text);
        }
    }

    /// 同一个分片两次重传至少隔多久：往返时间加上最近报告的排队延迟。
    fn resend_after_us(&self) -> u64 {
        let queue = self
            .last_obs
            .map_or(0, |o| u64::from(o.feedback.queue_delay_us));
        self.rtt_us.saturating_add(queue).max(1)
    }

    /// 还没确认的字节数。
    #[must_use]
    pub fn in_flight(&self) -> u64 {
        self.unacked.iter().map(|(_, _, bytes)| *bytes).sum()
    }

    /// 取走这段时间里各个模块的决定和原因（码率、帧率、编码分辨率、网络画像），诊断日志用。
    pub fn take_events(&mut self) -> Vec<String> {
        let mut out = std::mem::take(&mut self.events);
        out.extend(self.rate.take_notes());
        out
    }

    /// 带宽估计器的名字。
    #[must_use]
    pub fn estimator(&self) -> &'static str {
        self.rate.name()
    }

    /// 码率控制现在处于哪个状态（量尺统计各状态的时间占比用）。
    #[must_use]
    pub fn rate_state(&self) -> crate::estimator::RateState {
        self.rate.state()
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
    /// 纠错冗余比例（千分比）：画像给的值和按实测丢包率算的取大。
    ///
    /// 实测（2026-11-23，用户日志）：往返短时选“稳定”画像，纠错是 0，丢一个包就要等一个往返重传，
    /// 丢多了就重发关键帧；仿真 3% 丢包时冻结 8 次、关键帧 15 个。按丢包率的 3 倍加纠错
    /// （丢包低于 1% 时不加，最多 33%），大部分丢包当场补回，不用等重传。
    #[must_use]
    pub fn fec_permille(&self) -> u16 {
        let loss = self.last_obs.map_or(0, |o| o.feedback.loss_permille);
        let adaptive = if loss < FEC_FROM_LOSS_PERMILLE {
            0
        } else {
            loss.saturating_mul(3).clamp(100, FEC_MAX_PERMILLE)
        };
        self.rate.profile().fec_permille.max(adaptive)
    }

    fn frame_budget(&self) -> usize {
        // 按丢包额外加的纠错分片也占带宽：编码预算按比例让出来
        // （画像本身带的纠错已经算在它的预算折扣 safety_permille 里，不重复扣）
        let fec = u64::from(
            self.fec_permille()
                .saturating_sub(self.rate.profile().fec_permille),
        );
        let per_frame = self.rate.rate_bps() / 8 / u64::from(self.governor.fps().max(1));
        let per_frame = per_frame.saturating_mul(1000) / (1000 + fec);
        let budget =
            per_frame.saturating_mul(u64::from(self.rate.profile().safety_permille)) / 1000;
        let queue = u64::from(self.last_obs.map_or(0, |o| o.feedback.queue_delay_us));
        let left = QUEUE_TARGET_US
            .saturating_sub(queue)
            .max(QUEUE_TARGET_US / 4);
        usize::try_from(budget.saturating_mul(left) / QUEUE_TARGET_US).unwrap_or(usize::MAX)
    }

    fn observe(&mut self, now: Time, feedback: &Feedback, ext: Extension) {
        let limit = self.send_limit(now);
        // 这一刻发送端有没有东西等着发、被网络挡着（估计器据此判断送达速率是不是链路的能力）
        let blocked_lately = self
            .blocked_at
            .is_some_and(|at| self.observed_at.is_none_or(|last| at >= last));
        let backlog =
            blocked_lately || self.key_pending() || self.pacer_busy || self.in_flight_full(now);
        self.observed_at = Some(now);
        let obs = Observation {
            feedback: *feedback,
            ext,
            rtt_us: self.rtt_us,
            in_flight: self.in_flight(),
            limit,
            backlog,
        };
        // 还没开始起步探测就看到明显丢包：不探（有丢包的链路上探测结果不准，码率一下提上去之后在丢包里来回降：
        // 微软轨迹 ms-02560，8 Mbps、12% 随机丢包，卡顿 0 → 3 次）
        if self.start_probe.is_some_and(|p| p.started.is_none())
            && feedback.loss_permille >= FEC_FROM_LOSS_PERMILLE
        {
            self.start_probe = None;
            self.rate.set_startup(false, "不做起步探测：有丢包");
            self.event(format!(
                "不做起步探测：有丢包（{}‰），按画面流量慢慢升码率",
                feedback.loss_permille
            ));
        }
        // 起步探测有了结果（画面照常在发：反馈里的包串速率可能是帧的，见 start_probe_result）
        if self.start_probe.is_some_and(|p| p.started.is_some()) && ext.train_bytes_per_s > 0 {
            self.start_probe_result(now, u64::from(ext.train_bytes_per_s).saturating_mul(8));
        }
        // 静止时的探测有了结果（探测之后没发过帧：这个包串速率就是探测组的）
        if self
            .probing
            .take_if(|_| ext.train_bytes_per_s > 0)
            .is_some()
        {
            let capacity = u64::from(ext.train_bytes_per_s).saturating_mul(8);
            let from = self.rate.rate_bps();
            self.rate.on_idle_probe(now, capacity);
            if self.rate.rate_bps() != from {
                self.event(format!(
                    "码率 {} → {} kbps：画面静止时探测测得链路容量 {} kbps",
                    from / 1000,
                    self.rate.rate_bps() / 1000,
                    capacity / 1000
                ));
                self.idle_verdict(ProbeVerdict::Denied, "链路变差了，码率按测得的容量降");
            } else {
                self.idle_verdict(
                    ProbeVerdict::Confirmed,
                    &format!("测得 {} kbps，链路没有明显变差，码率不变", capacity / 1000),
                );
            }
        }
        self.rate.on_observation(now, &obs);
        // 探测期间估计器因为过载降了码率（离开了要探的那个状态）：中止，结果不用（I1）
        let state = self.rate.state();
        let moved = match self.run_probe {
            RunProbe::Idle => false,
            RunProbe::Pending { ask, .. } | RunProbe::Sent { ask, .. } => match ask.kind {
                crate::estimator::ProbeKind::Verify => state != crate::estimator::RateState::Verify,
                crate::estimator::ProbeKind::Periodic => {
                    state != crate::estimator::RateState::AppLimited
                }
            },
        };
        if moved {
            self.end_run_probe(now, ProbeEnd::Aborted("出现过载，降了码率"));
        }
        let delivered_bps = u64::from(feedback.recv_bytes_per_s).saturating_mul(8);
        if ext.train_bytes_per_s > 0 || delivered_bps.saturating_mul(2) >= self.rate.rate_bps() {
            self.rate_seen = Some(now);
        }
        self.last_obs = Some(obs);
        self.acknowledge(now, feedback);
        self.adapt_profile(feedback.loss_permille);
        // 画面停下了：半分辨率时在后台送原尺寸的关键帧（补清晰）
        let key_pending = self.key_pending();
        if self.scale.on_idle(now, self.rate.rate_bps(), key_pending) {
            self.scale_up = true;
            self.event("编码分辨率升回原尺寸：画面停下了，后台送原尺寸关键帧".to_owned());
        }
    }

    /// 会话开始时按往返时间选的画像不知道是不是 Wi-Fi：往返短就选“稳定”（不带纠错、把 2% 丢包当拥塞）。
    /// 连续两次反馈都有 1% 以上的丢包，说明是会随机丢包的链路（多半是 Wi-Fi），改用“无线”画像。
    /// 只往宽容的方向换一次，不来回切。
    fn adapt_profile(&mut self, loss_permille: u16) {
        if self.rate.profile().name != Profile::STABLE.name {
            return;
        }
        self.lossy_feedbacks = if loss_permille >= FEC_FROM_LOSS_PERMILLE {
            self.lossy_feedbacks.saturating_add(1)
        } else {
            0
        };
        if self.lossy_feedbacks >= 2 {
            self.rate.set_profile(Profile::WIRELESS);
            self.stats.profile_switches = self.stats.profile_switches.saturating_add(1);
            self.event(format!(
                "网络画像 稳定 → 无线：连续两次反馈丢包 ≥ {}‰（这次 {loss_permille}‰），加纠错、随机丢包不再降码率",
                FEC_FROM_LOSS_PERMILLE
            ));
        }
    }

    /// 接收端的反馈（没有扩展测量时用）。
    pub fn on_feedback(&mut self, now: Time, feedback: &Feedback) {
        self.observe(now, feedback, Extension::default());
    }

    fn acknowledge(&mut self, now: Time, feedback: &Feedback) {
        match self.encoder.as_mut() {
            Some(encoder) => encoder.on_feedback(feedback.floor, feedback.delivered),
            None => self.notes.1 = Some((feedback.floor, feedback.delivered)),
        }
        while let Some((id, _, bytes)) = self.unacked.front().copied() {
            if id > feedback.floor {
                break;
            }
            self.unacked.pop_front();
            let age = feedback.floor.wrapping_sub(id);
            let delivered = age < 32 && feedback.delivered >> age & 1 == 1;
            if !delivered {
                self.lost(id);
            }
            // 关键帧送到了：从发出到确认用了多久，是这条链路真实速度的直接测量（见 crate::scale）
            if let Some((_, sent)) = self.key_sent.filter(|(key, _)| *key == id) {
                if delivered {
                    self.scale.on_key_delivered(now.since(sent));
                    self.key_delivered(now, bytes, now.since(sent));
                }
                self.key_sent = None;
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
    /// 收齐的帧数（不管能不能解）。
    pub assembled: u32,
    /// 收齐一帧用的时间（第一个分片到最后一个，微秒）之和与最大值：大帧突发、丢包重传时变长。
    pub assembly_us_sum: u64,
    /// 收齐一帧用的最长时间（微秒）；诊断日志取走后清零（见 [`MediaReceiver::take_assembly_max`]）。
    pub assembly_us_max: u64,
    /// 发出的重传请求（分片数）。
    pub nacks_sent: u32,
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

    /// 现在用的解码器（详情面板用）。
    #[must_use]
    pub fn backend(&self) -> String {
        self.decoder.backend()
    }

    /// 解一帧。返回要显示的画面（`None` 表示这一帧没有新画面）；失败或因为等关键帧而没解时返回错误。
    pub fn decode(&mut self, job: &DecodeJob) -> Result<Option<Picture>, Error> {
        let independent = job.header.key || job.header.codec == Codec::Tile;
        if self.broken && !independent {
            return Err(Error::Inconsistent("之前的帧解码失败，等关键帧"));
        }
        match self.decoder.decode_picture(&job.header, &job.data) {
            Ok(picture) => {
                self.broken = false;
                Ok(picture)
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
    /// 发到达报告（双方都支持 [`crate::proto::Caps::TRANSPORT_CC`]）。
    reports: bool,
    arrivals: ArrivalRecorder,
    /// 反馈里的探测结果带组号（见 [`Self::set_probe_ids`]）。
    probe_ids: bool,
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
            reports: false,
            arrivals: ArrivalRecorder::new(),
            probe_ids: false,
        }
    }

    /// 打开到达报告（握手时双方都支持 [`crate::proto::Caps::TRANSPORT_CC`]）。
    pub fn set_reports(&mut self, on: bool) {
        self.reports = on;
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

    /// 设置纠错方式（和发送端一致，按握手协商）。
    pub fn set_fec(&mut self, fec: Fec) {
        self.reassembler.set_fec(fec);
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
    /// 收齐并解出帧时返回最新的一帧（一个数据报可能让几帧一起能解：参考帧补齐了，等它的帧跟着解）。
    /// 分片本身不合法时返回错误（调用方计为坏数据报）。
    pub fn on_datagram(
        &mut self,
        now: Time,
        send_time: Time,
        seq: u32,
        flags: u8,
        body: &[u8],
    ) -> Result<Option<Shown<'_>>, Error> {
        let jobs = self.assemble(now, send_time, seq, flags, body)?;
        if jobs.is_empty() {
            return Ok(None);
        }
        let decoder = self.decoder.as_mut().ok_or(Error::InvalidArgument(
            "解码器不在这里：用 on_datagram_deferred",
        ))?;
        let mut shown = None;
        let mut failed = false;
        for job in jobs {
            if failed {
                // 前一帧解失败了，接在它后面的帧也解不了
                self.stats.skipped = self.stats.skipped.saturating_add(1);
                self.log.advance(job.frame_id);
                continue;
            }
            match decoder.decode(&job.header, &job.data) {
                Ok(picture) => {
                    self.decoded = Some(job.frame_id);
                    self.log.delivered(job.frame_id);
                    if picture.is_some() {
                        self.stats.decoded = self.stats.decoded.saturating_add(1);
                        shown = Some((job.frame_id, job.header.key));
                    }
                }
                Err(_) => {
                    self.decoded = None;
                    self.stats.bad = self.stats.bad.saturating_add(1);
                    self.log.advance(job.frame_id);
                    failed = true;
                    shown = None;
                }
            }
        }
        let screen = decoder.screen();
        Ok(shown.zip(screen).map(|((frame_id, key), screen)| Shown {
            frame_id,
            key,
            screen,
        }))
    }

    /// 解码器放在别的线程时用：收齐帧、而且接得上时，返回交给解码线程的任务（按顺序，可能几帧一起：
    /// 参考帧补齐了，等它的帧跟着交出）。这些帧先算作送达；解码线程报告失败时调用 [`Self::decode_failed`]。
    pub fn on_datagram_deferred(
        &mut self,
        now: Time,
        send_time: Time,
        seq: u32,
        flags: u8,
        body: &[u8],
    ) -> Result<Vec<DecodeJob>, Error> {
        let jobs = self.assemble(now, send_time, seq, flags, body)?;
        for job in &jobs {
            self.decoded = Some(job.frame_id);
            self.log.delivered(job.frame_id);
        }
        Ok(jobs)
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

    /// 重组分片；收齐帧并且接得上时返回解码任务（按顺序）。
    fn assemble(
        &mut self,
        now: Time,
        send_time: Time,
        seq: u32,
        flags: u8,
        body: &[u8],
    ) -> Result<Vec<DecodeJob>, Error> {
        self.tracker.on_packet(now, send_time, seq, body.len());
        if self.reports {
            self.arrivals.on_packet(now, seq);
        }
        // 探测用的填充包：记到达时间；开头 4 字节是组号，按组测包串速率（见 Caps::PROBE_TRAIN），
        // 第 9–12 字节是这一组一共几个包（老版本发的是 0：不知道）
        if flags & FLAG_PADDING != 0 {
            if let Some(id) = body.first_chunk::<4>() {
                let total = body
                    .get(8..12)
                    .and_then(|b| b.first_chunk::<4>())
                    .map_or(0, |b| u32::from_le_bytes(*b));
                self.metrics
                    .on_probe(now, send_time, u32::from_le_bytes(*id), total, body.len());
            }
            return Ok(Vec::new());
        }
        let mut r = Reader::new(body);
        let header = VideoHeader::decode(&mut r)?;
        self.metrics
            .on_packet(now, send_time, header.frame_id, body.len());
        let packet = VideoPacket {
            header,
            parity: flags & FLAG_PARITY != 0,
            data: r.rest().to_vec(),
        };
        let Some(first) = self.reassembler.push(now, packet)? else {
            return Ok(Vec::new());
        };
        // 一帧收齐时，等它的帧（参考它、先收齐了）按顺序跟着交出
        let frames =
            std::iter::once(first).chain(std::iter::from_fn(|| self.reassembler.take_ready()));
        let frames: Vec<crate::transport::Frame> = frames.collect();
        let mut jobs = Vec::with_capacity(frames.len());
        // 前一个交给解码器的帧（解码在后面做；按顺序判断接不接得上）
        let mut last = self.decoded;
        for frame in frames {
            if let Some(job) = self.accept(frame, last) {
                last = Some(job.frame_id);
                jobs.push(job);
            } else {
                last = None;
            }
        }
        Ok(jobs)
    }

    /// 收齐的一帧：记计数；接在 `last`（前一个交给解码器的帧）后面能解时返回解码任务，否则记为没送达。
    fn accept(&mut self, frame: crate::transport::Frame, last: Option<u32>) -> Option<DecodeJob> {
        let took = frame.completed.since(frame.first_seen);
        self.stats.assembled = self.stats.assembled.saturating_add(1);
        self.stats.assembly_us_sum = self.stats.assembly_us_sum.saturating_add(took);
        self.stats.assembly_us_max = self.stats.assembly_us_max.max(took);
        self.stats.video_bytes = self
            .stats
            .video_bytes
            .saturating_add(u64::try_from(frame.data.len()).unwrap_or(u64::MAX));
        let Ok((fh, data)) = FrameHeader::parse(&frame.data) else {
            self.stats.bad = self.stats.bad.saturating_add(1);
            self.log.advance(frame.frame_id);
            return None;
        };
        // 视频编码的增量帧只能接在它参考的那一帧后面解；块编码的每一帧都能直接应用。
        let decodable = fh.key
            || fh.codec == Codec::Tile
            || last == Some(frame.ref_frame) && frame.ref_frame != frame.frame_id;
        if !decodable {
            // 没交给解码器：不算送达，发送端据此补关键帧。
            self.decoded = None;
            self.stats.skipped = self.stats.skipped.saturating_add(1);
            self.log.advance(frame.frame_id);
            return None;
        }
        Some(DecodeJob {
            frame_id: frame.frame_id,
            header: fh,
            data: data.to_vec(),
        })
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
        !self.nacks(now, 1).is_empty()
    }

    /// 要请求重传的分片：这一帧第一片到了 10 ms，缺的全要（发送端按“发出之后过了往返加排队”决定真的重传哪些）。
    /// 按空洞和超时判丢（RFC 9002）试过：虚假重传少了很多，但丢一大片时恢复变慢（drop-while-idle 冻结 1666 ms），
    /// 先没用，见 docs/research/L-framework.md 6.9。
    fn nacks(&self, now: Time, limit: usize) -> Vec<(u32, u16)> {
        self.reassembler.missing(now, NACK_AFTER_US, limit)
    }

    /// 生成一个反馈数据报：反馈加上缺失分片的重传请求。
    pub fn feedback_datagram(&mut self, now: Time) -> Vec<u8> {
        let feedback = self.feedback(now);
        let mut nacks = self.nacks(now, MAX_NACKS);
        self.stats.nacks_sent = self
            .stats
            .nacks_sent
            .saturating_add(u32::try_from(nacks.len()).unwrap_or(u32::MAX));
        // 新反馈格式：正在收的帧的进度排在前面（“还活着”和“要重传”是两个信号，见 PROGRESS）
        if self.probe_ids {
            let mut list = self.reassembler.progress(PROGRESS_FRAMES);
            nacks.truncate(MAX_NACKS.saturating_sub(list.len()));
            list.extend(nacks);
            nacks = list;
        }
        if self.reports {
            let report = self.arrivals.report();
            return encode_feedback_v2(&feedback, &nacks, report.as_ref());
        }
        let ext = self.metrics.report();
        if self.probe_ids {
            // 只带运行中探测那个号段的结果：起步、静止探测照旧只看包串速率，它们的反馈和老格式逐字节相同
            let probe = self.metrics.take_probe_report().filter(|p| {
                p.id & (RUN_PROBE_ID_BASE | START_PROBE_ID_BASE | IDLE_PROBE_ID_BASE)
                    == RUN_PROBE_ID_BASE
            });
            return encode_feedback_ids(&feedback, &ext, &nacks, probe.as_ref());
        }
        encode_feedback(&feedback, &ext, &nacks)
    }

    /// 反馈里的探测结果带组号（握手时双方都支持 [`crate::proto::Caps::PROBE_ID`]）。带到达报告的反馈格式不带。
    pub const fn set_probe_ids(&mut self, on: bool) {
        self.probe_ids = on;
    }

    /// 取走这段时间收齐一帧用的最长时间（微秒），清零。
    pub fn take_assembly_max(&mut self) -> u64 {
        std::mem::take(&mut self.stats.assembly_us_max)
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

    /// 每帧都比预算大得多的编码器（满屏视频、窄带宽）。
    struct HugeEncoder;

    impl VideoEncoder for HugeEncoder {
        fn codec(&self) -> Codec {
            Codec::H264
        }
        fn encode(
            &mut self,
            _id: u32,
            s: &Screen,
            _changed: bool,
            _budget: usize,
        ) -> Result<Option<EncodedFrame>, Error> {
            let header = FrameHeader {
                codec: Codec::H264,
                key: false,
                width: s.width(),
                height: s.height(),
            };
            Ok(Some(EncodedFrame {
                payload: header.frame(&[0u8; 20_000]),
                key: false,
            }))
        }
        fn on_feedback(&mut self, _floor: u32, _mask: u32) {}
        fn on_lost(&mut self, _frame_id: u32) {}
        fn wants_refine(&self) -> bool {
            false
        }
    }

    /// 适配的最后一步：帧率降到最低（5 帧）之后还一直装不进预算 2 秒，降分辨率，说明原因。
    #[test]
    fn content_that_never_fits_drops_resolution_as_the_last_resort() {
        let mut tx = MediaSender::new(
            SenderConfig {
                start_bps: 200_000,
                min_bps: 200_000,
                max_bps: 200_000,
                ..cfg()
            },
            Box::new(HugeEncoder),
        );
        tx.scale = ScalePolicy::full();
        let mut t = Time::ZERO;
        let mut events = Vec::new();
        for i in 0..600u32 {
            t = t.plus(33_334);
            if tx.due(t) {
                let _ = tx
                    .on_frame(t, &Screen::new(16, 16, i), true, 40_000)
                    .unwrap();
            }
            events.extend(tx.take_events());
            if tx.scale.permille() < crate::scale::FULL_PERMILLE {
                break;
            }
        }
        assert_eq!(tx.fps(), 5, "先把帧率降到最低");
        assert_eq!(tx.scale.permille(), crate::scale::START_PERMILLE);
        assert!(
            events.iter().any(|e| e.contains("降分辨率是最后手段")),
            "{events:?}"
        );
    }

    /// 交像素和交 [`Frame`] 是同一条路：同样的画面、同样的时刻，出来的数据报逐字节相同。
    #[test]
    fn frames_and_screens_take_the_same_path() {
        let screen = Screen::new(64, 64, 1);
        let mut other = screen.clone();
        other.fill(8, 8, 16, 16, 0x00ff_0000);
        let mut by_screen = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let mut by_frame = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        for (i, s) in [&screen, &other, &other].into_iter().enumerate() {
            let now = Time(u64::try_from(i).unwrap() * 40_000);
            let changed = i < 2;
            let a = by_screen.on_frame(now, s, changed, 10_000).unwrap();
            let b = by_frame
                .on_frame_any(now, &Frame::rgb(s.clone()), changed, 10_000)
                .unwrap();
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(&b) {
                assert_eq!(
                    (x.flags, &x.body, x.fragment),
                    (y.flags, &y.body, y.fragment)
                );
            }
            assert_eq!(by_screen.stats(), by_frame.stats());
        }
        // 内容描述：不收它的编码器（这里的测试编码器、块编码、真 H.264 后端）如实拒绝
        let content = ContentDesc {
            version: 1,
            size: (64, 64),
            area_pixels: Arc::from(Vec::new()),
            area_detail: Arc::from(Vec::new()),
            luma: Vec::new(),
            dirty: Vec::new(),
            moves: Vec::new(),
        };
        assert!(content.luma_at((64, 64)).is_none());
        assert!(by_frame
            .on_described(Time(200_000), &content, true, 10_000)
            .is_err());
        // 编码器不在发送端里（产品的用法）：两个入口都说明该走哪条路
        let mut detached = MediaSender::detached(cfg(), Codec::H264);
        assert!(detached
            .on_frame_any(Time(0), &Frame::rgb(screen), true, 10_000)
            .is_err());
    }

    /// 回归（2026-11-27 Mac 日志，`P-fps-bitrate.md` M2）：硬件编码器因为预算放不下把帧丢了，发送端什么都没发出去；
    /// 这是网络受限，不能判成应用受限（应用受限不升码率，画面一直停着）。
    #[test]
    fn frames_dropped_by_the_encoder_for_lack_of_budget_are_network_limited() {
        use crate::estimator::RateState;
        let screen = Screen::new(64, 64, 1);
        let acked = |id: u32| Feedback {
            floor: id,
            delivered: 1,
            ..Feedback::default()
        };
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.scale = ScalePolicy::full();
        let _ = tx.on_frame(Time(0), &screen, true, 10_000).unwrap();
        tx.observe(Time(50_000), &acked(1), Extension::default());
        // 600 ms 没有任何原因、也没发东西：应用受限
        let mut t = 50_000;
        while t < 700_000 {
            t += 25_000;
            tx.on_tick(Time(t));
        }
        tx.observe(Time(t), &acked(1), Extension::default());
        assert_eq!(tx.rate_state(), RateState::AppLimited);
        // 编码器丢帧：马上回跟踪，原因写进日志；之后 500 ms 内的反馈都算网络受限
        tx.encoder_dropped(Time(t + 10_000));
        assert_eq!(
            tx.net_limited,
            Some((Time(t + 10_000), NetCause::EncoderDropped))
        );
        assert_eq!(tx.rate_state(), RateState::Tracking);
        assert!(tx
            .take_events()
            .join("\n")
            .contains("应用受限 → 跟踪：网络受限：编码器因为预算放不下把帧丢了"),);
        tx.observe(Time(t + 300_000), &acked(1), Extension::default());
        assert_eq!(
            tx.last_observation().unwrap().limit,
            SendLimit::Network(NetCause::EncoderDropped)
        );
    }

    /// 发送量受谁限制按原因判：有任何网络受限的原因就不算应用受限，500 ms 都没有、发出的量也少才算。
    #[test]
    fn the_send_limit_is_judged_by_cause_not_by_ratio() {
        use crate::estimator::RateState;
        let screen = Screen::new(64, 64, 1);
        let acked = |id: u32| Feedback {
            floor: id,
            delivered: 1,
            ..Feedback::default()
        };
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.scale = ScalePolicy::full();
        assert_eq!(
            tx.send_limit(Time(0)),
            SendLimit::Network(NetCause::Start),
            "会话刚开始：还没观察够"
        );
        // 没东西要编、没到时间：不编，也不算被网络拦下
        assert!(!tx.frame_gate(Time(0), false, false));
        assert!(tx.frame_gate(Time(0), true, false));
        let key = tx.on_frame(Time(0), &screen, true, 10_000).unwrap();
        assert!(!tx.frame_gate(Time(1_000), true, false), "没到下一帧的时间");
        assert_eq!(tx.net_limited, Some((Time(0), NetCause::FrameNearBudget)));
        // 关键帧没确认：该编帧时被拦下，记“关键帧还没确认”
        assert!(!tx.frame_gate(Time(40_000), true, false));
        assert_eq!(tx.net_limited, Some((Time(40_000), NetCause::KeyPending)));
        tx.observe(Time(50_000), &acked(1), Extension::default());
        assert_eq!(
            tx.last_observation().unwrap().limit,
            SendLimit::Network(NetCause::KeyPending)
        );
        // 之后 500 ms 什么原因都没有、几乎不发（预算桶攒满）：应用受限，估计器进应用受限
        let mut t = 50_000;
        while t < 525_000 {
            t += 25_000;
            tx.on_tick(Time(t));
            assert_eq!(
                tx.send_limit(Time(t)),
                SendLimit::Network(NetCause::KeyPending),
                "{t}"
            );
        }
        tx.on_tick(Time(550_000));
        tx.observe(Time(550_000), &acked(1), Extension::default());
        assert_eq!(tx.last_observation().unwrap().limit, SendLimit::App);
        assert_eq!(tx.rate_state(), RateState::AppLimited);
        // 小帧（远不到预算的八成）照常发：还是应用受限
        assert!(tx.frame_gate(Time(560_000), true, false));
        let small = tx.on_frame(Time(560_000), &screen, true, 10_000).unwrap();
        tx.on_packet_sent(Time(560_000), 1, &small[0]);
        tx.observe(Time(580_000), &acked(2), Extension::default());
        assert_eq!(tx.rate_state(), RateState::AppLimited);
        // 节奏器里上一帧没发完：记一次网络受限，估计器不等反馈马上回跟踪
        assert!(!tx.frame_gate(Time(600_000), true, true));
        assert_eq!(
            tx.net_limited,
            Some((Time(600_000), NetCause::FrameBlocked))
        );
        assert_eq!(tx.rate_state(), RateState::Tracking);
        let events = tx.take_events().join("\n");
        assert!(
            events.contains("码率状态 跟踪 → 应用受限")
                && events.contains(
                    "码率状态 应用受限 → 跟踪：网络受限：该编帧时在途数据满了或上一帧没发完"
                ),
            "{events}"
        );
        // 500 ms 没有原因，但发出的量接近码率（预算桶没判受限）：算网络受限
        let big = Outgoing {
            body: vec![0u8; 1_200],
            ..key[0].clone()
        };
        let mut t = 600_000;
        while t < 1_200_000 {
            t += 5_000;
            tx.on_packet_sent(Time(t), 2, &big);
        }
        assert_eq!(
            tx.send_limit(Time(t)),
            SendLimit::Network(NetCause::UsingRate)
        );
        // 编码分辨率低于原尺寸：一直算网络受限
        tx.scale = ScalePolicy::new();
        assert_eq!(
            tx.send_limit(Time(t)),
            SendLimit::Network(NetCause::ScaleReduced)
        );
    }

    /// 内容装不进预算（帧率降档）、重传被额度截断，都是网络受限的原因。
    #[test]
    fn overflowing_frames_and_capped_resends_are_network_limits() {
        let mut tx = MediaSender::new(
            SenderConfig {
                start_bps: 200_000,
                min_bps: 200_000,
                max_bps: 200_000,
                ..cfg()
            },
            Box::new(HugeEncoder),
        );
        tx.scale = ScalePolicy::full();
        let mut t = Time::ZERO;
        let mut first = Vec::new();
        for i in 0..20u32 {
            t = t.plus(33_334);
            if tx.frame_gate(t, true, false) {
                let out = tx
                    .on_frame(t, &Screen::new(16, 16, i), true, 40_000)
                    .unwrap();
                assert_eq!(tx.net_limited, Some((t, NetCause::FrameNearBudget)));
                if first.is_empty() {
                    first = out;
                }
            }
        }
        assert!(tx.fps() < 30, "装不进预算，帧率降档了");
        assert_eq!(
            tx.send_limit(t),
            SendLimit::Network(NetCause::FpsReduced),
            "降档期间一直算网络受限"
        );
        // 第一帧的分片都发出去了，接收端全要重传：额度（码率 × 40 ms = 1000 字节）只够补 3 片，其余截断
        for (i, o) in first.iter().enumerate() {
            tx.on_packet_sent(Time(40_000), u32::try_from(i).unwrap(), o);
        }
        let nacks: Vec<(u32, u16)> = (0..10).map(|i| (1, i)).collect();
        let fb = encode_feedback(&Feedback::default(), &Extension::default(), &nacks);
        let at = t.plus(1_000_000);
        let again = tx.on_feedback_datagram(at, &fb).unwrap();
        assert_eq!(again.len(), 3);
        assert_eq!(tx.stats().resend_capped, 7);
        assert_eq!(tx.net_limited, Some((at, NetCause::ResendCapped)));
    }

    /// 起步探测的三种结束（链路不够宽、有丢包、探完）都告诉估计器，状态从起步回到跟踪。
    #[test]
    fn the_start_probe_drives_the_startup_state() {
        use crate::estimator::RateState;
        let screen = Screen::new(64, 64, 1);
        let acked = |id: u32, loss: u16| Feedback {
            floor: id,
            delivered: 1,
            loss_permille: loss,
            ..Feedback::default()
        };
        let begin = || {
            let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
            tx.set_start_probe(true);
            assert_eq!(tx.rate_state(), RateState::Startup);
            tx.on_frame(Time(0), &screen, true, 10_000).unwrap();
            tx
        };
        // 第一个关键帧送得慢：不探
        let mut tx = begin();
        tx.observe(Time(900_000), &acked(1, 0), Extension::default());
        assert_eq!(tx.rate_state(), RateState::Tracking);
        assert!(tx
            .take_events()
            .join("\n")
            .contains("码率状态 起步 → 跟踪：不做起步探测：链路不比起步码率宽多少"));
        // 有丢包：不探
        let mut tx = begin();
        tx.observe(Time(5_000), &acked(0, 50), Extension::default());
        assert_eq!(tx.rate_state(), RateState::Tracking);
        assert!(tx
            .take_events()
            .join("\n")
            .contains("码率状态 起步 → 跟踪：不做起步探测：有丢包"));
        // 探了，到 1 秒没测到结果
        let mut tx = begin();
        tx.observe(Time(10_000), &acked(1, 0), Extension::default());
        assert_eq!(tx.rate_state(), RateState::Startup, "在探");
        tx.on_tick(Time(10_000 + START_PROBE_MAX_US));
        assert_eq!(tx.rate_state(), RateState::Tracking);
        assert!(tx
            .take_events()
            .join("\n")
            .contains("码率状态 起步 → 跟踪：起步探测没测到结果"));
        // 探了，有结论
        let mut tx = begin();
        tx.observe(Time(10_000), &acked(1, 0), Extension::default());
        tx.observe(
            Time(60_000),
            &Feedback::default(),
            Extension {
                train_bytes_per_s: 125_000,
                ..Extension::default()
            },
        );
        tx.on_tick(Time(10_000 + START_PROBE_MAX_US));
        assert_eq!(tx.rate_state(), RateState::Tracking);
        assert!(tx
            .take_events()
            .join("\n")
            .contains("码率状态 起步 → 跟踪：起步探测有结论了"));
        // 不开起步探测：一直是跟踪
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.set_start_probe(false);
        assert_eq!(tx.rate_state(), RateState::Tracking);
    }

    #[test]
    fn too_many_nacks_in_a_v2_feedback_are_rejected() {
        let fb = Feedback {
            floor: 1,
            delivered: 1,
            loss_permille: 0,
            queue_delay_us: 0,
            recv_bytes_per_s: 0,
        };
        let mut buf = encode_feedback_v2(&fb, &[], None);
        // 数据报头之后第一个字节是重传请求的个数：改成超过上限
        if let Some(count) = buf.get_mut(Feedback::LEN) {
            *count = u8::try_from(MAX_NACKS + 1).unwrap();
        }
        assert!(matches!(
            decode_feedback_v2(&buf),
            Err(Error::TooManyItems { .. })
        ));
    }

    #[test]
    fn a_new_view_size_asks_for_a_frame() {
        let mut tx = MediaSender::detached(SenderConfig::default(), Codec::H264);
        assert!(!tx.wants_frame());
        tx.set_view(Some((640, 360)));
        assert!(tx.wants_frame(), "窗口变了：画面不动也要按新尺寸编一帧");
        let job = tx.start_frame(Time(0), 10_000);
        assert_eq!(job.view, Some((640, 360)));
        assert!(!tx.wants_frame());
        tx.set_view(Some((640, 360)));
        assert!(!tx.wants_frame(), "没变不用编");
        tx.set_view(Some((0, 0)));
        assert!(tx.wants_frame(), "原始大小：不限");
        assert_eq!(tx.start_frame(Time(1), 10_000).view, None);
    }

    /// 编码尺寸：开始半分辨率时放进控制端窗口；原尺寸时不看窗口。
    #[test]
    fn encode_size_follows_scale_and_view() {
        let mut tx = MediaSender::detached(SenderConfig::default(), Codec::H264);
        tx.set_view(Some((800, 400)));
        assert_eq!(
            tx.encode_size((2880, 1800)),
            (640, 400),
            "半分辨率 1440×900 再放进窗口"
        );
        let mut full = MediaSender::detached(SenderConfig::default(), Codec::Tile);
        full.set_view(Some((800, 400)));
        assert_eq!(full.encode_size((2880, 1800)), (2880, 1800));
    }
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
        assert!(rx.screen().is_some(), "显示的画面可以取到");
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
    fn sustained_loss_adds_fec_and_switches_to_the_wireless_profile() {
        let mut tx = MediaSender::new(
            SenderConfig {
                profile: Profile::STABLE,
                ..cfg()
            },
            Box::new(FakeEncoder::default()),
        );
        assert_eq!(tx.fec_permille(), 0);
        let lossy = |loss| Feedback {
            loss_permille: loss,
            ..Feedback::default()
        };
        // 丢包低于 1%：不加纠错，不换画像
        tx.on_feedback(Time(1), &lossy(5));
        assert_eq!(tx.fec_permille(), 0);
        // 6% 丢包：纠错按 3 倍加到 18%；连续两次才换画像
        tx.on_feedback(Time(2), &lossy(60));
        assert_eq!(tx.fec_permille(), 180);
        assert_eq!(tx.stats().profile_switches, 0);
        tx.on_feedback(Time(3), &lossy(60));
        assert_eq!(tx.stats().profile_switches, 1);
        assert_eq!(tx.profile_name(), "wireless");
        // 换成“无线”后不再换；丢包很高时纠错封顶 33%
        tx.on_feedback(Time(4), &lossy(300));
        tx.on_feedback(Time(5), &lossy(300));
        assert_eq!(tx.fec_permille(), FEC_MAX_PERMILLE);
        assert_eq!(tx.stats().profile_switches, 1);
        // 丢包恢复：回到画像自带的纠错
        tx.on_feedback(Time(6), &lossy(0));
        assert_eq!(tx.fec_permille(), Profile::WIRELESS.fec_permille);
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
    fn a_whole_frame_request_resends_all_its_data_fragments() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.set_profile(Profile {
            fec_permille: 0,
            ..Profile::STABLE
        });
        let out = tx
            .on_frame(Time::ZERO, &Screen::new(16, 16, 3), true, 40_000)
            .unwrap();
        assert!(out.len() > 3);
        for (seq, o) in (0u32..).zip(&out) {
            tx.on_packet_sent(Time::ZERO, seq, o);
        }
        let fb = Feedback {
            floor: 0,
            delivered: 0,
            queue_delay_us: 0,
            loss_permille: 0,
            recv_bytes_per_s: 0,
        };
        // 接收端一个分片都没收到这一帧，只从后面的帧知道它存在：整帧重传，每个数据分片一份
        let whole = encode_feedback(&fb, &Extension::default(), &[(1, WHOLE_FRAME)]);
        let again = tx.on_feedback_datagram(Time(60_000), &whole).unwrap();
        assert_eq!(again.len(), out.len());
        assert!(again.iter().all(|o| o.kind == PacketKind::Retransmit));
        // 不认识的帧：什么也不发
        let unknown = encode_feedback(&fb, &Extension::default(), &[(9, WHOLE_FRAME)]);
        assert!(tx
            .on_feedback_datagram(Time(60_000), &unknown)
            .unwrap()
            .is_empty());
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
        // 原包还在节奏器里没发（接收端只是还没收到）：不重传
        assert!(tx
            .on_feedback_datagram(Time(20_050), &fb)
            .unwrap()
            .is_empty());
        assert_eq!(tx.stats().resend_unsent, 1);
        // 节奏器发出了（含校验分片，不影响）；刚发出、还在路上时再要：不重传
        for (seq, o) in (0u32..).zip(&out) {
            tx.on_packet_sent(Time(20_000), seq, o);
        }
        assert!(tx
            .on_feedback_datagram(Time(30_000), &fb)
            .unwrap()
            .is_empty());
        // 发出后过了一个往返（40 ms）还没到：重传
        let again = tx.on_feedback_datagram(Time(60_100), &fb).unwrap();
        assert_eq!(
            again,
            vec![Outgoing {
                kind: PacketKind::Retransmit,
                ..out[1].clone()
            }],
            "只重传缺的那一片，标成重传"
        );
        // 重传还没发出去时再要：不重复排
        assert!(tx
            .on_feedback_datagram(Time(60_200), &fb)
            .unwrap()
            .is_empty());
        assert_eq!(tx.stats().retransmitted, 1);
        // 重传到了：整帧收齐、显示
        let shown = deliver(&mut rx, Time(60_000), &again, |_| false);
        assert_eq!(shown, vec![(1, 3)]);
        // 重传还在节奏器里没发出去：过了一个往返再要也不排第二份
        assert!(tx
            .on_feedback_datagram(Time(70_000), &fb)
            .unwrap()
            .is_empty());
        // 节奏器发出了；从实际发出算过了一个往返还缺（假设重传也丢了）：可以再发
        for o in &again {
            tx.on_packet_sent(Time(70_000), 9, o);
        }
        assert!(tx
            .on_feedback_datagram(Time(80_000), &fb)
            .unwrap()
            .is_empty());
        assert_eq!(
            tx.on_feedback_datagram(Time(120_000), &fb).unwrap().len(),
            1
        );
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

    /// 关键帧发出后一直没有反馈（丢了一片，接收端发现不了）：会话循环判它丢失，
    /// 之后允许编新帧、并且要编（恢复），而不是一直等那个关键帧。
    #[test]
    fn unacked_keyframe_is_expired_even_when_nothing_new_is_encoded() {
        let mut remote = MediaSender::detached(cfg(), Codec::H264);
        let mut encoder = FakeEncoder::default();
        let screen = Screen::new(16, 16, 1);
        let job = remote.start_frame(Time::ZERO, 50_000);
        let (encoded, refine) =
            encode_job(&mut encoder, &job, &Frame::rgb(screen.clone()), true).unwrap();
        assert!(!remote
            .finish_frame(Time::ZERO, &job, encoded, refine)
            .unwrap()
            .is_empty());
        // 关键帧没确认：不编新帧
        assert!(remote.in_flight_full(Time(10_000)));
        // 画面静止，不再调用 start_frame；会话循环照常检查
        remote.expire_unacked(Time(100_000));
        assert!(remote.in_flight_full(Time(100_000)), "还没到时限");
        remote.expire_unacked(Time(5_000_000));
        assert!(!remote.in_flight_full(Time(5_000_000)), "判丢之后不再挡着");
        assert!(remote.wants_frame(), "要重新编关键帧");
        let job = remote.start_frame(Time(5_000_000), 50_000);
        assert_eq!(job.lost.len(), 1);
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
            let (encoded, refine) =
                encode_job(&mut encoder, &job, &Frame::rgb(screen.clone()), true).unwrap();
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
        let (encoded, refine) = encode_job(
            &mut encoder,
            &job,
            &Frame::rgb(Screen::new(16, 16, 3)),
            false,
        )
        .unwrap();
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
                jobs.extend(
                    rx.on_datagram_deferred(Time(t), Time(t), seq, o.flags, &o.body)
                        .unwrap(),
                );
            }
            jobs
        };
        // 关键帧和一个增量帧都派出去了；解码器先解关键帧，再“解失败”增量帧
        let key = step(&mut tx, &mut rx, 1, 0);
        let bad = step(&mut tx, &mut rx, 0x00de_ad00, 1);
        let next = step(&mut tx, &mut rx, 3, 2);
        assert_eq!(
            worker
                .decode(&key[0])
                .unwrap()
                .unwrap()
                .to_screen()
                .unwrap()
                .pixels()[0],
            1
        );
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
        assert_eq!(
            worker
                .decode(&key[0])
                .unwrap()
                .unwrap()
                .to_screen()
                .unwrap()
                .pixels()[0],
            6
        );
    }

    #[test]
    fn idle_probes_back_off_and_their_result_sets_the_rate() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.set_idle_probe(true);
        // 还没发过帧：不探
        tx.on_tick(Time(5_000_000));
        assert!(tx.take_probes().is_empty());
        let screen = Screen::new(64, 64, 1);
        assert!(!tx
            .on_frame(Time(0), &screen, true, 10_000)
            .unwrap()
            .is_empty());
        // 停下不到 2 秒不探；2 秒探第一次（按码率的 4 倍发）
        tx.on_tick(Time(1_900_000));
        assert!(tx.take_probes().is_empty());
        tx.on_tick(Time(2_000_000));
        let first = tx.take_probes();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id, IDLE_PROBE_ID_BASE | 1);
        assert_eq!(
            first[0].target_bps,
            (tx.rate_bps() * IDLE_PROBE_SCALE).min(cfg().max_bps)
        );
        // 结果：测得 320 kbps（不到起步码率 1 Mbps 的一半）→ 码率按它降，写明原因
        tx.observe(
            Time(2_050_000),
            &Feedback::default(),
            Extension {
                train_bytes_per_s: 40_000,
                ..Extension::default()
            },
        );
        assert_eq!(tx.rate_bps(), 288_000);
        assert!(tx.take_events().iter().any(|e| e.contains("静止时探测")));
        // 间隔翻倍：下一次在 4 秒后，再下一次 8 秒后
        tx.on_tick(Time(5_900_000));
        assert!(tx.take_probes().is_empty());
        tx.on_tick(Time(6_000_000));
        assert_eq!(tx.take_probes().len(), 1);
        tx.on_tick(Time(13_900_000));
        assert!(tx.take_probes().is_empty());
        tx.on_tick(Time(14_000_000));
        assert_eq!(tx.take_probes().len(), 1);
        // 发了帧：从头排；探测之后发过帧，之后的包串速率不算探测结果
        tx.on_frame(Time(14_100_000), &Screen::new(64, 64, 2), true, 10_000)
            .unwrap();
        let rate = tx.rate_bps();
        tx.observe(
            Time(14_200_000),
            &Feedback::default(),
            Extension {
                train_bytes_per_s: 10_000,
                ..Extension::default()
            },
        );
        assert_eq!(tx.rate_bps(), rate);
        tx.on_tick(Time(16_000_000));
        assert!(tx.take_probes().is_empty());
        tx.on_tick(Time(16_100_000));
        assert_eq!(tx.take_probes().len(), 1);
        // 对端不支持：不探
        let mut off = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        off.set_idle_probe(false);
        off.on_frame(Time(0), &screen, true, 10_000).unwrap();
        off.on_tick(Time(5_000_000));
        assert!(off.take_probes().is_empty());
    }

    // ---- 运行中探测的状态机（空闲 → 待发 → 已发 → 判定）----

    use crate::estimator::{ProbeKind, RateState};
    use std::sync::{Arc, Mutex};

    /// 假估计器：想探一轮 `want`，把每轮怎么结束的记下来。
    #[derive(Debug)]
    struct Asker {
        profile: Profile,
        want: Option<ProbeAsk>,
        asked: bool,
        state: Arc<Mutex<RateState>>,
        ends: Arc<Mutex<Vec<ProbeEnd>>>,
    }

    impl RateEstimator for Asker {
        fn name(&self) -> &'static str {
            "asker"
        }
        fn rate_bps(&self) -> u64 {
            10_000_000
        }
        fn on_observation(&mut self, _now: Time, _obs: &Observation) {}
        fn profile(&self) -> &Profile {
            &self.profile
        }
        fn set_profile(&mut self, profile: Profile) {
            self.profile = profile;
        }
        fn wants_idle_probe(&self) -> bool {
            true
        }
        fn state(&self) -> RateState {
            *self.state.lock().unwrap()
        }
        fn on_net_limited(&mut self, _cause: NetCause) {
            *self.state.lock().unwrap() = RateState::Tracking;
        }
        fn probe_wanted(&mut self, _now: Time) -> Option<ProbeAsk> {
            assert!(!self.asked, "上一轮还没回报就又来问");
            self.asked = self.want.is_some();
            self.want.take()
        }
        fn on_probe_end(&mut self, _now: Time, end: ProbeEnd) -> ProbeVerdict {
            assert!(self.asked, "没要过探测却来回报");
            self.asked = false;
            self.ends.lock().unwrap().push(end);
            match end {
                ProbeEnd::Result(bps) if bps >= 14_000_000 => ProbeVerdict::Confirmed,
                ProbeEnd::Result(_) => ProbeVerdict::Denied,
                ProbeEnd::TimedOut => ProbeVerdict::TimedOut,
                ProbeEnd::Aborted(_) => ProbeVerdict::Aborted,
            }
        }
    }

    type Ends = Arc<Mutex<Vec<ProbeEnd>>>;

    /// 一个对端支持探测组号的发送端，估计器想按 20 Mbps 探一轮（`kind`）。
    fn asking(kind: ProbeKind) -> (MediaSender, Ends, Arc<Mutex<RateState>>) {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.scale = ScalePolicy::full();
        let ends = Ends::default();
        let state = Arc::new(Mutex::new(match kind {
            ProbeKind::Verify => RateState::Verify,
            ProbeKind::Periodic => RateState::AppLimited,
        }));
        tx.rate = Box::new(Asker {
            profile: Profile::STABLE,
            want: Some(ProbeAsk {
                kind,
                target_bps: 20_000_000,
            }),
            asked: false,
            state: state.clone(),
            ends: ends.clone(),
        });
        tx.set_probe_ids(true);
        (tx, ends, state)
    }

    /// 带探测结果的一份反馈。
    fn probe_feedback(id: u32, bps: u64, done: bool) -> Vec<u8> {
        encode_feedback_ids(
            &Feedback::default(),
            &Extension::default(),
            &[],
            Some(&ProbeReport {
                id,
                bytes_per_s: u32::try_from(bps / 8).unwrap(),
                packets: 62,
                done,
            }),
        )
    }

    #[test]
    fn a_run_probe_is_sent_when_clear_and_ends_with_the_tagged_result() {
        let (mut tx, ends, _) = asking(ProbeKind::Periodic);
        tx.on_tick(Time(1_000_000));
        // 能探：马上发一组，30 ms 的量（20 Mbps 是 62 个包），组号在运行中探测的号段
        let cluster = tx.take_probes();
        assert_eq!(cluster.len(), 1);
        let id = cluster[0].id;
        assert_eq!(id & RUN_PROBE_ID_BASE, RUN_PROBE_ID_BASE);
        assert_eq!(id & (IDLE_PROBE_ID_BASE | START_PROBE_ID_BASE), 0);
        assert_eq!(
            (cluster[0].target_bps, cluster[0].packets),
            (20_000_000, 62)
        );
        assert!(tx
            .take_events()
            .iter()
            .any(|e| e == "周期探测：按 20000 kbps 发 62 个包（码率 10000 kbps）"));
        // 探测期间照常出帧
        assert!(tx.frame_gate(Time(1_000_000), true, false));
        // 别的组的结果、没带结果的反馈：不认
        tx.on_feedback_datagram(Time(1_020_000), &probe_feedback(id ^ 1, 5_000_000, true))
            .unwrap();
        tx.on_feedback_datagram(
            Time(1_030_000),
            &encode_feedback_ids(&Feedback::default(), &Extension::default(), &[], None),
        )
        .unwrap();
        assert!(ends.lock().unwrap().is_empty());
        // 这一组收完的结果：结束，结论记一次
        tx.on_feedback_datagram(Time(1_040_000), &probe_feedback(id, 19_000_000, true))
            .unwrap();
        assert_eq!(*ends.lock().unwrap(), [ProbeEnd::Result(19_000_000)]);
        let p = tx.stats().probes;
        assert_eq!((p.run_sent, p.periodic_rounds, p.verify_rounds), (1, 1, 0));
        assert_eq!(p.run_verdicts, [1, 0, 0, 0]);
        // 之后迟到的结果不再认；估计器不想探就一直空闲
        tx.on_feedback_datagram(Time(1_060_000), &probe_feedback(id, 1_000_000, true))
            .unwrap();
        tx.on_tick(Time(1_100_000));
        assert!(tx.take_probes().is_empty());
        assert_eq!(ends.lock().unwrap().len(), 1);
    }

    /// I4：每一轮探测必然在时限内结束——已发的到 往返 + 一组的时长 + 65 ms 没有结果就是超时（收到过一半的结果
    /// 就按它判），待发的 200 ms 等不到能探的时机就中止。
    #[test]
    fn i4_every_probe_round_ends_within_its_time_limit() {
        // 没有任何结果：超时。一组 62 个包按 20 Mbps 发 29.76 ms，往返 0，时限 94.76 ms
        let (mut tx, ends, _) = asking(ProbeKind::Verify);
        tx.on_tick(Time(1_000_000));
        assert_eq!(tx.take_probes().len(), 1);
        tx.on_tick(Time(1_094_000));
        assert!(ends.lock().unwrap().is_empty(), "还没到时限");
        tx.on_tick(Time(1_095_000));
        assert_eq!(*ends.lock().unwrap(), [ProbeEnd::TimedOut]);
        let p = tx.stats().probes;
        assert_eq!((p.run_sent, p.verify_rounds), (1, 1));
        assert_eq!(p.run_verdicts, [0, 0, 1, 0]);
        // 只收到一半时报的结果：到时限按它判
        let (mut tx, ends, _) = asking(ProbeKind::Periodic);
        tx.on_tick(Time(0));
        let id = tx.take_probes()[0].id;
        tx.on_feedback_datagram(Time(20_000), &probe_feedback(id, 6_000_000, false))
            .unwrap();
        assert!(ends.lock().unwrap().is_empty());
        tx.on_tick(Time(95_000));
        assert_eq!(*ends.lock().unwrap(), [ProbeEnd::Result(6_000_000)]);
        assert_eq!(tx.stats().probes.run_verdicts, [0, 1, 0, 0]);
        // 收完了但测不出（速率 0）：有一半时的结果按它，没有就是超时
        let (mut tx, ends, _) = asking(ProbeKind::Periodic);
        tx.on_tick(Time(0));
        let id = tx.take_probes()[0].id;
        tx.on_feedback_datagram(Time(20_000), &probe_feedback(id, 0, true))
            .unwrap();
        assert_eq!(*ends.lock().unwrap(), [ProbeEnd::TimedOut]);
        let (mut tx, ends, _) = asking(ProbeKind::Periodic);
        tx.on_tick(Time(0));
        let id = tx.take_probes()[0].id;
        tx.on_feedback_datagram(Time(20_000), &probe_feedback(id, 16_000_000, false))
            .unwrap();
        tx.on_feedback_datagram(Time(40_000), &probe_feedback(id, 0, true))
            .unwrap();
        assert_eq!(*ends.lock().unwrap(), [ProbeEnd::Result(16_000_000)]);
        // 往返长：时限跟着长
        let (mut tx, ends, _) = asking(ProbeKind::Periodic);
        tx.rtt_us = 300_000;
        tx.on_tick(Time(0));
        tx.take_probes();
        tx.on_tick(Time(390_000));
        assert!(ends.lock().unwrap().is_empty());
        tx.on_tick(Time(395_000));
        assert_eq!(*ends.lock().unwrap(), [ProbeEnd::TimedOut]);
        // 待发：关键帧没确认，不探；200 ms 还不能探就中止，一个包都没发
        let (mut tx, ends, _) = asking(ProbeKind::Verify);
        tx.on_frame(Time(0), &Screen::new(64, 64, 1), true, 10_000)
            .unwrap();
        tx.on_tick(Time(10_000));
        tx.on_tick(Time(200_000));
        assert!(tx.take_probes().is_empty());
        assert!(ends.lock().unwrap().is_empty());
        tx.on_tick(Time(210_000));
        assert_eq!(*ends.lock().unwrap(), [ProbeEnd::Aborted("关键帧还没确认")]);
        assert!(tx.take_probes().is_empty());
        let p = tx.stats().probes;
        assert_eq!((p.run_sent, p.run_bytes), (0, 0));
        assert_eq!(p.run_verdicts, [0, 0, 0, 1]);
        // 待发时等到了能探的时机（另一组探测发完了）：发
        let (mut tx, ends, _) = asking(ProbeKind::Verify);
        tx.probing = Some(1);
        tx.on_tick(Time(10_000));
        assert!(tx.take_probes().is_empty());
        tx.probing = None;
        tx.on_tick(Time(60_000));
        assert_eq!(tx.take_probes().len(), 1);
        assert!(ends.lock().unwrap().is_empty());
    }

    #[test]
    fn a_run_probe_is_aborted_by_overload_or_a_network_limit() {
        // 探测期间出现网络受限的原因（这里是节奏器里上一帧没发完）：中止，结果不用
        let (mut tx, ends, state) = asking(ProbeKind::Periodic);
        tx.on_tick(Time(0));
        let id = tx.take_probes()[0].id;
        assert!(!tx.frame_gate(Time(40_000), true, true));
        assert_eq!(
            *ends.lock().unwrap(),
            [ProbeEnd::Aborted("该编帧时在途数据满了或上一帧没发完")]
        );
        assert_eq!(*state.lock().unwrap(), RateState::Tracking);
        tx.on_feedback_datagram(Time(50_000), &probe_feedback(id, 19_000_000, true))
            .unwrap();
        assert_eq!(ends.lock().unwrap().len(), 1, "中止之后到的结果不认");
        assert_eq!(tx.stats().probes.run_verdicts, [0, 0, 0, 1]);
        // 探测期间估计器因为过载降了码率、离开了要探的状态：中止（I1）
        for (kind, moved_to) in [
            (ProbeKind::Verify, RateState::DrainApp),
            (ProbeKind::Periodic, RateState::DrainApp),
        ] {
            let (mut tx, ends, state) = asking(kind);
            tx.on_tick(Time(0));
            assert_eq!(tx.take_probes().len(), 1);
            tx.observe(Time(20_000), &Feedback::default(), Extension::default());
            assert!(ends.lock().unwrap().is_empty(), "状态没变，接着等");
            *state.lock().unwrap() = moved_to;
            tx.observe(Time(40_000), &Feedback::default(), Extension::default());
            assert_eq!(
                *ends.lock().unwrap(),
                [ProbeEnd::Aborted("出现过载，降了码率")]
            );
        }
        // 不能探的各种原因
        let (mut tx, _, _) = asking(ProbeKind::Periodic);
        assert_eq!(tx.probe_blocked(Time(0)), None);
        tx.pacer_busy = true;
        assert_eq!(tx.probe_blocked(Time(0)), Some("上一帧还没发完"));
        tx.pacer_busy = false;
        let fb = |queue: u32, loss: u16| Feedback {
            queue_delay_us: queue,
            loss_permille: loss,
            ..Feedback::default()
        };
        tx.observe(Time(0), &fb(0, 20), Extension::default());
        assert_eq!(tx.probe_blocked(Time(0)), Some("有丢包"));
        tx.observe(Time(0), &fb(0, 0), Extension::default());
        assert_eq!(tx.probe_blocked(Time(0)), None);
        tx.probing = Some(1);
        assert_eq!(tx.probe_blocked(Time(0)), Some("另一组探测还没完"));
        tx.probing = None;
        // 在途数据满了（不是关键帧）：一个大的增量帧还没确认
        tx.unacked.push_back((7, Time(0), 10_000_000));
        tx.last_key = 1;
        assert_eq!(tx.probe_blocked(Time(0)), Some("在途数据满了"));
    }

    /// 对端是老版本（反馈里探测结果不带组号）：不做运行中探测，反馈按老格式解，行为和以前一样。
    #[test]
    fn no_run_probes_with_an_old_peer_or_during_the_start_probe() {
        let (mut tx, ends, _) = asking(ProbeKind::Periodic);
        tx.set_probe_ids(false);
        for i in 0..400u64 {
            tx.on_tick(Time(i * 25_000));
            assert!(tx.take_probes().is_empty());
        }
        assert!(ends.lock().unwrap().is_empty());
        assert_eq!(tx.stats().probes, ProbeStats::default());
        // 老格式的反馈照常解；带组号格式的（多 11 字节）在老格式下是坏包
        let old = encode_feedback(&Feedback::default(), &Extension::default(), &[(1, 0)]);
        assert!(tx.on_feedback_datagram(Time(0), &old).is_ok());
        assert!(tx
            .on_feedback_datagram(Time(0), &probe_feedback(1, 1_000_000, true))
            .is_err());
        // 用到达报告的会话、起步探测还没结束时：也不问
        let (mut tx, ends, _) = asking(ProbeKind::Periodic);
        tx.reports = true;
        tx.on_tick(Time(0));
        tx.reports = false;
        tx.start_probe = Some(StartProbe {
            started: Some(Time(0)),
            id: START_PROBE_ID_BASE,
            target_bps: 0,
            sent_at: Time(0),
            prev_target_bps: 0,
            best_bps: 0,
            round_bps: 0,
        });
        tx.on_tick(Time(25_000));
        assert!(tx.take_probes().is_empty());
        assert!(ends.lock().unwrap().is_empty());
        // 接收端：没协商组号时反馈是老格式；协商了、有探测结果时在最后带上
        let mut rx = MediaReceiver::new(Time::ZERO, 500_000, Box::new(FakeDecoder::default()));
        let mut body = vec![0u8; 1200];
        body[..4].copy_from_slice(&(RUN_PROBE_ID_BASE | 9).to_le_bytes());
        body[8..12].copy_from_slice(&3u32.to_le_bytes());
        let feed = |rx: &mut MediaReceiver, from: u64| {
            for i in 0..3u64 {
                rx.on_datagram(
                    Time(from + 10_000 + i * 2_000),
                    Time(from + i * 2_000),
                    u32::try_from(i).unwrap(),
                    FLAG_PADDING,
                    &body,
                )
                .unwrap();
            }
        };
        feed(&mut rx, 0);
        let plain = rx.feedback_datagram(Time(20_000));
        assert_eq!(plain.len(), Feedback::LEN + Extension::LEN);
        rx.set_probe_ids(true);
        feed(&mut rx, 100_000);
        let tagged = rx.feedback_datagram(Time(120_000));
        let (_, probe) = decode_feedback_ids(&tagged).unwrap();
        let probe = probe.unwrap();
        assert_eq!(
            (probe.id, probe.packets, probe.done),
            (RUN_PROBE_ID_BASE | 9, 3, true)
        );
        assert!(probe.bytes_per_s > 0);
        // 起步、静止探测的号段不带组号结果（反馈和老格式一样长）
        body[..4].copy_from_slice(&(IDLE_PROBE_ID_BASE | 9).to_le_bytes());
        for i in 0..3u64 {
            rx.on_datagram(
                Time(210_000 + i * 2_000),
                Time(200_000 + i * 2_000),
                0,
                FLAG_PADDING,
                &body,
            )
            .unwrap();
        }
        assert_eq!(
            rx.feedback_datagram(Time(220_000)).len(),
            Feedback::LEN + Extension::LEN
        );
        // 没有新的探测结果：和老格式逐字节一样长
        assert_eq!(
            rx.feedback_datagram(Time(140_000)).len(),
            Feedback::LEN + Extension::LEN
        );
    }

    /// “这一帧还活着”和“要重传”是两个信号：接收端报进度，发送端按进度增加刷新这一帧的存活时间，
    /// 进度标记不当成重传请求；对端是老版本时不发进度。
    #[test]
    fn progress_marks_keep_a_frame_alive_without_asking_for_retransmits() {
        let screen = Screen::new(64, 64, 1);
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.set_probe_ids(true);
        let key = tx.on_frame(Time(0), &screen, true, 10_000).unwrap();
        assert!(key.len() > 3);
        for (i, o) in key.iter().enumerate() {
            tx.on_packet_sent(Time(0), u32::try_from(i).unwrap(), o);
        }
        // 接收端：到了两片，报进度（新格式）
        let mut rx = MediaReceiver::new(Time::ZERO, 500_000, Box::new(FakeDecoder::default()));
        rx.set_probe_ids(true);
        for (i, o) in key.iter().take(2).enumerate() {
            rx.on_datagram(
                Time(5_000),
                Time(0),
                u32::try_from(i).unwrap(),
                o.flags,
                &o.body,
            )
            .unwrap();
        }
        let fb = rx.feedback_datagram(Time(6_000));
        let ((_, _, list), _) = decode_feedback_ids(&fb).unwrap();
        assert_eq!(
            list,
            vec![(1, PROGRESS | 2)],
            "还没到 10 ms：只有进度，没有重传请求"
        );
        // 发送端：进度不是重传请求，不重传；这一帧的存活时间刷新了
        let again = tx.on_feedback_datagram(Time(300_000), &fb).unwrap();
        assert!(again.is_empty());
        assert_eq!(tx.stats().nack_requests, 0);
        assert_eq!(
            tx.unacked.front().map(|(id, at, _)| (*id, *at)),
            Some((1, Time(300_000)))
        );
        // 同样的进度再报一次（没有新分片到）：不算进展，不刷新
        tx.on_feedback_datagram(Time(400_000), &fb).unwrap();
        assert_eq!(
            tx.unacked.front().map(|(_, at, _)| *at),
            Some(Time(300_000))
        );
        // 过了 10 ms：进度排在前面，后面是缺的分片（照旧全要）
        let fb = rx.feedback_datagram(Time(20_000));
        let ((_, _, list), _) = decode_feedback_ids(&fb).unwrap();
        assert_eq!(list[0], (1, PROGRESS | 2));
        assert_eq!(
            list.len(),
            1 + key.iter().filter(|o| o.flags & FLAG_PARITY == 0).count() - 2
        );
        assert!(list[1..].iter().all(|(_, frag)| frag & PROGRESS == 0));
        // 对端是老版本：不发进度
        let mut old = MediaReceiver::new(Time::ZERO, 500_000, Box::new(FakeDecoder::default()));
        old.on_datagram(Time(5_000), Time(0), 0, key[0].flags, &key[0].body)
            .unwrap();
        let (_, _, list) = decode_feedback(&old.feedback_datagram(Time(6_000))).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn feedback_with_a_tagged_probe_result_round_trips() {
        let fb = Feedback {
            floor: 3,
            delivered: 5,
            queue_delay_us: 7,
            loss_permille: 9,
            recv_bytes_per_s: 11,
        };
        let ext = Extension {
            trend_tenth_ms: -4,
            groups: 6,
            train_bytes_per_s: 8,
        };
        let report = ProbeReport {
            id: RUN_PROBE_ID_BASE | 5,
            bytes_per_s: 123_456,
            packets: 62,
            done: true,
        };
        let many: Vec<(u32, u16)> = (0..MAX_NACKS)
            .map(|i| (9, u16::try_from(i).unwrap()))
            .collect();
        for nacks in [&[][..], &[(1, 2)][..], &[(1, 2), (3, 4)][..], &many[..]] {
            // 没有探测结果：和老格式逐字节相同
            let plain = encode_feedback_ids(&fb, &ext, nacks, None);
            assert_eq!(plain, encode_feedback(&fb, &ext, nacks));
            assert_eq!(
                decode_feedback_ids(&plain).unwrap(),
                ((fb, ext, nacks.to_vec()), None)
            );
            // 有探测结果：最后多 11 字节
            let tagged = encode_feedback_ids(&fb, &ext, nacks, Some(&report));
            assert_eq!(tagged.len(), plain.len() + ProbeReport::LEN);
            assert_eq!(
                decode_feedback_ids(&tagged).unwrap(),
                ((fb, ext, nacks.to_vec()), Some(report))
            );
            // 少一个字节就是坏包
            assert!(decode_feedback_ids(&tagged[..tagged.len() - 1]).is_err());
        }
    }

    /// 静止探测也是每组必有结论：证实、否定、超时（3 秒）、中止（又开始发帧）。
    #[test]
    fn every_idle_probe_ends_with_a_verdict() {
        let train = |bps: u64| Extension {
            train_bytes_per_s: u32::try_from(bps / 8).unwrap(),
            ..Extension::default()
        };
        let screen = Screen::new(64, 64, 1);
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.set_idle_probe(true);
        tx.on_frame(Time(0), &screen, true, 10_000).unwrap();
        // 第一组（2 秒）：测得的容量够，证实
        tx.on_tick(Time(2_000_000));
        assert_eq!(tx.take_probes().len(), 1);
        tx.observe(Time(2_050_000), &Feedback::default(), train(3_000_000));
        // 第二组（6 秒）：3 秒没有结果，超时；之后迟到的包串速率不当成探测结果
        tx.on_tick(Time(6_000_000));
        assert_eq!(tx.take_probes().len(), 1);
        tx.on_tick(Time(8_975_000));
        assert_eq!(tx.stats().probes.idle_verdicts, [1, 0, 0, 0]);
        tx.on_tick(Time(9_000_000));
        assert_eq!(tx.stats().probes.idle_verdicts, [1, 0, 1, 0]);
        // 超时之后才到的结果照旧认（行为和没有时限时一样），但不再算一次结论
        tx.observe(Time(9_100_000), &Feedback::default(), train(3_000_000));
        assert_eq!(tx.stats().probes.idle_verdicts, [1, 0, 1, 0]);
        // 第三组（14 秒）：结果没回来就又开始发帧，中止
        tx.on_tick(Time(14_000_000));
        assert_eq!(tx.take_probes().len(), 1);
        tx.on_frame(Time(14_100_000), &Screen::new(64, 64, 2), true, 10_000)
            .unwrap();
        // 第四组：测得的容量不到码率的一半，否定（降）
        tx.on_tick(Time(16_100_000));
        assert_eq!(tx.take_probes().len(), 1);
        tx.observe(Time(16_150_000), &Feedback::default(), train(320_000));
        assert_eq!(tx.rate_bps(), 288_000);
        let p = tx.stats().probes;
        assert_eq!(p.idle_sent, 4);
        assert_eq!(p.idle_verdicts, [1, 1, 1, 1]);
        let events = tx.take_events().join("\n");
        for text in [
            "静止探测结论 证实：测得 3000 kbps，链路没有明显变差，码率不变",
            "静止探测结论 超时：到时间没有结果",
            "静止探测（已按超时下过结论）之后：测得 3000 kbps",
            "静止探测结论 中止：又开始发帧了，结果不用",
            "静止探测结论 否定：链路变差了，码率按测得的容量降",
        ] {
            assert!(events.contains(text), "{text}\n{events}");
        }
        // 运行中探测在进行时，静止探测先等着（两组不同时发）
        let (mut tx, ends, _) = asking(ProbeKind::Periodic);
        tx.set_idle_probe(true);
        tx.next_probe = Some(Time(0));
        tx.on_tick(Time(0));
        let first = tx.take_probes();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].id & RUN_PROBE_ID_BASE, RUN_PROBE_ID_BASE);
        tx.on_tick(Time(95_000));
        assert_eq!(*ends.lock().unwrap(), [ProbeEnd::TimedOut]);
        let second = tx.take_probes();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].id & IDLE_PROBE_ID_BASE, IDLE_PROBE_ID_BASE);
    }

    /// 起步有时间上限：第一个关键帧 2 秒还没确认就不探了，状态回跟踪，写明原因。
    #[test]
    fn startup_ends_after_two_seconds_without_the_first_keyframe() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        tx.set_start_probe(true);
        tx.on_tick(Time(100_000));
        tx.on_frame(Time(100_000), &Screen::new(64, 64, 1), true, 10_000)
            .unwrap();
        tx.on_tick(Time(2_075_000));
        assert!(tx.start_probing());
        assert_eq!(tx.rate_state(), RateState::Startup);
        tx.on_tick(Time(2_100_000));
        assert!(!tx.start_probing());
        assert_eq!(tx.rate_state(), RateState::Tracking);
        assert!(tx.take_events().join("\n").contains(
            "码率状态 起步 → 跟踪：不做起步探测：第一个关键帧 2 秒还没确认，起步到时限了"
        ));
        assert!(tx.take_probes().is_empty());
    }

    #[test]
    fn probe_bytes_are_counted_by_kind() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let probe = |id: u32| Outgoing {
            flags: FLAG_PADDING,
            body: vec![0u8; 1_000],
            kind: PacketKind::Probe(id),
            fragment: None,
        };
        let size = u64::try_from(1_000 + HEADER_LEN).unwrap();
        tx.on_packet_sent(Time(0), 0, &probe(START_PROBE_ID_BASE | 1));
        tx.on_packet_sent(Time(0), 1, &probe(IDLE_PROBE_ID_BASE | 1));
        tx.on_packet_sent(Time(0), 2, &probe(IDLE_PROBE_ID_BASE | 2));
        for seq in 3..6 {
            tx.on_packet_sent(Time(0), seq, &probe(RUN_PROBE_ID_BASE | 1));
        }
        let p = tx.stats().probes;
        assert_eq!(
            (p.start_bytes, p.idle_bytes, p.run_bytes),
            (size, 2 * size, 3 * size)
        );
        assert_eq!(p.bytes(), 6 * size);
        let mut total = p;
        total.run_sent = 2;
        total.run_verdicts = [1, 0, 1, 0];
        total.merge(&total.clone());
        assert_eq!(total.bytes(), 12 * size);
        assert_eq!((total.run_sent, total.run_verdicts), (4, [2, 0, 2, 0]));
        let line = total.line(1_000_000);
        assert!(
            line.starts_with(&format!(
                "探测流量 {} B（起步 {}、静止 {}、运行中 {}），占全部流量 1.22%；运行中探测发出 4 轮（核实 0、周期 0），\
                 结论：证实 2、否定 0、超时 2、中止 0；静止探测 0 组",
                12 * size,
                2 * size,
                4 * size,
                6 * size
            )),
            "{line}"
        );
    }

    /// 起步探测：第一个关键帧送得够快才探；按 4 倍逐组往上探，探不上去就停，码率定为测得容量的九成。
    #[test]
    fn the_start_probe_climbs_until_the_link_saturates_after_a_fast_first_keyframe() {
        let train = |bps: u64| Extension {
            train_bytes_per_s: u32::try_from(bps / 8).unwrap(),
            ..Extension::default()
        };
        let acked = |id: u32| Feedback {
            floor: id,
            delivered: 1,
            ..Feedback::default()
        };
        let screen = Screen::new(64, 64, 1);
        // 第一个关键帧 `took` 微秒送到
        let begin = |took: u64| {
            let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
            tx.set_start_probe(true);
            assert!(tx.start_probing());
            assert!(tx.due(Time(0)), "第一幅画面照常出");
            tx.on_tick(Time(0));
            assert!(tx.take_probes().is_empty(), "关键帧送到之前不探");
            tx.on_frame(Time(0), &screen, true, 10_000).unwrap();
            tx.observe(Time(took), &acked(1), Extension::default());
            tx
        };
        let mut tx = begin(10_000);
        let start = tx.rate_bps();
        // 第一组：码率的 4 倍，匀速发 30 ms
        let first = tx.take_probes();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].target_bps, start * START_PROBE_FIRST_SCALE);
        assert_eq!(first[0].id, START_PROBE_ID_BASE | 1);
        let bytes = first[0].target_bps * START_PROBE_SPAN_US / 8_000_000;
        assert_eq!(
            first[0].packets,
            u32::try_from(bytes / 1200)
                .unwrap()
                .max(START_PROBE_MIN_PACKETS)
        );
        // 探的时候不出新帧
        assert!(!tx.due(Time(20_000)));
        // 达到目标：按结果的 2 倍接着探
        tx.observe(Time(60_000), &Feedback::default(), train(3_900_000));
        let second = tx.take_probes();
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].target_bps, 7_800_000);
        // 上一组收尾的结果晚到（不比上一组目标高、这一组的结果还不可能回来）：不下结论
        tx.observe(Time(70_000), &Feedback::default(), train(3_800_000));
        assert!(tx.start_probing());
        assert!(tx.take_probes().is_empty());
        // 这一组的结果：只到 5 Mbps（不到七成）→ 探完，码率定为九成，又可以出帧了
        tx.observe(Time(150_000), &Feedback::default(), train(5_000_000));
        assert!(!tx.start_probing());
        assert!(tx.take_probes().is_empty());
        assert_eq!(tx.rate_bps(), 4_500_000);
        assert!(tx.due(Time(150_000)));
        assert!(tx
            .take_events()
            .iter()
            .any(|e| e.contains("起步探测") && e.contains("5000 kbps")));
        // 结果比上一组目标高：一定是这一组的，马上算；比这一组目标还高的（帧的包串）按目标算
        let mut quick = begin(10_000);
        quick.observe(Time(40_000), &Feedback::default(), train(4_000_000));
        quick.take_probes();
        quick.observe(Time(50_000), &Feedback::default(), train(5_000_000));
        assert!(!quick.start_probing());
        assert_eq!(quick.rate_bps(), 4_500_000);
        let mut capped = begin(10_000);
        capped.observe(Time(40_000), &Feedback::default(), train(4_000_000));
        let target = capped.take_probes()[0].target_bps;
        capped.observe(Time(500_000), &Feedback::default(), train(target * 10));
        assert!(capped.take_probes()[0].target_bps <= cfg().max_bps);
        // 1 秒还没探完：按测到的定；什么都没测到：码率不动，写明
        // 一直没有结果：这一组的结果该回来的时候（往返 + 30 ms + 25 ms + 20 ms）就结束，码率不动，写明
        let mut slow = begin(10_000);
        slow.on_tick(Time(60_000));
        assert!(slow.start_probing());
        slow.on_tick(Time(120_000));
        assert!(!slow.start_probing());
        assert_eq!(slow.rate_bps(), start);
        assert!(slow.take_events().iter().any(|e| e.contains("没测到结果")));
        // 结果一直在来、一直达标（每组都按结果的 2 倍接着探）：最多 1 秒
        let mut long = begin(10_000);
        let mut t = 10_000;
        while long.start_probing() && t < 2_000_000 {
            t += 20_000;
            let target = long.take_probes().first().map(|c| c.target_bps);
            if let Some(target) = target {
                long.observe(Time(t), &Feedback::default(), train(target.min(2_000_000)));
            }
            long.on_tick(Time(t));
        }
        assert!(t <= 1_100_000, "{t}");
        // 上一组收尾的结果晚到、这一组再没有结果：到时间按这一轮收到的判（不干等到 1 秒）
        let mut late = begin(10_000);
        late.observe(Time(40_000), &Feedback::default(), train(4_000_000));
        let target = late.take_probes()[0].target_bps;
        late.observe(Time(50_000), &Feedback::default(), train(3_500_000));
        assert!(late.start_probing());
        late.on_tick(Time(40_000 + 90_000));
        assert!(
            !late.start_probing(),
            "目标 {target}，这一轮最高 3.5 Mbps 不到七成"
        );
        assert_eq!(late.rate_bps(), 3_600_000);
        // 第一个关键帧送得比起步码率还慢（窄带）：不探
        let mut narrow = begin(5_000_000);
        assert!(!narrow.start_probing());
        assert!(narrow.take_probes().is_empty());
        assert!(narrow
            .take_events()
            .iter()
            .any(|e| e.contains("不做起步探测")));
        // 开头的反馈有明显丢包：不探
        let mut lossy = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        lossy.set_start_probe(true);
        lossy.on_frame(Time(0), &screen, true, 10_000).unwrap();
        let loss = Feedback {
            loss_permille: 50,
            ..Feedback::default()
        };
        lossy.observe(Time(5_000), &loss, Extension::default());
        assert!(!lossy.start_probing());
        assert!(lossy.take_events().iter().any(|e| e.contains("有丢包")));
        // 对端不支持：不探
        let mut off = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        off.set_start_probe(false);
        assert!(!off.start_probing());
    }

    #[test]
    fn the_rate_goes_stale_after_a_quiet_spell() {
        let mut tx = MediaSender::detached(cfg(), Codec::H264);
        // 还没被证实过（会话开头）：不算过期
        assert!(!tx.start_frame(Time(0), 0).rate_stale);
        assert!(!tx.start_frame(Time(5_000_000), 0).rate_stale);
        // 送达速率只有估计的一成：不算证实
        let rate = u32::try_from(tx.rate_bps() / 8).unwrap();
        let fb = |recv| Feedback {
            recv_bytes_per_s: recv,
            ..Feedback::default()
        };
        tx.on_feedback(Time(5_100_000), &fb(rate / 10));
        assert!(!tx.start_frame(Time(9_000_000), 0).rate_stale);
        // 送达速率到了估计的一半：证实；之后 2 秒内还新鲜，超过就过期
        tx.on_feedback(Time(9_100_000), &fb(rate));
        assert!(!tx.start_frame(Time(11_000_000), 0).rate_stale);
        assert!(tx.start_frame(Time(11_200_000), 0).rate_stale);
        // 包串测到速率也算证实
        tx.observe(
            Time(11_300_000),
            &fb(0),
            Extension {
                train_bytes_per_s: 100_000,
                ..Extension::default()
            },
        );
        assert!(!tx.start_frame(Time(11_400_000), 0).rate_stale);
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

    /// 带到达报告的反馈格式（发送端用网络控制器时）：反馈、重传请求、到达报告原样来回，坏数据报被拒绝。
    #[test]
    fn feedback_with_an_arrival_report_round_trips_and_rejects_garbage() {
        let fb = Feedback {
            floor: 9,
            delivered: 5,
            queue_delay_us: 3_000,
            loss_permille: 12,
            recv_bytes_per_s: 700_000,
        };
        let report = ArrivalReport {
            base_seq: 100,
            arrivals: vec![
                Some(Time(5_000)),
                None,
                Some(Time(5_250)),
                Some(Time(9_000)),
            ],
        };
        let nacks = [(7u32, 1u16), (7, 2)];
        let buf = encode_feedback_v2(&fb, &nacks, Some(&report));
        let (f, n, r) = decode_feedback_v2(&buf).unwrap();
        assert_eq!((f, n, r), (fb, nacks.to_vec(), Some(report)));
        // 没有报告（接收端还没收到过包）：最后一个字节是 0，解出来是 None
        let buf = encode_feedback_v2(&fb, &[], None);
        assert_eq!(buf.last(), Some(&0));
        assert_eq!(decode_feedback_v2(&buf).unwrap(), (fb, vec![], None));
        // 重传请求最多编 MAX_NACKS 个，多的丢掉
        let many: Vec<(u32, u16)> = (0..u32::try_from(MAX_NACKS).unwrap() + 5)
            .map(|i| (i, 0))
            .collect();
        let (_, n, _) = decode_feedback_v2(&encode_feedback_v2(&fb, &many, None)).unwrap();
        assert_eq!(n.len(), MAX_NACKS);
        // 伪造：声称有比上限还多的重传请求
        let mut forged = encode_feedback_v2(&fb, &[], None);
        forged[Feedback::LEN] = u8::try_from(MAX_NACKS + 1).unwrap();
        assert!(matches!(
            decode_feedback_v2(&forged),
            Err(Error::TooManyItems { .. })
        ));
        // 截断、多余的尾巴
        assert!(decode_feedback_v2(&buf[..3]).is_err());
        assert!(decode_feedback_v2(&buf[..buf.len() - 1]).is_err());
        let mut long = buf;
        long.push(0);
        assert!(
            decode_feedback_v2(&long).is_err(),
            "多出来的字节不能悄悄吞掉"
        );
    }

    /// 到达报告走通整条链：发送端记发送历史 → 接收端记到达 → 反馈带报告 → 发送端对账交给网络控制器。
    #[test]
    fn arrival_reports_flow_from_the_receiver_to_the_gcc_sender() {
        // 估计器不用报告时，打开也没用（握手协商了也一样）
        let mut classic = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        classic.set_reports(true);
        assert!(!classic.reports());
        let mut tx = MediaSender::new(
            SenderConfig {
                estimator: EstimatorKind::GccV1,
                ..cfg()
            },
            Box::new(FakeEncoder::default()),
        );
        let mut rx = MediaReceiver::new(Time::ZERO, 250_000, Box::new(FakeDecoder::default()));
        assert!(!tx.reports());
        tx.set_reports(true);
        rx.set_reports(true);
        assert!(tx.reports());
        assert_eq!(tx.estimator(), "gcc-v1");
        let mut seq = 0u32;
        let mut last_fb = Vec::new();
        for i in 0..40u32 {
            let t = Time(u64::from(i) * 33_000);
            let out = tx
                .on_frame(t, &Screen::new(32, 32, i + 1), true, 10_000)
                .unwrap();
            for o in &out {
                tx.on_packet_sent(t, seq, o);
                rx.on_datagram(t.plus(20_000), t, seq, o.flags, &o.body)
                    .unwrap();
                seq += 1;
            }
            tx.on_tick(t);
            last_fb = rx.feedback_datagram(t.plus(25_000));
            let resend = tx.on_feedback_datagram(t.plus(45_000), &last_fb).unwrap();
            assert!(resend.is_empty(), "什么都没丢，不用重传");
        }
        assert_eq!(rx.stats().decoded, 40);
        assert_eq!(tx.stats().frames, 40);
        // 反馈里确实带着报告，报的是最近收到的包
        let (_, _, report) = decode_feedback_v2(&last_fb).unwrap();
        let report = report.expect("接收端收到过包，反馈里应当有报告");
        assert!(!report.arrivals.is_empty());
        assert!(report.arrivals.iter().all(Option::is_some), "没丢包");
        // 发送端的网络控制器收到了对账后的报告：状态行里有“网络控制”，码率没有掉到底线
        let line = tx.controller_line().unwrap();
        assert!(line.contains("网络控制 目标"), "{line}");
        assert!(tx.rate_bps() >= cfg().start_bps, "{}", tx.rate_bps());
        // 会话开头没有应用受限那么久：没有要发的探测
        assert!(tx.take_probes().is_empty());
        // 坏反馈被拒绝，不影响后面的
        assert!(tx
            .on_feedback_datagram(Time(2_000_000), &[1, 2, 3])
            .is_err());
    }

    /// 一个数据报让好几帧一起能解（参考帧补齐了，等它的帧跟着交出）时，前一帧解失败，后面接在它后面的帧
    /// 不交给解码器（解码器会因为参考帧不对而出错），算跳过，发送端据此补关键帧。
    #[test]
    fn a_failed_decode_skips_the_frames_that_were_already_waiting_behind_it() {
        let mut tx = MediaSender::new(
            SenderConfig {
                max_payload: 40,
                ..cfg()
            },
            Box::new(FakeEncoder::default()),
        );
        tx.set_profile(Profile {
            fec_permille: 0,
            ..Profile::STABLE
        });
        let mut rx = MediaReceiver::new(Time::ZERO, 1_000_000, Box::new(FakeDecoder::default()));
        rx.set_keyframe_grace(true);
        let mut seq = 0u32;
        let mut give = |rx: &mut MediaReceiver, t: Time, o: &Outgoing| {
            seq += 1;
            rx.on_datagram(t, t, seq, o.flags, &o.body)
                .unwrap()
                .map(|s| s.frame_id)
        };
        let t = Time(1_000);
        let key = tx.on_frame(t, &Screen::new(8, 8, 1), true, 0).unwrap();
        assert_eq!(
            key.iter().filter_map(|o| give(&mut rx, t, o)).last(),
            Some(1)
        );
        // 第 2 帧（解码器会报错）有好几个分片，先丢掉第一个；第 3 帧整帧到齐，但要等第 2 帧
        let bad = tx
            .on_frame(t, &Screen::new(8, 8, 0x00de_ad00), true, 0)
            .unwrap();
        assert!(bad.len() >= 2, "第 2 帧要有好几个分片：{}", bad.len());
        let next = tx.on_frame(t, &Screen::new(8, 8, 3), true, 0).unwrap();
        for o in bad.iter().skip(1) {
            assert_eq!(give(&mut rx, t, o), None);
        }
        for o in &next {
            assert_eq!(give(&mut rx, t, o), None, "第 2 帧没齐，第 3 帧不能先解");
        }
        assert_eq!(rx.stats().bad, 0);
        // 第 2 帧最后一个分片到了：它解失败，第 3 帧跟着跳过，什么都不显示
        assert_eq!(give(&mut rx, t, &bad[0]), None);
        assert_eq!(rx.stats().bad, 1, "第 2 帧解码失败");
        assert_eq!(rx.stats().skipped, 1, "第 3 帧跟着跳过");
        assert_eq!(rx.stats().decoded, 1, "只有第 1 帧显示过");
        // 反馈里只有第 1 帧送达（位 0 是 floor 那一帧，往后依次是更早的帧），发送端据此补关键帧
        let fb = rx.feedback(t.plus(1));
        assert_eq!(fb.floor, 3);
        assert_eq!(fb.delivered & 0b111, 0b100);
    }

    /// 编码器：第 1、3 帧是关键帧，别的是增量帧；记下通知它丢了哪些帧。
    #[derive(Debug, Default)]
    struct KeyOnOddFrames {
        encoded: u32,
        lost: Arc<Mutex<Vec<u32>>>,
    }

    impl VideoEncoder for KeyOnOddFrames {
        fn codec(&self) -> Codec {
            Codec::H264
        }
        fn encode(
            &mut self,
            _id: u32,
            s: &Screen,
            _changed: bool,
            _budget: usize,
        ) -> Result<Option<EncodedFrame>, Error> {
            self.encoded += 1;
            let key = self.encoded % 2 == 1;
            let header = FrameHeader {
                codec: Codec::H264,
                key,
                width: s.width(),
                height: s.height(),
            };
            Ok(Some(EncodedFrame {
                payload: header.frame(&[7u8; 10]),
                key,
            }))
        }
        fn on_feedback(&mut self, _floor: u32, _mask: u32) {}
        fn on_lost(&mut self, frame_id: u32) {
            self.lost.lock().unwrap().push(frame_id);
        }
        fn wants_refine(&self) -> bool {
            false
        }
    }

    /// 最近一个关键帧之前的帧丢了没关系（接收端从那个关键帧开始就能解）：不算丢，不通知编码器、不补关键帧。
    #[test]
    fn frames_before_the_latest_keyframe_are_not_counted_as_lost() {
        let lost = Arc::new(Mutex::new(Vec::new()));
        let encoder = KeyOnOddFrames {
            lost: lost.clone(),
            ..KeyOnOddFrames::default()
        };
        let mut tx = MediaSender::new(cfg(), Box::new(encoder));
        for i in 0..3u32 {
            let t = Time(u64::from(i) * 10_000);
            let out = tx
                .on_frame(t, &Screen::new(16, 16, i), true, 10_000)
                .unwrap();
            assert!(!out.is_empty());
        }
        assert_eq!(tx.stats().keyframes, 2, "第 1、3 帧是关键帧");
        // 三帧都一直没有反馈：都超时。第 1、2 帧在第 3 帧（关键帧）之前，只有第 3 帧算丢
        tx.expire_unacked(Time(60_000_000));
        assert_eq!(tx.stats().lost, 1);
        assert_eq!(*lost.lock().unwrap(), vec![3]);
    }

    /// 节奏器里的包还要不要发：帧还没有定论就要；送到了（或接收端已经放弃）就不发；探测的填充包不属于哪一帧，照发。
    #[test]
    fn a_queued_packet_is_dropped_once_its_frame_has_a_verdict() {
        let mut tx = MediaSender::new(cfg(), Box::new(FakeEncoder::default()));
        let mut rx = MediaReceiver::new(Time::ZERO, 100_000, Box::new(FakeDecoder::default()));
        let t = Time(1_000);
        let out = tx
            .on_frame(t, &Screen::new(16, 16, 1), true, 10_000)
            .unwrap();
        let first = out.first().unwrap().clone();
        assert_eq!(first.fragment.map(|(frame, _)| frame), Some(1));
        assert!(tx.wanted(&first), "第 1 帧还没确认");
        deliver(&mut rx, t, &out, |_| false);
        let fb = rx.feedback(t.plus(1));
        tx.on_feedback(t.plus(1), &fb);
        assert!(!tx.wanted(&first), "第 1 帧送到了，排队的包不用发了");
        let padding = Outgoing {
            fragment: None,
            ..first
        };
        assert!(tx.wanted(&padding), "探测的填充包不属于哪一帧");
    }

    /// 发送端把码率估计交给估计器、画像交给估计器：换画像、报名字都要转给它。
    #[test]
    fn the_sender_reports_the_estimator_name_and_hands_it_the_profile() {
        let (mut tx, _, _) = asking(ProbeKind::Verify);
        assert_eq!(tx.estimator(), "asker");
        assert_eq!(tx.profile_name(), Profile::STABLE.name);
        tx.set_profile(Profile::WIRELESS);
        assert_eq!(tx.profile_name(), Profile::WIRELESS.name);
    }
}
