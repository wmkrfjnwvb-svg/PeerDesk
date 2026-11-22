//! 剪贴板同步：真实 QUIC 会话，两端用内存里的剪贴板（测试不碰开发者电脑的剪贴板）。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use peerdesk_core::clipboard::{Clip, ClipKind, Limits};
use peerdesk_core::crypto::Policy;
use peerdesk_core::proto::Caps;
use peerdesk_core::session::SessionConfig;
use peerdesk_net::clipsync::{ClipNote, ClipboardHandle, MemClipboard};
use peerdesk_net::controller::{self, ControllerOptions};
use peerdesk_net::demo::{CountSink, DemoDesktop};
use peerdesk_net::host::{self, HostOptions, InputSink};
use peerdesk_net::{Identity, Node};
use tokio::sync::mpsc;

struct NoInput;
impl InputSink for NoInput {
    fn inject(&mut self, _: &peerdesk_core::input::InputEvent) {}
}

fn session(id: &Identity, caps: Caps) -> SessionConfig {
    SessionConfig {
        device_id: id.device_id(),
        region: "test".into(),
        caps,
        policy: Policy::recommended(),
        auth: peerdesk_core::auth::Auth::Open,
        binding: Vec::new(),
    }
}

/// 等 `clip` 里出现 `want`，返回用了多久。
async fn arrives(clip: &MemClipboard, want: &Clip) -> Duration {
    let start = Instant::now();
    while clip.get().as_ref() != Some(want) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "剪贴板内容没有到达"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    start.elapsed()
}

/// 跑一次会话：`script` 在会话期间操作两端的剪贴板。
async fn session_with<F, Fut>(
    host_caps: Caps,
    host_allows: bool,
    host_has_clipboard: bool,
    script: F,
) where
    F: FnOnce(Arc<MemClipboard>, Arc<MemClipboard>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let any: SocketAddr = ([127, 0, 0, 1], 0).into();
    let (host_id, ctrl_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let policy = Policy::recommended();
    let host_node = Node::bind(any, &host_id, &policy).unwrap();
    let ctrl_node = Node::bind(any, &ctrl_id, &policy).unwrap();
    let addr = host_node.local_addr().unwrap();
    let pin = host_id.fingerprint();
    let (c, h) = tokio::join!(ctrl_node.connect(addr, Some(&pin)), host_node.accept());
    let (host_link, ctrl_link) = (h.unwrap().unwrap(), c.unwrap());

    let (host_clip, ctrl_clip) = (
        Arc::new(MemClipboard::default()),
        Arc::new(MemClipboard::default()),
    );
    let mut opts = HostOptions::new(session(&host_id, host_caps), 30, std::env::temp_dir());
    opts.clipboard = host_has_clipboard.then(|| ClipboardHandle(host_clip.clone()));
    opts.permissions.clipboard = host_allows;
    let host_task = tokio::spawn(host::serve(
        host_link,
        opts,
        Box::new(DemoDesktop::new(320, 180, 1)),
        Box::new(NoInput),
    ));
    let os_report = Arc::new(std::sync::Mutex::new(None));
    let probe = controller::EstablishedHook(Arc::new(|_| {}));
    assert_eq!(format!("{probe:?}"), "EstablishedHook");
    let seen_os = Arc::clone(&os_report);
    let (tx, rx) = mpsc::channel(4);
    let ctrl = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&ctrl_id, Caps::CLIPBOARD),
            deadline_us: 200_000,
            duration: None,
            clipboard: Some(ClipboardHandle(ctrl_clip.clone())),
            on_established: Some(controller::EstablishedHook(Arc::new(move |caps: Caps| {
                *seen_os.lock().unwrap() = Some(caps.os());
            }))),
            audio: None,
        },
        Box::new(CountSink::new()),
        rx,
        None,
    ));
    // 等会话建立、剪贴板流打开
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 握手完成时控制端得知了被控端的系统（新版本被控端在 Accept 里带上）
    assert_eq!(
        *os_report.lock().unwrap(),
        Some(peerdesk_core::proto::PeerOs::current())
    );
    script(host_clip, ctrl_clip).await;
    drop(tx);
    ctrl.abort();
    let _ = ctrl.await;
    let _ = tokio::time::timeout(Duration::from_secs(5), host_task).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn text_and_images_sync_both_ways() {
    session_with(Caps::CLIPBOARD, true, true, |host, ctrl| async move {
        // 控制端 → 被控端：文本。门槛：300 ms 内可以粘贴
        let text = Clip::text("从控制端复制的文字 copied text");
        ctrl.copy(text.clone());
        let took = arrives(&host, &text).await;
        assert!(took < Duration::from_millis(300), "文本同步用了 {took:?}");

        // 被控端 → 控制端：一张 2 MB 的“截图”（多块）
        let png = Clip {
            kind: ClipKind::Png,
            data: (0..2_000_000u32)
                .map(|i| u8::try_from(i % 249).unwrap())
                .collect(),
        };
        host.copy(png.clone());
        let took = arrives(&ctrl, &png).await;
        assert!(took < Duration::from_secs(1), "2 MB 图片同步用了 {took:?}");

        // 写进控制端的内容不会被发回去（防回声）：被控端的剪贴板保持不变
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(host.get(), Some(png));

        // 太大的内容不同步，两端都得到提示
        let huge = Clip {
            kind: ClipKind::Text,
            data: vec![b'a'; usize::try_from(Limits::DEFAULT.text).unwrap() + 1],
        };
        ctrl.copy(huge);
        let start = Instant::now();
        while !host
            .notes()
            .iter()
            .any(|n| matches!(n, ClipNote::RemoteTooLarge { .. }))
        {
            assert!(start.elapsed() < Duration::from_secs(5), "没有收到超限提示");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ctrl
            .notes()
            .iter()
            .any(|n| matches!(n, ClipNote::LocalTooLarge { .. })));
        assert!(host
            .notes()
            .iter()
            .any(|n| matches!(n, ClipNote::Received { .. })));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn old_peer_or_disabled_permission_means_no_sync() {
    // 被控端是不支持剪贴板的老版本（能力位里没有剪贴板），或者被控端关掉了剪贴板权限：
    // 会话照常进行，剪贴板不同步。
    // 第三种：双方都声明了能力，但被控端打不开系统剪贴板：拒绝剪贴板流，会话照常。
    for (caps, allows, has) in [
        (Caps(0), true, true),
        (Caps::CLIPBOARD, false, true),
        (Caps::CLIPBOARD, true, false),
    ] {
        session_with(caps, allows, has, |host, ctrl| async move {
            ctrl.copy(Clip::text("secret"));
            tokio::time::sleep(Duration::from_millis(500)).await;
            assert_eq!(host.get(), None);
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_write_does_not_stop_syncing() {
    session_with(Caps::CLIPBOARD, true, true, |host, ctrl| async move {
        // 被控端的剪贴板被别的程序占着：第一份内容重试后仍写不进去
        host.fail_next_writes(5);
        ctrl.copy(Clip::text("first"));
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(host.get(), None);
        // 之后的复制照常同步（原来写失败会结束整个剪贴板同步）
        let second = Clip::text("second");
        ctrl.copy(second.clone());
        let took = arrives(&host, &second).await;
        assert!(took < Duration::from_millis(600), "{took:?}");
    })
    .await;
}
