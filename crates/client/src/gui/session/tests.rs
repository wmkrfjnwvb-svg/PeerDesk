//! 会话线程的端到端测试：同一进程里一个被控端、一个或几个控制端，走真实的 QUIC。

use super::*;
use peerdesk_net::service::{RelayService, SignalService};
use std::sync::atomic::AtomicUsize;

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
        ViewEvent::Connected { .. } => panic!("不是 Ended"),
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
        let identity = Identity::load_or_create(dir).unwrap();
        let rt = runtime().unwrap();
        let (node, signal, relay) = rt.block_on(async {
            let relay = Arc::new(
                RelayService::start(([127, 0, 0, 1], 0).into(), None)
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
    wait_for(
        || h.poll(),
        &mut host_seen,
        |e| matches!(e, HostEvent::Registered { .. }),
    );
    let device = Identity::load_or_create(&host_dir).unwrap().fingerprint();

    let (repaint, _) = counter();
    let v = start_view(
        Target::Device {
            server: server.server(None),
            device,
            policy: peerdesk_net::rendezvous::PathPolicy::RelayOnly,
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
    assert!(wants_ipv6(&Target::Device {
        server: Server {
            addr: "[::1]:1".parse().unwrap(),
            pin: None
        },
        device: [0; 32],
        policy: peerdesk_net::rendezvous::PathPolicy::Auto,
    }));
}
