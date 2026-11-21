//! 服务端日志：写到标准错误，由 systemd（journald）收集并加时间戳。
//! 查看：`journalctl -u peerdesk-server -f`。

use std::io::Write;

/// 写标准错误的日志器。
#[derive(Debug)]
pub struct Stderr;

impl log::Log for Stderr {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.target().starts_with("peerdesk") && metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let _ = writeln!(std::io::stderr(), "{}", line(record));
        }
    }

    fn flush(&self) {}
}

/// 一行日志：`INFO  net::service: 内容`。
#[must_use]
pub fn line(record: &log::Record<'_>) -> String {
    let target = record.target();
    let module = target
        .strip_prefix("peerdesk")
        .unwrap_or(target)
        .trim_start_matches(['_', ':']);
    format!("{:<5} {module}: {}", record.level(), record.args())
}

/// 打开日志。只能打开一次，再次调用不起作用。
pub fn init() {
    if log::set_logger(&Stderr).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use log::Log as _;

    #[test]
    fn lines_and_filtering() {
        let record = log::Record::builder()
            .target("peerdesk_net::service")
            .level(log::Level::Info)
            .args(format_args!("设备登记"))
            .build();
        assert_eq!(line(&record), "INFO  net::service: 设备登记");
        Stderr.log(&record);
        Stderr.flush();
        let meta = log::Metadata::builder().target("quinn").build();
        assert!(!Stderr.enabled(&meta));
        init();
        init();
    }
}
