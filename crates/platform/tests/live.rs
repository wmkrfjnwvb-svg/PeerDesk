//! 实机测试：在真实的桌面上采集和注入。需要图形界面，平时跳过。
//!
//! ```text
//! cargo test -p peerdesk-platform --test live -- --ignored --test-threads 1 --nocapture
//! ```
//!
//! * Linux：在 Xvfb 之类的 X 服务器上运行（`DISPLAY` 指向它），需要 `xterm`；
//! * Windows：在有交互桌面的机器上运行（GitHub 的 Windows 机器可以）；
//! * macOS：需要“屏幕录制”“辅助功能”权限，没有权限时打印说明并跳过，不算失败。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    clippy::cast_possible_truncation
)]

use std::time::{Duration, Instant};

use peerdesk_core::input::InputEvent;
use peerdesk_net::host::ScreenSource;
use peerdesk_platform::{
    check_permissions, cursor_position, displays, open_input, pixel, CaptureOptions, PlatformError,
    ScreenCapture,
};

/// 反复调用 `f` 直到返回 `Some`，最多 `limit`。
fn wait_for<T>(limit: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if let Some(v) = f() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// 打开采集；macOS 缺少权限时返回 `None`（打印说明）。
fn open_capture(max_fps: u32) -> Option<ScreenCapture> {
    let started = Instant::now();
    match ScreenCapture::open(CaptureOptions {
        display: 0,
        max_fps,
    }) {
        Ok(cap) => {
            println!("打开采集并拿到第一帧用了 {:?}", started.elapsed());
            Some(cap)
        }
        Err(PlatformError::Permission(why)) if cfg!(target_os = "macos") => {
            println!("跳过：{why}");
            None
        }
        Err(e) => panic!("打开采集失败：{e}"),
    }
}

fn report(cap: &ScreenCapture) {
    let stats = cap.stats();
    println!(
        "采集 {} 帧（读取 {} 次），每次读取平均 {:?}，最近一次 {:?}，重新打开 {} 次",
        stats.frames,
        stats.grabs,
        stats.mean_grab(),
        stats.last_grab,
        stats.reopens
    );
}

#[test]
#[ignore = "需要图形界面"]
fn capture_returns_the_desktop() {
    let list = displays();
    println!("显示器：{list:#?}");
    println!("权限：{:?}", check_permissions());
    assert!(!list.is_empty(), "列不出显示器");
    let Some(mut cap) = open_capture(30) else {
        return;
    };
    let primary = &list[0];
    let screen = cap.snapshot().expect("应当有画面");
    println!("画面 {}x{}", screen.width(), screen.height());
    assert_eq!(
        (u32::from(screen.width()), u32::from(screen.height())),
        (primary.width, primary.height)
    );
    assert_eq!(cap.change_seq(), Some(1));
    let distinct = screen
        .pixels()
        .iter()
        .take(1 << 20)
        .collect::<std::collections::HashSet<_>>()
        .len();
    println!(
        "平均亮度 {}，不同颜色 {distinct} 种",
        pixel::mean_luma(&screen)
    );
    // Linux 上的 Xvfb 桌面本来就是黑的（画面内容由 x11 模块里画窗口的测试核对）；
    // Windows、macOS 的桌面有壁纸和任务栏，全黑说明采集没有拿到真实内容。
    if !cfg!(target_os = "linux") {
        assert!(
            screen.pixels().iter().any(|&p| p != 0),
            "画面是全黑的：采集没有拿到桌面内容"
        );
    }
    report(&cap);
}

#[test]
#[ignore = "需要图形界面"]
fn input_moves_the_cursor() {
    let list = displays();
    let d = list.first().expect("列不出显示器");
    let mut input = match open_input() {
        Ok(input) => input,
        Err(PlatformError::Permission(why)) if cfg!(target_os = "macos") => {
            println!("跳过：{why}");
            return;
        }
        Err(e) => panic!("打开注入失败：{e}"),
    };
    // 三个位置：左上角附近、中心、右下角
    for (nx, ny) in [(1000u16, 2000u16), (32768, 32768), (65535, 65535)] {
        input.inject(&InputEvent::MouseMove { x: nx, y: ny });
        let scale = |n: u16, origin: i32, size: u32| {
            origin + i32::try_from(u64::from(n) * u64::from(size - 1) / 65535).unwrap()
        };
        let expect = if cfg!(target_os = "macos") {
            // macOS 的光标位置是点，不是像素
            let points = |n: u16, origin: i32, size: u32| {
                let size = f64::from(size) / d.scale;
                origin + (f64::from(n) / 65535.0 * (size - 1.0 / d.scale)).floor() as i32
            };
            (points(nx, d.x, d.width), points(ny, d.y, d.height))
        } else {
            (scale(nx, d.x, d.width), scale(ny, d.y, d.height))
        };
        let got = wait_for(Duration::from_secs(2), || {
            cursor_position().filter(|p| (p.0 - expect.0).abs() <= 1 && (p.1 - expect.1).abs() <= 1)
        });
        println!(
            "移到 ({nx}, {ny})：期望 {expect:?}，实际 {:?}",
            cursor_position()
        );
        assert!(got.is_some(), "光标没有移到期望的位置");
    }
}

#[cfg(target_os = "linux")]
mod x11 {
    use super::*;
    use peerdesk_platform::keymap::{ascii_to_hid, hid};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, CreateWindowAux, WindowClass};
    use x11rb::rust_connection::RustConnection;

    fn sync(conn: &RustConnection) {
        conn.get_input_focus().unwrap().reply().unwrap();
    }

    /// 在屏幕上开一个纯色窗口（不经过窗口管理器）。
    fn color_window(conn: &RustConnection, root: u32, color: u32) -> u32 {
        let win = conn.generate_id().unwrap();
        let aux = CreateWindowAux::new()
            .override_redirect(1)
            .background_pixel(color);
        conn.create_window(
            0,
            win,
            root,
            40,
            30,
            200,
            150,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &aux,
        )
        .unwrap();
        conn.map_window(win).unwrap();
        sync(conn);
        win
    }

    fn pixel_at(cap: &mut ScreenCapture, x: u16, y: u16) -> u32 {
        cap.snapshot().unwrap().row(x, y, 1)[0]
    }

    #[test]
    #[ignore = "需要 X 服务器"]
    fn capture_sees_drawing_and_counts_changes() {
        let (conn, screen) = x11rb::connect(None).expect("连不上 X 服务器");
        let root = conn.setup().roots[screen].root;
        let mut cap = open_capture(60).unwrap();

        let win = color_window(&conn, root, 0x0012_3456);
        let seen = wait_for(Duration::from_secs(5), || {
            (pixel_at(&mut cap, 100, 100) == 0x0012_3456).then_some(())
        });
        assert!(seen.is_some(), "画面里没有出现画上去的颜色");
        // 窗口外面不受影响
        assert_ne!(pixel_at(&mut cap, 300, 300), 0x0012_3456);

        // 画面静止：变化计数不动
        std::thread::sleep(Duration::from_millis(500));
        cap.frame();
        let quiet = cap.change_seq().unwrap();
        std::thread::sleep(Duration::from_secs(1));
        cap.frame();
        assert_eq!(cap.change_seq(), Some(quiet), "画面没变，计数却变了");

        // 画面变化：计数增加，看到新颜色
        conn.destroy_window(win).unwrap();
        let win = color_window(&conn, root, 0x00ab_cdef);
        let started = Instant::now();
        let seen = wait_for(Duration::from_secs(5), || {
            (pixel_at(&mut cap, 100, 100) == 0x00ab_cdef).then_some(())
        });
        println!("画面变化到采集到用了 {:?}", started.elapsed());
        assert!(seen.is_some(), "没有采集到新的画面");
        assert!(cap.change_seq().unwrap() > quiet);
        conn.destroy_window(win).unwrap();
        sync(&conn);

        // 测一下整屏读取的耗时：连续变化 30 次
        for i in 0..30u32 {
            let w = color_window(&conn, root, 0x0010_0000 + i);
            conn.destroy_window(w).unwrap();
            sync(&conn);
            std::thread::sleep(Duration::from_millis(20));
        }
        report(&cap);
    }

    #[test]
    #[ignore = "需要 X 服务器和 xterm"]
    fn xtest_types_into_xterm() {
        let dir = std::env::temp_dir().join(format!("peerdesk-xtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut xterm = std::process::Command::new("xterm")
            .args(["-geometry", "80x20+0+0", "-e", "sh"])
            .current_dir(&dir)
            .spawn()
            .expect("启动 xterm 失败（需要安装 xterm）");
        std::thread::sleep(Duration::from_secs(2));

        let mut input = open_input().unwrap();
        let d = &displays()[0];
        // 把指针移进 xterm（没有窗口管理器时焦点跟着指针）
        let norm = |px: u32, size: u32| {
            u16::try_from(u64::from(px) * 65535 / u64::from(size - 1)).unwrap()
        };
        input.inject(&InputEvent::MouseMove {
            x: norm(100, d.width),
            y: norm(100, d.height),
        });
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(cursor_position(), Some((d.x + 100, d.y + 100)));

        // 按键：按物理键位逐个敲出命令
        let mut type_keys = |text: &str| {
            for c in text.chars() {
                let (code, shift) = ascii_to_hid(c).unwrap();
                if shift {
                    input.inject(&InputEvent::Key {
                        code: hid::LEFT_SHIFT,
                        down: true,
                    });
                }
                input.inject(&InputEvent::Key { code, down: true });
                input.inject(&InputEvent::Key { code, down: false });
                if shift {
                    input.inject(&InputEvent::Key {
                        code: hid::LEFT_SHIFT,
                        down: false,
                    });
                }
            }
        };
        type_keys("touch Typed_By_XTEST\n");
        // 文字：输入法上屏的内容
        input.inject(&InputEvent::Text("touch typed-as-text\n".into()));

        let typed = dir.join("Typed_By_XTEST");
        let text = dir.join("typed-as-text");
        let ok = wait_for(Duration::from_secs(5), || {
            (typed.exists() && text.exists()).then_some(())
        });
        input.inject(&InputEvent::ReleaseAll);
        let _ = xterm.kill();
        let _ = xterm.wait();
        let listing: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(ok.is_some(), "xterm 里没有创建出文件：{listing:?}");
    }
}
