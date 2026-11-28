//! 手机端核心在桌面上的闭环：真实的 QUIC 连接、真实的被控端会话，控制端是 [`peerdesk_mobile::Session`]。
//!
//! 被控端的画面是合成的办公画面（打字）：这里测的是连接、会话、输入这条链路通不通，不是画质；画质由 crates/e2e 量。
//! 桌面上没有手机的系统解码器，控制端用默认的 OpenH264（dev-dependencies 打开了 `soft`）。
#![allow(clippy::unwrap_used, clippy::panic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use peerdesk_core::auth::Auth;
use peerdesk_core::crypto::Policy;
use peerdesk_core::frame::Picture;
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::Caps;
use peerdesk_core::screen::Workload;
use peerdesk_core::session::SessionConfig;
use peerdesk_mobile::gesture::{Action, Gestures, Mode, Phase, Touch, View};
use peerdesk_mobile::{device_target, Event, ScreenSink, Session, SinkFactory};
use peerdesk_net::demo::DemoDesktop;
use peerdesk_net::host::{self, HostOptions, InputSink};
use peerdesk_net::viewer::ViewTarget;
use peerdesk_net::{Identity, Node};

const WAIT: Duration = Duration::from_secs(20);

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-mobile-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[derive(Default, Clone)]
struct Recorder(Arc<Mutex<Vec<InputEvent>>>);

impl InputSink for Recorder {
    fn inject(&mut self, event: &InputEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

/// 数收到几幅画面。
struct Counter(Arc<AtomicUsize>);

impl ScreenSink for Counter {
    fn present(&mut self, _picture: &Picture) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// 起一个只接一条连接的被控端，密码 `pw`。返回地址、指纹、收到的输入。
fn host(rt: &tokio::runtime::Runtime, pw: &str) -> (SocketAddr, [u8; 32], Recorder) {
    let id = Identity::generate().unwrap();
    let policy = Policy::recommended();
    let node = rt
        .block_on(async { Node::bind(([127, 0, 0, 1], 0).into(), &id, &policy) })
        .unwrap();
    let addr = node.local_addr().unwrap();
    let recorder = Recorder::default();
    let inputs = recorder.clone();
    let session = SessionConfig {
        device_id: id.device_id(),
        region: "test".into(),
        caps: Caps::H264.with(Caps::FEC_RS).with(Caps::VIEW_SIZE),
        policy,
        auth: Auth::Password(pw.to_owned()),
        binding: Vec::new(),
    };
    let recv = temp_dir("recv");
    rt.spawn(async move {
        while let Some(Ok(link)) = node.accept().await {
            let desktop =
                DemoDesktop::new(640, 360, 1).with_script(vec![(Workload::Typing, 100_000)]);
            let _ = host::serve(
                link,
                HostOptions::new(session.clone(), 30, recv.clone()),
                Box::new(desktop),
                Box::new(inputs.clone()),
            )
            .await;
        }
    });
    (addr, id.fingerprint(), recorder)
}

fn counting() -> (SinkFactory, Arc<AtomicUsize>) {
    let frames = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&frames);
    let make: SinkFactory =
        Arc::new(move || -> Box<dyn ScreenSink> { Box::new(Counter(Arc::clone(&f))) });
    (make, frames)
}

/// 等到 `done` 为真，期间收下所有事件。
fn wait(session: &Session, seen: &mut Vec<Event>, done: impl Fn(&[Event]) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !done(seen) {
        assert!(Instant::now() < deadline, "等待超时，已收到：{seen:?}");
        if let Some(e) = session.next_event(Duration::from_millis(50)) {
            seen.push(e);
        }
    }
}

#[test]
fn a_phone_session_sees_frames_and_its_taps_reach_the_host() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (addr, pin, inputs) = host(&rt, "pw");
    let (sink, frames) = counting();
    let session = Session::start(
        temp_dir("phone"),
        ViewTarget::Addr {
            addr,
            pin: Some(pin),
        },
        "pw",
        sink,
        None,
    );
    session.set_view(390, 844);
    let mut seen = Vec::new();
    wait(&session, &mut seen, |s| {
        s.iter().any(|e| matches!(e, Event::Connected { .. }))
            && s.iter().any(|e| matches!(e, Event::Peer { .. }))
    });
    // 连续出画面
    let deadline = Instant::now() + WAIT;
    while frames.load(Ordering::Relaxed) < 10 {
        assert!(
            Instant::now() < deadline,
            "画面不够：{}",
            frames.load(Ordering::Relaxed)
        );
        let _ = session
            .next_event(Duration::from_millis(50))
            .map(|e| seen.push(e));
    }
    // 在画面正中点一下：手势状态机给出的事件原样发给被控端
    let view = View {
        left: 0.0,
        top: 0.0,
        width: 390.0,
        height: 220.0,
    };
    let mut g = Gestures::new(Mode::Direct, view, 8.0, 40.0);
    let mut actions = g.touch(&Touch {
        id: 1,
        phase: Phase::Down,
        x: 195.0,
        y: 110.0,
        t_ms: 0,
    });
    actions.extend(g.touch(&Touch {
        id: 1,
        phase: Phase::Up,
        x: 195.0,
        y: 110.0,
        t_ms: 80,
    }));
    for a in actions {
        if let Action::Input(e) = a {
            assert!(session.send(e));
        }
    }
    let deadline = Instant::now() + WAIT;
    loop {
        let got = inputs.0.lock().unwrap().clone();
        if got.contains(&InputEvent::MouseButton {
            button: 0,
            down: false,
        }) {
            assert_eq!(
                got,
                vec![
                    InputEvent::MouseMove { x: 32768, y: 32768 },
                    InputEvent::MouseButton {
                        button: 0,
                        down: true
                    },
                    InputEvent::MouseButton {
                        button: 0,
                        down: false
                    },
                ],
                "被控端按顺序收到移动、按下、抬起"
            );
            break;
        }
        assert!(Instant::now() < deadline, "被控端没收到点击：{got:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
    // 每秒一次的链路质量
    wait(&session, &mut seen, |s| {
        s.iter().any(|e| matches!(e, Event::Quality(_)))
    });
    // 断开：最后一个事件是正常结束
    session.stop();
    wait(&session, &mut seen, |s| {
        s.iter().any(|e| matches!(e, Event::Ended { .. }))
    });
    assert_eq!(
        seen.last(),
        Some(&Event::Ended {
            ok: true,
            text: "已断开".to_owned()
        })
    );
    assert!(seen.iter().all(|e| !e.to_json().is_empty()));
}

#[test]
fn a_wrong_password_ends_with_the_reason() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (addr, pin, inputs) = host(&rt, "right");
    let (sink, frames) = counting();
    let session = Session::start(
        temp_dir("wrong"),
        ViewTarget::Addr {
            addr,
            pin: Some(pin),
        },
        "wrong",
        sink,
        None,
    );
    let mut seen = Vec::new();
    wait(&session, &mut seen, |s| {
        s.iter().any(|e| matches!(e, Event::Ended { .. }))
    });
    let Some(Event::Ended { ok, text }) = seen.last() else {
        panic!("最后不是结束：{seen:?}");
    };
    assert!(!ok && text.contains("密码"), "{text}");
    assert_eq!(frames.load(Ordering::Relaxed), 0);
    assert!(inputs.0.lock().unwrap().is_empty());
    // 结束之后：发输入失败，不再有事件
    assert!(!session.send(InputEvent::ReleaseAll));
    assert!(session.next_event(Duration::from_millis(100)).is_none());
}

#[test]
fn view_changes_go_back_to_the_ui_and_inputs_go_out() {
    let s = Session::failed("测试".to_owned());
    assert_eq!(
        peerdesk_mobile::apply(&s, vec![Action::Input(InputEvent::ReleaseAll)]),
        None
    );
    assert_eq!(
        peerdesk_mobile::apply(
            &s,
            vec![
                Action::Zoom {
                    factor: 1.5,
                    cx: 10.0,
                    cy: 20.5
                },
                Action::Input(InputEvent::ReleaseAll),
                Action::Pan { dx: -3.0, dy: 4.0 },
                Action::Haptic,
                Action::Cursor { x: 0.25, y: 1.0 },
            ]
        )
        .as_deref(),
        Some(
            r#"[{"type":"zoom","factor":1.5,"cx":10,"cy":20.5},{"type":"pan","dx":-3,"dy":4},{"type":"haptic"},{"type":"cursor","x":0.25,"y":1}]"#
        )
    );
}

#[test]
fn a_session_that_failed_before_starting_says_why() {
    let s = Session::failed("设备码应为 9 位数字".to_owned());
    assert_eq!(
        s.next_event(Duration::from_millis(10)),
        Some(Event::Ended {
            ok: false,
            text: "设备码应为 9 位数字".to_owned()
        })
    );
    assert!(s.next_event(Duration::from_millis(10)).is_none());
    assert!(!s.send(InputEvent::ReleaseAll));
    s.set_view(1, 1);
    s.stop();
}

#[test]
fn a_target_that_cannot_be_resolved_ends_with_the_reason() {
    let (sink, _) = counting();
    let s = Session::start_with(
        temp_dir("resolve"),
        || device_target("127.0.0.1", "12345"),
        "",
        sink,
        None,
    );
    let mut seen = Vec::new();
    wait(&s, &mut seen, |e| {
        e.iter().any(|e| matches!(e, Event::Ended { .. }))
    });
    assert_eq!(
        seen,
        vec![Event::Ended {
            ok: false,
            text: "设备码应为 9 位数字".to_owned()
        }]
    );
}

#[test]
fn handles_survive_tagged_pointers() {
    use peerdesk_mobile::{addr_from_handle, handle_from_addr};
    // Android 11 起堆指针最高字节带标记：当无符号数大于 i64::MAX，也要原样往返
    for addr in [0x7f12_3456_7890_usize, 0xb400_0078_1234_5670, usize::MAX] {
        let h = handle_from_addr(addr);
        assert_ne!(h, 0);
        assert_eq!(addr_from_handle(h), Some(addr), "{addr:#x}");
    }
    assert!(
        handle_from_addr(0xb400_0078_1234_5670) < 0,
        "带标记的地址是负的 long，Kotlin 只拿 0 当失败"
    );
    assert_eq!(handle_from_addr(0), 0);
    assert_eq!(addr_from_handle(0), None);
}

#[test]
fn device_targets_are_parsed_with_reasons() {
    let ViewTarget::Device { server, .. } = device_target("127.0.0.1", "123 456 789").unwrap()
    else {
        panic!("不是按设备码");
    };
    assert_eq!(server.addr, "127.0.0.1:4433".parse().unwrap());
    let ViewTarget::Device { server, .. } =
        device_target(" 127.0.0.1:9000 ", "123-456-789").unwrap()
    else {
        panic!("不是按设备码");
    };
    assert_eq!(server.addr.port(), 9000);
    for v6 in ["::1", "[::1]"] {
        let ViewTarget::Device { server, .. } = device_target(v6, "123456789").unwrap() else {
            panic!("不是按设备码");
        };
        assert_eq!(server.addr, "[::1]:4433".parse().unwrap(), "{v6}");
    }
    assert!(device_target("127.0.0.1", "12345")
        .unwrap_err()
        .contains("9 位"));
    assert!(device_target("  ", "123456789")
        .unwrap_err()
        .contains("服务器"));
    assert!(device_target("no-such-host.invalid", "123456789")
        .unwrap_err()
        .contains("找不到服务器"));
}

#[test]
fn events_become_one_line_of_json() {
    let cases = [
        (
            Event::Connected {
                route: "Direct".into(),
                detail: "局域网 \"快\"\n".into(),
            },
            r#"{"type":"connected","route":"Direct","detail":"局域网 \"快\"\u000a"}"#,
        ),
        (
            Event::Peer {
                os: "MacOs".into(),
                name: "a\\b".into(),
            },
            r#"{"type":"peer","os":"MacOs","name":"a\\b"}"#,
        ),
        (
            Event::Reconnecting {
                attempt: 2,
                why: "timed out".into(),
            },
            r#"{"type":"reconnecting","attempt":2,"why":"timed out"}"#,
        ),
        (
            Event::Notice("额度".into()),
            r#"{"type":"notice","text":"额度"}"#,
        ),
        (
            Event::Ended {
                ok: false,
                text: "拒绝".into(),
            },
            r#"{"type":"ended","ok":false,"text":"拒绝"}"#,
        ),
    ];
    for (e, want) in cases {
        assert_eq!(e.to_json(), want);
    }
    let q = Event::Quality(peerdesk_net::controller::LinkQuality {
        rtt_ms: 12,
        router_ms: Some(3),
        decoder: "MediaCodec".into(),
        fec: "RS",
        ..Default::default()
    });
    let j = q.to_json();
    assert!(
        j.starts_with(r#"{"type":"quality","rtt_ms":12,"#)
            && j.contains(r#""router_ms":3,"#)
            && j.contains(r#""decoder":"MediaCodec","fec":"RS"}"#),
        "{j}"
    );
    let none = Event::Quality(peerdesk_net::controller::LinkQuality::default()).to_json();
    assert!(none.contains(r#""router_ms":null,"#), "{none}");
}
