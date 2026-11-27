//! 帧率调节（适配处理器里管帧率的部分）：画质崩之前先降帧率。
//!
//! 调研里定的降级顺序是“画质 → 帧率 → 分辨率”。画质由码率控制连续地调；但有两种情况光调画质不够，
//! 两个信号各自给一个帧率上限，实际帧率取小的那个，换档的说明写明是哪个信号（照 WebRTC 把 CPU 和画质分开：
//! `OveruseFrameDetector`、`QualityScaler`）：
//! - **CPU 跟不上**（[`crate::cpu`]）：5 秒平均的编码占用连续两次 ≥ 85% 降一档，< 42% 且等够了升一档。
//!   不按单帧判断：一个关键帧、一次新开编码器慢一点，不该让帧率掉档。
//! - **内容装不进预算**：编出来的帧连续 3 帧超过预算 2 倍降一档（反应要快：带宽突发），
//!   连续 1.5 秒都装得下升一档。帧率到底了还一直装不进，由发送端降分辨率（最后手段）。
//!
//! 不做 IO，时间由调用方传入。

use crate::cpu::{CpuOveruse, Verdict};
use crate::policy::ContentPolicy;
use crate::time::Time;

/// 连续多少帧装不进才降一档。
const DOWN_AFTER: u32 = 3;
/// 连续装得下多久（微秒）才升一档。按时间而不是帧数：远程桌面多数时候画面不动、每秒只有几帧，
/// 按帧数（原来 45 帧）要十几秒才升一档，降到 5 帧后几分钟都升不回来。
const UP_AFTER_US: u64 = 1_500_000;
/// 编出来的帧超过预算这么多倍算装不进。
const OVER_BUDGET_TIMES: usize = 2;

/// 帧率调节器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpsGovernor {
    max_fps: u32,
    /// 帧率档位（从高到低，来自内容策略）。
    steps: &'static [u32],
    /// “装得进预算”允许的帧率。
    fit_fps: u32,
    /// CPU 允许的帧率。
    cpu_fps: u32,
    cpu: CpuOveruse,
    strained: u32,
    /// 从什么时候起一直装得下。
    calm_since: Option<Time>,
    last_frame: Option<Time>,
    /// 正在编的这一帧什么时候开始的（[`Self::on_start`]）。
    frame_start: Option<Time>,
    /// 从什么时候起帧率已经在最低档、编出来的帧还是装不进预算（再往下只能降分辨率，见 [`Self::overflowing_for`]）。
    overflow_since: Option<Time>,
    /// 最近一次换档的说明（诊断日志用，取走后清空）。
    change: Option<String>,
}

impl FpsGovernor {
    /// 创建，从 `max_fps` 开始，档位按办公策略。
    #[must_use]
    pub fn new(max_fps: u32) -> Self {
        Self::with_policy(max_fps, &ContentPolicy::OFFICE)
    }

    /// 创建，从 `max_fps` 开始，档位按内容策略 `policy`。
    #[must_use]
    pub fn with_policy(max_fps: u32, policy: &ContentPolicy) -> Self {
        let max_fps = max_fps.max(1);
        Self {
            max_fps,
            steps: policy.fps_steps,
            fit_fps: max_fps,
            cpu_fps: max_fps,
            cpu: CpuOveruse::new(),
            strained: 0,
            calm_since: None,
            last_frame: None,
            frame_start: None,
            overflow_since: None,
            change: None,
        }
    }

    /// 当前帧率：两个信号允许的帧率取小的。
    #[must_use]
    pub fn fps(&self) -> u32 {
        self.fit_fps.min(self.cpu_fps)
    }

    /// 到时间编下一帧了吗（离上一帧至少一个帧间隔；留 10% 余量，免得定时器稍早几毫秒就错过一拍）。
    #[must_use]
    pub fn due(&self, now: Time) -> bool {
        let interval = 1_000_000 / u64::from(self.fps().max(1));
        self.last_frame
            .is_none_or(|t| now.since(t) >= interval.saturating_mul(9) / 10)
    }

    /// 开始编一帧（帧间隔从这里算）。
    pub const fn on_start(&mut self, now: Time) {
        self.frame_start = Some(now);
    }

    /// 编完一帧：`encode_us` 是编码用的时间（不知道时给 0），`hardware` 是不是硬件编码，
    /// `size` 和 `budget` 是这帧的字节数和预算。返回新的帧率。
    ///
    /// 帧间隔从这一帧**开始编**的时刻算（[`Self::on_start`]），不从编完算：从编完算的话，编一帧要 2–4 ms 时
    /// 下一拍离“编完”不到 90% 的帧间隔，每隔一拍就跳过一拍（实测 100 帧只编出 50 帧；1080p 软件编码
    /// 15 ms、30 帧时会掉到 15 帧）。没调用过 `on_start` 时按编完的时刻算。
    pub fn on_frame(
        &mut self,
        now: Time,
        encode_us: u64,
        hardware: bool,
        size: usize,
        budget: usize,
    ) -> u32 {
        let start = self.frame_start.take().unwrap_or(now);
        self.last_frame = Some(start);
        self.on_cpu(now, start, encode_us, hardware);
        self.on_fit(now, size, budget);
        self.fps()
    }

    /// CPU 信号：5 秒检查一次（见 [`crate::cpu`]）。
    fn on_cpu(&mut self, now: Time, start: Time, encode_us: u64, hardware: bool) {
        self.cpu.on_frame(start, encode_us, hardware);
        let from = self.fps();
        match self.cpu.check(now) {
            Some(Verdict::Overuse(usage)) => {
                self.cpu_fps = self.step_from(from, false);
                if self.fps() != from {
                    self.change = Some(format!(
                        "帧率 {from} → {}：CPU 跟不上（最近 5 秒编码占用 {usage}%）",
                        self.fps()
                    ));
                }
            }
            Some(Verdict::Underuse(usage)) if self.cpu_fps < self.max_fps => {
                self.cpu_fps = self.step_from(self.cpu_fps, true);
                if self.fps() != from {
                    self.change = Some(format!(
                        "帧率 {from} → {}：CPU 轻松了（最近 5 秒编码占用 {usage}%）",
                        self.fps()
                    ));
                }
            }
            Some(Verdict::Underuse(_)) | None => {}
        }
    }

    /// “装不进预算”信号：反应快（帧级），装得下一阵子再升。
    fn on_fit(&mut self, now: Time, size: usize, budget: usize) {
        let over = budget > 0 && size > budget.saturating_mul(OVER_BUDGET_TIMES);
        // 帧率到底了还装不进：记下从什么时候开始的（发送端据此决定是否降分辨率）
        if over && self.at_floor() {
            self.overflow_since.get_or_insert(now);
        } else if !over {
            self.overflow_since = None;
        }
        let from = self.fps();
        if over {
            self.calm_since = None;
            self.strained = self.strained.saturating_add(1);
            if self.strained >= DOWN_AFTER {
                self.strained = 0;
                self.fit_fps = self.step_from(from, false);
                if self.fps() != from {
                    self.change = Some(format!(
                        "帧率 {from} → {}：内容装不进预算（这帧 {size} 字节，预算 {budget}）",
                        self.fps()
                    ));
                }
            }
            return;
        }
        self.strained = 0;
        if size > budget {
            self.calm_since = None;
            return;
        }
        let since = *self.calm_since.get_or_insert(now);
        if now.since(since) >= UP_AFTER_US && self.fit_fps < self.max_fps {
            self.calm_since = Some(now);
            self.fit_fps = self.step_from(self.fit_fps, true);
            if self.fps() != from {
                self.change = Some(format!(
                    "帧率 {from} → {}：装得下预算 {} 秒了",
                    self.fps(),
                    UP_AFTER_US / 1_000_000
                ));
            }
        }
    }

    /// 帧率是不是因为内容装不进预算降了档（CPU 跟不上降的不算：那不是网络的原因）。
    #[must_use]
    pub const fn fit_limited(&self) -> bool {
        self.fit_fps < self.max_fps
    }

    /// 帧率是不是已经在最低档。
    #[must_use]
    pub fn at_floor(&self) -> bool {
        self.step_from(self.fps(), false) == self.fps()
    }

    /// 帧率已经在最低档、帧还一直装不进预算多久了（微秒）；不是这种情况为 0。
    #[must_use]
    pub fn overflowing_for(&self, now: Time) -> u64 {
        self.overflow_since.map_or(0, |t| now.since(t))
    }

    /// 已经按“装不进”采取了措施（降了分辨率）：重新计时。
    pub const fn reset_overflow(&mut self) {
        self.overflow_since = None;
    }

    /// 取走最近一次换档的说明（诊断日志用）。
    pub fn take_change(&mut self) -> Option<String> {
        self.change.take()
    }

    /// `fps` 相邻的档位，不超过上限。
    fn step_from(&self, fps: u32, up: bool) -> u32 {
        let allowed = self.steps.iter().copied().filter(|f| *f <= self.max_fps);
        let next = if up {
            allowed.filter(|f| *f > fps).min()
        } else {
            allowed.filter(|f| *f < fps).max()
        };
        next.unwrap_or(if up { self.max_fps } else { fps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overflow_at_the_floor_is_timed_for_the_resolution_decision() {
        let mut g = FpsGovernor::new(30);
        let mut t = Time::ZERO;
        // 一直装不进：一档档降到最低（5 帧），降到底之前不计时
        while !g.at_floor() {
            assert_eq!(g.overflowing_for(t), 0);
            t = t.plus(100_000);
            g.on_frame(t, 1_000, false, 10_000, 1_000);
        }
        assert_eq!(g.fps(), 5);
        t = t.plus(200_000);
        g.on_frame(t, 1_000, false, 10_000, 1_000);
        let since = t;
        t = t.plus(1_000_000);
        g.on_frame(t, 1_000, false, 10_000, 1_000);
        assert_eq!(g.overflowing_for(t), t.since(since));
        // 有一帧装得下：重新计时
        g.on_frame(t.plus(200_000), 1_000, false, 500, 1_000);
        assert_eq!(g.overflowing_for(t.plus(300_000)), 0);
        g.on_frame(t.plus(400_000), 1_000, false, 10_000, 1_000);
        assert!(g.overflowing_for(t.plus(500_000)) > 0);
        g.reset_overflow();
        assert_eq!(g.overflowing_for(t.plus(500_000)), 0);
    }

    #[test]
    fn frame_interval_counts_from_when_the_frame_started() {
        // 30 帧（33 ms 一拍），编一帧 15 ms：下一拍离开始 33 ms、离编完只有 18 ms，照样该编
        let mut g = FpsGovernor::new(30);
        g.on_start(Time::ZERO);
        g.on_frame(Time(15_000), 15_000, false, 100, 1000);
        assert!(g.due(Time(33_000)));
        assert!(!g.due(Time(20_000)));
    }

    #[test]
    fn slows_down_when_the_cpu_cannot_keep_up_and_recovers() {
        let mut g = FpsGovernor::new(60);
        assert_eq!(g.fps(), 60);
        assert!(g.due(Time::ZERO));
        let mut t = Time::ZERO;
        // 一个慢帧（关键帧、新开编码器）不降档：CPU 按 5 秒平均判断
        g.on_start(t);
        g.on_frame(t, 50_000, false, 100, 1000);
        assert_eq!(g.fps(), 60);
        // 每帧编 20 ms，60 帧的间隔只有 16.7 ms（占用 120%）：两次检查都过载 → 降到 30，说明是 CPU
        while g.fps() == 60 {
            t = t.plus(16_667);
            g.on_start(t);
            g.on_frame(t, 20_000, false, 100, 1000);
            assert!(t.micros() < 6_000_000, "5 秒左右就该降");
        }
        assert_eq!(g.fps(), 30);
        let why = g.take_change().unwrap();
        assert!(why.contains("帧率 60 → 30：CPU 跟不上"), "{why}");
        assert!(g.take_change().is_none(), "取走后清空");
        assert!(!g.due(t.plus(10_000)), "30 帧：33 ms 一帧");
        assert!(g.due(t.plus(31_000)));
        // 硬件编码是流水线的：同样的占用不算过载
        let mut hw = FpsGovernor::new(60);
        let mut h = Time::ZERO;
        for _ in 0..800 {
            h = h.plus(16_667);
            hw.on_frame(h, 20_000, true, 100, 1000);
        }
        assert_eq!(hw.fps(), 60);
        // 之后很轻松：等够升档等待（10 秒起）再一档档升回 60，不超过上限
        for _ in 0..3000 {
            t = t.plus(33_000);
            g.on_start(t);
            g.on_frame(t, 1_000, false, 100, 1000);
        }
        assert_eq!(g.fps(), 60);
        assert!(g.take_change().unwrap().contains("CPU 轻松了"));
    }

    #[test]
    fn recovers_by_time_even_when_frames_are_rare() {
        // 用户日志：降到 5 帧后画面大多静止、每秒只有几帧，按帧数要几分钟才升回来
        let mut g = FpsGovernor::new(60);
        let mut t = Time::ZERO;
        for _ in 0..30 {
            t = t.plus(100_000);
            g.on_frame(t, 0, false, 50_000, 10_000);
        }
        assert_eq!(g.fps(), 5);
        // 之后每秒只有 3 帧，都很轻松：每 1.5 秒升一档，10 秒内回到 60
        let start = t;
        while g.fps() < 60 {
            t = t.plus(333_000);
            g.on_frame(t, 1_000, false, 100, 10_000);
            assert!(t.since(start) < 12_000_000, "{} 帧", g.fps());
        }
    }

    #[test]
    fn slows_down_when_frames_do_not_fit_the_budget() {
        let mut g = FpsGovernor::new(30);
        let mut t = Time::ZERO;
        for _ in 0..30 {
            t = t.plus(100_000);
            g.on_frame(t, 0, false, 50_000, 10_000);
        }
        assert_eq!(g.fps(), 5, "一直装不进：降到最低一档");
        assert!(g.take_change().unwrap().contains("内容装不进预算"));
        // 偶尔超一点、又不算轻松：不升也不降
        for _ in 0..100 {
            t = t.plus(200_000);
            g.on_frame(t, 0, false, 15_000, 10_000);
        }
        assert_eq!(g.fps(), 5);
        // 上限不在档位里（例如 24）：从 24 开始，降到 20，升回 24
        let mut odd = FpsGovernor::new(24);
        for _ in 0..3 {
            odd.on_frame(Time::ZERO, 0, false, 99, 10);
        }
        assert_eq!(odd.fps(), 20);
        odd.on_frame(Time::ZERO, 0, false, 1, 10);
        odd.on_frame(Time::ZERO.plus(UP_AFTER_US), 0, false, 1, 10);
        assert_eq!(odd.fps(), 24);
        // 档位来自内容策略：例如高帧率的档位表，144 帧装不进时降到 120，而不是办公的档位
        let fast = ContentPolicy {
            fps_steps: &[144, 120, 60],
            ..ContentPolicy::OFFICE
        };
        let mut game = FpsGovernor::with_policy(144, &fast);
        for _ in 0..3 {
            game.on_frame(Time::ZERO, 0, false, 99, 10);
        }
        assert_eq!(game.fps(), 120);
        // 预算为 0（不知道）时不算超
        let mut zero = FpsGovernor::new(30);
        for _ in 0..10 {
            zero.on_frame(Time::ZERO, 0, false, 99, 0);
        }
        assert_eq!(zero.fps(), 30);
    }
}
