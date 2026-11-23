//! Windows 无人值守的系统胶水：系统服务在当前控制台会话里拉起代理进程、代发 Ctrl+Alt+Del，
//! 代理的采集和注入线程跟着“当前输入桌面”走（锁屏、登录界面、UAC 安全桌面）。
//!
//! 做法见 `docs/research/G-unattended.md` 第 1.2–1.4 节：服务以 SYSTEM 身份运行在 Session 0，
//! 复制自己的令牌、把会话号改成活动控制台会话，用它 `CreateProcessAsUserW` 拉起代理；
//! 代理因此是 SYSTEM 身份、在用户的会话里，才能打开 Winlogon 桌面采集和注入。

#![allow(unsafe_code)]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_TIMEOUT};
use windows::Win32::Security::Authentication::Identity::SendSAS;
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityIdentification, SetTokenInformation, TokenPrimary, TokenSessionId,
    TOKEN_ALL_ACCESS,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, SetThreadDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS,
    HDESK,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetCurrentProcess, OpenProcessToken, TerminateProcess,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION,
    STARTUPINFOW,
};

/// 代理模式：采集和注入跟着当前输入桌面走。普通界面程序不打开（切不过去，也不该切）。
static FOLLOW: AtomicBool = AtomicBool::new(false);

/// 打开或关闭“跟随输入桌面”。代理进程启动时打开。
pub fn set_follow_input_desktop(on: bool) {
    FOLLOW.store(on, Ordering::Relaxed);
}

thread_local! {
    /// 这个线程当前所在的桌面句柄：切到新桌面后才能关掉旧的（正在用的桌面句柄关不掉）。
    static CURRENT: Cell<Option<HDESK>> = const { Cell::new(None) };
}

/// 把调用线程切到当前输入桌面（Default、Winlogon 等）。没打开跟随时什么都不做。
/// 线程上不能有窗口或钩子（采集线程、注入所在的线程都没有）。失败时留在原桌面，下一次再试。
pub(crate) fn follow_input_desktop() {
    if !FOLLOW.load(Ordering::Relaxed) {
        return;
    }
    // GENERIC_ALL：采集要读、注入要写。
    let access = DESKTOP_ACCESS_FLAGS(windows::Win32::Foundation::GENERIC_ALL.0);
    // SAFETY: 参数都是值；返回的句柄由下面关闭或保存在 CURRENT 里。
    let Ok(desk) = (unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, access) }) else {
        return;
    };
    // SAFETY: `desk` 是刚打开的有效桌面句柄。
    if unsafe { SetThreadDesktop(desk) }.is_ok() {
        if let Some(old) = CURRENT.with(|c| c.replace(Some(desk))) {
            // SAFETY: 旧句柄是之前打开的；线程已经切走，可以关。
            let _ = unsafe { CloseDesktop(old) };
        }
    } else {
        // SAFETY: 没切成，句柄没人用。
        let _ = unsafe { CloseDesktop(desk) };
    }
}

/// 活动控制台会话号；没有人在控制台（例如正在切换会话）时为 `None`。
#[must_use]
pub fn active_console_session() -> Option<u32> {
    // SAFETY: 没有参数。
    let id = unsafe { WTSGetActiveConsoleSessionId() };
    (id != u32::MAX).then_some(id)
}

/// 让 Windows 弹出 Ctrl+Alt+Del 安全界面。只有服务调用才有效（且策略允许服务模拟 SAS）。
pub fn send_sas() {
    // SAFETY: 没有指针参数；不满足条件时系统忽略这次调用。
    unsafe { SendSAS(false) };
}

/// 拉起的代理进程。丢掉时不结束进程，只关句柄。
#[derive(Debug)]
pub struct Child {
    process: HANDLE,
}

// SAFETY: 进程句柄可以在线程之间传递和使用。
unsafe impl Send for Child {}

impl Child {
    /// 进程还在运行。
    #[must_use]
    pub fn running(&self) -> bool {
        // SAFETY: `process` 在 Drop 之前一直有效。
        unsafe { WaitForSingleObject(self.process, 0) == WAIT_TIMEOUT }
    }

    /// 结束进程。
    pub fn kill(&self) {
        // SAFETY: 同上。
        let _ = unsafe { TerminateProcess(self.process, 1) };
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        // SAFETY: 只在这里关一次。
        let _ = unsafe { CloseHandle(self.process) };
    }
}

/// 关句柄的小工具（出错路径上也要关）。
struct Owned(HANDLE);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            // SAFETY: 句柄由本结构独占。
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 以本进程（SYSTEM）的身份在会话 `session` 里、`winsta0\default` 桌面上运行 `command_line`。
/// 本进程必须是 SYSTEM（服务）：改令牌的会话号需要 SeTcbPrivilege。
///
/// # Errors
/// 任何一步失败都带上是哪一步和系统给的原因。
pub fn spawn_in_session(session: u32, command_line: &str) -> Result<Child, String> {
    let mut own = HANDLE::default();
    // SAFETY: 输出参数指向局部变量。
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_ALL_ACCESS, &mut own) }
        .map_err(|e| format!("OpenProcessToken：{e}"))?;
    let own = Owned(own);
    let mut token = HANDLE::default();
    // SAFETY: `own.0` 是有效令牌；输出参数指向局部变量。
    unsafe {
        DuplicateTokenEx(
            own.0,
            TOKEN_ALL_ACCESS,
            None,
            SecurityIdentification,
            TokenPrimary,
            &mut token,
        )
    }
    .map_err(|e| format!("DuplicateTokenEx：{e}"))?;
    let token = Owned(token);
    let size = u32::try_from(size_of::<u32>()).unwrap_or(4);
    // SAFETY: 指针指向局部的 u32，长度正确。
    unsafe {
        SetTokenInformation(
            token.0,
            TokenSessionId,
            std::ptr::from_ref(&session).cast(),
            size,
        )
    }
    .map_err(|e| format!("SetTokenInformation（会话 {session}）：{e}"))?;

    let mut desktop = wide("winsta0\\default");
    let mut cmd = wide(command_line);
    let startup = STARTUPINFOW {
        cb: u32::try_from(size_of::<STARTUPINFOW>()).unwrap_or(0),
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..STARTUPINFOW::default()
    };
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: 字符串缓冲区在调用期间有效（命令行必须可写）；结构体大小已填。
    unsafe {
        CreateProcessAsUserW(
            Some(token.0),
            windows::core::PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            None,
            windows::core::PCWSTR::null(),
            &startup,
            &mut info,
        )
    }
    .map_err(|e| format!("CreateProcessAsUserW：{e}"))?;
    drop(Owned(info.hThread));
    Ok(Child {
        process: info.hProcess,
    })
}
