//! 日志（桌面端和手机端共用）：写到数据目录的 `logs/peerdesk.log`，出问题时把这个文件发出来就能排查。
//!
//! 内容包括连接的各个阶段（找服务器、打洞、直连还是中继、各用了多久、失败原因）
//! 和会话中每秒一行的统计（帧数、码率、目标码率、RTT、丢包、最长卡顿）。
//! 不记录画面、键盘输入的内容和密码。
//!
//! 文件超过 [`MAX_BYTES`] 时改名为 `peerdesk.1.log`（覆盖更早的那份）再重新开始，
//! 所以最多占两份的空间。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// 单个日志文件的大小上限。
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
/// 日志文件名。
pub const FILE: &str = "peerdesk.log";
/// 轮换出去的旧文件名。
pub const OLD_FILE: &str = "peerdesk.1.log";

/// 写文件的日志器。
#[derive(Debug)]
pub struct FileLog {
    dir: PathBuf,
    max_bytes: u64,
    state: Mutex<Option<(File, u64)>>,
    echo: bool,
}

impl FileLog {
    /// 在 `dir` 里写日志。`echo` 为真时同时写到标准错误（命令行用）。
    #[must_use]
    pub fn new(dir: &Path, max_bytes: u64, echo: bool) -> Self {
        Self {
            dir: dir.to_owned(),
            max_bytes,
            state: Mutex::new(None),
            echo,
        }
    }

    fn open(&self) -> Option<(File, u64)> {
        std::fs::create_dir_all(&self.dir).ok()?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(FILE))
            .ok()?;
        let len = file.metadata().map_or(0, |m| m.len());
        Some((file, len))
    }

    /// 写一行。写不进去（磁盘满、没有权限）时静默放弃：日志不能影响远程控制本身。
    pub fn write_line(&self, line: &str) {
        if self.echo {
            let _ = writeln!(std::io::stderr(), "{line}");
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state
            .as_ref()
            .is_some_and(|(_, len)| *len >= self.max_bytes)
        {
            *state = None;
            let _ = std::fs::rename(self.dir.join(FILE), self.dir.join(OLD_FILE));
        }
        if state.is_none() {
            *state = self.open();
        }
        if let Some((file, len)) = state.as_mut() {
            if writeln!(file, "{line}").is_ok() {
                *len = len.saturating_add(u64::try_from(line.len()).unwrap_or(0) + 1);
            }
        }
    }
}

impl log::Log for FileLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        // 只记我们自己的日志；第三方库（QUIC、TLS）的调试信息太多。
        metadata.target().starts_with("peerdesk") && metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            self.write_line(&format_line(
                now_ms(),
                record.level(),
                record.target(),
                record.args(),
            ));
        }
    }

    fn flush(&self) {}
}

/// 打开日志。只能打开一次，再次调用不起作用。
pub fn init(dir: &Path, echo: bool) {
    init_with(dir, MAX_BYTES, echo);
}

/// 同 [`init`]，单个文件的大小上限自己定（手机端空间小，用得比桌面端小）。
pub fn init_with(dir: &Path, max_bytes: u64, echo: bool) {
    let logger = Box::new(FileLog::new(dir, max_bytes, echo));
    if log::set_boxed_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
        log::info!(
            "PeerDesk {} 启动，系统 {} {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// 一行日志：`2026-11-21 08:30:05.123Z INFO 模块: 内容`（UTC 时间，跨时区对日志不会乱）。
#[must_use]
pub fn format_line(
    unix_ms: u64,
    level: log::Level,
    target: &str,
    args: &std::fmt::Arguments<'_>,
) -> String {
    let secs = unix_ms / 1000;
    let (y, m, d) = civil_from_days(secs / 86_400);
    let t = secs % 86_400;
    let module = target.strip_prefix("peerdesk").unwrap_or(target);
    let module = module.trim_start_matches(['_', ':']);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}.{:03}Z {level:<5} {module}: {args}",
        t / 3600,
        t / 60 % 60,
        t % 60,
        unix_ms % 1000
    )
}

/// 1970-01-01 起的天数 → 公历年月日（Howard Hinnant 的算法）。
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use log::Log as _;

    #[test]
    fn lines_have_utc_time_level_and_module() {
        // 2026-11-21 08:30:05.123 UTC
        let ms = 1_795_249_805_123;
        let line = format_line(
            ms,
            log::Level::Info,
            "peerdesk_net::rendezvous",
            &format_args!("直连成功"),
        );
        assert_eq!(
            line,
            "2026-11-21 08:30:05.123Z INFO  net::rendezvous: 直连成功"
        );
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
        let other = format_line(0, log::Level::Warn, "quinn", &format_args!("x"));
        assert_eq!(other, "1970-01-01 00:00:00.000Z WARN  quinn: x");
    }

    #[test]
    fn files_rotate_and_only_our_logs_are_kept() {
        let dir = std::env::temp_dir().join(format!("peerdesk-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let log = FileLog::new(&dir, 40, true);
        let meta = |target, level| log::Metadata::builder().target(target).level(level).build();
        assert!(log.enabled(&meta("peerdesk::stats", log::Level::Info)));
        assert!(!log.enabled(&meta("peerdesk", log::Level::Debug)));
        assert!(!log.enabled(&meta("quinn_proto", log::Level::Info)));
        let record = log::Record::builder()
            .target("peerdesk::stats")
            .level(log::Level::Info)
            .args(format_args!("接收 帧 60"))
            .build();
        log.log(&record);
        log.log(
            &log::Record::builder()
                .target("rustls")
                .args(format_args!("x"))
                .build(),
        );
        log.flush();
        let first = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert!(first.ends_with("stats: 接收 帧 60\n"), "{first}");
        assert_eq!(first.lines().count(), 1, "第三方库的日志不记");
        // 超过上限：旧内容挪到 .1，新文件重新开始
        log.write_line("second");
        assert_eq!(std::fs::read_to_string(dir.join(FILE)).unwrap(), "second\n");
        assert_eq!(std::fs::read_to_string(dir.join(OLD_FILE)).unwrap(), first);
        // 目录不可写（这里用一个文件冒充目录）：静默放弃
        let blocked = FileLog::new(&dir.join(FILE), 40, false);
        blocked.write_line("lost");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 打开全局日志：第二次调用不起作用。之后这个测试进程里其他测试的日志也写进这个目录。
    #[test]
    fn init_once() {
        let dir = std::env::temp_dir().join(format!("peerdesk-init-{}", std::process::id()));
        init(&dir, false);
        init(&dir, false);
        assert!(std::fs::read_to_string(dir.join(FILE))
            .unwrap()
            .contains("启动"));
    }
}
