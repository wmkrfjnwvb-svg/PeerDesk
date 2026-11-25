//! 探测：发一小串填充包（按指定速率），看接收端实际收得多快，直接测出链路能不能撑住这个速率。
//!
//! 照 WebRTC：
//! - 什么时候探（`modules/congestion_controller/goog_cc/probe_controller.cc`）：会话开始按起始码率的 3 倍、
//!   6 倍各探一次，结果达到目标的 70% 就再翻倍接着探；应用受限（屏幕静止、发得少）时每 5 秒按估计的 2 倍探一次；
//!   刚从过载恢复、估计比降前低三分之一以上时，按降前的 85% 探一次（5 秒内、每 5 秒最多一次）。
//! - 怎么算结果（`probe_bitrate_estimator.cc`）：一组收到 80% 以上的包才算；发送速率扣最后一个包、
//!   到达速率扣第一个包；两个时间段都要在 1 秒以内；结果取两者小的，到达速率明显低于发送速率
//!   （< 90%）时取到达速率的 95%；发送和到达速率差两倍以上的不可信，丢掉。
//!
//! 探测由节奏器按组的速率发出（填充包，接收端只记到达时间、丢弃内容）。

use std::collections::BTreeMap;

use super::report::{PacketKind, PacketResult};
use crate::time::Time;

/// 起始探测的倍数（千分比）。
const FIRST_SCALE: u64 = 3000;
const SECOND_SCALE: u64 = 6000;
/// 结果达到目标的这个比例（千分比）就接着探。
const FURTHER_THRESHOLD: u64 = 700;
/// 接着探时按结果的几倍。
const FURTHER_SCALE: u64 = 2000;
/// 应用受限时多久探一次（微秒）和按估计的几倍（千分比）。
const ALR_INTERVAL_US: u64 = 5_000_000;
const ALR_SCALE: u64 = 2000;
/// 大降：估计掉到降前的这个比例（千分比）以下。
const DROP_THRESHOLD: u64 = 660;
/// 大降之后多久之内还探（微秒）、按降前的多少（千分比）。
const DROP_TIMEOUT_US: u64 = 5_000_000;
const AFTER_DROP: u64 = 850;
/// 退出应用受限之后多久之内还算（微秒）。
const ALR_ENDED_TIMEOUT_US: u64 = 3_000_000;
/// 一组探测：至少多少个包、发多久（微秒）。
pub const MIN_PROBE_PACKETS: u32 = 5;
pub const PROBE_DURATION_US: u64 = 15_000;
/// 探测包的大小（字节）。
pub const PROBE_PACKET_BYTES: u32 = 1200;
/// 一组收到多少（千分比）才算。
const MIN_RECEIVED: u64 = 800;
/// 发送、到达速率相差多少倍以上不可信。
const MAX_VALID_RATIO: u64 = 2;

/// 一组探测：按 `target_bps` 发 `packets` 个包。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeCluster {
    /// 组号。
    pub id: u32,
    /// 目标速率。
    pub target_bps: u64,
    /// 包数。
    pub packets: u32,
}

impl ProbeCluster {
    fn new(id: u32, target_bps: u64) -> Self {
        let bytes = target_bps.saturating_mul(PROBE_DURATION_US) / 8_000_000;
        let packets = u32::try_from(bytes / u64::from(PROBE_PACKET_BYTES))
            .unwrap_or(u32::MAX)
            .max(MIN_PROBE_PACKETS);
        Self {
            id,
            target_bps,
            packets,
        }
    }
}

/// 探测控制：决定什么时候、按多少探。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeController {
    max_bps: u64,
    next_id: u32,
    /// 起始探测还在等结果：最近一次探的目标。
    waiting: Option<u64>,
    last_probe: Option<Time>,
    /// 最近一次大降：(降前的估计, 时刻)。
    drop: Option<(u64, Time)>,
    last_drop_probe: Option<Time>,
    /// 上一次的估计（判断大降用）。
    last_estimate: u64,
    pending: Vec<ProbeCluster>,
}

impl ProbeController {
    /// 创建；`max_bps` 是探测的上限。
    #[must_use]
    pub const fn new(max_bps: u64) -> Self {
        Self {
            max_bps,
            next_id: 1,
            waiting: None,
            last_probe: None,
            drop: None,
            last_drop_probe: None,
            last_estimate: 0,
            pending: Vec::new(),
        }
    }

    fn probe(&mut self, now: Time, target_bps: u64) -> bool {
        let target = target_bps.min(self.max_bps);
        if target == 0 {
            return false;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.pending.push(ProbeCluster::new(id, target));
        self.last_probe = Some(now);
        true
    }

    /// 会话开始：按起始码率的 3 倍、6 倍各探一次。
    pub fn start(&mut self, now: Time, start_bps: u64) {
        self.last_estimate = start_bps;
        let first = start_bps.saturating_mul(FIRST_SCALE) / 1000;
        let second = start_bps.saturating_mul(SECOND_SCALE) / 1000;
        if self.probe(now, first) {
            self.probe(now, second);
            self.waiting = Some(second.min(self.max_bps));
        }
    }

    /// 有了探测结果（起始探测阶段：达到目标的 70% 就翻倍接着探）。
    pub fn on_result(&mut self, now: Time, result_bps: u64) {
        let Some(target) = self.waiting.take() else {
            return;
        };
        if result_bps.saturating_mul(1000) >= target.saturating_mul(FURTHER_THRESHOLD)
            && target < self.max_bps
        {
            let next = result_bps.saturating_mul(FURTHER_SCALE) / 1000;
            if self.probe(now, next) {
                self.waiting = Some(next.min(self.max_bps));
            }
        }
    }

    /// 估计变了：记大降。
    pub fn on_estimate(&mut self, now: Time, estimate_bps: u64) {
        if estimate_bps.saturating_mul(1000) < self.last_estimate.saturating_mul(DROP_THRESHOLD) {
            self.drop = Some((self.last_estimate, now));
        }
        self.last_estimate = estimate_bps;
    }

    /// 刚从过载恢复（低载 → 正常）：如果之前大降过、而且是应用受限或刚退出，按降前的 85% 探一次。
    pub fn on_recovered(&mut self, now: Time, alr_now: bool, alr_ended: Option<Time>) {
        let Some((before, at)) = self.drop else {
            return;
        };
        let recent_alr = alr_now || alr_ended.is_some_and(|t| now.since(t) < ALR_ENDED_TIMEOUT_US);
        let fresh = now.since(at) < DROP_TIMEOUT_US;
        let spaced = self
            .last_drop_probe
            .is_none_or(|t| now.since(t) >= DROP_TIMEOUT_US);
        if recent_alr
            && fresh
            && spaced
            && self.probe(now, before.saturating_mul(AFTER_DROP) / 1000)
        {
            self.last_drop_probe = Some(now);
            self.drop = None;
        }
    }

    /// 定时调用：应用受限时每 5 秒按估计的 2 倍探一次。
    pub fn on_tick(&mut self, now: Time, alr_started: Option<Time>, estimate_bps: u64) {
        let Some(started) = alr_started else {
            return;
        };
        let since = self.last_probe.map_or(started, |p| p.max(started));
        if now.since(since) >= ALR_INTERVAL_US && estimate_bps < self.max_bps {
            self.probe(now, estimate_bps.saturating_mul(ALR_SCALE) / 1000);
        }
    }

    /// 取走要发的探测组。
    pub fn take(&mut self) -> Vec<ProbeCluster> {
        std::mem::take(&mut self.pending)
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ClusterStats {
    sent: u32,
    received: u32,
    bytes: u64,
    first_send: Option<Time>,
    last_send: Option<(Time, u64)>,
    first_recv: Option<(Time, u64)>,
    last_recv: Option<Time>,
}

/// 探测结果：按组统计收到的探测包，够了就算出速率。
#[derive(Debug, Clone, Default)]
pub struct ProbeEstimator {
    clusters: BTreeMap<u32, ClusterStats>,
    /// 每组要多少个包（发的时候登记）。
    expected: BTreeMap<u32, u32>,
}

impl ProbeEstimator {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            clusters: BTreeMap::new(),
            expected: BTreeMap::new(),
        }
    }

    /// 登记一组探测（发出之前）。
    pub fn expect(&mut self, cluster: &ProbeCluster) {
        self.expected.insert(cluster.id, cluster.packets);
        // 只留最近几组
        while self.expected.len() > 8 {
            if let Some((&old, _)) = self.expected.iter().next() {
                self.expected.remove(&old);
                self.clusters.remove(&old);
            }
        }
    }

    /// 处理一份报告里的探测包，返回这次能算出的最新结果（比特每秒）。
    pub fn on_packets(&mut self, packets: &[PacketResult]) -> Option<u64> {
        let mut touched = Vec::new();
        for p in packets {
            let PacketKind::Probe(id) = p.sent.kind else {
                continue;
            };
            if !self.expected.contains_key(&id) {
                continue;
            }
            let c = self.clusters.entry(id).or_default();
            let size = u64::from(p.sent.size);
            c.sent = c.sent.saturating_add(1);
            let send = p.sent.send_time;
            c.first_send = Some(c.first_send.map_or(send, |f| f.min(send)));
            if c.last_send.is_none_or(|(t, _)| send >= t) {
                c.last_send = Some((send, size));
            }
            if let Some(recv) = p.recv_time {
                c.received = c.received.saturating_add(1);
                c.bytes = c.bytes.saturating_add(size);
                if c.first_recv.is_none_or(|(t, _)| recv < t) {
                    c.first_recv = Some((recv, size));
                }
                c.last_recv = Some(c.last_recv.map_or(recv, |l| l.max(recv)));
            }
            touched.push(id);
        }
        touched
            .into_iter()
            .filter_map(|id| self.estimate(id))
            .last()
    }

    fn estimate(&mut self, id: u32) -> Option<u64> {
        let expected = u64::from(*self.expected.get(&id)?);
        let c = *self.clusters.get(&id)?;
        if u64::from(c.received).saturating_mul(1000) < expected.saturating_mul(MIN_RECEIVED) {
            return None;
        }
        let (first_send, (last_send, last_size)) = c.first_send.zip(c.last_send)?;
        let ((first_recv, first_size), last_recv) = c.first_recv.zip(c.last_recv)?;
        let send_us = last_send.since(first_send);
        let recv_us = last_recv.since(first_recv);
        if send_us == 0 || recv_us == 0 || send_us > 1_000_000 || recv_us > 1_000_000 {
            self.clusters.remove(&id);
            self.expected.remove(&id);
            return None;
        }
        let send_bps = c.bytes.saturating_sub(last_size).saturating_mul(8_000_000) / send_us;
        let recv_bps = c.bytes.saturating_sub(first_size).saturating_mul(8_000_000) / recv_us;
        // 一组只出一次结果
        self.clusters.remove(&id);
        self.expected.remove(&id);
        if send_bps > recv_bps.saturating_mul(MAX_VALID_RATIO)
            || recv_bps > send_bps.saturating_mul(MAX_VALID_RATIO)
        {
            return None;
        }
        Some(
            if recv_bps.saturating_mul(10) < send_bps.saturating_mul(9) {
                recv_bps.saturating_mul(95) / 100
            } else {
                send_bps.min(recv_bps)
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc::report::SentPacket;

    /// 一组探测：按 `send_bps` 发，瓶颈 `link_bps`（到达间隔按两者慢的）；`lost(i)` 的没收到。
    fn cluster(
        c: &ProbeCluster,
        start: u64,
        link_bps: u64,
        lost: impl Fn(u32) -> bool,
    ) -> Vec<PacketResult> {
        let size = u64::from(PROBE_PACKET_BYTES);
        let send_gap = size * 8_000_000 / c.target_bps;
        let recv_gap = size * 8_000_000 / link_bps.min(c.target_bps);
        (0..c.packets)
            .map(|i| PacketResult {
                sent: SentPacket {
                    seq: i,
                    send_time: Time(start + u64::from(i) * send_gap),
                    size: PROBE_PACKET_BYTES,
                    kind: PacketKind::Probe(c.id),
                },
                recv_time: (!lost(i)).then(|| Time(start + 20_000 + u64::from(i) * recv_gap)),
            })
            .collect()
    }

    #[test]
    fn start_probes_climb_while_the_link_keeps_up() {
        let mut p = ProbeController::new(30_000_000);
        p.start(Time(0), 1_000_000);
        let first = p.take();
        assert_eq!(first.len(), 2);
        assert_eq!(
            (first[0].target_bps, first[1].target_bps),
            (3_000_000, 6_000_000)
        );
        assert!(first[1].packets >= MIN_PROBE_PACKETS);
        // 结果跟上了：翻倍接着探
        p.on_result(Time(100_000), 5_500_000);
        assert_eq!(p.take()[0].target_bps, 11_000_000);
        // 没跟上：停
        p.on_result(Time(200_000), 3_000_000);
        assert!(p.take().is_empty());
        p.on_result(Time(300_000), 3_000_000);
        assert!(p.take().is_empty(), "不在等结果");
        // 上限
        let mut capped = ProbeController::new(2_000_000);
        capped.start(Time(0), 1_000_000);
        assert!(capped.take().iter().all(|c| c.target_bps == 2_000_000));
        capped.on_result(Time(1), 2_000_000);
        assert!(capped.take().is_empty(), "已经到上限");
        let mut zero = ProbeController::new(0);
        zero.start(Time(0), 1_000_000);
        assert!(zero.take().is_empty());
    }

    #[test]
    fn alr_and_drop_probes() {
        let mut p = ProbeController::new(30_000_000);
        p.on_tick(Time(1_000_000), None, 2_000_000);
        assert!(p.take().is_empty(), "不受限不探");
        p.on_tick(Time(4_000_000), Some(Time(0)), 2_000_000);
        assert!(p.take().is_empty(), "不到 5 秒");
        p.on_tick(Time(5_000_000), Some(Time(0)), 2_000_000);
        assert_eq!(p.take()[0].target_bps, 4_000_000);
        p.on_tick(Time(6_000_000), Some(Time(0)), 2_000_000);
        assert!(p.take().is_empty(), "刚探过");
        // 大降之后恢复：应用受限时按降前的 85% 探
        p.on_estimate(Time(7_000_000), 4_000_000);
        p.on_estimate(Time(7_100_000), 1_000_000);
        p.on_recovered(Time(7_500_000), false, None);
        assert!(p.take().is_empty(), "不受限不探");
        p.on_recovered(Time(7_600_000), true, None);
        assert_eq!(p.take()[0].target_bps, 3_400_000);
        p.on_estimate(Time(8_000_000), 4_000_000);
        p.on_estimate(Time(8_100_000), 1_000_000);
        p.on_recovered(Time(8_200_000), false, Some(Time(8_000_000)));
        assert!(p.take().is_empty(), "5 秒内只探一次");
        p.on_recovered(Time(20_000_000), true, None);
        assert!(p.take().is_empty(), "太久了");
    }

    #[test]
    fn estimator_measures_the_bottleneck() {
        let mut e = ProbeEstimator::new();
        let c = ProbeCluster::new(1, 6_000_000);
        e.expect(&c);
        // 瓶颈 3 Mbps：到达速率只有一半，取它的 95%
        let r = e.on_packets(&cluster(&c, 0, 3_000_000, |_| false)).unwrap();
        assert!((2_700_000..=3_000_000).contains(&r), "{r}");
        // 同一组不再出结果
        assert_eq!(e.on_packets(&cluster(&c, 0, 3_000_000, |_| false)), None);
        // 链路够快：取发送和到达里小的
        let c2 = ProbeCluster::new(2, 2_000_000);
        e.expect(&c2);
        let r = e
            .on_packets(&cluster(&c2, 0, 10_000_000, |_| false))
            .unwrap();
        assert!((1_800_000..=2_100_000).contains(&r), "{r}");
        // 丢得太多：不算
        let c3 = ProbeCluster::new(3, 2_000_000);
        e.expect(&c3);
        assert_eq!(
            e.on_packets(&cluster(&c3, 0, 10_000_000, |i| i % 2 == 0)),
            None
        );
        // 不认识的组、不是探测的包：不管
        let mut media = cluster(&ProbeCluster::new(9, 2_000_000), 0, 1, |_| false);
        assert_eq!(e.on_packets(&media), None);
        for m in &mut media {
            m.sent.kind = PacketKind::Media;
        }
        assert_eq!(e.on_packets(&media), None);
        // 太离谱（到达比发送快两倍以上）：丢掉
        let c4 = ProbeCluster::new(4, 2_000_000);
        e.expect(&c4);
        let mut fast = cluster(&c4, 0, 10_000_000, |_| false);
        for (i, f) in fast.iter_mut().enumerate() {
            f.recv_time = Some(Time(30_000 + u64::try_from(i).unwrap()));
        }
        assert_eq!(e.on_packets(&fast), None);
        // 时间段为 0：丢掉
        let c5 = ProbeCluster::new(5, 2_000_000);
        e.expect(&c5);
        let mut same = cluster(&c5, 0, 10_000_000, |_| false);
        for f in &mut same {
            f.sent.send_time = Time(0);
        }
        assert_eq!(e.on_packets(&same), None);
        // 登记太多：最老的扔掉
        for id in 10..30 {
            e.expect(&ProbeCluster::new(id, 1_000_000));
        }
        assert!(e.expected.len() <= 8);
    }
}
