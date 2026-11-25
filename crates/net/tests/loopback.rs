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
use peerdesk_core::screen::Workload;
use peerdesk_core::session::SessionConfig;
use peerdesk_net::controller::{self, ControllerOptions, Upload};
use peerdesk_net::demo::{CountSink, DemoDesktop};
use peerdesk_net::fs_store::{scan, PARTIAL_SUFFIX};
use peerdesk_net::host::{self, HostOptions, InputSink};
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

/// 链路测试默认用块编码：行为确定（发几帧到几帧）、调试构建里也快。
/// H.264 由 [`session_h264`] 的完整会话测试和 crates/e2e 覆盖。
fn session(id: &Identity, policy: &Policy) -> SessionConfig {
    SessionConfig {
        caps: Caps(0),
        ..session_h264(id, policy)
    }
}

fn session_h264(id: &Identity, policy: &Policy) -> SessionConfig {
    SessionConfig {
        device_id: id.device_id(),
        region: "test".into(),
        caps: Caps::H264,
        policy: policy.clone(),
        auth: peerdesk_core::auth::Auth::Open,
        binding: Vec::new(),
    }
}

#[derive(Default, Clone)]
struct Recorder(Arc<Mutex<Vec<InputEvent>>>);
impl InputSink for Recorder {
    fn inject(&mut self, event: &InputEvent) {
        self.0.lock().unwrap().push(event.clone());
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
    let sink = CountSink::new();
    // 被控端的桌面：打字 10 帧、拖窗口 10 帧，然后静止。会话 1.5 秒：即使定时器粒度很粗
    // （Windows 上约 15 毫秒一次）也足够它做完并收敛。
    let desktop = DemoDesktop::new(640, 360, 1)
        .with_script(vec![(Workload::Typing, 10), (Workload::Dragging, 10)]);
    let mirror = desktop.mirror();
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(session_h264(&p.host_id, &policy), 100, dst.clone()),
        Box::new(desktop),
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
            session: session_h264(&p.ctrl_id, &policy),
            deadline_us: 200_000,
            duration: Some(Duration::from_millis(1500)),
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(sink.clone()),
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

    // 画面：H.264 是有损的，观看端最终看到的与被控端的桌面在亮度上足够接近
    let seen = report.screen.clone().unwrap();
    assert_eq!((seen.width(), seen.height()), (640, 360));
    let psnr = luma_psnr(&seen, &mirror.lock().unwrap());
    // 这里只验证链路：内容对上了就行。CPU 很忙时（调试构建、并行测试）反馈会迟到，
    // 码率会被压低、画质跟着降，所以门槛放宽；画质由确定性的端到端测试（crates/e2e）把关。
    assert!(psnr > 20.0, "观看端画面与被控端对不上：{psnr:.1} dB");
    // 桌面变了 20 次；编码器忙时期间的变化合并成最新的一帧（不排队），所以帧数可能少于 20。
    assert!(host_report.frames_sent >= 5, "{}", host_report.frames_sent);
    // 桌面静止后不再发帧：帧数远少于 1.5 秒 × 100 帧每秒
    // （机器很慢时个别帧会被判定超时而重发，所以留一些余量。）
    assert!(host_report.frames_sent < 60, "{}", host_report.frames_sent);
    // 视频编码丢了一帧就要等关键帧，CPU 很忙时可能有个别帧没显示。
    assert!(report.frames_delivered <= u64::from(host_report.frames_sent));
    assert!(
        report.frames_delivered >= 10,
        "送到 {}，发出 {}",
        report.frames_delivered,
        host_report.frames_sent
    );
    assert_eq!(sink.count(), report.frames_delivered);
    assert!(report.video_bytes <= host_report.video_bytes && report.video_bytes > 0);
    assert_eq!(report.bad_frames, 0);
    assert_eq!(host_report.profile, "stable");
    assert!(host_report.final_rate_bps >= 100_000);
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
        Box::new(DemoDesktop::new(320, 240, 1).with_limit(10_000)),
        Box::new(Recorder::default()),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let report = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&p.ctrl_id, &policy),
            deadline_us: 200_000,
            duration: Some(Duration::from_millis(50)),
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(CountSink::new()),
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
        Box::new(DemoDesktop::new(320, 240, 1).with_limit(5)),
        Box::new(Recorder::default()),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let report = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&p.ctrl_id, &policy),
            deadline_us: 100_000,
            duration: None,
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(CountSink::new()),
        rx,
        None,
    )
    .await
    .unwrap();
    let host_report = host_task.await.unwrap().unwrap();
    assert_eq!(host_report.end, "画面来源结束");
    // 编码线程忙时，期间的画面变化合并成最新的一帧（不排队），所以最多 5 帧。
    assert!(
        (1..=5).contains(&host_report.frames_sent),
        "{}",
        host_report.frames_sent
    );
    // 被控端发完最后一帧就结束会话：结束消息走可靠流，可能比还在路上的视频数据报先到，
    // 所以送达几帧不确定；这个测试要的是控制端知道被控端结束了。
    assert!(report.frames_delivered <= u64::from(host_report.frames_sent));
    assert!(report.files.is_empty());
}

/// 打开发送节奏（实验开关）：会话照常建立、画面送达。
#[tokio::test(flavor = "multi_thread")]
async fn paced_session_delivers_frames() {
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let (host_link, ctrl_link) = links(&p).await;
    let mut opts = HostOptions::new(session_h264(&p.host_id, &policy), 30, temp_dir("paced"));
    opts.pacing = true;
    // 起步码率很低：大帧要靠发送节奏分几次发出
    opts.start_bps = 100_000;
    opts.max_bps = 150_000;
    let host_task = tokio::spawn(host::serve(
        host_link,
        opts,
        Box::new(DemoDesktop::new(1280, 720, 1).with_limit(40)),
        Box::new(Recorder::default()),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let report = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session_h264(&p.ctrl_id, &policy),
            deadline_us: 100_000,
            duration: Some(Duration::from_secs(4)),
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(CountSink::new()),
        rx,
        None,
    )
    .await
    .unwrap();
    let host_report = host_task.await.unwrap().unwrap();
    assert!(host_report.frames_sent > 0);
    assert!(report.frames_delivered > 0, "{report:?}");
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
        Box::new(DemoDesktop::new(320, 240, 1).with_limit(5)),
        Box::new(Recorder::default()),
    ));
    let (mut send, mut recv) = ctrl_link.open_stream().await.unwrap();
    use peerdesk_net::stream::{read_control, write_control, write_kind, StreamKind};
    write_kind(&mut send, StreamKind::Control).await.unwrap();
    let hello = peerdesk_core::proto::Control::Hello {
        version: 0,
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
        Box::new(DemoDesktop::new(320, 240, 1).with_limit(5)),
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
        Box::new(DemoDesktop::new(320, 240, 1).with_limit(0)),
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
        Box::new(DemoDesktop::new(320, 240, 1).with_limit(5)),
        Box::new(Recorder::default()),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let result = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&p.ctrl_id, &only_chacha),
            deadline_us: 1,
            duration: None,
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(CountSink::new()),
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

/// 画面静止时网络上几乎没有流量：这是“静止不发”在真实连接上的验证，不只是仿真里的数字。
#[tokio::test(flavor = "multi_thread")]
async fn idle_screen_costs_almost_nothing_on_the_wire() {
    async fn idle_session(seconds: u64) -> u64 {
        let policy = Policy::recommended();
        let p = pair(&policy, &policy);
        let (host_link, ctrl_link) = links(&p).await;
        // 脚本为空：桌面从头到尾静止，只有首屏。
        let desktop = DemoDesktop::new(640, 360, 1).with_script(vec![]);
        let host_task = tokio::spawn(host::serve(
            host_link,
            HostOptions::new(session(&p.host_id, &policy), 30, temp_dir("idle")),
            Box::new(desktop),
            Box::new(Recorder::default()),
        ));
        let (_tx, rx) = mpsc::channel(1);
        let report = controller::run(
            ctrl_link,
            ControllerOptions {
                session: session(&p.ctrl_id, &policy),
                deadline_us: 200_000,
                duration: Some(Duration::from_secs(seconds)),
                clipboard: None,
                on_established: None,
                on_link: None,
                view: None,
                on_quality: None,
                audio: None,
            },
            Box::new(CountSink::new()),
            rx,
            None,
        )
        .await
        .unwrap();
        host_task.await.unwrap().unwrap();
        assert!(report.screen.is_some());
        report.wire_bytes.0 + report.wire_bytes.1
    }
    // 两次会话只差 4 秒的静止时间，字节数之差就是静止的代价。
    let (short, long) = tokio::join!(idle_session(1), idle_session(5));
    let idle_bytes_per_s = long.saturating_sub(short) / 4;
    // 目标是 5 kbps（625 字节每秒）以内：只剩保活包和每秒一次的反馈。
    assert!(
        idle_bytes_per_s < 625,
        "静止时每秒 {idle_bytes_per_s} 字节（1 秒会话 {short}，5 秒会话 {long}）"
    );
}

/// 亮度 PSNR（dB）。
fn luma_psnr(a: &peerdesk_core::screen::Screen, b: &peerdesk_core::screen::Screen) -> f64 {
    let luma = |p: u32| {
        0.299 * f64::from((p >> 16) & 0xff)
            + 0.587 * f64::from((p >> 8) & 0xff)
            + 0.114 * f64::from(p & 0xff)
    };
    let sum: f64 = a
        .pixels()
        .iter()
        .zip(b.pixels())
        .map(|(x, y)| (luma(*x) - luma(*y)).powi(2))
        .sum();
    let mse = sum / a.pixels().len() as f64;
    10.0 * (255.0 * 255.0 / mse.max(1e-9)).log10()
}

/// 会报告变化计数的画面来源：被控端靠计数判断画面变没变，不再整屏比较。
struct Counted {
    inner: DemoDesktop,
    seq: u64,
}

impl host::ScreenSource for Counted {
    fn frame(&mut self) -> Option<peerdesk_core::frame::Frame> {
        self.seq += 1;
        self.inner.frame()
    }

    fn change_seq(&self) -> Option<u64> {
        // 每两次取画面才算变一次：没变的那一次不应该编码
        Some(self.seq / 2)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sources_with_a_change_counter_are_encoded_only_when_it_moves() {
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let (host_link, ctrl_link) = links(&p).await;
    let host_task = tokio::spawn(host::serve(
        host_link,
        // 预算给足：第一幅画面一帧发完，帧数只取决于变化计数。
        HostOptions {
            start_bps: 50_000_000,
            max_bps: 50_000_000,
            ..HostOptions::new(session(&p.host_id, &policy), 100, temp_dir("seq"))
        },
        Box::new(Counted {
            inner: DemoDesktop::new(320, 240, 1).with_limit(12),
            seq: 0,
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
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(CountSink::new()),
        rx,
        None,
    )
    .await
    .unwrap();
    let host_report = host_task.await.unwrap().unwrap();
    assert_eq!(host_report.end, "画面来源结束");
    // 12 次取画面里计数只动了 6 次左右：没动的那几次不编码
    assert!(
        (1..=7).contains(&host_report.frames_sent),
        "{}",
        host_report.frames_sent
    );
    assert!(report.frames_delivered <= u64::from(host_report.frames_sent));
}

/// 控制端窗口只有 320×180：被控端照样按原尺寸编码（业内做法：按原分辨率传，控制端显卡缩放，文字最清楚；
/// 只有网络不够、按比例缩小时才放进窗口）。开始的半分辨率阶段按窗口大小编。
#[tokio::test(flavor = "multi_thread")]
async fn host_encodes_native_size_even_in_a_small_window() {
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let (host_link, ctrl_link) = links(&p).await;
    let with_view = |id: &Identity| SessionConfig {
        caps: Caps::H264.with(Caps::VIEW_SIZE),
        ..session_h264(id, &policy)
    };
    // 一直在打字：画面不停变化
    let desktop = DemoDesktop::new(640, 360, 1).with_script(vec![(Workload::Typing, 1000)]);
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(with_view(&p.host_id), 30, temp_dir("view")),
        Box::new(desktop),
        Box::new(Recorder::default()),
    ));
    let (_view_tx, view_rx) = tokio::sync::watch::channel((320u16, 180u16));
    let (_tx, rx) = mpsc::channel(1);
    let report = controller::run(
        ctrl_link,
        ControllerOptions {
            session: with_view(&p.ctrl_id),
            deadline_us: 200_000,
            duration: Some(Duration::from_millis(1500)),
            clipboard: None,
            on_established: None,
            on_link: None,
            view: Some(view_rx),
            on_quality: None,
            audio: None,
        },
        Box::new(CountSink::new()),
        rx,
        None,
    )
    .await
    .unwrap();
    let _ = host_task.await.unwrap().unwrap();
    let seen = report.screen.unwrap();
    assert_eq!((seen.width(), seen.height()), (640, 360));
}

/// 显卡画面的来源（模拟 ScreenCaptureKit）：按会话告诉的编码尺寸交 NV12 画面，记下每次要的尺寸。
struct GpuSource {
    native: (u16, u16),
    target: (u16, u16),
    targets: Arc<Mutex<Vec<(u16, u16)>>>,
    frames: u32,
}

impl host::ScreenSource for GpuSource {
    fn frame(&mut self) -> Option<peerdesk_core::frame::Frame> {
        use peerdesk_core::frame::{Frame, Nv12, TestGpuFrame};
        self.frames += 1;
        if self.frames > 400 {
            return None;
        }
        let (w, h) = (usize::from(self.target.0), usize::from(self.target.1));
        // 每帧亮度不同：画面一直在变
        let luma = u8::try_from(16 + self.frames % 200).unwrap();
        let mut data = vec![luma; w * h];
        data.extend(std::iter::repeat_n(128u8, w * h / 2));
        let nv12 = Nv12::new(self.target.0, self.target.1, data).unwrap();
        Some(Frame::Gpu(Arc::new(TestGpuFrame::new(nv12))))
    }

    fn change_seq(&self) -> Option<u64> {
        Some(u64::from(self.frames))
    }

    fn native_size(&self) -> Option<(u16, u16)> {
        Some(self.native)
    }

    fn set_target_size(&mut self, size: (u16, u16)) {
        self.target = size;
        self.targets.lock().unwrap().push(size);
    }
}

/// 显卡画面整条链路：被控端把编码尺寸告诉采集端（开始半分辨率、之后原尺寸），交来的显卡画面直接编
/// （这里没有硬件编码器：软件编码读回一次 NV12），控制端看到原尺寸的画面。
#[tokio::test(flavor = "multi_thread")]
async fn gpu_frames_are_captured_at_the_encode_size() {
    let policy = Policy::recommended();
    let p = pair(&policy, &policy);
    let (host_link, ctrl_link) = links(&p).await;
    let targets = Arc::new(Mutex::new(Vec::new()));
    let source = GpuSource {
        native: (640, 360),
        target: (640, 360),
        targets: Arc::clone(&targets),
        frames: 0,
    };
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(session_h264(&p.host_id, &policy), 30, temp_dir("gpu")),
        Box::new(source),
        Box::new(Recorder::default()),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let report = controller::run(
        ctrl_link,
        ControllerOptions {
            session: session_h264(&p.ctrl_id, &policy),
            deadline_us: 200_000,
            duration: Some(Duration::from_millis(1500)),
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(CountSink::new()),
        rx,
        None,
    )
    .await
    .unwrap();
    let _ = host_task.await.unwrap().unwrap();
    let asked = targets.lock().unwrap().clone();
    assert_eq!(asked.first(), Some(&(320, 180)), "开始半分辨率：{asked:?}");
    assert!(asked.contains(&(640, 360)), "之后原尺寸：{asked:?}");
    assert!(report.frames_delivered > 5, "{}", report.frames_delivered);
    let seen = report.screen.unwrap();
    assert_eq!((seen.width(), seen.height()), (640, 360));
}
