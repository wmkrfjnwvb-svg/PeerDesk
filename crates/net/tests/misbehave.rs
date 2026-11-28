//! 对端行为异常时的健壮性：畸形数据报、乱序的控制消息、中途消失、文件传输中止。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use peerdesk_core::crypto::{Policy, Suite};
use peerdesk_core::file::{Deframer, FileMeta, FileMsg};
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::{Caps, Channel, Control, VERSION};
use peerdesk_core::session::SessionConfig;
use peerdesk_net::controller::{self, ControllerOptions, ScreenSink, Upload};
use peerdesk_net::demo::DemoDesktop;
use peerdesk_net::files::{receive_files, send_files};
use peerdesk_net::host::{self, HostOptions, InputSink};
use peerdesk_net::stream::{
    read_control, read_file_msgs, read_kind, write_control, write_file_msg, write_kind, StreamKind,
};
use peerdesk_net::{Identity, Link, NetError, Node};
use quinn::{RecvStream, SendStream};
use tokio::sync::mpsc;

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-mis-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn cfg() -> SessionConfig {
    SessionConfig {
        device_id: 7,
        region: "t".into(),
        caps: Caps::H264,
        policy: Policy::recommended(),
        auth: peerdesk_core::auth::Auth::Open,
        binding: Vec::new(),
    }
}

struct Null;
impl InputSink for Null {
    fn inject(&mut self, _: &InputEvent) {}
}
impl ScreenSink for Null {
    fn present(&mut self, _: &peerdesk_core::frame::Picture) {}
}

/// 持有两个端点，保证测试期间它们不被关闭。
struct Nodes(#[allow(dead_code)] Node, #[allow(dead_code)] Node);

/// 返回 (接受方的连接, 发起方的连接)。
async fn links() -> (Nodes, Link, Link) {
    let policy = Policy::recommended();
    let a = Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap();
    let b = Node::bind(any(), &Identity::generate().unwrap(), &policy).unwrap();
    let addr = a.local_addr().unwrap();
    let (out, inc) = tokio::join!(b.connect(addr, None), a.accept());
    (Nodes(a, b), inc.unwrap().unwrap(), out.unwrap())
}

fn hello() -> Control {
    Control::Hello {
        version: VERSION,
        device_id: 9,
        caps: Caps::H264,
        region: "t".into(),
        suites: vec![Suite::Aes256Gcm],
    }
}

/// 手工完成控制端一侧的握手。
async fn raw_controller_handshake(link: &Link) -> (SendStream, RecvStream) {
    let (mut send, mut recv) = link.open_stream().await.unwrap();
    write_kind(&mut send, StreamKind::Control).await.unwrap();
    write_control(&mut send, &hello()).await.unwrap();
    assert!(matches!(
        read_control(&mut recv).await.unwrap(),
        Control::Accept { .. }
    ));
    (send, recv)
}

/// 手工完成被控端一侧的握手，用 `answer` 作为应答。
async fn raw_host_handshake(link: &Link, answer: &Control) -> (SendStream, RecvStream) {
    let (mut send, mut recv) = link.accept_stream().await.unwrap();
    assert_eq!(read_kind(&mut recv).await.unwrap(), StreamKind::Control);
    assert!(matches!(
        read_control(&mut recv).await.unwrap(),
        Control::Hello { .. }
    ));
    write_control(&mut send, answer).await.unwrap();
    (send, recv)
}

const ACCEPT: Control = Control::Accept {
    suite: Suite::Aes256Gcm,
    session_id: 1,
    caps: Caps::H264,
    name: String::new(),
};

#[tokio::test(flavor = "multi_thread")]
async fn host_survives_a_hostile_controller() {
    let (_nodes, host_link, raw) = links().await;
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(cfg(), 50, temp_dir("h1")),
        Box::new(DemoDesktop::new(320, 240, 1)),
        Box::new(Null),
    ));
    let (mut send, mut recv) = raw_controller_handshake(&raw).await;

    // 四个畸形或不该出现的数据报
    raw.send(Channel::Video, b"controller must not send video")
        .unwrap();
    raw.send(Channel::Cursor, b"x").unwrap();
    raw.send(Channel::Input, &[0xff; 9]).unwrap();
    raw.send_raw(vec![0u8; 30]).unwrap(); // 魔数不对
    raw.send(Channel::Control, b"not a feedback message")
        .unwrap();
    // 超过当前路径允许的数据报长度：本地丢掉这一片（相当于丢包），不当作错误、不断开会话
    // （窄带宽上路径 MTU 会被 QUIC 调小，原来当错误处理时会话直接断开）
    assert_eq!(raw.send_raw(vec![0u8; 60_000]), Ok(()));

    // 第二条控制流：被拒绝但不影响会话
    let (mut dup, _dup_recv) = raw.open_stream().await.unwrap();
    write_kind(&mut dup, StreamKind::Control).await.unwrap();
    // 未知类型的流
    let (mut unknown, _unknown_recv) = raw.open_stream().await.unwrap();
    unknown.write_all(&[77]).await.unwrap();

    // 控制流上：Ping 得到 Pong；其他消息被忽略
    write_control(&mut send, &Control::Pong { nonce: 5 })
        .await
        .unwrap();
    write_control(&mut send, &hello()).await.unwrap();
    write_control(&mut send, &Control::Ping { nonce: 42 })
        .await
        .unwrap();
    assert_eq!(
        read_control(&mut recv).await.unwrap(),
        Control::Pong { nonce: 42 }
    );

    // 会话还活着：画面照常到达
    let (header, _) = raw.recv().await.unwrap();
    assert_eq!(header.channel, Channel::Video);

    // 数据报和控制流是两条独立的路：先给被控端留出时间把上面的数据报处理完，再结束会话。
    tokio::time::sleep(Duration::from_millis(600)).await;
    // 声称长度超限的控制消息：被控端结束会话，而不是去分配内存
    send.write_all(&5000u16.to_be_bytes()).await.unwrap();
    let report = tokio::time::timeout(Duration::from_secs(10), host_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.end, "对端结束会话");
    assert_eq!(report.bad_datagrams, 5);
    assert_eq!(report.input_events, 0);
    assert!(report.frames_sent > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn controller_survives_a_hostile_host() {
    let (_nodes, raw, ctrl_link) = links().await;
    let (tx, rx) = mpsc::channel(8);
    let ctrl_task = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            decoder: None,
            record: None,
            session: cfg(),
            deadline_us: 30_000,
            duration: None,
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(Null),
        rx,
        None,
    ));
    let (mut send, _recv) = raw_host_handshake(&raw, &ACCEPT).await;

    // 分片头非法（总数为 0）
    raw.send(Channel::Video, &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0])
        .unwrap();
    // 视频头被截断
    raw.send(Channel::Video, &[1, 2, 3]).unwrap();
    // 输入确认长度不对
    raw.send(Channel::Input, &[1, 2, 3]).unwrap();
    // 不该出现的通道
    raw.send(Channel::File, b"x").unwrap();
    // 魔数不对
    raw.send_raw(vec![9u8; 40]).unwrap();
    // 帧上带输入序号的戳被截断
    raw.send(Channel::Control, &[peerdesk_core::reaction::TAG, 1, 0])
        .unwrap();
    // 只发一帧的前半：超过截止时间后作废
    raw.send(Channel::Video, &[0, 0, 0, 5, 0, 0, 0, 2, 0, 0, 0, 4, 1, 1])
        .unwrap();
    // 一帧完整送达，但内容不是合法的码流：解码失败，不影响会话
    let mut junk_frame = vec![0, 0, 0, 3, 0, 0, 0, 1, 0, 0, 0, 3];
    junk_frame.extend_from_slice(b"this is not a tile stream");
    raw.send(Channel::Video, &junk_frame).unwrap();
    // 控制流上不相关的消息被忽略
    write_control(&mut send, &Control::Ping { nonce: 1 })
        .await
        .unwrap();
    write_control(&mut send, &ACCEPT).await.unwrap();

    // 控制端的输入仍然能发出来
    tx.send(InputEvent::Key {
        code: 4,
        down: true,
    })
    .await
    .unwrap();
    loop {
        let (header, _) = raw.recv().await.unwrap();
        if header.channel == Channel::Input {
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    write_control(&mut send, &Control::Bye).await.unwrap();
    let report = tokio::time::timeout(Duration::from_secs(10), ctrl_task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.end, "对端结束会话");
    assert_eq!(report.bad_datagrams, 6);
    assert_eq!(report.frames_delivered, 0);
    assert_eq!(report.bad_frames, 1);
    assert!(report.screen.is_none());
    // 半帧（帧 5）超时作废；它参考的帧 4 一个分片都没来，请求整帧重传等不到，也算作废一次
    assert_eq!(report.frames_expired, 2);
    assert!(report.trace.contains("frame_expired"));
    assert_eq!(report.input_sent, 1);
    assert_eq!(report.input_unacked, 1, "对端从不确认");
}

#[tokio::test(flavor = "multi_thread")]
async fn controller_handles_bad_handshake_answers_and_vanishing_host() {
    // 应答既不是 Accept 也不是 Reject
    let (_n1, raw, ctrl_link) = links().await;
    let (_tx, rx) = mpsc::channel(1);
    let task = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            decoder: None,
            record: None,
            session: cfg(),
            deadline_us: 1,
            duration: None,
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(Null),
        rx,
        None,
    ));
    let _streams = raw_host_handshake(&raw, &Control::Bye).await;
    assert_eq!(
        task.await.unwrap(),
        Err(NetError::Protocol("握手应答不是 Accept 或 Reject"))
    );

    // 被控端说我们的版本太旧（或它支持的最低版本比我们新）：用户看到要升级的说明
    for (reason, expect) in [
        (peerdesk_core::proto::RejectReason::Version, "版本太旧"),
        (
            peerdesk_core::proto::RejectReason::Busy,
            "正在被其他设备控制",
        ),
        (
            peerdesk_core::proto::RejectReason::NoCommonSuite,
            "NoCommonSuite",
        ),
    ] {
        let (_n, raw, ctrl_link) = links().await;
        let (_tx, rx) = mpsc::channel(1);
        let task = tokio::spawn(controller::run(
            ctrl_link,
            ControllerOptions {
                decoder: None,
                record: None,
                session: cfg(),
                deadline_us: 1,
                duration: None,
                clipboard: None,
                on_established: None,
                on_link: None,
                view: None,
                on_quality: None,
                audio: None,
            },
            Box::new(Null),
            rx,
            None,
        ));
        let _streams = raw_host_handshake(&raw, &Control::Reject { reason }).await;
        match task.await.unwrap() {
            Err(NetError::Connect(why)) => assert!(why.contains(expect), "{why}"),
            other => panic!("{other:?}"),
        }
    }

    // 握手后对端直接消失
    let (_n2, raw, ctrl_link) = links().await;
    let (_tx, rx) = mpsc::channel(1);
    let task = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            decoder: None,
            record: None,
            session: cfg(),
            deadline_us: 1,
            duration: None,
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(Null),
        rx,
        None,
    ));
    let _streams = raw_host_handshake(&raw, &ACCEPT).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    raw.close("gone");
    let report = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        report.end.contains("gone") || report.end == "对端结束会话",
        "{}",
        report.end
    );

    // 对端一直不应答：超时
    let (_n3, raw, ctrl_link) = links().await;
    let (_tx, rx) = mpsc::channel(1);
    let started = std::time::Instant::now();
    let result = controller::run(
        ctrl_link,
        ControllerOptions {
            decoder: None,
            record: None,
            session: cfg(),
            deadline_us: 1,
            duration: None,
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(Null),
        rx,
        None,
    )
    .await;
    assert_eq!(result, Err(NetError::Timeout("等待握手应答")));
    assert!(started.elapsed() < Duration::from_secs(8));
    drop(raw);
}

#[tokio::test(flavor = "multi_thread")]
async fn host_times_out_when_controller_never_opens_control_stream() {
    let (_nodes, host_link, _raw) = links().await;
    let result = host::serve(
        host_link,
        HostOptions::new(cfg(), 30, temp_dir("h2")),
        Box::new(DemoDesktop::new(320, 240, 1)),
        Box::new(Null),
    )
    .await;
    assert_eq!(result, Err(NetError::Timeout("等待控制流")));
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_of_a_missing_file_fails_the_session_cleanly() {
    let (_nodes, host_link, ctrl_link) = links().await;
    let recv_root = temp_dir("h3");
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(cfg(), 30, recv_root.clone()),
        Box::new(DemoDesktop::new(320, 240, 1)),
        Box::new(Null),
    ));
    let (_tx, rx) = mpsc::channel(1);
    let ghost = FileMeta {
        path: "ghost.bin".into(),
        size: 10,
        mtime: 0,
    };
    let result = controller::run(
        ctrl_link,
        ControllerOptions {
            decoder: None,
            record: None,
            session: cfg(),
            deadline_us: 100_000,
            duration: Some(Duration::from_millis(100)),
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(Null),
        rx,
        Some(Upload {
            root: temp_dir("empty-src"),
            files: vec![ghost],
        }),
    )
    .await;
    assert!(
        matches!(
            result,
            Err(NetError::Core(peerdesk_core::Error::Storage(_)))
        ),
        "{result:?}"
    );
    let report = host_task.await.unwrap().unwrap();
    assert!(
        report.files.iter().all(|f| f.outcome.is_none()),
        "{:?}",
        report.files
    );
    assert!(!recv_root.join("ghost.bin").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_is_abandoned_when_host_disappears_mid_transfer() {
    let (_nodes, raw, ctrl_link) = links().await;
    let src = temp_dir("big-src");
    std::fs::write(src.join("big.bin"), vec![3u8; 8 * 1024 * 1024]).unwrap();
    let files = peerdesk_net::fs_store::scan(&src, "big.bin").unwrap();
    let (_tx, rx) = mpsc::channel(1);
    let task = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            decoder: None,
            record: None,
            session: cfg(),
            deadline_us: 100_000,
            duration: None,
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(Null),
        rx,
        Some(Upload {
            root: src.clone(),
            files,
        }),
    ));
    let (mut ctrl_send, _ctrl_recv) = raw_host_handshake(&raw, &ACCEPT).await;
    // 接下文件流但一直不答复，然后让会话结束
    let (_file_send, mut file_recv) = raw.accept_stream().await.unwrap();
    assert_eq!(read_kind(&mut file_recv).await.unwrap(), StreamKind::File);
    write_control(&mut ctrl_send, &Control::Bye).await.unwrap();
    let report = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.end, "对端结束会话");
    assert!(report.files.is_empty());
    let _ = std::fs::remove_dir_all(src);
}

#[tokio::test(flavor = "multi_thread")]
async fn file_stream_edge_cases() {
    let meta = |path: &str| FileMeta {
        path: path.into(),
        size: 3,
        mtime: 0,
    };

    // 接收方在答复前关闭了流
    let (_n1, acceptor, opener) = links().await;
    let src = temp_dir("fsrc");
    std::fs::write(src.join("a"), b"abc").unwrap();
    let silent = tokio::spawn(async move {
        let (mut send, mut recv) = acceptor.accept_stream().await.unwrap();
        read_kind(&mut recv).await.unwrap();
        send.finish().unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        acceptor
    });
    let result = send_files(&opener, &src, vec![meta("a")]).await;
    assert_eq!(
        result,
        Err(NetError::Closed("对端在答复前关闭了文件流".into()))
    );
    drop(silent.await.unwrap());

    // 接收方答复后、给出结果前关闭了流：文件没有结果
    let (_n2, acceptor, opener) = links().await;
    let quitter = tokio::spawn(async move {
        let (mut send, mut recv) = acceptor.accept_stream().await.unwrap();
        read_kind(&mut recv).await.unwrap();
        let mut deframer = Deframer::new();
        let offer = read_file_msgs(&mut recv, &mut deframer).await.unwrap();
        assert!(matches!(offer[0], FileMsg::Offer { .. }));
        let answer = FileMsg::Answer {
            decisions: vec![peerdesk_core::file::Decision::Accept {
                offset: 0,
                prefix_hash: [0; 32],
            }],
        };
        write_file_msg(&mut send, &answer).await.unwrap();
        send.finish().unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        acceptor
    });
    let (results, bytes) = send_files(&opener, &src, vec![meta("a")]).await.unwrap();
    assert_eq!(results[0].outcome, None);
    assert_eq!(bytes, 3);
    drop(quitter.await.unwrap());

    // 发送方只发了 Offer 就结束：接收方正常返回，文件没有结果
    let (_n3, acceptor, opener) = links().await;
    let dst = temp_dir("fdst");
    let (mut send, _recv) = opener.open_stream().await.unwrap();
    write_file_msg(
        &mut send,
        &FileMsg::Offer {
            files: vec![meta("a")],
        },
    )
    .await
    .unwrap();
    send.finish().unwrap();
    let (s, r) = acceptor.accept_stream().await.unwrap();
    let results = receive_files(s, r, &dst).await.unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].outcome, None);

    // 发送方给出不安全的路径：接收方中止并报错，目录之外不会被写入
    let (_n4, acceptor, opener) = links().await;
    let (mut send, mut recv) = opener.open_stream().await.unwrap();
    write_file_msg(
        &mut send,
        &FileMsg::Offer {
            files: vec![meta("../escape")],
        },
    )
    .await
    .unwrap();
    let (s, r) = acceptor.accept_stream().await.unwrap();
    let result = receive_files(s, r, &dst).await;
    assert!(
        matches!(
            result,
            Err(NetError::Core(peerdesk_core::Error::UnsafePath(_)))
        ),
        "{result:?}"
    );
    let abort = read_file_msgs(&mut recv, &mut Deframer::new())
        .await
        .unwrap();
    assert!(matches!(abort[0], FileMsg::Abort { .. }));
    assert!(!dst.parent().unwrap().join("escape").exists());
    let _ = std::fs::remove_dir_all(src);
    let _ = std::fs::remove_dir_all(dst);
}

/// 控制端打开控制流后没发 Hello 就说再见：被控端报告“握手阶段结束”，不卡住。
#[tokio::test(flavor = "multi_thread")]
async fn controller_leaving_during_handshake_is_reported() {
    let (_nodes, host_link, raw) = links().await;
    let host_task = tokio::spawn(host::serve(
        host_link,
        HostOptions::new(cfg(), 30, temp_dir("bye")),
        Box::new(DemoDesktop::new(64, 64, 1)),
        Box::new(Null),
    ));
    let (mut send, _recv) = raw.open_stream().await.unwrap();
    write_kind(&mut send, StreamKind::Control).await.unwrap();
    write_control(&mut send, &Control::Bye).await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), host_task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&result, Err(NetError::Connect(why)) if why.contains("握手")),
        "{result:?}"
    );
}

/// 被控端在握手时回了不该出现的消息：控制端断开并报告协议错误，不卡住。
#[tokio::test(flavor = "multi_thread")]
async fn host_answering_hello_with_nonsense_is_a_protocol_error() {
    let (_nodes, raw, ctrl_link) = links().await;
    let (_tx, rx) = mpsc::channel(1);
    let ctrl_task = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            decoder: None,
            record: None,
            session: cfg(),
            deadline_us: 30_000,
            duration: None,
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(Null),
        rx,
        None,
    ));
    let (_send, _recv) = raw_host_handshake(&raw, &Control::Pong { nonce: 1 }).await;
    let result = tokio::time::timeout(Duration::from_secs(10), ctrl_task)
        .await
        .unwrap()
        .unwrap();
    assert!(result.is_err(), "{result:?}");
}
