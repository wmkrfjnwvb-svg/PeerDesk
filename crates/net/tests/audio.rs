//! 声音：真实 QUIC 会话，被控端用合成的声音，控制端用录音器（测试不碰开发者电脑的声卡）。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::crypto::Policy;
use peerdesk_core::proto::Caps;
use peerdesk_core::session::SessionConfig;
use peerdesk_net::audio::{
    AudioCapture, AudioOut, CaptureHandle, PlaybackHandle, Recorder, ToneCapture,
};
use peerdesk_net::controller::{self, ControllerOptions};
use peerdesk_net::demo::{CountSink, DemoDesktop};
use peerdesk_net::host::{self, HostOptions, InputSink};
use peerdesk_net::{Identity, Node};
use tokio::sync::{mpsc, watch};

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

/// 一帧的能量（均方值）。
fn energy(pcm: &[i16]) -> f64 {
    pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len().max(1) as f64
}

async fn session_with(
    host_caps: Caps,
    host_has_audio: bool,
) -> (Arc<Recorder>, watch::Sender<bool>, impl FnOnce()) {
    let capture = host_has_audio.then(|| CaptureHandle(Arc::new(ToneCapture)));
    session_with_capture(host_caps, capture).await
}

/// 只送 `frames` 帧就结束的声音来源（例如声卡被拔掉）。
struct Finite(usize);

impl AudioCapture for Finite {
    fn start(&self) -> Result<mpsc::Receiver<Vec<i16>>, String> {
        let (tx, rx) = mpsc::channel(self.0.max(1));
        for _ in 0..self.0 {
            let _ = tx.try_send(vec![4000; peerdesk_media::audio::FRAME_SAMPLES]);
        }
        Ok(rx)
    }
}

/// 跑一次会话：`host_caps` 是被控端声明的能力。返回录音器和开关。
async fn session_with_capture(
    host_caps: Caps,
    capture: Option<CaptureHandle>,
) -> (Arc<Recorder>, watch::Sender<bool>, impl FnOnce()) {
    let any: SocketAddr = ([127, 0, 0, 1], 0).into();
    let (host_id, ctrl_id) = (Identity::generate().unwrap(), Identity::generate().unwrap());
    let policy = Policy::recommended();
    let host_node = Node::bind(any, &host_id, &policy).unwrap();
    let ctrl_node = Node::bind(any, &ctrl_id, &policy).unwrap();
    let addr = host_node.local_addr().unwrap();
    let pin = host_id.fingerprint();
    let (c, h) = tokio::join!(ctrl_node.connect(addr, Some(&pin)), host_node.accept());
    let (host_link, ctrl_link) = (h.unwrap().unwrap(), c.unwrap());

    let mut opts = HostOptions::new(session(&host_id, host_caps), 30, std::env::temp_dir());
    opts.audio = capture;
    let host_task = tokio::spawn(host::serve(
        host_link,
        opts,
        Box::new(DemoDesktop::new(320, 180, 1)),
        Box::new(NoInput),
    ));
    let recorder = Arc::new(Recorder::default());
    let (switch, on) = watch::channel(false);
    let (tx, rx) = mpsc::channel(4);
    let ctrl = tokio::spawn(controller::run(
        ctrl_link,
        ControllerOptions {
            session: session(&ctrl_id, Caps::AUDIO),
            deadline_us: 200_000,
            duration: None,
            clipboard: None,
            on_established: None,
            on_quality: None,
            audio: Some(AudioOut {
                playback: PlaybackHandle(recorder.clone()),
                on,
            }),
        },
        Box::new(CountSink::new()),
        rx,
        None,
    ));
    let stop = move || {
        drop(tx);
        ctrl.abort();
        host_task.abort();
        drop((host_node, ctrl_node));
    };
    (recorder, switch, stop)
}

#[tokio::test(flavor = "multi_thread")]
async fn sound_is_off_by_default_then_plays_continuously() {
    let (recorder, switch, stop) = session_with(Caps::AUDIO, true).await;
    // 默认关：什么都不播
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(recorder.frames().is_empty());

    // 打开：开始播放
    switch.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let frames = recorder.frames();
    // 1.5 秒应播约 150 帧（每 10 ms 一帧）
    assert!(frames.len() >= 120, "只播了 {} 帧", frames.len());
    // 起步缓冲之后应当一直有声音：静音帧（缓冲等待）只出现在开头
    let first_sound = frames.iter().position(|f| energy(f) > 1e5).unwrap();
    assert!(first_sound <= 10, "开头等了 {first_sound} 帧才有声音");
    let silent_later = frames[first_sound..]
        .iter()
        .filter(|f| energy(f) < 1e5)
        .count();
    // 门槛：有声音以后的欠载静音不超过 2%（CI 虚拟机的定时器抖动偶尔会造成一帧）
    let played = frames.len() - first_sound;
    assert!(
        silent_later * 50 <= played,
        "有声音以后出现 {silent_later}/{played} 帧静音"
    );

    // 关闭：不再播放新的帧
    switch.send(false).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after_off = recorder.frames().len();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(recorder.frames().len(), after_off, "关掉以后还在播");
    stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn old_host_without_sound_support_plays_nothing() {
    // 被控端是不支持声音的老版本：开关无效，会话照常
    let (recorder, switch, stop) = session_with(Caps(0), true).await;
    switch.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(recorder.frames().is_empty());
    stop();
    // 被控端声明支持但没有声音来源（例如没有声卡）：拒绝声音流，控制端只播等待的静音
    let (recorder, switch, stop) = session_with(Caps::AUDIO, false).await;
    switch.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(recorder.frames().iter().all(|f| energy(f) < 1.0));
    // 远程窗口关掉、开关被丢弃：会话照常，不再理会开关
    drop(switch);
    tokio::time::sleep(Duration::from_millis(200)).await;
    stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn capture_that_ends_stops_the_sound_but_not_the_session() {
    let capture = Some(CaptureHandle(Arc::new(Finite(20))));
    let (recorder, switch, stop) = session_with_capture(Caps::AUDIO, capture).await;
    switch.send(true).unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    let frames = recorder.frames();
    // 20 帧一下子全到：超过目标的积压被加速丢掉，播出来的是最后几帧；之后只有等待的静音，会话照常
    let loud = frames.iter().filter(|f| energy(f) > 1e5).count();
    assert!((1..=20).contains(&loud), "有声音的帧 {loud}");
    assert!(frames.len() > loud, "来源结束后继续按时播放（静音）");
    stop();
}
