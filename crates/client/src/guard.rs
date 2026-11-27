//! 进程守护：`peerdesk guard` 运行图形界面，被杀或崩溃就重启，用户主动退出就不重启。
//!
//! 谁用它：Windows（没有“退出码非 0 才重启”的现成机制：计划任务的失败重启是否覆盖进程崩溃没有核实，
//! 服务又不能有桌面窗口）和没有 systemd 用户实例的 Linux。macOS 用 launchd 的 `KeepAlive`，
//! Linux 有 systemd 时用 `Restart=on-failure`，都不经过这里，见 `crate::autostart`。
//! 同一个二进制的子命令，不新增第三个二进制。
//!
//! 规则：
//! - 子进程退出码 0：用户从托盘退出（或第二次启动发现已有实例），守护也退出 0，不重启；
//! - 退出码 [`EXIT_NO_RETRY`]：重试也没用（缺系统库等），守护停下并保留这个退出码，原因在日志里；
//! - 其他（崩溃、被信号杀、被任务管理器结束）：等一会儿再启动，连续快速失败时等待加倍，最长 1 分钟，
//!   不放弃（放弃就是静默降级：用户以为一直有守护，其实早就没了）；跑满 1 分钟算恢复正常，等待从头算。
//!
//! 局限：守护进程本身被杀就没人拉起了，要等下次登录。它只做等待和重启，几乎不会出错。

use std::io::Write;
use std::time::{Duration, Instant};

use crate::{EXIT_FAILED, EXIT_NO_RETRY, EXIT_OK};

/// 子进程跑满这么久才算“正常运行过”，之后的失败从最短的等待重新算。
pub const HEALTHY: Duration = Duration::from_secs(60);
/// 最长等待。
pub const MAX_WAIT: Duration = Duration::from_secs(60);
/// 告诉被守护的进程“你在守护下运行”的环境变量（日志里区分用）。
pub const GUARDED_ENV: &str = "PEERDESK_GUARDED";

/// 守护的决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// 不再重启，守护以这个退出码结束。
    Stop(u8),
    /// 等这么久再重启。
    Restart(Duration),
}

/// 重启策略（连续快速失败的次数）。
#[derive(Debug, Default)]
pub struct Backoff {
    quick_failures: u32,
}

impl Backoff {
    /// 子进程退出了。`code` 为 `None` 表示没有退出码（被信号杀掉）；`ran` 是它跑了多久。
    pub fn on_exit(&mut self, code: Option<i32>, ran: Duration) -> Decision {
        match code {
            Some(0) => return Decision::Stop(EXIT_OK),
            Some(c) if c == i32::from(EXIT_NO_RETRY) => return Decision::Stop(EXIT_NO_RETRY),
            Some(_) | None => {}
        }
        if ran >= HEALTHY {
            self.quick_failures = 0;
        }
        let shift = self.quick_failures.min(6);
        self.quick_failures = self.quick_failures.saturating_add(1);
        Decision::Restart(Duration::from_secs(1_u64 << shift).min(MAX_WAIT))
    }
}

/// 一次运行的结果：退出码（没有则是被信号杀的）和运行时长。
pub type Run = (Option<i32>, Duration);

/// 守护循环。`launch` 启动子进程并等它结束（启动失败返回原因），`sleep` 等待。返回守护自己的退出码。
pub fn supervise(
    launch: &mut dyn FnMut() -> Result<Run, String>,
    sleep: &mut dyn FnMut(Duration),
) -> u8 {
    let mut backoff = Backoff::default();
    loop {
        let (code, ran) = launch().unwrap_or_else(|why| {
            log::error!("守护：启动 PeerDesk 失败：{why}");
            (None, Duration::ZERO)
        });
        match backoff.on_exit(code, ran) {
            Decision::Stop(exit) => {
                log::info!("守护：PeerDesk 退出码 {exit}，不再重启");
                return exit;
            }
            Decision::Restart(wait) => {
                log::warn!(
                    "守护：PeerDesk 异常结束（退出码 {code:?}，运行了 {} 秒），{} 秒后重启",
                    ran.as_secs(),
                    wait.as_secs()
                );
                sleep(wait);
            }
        }
    }
}

/// `peerdesk guard`。
pub fn cmd_guard(out: &mut dyn Write) -> u8 {
    match std::env::current_exe() {
        Ok(exe) => guard_program(&exe, &[], &mut std::thread::sleep),
        Err(e) => {
            let _ = writeln!(out, "错误：找不到自己的路径：{e}");
            EXIT_FAILED
        }
    }
}

/// 守护 `exe args`：真的启动子进程并等它结束。
fn guard_program(exe: &std::path::Path, args: &[&str], sleep: &mut dyn FnMut(Duration)) -> u8 {
    supervise(
        &mut || {
            let start = Instant::now();
            let status = std::process::Command::new(exe)
                .args(args)
                .env(GUARDED_ENV, "1")
                .status()
                .map_err(|e| format!("{}：{e}", exe.display()))?;
            Ok((status.code(), start.elapsed()))
        },
        sleep,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHORT: Duration = Duration::from_secs(2);

    #[test]
    fn clean_exit_and_no_retry_stop_the_guard() {
        let mut b = Backoff::default();
        assert_eq!(b.on_exit(Some(0), SHORT), Decision::Stop(0));
        assert_eq!(
            b.on_exit(Some(i32::from(EXIT_NO_RETRY)), SHORT),
            Decision::Stop(EXIT_NO_RETRY)
        );
    }

    #[test]
    fn crashes_restart_with_growing_wait_capped_at_a_minute() {
        let mut b = Backoff::default();
        let waits: Vec<u64> = (0..9)
            .map(
                |i| match b.on_exit(if i % 2 == 0 { Some(1) } else { None }, SHORT) {
                    Decision::Restart(d) => d.as_secs(),
                    Decision::Stop(_) => 0,
                },
            )
            .collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }

    #[test]
    fn a_healthy_run_resets_the_wait() {
        let mut b = Backoff::default();
        for _ in 0..4 {
            let _ = b.on_exit(Some(1), SHORT);
        }
        assert_eq!(
            b.on_exit(Some(1), HEALTHY),
            Decision::Restart(Duration::from_secs(1))
        );
        assert_eq!(
            b.on_exit(Some(1), SHORT),
            Decision::Restart(Duration::from_secs(2))
        );
    }

    #[test]
    fn supervise_restarts_until_a_clean_exit() {
        let mut results = vec![
            Ok((Some(0), Duration::from_secs(300))),
            Err("找不到程序".to_owned()),
            Ok((None, SHORT)),
            Ok((Some(1), SHORT)),
        ];
        let mut slept = Vec::new();
        let code = supervise(
            &mut || results.pop().unwrap_or(Ok((Some(0), SHORT))),
            &mut |d| slept.push(d.as_secs()),
        );
        assert_eq!(code, 0);
        assert_eq!(
            slept,
            [1, 2, 4],
            "三次异常，退让加倍，之后用户退出就不再重启"
        );
    }

    #[test]
    fn supervise_keeps_the_no_retry_code() {
        let code = supervise(
            &mut || Ok((Some(i32::from(EXIT_NO_RETRY)), SHORT)),
            &mut |_| {},
        );
        assert_eq!(code, EXIT_NO_RETRY);
    }

    #[cfg(unix)]
    #[test]
    fn real_child_is_restarted_after_a_crash_but_not_after_a_clean_exit() {
        let sh = std::path::Path::new("/bin/sh");
        let flag = std::env::temp_dir().join(format!("peerdesk-guard-{}", std::process::id()));
        let _ = std::fs::remove_file(&flag);
        // 第一次：创建标记文件后以 3 退出（崩溃）；第二次：看到标记文件，以 0 退出（用户退出）
        let script = format!(
            "if [ -f '{0}' ]; then exit 0; else touch '{0}'; exit 3; fi",
            flag.display()
        );
        let mut slept = Vec::new();
        let code = guard_program(sh, &["-c", &script], &mut |d| slept.push(d));
        assert_eq!(code, 0);
        assert_eq!(slept, [Duration::from_secs(1)], "崩溃后重启了一次");
        let _ = std::fs::remove_file(&flag);
        // 被信号杀掉也算崩溃
        let mut n = 0;
        let code = guard_program(
            sh,
            &[
                "-c",
                &format!(
                    "[ -f '{0}' ] && exit 0; touch '{0}'; kill -9 $$",
                    flag.display()
                ),
            ],
            &mut |_| n += 1,
        );
        assert_eq!((code, n), (0, 1));
        let _ = std::fs::remove_file(&flag);
        assert_eq!(
            guard_program(sh, &["-c", "exit 78"], &mut |_| {}),
            EXIT_NO_RETRY
        );
    }

    #[test]
    fn missing_program_is_retried_not_dropped() {
        let mut n = 0;
        let code = {
            let mut results = vec![Ok((Some(0), SHORT)), Err("x".to_owned())];
            supervise(
                &mut || results.pop().unwrap_or(Ok((Some(0), SHORT))),
                &mut |_| n += 1,
            )
        };
        assert_eq!((code, n), (0, 1));
        // 真的找不到程序：启动失败走和崩溃一样的重试路径（测试里最多重试两次就让它成功）
        let mut tries = 0;
        let mut sleeps = 0;
        let code = supervise(
            &mut || {
                tries += 1;
                if tries < 3 {
                    std::process::Command::new("/nonexistent/peerdesk-guard-test")
                        .status()
                        .map(|s| (s.code(), Duration::ZERO))
                        .map_err(|e| e.to_string())
                } else {
                    Ok((Some(0), SHORT))
                }
            },
            &mut |_| sleeps += 1,
        );
        assert_eq!((code, tries, sleeps), (0, 3, 2));
    }
}
