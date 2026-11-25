//! 按丢包限制码率：只对“拥塞丢包”反应，随机丢包不降码率（CLAUDE.md：Wi-Fi 干扰不降码率，
//! 只有伴随延迟上升的拥塞丢包才降）。
//!
//! 怎么分：拥塞丢包是瓶颈队列满了才丢，丢的包前后排队一定高；随机丢包和排队无关。所以逐个包看
//! “它被发出时链路上排了多久”（丢了的包按前一个收到的包算），分成两拨比：
//! - **随机丢包的底**：最近 100 个排队短（< 30 ms）的包的丢包率。
//! - **排队高时的丢包率**：最近半秒里排队 ≥ 30 ms 的包的丢包率。
//! - **拥塞丢包**：排队高的包至少 20 个，丢包率比底高出 10% 以上，而且统计上站得住
//!   （多丢的包数 ≥ 三倍标准差，二项分布）。至少 20 个是因为随机丢包会连着丢几个，丢了的包都按前一个
//!   收到的包算排队：样本少时一串连续丢包（20% 随机丢包时连丢 4 个并不罕见）就能凑出“排队高时丢 4/4”。
//!   这时限制到已确认速率的 85%；两次收紧至少隔“300 ms + 往返”。
//! - **放宽**：不再判拥塞、排队高时也不比底多丢（高出不到 2%）时每秒放宽 8%，放宽到不再起作用就撤掉。
//!
//! 排队延迟 = 单程延迟 − 最近 10 秒里最小的单程延迟（两端时钟的固定差在相减时抵消）。
//!
//! 为什么逐包比、底用同一时期的包（2026-11 实测，微软轨迹 ms-05729：1 Mbps，第 7–8 秒随机丢包从 0 升到 20%，
//! 整段队列一次都没满）：最早只看绝对丢包率，一次 31 ms 的排队碰上 16% 的随机丢包就被当成拥塞，
//! 码率 672 → 277 kbps，之后丢包率永远 ≥ 2%，限制再也不放宽；改成“比 5 秒的底高 10%”之后，
//! 底跟不上丢包率的上升，还是误判。两拨包在同一时期比，随机丢包怎么变都一样，拥塞时只有排队高的那拨多丢。
//!
//! WebRTC 的对应物是 `LossBasedBweV2`：它估计“固有丢包率”（inherent loss），丢包高于固有值才压码率，
//! 思路相同；这里用排队延迟把包分开，规则更直接。

use std::collections::VecDeque;

use super::report::PacketResult;
use crate::time::Time;

/// 丢包率统计的时间窗（微秒）。
const LOSS_WINDOW_US: u64 = 500_000;
/// 排队（微秒）不低于它的包算“排队高”。
const HIGH_QUEUE_US: i64 = 30_000;
/// 拥塞丢包：排队高时的丢包率比底高出（千分比）不少于它。
const CONGESTED_EXCESS_PERMILLE: u64 = 100;
/// 排队高时比底多丢不到它（千分比）时放宽。
const LOW_EXCESS_PERMILLE: u64 = 20;
/// 随机丢包的底按最近多少个排队短的包统计。
const FLOOR_PACKETS: usize = 100;
/// 排队高的包至少要有这么多才判断（样本太少时一串连续的随机丢包就能凑出“排队高时丢 4/4”）。
const MIN_HIGH_PACKETS: u64 = 20;
/// 多丢的包数至少是几倍标准差才算站得住。
const SIGNIFICANCE_SIGMAS: f64 = 3.0;
/// 收紧到已确认速率的多少（千分比）。
const BACKOFF_PERMILLE: u64 = 850;
/// 每秒放宽多少（千分比）。
const RELAX_PER_SECOND_PERMILLE: u64 = 80;
/// 两次收紧之间至少隔多久（往返之外，微秒）。
const BACKOFF_HOLD_US: u64 = 300_000;
/// 基准延迟的窗口（微秒），两个窗口取小，实际记 10–20 秒。
const BASE_WINDOW_US: u64 = 10_000_000;

/// 诊断用的丢包统计（千分比和微秒）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LossStats {
    /// 最近一份报告里最小的排队延迟（微秒）。
    pub queue_us: i64,
    /// 最近半秒的丢包率。
    pub loss_permille: u64,
    /// 随机丢包的底（排队短的包的丢包率）。
    pub floor_permille: u64,
    /// 最近半秒排队高的包的丢包率（没有这样的包时为 `None`）。
    pub high_permille: Option<u64>,
}

/// 按丢包的限制。
#[derive(Debug, Clone, Default)]
pub struct LossLimiter {
    /// 最近的报告：(时刻, 丢的包, 总包数)。
    recent: VecDeque<(Time, u64, u64)>,
    /// 最近报告里排队高的包：(时刻, 丢的包, 总包数)。
    high: VecDeque<(Time, u64, u64)>,
    /// 最近排队短的包是否丢了（统计随机丢包的底）。
    calm: VecDeque<bool>,
    calm_lost: usize,
    base: [Option<i64>; 2],
    base_started: Option<Time>,
    /// 上一个收到的包的排队延迟（下一份报告开头丢的包按它算）。
    last_queue: Option<i64>,
    limit: Option<u64>,
    last_backoff: Option<Time>,
    last_update: Option<Time>,
    stats: LossStats,
}

impl LossLimiter {
    /// 创建。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 现在的限制（`None`：不限）。
    #[must_use]
    pub const fn limit(&self) -> Option<u64> {
        self.limit
    }

    /// 最近的统计。
    #[must_use]
    pub const fn last(&self) -> LossStats {
        self.stats
    }

    fn push_calm(&mut self, lost: bool) {
        self.calm.push_back(lost);
        self.calm_lost += usize::from(lost);
        if self.calm.len() > FLOOR_PACKETS {
            if let Some(old) = self.calm.pop_front() {
                self.calm_lost -= usize::from(old);
            }
        }
    }

    /// 处理一份报告。`acked_bps` 是已确认速率，`estimate_bps` 是按延迟的估计（限制放宽到它以上就撤掉）。
    pub fn on_report(
        &mut self,
        now: Time,
        packets: &[PacketResult],
        acked_bps: Option<u64>,
        estimate_bps: u64,
        rtt_us: u64,
    ) -> Option<u64> {
        // 每个包的单程延迟（含两端时钟差）
        let delays: Vec<Option<i64>> = packets
            .iter()
            .map(|p| {
                p.recv_time.map(|r| {
                    i64::try_from(r.micros()).unwrap_or(i64::MAX)
                        - i64::try_from(p.sent.send_time.micros()).unwrap_or(i64::MAX)
                })
            })
            .collect();
        let base = delays.iter().flatten().min().copied().map(|min| {
            let started = *self.base_started.get_or_insert(now);
            if now.since(started) >= BASE_WINDOW_US {
                self.base = [None, self.base[0]];
                self.base_started = Some(now);
            }
            self.base[0] = Some(self.base[0].map_or(min, |b| b.min(min)));
            let base = self.base.iter().flatten().min().copied().unwrap_or(min);
            self.stats.queue_us = min - base;
            base
        });
        // 逐包分拨：丢了的包按前一个收到的包的排队算（报告开头丢的按上一份报告最后一个、或这份第一个收到的）
        let mut queue = self
            .last_queue
            .or_else(|| base.and_then(|b| delays.iter().flatten().next().map(|d| d - b)));
        let (mut high_lost, mut high_total, mut lost, mut total) = (0u64, 0u64, 0u64, 0u64);
        for d in &delays {
            if let (Some(d), Some(b)) = (d, base) {
                queue = Some(d - b);
            }
            let is_lost = d.is_none();
            total += 1;
            lost += u64::from(is_lost);
            match queue {
                Some(q) if q >= HIGH_QUEUE_US => {
                    high_total += 1;
                    high_lost += u64::from(is_lost);
                }
                Some(_) => self.push_calm(is_lost),
                None => {}
            }
        }
        self.last_queue = queue;
        push_window(&mut self.recent, now, lost, total);
        push_window(&mut self.high, now, high_lost, high_total);
        let (l, n) = sums(&self.recent);
        self.stats.loss_permille = l.saturating_mul(1000) / n.max(1);
        let floor = u64::try_from(self.calm_lost)
            .unwrap_or(u64::MAX)
            .saturating_mul(1000)
            / u64::try_from(self.calm.len()).unwrap_or(u64::MAX).max(1);
        self.stats.floor_permille = floor;
        let (hl, hn) = sums(&self.high);
        self.stats.high_permille = (hn > 0).then(|| hl.saturating_mul(1000) / hn);
        let excess = self
            .stats
            .high_permille
            .map_or(0, |h| h.saturating_sub(floor));
        let congested = hn >= MIN_HIGH_PACKETS
            && excess >= CONGESTED_EXCESS_PERMILLE
            && significant(hl, hn, floor);
        let hold = BACKOFF_HOLD_US.saturating_add(rtt_us);
        if congested {
            if let Some(acked) = acked_bps {
                if self.last_backoff.is_none_or(|t| now.since(t) >= hold) {
                    let target = acked.saturating_mul(BACKOFF_PERMILLE) / 1000;
                    self.limit = Some(self.limit.map_or(target, |l| l.min(target)));
                    self.last_backoff = Some(now);
                }
            }
        } else if excess < LOW_EXCESS_PERMILLE {
            if let Some(limit) = self.limit {
                let dt = self.last_update.map_or(0, |t| now.since(t)).min(1_000_000);
                let grown = limit
                    .saturating_add(
                        limit
                            .saturating_mul(RELAX_PER_SECOND_PERMILLE)
                            .saturating_mul(dt)
                            / 1_000_000_000,
                    )
                    .saturating_add(1);
                self.limit = (grown < estimate_bps).then_some(grown);
            }
        }
        self.last_update = Some(now);
        self.limit
    }
}

/// 记一份报告的 (丢的包, 总包数)，去掉半秒以前的。
fn push_window(window: &mut VecDeque<(Time, u64, u64)>, now: Time, lost: u64, total: u64) {
    window.push_back((now, lost, total));
    while window
        .front()
        .is_some_and(|(t, _, _)| now.since(*t) > LOSS_WINDOW_US)
    {
        window.pop_front();
    }
}

/// 一串报告的 (丢的包, 总包数)。
fn sums(reports: &VecDeque<(Time, u64, u64)>) -> (u64, u64) {
    reports
        .iter()
        .fold((0u64, 0u64), |(a, b), (_, l, n)| (a + l, b + n))
}

/// `n` 个包丢了 `lost` 个，比随机丢包的底 `floor_permille` 多出来的是否超过三倍标准差（二项分布）。
/// 底为 0 时标准差按 1 个包算，至少多丢 3 个才算。
fn significant(lost: u64, n: u64, floor_permille: u64) -> bool {
    let n = n as f64;
    let p = floor_permille as f64 / 1000.0;
    let sigma = (n * p * (1.0 - p)).sqrt().max(1.0);
    lost as f64 - n * p >= SIGNIFICANCE_SIGMAS * sigma
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cc::report::{PacketKind, SentPacket};

    /// 一份报告：20 个包，每 2 ms 一个；单程延迟 `delay_us`；`lost` 个丢了（均匀分布，第一个不丢）。
    fn report(start: u64, delay_us: u64, lost: u32) -> Vec<PacketResult> {
        (0..20u32)
            .map(|i| {
                let send = Time(start + u64::from(i) * 2_000);
                let is_lost = (i + 1) * lost / 20 > i * lost / 20;
                PacketResult {
                    sent: SentPacket {
                        seq: i,
                        send_time: send,
                        size: 1200,
                        kind: PacketKind::Media,
                    },
                    recv_time: (!is_lost).then(|| send.plus(delay_us)),
                }
            })
            .collect()
    }

    fn feed(l: &mut LossLimiter, t: &mut u64, delay_us: u64, lost: u32, acked: u64) -> Option<u64> {
        *t += 40_000;
        l.on_report(
            Time(*t),
            &report(*t, delay_us, lost),
            Some(acked),
            1_000_000,
            40_000,
        )
    }

    #[test]
    fn random_loss_with_a_short_queue_does_not_limit() {
        let mut l = LossLimiter::new();
        let mut t = 0;
        for _ in 0..50 {
            // 15% 丢包，但排队延迟不变（随机丢包）
            assert_eq!(feed(&mut l, &mut t, 20_000, 3, 600_000), None);
        }
        let s = l.last();
        assert!(s.queue_us < 5_000 && s.loss_permille >= 100, "{s:?}");
        assert_eq!(s.floor_permille, 150);
        assert_eq!(s.high_permille, None);
    }

    #[test]
    fn a_queue_blip_on_a_lossy_link_is_not_congestion() {
        // ms-05729 的情形：20% 随机丢包，排队很短；偶尔排队到 31 ms，丢包率还是 20% 左右
        let mut l = LossLimiter::new();
        let mut t = 0;
        for _ in 0..50 {
            feed(&mut l, &mut t, 20_000, 4, 600_000);
        }
        assert_eq!(l.last().floor_permille, 200, "底是 20%");
        for _ in 0..5 {
            assert_eq!(feed(&mut l, &mut t, 51_000, 5, 600_000), None);
        }
        assert_eq!(l.last().high_permille, Some(250));
        // 真拥塞：排队高的包在底之上多丢了一半
        assert_eq!(feed(&mut l, &mut t, 100_000, 14, 600_000), Some(510_000));
        // 回到 20% 随机丢包、排队短：照样放宽，直到撤掉
        for _ in 0..400 {
            feed(&mut l, &mut t, 20_000, 4, 600_000);
        }
        assert_eq!(l.limit(), None);
    }

    #[test]
    fn excess_loss_must_be_statistically_significant() {
        // 底 20%，10 个包丢 5 个（50%）：多出的 3 个不到三倍标准差（3.8 个）
        assert!(!significant(5, 10, 200));
        assert!(significant(6, 10, 200));
        // 底为 0：至少多丢 3 个
        assert!(!significant(2, 10, 0));
        assert!(significant(3, 10, 0));
    }

    #[test]
    fn loss_with_a_full_queue_limits_then_relaxes() {
        let mut l = LossLimiter::new();
        let mut t = 0;
        for _ in 0..10 {
            feed(&mut l, &mut t, 20_000, 0, 4_000_000);
        }
        assert_eq!(l.limit(), None);
        // 队列满了（排队 80 ms）还在丢：限制到已确认的 85%
        assert_eq!(
            feed(&mut l, &mut t, 100_000, 10, 4_000_000),
            Some(3_400_000)
        );
        // 太快不再收紧
        feed(&mut l, &mut t, 100_000, 10, 2_000_000);
        assert_eq!(l.limit(), Some(3_400_000));
        // 还在拥塞（排队高的包多丢）：不放宽
        for _ in 0..20 {
            feed(&mut l, &mut t, 100_000, 10, 4_000_000);
        }
        assert!(l.limit().is_some_and(|v| v <= 3_400_000));
        // 丢包停了、队列排空：每秒放宽 8%，放到估计以上就撤掉
        for _ in 0..200 {
            feed(&mut l, &mut t, 20_000, 0, 4_000_000);
        }
        assert_eq!(l.limit(), None);
        // 没有已确认速率：不收紧
        let mut fresh = LossLimiter::new();
        fresh.on_report(Time(0), &report(0, 20_000, 0), None, 6_000_000, 40_000);
        assert_eq!(
            fresh.on_report(
                Time(40_000),
                &report(40_000, 100_000, 10),
                None,
                6_000_000,
                40_000
            ),
            None
        );
    }

    #[test]
    fn lost_packets_take_the_queue_of_the_packet_before() {
        let mut l = LossLimiter::new();
        // 第一份全丢：还没有基准，不分拨
        l.on_report(Time(0), &report(0, 20_000, 20), Some(1), 1, 0);
        let s = l.last();
        assert_eq!(
            (s.loss_permille, s.high_permille, l.calm.len()),
            (1000, None, 0)
        );
        let mut t = 0;
        feed(&mut l, &mut t, 20_000, 0, 1);
        // 下一份排队高、开头就丢：开头的包按上一份最后一个包（排队短）算
        let mut r = report(t + 40_000, 90_000, 0);
        if let Some(first) = r.first_mut() {
            first.recv_time = None;
        }
        l.on_report(Time(t + 40_000), &r, Some(1), 1, 0);
        let s = l.last();
        assert_eq!((s.high_permille, s.floor_permille), (Some(0), 1000 / 21));
    }

    #[test]
    fn base_delay_window_rolls() {
        let mut w = LossLimiter::new();
        w.on_report(Time(0), &report(0, 20_000, 0), None, 1, 0);
        w.on_report(
            Time(BASE_WINDOW_US + 1),
            &report(BASE_WINDOW_US, 25_000, 0),
            None,
            1,
            0,
        );
        w.on_report(
            Time(2 * BASE_WINDOW_US + 2),
            &report(2 * BASE_WINDOW_US, 25_000, 0),
            None,
            1,
            0,
        );
        assert_eq!(w.last().queue_us, 0, "老的基准过期了");
    }
}
