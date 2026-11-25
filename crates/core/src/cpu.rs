//! CPU 过载检测：编码占用（编码时间 ÷ 帧间隔）一直太高就让帧率降一档，很低且等够了才升回去。
//!
//! 照 WebRTC `video/adaptation/overuse_frame_detector.cc`（`OveruseFrameDetector`）：
//! - 占用 = 一段时间里编码时间之和 ÷ 帧间隔之和（帧间隔最多按 1 秒算：画面不动时不把占用算低）；
//! - 第一次 100 ms 后检查，之后每 5 秒；连续两次 ≥ 85% 算过载；
//! - < 42% 而且离上次升档过了“升档等待”才算轻松：等待从 10 秒起，升上去之后很快又过载就加倍，最多 240 秒
//!   （WebRTC `kQuickRampUpDelayMs`、`kStandardRampUpDelayMs`、`kMaxRampUpDelayMs`）；
//! - 硬件编码器是流水线的（交进去和取出来之间编码器在并行干活），门限放宽到 200% / 150%
//!   （WebRTC `GetCpuOveruseOptions`）。
//!
//! 只管 CPU：“内容装不进预算”是另一个信号（[`crate::governor`]），两个信号各给一个帧率上限，
//! 帧率调节取小的那个，日志写明是哪个。不做 IO，时间由调用方传入。

use crate::time::Time;

/// 第一次检查在开始后多久（微秒）。
const FIRST_CHECK_US: u64 = 100_000;
/// 之后每隔多久检查一次（微秒）。
const CHECK_US: u64 = 5_000_000;
/// 连续几次超过门限才算过载。
const OVERUSE_CHECKS: u32 = 2;
/// 帧间隔最多按多长算（微秒）。
const MAX_INTERVAL_US: u64 = 1_000_000;
/// 升档等待：开始、标准、最长（微秒）。
const QUICK_RAMP_UP_US: u64 = 10_000_000;
const STANDARD_RAMP_UP_US: u64 = 40_000_000;
const MAX_RAMP_UP_US: u64 = 240_000_000;

/// 检查的结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// CPU 跟不上，降一档。占用（百分比）。
    Overuse(u64),
    /// 一直很轻松，可以升一档。占用（百分比）。
    Underuse(u64),
}

/// CPU 过载检测。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuOveruse {
    started: Option<Time>,
    next_check: Option<Time>,
    last_frame: Option<Time>,
    /// 这一段的编码时间之和、帧间隔之和（微秒）。
    encode_sum: u64,
    interval_sum: u64,
    /// 是不是硬件编码（门限放宽）。
    hardware: bool,
    above: u32,
    last_ramp_up: Option<Time>,
    ramp_up_us: u64,
}

impl Default for CpuOveruse {
    fn default() -> Self {
        Self::new()
    }
}

impl CpuOveruse {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            started: None,
            next_check: None,
            last_frame: None,
            encode_sum: 0,
            interval_sum: 0,
            hardware: false,
            above: 0,
            last_ramp_up: None,
            ramp_up_us: QUICK_RAMP_UP_US,
        }
    }

    /// 过载、轻松的门限（百分比）。
    const fn thresholds(&self) -> (u64, u64) {
        if self.hardware {
            (200, 150)
        } else {
            (85, 42)
        }
    }

    /// 编完一帧：开始的时刻 `start`，编了 `encode_us`，`hardware` 是不是硬件编码。
    pub fn on_frame(&mut self, start: Time, encode_us: u64, hardware: bool) {
        self.hardware = hardware;
        self.started.get_or_insert(start);
        if let Some(last) = self.last_frame {
            self.interval_sum = self
                .interval_sum
                .saturating_add(start.since(last).min(MAX_INTERVAL_US));
            self.encode_sum = self.encode_sum.saturating_add(encode_us);
        }
        self.last_frame = Some(start);
    }

    /// 定时检查（会话每次编帧、或定时调用都行）：到了检查时刻就给出结论。
    pub fn check(&mut self, now: Time) -> Option<Verdict> {
        let started = self.started?;
        let due = *self
            .next_check
            .get_or_insert_with(|| started.plus(FIRST_CHECK_US));
        if now < due || self.interval_sum == 0 {
            return None;
        }
        self.next_check = Some(now.plus(CHECK_US));
        let usage = self.encode_sum.saturating_mul(100) / self.interval_sum;
        self.encode_sum = 0;
        self.interval_sum = 0;
        let (high, low) = self.thresholds();
        if usage >= high {
            self.above = self.above.saturating_add(1);
            if self.above >= OVERUSE_CHECKS {
                self.above = 0;
                // 升上去不久又过载：下次等更久（WebRTC：升档之后标准等待内又过载就加倍）
                if self
                    .last_ramp_up
                    .is_some_and(|t| now.since(t) < STANDARD_RAMP_UP_US)
                {
                    self.ramp_up_us = self.ramp_up_us.saturating_mul(2).min(MAX_RAMP_UP_US);
                }
                return Some(Verdict::Overuse(usage));
            }
            return None;
        }
        self.above = 0;
        let waited = self
            .last_ramp_up
            .is_none_or(|t| now.since(t) >= self.ramp_up_us);
        if usage < low && waited {
            self.last_ramp_up = Some(now);
            return Some(Verdict::Underuse(usage));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每 `interval` 一帧、每帧编 `encode` 微秒，跑到 `until`，返回所有结论。
    fn run(
        c: &mut CpuOveruse,
        from: u64,
        until: u64,
        interval: u64,
        encode: u64,
        hardware: bool,
    ) -> Vec<(u64, Verdict)> {
        let mut out = Vec::new();
        let mut t = from;
        while t < until {
            c.on_frame(Time(t), encode, hardware);
            if let Some(v) = c.check(Time(t)) {
                out.push((t, v));
            }
            t += interval;
        }
        out
    }

    #[test]
    fn two_busy_checks_are_overuse_and_quiet_ones_ramp_up_after_a_delay() {
        let mut c = CpuOveruse::new();
        assert_eq!(c.check(Time(0)), None, "还没有帧");
        // 30 帧、每帧 30 ms（90%）：100 ms 后第一次检查、5 秒后第二次 → 过载
        let v = run(&mut c, 0, 6_000_000, 33_333, 30_000, false);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(matches!(v[0].1, Verdict::Overuse(u) if u >= 85));
        // 降下来以后很轻松（10%）：第一次轻松的检查就可以升（还没升过）
        let v = run(&mut c, 6_000_000, 11_200_000, 33_333, 3_333, false);
        let Some(&(up_at, Verdict::Underuse(u))) = v.last() else {
            panic!("{v:?}");
        };
        assert!(u < 42);
        // 升上去之后不久（40 秒内）又过载：升档等待从 10 秒加倍到 20 秒
        let v = run(&mut c, 11_200_000, 27_000_000, 33_333, 30_000, false);
        assert!(
            v.iter().any(|(_, x)| matches!(x, Verdict::Overuse(_))),
            "{v:?}"
        );
        // 又轻松了：离上次升档不到 20 秒不升，够了才升
        let ramp = run(&mut c, 27_000_000, 60_000_000, 33_333, 3_333, false);
        let first_up = ramp
            .iter()
            .find(|(_, x)| matches!(x, Verdict::Underuse(_)))
            .map_or(u64::MAX, |(t, _)| *t);
        assert!(
            first_up >= up_at + 20_000_000 && first_up < 60_000_000,
            "{up_at} {ramp:?}"
        );
    }

    #[test]
    fn hardware_encoders_get_more_headroom_and_idle_gaps_do_not_count() {
        // 硬件编码：占用 150%（流水线）不算过载，也不算轻松
        let mut c = CpuOveruse::new();
        let v = run(&mut c, 0, 12_000_000, 33_333, 50_000, true);
        assert!(v.is_empty(), "{v:?}");
        // 软件编码同样的数：过载
        let mut s = CpuOveruse::new();
        let v = run(&mut s, 0, 12_000_000, 33_333, 50_000, false);
        assert!(v.iter().any(|(_, x)| matches!(x, Verdict::Overuse(_))));
        // 画面停了 30 秒（没有帧）：间隔按 1 秒算，占用不会被算成 0 然后立刻升档
        let mut g = CpuOveruse::new();
        g.on_frame(Time(0), 30_000, false);
        g.on_frame(Time(30_000_000), 30_000, false);
        assert_eq!(g.check(Time(30_000_000)), Some(Verdict::Underuse(3)));
        assert_eq!(CpuOveruse::default(), CpuOveruse::new());
    }
}
