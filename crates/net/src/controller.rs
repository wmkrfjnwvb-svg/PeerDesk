//! 控制端的一次会话：握手，接收并重组画面，发送输入，发送文件。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::crypto::Suite;
use peerdesk_core::file::FileMeta;
use peerdesk_core::frame::Picture;
use peerdesk_core::input::{InputEvent, InputSender};
use peerdesk_core::media::{DecodeJob, DecodeWorker, MediaReceiver};
use peerdesk_core::proto::{Caps, Channel, Control};
use peerdesk_core::reaction::{FrameStamp, ReactionTracker, ReactionWindow, Shown};
use peerdesk_core::record::ViewResult;
use peerdesk_core::screen::Screen;
use peerdesk_core::session::{Session, SessionConfig, State};
use peerdesk_core::trace::{Stage, TraceId, Tracer};
use peerdesk_core::video::{TileDecoder, VideoDecoder};
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use crate::files::{send_files, FileResult};
use crate::host_record::{end_reason, wall_now_us};
use crate::stream::{
    control_reader, read_control, write_control, write_kind, StreamKind, HELLO_TIMEOUT,
};
use crate::view_record::{view_frame, ViewJournal, ViewParams};
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
    /// 会话记录（见 [`crate::recorder`]）：`None` 表示不记录。打不开时写日志说明原因，会话照常进行。
    pub record: Option<crate::recorder::ViewRecordSetup>,
    /// H.264 解码器从哪来：`None` 用默认的（桌面：本系统的硬件解码器优先，OpenH264 兜底）；
    /// 手机端传系统的解码器（Android 的 MediaCodec，见 `docs/research/R-android.md`）。
    pub decoder: Option<DecoderFactory>,
}

/// 造 H.264 解码器的函数（见 [`ControllerOptions::decoder`]）。每次会话造一个。
#[derive(Clone)]
pub struct DecoderFactory(
    pub Arc<dyn Fn() -> Result<Box<dyn VideoDecoder>, NetError> + Send + Sync>,
);

impl std::fmt::Debug for DecoderFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecoderFactory")
    }
}

/// 默认的 H.264 解码器：本系统的硬件解码器优先，出错时改用 OpenH264。
#[cfg(feature = "soft")]
fn default_h264_decoder() -> Result<Box<dyn VideoDecoder>, NetError> {
    Ok(Box::new(peerdesk_media::AutoH264Decoder::new()?))
}

/// 没带 OpenH264 的构建（手机端）没有默认解码器：调用方必须通过 [`ControllerOptions::decoder`] 提供，
/// 没提供就明确报错，不静默换成别的编码。
#[cfg(not(feature = "soft"))]
fn default_h264_decoder() -> Result<Box<dyn VideoDecoder>, NetError> {
    Err(NetError::Connect(
        "这个版本没有自带 H.264 解码器，要由调用方提供（手机端用系统解码器）".to_owned(),
    ))
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
    /// 这一秒“操作到画面”的中位数（毫秒）；没有操作时为 `None`。对方支持帧上带输入序号
    /// （[`Caps::INPUT_STAMP`]）时是“发出操作到显示第一幅带上它的画面”，否则是旧算法（见下一项）。
    pub reaction_ms: Option<u32>,
    /// 旧算法：这一秒“按键或点击到下一幅画面”的中位数（毫秒）。画面静止时被控端也在出帧（补清晰），
    /// 下一幅画面多半是操作生效之前采的，所以这个数偏小；留着只为和新算法对照。
    pub reaction_next_frame_ms: Option<u32>,
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
    /// 这一秒的操作到画面：新算法（帧上带输入序号）的各次和旧算法（操作后的下一幅画面）的各次。
    reaction: ReactionWindow,
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
const REACTION_LIMIT_US: u64 = peerdesk_core::reaction::REACTION_LIMIT_US;

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

/// 解码线程交回的结果：帧号、画面、解码耗时（微秒）、收到这一帧第一个分片的时间。
type Decoded = (
    u32,
    Result<Option<Picture>, peerdesk_core::Error>,
    u64,
    peerdesk_core::time::Time,
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
                .send((job.frame_id, result, took, job.first_seen))
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
                return Err(NetError::Connect(
                    crate::reason::reject_text(*reason).to_owned(),
                ));
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
        match &opts.decoder {
            Some(make) => (make.0)()?,
            None => default_h264_decoder()?,
        }
    } else {
        Box::new(TileDecoder::new())
    };
    let decoder_name = Arc::new(std::sync::Mutex::new(decoder.backend()));
    let fec_name = if caps.has(Caps::FEC_RS) { "RS" } else { "XOR" };
    let mut media = MediaReceiver::detached(link.now(), opts.deadline_us);
    media.set_keyframe_grace(caps.common(Caps::H264) == Caps::H264);
    // 双方都支持时在反馈里带到达报告（发送端的网络控制器用）
    media.set_reports(caps.has(Caps::TRANSPORT_CC));
    // 只看 PROBE_ID：控制端拿到的能力里第 24–26 位是被控端的系统，PROBE_TRAIN（第 24 位）在这边读不准
    // （被控端是 Mac 时是 0）。被控端只在自己也支持 PROBE_TRAIN 时才用新格式，见 host.rs
    media.set_probe_ids(caps.has(Caps::PROBE_ID));
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
    // 操作到画面的新算法（见 peerdesk_core::reaction）：对方随帧发“包含到第几个输入”时才有
    let stamped = caps.has(Caps::INPUT_STAMP);
    let mut tracker = ReactionTracker::new();
    let mut last_shown = link.now();
    let session_start = link.now();
    let mut first_frame: Option<u64> = None;
    let mut max_gap_us = 0u64;
    // 会话记录：握手完成、循环开始时打开（见 crate::view_record）。`failed` 是本端自己出的错，
    // `silent` 是对端太久没有任何数据（还没等到连接层报断开），一起决定结束原因。
    let mut journal = opts.record.as_ref().and_then(|setup| {
        let params = ViewParams {
            session_id: link.session_id().ok(),
            caps: caps.0,
            codec: if caps.common(Caps::H264) == Caps::H264 {
                "h264"
            } else {
                "tile"
            },
            wall_start_us: wall_now_us(),
        };
        ViewJournal::open(setup, &params, link.now(), last_quic)
    });
    let (mut failed, mut silent) = (false, false);

    let end = loop {
        if time_up && upload_task.is_none() {
            break "到达时长上限".to_owned();
        }
        tokio::select! {
            () = &mut time_limit, if !time_up => time_up = true,
            // 解码线程交回一帧：显示。
            done = decoded_rx.recv() => match done {
                Some((frame_id, Ok(Some(picture)), took, first_packet)) => {
                    window.decode_us = window.decode_us.max(took);
                    window.decode.add(took);
                    shown_size = picture.size();
                    media.decode_succeeded();
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
                            window.reaction.add_next_frame(took);
                        }
                    }
                    tracer.record(link.now(), trace, Stage::FrameDelivered, u64::from(frame_id));
                    let presenting = std::time::Instant::now();
                    sink.present(&picture);
                    frame_log.shown(frame_id, crate::framelog::unix_us());
                    let present_us = u64::try_from(presenting.elapsed().as_micros()).unwrap_or(u64::MAX);
                    window.present.add(present_us);
                    let shown = Shown { frame_id, first_packet, decoded: now, shown: link.now() };
                    for r in tracker.shown(shown) {
                        window.reaction.add(r);
                    }
                    if let Some(j) = &journal {
                        j.frame(now, view_frame(frame_id, ViewResult::Shown, took, present_us));
                    }
                    last_picture = Some(picture);
                }
                Some((frame_id, Ok(None), took, _)) => {
                    if let Some(j) = &journal {
                        j.frame(link.now(), view_frame(frame_id, ViewResult::NoPicture, took, 0));
                    }
                }
                Some((frame_id, Err(_), took, _)) => {
                    media.decode_failed();
                    if let Some(j) = &journal {
                        j.frame(link.now(), view_frame(frame_id, ViewResult::DecodeFailed, took, 0));
                    }
                }
                None => {
                    failed = true;
                    break "解码线程结束".to_owned();
                }
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
                    rx.tick();
                    // 每 500 ms 把丢包率报告给被控端（它据此决定带不带冗余）
                    audio_ticks = audio_ticks.wrapping_add(1);
                    if audio_ticks.is_multiple_of(crate::audio::REPORT_MS / 10) {
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
                            Ok(jobs) => {
                                if jobs.into_iter().any(|job| decode_jobs.send(job).is_err()) {
                                    failed = true;
                                    break "解码线程结束".to_owned();
                                }
                            }
                            Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                        }
                    }
                    Channel::Input => match <[u8; 4]>::try_from(&payload[..]) {
                        Ok(ack) => sender.ack(u32::from_be_bytes(ack)),
                        Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                    },
                    // 被控端随帧发的“包含到第几个输入”（操作到画面用）
                    Channel::Control if payload.first() == Some(&peerdesk_core::reaction::TAG) => {
                        match FrameStamp::decode(&payload) {
                            Ok(stamp) => {
                                for r in tracker.stamp(stamp) {
                                    window.reaction.add(r);
                                }
                            }
                            Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                        }
                    }
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
                    // 新算法：每次按键、点击、打的字都记序号，等第一幅带上它的画面
                    let counted = peerdesk_core::reaction::counts(&event);
                    sender.push(event);
                    if stamped && counted {
                        tracker.sent(link.now(), sender.next_seq().wrapping_sub(1));
                    }
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
                window.reaction.add_unanswered(tracker.expire(now));
                // 对方随帧发输入序号时用新算法；老版本的对方只能用旧算法，日志里写明
                let (reactions, reaction_label) = if stamped {
                    (window.reaction.totals(), "操作到画面")
                } else {
                    (window.reaction.next_frame(), "操作到画面（旧算法：操作后的下一幅画面，对方版本不带输入序号）")
                };
                crate::note!(info, target: "peerdesk::stats",
                    "接收 帧 {} 画面 {} kbps 最长间隔 {} ms 作废 {} 纠错恢复 {} 坏帧 {} RTT {} ms 输入未确认 {} {} {} 次 中位 {} ms 最大 {} ms",
                    window.frames,
                    stats.video_bytes.saturating_sub(window.bytes) * 8 / 1000,
                    gap / 1000,
                    reassembly.expired.saturating_sub(window.expired),
                    reassembly.recovered.saturating_sub(window.recovered),
                    stats.bad.saturating_sub(window.bad),
                    link.rtt().as_millis(),
                    sender.pending(),
                    reaction_label,
                    reactions.len(),
                    reaction_summary(reactions).0,
                    reaction_summary(reactions).1,
                );
                // 新算法的分段，和旧算法的数放在一起对照
                if let Some(line) = window.reaction.line().filter(|_| stamped) {
                    crate::note!(info, target: "peerdesk::stats", "{line}");
                }
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
                        reaction_ms: (!reactions.is_empty())
                            .then(|| u32::try_from(reaction_summary(reactions).0).unwrap_or(u32::MAX)),
                        reaction_next_frame_ms: (!window.reaction.next_frame().is_empty())
                            .then(|| u32::try_from(reaction_summary(window.reaction.next_frame()).0).unwrap_or(u32::MAX)),
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
                if let Some(j) = journal.as_mut() {
                    j.second(now, quic, input_sent, reactions);
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
                    silent = true;
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
                Some(Control::Bye) => break crate::reason::PEER_ENDED.to_owned(),
                // 控制流断了：网络断开时报网络原因（控制端据此换通道重连），否则是对端结束。
                None => break link.lost_reason().unwrap_or_else(|| crate::reason::PEER_ENDED.to_owned()),
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
    // 对端是因为退出程序而关闭的连接：说清楚，界面显示“对方退出了”，而不是网络错误
    let end = if link.peer_quit() {
        crate::node::END_PEER_QUIT.to_owned()
    } else {
        end
    };
    if let Some(t) = &router_task {
        t.abort();
    }
    frame_log.flush();
    // 会话记录收尾：最后不足一秒的汇总和结束记录；不等记录线程写完（它自己收尾、改名、清理）
    if let Some(j) = journal.take() {
        let reason = end_reason(
            failed,
            link.peer_quit(),
            silent || link.lost_reason().is_some(),
        );
        drop(j.finish(
            link.now(),
            link.quic_stats(),
            input_sent,
            if stamped {
                window.reaction.totals()
            } else {
                window.reaction.next_frame()
            },
            reason,
        ));
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
