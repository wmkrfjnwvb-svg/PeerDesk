//! 手机端的日志：一次真实会话（真实 QUIC、真实被控端）之后，数据目录的 `logs/peerdesk.log` 里有
//! 会话开始、结束，和控制端每秒一行的统计；设备码只留后 3 位，密码和输入内容不出现。
//!
//! 全局日志器每个进程只能装一次，所以这个测试单独一个文件（单独的进程）。
#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use peerdesk_core::auth::Auth;
use peerdesk_core::crypto::Policy;
use peerdesk_core::frame::Picture;
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::Caps;
use peerdesk_core::screen::Workload;
use peerdesk_core::session::SessionConfig;
use peerdesk_mobile::{describe_target, Event, ScreenSink, Session, SinkFactory};
use peerdesk_net::demo::DemoDesktop;
use peerdesk_net::host::{self, HostOptions, InputSink};
use peerdesk_net::rendezvous::{Lookup, PathPolicy, Server};
use peerdesk_net::viewer::ViewTarget;
use peerdesk_net::{Identity, Node};

struct Quiet;
impl InputSink for Quiet {
    fn inject(&mut self, _event: &InputEvent) {}
}

struct Nop;
impl ScreenSink for Nop {
    fn present(&mut self, _picture: &Picture) {}
}

const SECRET: &str = "s3cret-pw-9";

#[test]
fn a_session_leaves_a_log_with_stages_and_stats_but_no_secrets() {
    let dir = std::env::temp_dir().join(format!("peerdesk-phonelog-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let id = Identity::generate().unwrap();
    let policy = Policy::recommended();
    let node = rt
        .block_on(async { Node::bind(([127, 0, 0, 1], 0).into(), &id, &policy) })
        .unwrap();
    let addr = node.local_addr().unwrap();
    let config = SessionConfig {
        device_id: id.device_id(),
        region: "test".into(),
        caps: Caps::H264.with(Caps::FEC_RS).with(Caps::VIEW_SIZE),
        policy,
        auth: Auth::Password(SECRET.to_owned()),
        binding: Vec::new(),
    };
    let recv = dir.join("recv");
    rt.spawn(async move {
        while let Some(Ok(link)) = node.accept().await {
            let desktop =
                DemoDesktop::new(640, 360, 1).with_script(vec![(Workload::Typing, 100_000)]);
            let _ = host::serve(
                link,
                HostOptions::new(config.clone(), 30, recv.clone()),
                Box::new(desktop),
                Box::new(Quiet),
            )
            .await;
        }
    });
    let sink: SinkFactory = Arc::new(|| Box::new(Nop));
    let session = Session::start(
        dir.clone(),
        ViewTarget::Addr {
            addr,
            pin: Some(id.fingerprint()),
        },
        SECRET,
        sink,
        None,
    );
    session.set_view(390, 844);
    // 等到第二次质量事件：每秒一行的统计至少写过一行
    let seen = Mutex::new(0);
    let deadline = Instant::now() + Duration::from_secs(30);
    while *seen.lock().unwrap() < 2 {
        assert!(Instant::now() < deadline, "等待超时");
        if let Some(Event::Quality(_)) = session.next_event(Duration::from_millis(50)) {
            *seen.lock().unwrap() += 1;
        }
    }
    session.stop();
    while !matches!(
        session.next_event(Duration::from_millis(50)),
        Some(Event::Ended { .. })
    ) {
        assert!(Instant::now() < deadline, "等待结束超时");
    }
    let text = std::fs::read_to_string(dir.join("logs").join("peerdesk.log")).unwrap();
    // 给人看的样例：cargo test -p peerdesk-mobile --test phone_log -- --nocapture
    println!("{text}");
    assert!(text.contains("启动"), "{text}");
    assert!(
        text.contains("手机端会话开始：直连地址 127.0.0.1"),
        "{text}"
    );
    assert!(
        text.contains("手机端会话结束：成功=true，原因=已断开（cancelled）"),
        "{text}"
    );
    assert!(text.lines().count() >= 4, "{text}");
    assert!(!text.contains(SECRET), "密码不能进日志");

    // 设备码只留后 3 位
    let target = ViewTarget::Device {
        server: Server { addr, pin: None },
        device: Lookup::Code(123_456_789),
        policy: PathPolicy::Auto,
        account: String::new(),
    };
    let described = describe_target(&target);
    assert!(described.contains("***789"), "{described}");
    assert!(!described.contains("123456"), "{described}");
    let by_fp = ViewTarget::Device {
        server: Server { addr, pin: None },
        device: Lookup::Device(id.fingerprint()),
        policy: PathPolicy::Auto,
        account: String::new(),
    };
    assert!(describe_target(&by_fp).contains("指纹"));
}
