//! 通过信令服务器建立连接：直连、中继、计量。全部用真实的 QUIC 和 UDP 跑在回环地址上。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::crypto::Policy;
use peerdesk_core::paths::{Candidate, PathKind};
use peerdesk_core::proto::Channel;
use peerdesk_core::signal::{Nat, PunchInfo, SignalMsg};
use peerdesk_net::reason::Reason;
use peerdesk_net::rendezvous::{answer, dial, register, Dialed, Lookup, Path, PathPolicy, Server};
use peerdesk_net::service::{RelayService, SignalService};
use peerdesk_net::stream::{read_kind, read_signal, write_kind, write_signal, StreamKind};
use peerdesk_net::{Identity, Link, NetError, Node};

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

struct World {
    server: Server,
    signal: SignalService,
    relay: Option<Arc<RelayService>>,
    host: Arc<Node>,
    host_id: Identity,
    ctrl: Node,
    ctrl_id: Identity,
}

async fn world(with_relay: bool) -> World {
    world_on(with_relay, any(), any()).await
}

/// `server_bind` 是服务器、中继和控制端绑定的地址，`host_bind` 是被控端绑定的地址。
async fn world_on(with_relay: bool, server_bind: SocketAddr, host_bind: SocketAddr) -> World {
    let policy = Policy::recommended();
    let server_id = Identity::generate().unwrap();
    let server_node = Arc::new(Node::bind(server_bind, &server_id, &policy).unwrap());
    let relay = if with_relay {
        Some(Arc::new(
            RelayService::start(server_bind, None).await.unwrap(),
        ))
    } else {
        None
    };
    let server = Server {
        addr: server_node.local_addr().unwrap(),
        pin: Some(server_id.fingerprint()),
    };
    let signal = SignalService::start(server_node, relay.clone());
    let (host_id, ctrl_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    World {
        server,
        signal,
        relay,
        host: Arc::new(Node::bind(host_bind, &host_id, &policy).unwrap()),
        ctrl: Node::bind(server_bind, &ctrl_id, &policy).unwrap(),
        host_id,
        ctrl_id,
    }
}

/// 被控端：登记，然后对每个连接请求打洞并接上中继，把接受到的连接交出来。
fn spawn_host(w: &World) -> tokio::sync::mpsc::Receiver<Link> {
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let (node, server) = (Arc::clone(&w.host), w.server);
    tokio::spawn(async move {
        let mut registration = register(&node, &server).await.unwrap();
        assert_eq!(
            registration.observed.port(),
            node.local_addr().unwrap().port(),
            "回环上没有 NAT，服务器看到的端口就是本机端口"
        );
        let mut shims = Vec::new();
        loop {
            tokio::select! {
                Some(incoming) = registration.incoming.recv() => {
                    shims.push(answer(&node, &incoming).await.unwrap());
                }
                Some(link) = node.accept() => {
                    if tx.send(link.unwrap()).await.is_err() { break; }
                }
            }
        }
    });
    rx
}

async fn wait_online(w: &World) {
    for _ in 0..200 {
        if w.signal.online() == 1 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("被控端没有登记上");
}

async fn echo(dialed: &Dialed, host_link: &Link) {
    dialed
        .link
        .send(Channel::Video, b"from controller")
        .unwrap();
    assert_eq!(host_link.recv().await.unwrap().1, b"from controller");
    host_link.send(Channel::Video, &[7u8; 1000]).unwrap();
    assert_eq!(dialed.link.recv().await.unwrap().1, vec![7u8; 1000]);
}

#[tokio::test(flavor = "multi_thread")]
async fn direct_path_is_preferred() {
    let w = world(true).await;
    let mut accepted = spawn_host(&w);
    wait_online(&w).await;
    let target = w.host_id.fingerprint();
    let dialed = dial(
        &w.ctrl,
        &w.server,
        &Lookup::Device(target),
        PathPolicy::Auto,
    )
    .await
    .unwrap();
    assert_eq!(dialed.path, Path::Direct);
    assert!(dialed.shim.is_none() && dialed.relay_session.is_none());
    assert_eq!(dialed.link.remote_addr(), w.host.local_addr().unwrap());
    let host_link = accepted.recv().await.unwrap();
    assert_eq!(host_link.peer_fingerprint(), &w.ctrl_id.fingerprint());
    echo(&dialed, &host_link).await;
    // 直连不经过中继：中继只转发了测速用的几个探测包，没有会话数据
    let (total, _, _) = w.relay.as_ref().unwrap().totals();
    assert!(total.bytes < 4_000, "{}", total.bytes);
}

#[tokio::test(flavor = "multi_thread")]
async fn relay_path_carries_the_session_and_is_metered() {
    let w = world(true).await;
    let mut accepted = spawn_host(&w);
    wait_online(&w).await;
    let target = w.host_id.fingerprint();
    let dialed = dial(
        &w.ctrl,
        &w.server,
        &Lookup::Device(target),
        PathPolicy::RelayOnly,
    )
    .await
    .unwrap();
    assert_eq!(dialed.path, Path::Relay);
    let shim_addr = dialed.shim.as_ref().unwrap().local_addr();
    assert_eq!(
        dialed.link.remote_addr(),
        shim_addr,
        "控制端的 QUIC 以为对端是本机转发器"
    );
    // 端到端的身份校验照常生效：中继看不到也改不了内容
    assert_eq!(dialed.link.peer_fingerprint(), &target);
    let host_link = accepted.recv().await.unwrap();
    assert_eq!(host_link.peer_fingerprint(), &w.ctrl_id.fingerprint());
    assert_ne!(
        host_link.remote_addr(),
        w.ctrl.local_addr().unwrap(),
        "被控端看到的也是本机转发器"
    );
    echo(&dialed, &host_link).await;

    // 计量：这个会话的用量记在控制端名下，至少包含握手和上面两个数据报
    let relay = w.relay.as_ref().unwrap();
    let session = dialed.relay_session.unwrap();
    let usage = relay.usage(&session).unwrap();
    assert!(usage.bytes > 3000, "{usage:?}");
    assert!(usage.packets > 4, "{usage:?}");
    // 两次读取之间中继可能又转发了保活包、QUIC 确认：总量只会更多，不会更少。
    let (total, sessions, _) = relay.totals();
    assert!(
        total.bytes >= usage.bytes && total.packets >= usage.packets,
        "{total:?} {usage:?}"
    );
    assert_eq!(sessions, 1);
    assert!(
        relay.take_records().is_empty(),
        "会话还在进行，没有账单记录"
    );
    assert!(relay.addr().port() != 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_device_and_missing_relay_are_reported() {
    let w = world(false).await;
    // 没有人登记
    let nobody = [9u8; 32];
    match dial(
        &w.ctrl,
        &w.server,
        &Lookup::Device(nobody),
        PathPolicy::Auto,
    )
    .await
    {
        Err(e @ NetError::Connect(_)) => {
            assert_eq!(e, NetError::Connect("设备不在线".into()));
            assert_eq!(Reason::of_error(&e), Reason::DeviceOffline);
        }
        other => panic!("{other:?}"),
    }
    // 设备在线，但服务器没有中继，又要求只走中继
    let mut accepted = spawn_host(&w);
    wait_online(&w).await;
    let target = w.host_id.fingerprint();
    match dial(
        &w.ctrl,
        &w.server,
        &Lookup::Device(target),
        PathPolicy::RelayOnly,
    )
    .await
    {
        Err(NetError::Connect(why)) => assert_eq!(why, "服务器没有提供中继"),
        other => panic!("{other:?}"),
    }
    // 只走直连：可以
    let dialed = dial(
        &w.ctrl,
        &w.server,
        &Lookup::Device(target),
        PathPolicy::DirectOnly,
    )
    .await
    .unwrap();
    assert_eq!(dialed.path, Path::Direct);
    assert!(accepted.recv().await.is_some());
    assert!(format!("{:?}", w.signal).contains("online"));
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_server_or_wrong_device_identity_is_refused() {
    let w = world(true).await;
    let _accepted = spawn_host(&w);
    wait_online(&w).await;
    // 服务器的指纹不对：不向它透露要找谁
    let fake = Server {
        addr: w.server.addr,
        pin: Some([1; 32]),
    };
    let target = w.host_id.fingerprint();
    assert!(matches!(
        dial(&w.ctrl, &fake, &Lookup::Device(target), PathPolicy::Auto).await,
        Err(NetError::PinMismatch { .. })
    ));
    assert!(matches!(
        register(&w.ctrl, &fake).await,
        Err(NetError::PinMismatch { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_does_not_answer_is_called_unreachable() {
    let w = world(false).await;
    // 一个占着端口但从不回答的 UDP 套接字：像服务器停了、包被防火墙吞了
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let dead = Server {
        addr: silent.local_addr().unwrap(),
        pin: None,
    };
    let e = dial(&w.ctrl, &dead, &Lookup::Code(123_456_789), PathPolicy::Auto)
        .await
        .err()
        .unwrap();
    assert!(matches!(e, NetError::Unreachable(_)), "{e:?}");
    assert!(e.to_string().starts_with("连不上服务器："), "{e}");
    assert_eq!(Reason::of_error(&e), Reason::ServerUnreachable);
    let e = register(&w.host, &dead).await.err().unwrap();
    assert_eq!(Reason::of_error(&e), Reason::ServerUnreachable, "{e:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn device_goes_offline_when_its_registration_drops() {
    let w = world(false).await;
    let registration = register(&w.host, &w.server).await.unwrap();
    wait_online(&w).await;
    drop(registration);
    w.host.close();
    for _ in 0..300 {
        if w.signal.online() == 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("登记断开后设备仍然显示在线");
}

/// 被控端绑定在“所有地址”上（真实部署的常见情况），并且全程走 IPv6。
/// 没有 IPv6 回环地址的机器上跳过。
#[tokio::test(flavor = "multi_thread")]
async fn relay_works_over_ipv6_with_a_wildcard_host() {
    let v6_loopback: SocketAddr = "[::1]:0".parse().unwrap();
    if std::net::UdpSocket::bind(v6_loopback).is_err() {
        return;
    }
    let w = world_on(true, v6_loopback, "[::]:0".parse().unwrap()).await;
    let mut accepted = spawn_host(&w);
    wait_online(&w).await;
    let target = w.host_id.fingerprint();
    let dialed = dial(
        &w.ctrl,
        &w.server,
        &Lookup::Device(target),
        PathPolicy::RelayOnly,
    )
    .await
    .unwrap();
    assert_eq!(dialed.path, Path::Relay);
    assert!(dialed.shim.as_ref().unwrap().local_addr().is_ipv6());
    let host_link = accepted.recv().await.unwrap();
    echo(&dialed, &host_link).await;
}

/// IPv4 上被控端绑定在“所有地址”。
#[tokio::test(flavor = "multi_thread")]
async fn relay_works_with_a_wildcard_ipv4_host() {
    let w = world_on(true, any(), "0.0.0.0:0".parse().unwrap()).await;
    let mut accepted = spawn_host(&w);
    wait_online(&w).await;
    let target = w.host_id.fingerprint();
    let dialed = dial(
        &w.ctrl,
        &w.server,
        &Lookup::Device(target),
        PathPolicy::RelayOnly,
    )
    .await
    .unwrap();
    let host_link = accepted.recv().await.unwrap();
    echo(&dialed, &host_link).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_server_drops_misbehaving_devices() {
    let w = world(false).await;
    // 在信令端口上打开一条不是信令的流
    let link = w
        .ctrl
        .connect(w.server.addr, w.server.pin.as_ref())
        .await
        .unwrap();
    let (mut send, _recv) = link.open_stream().await.unwrap();
    write_kind(&mut send, StreamKind::File).await.unwrap();
    assert!(!link.closed().await.is_empty(), "服务器应当断开这条连接");
    // 发一条只有服务器才该发的消息
    let link = w
        .ctrl
        .connect(w.server.addr, w.server.pin.as_ref())
        .await
        .unwrap();
    let (mut send, mut recv) = link.open_stream().await.unwrap();
    write_kind(&mut send, StreamKind::Signal).await.unwrap();
    write_signal(&mut send, &SignalMsg::NotFound).await.unwrap();
    assert!(read_signal(&mut recv).await.is_err());
    assert_eq!(w.signal.online(), 0);
}

/// 一个不守规矩的信令服务器：对每条信令用 `reply` 作答。
fn plain() -> PunchInfo {
    PunchInfo {
        nonce: [0; 16],
        me: Nat::Unknown,
        peer: Nat::Unknown,
    }
}

fn fake_server(reply: impl Fn(SignalMsg) -> SignalMsg + Send + Sync + 'static) -> Server {
    let id = Identity::generate().unwrap();
    let node = Node::bind(any(), &id, &Policy::recommended()).unwrap();
    let server = Server {
        addr: node.local_addr().unwrap(),
        pin: Some(id.fingerprint()),
    };
    let reply = Arc::new(reply);
    tokio::spawn(async move {
        while let Some(Ok(link)) = node.accept().await {
            let reply = Arc::clone(&reply);
            tokio::spawn(async move {
                let (mut send, mut recv) = link.accept_stream().await.unwrap();
                read_kind(&mut recv).await.unwrap();
                while let Ok(msg) = read_signal(&mut recv).await {
                    write_signal(&mut send, &reply(msg)).await.unwrap();
                }
            });
        }
    });
    server
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lying_server_cannot_misdirect_us() {
    let policy = Policy::recommended();
    let (me_id, other_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let me = Node::bind(any(), &me_id, &policy).unwrap();
    // 另一台设备：服务器会把它冒充成目标
    let other = Arc::new(Node::bind(any(), &other_id, &policy).unwrap());
    let other_addr = other.local_addr().unwrap();
    let _other_sub = other.subscribe(plain().nonce);
    let keep = Arc::clone(&other);
    tokio::spawn(async move { while keep.accept().await.is_some() {} });
    let target = Identity::generate().unwrap().fingerprint();

    // 答非所问
    let server = fake_server(|_| SignalMsg::Register { locals: vec![] });
    assert!(matches!(
        register(&me, &server).await,
        Err(NetError::Protocol(_))
    ));
    assert!(matches!(
        dial(&me, &server, &Lookup::Device(target), PathPolicy::Auto).await,
        Err(NetError::Protocol(_))
    ));
    // 给的地址上是另一台设备：指纹不符，直接失败
    let server = fake_server(move |_| SignalMsg::Peer {
        device: target,
        addr: other_addr,
        locals: vec![],
        you: other_addr,
        punch: plain(),
        relay: None,
        note: String::new(),
    });
    assert!(matches!(
        dial(&me, &server, &Lookup::Device(target), PathPolicy::Auto).await,
        Err(NetError::PinMismatch { .. })
    ));
    // 服务器说的设备不是我们要找的那台：不连
    let server = fake_server(move |_| SignalMsg::Peer {
        device: [5; 32],
        addr: other_addr,
        locals: vec![],
        you: other_addr,
        punch: plain(),
        relay: None,
        note: String::new(),
    });
    assert!(matches!(
        dial(&me, &server, &Lookup::Device(target), PathPolicy::Auto).await,
        Err(NetError::Protocol(_))
    ));
}

/// 并行探测所有候选：回应的那条被选中；局域网和公网都通时局域网优先；
/// 选中的地址上是另一台设备时，指纹不符，直接失败。
#[tokio::test(flavor = "multi_thread")]
async fn probing_picks_the_candidate_that_answers() {
    let policy = Policy::recommended();
    let me = Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap();
    let host_id = Identity::generate().unwrap();
    let host = Arc::new(Node::bind(any(), &host_id, &policy).unwrap());
    let host_addr = host.local_addr().unwrap();
    // 被控端为这次连接的暗号登记探测，它的端口才会回 pong
    let _host_sub = host.subscribe(plain().nonce);
    let keep = Arc::clone(&host);
    tokio::spawn(async move { while keep.accept().await.is_some() {} });
    let target = host_id.fingerprint();
    let dead: SocketAddr = "127.0.0.1:9".parse().unwrap();
    let cand = |addr, kind| Candidate { addr, kind };

    // 公网地址不通，局域网地址通
    let server = fake_server(move |_| SignalMsg::Peer {
        device: target,
        addr: dead,
        locals: vec![
            cand("[::1]:9".parse().unwrap(), PathKind::Ipv6),
            cand(dead, PathKind::Lan),
            cand(host_addr, PathKind::Lan),
        ],
        you: "127.0.0.1:5".parse().unwrap(),
        punch: plain(),
        relay: None,
        note: String::new(),
    });
    let dialed = dial(&me, &server, &Lookup::Device(target), PathPolicy::Auto)
        .await
        .unwrap();
    assert_eq!(dialed.link.remote_addr(), host_addr);
    assert_eq!(dialed.route, PathKind::Lan);
    assert!(dialed.rtt_us.is_some());
    assert!(dialed.report.contains("不通"), "{}", dialed.report);

    // 只有公网地址通
    let server = fake_server(move |_| SignalMsg::Peer {
        device: target,
        addr: host_addr,
        locals: vec![cand("127.0.0.2:9".parse().unwrap(), PathKind::Lan)],
        you: "127.0.0.1:5".parse().unwrap(),
        punch: plain(),
        relay: None,
        note: String::new(),
    });
    let dialed = dial(&me, &server, &Lookup::Device(target), PathPolicy::Auto)
        .await
        .unwrap();
    assert_eq!(dialed.route, PathKind::Public);
    assert!(dialed.timing.probe > Duration::ZERO);

    // 一条都不通，又没有中继：报告原因
    let server = fake_server(move |_| SignalMsg::Peer {
        device: target,
        addr: dead,
        locals: vec![],
        you: "127.0.0.1:5".parse().unwrap(),
        punch: plain(),
        relay: None,
        note: String::new(),
    });
    // （普通打洞没通时还会退到多端口打洞，也不通）
    assert!(matches!(
        dial(&me, &server, &Lookup::Device(target), PathPolicy::Auto).await,
        Err(NetError::Timeout(_))
    ));

    // 局域网地址上是另一台设备（也回应探测）：指纹不符，直接失败
    let other = Arc::new(Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap());
    let _other_sub = other.subscribe(plain().nonce);
    let other_addr = other.local_addr().unwrap();
    let keep = Arc::clone(&other);
    tokio::spawn(async move { while keep.accept().await.is_some() {} });
    let server = fake_server(move |_| SignalMsg::Peer {
        device: target,
        addr: dead,
        locals: vec![cand(other_addr, PathKind::Lan)],
        you: "127.0.0.1:5".parse().unwrap(),
        punch: plain(),
        relay: None,
        note: String::new(),
    });
    assert!(matches!(
        dial(&me, &server, &Lookup::Device(target), PathPolicy::Auto).await,
        Err(NetError::PinMismatch { .. })
    ));
}

/// 按服务器分配的设备码连接：找到的就是登记的那台设备，并核对它的指纹。
#[tokio::test(flavor = "multi_thread")]
async fn device_code_finds_the_registered_device() {
    let w = world(true).await;
    let mut registration = register(&w.host, &w.server).await.unwrap();
    let code = registration.code;
    assert!((100_000_000..=999_999_999).contains(&code));
    let node = Arc::clone(&w.host);
    let (tx, mut accepted) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        let mut shims = Vec::new();
        loop {
            tokio::select! {
                Some(i) = registration.incoming.recv() => shims.push(answer(&node, &i).await.unwrap()),
                Some(link) = node.accept() => { let _ = tx.send(link.unwrap()).await; }
            }
        }
    });
    let dialed = dial(&w.ctrl, &w.server, &Lookup::Code(code), PathPolicy::Auto)
        .await
        .unwrap();
    assert_eq!(dialed.device, w.host_id.fingerprint());
    assert_eq!(dialed.path, Path::Direct);
    assert!(dialed.timing.lookup > Duration::ZERO && dialed.timing.relay == Duration::ZERO);
    assert!(dialed.direct_error.is_none());
    assert!(accepted.recv().await.is_some());
    // 不存在的码
    let wrong = if code == 999_999_999 {
        100_000_000
    } else {
        code + 1
    };
    match dial(&w.ctrl, &w.server, &Lookup::Code(wrong), PathPolicy::Auto).await {
        Err(e @ NetError::Connect(_)) => {
            assert!(e.to_string().contains("不存在"), "{e}");
            assert_eq!(Reason::of_error(&e), Reason::DeviceOffline);
        }
        other => panic!("{other:?}"),
    }
}

/// 设备码簿存盘：服务器重启后同一台设备拿到同一个码。
#[tokio::test(flavor = "multi_thread")]
async fn device_codes_persist_across_server_restarts() {
    let dir = std::env::temp_dir().join(format!("peerdesk-codes-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("device-codes.txt");
    let _ = std::fs::remove_file(&path);
    let policy = Policy::recommended();
    let host = Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap();
    let mut codes = Vec::new();
    for _ in 0..2 {
        let server_id = Identity::generate().unwrap();
        let node = Arc::new(Node::bind(any(), &server_id, &policy).unwrap());
        let server = Server {
            addr: node.local_addr().unwrap(),
            pin: Some(server_id.fingerprint()),
        };
        let signal = SignalService::start_with_codes(node, None, Some(path.clone()));
        codes.push(register(&host, &server).await.unwrap().code);
        drop(signal);
    }
    assert_eq!(codes[0], codes[1]);
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .starts_with(&codes[0].to_string()));
    // 码簿是个目录（写不进去）时服务照常，只是不存盘
    let node = Arc::new(Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap());
    let server = Server {
        addr: node.local_addr().unwrap(),
        pin: None,
    };
    let _signal = SignalService::start_with_codes(node, None, Some(dir.clone()));
    assert!(register(&host, &server).await.is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

/// 被控端不再处理连接请求（丢掉了接收端）时，登记任务自己结束，不会卡住。
#[tokio::test(flavor = "multi_thread")]
async fn registration_survives_a_closed_inbox() {
    let w = world(false).await;
    let mut registration = register(&w.host, &w.server).await.unwrap();
    registration.incoming.close();
    let target = w.host_id.fingerprint();
    for _ in 0..2 {
        let _ = dial(
            &w.ctrl,
            &w.server,
            &Lookup::Device(target),
            PathPolicy::DirectOnly,
        )
        .await;
    }
}

/// 网络体检：回环上没有 NAT，服务器在两个端口看到的地址一样，判为容易打洞；
/// 服务器没开中继时判断不了；没有服务器时只测本机部分。被控端准备时还会请求端口映射
/// （这里没有路由器，映射失败但给出原因）。
#[tokio::test(flavor = "multi_thread")]
async fn netcheck_reports_nat_type_and_latency() {
    use peerdesk_core::signal::Nat;
    use peerdesk_net::netcheck::{prepare_host, run};
    let w = world(true).await;
    let report = run(&w.ctrl, Some(&w.server)).await;
    assert_eq!(report.nat, Some(Nat::Easy), "{report:?}");
    assert_eq!(report.public, Some(w.ctrl.local_addr().unwrap()));
    assert!(report.server_rtt.is_some() && report.server_error.is_none());
    assert!(!report.lines().is_empty());

    let plain = world(false).await;
    let report = run(&plain.ctrl, Some(&plain.server)).await;
    assert_eq!(report.nat, Some(Nat::Unknown));

    let wrong = Server {
        addr: w.server.addr,
        pin: Some([1; 32]),
    };
    let report = run(&w.ctrl, Some(&wrong)).await;
    assert!(report.server_error.is_some() && report.nat.is_none());

    let (report, extra) = prepare_host(&w.host, None).await;
    assert!(report.public.is_none());
    assert!(report.mapping_error.is_some() || !extra.is_empty());
}

/// 被控端准备时向路由器申请端口映射：假的 NAT-PMP 路由器（127.0.0.1:5351）答应映射，
/// 映射出来的公网地址作为候选报告给服务器。
#[tokio::test(flavor = "multi_thread")]
async fn host_reports_the_port_its_router_mapped() {
    use peerdesk_core::paths::PathKind;
    use peerdesk_net::netcheck::prepare_host_via;
    let Ok(router) = tokio::net::UdpSocket::bind("127.0.0.1:5351").await else {
        return; // 端口被占用（极少见）：跳过
    };
    tokio::spawn(async move {
        let mut buf = [0u8; 128];
        while let Ok((len, from)) = router.recv_from(&mut buf).await {
            // NAT-PMP（版本 0）：外网地址请求和 UDP 映射请求；PCP（版本 2）不回应
            let reply: Vec<u8> = match (buf.first(), buf.get(1), len) {
                (Some(0), Some(0), 2) => {
                    [&[0u8, 128, 0, 0, 0, 0, 0, 1][..], &[203, 0, 113, 5]].concat()
                }
                (Some(0), Some(1), 12) => {
                    let internal = [buf[4], buf[5]];
                    [
                        &[0u8, 129, 0, 0, 0, 0, 0, 1][..],
                        &internal,
                        &[0x9c, 0x40],
                        &[0, 0, 0x0e, 0x10],
                    ]
                    .concat()
                }
                _ => continue,
            };
            let _ = router.send_to(&reply, from).await;
        }
    });
    let id = Identity::generate().unwrap();
    let node = Node::bind(any(), &id, &Policy::recommended()).unwrap();
    let (report, extra) = prepare_host_via(&node, None, vec!["127.0.0.1".parse().unwrap()]).await;
    let mapped: SocketAddr = "203.0.113.5:40000".parse().unwrap();
    assert_eq!(report.mapped, Some((mapped, "NAT-PMP")), "{report:?}");
    assert_eq!(extra.len(), 1);
    assert_eq!((extra[0].addr, extra[0].kind), (mapped, PathKind::Mapped));
}
