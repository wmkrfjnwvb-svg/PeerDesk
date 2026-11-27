//! 账户设备表：真实的服务器和客户端跑在回环地址上，入表、列出、改名、移除，以及和老服务器的兼容。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::unreachable)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::billing::issue_enroll;
use peerdesk_core::crypto::Policy;
use peerdesk_core::devices::{DeviceStore, MemoryStore, Refusal, Role};
use peerdesk_core::signal::{observe_packet, Features, SignalMsg};
use peerdesk_net::rendezvous::{register, server_features, AccountSession, Server};
use peerdesk_net::service::{Accounts, LocalDirectory, RelayService, SignalService};
use peerdesk_net::stream::{read_signal, write_kind, write_signal, StreamKind};
use peerdesk_net::{Identity, NetError, Node};

const KEY: &[u8] = b"billing secret";
const A: [u8; 8] = [0xa1; 8];
const B: [u8; 8] = [0xb2; 8];

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

fn node() -> (Node, Identity) {
    let id = Identity::generate().unwrap();
    (Node::bind(any(), &id, &Policy::recommended()).unwrap(), id)
}

struct World {
    server: Server,
    signal: SignalService,
    store: Arc<MemoryStore>,
    relay: Option<Arc<RelayService>>,
}

/// 一台服务器。`with_accounts` 为假时是没开账户设备表的服务器（和老版本服务器对客户端的表现一样：功能字段为空）。
async fn world(with_accounts: bool, with_relay: bool) -> World {
    let (server_node, server_id) = node();
    let server_node = Arc::new(server_node);
    let relay = if with_relay {
        Some(Arc::new(RelayService::start(any(), None).await.unwrap()))
    } else {
        None
    };
    let store = Arc::new(MemoryStore::new());
    let accounts = with_accounts.then(|| Accounts {
        store: store.clone(),
        key: KEY.to_vec(),
    });
    let server = Server {
        addr: server_node.local_addr().unwrap(),
        pin: Some(server_id.fingerprint()),
    };
    let signal = SignalService::start_with_accounts(
        server_node,
        relay.clone(),
        Arc::new(LocalDirectory::new(relay.clone(), None)),
        None,
        accounts,
    );
    World {
        server,
        signal,
        store,
        relay,
    }
}

fn far_future() -> u64 {
    u64::MAX / 2
}

fn refused(result: Result<impl std::fmt::Debug, NetError>) -> Refusal {
    match result {
        Err(NetError::Refused(why)) => why,
        other => unreachable!("应当被拒绝：{other:?}"),
    }
}

/// 闭环：三台设备入同一个账户，都能列出三台；第四台在别的账户，互相看不见；改名、移除立刻生效。
#[tokio::test]
async fn three_devices_join_an_account_and_list_each_other() {
    let w = world(true, false).await;
    let token = issue_enroll(KEY, A, far_future(), 1);
    let devices = [node(), node(), node()];
    let names = ["mac-mini", "办公室 PC", "linux-box"];
    for ((n, _), name) in devices.iter().zip(names) {
        let mut session = AccountSession::open(n, &w.server).await.unwrap();
        assert!(session.features.has(Features::DEVICES));
        // 没入表：看不了
        assert_eq!(refused(session.devices(0).await), Refusal::NotMember);
        let enrolled = session.enroll(&token, name, 2, Role::Host).await.unwrap();
        assert_eq!(enrolled.account, A);
        // 再入一次：还是原来的编号
        let again = session
            .enroll(&token, "别的名字", 2, Role::Host)
            .await
            .unwrap();
        assert_eq!((again.row, again.rev), (enrolled.row, enrolled.rev));
        session.close();
    }
    let (outsider, _) = node();
    let mut other = AccountSession::open(&outsider, &w.server).await.unwrap();
    other
        .enroll(
            &issue_enroll(KEY, B, far_future(), 2),
            "别人的",
            1,
            Role::Controller,
        )
        .await
        .unwrap();
    // 已经在别的账户里的设备，拿到这个账户的令牌也进不来
    assert_eq!(
        refused(other.enroll(&token, "x", 1, Role::Host).await),
        Refusal::OtherAccount
    );
    let theirs = other.devices(0).await.unwrap();
    assert_eq!(theirs.rows.len(), 1);
    assert_eq!(theirs.rows[0].name, "别人的");

    // 每台设备都列得出这三台（名字、指纹对得上），看不到别的账户的
    for (n, _) in &devices {
        let mut session = AccountSession::open(n, &w.server).await.unwrap();
        let all = session.devices(0).await.unwrap();
        assert!(all.full);
        assert_eq!(all.rev, 3);
        assert_eq!(
            all.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            names
        );
        for (row, (_, id)) in all.rows.iter().zip(&devices) {
            assert_eq!(row.fingerprint, id.fingerprint());
            assert!(!row.removed);
        }
        session.close();
    }

    // 第一台给第二台改名、把第三台移出账户
    let mut first = AccountSession::open(&devices[0].0, &w.server)
        .await
        .unwrap();
    assert_eq!(first.rename(2, "书房的 PC").await.unwrap(), 4);
    assert_eq!(refused(first.rename(2, "").await), Refusal::BadName);
    assert_eq!(refused(first.rename(9, "x").await), Refusal::NoSuchDevice);
    assert_eq!(first.remove(3).await.unwrap(), 5);
    // 增量：改名的一行和移除的一行
    let delta = first.devices(3).await.unwrap();
    assert!(!delta.full);
    assert_eq!(
        delta
            .rows
            .iter()
            .map(|r| (r.row, r.name.as_str(), r.removed))
            .collect::<Vec<_>>(),
        [(2, "书房的 PC", false), (3, "linux-box", true)]
    );
    // 被移除的那台：不再是成员
    let mut third = AccountSession::open(&devices[2].0, &w.server)
        .await
        .unwrap();
    assert_eq!(refused(third.devices(0).await), Refusal::NotMember);
    assert_eq!(refused(third.remove(1).await), Refusal::NotMember);
    // 第二台自己退出
    let mut second = AccountSession::open(&devices[1].0, &w.server)
        .await
        .unwrap();
    assert_eq!(second.remove(0).await.unwrap(), 6);
    assert_eq!(refused(second.devices(0).await), Refusal::NotMember);
    assert_eq!(first.devices(0).await.unwrap().rows.len(), 1);
}

/// 入表令牌不对、过期：拒绝并说明原因；说明里有解决办法。
#[tokio::test]
async fn bad_and_expired_tokens_are_refused_with_a_reason() {
    let w = world(true, false).await;
    let (n, _) = node();
    let mut session = AccountSession::open(&n, &w.server).await.unwrap();
    let result = session.enroll("pde1-nonsense", "mac", 2, Role::Host).await;
    assert_eq!(refused(result), Refusal::BadToken);
    let forged = issue_enroll(b"someone else", A, far_future(), 1);
    assert_eq!(
        refused(session.enroll(&forged, "mac", 2, Role::Host).await),
        Refusal::BadToken
    );
    let expired = issue_enroll(KEY, A, 1, 1);
    let result = session.enroll(&expired, "mac", 2, Role::Host).await;
    let why = result.unwrap_err();
    assert_eq!(why, NetError::Refused(Refusal::ExpiredToken));
    assert!(why.to_string().contains("过期") && why.to_string().contains("重新生成"));
    assert!(w.store.member(n.fingerprint()).is_none());
}

/// 回归（兼容）：服务器没开账户设备表（老服务器也是这样：功能字段为空）时，客户端一条设备表的消息都不发，
/// 每个操作直接说“服务器不支持”；同一台服务器上老的登记照常。
#[tokio::test]
async fn a_server_without_device_tables_is_left_alone() {
    let w = world(false, false).await;
    let (host, _) = node();
    let registration = register(&host, &w.server).await.unwrap();
    assert_eq!(registration.features, Features::NONE);
    assert_eq!(
        server_features(&host, &w.server).await.unwrap(),
        Features::NONE
    );
    let mut session = AccountSession::open(&host, &w.server).await.unwrap();
    let token = issue_enroll(KEY, A, far_future(), 1);
    assert_eq!(
        refused(session.enroll(&token, "mac", 2, Role::Host).await),
        Refusal::Unsupported
    );
    assert_eq!(refused(session.devices(0).await), Refusal::Unsupported);
    assert_eq!(refused(session.rename(1, "x").await), Refusal::Unsupported);
    assert_eq!(refused(session.remove(0).await), Refusal::Unsupported);
    // 没发过服务器不认识的东西：登记还在
    session.close();
    assert_eq!(w.signal.online(), 1);
    // 硬把设备表的消息发给没开设备表的服务器：它回“不支持”，不断线（老服务器会断，见
    // rendezvous.rs 的 an_unknown_signal_message_costs_the_registration）
    let link = host
        .connect(w.server.addr, w.server.pin.as_ref())
        .await
        .unwrap();
    let (mut send, mut recv) = link.open_stream().await.unwrap();
    write_kind(&mut send, StreamKind::Signal).await.unwrap();
    write_signal(&mut send, &SignalMsg::Devices { since: 0 })
        .await
        .unwrap();
    assert_eq!(
        read_signal(&mut recv).await.unwrap(),
        SignalMsg::Refused {
            code: Refusal::Unsupported.code()
        }
    );
}

/// 设备登记后下线：服务器记下“上次在线”的时间。没登记过的连接（只来查列表的）不记。
#[tokio::test]
async fn going_offline_records_last_seen() {
    let w = world(true, false).await;
    let (host, _) = node();
    let token = issue_enroll(KEY, A, far_future(), 1);
    let mut session = AccountSession::open(&host, &w.server).await.unwrap();
    session
        .enroll(&token, "mac-mini", 2, Role::Host)
        .await
        .unwrap();
    assert_eq!(session.devices(0).await.unwrap().rows[0].last_seen_s, 0);
    session.close();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        w.store.list(&A, 0).rows[0].last_seen_s,
        0,
        "只查列表的连接不算下线"
    );
    let registration = register(&host, &w.server).await.unwrap();
    assert_eq!(registration.features, Features::DEVICES);
    assert_eq!(w.signal.online(), 1);
    drop(registration);
    tokio::time::timeout(Duration::from_secs(15), async {
        while w.store.list(&A, 0).rows[0].last_seen_s == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("下线后应当记下上次在线时间");
    assert!(w.store.list(&A, 0).rows[0].last_seen_s > 1_700_000_000);
}

/// 只问功能的体检不动 NAT 探测的结果；普通体检才让服务器忘掉它（下一次体检要看新的探测）。
#[tokio::test]
async fn asking_for_features_keeps_the_nat_probe_result() {
    let w = world(true, true).await;
    let relay = w.relay.clone().unwrap();
    let (host, id) = node();
    let me = id.fingerprint();
    // 从这个端口向探测地址发探测包，服务器记下看到的地址
    let packet = observe_packet(&me);
    tokio::time::timeout(Duration::from_secs(5), async {
        while relay.observed(&me).is_none() {
            host.send_raw(relay.addr(), &packet);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("服务器应当看到探测包");
    // 问功能、做设备表的操作：探测结果还在
    assert_eq!(
        server_features(&host, &w.server).await.unwrap(),
        Features::DEVICES
    );
    let mut session = AccountSession::open(&host, &w.server).await.unwrap();
    let _ = session.devices(0).await;
    session.close();
    assert!(relay.observed(&me).is_some());
    // 普通体检：忘掉
    let link = host
        .connect(w.server.addr, w.server.pin.as_ref())
        .await
        .unwrap();
    let (mut send, mut recv) = link.open_stream().await.unwrap();
    write_kind(&mut send, StreamKind::Signal).await.unwrap();
    write_signal(&mut send, &SignalMsg::Check).await.unwrap();
    assert!(matches!(
        read_signal(&mut recv).await.unwrap(),
        SignalMsg::Checked { seen: Some(_), features, .. } if features == Features::DEVICES
    ));
    assert!(relay.observed(&me).is_none());
}
