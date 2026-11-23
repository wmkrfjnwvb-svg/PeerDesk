//! 真实进程内的多节点测试：三个节点用真实的 UDP 和 TCP 在回环地址上互联。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use peerdesk_core::cluster::ClusterConfig;
use peerdesk_core::config::ServerConfig;
use peerdesk_server::Node;

/// 测试用的快节奏：50 毫秒一次心跳，400 毫秒判死。
const FAST: ClusterConfig = ClusterConfig {
    heartbeat_us: 50_000,
    suspect_after_us: 200_000,
    dead_after_us: 400_000,
};

fn free_udp_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(id: u64, region: &str, port: u16, peers: &[u16]) -> ServerConfig {
    let peers: Vec<String> = peers.iter().map(|p| format!("\"127.0.0.1:{p}\"")).collect();
    ServerConfig::from_toml(&format!(
        "node_id = {id}\nregion = \"{region}\"\nhttp_listen = \"127.0.0.1:0\"\n\
         cluster_listen = \"127.0.0.1:{port}\"\nadvertise = \"127.0.0.1:{port}\"\n\
         peers = [{}]\nregion_fallback = [\"us\"]\n",
        peers.join(",")
    ))
    .unwrap()
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "等待超时：{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn http(addr: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    out
}

fn get(addr: SocketAddr, path: &str) -> String {
    http(addr, &format!("GET {path} HTTP/1.1\r\nHost: test\r\n\r\n"))
}

#[test]
fn three_nodes_interconnect_route_by_region_and_survive_a_crash() {
    // 先挑端口再绑定：中间别的测试可能抢走其中一个（Windows 上并行跑测试时出现过），抢了就换一组再试。
    let (ports, n1, n2, n3) = (0..5)
        .find_map(|_| {
            let ports = [free_udp_port(), free_udp_port(), free_udp_port()];
            // 节点 2、3 只配置了节点 1 作为种子。
            let n1 = Node::start(&config(1, "us", ports[0], &[]), FAST).ok()?;
            let n2 = Node::start(&config(2, "us", ports[1], &[ports[0]]), FAST).ok()?;
            let n3 = Node::start(&config(3, "eu", ports[2], &[ports[0]]), FAST).ok()?;
            Some((ports, n1, n2, n3))
        })
        .unwrap();
    assert_eq!(n1.cluster_addr().port(), ports[0]);

    wait_for("三个节点互相发现", || {
        n1.alive_count() == 3 && n2.alive_count() == 3 && n3.alive_count() == 3
    });

    // 管理接口
    let status = get(n1.http_addr(), "/api/status");
    assert!(status.starts_with("HTTP/1.1 200 OK"));
    assert!(status.contains("\"alive\":3"));
    assert!(get(n1.http_addr(), "/healthz").ends_with("ok\n"));
    assert!(get(n1.http_addr(), "/").contains("<html"));

    // 同区域优先：eu 的客户端无论问哪个节点，都被导向节点 3
    for node in [&n1, &n2, &n3] {
        assert!(get(node.http_addr(), "/api/route?region=eu").contains("\"id\":3"));
    }

    // 负载溢出：节点 1 过载后，us 的客户端被导向节点 2
    n1.set_load(950, 100);
    wait_for("节点 2 得知节点 1 过载", || {
        get(n2.http_addr(), "/api/status").contains("\"load_permille\":950")
    });
    assert!(get(n2.http_addr(), "/api/route?region=us").contains("\"id\":2"));

    // 节点 3 崩溃：其余节点继续服务，eu 的客户端按回退顺序去 us
    n3.stop();
    wait_for("节点 3 被判定死亡", || {
        [&n1, &n2]
            .iter()
            .all(|n| get(n.http_addr(), "/api/status").contains("\"state\":\"dead\""))
    });
    assert_eq!((n1.alive_count(), n2.alive_count()), (2, 2));
    let route = get(n2.http_addr(), "/api/route?region=eu");
    assert!(route.starts_with("HTTP/1.1 200 OK"), "{route}");
    assert!(route.contains("\"id\":2"), "{route}");

    // 用同样的标识和端口重启节点 3：自动回到集群
    let n3 = Node::start(&config(3, "eu", ports[2], &[ports[0]]), FAST).unwrap();
    wait_for("节点 3 回到集群", || {
        n1.alive_count() == 3 && n2.alive_count() == 3 && n3.alive_count() == 3
    });
    assert!(get(n1.http_addr(), "/api/route?region=eu").contains("\"id\":3"));

    n1.stop();
    n2.stop();
    n3.stop();
}

#[test]
fn http_rejects_malformed_and_oversized_requests() {
    let node = Node::start(&config(9, "us", free_udp_port(), &[]), FAST).unwrap();
    let addr = node.http_addr();
    assert!(http(addr, "nonsense\r\n\r\n").starts_with("HTTP/1.1 400"));
    assert!(http(addr, "POST / HTTP/1.1\r\n\r\n").starts_with("HTTP/1.1 405"));
    assert!(
        get(addr, "/api/route?region=mars").starts_with("HTTP/1.1 200"),
        "单节点时回退到自己"
    );
    // 没配 admin_token 的节点不能做管理操作
    let body = "{\"quota_gb\":1}";
    let req = format!(
        "POST /api/accounts HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    assert!(http(addr, &req).starts_with("HTTP/1.1 403"));
    assert!(get(addr, "/api/usage").contains("\"window_secs\""));
    assert!(get(addr, "/api/cluster").contains("\"nodes\""));
    let huge = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(20_000));
    assert!(http(addr, &huge).starts_with("HTTP/1.1 431"));
    // 不是 UTF-8 的请求头
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .write_all(&[0xff, 0xfe, b'\r', b'\n', b'\r', b'\n'])
        .unwrap();
    let mut out = String::new();
    stream.read_to_string(&mut out).unwrap();
    assert!(out.starts_with("HTTP/1.1 431"));
    // 连接后什么都不发就关闭写端
    let stream = TcpStream::connect(addr).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    // 集群端口收到垃圾数据不影响节点
    let junk = UdpSocket::bind("127.0.0.1:0").unwrap();
    junk.send_to(&[1, 2, 3], node.cluster_addr()).unwrap();
    junk.send_to(&[], node.cluster_addr()).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(get(addr, "/healthz").ends_with("ok\n"));
    assert_eq!(node.alive_count(), 1);
    assert!(format!("{node:?}").contains("Node"));
    node.stop();
}

#[test]
fn start_fails_cleanly_when_ports_are_taken_or_config_is_bad() {
    let port = free_udp_port();
    let first = Node::start(&config(1, "us", port, &[]), FAST).unwrap();
    // 集群端口被占
    let err = Node::start(&config(2, "us", port, &[]), FAST).unwrap_err();
    assert!(err.to_string().contains("绑定集群端口失败"), "{err}");
    // 管理端口被占
    let mut cfg = config(3, "us", free_udp_port(), &[]);
    cfg.http_listen = first.http_addr().to_string();
    let err = Node::start(&cfg, FAST).unwrap_err();
    assert!(err.to_string().contains("绑定管理端口失败"), "{err}");
    // 配置在解析后被改坏
    let mut cfg = config(4, "us", free_udp_port(), &[]);
    cfg.crypto.preset = Some("bogus".into());
    assert!(Node::start(&cfg, FAST).is_err());
    first.stop();
}

fn secured(id: u64, port: u16, peers: &[u16], secret: Option<&str>) -> ServerConfig {
    let mut cfg = config(id, "us", port, peers);
    cfg.cluster_secret = secret.map(str::to_owned);
    cfg
}

#[test]
fn only_nodes_with_the_shared_secret_can_join() {
    const SECRET: &str = "a-long-shared-secret";
    let ports = [
        free_udp_port(),
        free_udp_port(),
        free_udp_port(),
        free_udp_port(),
    ];
    let n1 = Node::start(&secured(1, ports[0], &[], Some(SECRET)), FAST).unwrap();
    let n2 = Node::start(&secured(2, ports[1], &[ports[0]], Some(SECRET)), FAST).unwrap();
    // 密钥不对的节点，和完全不带认证的节点
    let wrong = Node::start(
        &secured(3, ports[2], &[ports[0]], Some("a-different-secret!")),
        FAST,
    )
    .unwrap();
    let plain = Node::start(&secured(4, ports[3], &[ports[0]], None), FAST).unwrap();

    wait_for("两个有密钥的节点互相发现", || {
        n1.alive_count() == 2 && n2.alive_count() == 2
    });
    // 给另外两个节点足够的时间去尝试
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(n1.alive_count(), 2, "没有密钥的节点进不来");
    assert_eq!(n2.alive_count(), 2);
    assert_eq!(wrong.alive_count(), 1);
    assert_eq!(plain.alive_count(), 1);
    let status = get(n1.http_addr(), "/api/status");
    assert!(status.contains("\"cluster_auth\":true"));
    assert!(
        !status.contains("\"id\":3") && !status.contains("\"id\":4"),
        "{status}"
    );
    assert!(get(plain.http_addr(), "/api/status").contains("\"cluster_auth\":false"));

    // 往集群端口直接发一个没有认证的伪造心跳：被丢弃
    let forged = peerdesk_core::cluster::ClusterMsg::Heartbeat {
        from: peerdesk_core::cluster::NodeInfo {
            id: peerdesk_core::region::NodeId(66),
            region: "us".into(),
            addr: "127.0.0.1:1".into(),
            load_permille: 0,
            sessions: 0,
            signal: String::new(),
            fingerprint: [0; 32],
            role: peerdesk_core::cluster::Role::Free,
            relay: String::new(),
        },
        known: vec![],
    }
    .encode()
    .unwrap();
    let attacker = UdpSocket::bind("127.0.0.1:0").unwrap();
    attacker.send_to(&forged, n1.cluster_addr()).unwrap();
    // 重放一个合法的包：先封装一个带认证的包，发两次，第二次无效（第一次会让节点 77 出现）
    let now_ms = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let legit = peerdesk_core::cluster::seal(SECRET.as_bytes(), now_ms, &forged.clone());
    attacker.send_to(&legit, n1.cluster_addr()).unwrap();
    attacker.send_to(&legit, n1.cluster_addr()).unwrap();
    wait_for("带正确认证的心跳被接受", || {
        get(n1.http_addr(), "/api/status").contains("\"id\":66")
    });
    for node in [n1, n2, wrong, plain] {
        node.stop();
    }
}
