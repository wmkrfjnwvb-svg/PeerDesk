//! 代码逻辑测试：会话状态机的每一条转移，包括不允许的转移。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use peerdesk_core::auth::Auth;
use peerdesk_core::crypto::{Policy, Suite};
use peerdesk_core::proto::{Caps, Control, RejectReason, VERSION};
use peerdesk_core::session::{Role, Session, SessionConfig, State};
use peerdesk_core::trace::{Stage, TraceId, Tracer};
use peerdesk_core::{Error, Time};

const C: TraceId = TraceId(1);
const H: TraceId = TraceId(2);

fn cfg(device_id: u64, policy: Policy) -> SessionConfig {
    SessionConfig {
        device_id,
        region: "us".into(),
        caps: Caps::H264.with(Caps::AV1),
        policy,
        auth: Auth::Open,
        binding: b"channel".to_vec(),
    }
}

fn pair(cp: Policy, hp: Policy) -> (Session, Session, Tracer) {
    (
        Session::controller(cfg(10, cp), C),
        Session::host(cfg(20, hp), H),
        Tracer::new(256),
    )
}

fn established() -> (Session, Session, Tracer) {
    let (mut c, mut h, mut t) = pair(Policy::recommended(), Policy::recommended());
    let hello = c.hello(Time(0), &mut t).unwrap();
    let accept = h
        .on_control(Time(1), &hello, &mut t)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(c.on_control(Time(2), &accept, &mut t).unwrap(), vec![]);
    (c, h, t)
}

fn unexpected(result: Result<Vec<Control>, Error>) {
    assert!(
        matches!(result, Err(Error::UnexpectedMessage { .. })),
        "{result:?}"
    );
}

#[test]
fn happy_path() {
    let (c, h, t) = established();
    assert_eq!(c.role(), Role::Controller);
    assert_eq!(h.role(), Role::Host);
    assert_eq!(c.suite(), Some(Suite::Aes256Gcm));
    assert_eq!(h.suite(), Some(Suite::Aes256Gcm));
    match (c.state(), h.state()) {
        (
            State::Established {
                session_id: a,
                caps: ca,
                peer: pa,
                ..
            },
            State::Established {
                session_id: b,
                caps: cb,
                peer: pb,
                ..
            },
        ) => {
            assert_eq!(a, b);
            assert_ne!(*a, 0);
            assert_eq!(ca, cb);
            assert_eq!((*pa, *pb), (0, 10));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        t.path(C),
        vec![Stage::HelloSent, Stage::AcceptReceived, Stage::Established]
    );
    assert_eq!(
        t.path(H),
        vec![
            Stage::HelloReceived,
            Stage::SuiteNegotiated,
            Stage::AcceptSent,
            Stage::Established
        ]
    );
}

#[test]
fn hello_can_be_resent_and_accept_is_idempotent() {
    let (mut c, mut h, mut t) = pair(Policy::recommended(), Policy::recommended());
    let first = c.hello(Time(0), &mut t).unwrap();
    let again = c.hello(Time(5), &mut t).unwrap();
    assert_eq!(first, again);
    let a1 = h
        .on_control(Time(6), &first, &mut t)
        .unwrap()
        .pop()
        .unwrap();
    let a2 = h
        .on_control(Time(7), &again, &mut t)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(a1, a2, "重复的 Hello 得到同一个 Accept");
    assert_eq!(c.on_control(Time(8), &a1, &mut t).unwrap(), vec![]);
    assert_eq!(
        c.on_control(Time(9), &a2, &mut t).unwrap(),
        vec![],
        "重复的 Accept 被忽略"
    );
    assert_eq!(t.count(H, Stage::Established), 1);
    assert_eq!(t.count(C, Stage::Established), 1);
}

#[test]
fn host_rejects_wrong_version() {
    let (_, mut h, mut t) = pair(Policy::recommended(), Policy::recommended());
    let hello = Control::Hello {
        version: VERSION + 1,
        device_id: 10,
        caps: Caps::H264,
        region: "us".into(),
        suites: vec![Suite::Aes256Gcm],
    };
    assert_eq!(
        h.on_control(Time(0), &hello, &mut t).unwrap(),
        vec![Control::Reject {
            reason: RejectReason::Version
        }]
    );
    assert_eq!(*h.state(), State::Rejected(RejectReason::Version));
    assert_eq!(h.suite(), None);
    unexpected(h.on_control(Time(1), &hello, &mut t));
}

#[test]
fn no_common_suite_is_rejected_and_reported_to_controller() {
    let (mut c, mut h, mut t) = pair(Policy::none(), Policy::recommended());
    let hello = c.hello(Time(0), &mut t).unwrap();
    let reject = h
        .on_control(Time(1), &hello, &mut t)
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(
        reject,
        Control::Reject {
            reason: RejectReason::NoCommonSuite
        }
    );
    assert_eq!(c.on_control(Time(2), &reject, &mut t).unwrap(), vec![]);
    assert_eq!(*c.state(), State::Rejected(RejectReason::NoCommonSuite));
    assert_eq!(t.path(C), vec![Stage::HelloSent, Stage::Rejected]);
    assert_eq!(t.path(H), vec![Stage::HelloReceived, Stage::Rejected]);
    assert!(c.hello(Time(3), &mut t).is_err(), "被拒绝后不能再发 Hello");
}

#[test]
fn plaintext_only_when_both_sides_allow_it() {
    let lax = Policy::custom(&[Suite::None, Suite::Aes128Gcm]).unwrap();
    let (mut c, mut h, mut t) = pair(Policy::none(), lax);
    let hello = c.hello(Time(0), &mut t).unwrap();
    let accept = h
        .on_control(Time(1), &hello, &mut t)
        .unwrap()
        .pop()
        .unwrap();
    c.on_control(Time(2), &accept, &mut t).unwrap();
    assert_eq!(c.suite(), Some(Suite::None));
}

#[test]
fn controller_refuses_a_suite_outside_its_policy() {
    // 模拟恶意或有缺陷的被控端：回了一个控制端没提供的套件。
    let (mut c, _, mut t) = pair(Policy::recommended(), Policy::recommended());
    c.hello(Time(0), &mut t).unwrap();
    let forged = Control::Accept {
        suite: Suite::None,
        session_id: 1,
        caps: Caps::H264,
    };
    assert_eq!(
        c.on_control(Time(1), &forged, &mut t),
        Err(Error::NoCommonSuite)
    );
    assert_eq!(*c.state(), State::Rejected(RejectReason::NoCommonSuite));
    assert_eq!(
        t.path(C),
        vec![Stage::HelloSent, Stage::AcceptReceived, Stage::Rejected]
    );
}

#[test]
fn second_controller_gets_busy() {
    let (_, mut h, mut t) = established();
    let other = Control::Hello {
        version: VERSION,
        device_id: 999,
        caps: Caps::H264,
        region: "eu".into(),
        suites: vec![Suite::Aes256Gcm],
    };
    assert_eq!(
        h.on_control(Time(9), &other, &mut t).unwrap(),
        vec![Control::Reject {
            reason: RejectReason::Busy
        }]
    );
    assert!(h.suite().is_some(), "原会话不受影响");
}

#[test]
fn ping_pong_only_when_established() {
    let (mut c, mut h, mut t) = established();
    assert_eq!(
        h.on_control(Time(3), &Control::Ping { nonce: 5 }, &mut t)
            .unwrap(),
        vec![Control::Pong { nonce: 5 }]
    );
    assert_eq!(
        c.on_control(Time(4), &Control::Pong { nonce: 5 }, &mut t)
            .unwrap(),
        vec![]
    );

    let (mut c, mut h, mut t) = pair(Policy::recommended(), Policy::recommended());
    unexpected(h.on_control(Time(0), &Control::Ping { nonce: 1 }, &mut t));
    unexpected(c.on_control(Time(0), &Control::Pong { nonce: 1 }, &mut t));
}

#[test]
fn messages_in_the_wrong_direction_or_state_are_errors() {
    let accept = Control::Accept {
        suite: Suite::Aes256Gcm,
        session_id: 1,
        caps: Caps::H264,
    };
    let reject = Control::Reject {
        reason: RejectReason::Busy,
    };
    let hello = Control::Hello {
        version: VERSION,
        device_id: 1,
        caps: Caps::H264,
        region: "us".into(),
        suites: vec![],
    };

    let (mut c, mut h, mut t) = pair(Policy::recommended(), Policy::recommended());
    // 还没发 Hello 就收到应答
    unexpected(c.on_control(Time(0), &accept, &mut t));
    unexpected(c.on_control(Time(0), &reject, &mut t));
    // 控制端不接受 Hello，被控端不接受应答，被控端不能发起
    unexpected(c.on_control(Time(0), &hello, &mut t));
    unexpected(h.on_control(Time(0), &accept, &mut t));
    unexpected(h.on_control(Time(0), &reject, &mut t));
    assert!(h.hello(Time(0), &mut t).is_err());

    // 建立之后：内容不同的 Accept、迟到的 Reject、再次发起，都是错误
    let (mut c, _, mut t) = established();
    let different = Control::Accept {
        suite: Suite::Aes128Gcm,
        session_id: 1,
        caps: Caps::H264,
    };
    unexpected(c.on_control(Time(5), &different, &mut t));
    unexpected(c.on_control(Time(5), &reject, &mut t));
    assert!(c.hello(Time(5), &mut t).is_err());
}

#[test]
fn close_from_either_side_and_from_any_state() {
    let (mut c, mut h, mut t) = established();
    let bye = c.close(Time(10), &mut t);
    assert_eq!(bye, Control::Bye);
    assert_eq!(*c.state(), State::Closed);
    assert_eq!(h.on_control(Time(11), &bye, &mut t).unwrap(), vec![]);
    assert_eq!(*h.state(), State::Closed);
    assert_eq!(h.suite(), None);
    // 重复关闭不重复记录
    c.close(Time(12), &mut t);
    h.on_control(Time(13), &Control::Bye, &mut t).unwrap();
    assert_eq!(t.count(C, Stage::Closed), 1);
    assert_eq!(t.count(H, Stage::Closed), 1);
    // 关闭后一切消息都被拒绝
    unexpected(h.on_control(Time(14), &Control::Ping { nonce: 1 }, &mut t));
    let hello = Control::Hello {
        version: VERSION,
        device_id: 10,
        caps: Caps::H264,
        region: "us".into(),
        suites: vec![Suite::Aes256Gcm],
    };
    unexpected(h.on_control(Time(15), &hello, &mut t));
    assert!(c.hello(Time(16), &mut t).is_err());

    // 未建立时也能关闭
    let (mut c, _, mut t) = pair(Policy::recommended(), Policy::recommended());
    c.close(Time(0), &mut t);
    assert_eq!(t.path(C), vec![Stage::Closed]);
    for (state, name) in [
        (State::Idle, "Idle"),
        (State::HelloSent, "HelloSent"),
        (State::AwaitAuth, "AwaitAuth"),
        (State::Rejected(RejectReason::Busy), "Rejected"),
        (State::Closed, "Closed"),
    ] {
        assert_eq!(state.name(), name);
    }
}

// ---------------------------------------------------------------- 密码授权

fn auth_pair(controller: Auth, host: Auth) -> (Session, Session, Tracer) {
    let mut c = cfg(10, Policy::recommended());
    c.auth = controller;
    let mut h = cfg(20, Policy::recommended());
    h.auth = host;
    (
        Session::controller(c, C),
        Session::host(h, H),
        Tracer::new(256),
    )
}

fn pw(s: &str) -> Auth {
    Auth::Password(s.into())
}

/// 把消息在两端之间来回传到没有新消息为止。
fn drive(c: &mut Session, h: &mut Session, t: &mut Tracer) {
    let mut to_host = vec![c.hello(Time(0), t).unwrap()];
    for _ in 0..8 {
        let mut to_controller = Vec::new();
        for m in to_host.drain(..) {
            to_controller.extend(h.on_control(Time(1), &m, t).unwrap());
        }
        for m in to_controller {
            to_host.extend(c.on_control(Time(2), &m, t).unwrap());
        }
    }
}

#[test]
fn correct_password_establishes_with_mutual_proof() {
    let (mut c, mut h, mut t) = auth_pair(pw("open sesame"), pw("open sesame"));
    drive(&mut c, &mut h, &mut t);
    assert_eq!(c.suite(), Some(Suite::Aes256Gcm));
    assert_eq!(h.suite(), Some(Suite::Aes256Gcm));
    assert_eq!(
        t.path(C),
        vec![
            Stage::HelloSent,
            Stage::AuthRequested,
            Stage::AuthPassed,
            Stage::AcceptReceived,
            Stage::Established
        ]
    );
    assert_eq!(
        t.path(H),
        vec![
            Stage::HelloReceived,
            Stage::SuiteNegotiated,
            Stage::AuthRequested,
            Stage::AuthPassed,
            Stage::AcceptSent,
            Stage::Established
        ]
    );
}

#[test]
fn wrong_password_is_rejected_on_both_sides() {
    let (mut c, mut h, mut t) = auth_pair(pw("guess"), pw("real"));
    drive(&mut c, &mut h, &mut t);
    assert_eq!(*h.state(), State::Rejected(RejectReason::Unauthorized));
    assert_eq!(*c.state(), State::Rejected(RejectReason::Unauthorized));
    assert_eq!(t.count(H, Stage::AuthFailed), 1);
    assert_eq!(t.count(H, Stage::Established), 0);
    assert_eq!(t.count(C, Stage::Established), 0);
}

#[test]
fn controller_without_password_gives_up_when_asked() {
    let (mut c, mut h, mut t) = auth_pair(Auth::Open, pw("real"));
    drive(&mut c, &mut h, &mut t);
    assert_eq!(*c.state(), State::Rejected(RejectReason::Unauthorized));
    assert_eq!(*h.state(), State::AwaitAuth, "被控端还在等，由上层超时");
    assert_eq!(h.suite(), None);
}

#[test]
fn open_host_ignores_that_controller_has_a_password() {
    let (mut c, mut h, mut t) = auth_pair(pw("unused"), Auth::Open);
    drive(&mut c, &mut h, &mut t);
    assert!(c.suite().is_some() && h.suite().is_some());
    assert_eq!(t.count(C, Stage::AuthRequested), 0);
}

#[test]
fn different_channel_binding_fails_even_with_right_password() {
    // 中间人分别与两端建立了加密连接，然后原样转发密码交换：两端的通道绑定不同。
    let mut cc = cfg(10, Policy::recommended());
    cc.auth = pw("pw");
    cc.binding = b"connection-to-attacker".to_vec();
    let mut hc = cfg(20, Policy::recommended());
    hc.auth = pw("pw");
    hc.binding = b"connection-from-attacker".to_vec();
    let (mut c, mut h, mut t) = (
        Session::controller(cc, C),
        Session::host(hc, H),
        Tracer::new(64),
    );
    drive(&mut c, &mut h, &mut t);
    assert_eq!(*h.state(), State::Rejected(RejectReason::Unauthorized));
    assert_eq!(c.suite(), None);
}

#[test]
fn host_that_skips_its_proof_is_not_trusted() {
    // 冒充的被控端不知道密码，直接回 Accept。
    let (mut c, mut h, mut t) = auth_pair(pw("pw"), pw("pw"));
    let hello = c.hello(Time(0), &mut t).unwrap();
    let auth_start = h
        .on_control(Time(1), &hello, &mut t)
        .unwrap()
        .pop()
        .unwrap();
    let replies = c.on_control(Time(2), &auth_start, &mut t).unwrap();
    assert_eq!(replies.len(), 2);
    assert_eq!(*c.state(), State::AwaitAuth);
    let forged = Control::Accept {
        suite: Suite::Aes256Gcm,
        session_id: 1,
        caps: Caps::H264,
    };
    unexpected(c.on_control(Time(3), &forged, &mut t));
    assert_eq!(*c.state(), State::Rejected(RejectReason::Unauthorized));

    // 冒充的被控端给了一个错误的确认码
    let (mut c, mut h, mut t) = auth_pair(pw("pw"), pw("pw"));
    let hello = c.hello(Time(0), &mut t).unwrap();
    let auth_start = h
        .on_control(Time(1), &hello, &mut t)
        .unwrap()
        .pop()
        .unwrap();
    c.on_control(Time(2), &auth_start, &mut t).unwrap();
    assert!(c
        .on_control(Time(3), &Control::AuthProof { proof: [0; 32] }, &mut t)
        .unwrap()
        .is_empty());
    assert_eq!(*c.state(), State::Rejected(RejectReason::Unauthorized));
}

#[test]
fn auth_messages_out_of_order_or_malformed() {
    let start = Control::AuthStart { msg: vec![1; 33] };
    let proof = Control::AuthProof { proof: [0; 32] };

    // 不在密码交换阶段
    let (mut c, mut h, mut t) = auth_pair(Auth::Open, Auth::Open);
    unexpected(c.on_control(Time(0), &start, &mut t));
    unexpected(h.on_control(Time(0), &start, &mut t));
    unexpected(c.on_control(Time(0), &proof, &mut t));
    unexpected(h.on_control(Time(0), &proof, &mut t));
    let (mut c, mut h, mut t) = established();
    unexpected(c.on_control(Time(9), &start, &mut t));
    unexpected(h.on_control(Time(9), &proof, &mut t));

    // 被控端：还没收到交换消息就收到确认码
    let (mut c, mut h, mut t) = auth_pair(pw("pw"), pw("pw"));
    let hello = c.hello(Time(0), &mut t).unwrap();
    h.on_control(Time(1), &hello, &mut t).unwrap();
    assert_eq!(
        h.on_control(Time(2), &proof, &mut t).unwrap(),
        vec![Control::Reject {
            reason: RejectReason::Unauthorized
        }]
    );

    // 被控端：交换消息是垃圾
    let (mut c, mut h, mut t) = auth_pair(pw("pw"), pw("pw"));
    let hello = c.hello(Time(0), &mut t).unwrap();
    h.on_control(Time(1), &hello, &mut t).unwrap();
    assert_eq!(
        h.on_control(Time(2), &Control::AuthStart { msg: vec![0xff; 5] }, &mut t)
            .unwrap(),
        vec![Control::Reject {
            reason: RejectReason::Unauthorized
        }]
    );

    // 控制端：被控端发来的交换消息是垃圾
    let (mut c, _, mut t) = auth_pair(pw("pw"), pw("pw"));
    c.hello(Time(0), &mut t).unwrap();
    assert!(c
        .on_control(Time(1), &Control::AuthStart { msg: vec![0xff; 5] }, &mut t)
        .unwrap()
        .is_empty());
    assert_eq!(*c.state(), State::Rejected(RejectReason::Unauthorized));
}

#[test]
fn retries_during_auth_are_idempotent_and_others_get_busy() {
    let (mut c, mut h, mut t) = auth_pair(pw("pw"), pw("pw"));
    let hello = c.hello(Time(0), &mut t).unwrap();
    let first = h.on_control(Time(1), &hello, &mut t).unwrap();
    let again = h.on_control(Time(2), &hello, &mut t).unwrap();
    assert_eq!(first, again, "重复的 Hello 得到同一条 AuthStart");
    // 另一台设备在这期间来连：忙
    let other = Control::Hello {
        version: VERSION,
        device_id: 999,
        caps: Caps::H264,
        region: "x".into(),
        suites: vec![Suite::Aes256Gcm],
    };
    assert_eq!(
        h.on_control(Time(3), &other, &mut t).unwrap(),
        vec![Control::Reject {
            reason: RejectReason::Busy
        }]
    );
    // 控制端：重复的 AuthStart 被忽略；交换阶段不能再发 Hello
    let replies = c.on_control(Time(4), &first[0], &mut t).unwrap();
    assert!(c.on_control(Time(5), &first[0], &mut t).unwrap().is_empty());
    assert!(c.hello(Time(5), &mut t).is_err());
    // 被控端：重复的交换消息被忽略
    assert!(h
        .on_control(Time(6), &replies[0], &mut t)
        .unwrap()
        .is_empty());
    assert!(h
        .on_control(Time(7), &replies[0], &mut t)
        .unwrap()
        .is_empty());
    let done = h.on_control(Time(8), &replies[1], &mut t).unwrap();
    assert!(matches!(
        done[..],
        [Control::AuthProof { .. }, Control::Accept { .. }]
    ));
    for m in &done {
        c.on_control(Time(9), m, &mut t).unwrap();
    }
    assert!(c.suite().is_some());
    // 交换阶段被拒绝、关闭
    let (mut c, mut h, mut t) = auth_pair(pw("pw"), pw("pw"));
    let hello = c.hello(Time(0), &mut t).unwrap();
    let start = h
        .on_control(Time(1), &hello, &mut t)
        .unwrap()
        .pop()
        .unwrap();
    c.on_control(Time(2), &start, &mut t).unwrap();
    c.on_control(
        Time(3),
        &Control::Reject {
            reason: RejectReason::Locked,
        },
        &mut t,
    )
    .unwrap();
    assert_eq!(*c.state(), State::Rejected(RejectReason::Locked));
    h.close(Time(4), &mut t);
    assert_eq!(*h.state(), State::Closed);
    unexpected(h.on_control(Time(5), &Control::Ping { nonce: 1 }, &mut t));
}
