//! 剪贴板：图片格式转换（与系统无关，有单元测试）和真实系统剪贴板的入口。
//!
//! 系统剪贴板的读写和变化检测在 `clipboard_sys.rs`（系统胶水，测试构建不碰开发者电脑的剪贴板）。

/// 图片最多这么多像素（防止解压炸弹：一个很小的 PNG 声称有几十亿像素）。8K 是 3300 万。
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// RGBA 像素编码成 PNG。
pub fn rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    let expected = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4);
    if u64::try_from(rgba.len()).unwrap_or(u64::MAX) != expected {
        return Err(format!(
            "像素数据长度 {} 和 {width}×{height} 不符",
            rgba.len()
        ));
    }
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        // 截图里大片相同的颜色，快速压缩就够了：编码 4K 截图几十毫秒。
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// PNG 解码成 RGBA 像素：（宽，高，像素）。
pub fn png_to_rgba(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    // 调色板、灰度、16 位都展开成 8 位
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if u64::from(width).saturating_mul(u64::from(height)) > MAX_PIXELS {
        return Err(format!("图片太大：{width}×{height}"));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| "图片太大".to_owned())?;
    let mut buf = vec![0u8; size];
    let frame = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let bytes = buf.get(..frame.buffer_size()).unwrap_or_default();
    let rgba = match frame.color_type {
        png::ColorType::Rgba => bytes.to_vec(),
        png::ColorType::Rgb => bytes
            .chunks_exact(3)
            .flat_map(|p| {
                [p.first(), p.get(1), p.get(2)]
                    .map(|c| c.copied().unwrap_or(0))
                    .into_iter()
                    .chain([255])
            })
            .collect(),
        png::ColorType::GrayscaleAlpha => bytes
            .chunks_exact(2)
            .flat_map(|p| {
                let (g, a) = (
                    p.first().copied().unwrap_or(0),
                    p.get(1).copied().unwrap_or(255),
                );
                [g, g, g, a]
            })
            .collect(),
        png::ColorType::Grayscale => bytes.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return Err("调色板图片没有展开".into()),
    };
    Ok((width, height, rgba))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn encode(width: u32, height: u32, color: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, width, height);
        e.set_color(color);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(data).unwrap();
        out
    }

    #[test]
    fn rgba_round_trip() {
        let rgba: Vec<u8> = (0..4 * 3 * 2)
            .map(|i| u8::try_from(i * 7 % 256).unwrap())
            .collect();
        let png = rgba_to_png(3, 2, &rgba).unwrap();
        assert_eq!(png_to_rgba(&png).unwrap(), (3, 2, rgba));
        assert!(rgba_to_png(3, 2, &[0; 5]).is_err());
    }

    #[test]
    fn other_color_types_become_rgba() {
        let rgb = encode(2, 1, png::ColorType::Rgb, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(png_to_rgba(&rgb).unwrap().2, [1, 2, 3, 255, 4, 5, 6, 255]);
        let gray = encode(2, 1, png::ColorType::Grayscale, &[9, 8]);
        assert_eq!(png_to_rgba(&gray).unwrap().2, [9, 9, 9, 255, 8, 8, 8, 255]);
        let ga = encode(1, 1, png::ColorType::GrayscaleAlpha, &[7, 100]);
        assert_eq!(png_to_rgba(&ga).unwrap().2, [7, 7, 7, 100]);
        // 调色板图片展开成 RGB
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, 1, 1);
            e.set_color(png::ColorType::Indexed);
            e.set_depth(png::BitDepth::Eight);
            e.set_palette(vec![10, 20, 30]);
            e.write_header().unwrap().write_image_data(&[0]).unwrap();
        }
        assert_eq!(png_to_rgba(&out).unwrap().2, [10, 20, 30, 255]);
    }

    #[test]
    fn bad_and_huge_images_are_rejected() {
        assert!(png_to_rgba(b"not a png").is_err());
        // 声称 10 万 × 10 万像素：只读头就拒绝
        let mut huge = encode(1, 1, png::ColorType::Grayscale, &[0]);
        // IHDR 里的宽高在第 16..24 字节；改了以后 CRC 不对，解码器同样拒绝
        huge.splice(
            16..24,
            100_000u32
                .to_be_bytes()
                .into_iter()
                .chain(100_000u32.to_be_bytes()),
        );
        assert!(png_to_rgba(&huge).is_err());
    }
}
