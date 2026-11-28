//! 带宽估计（传输算法 v1）。
//!
//! 设计依据见 `docs/research/README.md` 第 3 节和 `docs/research/A-transport.md`。分两半：
//!
//! **接收端测量**（[`ReceiverMetrics`]），随反馈发回（[`Extension`]）：
//! - **延迟趋势**：按帧把包分组（一帧的包是连续发出的），算相邻两组“到达间隔 − 发送间隔”，
//!   累加后对时间做线性回归，斜率为正说明排队在增长。这是 WebRTC 趋势线估计的做法，
//!   能在延迟还不“高”的时候就发现拥塞正在形成。
//! - **包串速率**：一帧的包背靠背发出，到达端测“字节 ÷ 首尾到达间隔”，就是瓶颈带宽的估计。
//!   大帧（切窗口、滚动）因此成了免费的带宽探测，不需要额外的探测包。
//!
//! **发送端决策**（[`RateEstimator`] 的实现）：
//! - [`Legacy`]：原来的“延迟高就降、用满才升”，保留作对照；
//! - [`ClassicV1`]：趋势线过载检测（自适应门限）+ 包串测得的容量 + 区分随机丢包和拥塞丢包。
//!
//! 以后的 AI 模型也实现 [`RateEstimator`]，在同一套端到端测试里和经典算法比较。

use std::collections::VecDeque;

use crate::adaptive::{Feedback, Profile, RateController};
use crate::cc::probe::ProbeCluster;
use crate::cc::report::{PacketResult, SentPacket};
use crate::cc::Gcc;
use crate::error::Error;
use crate::time::Time;
use crate::wire::{Reader, Writer};

/// 趋势线回归用的最近分组数。
const TREND_WINDOW: usize = 20;
/// 累加延迟的平滑系数（千分比）。
const SMOOTHING_PERMILLE: i64 = 900;
/// 至少这么多个包的帧才用来测包串速率：包太少时间隔太短，测不准。
const MIN_TRAIN_PACKETS: u32 = 3;
/// 两份反馈相隔不超过这么久（微秒）才算不断档（控制端有新包时 20 ms 一份；静止时 1 秒一份，就断档了）。
const CONTIGUOUS_US: u64 = 60_000;
/// 包串首尾到达间隔至少这么久（微秒）才算数。接收端是应用读数据报时打的时间戳，程序忙一下、几十个包
/// 就一次读走，间隔几乎是 0（2026-11-26 用户日志：30 Mbps 的 Wi-Fi 测出 3 420 173 kbps，码率被直接拉到上限）。
/// 1 ms 时一帧 37 个包（44 KB）还能测到约 350 Mbps，足够办公场景。
const MIN_TRAIN_SPAN_US: u64 = 1_000;
/// 探测组还没收完时先报告的结果，首尾间隔至少这么久（微秒）：太短的一段被批量读包压扁就测不准
/// （按速率匀速发的组按发送间隔算，只会说“链路至少有目标速率”）。收完的组按 [`MIN_TRAIN_SPAN_US`]。
const MIN_PARTIAL_PROBE_SPAN_US: u64 = 10_000;
/// 探测组还没收完时先报告，至少要这么多个包：包少时延迟回归被抖动带偏（仿真 20 ms 抖动、5 个包时
/// 30 Mbps 的链路只测出 2.5 Mbps）。
const MIN_PARTIAL_PROBE_PACKETS: u32 = 10;

/// 接收端随反馈发回的扩展测量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Extension {
    /// 延迟趋势：回归斜率 × 分组数 × 4，单位 0.1 毫秒（与 WebRTC 的“修正趋势”同量纲，便于用它的门限）。
    pub trend_tenth_ms: i32,
    /// 参与回归的分组数。
    pub groups: u16,
    /// 这段时间里测到的最大包串速率（字节每秒），0 表示没有测到。
    pub train_bytes_per_s: u32,
}

impl Extension {
    /// 编码后的长度。
    pub const LEN: usize = 10;

    /// 编码。
    pub fn encode(&self, w: &mut Writer) {
        w.u32(u32::from_ne_bytes(self.trend_tenth_ms.to_ne_bytes()))
            .u16(self.groups)
            .u32(self.train_bytes_per_s);
    }

    /// 解码。
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            trend_tenth_ms: i32::from_ne_bytes(r.u32()?.to_ne_bytes()),
            groups: r.u16()?,
            train_bytes_per_s: r.u32()?,
        })
    }
}

/// 接收端对一组探测的测量结果，带组号随反馈带回（对端支持 [`crate::proto::Caps::PROBE_ID`] 时）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeReport {
    /// 组号。
    pub id: u32,
    /// 测得的速率（字节每秒）；0 表示测不出（包太少）。
    pub bytes_per_s: u32,
    /// 这一组收到了几个包。
    pub packets: u16,
    /// 这一组收完了（收齐，或者再没有包来）；`false` 是收到一半时先报的。
    pub done: bool,
}

impl ProbeReport {
    /// 编码后的长度。
    pub const LEN: usize = 11;

    /// 编码。
    pub fn encode(&self, w: &mut Writer) {
        w.u32(self.id)
            .u32(self.bytes_per_s)
            .u16(self.packets)
            .u8(u8::from(self.done));
    }

    /// 解码。
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            id: r.u32()?,
            bytes_per_s: r.u32()?,
            packets: r.u16()?,
            done: r.u8()? != 0,
        })
    }
}

/// 一组包（一帧）的发送和到达时间。
#[derive(Debug, Clone, Copy)]
struct Group {
    frame: u32,
    first_send: Time,
    last_send: Time,
    first_arrival: Time,
    last_arrival: Time,
    packets: u32,
    bytes: u64,
}

/// 正在收的一个探测组。
#[derive(Debug, Clone, Copy)]
struct ProbeGroup {
    id: u32,
    first_send: Time,
    last_send: Time,
    first_arrival: Time,
    last_arrival: Time,
    packets: u32,
    bytes: u64,
    /// 逐包单程延迟对发送时刻的回归累加：x = 发送时刻、d = 到达 − 发送（都相对这一组第一个收到的包，微秒）。
    /// [包数, Σx, Σd, Σx², Σxd]。
    sums: [i128; 5],
    /// 回归的原点：这一组第一个收到的包的发送、到达时刻（之后不变；`first_send` 会被更早发出的乱序包改小）。
    origin: (Time, Time),
}

impl ProbeGroup {
    fn new(id: u32, now: Time, send_time: Time, bytes: u64) -> Self {
        Self {
            id,
            first_send: send_time,
            last_send: send_time,
            first_arrival: now,
            last_arrival: now,
            packets: 1,
            bytes,
            sums: [1, 0, 0, 0, 0],
            origin: (send_time, now),
        }
    }

    fn add(&mut self, now: Time, send_time: Time, bytes: u64) {
        let signed = |t: Time| i128::from(t.micros());
        let x = signed(send_time) - signed(self.origin.0);
        let d = signed(now) - signed(self.origin.1) - x;
        let [n, sx, sd, sxx, sxd] = self.sums;
        self.sums = [n + 1, sx + x, sd + d, sxx + x * x, sxd + x * d];
        self.last_arrival = now;
        self.first_send = self.first_send.min(send_time);
        self.last_send = self.last_send.max(send_time);
        self.packets = self.packets.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
    }

    /// 测得的速率（字节每秒）。
    ///
    /// 按速率匀速发出的组（发送跨度够长）：发送速率 ÷（1 + 延迟随发送时刻上涨的斜率）。链路撑得住，排队
    /// 不涨，就是发送速率；撑不住，每发 1 秒排队多涨 s 秒，链路能力 = 发送速率 ÷（1 + s）。斜率用全组所有包
    /// 回归，随机抖动、接收端一次读走一批包都被平均掉。只按首尾到达间隔算时，30 ms 的组碰上 20 ms 的 Wi-Fi
    /// 抖动就只测出六成（仿真 30 Mbps 的链路测出 8.9 Mbps）。
    /// 突发发出的（帧的包、发送跨度太短）：首包到达之后的字节 ÷ 首尾间隔（见 [`ReceiverMetrics::train`]）。
    fn rate(&self) -> Option<u64> {
        if self.packets < MIN_TRAIN_PACKETS {
            return None;
        }
        let send_span = self.last_send.since(self.first_send);
        let after_first =
            self.bytes.saturating_mul(u64::from(self.packets - 1)) / u64::from(self.packets);
        if send_span < MIN_TRAIN_SPAN_US {
            let span = self.last_arrival.since(self.first_arrival).max(send_span);
            return (span >= MIN_TRAIN_SPAN_US)
                .then(|| after_first.saturating_mul(1_000_000) / span);
        }
        let send_rate = after_first.saturating_mul(1_000_000) / send_span;
        let [n, sx, sd, sxx, sxd] = self.sums;
        let den = n * sxx - sx * sx;
        let growth_ppm = if den > 0 {
            ((n * sxd - sx * sd) * 1_000_000 / den).max(0)
        } else {
            0
        };
        let growth = u64::try_from(growth_ppm).unwrap_or(u64::MAX);
        Some(send_rate.saturating_mul(1_000_000) / growth.saturating_add(1_000_000))
    }
}

/// 接收端测量：延迟趋势和包串速率。
#[derive(Debug, Clone, Default)]
pub struct ReceiverMetrics {
    current: Option<Group>,
    previous: Option<Group>,
    /// 累加的延迟变化（微秒）和它的平滑值。
    accumulated: i64,
    smoothed: i64,
    /// 回归用的点：(到达时间毫秒, 平滑后的累加延迟微秒)。
    points: Vec<(i64, i64)>,
    max_train: u32,
    /// 正在收的探测组（探测的填充包，见 [`crate::proto::Caps::PROBE_TRAIN`]）。
    probe: Option<ProbeGroup>,
    /// 这一组报告过结果时有几个包（结束时包数没变就不重复报告）。
    probe_reported: u32,
    /// 上次报告时这一组有几个包，和之后连着几次报告都没有新包（连着两次就结束这一组）。
    probe_seen: u32,
    probe_quiet: u32,
    /// 最近一组探测带组号的结果，等下一份反馈带走（见 [`Self::take_probe_report`]）。
    probe_report: Option<ProbeReport>,
}

impl ReceiverMetrics {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: None,
            previous: None,
            accumulated: 0,
            smoothed: 0,
            points: Vec::new(),
            max_train: 0,
            probe: None,
            probe_reported: 0,
            probe_seen: 0,
            probe_quiet: 0,
            probe_report: None,
        }
    }

    /// 取走最近一组探测带组号的结果（没有新结果时为 `None`）。在 [`Self::report`] 之后调用。
    pub const fn take_probe_report(&mut self) -> Option<ProbeReport> {
        self.probe_report.take()
    }

    fn tag_probe(&mut self, g: &ProbeGroup, done: bool) {
        self.probe_report = Some(ProbeReport {
            id: g.id,
            bytes_per_s: u32::try_from(g.rate().unwrap_or(0)).unwrap_or(u32::MAX),
            packets: u16::try_from(g.packets).unwrap_or(u16::MAX),
            done,
        });
    }

    /// 收到属于 `frame` 的一个数据报。`send_time` 是发送端打的时间戳。
    pub fn on_packet(&mut self, now: Time, send_time: Time, frame: u32, bytes: usize) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        // 旧帧的包（重传的分片）不算：它带着新的发送时间，却属于早就测过的那一组，
        // 混进来会让“到达间隔 − 发送间隔”出现很大的假负值，趋势线被带偏。
        let newest = self
            .current
            .map(|g| g.frame)
            .or(self.previous.map(|g| g.frame));
        if newest.is_some_and(|f| frame < f) {
            return;
        }
        match self.current.as_mut() {
            Some(g) if g.frame == frame => {
                g.last_arrival = now;
                g.first_send = g.first_send.min(send_time);
                g.last_send = g.last_send.max(send_time);
                g.packets = g.packets.saturating_add(1);
                g.bytes = g.bytes.saturating_add(bytes);
                return;
            }
            Some(_) | None => {}
        }
        // 新的一组：上一组结束了。
        if let Some(done) = self.current.take() {
            self.finish(done);
        }
        self.current = Some(Group {
            frame,
            first_send: send_time,
            last_send: send_time,
            first_arrival: now,
            last_arrival: now,
            packets: 1,
            bytes,
        });
    }

    /// 收到探测组 `id` 的一个填充包（`send_time` 是发送端打的时间戳，`total` 是这一组一共几个包，0 表示不知道）。
    /// 探测包不进延迟趋势，只用来测包串速率。收齐了马上结算（不等之后的报告里没有新包：控制端静止时 1 秒才报一次，
    /// 结果要晚 2 秒）。
    pub fn on_probe(&mut self, now: Time, send_time: Time, id: u32, total: u32, bytes: usize) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        match self.probe.as_mut() {
            Some(g) if g.id == id => g.add(now, send_time, bytes),
            Some(_) | None => {
                self.finish_probe();
                self.probe = Some(ProbeGroup::new(id, now, send_time, bytes));
            }
        }
        if total > 0 && self.probe.is_some_and(|g| g.packets >= total) {
            self.finish_probe();
        }
    }

    /// 结算正在收的探测组（先报告过同样多的包就不重复报告）。
    fn finish_probe(&mut self) {
        let done = self.probe.take();
        if let Some(g) = done {
            self.tag_probe(&g, true);
        }
        if let Some(rate) = done
            .filter(|g| g.packets != self.probe_reported)
            .and_then(|g| g.rate())
        {
            self.note_train(rate);
        }
        self.probe_reported = 0;
        self.probe_seen = 0;
        self.probe_quiet = 0;
    }

    /// 报告时：探测组不用等整组收完（窄带上一组要传一两百毫秒，反馈每几十毫秒一次），包够多、时间够长就先
    /// 报告，组留着、后面的包接着累计；连着两次报告都没有新包进来（窄带上两个包之间可能隔一次报告），这一组
    /// 就结束了，报告最终结果。
    fn report_probe(&mut self) {
        let Some(g) = self.probe else {
            return;
        };
        if g.packets == self.probe_seen {
            self.probe_quiet = self.probe_quiet.saturating_add(1);
            if self.probe_quiet >= 2 {
                self.finish_probe();
            }
            return;
        }
        self.probe_seen = g.packets;
        self.probe_quiet = 0;
        if g.packets >= MIN_PARTIAL_PROBE_PACKETS
            && g.last_arrival.since(g.first_arrival) >= MIN_PARTIAL_PROBE_SPAN_US
        {
            if let Some(rate) = g.rate() {
                self.note_train(rate);
                self.probe_reported = g.packets;
                self.tag_probe(&g, false);
            }
        }
    }

    /// 包串速率：首包到达之后的字节 ÷ 首尾间隔。间隔取到达和发送（`send_span`）里长的：瓶颈只会把包拉开，
    /// 到达比发送还挤只能是接收端一次读走了一批（或者无线把一批包聚合着送来），那段到达间隔不可信。
    /// 按速率匀速发的探测组因此测不出比发送速率更高的值（照 WebRTC `probe_bitrate_estimator` 取发送、到达速率小的）。
    fn train(&mut self, first: Time, last: Time, send_span: u64, packets: u32, bytes: u64) {
        let span = last.since(first).max(send_span);
        if packets >= MIN_TRAIN_PACKETS && span >= MIN_TRAIN_SPAN_US {
            let after_first = bytes.saturating_mul(u64::from(packets - 1)) / u64::from(packets);
            self.note_train(after_first.saturating_mul(1_000_000) / span);
        }
    }

    /// 记下一个包串速率（字节每秒）：这段时间报告最高的。
    fn note_train(&mut self, rate: u64) {
        self.max_train = self.max_train.max(u32::try_from(rate).unwrap_or(u32::MAX));
    }

    fn finish(&mut self, g: Group) {
        self.train(
            g.first_arrival,
            g.last_arrival,
            g.last_send.since(g.first_send),
            g.packets,
            g.bytes,
        );
        // 延迟趋势：和上一组比较。
        if let Some(prev) = self.previous {
            let signed = |t: Time| i64::try_from(t.micros()).unwrap_or(i64::MAX);
            let arrival_delta = signed(g.last_arrival) - signed(prev.last_arrival);
            let send_delta = signed(g.last_send) - signed(prev.last_send);
            self.accumulated += arrival_delta - send_delta;
            self.smoothed = (SMOOTHING_PERMILLE * self.smoothed
                + (1000 - SMOOTHING_PERMILLE) * self.accumulated)
                / 1000;
            let t_ms = i64::try_from(g.last_arrival.micros() / 1000).unwrap_or(i64::MAX);
            self.points.push((t_ms, self.smoothed));
            if self.points.len() > TREND_WINDOW {
                self.points.remove(0);
            }
        }
        self.previous = Some(g);
    }

    /// 生成这段时间的扩展测量，并开始下一段（包串速率清零，趋势线保留）。
    pub fn report(&mut self) -> Extension {
        self.report_probe();
        let slope_milli = slope_per_mille(&self.points);
        let n = i64::try_from(self.points.len()).unwrap_or(0);
        // 修正趋势 = 斜率 × 分组数（上限 60）× 4，单位毫秒（WebRTC 的定义，斜率无量纲）。
        // 这里斜率是 微秒/毫秒 再 ×1000，即无量纲斜率 ×10⁶；换成 0.1 毫秒要再除以 10⁵。
        let trend = slope_milli * n.min(60) * 4 / 100_000;
        let ext = Extension {
            trend_tenth_ms: i32::try_from(trend).unwrap_or(if trend > 0 {
                i32::MAX
            } else {
                i32::MIN
            }),
            groups: u16::try_from(self.points.len()).unwrap_or(u16::MAX),
            train_bytes_per_s: self.max_train,
        };
        self.max_train = 0;
        ext
    }
}

/// 最小二乘斜率 ×1000（y 的单位每 x 的单位，再乘 1000）。点少于 2 个或 x 全相同时为 0。
fn slope_per_mille(points: &[(i64, i64)]) -> i64 {
    let n = i64::try_from(points.len()).unwrap_or(0);
    if n < 2 {
        return 0;
    }
    let (sx, sy) = points
        .iter()
        .fold((0i64, 0i64), |(a, b), (x, y)| (a + x, b + y));
    let (mx, my) = (sx / n, sy / n);
    let (num, den) = points.iter().fold((0i64, 0i64), |(num, den), (x, y)| {
        let dx = x - mx;
        (num + dx * (y - my), den + dx * dx)
    });
    if den == 0 {
        return 0;
    }
    num.saturating_mul(1000) / den
}

/// 发送端看到的一次反馈及其上下文。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// 基本反馈。
    pub feedback: Feedback,
    /// 扩展测量（对端旧版本没有时为默认值）。
    pub ext: Extension,
    /// 当前的往返时间估计（微秒）。
    pub rtt_us: u64,
    /// 还没确认的字节数。
    pub in_flight: u64,
    /// 发送量现在是被谁限制的（发送端按原因判，见 [`SendLimit`]）。
    pub limit: SendLimit,
    /// 发送端这一刻有东西等着发、被网络挡着（上一份反馈以来该编帧时被在途上限或节奏器拦过、重传被额度截断过，
    /// 或者现在关键帧没确认、节奏器里还有包、在途满着）。[`SendLimit`] 的原因会保留 500 ms，这个只说现在。
    /// 估计器用它判断这份反馈里的送达速率是不是链路的能力（见 [`ClassicV1`] 的 `busy`）。
    pub backlog: bool,
}

/// 发送量被网络限制的原因（发送端记下最近一次，见 [`crate::media::MediaSender`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetCause {
    /// 不知道（老的调用方不报原因）：一律当成网络受限，行为和没有状态机时一样。
    #[default]
    Unknown,
    /// 会话刚开始，还没观察够。
    Start,
    /// 该编帧时被拦下：在途数据满了，或者节奏器里上一帧还没发完。
    FrameBlocked,
    /// 关键帧还没确认，之后的帧先不发。
    KeyPending,
    /// 编出的帧用掉了预算的八成以上：编码器是被码率卡着的。
    FrameNearBudget,
    /// 内容装不进预算，帧率在降档。
    FpsReduced,
    /// 编码分辨率低于原尺寸。
    ScaleReduced,
    /// 重传被额度截断：有要补的分片这一轮没发出去。
    ResendCapped,
    /// 硬件编码器的码控把帧丢了：预算放不下一帧。有东西要发、是码率不让发。
    EncoderDropped,
    /// 发出的量接近码率（预算桶没有判成受限）。
    UsingRate,
}

impl NetCause {
    /// 写进日志的说明。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unknown => "发送端没报原因",
            Self::Start => "会话刚开始",
            Self::FrameBlocked => "该编帧时在途数据满了或上一帧没发完",
            Self::KeyPending => "关键帧还没确认",
            Self::FrameNearBudget => "编出的帧用掉了预算的八成以上",
            Self::FpsReduced => "内容装不进预算，帧率在降档",
            Self::ScaleReduced => "编码分辨率低于原尺寸",
            Self::ResendCapped => "重传被额度截断",
            Self::EncoderDropped => "编码器因为预算放不下把帧丢了",
            Self::UsingRate => "发出的量接近码率",
        }
    }
}

/// 发送量是被谁限制的。**按原因判，不按“发出量 ÷ 码率”判**：发出量本身被拥塞信号节流（每帧预算随排队缩小、
/// 在途满了整帧不发），越拥塞发出量占码率的比例越低；按比例判会把真拥塞判成“没用满”。
/// 2026-11-26 实测，1 Mbps 链路上排队超过 30 ms 的反馈里 44% 发出量低于码率的 60%、35% 低于 25%，
/// 按比例放宽拥塞判断的五个版本窄带卡顿都变多（`docs/research/L-framework.md` 6.5、6.6）。
/// BBR / Linux 的定义也是按原因：没有数据要发、没被拥塞窗口拦着、丢的都重传了，才算应用受限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendLimit {
    /// 网络受限：最近有网络这边的原因在限制发送量。
    Network(NetCause),
    /// 应用受限：一段时间里没有任何网络受限的原因，发出的量也明显少于码率——发送端只是没那么多要发。
    App,
}

impl Default for SendLimit {
    fn default() -> Self {
        Self::Network(NetCause::Unknown)
    }
}

/// 码率控制所处的状态。量尺按它统计各状态的时间占比、核对“真实队列高时有没有被判成应用受限”；
/// 状态切换写事件日志。还没有显式状态机的估计器一律报告 [`RateState::Tracking`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RateState {
    /// 起步：会话开头的探测还没出结论。
    Startup,
    /// 跟踪：发送量被网络限制，按拥塞信号降、按送达速率升。
    #[default]
    Tracking,
    /// 排空（跟踪）：网络受限时刚降过码率，等队列排掉。只是标记：升降规则和跟踪完全相同（不变量 I3）。
    Drain,
    /// 排空（应用受限）：应用受限时降过码率，等队列排掉；不升，一个往返内不再降。
    DrainApp,
    /// 应用受限：发送端没那么多要发，送达速率不代表链路能力。
    AppLimited,
    /// 核实：应用受限时降过码率，等探测结论。
    Verify,
}

impl RateState {
    /// 全部状态，按固定顺序（统计表的列顺序）。
    pub const ALL: [Self; 6] = [
        Self::Startup,
        Self::Tracking,
        Self::Drain,
        Self::DrainApp,
        Self::AppLimited,
        Self::Verify,
    ];

    /// 名字（日志和报告用）。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Startup => "起步",
            Self::Tracking => "跟踪",
            Self::Drain => "排空（跟踪）",
            Self::DrainApp => "排空（应用受限）",
            Self::AppLimited => "应用受限",
            Self::Verify => "核实",
        }
    }

    /// 在 [`RateState::ALL`] 里的下标。
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Startup => 0,
            Self::Tracking => 1,
            Self::Drain => 2,
            Self::DrainApp => 3,
            Self::AppLimited => 4,
            Self::Verify => 5,
        }
    }
}

/// 运行中探测每组匀速发多久（微秒）、至少几个包。和起步探测一样（30 ms、16 个包）：接收端是应用读数据报时
/// 打的时间戳，程序忙一下几十个包一次读走，WebRTC 的 15 ms、5 个包在这里测不准。
pub const RUN_PROBE_SPAN_US: u64 = 30_000;
/// 见 [`RUN_PROBE_SPAN_US`]。
pub const RUN_PROBE_MIN_PACKETS: u32 = 16;

/// 按 `target_bps` 探一组要发几个包。
#[must_use]
pub fn run_probe_packets(target_bps: u64) -> u32 {
    let bytes = target_bps.saturating_mul(RUN_PROBE_SPAN_US) / 8_000_000;
    u32::try_from(bytes / u64::from(crate::cc::probe::PROBE_PACKET_BYTES))
        .unwrap_or(u32::MAX)
        .max(RUN_PROBE_MIN_PACKETS)
}

/// 运行中探测的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind {
    /// 核实：应用受限时降过码率（或者码率停在底线），探一下链路是不是真的变窄了。
    Verify,
    /// 周期：应用受限、码率明显低于已知的链路能力，定期往上探。
    Periodic,
}

impl ProbeKind {
    /// 名字。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Verify => "核实",
            Self::Periodic => "周期",
        }
    }
}

/// 估计器要发送端做的一轮探测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeAsk {
    /// 种类。
    pub kind: ProbeKind,
    /// 按多快发（比特每秒）。
    pub target_bps: u64,
}

/// 一轮探测怎么结束的（发送端告诉估计器）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeEnd {
    /// 接收端测得的速率（比特每秒）。
    Result(u64),
    /// 到时间没有结果。
    TimedOut,
    /// 中止（原因）：出现过载或网络受限的原因，或者一直等不到能探的时机。结果不用。
    Aborted(&'static str),
}

/// 一轮探测的结论。每轮必有其一（不变量 I4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeVerdict {
    /// 证实：链路承载得了目标速率（结果 ≥ 目标的七成），码率按结果升。
    Confirmed,
    /// 否定：链路承载不了，记下测得的容量，码率不升。
    Denied,
    /// 超时：结论未知。
    TimedOut,
    /// 中止。
    Aborted,
}

impl ProbeVerdict {
    /// 全部结论，按固定顺序（统计用）。
    pub const ALL: [Self; 4] = [Self::Confirmed, Self::Denied, Self::TimedOut, Self::Aborted];

    /// 名字。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Confirmed => "证实",
            Self::Denied => "否定",
            Self::TimedOut => "超时",
            Self::Aborted => "中止",
        }
    }

    /// 在 [`ProbeVerdict::ALL`] 里的下标。
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Confirmed => 0,
            Self::Denied => 1,
            Self::TimedOut => 2,
            Self::Aborted => 3,
        }
    }
}

/// 码率估计器：经典算法、以后的 AI 模型都实现它。
pub trait RateEstimator: Send + std::fmt::Debug {
    /// 名字，用在报告和特征记录里。
    fn name(&self) -> &'static str;
    /// 当前码率（比特每秒）。
    fn rate_bps(&self) -> u64;
    /// 处理一次反馈。
    fn on_observation(&mut self, now: Time, obs: &Observation);
    /// 网络画像（决定纠错比例、预算折扣）。
    fn profile(&self) -> &Profile;
    /// 换一套网络画像。
    fn set_profile(&mut self, profile: Profile);
    /// 取走最近的码率变化说明（诊断日志用：真实会话里码率掉下来、升上去时要知道为什么）。
    fn take_notes(&mut self) -> Vec<String> {
        Vec::new()
    }

    /// 用发送端的到达报告（[`crate::cc`]）：要接收端发到达报告、发送端记发送历史。
    fn uses_reports(&self) -> bool {
        false
    }

    /// 发出了一个包（含重传、探测）。
    fn on_sent(&mut self, _now: Time, _packet: &SentPacket) {}

    /// 一份到达报告（已对账）。
    fn on_report(&mut self, _now: Time, _packets: &[PacketResult], _rtt_us: u64) {}

    /// 定时调用（约 25 ms）。
    fn on_tick(&mut self, _now: Time) {}

    /// 节奏器的发送速率；`None` 表示按老规则（见 [`crate::pacer::rate_for`]）。
    fn pacing_bps(&self) -> Option<u64> {
        None
    }

    /// 取走要发的探测组。
    fn take_probes(&mut self) -> Vec<ProbeCluster> {
        Vec::new()
    }

    /// 画面静止时要不要定期探测链路（见 [`crate::media::MediaSender::set_idle_probe`]）。经典算法要：
    /// 它只靠视频帧的包串测容量，静止时没有帧就不知道链路变了。网络控制器（gcc）有自己的应用受限探测。
    fn wants_idle_probe(&self) -> bool {
        false
    }

    /// 静止时的探测测得链路容量 `capacity_bps`：按它调码率（链路在静止时变差就降下来，变好就升上去）。
    fn on_idle_probe(&mut self, _now: Time, _capacity_bps: u64) {}

    /// 起步探测测得链路容量至少 `capacity_bps`：码率直接定到它的安全比例（升降都按它）。
    fn on_start_probe(&mut self, _now: Time, _capacity_bps: u64) {}

    /// 一行状态（诊断日志用）。
    fn state_line(&self) -> Option<String> {
        None
    }

    /// 码率控制现在处于哪个状态（见 [`RateState`]）。
    fn state(&self) -> RateState {
        RateState::Tracking
    }

    /// 发送端的起步探测开始（`on` 为真）或结束了（`why` 是结束的原因，写进状态切换的日志）。
    fn set_startup(&mut self, _on: bool, _why: &str) {}

    /// 发送端刚出现一个网络受限的原因（不等下一份反馈：应用受限进慢出快）。
    fn on_net_limited(&mut self, _cause: NetCause) {}

    /// 现在要不要做一轮运行中探测（发送端空闲时每个定时周期问一次；对端的反馈带探测组号时才问）。
    /// 返回了一轮，发送端一定会用 [`Self::on_probe_end`] 回报它怎么结束的。
    fn probe_wanted(&mut self, _now: Time) -> Option<ProbeAsk> {
        None
    }

    /// 上一次 [`Self::probe_wanted`] 要的那一轮探测结束了。返回结论。
    fn on_probe_end(&mut self, _now: Time, _end: ProbeEnd) -> ProbeVerdict {
        ProbeVerdict::Aborted
    }
}

/// 照 WebRTC GoogCC 组织的网络控制器（见 [`crate::cc`]）。
#[derive(Debug, Clone)]
pub struct GccV1 {
    gcc: Gcc,
    profile: Profile,
    /// 会话开始时探测（按起始码率的 3、6 倍）。
    probe_at_start: bool,
}

impl GccV1 {
    /// 创建。
    #[must_use]
    pub fn new(profile: Profile, start_bps: u64, min_bps: u64, max_bps: u64) -> Self {
        Self {
            gcc: Gcc::new(start_bps, min_bps, max_bps),
            profile,
            probe_at_start: false,
        }
    }
}

impl RateEstimator for GccV1 {
    fn name(&self) -> &'static str {
        "gcc-v1"
    }

    fn rate_bps(&self) -> u64 {
        self.gcc.target_bps()
    }

    /// 接收端算的排队延迟、趋势、包串都不用：网络判断只看到达报告。
    fn on_observation(&mut self, _now: Time, _obs: &Observation) {}

    fn profile(&self) -> &Profile {
        &self.profile
    }

    fn set_profile(&mut self, profile: Profile) {
        self.profile = profile;
    }

    fn take_notes(&mut self) -> Vec<String> {
        self.gcc.take_notes()
    }

    fn uses_reports(&self) -> bool {
        true
    }

    fn on_sent(&mut self, now: Time, packet: &SentPacket) {
        self.gcc.on_sent(now, packet);
    }

    fn on_report(&mut self, now: Time, packets: &[PacketResult], rtt_us: u64) {
        self.gcc.on_report(now, packets, rtt_us);
    }

    fn on_tick(&mut self, now: Time) {
        self.gcc.on_tick(now, self.probe_at_start);
    }

    fn pacing_bps(&self) -> Option<u64> {
        Some(self.gcc.pacing_bps())
    }

    fn take_probes(&mut self) -> Vec<ProbeCluster> {
        self.gcc.take_probes()
    }

    fn state_line(&self) -> Option<String> {
        Some(self.gcc.state_line())
    }
}

/// 选哪个估计器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimatorKind {
    /// 原来的码率控制，对照用。
    Legacy,
    /// 传输算法 v1。
    ClassicV1,
    /// 照 WebRTC GoogCC 的网络控制器（发送端到达报告，见 [`crate::cc`]）。对端也要支持到达报告。
    GccV1,
}

impl EstimatorKind {
    /// 产品默认用的估计器。换默认的条件：端到端测试的所有场景都不比原来的差。
    /// v1 达到了：仿真里打平或更好（Wi-Fi 3% 随机丢包时画质 46.7 对 40.6 dB、最长等待 52 对 104 ms、
    /// 停下后的流量少九成）；真实二进制在 25 ms、2% 丢包的链路上，原来的码率掉到 108 kbps，v1 保持 3.8 Mbps。
    /// 原来的估计器留着当对照，每个场景都并排跑。
    pub const PRODUCT: Self = Self::ClassicV1;

    /// 按名字选（`legacy`、`v1`），实验开关用。认不出的名字返回 `None`。
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "legacy" => Some(Self::Legacy),
            "v1" | "classic-v1" => Some(Self::ClassicV1),
            "gcc" | "gcc-v1" => Some(Self::GccV1),
            _ => None,
        }
    }

    /// 创建估计器。
    #[must_use]
    pub fn build(
        self,
        profile: Profile,
        start_bps: u64,
        min_bps: u64,
        max_bps: u64,
    ) -> Box<dyn RateEstimator> {
        match self {
            Self::Legacy => Box::new(Legacy(RateController::new(
                profile, start_bps, min_bps, max_bps,
            ))),
            Self::ClassicV1 => Box::new(ClassicV1::new(profile, start_bps, min_bps, max_bps)),
            Self::GccV1 => Box::new(GccV1::new(profile, start_bps, min_bps, max_bps)),
        }
    }
}

/// 原来的码率控制（“延迟高就降、用满才升”），保留作对照。
#[derive(Debug, Clone)]
pub struct Legacy(pub RateController);

impl RateEstimator for Legacy {
    fn name(&self) -> &'static str {
        "legacy"
    }
    fn rate_bps(&self) -> u64 {
        self.0.rate_bps()
    }
    fn on_observation(&mut self, now: Time, obs: &Observation) {
        self.0.on_feedback(now, &obs.feedback);
    }
    fn profile(&self) -> &Profile {
        self.0.profile()
    }
    fn set_profile(&mut self, profile: Profile) {
        self.0.set_profile(profile);
    }
}

/// 过载检测的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Usage {
    /// 正常。
    Normal,
    /// 排队在增长：降。
    Over,
    /// 排队在消退：先别升，让队列排空。
    Under,
}

/// 传输算法 v1 的经典估计器。
#[derive(Debug, Clone)]
pub struct ClassicV1 {
    profile: Profile,
    rate_bps: u64,
    min_bps: u64,
    max_bps: u64,
    /// 自适应门限（0.1 毫秒），WebRTC 的做法：趋势超过门限就把门限往上拉，反之往下，
    /// 这样和同一链路上的 TCP 下载共存时不会被饿死。
    threshold: i64,
    last_threshold_update: Option<Time>,
    /// 最近测到的瓶颈带宽（比特每秒），带时间。
    capacity: Option<(u64, Time)>,
    last_decrease: Option<Time>,
    /// 上一份反馈的时间（判断反馈断没断档，见 [`CONTIGUOUS_US`]）。
    last_feedback: Option<Time>,
    usage: Usage,
    /// 码率变化说明（取走后清空）和上次写说明时的码率（升码率累计超过一定比例才写一次）。
    notes: Vec<String>,
    noted_bps: u64,
    /// 固有丢包率（千分比 × 1000，慢速平均）：随机丢包（Wi-Fi 干扰、链路本身）的底，高出它的丢包才算拥塞
    /// （WebRTC `LossBasedBweV2` 的“固有丢包”）。
    inherent_loss: u64,
    /// 状态机现在的状态。
    phase: Phase,
    /// 这一段不断档的反馈里，各份的时间和排队读数（只留最近一个窗口加一份，见 [`ClassicV1::standing_queue`]）。
    queue_run: VecDeque<(Time, u32)>,
    /// 运行中探测的触发和结论（见 [`Probing`]）。
    probing: Probing,
    /// 从什么时候起发送端一直在把链路往满里用（每份反馈都是用满了码率或有积压）。持续满一个窗口，
    /// 送达速率才代表链路的能力（见 [`ClassicV1::link_limited`]）。
    busy: Busy,
    /// 没在用链路时的排队读数（链路自己的抖动，见 [`Jitter`]）。
    jitter: Jitter,
}

/// 发送端是不是一直在把链路往满里用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Busy {
    /// 从会话开始就是（还没见过不是的反馈）。
    FromStart,
    /// 从这个时刻起一直是。
    Since(Time),
    /// 最近一份反馈不是。
    No,
}

/// 链路自己的抖动：发送端没在用链路（没用满码率、也没有积压）时的排队读数。这时我们的流量堆不出队列，
/// 读数高只能是链路自己的毛病（Wi-Fi 的信道争用、省电唤醒、接收端批量读包）。
///
/// 用它选延迟门限，而不是按“是不是 Wi-Fi”写死（以前会话里写死成“不是”，Wi-Fi 的门限从来没用上）：
/// 最近 [`JITTER_WINDOW`] 份这样的读数里有 [`JITTER_ON`] 份以上超过稳定链路的 high（12 ms），就按抖动链路的门限
/// （[`Profile::WIRELESS`] 的 low 8 ms、high 30 ms）；整整一个窗口一份都没有才换回来（滞回）。
/// 2026-11-27 用户日志：Mac 公网 Wi-Fi 和 Windows 局域网 Wi-Fi 上，降码率时的排队中位都是 17 ms（10%–90% 分位 12–50 ms），
/// 趋势正常、丢包 0——刚过 12 ms 的线。只换延迟门限，不换整套画像：纠错比例是给丢包准备的，这里没有丢包。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Jitter {
    /// 最近的读数是否超过了稳定链路的 high。
    recent: VecDeque<bool>,
    /// 现在按抖动链路的门限。
    on: bool,
    /// 一段超过稳定链路 high 的读数从什么时候开始、还没有定论（是尖峰还是队列，要看它多久回落）。
    spike_since: Option<Time>,
}

/// 看最近多少份“没在用链路”的排队读数。
const JITTER_WINDOW: usize = 64;
/// 其中有这么多份超过稳定链路的 high 就算抖动链路（约 5%：有线链路上这样的读数基本是 0）。
const JITTER_ON: usize = 3;

impl Jitter {
    const fn new() -> Self {
        Self {
            recent: VecDeque::new(),
            on: false,
            spike_since: None,
        }
    }

    /// 一份“没在用链路”的排队读数；返回门限是否换了。尖峰和队列按持续多久分（一个信号只表示一件事）：
    /// - 高读数在一个窗口（`window_us`，和常驻队列同一个窗口）之内回落：尖峰，是链路自己的抖动（Wi-Fi 的信道争用、
    ///   接收端批量读包）；
    /// - 高读数持续满一个窗口：是队列（例如家里有人在上传，或者我们自己一阵大帧还没排完），不算抖动，交给常驻队列判断；
    /// - 断档的读数（零星的包）：每一份都是孤立的，看不出持续多久，按读数本身算（Wi-Fi 省电下每份都是醒过来的延迟，
    ///   这本身就是链路的抖动）。
    fn reading(&mut self, now: Time, queue_us: u32, contiguous: bool, window_us: u64) -> bool {
        let over = queue_us > Profile::STABLE.high_delay_us;
        if !contiguous {
            self.spike_since = None;
            return self.add(over);
        }
        match (over, self.spike_since) {
            (true, None) => {
                self.spike_since = Some(now);
                false
            }
            (true, Some(t)) => {
                if now.since(t) >= window_us {
                    // 满一个窗口还高：是队列，这一段了结（不算抖动），下一份高读数重新开始
                    self.spike_since = None;
                    self.add(false)
                } else {
                    false
                }
            }
            (false, Some(t)) => {
                self.spike_since = None;
                self.add(now.since(t) < window_us)
            }
            (false, None) => self.add(false),
        }
    }

    /// 记一份读数；返回门限是否换了（换了要写日志）。
    fn add(&mut self, over: bool) -> bool {
        self.recent.push_back(over);
        if self.recent.len() > JITTER_WINDOW {
            self.recent.pop_front();
        }
        let count = self.recent.iter().filter(|o| **o).count();
        let was = self.on;
        if count >= JITTER_ON {
            self.on = true;
        } else if count == 0 && self.recent.len() >= JITTER_WINDOW {
            self.on = false;
        }
        self.on != was
    }
}

/// 运行中探测在估计器这边的状态：什么时候要探、探多快、结论怎么用。发、等、超时、中止在发送端
/// （[`crate::media::MediaSender`] 的探测状态机：空闲 → 待发 → 已发 → 判定）。
///
/// 触发：
/// - 核实（T1）：应用受限时降到了降前的 66% 以下，之后排队低于 low 满一个往返（降后 5 秒内有效）。
///   目标是降前的 85%（WebRTC `ProbeController::RequestProbe` 的 0.66、0.85、5 秒），但第一轮不超过当前码率的 4 倍、
///   证实了再按结果的 2 倍接着探，最多 3 轮。直接按 85% 探的话，链路真从 30 降到 3 Mbps 时一组是 96 KB，
///   在 3 Mbps 上要排 250 ms；先探 4 倍，最坏多排几十毫秒。
/// - 核实（I5）：应用受限时码率在底线停满 1 秒（之后间隔翻倍，最长 30 秒）。
/// - 周期（T2）：应用受限满 5 秒、距上次探测够久（5 秒起，没探上去就翻倍，最长 30 秒），并且码率低于已知链路能力的
///   七成。目标是码率的 2 倍，证实了接着翻倍，最多 3 轮。和 WebRTC（`TimeForAlrProbe`，应用受限时每 5 秒无条件探）
///   不同：码率已经接近链路能力时不探——一组探测是 30 ms 的满速流量（30 Mbps 上 112 KB），轻操作每秒只发十几 KB，
///   每 5–30 秒探一次的话探测流量占一两成，中继流量是成本。
///
/// 窄带保护：一组探测的字节数不许超过“已知容量 × 50 ms”，不知道容量就不探（I5 的核实除外）。一组至少 16 个包
/// （19 KB），所以已知容量低于 3 Mbps 的链路上运行中探测的字节数是 0。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Probing {
    /// 应用受限时降码率的这一段：(第一次降之前的码率, 最近一次降的时刻)。
    drop: Option<(u64, Time)>,
    /// 从什么时候起每份反馈的排队都低于 low。
    calm_since: Option<Time>,
    /// 从什么时候起一直在应用受限（含它的核实、排空）。
    app_since: Option<Time>,
    /// 正在等发送端回报的那一轮，和这一串已经探了几轮。
    asked: Option<ProbeAsk>,
    rounds: u8,
    /// 这一串接着探的目标（上一轮证实之后）。
    next_target: Option<u64>,
    /// 上一轮探测结束的时刻，周期探测和底线核实现在的间隔。
    last_end: Option<Time>,
    periodic_gap_us: u64,
    floor_gap_us: u64,
    /// 最近一份反馈的往返时间。
    rtt_us: u64,
    /// 链路能力的上沿探到过了（起步探测有结论、某一轮被否定、或者证实了却没探上去多少）。没探到过时，已知的容量
    /// 只是帧的包串测的下界（按码率发的帧测不出比码率高多少），码率贴着它也还要往上探一次；探到过之后，
    /// 只有码率低于它的七成才探——否则轻操作时每隔几秒到几十秒就白发一组满速的探测。
    limit_known: bool,
    /// 这一段的核实已经被否定过一次、在等复核。
    retried: bool,
}

/// 降到降前的这么多（千分比）以下才值得核实，核实的目标是降前的这么多。
const VERIFY_BELOW_PERMILLE: u64 = 660;
const VERIFY_TARGET_PERMILLE: u64 = 850;
/// 降码率之后这么久之内才核实（微秒）。
const VERIFY_WITHIN_US: u64 = 5_000_000;
/// 第一轮探测不超过当前码率的几倍；证实之后每轮按结果的几倍；一串最多几轮。
const PROBE_FIRST_SCALE: u64 = 4;
const PROBE_STEP: u64 = 2;
const PROBE_MAX_ROUNDS: u8 = 3;
/// 结果达到目标的这么多（千分比）算证实（和起步探测一样）。
const PROBE_CONFIRM_PERMILLE: u64 = 700;
/// 周期探测：应用受限满这么久才开始，间隔从这么久起、最长这么久（微秒）。
const PERIODIC_FIRST_US: u64 = 5_000_000;
const PERIODIC_MAX_US: u64 = 30_000_000;
/// 码率低于已知链路能力的这么多（千分比）才周期探测。
const PERIODIC_BELOW_PERMILLE: u64 = 700;
/// 一组探测的字节数不超过已知容量这么久的量（微秒）。
const PROBE_FIT_US: u64 = 50_000;
/// 固有丢包（千分比 × 1000）到这个值的链路不做运行中探测：1%，和起步探测、加纠错的门限一样。
const LOSSY_LINK: u64 = 10_000;
/// 一轮探测中止之后至少隔这么久再探（微秒）。
const PROBE_RETRY_US: u64 = 200_000;

/// [`ClassicV1`] 的状态机。码率控制要回答三件事，证据各不相同，所以分开：
/// 是否拥塞（排队、趋势）、链路容量（包串、探测）、发送量受谁限制（[`SendLimit`]，发送端按原因判）。
///
/// | 状态 | 降 | 升 | 出口 |
/// |---|---|---|---|
/// | 起步 | 和跟踪相同 | 和跟踪相同，另按起步探测的结论 | 发送端报告起步探测结束 → 跟踪（等第一个关键帧最多 2 秒，探测最多 1 秒） |
/// | 跟踪 | 一份反馈的排队、趋势、丢包加排队 | 送达速率 ≥ 码率 60% 时升 | 降了 → 排空；应用受限 → 应用受限 |
/// | 排空 | 同来源状态（一个往返内不重复降） | 同来源状态 | 排队低于 low → 回来源状态；max(1 秒, 4 个往返) → 跟踪；来源是应用受限时一有网络受限 → 跟踪 |
/// | 应用受限 | 只认常驻队列或趋势过载，一次最多降一半 | 不自己升，按探测结论 | 网络受限 → 跟踪（无时间上限）；降了 → 排空；降过且队列平静、或在底线停满 1 秒 → 核实 |
/// | 核实 | 同应用受限 | 按探测结论 | 探测结论（证实、否定、超时、中止）→ 应用受限；网络受限 → 跟踪；降了 → 排空；兜底 2 秒 |
///
/// 不变量：
/// - I1：任何状态下过载证据都优先，这一份反馈不会升。
/// - I2：码率只在三处变：拥塞降、跟踪（含起步、来源是跟踪的排空）的升、探测结论。
/// - I3：发送端一直是网络受限时（从不进应用受限），每一步的码率和没有状态机的版本完全相同。
///   所以来源是跟踪的“排空”只是一个标记，升降规则就是跟踪的规则；窄带不变差由这一条保证。
/// - I4：每轮探测必然在时限内以证实、否定、超时、中止之一结束（见 [`Probing`] 和发送端的探测状态机）。
/// - I5：应用受限时码率停在底线超过 1 秒必须进核实，不允许“底线、不升、没人管”。
///
/// 和 WebRTC 不一样的地方：WebRTC 应用受限时不放宽过载反应（`DelayBasedBwe::MaybeUpdateEstimate` 的 `in_alr`
/// 参数没有用），只用它限制升码率、触发探测。这里放宽，因为我们的过载信号是单份 20 ms 反馈的排队读数，不是
/// 趋势线：轻操作时一次 Wi-Fi 尖峰（13–27 ms）就会被当成拥塞，按“送达速率 × 0.85”降，而应用受限时送达速率
/// 只是要发的量，一下就到底（2026-11-26 用户日志：168 秒里这样降了 39 次，11 次降到 300 kbps 以下）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Startup,
    Tracking,
    /// 刚降过。`from_app`：降的时候是应用受限（或核实）；`since`：什么时候降的。
    Drain {
        from_app: bool,
        since: Time,
    },
    /// `floor_since`：从什么时候起码率一直在底线。
    AppLimited {
        floor_since: Option<Time>,
    },
    /// `since`：什么时候开始核实的。
    Verify {
        since: Time,
    },
}

impl Phase {
    const fn state(self) -> RateState {
        match self {
            Self::Startup => RateState::Startup,
            Self::Tracking => RateState::Tracking,
            Self::Drain {
                from_app: false, ..
            } => RateState::Drain,
            Self::Drain { from_app: true, .. } => RateState::DrainApp,
            Self::AppLimited { .. } => RateState::AppLimited,
            Self::Verify { .. } => RateState::Verify,
        }
    }

    /// 拥塞判断放宽（只认常驻队列、一次最多降一半、不升）：应用受限、核实，和从它们降下来的排空。
    const fn relaxed(self) -> bool {
        match self {
            Self::AppLimited { .. } | Self::Verify { .. } | Self::Drain { from_app: true, .. } => {
                true
            }
            Self::Startup
            | Self::Tracking
            | Self::Drain {
                from_app: false, ..
            } => false,
        }
    }
}

/// 应用受限时，排队要持续这么久（这段时间里的最小读数；往返更长时按往返）才算常驻队列。取 RFC 8289（CoDel）
/// 判断常驻队列的间隔 100 ms：一次 Wi-Fi 尖峰只影响几份反馈，最小值低；真有队列时这段时间每份都高。
const STANDING_US: u64 = 100_000;
/// 排空最多这么久（往返很长时按 4 个往返），到时不管队列排没排掉都回跟踪。
const DRAIN_MAX_US: u64 = 1_000_000;
/// 应用受限时码率在底线停这么久就进核实（I5）。
const FLOOR_VERIFY_US: u64 = 1_000_000;
/// 核实最多这么久。
const VERIFY_MAX_US: u64 = 1_000_000;

/// 门限的上下界（0.1 毫秒）和调整速度（每毫秒千万分之一）：WebRTC 的 6–600 毫秒、k_up 0.0087、k_down 0.039。
const THRESHOLD_MIN: i64 = 60;
const THRESHOLD_MAX: i64 = 6000;
const THRESHOLD_START: i64 = 125;
const K_UP_E7: i64 = 87_000;
const K_DOWN_E7: i64 = 390_000;
/// 测得的容量多久之后不再可信（微秒）。
const CAPACITY_TTL_US: u64 = 5_000_000;

impl ClassicV1 {
    /// 创建。
    #[must_use]
    pub fn new(profile: Profile, start_bps: u64, min_bps: u64, max_bps: u64) -> Self {
        let min_bps = min_bps.max(1);
        let max_bps = max_bps.max(min_bps);
        Self {
            profile,
            rate_bps: start_bps.clamp(min_bps, max_bps),
            min_bps,
            max_bps,
            threshold: THRESHOLD_START,
            last_threshold_update: None,
            capacity: None,
            last_decrease: None,
            last_feedback: None,
            notes: Vec::new(),
            noted_bps: start_bps,
            usage: Usage::Normal,
            inherent_loss: 0,
            phase: Phase::Tracking,
            queue_run: VecDeque::new(),
            busy: Busy::FromStart,
            jitter: Jitter::new(),
            probing: Probing {
                drop: None,
                calm_since: None,
                app_since: None,
                asked: None,
                rounds: 0,
                next_target: None,
                last_end: None,
                periodic_gap_us: PERIODIC_FIRST_US,
                floor_gap_us: FLOOR_VERIFY_US,
                rtt_us: 0,
                limit_known: false,
                retried: false,
            },
        }
    }

    /// 一组按 `target_bps` 的探测放不放得进链路：字节数不超过已知容量 [`PROBE_FIT_US`] 的量。不知道容量算放不进。
    ///
    /// 一直有随机丢包的链路（固有丢包 1% 以上）也算放不进：丢包下探测测不准。和起步探测的规则一样
    /// （有丢包不探：ms-02560 按探测升码率后卡顿 0 → 3 次）；只看最近一份反馈的话，12% 丢包的链路上总有几份
    /// 碰巧没丢，探了也多半被否定——微软轨迹 8 Mbps、12–13% 丢包的三条上 38 轮周期探测 26 轮否定，白发 1.7 MB。
    fn probe_fits(&self, target_bps: u64) -> bool {
        if self.inherent_loss >= LOSSY_LINK {
            return false;
        }
        let bytes = u64::from(run_probe_packets(target_bps))
            .saturating_mul(u64::from(crate::cc::probe::PROBE_PACKET_BYTES));
        self.capacity
            .is_some_and(|(c, _)| bytes <= c.saturating_mul(PROBE_FIT_US) / 8_000_000)
    }

    /// 已知的链路能力（测得的容量打安全折扣，不超过上限）；没测过时为 `None`。
    fn ceiling_bps(&self) -> Option<u64> {
        self.capacity.map(|(c, _)| {
            (c.saturating_mul(u64::from(self.profile.safety_permille)) / 1000).min(self.max_bps)
        })
    }

    /// 写一条说明（探测结论、状态切换用同一个额度）。
    fn note(&mut self, text: String) {
        if self.notes.len() < MAX_NOTES * 2 {
            self.notes.push(text);
        }
    }

    /// 切换状态，写一条带原因的说明（状态没变时不写）。
    fn go(&mut self, to: Phase, why: &str) {
        let (from, next) = (self.phase.state(), to.state());
        self.phase = to;
        // 状态切换的说明比码率变化的说明多留一倍的位置：码率在抖时也要看得出状态机在哪
        if from != next && self.notes.len() < MAX_NOTES * 2 {
            self.notes.push(format!(
                "码率状态 {} → {}：{why}（码率 {} kbps）",
                from.name(),
                next.name(),
                self.rate_bps / 1000
            ));
        }
    }

    /// 延迟门限 `(low, high)`：链路自己抖动大时用抖动链路的（见 [`Jitter`]），不低于画像本身的。
    fn delay_limits(&self) -> (u32, u32) {
        let p = &self.profile;
        if self.jitter.on {
            (
                p.low_delay_us.max(Profile::WIRELESS.low_delay_us),
                p.high_delay_us.max(Profile::WIRELESS.high_delay_us),
            )
        } else {
            (p.low_delay_us, p.high_delay_us)
        }
    }

    /// 这份反馈里的送达速率是不是链路的能力（“链路受限的样本”）。是的话按它降不会过头；不是的话它只是
    /// “这段时间要发的量”，不能用来降码率——BBR 的规则（draft-ietf-ccwg-bbr：应用受限的送达速率样本只能把估计
    /// 往上推；而且应用受限的标记要等受限期间发的包都确认完才清掉）。
    ///
    /// 两种情况算链路受限：
    /// - 送达速率已经在码率的六成以上（用满了）：按它降最多降一半，本身就是证据；
    /// - 发送端连着一个窗口（100 ms，往返更长时按往返）都有积压或用满码率：这份反馈确认的包是在链路一直忙着的
    ///   时候发的。刚开始有积压的那一个窗口不算——确认的还是之前零零星星发的包（“气泡”还没走完）。
    ///
    /// 和以前的区别：以前只要状态是“跟踪”，一份排队读数高就按送达速率降。但从应用受限切回跟踪只需要一个大帧，
    /// 那之后的送达速率还是静止时的：2026-11-27 Mac 日志 46 分钟里这样降了 496 次，317 次降到 1 Mbps 以下
    /// （`docs/research/P-fps-bitrate.md` M1）。
    fn link_limited(&mut self, now: Time, using: bool, obs: &Observation) -> bool {
        let backlog = obs.backlog
            || matches!(
                obs.limit,
                SendLimit::Network(NetCause::Unknown | NetCause::Start | NetCause::UsingRate)
            );
        if using || backlog {
            if self.busy == Busy::No {
                self.busy = Busy::Since(now);
            }
        } else {
            self.busy = Busy::No;
        }
        let window = STANDING_US.max(obs.rtt_us);
        using
            || match self.busy {
                Busy::FromStart => true,
                Busy::Since(t) => now.since(t) >= window,
                Busy::No => false,
            }
    }

    /// 记下这份反馈的排队读数，返回最近 `window_us` 里的最小读数（常驻队列）。这一段不断档的反馈还不够这么长时
    /// 返回 `None`：证据不够，不算常驻队列。断档时从头记。
    fn standing_queue(
        &mut self,
        now: Time,
        queue_us: u32,
        contiguous: bool,
        window_us: u64,
    ) -> Option<u32> {
        if !contiguous {
            self.queue_run.clear();
        }
        self.queue_run.push_back((now, queue_us));
        // 最老的一份留在窗口左边界上或之前（代表窗口开始时的队列），更早的丢掉
        while self
            .queue_run
            .get(1)
            .is_some_and(|(t, _)| now.since(*t) >= window_us)
        {
            self.queue_run.pop_front();
        }
        let (oldest, _) = self.queue_run.front()?;
        if now.since(*oldest) < window_us {
            return None;
        }
        self.queue_run.iter().map(|(_, q)| *q).min()
    }

    /// 按发送端报告的受限原因和时间走状态机的出口（降码率引起的切换在 [`RateEstimator::on_observation`] 里）。
    /// 一份反馈最多走一步。
    fn step(&mut self, now: Time, obs: &Observation) {
        let (low, _) = self.delay_limits();
        let queue = obs.feedback.queue_delay_us;
        match (self.phase, obs.limit) {
            // 起步由发送端的起步探测结束（见 `set_startup`）
            (Phase::Startup, _) => {}
            // （从跟踪降下来的排空只是标记，规则就是跟踪：发送端报应用受限时一样直接进）
            (
                Phase::Tracking
                | Phase::Drain {
                    from_app: false, ..
                },
                SendLimit::App,
            ) => self.go(
                Phase::AppLimited { floor_since: None },
                "发送端 500 ms 没有任何网络受限的原因，发出的量也明显少于码率",
            ),
            (Phase::Tracking, SendLimit::Network(_)) => {}
            (
                Phase::AppLimited { .. }
                | Phase::Verify { .. }
                | Phase::Drain { from_app: true, .. },
                SendLimit::Network(cause),
            ) => self.go(Phase::Tracking, &format!("网络受限：{}", cause.name())),
            (Phase::Drain { from_app, since }, _) => {
                if queue < low {
                    let back = if from_app {
                        Phase::AppLimited { floor_since: None }
                    } else {
                        Phase::Tracking
                    };
                    self.go(
                        back,
                        &format!(
                            "队列排掉了（排队 {} ms，低于 {} ms）",
                            queue / 1000,
                            low / 1000
                        ),
                    );
                } else if now.since(since) >= DRAIN_MAX_US.max(obs.rtt_us.saturating_mul(4)) {
                    self.go(
                        Phase::Tracking,
                        &format!("排空到时限了，排队还有 {} ms，按网络受限处理", queue / 1000),
                    );
                }
            }
            (Phase::AppLimited { floor_since }, SendLimit::App) => {
                // 记下码率从什么时候起一直在底线（I5，见 `probe_wanted`）
                let at_floor = self.rate_bps <= self.min_bps;
                let since = floor_since.filter(|_| at_floor).or(at_floor.then_some(now));
                self.phase = Phase::AppLimited { floor_since: since };
            }
            (Phase::Verify { since }, SendLimit::App) => {
                // 兜底：发送端的探测状态机每轮都有时限，正常到不了这里
                if now.since(since) >= VERIFY_MAX_US.saturating_mul(2) {
                    self.probing.asked = None;
                    self.go(
                        Phase::AppLimited { floor_since: None },
                        "核实超时：一直没有探测结论，码率不变",
                    );
                }
            }
        }
        if self.phase.relaxed() {
            self.probing.app_since.get_or_insert(now);
        } else {
            self.leave_app_limited();
        }
    }

    /// 离开了应用受限（和它的核实、排空）：探测的触发条件清掉。正在等的那一轮由发送端中止、回报。
    /// 降码率的那一段（`drop`）留着：5 秒内回到应用受限、码率还低着，照样核实。
    const fn leave_app_limited(&mut self) {
        self.probing.app_since = None;
        self.probing.next_target = None;
        self.probing.rounds = 0;
    }

    /// 这一串探测到此为止。
    const fn end_chain(&mut self) {
        self.probing.next_target = None;
        self.probing.rounds = 0;
    }

    /// 开始一轮探测：记下来等发送端回报。
    fn ask(&mut self, kind: ProbeKind, target_bps: u64) -> Option<ProbeAsk> {
        let ask = ProbeAsk { kind, target_bps };
        self.probing.asked = Some(ask);
        self.probing.rounds = self.probing.rounds.saturating_add(1);
        Some(ask)
    }

    /// 当前的过载判断（诊断用）。
    #[must_use]
    pub const fn usage(&self) -> Usage {
        self.usage
    }

    fn detect(&mut self, now: Time, ext: &Extension) -> Usage {
        let trend = i64::from(ext.trend_tenth_ms);
        let dt_ms = self
            .last_threshold_update
            .map_or(0, |t| i64::try_from(now.since(t) / 1000).unwrap_or(0))
            .min(100);
        self.last_threshold_update = Some(now);
        // 门限跟着趋势慢慢移动（离得太远的尖峰不参与，避免被一次抖动拉飞）。
        if trend.abs() < self.threshold + 150 {
            let k = if trend.abs() < self.threshold {
                K_DOWN_E7
            } else {
                K_UP_E7
            };
            self.threshold += k * (trend.abs() - self.threshold) * dt_ms / 10_000_000;
            self.threshold = self.threshold.clamp(THRESHOLD_MIN, THRESHOLD_MAX);
        }
        if ext.groups < 2 {
            Usage::Normal
        } else if trend > self.threshold {
            Usage::Over
        } else if trend < -self.threshold {
            Usage::Under
        } else {
            Usage::Normal
        }
    }
}

impl RateEstimator for ClassicV1 {
    fn name(&self) -> &'static str {
        "classic-v1"
    }

    fn wants_idle_probe(&self) -> bool {
        true
    }

    fn on_idle_probe(&mut self, now: Time, capacity_bps: u64) {
        self.capacity = Some((capacity_bps, now));
        let from = self.rate_bps;
        // 只降、而且要明显低（不到当前码率的一半）才降：链路在静止时变差了，下一帧要按它编。不按它升：
        // 几个包测的包串在波动、有丢包的链路上有噪声，按它升码率实测更卡（微软轨迹 ms-05729 卡顿 7 → 13 次）；
        // 升交给有真实流量时的正常爬升（WebRTC 也是靠延迟信号降，探测只用来往上试）。
        let target = capacity_bps.saturating_mul(u64::from(self.profile.safety_permille)) / 1000;
        if target.saturating_mul(2) < from {
            self.rate_bps = target.clamp(self.min_bps, self.max_bps);
        }
        if self.rate_bps != from && self.notes.len() < MAX_NOTES {
            self.notes.push(format!(
                "码率 {} → {} kbps：静止时探测测得容量 {} kbps",
                from / 1000,
                self.rate_bps / 1000,
                capacity_bps / 1000
            ));
            self.noted_bps = self.rate_bps;
        }
    }

    fn on_start_probe(&mut self, now: Time, capacity_bps: u64) {
        self.capacity = Some((capacity_bps, now));
        let target = capacity_bps.saturating_mul(u64::from(self.profile.safety_permille)) / 1000;
        self.rate_bps = target.clamp(self.min_bps, self.max_bps);
        self.noted_bps = self.rate_bps;
        self.probing.limit_known = true;
    }

    fn state(&self) -> RateState {
        self.phase.state()
    }

    fn on_net_limited(&mut self, cause: NetCause) {
        if self.phase.relaxed() {
            self.go(Phase::Tracking, &format!("网络受限：{}", cause.name()));
            self.leave_app_limited();
        }
    }

    fn probe_wanted(&mut self, now: Time) -> Option<ProbeAsk> {
        if self.probing.asked.is_some() {
            return None;
        }
        let rested = self
            .probing
            .last_end
            .is_none_or(|t| now.since(t) >= PROBE_RETRY_US);
        match self.phase {
            // 核实里上一轮证实了：接着探下一轮
            Phase::Verify { .. } => {
                let target = self.probing.next_target.take()?;
                self.ask(ProbeKind::Verify, target)
            }
            Phase::AppLimited { floor_since } => {
                // 周期探测上一轮证实了：接着探
                if let Some(target) = self.probing.next_target.take() {
                    return self.ask(ProbeKind::Periodic, target);
                }
                if !rested {
                    return None;
                }
                let goal = self
                    .probing
                    .drop
                    .map(|(before, _)| before.saturating_mul(VERIFY_TARGET_PERMILLE) / 1000);
                let first = self.rate_bps.saturating_mul(PROBE_FIRST_SCALE);
                // T1：降到降前的 66% 以下、之后排队低于 low 满一个往返、降后 5 秒内
                if let Some((before, at)) = self.probing.drop {
                    if now.since(at) > VERIFY_WITHIN_US {
                        self.probing.drop = None;
                    } else if self.rate_bps.saturating_mul(1000)
                        < before.saturating_mul(VERIFY_BELOW_PERMILLE)
                        && self
                            .probing
                            .calm_since
                            .is_some_and(|c| c >= at && now.since(c) >= self.probing.rtt_us)
                    {
                        let target = first.min(goal.unwrap_or(first)).min(self.max_bps);
                        if self.probe_fits(target) {
                            self.go(
                                Phase::Verify { since: now },
                                &format!(
                                    "应用受限时从 {} kbps 降下来了，队列排掉了，核实链路是不是真的变窄",
                                    before / 1000
                                ),
                            );
                            return self.ask(ProbeKind::Verify, target);
                        }
                    }
                }
                // 降过、还在等队列平静满一个往返：先不做周期探测，等核实
                if self.probing.drop.is_some_and(|(before, _)| {
                    self.rate_bps.saturating_mul(1000)
                        < before.saturating_mul(VERIFY_BELOW_PERMILLE)
                        && self.probe_fits(first.min(goal.unwrap_or(first)).min(self.max_bps))
                }) && floor_since.is_none()
                {
                    return None;
                }
                // I5：码率在底线停够了（不受窄带保护限制：停在底线不动比多发一组探测更糟；间隔翻倍）
                if floor_since.is_some_and(|t| now.since(t) >= self.probing.floor_gap_us) {
                    let target = first.min(goal.unwrap_or(first)).min(self.max_bps);
                    self.go(
                        Phase::Verify { since: now },
                        "应用受限时码率在底线停了一阵，核实链路是不是真的这么窄",
                    );
                    return self.ask(ProbeKind::Verify, target);
                }
                // T2：应用受限够久、距上次探测够久、码率明显低于已知的链路能力
                let long_enough = self
                    .probing
                    .app_since
                    .is_some_and(|t| now.since(t) >= PERIODIC_FIRST_US)
                    && self
                        .probing
                        .last_end
                        .is_none_or(|t| now.since(t) >= self.probing.periodic_gap_us);
                let low = !self.probing.limit_known
                    || self.ceiling_bps().is_some_and(|c| {
                        self.rate_bps.saturating_mul(1000)
                            < c.saturating_mul(PERIODIC_BELOW_PERMILLE)
                    });
                let target = self.rate_bps.saturating_mul(PROBE_STEP).min(self.max_bps);
                let calm = self.probing.calm_since.is_some();
                if long_enough && low && calm && target > self.rate_bps && self.probe_fits(target) {
                    return self.ask(ProbeKind::Periodic, target);
                }
                None
            }
            Phase::Startup | Phase::Tracking | Phase::Drain { .. } => None,
        }
    }

    fn on_probe_end(&mut self, now: Time, end: ProbeEnd) -> ProbeVerdict {
        let Some(ask) = self.probing.asked.take() else {
            return ProbeVerdict::Aborted;
        };
        self.probing.last_end = Some(now);
        let in_verify = matches!(self.phase, Phase::Verify { .. });
        let kind = ask.kind.name();
        let target = ask.target_bps;
        // 状态已经不是要探的那个状态了（降了码率进了排空，或者回了跟踪）：结果不用
        let stale = match ask.kind {
            ProbeKind::Verify => !in_verify,
            ProbeKind::Periodic => !matches!(self.phase, Phase::AppLimited { .. }),
        };
        let end = match end {
            ProbeEnd::Result(_) if stale => ProbeEnd::Aborted("状态变了"),
            ProbeEnd::Result(_) | ProbeEnd::TimedOut | ProbeEnd::Aborted(_) => end,
        };
        let from = self.rate_bps;
        let safety = u64::from(self.profile.safety_permille);
        let mut recheck = false;
        let (verdict, text) = match end {
            ProbeEnd::Result(result)
                if result.saturating_mul(1000) >= target.saturating_mul(PROBE_CONFIRM_PERMILLE) =>
            {
                self.capacity = Some((result.max(self.capacity.map_or(0, |(c, _)| c)), now));
                // 核实：降前的码率本来就是验证过的，探到了目标就回到目标（目标已经是降前的 85%）；
                // 周期探测是往没验证过的地方试，按结果打安全折扣
                let proven = match ask.kind {
                    ProbeKind::Verify => target.min(result),
                    ProbeKind::Periodic => target.min(result.saturating_mul(safety) / 1000),
                };
                self.rate_bps = self.rate_bps.max(proven).clamp(self.min_bps, self.max_bps);
                self.noted_bps = self.rate_bps;
                // 还没到头就接着探：核实探到降前的 85%，周期探到已知能力的七成以上
                let more = match ask.kind {
                    ProbeKind::Verify => self.probing.drop.is_some_and(|(before, _)| {
                        target < before.saturating_mul(VERIFY_TARGET_PERMILLE) / 1000
                    }),
                    ProbeKind::Periodic => self.rate_bps < self.max_bps,
                };
                let next = match (ask.kind, self.probing.drop) {
                    (ProbeKind::Verify, Some((before, _))) => result
                        .saturating_mul(PROBE_STEP)
                        .min(before.saturating_mul(VERIFY_TARGET_PERMILLE) / 1000),
                    (ProbeKind::Verify, None) | (ProbeKind::Periodic, _) => {
                        result.saturating_mul(PROBE_STEP)
                    }
                }
                .min(self.max_bps);
                if more
                    && self.probing.rounds < PROBE_MAX_ROUNDS
                    && next > target
                    && self.probe_fits(next)
                {
                    self.probing.next_target = Some(next);
                } else {
                    self.end_chain();
                }
                self.probing.periodic_gap_us = PERIODIC_FIRST_US;
                self.probing.floor_gap_us = FLOOR_VERIFY_US;
                // 证实了却没探上去多少（不到一成）：到头了
                if self.rate_bps.saturating_mul(10) < from.saturating_mul(11) {
                    self.probing.limit_known = true;
                    self.end_chain();
                }
                (
                    ProbeVerdict::Confirmed,
                    format!(
                        "按 {} kbps 探，测得 {} kbps，链路承载得了，码率 {} → {} kbps",
                        target / 1000,
                        result / 1000,
                        from / 1000,
                        self.rate_bps / 1000
                    ),
                )
            }
            // 核实第一次被否定：先不当真，过 200 ms 再探一次。一组探测只有 30 ms，碰上一次 Wi-Fi 延迟尖峰
            // 就测得很低（wifi-light 实测：30 Mbps 的链路上一组 25.5 Mbps 的探测只测出 8.2 Mbps，比当时畅通无阻的
            // 码率还低；按它“否定”之后码率在 9.5 Mbps 停了 10 秒）。连着两次都否定才算数。
            ProbeEnd::Result(result)
                if ask.kind == ProbeKind::Verify && in_verify && !self.probing.retried =>
            {
                self.end_chain();
                self.probing.retried = true;
                // 等复核：这一段还没了结
                recheck = true;
                (
                    ProbeVerdict::Denied,
                    format!(
                        "按 {} kbps 探，只测得 {} kbps；只有一组，可能碰上了延迟尖峰，过一会儿再探一次确认，码率不变",
                        target / 1000,
                        result / 1000
                    ),
                )
            }
            ProbeEnd::Result(result) => {
                self.end_chain();
                // 没探上去：记下测得的容量；明显比现在的码率还低（不到一半）才降（和静止探测的规则一样）
                self.capacity = Some((result, now));
                self.probing.limit_known = true;
                let target_rate = result.saturating_mul(safety) / 1000;
                if target_rate.saturating_mul(2) < from {
                    self.rate_bps = target_rate.clamp(self.min_bps, self.max_bps);
                    self.noted_bps = self.rate_bps;
                }
                (
                    ProbeVerdict::Denied,
                    format!(
                        "按 {} kbps 探，只测得 {} kbps，链路承载不了，码率 {} → {} kbps",
                        target / 1000,
                        result / 1000,
                        from / 1000,
                        self.rate_bps / 1000
                    ),
                )
            }
            ProbeEnd::TimedOut => {
                self.end_chain();
                (
                    ProbeVerdict::TimedOut,
                    format!("按 {} kbps 探，到时间没有结果，码率不变", target / 1000),
                )
            }
            ProbeEnd::Aborted(why) => {
                self.end_chain();
                (
                    ProbeVerdict::Aborted,
                    format!("按 {} kbps 探，{why}，结果不用，码率不变", target / 1000),
                )
            }
        };
        // 没探上去（否定、超时）：下次隔得更久。中止不算，等能探了再探
        if matches!(verdict, ProbeVerdict::Denied | ProbeVerdict::TimedOut) && !recheck {
            self.probing.periodic_gap_us = self
                .probing
                .periodic_gap_us
                .saturating_mul(2)
                .min(PERIODIC_MAX_US);
            self.probing.floor_gap_us = self
                .probing
                .floor_gap_us
                .saturating_mul(2)
                .min(PERIODIC_MAX_US);
        }
        self.note(format!("{kind}探测结论 {}：{text}", verdict.name()));
        // 核实有了结论（或者这一串探完了）：回应用受限。降码率的这一段到此了结（中止的留着，5 秒内能探了再核实）
        if in_verify && self.probing.next_target.is_none() {
            if verdict != ProbeVerdict::Aborted && !recheck {
                self.probing.drop = None;
            }
            self.go(
                Phase::AppLimited { floor_since: None },
                &format!("核实结论：{}", verdict.name()),
            );
        }
        verdict
    }

    fn set_startup(&mut self, on: bool, why: &str) {
        if on {
            self.go(Phase::Startup, why);
        } else if self.phase == Phase::Startup {
            self.go(Phase::Tracking, why);
        }
    }

    fn rate_bps(&self) -> u64 {
        self.rate_bps
    }

    fn profile(&self) -> &Profile {
        &self.profile
    }

    fn set_profile(&mut self, profile: Profile) {
        self.profile = profile;
    }

    fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    fn on_observation(&mut self, now: Time, obs: &Observation) {
        let p = self.profile;
        let fb = &obs.feedback;
        let recv_bps = u64::from(fb.recv_bytes_per_s).saturating_mul(8);
        let train_bps = u64::from(obs.ext.train_bytes_per_s).saturating_mul(8);
        if train_bps > 0 {
            self.capacity = Some((train_bps, now));
        }
        let capacity = self
            .capacity
            .filter(|(_, at)| now.since(*at) < CAPACITY_TTL_US)
            .map(|(c, _)| c);
        self.usage = self.detect(now, &obs.ext);
        // 拥塞：趋势过载，或者排队一直很高（趋势线可能还没攒够点）。
        // 丢包只有在排队也一直在时才算拥塞；单独的随机丢包交给纠错和重传，不降码率。
        // 发送端是不是在用满码率（没用满时送达速率只是“要发的量”，不代表链路能力；升码率时用）
        let using = recv_bps.saturating_mul(10) >= self.rate_bps.saturating_mul(6);
        // 排队读数只在反馈不断档时才算数：和上一份反馈相隔不超过 [`CONTIGUOUS_US`]，说明这段时间包一直在到，
        // 排队读数是链路在忙时量的，这时的送达速率也就是链路能力，按它降不会过头。断档的那份（画面静止时零星的小包）
        // 不算：它的排队读数是 Wi-Fi 省电（空闲后网卡要醒）的延迟，那份反馈里只有一两个包、送达速率被空闲时间稀释。
        // 2026-11-26 用户日志（Wi-Fi、30 Mbps）：趋势正常、零星小包排队 12–25 ms、送达 13 kbps，按它降一下就到底
        // （30 Mbps → 100 kbps）。要求连着几份都高（不管断不断档）在窄带上反应太慢：微软轨迹 4 条 × 3 个种子，
        // 连着 3 份时卡顿 31 → 58、2 份 35；只要求不断档 31（和改前一样），大屏 Wi-Fi 场景码率也不再塌。
        let contiguous = self
            .last_feedback
            .is_some_and(|t| now.since(t) <= CONTIGUOUS_US);
        self.last_feedback = Some(now);
        let standing = self.standing_queue(
            now,
            fb.queue_delay_us,
            contiguous,
            STANDING_US.max(obs.rtt_us),
        );
        self.probing.rtt_us = obs.rtt_us;
        // 这份反馈里的送达速率是不是链路的能力；不是的话，它的排队读数记进“链路自己的抖动”
        let link_limited = self.link_limited(now, using, obs);
        // 抖动只认“很快回落的高读数”，持续一个窗口的是队列（见 `Jitter::reading`）
        let window_us = STANDING_US.max(obs.rtt_us);
        if !link_limited
            && self
                .jitter
                .reading(now, fb.queue_delay_us, contiguous, window_us)
        {
            let (low, high) = self.delay_limits();
            self.note(format!(
                "延迟门限换成 low {} ms、high {} ms：{}",
                low / 1000,
                high / 1000,
                if self.jitter.on {
                    "没在用链路的时候排队读数也常常超过 12 ms，是链路自己的抖动（Wi-Fi 一类）"
                } else {
                    "最近没在用链路时的排队读数都很低，链路是稳的"
                }
            ));
        }
        let (low_us, high_us) = self.delay_limits();
        // 平静：排队低于 low。断档的那份反馈不算数（和判拥塞时一样：零星小包的读数是 Wi-Fi 省电的延迟，
        // 几乎每份都是十几毫秒；算上的话轻操作时永远“不平静”，核实要干等——大屏 Wi-Fi 场景实测等了 2 秒）
        if fb.queue_delay_us < low_us || !contiguous {
            self.probing.calm_since.get_or_insert(now);
        } else {
            self.probing.calm_since = None;
        }
        // 以上是各状态共用的测量（和状态无关，每份反馈都一样算）。下面先走状态机的出口，再按状态判断。
        self.step(now, obs);
        let relaxed_phase = self.phase.relaxed();
        // 放宽（只认常驻队列、一次最多降一半）：状态是应用受限一类，或者这份样本不是链路受限的（跟踪状态下也一样：
        // 送达速率不代表链路能力时，不能按它降）。
        // 应用受限时自己的流量堆不出队列，单次高读数多半是 Wi-Fi 的延迟尖峰，要常驻（持续一个窗口）才算；
        // 链路受限时照旧一份就算：窄带上要求持续会让真拥塞反应慢（连着 3 份才算时卡顿 31 → 58）。
        let relaxed = relaxed_phase || !link_limited;
        let queue_us = if relaxed {
            standing.unwrap_or(0)
        } else {
            fb.queue_delay_us
        };
        // 常驻队列（一个窗口里的最小值）已经把尖峰滤掉了，和画像本身的门限比；单份读数才按链路的抖动放宽
        let (low_cmp, high_cmp) = if relaxed {
            (p.low_delay_us, p.high_delay_us)
        } else {
            (low_us, high_us)
        };
        let queue_high = queue_us > high_cmp && contiguous;
        // 固有丢包慢慢跟（每份反馈 1/128，反馈 20 ms 一次时约 2.5 秒）
        let loss = u64::from(fb.loss_permille).saturating_mul(1000);
        self.inherent_loss = self.inherent_loss - self.inherent_loss / 128 + loss / 128;
        let inherent = u16::try_from(self.inherent_loss / 1000).unwrap_or(u16::MAX);
        // 丢包也按上面的读数：应用受限时一份反馈里只有几个包，丢一个就是几百‰（用户日志：2 个包丢 1 个，500‰），
        // 没有常驻队列就不算
        let lossy_and_queued = fb.loss_permille > p.loss_permille.saturating_add(inherent)
            && queue_us > low_cmp
            && contiguous;
        let congested = self.usage == Usage::Over || queue_high || lossy_and_queued;
        // 一个往返（至少 100 ms）之内只降一次：同一次拥塞的反馈会连着来好几条。
        let hold = obs.rtt_us.max(100_000);
        if congested {
            if self.last_decrease.is_none_or(|t| now.since(t) >= hold) {
                // 和 WebRTC GCC 一样：降到实测送达速率的一定比例，已经比它低就不再降。
                // 以前是“当前码率和送达速率取小再打折”，每个往返都在已经很低的码率上再打折：
                // 400 kbps 的链路上送达一直是 470 kbps，码率却一路降到底线 100 kbps，
                // 重传额度跟着只剩不到一个包，大关键帧永远补不齐（仿真和真实二进制都是 0 帧）。
                let target = if recv_bps > 0 {
                    recv_bps.saturating_mul(u64::from(p.decrease_permille)) / 1000
                } else {
                    self.rate_bps.saturating_mul(u64::from(p.decrease_permille)) / 1000
                };
                // 应用受限时送达速率只是“要发的量”，不是链路能力：一次最多降一半（每个往返一次，真拥塞时
                // 几个往返也就降下来了），不一下降到要发的量的 85%（轻操作时就是底线）
                let target = if relaxed {
                    target.max(self.rate_bps / 2)
                } else {
                    target
                };
                let from_bps = self.rate_bps;
                self.rate_bps = self.rate_bps.min(target).clamp(self.min_bps, self.max_bps);
                self.last_decrease = Some(now);
                // 码率真的降了才写（已经不比目标高时码率不变，以前也写一条“850 → 850”，日志里一长串没用的说明）
                if self.rate_bps < from_bps && self.notes.len() < MAX_NOTES {
                    self.notes.push(format!(
                        "降码率 {} → {} kbps：趋势{}（{} ms，门限 {} ms），排队 {} ms，丢包 {}‰，\
                         送达 {} kbps（{}），包串测得容量 {}",
                        from_bps / 1000,
                        self.rate_bps / 1000,
                        if self.usage == Usage::Over {
                            "过载"
                        } else {
                            "正常"
                        },
                        tenths(i64::from(obs.ext.trend_tenth_ms)),
                        tenths(self.threshold),
                        fb.queue_delay_us / 1000,
                        fb.loss_permille,
                        recv_bps / 1000,
                        if using {
                            "发送端用满了码率"
                        } else {
                            "发送端没用满码率"
                        },
                        capacity
                            .map_or_else(|| "未知".to_owned(), |c| format!("{} kbps", c / 1000)),
                    ));
                }
                self.noted_bps = self.rate_bps;
                // 应用受限时降的：记下这一段是从多少降下来的（之后核实用，见 `Probing`）；一串探测到此为止
                // 跟踪里降、但当时发送端没用满码率的也记：送达速率那一刻只是“要发的量”（一阵流量刚收尾），按它降
                // 多半降过头了；紧接着要是进了应用受限，就没有流量把码率带回去，得靠核实。
                // （wifi-light 实测：大屏开头的流量收尾时 30 → 9.5 Mbps，之后轻操作 5 秒都停在 9.5 Mbps。）
                // 用满了码率时降的是真凭实据，之前记的那一段作废。跟踪怎么降不受这里影响（I3）。
                if relaxed || !using {
                    let before = self
                        .probing
                        .drop
                        .filter(|(_, at)| now.since(*at) <= VERIFY_WITHIN_US)
                        .map_or(from_bps, |(b, _)| b);
                    self.probing.drop = Some((before, now));
                    self.end_chain();
                    self.probing.retried = false;
                } else {
                    self.probing.drop = None;
                }
                // 降了：进排空（起步期间不切，起步什么时候结束由发送端的探测决定）
                if self.phase != Phase::Startup {
                    self.go(
                        Phase::Drain {
                            from_app: relaxed_phase,
                            since: now,
                        },
                        if relaxed_phase {
                            "应用受限时降了码率（常驻队列或趋势过载）"
                        } else {
                            "降了码率，等队列排掉"
                        },
                    );
                }
            }
            return;
        }
        // 排队在消退，或者（不断档的反馈里）还有排队：先不升。断档的那份不算：Wi-Fi 省电下零星的帧几乎每份都是
        // 12–25 ms（空闲后网卡要醒），算上的话码率永远升不上去（仿真：大屏 Wi-Fi 一直停在 1.36 Mbps）。
        if self.usage == Usage::Under || (fb.queue_delay_us >= low_us && contiguous) {
            return;
        }
        // 应用受限（和它的核实、排空）不自己升：送达速率只是要发的量，证明不了链路能承载更多（升靠探测结论）
        if relaxed {
            return;
        }
        // 只有真在用这个码率时才升：屏幕静止时没有数据，估计冻结，不凭空上涨。
        if !using {
            return;
        }
        // 知道容量时快速逼近它的一部分（比例按网络画像：稳定链路九成，蜂窝七成——抖动会让包串
        // 测得偏高，要给满屏运动留余量）；不知道时按比例慢慢升。
        let target = capacity.map(|c| c.saturating_mul(u64::from(p.safety_permille)) / 1000);
        let step = match target {
            Some(t) if self.rate_bps < t => (t - self.rate_bps) / 2,
            Some(_) | None => self.rate_bps.saturating_mul(u64::from(p.increase_permille)) / 1000,
        };
        self.rate_bps = self
            .rate_bps
            .saturating_add(step.max(1))
            .clamp(self.min_bps, self.max_bps);
        // 累计升了四分之一以上才写一次说明（每次反馈都在小步升，全写会刷屏）
        if self.rate_bps.saturating_mul(4) >= self.noted_bps.saturating_mul(5)
            && self.notes.len() < MAX_NOTES
        {
            self.notes.push(format!(
                "升码率 {} → {} kbps：{}，送达 {} kbps，排队 {} ms",
                self.noted_bps / 1000,
                self.rate_bps / 1000,
                target.map_or_else(
                    || "不知道链路容量，按比例慢慢升".to_owned(),
                    |t| format!(
                        "包串测得容量，逼近它的 {}‰（{} kbps）",
                        p.safety_permille,
                        t / 1000
                    )
                ),
                recv_bps / 1000,
                fb.queue_delay_us / 1000,
            ));
            self.noted_bps = self.rate_bps;
        }
    }
}

/// 毫秒的十分之一写成“1.5”这样的字符串。
fn tenths(v: i64) -> String {
    format!(
        "{}{}.{}",
        if v < 0 { "-" } else { "" },
        v.unsigned_abs() / 10,
        v.unsigned_abs() % 10
    )
}

/// 最多攒多少条说明（会话每秒取一次，正常远不到）。
const MAX_NOTES: usize = 16;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn fb(queue_us: u32, loss: u16, recv_bytes: u32) -> Feedback {
        Feedback {
            floor: 0,
            delivered: 0,
            queue_delay_us: queue_us,
            loss_permille: loss,
            recv_bytes_per_s: recv_bytes,
        }
    }

    fn obs(feedback: Feedback, ext: Extension) -> Observation {
        Observation {
            feedback,
            ext,
            rtt_us: 20_000,
            in_flight: 0,
            limit: SendLimit::default(),
            backlog: false,
        }
    }

    #[test]
    fn rate_states_have_names_and_a_fixed_order() {
        for (i, state) in RateState::ALL.iter().enumerate() {
            assert_eq!(state.index(), i);
            assert!(!state.name().is_empty());
        }
        assert_eq!(RateState::default(), RateState::Tracking);
        // 还没有显式状态机的估计器一律报告“跟踪”
        let v = ClassicV1::new(Profile::STABLE, 1_000_000, 100_000, 10_000_000);
        assert_eq!(v.state(), RateState::Tracking);
        // 没有状态机的估计器：起步、网络受限的通知都不理，一直报告“跟踪”
        let mut old = EstimatorKind::Legacy.build(Profile::STABLE, 1_000_000, 100_000, 10_000_000);
        old.set_startup(true, "测试");
        old.on_net_limited(NetCause::FrameBlocked);
        assert_eq!(old.state(), RateState::Tracking);
    }

    // ---- 状态机（每个状态的每条出口、不变量 I1–I5）----

    const FLAT: Extension = Extension {
        trend_tenth_ms: 0,
        train_bytes_per_s: 0,
        groups: 10,
    };
    const OVER: Extension = Extension {
        trend_tenth_ms: 3000,
        train_bytes_per_s: 0,
        groups: 20,
    };
    const NET: SendLimit = SendLimit::Network(NetCause::FrameNearBudget);

    /// 每 20 ms 喂一份反馈（不断档）。
    struct Feed {
        v: ClassicV1,
        t: u64,
    }

    impl Feed {
        fn new(profile: Profile, start_bps: u64) -> Self {
            Self {
                v: ClassicV1::new(profile, start_bps, 100_000, 50_000_000),
                t: 0,
            }
        }
        fn put(&mut self, queue: u32, loss: u16, recv: u32, ext: Extension, limit: SendLimit) {
            self.t += 20_000;
            let o = Observation {
                limit,
                ..obs(fb(queue, loss, recv), ext)
            };
            self.v.on_observation(Time(self.t), &o);
        }
        /// 同 [`Self::put`]，但发送端这一刻有积压（有东西等着发、被网络挡着）。
        fn put_busy(&mut self, queue: u32, loss: u16, recv: u32, ext: Extension, limit: SendLimit) {
            self.t += 20_000;
            let o = Observation {
                limit,
                backlog: true,
                ..obs(fb(queue, loss, recv), ext)
            };
            self.v.on_observation(Time(self.t), &o);
        }
        /// 进入应用受限（先喂一份平静的反馈）。
        fn app_limited(profile: Profile, start_bps: u64) -> Self {
            let mut f = Self::new(profile, start_bps);
            f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
            assert_eq!(f.v.state(), RateState::AppLimited);
            f
        }
        fn notes(&mut self) -> String {
            self.v.take_notes().join("\n")
        }
    }

    #[test]
    fn startup_ends_only_when_the_sender_says_so() {
        let mut f = Feed::new(Profile::STABLE, 2_400_000);
        f.v.set_startup(false, "没在起步");
        assert_eq!(f.v.state(), RateState::Tracking, "不在起步时结束起步不做事");
        f.v.set_startup(true, "会话开头");
        assert_eq!(f.v.state(), RateState::Startup);
        // 起步里发送端报应用受限、出现网络受限的原因：都不切
        f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        f.v.on_net_limited(NetCause::KeyPending);
        assert_eq!(f.v.state(), RateState::Startup);
        // 起步里过载照样降（I1），但不进排空：起步什么时候结束由探测决定
        f.put(20_000, 0, 250_000, FLAT, NET);
        assert_eq!(f.v.rate_bps(), 1_700_000);
        assert_eq!(f.v.state(), RateState::Startup);
        // 探测结论：码率按它定（I2 的第三处），然后发送端宣布结束
        f.v.on_start_probe(Time(f.t), 10_000_000);
        assert_eq!(f.v.rate_bps(), 9_000_000);
        f.v.set_startup(false, "起步探测有结论了");
        assert_eq!(f.v.state(), RateState::Tracking);
        let notes = f.notes();
        assert!(notes.contains("码率状态 跟踪 → 起步：会话开头"), "{notes}");
        assert!(
            notes.contains("码率状态 起步 → 跟踪：起步探测有结论了"),
            "{notes}"
        );
    }

    #[test]
    fn tracking_leaves_on_a_cut_or_when_the_sender_is_app_limited() {
        // 出口一：降了 → 排空
        let mut f = Feed::new(Profile::STABLE, 2_400_000);
        f.put(1_000, 0, 250_000, FLAT, NET);
        assert_eq!(f.v.state(), RateState::Tracking);
        f.put(20_000, 0, 250_000, FLAT, NET);
        assert_eq!(f.v.rate_bps(), 1_700_000);
        assert_eq!(f.v.state(), RateState::Drain);
        assert!(f.notes().contains("码率状态 跟踪 → 排空（跟踪）：降了码率"));
        // 出口二：发送端报应用受限 → 应用受限
        let mut g = Feed::new(Profile::STABLE, 2_400_000);
        g.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(g.v.state(), RateState::AppLimited);
        assert!(g
            .notes()
            .contains("码率状态 跟踪 → 应用受限：发送端 500 ms"));
        // 网络受限的原因出现在跟踪里：不切，也不写日志
        g.v.on_net_limited(NetCause::FrameBlocked);
        g.v.on_net_limited(NetCause::FrameBlocked);
        assert_eq!(g.notes().matches("码率状态").count(), 1);
    }

    #[test]
    fn drain_from_tracking_returns_when_the_queue_empties_or_after_the_time_limit() {
        // 出口一：排队低于 low → 跟踪
        let mut f = Feed::new(Profile::STABLE, 2_400_000);
        f.put(1_000, 0, 250_000, FLAT, NET);
        f.put(20_000, 0, 250_000, FLAT, NET);
        assert_eq!(f.v.state(), RateState::Drain);
        f.put(5_000, 0, 250_000, FLAT, NET);
        assert_eq!(f.v.state(), RateState::Drain);
        f.put(1_000, 0, 250_000, FLAT, NET);
        assert_eq!(f.v.state(), RateState::Tracking);
        assert!(f
            .notes()
            .contains("码率状态 排空（跟踪） → 跟踪：队列排掉了（排队 1 ms，低于 3 ms）"));
        f.put(1_000, 0, 250_000, FLAT, SendLimit::App);
        assert_eq!(f.v.state(), RateState::AppLimited);
        // 出口三：排空期间发送端报应用受限 → 直接进应用受限（这种排空只是标记，规则就是跟踪）
        let mut e = Feed::new(Profile::STABLE, 2_400_000);
        e.put(1_000, 0, 250_000, FLAT, NET);
        e.put(20_000, 0, 250_000, FLAT, NET);
        assert_eq!(e.v.state(), RateState::Drain);
        e.put(5_000, 0, 250_000, FLAT, SendLimit::App);
        assert_eq!(e.v.state(), RateState::AppLimited);
        // 出口二：排队一直不低于 low，到 1 秒 → 跟踪。一个往返（至少 100 ms）过后还过载就再降，排空重新计时
        let mut g = Feed::new(Profile::STABLE, 2_400_000);
        g.put(1_000, 0, 250_000, FLAT, NET);
        g.put(20_000, 0, 250_000, FLAT, NET);
        let cut_at = g.t;
        for _ in 0..4 {
            g.put(20_000, 0, 125_000, FLAT, NET);
        }
        assert_eq!(
            g.v.rate_bps(),
            1_700_000,
            "一个往返（至少 100 ms）之内只降一次"
        );
        g.put(20_000, 0, 125_000, FLAT, NET);
        assert_eq!(g.v.rate_bps(), 850_000, "过了一个往返还过载：再降（I1）");
        let again_at = g.t;
        assert_eq!(again_at - cut_at, 100_000);
        // 之后排队在 low 和 high 之间：不降不升，也排不掉
        while g.t < again_at + 980_000 {
            g.put(5_000, 0, 125_000, FLAT, NET);
            assert_eq!(g.v.state(), RateState::Drain, "{} ms", g.t / 1000);
        }
        g.put(5_000, 0, 125_000, FLAT, NET);
        assert_eq!(g.v.state(), RateState::Tracking);
        assert!(g
            .notes()
            .contains("排空（跟踪） → 跟踪：排空到时限了，排队还有 5 ms"));
        assert_eq!(g.v.rate_bps(), 850_000);
        // 往返很长时时限是 4 个往返
        let mut h = Feed::new(Profile::STABLE, 2_400_000);
        let slow = |h: &mut Feed, queue| {
            h.t += 20_000;
            let o = Observation {
                rtt_us: 400_000,
                ..obs(fb(queue, 0, 250_000), FLAT)
            };
            h.v.on_observation(Time(h.t), &o);
        };
        slow(&mut h, 1_000);
        slow(&mut h, 20_000);
        let cut_at = h.t;
        while h.t < cut_at + 1_580_000 {
            slow(&mut h, 5_000);
            assert_eq!(h.v.state(), RateState::Drain);
        }
        slow(&mut h, 5_000);
        assert_eq!(h.v.state(), RateState::Tracking);
    }

    #[test]
    fn app_limited_ignores_spikes_and_cuts_at_most_half_on_a_standing_queue() {
        // 2026-11-26 用户日志：轻操作时一次 Wi-Fi 尖峰把码率从 17 Mbps 降到底线，画面 6 秒不动
        let mut f = Feed::app_limited(Profile::STABLE, 30_000_000);
        for _ in 0..10 {
            f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        }
        // 一阵尖峰：三份排队 25 ms，其中一份 2 个包丢 1 个
        f.put(25_000, 0, 25_000, FLAT, SendLimit::App);
        f.put(25_000, 500, 25_000, FLAT, SendLimit::App);
        f.put(25_000, 0, 25_000, FLAT, SendLimit::App);
        for _ in 0..10 {
            f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        }
        assert_eq!(f.v.rate_bps(), 30_000_000, "尖峰不是常驻队列");
        assert_eq!(f.v.state(), RateState::AppLimited);
        // 断档以后重新开始攒：刚开始的几份不够 100 ms，排队再高也不算
        f.t += 1_000_000;
        for _ in 0..5 {
            f.put(30_000, 0, 25_000, FLAT, SendLimit::App);
        }
        assert_eq!(f.v.rate_bps(), 30_000_000, "不断档的反馈还不到 100 ms");
        // 出口：排队持续 100 ms 以上是常驻队列，降，但一次最多降一半 → 排空
        f.put(30_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(f.v.rate_bps(), 15_000_000);
        assert_eq!(f.v.state(), RateState::DrainApp);
        assert!(f
            .notes()
            .contains("应用受限 → 排空（应用受限）：应用受限时降了码率"));
        for _ in 0..4 {
            f.put(30_000, 0, 25_000, FLAT, SendLimit::App);
        }
        assert_eq!(f.v.rate_bps(), 15_000_000, "一个往返之内只降一次");
        f.put(30_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(f.v.rate_bps(), 7_500_000, "队列还在：下一个往返再降一半");
        // 丢包明显、常驻队列高于 low：也算拥塞
        let mut g = Feed::app_limited(Profile::STABLE, 30_000_000);
        for _ in 0..6 {
            g.put(5_000, 300, 25_000, FLAT, SendLimit::App);
        }
        assert_eq!(g.v.rate_bps(), 15_000_000);
        // 趋势过载：不用等常驻队列，马上降一半
        let mut h = Feed::app_limited(Profile::STABLE, 30_000_000);
        h.put(0, 0, 25_000, OVER, SendLimit::App);
        assert_eq!(h.v.rate_bps(), 15_000_000);
        assert_eq!(h.v.state(), RateState::DrainApp);
        // 往返比 100 ms 长时，常驻队列的窗口按往返
        let mut k = Feed::app_limited(Profile::STABLE, 30_000_000);
        for i in 0..12 {
            k.t += 20_000;
            let o = Observation {
                rtt_us: 200_000,
                limit: SendLimit::App,
                ..obs(fb(30_000, 0, 25_000), FLAT)
            };
            k.v.on_observation(Time(k.t), &o);
            // 进应用受限时喂过一份低读数：要等它滑出 200 ms 的窗口（之后的第 11 份），最小值才高
            assert_eq!(k.v.rate_bps() < 30_000_000, i >= 10, "第 {i} 份");
        }
    }

    #[test]
    fn app_limited_returns_to_tracking_on_any_network_limit() {
        // 出口：下一份反馈报网络受限
        let mut f = Feed::app_limited(Profile::STABLE, 30_000_000);
        f.put(
            1_000,
            0,
            25_000,
            FLAT,
            SendLimit::Network(NetCause::ResendCapped),
        );
        assert_eq!(f.v.state(), RateState::Tracking);
        assert!(f
            .notes()
            .contains("应用受限 → 跟踪：网络受限：重传被额度截断"));
        // 出口：不等反馈，原因一出现就退出（进慢出快）
        let mut g = Feed::app_limited(Profile::STABLE, 30_000_000);
        g.v.on_net_limited(NetCause::FrameBlocked);
        assert_eq!(g.v.state(), RateState::Tracking);
        assert!(g
            .notes()
            .contains("应用受限 → 跟踪：网络受限：该编帧时在途数据满了或上一帧没发完"));
        // 回到跟踪以后：送达只有码率的一成（静止时要发的量），单独一份高读数不按它降——2026-11-27 Mac 日志里
        // 这样降了 496 次、317 次降到 1 Mbps 以下（`docs/research/P-fps-bitrate.md` M1）
        g.put(20_000, 0, 250_000, FLAT, NET);
        assert_eq!(
            g.v.rate_bps(),
            30_000_000,
            "没证明链路一直忙，不按送达速率降"
        );
        // 发送端连着一个窗口都有积压（链路一直忙着）：这时的送达速率是链路的能力，高读数按它降，不再“最多一半”
        let mut h = Feed::app_limited(Profile::STABLE, 30_000_000);
        h.v.on_net_limited(NetCause::FrameBlocked);
        for _ in 0..6 {
            h.put_busy(1_000, 0, 250_000, FLAT, NET);
        }
        h.put_busy(20_000, 0, 250_000, FLAT, NET);
        assert_eq!(h.v.rate_bps(), 1_700_000);
        // 每个原因都有说明
        for cause in [
            NetCause::Unknown,
            NetCause::Start,
            NetCause::FrameBlocked,
            NetCause::KeyPending,
            NetCause::FrameNearBudget,
            NetCause::FpsReduced,
            NetCause::ScaleReduced,
            NetCause::ResendCapped,
            NetCause::EncoderDropped,
            NetCause::UsingRate,
        ] {
            assert!(!cause.name().is_empty());
        }
        assert_eq!(SendLimit::default(), SendLimit::Network(NetCause::Unknown));
    }

    #[test]
    fn drain_from_app_limited_goes_back_or_to_tracking() {
        let cut = || {
            let mut f = Feed::app_limited(Profile::STABLE, 30_000_000);
            f.put(0, 0, 25_000, OVER, SendLimit::App);
            assert_eq!(
                (f.v.state(), f.v.rate_bps()),
                (RateState::DrainApp, 15_000_000)
            );
            f.notes();
            f
        };
        // 出口一：排队低于 low → 回应用受限
        let mut f = cut();
        f.put(5_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(f.v.state(), RateState::DrainApp);
        f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(f.v.state(), RateState::AppLimited);
        assert!(f
            .notes()
            .contains("排空（应用受限） → 应用受限：队列排掉了"));
        // 出口二：下一份反馈报网络受限 → 跟踪
        let mut g = cut();
        g.put(5_000, 0, 25_000, FLAT, NET);
        assert_eq!(g.v.state(), RateState::Tracking);
        // 出口三：原因一出现就 → 跟踪
        let mut h = cut();
        h.v.on_net_limited(NetCause::KeyPending);
        assert_eq!(h.v.state(), RateState::Tracking);
        assert!(h
            .notes()
            .contains("排空（应用受限） → 跟踪：网络受限：关键帧还没确认"));
        // 出口四：1 秒还没排掉 → 跟踪
        let mut k = cut();
        let cut_at = k.t;
        while k.t < cut_at + 980_000 {
            k.put(5_000, 0, 25_000, FLAT, SendLimit::App);
            assert_eq!(k.v.state(), RateState::DrainApp);
        }
        k.put(5_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(k.v.state(), RateState::Tracking);
        assert_eq!(k.v.rate_bps(), 15_000_000, "排空里不升");
    }

    // ---- 运行中探测：触发（T1 核实、I5 底线核实、T2 周期）和结论 ----

    /// 应用受限、码率 `rate`、已知容量 `capacity`。
    fn probing(rate: u64, capacity: Option<u64>) -> Feed {
        let mut f = Feed::app_limited(Profile::STABLE, rate);
        f.v.capacity = capacity.map(|c| (c, Time(0)));
        f.notes();
        f
    }

    /// 应用受限时常驻队列把码率降一半 `times` 次，然后队列排掉、平静一个往返。
    fn cut(f: &mut Feed, times: u32) {
        for _ in 0..times {
            let before = f.v.rate_bps();
            for _ in 0..500 {
                if f.v.rate_bps() != before {
                    break;
                }
                f.put(30_000, 0, 25_000, FLAT, SendLimit::App);
            }
            assert_ne!(
                f.v.rate_bps(),
                before,
                "常驻队列 30 ms 持续 10 秒，应用受限时也该降（最多一半）"
            );
        }
        assert_eq!(f.v.state(), RateState::DrainApp);
        assert_eq!(f.v.probe_wanted(Time(f.t)), None, "排空里不探");
        f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(f.v.state(), RateState::AppLimited);
        assert_eq!(
            f.v.probe_wanted(Time(f.t)),
            None,
            "排队低于 low 还没满一个往返"
        );
        f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
    }

    #[test]
    fn t1_a_cut_while_app_limited_is_verified_and_a_false_one_is_undone() {
        let mut f = probing(30_000_000, Some(30_000_000));
        cut(&mut f, 1);
        assert_eq!(f.v.rate_bps(), 15_000_000);
        // 目标是降前的 85%（没超过当前码率的 4 倍）
        let ask = f.v.probe_wanted(Time(f.t)).unwrap();
        assert_eq!(
            ask,
            ProbeAsk {
                kind: ProbeKind::Verify,
                target_bps: 25_500_000
            }
        );
        assert_eq!(f.v.state(), RateState::Verify);
        assert_eq!(f.v.probe_wanted(Time(f.t)), None, "这一轮还没回报");
        // 证实：码率 = min(目标, 结果)，回应用受限
        f.t += 100_000;
        assert_eq!(
            f.v.on_probe_end(Time(f.t), ProbeEnd::Result(25_000_000)),
            ProbeVerdict::Confirmed
        );
        assert_eq!(f.v.rate_bps(), 25_000_000);
        assert_eq!(f.v.state(), RateState::AppLimited);
        let notes = f.notes();
        assert!(
            notes.contains("应用受限 → 核实：应用受限时从 30000 kbps 降下来了"),
            "{notes}"
        );
        assert!(
            notes.contains(
                "核实探测结论 证实：按 25500 kbps 探，测得 25000 kbps，链路承载得了，码率 15000 → 25000 kbps"
            ),
            "{notes}"
        );
        assert!(notes.contains("核实 → 应用受限：核实结论：证实"), "{notes}");
        // 这一段了结了：不再核实
        f.t += 1_000_000;
        assert_eq!(f.v.probe_wanted(Time(f.t)), None);
        // 没有要等的探测时回报结束：中止，什么都不动
        assert_eq!(
            f.v.on_probe_end(Time(f.t), ProbeEnd::Result(1)),
            ProbeVerdict::Aborted
        );
        assert_eq!(f.v.rate_bps(), 25_000_000);
    }

    #[test]
    fn t1_after_a_deep_cut_the_first_probe_is_at_most_four_times_the_rate() {
        // 30 → 15 → 7.5 → 3.75 Mbps：第一轮只探 15 Mbps（4 倍），证实了再按结果的 2 倍，到降前的 85% 为止
        let mut f = probing(30_000_000, Some(30_000_000));
        cut(&mut f, 3);
        assert_eq!(f.v.rate_bps(), 3_750_000);
        let ask = f.v.probe_wanted(Time(f.t)).unwrap();
        assert_eq!(ask.target_bps, 15_000_000);
        f.t += 100_000;
        assert_eq!(
            f.v.on_probe_end(Time(f.t), ProbeEnd::Result(15_000_000)),
            ProbeVerdict::Confirmed
        );
        assert_eq!(f.v.rate_bps(), 15_000_000);
        assert_eq!(f.v.state(), RateState::Verify, "还没到降前的 85%，接着探");
        let ask = f.v.probe_wanted(Time(f.t)).unwrap();
        assert_eq!((ask.kind, ask.target_bps), (ProbeKind::Verify, 25_500_000));
        f.t += 100_000;
        f.v.on_probe_end(Time(f.t), ProbeEnd::Result(25_500_000));
        assert_eq!(f.v.rate_bps(), 25_500_000);
        assert_eq!(f.v.state(), RateState::AppLimited);
        assert_eq!(f.notes().matches("核实探测结论 证实").count(), 2);
    }

    #[test]
    fn t1_a_real_drop_is_denied_and_the_rate_does_not_bounce_back() {
        // 探一轮、被否定（测得 `result`）
        let denied = |f: &mut Feed, result: u64| {
            let ask = f.v.probe_wanted(Time(f.t)).unwrap();
            assert_eq!(f.v.state(), RateState::Verify);
            f.t += 100_000;
            assert_eq!(
                f.v.on_probe_end(Time(f.t), ProbeEnd::Result(result)),
                ProbeVerdict::Denied
            );
            assert_eq!(f.v.state(), RateState::AppLimited);
            ask
        };
        // 链路真的从 30 降到了 3 Mbps
        let mut f = probing(30_000_000, Some(30_000_000));
        cut(&mut f, 3);
        // 第一次否定先不当真（一组探测只有 30 ms，可能碰上了延迟尖峰）：容量不记、码率不动，200 ms 后复核
        denied(&mut f, 3_000_000);
        assert_eq!(f.v.rate_bps(), 3_750_000);
        assert_eq!(f.v.capacity.map(|(c, _)| c), Some(30_000_000));
        assert!(f.notes().contains("过一会儿再探一次确认"));
        assert_eq!(f.v.probe_wanted(Time(f.t + 100_000)), None);
        f.t += 200_000;
        // 第二次还是否定：算数
        let ask = denied(&mut f, 3_000_000);
        assert_eq!(
            f.v.rate_bps(),
            3_750_000,
            "不升；也没低到要降（不到一半才降）"
        );
        assert!(f.notes().contains(&format!(
            "核实探测结论 否定：按 {} kbps 探，只测得 3000 kbps，链路承载不了，码率 3750 → 3750 kbps",
            ask.target_bps / 1000
        )));
        // 记下了容量 3 Mbps：之后任何一组探测（至少 19 KB）都超过它 50 ms 的量，不再探
        f.t += 60_000_000;
        f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(f.v.probe_wanted(Time(f.t)), None);
        // 第一次否定、复核证实（第一次是碰上了尖峰）：码率回去
        let mut h = probing(30_000_000, Some(30_000_000));
        cut(&mut h, 1);
        denied(&mut h, 8_000_000);
        h.t += 200_000;
        h.v.probe_wanted(Time(h.t)).unwrap();
        assert_eq!(
            h.v.on_probe_end(Time(h.t), ProbeEnd::Result(25_000_000)),
            ProbeVerdict::Confirmed
        );
        assert_eq!(h.v.rate_bps(), 25_000_000);
        // 两次都测得明显比码率还低：按它降
        let mut g = probing(30_000_000, Some(30_000_000));
        cut(&mut g, 3);
        denied(&mut g, 1_000_000);
        assert_eq!(g.v.rate_bps(), 3_750_000);
        g.t += 200_000;
        denied(&mut g, 1_000_000);
        assert_eq!(g.v.rate_bps(), 900_000);
    }

    /// 跟踪里一阵流量刚收尾（发送端没用满码率，送达速率只是要发的量），一份高读数不降（#71 第 2 步：
    /// 以前按它降到 9.52 Mbps，再靠核实拉回来；wifi-light 实测大屏开头 30 → 9.5 Mbps，之后 5 秒停在那里）。
    /// 用满了码率时降的是真凭实据，照降，也不核实。
    #[test]
    fn t1_a_tapering_flow_is_not_cut_and_a_cut_while_using_the_rate_is_not_verified() {
        let mut f = Feed::new(Profile::STABLE, 30_000_000);
        f.v.capacity = Some((30_000_000, Time(0)));
        f.put(1_000, 0, 3_500_000, FLAT, NET);
        let before = f.v.rate_bps();
        // 送达 11.2 Mbps（不到码率的六成），一份排队 18 ms：流量收尾，不降
        f.put(18_000, 0, 1_400_000, FLAT, NET);
        assert_eq!((f.v.state(), f.v.rate_bps()), (RateState::Tracking, before));
        f.put(1_000, 0, 25_000, FLAT, NET);
        f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(f.v.state(), RateState::AppLimited);
        assert_eq!(f.v.probe_wanted(Time(f.t)), None, "没降过，不用核实");
        // 用满了码率时降的（送达 20 Mbps ≥ 码率的六成）：是真凭实据，不核实
        let mut g = Feed::new(Profile::STABLE, 30_000_000);
        g.v.capacity = Some((30_000_000, Time(0)));
        g.put(1_000, 0, 3_500_000, FLAT, NET);
        g.put(18_000, 0, 2_500_000, FLAT, NET);
        assert_eq!(g.v.rate_bps(), 17_000_000);
        assert_eq!(g.v.probing.drop, None);
        g.put(1_000, 0, 25_000, FLAT, NET);
        for _ in 0..3 {
            g.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        }
        assert_eq!(g.v.state(), RateState::AppLimited);
        assert_eq!(g.v.probe_wanted(Time(g.t)), None);
    }

    #[test]
    fn t1_timeouts_and_aborts_leave_the_rate_alone() {
        // 超时：结论未知，这一段了结
        let mut f = probing(30_000_000, Some(30_000_000));
        cut(&mut f, 1);
        f.v.probe_wanted(Time(f.t)).unwrap();
        assert_eq!(
            f.v.on_probe_end(Time(f.t), ProbeEnd::TimedOut),
            ProbeVerdict::TimedOut
        );
        assert_eq!(
            (f.v.state(), f.v.rate_bps()),
            (RateState::AppLimited, 15_000_000)
        );
        assert!(f
            .notes()
            .contains("核实探测结论 超时：按 25500 kbps 探，到时间没有结果，码率不变"));
        f.t += 1_000_000;
        assert_eq!(f.v.probe_wanted(Time(f.t)), None);
        // 中止：码率不变，回应用受限；200 ms 之后、降后 5 秒之内还会再核实
        let mut g = probing(30_000_000, Some(30_000_000));
        cut(&mut g, 1);
        g.v.probe_wanted(Time(g.t)).unwrap();
        assert_eq!(
            g.v.on_probe_end(Time(g.t), ProbeEnd::Aborted("最近的反馈里还有排队")),
            ProbeVerdict::Aborted
        );
        assert_eq!(
            (g.v.state(), g.v.rate_bps()),
            (RateState::AppLimited, 15_000_000)
        );
        assert!(g.notes().contains(
            "核实探测结论 中止：按 25500 kbps 探，最近的反馈里还有排队，结果不用，码率不变"
        ));
        assert_eq!(
            g.v.probe_wanted(Time(g.t + 100_000)),
            None,
            "刚中止，先歇 200 ms"
        );
        assert!(g.v.probe_wanted(Time(g.t + 200_000)).is_some());
        g.v.on_probe_end(Time(g.t + 200_000), ProbeEnd::Aborted("有丢包"));
        // 降了超过 5 秒：不核实了（码率还远低于已知的链路能力，改由周期探测往上探）
        let later = g.v.probe_wanted(Time(g.t + 6_000_000)).unwrap();
        assert_eq!(later.kind, ProbeKind::Periodic);
        assert_eq!(g.v.state(), RateState::AppLimited);
        assert_eq!(g.v.probing.drop, None);
        // 核实期间出现过载：降码率进排空（I1）；之后回报的结果不用
        let mut h = probing(30_000_000, Some(30_000_000));
        cut(&mut h, 1);
        h.v.probe_wanted(Time(h.t)).unwrap();
        h.t += 200_000;
        h.put(0, 0, 25_000, OVER, SendLimit::App);
        assert_eq!(
            (h.v.state(), h.v.rate_bps()),
            (RateState::DrainApp, 7_500_000)
        );
        assert_eq!(
            h.v.on_probe_end(Time(h.t), ProbeEnd::Result(25_000_000)),
            ProbeVerdict::Aborted
        );
        assert_eq!(h.v.rate_bps(), 7_500_000);
        assert!(h.notes().contains("状态变了，结果不用"));
        // 核实期间出现网络受限的原因：回跟踪；结果不用
        let mut k = probing(30_000_000, Some(30_000_000));
        cut(&mut k, 1);
        k.v.probe_wanted(Time(k.t)).unwrap();
        k.v.on_net_limited(NetCause::FrameNearBudget);
        assert_eq!(k.v.state(), RateState::Tracking);
        assert_eq!(
            k.v.on_probe_end(Time(k.t), ProbeEnd::Result(25_000_000)),
            ProbeVerdict::Aborted
        );
        assert_eq!(k.v.rate_bps(), 15_000_000);
        assert_eq!(k.v.probe_wanted(Time(k.t + 1_000_000)), None, "跟踪里不探");
        // 兜底：发送端一直不回报，核实 2 秒后自己结束
        let mut m = probing(30_000_000, Some(30_000_000));
        cut(&mut m, 1);
        m.v.probe_wanted(Time(m.t)).unwrap();
        let since = m.t;
        while m.t < since + 2_000_000 {
            assert_eq!(m.v.state(), RateState::Verify);
            m.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        }
        assert_eq!(m.v.state(), RateState::AppLimited);
        assert!(m.notes().contains("核实超时：一直没有探测结论"));
    }

    #[test]
    fn t1_needs_a_big_cut_and_a_link_known_to_be_wide_enough() {
        // 不知道容量、或者已知容量放不下一组探测（窄带保护）：不核实
        for capacity in [None, Some(2_000_000)] {
            let mut f = probing(30_000_000, capacity);
            cut(&mut f, 1);
            f.t += 1_000_000;
            assert_eq!(f.v.probe_wanted(Time(f.t)), None);
            assert_eq!(f.v.state(), RateState::AppLimited);
        }
        // 降得不多（没到降前的 66% 以下）：不核实
        let mut g = probing(30_000_000, Some(30_000_000));
        for _ in 0..500 {
            if g.v.rate_bps() != 30_000_000 {
                break;
            }
            // 送达 24 Mbps：降到它的 85% = 20.4 Mbps（降前的 68%）
            g.put(30_000, 0, 3_000_000, FLAT, SendLimit::App);
        }
        assert_eq!(g.v.rate_bps(), 20_400_000);
        g.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        g.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(g.v.state(), RateState::AppLimited);
        assert_eq!(g.v.probe_wanted(Time(g.t)), None);
        // 一组探测要发几个包：30 ms 的量，至少 16 个
        assert_eq!(run_probe_packets(100_000), 16);
        assert_eq!(run_probe_packets(30_000_000), 93);
        assert_eq!(ProbeKind::Verify.name(), "核实");
        assert_eq!(ProbeKind::Periodic.name(), "周期");
        for (i, v) in ProbeVerdict::ALL.iter().enumerate() {
            assert_eq!(v.index(), i);
            assert!(!v.name().is_empty());
        }
    }

    /// I5：应用受限时码率停在底线超过 1 秒必须核实（不管知不知道容量），不允许“底线、不升、没人管”。
    #[test]
    fn i5_app_limited_at_the_floor_must_be_verified_within_a_second() {
        let mut f = probing(100_000, None);
        let start = f.t;
        while f.t < start + 1_020_000 {
            assert_eq!(f.v.probe_wanted(Time(f.t)), None, "{} ms", f.t / 1000);
            f.put(1_000, 0, 1_000, FLAT, SendLimit::App);
        }
        let ask = f.v.probe_wanted(Time(f.t)).unwrap();
        assert_eq!(
            ask,
            ProbeAsk {
                kind: ProbeKind::Verify,
                target_bps: 400_000
            }
        );
        assert_eq!(f.v.state(), RateState::Verify);
        assert!(f
            .notes()
            .contains("应用受限 → 核实：应用受限时码率在底线停了一阵"));
        // 超时：回应用受限，下一次隔 2 秒（间隔翻倍），不会停死也不会刷屏
        assert_eq!(
            f.v.on_probe_end(Time(f.t), ProbeEnd::TimedOut),
            ProbeVerdict::TimedOut
        );
        assert_eq!(f.v.state(), RateState::AppLimited);
        let again = f.t;
        while f.t < again + 2_020_000 {
            assert_eq!(f.v.probe_wanted(Time(f.t)), None, "{} ms", f.t / 1000);
            f.put(1_000, 0, 1_000, FLAT, SendLimit::App);
        }
        assert!(f.v.probe_wanted(Time(f.t)).is_some(), "2 秒后又核实");
        // 证实：码率离开底线
        assert_eq!(
            f.v.on_probe_end(Time(f.t), ProbeEnd::Result(400_000)),
            ProbeVerdict::Confirmed
        );
        assert_eq!(f.v.rate_bps(), 400_000);
        assert_eq!(f.v.state(), RateState::AppLimited);
        // 码率不在底线：不核实
        f.t += 5_000_000;
        f.put(1_000, 0, 1_000, FLAT, SendLimit::App);
        assert_eq!(f.v.probe_wanted(Time(f.t)), None);
    }

    #[test]
    fn t2_periodic_probes_climb_when_the_rate_is_far_below_the_known_capacity() {
        // 应用受限、码率 1 Mbps、已知容量 30 Mbps（例如起步时没探）：满 5 秒开始往上探
        let mut f = probing(1_000_000, Some(30_000_000));
        let start = f.t;
        while f.t < start + 4_980_000 {
            assert_eq!(f.v.probe_wanted(Time(f.t)), None, "{} ms", f.t / 1000);
            f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        }
        f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        let mut targets = Vec::new();
        // 一串最多 3 轮：每轮按上一轮结果的 2 倍（2、4、8 Mbps），证实后码率是结果的九成
        while let Some(ask) = f.v.probe_wanted(Time(f.t)) {
            assert_eq!(ask.kind, ProbeKind::Periodic);
            assert_eq!(f.v.state(), RateState::AppLimited, "周期探测不换状态");
            targets.push(ask.target_bps);
            f.t += 100_000;
            assert_eq!(
                f.v.on_probe_end(Time(f.t), ProbeEnd::Result(ask.target_bps)),
                ProbeVerdict::Confirmed
            );
        }
        assert_eq!(targets, [2_000_000, 4_000_000, 8_000_000]);
        assert_eq!(f.v.rate_bps(), 7_200_000);
        assert_eq!(f.notes().matches("周期探测结论 证实").count(), 3);
        // 证实了：5 秒后接着探
        f.t += 4_900_000;
        assert_eq!(f.v.probe_wanted(Time(f.t)), None);
        f.t += 100_000;
        let ask = f.v.probe_wanted(Time(f.t)).unwrap();
        assert_eq!(ask.target_bps, 14_400_000);
        // 没探上去（否定）：记下容量，间隔翻倍到 10 秒
        assert_eq!(
            f.v.on_probe_end(Time(f.t), ProbeEnd::Result(7_000_000)),
            ProbeVerdict::Denied
        );
        assert_eq!(f.v.rate_bps(), 7_200_000);
        assert_eq!(f.v.probing.periodic_gap_us, 10_000_000);
        // 码率已经到了已知能力（7 Mbps 的九成）的七成以上：不探了
        f.t += 60_000_000;
        assert_eq!(f.v.probe_wanted(Time(f.t)), None);
        // 超时也翻倍，最长 30 秒
        let mut g = probing(1_000_000, Some(30_000_000));
        g.t += 5_000_000;
        g.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        for gap in [10_000_000u64, 20_000_000, 30_000_000, 30_000_000] {
            assert!(g.v.probe_wanted(Time(g.t)).is_some());
            assert_eq!(
                g.v.on_probe_end(Time(g.t), ProbeEnd::TimedOut),
                ProbeVerdict::TimedOut
            );
            assert_eq!(g.v.probing.periodic_gap_us, gap);
            assert_eq!(g.v.probe_wanted(Time(g.t + gap - 1)), None);
            g.t += gap;
        }
        assert_eq!(g.v.rate_bps(), 1_000_000);
        // 周期探测期间回了跟踪：结果不用
        let mut h = probing(1_000_000, Some(30_000_000));
        h.t += 5_000_000;
        h.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        h.v.probe_wanted(Time(h.t)).unwrap();
        h.v.on_net_limited(NetCause::FrameBlocked);
        assert_eq!(
            h.v.on_probe_end(Time(h.t), ProbeEnd::Result(2_000_000)),
            ProbeVerdict::Aborted
        );
        assert_eq!(h.v.rate_bps(), 1_000_000);
        assert_eq!(h.v.probing.app_since, None);
    }

    /// 窄带保护：已知容量低于 3 Mbps（或者不知道容量）的链路上，周期探测一次都不发。
    #[test]
    fn t2_never_probes_a_narrow_or_unknown_link() {
        for capacity in [None, Some(1_000_000), Some(3_000_000)] {
            let mut f = probing(300_000, capacity);
            for _ in 0..3_000 {
                f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
                assert_eq!(f.v.probe_wanted(Time(f.t)), None, "{capacity:?}");
            }
            assert_eq!(f.v.state(), RateState::AppLimited);
        }
        // 一直有随机丢包的链路（固有丢包 1% 以上）：不探，哪怕最近一份反馈碰巧没丢
        let mut lossy = probing(1_000_000, Some(30_000_000));
        for _ in 0..400 {
            lossy.put(1_000, 120, 25_000, FLAT, SendLimit::App);
        }
        lossy.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(lossy.v.state(), RateState::AppLimited);
        assert_eq!(lossy.v.probe_wanted(Time(lossy.t)), None);
        // 码率已经在上限：没有可探的
        let mut g = probing(50_000_000, Some(100_000_000));
        g.t += 10_000_000;
        g.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        assert_eq!(g.v.probe_wanted(Time(g.t)), None);
        // 没有状态机的估计器从不要探测
        let mut old = EstimatorKind::Legacy.build(Profile::STABLE, 1_000_000, 100_000, 10_000_000);
        assert_eq!(old.probe_wanted(Time(0)), None);
        assert_eq!(
            old.on_probe_end(Time(0), ProbeEnd::TimedOut),
            ProbeVerdict::Aborted
        );
    }

    #[test]
    fn probe_results_are_tagged_with_the_cluster_id() {
        let mut m = ReceiverMetrics::new();
        assert_eq!(m.take_probe_report(), None);
        // 20 个包的一组，每毫秒一个：收到 10 个、过了 10 ms 先报一次（没收完）
        for i in 0..12u64 {
            m.on_probe(Time(50_000 + i * 1_000), Time(i * 1_000), 7, 20, 1200);
        }
        m.report();
        let partial = m.take_probe_report().unwrap();
        assert_eq!((partial.id, partial.packets, partial.done), (7, 12, false));
        assert!(partial.bytes_per_s > 1_000_000, "{partial:?}");
        assert_eq!(m.take_probe_report(), None, "取走了");
        // 收齐：马上结算，带“收完了”
        for i in 12..20u64 {
            m.on_probe(Time(50_000 + i * 1_000), Time(i * 1_000), 7, 20, 1200);
        }
        m.report();
        let done = m.take_probe_report().unwrap();
        assert_eq!((done.id, done.packets, done.done), (7, 20, true));
        // 只到了两个包的一组：再没有包来，两份报告之后结束，测不出（速率 0）
        m.on_probe(Time(200_000), Time(100_000), 8, 20, 1200);
        m.on_probe(Time(201_000), Time(101_000), 8, 20, 1200);
        m.report();
        assert_eq!(m.take_probe_report(), None);
        m.report();
        m.report();
        assert_eq!(
            m.take_probe_report(),
            Some(ProbeReport {
                id: 8,
                bytes_per_s: 0,
                packets: 2,
                done: true
            })
        );
        // 编码往返
        let mut w = Writer::new();
        done.encode(&mut w);
        let bytes = w.into_vec();
        assert_eq!(ProbeReport::decode(&mut Reader::new(&bytes)).unwrap(), done);
        assert!(ProbeReport::decode(&mut Reader::new(&bytes[..5])).is_err());
    }

    /// 把估计器摆到某个状态（码率 30 Mbps）。
    fn in_state(state: RateState) -> Feed {
        let mut f = match state {
            RateState::Startup => {
                let mut f = Feed::new(Profile::STABLE, 30_000_000);
                f.v.set_startup(true, "测试");
                f
            }
            RateState::Tracking => Feed::new(Profile::STABLE, 30_000_000),
            RateState::Drain => {
                let mut f = Feed::new(Profile::STABLE, 35_000_000);
                f.put(1_000, 0, 4_400_000, FLAT, NET);
                f.put(20_000, 0, 4_411_765, FLAT, NET);
                f.t += 200_000;
                f
            }
            RateState::AppLimited => Feed::app_limited(Profile::STABLE, 30_000_000),
            RateState::DrainApp => {
                let mut f = Feed::app_limited(Profile::STABLE, 60_000_000);
                f.put(0, 0, 25_000, OVER, SendLimit::App);
                f.t += 200_000;
                f
            }
            RateState::Verify => {
                let mut f = Feed::app_limited(Profile::STABLE, 30_000_000);
                f.v.phase = Phase::Verify { since: Time(f.t) };
                f
            }
        };
        // 排空以外的状态再喂两份平静的反馈（排空一见排队低就结束了）
        if !matches!(state, RateState::Drain | RateState::DrainApp) {
            let limit = if f.v.phase.relaxed() {
                SendLimit::App
            } else {
                NET
            };
            f.put(1_000, 0, 1_000, FLAT, limit);
            f.put(1_000, 0, 1_000, FLAT, limit);
        }
        assert_eq!(f.v.state(), state);
        f
    }

    /// I1：任何状态下，过载证据都让码率降、这一份反馈不升。
    #[test]
    fn i1_overload_always_cuts_in_every_state() {
        for state in RateState::ALL {
            let mut f = in_state(state);
            let before = f.v.rate_bps();
            let limit = if f.v.phase.relaxed() {
                SendLimit::App
            } else {
                NET
            };
            // 趋势过载，同时送达速率很高（没过载的话这一份会升）
            f.put(0, 0, 3_500_000, OVER, limit);
            assert!(
                f.v.rate_bps() < before,
                "{}：{} → {}",
                state.name(),
                before,
                f.v.rate_bps()
            );
        }
    }

    /// I2：码率只在三处变——拥塞降、跟踪（含起步、来源是跟踪的排空）的升、探测结论。
    /// 应用受限、核实和它们的排空里，没有过载就一动不动，送达速率再高也不升。
    #[test]
    fn i2_the_rate_moves_only_on_a_cut_a_tracking_increase_or_a_probe() {
        for state in RateState::ALL {
            let mut f = in_state(state);
            let before = f.v.rate_bps();
            let relaxed = f.v.phase.relaxed();
            let limit = if relaxed { SendLimit::App } else { NET };
            for _ in 0..20 {
                f.put(1_000, 0, 3_500_000, FLAT, limit);
            }
            if relaxed {
                assert_eq!(f.v.rate_bps(), before, "{} 不自己升", state.name());
            } else {
                assert!(f.v.rate_bps() > before, "{} 用满了码率就升", state.name());
            }
        }
        // 来源是应用受限的排空也不升
        let mut f = Feed::app_limited(Profile::STABLE, 30_000_000);
        f.put(0, 0, 25_000, OVER, SendLimit::App);
        for _ in 0..4 {
            f.put(5_000, 0, 3_500_000, FLAT, SendLimit::App);
        }
        assert_eq!(
            (f.v.state(), f.v.rate_bps()),
            (RateState::DrainApp, 15_000_000)
        );
        // 探测结论在任何状态下都能改码率
        for state in RateState::ALL {
            let mut f = in_state(state);
            f.v.on_idle_probe(Time(f.t), 2_000_000);
            assert_eq!(f.v.rate_bps(), 1_800_000, "{}", state.name());
        }
    }

    /// 没有状态机的版本（ui-bugs 分支的 `ClassicV1::on_observation`，原样抄下来，只去掉写日志）。I3 拿它当参照。
    struct Reference {
        p: Profile,
        rate_bps: u64,
        min_bps: u64,
        max_bps: u64,
        threshold: i64,
        last_threshold_update: Option<Time>,
        capacity: Option<(u64, Time)>,
        last_decrease: Option<Time>,
        last_feedback: Option<Time>,
        usage: Usage,
        inherent_loss: u64,
    }

    impl Reference {
        fn detect(&mut self, now: Time, ext: &Extension) -> Usage {
            let trend = i64::from(ext.trend_tenth_ms);
            let dt_ms = self
                .last_threshold_update
                .map_or(0, |t| i64::try_from(now.since(t) / 1000).unwrap_or(0))
                .min(100);
            self.last_threshold_update = Some(now);
            if trend.abs() < self.threshold + 150 {
                let k = if trend.abs() < self.threshold {
                    K_DOWN_E7
                } else {
                    K_UP_E7
                };
                self.threshold += k * (trend.abs() - self.threshold) * dt_ms / 10_000_000;
                self.threshold = self.threshold.clamp(THRESHOLD_MIN, THRESHOLD_MAX);
            }
            if ext.groups < 2 {
                Usage::Normal
            } else if trend > self.threshold {
                Usage::Over
            } else if trend < -self.threshold {
                Usage::Under
            } else {
                Usage::Normal
            }
        }

        fn on_observation(&mut self, now: Time, obs: &Observation) {
            let p = self.p;
            let fb = &obs.feedback;
            let recv_bps = u64::from(fb.recv_bytes_per_s).saturating_mul(8);
            let train_bps = u64::from(obs.ext.train_bytes_per_s).saturating_mul(8);
            if train_bps > 0 {
                self.capacity = Some((train_bps, now));
            }
            let capacity = self
                .capacity
                .filter(|(_, at)| now.since(*at) < CAPACITY_TTL_US)
                .map(|(c, _)| c);
            self.usage = self.detect(now, &obs.ext);
            let using = recv_bps.saturating_mul(10) >= self.rate_bps.saturating_mul(6);
            let contiguous = self
                .last_feedback
                .is_some_and(|t| now.since(t) <= CONTIGUOUS_US);
            self.last_feedback = Some(now);
            let queue_high = fb.queue_delay_us > p.high_delay_us && contiguous;
            let loss = u64::from(fb.loss_permille).saturating_mul(1000);
            self.inherent_loss = self.inherent_loss - self.inherent_loss / 128 + loss / 128;
            let inherent = u16::try_from(self.inherent_loss / 1000).unwrap_or(u16::MAX);
            let lossy_and_queued = fb.loss_permille > p.loss_permille.saturating_add(inherent)
                && fb.queue_delay_us > p.low_delay_us
                && contiguous;
            let congested = self.usage == Usage::Over || queue_high || lossy_and_queued;
            let hold = obs.rtt_us.max(100_000);
            if congested {
                if self.last_decrease.is_none_or(|t| now.since(t) >= hold) {
                    let target = if recv_bps > 0 {
                        recv_bps.saturating_mul(u64::from(p.decrease_permille)) / 1000
                    } else {
                        self.rate_bps.saturating_mul(u64::from(p.decrease_permille)) / 1000
                    };
                    self.rate_bps = self.rate_bps.min(target).clamp(self.min_bps, self.max_bps);
                    self.last_decrease = Some(now);
                }
                return;
            }
            if self.usage == Usage::Under || (fb.queue_delay_us >= p.low_delay_us && contiguous) {
                return;
            }
            if !using {
                return;
            }
            let target = capacity.map(|c| c.saturating_mul(u64::from(p.safety_permille)) / 1000);
            let step = match target {
                Some(t) if self.rate_bps < t => (t - self.rate_bps) / 2,
                Some(_) | None => {
                    self.rate_bps.saturating_mul(u64::from(p.increase_permille)) / 1000
                }
            };
            self.rate_bps = self
                .rate_bps
                .saturating_add(step.max(1))
                .clamp(self.min_bps, self.max_bps);
        }
    }

    /// I3：发送端一直是网络受限时，每一步的码率、过载判断都和没有状态机的版本完全相同
    /// （随机的反馈序列，三种画像各两万份；状态在起步、跟踪、排空之间来回）。
    #[test]
    fn i3_tracking_is_identical_to_the_version_without_a_state_machine() {
        let causes = [
            NetCause::Unknown,
            NetCause::FrameBlocked,
            NetCause::KeyPending,
            NetCause::UsingRate,
        ];
        for (seed, profile) in [Profile::STABLE, Profile::WIRELESS, Profile::CELLULAR]
            .into_iter()
            .enumerate()
        {
            let mut rng = crate::sim::Rng::new(seed as u64 + 7);
            let mut v = ClassicV1::new(profile, 1_000_000, 100_000, 30_000_000);
            let mut r = Reference {
                p: profile,
                rate_bps: 1_000_000,
                min_bps: 100_000,
                max_bps: 30_000_000,
                threshold: THRESHOLD_START,
                last_threshold_update: None,
                capacity: None,
                last_decrease: None,
                last_feedback: None,
                usage: Usage::Normal,
                inherent_loss: 0,
            };
            let mut t = 0u64;
            let mut seen = [false; 6];
            for i in 0..20_000u32 {
                // 多数 20 ms 一份，偶尔断档；排队、丢包、送达、趋势、包串都随机
                t += if rng.chance(50) { 1_000_000 } else { 20_000 };
                let queue = if rng.chance(300) {
                    rng.below(80_000)
                } else {
                    rng.below(4_000)
                };
                let loss = if rng.chance(200) { rng.below(400) } else { 0 };
                // 偶尔一份反馈里没有送达速率（接收端刚醒来）
                let recv = if rng.chance(30) {
                    0
                } else {
                    rng.below(v.rate_bps() / 4 + 1)
                };
                let o = Observation {
                    feedback: fb(
                        u32::try_from(queue).unwrap(),
                        u16::try_from(loss).unwrap(),
                        u32::try_from(recv).unwrap(),
                    ),
                    ext: Extension {
                        // 多数在门限附近，偶尔远远超出（门限更新只看离门限不到 15 ms 的）
                        trend_tenth_ms: if rng.chance(100) {
                            i32::try_from(rng.below(4_000)).unwrap() - 2_000
                        } else {
                            i32::try_from(rng.below(400)).unwrap() - 200
                        },
                        train_bytes_per_s: if rng.chance(100) {
                            u32::try_from(rng.below(2_000_000)).unwrap()
                        } else {
                            0
                        },
                        groups: u16::try_from(rng.below(30)).unwrap(),
                    },
                    rtt_us: 20_000 + rng.below(300_000),
                    in_flight: 0,
                    limit: SendLimit::Network(causes[usize::try_from(rng.below(4)).unwrap()]),
                    // 发送端一直有积压：每份反馈的送达速率都是链路的能力
                    backlog: true,
                };
                // 起步探测开始、结束、出结论也随机穿插（参照版本里结论就是直接定码率）
                if i % 5_000 == 10 {
                    v.set_startup(true, "测试");
                }
                if i % 5_000 == 60 {
                    v.on_start_probe(Time(t), 8_000_000);
                    r.capacity = Some((8_000_000, Time(t)));
                    r.rate_bps = (8_000_000 * u64::from(profile.safety_permille) / 1000)
                        .clamp(r.min_bps, r.max_bps);
                    v.set_startup(false, "测试");
                }
                v.on_net_limited(NetCause::FrameBlocked);
                v.on_observation(Time(t), &o);
                r.on_observation(Time(t), &o);
                assert_eq!(v.rate_bps(), r.rate_bps, "第 {i} 份（{}）", profile.name);
                assert_eq!(v.usage(), r.usage, "第 {i} 份");
                assert!(!v.phase.relaxed());
                seen[v.state().index()] = true;
                v.take_notes();
            }
            assert_eq!(
                seen,
                [true, true, true, false, false, false],
                "走到过起步、跟踪、排空（跟踪）"
            );
        }
    }

    #[test]
    fn kinds_parse_and_build() {
        assert_eq!(EstimatorKind::parse("legacy"), Some(EstimatorKind::Legacy));
        assert_eq!(EstimatorKind::parse("v1"), Some(EstimatorKind::ClassicV1));
        assert_eq!(EstimatorKind::parse("x"), None);
        for k in [EstimatorKind::Legacy, EstimatorKind::ClassicV1] {
            assert_eq!(
                k.build(Profile::STABLE, 1_000_000, 1, 2_000_000).rate_bps(),
                1_000_000
            );
        }
    }

    #[test]
    fn extension_round_trip() {
        let e = Extension {
            trend_tenth_ms: -1234,
            groups: 20,
            train_bytes_per_s: 9_000_000,
        };
        let mut w = Writer::new();
        e.encode(&mut w);
        let bytes = w.into_vec();
        assert_eq!(bytes.len(), Extension::LEN);
        assert_eq!(Extension::decode(&mut Reader::new(&bytes)).unwrap(), e);
        assert!(Extension::decode(&mut Reader::new(&bytes[..5])).is_err());
    }

    /// 每帧 `packets` 个包、每包 1000 字节，发送端每 `gap_us` 发一帧；链路每包需要 `per_packet_us`，
    /// 外加 `extra_us` 的固定额外排队（每帧递增时模拟队列在涨）。
    fn feed(
        m: &mut ReceiverMetrics,
        frames: u32,
        packets: u32,
        gap_us: u64,
        per_packet_us: u64,
        grow_us: u64,
    ) {
        for f in 0..frames {
            let send = Time(u64::from(f) * gap_us);
            let queue = u64::from(f) * grow_us;
            for p in 0..packets {
                let arrival = send.plus(10_000 + queue + u64::from(p + 1) * per_packet_us);
                m.on_packet(arrival, send, f, 1000);
            }
        }
    }

    #[test]
    fn packet_trains_measure_the_bottleneck() {
        let mut m = ReceiverMetrics::new();
        // 8 Mbps 的瓶颈：1000 字节要 1 毫秒
        feed(&mut m, 10, 10, 33_000, 1_000, 0);
        let ext = m.report();
        assert!(
            (900_000..=1_100_000).contains(&ext.train_bytes_per_s),
            "{ext:?}"
        );
        // 报告之后清零
        assert_eq!(m.report().train_bytes_per_s, 0);
        // 包太少的帧不测
        let mut few = ReceiverMetrics::new();
        feed(&mut few, 5, 2, 33_000, 1_000, 0);
        assert_eq!(few.report().train_bytes_per_s, 0);
    }

    #[test]
    fn probe_trains_are_measured_per_cluster_even_before_they_finish() {
        let mut m = ReceiverMetrics::new();
        // 起步探测按 9.6 Mbps 匀速发（1 ms 一个包），4 Mbps 的瓶颈（1200 字节 2.4 ms）：排队一直在涨，
        // 按斜率算出瓶颈。报告时还没收完、够 10 个包和 10 ms 就先报
        for i in 0..9u64 {
            m.on_probe(Time(1_000 + i * 2_400), Time(i * 1_000), 9, 0, 1200);
        }
        assert_eq!(m.report().train_bytes_per_s, 0, "9 个包，还不够");
        m.on_probe(Time(1_000 + 9 * 2_400), Time(9_000), 9, 0, 1200);
        let ext = m.report();
        assert!(
            (450_000..=550_000).contains(&ext.train_bytes_per_s),
            "{ext:?}"
        );
        // 后面的包接着累计；连着两次没有新包，这一组结束（包数和上次报告时一样，不重复报）
        m.on_probe(Time(1_000 + 10 * 2_400), Time(10_000), 9, 0, 1200);
        assert!(m.report().train_bytes_per_s > 0);
        assert_eq!(m.report().train_bytes_per_s, 0);
        assert_eq!(m.report().train_bytes_per_s, 0);
        assert!(m.probe.is_none(), "这一组结束了");
        // 窄带：8 个包、每 24 ms 到一个，报告 20 ms 一次（中间偶尔有一次没有新包）：收完之后报最终结果
        let mut narrow = ReceiverMetrics::new();
        let mut reported = 0;
        for i in 0..8u64 {
            narrow.on_probe(Time(i * 24_000), Time(i * 7_000), 3, 0, 1200);
            reported += narrow.report().train_bytes_per_s;
            if i % 2 == 1 {
                reported += narrow.report().train_bytes_per_s;
            }
        }
        assert_eq!(reported, 0, "收完之前不报（包少，时间也不够长）");
        let rate = narrow.report().train_bytes_per_s;
        assert!((45_000..=55_000).contains(&rate), "{rate}");
        // 包里写了一共几个：收齐马上结算，下一次报告就有最终结果（不等“之后没有新包”）
        let mut counted = ReceiverMetrics::new();
        for i in 0..8u64 {
            counted.on_probe(Time(i * 24_000), Time(i * 7_000), 4, 8, 1200);
        }
        let rate = counted.report().train_bytes_per_s;
        assert!((45_000..=55_000).contains(&rate), "{rate}");
        assert!(counted.probe.is_none());
        // 换了组：上一组先结算；包太少的组不测
        m.on_probe(Time(50_000), Time(0), 1, 0, 1200);
        m.on_probe(Time(51_000), Time(0), 1, 0, 1200);
        m.on_probe(Time(60_000), Time(0), 2, 0, 1200);
        assert_eq!(m.report().train_bytes_per_s, 0);
        // 探测包不进延迟趋势
        assert_eq!(m.report().groups, 0);
    }

    /// 匀速发的探测组按“延迟随发送时刻上涨的斜率”算速率：Wi-Fi 抖动、接收端一次读走一批都不影响。
    #[test]
    fn paced_probes_measure_through_jitter_and_batched_reads() {
        // 按 10 Mbps 匀速发 30 ms（32 个包，0.96 ms 一个），链路 30 Mbps（撑得住）
        let feed = |m: &mut ReceiverMetrics, bottleneck_us: u64, jitter: &dyn Fn(u64) -> u64| {
            let mut free = 0u64;
            for i in 0..32u64 {
                let send = i * 960;
                // 瓶颈：前一个包走完才轮到这个
                free = free.max(send) + bottleneck_us;
                m.on_probe(Time(10_000 + free + jitter(i)), Time(send), 5, 0, 1200);
            }
            m.finish_probe();
            m.report().train_bytes_per_s
        };
        let in_range = |rate: u32, bps: u32| {
            let got = rate.saturating_mul(8);
            got >= bps / 100 * 85 && got <= bps / 100 * 115
        };
        // 0–20 ms 的随机抖动（确定的伪随机）：按首尾间隔算只有六成，按斜率还是 10 Mbps
        let jitter = |i: u64| (i * 7_919) % 20_000;
        let rate = feed(&mut ReceiverMetrics::new(), 320, &jitter);
        assert!(in_range(rate, 10_000_000), "{rate}");
        // 接收端每 8 ms 才读一次（读之前到的包都打成读的时刻）
        let batched = |i: u64| {
            let at = 10_000 + i * 960 + 320;
            at.div_ceil(8_000) * 8_000 - at
        };
        let rate = feed(&mut ReceiverMetrics::new(), 320, &batched);
        assert!(in_range(rate, 10_000_000), "{rate}");
        // 瓶颈只有 5 Mbps（1200 字节 1.92 ms）：测出 5 Mbps，抖动也一样
        let rate = feed(&mut ReceiverMetrics::new(), 1_920, &jitter);
        assert!(in_range(rate, 5_000_000), "{rate}");
    }

    #[test]
    fn an_idle_probe_moves_the_rate_to_the_measured_capacity() {
        let mut v = ClassicV1::new(Profile::STABLE, 17_000_000, 100_000, 30_000_000);
        assert!(v.wants_idle_probe());
        // 静止时链路降到 2 Mbps：直接降到它的九成
        v.on_idle_probe(Time(0), 2_000_000);
        assert_eq!(v.rate_bps(), 1_800_000);
        assert!(v.take_notes().iter().any(|n| n.contains("静止时探测")));
        // 变好了（测得 20 Mbps）：不按它升（升交给有真实流量时的爬升）
        v.on_idle_probe(Time(1), 20_000_000);
        assert_eq!(v.rate_bps(), 1_800_000);
        // 只低一点（不到一半才降）：不动，不写说明
        let mut same = ClassicV1::new(Profile::STABLE, 1_500_000, 100_000, 30_000_000);
        same.on_idle_probe(Time(0), 1_000_000);
        assert_eq!(same.rate_bps(), 1_500_000);
        assert!(same.take_notes().is_empty());
        // 老的估计器不做
        let mut legacy =
            EstimatorKind::Legacy.build(Profile::STABLE, 4_000_000, 100_000, 30_000_000);
        assert!(!legacy.wants_idle_probe());
        legacy.on_idle_probe(Time(0), 1);
        assert_eq!(legacy.rate_bps(), 4_000_000);
    }

    #[test]
    fn trend_rises_when_the_queue_grows_and_stays_flat_otherwise() {
        let mut flat = ReceiverMetrics::new();
        feed(&mut flat, 30, 3, 33_000, 500, 0);
        let e = flat.report();
        assert!(e.trend_tenth_ms.abs() < 3, "{e:?}");
        assert!(e.groups >= 2);
        let mut growing = ReceiverMetrics::new();
        // 每帧多排队 2 毫秒
        feed(&mut growing, 30, 3, 33_000, 500, 2_000);
        let g = growing.report();
        // 每 33 毫秒多排队 2 毫秒：斜率约 0.06，× 20 组 × 4 ≈ 4.8 毫秒 = 48 个 0.1 毫秒
        assert!((35..=60).contains(&g.trend_tenth_ms), "{g:?}");
        let mut draining = ReceiverMetrics::new();
        feed(&mut draining, 30, 3, 33_000, 500, 0);
        for f in 30..60u32 {
            let send = Time(u64::from(f) * 33_000);
            let queue = 60_000u64.saturating_sub(u64::from(f - 30) * 2_000);
            draining.on_packet(send.plus(10_000 + queue), send, f, 1000);
        }
        assert!(draining.report().trend_tenth_ms < 0);
        assert_eq!(slope_per_mille(&[(1, 1), (1, 5)]), 0, "x 都相同");
    }

    #[test]
    fn retransmitted_fragments_do_not_bend_the_trend() {
        let mut m = ReceiverMetrics::new();
        for f in 0..30u32 {
            let send = Time(u64::from(f) * 33_000);
            for p in 0..3u64 {
                m.on_packet(send.plus(10_000 + p * 500), send, f, 1000);
            }
            // 每帧之后都有一个旧帧的分片被重传：新的发送时间、旧的帧号
            if f > 2 {
                let resend = send.plus(20_000);
                m.on_packet(resend.plus(10_000), resend, f - 2, 1000);
            }
        }
        let e = m.report();
        assert!(e.trend_tenth_ms.abs() < 3, "重传不应影响趋势：{e:?}");
    }

    #[test]
    fn legacy_adapter_behaves_like_the_old_controller() {
        let mut l = Legacy(RateController::new(
            Profile::STABLE,
            4_000_000,
            100_000,
            30_000_000,
        ));
        assert_eq!(l.name(), "legacy");
        l.on_observation(Time(1), &obs(fb(50_000, 0, 100_000), Extension::default()));
        assert!(l.rate_bps() < 4_000_000);
        l.set_profile(Profile::WIRELESS);
        assert_eq!(l.profile().name, "wireless");
    }

    #[test]
    fn v1_ramps_to_measured_capacity_quickly() {
        let mut v = ClassicV1::new(Profile::STABLE, 4_000_000, 100_000, 100_000_000);
        assert_eq!(v.name(), "classic-v1");
        // 包串测得 50 Mbps，链路在用：几次反馈就逼近 45 Mbps
        let ext = Extension {
            train_bytes_per_s: 6_250_000,
            groups: 10,
            ..Extension::default()
        };
        let mut t = 0u64;
        for _ in 0..6 {
            t += 20_000;
            let recv = u32::try_from(v.rate_bps() / 8).unwrap();
            v.on_observation(Time(t), &obs(fb(0, 0, recv), ext));
        }
        // STABLE 的余量是九成：逼近 45 Mbps
        assert!(v.rate_bps() > 40_000_000, "{}", v.rate_bps());
        assert!(v.rate_bps() <= 45_000_000);
        assert_eq!(v.usage(), Usage::Normal);
    }

    #[test]
    fn steady_random_loss_becomes_the_inherent_floor_and_only_loss_above_it_cuts() {
        let mut v = ClassicV1::new(Profile::WIRELESS, 1_000_000, 100_000, 100_000_000);
        let normal = Extension {
            groups: 10,
            ..Extension::default()
        };
        // 一开始：20% 丢包 + 排队连着几份反馈都在，算拥塞，降
        for i in 0..3u64 {
            v.on_observation(
                Time(20_000 + i * 20_000),
                &obs(fb(10_000, 200, 125_000), normal),
            );
        }
        assert!(v.rate_bps() < 1_000_000);
        // 链路一直是 20% 的随机丢包：固有丢包跟上来之后，同样的丢包不再降码率
        let mut t = 20_000;
        for _ in 0..1000 {
            t += 20_000;
            v.on_observation(Time(t), &obs(fb(0, 200, 125_000), normal));
        }
        let before = v.rate_bps();
        t += 200_000;
        v.on_observation(Time(t), &obs(fb(10_000, 200, 125_000), normal));
        assert_eq!(v.rate_bps(), before, "随机丢包不降码率");
        // 丢包明显高出固有丢包、排队连着几份都在：拥塞，降
        t += 200_000;
        for i in 0..3u64 {
            v.on_observation(Time(t + i * 20_000), &obs(fb(10_000, 400, 125_000), normal));
        }
        assert!(v.rate_bps() < before);
    }

    /// 2026-11-26 用户日志（Wi-Fi、30 Mbps）：画面静止时零星小包的排队读数 12–25 ms、趋势正常、送达十几 kbps。
    /// 排队读数只在反馈不断档（包一直在到、链路在忙）时才算：零星小包不降；链路在忙时排队高就按这时的送达速率降。
    #[test]
    fn queue_readings_count_only_while_feedback_is_continuous() {
        let flat = Extension {
            groups: 10,
            ..Extension::default()
        };
        let mut v = ClassicV1::new(Profile::STABLE, 30_000_000, 100_000, 50_000_000);
        // 静止时零星小包（1 秒一份反馈）：排队 15 ms、送达 14 kbps，连着 5 份也不降
        for i in 1..=5u64 {
            v.on_observation(Time(i * 1_000_000), &obs(fb(15_000, 0, 1_750), flat));
        }
        assert_eq!(v.rate_bps(), 30_000_000);
        // 几帧小画面，每帧隔 100 ms（反馈断档）：排队 20 ms、丢一个包（几百‰），也不降
        for i in 1..=5u64 {
            v.on_observation(
                Time(5_000_000 + i * 100_000),
                &obs(fb(20_000, 500, 20_000), flat),
            );
        }
        assert_eq!(v.rate_bps(), 30_000_000);
        // 连续在发（20 ms 一份反馈）：断档后的第一份不算，第二份起排队高就按送达速率（2 Mbps）降
        let t = 7_000_000;
        v.on_observation(Time(t), &obs(fb(20_000, 0, 250_000), flat));
        assert_eq!(v.rate_bps(), 30_000_000);
        v.on_observation(Time(t + 20_000), &obs(fb(20_000, 0, 250_000), flat));
        assert_eq!(v.rate_bps(), 1_700_000);
        // 连续在发时丢包明显、又在排队：拥塞
        let mut x = ClassicV1::new(Profile::STABLE, 30_000_000, 100_000, 50_000_000);
        for i in 0..2u64 {
            x.on_observation(Time(i * 20_000), &obs(fb(5_000, 200, 250_000), flat));
        }
        assert_eq!(x.rate_bps(), 1_700_000);
        // 延迟趋势过载：断档也算（趋势本身是很多组攒出来的）
        let mut y = ClassicV1::new(Profile::STABLE, 30_000_000, 100_000, 50_000_000);
        let over = Extension {
            trend_tenth_ms: 3000,
            groups: 20,
            ..Extension::default()
        };
        y.on_observation(Time(20_000), &obs(fb(0, 0, 250_000), over));
        assert_eq!(y.rate_bps(), 1_700_000);
        // 已知的局限：连续在发时碰上一阵 Wi-Fi 尖峰（排队 20 ms），也会按当时的送达速率降一次
    }

    #[test]
    fn v1_cuts_on_a_rising_trend_but_not_on_random_loss() {
        let mut v = ClassicV1::new(Profile::WIRELESS, 10_000_000, 100_000, 100_000_000);
        v.set_profile(Profile::WIRELESS);
        assert_eq!(v.profile().name, "wireless");
        // 随机丢包 5%，排队不涨：不降
        v.on_observation(
            Time(20_000),
            &obs(
                fb(0, 50, 1_250_000),
                Extension {
                    groups: 10,
                    ..Extension::default()
                },
            ),
        );
        assert!(v.rate_bps() >= 10_000_000);
        // 趋势超过门限：降到收到速率的 85%
        let rising = Extension {
            trend_tenth_ms: 3000,
            groups: 20,
            ..Extension::default()
        };
        v.on_observation(Time(40_000), &obs(fb(4_000, 0, 1_000_000), rising));
        assert_eq!(v.usage(), Usage::Over);
        assert_eq!(v.rate_bps(), 6_800_000);
        // 同一个往返之内不重复降
        v.on_observation(Time(60_000), &obs(fb(4_000, 0, 1_000_000), rising));
        assert_eq!(v.rate_bps(), 6_800_000);
        // 排队消退：先不升
        let falling = Extension {
            trend_tenth_ms: -3000,
            groups: 20,
            ..Extension::default()
        };
        v.on_observation(Time(300_000), &obs(fb(0, 0, 850_000), falling));
        assert_eq!(v.usage(), Usage::Under);
        assert_eq!(v.rate_bps(), 6_800_000);
        // 排队一直很高（趋势线还没攒够点）：连着几份反馈之后也降
        let mut fresh = ClassicV1::new(Profile::STABLE, 10_000_000, 100_000, 100_000_000);
        for i in 0..3u64 {
            fresh.on_observation(
                Time(1 + i * 20_000),
                &obs(fb(50_000, 0, 0), Extension::default()),
            );
        }
        assert_eq!(fresh.rate_bps(), 8_500_000);
        // 静止（没在用）：不升
        let mut idle = ClassicV1::new(Profile::STABLE, 10_000_000, 100_000, 100_000_000);
        idle.on_observation(Time(1), &obs(fb(0, 0, 1_000), Extension::default()));
        assert_eq!(idle.rate_bps(), 10_000_000);
        // 不知道容量：按比例升
        let mut blind = ClassicV1::new(Profile::STABLE, 10_000_000, 100_000, 100_000_000);
        blind.on_observation(Time(1), &obs(fb(0, 0, 1_250_000), Extension::default()));
        assert_eq!(blind.rate_bps(), 10_800_000);
        // 测得的容量过期后不再用
        let mut stale = ClassicV1::new(Profile::STABLE, 10_000_000, 100_000, 100_000_000);
        let cap = Extension {
            train_bytes_per_s: 12_500_000,
            ..Extension::default()
        };
        stale.on_observation(Time(1), &obs(fb(0, 0, 0), cap));
        stale.on_observation(
            Time(10_000_000),
            &obs(fb(0, 0, 1_250_000), Extension::default()),
        );
        assert_eq!(stale.rate_bps(), 10_800_000);
    }

    #[test]
    fn threshold_adapts_within_bounds() {
        let mut v = ClassicV1::new(Profile::STABLE, 1_000_000, 100_000, 100_000_000);
        let e = |trend| Extension {
            trend_tenth_ms: trend,
            groups: 20,
            ..Extension::default()
        };
        let mut t = 0;
        for _ in 0..500 {
            t += 20_000;
            let _ = v.detect(Time(t), &e(0));
        }
        assert_eq!(v.threshold, THRESHOLD_MIN, "长期平稳：门限降到下界");
        for _ in 0..2000 {
            t += 20_000;
            let x = v.threshold + 100;
            let _ = v.detect(Time(t), &e(i32::try_from(x).unwrap()));
        }
        assert!(
            v.threshold > 1000,
            "持续略高于门限：门限慢慢上移 {}",
            v.threshold
        );
        assert!(v.threshold <= THRESHOLD_MAX);
    }

    /// 只实现必须实现的几个方法的估计器：其余都用 trait 的默认实现。默认值就是约定——
    /// 不用到达报告、没有探测、没有节奏器速率、一直是“跟踪”、有探测结果送来就当没这回事。
    #[derive(Debug)]
    struct Bare {
        profile: Profile,
    }

    impl RateEstimator for Bare {
        fn name(&self) -> &'static str {
            "bare"
        }
        fn rate_bps(&self) -> u64 {
            1_000_000
        }
        fn on_observation(&mut self, _now: Time, _obs: &Observation) {}
        fn profile(&self) -> &Profile {
            &self.profile
        }
        fn set_profile(&mut self, profile: Profile) {
            self.profile = profile;
        }
    }

    #[test]
    fn an_estimator_that_overrides_nothing_opts_out_of_every_optional_feature() {
        let mut e = Bare {
            profile: Profile::STABLE,
        };
        e.set_profile(Profile::WIRELESS);
        assert_eq!(e.profile().name, Profile::WIRELESS.name);
        assert!(e.take_notes().is_empty());
        assert!(!e.uses_reports(), "默认不要到达报告");
        // 这些通知默认都没有副作用
        let packet = SentPacket {
            seq: 1,
            send_time: Time(1),
            size: 1200,
            kind: crate::cc::report::PacketKind::Media,
        };
        e.on_sent(Time(1), &packet);
        e.on_report(Time(2), &[], 20_000);
        e.on_tick(Time(3));
        e.on_idle_probe(Time(4), 5_000_000);
        e.on_start_probe(Time(5), 5_000_000);
        e.set_startup(true, "测试");
        e.on_net_limited(NetCause::FrameBlocked);
        assert_eq!(e.rate_bps(), 1_000_000, "这些通知都不改码率");
        assert_eq!(e.pacing_bps(), None, "没有节奏器速率：按老规则");
        assert!(e.take_probes().is_empty());
        assert!(!e.wants_idle_probe());
        assert_eq!(e.state_line(), None);
        assert_eq!(e.state(), RateState::Tracking);
        // 不探测：发送端不会收到探测的要求；万一回报了结果，结论是中止
        assert_eq!(e.probe_wanted(Time(6)), None);
        assert_eq!(
            e.on_probe_end(Time(7), ProbeEnd::Result(9_000_000)),
            ProbeVerdict::Aborted
        );
    }

    #[test]
    fn the_gcc_estimator_forwards_to_the_network_controller() {
        use crate::cc::report::{PacketKind, PacketResult};
        assert_eq!(EstimatorKind::parse("gcc"), Some(EstimatorKind::GccV1));
        assert_eq!(EstimatorKind::parse("gcc-v1"), Some(EstimatorKind::GccV1));
        let mut g = EstimatorKind::GccV1.build(Profile::STABLE, 2_000_000, 100_000, 30_000_000);
        assert_eq!(g.name(), "gcc-v1");
        assert!(g.uses_reports(), "网络控制器只看到达报告");
        assert_eq!(g.rate_bps(), 2_000_000);
        assert_eq!(
            g.pacing_bps(),
            Some(5_000_000),
            "还没有反馈：节奏速率是码率的 2.5 倍"
        );
        // 接收端算的排队延迟、趋势它都不用：喂一份过载的反馈，码率不动
        g.on_observation(Time(1), &obs(fb(80_000, 300, 10_000), OVER));
        assert_eq!(g.rate_bps(), 2_000_000);
        g.set_profile(Profile::WIRELESS);
        assert_eq!(g.profile().name, Profile::WIRELESS.name);
        // 1 秒、每 5 ms 一个包（1.9 Mbps），单程延迟不变，全部到达：链路没有压力，码率不降
        let mut results = Vec::new();
        for i in 0..200u32 {
            let send = Time(u64::from(i) * 5_000);
            let sent = SentPacket {
                seq: i,
                send_time: send,
                size: 1200,
                kind: PacketKind::Media,
            };
            g.on_sent(send, &sent);
            results.push(PacketResult {
                sent,
                recv_time: Some(send.plus(20_000)),
            });
        }
        g.on_report(Time(1_040_000), &results, 40_000);
        g.on_tick(Time(1_050_000));
        assert!(g.rate_bps() >= 2_000_000, "{}", g.rate_bps());
        assert_eq!(
            g.pacing_bps(),
            Some(g.rate_bps() * 1100 / 1000),
            "有了反馈：节奏速率是码率的 1.1 倍"
        );
        assert!(
            g.take_probes().is_empty(),
            "会话开头不探测，也还没有应用受限那么久"
        );
        let line = g.state_line().unwrap();
        assert!(line.contains("网络控制 目标"), "{line}");
        g.take_notes();
        assert!(g.take_notes().is_empty(), "说明取走一次就空了");
        // 没有状态机：一直报告“跟踪”，也没有探测
        assert_eq!(g.state(), RateState::Tracking);
        assert_eq!(g.probe_wanted(Time(2_000_000)), None);
    }

    /// 探测的包一个紧接一个发出去（发送跨度不到 1 ms）：没有发送速率可言，只能按到达间隔算，
    /// 接收端一次读走一批包时间隔不可信（到达跨度也不到 1 ms），就报“测不出”，不能编一个速率。
    #[test]
    fn a_burst_probe_is_measured_by_arrivals_and_a_too_tight_one_is_not_measured() {
        let mut m = ReceiverMetrics::new();
        // 5 个包同一时刻发出，每 1 ms 到一个：首包之后 4 个包 4800 字节、跨度 4 ms，1.2 MB/s
        for i in 0..5u64 {
            m.on_probe(Time(10_000 + i * 1_000), Time(0), 11, 5, 1200);
        }
        assert_eq!(m.report().train_bytes_per_s, 1_200_000);
        // 同样的包每 200 µs 到一个，全在 1 ms 内到齐（接收端一次读走了一批）：不可信
        let mut tight = ReceiverMetrics::new();
        for i in 0..5u64 {
            tight.on_probe(Time(10_000 + i * 200), Time(0), 12, 5, 1200);
        }
        assert_eq!(
            tight.report().train_bytes_per_s,
            0,
            "到达跨度不到 1 ms：测不出"
        );
    }

    /// 说明攒满了（会话每秒才取走一次）：后面的降码率不再写说明，码率照降，不能因为说明满了就不降。
    /// 没满时，只有码率真的变了才写“降码率”（以前码率已经到目标了还每个往返写一条“850 → 850”）。
    #[test]
    fn a_full_note_buffer_keeps_the_first_notes_and_never_blocks_a_cut() {
        let mut f = Feed::new(Profile::STABLE, 30_000_000);
        f.put(1_000, 0, 3_500_000, FLAT, NET);
        let _ = f.v.take_notes();
        for i in 0..MAX_NOTES {
            f.v.notes.push(format!("先攒着的说明 {i}"));
        }
        // 持续拥塞，发送端一直有积压（送达速率是链路的能力）：按送达速率（100 kbps）降到底线
        for _ in 0..30 {
            f.put_busy(20_000, 0, 12_500, FLAT, NET);
        }
        assert_eq!(f.v.rate_bps(), 100_000, "说明满了不影响降码率");
        let notes = f.v.take_notes();
        assert!(
            !notes.iter().any(|n| n.starts_with("降码率")),
            "满了以后的不再写：{notes:?}"
        );
        // 没满：降了才写，码率不变的那几个往返不写
        let mut g = Feed::new(Profile::STABLE, 30_000_000);
        g.put(1_000, 0, 3_500_000, FLAT, NET);
        for _ in 0..300 {
            g.put_busy(20_000, 0, 125_000, FLAT, NET);
        }
        assert_eq!(g.v.rate_bps(), 850_000, "降到送达速率（1 Mbps）的 85%");
        let cuts: Vec<String> =
            g.v.take_notes()
                .into_iter()
                .filter(|n| n.starts_with("降码率"))
                .collect();
        assert!(!cuts.is_empty());
        let pair = |n: &str| -> Option<(u64, u64)> {
            let rest = n.strip_prefix("降码率 ")?;
            let mut it = rest.split(' ');
            let from = it.next()?.parse().ok()?;
            let to = it.nth(1)?.parse().ok()?;
            Some((from, to))
        };
        assert!(
            cuts.iter().all(|n| pair(n).is_some_and(|(a, b)| b < a)),
            "{cuts:?}"
        );
    }

    /// 周期探测证实了、码率却没涨多少（不到一成：已经贴着上限）：到头了，链路能力记作已知，不再接着探。
    #[test]
    fn a_confirmed_periodic_probe_that_gains_under_ten_percent_ends_the_climb() {
        // 应用受限、码率 46 Mbps、上限 50 Mbps、测得的容量很大（探得动）、但还没确认过到头了
        let mut f = probing(46_000_000, Some(100_000_000));
        assert!(!f.v.probing.limit_known);
        let start = f.t;
        while f.t < start + 5_000_000 {
            f.put(1_000, 0, 25_000, FLAT, SendLimit::App);
        }
        let ask = f.v.probe_wanted(Time(f.t)).unwrap();
        assert_eq!(
            ask,
            ProbeAsk {
                kind: ProbeKind::Periodic,
                target_bps: 50_000_000
            }
        );
        f.t += 100_000;
        assert_eq!(
            f.v.on_probe_end(Time(f.t), ProbeEnd::Result(50_000_000)),
            ProbeVerdict::Confirmed
        );
        // 证实了，但按安全比例只能到 45 Mbps，比现在的 46 低：码率不动
        assert_eq!(f.v.rate_bps(), 46_000_000);
        assert!(f.v.probing.limit_known, "没探上去多少：链路能力记作已知");
        assert_eq!(f.v.probe_wanted(Time(f.t)), None, "这一串探测到此为止");
    }
}
