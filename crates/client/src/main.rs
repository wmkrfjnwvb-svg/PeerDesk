//! PeerDesk 客户端入口。逻辑都在库里，这里只接线。
//!
//! 不带参数运行（例如双击）时打开图形界面，否则是命令行。
//! Windows 上这是一个图形界面程序，双击时不会弹出控制台窗口；命令行子命令运行时
//! 挂到启动它的终端上输出。代价是 cmd.exe 不会等它结束（提示符先回来），
//! 需要等结果时用 `start /wait peerdesk ...`，或者把输出重定向/管道出去。

#![cfg_attr(windows, windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    // 旧版 macOS 从访达启动程序包时会加一个 `-psn_…` 参数，它不是命令。
    let args: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("-psn_"))
        .collect();
    // 日志写到数据目录的 logs 里，出问题时把它发出来。
    peerdesk::logfile::init(&peerdesk::data_dir().join("logs"), false);
    peerdesk::crashreport::install_panic_hook(peerdesk::data_dir().join("logs"));
    if args.is_empty() {
        return ExitCode::from(peerdesk::gui::run());
    }
    #[cfg(windows)]
    attach_parent_console();
    let mut out = std::io::stdout().lock();
    ExitCode::from(peerdesk::run(&args, &mut out))
}

/// 标准输出没有被重定向时，挂到父进程（终端）的控制台上，命令行的输出才看得到。
#[cfg(windows)]
fn attach_parent_console() {
    use win32console::console::{HandleType, WinConsole};
    let missing = WinConsole::get_std_handle(HandleType::Output).map_or(true, |h| h.is_null());
    if missing {
        // ATTACH_PARENT_PROCESS。没有父控制台（例如从资源管理器启动）时失败，输出照常丢弃。
        let _ = WinConsole::attach_console(u32::MAX);
    }
}
