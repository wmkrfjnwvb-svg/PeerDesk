//! 控制端的一次会话：握手，接收并重组画面，发送输入，发送文件。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::avsync::{AvSync, FrameQueue};
use peerdesk_core::crypto::Suite;
use peerdesk_core::file::FileMeta;
use peerdesk_core::frame::Picture;
use peerdesk_core::input::{InputEvent, InputSender};
use peerdesk_core::media::{DecodeJob, DecodeWorker, MediaReceiver};
use peerdesk_core::proto::{Caps, Channel, Control, RejectReason};
use peerdesk_core::screen::Screen;
use peerdesk_core::session::{Session, SessionConfig, State};
use peerdesk_core::trace::{Stage, TraceId, Tracer};
use peerdesk_core::video::{TileDecoder, VideoDecoder};
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
    /// 画面有更新。`picture` 是解码器给的格式（NV12 或 RGB）：界面把 NV12 交给显卡转颜色。
    fn present(&mut self, picture: &Picture);
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
    /// 握手完成后交出这条连接（文件管理在上面开浏览流），会话结束时交出 `None`。
    pub on_link: Option<LinkHook>,
    /// 显示画面的区域（像素，0×0 表示不限）：变了就告诉被控端，被控端按这个大小编码。
    /// 对方不支持（老版本）时不发。
    pub view: Option<tokio::sync::watch::Receiver<(u16, u16)>>,
    /// 音画同步和画面的播放缓冲（看视频用，见 [`peerdesk_core::avsync`]）：播放延迟的毫秒数，会话中途可以改。
    /// `None` 或 0 表示关：画面收到就显示，声音走自己的抖动缓冲，和没有这个功能时一样。
    /// 打开后画面和声音都按“采集时刻 + 播放延迟”播放。对方不带采集时刻（老版本）时无效。
    /// 给了这个设置（哪怕现在是 0）握手时才声明 `Caps::AV_SYNC`，被控端才给每帧带采集时刻（多 8 字节）。
    pub playout_ms: Option<tokio::sync::watch::Receiver<u32>>,
}

/// 交出连接的回调（见 [`ControllerOptions::on_link`]）。
#[derive(Clone)]
pub struct LinkHook(pub Arc<dyn Fn(Option<Arc<Link>>) + Send + Sync>);

impl std::fmt::Debug for LinkHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LinkHook")
    }
}

/// 会话结束（任何路径）时告诉界面连接没了。
struct LinkGone(Option<LinkHook>);

impl Drop for LinkGone {
    fn drop(&mut self) {
        if let Some(hook) = &self.0 {
            (hook.0)(None);
        }
    }
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
    /// 这一秒的解码耗时（平均 / 最大）。
    decode: crate::diag::Stat,
    /// 这一秒交给界面显示的耗时（转成界面的图片格式等，平均 / 最大）。
    present: crate::diag::Stat,
    /// 上一秒末的累计值（算这一秒的增量）。
    assembled: u32,
    assembly_us: u64,
    nacks: u32,
    skipped: u32,
    duplicates: u64,
    stale: u64,
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
    /// 从会话开始（握手完成）到显示第一幅画面用了多久；没显示过画面时为 `None`。
    pub first_frame: Option<Duration>,
    /// 显示第一幅画面之后，相邻两次画面更新最长隔了多久（停下时画面不变也算在内，看卡顿要结合场景）。
    pub max_gap: Duration,
}

/// 解码线程交回的结果：帧号、画面、解码耗时（微秒）、采集时刻（被控端时钟，对方没带时为 `None`）。
type Decoded = (
    u32,
    Result<Option<Picture>, peerdesk_core::Error>,
    u64,
    Option<u64>,
);

/// 启动解码线程。解码一帧要好几毫秒到几十毫秒，放在会话循环里会让本地的键鼠输入排队等它。
/// 任务通道关闭（会话结束）时线程退出。
fn spawn_decoder(
    decoder: Box<dyn VideoDecoder>,
    backend: Arc<std::sync::Mutex<String>>,
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
            // 解码器可能中途从硬件换成软件：每帧记下现在用的是哪个
            let name = worker.backend();
            if let Ok(mut b) = backend.lock() {
                if *b != name {
                    *b = name;
                }
            }
            if done_tx
                .send((job.frame_id, result, took, job.capture_us))
                .is_err()
            {
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
    // 测量用的逐帧时间日志（PEERDESK_FRAME_LOG，默认关）
    let mut frame_log = crate::framelog::FrameLog::from_env();
    let mut config = opts.session.clone();
    config.binding = link.channel_binding()?;
    if opts.playout_ms.is_none() {
        // 这次会话用不到采集时刻：不声明音画同步，被控端就不给每帧多带 8 字节，线上和没有这个功能时一样
        config.caps = Caps(config.caps.0 & !Caps::AV_SYNC.0);
    }
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
    // 对方支持文件管理时交出连接；会话结束时收回
    let _link_gone = LinkGone(opts.on_link.clone().filter(|_| caps.has(Caps::FILE_BROWSE)));
    if let Some(hook) = &_link_gone.0 {
        (hook.0)(Some(Arc::clone(&link)));
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

    // 画面区域：变了就写给被控端（第一次写时开流）。一开始就发一次当前值。
    let mut view_rx = opts.view.clone().filter(|_| caps.has(Caps::VIEW_SIZE));
    let mut view_stream: Option<quinn::SendStream> = None;
    let mut view_pending = view_rx.is_some();

    // 声音：开关打开时开一条声音流（开着被控端就发），关掉时关流。
    let mut audio_switch = opts.audio.clone().filter(|_| caps.has(Caps::AUDIO));
    let mut audio_stream: Option<quinn::SendStream> = None;
    let mut audio_rx: Option<crate::audio::Receiver> = None;
    let mut audio_tick = tokio::time::interval(Duration::from_millis(10));
    audio_tick.set_missed_tick_behavior(MissedTickBehavior::Burst);
    let mut audio_changed = audio_switch.is_some();
    let mut audio_ticks = 0u64;

    // 音画同步和播放缓冲（默认关）。对方带采集时刻时一直记时钟差（打开的那一刻就能用），
    // 但只有播放延迟不是 0 时才按它排画面和声音。
    let mut playout_rx = opts.playout_ms.clone().filter(|_| caps.has(Caps::AV_SYNC));
    let mut playout_pending = playout_rx.is_some();
    let mut sync = playout_rx.as_ref().and_then(|_| AvSync::new(1));
    let mut playout_on = false;
    // 解码好、等播放时刻的画面（关着时每一帧放进去马上取出来）
    let mut held: FrameQueue<(u32, Picture)> = FrameQueue::default();

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
        Box::new(peerdesk_media::AutoH264Decoder::new()?)
    } else {
        Box::new(TileDecoder::new())
    };
    let decoder_name = Arc::new(std::sync::Mutex::new(decoder.backend()));
    let fec_name = if caps.has(Caps::FEC_RS) { "RS" } else { "XOR" };
    let mut media = MediaReceiver::detached(link.now(), opts.deadline_us);
    media.set_keyframe_grace(caps.common(Caps::H264) == Caps::H264);
    // 双方都支持时在反馈里带到达报告（发送端的网络控制器用）
    media.set_reports(caps.has(Caps::TRANSPORT_CC));
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
    let (decode_jobs, mut decoded_rx) = spawn_decoder(decoder, Arc::clone(&decoder_name));
    // 最后一幅画面（会话结束时转成 RGB 交给报告，测试据此核对内容）
    let mut last_picture: Option<Picture> = None;
    // 反馈只在有新情况时发；什么都没发生时每秒一次，证明自己还在。
    let mut video_since_feedback = 0u32;
    let mut reported_floor = 0u32;
    let mut last_feedback = link.now();

    // 每秒一行统计：收到几帧、码率、两帧之间最长隔了多久（卡顿）、作废和纠错。
    let mut stats_tick = tokio::time::interval(STATS_EVERY);
    stats_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut window = StatsWindow::default();
    let mut last_quic = link.quic_stats();
    let mut waiting_input: Option<peerdesk_core::time::Time> = None;
    let mut last_shown = link.now();
    let session_start = link.now();
    let mut first_frame: Option<u64> = None;
    let mut max_gap_us = 0u64;

    let end = loop {
        // 到了播放时刻的画面：显示（播放缓冲关着时就是刚解出来的那一帧）。
        while let Some((frame_id, picture)) = held.pop_due(link.now().micros()) {
            let now = link.now();
            window.frames = window.frames.saturating_add(1);
            window.max_gap_us = window.max_gap_us.max(now.since(last_shown));
            if first_frame.is_some() {
                max_gap_us = max_gap_us.max(now.since(last_shown));
            } else {
                first_frame = Some(now.since(session_start));
            }
            last_shown = now;
            if let Some(at) = waiting_input.take() {
                let took = now.since(at);
                if took <= REACTION_LIMIT_US {
                    window.reactions.push(took);
                }
            }
            tracer.record(
                link.now(),
                trace,
                Stage::FrameDelivered,
                u64::from(frame_id),
            );
            let presenting = std::time::Instant::now();
            sink.present(&picture);
            frame_log.shown(frame_id, crate::framelog::unix_us());
            window
                .present
                .add(u64::try_from(presenting.elapsed().as_micros()).unwrap_or(u64::MAX));
            last_picture = Some(picture);
        }
        if time_up && upload_task.is_none() {
            break "到达时长上限".to_owned();
        }
        tokio::select! {
            () = &mut time_limit, if !time_up => time_up = true,
            // 解码线程交回一帧：放进播放缓冲（关着时下一轮循环开头马上显示）。
            done = decoded_rx.recv() => match done {
                Some((frame_id, Ok(Some(picture)), took, capture_us)) => {
                    window.decode_us = window.decode_us.max(took);
                    window.decode.add(took);
                    shown_size = picture.size();
                    media.decode_succeeded();
                    let due = sync
                        .as_ref()
                        .filter(|_| playout_on)
                        .zip(capture_us)
                        .and_then(|(s, capture)| s.due_us(capture));
                    held.push(link.now().micros(), due, (frame_id, picture));
                }
                Some((_, Ok(None), _, _)) => {}
                Some((_, Err(_), _, _)) => media.decode_failed(),
                None => break "解码线程结束".to_owned(),
            },
            // 播放缓冲里最早的一帧到时间了：回到循环开头显示。
            () = async {
                match held.next_due() {
                    Some(due) => {
                        let wait = due.saturating_sub(link.now().micros());
                        tokio::time::sleep(Duration::from_micros(wait)).await;
                    }
                    None => std::future::pending().await,
                }
            } => {}
            // 播放延迟改了（一开始也走一次）。
            ok = async {
                match playout_rx.as_mut() {
                    Some(_) if playout_pending => true,
                    Some(rx) => rx.changed().await.is_ok(),
                    None => std::future::pending().await,
                }
            } => {
                playout_pending = false;
                let Some(rx) = playout_rx.as_ref().filter(|_| ok) else {
                    playout_rx = None;
                    continue;
                };
                let delay_ms = *rx.borrow();
                let was_on = playout_on;
                playout_on = delay_ms > 0;
                if let Some(s) = sync.as_mut().filter(|_| playout_on) {
                    s.set_delay_ms(delay_ms);
                }
                if was_on && !playout_on {
                    // 关掉：压着的画面不再等
                    held.release();
                }
                if was_on || playout_on {
                    crate::note!(info, "音画同步：播放延迟 {delay_ms} ms（0 表示关）");
                }
            }
            result = async {
                match upload_task.as_mut() {
                    Some(task) => task.await,
                    None => std::future::pending().await,
                }
            } => {
                upload_task = None;
                upload_result = Some(result.unwrap_or_else(|e| Err(NetError::Io(e.to_string()))));
            }
            ok = async {
                match view_rx.as_mut() {
                    Some(_) if view_pending => true,
                    Some(rx) => rx.changed().await.is_ok(),
                    None => std::future::pending().await,
                }
            } => {
                view_pending = false;
                let Some(rx) = view_rx.as_ref().filter(|_| ok) else {
                    view_rx = None;
                    continue;
                };
                let (w, h) = *rx.borrow();
                if view_stream.is_none() {
                    let (mut send, _recv) = link.open_stream().await?;
                    write_kind(&mut send, StreamKind::View).await?;
                    view_stream = Some(send);
                }
                if let Some(send) = view_stream.as_mut() {
                    let packed = (u32::from(w) << 16) | u32::from(h);
                    send.write_all(&packed.to_be_bytes())
                        .await
                        .map_err(|e| NetError::Closed(e.to_string()))?;
                    crate::note!(info, "画面区域 {w}×{h}（0×0 表示不限）");
                }
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
                    // 音画同步打开时按采集时刻的时间线播放，和画面对齐；关着时按水位
                    rx.tick_synced(link.now().micros(), sync.as_ref().filter(|_| playout_on));
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
                            let now_us = link.now().micros();
                            let capture = rx.on_datagram(now_us, &payload);
                            // 采集时刻记进时钟差（画面和声音共用一个，有声音时以声音包为准）
                            if let Some((s, capture)) = sync.as_mut().zip(capture) {
                                s.observe_sound(now_us, capture);
                            }
                        }
                    }
                    Channel::Video => {
                        let now = link.now();
                        video_since_feedback = video_since_feedback.saturating_add(1);
                        match media.on_datagram_deferred(now, header.send_time, header.seq, header.flags, &payload) {
                            Ok(jobs) => {
                                // 收齐的时刻记进时钟差（没有声音时用它）
                                if let Some(s) = sync.as_mut() {
                                    for capture in jobs.iter().filter_map(|job| job.capture_us) {
                                        s.observe_picture(now.micros(), capture);
                                    }
                                }
                                if jobs.into_iter().any(|job| decode_jobs.send(job).is_err()) { break "解码线程结束".to_owned(); }
                            }
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
                // 接收端各步骤（见 crate::diag）：收齐一帧用了多久、请求了多少重传、解码和显示各花多少
                let assembled = stats.assembled.saturating_sub(window.assembled);
                crate::note!(info, target: "peerdesk::stats",
                    "接收端 收齐 {} 帧，收齐用时 平均 {} ms、最长 {} ms；发出重传请求 {} 片；重复分片 {}，过期分片 {}；\
                     等关键帧跳过 {} 帧；解码 {} ms（{} 次）；交给界面显示 {} ms",
                    assembled,
                    crate::diag::tenth_ms(stats.assembly_us_sum.saturating_sub(window.assembly_us).checked_div(u64::from(assembled)).unwrap_or(0)),
                    crate::diag::tenth_ms(media.take_assembly_max()),
                    stats.nacks_sent.saturating_sub(window.nacks),
                    reassembly.duplicates.saturating_sub(window.duplicates),
                    reassembly.stale.saturating_sub(window.stale),
                    stats.skipped.saturating_sub(window.skipped),
                    window.decode.ms(),
                    window.decode.count(),
                    window.present.ms(),
                );
                let quic = link.quic_stats();
                crate::note!(info, target: "peerdesk::stats", "{}", crate::diag::quic_line(&last_quic, &quic));
                last_quic = quic;
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
                        decoder: decoder_name.lock().map(|n| n.clone()).unwrap_or_default(),
                        fec: fec_name,
                        host: host_stats.clone(),
                    });
                }
                window = StatsWindow {
                    bytes: stats.video_bytes,
                    expired: reassembly.expired,
                    recovered: reassembly.recovered,
                    bad: stats.bad,
                    assembled: stats.assembled,
                    assembly_us: stats.assembly_us_sum,
                    nacks: stats.nacks_sent,
                    skipped: stats.skipped,
                    duplicates: reassembly.duplicates,
                    stale: reassembly.stale,
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
    // 还压在播放缓冲里的画面不再等：最后一幅留给报告
    held.release();
    let last_held = std::iter::from_fn(|| held.pop_due(u64::MAX)).last();
    let last_picture = last_held.map(|(_, picture)| picture).or(last_picture);
    frame_log.flush();

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
        screen: last_picture.and_then(|p| p.to_screen().ok()),
        bad_datagrams,
        input_sent,
        input_unacked: sender.pending(),
        files,
        file_bytes,
        rtt,
        wire_bytes,
        end,
        trace: tracer.dump(),
        first_frame: first_frame.map(Duration::from_micros),
        max_gap: Duration::from_micros(max_gap_us),
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
