//! 把结果写成人能直接看的东西：并排截图（PNG）、一行一个阶段的表格（Markdown）、机器读的 JSON。

use std::fmt::Write as _;
use std::fs;
use std::io::BufWriter;
use std::path::Path;

use peerdesk_core::error::Error;
use peerdesk_core::screen::Screen;

use crate::{Outcome, PhaseResult};

fn io(e: &std::io::Error) -> Error {
    Error::Storage(e.to_string())
}

/// 并排图：源画面 | 显示的画面 | 差异（放大 8 倍，越亮差得越多）。
#[must_use]
pub fn side_by_side(source: &Screen, shown: &Screen) -> (u32, u32, Vec<u8>) {
    let (w, h) = (u32::from(source.width()), u32::from(source.height()));
    let mut rgb = Vec::with_capacity(usize::try_from(w * 3 * h * 3).unwrap_or(0));
    let channel = |p: u32, shift: u32| u8::try_from((p >> shift) & 0xff).unwrap_or(0);
    for y in 0..u16::try_from(h).unwrap_or(0) {
        let a = source.row(0, y, source.width());
        let b = if (shown.width(), shown.height()) == (source.width(), source.height()) {
            shown.row(0, y, shown.width())
        } else {
            a
        };
        for p in a.iter().chain(b) {
            rgb.extend([channel(*p, 16), channel(*p, 8), channel(*p, 0)]);
        }
        for (pa, pb) in a.iter().zip(b) {
            let d = [16, 8, 0]
                .iter()
                .map(|s| channel(*pa, *s).abs_diff(channel(*pb, *s)))
                .max()
                .unwrap_or(0);
            let v = d.saturating_mul(8);
            rgb.extend([v, v, v]);
        }
    }
    (w * 3, h, rgb)
}

/// 写 PNG。
pub fn write_png(path: &Path, width: u32, height: u32, rgb: &[u8]) -> Result<(), Error> {
    let file = fs::File::create(path).map_err(|e| io(&e))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|e| Error::Storage(e.to_string()))?;
    writer
        .write_image_data(rgb)
        .map_err(|e| Error::Storage(e.to_string()))
}

fn opt(v: Option<f64>) -> String {
    v.map_or_else(|| "—".to_owned(), |v| format!("{v:.0}"))
}

/// 一个阶段的表格行。
fn row(o: &Outcome, p: &PhaseResult) -> String {
    format!(
        "| {} | {}/{} | {} | {} | {:.0} / {:.0} | {:.0} | {} | {:.1} | {} |",
        o.name,
        o.codec.name(),
        o.estimator,
        p.action.name(),
        p.kbps,
        p.latency_ms.0,
        p.latency_ms.1,
        p.max_wait_ms,
        p.freezes,
        p.end_psnr,
        opt(p.sharp_ms),
    )
}

/// 表头。
pub const TABLE_HEADER: &str = "| 场景 | 编码/估计器 | 操作 | 码率 kbps | 延迟 p50/p95 ms | 最长等待 ms | 卡顿次数 | 结束 PSNR dB | 变清晰 ms |\n|---|---|---|---|---|---|---|---|---|\n";

/// 整个场景的 Markdown 表格行（不含表头）。
#[must_use]
pub fn markdown(o: &Outcome) -> String {
    let mut out = String::new();
    for p in &o.phases {
        let _ = writeln!(out, "{}", row(o, p));
    }
    out
}

/// JSON（手写，字段固定，不需要额外依赖）。
#[must_use]
pub fn json(o: &Outcome) -> String {
    let phases: Vec<String> = o
        .phases
        .iter()
        .map(|p| {
            format!(
                "{{\"action\":\"{}\",\"bytes\":{},\"kbps\":{},\"frames_shown\":{},\"latency_p50_ms\":{:.1},\"latency_p95_ms\":{:.1},\"max_wait_ms\":{:.1},\"freezes\":{},\"end_psnr\":{:.2},\"sharp_ms\":{}}}",
                p.action.name(),
                p.bytes,
                p.kbps,
                p.frames_shown,
                p.latency_ms.0,
                p.latency_ms.1,
                p.max_wait_ms,
                p.freezes,
                p.end_psnr,
                p.sharp_ms.map_or_else(|| "null".to_owned(), |v| format!("{v:.1}")),
            )
        })
        .collect();
    format!(
        "{{\"name\":\"{}\",\"codec\":\"{}\",\"estimator\":\"{}\",\"keyframes\":{},\"recoveries\":{},\"skipped\":{},\"bad\":{},\"net_lost\":{},\"net_queue_dropped\":{},\"max_wait_ms\":{:.1},\"freezes\":{},\"phases\":[{}]}}",
        o.name,
        o.codec.name(),
        o.estimator,
        o.keyframes,
        o.recoveries,
        o.skipped,
        o.bad,
        o.net.lost,
        o.net.queue_dropped,
        o.max_wait_ms,
        o.freezes,
        phases.join(","),
    )
}

/// 把一个场景的截图、JSON 和表格写进 `dir/<场景名>-<编码>/`。
pub fn save(dir: &Path, o: &Outcome) -> Result<(), Error> {
    let out = dir.join(format!("{}-{}-{}", o.name, o.codec.name(), o.estimator));
    fs::create_dir_all(&out).map_err(|e| io(&e))?;
    for (i, p) in o.phases.iter().enumerate() {
        let (w, h, rgb) = side_by_side(&p.snapshot.0, &p.snapshot.1);
        write_png(
            &out.join(format!("{:02}-{}.png", i + 1, p.action.name())),
            w,
            h,
            &rgb,
        )?;
    }
    fs::write(out.join("result.json"), json(o)).map_err(|e| io(&e))?;
    let mut csv = String::from(crate::FeatureRow::HEADER);
    for row in &o.features {
        csv.push('\n');
        csv.push_str(&row.csv());
    }
    fs::write(out.join("features.csv"), csv).map_err(|e| io(&e))?;
    fs::write(
        out.join("result.md"),
        format!("{TABLE_HEADER}{}", markdown(o)),
    )
    .map_err(|e| io(&e))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn mismatched_sizes_and_unwritable_paths() {
        let a = Screen::new(4, 2, 0x0010_2030);
        let (w, h, rgb) = side_by_side(&a, &Screen::new(1, 1, 0));
        assert_eq!((w, h, rgb.len()), (12, 2, 12 * 2 * 3));
        let dir = std::env::temp_dir().join(format!("peerdesk-report-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("not-a-dir");
        fs::write(&file, b"x").unwrap();
        assert!(write_png(&file.join("a.png"), 1, 1, &[0, 0, 0]).is_err());
        assert!(save(&file, &crate::tests::tiny()).is_err());
        assert!(opt(None) == "—" && opt(Some(1.4)) == "1");
        fs::remove_dir_all(&dir).unwrap();
    }
}
