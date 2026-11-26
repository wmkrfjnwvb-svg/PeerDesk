//! 被控端的一次会话：握手，按帧率发送画面，接收输入，接收文件。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use peerdesk_core::adaptive::Profile;
use peerdesk_core::auth::{Auth, Limiter};
use peerdesk_core::crypto::Suite;
use peerdesk_core::estimator::EstimatorKind;
use peerdesk_core::file::FileMsg;
use peerdesk_core::frame::Frame;
use peerdesk_core::input::{InputEvent, InputReceiver, KeyState};
use peerdesk_core::media::{encode_job, FrameJob, MediaSender, SenderConfig};
use peerdesk_core::policy::ContentPolicy;
use peerdesk_core::proto::{Caps, Channel, Control, RejectReason};
use peerdesk_core::session::{Session, SessionConfig, State};
use peerdesk_core::trace::{Stage, TraceId, Tracer};
use peerdesk_core::transport::Fec;
use peerdesk_core::video::{EncodedFrame, TileEncoder, VideoEncoder};
use peerdesk_media::H264Encoder;
use tokio::sync::mpsc;

use crate::clipsync::ClipboardHandle;
use peerdesk_core::clipboard::Limits;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::files::{receive_files, FileResult};
use crate::stream::{read_control, read_kind, write_control, write_file_msg, StreamKind};
use crate::{Fingerprint, Link, NetError};

/// 等待对端完成应用层握手的时间上限。
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// 会话结束时等待进行中的文件传输收尾的时间上限。
const FILE_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// 统计日志的间隔。
const STATS_EVERY: Duration = Duration::from_secs(1);

/// 画面来源。屏幕采集实现这个接口；测试和演示用合成桌面。
pub trait ScreenSource: Send {
    /// 取当前画面。返回 `None` 表示没有更多画面，会话结束。
    ///
    /// 真实采集应当在自己的线程里进行，这里只交出最新的一幅，不能阻塞：
    /// 会话循环同时在处理输入，阻塞会让操作变卡。能在显卡上缩放、转颜色的来源交 [`Frame::Gpu`]
    /// （按 [`Self::set_target_size`] 的尺寸），其他交 [`Frame::Rgb`]。
    fn frame(&mut self) -> Option<Frame>;

    /// 原始画面的尺寸（像素，例如 Retina 屏 2880×1800）；不知道时为 `None`（按交来的画面尺寸算）。
    fn native_size(&self) -> Option<(u16, u16)> {
        None
    }

    /// 要按多大采：编码尺寸（发送端按网络和控制端窗口定，见 `peerdesk_core::scale::encode_size`）。
    /// 能在采集时缩放的来源（显卡）照做，之后交来的画面就是这个尺寸；不能的忽略，由编码器缩放。
    fn set_target_size(&mut self, _size: (u16, u16)) {}

    /// 画面内容的变化计数：内容每变一次加一。没变时会话直接跳过编码，省下整屏比较的 CPU。
    /// 返回 `None` 表示来源不知道（合成画面、测试），会话每次都自己比较。
    fn change_seq(&self) -> Option<u64> {
        None
    }

    /// 采集这一秒的情况（帧数、读取耗时、尺寸、格式），诊断日志用；不知道时为 `None`。
    fn stats_line(&mut self) -> Option<String> {
        None
    }
}

/// 起始码率（办公策略的值，为什么见 [`ContentPolicy::OFFICE`]）。
pub const DEFAULT_START_BPS: u64 = ContentPolicy::OFFICE.start_bps;
/// 码率上限（办公策略的值）。
pub const DEFAULT_MAX_BPS: u64 = ContentPolicy::OFFICE.max_bps;
/// 码率下限。
const MIN_BPS: u64 = 100_000;
/// 一帧在接收端从首个分片到收齐允许的时间，也是发送端判定丢失的基准（微秒）。
pub const FRAME_DEADLINE_US: u64 = 100_000;

/// 输入去向。M1 的系统输入注入实现这个接口。
pub trait InputSink: Send {
    /// 注入一个输入事件。
    fn inject(&mut self, event: &InputEvent);
}

/// 控制端被允许做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    /// 是否允许键鼠输入。关闭时只能看。
    pub input: bool,
    /// 是否允许向本机传文件。
    pub files: bool,
    /// 是否同步剪贴板。
    pub clipboard: bool,
    /// 是否允许控制端听本机的声音。
    pub audio: bool,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            input: true,
            files: true,
            clipboard: true,
            audio: true,
        }
    }
}

/// 被控端会话的参数。
#[derive(Debug, Clone)]
pub struct HostOptions {
    /// 握手配置。其中的 `auth` 是要求的密码；`binding` 会被替换成这条连接的通道绑定。
    pub session: SessionConfig,
    /// 这些指纹的设备不需要密码。
    pub trusted: Vec<Fingerprint>,
    /// 权限。
    pub permissions: Permissions,
    /// 认证失败限制，跨会话共享。
    pub limiter: Arc<Mutex<Limiter>>,
    /// 起始码率（比特每秒）。
    pub start_bps: u64,
    /// 码率上限（比特每秒）。
    pub max_bps: u64,
    /// 帧率。
    pub fps: u32,
    /// 收到的文件放在哪个目录。
    pub recv_root: PathBuf,
    /// 本机剪贴板；`None` 表示不同步剪贴板。
    pub clipboard: Option<ClipboardHandle>,
    /// 本机声音来源；`None` 表示不提供声音。
    pub audio: Option<crate::audio::CaptureHandle>,
    /// 本机电脑名：握手时告诉控制端，显示在对方的设备列表里。空表示不发。
    pub name: String,
    /// 发送节奏（实验，见 [`peerdesk_core::pacer`]）。默认关，环境变量 `PEERDESK_PACE=1` 打开。
    pub pacing: bool,
    /// 内容策略（帧率档位、QP 上限、大帧时限等，见 [`peerdesk_core::policy`]）。现在只有办公。
    pub policy: ContentPolicy,
}

impl HostOptions {
    /// 常用默认值：没有免密码的设备，允许输入和文件，新的失败计数。
    #[must_use]
    pub fn new(session: SessionConfig, fps: u32, recv_root: PathBuf) -> Self {
        Self {
            session,
            trusted: Vec::new(),
            permissions: Permissions::default(),
            limiter: Arc::new(Mutex::new(Limiter::default())),
            start_bps: DEFAULT_START_BPS,
            max_bps: DEFAULT_MAX_BPS,
            fps,
            recv_root,
            clipboard: None,
            audio: None,
            name: String::new(),
            pacing: peerdesk_core::pacer::enabled(),
            policy: ContentPolicy::OFFICE,
        }
    }
}

/// 把发送队列里现在可以发的视频分片（和探测包）发出去。节奏器是唯一出口：发送速率由网络控制器给
/// （老的估计器按 `PEERDESK_PACE` 开关）。每个发出的包记进发送历史，等到达报告对账。
fn send_paced(
    link: &Link,
    pacer: &mut peerdesk_core::pacer::Pacer,
    pacing: bool,
    media: &mut MediaSender,
) -> Result<(), NetError> {
    for o in pacer.pop_ready(link.now(), media.pacing_bps(pacing)) {
        let seq = link.send_flags(Channel::Video, o.flags, &o.body)?;
        media.on_packet_sent(link.now(), seq, &o);
    }
    Ok(())
}

/// 网络控制器的定时任务间隔（WebRTC GoogCC 的 `kUpdateIntervalMs`）。
const CC_TICK: Duration = Duration::from_millis(25);

/// 被控端会话的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReport {
    /// 对端指纹。
    pub peer: Fingerprint,
    /// 选中的加密套件。
    pub suite: Option<Suite>,
    /// 发出的帧数。
    pub frames_sent: u32,
    /// 发出的画面字节数（编码后，不含包头和纠错）。
    pub video_bytes: u64,
    /// 被判定丢失而重新发送内容的帧数。
    pub frames_repaired: u32,
    /// 会话结束时的码率（比特每秒）。
    pub final_rate_bps: u64,
    /// 使用的码率控制参数名。
    pub profile: &'static str,
    /// 注入的输入事件数。
    pub input_events: u64,
    /// 因为没有权限而被丢弃的输入事件数。
    pub input_denied: u64,
    /// 无法解析而被丢弃的数据报数。
    pub bad_datagrams: u64,
    /// 收到的文件。
    pub files: Vec<FileResult>,
    /// 会话为什么结束。
    pub end: String,
    /// 业务追踪。
    pub trace: String,
}

pub(crate) fn control_reader(mut recv: quinn::RecvStream) -> mpsc::Receiver<Control> {
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        while let Ok(msg) = read_control(&mut recv).await {
            if tx.send(msg).await.is_err() {
                break;
            }
        }
    });
    rx
}

/// 编码线程的任务：一帧任务、画面、画面是否变了。
type EncodeJob = (FrameJob, Frame, bool);
/// 编码线程交回的结果。
type EncodeDone = (
    FrameJob,
    Result<(Option<EncodedFrame>, bool), peerdesk_core::Error>,
    u64,
    u64,
    peerdesk_core::video::EncodeTiming,
);

/// 启动编码线程。编码要好几毫秒到几十毫秒，放在会话循环里会让输入和反馈排队等它。
/// 任务通道关闭（会话结束）时线程退出。
fn spawn_encoder(
    mut encoder: Box<dyn VideoEncoder>,
    backend: Arc<Mutex<String>>,
) -> (
    std::sync::mpsc::Sender<EncodeJob>,
    mpsc::UnboundedReceiver<EncodeDone>,
) {
    let (jobs, job_rx) = std::sync::mpsc::channel::<EncodeJob>();
    let (done_tx, done_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok((job, frame, changed)) = job_rx.recv() {
            let started = std::time::Instant::now();
            let result = encode_job(encoder.as_mut(), &job, &frame, changed);
            let took = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
            // 编码器可能中途从硬件换成软件：每帧记下现在用的是哪个
            let name = encoder.backend();
            if let Ok(mut b) = backend.lock() {
                if *b != name {
                    *b = name;
                }
            }
            let convert = encoder.convert_micros();
            let timing = encoder.timing();
            if done_tx.send((job, result, took, convert, timing)).is_err() {
                break;
            }
        }
    });
    (jobs, done_rx)
}

/// 在一条已建立的连接上运行被控端会话，直到任一方结束。
#[allow(clippy::too_many_lines)]
pub async fn serve(
    link: Link,
    opts: HostOptions,
    mut source: Box<dyn ScreenSource>,
    mut sink: Box<dyn InputSink>,
) -> Result<HostReport, NetError> {
    if opts.fps == 0 {
        return Err(NetError::Protocol("帧率不能为 0"));
    }
    let link = Arc::new(link);
    let trace = TraceId(opts.session.device_id);
    let mut tracer = Tracer::new(4096);
    // 测量用的逐帧时间日志（PEERDESK_FRAME_LOG，默认关）
    let mut frame_log = crate::framelog::FrameLog::from_env();
    let mut config = opts.session.clone();
    // 实验开关：PEERDESK_ESTIMATOR=legacy、v1 或 gcc 选估计器，默认用产品默认的那个。
    let wanted = std::env::var("PEERDESK_ESTIMATOR")
        .ok()
        .and_then(|name| EstimatorKind::parse(&name))
        .unwrap_or(EstimatorKind::PRODUCT);
    // 到达报告（TRANSPORT_CC）只在本端的估计器要用时才声明：协商出来就是双方都用 v2 反馈格式，
    // 没协商出来双方都用原来的格式。不能“声明了却不用”：控制端会发 v2 反馈，这里按老格式解不了。
    if wanted != EstimatorKind::GccV1 {
        config.caps = Caps(config.caps.0 & !Caps::TRANSPORT_CC.0);
    }
    config.binding = link.channel_binding()?;
    if opts.trusted.contains(link.peer_fingerprint()) {
        config.auth = Auth::Open;
    }
    let needs_password = matches!(config.auth, Auth::Password(_));
    let mut session = Session::host(config, trace);
    session.set_name(&opts.name);

    // 应用层握手：对端先打开控制流并发 Hello。
    let (mut ctrl_send, mut ctrl_recv) = tokio::time::timeout(HELLO_TIMEOUT, link.accept_stream())
        .await
        .map_err(|_| NetError::Timeout("等待控制流"))??;
    if read_kind(&mut ctrl_recv).await? != StreamKind::Control {
        link.close("protocol");
        return Err(NetError::Protocol("第一条流必须是控制流"));
    }
    let locked = !opts
        .limiter
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .allowed(link.now_wall());
    if needs_password && locked {
        // 失败次数过多：不进入密码交换，直接拒绝。
        let reject = Control::Reject {
            reason: RejectReason::Locked,
        };
        let _ = write_control(&mut ctrl_send, &reject).await;
        let _ = ctrl_send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(1), link.closed()).await;
        link.close("locked");
        return Err(NetError::Connect("认证失败次数过多，暂时锁定".into()));
    }
    let (suite, caps) = loop {
        let msg = tokio::time::timeout(HELLO_TIMEOUT, read_control(&mut ctrl_recv))
            .await
            .map_err(|_| NetError::Timeout("等待握手消息"))??;
        for reply in session.on_control(link.now(), &msg, &mut tracer)? {
            write_control(&mut ctrl_send, &reply).await?;
        }
        match session.state() {
            State::Established { suite, caps, .. } => break (*suite, *caps),
            State::Idle | State::HelloSent | State::AwaitAuth => {}
            State::Rejected(reason) => {
                if needs_password && *reason == RejectReason::Unauthorized {
                    let mut limiter = opts.limiter.lock().unwrap_or_else(PoisonError::into_inner);
                    limiter.failure(link.now_wall());
                }
                let _ = ctrl_send.finish();
                // 让拒绝消息送达后再关闭。
                let _ = tokio::time::timeout(Duration::from_secs(1), link.closed()).await;
                link.close("rejected");
                return Err(NetError::Connect(format!("会话未建立：{reason:?}")));
            }
            State::Closed => {
                link.close("closed");
                return Err(NetError::Connect("对端在握手阶段结束了会话".into()));
            }
        }
    };
    if needs_password {
        opts.limiter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .success();
    }
    let mut ctrl_rx = control_reader(ctrl_recv);

    let mut ticker = tokio::time::interval(Duration::from_micros(1_000_000 / u64::from(opts.fps)));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut receiver = InputReceiver::new();
    let mut keys = KeyState::new();
    let mut file_tasks: Vec<JoinHandle<Result<Vec<FileResult>, NetError>>> = Vec::new();
    // 会话结束时通知剪贴板同步停下（它的流要等连接关闭才会断）。
    let (stop_clip, clip_stopped) = tokio::sync::watch::channel(false);
    // 剪贴板：双方都支持、本机允许并且有剪贴板时才同步。
    let clipboard = opts
        .clipboard
        .clone()
        .filter(|_| opts.permissions.clipboard && caps.has(Caps::CLIPBOARD));
    // 控制端显示画面的区域（宽 << 16 | 高；0 表示不限），由“画面区域”流更新（见 StreamKind::View）
    let view = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let view_ok = caps.has(Caps::VIEW_SIZE);
    let audio = opts
        .audio
        .clone()
        .filter(|_| opts.permissions.audio && caps.has(Caps::AUDIO));
    let (mut input_events, mut bad_datagrams) = (0u64, 0u64);
    let mut input_denied = 0u64;
    let rtt_us = u64::try_from(link.rtt().as_micros()).unwrap_or(u64::MAX);
    // 两端都支持 H.264 就用它，否则用块编码。
    let encoder: Box<dyn VideoEncoder> = if caps.common(Caps::H264) == Caps::H264 {
        Box::new(H264Encoder::with_max_bps(opts.fps, opts.max_bps).with_policy(&opts.policy))
    } else {
        Box::new(TileEncoder::new())
    };
    let codec = encoder.codec();
    let backend = Arc::new(Mutex::new(String::new()));
    let (jobs, encoded_rx) = spawn_encoder(encoder, Arc::clone(&backend));
    // 详情面板：对方支持时每秒发一次本机状态；本机到路由器的往返在后台测
    let send_stats = caps.has(Caps::HOST_STATS);
    let router_ms = Arc::new(std::sync::atomic::AtomicU32::new(u32::MAX));
    let router_task =
        send_stats.then(|| tokio::spawn(crate::gateway::watch_router(Arc::clone(&router_ms))));
    let mut screen_size = (0u16, 0u16);
    // 要到达报告的估计器（gcc）对端也得支持，老版本对端退回 v1。
    let estimator = if wanted == EstimatorKind::GccV1 && !caps.has(Caps::TRANSPORT_CC) {
        crate::note!(info, "对方版本不支持到达报告，码率估计改用 classic-v1");
        EstimatorKind::ClassicV1
    } else {
        wanted
    };
    let mut media = MediaSender::detached(
        SenderConfig {
            fps: opts.fps,
            start_bps: opts.start_bps,
            min_bps: MIN_BPS,
            max_bps: opts.max_bps,
            profile: Profile::select(rtt_us, false, false),
            deadline_us: FRAME_DEADLINE_US,
            max_payload: link.max_payload(),
            estimator,
            policy: opts.policy,
        },
        codec,
    );
    crate::note!(info, "内容策略：{}", opts.policy.name);
    media.set_reports(caps.has(Caps::TRANSPORT_CC));
    // 音画同步：对方也支持时，画面帧和声音包都带会话时钟的采集时刻（控制端打不打开播放缓冲由它自己定）
    let av_sync = caps.has(Caps::AV_SYNC);
    media.set_capture_stamps(av_sync);
    // 画面静止时定期探测链路（对端能测填充包的包串速率时）
    media.set_idle_probe(caps.has(Caps::PROBE_TRAIN));
    // 双方都支持 RS 纠错时用它（老版本只认异或）
    media.set_fec(if caps.has(Caps::FEC_RS) {
        Fec::Rs
    } else {
        Fec::Xor
    });
    let mut encoded_rx = encoded_rx;
    // 判断画面变没变：来源会报告变化计数时用它；否则和上一幅比较。
    let mut last_seq: Option<u64> = None;
    let mut last_screen: Option<Frame> = None;
    // 告诉采集端的编码尺寸（变了才再告诉）
    let mut target_size: Option<(u16, u16)> = None;
    // 编码线程正在编一帧时不再派新的；这期间画面变了先记下，编完再编最新的那一幅（不排队）。
    let mut encoding = false;
    let mut pending_change = false;

    // 每秒一行统计写进日志，用户发日志就能看到真实效果。
    let mut stats_tick = tokio::time::interval(STATS_EVERY);
    // 发送节奏（实验，默认关闭：关闭时队列每次一下发空，和原来一样）。见 peerdesk_core::pacer。
    let pacing = opts.pacing;
    let mut pacer = peerdesk_core::pacer::Pacer::new(link.now());
    let mut pace_tick = tokio::time::interval(Duration::from_millis(2));
    let mut cc_tick = tokio::time::interval(CC_TICK);
    cc_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    pace_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    stats_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut last_stats = media.stats();
    let mut last_encode_us = 0u64;
    let mut encode_window = crate::diag::EncodeWindow::default();
    let mut last_budget = 0usize;
    let mut last_quic = link.quic_stats();
    // 这一秒里编码和颜色转换的总耗时、帧数（统计行用平均值，单帧的值跳得厉害）
    let mut window = (0u64, 0u64, 0u64);

    let end = loop {
        tokio::select! {
            // 按顺序检查：先处理收到的数据报（输入、反馈）和控制消息，最后才采集、编码下一帧。
            // 编码可能要好几毫秒；不这样排，编码期间到达的输入要排在后面。
            biased;
            datagram = link.recv() => match datagram {
                Ok((header, payload)) => match header.channel {
                    Channel::Input => match receiver.on_packet(&payload) {
                        Ok(events) => {
                            let count = u64::try_from(events.len()).unwrap_or(u64::MAX);
                            if opts.permissions.input {
                                for event in &events {
                                    keys.apply(event);
                                    sink.inject(event);
                                }
                                input_events = input_events.saturating_add(count);
                            } else {
                                // 没有输入权限：丢弃，但照常确认，免得对端一直重发。
                                input_denied = input_denied.saturating_add(count);
                            }
                            let ack = receiver.next_expected().to_be_bytes();
                            if let Err(e) = link.send(Channel::Input, &ack) { break e.to_string(); }
                        }
                        Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                    },
                    // 控制通道的数据报是观看端的反馈：哪些帧到了、链路有没有积压。
                    Channel::Control => match media.on_feedback_datagram(link.now(), &payload) {
                        Ok(again) => {
                            // 已经有定论的帧（送到了、或接收端放弃了）的包不再发
                            pacer.retain(|o| media.wanted(o));
                            pacer.push_front(again);
                            if let Err(e) = send_paced(&link, &mut pacer, pacing, &mut media) { break e.to_string(); }
                        }
                        Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                    },
                    Channel::Cursor | Channel::Video | Channel::Clipboard | Channel::File | Channel::Audio => {
                        bad_datagrams = bad_datagrams.saturating_add(1);
                    }
                },
                Err(NetError::Closed(why)) => break why,
                Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
            },
            stream = link.accept_stream() => match stream {
                Ok((mut send, mut recv)) => {
                    let root = opts.recv_root.clone();
                    let allowed = opts.permissions.files;
                    let clipboard = clipboard.clone();
                    let files = caps.has(Caps::CLIPBOARD_FILES);
                    let audio = audio.clone();
                    let view = Arc::clone(&view);
                    let link = Arc::clone(&link);
                    let mut clip_stopped = clip_stopped.clone();
                    file_tasks.push(tokio::spawn(async move {
                        match read_kind(&mut recv).await? {
                            StreamKind::File if !allowed => {
                                let abort = FileMsg::Abort { reason: "file transfer not permitted".into() };
                                let _ = write_file_msg(&mut send, &abort).await;
                                let _ = send.finish();
                                Err(NetError::Protocol("没有文件传输权限"))
                            }
                            StreamKind::File => receive_files(send, recv, &root).await,
                            StreamKind::Clipboard => {
                                let Some(clipboard) = clipboard else {
                                    return Err(NetError::Protocol("没有协商剪贴板，或者本机不允许"));
                                };
                                tokio::select! {
                                    r = crate::clipsync::run(send, recv, clipboard.0, Limits::DEFAULT, files) => r?,
                                    _ = clip_stopped.wait_for(|stop| *stop) => {}
                                }
                                Ok(Vec::new())
                            }
                            StreamKind::Audio => {
                                let Some(audio) = audio else {
                                    return Err(NetError::Protocol("没有协商声音，或者本机不允许"));
                                };
                                // 声音流开着就一直发：控制端关掉声音（流结束）或会话结束时停
                                let stopped = async move {
                                    let _ = clip_stopped.wait_for(|stop| *stop).await;
                                };
                                crate::audio::send(&link, audio.0.as_ref(), recv, stopped, av_sync).await?;
                                drop(send);
                                Ok(Vec::new())
                            }
                            StreamKind::View if !view_ok => {
                                Err(NetError::Protocol("没有协商画面区域"))
                            }
                            // 控制端窗口大小：每次 4 字节，流结束（控制端走了）就停
                            StreamKind::View => {
                                let mut buf = [0u8; 4];
                                while recv.read_exact(&mut buf).await.is_ok() {
                                    view.store(u32::from_be_bytes(buf), std::sync::atomic::Ordering::Relaxed);
                                }
                                drop(send);
                                Ok(Vec::new())
                            }
                            // 文件管理：和文件传输用同一个权限开关
                            StreamKind::Browse => {
                                crate::browse::serve(send, recv, allowed, &crate::files::Progress::default()).await
                            }
                            StreamKind::Control | StreamKind::Signal | StreamKind::Crash => {
                                Err(NetError::Protocol("会话里不该出现这种流"))
                            }
                        }
                    }));
                }
                Err(e) => break e.to_string(),
            },
            msg = ctrl_rx.recv() => match msg {
                Some(Control::Ping { nonce }) => {
                    let pong = write_control(&mut ctrl_send, &Control::Pong { nonce }).await;
             if let Err(e) = pong { break e.to_string(); }
                }
                Some(Control::Bye) => break "对端结束会话".to_owned(),
                // 控制流断了：网络断开时报网络原因（控制端据此换通道重连），否则是对端结束。
                None => break link.lost_reason().unwrap_or_else(|| "对端结束会话".to_owned()),
                Some(
                    Control::Hello { .. }
                    | Control::Accept { .. }
                    | Control::Reject { .. }
                    | Control::Pong { .. }
                    | Control::AuthStart { .. }
                    | Control::AuthProof { .. },
                ) => {}
            },
            // 编码线程交回一帧：切片发出。
            done = encoded_rx.recv() => {
                encoding = false;
                let (job, result, took, convert, timing) = match done {
                    Some(done) => done,
                    None => break "编码线程结束".to_owned(),
                };
                media.set_encode_time(took, timing.hardware);
                last_encode_us = took;
                window = (window.0.saturating_add(took), window.1.saturating_add(convert), window.2 + 1);
                encode_window.add(took, &timing, result.as_ref().ok().and_then(|(e, _)| e.as_ref()).map(|e| e.payload.len()));
                last_budget = job.budget;
                media.set_max_payload(link.max_payload());
                let out = match result.and_then(|(encoded, refine)| media.finish_frame(link.now(), &job, encoded, refine)) {
                    Ok(out) => out,
                    Err(e) => break e.to_string(),
                };
                if out.is_empty() {
                    continue;
                }
                pacer.push(out);
                if let Err(e) = send_paced(&link, &mut pacer, pacing, &mut media) { break e.to_string(); }
                tracer.record(link.now(), trace, Stage::FrameSent, u64::from(media.stats().frames));
            }
            _ = cc_tick.tick() => {
                // 网络控制器的定时任务：应用受限的预算、定期探测；要探测时交给节奏器发
                media.on_tick(link.now());
                for cluster in media.take_probes() {
                    pacer.push_probe(cluster);
                }
                if let Err(e) = send_paced(&link, &mut pacer, pacing, &mut media) { break e.to_string(); }
            }
            _ = pace_tick.tick(), if !pacer.is_empty() => {
                if let Err(e) = send_paced(&link, &mut pacer, pacing, &mut media) { break e.to_string(); }
            }
            _ = stats_tick.tick() => {
                let now = media.stats();
                // 这一秒的平均编码耗时；这一秒没编码（画面静止）时沿用上一帧的
                let (encode_us, convert_us) = match window {
                    (sum, conv, n) if n > 0 => (sum / n, conv / n),
                    _ => (last_encode_us, 0),
                };
                window = (0, 0, 0);
                crate::note!(info, target: "peerdesk::stats",
                    "发送 帧 {} 关键帧 {} 画面 {} kbps 线上 {} kbps 目标 {} kbps 帧率上限 {} 编码 {} ms（其中颜色转换 {} ms） 在途 {} KB 丢失 {} 重传 {} RTT {} ms 估计器 {} 纠错 {}‰ 画像 {}",
                    now.frames.saturating_sub(last_stats.frames),
                    now.keyframes.saturating_sub(last_stats.keyframes),
                    now.video_bytes.saturating_sub(last_stats.video_bytes) * 8 / 1000,
                    now.wire_bytes.saturating_sub(last_stats.wire_bytes) * 8 / 1000,
                    media.rate_bps() / 1000,
                    media.fps(),
                    encode_us / 1000,
                    convert_us / 1000,
                    media.in_flight() / 1024,
                    now.lost.saturating_sub(last_stats.lost),
                    now.retransmitted.saturating_sub(last_stats.retransmitted),
                    link.rtt().as_millis(),
                    media.estimator(),
                    media.fec_permille(),
                    media.profile_name(),
                );
                // 各模块这一秒的细节（见 crate::diag）：采集、编码、发送端、码率估计、QUIC，加上各模块的决定
                if let Some(line) = source.stats_line() {
                    crate::note!(info, target: "peerdesk::stats", "{line}");
                }
                if let Some(line) = encode_window.take_line() {
                    crate::note!(info, target: "peerdesk::stats", "{line}");
                }
                crate::note!(info, target: "peerdesk::stats",
                    "发送端 每帧预算 {} KB，在途 {} KB，排队待发 {} KB；收到重传请求 {} 片，重传 {} 片，\
                     没重传（刚重传过、可能还在路上）{} 片，没重传（这轮额度用完）{} 片；判定丢帧 {}",
                    last_budget / 1024,
                    media.in_flight() / 1024,
                    pacer.queued_bytes() / 1024,
                    now.nack_requests.saturating_sub(last_stats.nack_requests),
                    now.retransmitted.saturating_sub(last_stats.retransmitted),
                    now.resend_skipped.saturating_sub(last_stats.resend_skipped),
                    now.resend_capped.saturating_sub(last_stats.resend_capped),
                    now.lost.saturating_sub(last_stats.lost),
                );
                if let Some(line) = media.controller_line() {
                    crate::note!(info, target: "peerdesk::stats", "{line}");
                } else if let Some(o) = media.last_observation() {
                    crate::note!(info, target: "peerdesk::stats",
                        "码率估计 最近反馈：排队 {} ms，延迟趋势 {} ms，丢包 {}‰，送达 {} kbps，包串测速 {} kbps",
                        o.feedback.queue_delay_us / 1000,
                        crate::diag::signed_tenths(o.ext.trend_tenth_ms),
                        o.feedback.loss_permille,
                        u64::from(o.feedback.recv_bytes_per_s) * 8 / 1000,
                        u64::from(o.ext.train_bytes_per_s) * 8 / 1000,
                    );
                }
                let quic = link.quic_stats();
                crate::note!(info, target: "peerdesk::stats", "{}", crate::diag::quic_line(&last_quic, &quic));
                last_quic = quic;
                for event in media.take_events() {
                    crate::note!(info, target: "peerdesk::stats", "决定 {event}");
                }
                if send_stats {
                    let stats = peerdesk_core::stats::HostStats {
                        encoder: backend.lock().map(|b| b.clone()).unwrap_or_default(),
                        width: screen_size.0,
                        height: screen_size.1,
                        fps: u16::try_from(now.frames.saturating_sub(last_stats.frames)).unwrap_or(u16::MAX),
                        fps_cap: u16::try_from(media.fps()).unwrap_or(u16::MAX),
                        encode_ms: u16::try_from(encode_us / 1000).unwrap_or(u16::MAX),
                        target_kbps: u32::try_from(media.rate_bps() / 1000).unwrap_or(u32::MAX),
                        sent_kbps: u32::try_from(now.video_bytes.saturating_sub(last_stats.video_bytes) * 8 / 1000).unwrap_or(u32::MAX),
                        fec_permille: media.fec_permille(),
                        profile: media.profile_name().to_owned(),
                        router_ms: u16::try_from(router_ms.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(peerdesk_core::stats::UNKNOWN),
                    };
                    if let Ok(bytes) = stats.encode() {
                        let _ = link.send(Channel::Control, &bytes);
                    }
                }
                last_stats = now;
            }
            _ = ticker.tick() => {
                // 先取变化计数再取画面：中间变了的话，下一帧会再编一次，不会漏。
                let seq = source.change_seq();
                let Some(frame) = source.frame() else {
                    // 画面来源结束：正在编的那一帧编完发出去再结束。
                    let last = if encoding { encoded_rx.recv().await } else { None };
                    let last = last.and_then(|(job, r, _, _, _)| r.ok().map(|(e, f)| (job, e, f)));
                    media.set_max_payload(link.max_payload());
                    let out = last.map(|(job, e, f)| media.finish_frame(link.now(), &job, e, f).unwrap_or_default());
                    out.unwrap_or_default().iter().for_each(|o| drop(link.send_flags(Channel::Video, o.flags, &o.body)));
                    break "画面来源结束".to_owned();
                };
                screen_size = frame.size();
                let captured = crate::framelog::unix_us();
                let changed = match seq {
                    Some(seq) => last_seq.replace(seq) != Some(seq),
                    None => {
                        let changed = !last_screen.as_ref().is_some_and(|l| l.same_content(&frame));
                        if changed {
                            last_screen = Some(frame.clone());
                        }
                        changed
                    }
                };
                pending_change |= changed;
                if changed {
                    media.screen_changed(link.now());
                    frame_log.changed(captured);
                }
                // 每一轮都检查迟迟没有确认的帧（不能只在编帧时查：关键帧没确认时根本不编帧）
                media.expire_unacked(link.now());
                // 控制端窗口大小（变了的话画面不动也要按新尺寸编一帧）
                let packed = view.load(std::sync::atomic::Ordering::Relaxed);
                media.set_view(Some((
                    u16::try_from(packed >> 16).unwrap_or(0),
                    u16::try_from(packed & 0xffff).unwrap_or(0),
                )));
                // 编码尺寸告诉采集端：能在显卡上缩放的来源之后就按它采（变了才告诉）
                let target = peerdesk_core::scale::even_size(
                    media.encode_size(source.native_size().unwrap_or(screen_size)),
                );
                if target_size.replace(target) != Some(target) {
                    source.set_target_size(target);
                }
                // 在途数据超过上限：这一帧不编，变化攒着，等确认之后编最新的画面。
                if encoding || pacer.busy(media.pacing_bps(pacing), 1_000_000 / u64::from(opts.fps.max(1))) || media.in_flight_full(link.now()) || !media.due(link.now()) || (!pending_change && !media.wants_frame()) {
                    continue;
                }
                let rtt = u64::try_from(link.rtt().as_micros()).unwrap_or(u64::MAX);
                let job = media.start_frame(link.now(), rtt);
                frame_log.encoded(job.frame_id, captured);
                if jobs.send((job, frame, std::mem::take(&mut pending_change))).is_err() {
                    break "编码线程结束".to_owned();
                }
                encoding = true;
            }
        }
    };

    // 无论会话怎么结束，都把还按着的键和鼠标按键释放掉。
    for event in keys.release_all() {
        sink.inject(&event);
    }
    frame_log.flush();
    let bye = session.close(link.now(), &mut tracer);
    let _ = write_control(&mut ctrl_send, &bye).await;
    let _ = ctrl_send.finish();

    let _ = stop_clip.send(true);
    if let Some(t) = &router_task {
        t.abort();
    }
    let mut files = Vec::new();
    for task in file_tasks {
        if let Ok(Ok(Ok(results))) = tokio::time::timeout(FILE_DRAIN_TIMEOUT, task).await {
            files.extend(results);
        }
    }
    link.close("bye");
    Ok(HostReport {
        peer: *link.peer_fingerprint(),
        suite: Some(suite),
        frames_sent: media.stats().frames,
        video_bytes: media.stats().video_bytes,
        frames_repaired: media.stats().lost,
        final_rate_bps: media.rate_bps(),
        profile: media.profile().name,
        input_events,
        input_denied,
        bad_datagrams,
        files,
        end,
        trace: tracer.dump(),
    })
}
