//! 真实连接上的访问授权：密码、锁定、受信任设备、权限。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peerdesk_core::auth::{Auth, Limiter};
use peerdesk_core::crypto::Policy;
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::Caps;
use peerdesk_core::session::SessionConfig;
use peerdesk_core::transport::Frame;
use peerdesk_net::controller::{self, ControllerOptions, ControllerReport, FrameSink, Upload};
use peerdesk_net::host::{self, FrameSource, HostOptions, HostReport, InputSink, Permissions};
use peerdesk_net::{Identity, NetError, Node};
use tokio::sync::mpsc;

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("peerdesk-auth-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Frames(u32);
impl FrameSource for Frames {
    fn next_frame(&mut self) -> Option<Vec<u8>> {
        self.0 = self.0.checked_sub(1)?;
        Some(vec![1; 500])
    }
}
#[derive(Clone, Default)]
struct Count(Arc<Mutex<u32>>);
impl InputSink for Count {
    fn inject(&mut self, _: &InputEvent) {
        *self.0.lock().unwrap() += 1;
    }
}
struct Null;
impl FrameSink for Null {
    fn frame(&mut self, _: &Frame) {}
}

fn cfg(id: &Identity, auth: Auth) -> SessionConfig {
    SessionConfig {
        device_id: id.device_id(),
        region: "t".into(),
        caps: Caps::H264,
        policy: Policy::recommended(),
        auth,
        binding: Vec::new(),
    }
}

fn pw(s: &str) -> Auth {
    Auth::Password(s.into())
}

struct World {
    host_node: Node,
    ctrl_node: Node,
    host_id: Identity,
    ctrl_id: Identity,
    limiter: Arc<Mutex<Limiter>>,
}

fn world(limiter: Limiter) -> World {
    let policy = Policy::recommended();
    let (host_id, ctrl_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    World {
        host_node: Node::bind(any(), &host_id, &policy).unwrap(),
        ctrl_node: Node::bind(any(), &ctrl_id, &policy).unwrap(),
        host_id,
        ctrl_id,
        limiter: Arc::new(Mutex::new(limiter)),
    }
}

type Outcome = (
    Result<ControllerReport, NetError>,
    Result<HostReport, NetError>,
    u32,
);

/// 跑一次会话：被控端要求 `host_auth`，控制端提供 `ctrl_auth`。
async fn attempt(
    w: &World,
    host_auth: Auth,
    ctrl_auth: Auth,
    tweak: impl FnOnce(&mut HostOptions),
    upload: Option<Upload>,
) -> Outcome {
    let addr = w.host_node.local_addr().unwrap();
    let (c, h) = tokio::join!(w.ctrl_node.connect(addr, None), w.host_node.accept());
    let (ctrl_link, host_link) = (c.unwrap(), h.unwrap().unwrap());
    let mut opts = HostOptions::new(cfg(&w.host_id, host_auth), 100, temp_dir("recv"));
    opts.limiter = Arc::clone(&w.limiter);
    tweak(&mut opts);
    let count = Count::default();
    let host_task = tokio::spawn(host::serve(
        host_link,
        opts,
        Box::new(Frames(10_000)),
        Box::new(count.clone()),
    ));
    let (tx, rx) = mpsc::channel(8);
    tx.send(InputEvent::Key {
        code: 4,
        down: true,
    })
    .await
    .unwrap();
    tx.send(InputEvent::Key {
        code: 4,
        down: false,
    })
    .await
    .unwrap();
    let result = controller::run(
        ctrl_link,
        ControllerOptions {
            session: cfg(&w.ctrl_id, ctrl_auth),
            deadline_us: 100_000,
            duration: Some(Duration::from_millis(200)),
        },
        Box::new(Null),
        rx,
        upload,
    )
    .await;
    let host_result = tokio::time::timeout(Duration::from_secs(20), host_task)
        .await
        .unwrap()
        .unwrap();
    let injected = *count.0.lock().unwrap();
    (result, host_result, injected)
}

#[tokio::test(flavor = "multi_thread")]
async fn correct_password_gets_in_wrong_password_does_not() {
    let w = world(Limiter::default());
    let (c, h, injected) =
        attempt(&w, pw("correct horse"), pw("correct horse"), |_| {}, None).await;
    let (c, h) = (c.unwrap(), h.unwrap());
    assert!(c.frames_delivered > 0);
    assert!(c.trace.contains("auth_passed") && h.trace.contains("auth_passed"));
    assert_eq!(injected, 2);

    let (c, h, injected) = attempt(&w, pw("correct horse"), pw("wrong guess"), |_| {}, None).await;
    match c {
        Err(NetError::Connect(why)) => assert!(why.contains("密码错误"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(h, Err(NetError::Connect(_))));
    assert_eq!(injected, 0, "认证失败时一个输入事件都不能注入");

    // 控制端根本没有密码
    let (c, _, injected) = attempt(&w, pw("correct horse"), Auth::Open, |_| {}, None).await;
    assert!(matches!(c, Err(NetError::Connect(_))), "{c:?}");
    assert_eq!(injected, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_failures_lock_out_even_the_right_password() {
    // 2 次失败后锁定 60 秒
    let w = world(Limiter::new(2, 60_000_000, 60_000_000));
    for _ in 0..2 {
        let (c, _, _) = attempt(&w, pw("correct horse"), pw("guess"), |_| {}, None).await;
        assert!(matches!(c, Err(NetError::Connect(_))));
    }
    let (c, h, injected) =
        attempt(&w, pw("correct horse"), pw("correct horse"), |_| {}, None).await;
    match c {
        Err(NetError::Connect(why)) => assert!(why.contains("锁定"), "{why}"),
        other => panic!("{other:?}"),
    }
    match h {
        Err(NetError::Connect(why)) => assert!(why.contains("锁定"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(injected, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn success_resets_the_failure_count() {
    let w = world(Limiter::new(2, 60_000_000, 60_000_000));
    let (c, _, _) = attempt(&w, pw("correct horse"), pw("guess"), |_| {}, None).await;
    assert!(c.is_err());
    let (c, _, _) = attempt(&w, pw("correct horse"), pw("correct horse"), |_| {}, None).await;
    assert!(c.is_ok());
    // 之前那 1 次失败已清零：再错 1 次不会锁定
    let (c, _, _) = attempt(&w, pw("correct horse"), pw("guess"), |_| {}, None).await;
    assert!(c.is_err());
    let (c, _, _) = attempt(&w, pw("correct horse"), pw("correct horse"), |_| {}, None).await;
    assert!(c.is_ok(), "{c:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn trusted_device_needs_no_password_and_is_not_rate_limited() {
    let w = world(Limiter::new(1, 60_000_000, 60_000_000));
    let trusted = w.ctrl_id.fingerprint();
    // 先让别人把它锁住
    let (c, _, _) = attempt(&w, pw("correct horse"), pw("guess"), |_| {}, None).await;
    assert!(c.is_err());
    // 受信任的设备不受影响，也不需要密码
    let (c, h, injected) = attempt(
        &w,
        pw("correct horse"),
        Auth::Open,
        |o| o.trusted = vec![trusted],
        None,
    )
    .await;
    assert!(c.is_ok(), "{c:?}");
    assert!(!h.unwrap().trace.contains("auth_requested"));
    assert_eq!(injected, 2);
    // 不在名单里的指纹不算
    let (c, _, _) = attempt(
        &w,
        pw("correct horse"),
        Auth::Open,
        |o| o.trusted = vec![[9; 32]],
        None,
    )
    .await;
    assert!(c.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn view_only_and_no_files_permissions_are_enforced() {
    let w = world(Limiter::default());
    let src = temp_dir("src");
    std::fs::write(src.join("a.txt"), b"data").unwrap();
    let upload = || Upload {
        root: src.clone(),
        files: peerdesk_net::fs_store::scan(&src, "a.txt").unwrap(),
    };
    let deny = |o: &mut HostOptions| {
        o.permissions = Permissions {
            input: false,
            files: false,
        }
    };

    let (c, h, injected) = attempt(&w, Auth::Open, Auth::Open, deny, Some(upload())).await;
    let h = h.unwrap();
    assert_eq!(injected, 0, "只读模式下输入不注入");
    assert_eq!(h.input_events, 0);
    assert_eq!(h.input_denied, 2);
    assert!(h.files.is_empty());
    // 上传被被控端中止：控制端看到的是没有结果的文件
    let c = c.unwrap();
    assert!(c.files.iter().all(|f| f.outcome.is_none()), "{:?}", c.files);
    assert_eq!(c.input_unacked, 0, "被拒绝的输入仍然被确认，不会无限重发");
    assert!(c.frames_delivered > 0, "仍然可以观看");

    // 对照：默认权限下同样的操作都成功
    let (c, h, injected) = attempt(&w, Auth::Open, Auth::Open, |_| {}, Some(upload())).await;
    assert_eq!(injected, 2);
    assert_eq!(h.unwrap().files.len(), 1);
    assert_eq!(
        c.unwrap().files[0].outcome,
        Some(peerdesk_core::file::Outcome::Done)
    );
    assert_eq!(
        Permissions::default(),
        Permissions {
            input: true,
            files: true
        }
    );
}
