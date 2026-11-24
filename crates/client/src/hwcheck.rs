//! `peerdesk hw-check`：编码器体检，用户在自己的电脑上跑一次，把结果发回来。
//!
//! 用本机真实屏幕（打不开时用合成桌面，并写明）做 30 帧“鼠标在动”的画面，然后：
//! 1. 软件编码（OpenH264）全分辨率和半分辨率各编一遍：每帧耗时、其中颜色转换耗时、关键帧数；
//! 2. 硬件编码器（macOS 对比几种设置）各编一遍：关键帧数、耗时、软件解码器接不接得上。
//!
//! 起因：Intel Mac 上 VideoToolbox 每帧都出关键帧，CI 的 Apple 芯片 Mac 复现不了，要在出问题的机器上测。

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use peerdesk_core::screen::Screen;
use peerdesk_core::video::VideoEncoder;
use peerdesk_media::{H264Encoder, HardwareEncoder};
use peerdesk_net::host::ScreenSource;

/// 编几帧。
const FRAMES: u16 = 30;

fn software_only(_: u32, _: u32, _: u32, _: u64) -> Result<Box<dyn HardwareEncoder>, String> {
    Err("体检时单独测软件编码".into())
}

/// 第 `i` 帧：原画面上一个“光标”方块往右挪了一点。
fn moved(base: &Screen, i: u16) -> Screen {
    let mut s = base.clone();
    let x = (i.saturating_mul(24)) % base.width().saturating_sub(32).max(1);
    let y = base.height() / 2;
    s.fill(x, y, 32, 32, 0x00ff_ffff);
    s
}

/// 宽高各缩一半（2×2 取平均）：Retina 屏按逻辑分辨率编码时的画面。
fn half(screen: &Screen) -> Screen {
    let (w, h) = (usize::from(screen.width()), usize::from(screen.height()));
    let (hw, hh) = (w / 2, h / 2);
    let px = screen.pixels();
    let at = |x: usize, y: usize| px.get(y * w + x).copied().unwrap_or(0);
    let mut out = Vec::with_capacity(hw * hh);
    for y in 0..hh {
        for x in 0..hw {
            let quad = [
                at(2 * x, 2 * y),
                at(2 * x + 1, 2 * y),
                at(2 * x, 2 * y + 1),
                at(2 * x + 1, 2 * y + 1),
            ];
            let avg = |shift: u32| quad.iter().map(|p| (p >> shift) & 0xff).sum::<u32>() / 4;
            out.push((avg(16) << 16) | (avg(8) << 8) | avg(0));
        }
    }
    Screen::from_pixels(
        u16::try_from(hw).unwrap_or(u16::MAX),
        u16::try_from(hh).unwrap_or(u16::MAX),
        out,
    )
    .unwrap_or_else(|_| Screen::new(1, 1, 0))
}

/// 软件编码 `frames` 帧：一行结果。
fn software(label: &str, frames: &[Screen]) -> String {
    let mut enc = H264Encoder::new(30).with_hardware(software_only);
    let (mut total, mut convert, mut keys, mut bytes) = (0u128, 0u128, 0u32, 0usize);
    for (i, screen) in frames.iter().enumerate() {
        let started = Instant::now();
        let frame = enc.encode(u32::try_from(i).unwrap_or(0) + 1, screen, true, 60_000);
        total += started.elapsed().as_micros();
        convert += u128::from(enc.convert_micros());
        if let Ok(Some(f)) = frame {
            keys += u32::from(f.key);
            bytes += f.payload.len();
        }
    }
    let n = u128::try_from(frames.len().max(1)).unwrap_or(1);
    let size = frames.first().map_or((0, 0), |s| (s.width(), s.height()));
    format!(
        "软件 OpenH264 {label} {}×{}：编码平均 {} ms（其中颜色转换 {} ms），关键帧 {keys}/{}，平均 {} 字节/帧",
        size.0,
        size.1,
        total / n / 1000,
        convert / n / 1000,
        frames.len(),
        bytes / frames.len().max(1)
    )
}

/// 合成桌面的一帧（屏幕打不开时用）。
fn synthetic() -> Screen {
    let mut demo = peerdesk_net::demo::DemoDesktop::new(1920, 1080, 1);
    demo.capture()
        .map_or_else(|| Screen::new(1920, 1080, 0), Clone::clone)
}

/// 体检报告的各行。`screen` 是本机屏幕的一帧（`None` 时用合成桌面）。
fn report(screen: Option<Screen>) -> Vec<String> {
    let mut lines = vec![format!(
        "PeerDesk {}（{}），系统 {} {}",
        peerdesk_core::VERSION,
        option_env!("PEERDESK_COMMIT").unwrap_or("dev"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )];
    let base = screen.unwrap_or_else(|| {
        lines.push("本机屏幕打不开，用合成桌面（结果参考价值低一些）".into());
        synthetic()
    });
    lines.push(format!(
        "画面 {}×{}，{FRAMES} 帧，方块每帧移动（模拟鼠标）",
        base.width(),
        base.height()
    ));
    let full: Vec<Screen> = (0..FRAMES).map(|i| moved(&base, i)).collect();
    let halves: Vec<Screen> = full.iter().map(half).collect();
    lines.push(software("全分辨率", &full));
    lines.push(software("半分辨率", &halves));
    let nv12: Vec<Vec<u8>> = full
        .iter()
        .map(|s| peerdesk_media::screen_to_nv12(s).2)
        .collect();
    let (w, h, _) = peerdesk_media::screen_to_nv12(&base);
    lines.push("硬件编码：".into());
    lines.extend(
        peerdesk_media::hardware_report(w, h, &nv12)
            .into_iter()
            .map(|l| format!("  {l}")),
    );
    lines
}

/// 抓本机屏幕的一帧（最多等 2 秒）。
fn grab() -> Option<Screen> {
    let mut source = crate::local::local_screen().ok()?;
    // 采集线程刚启动时可能还没有画面：最多试 40 次、每次等 50 ms
    (0..40).find_map(|_| {
        let screen = source.capture().cloned();
        std::thread::sleep(std::time::Duration::from_millis(
            50 * u64::from(screen.is_none()),
        ));
        screen
    })
}

/// `peerdesk hw-check`：打印报告，并存到日志目录的 `hw-check.txt`。
pub fn hw_check(out: &mut dyn Write, logs: &Path) -> u8 {
    let _ = writeln!(out, "编码器体检中（约半分钟）……");
    let lines = report(grab());
    let text = lines.join("\n") + "\n";
    let _ = write!(out, "{text}");
    let file = logs.join("hw-check.txt");
    match std::fs::create_dir_all(logs).and_then(|()| std::fs::write(&file, &text)) {
        Ok(()) => {
            let _ = writeln!(out, "\n已保存到 {}，把它发给我们", file.display());
        }
        Err(e) => {
            let _ = writeln!(
                out,
                "\n保存 {} 失败：{e}（请直接复制上面的内容）",
                file.display()
            );
        }
    }
    log::info!("编码器体检：\n{text}");
    crate::EXIT_OK
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn halves_and_moves() {
        let mut s = Screen::new(4, 2, 0x0010_2030);
        s.fill(0, 0, 1, 1, 0x0050_6070);
        let h = half(&s);
        assert_eq!((h.width(), h.height()), (2, 1));
        // 左上 2×2：一个 0x50.. 三个 0x10..，平均 0x20 0x30 0x40
        assert_eq!(h.pixels()[0], 0x0020_3040);
        assert_eq!(h.pixels()[1], 0x0010_2030);
        let base = Screen::new(100, 80, 0);
        assert_ne!(moved(&base, 1).pixels(), moved(&base, 2).pixels());
    }

    #[test]
    fn reports_software_and_hardware() {
        let dir = std::env::temp_dir().join(format!("peerdesk-hwcheck-{}", std::process::id()));
        let mut out = Vec::new();
        assert_eq!(hw_check(&mut out, &dir), crate::EXIT_OK);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("全分辨率") && text.contains("半分辨率"),
            "{text}"
        );
        assert!(text.contains("硬件编码"), "{text}");
        assert!(std::fs::read_to_string(dir.join("hw-check.txt"))
            .unwrap()
            .contains("半分辨率"));
        // 屏幕打不开：用合成桌面并写明
        assert!(report(None).iter().any(|l| l.contains("合成桌面")));
        // 日志目录写不了：照样输出，并说明
        let file = dir.join("not-a-dir");
        std::fs::write(&file, b"").unwrap();
        let mut out = Vec::new();
        let _ = hw_check(&mut out, &file);
        assert!(String::from_utf8(out).unwrap().contains("保存"));
    }
}
