//! 剪贴板同步：真实 QUIC 会话，两端用内存里的剪贴板（测试不碰开发者电脑的剪贴板）。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use peerdesk_core::clipboard::{pack_files, Clip, ClipKind, Limits, PackedFile};
use peerdesk_core::crypto::Policy;
use peerdesk_core::input::InputEvent;
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
    fn inject(&mut self, _: &InputEvent) {}
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
    session_with_input(
        host_caps,
        host_allows,
        host_has_clipboard,
        |_| Box::new(NoInput),
        |host, ctrl, _input| script(host, ctrl),
    )
    .await;
}

/// 被控端收到的按键，连同收到那一刻被控端剪贴板里的内容。
type Typed = Arc<Mutex<Vec<(InputEvent, Option<Clip>)>>>;

/// 记下收到的按键和当时剪贴板内容的被控端输入（不动真的键鼠）。
struct Recorder {
    clip: Arc<MemClipboard>,
    typed: Typed,
}
impl InputSink for Recorder {
    fn inject(&mut self, event: &InputEvent) {
        if matches!(event, InputEvent::Key { .. }) {
            let now = self.clip.get();
            self.typed.lock().unwrap().push((event.clone(), now));
        }
    }
}

/// 等被控端收到 `n` 个按键。
async fn typed_n(typed: &Typed, n: usize) -> Vec<(InputEvent, Option<Clip>)> {
    let start = Instant::now();
    loop {
        let got = typed.lock().unwrap().clone();
        if got.len() >= n {
            return got;
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "按键没有到达被控端：{}",
            got.len()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 同上，另外给出控制端的输入通道；`sink` 用被控端的剪贴板造出被控端的输入。
async fn session_with_input<S, F, Fut>(
    host_caps: Caps,
    host_allows: bool,
    host_has_clipboard: bool,
    sink: S,
    script: F,
) where
    S: FnOnce(Arc<MemClipboard>) -> Box<dyn InputSink>,
    F: FnOnce(Arc<MemClipboard>, Arc<MemClipboard>, mpsc::Sender<InputEvent>) -> Fut,
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
    opts.name = "测试电脑 01".into();
    let host_task = tokio::spawn(host::serve(
        host_link,
        opts,
        Box::new(DemoDesktop::new(320, 180, 1)),
        sink(host_clip.clone()),
    ));
    let os_report = Arc::new(Mutex::new(None));
    let probe = controller::EstablishedHook(Arc::new(|_, _| {}));
    assert_eq!(format!("{probe:?}"), "EstablishedHook");
    let seen_os = Arc::clone(&os_report);
    let (tx, rx) = mpsc::channel(64);
    let ctrl = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(
                &ctrl_id,
                Caps::CLIPBOARD
                    .with(Caps::CLIPBOARD_FILES)
                    .with(Caps::CLIPBOARD_ACK),
            ),
            deadline_us: 200_000,
            duration: None,
            clipboard: Some(ClipboardHandle(ctrl_clip.clone())),
            on_established: Some(controller::EstablishedHook(Arc::new(
                move |caps: Caps, name: &str| {
                    *seen_os.lock().unwrap() = Some((caps.os(), name.to_owned()));
                },
            ))),
            on_link: None,
            view: None,
            on_quality: None,
            audio: None,
        },
        Box::new(CountSink::new()),
        rx,
        None,
    ));
    // 等会话建立、剪贴板流打开
    tokio::time::sleep(Duration::from_millis(300)).await;
    // 握手完成时控制端得知了被控端的系统和电脑名（新版本被控端在 Accept 里带上）
    assert_eq!(
        *os_report.lock().unwrap(),
        Some((
            peerdesk_core::proto::PeerOs::current(),
            "测试电脑 01".into()
        ))
    );
    script(host_clip, ctrl_clip, tx.clone()).await;
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

fn packed(files: &[(&str, usize)]) -> Clip {
    let files: Vec<PackedFile> = files
        .iter()
        .map(|(p, n)| PackedFile {
            path: (*p).into(),
            data: vec![7; *n],
        })
        .collect();
    Clip {
        kind: ClipKind::Files,
        data: pack_files(&files).unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn files_up_to_20_mb_are_copied_and_larger_ones_explained() {
    session_with(
        Caps::CLIPBOARD.with(Caps::CLIPBOARD_FILES),
        true,
        true,
        |host, ctrl| async move {
            // 一个文件夹（两个文件）加一个 5 MB 的文件
            let files = packed(&[
                ("报告/a.txt", 10),
                ("报告/b.bin", 1000),
                ("视频.mp4", 5 << 20),
            ]);
            ctrl.copy(files.clone());
            let took = arrives(&host, &files).await;
            assert!(took < Duration::from_secs(2), "5 MB 文件用了 {took:?}");
            // 超过 20 MB：不发，两端都有提示
            ctrl.copy_too_large(ClipKind::Files, 25 << 20);
            let start = Instant::now();
            while !host.notes().iter().any(|n| {
                matches!(
                    n,
                    ClipNote::RemoteTooLarge {
                        kind: ClipKind::Files,
                        ..
                    }
                )
            }) {
                assert!(start.elapsed() < Duration::from_secs(5), "没有收到超限提示");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(ctrl.notes().iter().any(|n| matches!(
                n,
                ClipNote::LocalTooLarge {
                    kind: ClipKind::Files,
                    ..
                }
            )));
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn files_are_not_sent_to_an_old_peer() {
    // 被控端支持剪贴板但不支持文件（老版本）：不发文件，提示对方版本不支持；文字照常同步
    session_with(Caps::CLIPBOARD, true, true, |host, ctrl| async move {
        ctrl.copy(packed(&[("a.txt", 3)]));
        let start = Instant::now();
        while !ctrl.notes().contains(&ClipNote::FilesUnsupported) {
            assert!(start.elapsed() < Duration::from_secs(5), "没有提示");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(host.get(), None);
        let text = Clip::text("still works");
        ctrl.copy(text.clone());
        arrives(&host, &text).await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn identical_content_on_both_sides_is_not_rewritten() {
    // 另一个远程软件（例如 ToDesk）也在同步剪贴板：两边几乎同时有了同样的内容。
    // 各自收到对方发来的同一份内容时不再写一遍（原来会来回覆盖）。
    session_with(Caps::CLIPBOARD, true, true, |host, ctrl| async move {
        let same = Clip::text("同一段文字");
        host.copy(same.clone());
        ctrl.copy(same.clone());
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(host.get(), Some(same.clone()));
        assert_eq!(ctrl.get(), Some(same));
        let rewrites = |c: &MemClipboard| {
            c.notes()
                .iter()
                .filter(|n| matches!(n, ClipNote::Received { .. }))
                .count()
        };
        assert_eq!((rewrites(&host), rewrites(&ctrl)), (0, 0));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_copy_survives_writes_that_take_longer_than_a_poll() {
    // 15 MB 的图片：写一块要等对方读走（流量控制），比 100 ms 的轮询间隔长。
    // 原来写到一半被轮询打断后整块重写，流里的数据错位，剪贴板同步断掉（CI 上复现）。
    session_with(Caps::CLIPBOARD, true, true, |host, ctrl| async move {
        let big = Clip {
            kind: ClipKind::Png,
            data: (0..15u32 << 20)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect(),
        };
        ctrl.copy(big.clone());
        arrives(&host, &big).await;
        // 之后同步照常
        let after = Clip::text("after");
        ctrl.copy(after.clone());
        arrives(&host, &after).await;
    })
    .await;
}

const fn key(code: u16, down: bool) -> InputEvent {
    InputEvent::Key { code, down }
}

/// Ctrl+V 的四个按键事件。
const CTRL_V: [InputEvent; 4] = [
    key(0xE0, true),
    key(0x19, true),
    key(0x19, false),
    key(0xE0, false),
];

/// 产品负责人报的问题（Mac 上复制文件，马上到 Windows 里粘贴，粘不出来）的时序：
/// 输入走数据报，剪贴板内容要先被轮询到（最多 100 ms）、再读盘、传输、解包，粘贴键先到。
/// 现在粘贴键等内容进了对方剪贴板（对方回确认）再发。
#[tokio::test(flavor = "multi_thread")]
async fn a_paste_right_after_a_copy_pastes_the_new_content() {
    let typed = Typed::default();
    let seen = Arc::clone(&typed);
    session_with_input(
        Caps::CLIPBOARD
            .with(Caps::CLIPBOARD_FILES)
            .with(Caps::CLIPBOARD_ACK),
        true,
        true,
        move |clip| Box::new(Recorder { clip, typed: seen }),
        |host, ctrl, input| async move {
            // 文字：复制后马上粘贴（连轮询都还没轮到）
            let text = Clip::text("刚复制的文字");
            ctrl.copy(text.clone());
            for event in CTRL_V {
                input.send(event).await.unwrap();
            }
            let got = typed_n(&typed, 4).await;
            let keys: Vec<InputEvent> = got.iter().map(|(e, _)| e.clone()).collect();
            assert_eq!(keys, CTRL_V, "按键顺序不能变");
            assert_eq!(got[1].1, Some(text), "按下 V 时新内容已经在对方剪贴板里");

            // 5 MB 的文件：同样
            let files = packed(&[("报告/a.txt", 10), ("视频.mp4", 5 << 20)]);
            ctrl.copy(files.clone());
            for event in CTRL_V {
                input.send(event).await.unwrap();
            }
            let got = typed_n(&typed, 8).await;
            assert_eq!(got[5].0, key(0x19, true));
            assert_eq!(got[5].1, Some(files.clone()));
            assert_eq!(host.get(), Some(files.clone()));

            // 没有新复制的内容：粘贴键不耽搁（不到提示的时间）
            let start = Instant::now();
            for event in CTRL_V {
                input.send(event).await.unwrap();
            }
            typed_n(&typed, 12).await;
            let took = start.elapsed();
            assert!(took < Duration::from_millis(300), "{took:?}");

            // 复制的内容太大、不同步：粘贴键不白等
            ctrl.copy_too_large(ClipKind::Files, 25 << 20);
            let start = Instant::now();
            for event in CTRL_V {
                input.send(event).await.unwrap();
            }
            let got = typed_n(&typed, 16).await;
            let took = start.elapsed();
            assert!(took < Duration::from_secs(1), "{took:?}");
            assert_eq!(got[13].1, Some(files));
        },
    )
    .await;
}

/// 被控端是不回确认的老版本：粘贴键等内容写进流里就发（比原来晚，但不保证对方已经写进剪贴板），
/// 不会卡住。
#[tokio::test(flavor = "multi_thread")]
async fn pastes_still_go_through_to_a_peer_without_acks() {
    let typed = Typed::default();
    let seen = Arc::clone(&typed);
    session_with_input(
        Caps::CLIPBOARD,
        true,
        true,
        move |clip| Box::new(Recorder { clip, typed: seen }),
        |host, ctrl, input| async move {
            let text = Clip::text("old peer");
            ctrl.copy(text.clone());
            let start = Instant::now();
            for event in CTRL_V {
                input.send(event).await.unwrap();
            }
            let got = typed_n(&typed, 4).await;
            let took = start.elapsed();
            assert!(took < Duration::from_secs(1), "{took:?}");
            let keys: Vec<InputEvent> = got.iter().map(|(e, _)| e.clone()).collect();
            assert_eq!(keys, CTRL_V);
            arrives(&host, &text).await;
        },
    )
    .await;
}
