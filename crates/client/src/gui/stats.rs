//! 观看窗口的统计：帧率和显示用的格式。

use std::collections::VecDeque;
use std::time::Duration;

/// 帧率统计的窗口。
const WINDOW: Duration = Duration::from_secs(1);

/// 最近一秒收到了几帧。时间由调用方给出（从任意起点算的时长），便于测试。
#[derive(Debug, Default)]
pub struct FpsMeter {
    frames: VecDeque<Duration>,
}

impl FpsMeter {
    /// 记录 `n` 帧在 `now` 到达。
    pub fn record(&mut self, now: Duration, n: u64) {
        for _ in 0..n.min(1000) {
            self.frames.push_back(now);
        }
        self.trim(now);
    }

    fn trim(&mut self, now: Duration) {
        while self
            .frames
            .front()
            .is_some_and(|t| now.saturating_sub(*t) >= WINDOW)
        {
            let _ = self.frames.pop_front();
        }
    }

    /// 到 `now` 为止最近一秒的帧数。
    pub fn fps(&mut self, now: Duration) -> usize {
        self.trim(now);
        self.frames.len()
    }
}

/// 码率，例如 `850 kbps`、`12.3 Mbps`。
#[must_use]
pub fn bitrate(bits_per_second: u64) -> String {
    if bits_per_second >= 1_000_000 {
        format!(
            "{}.{} Mbps",
            bits_per_second / 1_000_000,
            bits_per_second % 1_000_000 / 100_000
        )
    } else {
        format!("{} kbps", bits_per_second / 1000)
    }
}

/// 平均码率：`bytes` 字节用了 `elapsed`。
#[must_use]
pub fn average_bps(bytes: u64, elapsed: Duration) -> u64 {
    let ms = u64::try_from(elapsed.as_millis())
        .unwrap_or(u64::MAX)
        .max(1);
    bytes.saturating_mul(8).saturating_mul(1000) / ms
}

/// 毫秒，保留一位小数。
#[must_use]
pub fn millis(d: Duration) -> String {
    let us = d.as_micros();
    format!("{}.{} ms", us / 1000, us % 1000 / 100)
}

/// 指纹的简写：前 8 位和后 8 位。
#[must_use]
pub fn short_fingerprint(hex: &str) -> String {
    match (hex.get(..8), hex.get(hex.len().saturating_sub(8)..)) {
        (Some(head), Some(tail)) if hex.len() > 16 => format!("{head}…{tail}"),
        (Some(_) | None, Some(_) | None) => hex.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_counts_the_last_second() {
        let mut m = FpsMeter::default();
        let ms = Duration::from_millis;
        assert_eq!(m.fps(ms(0)), 0);
        for i in 0..30 {
            m.record(ms(i * 33), 1);
        }
        assert_eq!(m.fps(ms(990)), 30);
        assert_eq!(m.fps(ms(1500)), 14);
        m.record(ms(1500), 5);
        assert_eq!(m.fps(ms(1500)), 19);
        assert_eq!(m.fps(ms(5000)), 0);
        m.record(ms(6000), u64::MAX);
        assert_eq!(m.fps(ms(6000)), 1000);
    }

    #[test]
    fn formatting() {
        assert_eq!(bitrate(850_000), "850 kbps");
        assert_eq!(bitrate(12_345_678), "12.3 Mbps");
        assert_eq!(average_bps(1_000_000, Duration::from_secs(2)), 4_000_000);
        assert_eq!(average_bps(10, Duration::ZERO), 80_000);
        assert_eq!(millis(Duration::from_micros(12_345)), "12.3 ms");
        assert_eq!(short_fingerprint(&"ab".repeat(32)), "abababab…abababab");
        assert_eq!(short_fingerprint("abc"), "abc");
        assert_eq!(short_fingerprint("ab汉"), "ab汉");
    }
}
