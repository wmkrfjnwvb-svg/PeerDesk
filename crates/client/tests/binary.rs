//! 对编译出的 `peerdesk` 二进制做冒烟测试。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use peerdesk_core::cluster::ClusterConfig;
use peerdesk_core::config::ServerConfig;

const BIN: &str = env!("CARGO_BIN_EXE_peerdesk");

#[test]
fn version_and_exit_codes() {
    let out = Command::new(BIN).arg("version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .starts_with("peerdesk "));

    let out = Command::new(BIN).arg("frobnicate").output().unwrap();
    assert_eq!(out.status.code(), Some(2));

    let out = Command::new(BIN)
        .args(["sim", "--frames", "10"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("送达 10"));
}

/// 不带参数时打开图形界面；没有显示器时报错退出，而不是卡住。
#[cfg(target_os = "linux")]
#[test]
fn gui_without_a_display_fails_cleanly() {
    let out = Command::new(BIN)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("PEERDESK_DATA_DIR", temp_dir("gui"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    // 两种环境，两种退出码，语义都对：
    // - 机器上没装图形系统库（CI 的覆盖率机器）：程序在 libcheck 那一步就退出，重试没用，
    //   退出码是 EXIT_NO_RETRY（守护不重试），stderr 里说明缺哪些库；
    // - 装了库（开发机）：走到“没有显示器”，退出码 1（守护会重试），stderr 里提到 GUI。
    if stderr.contains("Missing system libraries") {
        assert_eq!(out.status.code(), Some(i32::from(peerdesk::EXIT_NO_RETRY)));
    } else {
        assert_eq!(out.status.code(), Some(1));
        assert!(stderr.contains("GUI"), "{stderr}");
    }
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-bin-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 进程内启动一个开了信令和中继的 `peerdesk-server` 节点。
fn start_server(dir: &std::path::Path) -> peerdesk_server::Node {
    let cfg = ServerConfig::from_toml(&format!(
        "node_id = 1\nregion = \"us\"\nhttp_listen = \"127.0.0.1:0\"\n\
         cluster_listen = \"127.0.0.1:0\"\nadvertise = \"127.0.0.1:1\"\n\
         signal_listen = \"127.0.0.1:0\"\nrelay_listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n",
        dir.to_str().unwrap()
    ))
    .unwrap();
    peerdesk_server::Node::start(&cfg, ClusterConfig::default()).unwrap()
}

fn status(node: &peerdesk_server::Node) -> String {
    let mut stream = TcpStream::connect(node.http_addr()).unwrap();
    stream
        .write_all(b"GET /api/status HTTP/1.1\r\nHost: test\r\n\r\n")
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    out
}

/// 通过服务器跑一次完整会话：服务器、被控端（`host --server`）、控制端（`connect --device`）。
/// 返回控制端的输出和会话结束后服务器的状态。
fn session_through_server(tag: &str, extra: &[&str]) -> (String, String) {
    let (server_dir, host_dir, ctrl_dir) = (
        temp_dir(&format!("{tag}-server")),
        temp_dir(&format!("{tag}-host")),
        temp_dir(&format!("{tag}-ctrl")),
    );
    let server = start_server(&server_dir);
    let info = server.signal().unwrap();
    let (addr, server_pin) = (
        info.addr.to_string(),
        peerdesk_net::identity::to_hex(&info.fingerprint),
    );

    let mut host = Command::new(BIN)
        .args([
            "host",
            "--listen",
            "127.0.0.1:0",
            "--open",
            "--once",
            "--source",
            "demo",
        ])
        .args(["--server", &addr, "--server-pin", &server_pin])
        .arg("--data-dir")
        .arg(&host_dir)
        // 日志也写到这个目录（失败时附上，看被控端编码器的情况）
        .env("PEERDESK_DATA_DIR", &host_dir)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (tx, lines) = mpsc::channel();
    let stdout = host.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let mut host_pin = None;
    loop {
        let line = lines.recv_timeout(Duration::from_secs(20)).unwrap();
        if let Some(pin) = line.strip_prefix("指纹 ") {
            host_pin = Some(pin.to_owned());
        }
        if line.starts_with("已在服务器") {
            break;
        }
    }
    let host_pin = host_pin.unwrap();
    assert!(status(&server).contains("\"signal\":{\"enabled\":true,\"online\":1}"));

    // 会话 3 秒：CI 的 Apple 芯片虚拟机上 VideoToolbox 建编码会话加第一帧约 0.6 秒（2026-11-25 日志：
    // 连上 0.53 秒后才出第一帧，半分辨率升到原尺寸又建一次），解码端也要建两次；1 秒的会话里控制端
    // 一帧都没解出来。这里测的是经服务器连上、出画面，首帧快慢由端到端测试卡门槛。
    let out = Command::new(BIN)
        .args(["connect", "--server", &addr, "--server-pin", &server_pin])
        .args(["--device", &host_pin, "--seconds", "3"])
        .args(extra)
        .arg("--data-dir")
        .arg(&ctrl_dir)
        .env("PEERDESK_DATA_DIR", &ctrl_dir)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    // 失败时附上两边的日志（编码器、关键帧、解码器）
    let logs = |dir: &std::path::Path| {
        std::fs::read_to_string(dir.join("logs").join("peerdesk.log")).unwrap_or_default()
    };
    let text = format!(
        "{text}\n--- 被控端日志\n{}\n--- 控制端日志\n{}",
        logs(&host_dir),
        logs(&ctrl_dir)
    );
    assert!(out.status.success(), "{text}");
    // 硬件编码器中途出错或每帧都是关键帧而改用软件：是代码或用法的问题，不能悄悄过去。
    // （没有显卡时一开始就用软件编码，日志写的是“不用硬件编码”，不算。）
    assert!(!logs(&host_dir).contains("改用软件编码"), "{text}");
    assert!(!logs(&ctrl_dir).contains("改用软件解码"), "{text}");
    assert!(text.contains(&format!("对端指纹 {host_pin}")), "{text}");
    assert!(text.contains("画面尺寸      1280×720"), "{text}");
    assert!(host.wait().unwrap().success());
    reader.join().unwrap();
    let status = status(&server);
    server.stop();
    for dir in [server_dir, host_dir, ctrl_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }
    (text, status)
}

#[test]
fn connect_by_device_prefers_the_direct_path() {
    let (text, status) = session_through_server("auto", &[]);
    assert!(text.contains("连接路径      直连"), "{text}");
    assert!(
        status.contains("\"relay\":{\"enabled\":true,\"sessions\":"),
        "{status}"
    );
    // 直连不经过中继：中继只转发了测速用的几个探测包，没有会话数据
    let relay_bytes = relay_bytes(&status);
    assert!(relay_bytes < 4_000, "直连不经过中继：{status}");
}

/// 状态接口里中继转发的字节数。
fn relay_bytes(status: &str) -> u64 {
    let after = status.split("\"relay\":").nth(1).unwrap();
    let digits: String = after
        .split("\"bytes\":")
        .nth(1)
        .unwrap()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap()
}

#[test]
fn connect_by_device_over_the_relay() {
    let (text, status) = session_through_server("relay", &["--relay-only"]);
    assert!(text.contains("连接路径      中继"), "{text}");
    assert!(relay_bytes(&status) > 10_000, "会话经过中继：{status}");
}
