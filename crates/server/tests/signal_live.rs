//! 服务端的信令和中继：用真实的 QUIC 和 UDP 在回环地址上启动、登记设备、读状态。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use peerdesk_core::cluster::ClusterConfig;
use peerdesk_core::config::ServerConfig;
use peerdesk_core::crypto::Policy;
use peerdesk_net::identity::to_hex;
use peerdesk_net::rendezvous::{register, Server};
use peerdesk_net::{Identity, Node as NetNode};
use peerdesk_server::node::launch;
use peerdesk_server::Node;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-signal-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config_text(extra: &str, dir: &Path) -> String {
    format!(
        "node_id = 1\nregion = \"us\"\nhttp_listen = \"127.0.0.1:0\"\n\
         cluster_listen = \"127.0.0.1:0\"\nadvertise = \"127.0.0.1:1\"\n\
         signal_listen = \"127.0.0.1:0\"\ndata_dir = {:?}\n{extra}",
        dir.to_str().unwrap()
    )
}

fn start(extra: &str, dir: &Path) -> Result<Node, peerdesk_core::Error> {
    Node::start(
        &ServerConfig::from_toml(&config_text(extra, dir)).unwrap(),
        ClusterConfig::default(),
    )
}

fn status(node: &Node) -> String {
    let mut stream = TcpStream::connect(node.http_addr()).unwrap();
    stream
        .write_all(b"GET /api/status HTTP/1.1\r\nHost: test\r\n\r\n")
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    out
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "等待超时：{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn signal_and_relay_serve_devices_and_report_status() {
    let dir = temp_dir("full");
    let node = start("relay_listen = \"127.0.0.1:0\"\n", &dir).unwrap();
    let info = node.signal().unwrap();
    assert!(info.addr.ip().is_loopback() && info.addr.port() != 0);
    let relay = info.relay.unwrap();
    assert!(relay.ip().is_loopback() && relay.port() != 0);
    assert!(status(&node).contains(
        "\"signal\":{\"enabled\":true,\"online\":0},\"relay\":{\"enabled\":true,\"sessions\":0,"
    ));

    // 一台设备登记上来：状态里的在线数变成 1
    let rt = tokio::runtime::Runtime::new().unwrap();
    let device = Identity::generate().unwrap();
    let registration = rt.block_on(async {
        let me =
            NetNode::bind(([127, 0, 0, 1], 0).into(), &device, &Policy::recommended()).unwrap();
        let server = Server {
            addr: info.addr,
            pin: Some(info.fingerprint),
        };
        (register(&me, &server).await.unwrap(), me)
    });
    wait_for("设备在线", || node.rendezvous().online == Some(1));
    assert!(status(&node).contains("\"signal\":{\"enabled\":true,\"online\":1}"));
    drop(registration);
    drop(rt);
    node.stop();

    // 身份是持久的：用同一个数据目录重启，指纹不变
    let again = start("", &dir).unwrap();
    let info2 = again.signal().unwrap();
    assert_eq!(info2.fingerprint, info.fingerprint);
    assert_eq!(info2.relay, None);
    assert!(status(&again).contains("\"relay\":{\"enabled\":false,"));
    assert_eq!(
        to_hex(&Identity::load_or_create(&dir).unwrap().fingerprint()),
        to_hex(&info.fingerprint)
    );
    again.stop();
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn signal_startup_failures_are_reported() {
    let dir = temp_dir("fail");
    // 数据目录其实是一个文件：保存不了身份
    let file = dir.join("not-a-dir");
    std::fs::write(&file, b"x").unwrap();
    let err = start("", &file).unwrap_err().to_string();
    assert!(err.contains("启动信令服务失败"), "{err}");
    // 中继端口、信令端口被占
    let taken = UdpSocket::bind("127.0.0.1:0").unwrap();
    let taken: SocketAddr = taken.local_addr().unwrap();
    let err = start(&format!("relay_listen = \"{taken}\"\n"), &dir)
        .unwrap_err()
        .to_string();
    assert!(err.contains("启动中继失败"), "{err}");
    let text = config_text("", &dir).replace(
        "signal_listen = \"127.0.0.1:0\"",
        &format!("signal_listen = \"{taken}\""),
    );
    let err = Node::start(
        &ServerConfig::from_toml(&text).unwrap(),
        ClusterConfig::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("绑定"), "{err}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn launch_prints_the_server_fingerprint() {
    let dir = temp_dir("launch");
    for (extra, relay_line) in [
        ("relay_listen = \"127.0.0.1:0\"\n", "中继 127.0.0.1:"),
        ("", "中继 未开启"),
    ] {
        let path = dir.join("server.toml");
        std::fs::write(&path, config_text(extra, &dir)).unwrap();
        let mut out = Vec::new();
        let node = launch(&[path.to_str().unwrap().to_owned()], &mut out).unwrap();
        let fingerprint = to_hex(&node.signal().unwrap().fingerprint);
        node.stop();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains(&format!("服务器指纹 {fingerprint}")),
            "{text}"
        );
        assert!(text.contains(relay_line), "{text}");
    }
    let _ = std::fs::remove_dir_all(dir);
}
