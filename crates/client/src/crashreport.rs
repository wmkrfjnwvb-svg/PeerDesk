//! 崩溃报告：发布版遇到 panic 会直接退出，来不及联网。所以崩溃时只在日志目录留一个标记，
//! 下次启动时把标记里的原因和日志的最后一段传给服务器（测试版默认打开，见 `crash-upload` 特性）。

use std::path::Path;

use peerdesk_core::crash::Report;

/// 崩溃标记文件（在日志目录里）。第一行是失败次数，后面是崩溃原因。
pub const MARKER: &str = "crash-pending.txt";
/// 传失败这么多次就放弃（删掉标记），不一直重试。
pub const MAX_ATTEMPTS: u32 = 3;
/// 没有配置服务器时传到这里（测试版的默认服务器）。
pub const DEFAULT_SERVER: &str = "47.95.203.214:4433";

/// 程序版本、提交号、系统。
#[must_use]
pub fn build_info() -> String {
    format!(
        "PeerDesk {} ({}) {} {}",
        env!("CARGO_PKG_VERSION"),
        env!("PEERDESK_COMMIT"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

/// 测试版的默认服务器（不核对证书指纹）。
#[must_use]
pub fn default_server() -> peerdesk_net::rendezvous::Server {
    let addr = std::net::SocketAddr::from(([47, 95, 203, 214], 4433));
    peerdesk_net::rendezvous::Server { addr, pin: None }
}

/// 发布版遇到 panic 直接退出（`panic = "abort"`）：退出前把原因写进日志、留下崩溃标记，
/// 下次启动时上报。原来的处理（打印到标准错误）照常执行。
pub fn install_panic_hook(logs: std::path::PathBuf) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("程序崩溃：{info}");
        record(&logs, &info.to_string());
        default_hook(info);
    }));
}

/// 崩溃时调用：留下标记。写不成也不要紧（只是这次不上报）。
pub fn record(logs: &Path, why: &str) {
    let _ = std::fs::create_dir_all(logs);
    let _ = std::fs::write(logs.join(MARKER), format!("0\n{why}"));
}

/// 有待上报的崩溃时，组装报告（原因 + 日志最后一段，含上一份滚动出去的日志）。
#[must_use]
pub fn pending(logs: &Path) -> Option<(u32, Report)> {
    let marker = std::fs::read_to_string(logs.join(MARKER)).ok()?;
    let (attempts, why) = marker.split_once('\n').unwrap_or(("0", marker.as_str()));
    let attempts = attempts.trim().parse().unwrap_or(0);
    let mut log = std::fs::read(logs.join(crate::logfile::OLD_FILE)).unwrap_or_default();
    log.extend(std::fs::read(logs.join(crate::logfile::FILE)).unwrap_or_default());
    let summary = format!("{}\n{why}", build_info());
    Some((attempts, Report::new(&summary, &log)))
}

/// 上报结束：成功或者失败太多次就删掉标记，否则记下失败次数下次再试。
pub fn finish(logs: &Path, attempts: u32, ok: bool) {
    let path = logs.join(MARKER);
    if ok || attempts.saturating_add(1) >= MAX_ATTEMPTS {
        let _ = std::fs::remove_file(path);
    } else if let Ok(text) = std::fs::read_to_string(&path) {
        let why = text.split_once('\n').map_or("", |(_, w)| w);
        let _ = std::fs::write(path, format!("{}\n{why}", attempts.saturating_add(1)));
    }
}

/// 在后台线程里把待上报的崩溃传给 `server`（没有配置服务器时传给 [`DEFAULT_SERVER`]）。
/// 关掉 `crash-upload` 特性时什么都不做。
pub fn upload_in_background(data_dir: &Path, server: Option<peerdesk_net::rendezvous::Server>) {
    let logs = data_dir.join("logs");
    let Some((attempts, report)) = pending(&logs).filter(|_| cfg!(feature = "crash-upload")) else {
        return;
    };
    let server = server.unwrap_or_else(default_server);
    let data_dir = data_dir.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("peerdesk-crash-upload".into())
        .spawn(move || {
            let result = crate::remote::upload_crash(&data_dir, &server, &report);
            match &result {
                Ok(()) => log::info!("上次崩溃的日志已传给服务器 {}", server.addr),
                Err(e) => log::warn!("上次崩溃的日志没传成（第 {} 次）：{e}", attempts + 1),
            }
            finish(&logs, attempts, result.is_ok());
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_lifecycle() {
        let dir = std::env::temp_dir().join(format!("peerdesk-crashrep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(pending(&dir).is_none());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::logfile::OLD_FILE), "old\n").unwrap();
        std::fs::write(dir.join(crate::logfile::FILE), "new\n").unwrap();
        record(&dir, "panicked at src/x.rs:1");
        let (attempts, report) = pending(&dir).unwrap();
        assert_eq!(attempts, 0);
        assert!(
            report.summary.starts_with("PeerDesk ")
                && report.summary.ends_with("panicked at src/x.rs:1")
        );
        assert_eq!(report.log, b"old\nnew\n");
        // 失败：记次数，原因保留
        finish(&dir, 0, false);
        let (attempts, report) = pending(&dir).unwrap();
        assert_eq!(attempts, 1);
        assert!(report.summary.ends_with("panicked at src/x.rs:1"));
        // 失败太多次：放弃
        finish(&dir, MAX_ATTEMPTS - 1, false);
        assert!(pending(&dir).is_none());
        // 成功：删掉
        record(&dir, "again");
        finish(&dir, 0, true);
        assert!(pending(&dir).is_none());
        // 标记被删以后再记失败：什么都不做
        finish(&dir, 0, false);
        assert!(pending(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uploads_to_a_real_server_in_the_background() {
        use std::sync::Arc;
        let dir = std::env::temp_dir().join(format!("peerdesk-crashup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let (server, _signal) = rt.block_on(async {
            let id = peerdesk_net::Identity::generate().unwrap();
            let policy = peerdesk_core::crypto::Policy::recommended();
            let node = Arc::new(
                peerdesk_net::Node::bind(([127, 0, 0, 1], 0).into(), &id, &policy).unwrap(),
            );
            let server = peerdesk_net::rendezvous::Server {
                addr: node.local_addr().unwrap(),
                pin: Some(id.fingerprint()),
            };
            let signal = peerdesk_net::service::SignalService::start_with_codes(
                node,
                None,
                Some(dir.join("server").join("device-codes.txt")),
            );
            (server, signal)
        });
        let client = dir.join("client");
        record(&client.join("logs"), "panicked at test");
        upload_in_background(&client, Some(server));

        // 后台线程传完会删掉标记，服务器那边多了一份报告
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while pending(&client.join("logs")).is_some() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(pending(&client.join("logs")).is_none());
        assert_eq!(
            std::fs::read_dir(dir.join("server").join("crashes"))
                .unwrap()
                .count(),
            1
        );
        // 马上又崩：服务器限频拒绝，标记留着、记一次失败，下次再试
        record(&client.join("logs"), "panicked again");
        upload_in_background(&client, Some(server));
        while pending(&client.join("logs")).is_some_and(|(n, _)| n == 0)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert_eq!(pending(&client.join("logs")).map(|(n, _)| n), Some(1));
        drop(rt);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panic_hook_leaves_a_marker() {
        let dir = std::env::temp_dir().join(format!("peerdesk-hook-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        install_panic_hook(dir.clone());
        let caught = std::panic::catch_unwind(|| std::panic::panic_any("测试用的崩溃"));
        // 换回默认处理，免得影响别的测试
        let _ = std::panic::take_hook();
        assert!(caught.is_err());
        let (_, report) = pending(&dir).unwrap();
        assert!(
            report.summary.contains("测试用的崩溃"),
            "{}",
            report.summary
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_info_has_commit() {
        assert!(build_info().contains(env!("PEERDESK_COMMIT")));
        assert_eq!(default_server().addr.to_string(), DEFAULT_SERVER);
        // 没有待上报的崩溃时直接返回
        upload_in_background(&std::env::temp_dir().join("peerdesk-no-such-dir"), None);
    }
}
