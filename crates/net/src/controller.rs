//! 控制端的一次会话：握手，接收并重组画面，发送输入，发送文件。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::crypto::Suite;
use peerdesk_core::file::FileMeta;
use peerdesk_core::input::{InputEvent, InputSender};
use peerdesk_core::media::{DecodeJob, DecodeWorker, MediaReceiver};
use peerdesk_core::proto::{Caps, Channel, Control, RejectReason};
use peerdesk_core::screen::Screen;
use peerdesk_core::session::{Session, SessionConfig, State};
use peerdesk_core::trace::{Stage, TraceId, Tracer};
use peerdesk_core::video::{TileDecoder, VideoDecoder};
use peerdesk_media::H264Decoder;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::files::{send_files, FileResult};
use crate::host::{control_reader, HELLO_TIMEOUT};
use crate::stream::{read_control, write_control, write_kind, StreamKind};
use crate::{Fingerprint, Link, NetError};

/// 输入包的重发间隔：有未确认的事件时按这个间隔重发。
const INPUT_RESEND: Duration = Duration::from_millis(20);
/// 没有新情况时两次反馈之间的间隔（微秒）。
const IDLE_FEEDBACK_US: u64 = 1_000_000;
/// 这么久一个包都没收到就认为网络断了（QUIC 保活每 2 秒一次，正常时不会这么久没包），
/// 不等 10 秒的空闲超时，好让界面尽快显示“网络重连中”并换通道。
const SILENCE_LOST_US: u64 = 3_000_000;

/// 画面去向。窗口显示实现这个接口。
pub trait ScreenSink: Send {
    /// 画面有更新。`screen` 是解码后的完整画面。
    fn present(&mut self, screen: &Screen);
}

/// 要上传的文件。
#[derive(Debug, Clone)]
pub struct Upload {
    /// 本地根目录。
    pub root: PathBuf,
    /// 根目录下要发送的文件。
    pub files: Vec<FileMeta>,
}

/// 控制端会话的参数。
#[derive(Debug, Clone)]
pub struct ControllerOptions {
    /// 握手配置。
    pub session: SessionConfig,
    /// 一帧从首个分片到收齐允许的最长时间（微秒）。
    pub deadline_us: u64,
    /// 会话时长上限；`None` 表示一直运行到对端结束。到时间后仍会等上传完成。
    pub duration: Option<Duration>,
    /// 本机剪贴板；`None` 表示不同步剪贴板。双方都协商出剪贴板能力时才同步。
    pub clipboard: Option<crate::clipsync::ClipboardHandle>,
    /// 握手完成时调用，参数是被控端 Accept 里的能力（含被控端的系统，见 `Caps::os`）
    /// 和被控端的电脑名（老版本为空）。
    pub on_established: Option<EstablishedHook>,
    /// 声音：去向和开关；`None` 表示本机不播放声音。对方不支持声音（老版本）时开关无效。
    pub audio: Option<crate::audio::AudioOut>,
    /// 每秒报告一次链路质量（界面显示网络状况用）。
    pub on_quality: Option<QualityHook>,
}

/// 每秒报告一次的链路质量和两端的状态（“详情”面板显示）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinkQuality {
    /// 往返时间（毫秒）。
    pub rtt_ms: u32,
    /// 接收端测得的丢包率（千分比）。
    pub loss_permille: u16,
    /// 排队延迟（毫秒）：链路被占满时变大。
    pub queue_ms: u32,
    /// 这一秒显示的帧数。
    pub fps: u32,
    /// 这一秒“按键或点击到下一幅画面”的中位数（毫秒）；没有操作时为 `None`。
    pub reaction_ms: Option<u32>,
    /// 这一秒收到的画面码率（kbps）。
    pub recv_kbps: u32,
    /// 这一秒最慢的一次解码（毫秒）。
    pub decode_ms: u32,
    /// 显示的画面分辨率。
    pub width: u16,
    /// 显示的画面分辨率。
    pub height: u16,
    /// 本机到路由器的往返（毫秒）；测不出来时为 `None`。
    pub router_ms: Option<u32>,
    /// 本机的解码器。
    pub decoder: String,
    /// 纠错方式（"RS" 或 "XOR"）。
    pub fec: &'static str,
    /// 被控端最近一次发来的状态（老版本不发）。
    pub host: Option<peerdesk_core::stats::HostStats>,
}

/// 链路质量的回调。
#[derive(Clone)]
pub struct QualityHook(pub Arc<dyn Fn(LinkQuality) + Send + Sync>);

impl std::fmt::Debug for QualityHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("QualityHook")
    }
}

/// 握手完成时的回调。
#[derive(Clone)]
pub struct EstablishedHook(pub Arc<OnEstablished>);

/// 握手完成回调的签名：被控端的能力和电脑名。
pub type OnEstablished = dyn Fn(Caps, &str) + Send + Sync;

impl std::fmt::Debug for EstablishedHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EstablishedHook")
    }
}

/// 统计日志的一个窗口：上一次写日志时的累计值，以及这一秒里的帧数和最长间隔。
#[derive(Debug, Default)]
struct StatsWindow {
    /// 这一秒最慢的一次解码（微秒）。
    decode_us: u64,
    /// 这一秒里“按键或点击之后到下一幅画面”的时间（微秒）：用户感觉到的操作延迟（上限）。
    reactions: Vec<u64>,
    frames: u32,
    max_gap_us: u64,
    bytes: u64,
    expired: u64,
    recovered: u64,
    bad: u32,
}

/// 按键或点击之后这么久还没有新画面，就不算这一次（多半是操作没有改变画面）。
const REACTION_LIMIT_US: u64 = 2_000_000;

/// 一组操作延迟的中位数和最大值（毫秒）。
fn reaction_summary(v: &[u64]) -> (u64, u64) {
    let mut v = v.to_vec();
    v.sort_unstable();
    let median = v.get(v.len() / 2).copied().unwrap_or(0);
    let max = v.last().copied().unwrap_or(0);
    (median / 1000, max / 1000)
}

/// 统计日志的间隔。
const STATS_EVERY: Duration = Duration::from_secs(1);

/// 控制端会话的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerReport {
    /// 对端指纹。
    pub peer: Fingerprint,
    /// 选中的加密套件。
    pub suite: Suite,
    /// 完整收到的帧数。
    pub frames_delivered: u64,
    /// 超时或被新帧取代而作废的帧数。
    pub frames_expired: u64,
    /// 收到的画面字节数。
    pub video_bytes: u64,
    /// 无法解码的帧数。
    pub bad_frames: u64,
    /// 靠纠错恢复的分片数。
    pub recovered: u64,
    /// 会话结束时解码器里的画面；一帧都没收到时为 `None`。
    pub screen: Option<Screen>,
    /// 无法解析而被丢弃的数据报数。
    pub bad_datagrams: u64,
    /// 发出的输入事件数。
    pub input_sent: u64,
    /// 会话结束时还没被确认的输入事件数。
    pub input_unacked: usize,
    /// 上传的文件。
    pub files: Vec<FileResult>,
    /// 上传实际发送的字节数。
    pub file_bytes: u64,
    /// 会话结束时的往返延迟估计。
    pub rtt: Duration,
    /// 网络上实际发出和收到的字节数（含一切开销）。
    pub wire_bytes: (u64, u64),
    /// 会话为什么结束。
    pub end: String,
    /// 业务追踪。
    pub trace: String,
}

/// 解码线程交回的结果：帧号和解出的画面。
/// 解码结果：帧号、画面、解码耗时（微秒）。
type Decoded = (u32, Result<Option<Screen>, peerdesk_core::Error>, u64);

/// 启动解码线程。解码一帧要好几毫秒到几十毫秒，放在会话循环里会让本地的键鼠输入排队等它。
/// 任务通道关闭（会话结束）时线程退出。
fn spawn_decoder(
    decoder: Box<dyn VideoDecoder>,
) -> (
    std::sync::mpsc::Sender<DecodeJob>,
    mpsc::UnboundedReceiver<Decoded>,
) {
    let (jobs, job_rx) = std::sync::mpsc::channel::<DecodeJob>();
    let (done_tx, done_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut worker = DecodeWorker::new(decoder);
        while let Ok(job) = job_rx.recv() {
            let started = std::time::Instant::now();
            let result = worker.decode(&job);
            let took = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            if done_tx.send((job.frame_id, result, took)).is_err() {
                break;
            }
        }
    });
    (jobs, done_rx)
}

/// 在一条已建立的连接上运行控制端会话。
#[allow(clippy::too_many_lines)]
pub async fn run(
    link: Link,
    opts: ControllerOptions,
    mut sink: Box<dyn ScreenSink>,
    mut input: mpsc::Receiver<InputEvent>,
    upload: Option<Upload>,
) -> Result<ControllerReport, NetError> {
    let link = Arc::new(link);
    let trace = TraceId(opts.session.device_id);
    let mut tracer = Tracer::new(4096);
    let mut config = opts.session.clone();
    config.binding = link.channel_binding()?;
    let mut session = Session::controller(config, trace);

    // 握手阶段对端拒绝时（例如屏幕暂时打不开）会带着原因关闭连接；流上的错误只有“连接断了”，
    // 换成对端给的原因（也不算网络断开，不用重连）。
    let refused = |e: NetError| match link.refused_reason() {
        Some(why) => NetError::Connect(format!("对方拒绝了连接：{why}")),
        None => e,
    };
    let (mut ctrl_send, mut ctrl_recv) = link.open_stream().await.map_err(refused)?;
    write_kind(&mut ctrl_send, StreamKind::Control)
        .await
        .map_err(refused)?;
    let hello = session.hello(link.now(), &mut tracer)?;
    write_control(&mut ctrl_send, &hello)
        .await
        .map_err(refused)?;
    let (suite, caps) = loop {
        let msg = tokio::time::timeout(HELLO_TIMEOUT, read_control(&mut ctrl_recv))
            .await
            .map_err(|_| NetError::Timeout("等待握手应答"))?
            .map_err(refused)?;
        let replies = match session.on_control(link.now(), &msg, &mut tracer) {
            Ok(replies) => replies,
            Err(e) => {
                link.close("protocol");
                return Err(e.into());
            }
        };
        for reply in &replies {
            write_control(&mut ctrl_send, reply).await?;
        }
        match session.state() {
            State::Established { suite, caps, .. } => break (*suite, *caps),
            State::HelloSent | State::AwaitAuth => {}
            State::Rejected(reason) => {
                link.close("rejected");
                return Err(NetError::Connect(match reason {
                    RejectReason::Unauthorized => "密码错误，或被控端需要密码".to_owned(),
                    RejectReason::Locked => "被控端因多次认证失败暂时锁定".to_owned(),
                    RejectReason::Busy => "被控端正在被其他设备控制".to_owned(),
                    RejectReason::Version => {
                        "对方的 PeerDesk 版本太旧，不能连接，请先升级对方".to_owned()
                    }
                    RejectReason::NoCommonSuite => {
                        format!("被控端拒绝：{reason:?}")
                    }
                }));
            }
            State::Idle | State::Closed => {
                link.close("protocol");
                return Err(NetError::Protocol("握手应答不是 Accept 或 Reject"));
            }
        }
    };
    let mut ctrl_rx = control_reader(ctrl_recv);
    if let Some(hook) = &opts.on_established {
        (hook.0)(caps, session.peer_name());
    }

    // 剪贴板同步：由控制端打开专用的流。对方是不支持剪贴板的老版本时不开。
    let clip_task = match opts.clipboard.clone().filter(|_| caps.has(Caps::CLIPBOARD)) {
        Some(clipboard) => {
            let (mut send, recv) = link.open_stream().await?;
            write_kind(&mut send, StreamKind::Clipboard).await?;
            Some(tokio::spawn(crate::clipsync::run(
                send,
                recv,
                clipboard.0,
                peerdesk_core::clipboard::Limits::DEFAULT,
                caps.has(Caps::CLIPBOARD_FILES),
            )))
        }
        None => None,
    };

    // 声音：开关打开时开一条声音流（开着被控端就发），关掉时关流。
    let mut audio_switch = opts.audio.clone().filter(|_| caps.has(Caps::AUDIO));
    let mut audio_stream: Option<quinn::SendStream> = None;
    let mut audio_rx: Option<crate::audio::Receiver> = None;
    let mut audio_tick = tokio::time::interval(Duration::from_millis(10));
    audio_tick.set_missed_tick_behavior(MissedTickBehavior::Burst);
    let mut audio_changed = audio_switch.is_some();
    let mut audio_ticks = 0u64;

    let mut upload_task = upload.map(|u| {
        let link = Arc::clone(&link);
        tokio::spawn(async move { send_files(&link, &u.root, u.files).await })
    });
    let mut upload_result: Option<Result<(Vec<FileResult>, u64), NetError>> = None;

    let (mut last_received, mut last_heard) = (link.received_packets(), link.now());
    let mut resend = tokio::time::interval(INPUT_RESEND);
    resend.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let time_limit = tokio::time::sleep(opts.duration.unwrap_or(Duration::MAX / 4));
    tokio::pin!(time_limit);
    let mut time_up = false;
    let mut input_open = true;

    let mut sender = InputSender::new();
    let (mut bad_datagrams, mut input_sent) = (0u64, 0u64);
    // 两端都支持 H.264 就用它，否则用块编码。
    let decoder: Box<dyn VideoDecoder> = if caps.common(Caps::H264) == Caps::H264 {
        Box::new(H264Decoder::new()?)
    } else {
        Box::new(TileDecoder::new())
    };
    let decoder_name = if caps.common(Caps::H264) == Caps::H264 {
        "软件 OpenH264".to_owned()
    } else {
        "块编码".to_owned()
    };
    let fec_name = if caps.has(Caps::FEC_RS) { "RS" } else { "XOR" };
    let mut media = MediaReceiver::detached(link.now(), opts.deadline_us);
    media.set_keyframe_grace(caps.common(Caps::H264) == Caps::H264);
    // 详情面板：本机到路由器的往返在后台测；被控端的状态和画面尺寸随时更新
    let router_ms = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
    let router_task = opts
        .on_quality
        .as_ref()
        .map(|_| tokio::spawn(crate::gateway::watch_router(Arc::clone(&router_ms))));
    let mut host_stats: Option<peerdesk_core::stats::HostStats> = None;
    let mut shown_size = (0u16, 0u16);
    media.set_fec(if caps.has(Caps::FEC_RS) {
        peerdesk_core::transport::Fec::Rs
    } else {
        peerdesk_core::transport::Fec::Xor
    });
    let (decode_jobs, mut decoded_rx) = spawn_decoder(decoder);
    let mut last_screen: Option<Screen> = None;
    // 反馈只在有新情况时发；什么都没发生时每秒一次，证明自己还在。
    let mut video_since_feedback = 0u32;
    let mut reported_floor = 0u32;
    let mut last_feedback = link.now();

    // 每秒一行统计：收到几帧、码率、两帧之间最长隔了多久（卡顿）、作废和纠错。
    let mut stats_tick = tokio::time::interval(STATS_EVERY);
    stats_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut window = StatsWindow::default();
    let mut waiting_input: Option<peerdesk_core::time::Time> = None;
    let mut last_shown = link.now();

    let end = loop {
        if time_up && upload_task.is_none() {
            break "到达时长上限".to_owned();
        }
        tokio::select! {
            () = &mut time_limit, if !time_up => time_up = true,
            // 解码线程交回一帧：显示。
            done = decoded_rx.recv() => match done {
                Some((frame_id, Ok(Some(screen)), took)) => {
                    window.decode_us = window.decode_us.max(took);
                    shown_size = (screen.width(), screen.height());
                    media.decode_succeeded();
                    let now = link.now();
                    window.frames = window.frames.saturating_add(1);
                    window.max_gap_us = window.max_gap_us.max(now.since(last_shown));
                    last_shown = now;
                    if let Some(at) = waiting_input.take() {
                        let took = now.since(at);
                        if took <= REACTION_LIMIT_US {
                            window.reactions.push(took);
                        }
                    }
                    tracer.record(link.now(), trace, Stage::FrameDelivered, u64::from(frame_id));
                    sink.present(&screen);
                    last_screen = Some(screen);
                }
                Some((_, Ok(None), _)) => {}
                Some((_, Err(_), _)) => media.decode_failed(),
                None => break "解码线程结束".to_owned(),
            },
            result = async {
                match upload_task.as_mut() {
                    Some(task) => task.await,
                    None => std::future::pending().await,
                }
            } => {
                upload_task = None;
                upload_result = Some(result.unwrap_or_else(|e| Err(NetError::Io(e.to_string()))));
            }
            changed = async {
                match audio_switch.as_mut() {
                    Some(sw) if !audio_changed => sw.on.changed().await.is_ok(),
                    Some(_) => true,
                    None => std::future::pending().await,
                }
            } => {
                audio_changed = false;
                let Some(sw) = audio_switch.as_ref().filter(|_| changed) else {
                    audio_switch = None;
                    continue;
                };
                let on = *sw.on.borrow();
                if on && audio_stream.is_none() {
                    let (mut send, _recv) = link.open_stream().await?;
                    write_kind(&mut send, StreamKind::Audio).await?;
                    audio_stream = Some(send);
                    audio_rx = Some(crate::audio::Receiver::new(sw.playback.clone())?);
                    // 关着的时候积下的节拍作废：否则一打开就连播几十帧静音，凭空多出几百毫秒延迟
                    audio_tick.reset();
                    crate::note!(info, "声音：打开");
                } else if !on {
                    if let Some(mut send) = audio_stream.take() {
                        let _ = send.finish();
                        crate::note!(info, "声音：关闭");
                    }
                    audio_rx = None;
                }
            }
            _ = audio_tick.tick(), if audio_rx.is_some() => {
                if let Some(rx) = audio_rx.as_mut() {
                    rx.tick();
                    // 每 500 ms 把丢包率报告给被控端（它据此决定带不带冗余）
                    audio_ticks = audio_ticks.wrapping_add(1);
                    if audio_ticks % (crate::audio::REPORT_MS / 10) == 0 {
                        let loss = rx.loss_report();
                        if let Some(send) = audio_stream.as_mut() {
                            let _ = send.write_all(&loss.to_be_bytes()).await;
                        }
                    }
                }
            }
            datagram = link.recv() => match datagram {
                Ok((header, payload)) => match header.channel {
                    Channel::Audio => {
                        if let Some(rx) = audio_rx.as_mut() {
                            rx.on_datagram(link.now().micros(), &payload);
                        }
                    }
                    Channel::Video => {
                        let now = link.now();
                        video_since_feedback = video_since_feedback.saturating_add(1);
                        match media.on_datagram_deferred(now, header.send_time, header.seq, header.flags, &payload) {
                            Ok(Some(job)) => {
                                if decode_jobs.send(job).is_err() { break "解码线程结束".to_owned(); }
                            }
                            Ok(None) => {}
                            Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                        }
                    }
                    Channel::Input => match <[u8; 4]>::try_from(&payload[..]) {
                        Ok(ack) => sender.ack(u32::from_be_bytes(ack)),
                        Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                    },
                    // 被控端每秒的状态（详情面板）
                    Channel::Control => match peerdesk_core::stats::HostStats::decode(&payload) {
                        Ok(stats) => host_stats = Some(stats),
                        Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                    },
                    Channel::Cursor | Channel::Clipboard | Channel::File => {
                        bad_datagrams = bad_datagrams.saturating_add(1);
                    }
                },
                Err(NetError::Closed(why)) => break why,
                Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
            },
            event = input.recv(), if input_open => match event {
                Some(event) => {
                    // 按下键或鼠标键：记下时间，等下一幅画面
                    let press = matches!(
                        event,
                        InputEvent::Key { down: true, .. } | InputEvent::MouseButton { down: true, .. }
                    );
                    if press && waiting_input.is_none() {
                        waiting_input = Some(link.now());
                    }
                    sender.push(event);
                    input_sent = input_sent.saturating_add(1);
                    let failed = sender.packet()?.and_then(|p| link.send(Channel::Input, &p).err());
             if let Some(e) = failed { break e.to_string(); }
                }
                None => input_open = false,
            },
            _ = stats_tick.tick() => {
                let now = link.now();
                // 这一秒一帧都没显示：从上一帧到现在都算等待。
                let gap = window.max_gap_us.max(now.since(last_shown));
                let reassembly = media.reassembly();
                let stats = media.stats();
                crate::note!(info, target: "peerdesk::stats",
                    "接收 帧 {} 画面 {} kbps 最长间隔 {} ms 作废 {} 纠错恢复 {} 坏帧 {} RTT {} ms 输入未确认 {} 操作到画面 {} 次 中位 {} ms 最大 {} ms",
                    window.frames,
                    stats.video_bytes.saturating_sub(window.bytes) * 8 / 1000,
                    gap / 1000,
                    reassembly.expired.saturating_sub(window.expired),
                    reassembly.recovered.saturating_sub(window.recovered),
                    stats.bad.saturating_sub(window.bad),
                    link.rtt().as_millis(),
                    sender.pending(),
                    window.reactions.len(),
                    reaction_summary(&window.reactions).0,
                    reaction_summary(&window.reactions).1,
                );
                if let Some(hook) = &opts.on_quality {
                    let fb = media.last_feedback().unwrap_or_default();
                    (hook.0)(LinkQuality {
                        rtt_ms: u32::try_from(link.rtt().as_millis()).unwrap_or(u32::MAX),
                        loss_permille: fb.loss_permille,
                        queue_ms: fb.queue_delay_us / 1000,
                        fps: window.frames,
                        reaction_ms: (!window.reactions.is_empty())
                            .then(|| u32::try_from(reaction_summary(&window.reactions).0).unwrap_or(u32::MAX)),
                        recv_kbps: u32::try_from(stats.video_bytes.saturating_sub(window.bytes) * 8 / 1000).unwrap_or(u32::MAX),
                        decode_ms: u32::try_from(window.decode_us / 1000).unwrap_or(u32::MAX),
                        width: shown_size.0,
                        height: shown_size.1,
                        router_ms: Some(router_ms.load(std::sync::atomic::Ordering::Relaxed)).filter(|v| *v != u32::MAX),
                        decoder: decoder_name.clone(),
                        fec: fec_name,
                        host: host_stats.clone(),
                    });
                }
                window = StatsWindow {
                    bytes: stats.video_bytes,
                    expired: reassembly.expired,
                    recovered: reassembly.recovered,
                    bad: stats.bad,
                    ..StatsWindow::default()
                };
            }
            _ = resend.tick() => {
                let now = link.now();
                let received = link.received_packets();
                if received != last_received {
                    last_received = received;
                    last_heard = now;
                } else if now.since(last_heard) >= SILENCE_LOST_US {
                    break format!("connection lost：{} 秒没有收到对端的数据", SILENCE_LOST_US / 1_000_000);
                }
                // 反馈：哪些帧到了、排队延迟、丢包率、接收速率。
                let (floor, expired) = media.settle(now);
                for frame_id in expired {
                    tracer.record(now, trace, Stage::FrameExpired, u64::from(frame_id));
                }
                let news = video_since_feedback > 0 || floor != reported_floor || media.wants_nack(now);
                if news || now.since(last_feedback) >= IDLE_FEEDBACK_US {
                    video_since_feedback = 0;
                    reported_floor = floor;
                    last_feedback = now;
                    let feedback = media.feedback_datagram(now);
                    if let Err(e) = link.send(Channel::Control, &feedback) { break e.to_string(); }
                }
                if sender.pending() > 0 {
                    let failed = sender.packet()?.and_then(|p| link.send(Channel::Input, &p).err());
             if let Some(e) = failed { break e.to_string(); }
                }
            }
            msg = ctrl_rx.recv() => match msg {
                Some(Control::Bye) => break "对端结束会话".to_owned(),
                // 控制流断了：网络断开时报网络原因（控制端据此换通道重连），否则是对端结束。
                None => break link.lost_reason().unwrap_or_else(|| "对端结束会话".to_owned()),
                Some(
                    Control::Hello { .. }
                    | Control::Accept { .. }
                    | Control::Reject { .. }
                    | Control::Ping { .. }
                    | Control::Pong { .. }
                    | Control::AuthStart { .. }
                    | Control::AuthProof { .. },
                ) => {}
            },
        }
    };
    if let Some(t) = &router_task {
        t.abort();
    }

    let bye = session.close(link.now(), &mut tracer);
    let _ = write_control(&mut ctrl_send, &bye).await;
    let _ = ctrl_send.finish();
    if let Some(task) = upload_task {
        task.abort();
    }
    if let Some(task) = clip_task {
        task.abort();
    }
    let rtt = link.rtt();
    let wire_bytes = link.wire_bytes();
    // 给对端一点时间读到 Bye，再关闭连接。
    let _ = tokio::time::timeout(Duration::from_millis(500), link.closed()).await;
    link.close("bye");

    let stats = media.reassembly();
    let (files, file_bytes) = match upload_result {
        Some(Ok(done)) => done,
        Some(Err(e)) => return Err(e),
        None => (Vec::new(), 0),
    };
    Ok(ControllerReport {
        peer: *link.peer_fingerprint(),
        suite,
        frames_delivered: u64::from(media.stats().decoded),
        frames_expired: stats.expired,
        video_bytes: media.stats().video_bytes,
        bad_frames: u64::from(media.stats().bad),
        recovered: stats.recovered,
        screen: last_screen,
        bad_datagrams,
        input_sent,
        input_unacked: sender.pending(),
        files,
        file_bytes,
        rtt,
        wire_bytes,
        end,
        trace: tracer.dump(),
    })
}

#[cfg(test)]
mod tests {
    use super::reaction_summary;

    #[test]
    fn reaction_summary_is_median_and_max_in_ms() {
        assert_eq!(reaction_summary(&[]), (0, 0));
        assert_eq!(reaction_summary(&[90_000, 30_000, 250_000]), (90, 250));
        let hook = super::QualityHook(std::sync::Arc::new(|_| {}));
        assert_eq!(format!("{hook:?}"), "QualityHook");
    }
}
