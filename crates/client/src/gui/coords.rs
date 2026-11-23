//! 远程画面在窗口里的位置，以及本地指针位置到被控端坐标的换算。

use eframe::egui::{pos2, vec2, Pos2, Rect};
use num_traits::ToPrimitive;

/// 画面在 `avail` 里显示的位置（单位是 egui 的点）。
///
/// 保持宽高比缩放到正好放进窗口（放大或缩小），居中；宽高比不同时只在一个方向留边。
/// 原来放得下时按 1:1 显示，结果在 Retina 屏上全屏时 1080p 的远程画面只占屏幕中间四分之一
/// （1920×1080 像素 = 960×540 点）。同类产品默认都是“适应窗口”。
/// `pixels_per_point` 是本地屏幕的缩放比例；只在画面为空时有意义，留着参数便于以后加“原始大小”。
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
        .max(0.0);
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
    fn scales_up_to_fill_the_window() {
        // 窗口比画面大：放大到碰到一边（这里是高度），左右留边
        let r = fit(rect(0.0, 0.0, 2000.0, 1000.0), [1280, 720], 1.0);
        assert!((r.height() - 1000.0).abs() < 0.01 && r.center() == pos2(1000.0, 500.0));
        // Retina（2 倍）全屏：1920×1080 的画面铺满 1512×850 点的屏幕宽度，不是只占中间一块
        // （屏幕比 16:9 略宽，所以高度铺满、左右各留不到 1 个点）
        let r = fit(rect(0.0, 0.0, 1512.0, 850.0), [1920, 1080], 2.0);
        assert!(
            (r.height() - 850.0).abs() < 0.01 && r.width() > 1510.0,
            "{r:?}"
        );
        // 缩放比例不合法时按 1 处理，结果一样
        let r = fit(rect(0.0, 0.0, 1512.0, 850.0), [1920, 1080], 0.0);
        assert!((r.height() - 850.0).abs() < 0.01);
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
