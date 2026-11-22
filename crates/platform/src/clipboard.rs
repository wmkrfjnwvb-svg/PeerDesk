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

/// PNG 解码成 RGBA 像素：（宽，高，像素）。超过 [`MAX_PIXELS`] 的拒绝。
pub fn png_to_rgba(data: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    png_to_rgba_within(data, MAX_PIXELS)
}

fn png_to_rgba_within(data: &[u8], max_pixels: u64) -> Result<(u32, u32, Vec<u8>), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    // 调色板、灰度、16 位都展开成 8 位
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let (width, height) = {
        let info = reader.info();
        (info.width, info.height)
    };
    if u64::from(width).saturating_mul(u64::from(height)) > max_pixels {
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

/// 把复制的文件和文件夹打包（[`peerdesk_core::clipboard::pack_files`]）。
/// 总大小超过 `limit` 或文件太多时停止读取，返回 `Err(已经统计到的大小)`；读不了的文件跳过。
pub fn pack_paths(paths: &[std::path::PathBuf], limit: u64) -> Result<Vec<u8>, u64> {
    use peerdesk_core::clipboard::{pack_files, PackedFile, MAX_FILES};
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut stack: Vec<(std::path::PathBuf, String)> = paths
        .iter()
        .filter_map(|p| Some((p.clone(), p.file_name()?.to_string_lossy().into_owned())))
        .collect();
    while let Some((path, rel)) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            let Ok(entries) = std::fs::read_dir(&path) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                stack.push((entry.path(), format!("{rel}/{name}")));
            }
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
            if total > limit || files.len() >= MAX_FILES {
                return Err(total.max(limit.saturating_add(1)));
            }
            if let Ok(data) = std::fs::read(&path) {
                files.push(PackedFile { path: rel, data });
            }
        }
        // 符号链接等跳过（不跟随，避免打包到复制范围以外的东西）
    }
    pack_files(&files).map_err(|_| total)
}

/// 把收到的文件解包到 `dir` 下，返回最上层的路径（放进剪贴板，粘贴时就是这些文件和文件夹）。
pub fn unpack_to(data: &[u8], dir: &std::path::Path) -> Result<Vec<std::path::PathBuf>, String> {
    let files = peerdesk_core::clipboard::unpack_files(data).map_err(|e| e.to_string())?;
    let mut top: Vec<std::path::PathBuf> = Vec::new();
    for f in files {
        let target = dir.join(&f.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("建目录失败：{e}"))?;
        }
        std::fs::write(&target, &f.data).map_err(|e| format!("写文件失败：{e}"))?;
        let first = f.path.split('/').next().unwrap_or_default();
        let first = dir.join(first);
        if !top.contains(&first) {
            top.push(first);
        }
    }
    Ok(top)
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

    fn temp(tag: &str) -> std::path::PathBuf {
        let d =
            std::env::temp_dir().join(format!("peerdesk-clipfiles-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn files_and_folders_round_trip_through_the_clipboard() {
        let src = temp("src");
        std::fs::write(src.join("a.txt"), b"hello").unwrap();
        std::fs::create_dir_all(src.join("folder/sub")).unwrap();
        std::fs::write(src.join("folder/sub/b.bin"), [1u8, 2, 3]).unwrap();
        std::fs::write(src.join("folder/c.txt"), b"").unwrap();
        let packed = pack_paths(
            &[src.join("a.txt"), src.join("folder"), src.join("missing")],
            1024,
        )
        .unwrap();
        let dst = temp("dst");
        let mut top = unpack_to(&packed, &dst).unwrap();
        top.sort();
        assert_eq!(top, vec![dst.join("a.txt"), dst.join("folder")]);
        assert_eq!(std::fs::read(dst.join("a.txt")).unwrap(), b"hello");
        assert_eq!(
            std::fs::read(dst.join("folder/sub/b.bin")).unwrap(),
            [1, 2, 3]
        );
        assert!(dst.join("folder/c.txt").exists());
        // 超过上限：不读完，返回大小
        let over = pack_paths(&[src.join("a.txt"), src.join("folder")], 4).unwrap_err();
        assert!(over > 4);
        // 坏数据
        assert!(unpack_to(&[1, 2, 3], &dst).is_err());
        for d in [src, dst] {
            let _ = std::fs::remove_dir_all(d);
        }
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
        // 像素数超过上限：只读头就拒绝
        let two = encode(2, 1, png::ColorType::Grayscale, &[0, 0]);
        assert!(png_to_rgba_within(&two, 1)
            .unwrap_err()
            .contains("图片太大"));
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
