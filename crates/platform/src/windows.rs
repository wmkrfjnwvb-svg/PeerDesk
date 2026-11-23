//! Windows 的系统胶水：DXGI 桌面复制采集（还没有画面时用 GDI）、`SendInput` 注入、每显示器 DPI 感知。
//!
//! * 进程设为“每显示器 DPI 感知（V2）”：DXGI 给的是物理像素，DPI 未感知的进程调用 `GetSystemMetrics`、
//!   `GetCursorPos` 拿到的却是按缩放比例换算过的坐标，两边对不上，鼠标会落到错误的位置；
//! * 画面变化由 DXGI 告诉我们：`AcquireNextFrame` 等到新的一帧才返回，`LastPresentTime` 为 0
//!   表示只有鼠标动了（画面里不含光标），这时不读画面；
//! * 刚打开时 DXGI 可能还没有任何已呈现的画面（静止的桌面上要等到有东西重画），第一帧用 GDI 读；
//! * `DXGI_ERROR_ACCESS_LOST`（切换到安全桌面、改分辨率、全屏程序切换模式）时报告采集中断，
//!   由采集线程重新打开。
//!
//! 部分代码改编自 Airhand（MIT 许可，见仓库根目录的 THIRD_PARTY_NOTICES.md）：
//! `client/src-tauri/src/host/capture/dxgi.rs`（输出枚举、主显示器判断、设备创建、桌面复制）、
//! `client/src-tauri/src/host/input/windows.rs`（`SendInput` 的事件构造）、
//! `client/src-tauri/src/host/mod.rs`（DPI 感知）。

#![allow(unsafe_code)]

use std::collections::HashSet;
use std::sync::Once;
use std::time::{Duration, Instant};

use peerdesk_core::input::InputEvent;
use peerdesk_core::screen::Screen;
use peerdesk_net::host::InputSink;
use windows::core::Interface;
use windows::Win32::Foundation::{HMODULE, POINT, RECT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_10_0, D3D_FEATURE_LEVEL_10_1,
    D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetMonitorInfoW, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT,
    DIB_RGB_COLORS, HMONITOR, MONITORINFO, SRCCOPY,
};
use windows::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SetProcessDPIAware, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
    SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::capture::Grabber;
use crate::keymap::{to_windows, WinKey};
use crate::pixel::{bgra_to_screen, screen_size, BYTES_PER_PIXEL};
use crate::pointer::{to_pixel, to_virtual_desk};
use crate::{pick_display, CaptureOptions, DisplayInfo, PermissionStatus};
use crate::{PlatformError, PlatformError as E};

/// 注入时用的显示器位置多久刷新一次。
const GEOMETRY_REFRESH: Duration = Duration::from_secs(2);
/// `MONITORINFO::dwFlags` 里表示主显示器的位。
const MONITORINFOF_PRIMARY: u32 = 1;
/// 侧键的编号（`XBUTTON1`、`XBUTTON2`）。
const XBUTTON1: u32 = 1;
const XBUTTON2: u32 = 2;

fn os(what: &str, e: &windows::core::Error) -> PlatformError {
    E::Os(format!("{what}：{e}"))
}

/// 把整个进程设为每显示器 DPI 感知（只做一次）。程序清单里已经声明过时系统会拒绝，不影响结果。
fn ensure_dpi_aware() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: 只修改本进程的 DPI 感知设置，参数是系统定义的常量。
        let v2 =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if v2.is_err() {
            // 早于 Windows 10 1703 的系统没有 V2：退一步，至少是系统 DPI 感知。
            // SAFETY: 无参数的系统调用。
            let _ = unsafe { SetProcessDPIAware() };
        }
    });
}

/// 一个接在桌面上的显示器输出。
struct Output {
    info: DisplayInfo,
    adapter: IDXGIAdapter1,
    output: IDXGIOutput1,
}

fn monitor_primary(monitor: HMONITOR) -> Option<bool> {
    let mut mi = MONITORINFO {
        cbSize: u32::try_from(size_of::<MONITORINFO>()).unwrap_or(0),
        ..Default::default()
    };
    // SAFETY: `mi` 是有效、可写的 MONITORINFO，cbSize 已按要求设置。
    let ok = unsafe { GetMonitorInfoW(monitor, &mut mi) };
    ok.as_bool()
        .then_some(mi.dwFlags & MONITORINFOF_PRIMARY != 0)
}

fn monitor_scale(monitor: HMONITOR) -> f64 {
    let (mut dx, mut dy) = (0u32, 0u32);
    // SAFETY: 两个输出参数都是有效、可写的 u32。
    let ok = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
    if ok.is_ok() && dx > 0 {
        f64::from(dx) / 96.0
    } else {
        1.0
    }
}

/// 列出所有接在桌面上的输出，排序与 [`crate::sort_displays`] 一致：主显示器第一，其余从左到右。
fn outputs() -> Result<Vec<Output>, PlatformError> {
    ensure_dpi_aware();
    let mut list = Vec::new();
    // SAFETY: 标准的 DXGI 工厂创建；返回的 COM 接口由 windows crate 管理引用计数。
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|e| os("CreateDXGIFactory1", &e))?;
    let mut ai = 0;
    // SAFETY: 依次枚举适配器和输出，越界时返回错误结束循环。
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(ai) } {
        ai += 1;
        let mut oi = 0;
        // SAFETY: 同上。
        while let Ok(output) = unsafe { adapter.EnumOutputs(oi) } {
            oi += 1;
            // SAFETY: `output` 是有效的输出接口。
            let Ok(desc) = (unsafe { output.GetDesc() }) else {
                continue;
            };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let Ok(output) = output.cast::<IDXGIOutput1>() else {
                continue;
            };
            let r = desc.DesktopCoordinates;
            let (Ok(width), Ok(height)) = (
                u32::try_from(r.right - r.left),
                u32::try_from(r.bottom - r.top),
            ) else {
                continue;
            };
            let name = String::from_utf16_lossy(&desc.DeviceName)
                .trim_end_matches('\0')
                .trim_start_matches(r"\\.\")
                .to_owned();
            let primary = monitor_primary(desc.Monitor).unwrap_or(r.left == 0 && r.top == 0);
            list.push(Output {
                info: DisplayInfo {
                    index: 0,
                    name,
                    x: r.left,
                    y: r.top,
                    width,
                    height,
                    scale: monitor_scale(desc.Monitor),
                    primary,
                },
                adapter: adapter.clone(),
                output,
            });
        }
    }
    list.sort_by_key(|o| (!o.info.primary, o.info.x, o.info.y));
    let has_primary = list.iter().any(|o| o.info.primary);
    for (i, o) in list.iter_mut().enumerate() {
        o.info.index = i;
        o.info.primary |= i == 0 && !has_primary;
    }
    Ok(list)
}

pub(crate) fn displays() -> Result<Vec<DisplayInfo>, PlatformError> {
    Ok(outputs()?.into_iter().map(|o| o.info).collect())
}

pub(crate) fn cursor_position() -> Option<(i32, i32)> {
    ensure_dpi_aware();
    let mut p = POINT::default();
    // SAFETY: `p` 是有效、可写的 POINT。
    unsafe { GetCursorPos(&mut p) }.ok()?;
    Some((p.x, p.y))
}

pub(crate) fn current_os() -> crate::guide::Os {
    crate::guide::Os::Windows
}

pub(crate) fn perform(action: crate::guide::Action) -> Result<(), String> {
    Err(format!("这个系统不需要这一步：{action:?}"))
}

pub(crate) fn permissions() -> PermissionStatus {
    // Windows 不需要用户授权。（注入到以管理员身份运行的窗口需要本进程也以管理员身份运行。）
    PermissionStatus {
        screen_capture: true,
        input_injection: true,
    }
}

// ───────────────────────────── 采集 ─────────────────────────────

/// 下一次 `grab` 从哪里读。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// 没有新画面。
    Nothing,
    /// DXGI 的画面已经复制进 staging 纹理（宽、高）。
    Staged(u32, u32),
    /// DXGI 还没有呈现过画面：用 GDI 读第一帧。
    Gdi,
}

struct DxgiGrabber {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: Option<(ID3D11Texture2D, u32, u32)>,
    pending: Pending,
    /// 还没有交出过任何一帧。
    first: bool,
    /// 显示器在桌面上的范围（物理像素），GDI 用。
    rect: RECT,
}

fn create_device(
    adapter: &IDXGIAdapter1,
) -> Result<(ID3D11Device, ID3D11DeviceContext), PlatformError> {
    let levels: [D3D_FEATURE_LEVEL; 4] = [
        D3D_FEATURE_LEVEL_11_1,
        D3D_FEATURE_LEVEL_11_0,
        D3D_FEATURE_LEVEL_10_1,
        D3D_FEATURE_LEVEL_10_0,
    ];
    let mut device: Option<ID3D11Device> = None;
    let mut ctx: Option<ID3D11DeviceContext> = None;
    // SAFETY: 输出参数指向本函数里的局部变量；指定了适配器时驱动类型必须是 UNKNOWN。
    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut ctx),
        )
    }
    .map_err(|e| os("D3D11CreateDevice", &e))?;
    match (device, ctx) {
        (Some(d), Some(c)) => Ok((d, c)),
        (Some(_) | None, Some(_) | None) => Err(E::Os("D3D11CreateDevice 没有返回设备".into())),
    }
}

pub(crate) fn open_grabber(opts: &CaptureOptions) -> Result<Box<dyn Grabber>, PlatformError> {
    // 代理模式：先切到当前输入桌面（锁屏、登录界面时是 Winlogon），桌面复制才能打开。
    // 切换桌面会让旧的复制报 ACCESS_LOST，采集线程随后重新打开，又走到这里。
    crate::winsession::follow_input_desktop();
    let list = outputs()?;
    let count = list.len();
    let o = list.into_iter().nth(opts.display).ok_or(E::NoDisplay {
        index: opts.display,
        count,
    })?;
    let (device, ctx) = create_device(&o.adapter)?;
    // SAFETY: `device` 是刚在这个输出所属的适配器上创建的设备。
    let dup = unsafe { o.output.DuplicateOutput(&device) }.map_err(|e| {
        if e.code() == windows::Win32::Foundation::E_ACCESSDENIED {
            E::Lost("现在无法复制桌面（可能处于锁屏或 UAC 安全桌面）".into())
        } else {
            os("DuplicateOutput", &e)
        }
    })?;
    let i = &o.info;
    let rect = RECT {
        left: i.x,
        top: i.y,
        right: i
            .x
            .saturating_add(i32::try_from(i.width).unwrap_or(i32::MAX)),
        bottom: i
            .y
            .saturating_add(i32::try_from(i.height).unwrap_or(i32::MAX)),
    };
    Ok(Box::new(DxgiGrabber {
        device,
        ctx,
        dup,
        staging: None,
        pending: Pending::Nothing,
        first: true,
        rect,
    }))
}

impl DxgiGrabber {
    fn staging(&mut self, w: u32, h: u32) -> Result<ID3D11Texture2D, PlatformError> {
        if let Some((t, sw, sh)) = &self.staging {
            if (*sw, *sh) == (w, h) {
                return Ok(t.clone());
            }
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: u32::try_from(D3D11_CPU_ACCESS_READ.0).unwrap_or(0),
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        // SAFETY: `desc` 是完整填写的纹理描述，输出参数指向局部变量。
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex)) }
            .map_err(|e| os("CreateTexture2D", &e))?;
        let tex = tex.ok_or_else(|| E::Os("CreateTexture2D 没有返回纹理".into()))?;
        self.staging = Some((tex.clone(), w, h));
        Ok(tex)
    }

    /// 把桌面复制交出来的纹理复制进 staging 纹理。
    fn stage(&mut self, resource: IDXGIResource) -> Result<Pending, PlatformError> {
        let tex: ID3D11Texture2D = resource
            .cast()
            .map_err(|e| os("IDXGIResource → ID3D11Texture2D", &e))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: `desc` 是有效、可写的纹理描述。
        unsafe { tex.GetDesc(&mut desc) };
        if desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM {
            return Err(E::Unsupported(format!(
                "桌面的像素格式不受支持：{:?}",
                desc.Format
            )));
        }
        let staging = self.staging(desc.Width, desc.Height)?;
        // SAFETY: 两个纹理尺寸、格式相同，都属于同一个设备。
        unsafe { self.ctx.CopyResource(&staging, &tex) };
        Ok(Pending::Staged(desc.Width, desc.Height))
    }

    /// 读 staging 纹理里的画面。
    fn read_staged(
        &mut self,
        w: u32,
        h: u32,
        spare: Option<Screen>,
    ) -> Result<Screen, PlatformError> {
        let (tex, _, _) = self
            .staging
            .clone()
            .ok_or_else(|| E::Os("没有复制好的画面".into()))?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: staging 纹理允许 CPU 读取，输出参数指向局部变量。
        unsafe { self.ctx.Map(&tex, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
            .map_err(|e| os("Map", &e))?;
        let (width, height) = (
            usize::try_from(w).unwrap_or(0),
            usize::try_from(h).unwrap_or(0),
        );
        let pitch = usize::try_from(mapped.RowPitch).unwrap_or(0);
        let result = if mapped.pData.is_null() {
            Err(E::Os("Map 没有返回数据".into()))
        } else {
            // SAFETY: Map 成功后 `pData` 指向至少 RowPitch * 高度 字节的可读内存，直到 Unmap。
            let data = unsafe {
                std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch.saturating_mul(height))
            };
            bgra_to_screen(spare, width, height, data, pitch)
        };
        // SAFETY: 与上面成功的 Map 配对。
        unsafe { self.ctx.Unmap(&tex, 0) };
        result
    }

    /// 用 GDI 读整个显示器（DXGI 还没有呈现过画面时）。
    fn read_gdi(&self, spare: Option<Screen>) -> Result<Screen, PlatformError> {
        let r = self.rect;
        let (w, h) = (r.right - r.left, r.bottom - r.top);
        let (width, height) = (
            usize::try_from(w).unwrap_or(0),
            usize::try_from(h).unwrap_or(0),
        );
        screen_size(width, height)?;
        let mut buf = vec![0u8; width * height * BYTES_PER_PIXEL];
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).unwrap_or(0),
                biWidth: w,
                // 负数表示自上而下的行序，与 Screen 一致
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // SAFETY: 标准的 GDI 截屏流程：屏幕 DC → 兼容位图 → BitBlt → GetDIBits，
        // 每个创建出来的对象都在本块末尾释放；`buf` 的大小正好是 宽 × 高 × 4 字节。
        let lines = unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(Some(screen));
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            let old = SelectObject(mem, bitmap.into());
            let blit = BitBlt(
                mem,
                0,
                0,
                w,
                h,
                Some(screen),
                r.left,
                r.top,
                SRCCOPY | CAPTUREBLT,
            );
            let lines = if blit.is_ok() {
                GetDIBits(
                    mem,
                    bitmap,
                    0,
                    u32::try_from(h).unwrap_or(0),
                    Some(buf.as_mut_ptr().cast()),
                    &mut bmi,
                    DIB_RGB_COLORS,
                )
            } else {
                0
            };
            SelectObject(mem, old);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            lines
        };
        if lines != h {
            return Err(E::Os("GDI 读取屏幕失败".into()));
        }
        bgra_to_screen(spare, width, height, &buf, width * BYTES_PER_PIXEL)
    }
}

impl Grabber for DxgiGrabber {
    fn wait(&mut self, timeout: Duration) -> Result<bool, PlatformError> {
        if self.pending != Pending::Nothing {
            return Ok(true);
        }
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        // 第一帧不等太久：DXGI 没有现成的画面就改用 GDI
        let ms = if self.first { 50 } else { timeout.as_millis() };
        let ms = u32::try_from(ms).unwrap_or(u32::MAX);
        // SAFETY: 两个输出参数都指向局部变量；成功后必须 ReleaseFrame（见下面）。
        let acquired = unsafe { self.dup.AcquireNextFrame(ms, &mut info, &mut resource) };
        if let Err(e) = acquired {
            if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                if self.first {
                    self.pending = Pending::Gdi;
                }
                return Ok(self.first);
            }
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                return Err(E::Lost("桌面复制中断（切换了桌面或显示模式）".into()));
            }
            return Err(os("AcquireNextFrame", &e));
        }
        // 只有鼠标动了时 LastPresentTime 为 0：画面没变（画面里不含光标）。
        let presented = info.LastPresentTime != 0 && info.AccumulatedFrames > 0;
        let staged = match resource.take() {
            Some(r) if presented => self.stage(r).map(Some),
            Some(_) | None => Ok(None),
        };
        // 尽早释放，让桌面窗口管理器继续合成。
        // SAFETY: 与上面成功的 AcquireNextFrame 配对。
        let released = unsafe { self.dup.ReleaseFrame() };
        if let Err(e) = released {
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                return Err(E::Lost("桌面复制中断（切换了桌面或显示模式）".into()));
            }
        }
        self.pending = match staged? {
            Some(p) => p,
            None if self.first => Pending::Gdi,
            None => Pending::Nothing,
        };
        Ok(self.pending != Pending::Nothing)
    }

    fn grab(&mut self, spare: Option<Screen>) -> Result<Screen, PlatformError> {
        let pending = std::mem::replace(&mut self.pending, Pending::Nothing);
        let screen = match pending {
            Pending::Staged(w, h) => self.read_staged(w, h, spare)?,
            Pending::Gdi => self.read_gdi(spare)?,
            Pending::Nothing => return Err(E::Os("没有可读的画面".into())),
        };
        self.first = false;
        Ok(screen)
    }
}

// ───────────────────────────── 注入 ─────────────────────────────

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: u32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn keyboard(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key_input(key: WinKey, down: bool) -> INPUT {
    let up = if down {
        KEYBD_EVENT_FLAGS(0)
    } else {
        KEYEVENTF_KEYUP
    };
    match key {
        WinKey::Scan { code, extended } => {
            let ext = if extended {
                KEYEVENTF_EXTENDEDKEY
            } else {
                KEYBD_EVENT_FLAGS(0)
            };
            keyboard(0, code, KEYEVENTF_SCANCODE | ext | up)
        }
        WinKey::Virtual(vk) => keyboard(vk, 0, up),
    }
}

/// 鼠标按键 → `SendInput` 的标志和附加数据；不认识的按键为 `None`。
fn button_input(button: u8, down: bool) -> Option<INPUT> {
    let (flags, data) = match (button, down) {
        (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (0, false) => (MOUSEEVENTF_LEFTUP, 0),
        (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (3, true) => (MOUSEEVENTF_XDOWN, XBUTTON1),
        (3, false) => (MOUSEEVENTF_XUP, XBUTTON1),
        (4, true) => (MOUSEEVENTF_XDOWN, XBUTTON2),
        (4, false) => (MOUSEEVENTF_XUP, XBUTTON2),
        _ => return None,
    };
    Some(mouse(flags, 0, 0, data))
}

/// 滚轮增量（WHEEL_DELTA 为单位，有符号）放进 `mouseData`（按位解释的 u32）。
fn wheel_data(delta: i16) -> u32 {
    u32::from_ne_bytes(i32::from(delta).to_ne_bytes())
}

struct WinInput {
    index: usize,
    display: DisplayInfo,
    refreshed: Instant,
    keys: HashSet<WinKey>,
    buttons: HashSet<u8>,
    batch: Vec<INPUT>,
}

pub(crate) fn open_input(display: usize) -> Result<Box<dyn InputSink>, PlatformError> {
    let geometry = pick_display(displays()?, display)?;
    Ok(Box::new(WinInput {
        index: display,
        display: geometry,
        refreshed: Instant::now(),
        keys: HashSet::new(),
        buttons: HashSet::new(),
        batch: Vec::new(),
    }))
}

impl WinInput {
    fn move_to(&mut self, x: u16, y: u16) {
        if self.refreshed.elapsed() > GEOMETRY_REFRESH {
            self.refreshed = Instant::now();
            if let Ok(d) = displays().and_then(|l| pick_display(l, self.index)) {
                self.display = d;
            }
        }
        let d = &self.display;
        let (px, py) = (to_pixel(x, d.x, d.width), to_pixel(y, d.y, d.height));
        // SAFETY: 只读取系统指标。进程是 DPI 感知的，得到的是物理像素，与 DXGI 一致。
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let (nx, ny) = (to_virtual_desk(px, vx, vw), to_virtual_desk(py, vy, vh));
        self.batch.push(mouse(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            nx,
            ny,
            0,
        ));
    }

    fn push_release_all(&mut self) {
        for key in std::mem::take(&mut self.keys) {
            self.batch.push(key_input(key, false));
        }
        for b in std::mem::take(&mut self.buttons) {
            self.batch.extend(button_input(b, false));
        }
    }

    fn flush(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        // 代理模式：输入要送到当前输入桌面（锁屏时是 Winlogon 桌面），否则被丢掉。
        crate::winsession::follow_input_desktop();
        let size = i32::try_from(size_of::<INPUT>()).unwrap_or(0);
        // SAFETY: `batch` 里是完整构造的 INPUT 结构，`size` 是它的大小。
        // 返回值是成功注入的个数；被 UIPI 拦下（目标窗口权限更高）时没有办法补救，只能丢弃。
        let _ = unsafe { SendInput(&self.batch, size) };
        self.batch.clear();
    }
}

impl InputSink for WinInput {
    fn inject(&mut self, event: &InputEvent) {
        match event {
            InputEvent::MouseMove { x, y } => self.move_to(*x, *y),
            // 相对移动：和真鼠标一样，受系统的指针速度和加速设置影响
            InputEvent::MouseRelative { dx, dy } => {
                self.batch
                    .push(mouse(MOUSEEVENTF_MOVE, i32::from(*dx), i32::from(*dy), 0));
            }
            InputEvent::MouseButton { button, down } => {
                if *down {
                    self.buttons.insert(*button);
                } else if !self.buttons.remove(button) {
                    // 没按下过的键不发送抬起，避免干扰本地用户
                    return;
                }
                self.batch.extend(button_input(*button, *down));
            }
            InputEvent::Wheel { dx, dy } => {
                if *dy != 0 {
                    self.batch
                        .push(mouse(MOUSEEVENTF_WHEEL, 0, 0, wheel_data(*dy)));
                }
                if *dx != 0 {
                    self.batch
                        .push(mouse(MOUSEEVENTF_HWHEEL, 0, 0, wheel_data(*dx)));
                }
            }
            InputEvent::Key { code, down } => {
                let Some(key) = to_windows(*code) else {
                    return;
                };
                if *down {
                    self.keys.insert(key);
                } else if !self.keys.remove(&key) {
                    return;
                }
                self.batch.push(key_input(key, *down));
            }
            InputEvent::Text(text) => {
                for unit in text.encode_utf16() {
                    self.batch.push(keyboard(0, unit, KEYEVENTF_UNICODE));
                    self.batch
                        .push(keyboard(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
            }
            InputEvent::ReleaseAll => self.push_release_all(),
        }
        self.flush();
    }
}
