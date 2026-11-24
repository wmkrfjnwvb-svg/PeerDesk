//! Windows 服务管理器的胶水：把本进程登记成服务，处理“停止”请求，报告状态。
//!
//! 服务管理器在自己的线程里回调 `service_main`，回调没有用户数据参数，
//! 所以服务主体和状态句柄放在静态变量里（一个进程只跑一个服务）。

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SetServiceStatus, StartServiceCtrlDispatcherW,
    SERVICE_ACCEPT_SHUTDOWN, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP,
    SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE,
    SERVICE_STATUS_HANDLE, SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_TABLE_ENTRYW,
    SERVICE_WIN32_OWN_PROCESS,
};

type Body = Box<dyn FnOnce(&AtomicBool) + Send>;

static BODY: Mutex<Option<Body>> = Mutex::new(None);
static STOP: AtomicBool = AtomicBool::new(false);
/// 状态句柄（指针存成整数，静态变量里要能跨线程）。
static HANDLE: AtomicUsize = AtomicUsize::new(0);
static NAME: Mutex<Vec<u16>> = Mutex::new(Vec::new());

fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let handle = SERVICE_STATUS_HANDLE(HANDLE.load(Ordering::Relaxed) as *mut c_void);
    if handle.0.is_null() {
        return;
    }
    let accepted = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN
    } else {
        0
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 5000,
    };
    // SAFETY: 句柄来自 RegisterServiceCtrlHandlerExW，状态结构在栈上、调用期间有效。
    let _ = unsafe { SetServiceStatus(handle, &status) };
}

unsafe extern "system" fn handler(
    control: u32,
    _event: u32,
    _data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    // 其他控制码（查询状态等）不用处理，返回成功即可
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        STOP.store(true, Ordering::Relaxed);
        report(SERVICE_STOP_PENDING);
    }
    0
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    let name = NAME.lock().unwrap_or_else(PoisonError::into_inner).clone();
    // SAFETY: `name` 以 0 结尾，在调用期间有效；回调是本文件里的函数。
    let registered =
        unsafe { RegisterServiceCtrlHandlerExW(PCWSTR(name.as_ptr()), Some(handler), None) };
    let Ok(handle) = registered else {
        log::error!("服务：登记控制回调失败");
        return;
    };
    HANDLE.store(handle.0 as usize, Ordering::Relaxed);
    report(SERVICE_START_PENDING);
    let body = BODY.lock().unwrap_or_else(PoisonError::into_inner).take();
    report(SERVICE_RUNNING);
    if let Some(body) = body {
        body(&STOP);
    }
    report(SERVICE_STOPPED);
}

/// 作为服务 `name` 运行 `body`，直到服务管理器要求停止（`body` 收到的标志置位后应尽快返回）。
/// 只能由服务管理器启动的进程调用。
///
/// # Errors
/// 不是由服务管理器启动的（例如在命令行里直接运行）。
pub fn run(name: &str, body: impl FnOnce(&AtomicBool) + Send + 'static) -> Result<(), String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    *NAME.lock().unwrap_or_else(PoisonError::into_inner) = wide.clone();
    *BODY.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(body));
    let mut wide = wide;
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(wide.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: 表以全空项结尾；`wide` 和 `table` 在调用返回前一直有效（调用阻塞到服务结束）。
    unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) }.map_err(|e| {
        format!("只能由 Windows 服务管理器启动（请用 peerdesk service install 安装）：{e}")
    })
}
