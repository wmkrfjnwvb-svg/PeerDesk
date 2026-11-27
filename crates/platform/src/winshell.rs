//! Windows 桌面外壳的胶水：开机自启的注册表项、主窗口的显示和退出、首次关闭的提示框，
//! 以及实机测试用的窗口、子进程探针。
//!
//! 为什么主窗口的显示和退出要走窗口消息，而不是 eframe 的 `ViewportCommand`：
//! 窗口被隐藏（`ShowWindow(SW_HIDE)`）后，Windows 不再给它发 `WM_PAINT`，winit 的 `RedrawRequested`
//! 也就不来，eframe 的 `update` 不再运行，界面线程里排队的“显示窗口”“关闭”命令没人处理
//! （`winit` 的 `request_redraw` 就是 `RedrawWindow(.., RDW_INTERNALPAINT)`，对隐藏窗口不产生绘制消息）。
//! 所以托盘的“打开”“退出”必须不依赖 `update`：给主窗口发一个自定义消息，窗口过程在界面线程里直接
//! 调 `ShowWindow` / 退出回调。消息可以从任意线程投递（托盘回调、第二次启动的监听线程）。
//! 窗口过程用 `SetWindowSubclass` 挂在 winit 创建的窗口上，不改 winit。

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::OnceLock;

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, EnumWindows, GetClassNameW, GetWindowLongW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, MessageBoxW, PostMessageW,
    SetForegroundWindow, ShowWindow, ASFW_ANY, GWL_EXSTYLE, MB_ICONINFORMATION, MB_OK,
    MB_SETFOREGROUND, MB_TOPMOST, SW_RESTORE, SW_SHOW, WM_APP, WM_CLOSE, WM_NCDESTROY,
    WS_EX_TOOLWINDOW,
};

/// 开机自启放在当前用户的这个注册表项下（不需要管理员）。
const RUN_SUBKEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// 显示主窗口的自定义消息。
const WM_SHOW_MAIN: u32 = WM_APP + 1;
/// 退出程序的自定义消息。
const WM_QUIT_MAIN: u32 = WM_APP + 2;
const SUBCLASS_ID: usize = 0x5045_4552;

/// 主窗口句柄（没有安装时是 0）。
static MAIN_WINDOW: AtomicIsize = AtomicIsize::new(0);
/// 收到退出消息时在界面线程里调用。
static ON_QUIT: OnceLock<fn()> = OnceLock::new();

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn registry_error(what: &str, code: windows::Win32::Foundation::WIN32_ERROR) -> String {
    format!("{what}失败（错误码 {}）", code.0)
}

/// 在当前用户的 `Run` 项下写一个字符串值（开机自启）。直接用注册表接口：
/// 不再运行 `reg.exe`（控制台程序，从没有控制台的图形界面进程里启动会闪一个控制台窗口）。
pub fn set_run_value(name: &str, command: &str) -> Result<(), String> {
    let (subkey, name, data) = (wide(RUN_SUBKEY), wide(name), wide(command));
    let bytes: Vec<u8> = data.iter().flat_map(|c| c.to_le_bytes()).collect();
    let mut key = HKEY::default();
    // SAFETY: 指针都指向本函数里活着的、以 0 结尾的缓冲区；`key` 在用完后关闭。
    unsafe {
        let code = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        );
        if code.is_err() {
            return Err(registry_error("打开注册表的开机启动项", code));
        }
        let code = RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&bytes));
        let _ = RegCloseKey(key);
        if code.is_err() {
            return Err(registry_error("写注册表的开机启动项", code));
        }
    }
    Ok(())
}

/// 删掉 `Run` 项下的值。值本来就不存在不算错。
pub fn delete_run_value(name: &str) -> Result<(), String> {
    let (subkey, name) = (wide(RUN_SUBKEY), wide(name));
    let mut key = HKEY::default();
    // SAFETY: 同 `set_run_value`。
    unsafe {
        let code = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        );
        if code.is_err() {
            return Err(registry_error("打开注册表的开机启动项", code));
        }
        let code = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
        let _ = RegCloseKey(key);
        // ERROR_FILE_NOT_FOUND（2）：本来就没有
        if code.is_err() && code.0 != 2 {
            return Err(registry_error("删除注册表的开机启动项", code));
        }
    }
    Ok(())
}

fn hwnd_of(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

/// 把窗口显示出来并放到最前：隐藏的窗口先显示（回到任务栏），最小化的先还原。
fn show_now(hwnd: HWND) {
    // SAFETY: 句柄是本进程的窗口；这些调用对无效句柄只会失败，不会出错。
    unsafe {
        let _ = ShowWindow(
            hwnd,
            if IsIconic(hwnd).as_bool() {
                SW_RESTORE
            } else {
                SW_SHOW
            },
        );
        let _ = BringWindowToTop(hwnd);
        // 由托盘点击触发时本进程刚收到用户输入，系统允许抢前台；第二次启动的进程见 `allow_foreground`。
        let _ = SetForegroundWindow(hwnd);
    }
}

unsafe extern "system" fn main_window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match msg {
        WM_SHOW_MAIN => {
            show_now(hwnd);
            LRESULT(0)
        }
        WM_QUIT_MAIN => {
            if let Some(on_quit) = ON_QUIT.get() {
                on_quit();
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            // SAFETY: 窗口正在销毁，摘掉自己挂的子类化。
            let _ = unsafe { RemoveWindowSubclass(hwnd, Some(main_window_proc), SUBCLASS_ID) };
            // SAFETY: 交给下一层窗口过程继续处理。
            unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
        }
        // SAFETY: 其余消息交给下一层窗口过程。
        _ => unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) },
    }
}

/// 在界面线程里登记主窗口：之后任何线程都可以用 [`request_show`]、[`request_quit`]。
/// `on_quit` 在界面线程里被调用，应当清理托盘图标并结束进程。
pub fn install(hwnd: isize, on_quit: fn()) -> bool {
    let _ = ON_QUIT.set(on_quit);
    // SAFETY: 必须在窗口所属线程调用（调用方是界面创建时的回调）；窗口过程是本文件里的静态函数。
    let ok = unsafe { SetWindowSubclass(hwnd_of(hwnd), Some(main_window_proc), SUBCLASS_ID, 0) };
    if ok.as_bool() {
        MAIN_WINDOW.store(hwnd, Ordering::SeqCst);
    }
    ok.as_bool()
}

fn post(msg: u32) -> bool {
    let raw = MAIN_WINDOW.load(Ordering::SeqCst);
    if raw == 0 {
        return false;
    }
    // SAFETY: 向本进程的窗口投递消息，任意线程都可以。
    unsafe { PostMessageW(Some(hwnd_of(raw)), msg, WPARAM(0), LPARAM(0)).is_ok() }
}

/// 请界面线程显示主窗口并放到最前（不依赖 eframe 的 `update`）。没有登记窗口时返回 false。
pub fn request_show() -> bool {
    post(WM_SHOW_MAIN)
}

/// 请界面线程立即退出（不依赖 eframe 的 `update`）。没有登记窗口时返回 false。
pub fn request_quit() -> bool {
    post(WM_QUIT_MAIN)
}

/// 请界面线程像用户点关闭按钮一样关窗口：投递 `WM_CLOSE`，winit 转成关闭请求，走和点关闭按钮完全相同的路径。
pub fn request_close() -> bool {
    post(WM_CLOSE)
}

/// 允许任何进程抢前台。第二次启动的进程在请已在运行的那个显示窗口之前调用：
/// 它刚被用户启动，有权这样做；否则已在运行的进程在后台，`SetForegroundWindow` 会被系统拒绝，窗口只在任务栏闪烁。
pub fn allow_foreground() {
    // SAFETY: 只改本进程的前台权限设置。
    let _ = unsafe { AllowSetForegroundWindow(ASFW_ANY) };
}

/// 弹一个提示框（不阻塞调用者，用户点“确定”才消失）。用于第一次关闭窗口时说明程序还在后台运行：
/// 窗口马上隐藏，窗口里的提示看不到；不用 PowerShell 的气泡，它每次都要起一个外部进程，会闪控制台窗口。
pub fn message_box_async(title: &str, body: &str) -> bool {
    let (title, body) = (wide(title), wide(body));
    std::thread::Builder::new()
        .name("hint-box".to_owned())
        .spawn(move || {
            // SAFETY: 两个缓冲区在调用期间活着，以 0 结尾。
            let _ = unsafe {
                MessageBoxW(
                    None,
                    PCWSTR(body.as_ptr()),
                    PCWSTR(title.as_ptr()),
                    MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND | MB_TOPMOST,
                )
            };
        })
        .is_ok()
}

/// 一个顶层窗口的状态（实机测试用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    /// 窗口句柄。
    pub hwnd: isize,
    /// 窗口类名。
    pub class: String,
    /// 标题。
    pub title: String,
    /// 是否可见（`IsWindowVisible`）。
    pub visible: bool,
    /// 是不是工具窗口（`WS_EX_TOOLWINDOW`：工具窗口不出现在任务栏）。
    pub tool_window: bool,
}

struct Collect {
    pid: u32,
    found: Vec<WindowInfo>,
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: lparam 是 `top_windows_of_process` 传进来的 `Collect` 的地址，枚举期间一直有效。
    let collect = unsafe { &mut *(lparam.0 as *mut Collect) };
    let mut pid = 0u32;
    // SAFETY: 枚举得到的句柄。
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid == collect.pid {
        let (mut class, mut title) = ([0u16; 256], [0u16; 256]);
        // SAFETY: 缓冲区足够长，返回值是写入的字符数。
        let (class_len, title_len) = unsafe {
            (
                GetClassNameW(hwnd, &mut class),
                GetWindowTextW(hwnd, &mut title),
            )
        };
        // SAFETY: 读窗口属性。
        let (visible, ex_style) = unsafe {
            (
                IsWindowVisible(hwnd).as_bool(),
                GetWindowLongW(hwnd, GWL_EXSTYLE),
            )
        };
        collect.found.push(WindowInfo {
            hwnd: hwnd.0 as isize,
            class: String::from_utf16_lossy(
                class
                    .get(..usize::try_from(class_len).unwrap_or(0))
                    .unwrap_or(&[]),
            ),
            title: String::from_utf16_lossy(
                title
                    .get(..usize::try_from(title_len).unwrap_or(0))
                    .unwrap_or(&[]),
            ),
            visible,
            tool_window: ex_style.cast_unsigned() & WS_EX_TOOLWINDOW.0 != 0,
        });
    }
    BOOL(1)
}

/// 某个进程的所有顶层窗口（实机测试用）。
#[must_use]
pub fn top_windows_of_process(pid: u32) -> Vec<WindowInfo> {
    let mut collect = Collect {
        pid,
        found: Vec::new(),
    };
    // SAFETY: 回调只在本次调用期间运行，`collect` 活得比它久。
    let _ = unsafe {
        EnumWindows(
            Some(collect_window),
            LPARAM(std::ptr::addr_of_mut!(collect) as isize),
        )
    };
    collect.found
}

/// 给窗口投递 `WM_CLOSE`，和用户点窗口的关闭按钮一样（实机测试用）。
#[must_use]
pub fn post_close(hwnd: isize) -> bool {
    // SAFETY: 向窗口投递消息；句柄无效时只会失败。
    unsafe { PostMessageW(Some(hwnd_of(hwnd)), WM_CLOSE, WPARAM(0), LPARAM(0)).is_ok() }
}

/// 某个进程的直接子进程的程序名（实机测试用，查有没有 reg.exe、powershell.exe 这样的外部程序被启动）。
#[must_use]
pub fn child_process_names(parent: u32) -> Vec<String> {
    let mut names = Vec::new();
    // SAFETY: 进程快照句柄用完关闭；结构体按接口要求先填好 dwSize。
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return names;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).unwrap_or(0),
            ..PROCESSENTRY32W::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            if entry.th32ParentProcessID == parent {
                let len = entry
                    .szExeFile
                    .iter()
                    .position(|c| *c == 0)
                    .unwrap_or(entry.szExeFile.len());
                names.push(String::from_utf16_lossy(
                    entry.szExeFile.get(..len).unwrap_or(&[]),
                ));
            }
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    names
}
