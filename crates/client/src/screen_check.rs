//! `peerdesk screen-check`：采集一帧屏幕，报告尺寸和耗时，可以存成 BMP 图片。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use peerdesk_core::screen::Screen;
use peerdesk_net::demo::to_bmp;
use peerdesk_net::host::ScreenSource;
use peerdesk_platform::{
    check_permissions, displays, pixel, CaptureOptions, CaptureStats, DisplayInfo,
    PermissionStatus, PlatformError, ScreenCapture,
};

use crate::{parse_number, usage_error, EXIT_FAILED, EXIT_OK};

struct Args {
    out: Option<PathBuf>,
    display: usize,
}

fn parse(args: &[String]) -> Result<Args, String> {
    let mut parsed = Args {
        out: None,
        display: 0,
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--out" => parsed.out = Some(it.next().ok_or("--out 需要一个值")?.into()),
            "--display" => parsed.display = parse_number(flag, it.next())?,
            other => return Err(format!("未知选项：{other}")),
        }
    }
    Ok(parsed)
}

/// 一次采集的结果。
struct Captured {
    screen: Screen,
    /// 从开始采集到拿到第一帧。
    first_frame: Duration,
    stats: CaptureStats,
}

/// 把结果写出来；给了 `out` 时存成 BMP。
fn report(
    displays: &[DisplayInfo],
    captured: &Captured,
    save: Option<&Path>,
    out: &mut dyn Write,
) -> u8 {
    for d in displays {
        let _ = writeln!(
            out,
            "显示器 {}{}：{}x{}，位置 ({}, {})，缩放 {}",
            d.index,
            if d.primary { "（主）" } else { "" },
            d.width,
            d.height,
            d.x,
            d.y,
            d.scale
        );
    }
    let s = &captured.screen;
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let _ = writeln!(out, "画面          {}x{}", s.width(), s.height());
    let _ = writeln!(
        out,
        "第一帧        {:.1} ms（含打开采集）",
        ms(captured.first_frame)
    );
    let _ = writeln!(
        out,
        "读取一帧      {:.1} ms（读出画面并转换格式）",
        ms(captured.stats.last_grab)
    );
    let luma = pixel::mean_luma(s);
    let _ = writeln!(out, "平均亮度      {luma}");
    if s.pixels().iter().all(|&p| p == 0) {
        let _ = writeln!(out, "警告：画面全黑，可能没有拿到真实的桌面内容。");
    }
    if let Some(path) = save {
        if let Err(e) = std::fs::write(path, to_bmp(s)) {
            let _ = writeln!(out, "错误：保存 {} 失败：{e}", path.display());
            return EXIT_FAILED;
        }
        let _ = writeln!(out, "已保存到 {}", path.display());
    }
    EXIT_OK
}

/// 缺少系统权限时告诉用户去哪里授权。
fn print_hints(status: PermissionStatus, out: &mut dyn Write) {
    for hint in status.hints() {
        let _ = writeln!(out, "{hint}");
    }
}

/// 打开采集，拿到第一帧就结束。
fn capture_once(opts: CaptureOptions) -> Result<Captured, PlatformError> {
    let started = Instant::now();
    let mut capture = ScreenCapture::open(opts)?;
    let first_frame = started.elapsed();
    let screen = capture.capture().cloned();
    let screen = screen.ok_or_else(|| PlatformError::Os("没有采集到画面".into()))?;
    Ok(Captured {
        screen,
        first_frame,
        stats: capture.stats(),
    })
}

/// `peerdesk screen-check`。
pub fn screen_check(args: &[String], out: &mut dyn Write) -> u8 {
    check_with(args, out, capture_once)
}

fn check_with(
    args: &[String],
    out: &mut dyn Write,
    capture: fn(CaptureOptions) -> Result<Captured, PlatformError>,
) -> u8 {
    let parsed = match parse(args) {
        Ok(parsed) => parsed,
        Err(why) => return usage_error(out, &why),
    };
    let opts = CaptureOptions {
        display: parsed.display,
        max_fps: 60,
    };
    match capture(opts) {
        Ok(captured) => report(&displays(), &captured, parsed.out.as_deref(), out),
        Err(e) => {
            let _ = writeln!(out, "错误：无法采集屏幕：{e}");
            print_hints(check_permissions(), out);
            EXIT_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> (u8, String) {
        let args: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
        let mut out = Vec::new();
        let code = screen_check(&args, &mut out);
        (code, String::from_utf8(out).unwrap())
    }

    #[test]
    fn bad_arguments() {
        for bad in [
            vec!["--bogus"],
            vec!["--out"],
            vec!["--display"],
            vec!["--display", "x"],
        ] {
            assert_eq!(run(&bad).0, crate::EXIT_USAGE, "{bad:?}");
        }
    }

    #[test]
    fn permission_hints_are_printed() {
        let mut out = Vec::new();
        let denied = PermissionStatus {
            screen_capture: false,
            input_injection: true,
        };
        print_hints(denied, &mut out);
        assert!(String::from_utf8(out).unwrap().contains("屏幕与系统录音"));
    }

    #[test]
    fn missing_display_is_an_error() {
        let (code, text) = run(&["--display", "999"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("无法采集屏幕"), "{text}");
    }

    #[test]
    fn captures_through_the_given_function() {
        fn fake(opts: CaptureOptions) -> Result<Captured, PlatformError> {
            assert_eq!(opts.display, 2);
            Ok(Captured {
                screen: Screen::new(3, 3, 0x0010_2030),
                first_frame: Duration::from_millis(5),
                stats: CaptureStats::default(),
            })
        }
        let args: Vec<String> = ["--display", "2"].iter().map(|s| (*s).to_owned()).collect();
        let mut out = Vec::new();
        assert_eq!(check_with(&args, &mut out, fake), EXIT_OK);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("画面          3x3"), "{text}");
    }

    #[test]
    fn report_prints_and_saves() {
        let display = DisplayInfo {
            index: 0,
            name: "test".into(),
            x: 0,
            y: 0,
            width: 4,
            height: 2,
            scale: 2.0,
            primary: true,
        };
        let other = DisplayInfo {
            index: 1,
            primary: false,
            ..display.clone()
        };
        let mut captured = Captured {
            screen: Screen::new(4, 2, 0x0080_8080),
            first_frame: Duration::from_millis(12),
            stats: CaptureStats {
                last_grab: Duration::from_micros(2500),
                ..CaptureStats::default()
            },
        };
        let path = std::env::temp_dir().join(format!("peerdesk-check-{}.bmp", std::process::id()));
        let mut out = Vec::new();
        let code = report(&[display, other], &captured, Some(&path), &mut out);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("显示器 0（主）：4x2"));
        assert!(text.contains("显示器 1："));
        assert!(text.contains("画面          4x2"));
        assert!(text.contains("第一帧        12.0 ms"));
        assert!(text.contains("读取一帧      2.5 ms"));
        assert!(!text.contains("全黑"));
        assert_eq!(std::fs::read(&path).unwrap(), to_bmp(&captured.screen));
        std::fs::remove_file(&path).unwrap();

        // 全黑的画面给出警告；存不下来时报错
        captured.screen = Screen::new(4, 2, 0);
        let bad = std::env::temp_dir().join("peerdesk-no-such-dir/x.bmp");
        let mut out = Vec::new();
        assert_eq!(report(&[], &captured, Some(&bad), &mut out), EXIT_FAILED);
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("全黑"));
        assert!(text.contains("保存"));
        // 不保存
        assert_eq!(report(&[], &captured, None, &mut Vec::new()), EXIT_OK);
    }
}
