//! 控制端的一次会话：握手，接收并重组画面，发送输入，发送文件。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use peerdesk_core::adaptive::{DelayTracker, DeliveryLog, Feedback};
use peerdesk_core::codec::{frame_size, Decoder};
use peerdesk_core::crypto::Suite;
use peerdesk_core::desktop_sim::FLAG_PARITY;
use peerdesk_core::file::FileMeta;
use peerdesk_core::input::{InputEvent, InputSender};
use peerdesk_core::proto::{Channel, Control, RejectReason, VideoHeader};
use peerdesk_core::screen::Screen;
use peerdesk_core::session::{Session, SessionConfig, State};
use peerdesk_core::trace::{Stage, TraceId, Tracer};
use peerdesk_core::transport::{Reassembler, VideoPacket};
use peerdesk_core::wire::Reader;
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
}

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

    let (mut ctrl_send, mut ctrl_recv) = link.open_stream().await?;
    write_kind(&mut ctrl_send, StreamKind::Control).await?;
    let hello = session.hello(link.now(), &mut tracer)?;
    write_control(&mut ctrl_send, &hello).await?;
    let suite = loop {
        let msg = tokio::time::timeout(HELLO_TIMEOUT, read_control(&mut ctrl_recv))
            .await
            .map_err(|_| NetError::Timeout("等待握手应答"))??;
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
            State::Established { suite, .. } => break *suite,
            State::HelloSent | State::AwaitAuth => {}
            State::Rejected(reason) => {
                link.close("rejected");
                return Err(NetError::Connect(match reason {
                    RejectReason::Unauthorized => "密码错误，或被控端需要密码".to_owned(),
                    RejectReason::Locked => "被控端因多次认证失败暂时锁定".to_owned(),
                    RejectReason::Busy => "被控端正在被其他设备控制".to_owned(),
                    RejectReason::Version | RejectReason::NoCommonSuite => {
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

    let mut upload_task = upload.map(|u| {
        let link = Arc::clone(&link);
        tokio::spawn(async move { send_files(&link, &u.root, u.files).await })
    });
    let mut upload_result: Option<Result<(Vec<FileResult>, u64), NetError>> = None;

    let mut resend = tokio::time::interval(INPUT_RESEND);
    resend.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let time_limit = tokio::time::sleep(opts.duration.unwrap_or(Duration::MAX / 4));
    tokio::pin!(time_limit);
    let mut time_up = false;
    let mut input_open = true;

    let mut sender = InputSender::new();
    let mut reassembler = Reassembler::new(opts.deadline_us, 8);
    let (mut video_bytes, mut bad_datagrams, mut input_sent) = (0u64, 0u64, 0u64);
    let mut decoder: Option<Decoder> = None;
    let mut tracker = DelayTracker::new(link.now());
    let mut delivery = DeliveryLog::new();
    let (mut bad_frames, mut frames_applied) = (0u64, 0u64);
    // 反馈只在有新情况时发；什么都没发生时每秒一次，证明自己还在。
    let mut video_since_feedback = 0u32;
    let mut reported_floor = 0u32;
    let mut last_feedback = link.now();

    let end = loop {
        if time_up && upload_task.is_none() {
            break "到达时长上限".to_owned();
        }
        tokio::select! {
            () = &mut time_limit, if !time_up => time_up = true,
            result = async {
                match upload_task.as_mut() {
                    Some(task) => task.await,
                    None => std::future::pending().await,
                }
            } => {
                upload_task = None;
                upload_result = Some(result.unwrap_or_else(|e| Err(NetError::Io(e.to_string()))));
            }
            datagram = link.recv() => match datagram {
                Ok((header, payload)) => match header.channel {
                    Channel::Video => {
                        let now = link.now();
                        tracker.on_packet(now, header.send_time, header.seq, payload.len());
                        video_since_feedback = video_since_feedback.saturating_add(1);
                        let mut r = Reader::new(&payload);
                        let packet = VideoHeader::decode(&mut r).map(|video| VideoPacket {
                            header: video,
                            parity: header.flags & FLAG_PARITY != 0,
                            data: r.rest().to_vec(),
                        });
                        match packet.and_then(|p| reassembler.push(now, p)) {
                            Ok(Some(frame)) => {
                                // 画面尺寸变了（或第一帧）：按帧里声明的尺寸建解码器。
                                let applied = frame_size(&frame.data).and_then(|(w, h)| {
                                    let fits = decoder
                                        .as_ref()
                                        .is_some_and(|d| (d.screen().width(), d.screen().height()) == (w, h));
                                    let dec = if fits { decoder.as_mut() } else { None };
                                    let dec = match dec {
                                        Some(dec) => dec,
                                        None => decoder.insert(Decoder::new(w, h)),
                                    };
                                    dec.apply(&frame.data).map(|_| ())
                                });
                                // 解不了的帧也算“有定论”：它带的块由发送端在下一轮重发。
                                match applied {
                                    Ok(()) => {
                                        delivery.delivered(frame.frame_id);
                                        frames_applied = frames_applied.saturating_add(1);
                                        video_bytes = video_bytes.saturating_add(
                                            u64::try_from(frame.data.len()).unwrap_or(u64::MAX),
                                        );
                                        tracer.record(now, trace, Stage::FrameDelivered, u64::from(frame.frame_id));
                                        if let Some(dec) = decoder.as_ref() {
                                            sink.present(dec.screen());
                                        }
                                    }
                                    Err(_) => {
                                        delivery.advance(frame.frame_id);
                                        bad_frames = bad_frames.saturating_add(1);
                                    }
                                }
                            }
                            Ok(None) => {}
                            Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                        }
                    }
                    Channel::Input => match <[u8; 4]>::try_from(&payload[..]) {
                        Ok(ack) => sender.ack(u32::from_be_bytes(ack)),
                        Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
                    },
                    Channel::Control | Channel::Cursor | Channel::Clipboard | Channel::File => {
                        bad_datagrams = bad_datagrams.saturating_add(1);
                    }
                },
                Err(NetError::Closed(why)) => break why,
                Err(_) => bad_datagrams = bad_datagrams.saturating_add(1),
            },
            event = input.recv(), if input_open => match event {
                Some(event) => {
                    sender.push(event);
                    input_sent = input_sent.saturating_add(1);
                    let failed = sender.packet()?.and_then(|p| link.send(Channel::Input, &p).err());
             if let Some(e) = failed { break e.to_string(); }
                }
                None => input_open = false,
            },
            _ = resend.tick() => {
                let now = link.now();
                for frame_id in reassembler.expire(now) {
                    tracer.record(now, trace, Stage::FrameExpired, u64::from(frame_id));
                }
                // 反馈：哪些帧到了、排队延迟、丢包率、接收速率。
                if let Some(floor) = reassembler.floor() {
                    delivery.advance(floor);
                }
                let (floor, delivered) = delivery.snapshot();
                let news = video_since_feedback > 0 || floor != reported_floor;
                if news || now.since(last_feedback) >= IDLE_FEEDBACK_US {
                    video_since_feedback = 0;
                    reported_floor = floor;
                    last_feedback = now;
                    let sample = tracker.report(now);
                    let feedback = Feedback {
                        floor,
                        delivered,
                        queue_delay_us: sample.queue_delay_us,
                        loss_permille: sample.loss_permille,
                        recv_bytes_per_s: sample.recv_bytes_per_s,
                    };
                    if let Err(e) = link.send(Channel::Control, &feedback.encode()) { break e.to_string(); }
                }
                if sender.pending() > 0 {
                    let failed = sender.packet()?.and_then(|p| link.send(Channel::Input, &p).err());
             if let Some(e) = failed { break e.to_string(); }
                }
            }
            msg = ctrl_rx.recv() => match msg {
                Some(Control::Bye) | None => break "对端结束会话".to_owned(),
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

    let bye = session.close(link.now(), &mut tracer);
    let _ = write_control(&mut ctrl_send, &bye).await;
    let _ = ctrl_send.finish();
    if let Some(task) = upload_task {
        task.abort();
    }
    let rtt = link.rtt();
    let wire_bytes = link.wire_bytes();
    // 给对端一点时间读到 Bye，再关闭连接。
    let _ = tokio::time::timeout(Duration::from_millis(500), link.closed()).await;
    link.close("bye");

    let stats = reassembler.stats();
    let (files, file_bytes) = match upload_result {
        Some(Ok(done)) => done,
        Some(Err(e)) => return Err(e),
        None => (Vec::new(), 0),
    };
    Ok(ControllerReport {
        peer: *link.peer_fingerprint(),
        suite,
        frames_delivered: frames_applied,
        frames_expired: stats.expired,
        video_bytes,
        bad_frames,
        recovered: stats.recovered,
        screen: decoder.map(|d| d.screen().clone()),
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
