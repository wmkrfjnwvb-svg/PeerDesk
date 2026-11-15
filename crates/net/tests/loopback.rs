//! 在回环地址上用真实的 QUIC 连接跑完整会话。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peerdesk_core::crypto::{Policy, Suite};
use peerdesk_core::file::Outcome;
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::Caps;
use peerdesk_core::session::SessionConfig;
use peerdesk_core::transport::Frame;
use peerdesk_net::controller::{self, ControllerOptions, FrameSink, Upload};
use peerdesk_net::fs_store::{scan, PARTIAL_SUFFIX};
use peerdesk_net::host::{self, FrameSource, HostOptions, InputSink};
use peerdesk_net::{bench, Identity, Link, NetError, Node};
use tokio::sync::mpsc;

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-net-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn session(id: &Identity, policy: &Policy) -> SessionConfig {
    SessionConfig {
        device_id: id.device_id(),
        region: "test".into(),
        caps: Caps::H264,
        policy: policy.clone(),
        auth: peerdesk_core::auth::Auth::Open,
        binding: Vec::new(),
    }
}

/// 每帧内容由帧号决定，接收端据此校验。
struct Pattern {
    remaining: u32,
    size: usize,
    n: u32,
}
impl FrameSource for Pattern {
    fn next_frame(&mut self) -> Option<Vec<u8>> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        self.n += 1;
        Some(vec![u8::try_from(self.n % 251).unwrap(); self.size])
    }
}

#[derive(Default, Clone)]
struct Recorder(Arc<Mutex<Vec<InputEvent>>>);
impl InputSink for Recorder {
    fn inject(&mut self, event: &InputEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[derive(Default, Clone)]
struct Checker {
    good: Arc<Mutex<u32>>,
    bad: Arc<Mutex<u32>>,
}
impl FrameSink for Checker {
    fn frame(&mut self, frame: &Frame) {
        let expected = u8::try_from(frame.frame_id % 251).unwrap();
        let ok = frame.data.iter().all(|b| *b == expected);
        *(if ok { &self.good } else { &self.bad }).lock().unwrap() += 1;
    }
}

struct Pair {
    host_node: Node,
    ctrl_node: Node,
    host_id: Identity,
    ctrl_id: Identity,
}

fn pair(host_policy: &Policy, ctrl_policy: &Policy) -> Pair {
    let (host_id, ctrl_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    Pair {
        host_node: Node::bind(any(), &host_id, host_policy).unwrap(),
        ctrl_node: Node::bind(any(), &ctrl_id, ctrl_policy).unwrap(),
        host_id,
        ctrl_id,
    }
}

async fn links(p: &Pair) -> (Link, Link) {
    let addr = p.host_node.local_addr().unwrap();
    let pin = p.host_id.fingerprint();
    let (c, h) = tokio::join!(p.ctrl_node.connect(addr, Some(&pin)), p.host_node.accept());
    (h.unwrap().unwrap(), c.unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn full_session_video_input_and_files() {
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let (host_link, ctrl_link) = links(&p).await;
    assert_eq!(host_link.peer_fingerprint(), &p.ctrl_id.fingerprint());
    assert_eq!(ctrl_link.peer_fingerprint(), &p.host_id.fingerprint());
    assert_eq!(ctrl_link.remote_addr(), p.host_node.local_addr().unwrap());
    assert_eq!(p.host_node.fingerprint(), &p.host_id.fingerprint());
    assert!(ctrl_link.max_payload() > 1000);

    // 要上传的文件：一个小文件、一个多块的大文件、一个空文件
    let (src, dst) = (temp_dir("src"), temp_dir("dst"));
    std::fs::create_dir_all(src.join("up/sub")).unwrap();
    let big: Vec<u8> = (0..700_000u32)
        .map(|i| u8::try_from(i % 253).unwrap())
        .collect();
    std::fs::write(src.join("up/big.bin"), &big).unwrap();
    std::fs::write(src.join("up/sub/note.txt"), "你好").unwrap();
    std::fs::write(src.join("up/empty"), b"").unwrap();
    let files = scan(&src, "up").unwrap();

    let recorder = Recorder::default();
    let checker = Checker::default();
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(session(&p.host_id, &policy), 100, dst.clone()),
        Box::new(Pattern {
            remaining: 10_000,
            size: 5000,
            n: 0,
        }),
        Box::new(recorder.clone()),
    ));

    let (tx, rx) = mpsc::channel(64);
    let events = vec![
        InputEvent::Key {
            code: 4,
            down: true,
        },
        InputEvent::MouseMove { x: 100, y: 200 },
        InputEvent::MouseButton {
            button: 0,
            down: true,
        },
        InputEvent::Text("hi".into()),
        InputEvent::Key {
            code: 4,
            down: false,
        },
    ];
    for e in &events {
        tx.send(e.clone()).await.unwrap();
    }
    drop(tx);

    let report = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&p.ctrl_id, &policy),
            deadline_us: 200_000,
            duration: Some(Duration::from_millis(600)),
        },
        Box::new(checker.clone()),
        rx,
        Some(Upload {
            root: src.clone(),
            files,
        }),
    )
    .await
    .unwrap();
    let host_report = host_task.await.unwrap().unwrap();

    // 握手
    assert_eq!(report.suite, Suite::Aes256Gcm);
    assert_eq!(host_report.suite, Some(Suite::Aes256Gcm));
    assert_eq!(report.peer, p.host_id.fingerprint());
    assert_eq!(host_report.peer, p.ctrl_id.fingerprint());
    assert!(report.trace.contains("established"));
    assert!(host_report.trace.contains("accept_sent"));
    assert_eq!(report.end, "到达时长上限");
    assert_eq!(host_report.end, "对端结束会话");

    // 画面：回环上应当几乎全部送达，内容全部正确
    assert!(host_report.frames_sent >= 30, "{}", host_report.frames_sent);
    assert!(report.frames_delivered >= 30, "{report:?}");
    assert_eq!(*checker.bad.lock().unwrap(), 0);
    assert_eq!(
        u64::from(*checker.good.lock().unwrap()),
        report.frames_delivered
    );
    assert_eq!(report.video_bytes, report.frames_delivered * 5000);
    assert_eq!(report.bad_datagrams, 0);
    assert_eq!(host_report.bad_datagrams, 0);

    // 输入：按顺序、不重复地注入；会话结束时还按着的鼠标左键被自动释放
    let injected = recorder.0.lock().unwrap().clone();
    let mut expected = events.clone();
    expected.push(InputEvent::MouseButton {
        button: 0,
        down: false,
    });
    assert_eq!(injected, expected);
    assert_eq!(report.input_sent, 5);
    assert_eq!(report.input_unacked, 0);
    assert_eq!(host_report.input_events, 5);

    // 文件
    assert_eq!(report.files.len(), 3);
    assert!(
        report
            .files
            .iter()
            .all(|f| f.outcome == Some(Outcome::Done)),
        "{:?}",
        report.files
    );
    assert_eq!(report.file_bytes, 700_000 + 6);
    assert_eq!(host_report.files.len(), 3);
    assert_eq!(std::fs::read(dst.join("up/big.bin")).unwrap(), big);
    assert_eq!(
        std::fs::read_to_string(dst.join("up/sub/note.txt")).unwrap(),
        "你好"
    );
    assert_eq!(std::fs::read(dst.join("up/empty")).unwrap(), b"");
    assert!(report.rtt < Duration::from_millis(100));

    p.host_node.close();
    p.ctrl_node.close();
    p.host_node.wait_idle().await;
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dst);
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_upload_resumes_from_partial_file() {
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let (host_link, ctrl_link) = links(&p).await;
    let (src, dst) = (temp_dir("rsrc"), temp_dir("rdst"));
    let data: Vec<u8> = (0..500_000u32)
        .map(|i| u8::try_from(i % 241).unwrap())
        .collect();
    std::fs::write(src.join("a.bin"), &data).unwrap();
    // 上次传了 30 万字节后中断，留下了 .part 文件
    std::fs::write(dst.join(format!("a.bin{PARTIAL_SUFFIX}")), &data[..300_000]).unwrap();

    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(session(&p.host_id, &policy), 30, dst.clone()),
        Box::new(Pattern {
            remaining: 10_000,
            size: 100,
            n: 0,
        }),
        Box::new(Recorder::default()),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let report = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&p.ctrl_id, &policy),
            deadline_us: 200_000,
            duration: Some(Duration::from_millis(50)),
        },
        Box::new(Checker::default()),
        rx,
        Some(Upload {
            root: src.clone(),
            files: scan(&src, "a.bin").unwrap(),
        }),
    )
    .await
    .unwrap();
    host_task.await.unwrap().unwrap();
    assert_eq!(report.file_bytes, 200_000, "只发送缺的那部分");
    assert_eq!(std::fs::read(dst.join("a.bin")).unwrap(), data);
    assert!(!dst.join(format!("a.bin{PARTIAL_SUFFIX}")).exists());
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dst);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_ending_first_is_reported_to_controller() {
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let (host_link, ctrl_link) = links(&p).await;
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(session(&p.host_id, &policy), 200, temp_dir("h")),
        Box::new(Pattern {
            remaining: 5,
            size: 100,
            n: 0,
        }),
        Box::new(Recorder::default()),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let report = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&p.ctrl_id, &policy),
            deadline_us: 100_000,
            duration: None,
        },
        Box::new(Checker::default()),
        rx,
        None,
    )
    .await
    .unwrap();
    let host_report = host_task.await.unwrap().unwrap();
    assert_eq!(host_report.end, "画面来源结束");
    assert_eq!(host_report.frames_sent, 5);
    assert_eq!(report.frames_delivered, 5);
    assert!(report.files.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_fingerprint_is_refused() {
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let addr = p.host_node.local_addr().unwrap();
    let wrong = p.ctrl_id.fingerprint(); // 不是被控端的指纹
    let (c, _h) = tokio::join!(
        p.ctrl_node.connect(addr, Some(&wrong)),
        p.host_node.accept()
    );
    match c {
        Err(NetError::PinMismatch { expected, actual }) => {
            assert_eq!(expected, peerdesk_net::identity::to_hex(&wrong));
            assert_eq!(
                actual,
                peerdesk_net::identity::to_hex(&p.host_id.fingerprint())
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn disjoint_cipher_policies_cannot_connect() {
    let p = pair(
        &Policy::custom(&[Suite::Aes256Gcm]).unwrap(),
        &Policy::custom(&[Suite::ChaCha20Poly1305]).unwrap(),
    );
    let addr = p.host_node.local_addr().unwrap();
    let (c, h) = tokio::join!(p.ctrl_node.connect(addr, None), p.host_node.accept());
    assert!(matches!(c, Err(NetError::Connect(_))), "{c:?}");
    assert!(matches!(h, Some(Err(NetError::Connect(_)))), "{h:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn each_single_suite_policy_connects() {
    for suite in [Suite::Aes128Gcm, Suite::Aes256Gcm, Suite::ChaCha20Poly1305] {
        let policy = Policy::custom(&[suite]).unwrap();
        let p = pair(&policy, &policy);
        let (host_link, ctrl_link) = links(&p).await;
        ctrl_link
            .send(peerdesk_core::proto::Channel::Video, b"ping")
            .unwrap();
        let (header, payload) = host_link.recv().await.unwrap();
        assert_eq!(header.channel, peerdesk_core::proto::Channel::Video);
        assert_eq!(payload, b"ping");
        ctrl_link.close("done");
        assert!(host_link.closed().await.contains("done"));
        assert!(matches!(host_link.recv().await, Err(NetError::Closed(_))));
        assert!(matches!(
            host_link.send(header.channel, b"x"),
            Err(NetError::Closed(_))
        ));
        assert!(matches!(
            host_link.open_stream().await,
            Err(NetError::Closed(_))
        ));
        assert!(matches!(
            host_link.accept_stream().await,
            Err(NetError::Closed(_))
        ));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn application_level_rejection_and_protocol_errors() {
    // TLS 能连上（都有 aes-256），但应用层版本不同的情况用伪造的 Hello 模拟
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let (host_link, ctrl_link) = links(&p).await;
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(session(&p.host_id, &policy), 30, temp_dir("rej")),
        Box::new(Pattern {
            remaining: 5,
            size: 10,
            n: 0,
        }),
        Box::new(Recorder::default()),
    ));
    let (mut send, mut recv) = ctrl_link.open_stream().await.unwrap();
    use peerdesk_net::stream::{read_control, write_control, write_kind, StreamKind};
    write_kind(&mut send, StreamKind::Control).await.unwrap();
    let hello = peerdesk_core::proto::Control::Hello {
        version: 99,
        device_id: 1,
        caps: Caps::H264,
        region: "x".into(),
        suites: vec![Suite::Aes256Gcm],
    };
    write_control(&mut send, &hello).await.unwrap();
    let reply = read_control(&mut recv).await.unwrap();
    assert_eq!(
        reply,
        peerdesk_core::proto::Control::Reject {
            reason: peerdesk_core::proto::RejectReason::Version
        }
    );
    ctrl_link.close("bye");
    assert!(matches!(
        host_task.await.unwrap(),
        Err(NetError::Connect(_))
    ));

    // 第一条流不是控制流
    let (host_link, ctrl_link) = links(&p).await;
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(session(&p.host_id, &policy), 30, temp_dir("rej2")),
        Box::new(Pattern {
            remaining: 5,
            size: 10,
            n: 0,
        }),
        Box::new(Recorder::default()),
    ));
    let (mut send, _recv) = ctrl_link.open_stream().await.unwrap();
    write_kind(&mut send, StreamKind::File).await.unwrap();
    assert_eq!(
        host_task.await.unwrap(),
        Err(NetError::Protocol("第一条流必须是控制流"))
    );

    // 帧率为 0
    let (host_link, _ctrl_link) = links(&p).await;
    let result = host::serve(
        host_link,
        HostOptions::new(session(&p.host_id, &policy), 0, temp_dir("rej3")),
        Box::new(Pattern {
            remaining: 0,
            size: 0,
            n: 0,
        }),
        Box::new(Recorder::default()),
    )
    .await;
    assert_eq!(result, Err(NetError::Protocol("帧率不能为 0")));
}

#[tokio::test(flavor = "multi_thread")]
async fn controller_reports_rejection() {
    // 被控端只允许不加密以外的套件，控制端应用层策略与之不相交：
    // 两端 TLS 都用 recommended，但应用层握手里被控端的策略更严格。
    let tls = Policy::recommended();
    let p = pair(&tls, &tls);
    let (host_link, ctrl_link) = links(&p).await;
    let strict = Policy::custom(&[Suite::Aes128Gcm]).unwrap();
    let only_chacha = Policy::custom(&[Suite::ChaCha20Poly1305]).unwrap();
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(session(&p.host_id, &strict), 30, temp_dir("cr")),
        Box::new(Pattern {
            remaining: 5,
            size: 10,
            n: 0,
        }),
        Box::new(Recorder::default()),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let result = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&p.ctrl_id, &only_chacha),
            deadline_us: 1,
            duration: None,
        },
        Box::new(Checker::default()),
        rx,
        None,
    )
    .await;
    match result {
        Err(NetError::Connect(why)) => assert!(why.contains("NoCommonSuite"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(host_task.await.unwrap().is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn connecting_to_nothing_fails_and_closed_endpoint_stops_accepting() {
    let policy = Policy::recommended();
    let id = Identity::generate().unwrap();
    let node = Node::bind(any(), &id, &policy).unwrap();
    // 没有人监听的端口
    let dead: SocketAddr = ([127, 0, 0, 1], 9).into();
    let started = std::time::Instant::now();
    let result = node.connect(dead, None).await;
    assert!(
        matches!(result, Err(NetError::Timeout(_) | NetError::Connect(_))),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(12));
    node.close();
    assert!(node.accept().await.is_none());
    // 端口被占用
    let taken = Node::bind(any(), &id, &policy).unwrap();
    assert!(matches!(
        Node::bind(taken.local_addr().unwrap(), &id, &policy),
        Err(NetError::Io(_))
    ));
    // 身份文件损坏：证书和私钥不是合法的 DER
    let broken_dir = temp_dir("broken-id");
    std::fs::write(broken_dir.join("identity.crt"), b"not a certificate").unwrap();
    std::fs::write(broken_dir.join("identity.key"), b"not a key").unwrap();
    let broken = Identity::load_or_create(&broken_dir).unwrap();
    assert!(matches!(
        Node::bind(any(), &broken, &policy),
        Err(NetError::Tls(_))
    ));
    let _ = std::fs::remove_dir_all(broken_dir);
    // 策略里没有加密套件
    assert!(matches!(
        Node::bind(any(), &id, &Policy::none()),
        Err(NetError::Tls(_))
    ));
    assert!(format!("{taken:?}").contains("Node"));
}

#[tokio::test(flavor = "multi_thread")]
async fn transport_benchmark_runs() {
    let result = bench::compare(300, 1100).await.unwrap();
    assert_eq!(result.packets, 300);
    // 回环上不应丢包
    assert_eq!(result.udp.received, 300);
    assert!(result.quic.received >= 290, "{result:?}");
    assert!(result.udp.p50_us <= result.udp.p99_us);
    assert!(result.quic.p99_us < 200_000, "{result:?}");
}
