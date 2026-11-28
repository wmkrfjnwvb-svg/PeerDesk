//! “操作到画面”的闭环测试：真实的被控端、控制端会话（QUIC、H.264），操作的时候有别的帧在流
//! （输入框右边的提示标记刚变了一下，编码器正在一帧一帧给它补清晰）。新算法只在第一幅带上这次操作的
//! 画面那里算，和按画面内容量出来的时间对得上；旧算法（操作后的下一幅画面）算在了补清晰的帧上，
//! 读出来比应用把字画出来的时间还短。
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use peerdesk_core::crypto::Policy;
use peerdesk_core::frame::{Frame, Picture};
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::Caps;
use peerdesk_core::screen::{SyntheticDesktop, Workload};
use peerdesk_core::session::SessionConfig;
use peerdesk_net::controller::{self, ControllerOptions, LinkQuality, QualityHook, ScreenSink};
use peerdesk_net::host::{self, HostOptions, InputSink, ScreenSource};
use peerdesk_net::{Identity, Node};
use tokio::sync::mpsc;

/// 应用收到输入到把字画出来的时间（真实应用要一两帧；这里取 50 ms）。
const RENDER: Duration = Duration::from_millis(50);
/// 和操作无关的变化发生在操作之前多久。
const BEFORE: Duration = Duration::from_millis(400);
/// 再往前这么久，旁边的窗口滚一下。
const SCROLL_BEFORE: Duration = Duration::from_millis(300);
const SIZE: (u16, u16) = (640, 360);
/// 字块：第 `k` 个输入画在第 `k` 格（白底上的黑块，够大，开头半分辨率时也认得出）。
const GLYPH: u16 = 32;
const TEXT_Y: u16 = 200;
const WHITE: u32 = 0x00ff_ffff;

/// 统计日志收进内存：测试结束时挑出“操作”相关的几行打出来，也据此核对被控端那一行。
struct Lines(Mutex<Vec<String>>);

impl log::Log for Lines {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        true
    }
    fn log(&self, record: &log::Record<'_>) {
        let line = record.args().to_string();
        if line.contains("操作") {
            self.0.lock().unwrap().push(line);
        }
    }
    fn flush(&self) {}
}

static LINES: Lines = Lines(Mutex::new(Vec::new()));

/// 被控端的桌面：合成桌面（窗口、文字），上面盖一行输入框。
struct Desk {
    desktop: SyntheticDesktop,
    /// 和操作无关的变化：输入框最右边的提示标记（亮 / 灭）。
    badge: bool,
    /// 旁边的窗口要滚几下（下一次取画面时滚一下）。
    scrolls: u32,
    typed: u16,
    /// 已经注入、应用还没画出来的输入：到这个时刻才出现在画面上。
    drawing: Vec<Instant>,
}

impl Desk {
    fn new() -> Self {
        Self {
            desktop: SyntheticDesktop::new(SIZE.0, SIZE.1, 7),
            badge: false,
            scrolls: 0,
            typed: 0,
            drawing: Vec::new(),
        }
    }
}

struct DeskSource(Arc<Mutex<Desk>>);

impl ScreenSource for DeskSource {
    fn frame(&mut self) -> Option<Frame> {
        let mut d = self.0.lock().unwrap();
        let now = Instant::now();
        while d.drawing.first().is_some_and(|at| *at <= now) {
            d.drawing.remove(0);
            d.typed += 1;
        }
        if d.scrolls > 0 {
            d.scrolls -= 1;
            d.desktop.step(Workload::Scrolling);
        }
        let mut screen = d.desktop.screen().clone();
        screen.fill(0, TEXT_Y, SIZE.0, GLYPH, WHITE);
        if d.badge {
            screen.fill(SIZE.0 - GLYPH, TEXT_Y, GLYPH, GLYPH, 0x00c0_2020);
        }
        screen.fill(0, TEXT_Y, d.typed * GLYPH, GLYPH, 0);
        Some(Frame::rgb(screen))
    }
}

struct DeskInput(Arc<Mutex<Desk>>);

impl InputSink for DeskInput {
    fn inject(&mut self, event: &InputEvent) {
        // 按下键、打的字：应用过 RENDER 才画出来。抬起键不改变画面。
        if matches!(
            event,
            InputEvent::Key { down: true, .. } | InputEvent::Text(_)
        ) {
            self.0.lock().unwrap().drawing.push(Instant::now() + RENDER);
        }
    }
}

/// 控制端的显示：看画面里已经有几个字块，记下每个字块第一次出现的时刻（这是“真的看到了”的时刻）。
struct Watch(Arc<Mutex<Vec<Instant>>>);

impl ScreenSink for Watch {
    fn present(&mut self, picture: &Picture) {
        // 直接看亮度平面里的一个点：整幅转 RGB 要十几毫秒，会把控制端的会话循环卡住，输入就发晚了
        // H.264 解出来的是 NV12；不是的话什么都认不出，测试会因为“字块没显示出来”失败
        let Picture::Nv12(nv12) = picture else {
            return;
        };
        let (w, h) = picture.size();
        let (luma, _) = nv12.planes();
        let mut seen = self.0.lock().unwrap();
        loop {
            // 第 seen.len() 格的中心（画面可能按比例缩小过）
            let k = u32::try_from(seen.len()).unwrap();
            let x =
                (k * u32::from(GLYPH) + u32::from(GLYPH) / 2) * u32::from(w) / u32::from(SIZE.0);
            let y = (u32::from(TEXT_Y) + u32::from(GLYPH) / 2) * u32::from(h) / u32::from(SIZE.1);
            let (x, y) = (u16::try_from(x).unwrap(), u16::try_from(y).unwrap());
            if x >= w || luma[usize::from(y) * usize::from(w) + usize::from(x)] > 0x60 {
                break;
            }
            seen.push(Instant::now());
        }
    }
}

fn config(id: &Identity, policy: &Policy, caps: Caps) -> SessionConfig {
    SessionConfig {
        device_id: id.device_id(),
        region: "test".into(),
        caps,
        policy: policy.clone(),
        auth: peerdesk_core::auth::Auth::Open,
        binding: Vec::new(),
    }
}

/// 一次会话的结果：每次操作的真实延迟（按画面内容）、每秒报告的新算法和旧算法的数。
struct Run {
    truth_ms: Vec<u128>,
    reaction_ms: Vec<u32>,
    next_frame_ms: Vec<u32>,
}

/// 跑一次会话：每 1.25 秒按一次键（每秒最多一次，每秒报告的中位数就是那一次）；按键之前 [`BEFORE`]
/// 提示标记变一下（和操作无关的变化，按键时编码器正在给它补清晰），再往前 [`SCROLL_BEFORE`] 旁边的窗口滚一下
/// （大变化，把码率用紧）。最后打一个字（旧算法不算打字）。
async fn run(ctrl_caps: Caps, keys: u16) -> Run {
    let any: SocketAddr = ([127, 0, 0, 1], 0).into();
    let policy = Policy::recommended();
    let (host_id, ctrl_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let host_node = Node::bind(any, &host_id, &policy).unwrap();
    let ctrl_node = Node::bind(any, &ctrl_id, &policy).unwrap();
    let pin = host_id.fingerprint();
    let addr = host_node.local_addr().unwrap();
    let (c, h) = tokio::join!(ctrl_node.connect(addr, Some(&pin)), host_node.accept());
    let (host_link, ctrl_link) = (h.unwrap().unwrap(), c.unwrap());

    let desk = Arc::new(Mutex::new(Desk::new()));
    let recv = std::env::temp_dir().join(format!("peerdesk-reaction-{}", std::process::id()));
    let host_caps = Caps::H264.with(Caps::INPUT_STAMP);
    let host_task = tokio::spawn(host::serve(
        host_link,
        // 码率给得紧，前面又刚滚过一下：提示标记那一帧编不到最清楚，停下 200 ms 后编码器一帧一帧补清晰
        // （约 10 帧，按键时正在流）。真机上的硬件编码器每次变化之后都这样补；软件编码器码率够时一帧就到位，
        // 所以这里把码率压低。
        HostOptions {
            start_bps: 200_000,
            max_bps: 200_000,
            ..HostOptions::new(config(&host_id, &policy, host_caps), 30, recv)
        },
        Box::new(DeskSource(Arc::clone(&desk))),
        Box::new(DeskInput(Arc::clone(&desk))),
    ));

    let seen = Arc::new(Mutex::new(Vec::new()));
    let quality = Arc::new(Mutex::new(Vec::<LinkQuality>::new()));
    let hook = Arc::clone(&quality);
    let (tx, rx) = mpsc::channel(8);
    let total = Duration::from_millis(1500 + 1250 * u64::from(keys) + 1250 + 1200);
    let ctrl_task = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            decoder: None,
            record: None,
            session: config(&ctrl_id, &policy, ctrl_caps),
            deadline_us: 200_000,
            duration: Some(total),
            clipboard: None,
            on_established: None,
            on_link: None,
            view: None,
            on_quality: Some(QualityHook(Arc::new(move |q| hook.lock().unwrap().push(q)))),
            audio: None,
        },
        Box::new(Watch(Arc::clone(&seen))),
        rx,
        None,
    ));

    // 等开头的画面稳定（起步的关键帧、补清晰都过去）
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let mut sent = Vec::new();
    for i in 0..=keys {
        tokio::time::sleep(Duration::from_millis(1250) - BEFORE - SCROLL_BEFORE).await;
        desk.lock().unwrap().scrolls = 1;
        tokio::time::sleep(SCROLL_BEFORE).await;
        {
            let mut d = desk.lock().unwrap();
            d.badge = !d.badge;
        }
        tokio::time::sleep(BEFORE).await;
        sent.push(Instant::now());
        if i < keys {
            tx.send(InputEvent::Key {
                code: 4,
                down: true,
            })
            .await
            .unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(InputEvent::Key {
                code: 4,
                down: false,
            })
            .await
            .unwrap();
        } else {
            tx.send(InputEvent::Text("好".into())).await.unwrap();
        }
    }
    let report = ctrl_task.await.unwrap().unwrap();
    host_task.abort();
    assert_eq!(report.input_unacked, 0);

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), sent.len(), "每次操作的字块都显示出来了");
    let quality = quality.lock().unwrap();
    Run {
        truth_ms: seen
            .iter()
            .zip(&sent)
            .map(|(shown, at)| shown.duration_since(*at).as_millis())
            .collect(),
        reaction_ms: quality.iter().filter_map(|q| q.reaction_ms).collect(),
        next_frame_ms: quality
            .iter()
            .filter_map(|q| q.reaction_next_frame_ms)
            .collect(),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[allow(clippy::print_stderr)]
async fn reaction_is_counted_at_the_first_frame_that_shows_the_input() {
    if log::set_logger(&LINES).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    let render = u32::try_from(RENDER.as_millis()).unwrap();
    let keys = 6u16;
    let new = run(Caps::H264.with(Caps::INPUT_STAMP), keys).await;
    for line in LINES.0.lock().unwrap().drain(..) {
        eprintln!("{line}");
    }
    eprintln!(
        "按画面内容量的真实延迟 {:?} ms\n新算法 {:?} ms\n旧算法 {:?} ms",
        new.truth_ms, new.reaction_ms, new.next_frame_ms
    );
    // 新算法：每次操作（6 次按键、1 次打字）都算到了，而且就是字块显示出来的那一刻
    // （毫秒取整，加上测试线程记发出时刻和会话循环收到之间的一点点差）
    assert_eq!(new.reaction_ms.len(), usize::from(keys) + 1);
    for (got, truth) in new.reaction_ms.iter().zip(&new.truth_ms) {
        assert!(
            u128::from(*got) + 3 >= *truth && u128::from(*got) <= *truth + 1,
            "新算法 {got} ms，真实 {truth} ms"
        );
        assert!(
            *got >= render,
            "应用 {render} ms 才画出来，不可能更快：{got}"
        );
    }
    // 旧算法：只算按键（打字不算）；有帧在流，下一幅画面多半是操作生效之前采的，
    // 读出来比应用画出来的时间还短
    assert_eq!(new.next_frame_ms.len(), usize::from(keys));
    let impossible = new.next_frame_ms.iter().filter(|v| **v < render).count();
    assert!(
        impossible * 2 >= usize::from(keys),
        "旧算法 {:?}",
        new.next_frame_ms
    );

    // 老版本的控制端（不带输入序号）：被控端不发戳，控制端用旧算法，日志里写明
    let old = run(Caps::H264, 1).await;
    assert_eq!(old.reaction_ms, old.next_frame_ms);
    assert_eq!(old.reaction_ms.len(), 1);
    let lines: Vec<String> = LINES.0.lock().unwrap().drain(..).collect();
    assert!(lines.iter().any(|l| l.contains("操作到画面（旧算法")));
    assert!(
        !lines.iter().any(|l| l.starts_with("操作响应 ")),
        "没有戳就没有新算法的行"
    );
    assert!(
        lines.iter().any(|l| l.starts_with("操作响应（被控端）")),
        "被控端自己那一行照常有"
    );
}
