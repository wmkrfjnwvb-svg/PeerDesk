//! 远程画面在窗口里的位置，以及本地指针位置到被控端坐标的换算。

use eframe::egui::{pos2, vec2, Pos2, Rect};
use num_traits::ToPrimitive;

/// 画面在 `avail` 里显示的位置（单位是 egui 的点）。
///
/// 放得下时按 1:1 显示（一个远程像素对应一个物理像素），否则保持宽高比缩小到正好放下。
/// 都居中。`pixels_per_point` 是本地屏幕的缩放比例。
#[must_use]
pub fn fit(avail: Rect, image: [u16; 2], pixels_per_point: f32) -> Rect {
    let ppp = if pixels_per_point > 0.0 {
        pixels_per_point
    } else {
        1.0
    };
    let native = vec2(f32::from(image[0]), f32::from(image[1])) / ppp;
    if native.x <= 0.0 || native.y <= 0.0 {
        return Rect::from_center_size(avail.center(), vec2(0.0, 0.0));
    }
    let scale = (avail.width() / native.x)
        .min(avail.height() / native.y)
        .clamp(0.0, 1.0);
    Rect::from_center_size(avail.center(), native * scale)
}

/// 指针位置换算成被控端的归一化坐标（0..=65535）。在画面外时返回 `None`。
#[must_use]
pub fn normalize(pos: Pos2, picture: Rect) -> Option<(u16, u16)> {
    if !picture.contains(pos) || picture.width() <= 0.0 || picture.height() <= 0.0 {
        return None;
    }
    let axis = |offset: f32, size: f32| {
        (offset / size * 65535.0)
            .round()
            .clamp(0.0, 65535.0)
            .to_u16()
    };
    let x = axis(pos.x - picture.min.x, picture.width())?;
    let y = axis(pos.y - picture.min.y, picture.height())?;
    Some((x, y))
}

/// 被控端的归一化坐标换回本地位置（测试和光标提示用）。
#[must_use]
pub fn denormalize(x: u16, y: u16, picture: Rect) -> Pos2 {
    pos2(
        picture.min.x + f32::from(x) / 65535.0 * picture.width(),
        picture.min.y + f32::from(y) / 65535.0 * picture.height(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), vec2(w, h))
    }

    #[test]
    fn draws_one_to_one_when_it_fits() {
        let r = fit(rect(0.0, 0.0, 2000.0, 1000.0), [1280, 720], 1.0);
        assert_eq!(r, rect(360.0, 140.0, 1280.0, 720.0));
        // 2 倍缩放的屏幕上，1:1 是 640×360 个点
        let r = fit(rect(0.0, 0.0, 2000.0, 1000.0), [1280, 720], 2.0);
        assert_eq!(r.size(), vec2(640.0, 360.0));
        // 缩放比例不合法时按 1 处理
        let r = fit(rect(0.0, 0.0, 2000.0, 1000.0), [1280, 720], 0.0);
        assert_eq!(r.size(), vec2(1280.0, 720.0));
    }

    #[test]
    fn scales_down_keeping_the_aspect_ratio() {
        // 太宽：按宽度缩小，上下留黑边
        let r = fit(rect(0.0, 0.0, 640.0, 1000.0), [1280, 720], 1.0);
        assert_eq!(r, rect(0.0, 320.0, 640.0, 360.0));
        // 太高：按高度缩小，左右留黑边
        let r = fit(rect(10.0, 10.0, 2000.0, 360.0), [1280, 720], 1.0);
        assert_eq!(r, rect(690.0, 10.0, 640.0, 360.0));
        // 空画面
        assert_eq!(
            fit(rect(0.0, 0.0, 10.0, 10.0), [0, 720], 1.0).size(),
            vec2(0.0, 0.0)
        );
    }

    #[test]
    fn pointer_mapping() {
        let pic = rect(100.0, 50.0, 400.0, 200.0);
        assert_eq!(normalize(pos2(100.0, 50.0), pic), Some((0, 0)));
        assert_eq!(normalize(pos2(500.0, 250.0), pic), Some((65535, 65535)));
        assert_eq!(normalize(pos2(300.0, 150.0), pic), Some((32768, 32768)));
        assert_eq!(normalize(pos2(99.0, 60.0), pic), None);
        assert_eq!(normalize(pos2(200.0, 251.0), pic), None);
        assert_eq!(normalize(pos2(0.0, 0.0), rect(0.0, 0.0, 0.0, 0.0)), None);
        let (x, y) = normalize(pos2(123.0, 77.0), pic).unwrap();
        let back = denormalize(x, y, pic);
        assert!((back.x - 123.0).abs() < 0.01 && (back.y - 77.0).abs() < 0.01);
    }
}
