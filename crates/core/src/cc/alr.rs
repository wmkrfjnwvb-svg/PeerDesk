//! 应用受限（ALR，application-limited region）检测：发送端自己发得比估计少很多时，
//! 送达速率不代表链路能力，丢包估计不拿它当依据，探测控制器定期探一探上限。
//!
//! 照 WebRTC `modules/congestion_controller/goog_cc/alr_detector.cc`：按估计的 65% 攒预算（最多 500 ms 的量），
//! 发出去的字节从预算里扣；预算剩下 80% 以上 → 进入 ALR，掉到 50% 以下 → 退出。
//! 屏幕静止时没有包发，会话循环每 25 ms 调一次 [`Alr::on_tick`] 让预算照常累积。

use crate::time::Time;

/// 预算按估计的多少（千分比）累积。
const USAGE_PERMILLE: u64 = 650;
/// 预算剩下多少（千分比）以上进入 ALR。
const START_PERMILLE: i64 = 800;
/// 预算剩下多少（千分比）以下退出 ALR。
const STOP_PERMILLE: i64 = 500;
/// 预算最多攒多久的量（微秒）。
const WINDOW_US: u64 = 500_000;

/// 应用受限检测。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alr {
    target_bps: u64,
    /// 剩余预算（字节，可以为负：发多了）。
    budget: i64,
    last: Option<Time>,
    /// 什么时候进入的 ALR。
    started: Option<Time>,
    /// 什么时候退出的（探测控制器用：刚退出时大降也要探测）。
    ended: Option<Time>,
}

impl Alr {
    /// 按估计 `estimate_bps` 创建。
    #[must_use]
    pub const fn new(estimate_bps: u64) -> Self {
        Self {
            target_bps: estimate_bps * USAGE_PERMILLE / 1000,
            budget: 0,
            last: None,
            started: None,
            ended: None,
        }
    }

    /// 估计变了。
    pub fn set_estimate(&mut self, estimate_bps: u64) {
        self.target_bps = estimate_bps.saturating_mul(USAGE_PERMILLE) / 1000;
    }

    fn max_budget(&self) -> i64 {
        i64::try_from(self.target_bps.saturating_mul(WINDOW_US) / 8_000_000)
            .unwrap_or(i64::MAX)
            .max(1)
    }

    /// 发出了 `bytes` 字节。
    pub fn on_sent(&mut self, now: Time, bytes: u64) {
        let Some(last) = self.last.replace(now) else {
            return;
        };
        let max = self.max_budget();
        let earned = i64::try_from(self.target_bps.saturating_mul(now.since(last)) / 8_000_000)
            .unwrap_or(i64::MAX);
        let used = i64::try_from(bytes).unwrap_or(i64::MAX);
        self.budget = self
            .budget
            .saturating_sub(used)
            .max(-max)
            .saturating_add(earned)
            .min(max);
        let ratio = self.budget.saturating_mul(1000) / max;
        if ratio > START_PERMILLE && self.started.is_none() {
            self.started = Some(now);
        } else if ratio < STOP_PERMILLE && self.started.is_some() {
            self.started = None;
            self.ended = Some(now);
        }
    }

    /// 定时调用（没有包发时预算照常累积）。
    pub fn on_tick(&mut self, now: Time) {
        self.on_sent(now, 0);
    }

    /// 现在是否应用受限。
    #[must_use]
    pub const fn in_alr(&self) -> bool {
        self.started.is_some()
    }

    /// 什么时候进入的 ALR。
    #[must_use]
    pub const fn started(&self) -> Option<Time> {
        self.started
    }

    /// 最近一次退出 ALR 的时刻。
    #[must_use]
    pub const fn ended(&self) -> Option<Time> {
        self.ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_sender_enters_and_busy_sender_leaves() {
        let mut a = Alr::new(4_000_000);
        assert!(!a.in_alr());
        // 每 10 ms 发 1 KB（0.8 Mbps，远低于估计的 65%）
        let mut t = Time(0);
        for _ in 0..100 {
            t = t.plus(10_000);
            a.on_sent(t, 1000);
        }
        assert!(a.in_alr());
        assert!(a.started().is_some() && a.ended().is_none());
        // 发满（每 10 ms 6 KB ≈ 4.8 Mbps）：退出
        for _ in 0..100 {
            t = t.plus(10_000);
            a.on_sent(t, 6000);
        }
        assert!(!a.in_alr());
        assert!(a.ended().is_some());
        // 静止：只有定时调用，也会进入
        for _ in 0..40 {
            t = t.plus(25_000);
            a.on_tick(t);
        }
        assert!(a.in_alr());
        // 估计变小：同样的发送量不再算受限
        a.set_estimate(100_000);
        for _ in 0..100 {
            t = t.plus(10_000);
            a.on_sent(t, 1000);
        }
        assert!(!a.in_alr());
    }
}
