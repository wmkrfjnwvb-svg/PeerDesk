//! 单实例：同一个用户只允许一个 PeerDesk 界面进程。
//!
//! 为什么需要：关闭退到托盘之后，用户再双击图标不能再开一个（两个进程抢同一个被控端口和托盘）；
//! 进程守护（开机自启）和用户手动启动也会撞上。第二次启动应该是“把已经在跑的那个窗口叫出来”，然后自己退出（退出码 0，
//! 守护不会把它当成崩溃再拉起）。
//!
//! 做法：数据目录里一个锁文件，用文件锁（进程死了系统自动释放，所以被杀、崩溃后守护拉起的新进程一定拿得到锁，
//! 不会被陈旧的锁文件卡住）；第二个进程在数据目录里放一个 `show.request` 文件，第一个进程的后台线程看到就删掉并打开窗口。
//! 用文件而不是本机端口或套接字：不占端口，三个平台一样，不需要新依赖。

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;

/// 锁文件名。
pub const LOCK_FILE: &str = "gui.lock";
/// 第二次启动留下的“请显示窗口”请求。
pub const SHOW_FILE: &str = "show.request";

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

/// 请已经在跑的那个进程显示窗口。
pub fn request_show(dir: &Path) -> std::io::Result<()> {
    std::fs::write(dir.join(SHOW_FILE), b"")
}

/// 有没有显示请求；有就消费掉（删文件）。
#[must_use]
pub fn take_show_request(dir: &Path) -> bool {
    std::fs::remove_file(dir.join(SHOW_FILE)).is_ok()
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
    fn show_request_is_consumed_once() {
        let dir = temp("show");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!take_show_request(&dir));
        request_show(&dir).unwrap();
        assert!(take_show_request(&dir));
        assert!(!take_show_request(&dir));
        let _ = std::fs::remove_dir_all(&dir);
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
