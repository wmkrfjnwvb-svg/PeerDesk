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
use peerdesk_net::clipsync::{ClipNote, ClipboardHandle, MemClipboard, SendFail};
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

/// 被控端收到的输入，连同收到那一刻被控端剪贴板里的内容。
type Typed = Arc<Mutex<Vec<(InputEvent, Option<Clip>)>>>;

/// 记下收到的输入和当时剪贴板内容的被控端输入（不动真的键鼠）。
struct Recorder {
    clip: Arc<MemClipboard>,
    typed: Typed,
}
impl InputSink for Recorder {
    fn inject(&mut self, event: &InputEvent) {
        let now = self.clip.get();
        self.typed.lock().unwrap().push((event.clone(), now));
    }
}

/// 被控端到现在收到的按键（不含鼠标）。
fn keys_of(typed: &Typed) -> Vec<(InputEvent, Option<Clip>)> {
    let all = typed.lock().unwrap();
    all.iter()
        .filter(|(e, _)| matches!(e, InputEvent::Key { .. }))
        .cloned()
        .collect()
}

/// 等被控端收到 `n` 个按键。
async fn typed_n(typed: &Typed, n: usize) -> Vec<(InputEvent, Option<Clip>)> {
    let start = Instant::now();
    loop {
        let got = keys_of(typed);
        if got.len() >= n {
            return got;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "按键没有到达被控端：{}",
            got.len()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 等 `clip` 收到一条满足条件的提示。
async fn noted(clip: &MemClipboard, what: &str, want: impl Fn(&ClipNote) -> bool) {
    let start = Instant::now();
    while !clip.notes().iter().any(&want) {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "没有等到：{what}"
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
                    .with(Caps::CLIPBOARD_LAZY),
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

/// 支持“粘贴时才传”的被控端。
const LAZY: Caps = Caps(Caps::CLIPBOARD.0 | Caps::CLIPBOARD_FILES.0 | Caps::CLIPBOARD_LAZY.0);

/// 跑一次“粘贴时才传”的会话，被控端记下收到的输入。
async fn lazy_session<F, Fut>(script: F)
where
    F: FnOnce(Arc<MemClipboard>, Arc<MemClipboard>, mpsc::Sender<InputEvent>, Typed) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let typed = Typed::default();
    let seen = Arc::clone(&typed);
    session_with_input(
        LAZY,
        true,
        true,
        move |clip| Box::new(Recorder { clip, typed: seen }),
        |host, ctrl, input| script(host, ctrl, input, typed),
    )
    .await;
}

async fn send(input: &mpsc::Sender<InputEvent>, events: &[InputEvent]) {
    for event in events {
        input.send(event.clone()).await.unwrap();
    }
}

fn received(clip: &MemClipboard) -> usize {
    clip.notes()
        .iter()
        .filter(|n| matches!(n, ClipNote::Received { .. }))
        .count()
}

/// 产品负责人报的问题：复制了文件，鼠标移到远程窗口还没粘贴就卡。原因是一复制就读盘、整份传。
/// 现在复制时什么都不读、不传；按粘贴时才传，传完对方才执行粘贴；传输期间鼠标照常；
/// 同一份内容只传一次。
#[tokio::test(flavor = "multi_thread")]
async fn files_are_sent_when_pasted_not_when_copied() {
    lazy_session(|host, ctrl, input, typed| async move {
        // 复制 5 MB 的文件，不粘贴：不读（真实剪贴板上是读盘、打包），对方什么都没收到，
        // 只知道“对方复制了文件”
        let files = packed(&[("报告/a.txt", 10), ("视频.mp4", 5 << 20)]);
        ctrl.copy(files.clone());
        noted(&host, "对方复制了文件", |n| {
            *n == ClipNote::RemoteHeld {
                kind: ClipKind::Files,
            }
        })
        .await;
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(ctrl.reads(), 0, "复制时不能读剪贴板里的文件");
        assert_eq!(host.get(), None, "复制时不能把内容传给对方");
        assert_eq!(received(&host), 0);

        // 在远程窗口按 Ctrl+V，然后一边传一边动鼠标、松开按键
        send(&input, &[key(0xE0, true), key(0x19, true)]).await;
        noted(
            &ctrl,
            "传输进度",
            |n| matches!(n, ClipNote::Sending { sent, .. } if *sent > 0),
        )
        .await;
        send(&input, &[key(0x19, false), key(0xE0, false)]).await;
        let mut moves = 0u16;
        while host.get().is_none() {
            moves += 1;
            send(&input, &[InputEvent::MouseMove { x: moves, y: 7 }]).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // 被控端：Ctrl 按下、抬起照常到；内容进了剪贴板以后才有 V（由被控端按 Ctrl+V 再松开）
        let got = typed_n(&typed, 6).await;
        let keys: Vec<InputEvent> = got.iter().map(|(e, _)| e.clone()).collect();
        assert_eq!(
            keys,
            vec![
                key(0xE0, true),
                key(0xE0, false),
                key(0xE0, true),
                key(0x19, true),
                key(0x19, false),
                key(0xE0, false),
            ]
        );
        assert_eq!(got[0].1, None);
        assert_eq!(got[1].1, None, "Ctrl 的抬起没有被推迟");
        assert_eq!(
            got[3].1,
            Some(files.clone()),
            "按下 V 时文件已经在对方剪贴板里"
        );
        // 传输期间鼠标照常送达（到的时候对方剪贴板还是空的），没有排在粘贴后面
        let early_moves = typed
            .lock()
            .unwrap()
            .iter()
            .filter(|(e, clip)| matches!(e, InputEvent::MouseMove { .. }) && clip.is_none())
            .count();
        assert!(early_moves >= 5, "传输期间只到了 {early_moves} 次鼠标移动");
        // 界面：先有进度（2 个文件、总大小），最后是完成
        let notes = ctrl.notes();
        let total = u64::try_from(files.data.len()).unwrap();
        assert!(notes.contains(&ClipNote::Sending {
            kind: ClipKind::Files,
            files: 2,
            sent: 0,
            total
        }));
        assert_eq!(notes.last(), Some(&ClipNote::SendDone));
        assert_eq!((ctrl.reads(), received(&host)), (1, 1));

        // 再粘贴一次：剪贴板没变，不读、不传，按键原样马上到
        let start = Instant::now();
        send(&input, &CTRL_V[..2]).await;
        typed_n(&typed, 8).await;
        send(&input, &CTRL_V[2..]).await;
        let got = typed_n(&typed, 10).await;
        assert!(start.elapsed() < Duration::from_millis(500));
        let keys: Vec<InputEvent> = got[6..].iter().map(|(e, _)| e.clone()).collect();
        assert_eq!(keys, CTRL_V);
        assert_eq!((ctrl.reads(), received(&host)), (1, 1));

        // 又复制了一遍同样的文件：粘贴时要读出来比对，内容一样（哈希相同）就不再传
        ctrl.copy(files.clone());
        tokio::time::sleep(Duration::from_millis(300)).await;
        send(&input, &CTRL_V[..2]).await;
        typed_n(&typed, 12).await;
        send(&input, &CTRL_V[2..]).await;
        typed_n(&typed, 14).await;
        assert_eq!((ctrl.reads(), received(&host)), (2, 1));

        // 对方自己复制了别的东西以后，再粘贴本机这份：对方剪贴板里已经不是它，重新传
        host.copy(Clip::text("被控端自己复制的"));
        arrives(&ctrl, &Clip::text("被控端自己复制的")).await;
        ctrl.copy(files.clone());
        tokio::time::sleep(Duration::from_millis(300)).await;
        send(&input, &CTRL_V).await;
        arrives_within(&host, &files, Duration::from_secs(10)).await;
        assert_eq!(received(&host), 2);
    })
    .await;
}

/// 等 `clip` 里出现 `want`（大内容限速传，给够时间）。
async fn arrives_within(clip: &MemClipboard, want: &Clip, limit: Duration) {
    let start = Instant::now();
    while clip.get().as_ref() != Some(want) {
        assert!(start.elapsed() < limit, "剪贴板内容没有到达");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// 传到一半取消、读不出来、太大、对方写不进去：这次粘贴不执行（不粘贴出旧内容），告诉用户原因；
/// 之后再按粘贴照常。
#[tokio::test(flavor = "multi_thread")]
async fn a_transfer_that_fails_drops_the_paste_and_says_why() {
    lazy_session(|host, ctrl, input, typed| async move {
        let failed = |why: SendFail| move |n: &ClipNote| *n == ClipNote::SendFailed(why);
        let files = packed(&[("大文件.bin", 6 << 20)]);
        // 1. 取消
        ctrl.copy(files.clone());
        send(&input, &CTRL_V).await;
        noted(
            &ctrl,
            "传输进度",
            |n| matches!(n, ClipNote::Sending { sent, .. } if *sent > 0),
        )
        .await;
        ctrl.cancel();
        noted(&ctrl, "已取消", failed(SendFail::Cancelled)).await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(host.get(), None, "取消以后对方不能收到内容");
        // 2. 读不出来（文件没有权限）
        ctrl.set_unreadable(true);
        send(&input, &CTRL_V).await;
        noted(&ctrl, "读不出来", failed(SendFail::Unreadable)).await;
        ctrl.set_unreadable(false);
        // 3. 对方的剪贴板写不进去（重试 5 次都失败）
        host.fail_next_writes(5);
        send(&input, &CTRL_V).await;
        noted(&ctrl, "对方写不进去", failed(SendFail::RemoteWrite)).await;
        assert_eq!(host.get(), None);
        // 4. 太大
        ctrl.copy_too_large(ClipKind::Files, 25 << 20);
        tokio::time::sleep(Duration::from_millis(300)).await;
        send(&input, &CTRL_V).await;
        noted(&ctrl, "太大", |n| {
            matches!(n, ClipNote::LocalTooLarge { .. })
        })
        .await;
        // 到这里四次粘贴都没有执行：被控端只收到 Ctrl 的按下和抬起，没有 V
        tokio::time::sleep(Duration::from_millis(300)).await;
        let keys = keys_of(&typed);
        assert_eq!(keys.len(), 8);
        assert!(keys
            .iter()
            .all(|(e, _)| matches!(e, InputEvent::Key { code: 0xE0, .. })));
        // 5. 取消、失败以后同一份内容再按粘贴：重新传，传完粘贴
        ctrl.copy(files.clone());
        tokio::time::sleep(Duration::from_millis(300)).await;
        send(&input, &CTRL_V).await;
        noted(&ctrl, "完成", |n| *n == ClipNote::SendDone).await;
        let got = typed_n(&typed, 14).await;
        assert_eq!(got[11].0, key(0x19, true));
        assert_eq!(got[11].1, Some(files));
    })
    .await;
}

/// 短文字照旧一复制就同步（远程右键菜单里点“粘贴”拿得到）；复制后马上按粘贴，等它到了再执行。
/// 长文字（超过 64 KB）和图片与文件一样，粘贴时才传。
#[tokio::test(flavor = "multi_thread")]
async fn short_text_syncs_at_once_and_long_text_and_images_when_pasted() {
    lazy_session(|host, ctrl, input, typed| async move {
        // 不按粘贴也会到
        let text = Clip::text("短文字");
        ctrl.copy(text.clone());
        let took = arrives(&host, &text).await;
        assert!(took < Duration::from_millis(300), "文本同步用了 {took:?}");
        // 复制后马上粘贴（连轮询都还没轮到）：按下 V 时新内容已经在对方剪贴板里
        let second = Clip::text("刚复制的文字");
        ctrl.copy(second.clone());
        send(&input, &CTRL_V[..2]).await;
        let got = typed_n(&typed, 2).await;
        assert_eq!(got[1].1, Some(second.clone()));
        send(&input, &CTRL_V[2..]).await;
        typed_n(&typed, 4).await;
        // 长文字、图片：复制时不传
        let long = Clip::text(&"字".repeat(40_000));
        let png = Clip {
            kind: ClipKind::Png,
            data: vec![9; 300_000],
        };
        for clip in [long, png] {
            let had = received(&host);
            ctrl.copy(clip.clone());
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert_eq!(received(&host), had, "复制时不能传");
            let before = keys_of(&typed).len();
            send(&input, &[key(0xE1, true), key(0x49, true)]).await;
            // Shift+Insert 也是粘贴；传完由被控端按原来的组合键
            let got = typed_n(&typed, before + 2).await;
            assert_eq!(got[before + 1].0, key(0x49, true));
            assert_eq!(got[before + 1].1, Some(clip));
            send(&input, &[key(0x49, false), key(0xE1, false)]).await;
            typed_n(&typed, before + 4).await;
        }
    })
    .await;
}

/// 被控端复制文件、图片：这个方向目前不传（没有粘贴的时机，方案见 docs/research/clipboard-lazy.md），
/// 不读、不推送，告诉控制端；文字照常同步。
#[tokio::test(flavor = "multi_thread")]
async fn the_host_only_pushes_text() {
    lazy_session(|host, ctrl, _input, _typed| async move {
        let png = Clip {
            kind: ClipKind::Png,
            data: vec![3; 2_000_000],
        };
        host.copy(png);
        noted(&ctrl, "对方复制了图片", |n| {
            *n == ClipNote::RemoteHeld {
                kind: ClipKind::Png,
            }
        })
        .await;
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!((host.reads(), ctrl.get()), (0, None));
        let text = Clip::text("被控端复制的文字");
        host.copy(text.clone());
        arrives(&ctrl, &text).await;
    })
    .await;
}

/// 被控端是老版本（不支持“粘贴时才传”）：和原来一样，复制后马上整份同步，粘贴键原样发、不过闸门。
#[tokio::test(flavor = "multi_thread")]
async fn an_old_peer_gets_the_old_behaviour() {
    let typed = Typed::default();
    let seen = Arc::clone(&typed);
    session_with_input(
        Caps::CLIPBOARD.with(Caps::CLIPBOARD_FILES),
        true,
        true,
        move |clip| Box::new(Recorder { clip, typed: seen }),
        |host, ctrl, input| async move {
            // 不粘贴也整份同步
            let files = packed(&[("a.bin", 1 << 20)]);
            ctrl.copy(files.clone());
            arrives(&host, &files).await;
            assert_eq!(ctrl.reads(), 1);
            // 粘贴键原样、按顺序到，没有粘贴事件展开出来的多余按键
            let start = Instant::now();
            send(&input, &CTRL_V).await;
            let got = typed_n(&typed, 4).await;
            assert!(start.elapsed() < Duration::from_millis(500));
            let keys: Vec<InputEvent> = got.iter().map(|(e, _)| e.clone()).collect();
            assert_eq!(keys, CTRL_V);
            assert!(!ctrl
                .notes()
                .iter()
                .any(|n| matches!(n, ClipNote::Sending { .. })));
        },
    )
    .await;
}
