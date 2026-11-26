//! Windows 剪贴板的延迟渲染（系统胶水）：在剪贴板上只声明格式，别的程序来取时才给数据。
//!
//! 依据（Microsoft Learn, *Clipboard Operations*, Delayed Rendering 一节，
//! <https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-operations>）：
//! - `SetClipboardData(格式, NULL)` 声明格式；有程序来取这个格式时，系统给剪贴板所有者的窗口发
//!   `WM_RENDERFORMAT`，窗口这时调 `SetClipboardData(格式, 数据)`，而且**不能**先 `OpenClipboard`
//!   （来取的程序正开着）；
//! - 所有者窗口要销毁时收到 `WM_RENDERALLFORMATS`，没渲染的格式之后就没有了；
//! - 剪贴板被别人清空时收到 `WM_DESTROYCLIPBOARD`。
//!
//! 所以需要一个自己的窗口和消息循环：这里在专门的线程上建一个只收消息的窗口。
//!
//! 不让剪贴板历史和云剪贴板来取：它们会在内容一变时就把所有格式取走，等于复制时就传。
//! 声明格式的同时放一个 `ExcludeClipboardContentFromMonitorProcessing`（原文，*Clipboard Formats*，
//! <https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats>：*Place any data on the
//! clipboard in this format to prevent all clipboard formats being included in the clipboard history or
//! synchronized to the user's other devices*）。别的剪贴板工具照样可能马上来取，管不了。
//!
//! 剪贴板序号（`GetClipboardSequenceNumber`）在渲染时会变（原文：*If clipboard rendering is delayed,
//! the sequence number is not incremented until the changes are rendered*），所以“剪贴板里是不是还是
//! 我们的声明”不能看序号，要看所有者是不是这个窗口（[`DelayedClipboard::owns`]）。
//!
//! 系统的限制（同一页原文）：处理 `WM_RENDERFORMAT` 期间来取数据的那个程序一直在等，界面没有响应，
//! 系统不提供进度和取消。所以渲染函数里等对方数据必须有上限
//! （`peerdesk_net::clipsync::FETCH_TIMEOUT`），到时间给空结果（不调 `SetClipboardData`，
//! 来取的程序得到“没有这个格式”）。
//!
//! 这个文件只依赖 `windows` 和标准库，和会话无关：给什么格式、怎么把内容变成字节由调用方决定
//! （[`crate::clipboard_sys`]）。只在 Windows 上编译，要在实机上验证。
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GlobalFree, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardOwner, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassW, SendMessageW, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP,
    WM_CLOSE, WM_DESTROY, WM_DESTROYCLIPBOARD, WM_RENDERALLFORMATS, WM_RENDERFORMAT, WNDCLASSW,
};

/// 文字（UTF-16，以 0 结尾）。
pub const CF_UNICODETEXT: u32 = 13;
/// 文件列表（`DROPFILES`）。
pub const CF_HDROP: u32 = 15;
/// 图片（`BITMAPV5HEADER` 加像素）。
pub const CF_DIBV5: u32 = 17;

/// 有程序来取某个格式时调用：返回这个格式的数据；给不了时返回 `None`。会阻塞来取的那个程序。
pub type Render = Arc<dyn Fn(u32) -> Option<Vec<u8>> + Send + Sync>;
/// 我们声明的格式被别人顶掉了（别的程序往剪贴板放了东西）。
pub type Lost = Arc<dyn Fn() + Send + Sync>;

/// 让窗口线程把 [`Shared::formats`] 里的格式声明到剪贴板上。
const WM_OFFER: u32 = WM_APP + 1;

/// 调用方和窗口线程共用的东西。
struct Shared {
    /// 下一次要声明的格式。
    formats: Mutex<Vec<u32>>,
    render: Render,
    lost: Lost,
}

thread_local! {
    /// 窗口线程上的状态（窗口过程拿不到别的参数）。
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
}

/// 延迟渲染的剪贴板所有者：一个只收消息的窗口和它的线程。进程退出时线程随之结束。
#[derive(Debug)]
pub struct DelayedClipboard {
    /// 窗口句柄（`HWND` 不能跨线程传，存成整数）。
    hwnd: isize,
    formats: Arc<SharedFormats>,
}

/// [`Shared`] 里调用方要写的那一部分。
struct SharedFormats(Arc<Shared>);

impl std::fmt::Debug for SharedFormats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedFormats")
    }
}

/// 注册一个按名字约定的剪贴板格式（例如 `PNG`）；失败时为 0。
#[must_use]
pub fn register_format(name: PCWSTR) -> u32 {
    // SAFETY: 参数是调用方给的以 0 结尾的宽字符串。
    unsafe { RegisterClipboardFormatW(name) }
}

/// `PNG` 格式（很多程序认，带透明度）。
#[must_use]
pub fn png_format() -> u32 {
    register_format(w!("PNG"))
}

impl DelayedClipboard {
    /// 建窗口、起消息循环。
    pub fn start(render: Render, lost: Lost) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            formats: Mutex::new(Vec::new()),
            render,
            lost,
        });
        let (ready, started) = mpsc::channel::<Result<isize, String>>();
        let for_thread = Arc::clone(&shared);
        std::thread::Builder::new()
            .name("peerdesk-clipboard".into())
            .spawn(move || window_thread(for_thread, &ready))
            .map_err(|e| format!("起剪贴板线程失败：{e}"))?;
        let hwnd = started
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| format!("剪贴板窗口没有建起来：{e}"))??;
        Ok(Self {
            hwnd,
            formats: Arc::new(SharedFormats(shared)),
        })
    }

    /// 剪贴板的所有者是不是这个窗口（我们的声明还在，没被别的程序放的东西顶掉）。
    #[must_use]
    pub fn owns(&self) -> bool {
        // SAFETY: 只读一个句柄，没有参数。
        unsafe { GetClipboardOwner() }.is_ok_and(|owner| owner.0 as isize == self.hwnd)
    }

    /// 清空剪贴板并声明这些格式（不放数据）。之后有程序来取时调用渲染函数。
    pub fn offer(&self, formats: &[u32]) -> Result<(), String> {
        *self
            .formats
            .0
            .formats
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = formats.to_vec();
        // SAFETY: 句柄是本进程自己建的窗口；SendMessageW 跨线程调用会等窗口线程处理完。
        let result = unsafe { SendMessageW(HWND(self.hwnd as _), WM_OFFER, None, None) };
        if result.0 == 1 {
            Ok(())
        } else {
            Err("剪贴板被别的程序占着，放不上去".into())
        }
    }
}

/// 窗口线程：建只收消息的窗口，然后一直分发消息。
fn window_thread(shared: Arc<Shared>, ready: &mpsc::Sender<Result<isize, String>>) {
    SHARED.with(|s| *s.borrow_mut() = Some(shared));
    let class = WNDCLASSW {
        lpfnWndProc: Some(window_proc),
        lpszClassName: w!("PeerDeskClipboard"),
        ..Default::default()
    };
    // SAFETY: 类结构在调用期间有效，窗口过程是下面的函数；重复注册（同一进程开第二个会话）返回 0，
    // 这时类已经存在，照样能建窗口。
    let _ = unsafe { RegisterClassW(&raw const class) };
    // SAFETY: 类名和窗口名是静态的宽字符串；父窗口是 HWND_MESSAGE（只收消息、不显示）。
    let created = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("PeerDeskClipboard"),
            w!("PeerDesk clipboard"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        )
    };
    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(e) => {
            let _ = ready.send(Err(format!("建剪贴板窗口失败：{e}")));
            return;
        }
    };
    let _ = ready.send(Ok(hwnd.0 as isize));
    let mut msg = MSG::default();
    // SAFETY: msg 在循环期间有效；GetMessageW 返回 0（WM_QUIT）或 -1（出错）时结束。
    while unsafe { GetMessageW(&raw mut msg, None, 0, 0) }.0 > 0 {
        // SAFETY: msg 是 GetMessageW 刚填好的。
        let _ = unsafe { DispatchMessageW(&raw const msg) };
    }
}

fn shared() -> Option<Arc<Shared>> {
    SHARED.with(|s| s.borrow().clone())
}

/// 窗口过程。
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_OFFER => LRESULT(isize::from(offer(hwnd))),
        WM_RENDERFORMAT => {
            let format = u32::try_from(wparam.0).unwrap_or(0);
            render(format);
            LRESULT(0)
        }
        WM_RENDERALLFORMATS => {
            // 我们要退出了：对方的数据不留在这台电脑的剪贴板里，把声明撤掉
            // SAFETY: 打开、检查所有者、清空、关闭，都在本线程上，句柄是自己的窗口。
            unsafe {
                if OpenClipboard(Some(hwnd)).is_ok() {
                    if GetClipboardOwner().is_ok_and(|owner| owner == hwnd) {
                        let _ = EmptyClipboard();
                    }
                    let _ = CloseClipboard();
                }
            }
            LRESULT(0)
        }
        WM_DESTROYCLIPBOARD => {
            if let Some(shared) = shared() {
                (shared.lost)();
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: 没有参数要求；让消息循环结束。
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: 其余消息原样交给系统的默认处理。
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// 清空剪贴板、声明格式。剪贴板被别的程序开着时重试几次。
fn offer(hwnd: HWND) -> bool {
    let Some(shared) = shared() else { return false };
    let formats = shared
        .formats
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    let mut opened = false;
    for _ in 0..10 {
        // SAFETY: 句柄是自己的窗口；成功后下面一定会 CloseClipboard。
        if unsafe { OpenClipboard(Some(hwnd)) }.is_ok() {
            opened = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !opened {
        return false;
    }
    // SAFETY: 剪贴板已经由本窗口打开。EmptyClipboard 让本窗口成为所有者（之前自己声明的格式会先收到
    // WM_DESTROYCLIPBOARD）；SetClipboardData 传空句柄是声明延迟渲染，按文档它这时返回空，
    // 所以不看返回值，用 IsClipboardFormatAvailable 确认。
    unsafe {
        let emptied = EmptyClipboard().is_ok();
        for format in &formats {
            let _ = SetClipboardData(*format, None);
        }
        let declared = formats
            .iter()
            .all(|format| IsClipboardFormatAvailable(*format).is_ok());
        // 告诉剪贴板历史和云剪贴板不要收这一项（见本文件开头）；放不上也不影响声明
        let exclude = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
        if exclude != 0 {
            if let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, 4) {
                if SetClipboardData(exclude, Some(HANDLE(memory.0))).is_err() {
                    let _ = GlobalFree(Some(memory));
                }
            }
        }
        let _ = CloseClipboard();
        emptied && declared
    }
}

impl Drop for DelayedClipboard {
    fn drop(&mut self) {
        // 会话结束：关掉窗口（系统先发 WM_RENDERALLFORMATS，我们把声明撤掉），消息循环随之结束
        // SAFETY: 句柄是本进程自己建的窗口；窗口已经没了时这个调用什么都不做。
        let _ = unsafe { SendMessageW(HWND(self.hwnd as _), WM_CLOSE, None, None) };
    }
}

/// 有程序来取 `format`：向调用方要数据，放进一块全局内存交给系统。
fn render(format: u32) {
    let Some(shared) = shared() else { return };
    let Some(bytes) = (shared.render)(format) else {
        return;
    };
    // SAFETY: 分配一块可移动的全局内存、锁定、把数据拷进去、解锁；交给 SetClipboardData 以后内存归系统
    // 管（失败时自己释放）。处理 WM_RENDERFORMAT 时剪贴板由来取的程序开着，按文档不能再打开。
    unsafe {
        let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else {
            return;
        };
        let target = GlobalLock(memory);
        if target.is_null() {
            let _ = GlobalFree(Some(memory));
            return;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), target.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(memory);
        if SetClipboardData(format, Some(HANDLE(memory.0))).is_err() {
            let _ = GlobalFree(Some(memory));
        }
    }
}
