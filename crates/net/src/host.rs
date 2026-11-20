//! 被控端的一次会话：握手，按帧率发送画面，接收输入，接收文件。

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use peerdesk_core::adaptive::{Feedback, Profile};
use peerdesk_core::auth::{Auth, Limiter};
use peerdesk_core::crypto::Suite;
use peerdesk_core::file::FileMsg;
use peerdesk_core::input::{InputEvent, InputReceiver, KeyState};
use peerdesk_core::media::{MediaSender, SenderConfig};
use peerdesk_core::proto::{Caps, Channel, Control, RejectReason};
use peerdesk_core::screen::Screen;
use peerdesk_core::session::{Session, SessionConfig, State};
use peerdesk_core::trace::{Stage, TraceId, Tracer};
use peerdesk_core::video::{TileEncoder, VideoEncoder};
use peerdesk_media::H264Encoder;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use crate::files::{receive_files, FileResult};
use crate::stream::{read_control, read_kind, write_control, write_file_msg, StreamKind};
use crate::{Fingerprint, Link, NetError};

/// 等待对端完成应用层握手的时间上限。
pub(crate) const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
/// 会话结束时等待进行中的文件传输收尾的时间上限。
const FILE_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// 画面来源。屏幕采集实现这个接口；测试和演示用合成桌面。
pub trait ScreenSource: Send {
    /// 取当前画面。返回 `None` 表示没有更多画面，会话结束。
    ///
    /// 真实采集应当在自己的线程里进行，这里只交出最新的一幅，不能阻塞：
    /// 会话循环同时在处理输入，阻塞会让操作变卡。
    fn capture(&mut self) -> Option<&Screen>;

    /// 画面内容的变化计数：内容每变一次加一。没变时会话直接跳过编码，省下整屏比较的 CPU。
    /// 返回 `None` 表示来源不知道（合成画面、测试），会话每次都自己比较。
    fn change_seq(&self) -> Option<u64> {
        None
    }
}

/// 起始码率：没有历史数据时从这里起步。
pub const DEFAULT_START_BPS: u64 = 4_000_000;
/// 码率上限。
pub const DEFAULT_MAX_BPS: u64 = 30_000_000;
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
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            input: true,
            files: true,
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
        }
    }
}

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
    let mut config = opts.session.clone();
    config.binding = link.channel_binding()?;
    if opts.trusted.contains(link.peer_fingerprint()) {
        config.auth = Auth::Open;
    }
    let needs_password = matches!(config.auth, Auth::Password(_));
    let mut session = Session::host(config, trace);

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
    let (mut input_events, mut bad_datagrams) = (0u64, 0u64);
    let mut input_denied = 0u64;
    let rtt_us = u64::try_from(link.rtt().as_micros()).unwrap_or(u64::MAX);
    // 两端都支持 H.264 就用它，否则用块编码。
    let encoder: Box<dyn VideoEncoder> = if caps.common(Caps::H264) == Caps::H264 {
        Box::new(H264Encoder::with_max_bps(opts.fps, opts.max_bps))
    } else {
        Box::new(TileEncoder::new())
    };
    let mut media = MediaSender::new(
        SenderConfig {
            fps: opts.fps,
            start_bps: opts.start_bps,
            min_bps: MIN_BPS,
            max_bps: opts.max_bps,
            profile: Profile::select(rtt_us, false, false),
            deadline_us: FRAME_DEADLINE_US,
            max_payload: link.max_payload(),
        },
        encoder,
    );
    // 判断画面变没变：来源会报告变化计数时用它；否则和上一幅比较。
    let mut last_seq: Option<u64> = None;
    let mut last_screen: Option<Screen> = None;

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
                    Channel::Control => match Feedback::decode(&payload) {
                        Ok(feedback) => media.on_feedback(link.now(), &feedback),
                        Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                    },
                    Channel::Cursor | Channel::Video | Channel::Clipboard | Channel::File => {
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
                    file_tasks.push(tokio::spawn(async move {
                        match read_kind(&mut recv).await? {
                            StreamKind::File if !allowed => {
                                let abort = FileMsg::Abort { reason: "file transfer not permitted".into() };
                                let _ = write_file_msg(&mut send, &abort).await;
                                let _ = send.finish();
                                Err(NetError::Protocol("没有文件传输权限"))
                            }
                            StreamKind::File => receive_files(send, recv, &root).await,
                            StreamKind::Control | StreamKind::Signal => {
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
                Some(Control::Bye) | None => break "对端结束会话".to_owned(),
                Some(
                    Control::Hello { .. }
                    | Control::Accept { .. }
                    | Control::Reject { .. }
                    | Control::Pong { .. }
                    | Control::AuthStart { .. }
                    | Control::AuthProof { .. },
                ) => {}
            },
            _ = ticker.tick() => {
                // 先取变化计数再取画面：中间变了的话，下一帧会再编一次，不会漏。
                let seq = source.change_seq();
                let Some(screen) = source.capture() else {
                    break "画面来源结束".to_owned();
                };
                let changed = match seq {
                    Some(seq) => last_seq.replace(seq) != Some(seq),
                    None => {
                        let changed = last_screen.as_ref() != Some(screen);
                        if changed {
                            last_screen = Some(screen.clone());
                        }
                        changed
                    }
                };
                if !changed && !media.wants_frame() {
                    continue;
                }
                let rtt = u64::try_from(link.rtt().as_micros()).unwrap_or(u64::MAX);
                let out = match media.on_frame(link.now(), screen, changed, rtt) {
                    Ok(out) => out,
                    Err(e) => break e.to_string(),
                };
                if out.is_empty() {
                    continue;
                }
                let sent = out
                    .iter()
                    .try_for_each(|o| link.send_flags(Channel::Video, o.flags, &o.body));
                if let Err(e) = sent { break e.to_string(); }
                tracer.record(link.now(), trace, Stage::FrameSent, u64::from(media.stats().frames));
            }
        }
    };

    // 无论会话怎么结束，都把还按着的键和鼠标按键释放掉。
    for event in keys.release_all() {
        sink.inject(&event);
    }
    let bye = session.close(link.now(), &mut tracer);
    let _ = write_control(&mut ctrl_send, &bye).await;
    let _ = ctrl_send.finish();

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
