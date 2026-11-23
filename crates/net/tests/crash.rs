//! 崩溃报告：真实 QUIC 上传到信令服务器，存到设备码簿旁边的 crashes/，同一设备限频。
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::net::SocketAddr;
use std::sync::Arc;

use peerdesk_core::crash::Report;
use peerdesk_core::crypto::Policy;
use peerdesk_net::rendezvous::{upload_crash, Server};
use peerdesk_net::service::SignalService;
use peerdesk_net::{Identity, Node};

#[tokio::test(flavor = "multi_thread")]
async fn crash_reports_are_saved_and_rate_limited() {
    let any: SocketAddr = ([127, 0, 0, 1], 0).into();
    let policy = Policy::recommended();
    let dir = std::env::temp_dir().join(format!("peerdesk-crash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server_id = Identity::generate().unwrap();
    let server_node = Arc::new(Node::bind(any, &server_id, &policy).unwrap());
    let server = Server {
        addr: server_node.local_addr().unwrap(),
        pin: Some(server_id.fingerprint()),
    };
    let _signal =
        SignalService::start_with_codes(server_node, None, Some(dir.join("device-codes.txt")));

    let client = Node::bind(any, &Identity::generate().unwrap(), &policy).unwrap();
    let report = Report::new(
        "PeerDesk 0.1.0 (abc1234) linux：panicked at x",
        b"log line\n",
    );
    upload_crash(&client, &server, &report).await.unwrap();
    let files: Vec<_> = std::fs::read_dir(dir.join("crashes")).unwrap().collect();
    assert_eq!(files.len(), 1);
    let text = std::fs::read_to_string(files[0].as_ref().unwrap().path()).unwrap();
    assert!(text.starts_with("PeerDesk 0.1.0") && text.ends_with("log line\n"));

    // 同一台设备马上再传：被拒绝，不存
    assert!(upload_crash(&client, &server, &report).await.is_err());
    assert_eq!(std::fs::read_dir(dir.join("crashes")).unwrap().count(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}
