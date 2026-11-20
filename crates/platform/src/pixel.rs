//! 像素格式转换：系统给的 BGRA（内存里依次是 B、G、R、A 四个字节）→ [`Screen`] 的 `0x00RRGGBB`。
//!
//! 三个系统的采集结果都是这种排列（X11 的 24/32 位小端 TrueColor、DXGI 的 `B8G8R8A8_UNORM`、
//! ScreenCaptureKit 的 `kCVPixelFormatType_32BGRA`），每行末尾可能有对齐用的填充（`stride`）。

use peerdesk_core::screen::Screen;

use crate::PlatformError;

/// 每像素字节数。
pub const BYTES_PER_PIXEL: usize = 4;

/// 一个 BGRA 像素 → `0x00RRGGBB`（丢掉 alpha）。
#[inline]
#[must_use]
pub const fn bgra_to_rgb(b: u8, g: u8, r: u8) -> u32 {
    u32::from_le_bytes([b, g, r, 0])
}

/// 检查画面尺寸能放进 [`Screen`]（宽高都是 u16 且不为 0），返回 `(宽, 高)`。
pub fn screen_size(width: usize, height: usize) -> Result<(u16, u16), PlatformError> {
    match (u16::try_from(width), u16::try_from(height)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Ok((w, h)),
        (Ok(_) | Err(_), Ok(_) | Err(_)) => Err(PlatformError::Os(format!(
            "画面尺寸 {width}x{height} 不受支持"
        ))),
    }
}

/// 把一幅 BGRA 画面转换进 `spare`（尺寸相同时复用它的内存，否则新建）。
///
/// `data` 至少要有 `stride * (height - 1) + width * 4` 个字节，`stride` 不小于 `width * 4`。
pub fn bgra_to_screen(
    spare: Option<Screen>,
    width: usize,
    height: usize,
    data: &[u8],
    stride: usize,
) -> Result<Screen, PlatformError> {
    let (w, h) = screen_size(width, height)?;
    let row_bytes = width.saturating_mul(BYTES_PER_PIXEL);
    let needed = stride
        .checked_mul(height - 1)
        .and_then(|n| n.checked_add(row_bytes));
    if stride < row_bytes || needed.is_none_or(|n| data.len() < n) {
        return Err(PlatformError::Os(format!(
            "画面数据不完整：{width}x{height}，每行 {stride} 字节，共 {} 字节",
            data.len()
        )));
    }
    let mut screen = match spare {
        Some(s) if (s.width(), s.height()) == (w, h) => s,
        Some(_) | None => Screen::new(w, h, 0),
    };
    for (y, src) in (0..h).zip(data.chunks(stride)) {
        let dst = screen.row_mut(0, y, w);
        for (d, px) in dst.iter_mut().zip(src.chunks_exact(BYTES_PER_PIXEL)) {
            if let [b, g, r, _] = *px {
                *d = bgra_to_rgb(b, g, r);
            }
        }
    }
    Ok(screen)
}

/// 平均亮度（0 到 255），用来判断画面是不是全黑。
#[must_use]
pub fn mean_luma(screen: &Screen) -> u8 {
    let pixels = screen.pixels();
    if pixels.is_empty() {
        return 0;
    }
    let sum: u64 = pixels
        .iter()
        .map(|p| {
            let [b, g, r, _] = p.to_le_bytes();
            // BT.601 的近似：(2R + 5G + B) / 8
            (2 * u64::from(r) + 5 * u64::from(g) + u64::from(b)) / 8
        })
        .sum();
    let count = u64::try_from(pixels.len()).unwrap_or(u64::MAX);
    u8::try_from(sum / count).unwrap_or(u8::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_with_padding() {
        // 2x2，每行 12 字节（8 字节像素 + 4 字节填充）
        let data = [
            0x56, 0x34, 0x12, 0xff, 1, 2, 3, 0, 9, 9, 9, 9, //
            0, 0, 0xff, 0x80, 0xff, 0xff, 0xff, 0xff, 9, 9, 9, 9,
        ];
        let s = bgra_to_screen(None, 2, 2, &data, 12).unwrap();
        assert_eq!(
            s.pixels(),
            &[0x0012_3456, 0x0003_0201, 0x00ff_0000, 0x00ff_ffff]
        );
        // 最后一行不需要填充
        let s = bgra_to_screen(None, 2, 2, &data[..20], 12).unwrap();
        assert_eq!(s.pixels()[3], 0x00ff_ffff);
    }

    #[test]
    fn reuses_a_matching_buffer() {
        let data = [1u8, 2, 3, 4];
        let spare = Screen::new(1, 1, 0x00ab_cdef);
        let s = bgra_to_screen(Some(spare), 1, 1, &data, 4).unwrap();
        assert_eq!(s.pixels(), &[0x0003_0201]);
        // 尺寸不同：不复用
        let spare = Screen::new(5, 5, 0);
        let s = bgra_to_screen(Some(spare), 1, 1, &data, 4).unwrap();
        assert_eq!((s.width(), s.height()), (1, 1));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(bgra_to_screen(None, 0, 1, &[], 0).is_err());
        assert!(bgra_to_screen(None, 70_000, 1, &[], 280_000).is_err());
        assert!(bgra_to_screen(None, 1, 70_000, &[], 4).is_err());
        let err = bgra_to_screen(None, 2, 2, &[0; 15], 8).unwrap_err();
        assert!(err.to_string().contains("不完整"), "{err}");
        assert!(bgra_to_screen(None, 2, 1, &[0; 8], 4).is_err(), "每行不够");
        assert!(bgra_to_screen(None, 2, 2, &[0; 8], usize::MAX).is_err());
    }

    #[test]
    fn luma() {
        assert_eq!(mean_luma(&Screen::new(4, 4, 0)), 0);
        assert_eq!(mean_luma(&Screen::new(4, 4, 0x00ff_ffff)), 255);
        assert_eq!(mean_luma(&Screen::new(0, 0, 0)), 0);
        let mut s = Screen::new(2, 1, 0);
        s.fill(0, 0, 1, 1, 0x00ff_ffff);
        assert_eq!(mean_luma(&s), 127);
    }
}
