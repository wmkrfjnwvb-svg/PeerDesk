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

/// 一组包（一帧）的发送和到达时间。
#[derive(Debug, Clone, Copy)]
struct Group {
    frame: u32,
    last_send: Time,
    first_arrival: Time,
    last_arrival: Time,
    packets: u32,
    bytes: u64,
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
    /// 正在收的探测组（起步探测的填充包，见 [`crate::proto::Caps::PROBE_TRAIN`]）：(组号, 首包到达, 末包到达, 包数, 字节数)。
    probe: Option<(u32, Time, Time, u32, u64)>,
    /// 上次报告时这一组有几个包（没有新包进来就结束这一组，不重复报告旧结果）。
    probe_reported: u32,
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
        }
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
            last_send: send_time,
            first_arrival: now,
            last_arrival: now,
            packets: 1,
            bytes,
        });
    }

    /// 收到探测组 `id` 的一个填充包。探测包不进延迟趋势（它们背靠背发出，只用来测包串速率）。
    pub fn on_probe(&mut self, now: Time, id: u32, bytes: usize) {
        let bytes = u64::try_from(bytes).unwrap_or(u64::MAX);
        match self.probe.as_mut() {
            Some((g, _, last, packets, total)) if *g == id => {
                *last = now;
                *packets = packets.saturating_add(1);
                *total = total.saturating_add(bytes);
            }
            Some(_) | None => {
                self.finish_probe();
                self.probe = Some((id, now, now, 1, bytes));
            }
        }
    }

    /// 结算正在收的探测组。
    fn finish_probe(&mut self) {
        if let Some((_, first, last, packets, bytes)) = self.probe.take() {
            self.train(first, last, packets, bytes);
        }
        self.probe_reported = 0;
    }

    /// 报告时：探测组不用等整组收完（窄带上一组要传一两百毫秒，反馈每几十毫秒一次），够包数就先报告，
    /// 组留着、后面的包接着累计；上次报告之后没有新包进来，这一组就结束了。
    fn report_probe(&mut self) {
        let Some((_, first, last, packets, bytes)) = self.probe else {
            return;
        };
        if packets == self.probe_reported {
            self.probe = None;
            self.probe_reported = 0;
        } else if packets >= MIN_TRAIN_PACKETS {
            self.train(first, last, packets, bytes);
            self.probe_reported = packets;
        }
    }

    /// 包串速率：首包到达之后的字节 ÷ 首尾到达间隔。
    fn train(&mut self, first: Time, last: Time, packets: u32, bytes: u64) {
        let span = last.since(first);
        if packets >= MIN_TRAIN_PACKETS && span > 0 {
            let after_first = bytes.saturating_mul(u64::from(packets - 1)) / u64::from(packets);
            let rate = after_first.saturating_mul(1_000_000) / span;
            self.max_train = self.max_train.max(u32::try_from(rate).unwrap_or(u32::MAX));
        }
    }

    fn finish(&mut self, g: Group) {
        self.train(g.first_arrival, g.last_arrival, g.packets, g.bytes);
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

    /// 一行状态（诊断日志用）。
    fn state_line(&self) -> Option<String> {
        None
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
    usage: Usage,
    /// 码率变化说明（取走后清空）和上次写说明时的码率（升码率累计超过一定比例才写一次）。
    notes: Vec<String>,
    noted_bps: u64,
    /// 固有丢包率（千分比 × 1000，慢速平均）：随机丢包（Wi-Fi 干扰、链路本身）的底，高出它的丢包才算拥塞
    /// （WebRTC `LossBasedBweV2` 的“固有丢包”）。
    inherent_loss: u64,
}

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
            notes: Vec::new(),
            noted_bps: start_bps,
            usage: Usage::Normal,
            inherent_loss: 0,
        }
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
        // 拥塞：趋势过载，或者排队已经很高（趋势线可能还没攒够点）。
        // 丢包只有在排队也在涨时才算拥塞；单独的随机丢包交给纠错和重传，不降码率。
        let queue_high = fb.queue_delay_us > p.high_delay_us;
        // 固有丢包慢慢跟（每份反馈 1/128，反馈 20 ms 一次时约 2.5 秒）
        let loss = u64::from(fb.loss_permille).saturating_mul(1000);
        self.inherent_loss = self.inherent_loss - self.inherent_loss / 128 + loss / 128;
        let inherent = u16::try_from(self.inherent_loss / 1000).unwrap_or(u16::MAX);
        let lossy_and_queued = fb.loss_permille > p.loss_permille.saturating_add(inherent)
            && fb.queue_delay_us > p.low_delay_us;
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
                // 只给日志看：发送端是不是在用满码率（没用满时送达速率不代表链路能力）
                let using = recv_bps.saturating_mul(10) >= self.rate_bps.saturating_mul(6);
                let from_bps = self.rate_bps;
                self.rate_bps = self.rate_bps.min(target).clamp(self.min_bps, self.max_bps);
                self.last_decrease = Some(now);
                if self.notes.len() < MAX_NOTES {
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
            }
            return;
        }
        if self.usage == Usage::Under || fb.queue_delay_us >= p.low_delay_us {
            return;
        }
        // 只有真在用这个码率时才升：屏幕静止时没有数据，估计冻结，不凭空上涨。
        let using = recv_bps.saturating_mul(10) >= self.rate_bps.saturating_mul(6);
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
        // 起步探测、4 Mbps 的瓶颈（1200 字节 2.4 ms）：报告时还没收完、够 3 个包就能测
        for i in 0..5u64 {
            m.on_probe(Time(1_000 + i * 2_400), 9, 1200);
        }
        let ext = m.report();
        assert!(
            (450_000..=550_000).contains(&ext.train_bytes_per_s),
            "{ext:?}"
        );
        // 后面的包接着累计；没有新包进来的那次报告结束这一组，不重复报告
        m.on_probe(Time(13_000), 9, 1200);
        assert!(m.report().train_bytes_per_s > 0);
        assert_eq!(m.report().train_bytes_per_s, 0);
        // 窄带：每次报告之间只到一个包，攒够 3 个才报
        let mut narrow = ReceiverMetrics::new();
        narrow.on_probe(Time(0), 3, 1200);
        assert_eq!(narrow.report().train_bytes_per_s, 0);
        narrow.on_probe(Time(24_000), 3, 1200);
        assert_eq!(narrow.report().train_bytes_per_s, 0);
        narrow.on_probe(Time(48_000), 3, 1200);
        let rate = narrow.report().train_bytes_per_s;
        assert!((45_000..=55_000).contains(&rate), "{rate}");
        // 换了组：上一组先结算；包太少的组不测
        m.on_probe(Time(50_000), 1, 1200);
        m.on_probe(Time(51_000), 1, 1200);
        m.on_probe(Time(60_000), 2, 1200);
        assert_eq!(m.report().train_bytes_per_s, 0);
        // 探测包不进延迟趋势
        assert_eq!(m.report().groups, 0);
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
        // 一开始：20% 丢包 + 少量排队（一帧自己的突发）就算拥塞，降
        v.on_observation(Time(20_000), &obs(fb(10_000, 200, 125_000), normal));
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
        // 丢包明显高出固有丢包、又在排队：拥塞，降
        t += 200_000;
        v.on_observation(Time(t), &obs(fb(10_000, 400, 125_000), normal));
        assert!(v.rate_bps() < before);
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
        // 排队很高（趋势线还没攒够点）：也降
        let mut fresh = ClassicV1::new(Profile::STABLE, 10_000_000, 100_000, 100_000_000);
        fresh.on_observation(Time(1), &obs(fb(50_000, 0, 0), Extension::default()));
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
}
