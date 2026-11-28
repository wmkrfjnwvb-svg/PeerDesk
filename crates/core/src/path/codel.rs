//! CoDel（RFC 8289 第 5 节的伪代码），按“包一个一个出队”的方式写成状态机。
//!
//! 和 RFC 的差别只有一处，原因是仿真在发送时就要算出送达时间（见 `path` 模块文档）：
//! RFC 的 `dodequeue` 在包出队时还要看队列里剩多少字节（`maxpacket` 保护：队列里只剩不到一个 MTU 时不丢，
//! 因为丢了也降不了队列）。包 k 出队时，排在它后面的包还没发出来，仿真不知道，所以这里没有这条保护。
//! 影响：排队退到只剩一个包、同时队首的排队时长还在目标以上时，可能多丢一个。这种情况下排队时长已经很短，
//! 持续过载（要控制的情形）下队列里不会只剩一个包。
//!
//! 包 k 的出队时刻 = 链路空出来的时刻，在包 k 到达时就能算出，所以状态机只用到比它早的包。
//! 被丢的包不占发送时间，下一个包仍从同一时刻出队，和 RFC 里“丢了队首接着取下一个”一致。

/// 上一个包出队时的处理，影响下一个包的判断（RFC 伪代码里 `while` 循环和首次丢包后的“接着出队”）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// 普通的下一个包。
    Fresh,
    /// 上一个包是进入丢包状态时丢的第一个。下一个包只更新“高于目标”的计时，不参与丢包判断。
    AfterFirstDrop,
    /// 上一个包是丢包状态里按时刻丢的。下一个包要先判断是否继续丢（RFC 的 while 循环）。
    AfterLoopDrop,
}

/// CoDel 状态。
#[derive(Debug, Clone)]
pub struct Codel {
    target_us: u64,
    interval_us: u64,
    /// 排队时长第一次持续高于目标的截止时刻（RFC 的 `first_above_time`，`None` 即 0）。
    first_above: Option<u64>,
    /// 下一次丢包的时刻。
    drop_next: u64,
    /// 这一轮丢包状态里已经丢了几个包（RFC 的 `count`）。
    count: u64,
    /// 上一轮结束时的 `count`。
    last_count: u64,
    dropping: bool,
    next: Next,
}

impl Codel {
    /// RFC 8289 的默认值：目标 5 ms，窗口 100 ms。
    pub const DEFAULT_TARGET_US: u64 = 5_000;
    /// 窗口 100 ms。
    pub const DEFAULT_INTERVAL_US: u64 = 100_000;

    /// 新状态。`target_us`、`interval_us` 至少取 1。
    #[must_use]
    pub fn new(target_us: u64, interval_us: u64) -> Self {
        Self {
            target_us: target_us.max(1),
            interval_us: interval_us.max(1),
            first_above: None,
            drop_next: 0,
            count: 0,
            last_count: 0,
            dropping: false,
            next: Next::Fresh,
        }
    }

    /// 控制律：`t + interval / sqrt(count)`，纯整数（`isqrt(count * 10^8)` = `10^4 * sqrt(count)`）。
    #[must_use]
    pub fn control_law(&self, t: u64, count: u64) -> u64 {
        let root = count.max(1).saturating_mul(100_000_000).isqrt().max(1);
        t.saturating_add(self.interval_us.saturating_mul(10_000) / root)
    }

    /// 是否在丢包状态。
    #[must_use]
    pub const fn dropping(&self) -> bool {
        self.dropping
    }

    /// 这一轮丢包状态里已经丢的个数。
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// 下一次丢包的时刻（微秒）。
    #[must_use]
    pub const fn drop_next(&self) -> u64 {
        self.drop_next
    }

    /// RFC 的 `dodequeue` 里对 `ok_to_drop` 的判断（排队时长连续一个窗口高于目标）。
    fn ok_to_drop(&mut self, now: u64, sojourn: u64) -> bool {
        if sojourn < self.target_us {
            self.first_above = None;
            return false;
        }
        match self.first_above {
            None => {
                self.first_above = Some(now.saturating_add(self.interval_us));
                false
            }
            Some(t) => now >= t,
        }
    }

    /// 队首的包在 `now` 出队，它已经排了 `sojourn` 微秒。返回 true 表示丢掉它。
    /// 调用的顺序必须和包出队的先后一致，`now` 不能倒退。
    pub fn dequeue(&mut self, now: u64, sojourn: u64) -> bool {
        let ok = self.ok_to_drop(now, sojourn);
        let next = self.next;
        self.next = Next::Fresh;
        if next == Next::AfterFirstDrop {
            // RFC：首次丢包后接着取的那个包直接发出去，它的判断留给下一个包
            return false;
        }
        if self.dropping {
            if next == Next::AfterLoopDrop && ok {
                self.drop_next = self.control_law(self.drop_next, self.count);
            }
            if !ok {
                self.dropping = false;
            }
            if self.dropping && now >= self.drop_next {
                self.count = self.count.saturating_add(1);
                self.next = Next::AfterLoopDrop;
                return true;
            }
            return false;
        }
        if ok {
            // 进入丢包状态：丢这个包。上一轮刚结束不久就回来的话，从上一轮的丢包速率附近开始。
            self.dropping = true;
            let delta = self.count.saturating_sub(self.last_count);
            if delta > 1 && now.saturating_sub(self.drop_next) < self.interval_us.saturating_mul(16)
            {
                self.count = delta;
            } else {
                self.count = 1;
            }
            self.drop_next = self.control_law(now, self.count);
            self.last_count = self.count;
            self.next = Next::AfterFirstDrop;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_law_is_interval_over_sqrt_count() {
        let c = Codel::new(5_000, 100_000);
        assert_eq!(c.control_law(1_000, 1), 101_000);
        assert_eq!(c.control_law(0, 4), 50_000);
        assert_eq!(c.control_law(0, 100), 10_000);
        for count in [1u64, 2, 3, 5, 10, 50, 1000, 40_000] {
            let want = 100_000.0 / (count as f64).sqrt();
            let got = c.control_law(0, count) as f64;
            assert!(
                (got - want).abs() <= want * 2e-3 + 1.0,
                "count={count} got={got} want={want}"
            );
        }
        // count 为 0 按 1 算
        assert_eq!(c.control_law(5, 0), 100_005);
        assert_eq!(c.control_law(u64::MAX, 1), u64::MAX);
    }

    /// 排队时长一直高于目标：第一次丢包在连续高于目标一个窗口之后，之后每次间隔 interval/sqrt(count)。
    #[test]
    fn persistent_delay_drops_first_after_one_interval_then_shortens() {
        let mut c = Codel::new(5_000, 100_000);
        let mut drops: Vec<u64> = Vec::new();
        // 每 1 ms 出队一个包，排队时长恒为 20 ms（不理会丢包，持续过载）
        for t in 0..2_000u64 {
            let now = t * 1_000;
            if c.dequeue(now, 20_000) {
                drops.push(now);
            }
        }
        // 第 0 ms 记下“高于目标”，窗口到 100 ms 才开始丢
        assert_eq!(drops[0], 100_000);
        assert!(c.dropping());
        // 相邻两次丢包的间隔 = interval / sqrt(count)（按 1 ms 的出队粒度向上取整）
        for (i, w) in drops.windows(2).enumerate() {
            let count = (i + 1) as f64; // 第 i+2 个丢包之前 count 已经是 i+1
            let want = 100_000.0 / count.sqrt();
            let got = (w[1] - w[0]) as f64;
            assert!(
                (got - want).abs() <= 1_000.0,
                "第 {} 次间隔 {got} 应为约 {want}",
                i + 2
            );
        }
        assert!(drops.len() > 20);
    }

    /// 排队时长降到目标以下就退出丢包状态；很快又超过时，从上一轮的丢包速率附近开始，不从头来。
    #[test]
    fn leaves_dropping_when_delay_recovers_and_resumes_near_the_old_rate() {
        let mut c = Codel::new(5_000, 100_000);
        let mut now = 0;
        while c.count() < 8 {
            let _ = c.dequeue(now, 20_000);
            now += 1_000;
        }
        let count_before = c.count();
        assert!(c.dropping());
        assert!(!c.dequeue(now, 1_000), "排队时长回到目标以下，不丢");
        assert!(!c.dropping());
        // 马上又高了：先要再高满一个窗口
        now += 1_000;
        assert!(!c.dequeue(now, 20_000));
        let mut resumed = None;
        for _ in 0..300 {
            now += 1_000;
            if c.dequeue(now, 20_000) {
                resumed = Some(c.count());
                break;
            }
        }
        // 上一轮 count_before 个，count_before - last_count(1) > 1 且离上一轮不超过 16 个窗口：从 count_before - 1 开始
        assert_eq!(resumed, Some(count_before - 1));
        // 隔了很久再回来则从 1 开始
        let mut c2 = c.clone();
        let _ = c2.dequeue(now + 5_000_000, 1_000); // 首次丢包后的那个包不参与判断
        let _ = c2.dequeue(now + 5_001_000, 1_000);
        assert!(!c2.dropping());
        let t0 = now + 10_000_000;
        let _ = c2.dequeue(t0, 20_000);
        assert!(c2.dequeue(t0 + 100_000, 20_000));
        assert_eq!(c2.count(), 1);
    }

    /// 排队时长低于目标时从不丢包。
    #[test]
    fn never_drops_below_target() {
        let mut c = Codel::new(5_000, 100_000);
        assert!((0..10_000u64).all(|t| !c.dequeue(t * 1_000, 4_999)));
        assert!(!c.dropping());
        assert_eq!(c.drop_next(), 0);
    }

    /// 首次丢包后接着取的那个包直接发出（RFC：它的判断留给下一个包）；丢包状态里，同一时刻要连丢的包连着丢。
    #[test]
    fn first_drop_is_followed_by_a_free_pass_and_loop_drops_chain() {
        let mut c = Codel::new(5_000, 100_000);
        assert!(!c.dequeue(0, 20_000));
        assert!(c.dequeue(100_000, 20_000), "窗口满了，丢第一个");
        assert!(!c.dequeue(100_000, 20_000), "接着取的包直接发出");
        // 下一次丢包在 100_000 + 100_000；同一时刻到了又有好几个包在队里：RFC 的 while 循环连丢，
        // 每丢一个 count 加 1，drop_next 按控制律往后推，推到还没到就停。
        let jump = 100_000 + 100_000;
        assert!(c.dequeue(jump, 20_000)); // count=2，drop_next 之后推到 +70_710.7（取整 70_711）
        assert_eq!(c.count(), 2);
        assert!(!c.dequeue(jump, 20_000), "更新后的 drop_next 还没到：发出");
        assert_eq!(c.drop_next(), jump + 70_711);
        // 循环里下一个包排队时长已经回到目标以下：退出丢包状态，不更新 drop_next
        let before = c.drop_next();
        let mut d = c.clone();
        let _ = d.dequeue(before, 20_000); // 丢（count=3）
        assert!(!d.dequeue(before, 1_000));
        assert!(!d.dropping());
        assert_eq!(d.drop_next(), before, "没有继续丢，drop_next 不动");
    }
}
