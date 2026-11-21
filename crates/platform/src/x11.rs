//! Linux X11 的系统胶水：显示器枚举、MIT-SHM / GetImage 采集、DAMAGE 变化检测、XTEST 注入。
//!
//! 用 x11rb 的纯 Rust 连接（不依赖 libX11/libxcb）。采集线程和注入各用一条独立连接，互不阻塞。
//!
//! 部分代码改编自 Airhand（MIT 许可，见仓库根目录的 THIRD_PARTY_NOTICES.md）：
//! `client/src-tauri/src/host/x11.rs`（连接、显示器枚举、等待可读）、
//! `client/src-tauri/src/host/capture/x11.rs`（共享内存段、DAMAGE、像素格式检查）、
//! `client/src-tauri/src/host/input/x11.rs`（XTEST 注入、临时绑定键码输入文字）。

#![allow(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant};

use peerdesk_core::input::InputEvent;
use peerdesk_core::screen::Screen;
use peerdesk_net::host::InputSink;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::randr::{self, ConnectionExt as _};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _, ImageFormat, ImageOrder, Window};
use x11rb::protocol::xtest::{self, ConnectionExt as _};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use crate::capture::Grabber;
use crate::keymap::{to_x11, x11_keysym};
use crate::pixel::{bgra_to_screen, BYTES_PER_PIXEL};
use crate::pointer::{to_pixel, x11_button, x11_wheel_buttons, WheelAccumulator, WHEEL_NOTCH};
use crate::session::SessionEnv;
use crate::{pick_display, sort_displays, CaptureOptions, DisplayInfo, PermissionStatus};
use crate::{PlatformError, PlatformError as E};

/// 多久检查一次显示器布局有没有变（有些变化不发 RandR 通知，例如 `xrandr --setmonitor`）。
const LAYOUT_CHECK: Duration = Duration::from_secs(2);
/// 注入时用的显示器位置多久刷新一次。
const GEOMETRY_REFRESH: Duration = Duration::from_secs(2);
/// 输入文字时临时改了键盘映射后，等各个程序处理完 MappingNotify 再按键。
const REMAP_SETTLE: Duration = Duration::from_millis(10);
/// keysym：左 Shift。
const XK_SHIFT_L: u32 = 0xffe1;

fn os<T: Display>(what: &str, e: T) -> PlatformError {
    E::Os(format!("X11 {what}：{e}"))
}

fn lost<T: Display>(e: T) -> PlatformError {
    E::Lost(format!("与 X 服务器的连接出错：{e}"))
}

/// 一条 X11 连接和它的默认屏幕。
struct X11 {
    conn: RustConnection,
    root: Window,
    width: u16,
    height: u16,
}

impl X11 {
    fn connect() -> Result<Self, PlatformError> {
        SessionEnv::current().check_x11()?;
        let (conn, screen) = x11rb::connect(None).map_err(|e| {
            E::Unsupported(format!("无法连接 X 服务器（DISPLAY 指向的图形界面）：{e}"))
        })?;
        let s = conn
            .setup()
            .roots
            .get(screen)
            .ok_or_else(|| E::Os(format!("X 服务器没有第 {screen} 个屏幕")))?;
        let (root, width, height) = (s.root, s.width_in_pixels, s.height_in_pixels);
        Ok(Self {
            conn,
            root,
            width,
            height,
        })
    }

    fn has_extension(&self, name: &'static str) -> bool {
        matches!(self.conn.extension_information(name), Ok(Some(_)))
    }

    /// 等服务器处理完之前的所有请求（顺便刷新发送缓冲）。
    fn sync(&self) -> Result<(), PlatformError> {
        self.conn
            .get_input_focus()
            .map_err(lost)?
            .reply()
            .map_err(lost)?;
        Ok(())
    }

    /// 等连接上有数据可读，最多 `timeout`。
    fn wait_readable(&self, timeout: Duration) -> Result<(), PlatformError> {
        self.conn.flush().map_err(lost)?;
        let mut fds = libc::pollfd {
            fd: self.conn.stream().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
        // SAFETY: `fds` 是一个有效、可写的 pollfd，个数为 1；文件描述符在调用期间由连接持有。
        // 返回值不用看：被信号打断或超时都由调用方重新检查事件。
        let _ = unsafe { libc::poll(&mut fds, 1, ms) };
        Ok(())
    }

    /// 列出显示器：有 RandR 1.5 时按显示器列，否则把整个屏幕当作一个显示器。
    fn monitors(&self) -> Result<Vec<DisplayInfo>, PlatformError> {
        let mut list = Vec::new();
        let randr15 = self.has_extension(randr::X11_EXTENSION_NAME)
            && self
                .conn
                .randr_query_version(1, 5)
                .map_err(lost)?
                .reply()
                .is_ok_and(|v| (v.major_version, v.minor_version) >= (1, 5));
        if randr15 {
            let reply = self
                .conn
                .randr_get_monitors(self.root, true)
                .map_err(lost)?
                .reply()
                .map_err(|e| os("RRGetMonitors", e))?;
            for m in reply.monitors {
                // 超出根窗口的部分采集不到，裁掉
                let (x, y) = (i32::from(m.x.max(0)), i32::from(m.y.max(0)));
                let w = i32::from(m.width).min(i32::from(self.width) - x);
                let h = i32::from(m.height).min(i32::from(self.height) - y);
                let (Ok(width), Ok(height)) = (u32::try_from(w), u32::try_from(h)) else {
                    continue;
                };
                if width == 0 || height == 0 {
                    continue;
                }
                let name = self
                    .conn
                    .get_atom_name(m.name)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                    .unwrap_or_default();
                list.push(DisplayInfo {
                    index: 0,
                    name,
                    x,
                    y,
                    width,
                    height,
                    scale: 1.0,
                    primary: m.primary,
                });
            }
        }
        if list.is_empty() {
            list.push(DisplayInfo {
                index: 0,
                name: "X11".into(),
                x: 0,
                y: 0,
                width: u32::from(self.width),
                height: u32::from(self.height),
                scale: 1.0,
                primary: true,
            });
        }
        Ok(sort_displays(list))
    }

    /// 只支持最常见的 24/32 位小端 TrueColor（内存里是 BGRX 排列）。
    fn check_format(&self) -> Result<(), PlatformError> {
        let setup = self.conn.setup();
        let screen = setup
            .roots
            .iter()
            .find(|s| s.root == self.root)
            .ok_or_else(|| E::Os("找不到根窗口所在的屏幕".into()))?;
        let bpp = setup
            .pixmap_formats
            .iter()
            .find(|f| f.depth == screen.root_depth)
            .map(|f| f.bits_per_pixel);
        let masks = screen
            .allowed_depths
            .iter()
            .flat_map(|d| &d.visuals)
            .find(|v| v.visual_id == screen.root_visual)
            .map(|v| (v.red_mask, v.green_mask, v.blue_mask));
        let ok = matches!(screen.root_depth, 24 | 32)
            && bpp == Some(32)
            && setup.image_byte_order == ImageOrder::LSB_FIRST
            && masks == Some((0x00ff_0000, 0x0000_ff00, 0x0000_00ff));
        if ok {
            Ok(())
        } else {
            Err(E::Unsupported(format!(
                "桌面的像素格式不受支持（色深 {}，每像素 {bpp:?} 位），需要 24 或 32 位真彩色",
                screen.root_depth
            )))
        }
    }
}

pub(crate) fn displays() -> Result<Vec<DisplayInfo>, PlatformError> {
    X11::connect()?.monitors()
}

pub(crate) fn cursor_position() -> Option<(i32, i32)> {
    let x = X11::connect().ok()?;
    let r = x.conn.query_pointer(x.root).ok()?.reply().ok()?;
    Some((i32::from(r.root_x), i32::from(r.root_y)))
}

pub(crate) fn current_os() -> crate::guide::Os {
    crate::guide::linux_session(std::env::var("XDG_SESSION_TYPE").ok().as_deref())
}

pub(crate) fn perform(action: crate::guide::Action) -> Result<(), String> {
    Err(format!("这个系统不需要这一步：{action:?}"))
}

pub(crate) fn permissions() -> PermissionStatus {
    // X11 没有权限的概念：能连上 X 服务器就能采集和注入。
    PermissionStatus {
        screen_capture: true,
        input_injection: true,
    }
}

// ───────────────────────────── 采集 ─────────────────────────────

/// 共享内存段（memfd + mmap），丢弃时解除映射。
struct Shm {
    seg: shm::Seg,
    ptr: *mut u8,
    len: usize,
}

impl Shm {
    fn new(x: &X11, len: usize) -> Result<Self, PlatformError> {
        let seg = x.conn.generate_id().map_err(lost)?;
        // SAFETY: 参数是以 NUL 结尾的常量字符串；返回的文件描述符立刻交给 OwnedFd 管理。
        let raw = unsafe { libc::memfd_create(c"peerdesk-capture".as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err(E::Os(format!(
                "memfd_create：{}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: `raw` 是刚创建、只属于这里的有效文件描述符。
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let size = libc::off_t::try_from(len).map_err(|_| E::Os("画面太大".into()))?;
        // SAFETY: `fd` 是有效的 memfd。
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size) } != 0 {
            return Err(E::Os(format!(
                "ftruncate：{}",
                std::io::Error::last_os_error()
            )));
        }
        // SAFETY: 映射一个刚设好大小的 memfd，长度与它一致；失败时返回 MAP_FAILED，下面检查。
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(E::Os(format!("mmap：{}", std::io::Error::last_os_error())));
        }
        // 先构造出来：后面出错时由 Drop 解除映射。
        let me = Self {
            seg,
            ptr: ptr.cast(),
            len,
        };
        // 文件描述符交给服务器后在本进程关闭，映射仍然有效。
        x.conn
            .shm_attach_fd(me.seg, fd, false)
            .map_err(lost)?
            .check()
            .map_err(|e| os("ShmAttachFd", e))?;
        Ok(me)
    }

    fn bytes(&self) -> &[u8] {
        // SAFETY: `ptr` 指向长度为 `len` 的有效映射，直到 Drop 才解除；
        // X 服务器只在 ShmGetImage 的应答返回之前写它，读的时候没有并发写入。
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        // SAFETY: `ptr`、`len` 来自成功的 mmap，只解除一次。
        let _ = unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

struct X11Grabber {
    x: X11,
    index: usize,
    mon: DisplayInfo,
    rect: (i16, i16, u16, u16),
    shm: Option<Shm>,
    damage: Option<damage::Damage>,
    /// 显示器范围内有还没读取的变化。
    dirty: bool,
    layout_checked: Instant,
}

pub(crate) fn open_grabber(opts: &CaptureOptions) -> Result<Box<dyn Grabber>, PlatformError> {
    let x = X11::connect()?;
    x.check_format()?;
    let mon = pick_display(x.monitors()?, opts.display)?;
    let rect = (
        i16::try_from(mon.x).map_err(|_| E::Os("显示器位置超出范围".into()))?,
        i16::try_from(mon.y).map_err(|_| E::Os("显示器位置超出范围".into()))?,
        u16::try_from(mon.width).map_err(|_| E::Os("显示器太大".into()))?,
        u16::try_from(mon.height).map_err(|_| E::Os("显示器太大".into()))?,
    );
    let len = usize::from(rect.2) * usize::from(rect.3) * BYTES_PER_PIXEL;
    let shm_ok = x.has_extension(shm::X11_EXTENSION_NAME)
        && x.conn
            .shm_query_version()
            .map_err(lost)?
            .reply()
            .is_ok_and(|v| (v.major_version, v.minor_version) >= (1, 2));
    // 共享内存不可用（例如远程 X 服务器）时退回 GetImage。
    let shm = if shm_ok { Shm::new(&x, len).ok() } else { None };
    // DAMAGE 依赖 XFIXES：两个都初始化过才能用。
    let damage_ok = x.has_extension(xfixes::X11_EXTENSION_NAME)
        && x.has_extension(damage::X11_EXTENSION_NAME)
        && x.conn
            .xfixes_query_version(4, 0)
            .map_err(lost)?
            .reply()
            .is_ok()
        && x.conn
            .damage_query_version(1, 1)
            .map_err(lost)?
            .reply()
            .is_ok();
    let damage = if damage_ok {
        let id = x.conn.generate_id().map_err(lost)?;
        x.conn
            .damage_create(id, x.root, damage::ReportLevel::DELTA_RECTANGLES)
            .map_err(lost)?;
        Some(id)
    } else {
        None
    };
    x.sync()?;
    Ok(Box::new(X11Grabber {
        x,
        index: opts.display,
        mon,
        rect,
        shm,
        damage,
        dirty: true,
        layout_checked: Instant::now(),
    }))
}

fn intersects(r: (i16, i16, u16, u16), area: &xproto::Rectangle) -> bool {
    let (ax, ay) = (i32::from(area.x), i32::from(area.y));
    let (aw, ah) = (i32::from(area.width), i32::from(area.height));
    let (bx, by) = (i32::from(r.0), i32::from(r.1));
    let (bw, bh) = (i32::from(r.2), i32::from(r.3));
    ax < bx + bw && bx < ax + aw && ay < by + bh && by < ay + ah
}

impl X11Grabber {
    /// 处理已经到达的事件，不阻塞。
    fn drain(&mut self) -> Result<(), PlatformError> {
        while let Some(event) = self.x.conn.poll_for_event().map_err(lost)? {
            if let Event::DamageNotify(e) = event {
                if intersects(self.rect, &e.area) {
                    self.dirty = true;
                }
            }
        }
        Ok(())
    }
}

impl Grabber for X11Grabber {
    fn wait(&mut self, timeout: Duration) -> Result<bool, PlatformError> {
        if self.layout_checked.elapsed() > LAYOUT_CHECK {
            self.layout_checked = Instant::now();
            let now = pick_display(self.x.monitors()?, self.index)?;
            if now != self.mon {
                return Err(E::Lost("显示器布局或分辨率变了".into()));
            }
        }
        if self.damage.is_none() {
            // 没有 DAMAGE：按帧率读取，由采集线程比较内容。
            return Ok(true);
        }
        self.drain()?;
        if !self.dirty {
            self.x.wait_readable(timeout)?;
            self.drain()?;
        }
        Ok(self.dirty)
    }

    fn grab(&mut self, spare: Option<Screen>) -> Result<Screen, PlatformError> {
        // 先清掉累积的变化再读：读的过程中发生的变化会产生新的通知，不会漏掉。
        if let Some(d) = self.damage {
            self.x
                .conn
                .damage_subtract(d, x11rb::NONE, x11rb::NONE)
                .map_err(lost)?;
        }
        self.dirty = false;
        let (x, y, w, h) = self.rect;
        let (width, height) = (usize::from(w), usize::from(h));
        let stride = width * BYTES_PER_PIXEL;
        let format = u8::from(ImageFormat::Z_PIXMAP);
        match &self.shm {
            Some(s) => {
                self.x
                    .conn
                    .shm_get_image(self.x.root, x, y, w, h, !0, format, s.seg, 0)
                    .map_err(lost)?
                    .reply()
                    .map_err(|e| os("ShmGetImage", e))?;
                bgra_to_screen(spare, width, height, s.bytes(), stride)
            }
            None => {
                let reply = self
                    .x
                    .conn
                    .get_image(ImageFormat::Z_PIXMAP, self.x.root, x, y, w, h, !0)
                    .map_err(lost)?
                    .reply()
                    .map_err(|e| os("GetImage", e))?;
                bgra_to_screen(spare, width, height, &reply.data, stride)
            }
        }
    }
}

// ───────────────────────────── 注入 ─────────────────────────────

struct Conn {
    x: X11,
    /// 为了输入文字临时绑定到空闲键码上的字符：键码 → keysym。
    bound: HashMap<u8, u32>,
    /// 下一个用来临时绑定的空闲键码的位置（轮流使用）。
    next_spare: usize,
}

impl Conn {
    fn open() -> Result<Self, PlatformError> {
        let x = X11::connect()?;
        if !x.has_extension(xtest::X11_EXTENSION_NAME) {
            return Err(E::Unsupported(
                "X 服务器不支持 XTEST 扩展，无法注入键鼠".into(),
            ));
        }
        x.conn
            .xtest_get_version(2, 2)
            .map_err(lost)?
            .reply()
            .map_err(|e| os("XTestGetVersion", e))?;
        Ok(Self {
            x,
            bound: HashMap::new(),
            next_spare: 0,
        })
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), PlatformError> {
        self.x
            .conn
            .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.x.root, x, y, 0)
            .map_err(lost)?;
        Ok(())
    }

    fn key(&self, kc: u8, down: bool) -> Result<(), PlatformError> {
        let kind = if down {
            xproto::KEY_PRESS_EVENT
        } else {
            xproto::KEY_RELEASE_EVENT
        };
        self.fake(kind, kc, 0, 0)
    }

    fn button(&self, b: u8, down: bool) -> Result<(), PlatformError> {
        let kind = if down {
            xproto::BUTTON_PRESS_EVENT
        } else {
            xproto::BUTTON_RELEASE_EVENT
        };
        self.fake(kind, b, 0, 0)
    }

    fn tap(&self, kc: u8, shift: Option<u8>) -> Result<(), PlatformError> {
        if let Some(s) = shift {
            self.key(s, true)?;
        }
        self.key(kc, true)?;
        self.key(kc, false)?;
        if let Some(s) = shift {
            self.key(s, false)?;
        }
        Ok(())
    }

    /// 输入一段文字：当前键盘映射里有的字符直接按对应的键（需要时加 Shift），
    /// 没有的（中文、特殊符号）临时绑定到一个空闲键码上再按，连接关闭时恢复。
    fn type_text(&mut self, text: &str) -> Result<(), PlatformError> {
        let setup = self.x.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let map = self
            .x
            .conn
            .get_keyboard_mapping(min, max.saturating_sub(min).saturating_add(1))
            .map_err(lost)?
            .reply()
            .map_err(|e| os("GetKeyboardMapping", e))?;
        let per = usize::from(map.keysyms_per_keycode);
        let sym = |kc: u8, col: usize| -> u32 {
            if col >= per {
                return 0;
            }
            let i = usize::from(kc - min) * per + col;
            map.keysyms.get(i).copied().unwrap_or(0)
        };
        // 被别人改掉的临时绑定作废
        self.bound.retain(|&kc, ks| sym(kc, 0) == *ks);
        let mut existing: HashMap<u32, (u8, bool)> = HashMap::new();
        for col in [0, 1] {
            for kc in (min..=max).filter(|kc| !self.bound.contains_key(kc)) {
                let ks = sym(kc, col);
                if ks != 0 {
                    existing.entry(ks).or_insert((kc, col == 1));
                }
            }
        }
        let shift = existing
            .get(&XK_SHIFT_L)
            .filter(|(_, shifted)| !shifted)
            .map(|(kc, _)| *kc);
        let spare: Vec<u8> = (min..=max)
            .filter(|&kc| self.bound.contains_key(&kc) || (0..per).all(|c| sym(kc, c) == 0))
            .collect();
        for ch in text.chars() {
            let Some(ks) = x11_keysym(ch) else { continue };
            match existing.get(&ks) {
                Some(&(kc, false)) => self.tap(kc, None)?,
                Some(&(kc, true)) if shift.is_some() => self.tap(kc, shift)?,
                Some(_) | None => {
                    let found = self.bound.iter().find(|(_, s)| **s == ks).map(|(k, _)| *k);
                    let kc = match found {
                        Some(kc) => kc,
                        None => {
                            let Some(kc) = spare.get(self.next_spare % spare.len().max(1)) else {
                                continue;
                            };
                            self.next_spare = self.next_spare.wrapping_add(1);
                            self.x
                                .conn
                                .change_keyboard_mapping(1, *kc, 2, &[ks, ks])
                                .map_err(lost)?;
                            self.bound.insert(*kc, ks);
                            // 让各个程序先处理 MappingNotify（刷新键盘映射），再收到按键
                            self.x.sync()?;
                            std::thread::sleep(REMAP_SETTLE);
                            *kc
                        }
                    };
                    self.tap(kc, None)?;
                    // 下一个字符可能要复用这个键码：先让程序读到这次按键
                    self.x.sync()?;
                }
            }
        }
        Ok(())
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // 恢复临时绑定的键码；等服务器处理完再断开。
        for &kc in self.bound.keys() {
            let _ = self.x.conn.change_keyboard_mapping(1, kc, 2, &[0, 0]);
        }
        let _ = self.x.sync();
    }
}

struct X11Input {
    conn: Option<Conn>,
    index: usize,
    display: DisplayInfo,
    refreshed: Instant,
    /// 按下的键（X11 键码）和鼠标按钮。
    keys: HashSet<u8>,
    buttons: HashSet<u8>,
    wheel: (WheelAccumulator, WheelAccumulator),
}

pub(crate) fn open_input(display: usize) -> Result<Box<dyn InputSink>, PlatformError> {
    let conn = Conn::open()?;
    let geometry = pick_display(conn.x.monitors()?, display)?;
    Ok(Box::new(X11Input {
        conn: Some(conn),
        index: display,
        display: geometry,
        refreshed: Instant::now(),
        keys: HashSet::new(),
        buttons: HashSet::new(),
        wheel: (
            WheelAccumulator::new(WHEEL_NOTCH),
            WheelAccumulator::new(WHEEL_NOTCH),
        ),
    }))
}

impl X11Input {
    fn apply(&mut self, event: &InputEvent) -> Result<(), PlatformError> {
        if self.conn.is_none() {
            self.conn = Some(Conn::open()?);
        }
        let Some(c) = self.conn.as_mut() else {
            return Ok(());
        };
        // 不读的话 MappingNotify 之类的事件会一直堆在连接里
        while c.x.conn.poll_for_event().map_err(lost)?.is_some() {}
        match event {
            InputEvent::MouseMove { x, y } => {
                if self.refreshed.elapsed() > GEOMETRY_REFRESH {
                    self.refreshed = Instant::now();
                    if let Ok(d) = c.x.monitors().and_then(|l| pick_display(l, self.index)) {
                        self.display = d;
                    }
                }
                let d = &self.display;
                let px = to_pixel(*x, d.x, d.width).clamp(0, i32::from(i16::MAX));
                let py = to_pixel(*y, d.y, d.height).clamp(0, i32::from(i16::MAX));
                let (px, py) = (
                    i16::try_from(px).unwrap_or(0),
                    i16::try_from(py).unwrap_or(0),
                );
                c.fake(xproto::MOTION_NOTIFY_EVENT, 0, px, py)?;
            }
            // XTEST 的移动事件 detail = 1 表示相对移动
            InputEvent::MouseRelative { dx, dy } => {
                c.fake(xproto::MOTION_NOTIFY_EVENT, 1, *dx, *dy)?;
            }
            InputEvent::MouseButton { button, down } => {
                let Some(b) = x11_button(*button) else {
                    return Ok(());
                };
                if *down {
                    self.buttons.insert(b);
                } else if !self.buttons.remove(&b) {
                    // 没按下过的键不发送抬起，避免干扰本地用户
                    return Ok(());
                }
                c.button(b, *down)?;
            }
            InputEvent::Wheel { dx, dy } => {
                // X11 只有“一格”的滚轮按钮（4/5 纵向、6/7 横向），小幅滚动先累积
                let steps = (self.wheel.1.push(*dy), self.wheel.0.push(*dx));
                for (b, count) in x11_wheel_buttons(steps.0, steps.1) {
                    for _ in 0..count {
                        c.button(b, true)?;
                        c.button(b, false)?;
                    }
                }
            }
            InputEvent::Key { code, down } => {
                let Some(kc) = to_x11(*code) else {
                    return Ok(());
                };
                if *down {
                    if !self.keys.insert(kc) {
                        // 已经按着：自动重复由 X 服务器负责
                        return Ok(());
                    }
                } else if !self.keys.remove(&kc) {
                    return Ok(());
                }
                c.key(kc, *down)?;
            }
            InputEvent::Text(text) => c.type_text(text)?,
            InputEvent::ReleaseAll => {
                for kc in self.keys.drain() {
                    c.key(kc, false)?;
                }
                for b in self.buttons.drain() {
                    c.button(b, false)?;
                }
                self.wheel.0.reset();
                self.wheel.1.reset();
            }
        }
        c.x.conn.flush().map_err(lost)?;
        Ok(())
    }
}

impl InputSink for X11Input {
    fn inject(&mut self, event: &InputEvent) {
        // 注入失败（X 服务器重启、连接断开）：丢掉这个事件，下一个事件重新连接。
        if self.apply(event).is_err() {
            self.conn = None;
        }
    }
}
