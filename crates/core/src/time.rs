//! 虚拟时间。核心库只认这个类型，真实程序用单调时钟换算，仿真直接推进。

/// 自某个起点以来的微秒数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Time(pub u64);

impl Time {
    /// 起点。
    pub const ZERO: Time = Time(0);

    /// 由毫秒构造。
    #[must_use]
    pub const fn from_millis(ms: u64) -> Self {
        Time(ms.saturating_mul(1000))
    }

    /// 微秒数。
    #[must_use]
    pub const fn micros(self) -> u64 {
        self.0
    }

    /// 加上若干微秒，溢出时饱和。
    #[must_use]
    pub const fn plus(self, us: u64) -> Self {
        Time(self.0.saturating_add(us))
    }

    /// 距离 `earlier` 过了多少微秒；`earlier` 更晚时返回 0。
    #[must_use]
    pub const fn since(self, earlier: Time) -> u64 {
        self.0.saturating_sub(earlier.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_saturates() {
        assert_eq!(Time::from_millis(2).micros(), 2000);
        assert_eq!(Time(5).plus(7), Time(12));
        assert_eq!(Time(u64::MAX).plus(1), Time(u64::MAX));
        assert_eq!(Time::from_millis(u64::MAX), Time(u64::MAX));
        assert_eq!(Time(10).since(Time(4)), 6);
        assert_eq!(Time(4).since(Time(10)), 0);
        assert_eq!(Time::default(), Time::ZERO);
    }
}
