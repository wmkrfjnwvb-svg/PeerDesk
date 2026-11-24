//! 会话线程的端到端测试：同一进程里一个被控端、一个或几个控制端，走真实的 QUIC。

use super::*;
use peerdesk_net::service::{RelayService, SignalService};
use std::sync::atomic::{AtomicUsize, Ordering};

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-gui-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn counter() -> (Repaint, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&count);
    (
        Arc::new(move || {
            c.fetch_add(1, Ordering::Relaxed);
        }),
        count,
    )
}

const WAIT: Duration = Duration::from_secs(20);

/// 收到的全部事件，以及已经看过的位置。
#[derive(Debug)]
struct Log<E> {
    all: Vec<E>,
    next: usize,
}

impl<E> Log<E> {
    fn new() -> Self {
        Self {
            all: Vec::new(),
            next: 0,
        }
    }
}

/// 等到下一个满足条件的事件出现，返回它。之前的事件跳过（但留在记录里）。
fn wait_for<E: Clone + std::fmt::Debug>(
    poll: impl Fn() -> Vec<E>,
    log: &mut Log<E>,
    want: impl Fn(&E) -> bool,
) -> E {
    let deadline = Instant::now() + WAIT;
    loop {
        while let Some(e) = log.all.get(log.next).cloned() {
            log.next += 1;
            if want(&e) {
                return e;
            }
        }
        log.all.extend(poll());
        assert!(Instant::now() < deadline, "等待超时，已收到：{:?}", log.all);
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn host(dir: &Path, password: &str, server: Option<Server>) -> (HostHandle, u16, Log<HostEvent>) {
    let (repaint, _) = counter();
    let handle = start_host(
        HostConfig {
            port: 0,
            data_dir: dir.to_path_buf(),
            server,
            fps: 60,
        },
        password,
        repaint,
    );
    let mut seen = Log::new();
    let HostEvent::Listening { port, .. } = wait_for(
        || handle.poll(),
        &mut seen,
        |e| matches!(e, HostEvent::Listening { .. }),
    ) else {
        panic!("不是 Listening")
    };
    (handle, port, seen)
}

use std::path::Path;

fn local(port: u16, pin: Option<Fingerprint>) -> Target {
    Target::Addr {
        addr: (Ipv4Addr::LOCALHOST, port).into(),
        pin,
    }
}

use peerdesk_net::Fingerprint;

fn ended(v: &ViewHandle, seen: &mut Log<ViewEvent>) -> Result<Summary, NetError> {
    match wait_for(|| v.poll(), seen, |e| matches!(e, ViewEvent::Ended(_))) {
        ViewEvent::Ended(r) => r,
        ViewEvent::Connected { .. }
        | ViewEvent::Reconnecting { .. }
        | ViewEvent::Notice(_)
        | ViewEvent::Clipboard(_)
        | ViewEvent::Quality(_)
        | ViewEvent::Peer(..) => {
            panic!("不是 Ended")
        }
    }
}

#[test]
fn view_a_host_send_input_and_get_kicked() {
    let (host_dir, ctrl_dir) = (temp_dir("h1"), temp_dir("c1"));
    let (h, port, _) = host(&host_dir, "secret-pw", None);
    let host_fp = Identity::load_or_create(&host_dir).unwrap().fingerprint();
    h.set_password("right-pw");

    let (repaint, repaints) = counter();
    let v = start_view(
        local(port, Some(host_fp)),
        "right-pw",
        ctrl_dir.clone(),
        repaint,
    );
    let mut seen = Log::new();
    let connected = wait_for(
        || v.poll(),
        &mut seen,
        |e| matches!(e, ViewEvent::Connected { .. }),
    );
    assert!(
        matches!(connected, ViewEvent::Connected { route: Route::Direct, ref peer, .. } if *peer == to_hex(&host_fp))
    );
    // 远程声音默认关，可以打开、关闭（测试构建里是合成声音和录音器，不碰声卡）；握手后得知对方系统
    assert!(!v.sound());
    v.set_sound(true);
    assert!(v.sound());
    let os = wait_for(|| v.poll(), &mut seen, |e| matches!(e, ViewEvent::Peer(..)));
    assert_eq!(
        os,
        ViewEvent::Peer(PeerOs::current(), crate::local::device_name())
    );
    std::thread::sleep(Duration::from_millis(300));
    v.set_sound(false);

    let mut host_seen = Log::new();
    wait_for(
        || h.poll(),
        &mut host_seen,
        |e| *e == HostEvent::Established,
    );
    assert!(host_seen
        .all
        .iter()
        .any(|e| matches!(e, HostEvent::Peer { .. })));

    // 画面到了
    let deadline = Instant::now() + WAIT;
    let image = loop {
        if let Some(image) = v.frames().take() {
            break image;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(image.size, [1280, 720]);
    assert_eq!(v.frames().size(), [1280, 720]);
    assert!(v.frames().frames() >= 1);
    assert!(repaints.load(Ordering::Relaxed) >= 1);

    // 输入到了被控端
    assert!(v.send(InputEvent::Key {
        code: 4,
        down: true
    }));
    assert!(v.send(InputEvent::Key {
        code: 4,
        down: false
    }));
    let deadline = Instant::now() + WAIT;
    while h.tap().count() < 2 {
        assert!(Instant::now() < deadline, "输入没有到达");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        h.tap().last(),
        Some(InputEvent::Key {
            code: 4,
            down: false
        })
    );

    // 被控端断开控制端，然后继续等下一个
    h.kick();
    let summary = ended(&v, &mut seen).unwrap();
    assert!(!summary.cancelled);
    assert!(
        summary.rtt.is_some() && summary.video_bytes > 0,
        "{summary:?}"
    );
    let host_end = wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::SessionEnded(_)),
    );
    assert!(
        matches!(host_end, HostEvent::SessionEnded(Ok(_))),
        "{host_end:?}"
    );
    // 会话结束后发输入：丢弃
    let deadline = Instant::now() + WAIT;
    while v.send(InputEvent::ReleaseAll) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }

    // 控制端主动断开
    let (repaint, _) = counter();
    let v = start_view(local(port, None), "right-pw", ctrl_dir.clone(), repaint);
    let mut seen = Log::new();
    wait_for(
        || v.poll(),
        &mut seen,
        |e| matches!(e, ViewEvent::Connected { .. }),
    );
    wait_for(
        || h.poll(),
        &mut host_seen,
        |e| *e == HostEvent::Established,
    );
    v.stop();
    assert!(ended(&v, &mut seen).unwrap().cancelled);
    wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::SessionEnded(_)),
    );

    // 停止被控端
    h.stop();
    let stopped = wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::Stopped(_)),
    );
    assert_eq!(stopped, HostEvent::Stopped(None));
    for dir in [host_dir, ctrl_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn wrong_password_wrong_pin_and_nobody_there() {
    let (host_dir, ctrl_dir) = (temp_dir("h2"), temp_dir("c2"));
    let (h, port, _) = host(&host_dir, "secret-pw", None);
    let mut host_seen = Log::new();

    // 密码不对
    let (repaint, _) = counter();
    let v = start_view(local(port, None), "guess-pw", ctrl_dir.clone(), repaint);
    let mut seen = Log::new();
    let err = ended(&v, &mut seen).unwrap_err();
    assert!(err.to_string().contains("密码错误"), "{err}");
    let failed = wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::SessionEnded(_)),
    );
    assert!(
        matches!(failed, HostEvent::SessionEnded(Err(_))),
        "{failed:?}"
    );

    // 不带密码
    let (repaint, _) = counter();
    let v = start_view(local(port, None), "", ctrl_dir.clone(), repaint);
    assert!(ended(&v, &mut Log::new()).is_err());

    // 指纹不对：握手后立即断开
    let (repaint, _) = counter();
    let v = start_view(
        local(port, Some([7; 32])),
        "secret-pw",
        ctrl_dir.clone(),
        repaint,
    );
    assert!(matches!(
        ended(&v, &mut Log::new()),
        Err(NetError::PinMismatch { .. })
    ));
    wait_for(
        || h.poll(),
        &mut host_seen,
        |e| {
            matches!(
                e,
                HostEvent::HandshakeFailed(_) | HostEvent::SessionEnded(_)
            )
        },
    );

    // 同一个端口不能再开一个被控端
    let (repaint, _) = counter();
    let second = start_host(
        HostConfig {
            port,
            data_dir: host_dir.clone(),
            server: None,
            fps: 30,
        },
        "secret-pw",
        repaint,
    );
    let stopped = wait_for(
        || second.poll(),
        &mut Log::new(),
        |e| matches!(e, HostEvent::Stopped(_)),
    );
    assert!(
        matches!(stopped, HostEvent::Stopped(Some(NetError::Io(_)))),
        "{stopped:?}"
    );
    drop(h);

    // 数据目录不可用
    let file = ctrl_dir.join("file");
    std::fs::write(&file, b"x").unwrap();
    let (repaint, _) = counter();
    let v = start_view(local(9, None), "", file.clone(), repaint);
    assert!(ended(&v, &mut Log::new()).is_err());

    // 地址上没有人：可以随时放弃
    let (repaint, _) = counter();
    let v = start_view(
        Target::Addr {
            addr: "[::1]:9".parse().unwrap(),
            pin: None,
        },
        "",
        ctrl_dir.clone(),
        repaint,
    );
    std::thread::sleep(Duration::from_millis(100));
    drop(v);
    for dir in [host_dir, ctrl_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 测试用的信令服务器（带中继），跑在自己的运行时上。
struct TestServer {
    node: Arc<Node>,
    _signal: SignalService,
    _relay: Arc<RelayService>,
    rt: tokio::runtime::Runtime,
    pin: Fingerprint,
}

impl TestServer {
    fn start(dir: &Path) -> Self {
        Self::start_with(dir, peerdesk_core::relay::FreePolicy::UNLIMITED)
    }

    /// 免费通道额度为 `free` 的服务器。
    fn start_with(dir: &Path, free: peerdesk_core::relay::FreePolicy) -> Self {
        let identity = Identity::load_or_create(dir).unwrap();
        let rt = runtime().unwrap();
        let (node, signal, relay) = rt.block_on(async {
            let options = peerdesk_net::service::RelayOptions {
                free,
                ..peerdesk_net::service::RelayOptions::local()
            };
            let relay = Arc::new(
                RelayService::start_with(([127, 0, 0, 1], 0).into(), None, options)
                    .await
                    .unwrap(),
            );
            let node = Arc::new(
                Node::bind(
                    ([127, 0, 0, 1], 0).into(),
                    &identity,
                    &Policy::recommended(),
                )
                .unwrap(),
            );
            let signal = SignalService::start(Arc::clone(&node), Some(Arc::clone(&relay)));
            (node, signal, relay)
        });
        Self {
            node,
            _signal: signal,
            _relay: relay,
            rt,
            pin: identity.fingerprint(),
        }
    }

    fn server(&self, pin: Option<Fingerprint>) -> Server {
        Server {
            addr: self.node.local_addr().unwrap(),
            pin,
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.node.close();
        let node = &self.node;
        let _ = self.rt.block_on(async {
            tokio::time::timeout(Duration::from_secs(2), node.wait_idle()).await
        });
    }
}

#[test]
fn connect_through_a_signaling_server() {
    let (server_dir, host_dir, ctrl_dir) = (temp_dir("s3"), temp_dir("h3"), temp_dir("c3"));
    let server = TestServer::start(&server_dir);

    // 服务器指纹不对：登记失败，但仍然可以直连
    let (h, _, mut host_seen) = host(&host_dir, "secret-pw", Some(server.server(Some([9; 32]))));
    let failed = wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::RegisterFailed(_)),
    );
    assert!(
        matches!(
            failed,
            HostEvent::RegisterFailed(NetError::PinMismatch { .. })
        ),
        "{failed:?}"
    );
    drop(h);

    let (h, _, mut host_seen) = host(
        &host_dir,
        "secret-pw",
        Some(server.server(Some(server.pin))),
    );
    let registered = wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::Registered { .. }),
    );
    let HostEvent::Registered { code, .. } = registered else {
        panic!("{registered:?}")
    };
    let device = peerdesk_net::rendezvous::Lookup::Code(code);

    let (repaint, _) = counter();
    let v = start_view(
        Target::Device {
            server: server.server(None),
            device,
            policy: peerdesk_net::rendezvous::PathPolicy::RelayOnly,
            account: String::new(),
        },
        "secret-pw",
        ctrl_dir.clone(),
        repaint,
    );
    let mut seen = Log::new();
    let connected = wait_for(
        || v.poll(),
        &mut seen,
        |e| matches!(e, ViewEvent::Connected { .. }),
    );
    assert!(
        matches!(
            connected,
            ViewEvent::Connected {
                route: Route::Relay,
                ..
            }
        ),
        "{connected:?}"
    );
    wait_for(
        || h.poll(),
        &mut host_seen,
        |e| *e == HostEvent::Established,
    );
    drop(v);
    wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::SessionEnded(_)),
    );
    drop(h);
    drop(server);
    for dir in [server_dir, host_dir, ctrl_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// 开启被控时屏幕就打不开（没有权限等）：不提供远程，说明原因。
#[test]
fn host_that_cannot_open_the_screen_says_why() {
    let dir = temp_dir("unavailable");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(crate::local::TEST_LOCK_FILE), b"").unwrap();
    let (repaint, _) = counter();
    let h = start_host(
        HostConfig {
            port: 0,
            data_dir: dir.clone(),
            server: None,
            fps: 30,
        },
        "pw",
        repaint,
    );
    let mut seen = Log::new();
    let e = wait_for(
        || h.poll(),
        &mut seen,
        |e| matches!(e, HostEvent::Unavailable(_)),
    );
    assert!(
        matches!(&e, HostEvent::Unavailable(why) if why.contains("锁定")),
        "{e:?}"
    );
}

#[test]
fn locked_screen_refuses_one_connection_and_keeps_hosting() {
    let (host_dir, ctrl_dir) = (temp_dir("h7"), temp_dir("c7"));
    let (h, port, mut host_seen) = host(&host_dir, "pw", None);
    // 屏幕暂时采集不到（例如锁屏）：拒绝这一次，原因告诉控制端
    let lock = host_dir.join(crate::local::TEST_LOCK_FILE);
    std::fs::write(&lock, b"").unwrap();
    let (repaint, _) = counter();
    let v = start_view(local(port, None), "pw", ctrl_dir.clone(), repaint);
    let err = ended(&v, &mut Log::new()).unwrap_err();
    assert!(err.to_string().contains("屏幕已锁定"), "{err}");
    let refused = wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::Refused(_)),
    );
    assert!(matches!(refused, HostEvent::Refused(ref why) if why.contains("屏幕已锁定")));
    // 解锁后，同一个被控端接受下一次连接（原来会停止被控，设备离线）
    std::fs::remove_file(&lock).unwrap();
    let (repaint, _) = counter();
    let v = start_view(local(port, None), "pw", ctrl_dir.clone(), repaint);
    wait_for(
        || v.poll(),
        &mut Log::new(),
        |e| matches!(e, ViewEvent::Connected { .. }),
    );
    wait_for(
        || h.poll(),
        &mut host_seen,
        |e| *e == HostEvent::Established,
    );
    drop(v);
    drop(h);
    for d in [host_dir, ctrl_dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// 被控端在 `server` 上登记，控制端按设备码只走中继连过去，等到连上。
fn relay_session(
    server: &TestServer,
    host_dir: &Path,
    ctrl_dir: &Path,
) -> (HostHandle, Log<HostEvent>, ViewHandle, Log<ViewEvent>) {
    let (h, _, mut host_seen) = host(host_dir, "secret-pw", Some(server.server(Some(server.pin))));
    let HostEvent::Registered { code, .. } = wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::Registered { .. }),
    ) else {
        panic!("不是 Registered")
    };
    let (repaint, _) = counter();
    let v = start_view(
        Target::Device {
            server: server.server(None),
            device: peerdesk_net::rendezvous::Lookup::Code(code),
            policy: peerdesk_net::rendezvous::PathPolicy::RelayOnly,
            account: String::new(),
        },
        "secret-pw",
        ctrl_dir.to_path_buf(),
        repaint,
    );
    let mut seen = Log::new();
    wait_for(
        || v.poll(),
        &mut seen,
        |e| matches!(e, ViewEvent::Connected { .. }),
    );
    (h, host_seen, v, seen)
}

#[test]
fn free_relay_quota_ends_the_session_with_a_notice() {
    let (server_dir, host_dir, ctrl_dir) = (temp_dir("s4"), temp_dir("h4"), temp_dir("c4"));
    // 每台设备每 10 分钟只给 30 KB：握手后传一会儿就超
    let server = TestServer::start_with(
        &server_dir,
        peerdesk_core::relay::FreePolicy {
            window_us: 600_000_000,
            device_bytes: 30_000,
            ip_bytes: 0,
        },
    );
    let (h, _, v, mut seen) = relay_session(&server, &host_dir, &ctrl_dir);
    let notice = wait_for(
        || v.poll(),
        &mut seen,
        |e| matches!(e, ViewEvent::Notice(_)),
    );
    let ViewEvent::Notice(text) = notice else {
        panic!("{notice:?}")
    };
    assert!(text.contains("免费服务器拥挤"), "{text}");
    let summary = ended(&v, &mut seen).unwrap();
    assert_eq!(summary.end, text);
    drop(v);
    drop(h);
    drop(server);
    for dir in [server_dir, host_dir, ctrl_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn server_crash_shows_reconnecting_then_gives_up() {
    let (server_dir, host_dir, ctrl_dir) = (temp_dir("s5"), temp_dir("h5"), temp_dir("c5"));
    let server = TestServer::start(&server_dir);
    let (h, _, v, mut seen) = relay_session(&server, &host_dir, &ctrl_dir);
    // 中继所在的服务器崩了：控制端显示重连中，换通道（这里没有别的节点）重试，用完次数报错
    drop(server);
    let crashed = Instant::now();
    let mut first = None;
    let mut attempts = Vec::new();
    let result = loop {
        match wait_for(
            || v.poll(),
            &mut seen,
            |e| matches!(e, ViewEvent::Reconnecting { .. } | ViewEvent::Ended(_)),
        ) {
            ViewEvent::Reconnecting { attempt, .. } => {
                first.get_or_insert_with(|| crashed.elapsed());
                attempts.push(attempt);
            }
            ViewEvent::Ended(r) => break r,
            other @ (ViewEvent::Connected { .. }
            | ViewEvent::Notice(_)
            | ViewEvent::Clipboard(_)
            | ViewEvent::Quality(_)
            | ViewEvent::Peer(..)) => panic!("{other:?}"),
        }
    };
    assert_eq!(
        attempts,
        (1..=RECONNECT_TRIES).collect::<Vec<_>>(),
        "{result:?}"
    );
    assert!(result.is_err(), "{result:?}");
    // 门槛：崩溃后 5 秒内显示“网络重连中”（不等 10 秒空闲超时）
    let first = first.unwrap();
    assert!(first < Duration::from_secs(5), "{first:?}");
    drop(v);
    drop(h);
    for dir in [server_dir, host_dir, ctrl_dir] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn reconnect_helpers() {
    assert!(network_lost("timed out"));
    assert!(network_lost("connection lost"));
    assert!(!network_lost("对端结束会话：timed out"));
    assert!(!network_lost("用户结束"));
    assert!(free_limit_text(0).contains("约 1 分钟"));
    assert!(free_limit_text(61).contains("约 2 分钟"));
    let server = |port: u16| Server {
        addr: (Ipv4Addr::LOCALHOST, port).into(),
        pin: None,
    };
    let order = server_order(server(1), vec![server(2), server(1), server(3), server(2)]);
    let ports: Vec<u16> = order.iter().map(|s| s.addr.port()).collect();
    assert_eq!(ports, [1, 2, 3]);
}

#[test]
fn pixels_are_converted() {
    let mut screen = Screen::new(2, 1, 0x0011_2233);
    screen.fill(1, 0, 1, 1, 0x00AA_BBCC);
    let image = to_color_image(&screen);
    assert_eq!(image.size, [2, 1]);
    assert_eq!(
        image.pixels,
        vec![
            Color32::from_rgb(0x11, 0x22, 0x33),
            Color32::from_rgb(0xAA, 0xBB, 0xCC)
        ]
    );
    // 通过服务器连接时用双栈端口；直接连地址时跟着地址族
    assert!(view_bind(&Target::Device {
        server: Server {
            addr: "1.2.3.4:1".parse().unwrap(),
            pin: None
        },
        device: peerdesk_net::rendezvous::Lookup::Device([0; 32]),
        policy: peerdesk_net::rendezvous::PathPolicy::Auto,
        account: String::new(),
    })
    .is_ipv6());
    let v4 = Target::Addr {
        addr: "1.2.3.4:1".parse().unwrap(),
        pin: None,
    };
    let v6 = Target::Addr {
        addr: "[::1]:1".parse().unwrap(),
        pin: None,
    };
    assert!(view_bind(&v4).is_ipv4() && view_bind(&v6).is_ipv6());
}
