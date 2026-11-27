//! 硬件编码器“卡住了”的判定：**交了活之后**这么久没有任何动静才算。
//!
//! 一个信号只表示一件事。以前量的是“离编码器上一次发事件多久”，画面静止几秒（没人交帧，编码器自然没有事件）
//! 之后交的第一帧，还没来得及出结果就被判成“2 秒没有任何进展”，整场会话退回软件编码
//! （2026-11-27 两台 Intel 核显 Windows 的日志，三段会话三次都是这样，见 `docs/research/P-fps-bitrate.md` W1）。
//! 现在计时从“我们开始等它”算起：每次交帧（或开始排空）时重新计，编码器有动静时也重新计。
//!
//! 业内没有现成做法可抄（Chromium 的 Media Foundation 编码器出错即上报，没有“没事件”的看门狗，
//! 见 `docs/research/P-industry.md` 主题 2）；这里只保证判定本身讲得通。和系统无关，Linux 上也测。

use std::time::{Duration, Instant};

/// “交了活之后多久没动静”的计时。
#[derive(Debug, Clone, Copy)]
pub struct StallClock {
    limit: Duration,
    since: Instant,
}

impl StallClock {
    /// 创建：超过 `limit` 没动静算卡住，从 `now` 开始计。
    #[must_use]
    pub const fn new(limit: Duration, now: Instant) -> Self {
        Self { limit, since: now }
    }

    /// 开始等编码器（交帧、排空）：从现在重新计。之前闲了多久不算。
    pub const fn asked(&mut self, now: Instant) {
        self.since = now;
    }

    /// 编码器有动静（要输入、交出帧、排空完成）：重新计。
    pub const fn progress(&mut self, now: Instant) {
        self.since = now;
    }

    /// 等了超过时限还没有任何动静。
    #[must_use]
    pub fn stalled(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) > self.limit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归（2026-11-27 Windows 局域网日志）：画面静止 12 秒后交帧，不能马上判卡住；交了帧 2 秒没动静才算。
    #[test]
    fn idle_time_before_a_frame_is_handed_over_does_not_count() {
        let t0 = Instant::now();
        let limit = Duration::from_secs(2);
        let mut clock = StallClock::new(limit, t0);
        // 上一次事件之后静止了 12 秒：按老的量法已经“卡住”了
        let later = t0 + Duration::from_secs(12);
        assert!(clock.stalled(later), "没人交活时这个读数没有意义");
        // 交帧：重新计，1 毫秒后还没出结果不算卡住
        clock.asked(later);
        assert!(!clock.stalled(later + Duration::from_millis(1)));
        assert!(!clock.stalled(later + limit), "正好到时限不算");
        // 交了之后一直没动静：卡住
        assert!(clock.stalled(later + limit + Duration::from_millis(1)));
        // 中间有过动静：从那时重新计
        clock.progress(later + Duration::from_secs(1));
        assert!(!clock.stalled(later + Duration::from_millis(2_500)));
        assert!(clock.stalled(later + Duration::from_millis(3_001)));
        // 时钟回退（不该发生）不当成卡住
        assert!(!clock.stalled(t0));
    }
}
