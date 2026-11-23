//! RGB 和 YUV 4:2:0 之间的转换。
//!
//! openh264 自带的转换逐像素做浮点运算，而且按列遍历，1080p 一帧要几十毫秒，比编码本身还慢。
//! 这里用同一套 BT.601 有限范围系数（都是 1/256 的整数倍）做整数运算、按行遍历，
//! 结果和 openh264 的逐字节相同，快一个数量级。缓冲区由调用方复用，不必每帧重新分配。

/// 把 `0x00RRGGBB` 像素（宽高都是偶数）转成 I420 的三个平面，写进 `y`、`u`、`v`（先清空）。
pub fn rgb_to_i420(
    pixels: &[u32],
    width: usize,
    y: &mut Vec<u8>,
    u: &mut Vec<u8>,
    v: &mut Vec<u8>,
) {
    y.clear();
    u.clear();
    v.clear();
    if width == 0 {
        return;
    }
    let rgb = |p: u32| ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
    // 系数 ×256：Y = (66R + 129G + 25B)/256 + 16，U、V 用 2×2 块的和（再除以 4）。
    let luma = |p: u32| {
        let (r, g, b) = rgb(p);
        u8::try_from((66 * r + 129 * g + 25 * b + (16 << 8)) >> 8).unwrap_or(u8::MAX)
    };
    for rows in pixels.chunks_exact(width * 2) {
        let (top, bottom) = rows.split_at(width);
        y.extend(top.iter().map(|p| luma(*p)));
        y.extend(bottom.iter().map(|p| luma(*p)));
        for (a, b) in top.chunks_exact(2).zip(bottom.chunks_exact(2)) {
            let (mut sr, mut sg, mut sb) = (0i32, 0i32, 0i32);
            for p in a.iter().chain(b) {
                let (r, g, b) = rgb(*p);
                sr += i32::try_from(r).unwrap_or(0);
                sg += i32::try_from(g).unwrap_or(0);
                sb += i32::try_from(b).unwrap_or(0);
            }
            let chroma =
                |c: i32| u8::try_from(((c + (128 << 10)) >> 10).clamp(0, 255)).unwrap_or(0);
            u.push(chroma(-38 * sr - 74 * sg + 112 * sb));
            v.push(chroma(112 * sr - 94 * sg - 18 * sb));
        }
    }
}

/// RGB 字节（每像素 3 字节）打包成 `0x00RRGGBB`，追加到 `out` 后面。
pub fn pack_rgb(rgb: &[u8], out: &mut Vec<u32>) {
    out.extend(
        rgb.chunks_exact(3)
            .map(|p| p.iter().fold(0u32, |acc, c| acc << 8 | u32::from(*c))),
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use openh264::formats::{ArgbSliceU32, YUVBuffer, YUVSource};

    #[test]
    fn matches_openh264_byte_for_byte() {
        let (w, h) = (64usize, 34usize);
        let mut seed = 5u64;
        let pixels: Vec<u32> = (0..w * h)
            .map(|_| {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                u32::try_from(seed >> 40).unwrap() & 0x00ff_ffff
            })
            .collect();
        let reference = YUVBuffer::from_rgb_source(ArgbSliceU32::new(&pixels, (w, h)));
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        rgb_to_i420(&pixels, w, &mut y, &mut u, &mut v);
        assert_eq!(y.as_slice(), reference.y());
        assert_eq!(u.as_slice(), reference.u());
        assert_eq!(v.as_slice(), reference.v());
        // 极端颜色
        rgb_to_i420(&[0x00ff_ffff; 4], 2, &mut y, &mut u, &mut v);
        assert_eq!((y[0], u[0], v[0]), (235, 128, 128));
        rgb_to_i420(&[], 0, &mut y, &mut u, &mut v);
        assert!(y.is_empty());
        let mut out = vec![7];
        pack_rgb(&[1, 2, 3, 4, 5, 6], &mut out);
        assert_eq!(out, vec![7, 0x0001_0203, 0x0004_0506]);
    }
}

/// I420（Y、U、V 三个平面）转成 NV12（Y 平面加 U、V 交错的一个平面），硬件编码器要这种格式。
/// `out` 复用，长度变成 `y.len() + u.len() + v.len()`。
pub fn i420_to_nv12(y: &[u8], u: &[u8], v: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.reserve(y.len() + u.len() + v.len());
    out.extend_from_slice(y);
    for (a, b) in u.iter().zip(v) {
        out.push(*a);
        out.push(*b);
    }
}

#[cfg(test)]
mod nv12_tests {
    #[test]
    fn interleaves_chroma() {
        let mut out = vec![9; 3];
        super::i420_to_nv12(&[1, 2, 3, 4], &[10], &[20], &mut out);
        assert_eq!(out, [1, 2, 3, 4, 10, 20]);
    }
}
