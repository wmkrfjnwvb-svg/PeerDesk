//! 网络控制器（发送端）：所有网络决定都在这里，照 WebRTC 的 GoogCC
//! （`modules/congestion_controller/goog_cc/goog_cc_network_control.cc`）组织，见 `docs/research/L-framework.md`。
//!
//! 三种时机（框架的“时钟”），每种只做规定的事：
//! - **每发一个包**（[`Gcc::on_sent`]）：只记账（应用受限的预算）。
//! - **每份到达报告**（[`Gcc::on_report`]）：送达速率 → 探测结果 → 延迟趋势判断 → AIMD 定码率 →
//!   丢包限制 → 码率 = 两者小的；从过载恢复时请求探测。
//! - **每 25 ms**（[`Gcc::on_tick`]）：应用受限的预算累积，应用受限时的定期探测。
//!
//! 输出只有三样：目标码率（[`Gcc::target_bps`]）、节奏速率（[`Gcc::pacing_bps`]）、
//! 要发的探测（[`Gcc::take_probes`]）。下游只读这三样，不再对原始信号（延迟、丢包）各自反应。
//!
//! 每个信号只有一个主人：延迟只给 [`delay`]，丢包只给 [`loss`]，送达速率只给 [`acked`]，
//! 发送量只给 [`alr`]；码率只由 [`aimd`]（和丢包限制取小）决定。

pub mod acked;
pub mod aimd;
pub mod alr;
pub mod delay;
pub mod loss;
pub mod probe;
pub mod report;

use self::acked::AckedRate;
use self::aimd::Aimd;
use self::alr::Alr;
use self::delay::{DelayDetector, Usage};
use self::loss::LossLimiter;
use self::probe::{ProbeCluster, ProbeController, ProbeEstimator};
use self::report::{PacketResult, SentPacket};
use crate::time::Time;

/// 节奏速率是目标码率的多少倍（千分比）。WebRTC 视频是 1.1 倍（`kStrictPacingMultiplier`），
/// 还没收到第一份报告时 2.5 倍（`kDefaultPaceMultiplier`）。
pub const PACING_PERMILLE: u64 = 1100;
const PACING_BEFORE_FEEDBACK_PERMILLE: u64 = 2500;
/// 过载时还没有已确认速率：每 200 ms 减半（`InitialTimeToReduceFurther`）。
const INITIAL_HALVE_US: u64 = 200_000;

/// 网络控制器。
#[derive(Debug, Clone)]
pub struct Gcc {
    delay: DelayDetector,
    aimd: Aimd,
    acked: AckedRate,
    alr: Alr,
    loss: LossLimiter,
    probe: ProbeController,
    probe_est: ProbeEstimator,
    min_bps: u64,
    max_bps: u64,
    target_bps: u64,
    rtt_us: u64,
    reports: u64,
    last_halve: Option<Time>,
    started: bool,
    notes: Vec<String>,
}

/// 一条说明最多攒多少条（会话每秒取走）。
const MAX_NOTES: usize = 32;

impl Gcc {
    /// 从 `start_bps` 开始，夹在 `[min_bps, max_bps]`。
    #[must_use]
    pub fn new(start_bps: u64, min_bps: u64, max_bps: u64) -> Self {
        let aimd = Aimd::new(start_bps, min_bps, max_bps);
        let target = aimd.rate_bps();
        Self {
            delay: DelayDetector::new(),
            aimd,
            acked: AckedRate::new(),
            alr: Alr::new(target),
            loss: LossLimiter::new(),
            probe: ProbeController::new(max_bps),
            probe_est: ProbeEstimator::new(),
            min_bps: min_bps.max(1),
            max_bps: max_bps.max(min_bps.max(1)),
            target_bps: target,
            rtt_us: 0,
            reports: 0,
            last_halve: None,
            started: false,
            notes: Vec::new(),
        }
    }

    fn note(&mut self, text: String) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(text);
        }
    }

    /// 目标码率。
    #[must_use]
    pub const fn target_bps(&self) -> u64 {
        self.target_bps
    }

    /// 节奏速率：目标的 1.1 倍（还没有反馈时 2.5 倍）。
    #[must_use]
    pub const fn pacing_bps(&self) -> u64 {
        let factor = if self.reports == 0 {
            PACING_BEFORE_FEEDBACK_PERMILLE
        } else {
            PACING_PERMILLE
        };
        self.target_bps.saturating_mul(factor) / 1000
    }

    /// 是否应用受限（发得比估计少很多）。
    #[must_use]
    pub const fn in_alr(&self) -> bool {
        self.alr.in_alr()
    }

    /// 发出了一个包（含探测、重传）。
    pub fn on_sent(&mut self, now: Time, packet: &SentPacket) {
        self.alr.on_sent(now, u64::from(packet.size));
    }

    /// 定时（约 25 ms）：应用受限的预算照常累积；应用受限时定期探测。第一次调用时开始起始探测。
    pub fn on_tick(&mut self, now: Time, probe_at_start: bool) {
        if !self.started {
            self.started = true;
            if probe_at_start {
                self.probe.start(now, self.target_bps);
            }
        }
        self.alr.on_tick(now);
        self.probe.on_tick(now, self.alr.started(), self.target_bps);
    }

    /// 要发的探测组（已经登记好，等报告里的探测包回来算结果）。
    pub fn take_probes(&mut self) -> Vec<ProbeCluster> {
        let clusters = self.probe.take();
        for c in &clusters {
            self.probe_est.expect(c);
            self.note(format!(
                "探测 {} kbps（{} 个包）",
                c.target_bps / 1000,
                c.packets
            ));
        }
        clusters
    }

    /// 收到一份到达报告（已对账）。`rtt_us` 是当前往返时间。
    pub fn on_report(&mut self, now: Time, packets: &[PacketResult], rtt_us: u64) {
        if packets.is_empty() {
            return;
        }
        self.reports = self.reports.saturating_add(1);
        self.rtt_us = rtt_us;
        self.aimd.set_rtt(rtt_us);
        self.acked.on_packets(packets);
        let acked = self.acked.bitrate_bps();
        let probe = self.probe_est.on_packets(packets);
        let before_usage = self.delay.usage();
        let usage = self.delay.on_packets(packets);
        let recovered = before_usage == Usage::Under && usage == Usage::Normal;
        let before = self.target_bps;
        match (usage, acked, probe) {
            (Usage::Over, Some(a), _) => {
                if self.aimd.time_to_reduce(now, a) {
                    self.aimd.update(now, Usage::Over, Some(a));
                }
            }
            (Usage::Over, None, _) => {
                if self
                    .last_halve
                    .is_none_or(|t| now.since(t) >= INITIAL_HALVE_US)
                {
                    let half = self.aimd.rate_bps() / 2;
                    self.aimd.set_rate(now, half);
                    self.last_halve = Some(now);
                }
            }
            (Usage::Normal | Usage::Under, _, Some(p)) => {
                self.aimd.set_rate(now, p);
                self.probe.on_result(now, p);
                self.note(format!("探测结果 {} kbps", p / 1000));
            }
            (Usage::Normal | Usage::Under, _, None) => {
                self.aimd.update(now, usage, acked);
            }
        }
        let delay_bps = self.aimd.rate_bps();
        let limit = self.loss.on_report(now, packets, acked, delay_bps, rtt_us);
        self.target_bps = limit
            .map_or(delay_bps, |l| l.min(delay_bps))
            .clamp(self.min_bps, self.max_bps);
        self.alr.set_estimate(self.target_bps);
        self.probe.on_estimate(now, self.target_bps);
        if recovered {
            self.probe
                .on_recovered(now, self.alr.in_alr(), self.alr.ended());
        }
        self.explain(before, usage, acked, limit);
    }

    /// 码率变化较大（降、或升了 25% 以上）时写一条说明。
    fn explain(&mut self, before: u64, usage: Usage, acked: Option<u64>, limit: Option<u64>) {
        let after = self.target_bps;
        let big_rise = after.saturating_mul(4) >= before.saturating_mul(5);
        if after >= before && !big_rise {
            return;
        }
        let (trend, threshold) = self.delay.trend_and_threshold();
        let stats = self.loss.last();
        let why = match (usage, limit) {
            (_, Some(l)) if l <= after => "拥塞丢包（排队满了还在丢）".to_owned(),
            (Usage::Over, _) => "延迟趋势判过载".to_owned(),
            (Usage::Normal | Usage::Under, _) => "延迟正常，逐步上升".to_owned(),
        };
        self.note(format!(
            "码率 {} → {} kbps：{why}；趋势 {trend:.1} ms（门限 {threshold:.1}），已确认 {}，链路容量 {}，\
             排队 {} ms，丢包 {}‰（排队高时 {}，随机丢包的底 {}‰），{}",
            before / 1000,
            after / 1000,
            acked.map_or_else(|| "未知".to_owned(), |a| format!("{} kbps", a / 1000)),
            self.aimd
                .capacity_bps()
                .map_or_else(|| "未知".to_owned(), |c| format!("{} kbps", c / 1000)),
            stats.queue_us / 1000,
            stats.loss_permille,
            stats
                .high_permille
                .map_or_else(|| "没有排队高的包".to_owned(), |h| format!("{h}‰")),
            stats.floor_permille,
            if self.alr.in_alr() {
                "发送端少发（应用受限）"
            } else {
                "发送端在用满码率"
            },
        ));
    }

    /// 取走码率变化和探测的说明（诊断日志用）。
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// 一行状态（诊断日志用）。
    #[must_use]
    pub fn state_line(&self) -> String {
        let (trend, threshold) = self.delay.trend_and_threshold();
        let stats = self.loss.last();
        format!(
            "网络控制 目标 {} kbps，节奏 {} kbps；延迟 {:?}（趋势 {trend:.1} ms，门限 {threshold:.1}）；\
             已确认 {}；排队 {} ms，丢包 {}‰（底 {}‰）；{}",
            self.target_bps / 1000,
            self.pacing_bps() / 1000,
            self.delay.usage(),
            self.acked
                .bitrate_bps()
                .map_or_else(|| "未知".to_owned(), |a| format!("{} kbps", a / 1000)),
            stats.queue_us / 1000,
            stats.loss_permille,
            stats.floor_permille,
            if self.alr.in_alr() { "应用受限" } else { "在用满" },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc::report::PacketKind;

    /// 仿真一段：发送端按 `send_bps` 发 1200 字节的包，瓶颈 `link_bps`、单程 20 ms，
    /// 每 50 ms 一份报告。返回结束时刻。
    fn run(g: &mut Gcc, start: u64, ms: u64, send_bps: u64, link_bps: u64, seq: &mut u32) -> u64 {
        let gap = 1200 * 8 * 1_000_000 / send_bps;
        let service = 1200 * 8 * 1_000_000 / link_bps;
        let mut t = start;
        let mut queue_free = start;
        let mut batch = Vec::new();
        let mut next_report = start + 50_000;
        while t < start + ms * 1000 {
            let p = SentPacket {
                seq: *seq,
                send_time: Time(t),
                size: 1200,
                kind: PacketKind::Media,
            };
            *seq += 1;
            g.on_sent(Time(t), &p);
            // 瓶颈：先进先出，排队超过 200 ms 丢
            let begin = queue_free.max(t);
            let lost = begin - t > 200_000;
            if !lost {
                queue_free = begin + service;
            }
            batch.push(PacketResult {
                sent: p,
                recv_time: (!lost).then(|| Time(queue_free + 20_000)),
            });
            t += gap;
            if t >= next_report {
                g.on_report(Time(t + 40_000), &batch, 40_000);
                g.on_tick(Time(t), false);
                batch.clear();
                next_report += 50_000;
            }
        }
        t
    }

    #[test]
    fn overshooting_the_link_backs_off_near_capacity() {
        let mut g = Gcc::new(4_000_000, 100_000, 30_000_000);
        let mut seq = 0;
        // 发 4 Mbps 进 2 Mbps 的瓶颈：过载，降到已确认速率附近
        let t = run(&mut g, 0, 3_000, 4_000_000, 2_000_000, &mut seq);
        assert!(g.target_bps() < 2_200_000, "{}", g.target_bps());
        assert!(g.target_bps() > 1_000_000, "{}", g.target_bps());
        assert!(g.take_notes().iter().any(|n| n.contains("过载")));
        // 按估计发：稳住
        let target = g.target_bps();
        run(&mut g, t, 5_000, target, 2_000_000, &mut seq);
        assert!(
            g.target_bps() > 1_200_000 && g.target_bps() < 2_600_000,
            "{}",
            g.target_bps()
        );
        assert!(g.state_line().contains("网络控制 目标"));
        assert_eq!(g.pacing_bps(), g.target_bps() * PACING_PERMILLE / 1000);
    }

    #[test]
    fn an_application_limited_sender_is_not_cut() {
        let mut g = Gcc::new(8_000_000, 100_000, 30_000_000);
        let mut seq = 0;
        assert_eq!(
            g.pacing_bps(),
            8_000_000 * 2500 / 1000,
            "还没有反馈时 2.5 倍"
        );
        // 只发 300 kbps，链路 20 Mbps：不过载，不降（K-pipeline.md P3 的反面）
        run(&mut g, 0, 6_000, 300_000, 20_000_000, &mut seq);
        // 开头几份报告还没有已确认速率（包太少），按每秒 8% 稍微涨一点；之后被“已确认 × 1.5”挡住，不降也不再涨
        assert!(
            (8_000_000..8_200_000).contains(&g.target_bps()),
            "{}",
            g.target_bps()
        );
        assert!(g.in_alr());
        // 应用受限 5 秒以上：按估计的 2 倍探测
        let probes = g.take_probes();
        assert!(!probes.is_empty());
        assert!(probes
            .iter()
            .all(|p| (16_000_000..16_400_000).contains(&p.target_bps)));
    }

    #[test]
    fn start_probes_and_probe_results_set_the_rate() {
        let mut g = Gcc::new(1_000_000, 100_000, 30_000_000);
        g.on_tick(Time(0), true);
        let probes = g.take_probes();
        assert_eq!(probes.len(), 2);
        // 第二组探测（6 Mbps）全收到、链路 5 Mbps：结果约 4.75 Mbps
        let c = probes[1];
        let mut packets = Vec::new();
        for i in 0..c.packets {
            let send = Time(u64::from(i) * 1600);
            packets.push(PacketResult {
                sent: SentPacket {
                    seq: i,
                    send_time: send,
                    size: 1200,
                    kind: PacketKind::Probe(c.id),
                },
                recv_time: Some(Time(20_000 + u64::from(i) * 1920)),
            });
        }
        g.on_report(Time(100_000), &packets, 40_000);
        assert!(
            (4_000_000..=5_100_000).contains(&g.target_bps()),
            "{}",
            g.target_bps()
        );
        assert!(g.take_notes().iter().any(|n| n.contains("探测结果")));
        g.on_report(Time(100_000), &[], 40_000);
    }

    #[test]
    fn overuse_without_acked_rate_halves() {
        let mut g = Gcc::new(8_000_000, 100_000, 30_000_000);
        // 直接制造“过载”且没有已确认速率（包太少）：手动喂延迟检测器
        let mut t = 0u64;
        let mut halved = false;
        for (seq, i) in (0..400u64).enumerate() {
            let p = SentPacket {
                seq: u32::try_from(seq).unwrap(),
                send_time: Time(t),
                size: 1200,
                kind: PacketKind::Media,
            };
            let r = PacketResult {
                sent: p,
                recv_time: Some(Time(t + 20_000 + i * 1_500)),
            };
            // 每份报告只有一个包：已确认速率攒不够 10 个包之前没有值；之后有值
            g.acked = AckedRate::new();
            g.on_report(Time(t + 50_000), &[r], 40_000);
            if g.target_bps() <= 4_000_000 {
                halved = true;
                break;
            }
            t += 10_000;
        }
        assert!(halved, "{}", g.target_bps());
    }
}
