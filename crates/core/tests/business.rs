//! 业务测试：端到端场景，并用追踪记录断言“业务走了哪条路径”。
//! 线上出问题时拿到的也是同一种追踪文本，可以直接对照这里的期望路径。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use peerdesk_core::crypto::{Policy, Suite};
use peerdesk_core::proto::RejectReason;
use peerdesk_core::scenario::{run, Params};
use peerdesk_core::sim::Link;
use peerdesk_core::trace::Stage;
use peerdesk_core::Error;

const CONTROLLER_OK: [Stage; 3] = [Stage::HelloSent, Stage::AcceptReceived, Stage::Established];
const HOST_OK: [Stage; 4] = [
    Stage::HelloReceived,
    Stage::SuiteNegotiated,
    Stage::AcceptSent,
    Stage::Established,
];

#[test]
fn remote_session_on_lan_delivers_every_frame() {
    let report = run(&Params::default()).unwrap();
    assert!(report.established);
    assert_eq!(report.suite, Some(Suite::Aes256Gcm));
    assert_eq!(report.rejected, None);
    assert_eq!(report.hello_attempts, 1);
    assert_eq!(report.frames_sent, 120);
    assert_eq!(report.frames_delivered, 120);
    assert_eq!(report.bad_datagrams, 0);
    assert_eq!(report.net.lost, 0);
    assert_eq!(report.controller_path, CONTROLLER_OK);
    assert_eq!(report.host_path, HOST_OK);
    // 局域网上每帧的传输延迟在 1 毫秒以内
    assert!(report.latency_p95_us < 1_000, "{}", report.latency_p95_us);
    assert!(report.latency_p50_us <= report.latency_p95_us);
    assert!(report.handshake_us < 2_000);
    assert_eq!(report.trace_dump.matches("frame_sent").count(), 120);
    assert_eq!(report.trace_dump.matches("frame_delivered").count(), 120);
}

#[test]
fn every_suite_combination_carries_frames() {
    for suite in Suite::ALL {
        let policy = Policy::custom(&[suite]).unwrap();
        let report = run(&Params {
            controller_policy: policy.clone(),
            host_policy: policy,
            frames: 10,
            ..Params::default()
        })
        .unwrap();
        assert_eq!(report.suite, Some(suite));
        assert_eq!(report.frames_delivered, 10, "{suite}");
    }
}

#[test]
fn incompatible_policies_end_in_rejection_without_frames() {
    let report = run(&Params {
        controller_policy: Policy::none(),
        host_policy: Policy::fips(),
        ..Params::default()
    })
    .unwrap();
    assert!(!report.established);
    assert_eq!(report.rejected, Some(RejectReason::NoCommonSuite));
    assert_eq!(report.frames_sent, 0);
    assert_eq!(report.controller_path, [Stage::HelloSent, Stage::Rejected]);
    assert_eq!(report.host_path, [Stage::HelloReceived, Stage::Rejected]);
}

#[test]
fn handshake_retries_through_heavy_loss() {
    let report = run(&Params {
        seed: 4,
        link: Link {
            loss_permille: 600,
            ..Link::lan()
        },
        frames: 0,
        ..Params::default()
    })
    .unwrap();
    assert!(report.established, "60% 丢包下靠重发仍能握手");
    assert!(report.hello_attempts > 1);
    assert_eq!(report.frames_sent, 0);
    // 多次 Hello，但只建立一次
    assert_eq!(
        report
            .controller_path
            .iter()
            .filter(|s| **s == Stage::Established)
            .count(),
        1
    );
    assert_eq!(
        report
            .host_path
            .iter()
            .filter(|s| **s == Stage::Established)
            .count(),
        1
    );
}

#[test]
fn unreachable_host_gives_up_after_bounded_attempts() {
    let report = run(&Params {
        link: Link {
            up: false,
            ..Link::lan()
        },
        ..Params::default()
    })
    .unwrap();
    assert!(!report.established);
    assert_eq!(report.hello_attempts, 20);
    assert_eq!(report.suite, None);
    assert_eq!(report.rejected, None);
    assert_eq!(report.handshake_us, 0);
    assert!(report.host_path.is_empty());
    assert_eq!(report.latency_p50_us, 0);
}

#[test]
fn zero_fps_is_an_argument_error() {
    assert_eq!(
        run(&Params {
            fps: 0,
            ..Params::default()
        }),
        Err(Error::InvalidArgument("fps 不能为 0"))
    );
}

#[test]
fn password_protected_session_end_to_end() {
    use peerdesk_core::auth::Auth;
    let ok = run(&Params {
        controller_auth: Auth::Password("correct horse".into()),
        host_auth: Auth::Password("correct horse".into()),
        frames: 20,
        ..Params::default()
    })
    .unwrap();
    assert!(ok.established);
    assert_eq!(ok.frames_delivered, 20);
    assert!(ok.host_path.contains(&Stage::AuthPassed));
    assert!(ok.controller_path.contains(&Stage::AuthPassed));

    let wrong = run(&Params {
        controller_auth: Auth::Password("guess".into()),
        host_auth: Auth::Password("correct horse".into()),
        ..Params::default()
    })
    .unwrap();
    assert!(!wrong.established);
    assert_eq!(wrong.rejected, Some(RejectReason::Unauthorized));
    assert_eq!(wrong.frames_sent, 0);
    assert!(wrong.host_path.contains(&Stage::AuthFailed));
}
