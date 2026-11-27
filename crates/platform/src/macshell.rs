//! macOS 应用外壳的胶水：接住“点程序坞图标”（重新打开）事件。
//!
//! 问题：窗口被关掉（其实是隐藏，程序在菜单栏继续运行）后，用户点程序坞图标，系统给应用代理发
//! `applicationShouldHandleReopen:hasVisibleWindows:`，应用应当把窗口重新打开。winit 0.30 的应用代理类
//! 没有实现这个方法（也没有转成事件），所以点了没反应，只有菜单栏变成了本程序的。
//! 做法：运行时给 winit 已经装好的应用代理类加上这个方法（`class_addMethod`；类里本来没有才会成功，
//! 以后 winit 自己实现了，这里返回失败并写日志，不会覆盖它）。这是同类程序（Tauri 的 `RunEvent::Reopen`、
//! Electron 的 `activate` 事件）都用的办法：在代理上接这个回调。

#![allow(unsafe_code)]

use std::ffi::c_char;
use std::sync::OnceLock;

use objc2::ffi;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{msg_send, sel};

/// 收到“重新打开”时调用（在主线程）。
static ON_REOPEN: OnceLock<fn()> = OnceLock::new();

extern "C-unwind" fn should_handle_reopen(
    _this: &AnyObject,
    _cmd: Sel,
    _app: &AnyObject,
    _has_visible_windows: Bool,
) -> Bool {
    if let Some(on_reopen) = ON_REOPEN.get() {
        on_reopen();
    }
    // NO：我们自己处理了，系统不必再做默认动作
    Bool::NO
}

/// 给应用代理加上“点程序坞图标重新打开”的处理。必须在主线程、winit 装好应用代理之后调用
/// （eframe 创建界面的回调里就满足）。成功返回 true，失败（没有代理、类里已有这个方法）返回原因。
pub fn install_reopen_handler(on_reopen: fn()) -> Result<(), String> {
    let _ = ON_REOPEN.set(on_reopen);
    let ns_application = AnyClass::get(c"NSApplication").ok_or("找不到 NSApplication")?;
    // SAFETY: NSApplication 的 sharedApplication、delegate 是无参数的取值方法，返回对象指针（可为空）。
    let delegate: *mut AnyObject = unsafe {
        let app: *mut AnyObject = msg_send![ns_application, sharedApplication];
        if app.is_null() {
            return Err("没有 NSApplication 实例".to_owned());
        }
        msg_send![app, delegate]
    };
    // SAFETY: 上面检查了非空的是对象指针；`class` 返回它的类。
    let class = unsafe { delegate.as_ref() }
        .ok_or("应用代理还没有设置")?
        .class();
    let selector = sel!(applicationShouldHandleReopen:hasVisibleWindows:);
    // 类型编码：返回 BOOL（c），参数 self（@）、_cmd（:）、sender（@）、hasVisibleWindows（c）
    let types: *const c_char = c"c@:@c".as_ptr();
    // SAFETY: 函数的参数和返回类型与上面的编码一致；类是 winit 注册的代理类，用 `class_addMethod`
    // 只会在方法不存在时添加，已有则返回 NO 而不改动。
    let added = unsafe {
        let imp: Imp = std::mem::transmute::<
            extern "C-unwind" fn(&AnyObject, Sel, &AnyObject, Bool) -> Bool,
            Imp,
        >(should_handle_reopen);
        ffi::class_addMethod(std::ptr::from_ref(class).cast_mut(), selector, imp, types)
    };
    if added.as_bool() {
        Ok(())
    } else {
        Err(format!(
            "应用代理 {} 已经有这个方法，没有覆盖",
            class.name().to_string_lossy()
        ))
    }
}
