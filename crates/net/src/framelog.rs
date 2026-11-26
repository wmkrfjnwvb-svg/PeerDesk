//! 逐帧时间日志（测量用，默认关）：环境变量 `PEERDESK_FRAME_LOG` 给出文件路径时，被控端记下每次画面变化和
//! 每一帧的采集时刻，控制端记下每一帧显示出来的时刻。两端在同一台机器上跑时用同一个时钟（Unix 微秒），
//! `scripts/trace-lab.py` 按它们离线算“画面变了到控制端看到新画面”要等多久——和仿真
//! （`crates/e2e`）的等待时间同一个定义，这样真实程序和仿真的结论才能对照。
//!
//! 每行一条：`C <时刻>`（画面变了）、`E <帧号> <采集时刻>`（这一帧交给编码器）、`S <帧号> <时刻>`（控制端显示了这一帧）。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 逐帧时间日志。没打开时什么也不做。
#[derive(Debug, Default)]
pub struct FrameLog {
    out: Option<BufWriter<File>>,
}

/// 现在的 Unix 时间（微秒）。
#[must_use]
pub fn unix_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

impl FrameLog {
    /// 按环境变量 `PEERDESK_FRAME_LOG` 打开；没设或打不开时关着。
    #[must_use]
    pub fn from_env() -> Self {
        std::env::var_os("PEERDESK_FRAME_LOG")
            .map_or_else(Self::default, |p| Self::open(Path::new(&p)))
    }

    /// 写到 `path`（覆盖）；打不开时关着。
    #[must_use]
    pub fn open(path: &Path) -> Self {
        Self {
            out: File::create(path).ok().map(BufWriter::new),
        }
    }

    /// 是否打开。
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.out.is_some()
    }

    fn line(&mut self, args: std::fmt::Arguments<'_>) {
        if let Some(out) = self.out.as_mut() {
            // 写不进去（磁盘满）就关掉，不影响会话
            if out
                .write_fmt(args)
                .and_then(|()| out.write_all(b"\n"))
                .is_err()
            {
                self.out = None;
            }
        }
    }

    /// 被控端：画面变了（采集时刻 `at`，Unix 微秒）。
    pub fn changed(&mut self, at: u64) {
        self.line(format_args!("C {at}"));
    }

    /// 被控端：第 `frame_id` 帧交给编码器，画面是 `captured` 时刻采集的。
    pub fn encoded(&mut self, frame_id: u32, captured: u64) {
        self.line(format_args!("E {frame_id} {captured}"));
    }

    /// 控制端：第 `frame_id` 帧在 `at` 时刻显示出来。
    pub fn shown(&mut self, frame_id: u32, at: u64) {
        self.line(format_args!("S {frame_id} {at}"));
    }

    /// 写到磁盘（会话结束时）。
    pub fn flush(&mut self) {
        if let Some(out) = self.out.as_mut() {
            if out.flush().is_err() {
                self.out = None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn writes_lines_only_when_open() {
        let mut off = FrameLog::default();
        assert!(!off.enabled());
        off.changed(1);
        off.flush();
        let dir = std::env::temp_dir().join(format!("peerdesk-framelog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.log");
        let mut log = FrameLog::open(&path);
        assert!(log.enabled());
        log.changed(10);
        log.encoded(3, 10);
        log.shown(3, 25);
        log.flush();
        drop(log);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "C 10\nE 3 10\nS 3 25\n"
        );
        // 打不开（目录不存在）：关着
        assert!(!FrameLog::open(&dir.join("no/such/dir/f.log")).enabled());
        assert!(unix_us() > 1_600_000_000_000_000);
        // 环境变量没设：关着（测试进程里不设它）
        if std::env::var_os("PEERDESK_FRAME_LOG").is_none() {
            assert!(!FrameLog::from_env().enabled());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
