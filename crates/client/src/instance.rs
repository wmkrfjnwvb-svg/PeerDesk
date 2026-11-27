//! 单实例：同一个用户只允许一个 PeerDesk 界面进程。
//!
//! 为什么需要：关闭退到托盘之后，用户再双击图标不能再开一个（两个进程抢同一个被控端口和托盘）；
//! 进程守护（开机自启）和用户手动启动也会撞上。第二次启动应该是“把已经在跑的那个窗口叫出来”，然后自己退出（退出码 0，
//! 守护不会把它当成崩溃再拉起）。
//!
//! 做法：数据目录里一个锁文件，用文件锁（进程死了系统自动释放，所以被杀、崩溃后守护拉起的新进程一定拿得到锁，
//! 不会被陈旧的锁文件卡住）；第二个进程在数据目录里放一个 `window.request` 文件（内容是 `show`、`close` 或 `quit`，空文件当作 `show`），
//! 第一个进程的后台线程看到就删掉并照办：显示窗口、像点关闭按钮一样关窗口、退出。`peerdesk window ...` 命令也用它
//! （安装程序、更新、自动化测试要让正在运行的界面退出时用）。
//! 用文件而不是本机端口或套接字：不占端口，三个平台一样，不需要新依赖。

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// 锁文件名。
pub const LOCK_FILE: &str = "gui.lock";
/// 留给正在运行的界面的请求文件。
pub const REQUEST_FILE: &str = "window.request";

/// 对正在运行的界面的请求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// 显示窗口并放到最前。第二次启动程序就是这个请求。
    Show,
    /// 像用户点窗口的关闭按钮一样关窗口（有托盘时是隐藏）。
    Close,
    /// 退出程序，和托盘菜单的“退出”一样。
    Quit,
}

impl Request {
    /// 写进文件的词。
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Close => "close",
            Self::Quit => "quit",
        }
    }

    /// 从命令行或文件内容解析。空内容是 `Show`（旧版本留下的空文件）。
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim() {
            "" | "show" => Some(Self::Show),
            "close" => Some(Self::Close),
            "quit" => Some(Self::Quit),
            _ => None,
        }
    }
}

/// 抢单实例锁的结果。
#[derive(Debug)]
pub enum Instance {
    /// 本进程是第一个。文件要一直拿着（进程退出时系统释放锁）。
    First(File),
    /// 已经有一个在跑了。
    Running,
}

/// 抢锁。锁文件打不开（目录没权限等）时返回错误：不能假装自己是唯一的，也不能静默退出。
pub fn acquire(dir: &Path) -> std::io::Result<Instance> {
    std::fs::create_dir_all(dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))?;
    // `File::try_lock` 从 Rust 1.89 起稳定（工作区最低版本因此是 1.89）
    let locked = file.try_lock();
    match locked {
        Ok(()) => Ok(Instance::First(file)),
        Err(TryLockError::WouldBlock) => Ok(Instance::Running),
        Err(TryLockError::Error(e)) => Err(e),
    }
}

/// 向已经在跑的那个进程发请求。先写临时文件再改名，对方不会读到写了一半的内容。
pub fn send_request(dir: &Path, request: Request) -> std::io::Result<()> {
    // 本进程刚被用户启动，有权让对方抢前台；对方在后台时系统默认不许它把窗口放到最前
    #[cfg(windows)]
    peerdesk_platform::winshell::allow_foreground();
    let tmp = dir.join(format!("{REQUEST_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, request.word())?;
    std::fs::rename(&tmp, dir.join(REQUEST_FILE))
}

/// 请已经在跑的那个进程显示窗口。
pub fn request_show(dir: &Path) -> std::io::Result<()> {
    send_request(dir, Request::Show)
}

/// 有没有请求；有就消费掉（删文件）。内容看不懂的当作 `Show`：宁可多显示一次窗口，也不能吞掉用户的启动。
#[must_use]
pub fn take_request(dir: &Path) -> Option<Request> {
    let path = dir.join(REQUEST_FILE);
    let text = std::fs::read_to_string(&path).ok()?;
    std::fs::remove_file(&path).ok()?;
    Some(Request::parse(&text).unwrap_or(Request::Show))
}

/// `peerdesk window show|close|quit`：对正在运行的界面发请求。
/// 给安装程序、更新脚本和自动化测试用（让界面退出、把窗口叫出来）。没有在运行的界面时报错。
pub fn cmd_window(args: &[String], out: &mut dyn std::io::Write, dir: &Path) -> u8 {
    let Some(request) = args
        .first()
        .and_then(|a| Request::parse(a))
        .filter(|_| args.len() == 1)
    else {
        let _ = writeln!(out, "用法：peerdesk window show|close|quit");
        return crate::EXIT_USAGE;
    };
    // 抢得到锁说明没有界面在运行：不能把请求留下，否则下次启动会莫名其妙地执行它
    match acquire(dir) {
        Ok(Instance::Running) => {}
        Ok(Instance::First(_)) => {
            let _ = writeln!(out, "错误：没有正在运行的 PeerDesk 界面");
            return crate::EXIT_FAILED;
        }
        Err(e) => {
            let _ = writeln!(out, "错误：{}：{e}", dir.display());
            return crate::EXIT_FAILED;
        }
    }
    match send_request(dir, request) {
        Ok(()) => crate::EXIT_OK,
        Err(e) => {
            let _ = writeln!(out, "错误：{e}");
            crate::EXIT_FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdesk-inst-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn second_instance_is_detected_and_lock_is_released_on_drop() {
        let dir = temp("lock");
        let first = acquire(&dir).unwrap();
        assert!(matches!(first, Instance::First(_)));
        assert!(matches!(acquire(&dir).unwrap(), Instance::Running));
        drop(first);
        // 第一个进程没了（被杀也一样，锁由系统释放）：新进程拿得到
        assert!(matches!(acquire(&dir).unwrap(), Instance::First(_)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn requests_are_consumed_once_and_keep_their_meaning() {
        let dir = temp("show");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(take_request(&dir), None);
        request_show(&dir).unwrap();
        assert_eq!(take_request(&dir), Some(Request::Show));
        assert_eq!(take_request(&dir), None);
        for request in [Request::Show, Request::Close, Request::Quit] {
            send_request(&dir, request).unwrap();
            assert_eq!(take_request(&dir), Some(request));
        }
        // 旧版本留下的空文件、看不懂的内容：显示窗口
        std::fs::write(dir.join(REQUEST_FILE), b"").unwrap();
        assert_eq!(take_request(&dir), Some(Request::Show));
        std::fs::write(dir.join(REQUEST_FILE), b"???").unwrap();
        assert_eq!(take_request(&dir), Some(Request::Show));
        // 没有临时文件留下
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn window_command_needs_a_running_interface() {
        let dir = temp("cmd");
        let run = |args: &[&str], dir: &Path| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            let mut out = Vec::new();
            let code = cmd_window(&args, &mut out, dir);
            (code, String::from_utf8(out).unwrap())
        };
        assert_eq!(run(&[], &dir).0, crate::EXIT_USAGE);
        assert_eq!(run(&["exit"], &dir).0, crate::EXIT_USAGE);
        assert_eq!(run(&["quit", "now"], &dir).0, crate::EXIT_USAGE);
        // 没有界面在运行：报错，也不留下请求
        let (code, text) = run(&["quit"], &dir);
        assert_eq!(code, crate::EXIT_FAILED);
        assert!(text.contains("没有正在运行"), "{text}");
        assert_eq!(take_request(&dir), None);
        // 有界面在运行：请求送到
        let first = acquire(&dir).unwrap();
        assert_eq!(run(&["close"], &dir).0, crate::EXIT_OK);
        assert_eq!(take_request(&dir), Some(Request::Close));
        drop(first);
        // 目录不可用
        let blocker = dir.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        assert_eq!(run(&["show"], &blocker.join("sub")).0, crate::EXIT_FAILED);
        // 有界面但请求写不进去（目录只读之类）：目录里放一个同名目录让改名失败
        let first = acquire(&dir).unwrap();
        std::fs::create_dir_all(dir.join(REQUEST_FILE)).unwrap();
        let (code, text) = run(&["show"], &dir);
        assert_eq!(code, crate::EXIT_FAILED, "{text}");
        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn request_words_round_trip() {
        for request in [Request::Show, Request::Close, Request::Quit] {
            assert_eq!(Request::parse(request.word()), Some(request));
        }
        assert_eq!(Request::parse(" quit\n"), Some(Request::Quit));
        assert_eq!(Request::parse("exit"), None);
    }

    #[test]
    fn unusable_directory_is_an_error() {
        let dir = temp("bad");
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("file");
        std::fs::write(&blocker, b"x").unwrap();
        // 目录位置上是个文件：建不了目录
        assert!(acquire(&blocker.join("sub")).is_err());
        assert!(request_show(&blocker.join("sub")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
