//! 集群、中继计费和故障切换：三个真实的服务端节点（一个付费、两个免费）跑在本机上，
//! 被控端在所有节点登记，控制端按设备码连接。
//!
//! - 会员（带令牌）的中继放在付费节点上，免费用户的放在免费节点上；
//! - 中继用量记在会员账户上，写进账本文件，节点重启后读回；
//! - 免费额度用完时，中继停止转发并通知客户端；
//! - 控制端配置的那个节点崩了，换集群里的其他节点照样连上；
//! - 管理接口：集群总览、用量、签发会员令牌、加入节点，写操作要管理令牌。

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::panic
)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::billing::issue;
use peerdesk_core::cluster::ClusterConfig;
use peerdesk_core::config::ServerConfig;
use peerdesk_core::crypto::Policy;
use peerdesk_core::proto::Channel;
use peerdesk_net::rendezvous::{
    answer, dial_via, fetch_nodes, register, Lookup, Path as Route, PathPolicy, Server,
};
use peerdesk_net::{Identity, Link, Node};
use peerdesk_server::node::Node as ServerNode;

const SECRET: &str = "cluster-secret-for-tests-0123456789";
const ADMIN: &str = "admin-token-for-tests-0123456789";

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn tcp_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// 一个节点的端口（固定下来，重启时用同样的端口和数据目录）。
#[derive(Clone)]
struct Spec {
    id: u64,
    role: &'static str,
    http: u16,
    cluster: u16,
    signal: u16,
    relay: u16,
    dir: PathBuf,
    free_mb: u64,
}

impl Spec {
    fn new(id: u64, role: &'static str, base: &Path, free_mb: u64) -> Self {
        Self {
            id,
            role,
            http: tcp_port(),
            cluster: free_port(),
            signal: free_port(),
            relay: free_port(),
            dir: base.join(format!("node{id}")),
            free_mb,
        }
    }

    fn toml(&self, peers: &[&Spec]) -> String {
        let peers: Vec<String> = peers
            .iter()
            .map(|p| format!("\"127.0.0.1:{}\"", p.cluster))
            .collect();
        format!(
            r#"node_id = {id}
region = "cn"
role = "{role}"
http_listen = "127.0.0.1:{http}"
cluster_listen = "127.0.0.1:{cluster}"
advertise = "127.0.0.1:{cluster}"
peers = [{peers}]
cluster_secret = "{SECRET}"
admin_token = "{ADMIN}"
signal_listen = "127.0.0.1:{signal}"
relay_listen = "127.0.0.1:{relay}"
data_dir = "{dir}"
relay_price_cents_per_gb = 80
free_device_mb = {free_mb}
free_ip_mb = 0
bill_every_ms = 200
"#,
            id = self.id,
            role = self.role,
            http = self.http,
            cluster = self.cluster,
            signal = self.signal,
            relay = self.relay,
            dir = self.dir.display(),
            free_mb = self.free_mb,
            peers = peers.join(","),
        )
    }

    /// 启动节点。节点有自己的异步运行时，要在测试的运行时之外（普通线程里）启动和停止。
    fn start(&self, peers: &[&Spec]) -> ServerNode {
        let cfg = ServerConfig::from_toml(&self.toml(peers)).unwrap();
        let timing = ClusterConfig {
            heartbeat_us: 100_000,
            suspect_after_us: 500_000,
            dead_after_us: 1_000_000,
        };
        std::thread::spawn(move || ServerNode::start(&cfg, timing).unwrap())
            .join()
            .unwrap()
    }
}

fn stop(node: ServerNode) {
    std::thread::spawn(move || node.stop()).join().unwrap();
}

impl Spec {}

fn http(port: u16, method: &str, path: &str, token: Option<&str>, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let auth = token.map_or_else(String::new, |t| format!("Authorization: Bearer {t}\r\n"));
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: x\r\n{auth}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    let status = response
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..200 {
        if ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("等待超时：{what}");
}

/// 被控端：在所有节点登记，响应连接请求，把接受到的连接交出来。
fn spawn_host(host: Arc<Node>, servers: Vec<Server>) -> tokio::sync::mpsc::Receiver<Link> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    for server in servers {
        let host = Arc::clone(&host);
        tokio::spawn(async move {
            let mut registration = register(&host, &server).await.unwrap();
            let mut shims = Vec::new();
            while let Some(incoming) = registration.incoming.recv().await {
                shims.push(answer(&host, &incoming).await.ok().flatten());
            }
        });
    }
    tokio::spawn(async move {
        while let Some(link) = host.accept().await {
            if let Ok(link) = link {
                if tx.send(link).await.is_err() {
                    break;
                }
            }
        }
    });
    rx
}

/// 经中继来回发 `n` 个 1000 字节的数据报，返回对方收到的个数。
async fn push(from: &Link, to: &Link, n: usize) -> usize {
    for _ in 0..n {
        let _ = from.send(Channel::Video, &[7u8; 1000]);
        tokio::time::sleep(Duration::from_micros(300)).await;
    }
    let mut got = 0;
    while let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(300), to.recv()).await {
        got += 1;
    }
    got
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paid_and_free_relays_billing_and_failover() {
    let base = std::env::temp_dir().join(format!("peerdesk-cluster-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (p1, f2, f3) = (
        Spec::new(1, "paid", &base, 0),
        Spec::new(2, "free", &base, 1),
        Spec::new(3, "free", &base, 1),
    );
    let n1 = p1.start(&[&f2, &f3]);
    let n2 = f2.start(&[&p1, &f3]);
    let n3 = f3.start(&[&p1, &f2]);
    let server = |n: &ServerNode| {
        let info = n.signal().unwrap();
        Server {
            addr: info.addr,
            pin: Some(info.fingerprint),
        }
    };
    let (s1, s2, s3) = (server(&n1), server(&n2), server(&n3));
    wait_until("集群互相发现", || n2.alive_count() == 3).await;

    // 节点列表：从任何一个节点问到全部三个，带指纹和角色
    let policy = Policy::recommended();
    let ctrl_id = Identity::generate().unwrap();
    let ctrl = Node::bind(any(), &ctrl_id, &policy).unwrap();
    wait_until("节点公布信令地址", || true).await;
    let nodes = fetch_nodes(&ctrl, &s2).await.unwrap();
    assert_eq!(nodes.len(), 3, "{nodes:?}");
    assert!(nodes.iter().any(|n| n.addr == s1.addr && n.pin == s1.pin));

    // 管理接口：集群总览
    let (status, body) = http(f2.http, "GET", "/api/cluster", None, "");
    assert_eq!(status, 200);
    assert!(
        body.contains("\"role\":\"paid\"") && body.contains("\"role\":\"free\""),
        "{body}"
    );
    // 签发会员令牌：没有令牌 401，有令牌 200
    let (status, _) = http(
        f2.http,
        "POST",
        "/api/accounts",
        Some("wrong-token-xxxxxxxx"),
        "{\"quota_gb\":5}",
    );
    assert_eq!(status, 401);
    let (status, body) = http(
        f2.http,
        "POST",
        "/api/accounts",
        Some(ADMIN),
        "{\"quota_gb\":5}",
    );
    assert_eq!(status, 200, "{body}");
    let token = body
        .split("\"token\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    let account = body
        .split("\"account\":\"")
        .nth(1)
        .unwrap()
        .split('"')
        .next()
        .unwrap()
        .to_owned();
    assert_eq!(
        token,
        issue(
            SECRET.as_bytes(),
            {
                let mut id = [0u8; 8];
                for (i, b) in id.iter_mut().enumerate() {
                    *b = u8::from_str_radix(&account[i * 2..i * 2 + 2], 16).unwrap();
                }
                id
            },
            5
        )
    );
    let (status, _) = http(
        f2.http,
        "POST",
        "/api/peers",
        Some(ADMIN),
        "{\"addr\":\"bad\"}",
    );
    assert_eq!(status, 400);
    let (status, _) = http(
        f2.http,
        "POST",
        "/api/peers",
        Some(ADMIN),
        "{\"addr\":\"127.0.0.1:9\"}",
    );
    assert_eq!(status, 200);

    // 被控端在三个节点都登记
    let host_id = Identity::generate().unwrap();
    let host = Arc::new(Node::bind(any(), &host_id, &policy).unwrap());
    let mut accepted = spawn_host(Arc::clone(&host), vec![s1, s2, s3]);
    let device = Lookup::Device(host_id.fingerprint());
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 会员：中继在付费节点 1 上
    let vip = dial_via(&ctrl, &[s2], &device, PathPolicy::RelayOnly, &token)
        .await
        .unwrap();
    assert_eq!(vip.path, Route::Relay);
    assert!(
        vip.relay_note.contains("会员通道（付费节点 1）"),
        "{}",
        vip.relay_note
    );
    let host_link = accepted.recv().await.unwrap();
    assert!(push(&vip.link, &host_link, 300).await > 250);
    vip.link.close("done");
    // 记账：会员账户在节点 1 上有用量，写进了账本文件
    wait_until("会员用量记账", || {
        http(p1.http, "GET", "/api/usage", None, "")
            .1
            .contains(&account)
    })
    .await;
    let usage_dir = p1.dir.join("usage");
    let logged = std::fs::read_dir(&usage_dir).unwrap().count();
    assert_eq!(logged, 1, "本月一个账本文件");

    // 免费用户：中继在免费节点上；免费额度 1 MB 用完后停止转发并通知
    let free = dial_via(&ctrl, &[s2], &device, PathPolicy::RelayOnly, "")
        .await
        .unwrap();
    assert!(
        free.relay_note.starts_with("免费通道"),
        "{}",
        free.relay_note
    );
    let host_link = accepted.recv().await.unwrap();
    let got = push(&free.link, &host_link, 1_500).await;
    assert!(got < 1_200, "超过 1 MB 后不再转发：收到 {got}");
    let shim = free.shim.as_ref().unwrap();
    wait_until("免费额度通知", || shim.notice().is_some()).await;
    assert_eq!(
        shim.notice().unwrap().0,
        peerdesk_core::relay::NOTICE_FREE_LIMIT
    );
    let (_, usage) = http(f2.http, "GET", "/api/usage", None, "");
    assert!(usage.contains("\"window_secs\":600"), "{usage}");

    // 节点 1 重启：账本读回，会员用量还在
    stop(n1);
    let n1 = p1.start(&[&f2, &f3]);
    let (_, usage) = http(p1.http, "GET", "/api/usage", None, "");
    assert!(usage.contains(&account), "重启后读回：{usage}");

    // 控制端配置的节点 2 崩了：换集群里的其他节点连上
    stop(n2);
    let ctrl2 = Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap();
    let failover = dial_via(&ctrl2, &[s2, s3, s1], &device, PathPolicy::Auto, "")
        .await
        .unwrap();
    let _ = accepted.recv().await.unwrap();
    assert_eq!(failover.device, host_id.fingerprint());
    stop(n1);
    stop(n3);
    let _ = std::fs::remove_dir_all(&base);
}
