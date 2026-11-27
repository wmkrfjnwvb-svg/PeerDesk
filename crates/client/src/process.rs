//! 启动外部程序的公共设置。
//!
//! Windows 上图形界面的 exe 没有控制台，它启动控制台程序（`reg.exe`、`systemctl` 以外的各种命令行工具）时，
//! 系统会为子进程新开一个控制台窗口，用户看到的就是“闪了一下”。所以凡是启动外部程序的地方都加
//! `CREATE_NO_WINDOW`。其他系统上什么也不做。

use std::process::Command;

/// `CreateProcess` 的标志：不为控制台程序创建控制台窗口。
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 让子进程不闪出控制台窗口。
pub fn quiet(command: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn a_quiet_command_still_runs_and_reports_output() {
        let mut command = if cfg!(windows) {
            let mut c = Command::new("cmd");
            let _ = c.args(["/C", "echo ok"]);
            c
        } else {
            let mut c = Command::new("sh");
            let _ = c.args(["-c", "echo ok"]);
            c
        };
        let out = quiet(&mut command).output().unwrap();
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));
        assert_eq!(CREATE_NO_WINDOW, 0x0800_0000);
    }
}
