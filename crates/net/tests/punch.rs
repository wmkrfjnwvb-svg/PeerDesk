//! 按 NAT 类型选择的打洞方法，在回环上逐个验证（真实 NAT 下的效果由 scripts/nat-lab.sh 测）。
//!
//! 测试扮演“服务器”和“另一端”：服务器按测试的需要给出打洞方式；另一端用普通 UDP 端口
//! 收发探测包，撞上之后再把这个端口变成 QUIC 端点，和真实情况下撞上的端口一样。

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::crypto::Policy;
use peerdesk_core::signal::{parse_observe, parse_probe, probe_packet, Nat, PunchInfo, SignalMsg};
use peerdesk_net::rendezvous::{answer, dial, Incoming, Lookup, Path, PathPolicy, Server};
use peerdesk_net::stream::{read_kind, read_signal, write_signal};
use peerdesk_net::{Identity, NetError, Node};
use tokio::net::UdpSocket;

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

/// 每个测试用自己的暗号：测试并行运行，一个测试撒的探测包可能落到另一个测试的端口上。
fn info(me: Nat, peer: Nat, nonce: [u8; 16]) -> PunchInfo {
    PunchInfo { nonce, me, peer }
}

/// 一个假服务器：第 n 个请求（从 0 数）的回答由 `reply(n)` 给出。
fn fake_server(reply: impl Fn(usize) -> SignalMsg + Send + Sync + 'static) -> Server {
    let id = Identity::generate().unwrap();
    let node = Node::bind(any(), &id, &Policy::recommended()).unwrap();
    let server = Server {
        addr: node.local_addr().unwrap(),
        pin: Some(id.fingerprint()),
    };
    let reply = Arc::new(reply);
    let count = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        while let Some(Ok(link)) = node.accept().await {
            let (reply, count) = (Arc::clone(&reply), Arc::clone(&count));
            tokio::spawn(async move {
                let (mut send, mut recv) = link.accept_stream().await.unwrap();
                read_kind(&mut recv).await.unwrap();
                while read_signal(&mut recv).await.is_ok() {
                    let n = count.fetch_add(1, Ordering::Relaxed);
                    write_signal(&mut send, &reply(n)).await.unwrap();
                }
            });
        }
    });
    server
}

fn peer(device: [u8; 32], addr: SocketAddr, punch: PunchInfo) -> SignalMsg {
    SignalMsg::Peer {
        device,
        addr,
        locals: vec![],
        // 和被控端的 IP 不同：不试局域网地址
        you: "127.0.0.2:1".parse().unwrap(),
        punch,
        relay: None,
    }
}

/// 控制端是难打洞的一端：开很多端口探测，用撞上的那个端口连过去。
/// 服务器先要求探测 NAT（控制端从连接用的端口发探测包），再给地址。
#[tokio::test(flavor = "multi_thread")]
async fn controller_on_a_hard_nat_connects_from_the_port_that_was_hit() {
    let nonce = [13u8; 16];
    let policy = Policy::recommended();
    let ctrl_id = Identity::generate().unwrap();
    let ctrl = Node::bind(any(), &ctrl_id, &policy).unwrap();
    let host_id = Identity::generate().unwrap();
    let host_fp = host_id.fingerprint();
    // 被控端的“公网地址”：先是普通端口，撞上之后变成 QUIC 端点
    let host_socket = UdpSocket::bind(any()).await.unwrap();
    let host_addr = host_socket.local_addr().unwrap();
    let observer = UdpSocket::bind(any()).await.unwrap();
    let observer_addr = observer.local_addr().unwrap();
    let server = fake_server(move |n| match n {
        0 => SignalMsg::Probe {
            relay: observer_addr,
        },
        _ => peer(host_fp, host_addr, info(Nat::Hard, Nat::Easy, nonce)),
    });
    let dialing = tokio::spawn(async move {
        let dialed = dial(
            &ctrl,
            &server,
            &Lookup::Device(host_fp),
            PathPolicy::DirectOnly,
        )
        .await;
        (dialed, ctrl)
    });

    // NAT 探测包从控制端连接用的端口来，带着它的指纹
    let mut buf = [0u8; 128];
    let (len, _) = observer.recv_from(&mut buf).await.unwrap();
    assert_eq!(parse_observe(&buf[..len]), Some(ctrl_id.fingerprint()));

    // 某个探测端口的包到了：回一个带暗号的包，撞上
    let (len, from) = host_socket.recv_from(&mut buf).await.unwrap();
    assert_eq!(parse_probe(&buf[..len], &nonce), Some(host_addr.port()));
    host_socket
        .send_to(&probe_packet(&nonce, from.port()), from)
        .await
        .unwrap();
    let host = Node::on_socket(host_socket.into_std().unwrap(), &host_id, &policy).unwrap();

    let (dialed, ctrl) = dialing.await.unwrap();
    let dialed = dialed.unwrap();
    assert_eq!(dialed.path, Path::Direct);
    assert_eq!(dialed.link.remote_addr(), host_addr);
    assert_eq!(dialed.device, host_fp);
    let accepted = host.accept().await.unwrap().unwrap();
    assert_eq!(
        accepted.remote_addr(),
        from,
        "用的是撞上的那个端口，不是原来的端口"
    );
    assert_ne!(from, ctrl.local_addr().unwrap());
    ctrl.close();
}

/// 控制端是容易的一端、被控端难：控制端撒探测包，等被控端从撞上的端口连过来；
/// 先连过来的陌生设备被拒绝。
#[tokio::test(flavor = "multi_thread")]
async fn controller_on_an_easy_nat_sprays_and_waits_for_the_host() {
    let nonce = [16u8; 16];
    let policy = Policy::recommended();
    let ctrl_id = Identity::generate().unwrap();
    let ctrl = Node::bind(any(), &ctrl_id, &policy).unwrap();
    let ctrl_addr = ctrl.local_addr().unwrap();
    let host_id = Identity::generate().unwrap();
    let host_fp = host_id.fingerprint();
    let host_socket = UdpSocket::bind(any()).await.unwrap();
    let host_addr = host_socket.local_addr().unwrap();
    let server = fake_server(move |_| peer(host_fp, host_addr, info(Nat::Easy, Nat::Hard, nonce)));
    let dialing = tokio::spawn(async move {
        dial(
            &ctrl,
            &server,
            &Lookup::Device(host_fp),
            PathPolicy::DirectOnly,
        )
        .await
    });

    // 撒来的探测包（发往已知地址的那份）
    let mut buf = [0u8; 128];
    let (len, from) = host_socket.recv_from(&mut buf).await.unwrap();
    assert_eq!(parse_probe(&buf[..len], &nonce), Some(host_addr.port()));
    assert_eq!(from, ctrl_addr);

    // 一个陌生设备先连过来：被断开
    let stranger = Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap();
    let rejected = stranger.connect(ctrl_addr, None).await.unwrap();
    assert!(!rejected.closed().await.is_empty());

    // 被控端从撞上的端口主动连过来
    let host = Node::on_socket(host_socket.into_std().unwrap(), &host_id, &policy).unwrap();
    let link = host
        .connect(ctrl_addr, Some(&ctrl_id.fingerprint()))
        .await
        .unwrap();
    let dialed = dialing.await.unwrap().unwrap();
    assert_eq!(dialed.path, Path::Direct);
    assert_eq!(dialed.device, host_fp);
    drop(link);
}

/// 被控端按打洞方式响应：两端都难时什么都不做；本端容易时撒包；本端难时开很多端口，
/// 撞上后主动连控制端，这条连接从 accept 出来。没撞上或撞上了连不上时放弃，不影响别的。
#[tokio::test(flavor = "multi_thread")]
async fn host_answers_according_to_the_plan() {
    let nonce = [19u8; 16];
    let policy = Policy::recommended();
    let host = Arc::new(Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap());
    let host_addr = host.local_addr().unwrap();
    let ctrl_id = Identity::generate().unwrap();
    let incoming = |addr, punch| Incoming {
        peer: ctrl_id.fingerprint(),
        addr,
        punch,
        relay: None,
    };

    // 两端都难：不打洞
    let skip = incoming(any(), info(Nat::Hard, Nat::Hard, nonce));
    assert!(answer(&host, &skip).await.unwrap().is_none());

    // 本端容易：向对方撒包（包括对方的已知地址）
    let target = UdpSocket::bind(any()).await.unwrap();
    let spray = incoming(
        target.local_addr().unwrap(),
        info(Nat::Easy, Nat::Hard, nonce),
    );
    answer(&host, &spray).await.unwrap();
    let mut buf = [0u8; 128];
    let (len, from) = target.recv_from(&mut buf).await.unwrap();
    assert_eq!(from, host_addr);
    assert!(parse_probe(&buf[..len], &nonce).is_some());

    // 本端难：撞上之后主动连控制端
    let ctrl_socket = UdpSocket::bind(any()).await.unwrap();
    let ctrl_addr = ctrl_socket.local_addr().unwrap();
    answer(
        &host,
        &incoming(ctrl_addr, info(Nat::Hard, Nat::Easy, nonce)),
    )
    .await
    .unwrap();
    let (_, from) = ctrl_socket.recv_from(&mut buf).await.unwrap();
    ctrl_socket
        .send_to(&probe_packet(&nonce, from.port()), from)
        .await
        .unwrap();
    let ctrl = Node::on_socket(ctrl_socket.into_std().unwrap(), &ctrl_id, &policy).unwrap();
    tokio::spawn(async move { while ctrl.accept().await.is_some() {} });
    let link = tokio::time::timeout(Duration::from_secs(10), host.accept())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(link.peer_fingerprint(), &ctrl_id.fingerprint());
    assert_eq!(link.remote_addr(), ctrl_addr);

    // 撞上了但对方不说 QUIC：连接超时，放弃
    let mute = UdpSocket::bind(any()).await.unwrap();
    answer(
        &host,
        &incoming(
            mute.local_addr().unwrap(),
            info(Nat::Hard, Nat::Easy, nonce),
        ),
    )
    .await
    .unwrap();
    let (_, from) = mute.recv_from(&mut buf).await.unwrap();
    mute.send_to(&probe_packet(&nonce, from.port()), from)
        .await
        .unwrap();
    // 没人回应：多端口打洞超时，放弃
    let silent = UdpSocket::bind(any()).await.unwrap();
    answer(
        &host,
        &incoming(
            silent.local_addr().unwrap(),
            info(Nat::Hard, Nat::Easy, nonce),
        ),
    )
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(5600)).await;
    host.close();
}

/// 两端都难：控制端不打洞，直接走中继（这里没有中继，所以报告原因）；多端口打洞撞不上时也一样。
#[tokio::test(flavor = "multi_thread")]
async fn hopeless_or_missed_punching_reports_why() {
    let nonce = [21u8; 16];
    let policy = Policy::recommended();
    let ctrl = Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap();
    let silent = UdpSocket::bind(any()).await.unwrap();
    let silent_addr = silent.local_addr().unwrap();
    let both_hard =
        fake_server(move |_| peer([3; 32], silent_addr, info(Nat::Hard, Nat::Hard, nonce)));
    match dial(
        &ctrl,
        &both_hard,
        &Lookup::Device([3; 32]),
        PathPolicy::DirectOnly,
    )
    .await
    {
        Err(NetError::Connect(why)) => assert!(why.contains("对称型"), "{why}"),
        other => panic!("{other:?}"),
    }
    let missed =
        fake_server(move |_| peer([3; 32], silent_addr, info(Nat::Hard, Nat::Easy, nonce)));
    assert!(matches!(
        dial(
            &ctrl,
            &missed,
            &Lookup::Device([3; 32]),
            PathPolicy::DirectOnly
        )
        .await,
        Err(NetError::Timeout(_))
    ));
    let nobody =
        fake_server(move |_| peer([3; 32], silent_addr, info(Nat::Easy, Nat::Hard, nonce)));
    assert!(matches!(
        dial(
            &ctrl,
            &nobody,
            &Lookup::Device([3; 32]),
            PathPolicy::DirectOnly
        )
        .await,
        Err(NetError::Timeout(_))
    ));
    // 端点关闭后不再等对方连过来
    ctrl.close();
    assert!(dial(
        &ctrl,
        &nobody,
        &Lookup::Device([3; 32]),
        PathPolicy::DirectOnly
    )
    .await
    .is_err());
}

/// 等被控端连过来时，本端的端点被关掉：不再等，报告原因。
#[tokio::test(flavor = "multi_thread")]
async fn closing_the_node_stops_waiting_for_the_host() {
    let nonce = [31u8; 16];
    let policy = Policy::recommended();
    let ctrl = Arc::new(Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap());
    let host_socket = UdpSocket::bind(any()).await.unwrap();
    let host_addr = host_socket.local_addr().unwrap();
    let server = fake_server(move |_| peer([4; 32], host_addr, info(Nat::Easy, Nat::Hard, nonce)));
    let node = Arc::clone(&ctrl);
    let dialing = tokio::spawn(async move {
        dial(
            &node,
            &server,
            &Lookup::Device([4; 32]),
            PathPolicy::DirectOnly,
        )
        .await
    });
    let mut buf = [0u8; 128];
    let _ = host_socket.recv_from(&mut buf).await.unwrap();
    ctrl.close();
    match dialing.await.unwrap() {
        Err(NetError::Closed(why)) => assert!(why.contains("关闭"), "{why}"),
        other => panic!("{other:?}"),
    }
    // 关掉之后不能再开打洞端口，交出来的连接也没人收
    let spare = std::net::UdpSocket::bind(any()).unwrap();
    assert!(matches!(
        ctrl.connect_via(spare, host_addr, None, Duration::from_millis(100))
            .await,
        Err(NetError::Closed(_))
    ));
}
