//! 鼠标相关的换算：归一化坐标 → 像素，Windows 虚拟桌面坐标，滚轮累积，连续点击计数，按键编号。
//!
//! 约定（与 [`peerdesk_core::input::InputEvent`] 一致）：
//! * 绝对坐标在被采集的显示器上归一化到 `0..=65535`：0 是最左（上）一个像素，65535 是最右（下）一个像素；
//! * 鼠标按键：0 左键，1 右键，2 中键，3 后退（侧键 1），4 前进（侧键 2），更大的编号不注入；
//! * 滚轮是高精度增量，[`WHEEL_NOTCH`]（120）= 滚轮的一格，与 Windows 的 `WHEEL_DELTA` 相同。
//!   `dy` 为正是向上滚（远离用户），`dx` 为正是向右滚。

use std::time::{Duration, Instant};

/// 滚轮一格对应的增量（Windows 的 `WHEEL_DELTA`）。
pub const WHEEL_NOTCH: i32 = 120;
/// 归一化坐标的最大值。
pub const NORM_MAX: u16 = u16::MAX;

/// 归一化坐标 → 显示器上的像素位置（含显示器的偏移）。`size` 是显示器在这个方向上的像素数。
#[must_use]
pub fn to_pixel(norm: u16, origin: i32, size: u32) -> i32 {
    let span = u64::from(size.saturating_sub(1));
    // 四舍五入：(norm * span + 65535/2) / 65535
    let offset = (u64::from(norm) * span + u64::from(NORM_MAX / 2)) / u64::from(NORM_MAX);
    origin.saturating_add(i32::try_from(offset).unwrap_or(i32::MAX))
}

/// 归一化坐标 → 显示器上的位置（浮点），macOS 用：`origin`、`size` 的单位是点，
/// `pixel` 是一个物理像素有多少点（Retina 屏为 0.5），最大值落在最右（下）一个像素上而不是显示器外面。
#[must_use]
pub fn to_points(norm: u16, origin: f64, size: f64, pixel: f64) -> f64 {
    let span = (size - pixel).max(0.0);
    origin + f64::from(norm) / f64::from(NORM_MAX) * span
}

/// 浮点坐标四舍五入成整数，超出 i32 范围的钳到边上，NaN 当作 0。
#[must_use]
#[allow(clippy::cast_possible_truncation)]
pub fn round_to_i32(v: f64) -> i32 {
    if v.is_nan() {
        return 0;
    }
    // 先钳到范围内，转换不会截断
    v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

/// 像素位置 → `SendInput` 带 `MOUSEEVENTF_VIRTUALDESK` 时的绝对坐标（0..=65535，覆盖整个虚拟桌面）。
///
/// Windows 把绝对坐标 `n` 换算成像素 `origin + n * size / 65536`（向下取整），
/// 这里取能落在 `px` 上的最小的 `n`，换算回去正好是 `px`，不会差一个像素。
/// `origin`、`size` 是虚拟桌面在这个方向上的起点和像素数。
#[must_use]
pub fn to_virtual_desk(px: i32, origin: i32, size: i32) -> i32 {
    let size = i64::from(size.max(1));
    let offset = (i64::from(px) - i64::from(origin)).clamp(0, size - 1);
    // 向上取整：ceil(offset * 65536 / size)
    let n = (offset * 65_536 + size - 1) / size;
    i32::try_from(n.clamp(0, i64::from(NORM_MAX))).unwrap_or(0)
}

/// Windows 收到绝对坐标后落在哪个像素上（[`to_virtual_desk`] 的逆运算，测试和自检用）。
#[must_use]
pub fn from_virtual_desk(n: i32, origin: i32, size: i32) -> i32 {
    let offset = i64::from(n) * i64::from(size.max(1)) / 65_536;
    origin.saturating_add(i32::try_from(offset).unwrap_or(i32::MAX))
}

/// 把高精度的滚动增量累积起来，攒够一步再发出去。换方向时丢弃反方向的余量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WheelAccumulator {
    step: i32,
    acc: i32,
}

impl WheelAccumulator {
    /// `step`：多少增量算一步（例如 X11 只能按格滚动，一步就是 [`WHEEL_NOTCH`]）。
    #[must_use]
    pub const fn new(step: i32) -> Self {
        Self {
            step: if step > 0 { step } else { 1 },
            acc: 0,
        }
    }

    /// 加入一个增量，返回现在可以发出的步数（带符号）。
    pub fn push(&mut self, delta: i16) -> i32 {
        let delta = i32::from(delta);
        self.acc = if self.acc.signum() * delta.signum() < 0 {
            delta
        } else {
            self.acc.saturating_add(delta)
        };
        let steps = self.acc / self.step;
        self.acc -= steps * self.step;
        steps
    }

    /// 丢弃还不够一步的余量。
    pub fn reset(&mut self) {
        self.acc = 0;
    }
}

/// 连续点击的判定间隔（系统默认的双击间隔一般是 0.5 秒）。
pub const MULTI_CLICK_TIME: Duration = Duration::from_millis(500);
/// 连续点击的判定距离（两次按下的位置在这个范围内）。
pub const MULTI_CLICK_DISTANCE: f64 = 4.0;

/// 连续点击计数（macOS 的鼠标事件要带上这是第几次点击，否则双击、三击不起作用）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ClickTracker {
    last: Option<(u8, Instant, (f64, f64), i64)>,
}

impl ClickTracker {
    /// 按下：返回这是第几次连续点击（同一个键、间隔和距离都很近时累加）。
    pub fn press(&mut self, button: u8, pos: (f64, f64), now: Instant) -> i64 {
        let count = match self.last {
            Some((b, at, (x, y), count))
                if b == button
                    && now.saturating_duration_since(at) <= MULTI_CLICK_TIME
                    && (x - pos.0).abs() <= MULTI_CLICK_DISTANCE
                    && (y - pos.1).abs() <= MULTI_CLICK_DISTANCE =>
            {
                count.saturating_add(1)
            }
            Some(_) | None => 1,
        };
        self.last = Some((button, now, pos, count));
        count
    }

    /// 抬起：与对应的按下是同一次点击计数。
    #[must_use]
    pub fn release(&self, button: u8) -> i64 {
        match self.last {
            Some((b, _, _, count)) if b == button => count,
            Some(_) | None => 1,
        }
    }
}

/// 鼠标按键 → X11 的按钮编号（1 左、2 中、3 右、8 后退、9 前进）。4 到 7 是滚轮，不在这里。
#[must_use]
pub const fn x11_button(button: u8) -> Option<u8> {
    match button {
        0 => Some(1),
        1 => Some(3),
        2 => Some(2),
        3 => Some(8),
        4 => Some(9),
        _ => None,
    }
}

/// 滚动步数 → X11 的滚轮按钮和点击次数：`(纵向, 横向)` 各自的 `(按钮, 次数)`。
#[must_use]
pub fn x11_wheel_buttons(dy_steps: i32, dx_steps: i32) -> [(u8, u32); 2] {
    let vertical = if dy_steps > 0 { 4 } else { 5 };
    let horizontal = if dx_steps > 0 { 7 } else { 6 };
    [
        (vertical, dy_steps.unsigned_abs()),
        (horizontal, dx_steps.unsigned_abs()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_to_pixels() {
        assert_eq!(to_pixel(0, 0, 1920), 0);
        assert_eq!(to_pixel(65535, 0, 1920), 1919);
        assert_eq!(to_pixel(32768, 0, 1920), 960);
        assert_eq!(to_pixel(65535, -1280, 1280), -1);
        assert_eq!(to_pixel(65535, 100, 0), 100, "空的显示器");
        // 每个像素都能精确命中
        let w = 1366u32;
        for px in 0..w {
            let norm = u16::try_from(u64::from(px) * 65535 / u64::from(w - 1)).unwrap();
            assert_eq!(to_pixel(norm, 0, w), i32::try_from(px).unwrap());
        }
    }

    #[test]
    fn normalized_to_points() {
        assert!((to_points(0, -1512.0, 1512.0, 0.5) - -1512.0).abs() < 1e-9);
        assert!((to_points(65535, -1512.0, 1512.0, 0.5) - -0.5).abs() < 1e-9);
        assert!((to_points(65535, 0.0, 0.2, 0.5) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn rounding() {
        assert_eq!(round_to_i32(1.5), 2);
        assert_eq!(round_to_i32(-1.5), -2);
        assert_eq!(round_to_i32(-0.4), 0);
        assert_eq!(round_to_i32(1e12), i32::MAX);
        assert_eq!(round_to_i32(-1e12), i32::MIN);
        assert_eq!(round_to_i32(f64::NAN), 0);
    }

    #[test]
    fn virtual_desk_round_trips_exactly() {
        for (origin, size) in [(0, 1024), (-1920, 3840), (0, 1), (-3, 7), (0, 65_536)] {
            for px in origin..origin + size {
                let n = to_virtual_desk(px, origin, size);
                assert!((0..=65535).contains(&n));
                assert_eq!(
                    from_virtual_desk(n, origin, size),
                    px,
                    "{origin} {size} {px}"
                );
            }
        }
        // 超出范围的位置钳到边上
        assert_eq!(to_virtual_desk(-5000, 0, 1920), 0);
        assert_eq!(to_virtual_desk(5000, 0, 1920), 65_502);
        assert_eq!(to_virtual_desk(10, 0, 0), 0);
    }

    #[test]
    fn wheel_accumulates_and_resets_on_reversal() {
        let mut w = WheelAccumulator::new(WHEEL_NOTCH);
        assert_eq!(w.push(60), 0);
        assert_eq!(w.push(60), 1);
        assert_eq!(w.push(360), 3);
        assert_eq!(w.push(100), 0);
        assert_eq!(w.push(-30), 0, "换方向时丢弃余量");
        assert_eq!(w.push(-90), -1);
        w.push(-100);
        w.reset();
        assert_eq!(w.push(-20), 0);
        let mut one = WheelAccumulator::new(0);
        assert_eq!(one.push(-7), -7);
    }

    #[test]
    fn click_counts() {
        let t = Instant::now();
        let mut c = ClickTracker::default();
        assert_eq!(c.release(0), 1);
        assert_eq!(c.press(0, (100.0, 100.0), t), 1);
        assert_eq!(c.release(0), 1);
        assert_eq!(c.press(0, (102.0, 99.0), t + Duration::from_millis(200)), 2);
        assert_eq!(c.release(0), 2);
        assert_eq!(c.release(1), 1);
        assert_eq!(
            c.press(0, (100.0, 100.0), t + Duration::from_millis(400)),
            3
        );
        assert_eq!(
            c.press(1, (100.0, 100.0), t + Duration::from_millis(500)),
            1,
            "换了键"
        );
        assert_eq!(
            c.press(1, (200.0, 100.0), t + Duration::from_millis(600)),
            1,
            "太远"
        );
        assert_eq!(
            c.press(1, (200.0, 100.0), t + Duration::from_secs(2)),
            1,
            "太久"
        );
    }

    #[test]
    fn x11_buttons() {
        assert_eq!(
            (0..6).map(x11_button).collect::<Vec<_>>(),
            [Some(1), Some(3), Some(2), Some(8), Some(9), None]
        );
        assert_eq!(x11_wheel_buttons(2, -3), [(4, 2), (6, 3)]);
        assert_eq!(x11_wheel_buttons(-1, 1), [(5, 1), (7, 1)]);
        assert_eq!(x11_wheel_buttons(0, 0), [(5, 0), (6, 0)]);
    }
}
