//! 帧率调节：画质崩之前先降帧率。
//!
//! 调研里定的降级顺序是“画质 → 帧率 → 分辨率”。画质由码率控制连续地调；但有两种情况光调画质不够：
//! - **CPU 跟不上**：编一帧的时间接近甚至超过帧间隔（软件编码、满屏变化、慢机器），
//!   帧率照旧的话编码器永远在追，延迟越积越多；
//! - **内容装不进预算**：编出来的帧一再超过预算好几倍（满屏变化、低带宽），
//!   说明这个帧率下每帧的字节太少，画质已经到底了。
//!
//! 这两种情况下降帧率：每帧的时间和预算都变多，画面仍然清楚，只是更新慢一些。
//! 两种信号都平静一段时间后再一档档升回去。不做 IO，时间由调用方传入。

use crate::time::Time;

/// 帧率档位（从高到低）。
const STEPS: [u32; 7] = [60, 30, 20, 15, 10, 8, 5];
/// 连续多少帧出现问题才降一档。
const DOWN_AFTER: u32 = 3;
/// 连续多少帧都轻松才升一档。
const UP_AFTER: u32 = 45;
/// 编码时间超过帧间隔的这个比例（千分比）算 CPU 跟不上。
const CPU_BUSY_PERMILLE: u64 = 800;
/// 编码时间低于帧间隔的这个比例算轻松。
const CPU_EASY_PERMILLE: u64 = 400;
/// 编出来的帧超过预算这么多倍算装不进。
const OVER_BUDGET_TIMES: usize = 2;

/// 帧率调节器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FpsGovernor {
    max_fps: u32,
    fps: u32,
    strained: u32,
    easy: u32,
    last_frame: Option<Time>,
}

impl FpsGovernor {
    /// 创建，从 `max_fps` 开始。
    #[must_use]
    pub fn new(max_fps: u32) -> Self {
        let max_fps = max_fps.max(1);
        Self {
            max_fps,
            fps: max_fps,
            strained: 0,
            easy: 0,
            last_frame: None,
        }
    }

    /// 当前帧率。
    #[must_use]
    pub const fn fps(&self) -> u32 {
        self.fps
    }

    /// 到时间编下一帧了吗（离上一帧至少一个帧间隔；留 10% 余量，免得定时器稍早几毫秒就错过一拍）。
    #[must_use]
    pub fn due(&self, now: Time) -> bool {
        let interval = 1_000_000 / u64::from(self.fps);
        self.last_frame
            .is_none_or(|t| now.since(t) >= interval.saturating_mul(9) / 10)
    }

    /// 编完一帧：`encode_us` 是编码用的时间（不知道时给 0），`size` 和 `budget` 是这帧的字节数和预算。
    /// 返回新的帧率。
    pub fn on_frame(&mut self, now: Time, encode_us: u64, size: usize, budget: usize) -> u32 {
        self.last_frame = Some(now);
        let interval = 1_000_000 / u64::from(self.fps);
        let cpu_busy = encode_us.saturating_mul(1000) > interval.saturating_mul(CPU_BUSY_PERMILLE);
        let over = budget > 0 && size > budget.saturating_mul(OVER_BUDGET_TIMES);
        let easy = encode_us.saturating_mul(1000) < interval.saturating_mul(CPU_EASY_PERMILLE)
            && size <= budget;
        if cpu_busy || over {
            self.easy = 0;
            self.strained = self.strained.saturating_add(1);
            if self.strained >= DOWN_AFTER {
                self.strained = 0;
                self.fps = self.step(false);
            }
        } else {
            self.strained = 0;
            self.easy = if easy { self.easy.saturating_add(1) } else { 0 };
            if self.easy >= UP_AFTER {
                self.easy = 0;
                self.fps = self.step(true);
            }
        }
        self.fps
    }

    /// 相邻的档位，不超过上限。
    fn step(&self, up: bool) -> u32 {
        let allowed = STEPS.iter().copied().filter(|f| *f <= self.max_fps);
        let next = if up {
            allowed.filter(|f| *f > self.fps).min()
        } else {
            allowed.filter(|f| *f < self.fps).max()
        };
        next.unwrap_or(if up { self.max_fps } else { self.fps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slows_down_when_the_cpu_cannot_keep_up_and_recovers() {
        let mut g = FpsGovernor::new(60);
        assert_eq!(g.fps(), 60);
        assert!(g.due(Time::ZERO));
        let mut t = Time::ZERO;
        // 编一帧 20 ms，60 帧的间隔只有 16.7 ms：降到 30
        for _ in 0..3 {
            t = t.plus(20_000);
            g.on_frame(t, 20_000, 100, 1000);
        }
        assert_eq!(g.fps(), 30);
        assert!(!g.due(t.plus(10_000)), "30 帧：33 ms 一帧");
        assert!(g.due(t.plus(31_000)));
        // 一直很轻松：一档档升回 60，不超过上限
        for _ in 0..200 {
            t = t.plus(33_000);
            g.on_frame(t, 1_000, 100, 1000);
        }
        assert_eq!(g.fps(), 60);
    }

    #[test]
    fn slows_down_when_frames_do_not_fit_the_budget() {
        let mut g = FpsGovernor::new(30);
        let mut t = Time::ZERO;
        for _ in 0..30 {
            t = t.plus(100_000);
            g.on_frame(t, 0, 50_000, 10_000);
        }
        assert_eq!(g.fps(), 5, "一直装不进：降到最低一档");
        // 偶尔超一点、又不算轻松：不升也不降
        for _ in 0..100 {
            t = t.plus(200_000);
            g.on_frame(t, 0, 15_000, 10_000);
        }
        assert_eq!(g.fps(), 5);
        // 上限不在档位里（例如 24）：从 24 开始，降到 20，升回 24
        let mut odd = FpsGovernor::new(24);
        for _ in 0..3 {
            odd.on_frame(Time::ZERO, 0, 99, 10);
        }
        assert_eq!(odd.fps(), 20);
        for _ in 0..45 {
            odd.on_frame(Time::ZERO, 0, 1, 10);
        }
        assert_eq!(odd.fps(), 24);
        // 预算为 0（不知道）时不算超
        let mut zero = FpsGovernor::new(30);
        for _ in 0..10 {
            zero.on_frame(Time::ZERO, 0, 99, 0);
        }
        assert_eq!(zero.fps(), 30);
    }
}
