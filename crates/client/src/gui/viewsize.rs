//! 显示画面的区域有多大（像素），告诉被控端按这个大小编码（见 `peerdesk_core::scale::fit`）。
//!
//! 每次变化被控端都要出一个关键帧，所以要防抖：拖动窗口边框时不发，停下 [`SETTLE`] 之后才发；
//! 变化不到一成也不发。“原始大小”模式发 0×0（不限）。

use std::time::{Duration, Instant};

use num_traits::ToPrimitive as _;

/// 区域稳定这么久才发。
pub const SETTLE: Duration = Duration::from_millis(500);

/// 防抖器。
#[derive(Debug, Default)]
pub struct ViewSizer {
    sent: Option<(u16, u16)>,
    pending: Option<((u16, u16), Instant)>,
}

/// 两个尺寸差不到一成（两边都是）。0×0（不限）只和自己相同。
fn close(a: (u16, u16), b: (u16, u16)) -> bool {
    let near = |x: u16, y: u16| u32::from(x.abs_diff(y)) * 10 <= u32::from(x.max(y));
    (a == (0, 0)) == (b == (0, 0)) && near(a.0, b.0) && near(a.1, b.1)
}

/// 显示区域（点）换成像素：乘上屏幕缩放（Retina 上一个点是两个像素）。
#[must_use]
pub fn pixels(width: f32, height: f32, pixels_per_point: f32) -> (u16, u16) {
    let px = |v: f32| {
        (v * pixels_per_point)
            .round()
            .clamp(0.0, 65535.0)
            .to_u16()
            .unwrap_or(0)
    };
    (px(width), px(height))
}

impl ViewSizer {
    /// 有一个尺寸在等稳定（调用方要在 [`SETTLE`] 之后再调用一次 [`Self::update`]，例如预约一次重绘）。
    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// 这一帧显示区域是 `size`（像素；`(0, 0)` 表示不限）。返回要发给被控端的新尺寸。
    pub fn update(&mut self, now: Instant, size: (u16, u16)) -> Option<(u16, u16)> {
        if self.sent.is_some_and(|s| close(s, size)) {
            self.pending = None;
            return None;
        }
        match self.pending {
            Some((p, since)) if close(p, size) => {
                if now.duration_since(since) >= SETTLE {
                    self.sent = Some(size);
                    self.pending = None;
                    return Some(size);
                }
            }
            Some(_) | None => self.pending = Some((size, now)),
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_to_pixels() {
        assert_eq!(pixels(1440.0, 900.0, 2.0), (2880, 1800));
        assert_eq!(pixels(-1.0, 1e9, 1.0), (0, 65535));
    }

    #[test]
    fn settles_before_sending_and_ignores_small_changes() {
        let t0 = Instant::now();
        let mut v = ViewSizer::default();
        assert!(!v.is_pending());
        assert_eq!(v.update(t0, (1440, 900)), None, "刚出现，等它稳定");
        assert!(v.is_pending());
        assert_eq!(v.update(t0 + SETTLE / 2, (1440, 900)), None);
        assert_eq!(v.update(t0 + SETTLE, (1440, 900)), Some((1440, 900)));
        // 不到一成：不发
        assert_eq!(v.update(t0 + SETTLE * 3, (1400, 880)), None);
        // 拖动边框：一直在变，不发
        let mut t = t0 + SETTLE * 4;
        for w in [1000u16, 800, 600] {
            assert_eq!(v.update(t, (w, 500)), None);
            t += Duration::from_millis(100);
        }
        assert_eq!(v.update(t + SETTLE, (600, 500)), Some((600, 500)));
        // 原始大小：不限
        let t = t + SETTLE * 2;
        assert_eq!(v.update(t, (0, 0)), None);
        assert_eq!(v.update(t + SETTLE, (0, 0)), Some((0, 0)));
        assert_eq!(v.update(t + SETTLE * 2, (0, 0)), None, "已经发过");
    }
}
