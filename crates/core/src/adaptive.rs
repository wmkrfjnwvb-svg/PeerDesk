//! 自适应传输：接收端测量、反馈消息、发送端码率控制。
//!
//! 接收端用 [`DelayTracker`] 测量排队延迟、丢包率和接收速率，连同“哪些帧送达了”
//! 一起放进 [`Feedback`] 发回去。发送端的 [`RateController`] 据此调整码率：
//! 排队延迟升高说明链路在积压，立刻降；延迟很低说明还有余量，慢慢升。
//! 单纯的丢包不触发降码率（Wi-Fi 的随机丢包不代表拥塞），除非它伴随着延迟升高。
//!
//! 码率最终变成“每帧字节预算”交给编码器，编码器必须遵守。

use crate::wire::{Reader, Writer};
use crate::{Error, Time};

/// 接收端发给发送端的反馈。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Feedback {
    /// 帧号不大于它的帧都已有定论（送达或作废）。
    pub floor: u32,
    /// 第 i 位表示第 `floor - i` 帧已送达。
    pub delivered: u32,
    /// 这段时间里估计的排队延迟（微秒）。
    pub queue_delay_us: u32,
    /// 这段时间里的丢包率（千分比）。
    pub loss_permille: u16,
    /// 这段时间里的接收速率（字节每秒）。
    pub recv_bytes_per_s: u32,
}

impl Feedback {
    /// 编码后的长度。
    pub const LEN: usize = 18;

    /// 编码。
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(self.floor)
            .u32(self.delivered)
            .u32(self.queue_delay_us)
            .u16(self.loss_permille)
            .u32(self.recv_bytes_per_s);
        w.into_vec()
    }

    /// 解码。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let feedback = Self {
            floor: r.u32()?,
            delivered: r.u32()?,
            queue_delay_us: r.u32()?,
            loss_permille: r.u16()?,
            recv_bytes_per_s: r.u32()?,
        };
        r.finish()?;
        if feedback.loss_permille > 1000 {
            return Err(Error::Inconsistent("丢包率超过 100%"));
        }
        Ok(feedback)
    }
}

/// 记录最近哪些帧送达了，生成反馈里的 `floor` 和 `delivered`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DeliveryLog {
    floor: u32,
    mask: u32,
}

impl DeliveryLog {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self { floor: 0, mask: 0 }
    }

    /// 把“已有定论”的界线推进到 `floor`。
    pub fn advance(&mut self, floor: u32) {
        if floor > self.floor {
            let shift = floor - self.floor;
            self.mask = self.mask.checked_shl(shift).unwrap_or(0);
            self.floor = floor;
        }
    }

    /// 记录第 `frame_id` 帧已送达。
    pub fn delivered(&mut self, frame_id: u32) {
        self.advance(frame_id);
        let age = self.floor - frame_id;
        if age < 32 {
            self.mask |= 1 << age;
        }
    }

    /// 当前的 `(floor, delivered)`。
    #[must_use]
    pub const fn snapshot(&self) -> (u32, u32) {
        (self.floor, self.mask)
    }
}

/// 基准延迟的统计窗口（微秒）：取两个窗口里的最小值，所以实际记忆 10 到 20 秒。
const BASE_WINDOW_US: u64 = 10_000_000;

/// 接收端：测量排队延迟、丢包率、接收速率。
///
/// 排队延迟 = 这段时间里最小的单程延迟 − 最近十几秒里最小的单程延迟。
/// 两端时钟不同步没有关系，固定的差值在相减时抵消了。
#[derive(Debug, Clone)]
pub struct DelayTracker {
    base: [Option<i64>; 2],
    base_started: Time,
    interval_min: Option<i64>,
    interval_started: Time,
    received: u32,
    bytes: u64,
    /// 上一次报告时见过的最大序号，和这段时间里见过的最大序号。
    reported_seq: Option<u32>,
    highest_seq: Option<u32>,
}

/// [`DelayTracker::report`] 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DelaySample {
    /// 排队延迟（微秒）。
    pub queue_delay_us: u32,
    /// 丢包率（千分比）。
    pub loss_permille: u16,
    /// 接收速率（字节每秒）。
    pub recv_bytes_per_s: u32,
}

impl DelayTracker {
    /// 创建。
    #[must_use]
    pub const fn new(now: Time) -> Self {
        Self {
            base: [None, None],
            base_started: now,
            interval_min: None,
            interval_started: now,
            received: 0,
            bytes: 0,
            reported_seq: None,
            highest_seq: None,
        }
    }

    /// 收到一个数据包。`seq` 是发送端在这个通道上连续递增的序号。
    pub fn on_packet(&mut self, now: Time, send_time: Time, seq: u32, bytes: usize) {
        let delay = i64::try_from(now.micros()).unwrap_or(i64::MAX)
            - i64::try_from(send_time.micros()).unwrap_or(i64::MAX);
        if now.since(self.base_started) >= BASE_WINDOW_US {
            self.base = [None, self.base[0]];
            self.base_started = now;
        }
        self.base[0] = Some(self.base[0].map_or(delay, |b| b.min(delay)));
        self.interval_min = Some(self.interval_min.map_or(delay, |m| m.min(delay)));
        self.received = self.received.saturating_add(1);
        self.bytes = self
            .bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        // 序号会回绕：差值小于一半范围才算“更新”。
        let newer = self
            .highest_seq
            .is_none_or(|h| seq != h && seq.wrapping_sub(h) < u32::MAX / 2);
        if newer {
            self.highest_seq = Some(seq);
        }
    }

    /// 结算从上一次报告到现在的这段时间。
    pub fn report(&mut self, now: Time) -> DelaySample {
        let base = self.base.iter().flatten().min().copied();
        let queue_delay = match (self.interval_min, base) {
            (Some(min), Some(base)) => u32::try_from((min - base).max(0)).unwrap_or(u32::MAX),
            (Some(_), None) | (None, _) => 0,
        };
        let expected = match (self.highest_seq, self.reported_seq) {
            (Some(high), Some(prev)) => high.wrapping_sub(prev),
            (Some(_), None) => self.received,
            (None, _) => 0,
        };
        let lost = expected.saturating_sub(self.received);
        let loss = if expected == 0 {
            0
        } else {
            u16::try_from(u64::from(lost) * 1000 / u64::from(expected)).unwrap_or(1000)
        };
        let elapsed = now.since(self.interval_started).max(1);
        let rate =
            u32::try_from(self.bytes.saturating_mul(1_000_000) / elapsed).unwrap_or(u32::MAX);

        if self.highest_seq.is_some() {
            self.reported_seq = self.highest_seq;
        }
        self.interval_min = None;
        self.interval_started = now;
        self.received = 0;
        self.bytes = 0;
        DelaySample {
            queue_delay_us: queue_delay,
            loss_permille: loss,
            recv_bytes_per_s: rate,
        }
    }
}

/// 一套码率控制参数，对应一类网络。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Profile {
    /// 名字，显示在诊断信息里。
    pub name: &'static str,
    /// 排队延迟低于它：还有余量，可以升码率。
    pub low_delay_us: u32,
    /// 排队延迟高于它：在积压，降码率。
    pub high_delay_us: u32,
    /// 丢包率高于它、并且延迟不低时，也算拥塞。
    pub loss_permille: u16,
    /// 降码率时乘的系数（千分比）。
    pub decrease_permille: u32,
    /// 每次升码率的幅度（千分比）。
    pub increase_permille: u32,
    /// 每帧预算相对码率打的折扣（千分比），给重传和控制消息留余量。
    pub safety_permille: u32,
    /// 纠错冗余比例（千分比）。
    pub fec_permille: u16,
}

impl Profile {
    /// 有线、低抖动：反应快，不带纠错。
    pub const STABLE: Profile = Profile {
        name: "stable",
        low_delay_us: 3_000,
        high_delay_us: 12_000,
        loss_permille: 20,
        decrease_permille: 850,
        increase_permille: 80,
        safety_permille: 900,
        fec_permille: 0,
    };

    /// Wi-Fi：容忍抖动和随机丢包，常开少量纠错。
    pub const WIRELESS: Profile = Profile {
        name: "wireless",
        low_delay_us: 8_000,
        high_delay_us: 30_000,
        loss_permille: 80,
        decrease_permille: 850,
        increase_permille: 50,
        safety_permille: 800,
        fec_permille: 150,
    };

    /// 蜂窝：带宽变化快，降得快、升得慢。
    pub const CELLULAR: Profile = Profile {
        name: "cellular",
        low_delay_us: 15_000,
        high_delay_us: 50_000,
        loss_permille: 80,
        decrease_permille: 700,
        increase_permille: 30,
        safety_permille: 700,
        fec_permille: 200,
    };

    /// 跨洲：反馈来得晚，保守，纠错比例高（重传来不及）。
    pub const LONG_HAUL: Profile = Profile {
        name: "long-haul",
        low_delay_us: 10_000,
        high_delay_us: 40_000,
        loss_permille: 50,
        decrease_permille: 800,
        increase_permille: 30,
        safety_permille: 750,
        fec_permille: 250,
    };

    /// 根据往返延迟和链路类型选一套参数。
    #[must_use]
    pub const fn select(rtt_us: u64, wireless: bool, cellular: bool) -> Profile {
        if rtt_us >= 120_000 {
            Profile::LONG_HAUL
        } else if cellular {
            Profile::CELLULAR
        } else if wireless {
            Profile::WIRELESS
        } else {
            Profile::STABLE
        }
    }
}

/// 两次降码率之间至少隔这么久（微秒），避免对同一次拥塞反复降。
const DECREASE_HOLD_US: u64 = 150_000;

/// 发送端：根据反馈调整码率。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateController {
    profile: Profile,
    rate_bps: u64,
    min_bps: u64,
    max_bps: u64,
    last_decrease: Option<Time>,
}

impl RateController {
    /// 创建。`start_bps` 是起始码率（例如上次会话的稳定值），会被夹在上下限之间。
    #[must_use]
    pub fn new(profile: Profile, start_bps: u64, min_bps: u64, max_bps: u64) -> Self {
        let min_bps = min_bps.max(1);
        let max_bps = max_bps.max(min_bps);
        Self {
            profile,
            rate_bps: start_bps.clamp(min_bps, max_bps),
            min_bps,
            max_bps,
            last_decrease: None,
        }
    }

    /// 当前码率（比特每秒）。
    #[must_use]
    pub const fn rate_bps(&self) -> u64 {
        self.rate_bps
    }

    /// 使用的参数。
    #[must_use]
    pub const fn profile(&self) -> &Profile {
        &self.profile
    }

    /// 换一套参数，码率不变。
    pub const fn set_profile(&mut self, profile: Profile) {
        self.profile = profile;
    }

    /// 按当前码率和帧率算出每帧字节预算。
    #[must_use]
    pub fn frame_budget(&self, fps: u32) -> usize {
        let per_frame = self.rate_bps / 8 / u64::from(fps.max(1));
        let budget = per_frame.saturating_mul(u64::from(self.profile.safety_permille)) / 1000;
        usize::try_from(budget).unwrap_or(usize::MAX)
    }

    /// 处理一条反馈，返回新的码率。
    pub fn on_feedback(&mut self, now: Time, feedback: &Feedback) -> u64 {
        let p = self.profile;
        let recv_bps = u64::from(feedback.recv_bytes_per_s).saturating_mul(8);
        let congested = feedback.queue_delay_us > p.high_delay_us
            || (feedback.loss_permille > p.loss_permille
                && feedback.queue_delay_us > p.low_delay_us);
        if congested {
            let held = self
                .last_decrease
                .is_some_and(|t| now.since(t) < DECREASE_HOLD_US);
            if !held {
                // 以实际收到的速率为基准往下降：它反映了链路真正能通过多少。
                let base = if recv_bps > 0 {
                    self.rate_bps.min(recv_bps)
                } else {
                    self.rate_bps
                };
                self.rate_bps = (base.saturating_mul(u64::from(p.decrease_permille)) / 1000)
                    .clamp(self.min_bps, self.max_bps);
                self.last_decrease = Some(now);
            }
        } else if feedback.queue_delay_us < p.low_delay_us {
            // 只有真的在用这个码率时才往上升：屏幕静止时没有数据，不能凭空涨上去。
            let using = recv_bps.saturating_mul(10) >= self.rate_bps.saturating_mul(6);
            if using {
                let step = self.rate_bps.saturating_mul(u64::from(p.increase_permille)) / 1000;
                self.rate_bps = self
                    .rate_bps
                    .saturating_add(step.max(1))
                    .clamp(self.min_bps, self.max_bps);
            }
        }
        self.rate_bps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_roundtrip_and_errors() {
        let f = Feedback {
            floor: 77,
            delivered: 0b1011,
            queue_delay_us: 12_345,
            loss_permille: 1000,
            recv_bytes_per_s: u32::MAX,
        };
        let bytes = f.encode();
        assert_eq!(bytes.len(), Feedback::LEN);
        assert_eq!(Feedback::decode(&bytes).unwrap(), f);
        assert!(matches!(
            Feedback::decode(&bytes[..17]),
            Err(Error::Truncated { .. })
        ));
        let mut long = bytes.clone();
        long.push(0);
        assert_eq!(Feedback::decode(&long), Err(Error::TrailingBytes(1)));
        let mut bad = bytes;
        bad[12..14].copy_from_slice(&1001u16.to_be_bytes());
        assert!(matches!(
            Feedback::decode(&bad),
            Err(Error::Inconsistent(_))
        ));
    }

    #[test]
    fn delivery_log_tracks_a_sliding_window() {
        let mut log = DeliveryLog::new();
        assert_eq!(log.snapshot(), (0, 0));
        log.delivered(1);
        log.delivered(2);
        log.advance(4); // 第 3、4 帧作废
        assert_eq!(log.snapshot(), (4, 0b1100));
        log.delivered(5);
        assert_eq!(log.snapshot(), (5, 0b11001));
        // 迟到的旧帧（仍在窗口内）
        log.delivered(3);
        assert_eq!(log.snapshot(), (5, 0b11101));
        // 界线不会后退
        log.advance(2);
        assert_eq!(log.snapshot().0, 5);
        // 跳得很远：窗口清空
        log.advance(500);
        assert_eq!(log.snapshot(), (500, 0));
        // 比窗口还老的帧被忽略
        log.delivered(100);
        assert_eq!(log.snapshot(), (500, 0));
        assert_eq!(DeliveryLog::default(), DeliveryLog::new());
    }

    #[test]
    fn delay_tracker_measures_queueing_despite_clock_offset() {
        // 接收端时钟比发送端快 5 秒；链路固定延迟 20 ms。
        let offset = 5_000_000u64;
        let mut t = DelayTracker::new(Time(offset));
        let mut seq = 0;
        let mut send = |t: &mut DelayTracker, at: u64, extra: u64| {
            t.on_packet(Time(offset + at + 20_000 + extra), Time(at), seq, 1000);
            seq += 1;
        };
        for i in 0..10 {
            send(&mut t, i * 1000, 0);
        }
        let s = t.report(Time(offset + 100_000));
        assert_eq!(s.queue_delay_us, 0);
        assert_eq!(s.loss_permille, 0);
        assert_eq!(s.recv_bytes_per_s, 100_000);
        // 链路开始积压：每个包多等 15 ms 以上
        for i in 0..10 {
            send(&mut t, 100_000 + i * 1000, 15_000 + i * 100);
        }
        let s = t.report(Time(offset + 200_000));
        assert_eq!(s.queue_delay_us, 15_000, "取这段时间里的最小值，滤掉抖动");
        // 没有包的时段
        let s = t.report(Time(offset + 300_000));
        assert_eq!(s, DelaySample::default());
    }

    #[test]
    fn delay_tracker_counts_loss_from_sequence_gaps() {
        let mut t = DelayTracker::new(Time(0));
        for seq in [0u32, 1, 2, 3] {
            t.on_packet(Time(10), Time(0), seq, 100);
        }
        assert_eq!(t.report(Time(1000)).loss_permille, 0);
        // 序号 4..=13 里只收到 6 个；乱序和重复不影响
        for seq in [4u32, 6, 7, 9, 13, 12] {
            t.on_packet(Time(1010), Time(1000), seq, 100);
        }
        assert_eq!(t.report(Time(2000)).loss_permille, 400);
        // 只来了迟到的旧包：不算丢
        t.on_packet(Time(2010), Time(2000), 5, 100);
        assert_eq!(t.report(Time(3000)).loss_permille, 0);
        // 序号回绕
        let mut t = DelayTracker::new(Time(0));
        t.on_packet(Time(1), Time(0), u32::MAX, 1);
        t.report(Time(2));
        t.on_packet(Time(3), Time(2), 0, 1);
        t.on_packet(Time(3), Time(2), 2, 1);
        assert_eq!(t.report(Time(4)).loss_permille, 333);
    }

    #[test]
    fn base_delay_forgets_old_minimum_after_route_change() {
        let mut t = DelayTracker::new(Time(0));
        t.on_packet(Time(10_000), Time(0), 0, 1);
        t.report(Time(10_000));
        // 路由变了，固定延迟从 10 ms 变成 40 ms，持续 25 秒
        let mut last = DelaySample::default();
        for i in 1..=250u32 {
            let at = u64::from(i) * 100_000;
            t.on_packet(Time(at + 40_000), Time(at), i, 1);
            last = t.report(Time(at + 40_000));
        }
        assert_eq!(
            last.queue_delay_us, 0,
            "新的固定延迟被接受为基准，不再被当成排队"
        );
    }

    fn fb(queue_delay_us: u32, loss_permille: u16, recv_bytes_per_s: u32) -> Feedback {
        Feedback {
            queue_delay_us,
            loss_permille,
            recv_bytes_per_s,
            ..Feedback::default()
        }
    }

    #[test]
    fn rate_rises_when_clear_and_in_use() {
        let mut rc = RateController::new(Profile::STABLE, 1_000_000, 100_000, 2_000_000);
        // 排队很低，而且确实在用这个码率
        assert_eq!(rc.on_feedback(Time(0), &fb(0, 0, 125_000)), 1_080_000);
        // 没在用（屏幕静止）：不升
        assert_eq!(rc.on_feedback(Time(1), &fb(0, 0, 1_000)), 1_080_000);
        // 介于高低阈值之间：保持
        assert_eq!(rc.on_feedback(Time(2), &fb(5_000, 0, 135_000)), 1_080_000);
        // 一直空闲地升，封顶
        for i in 0..100 {
            rc.on_feedback(Time(10 + i), &fb(0, 0, u32::MAX));
        }
        assert_eq!(rc.rate_bps(), 2_000_000);
    }

    #[test]
    fn rate_drops_on_delay_but_not_on_loss_alone() {
        let mut rc = RateController::new(Profile::STABLE, 1_000_000, 100_000, 2_000_000);
        // 丢包很高但延迟很低（Wi-Fi 随机丢包）：不降
        let r = rc.on_feedback(Time(0), &fb(0, 300, 125_000));
        assert!(r >= 1_000_000);
        // 延迟超过上限：以实际接收速率为基准降
        let mut rc = RateController::new(Profile::STABLE, 1_000_000, 100_000, 2_000_000);
        assert_eq!(rc.on_feedback(Time(0), &fb(20_000, 0, 100_000)), 680_000);
        // 紧接着的拥塞反馈被合并，不重复降
        assert_eq!(
            rc.on_feedback(Time(100_000), &fb(20_000, 0, 80_000)),
            680_000
        );
        // 过了保持时间仍然拥塞：继续降
        assert_eq!(
            rc.on_feedback(Time(200_000), &fb(20_000, 0, 80_000)),
            544_000
        );
        // 丢包伴随中等延迟：算拥塞
        let mut rc = RateController::new(Profile::STABLE, 1_000_000, 100_000, 2_000_000);
        assert_eq!(rc.on_feedback(Time(0), &fb(5_000, 100, 125_000)), 850_000);
        // 没有接收速率可参考时按当前码率降；不会低于下限
        let mut rc = RateController::new(Profile::STABLE, 110_000, 100_000, 2_000_000);
        assert_eq!(rc.on_feedback(Time(0), &fb(20_000, 0, 0)), 100_000);
    }

    #[test]
    fn budgets_limits_and_profiles() {
        let rc = RateController::new(Profile::STABLE, 4_800_000, 0, 0);
        assert_eq!(rc.rate_bps(), 1, "上下限都是 0 时按 1 处理");
        let mut rc = RateController::new(Profile::STABLE, 4_800_000, 100_000, 10_000_000);
        // 4.8 Mbps、60 帧：每帧 10000 字节，打九折
        assert_eq!(rc.frame_budget(60), 9_000);
        assert_eq!(rc.frame_budget(0), 540_000, "帧率 0 按 1 处理");
        rc.set_profile(Profile::CELLULAR);
        assert_eq!(rc.profile().name, "cellular");
        assert_eq!(rc.frame_budget(60), 7_000);

        assert_eq!(Profile::select(5_000, false, false), Profile::STABLE);
        assert_eq!(Profile::select(5_000, true, false), Profile::WIRELESS);
        assert_eq!(Profile::select(50_000, true, true), Profile::CELLULAR);
        assert_eq!(Profile::select(150_000, false, false), Profile::LONG_HAUL);
        for p in [
            Profile::STABLE,
            Profile::WIRELESS,
            Profile::CELLULAR,
            Profile::LONG_HAUL,
        ] {
            assert!(p.low_delay_us < p.high_delay_us, "{}", p.name);
            assert!(p.decrease_permille < 1000 && p.safety_permille <= 1000);
        }
    }
}
