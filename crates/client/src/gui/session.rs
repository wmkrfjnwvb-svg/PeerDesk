//! 在后台线程里运行会话：每个会话一个线程、一个 tokio 运行时。
//!
//! 界面线程和会话线程之间只有通道：会话线程发事件过来并请求重绘，界面每帧取走；
//! 界面通过句柄发输入、断开或停止。句柄被丢弃时会话自己结束。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use eframe::egui::{Color32, ColorImage};
use peerdesk_core::auth::{Auth, Limiter};
use peerdesk_core::crypto::Policy;
use peerdesk_core::frame::{Frame, Picture};
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::PeerOs;
use peerdesk_core::screen::Screen;
use peerdesk_core::session::SessionConfig;
use peerdesk_core::signal::format_code;
use peerdesk_net::clipsync::{ClipNote, ClipboardHandle};
use peerdesk_net::controller::{self, LinkHook, ScreenSink};
use peerdesk_net::host::{self, HostOptions, ScreenSource};
use peerdesk_net::identity::to_hex;
use peerdesk_net::netcheck::{prepare_host, NetReport};
use peerdesk_net::rendezvous::{register_with, Path as Route, Server};
use peerdesk_net::viewer::{self, ViewEnv};
pub use peerdesk_net::viewer::{
    free_limit_text, network_lost, server_order, Summary, RECONNECT_TRIES,
};
use peerdesk_net::{Identity, NetError, Node};
use tokio::sync::watch;

use super::form::Target;
use crate::local::{check_local_in, local_input, local_screen_in, Tap};
use crate::remote::{join_cluster, runtime, session, stay_registered};

/// 请求界面重绘。
pub type Repaint = Arc<dyn Fn() + Send + Sync>;

/// 关闭连接时等关闭通知送到对端的时间上限。
const CLOSE_GRACE: Duration = Duration::from_millis(500);

/// 发事件并请求重绘。
struct Emitter<E> {
    tx: Sender<E>,
    repaint: Repaint,
}

impl<E> Clone for Emitter<E> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            repaint: Arc::clone(&self.repaint),
        }
    }
}

impl<E> Emitter<E> {
    fn send(&self, event: E) {
        // 界面已经不要这个会话了：丢掉即可。
        let _ = self.tx.send(event);
        (self.repaint)();
    }
}

/// 在新线程上跑 `task`，结束后用 `last` 把结果变成最后一个事件发出。
fn spawn<E, F, T>(name: &str, emitter: Emitter<E>, task: F, last: fn(Result<T, NetError>) -> E)
where
    E: Send + 'static,
    T: Send + 'static,
    F: std::future::Future<Output = Result<T, NetError>> + Send + 'static,
{
    let inner = emitter.clone();
    let spawned = std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || {
            let result = runtime().and_then(|rt| rt.block_on(task));
            inner.send(last(result));
        });
    if let Err(e) = spawned {
        emitter.send(last(Err(NetError::Io(format!("启动线程失败：{e}")))));
    }
}

/// 等到界面要求停止（或者句柄被丢弃）。
async fn stopped(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow_and_update() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// 退出时最多等多久让关闭帧发出去（用户点了退出，不能拖）。
const QUIT_GRACE: Duration = Duration::from_millis(300);

/// 用户退出程序之前：给所有正在进行的会话（被控端、控制端都算）发带原因的关闭，对端马上知道
/// “对方退出了 PeerDesk”，不用等超时。最多等 [`QUIT_GRACE`]。任何线程都能调用，不依赖界面刷新。
/// 返回通知了几个有连接的端点。
pub fn say_goodbye() -> usize {
    say_goodbye_where(|_| true)
}

/// 同 [`say_goodbye`]，只处理本端指纹满足条件的端点（测试里同一进程有多个身份）。
pub fn say_goodbye_where(mine: impl Fn(&peerdesk_net::Fingerprint) -> bool) -> usize {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build();
    match runtime {
        Ok(rt) => rt.block_on(peerdesk_net::node::quit_matching(QUIT_GRACE, mine)),
        Err(e) => {
            log::warn!("退出前通知对端失败：{e}");
            0
        }
    }
}

/// 先关端点，再等关闭通知发出去（有上限）。
async fn close_nodes(nodes: &[Arc<Node>]) {
    for node in nodes {
        node.close();
    }
    let _ = tokio::time::timeout(CLOSE_GRACE, async {
        for node in nodes {
            node.wait_idle().await;
        }
    })
    .await;
}

// ---------------------------------------------------------------- 被控端

/// 被控端的设置。
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// 监听端口；0 表示随便选一个（测试用）。
    pub port: u16,
    /// 数据目录：设备身份、收到的文件。
    pub data_dir: PathBuf,
    /// 登记到这台信令服务器上。
    pub server: Option<Server>,
    /// 帧率上限。
    pub fps: u32,
}

/// 被控端会话线程发给界面的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// 开始监听。`ipv6` 表示 IPv6 地址也能连。
    Listening {
        /// 实际的端口。
        port: u16,
        /// 是否同时在 IPv6 上监听。
        ipv6: bool,
    },
    /// 已在信令服务器登记。
    Registered {
        /// 服务器看到的本机地址。
        observed: SocketAddr,
        /// 服务器分配的设备码。
        code: u32,
    },
    /// 网络体检的结果。
    Network(NetReport),
    /// 登记失败（不影响直连）。
    RegisterFailed(NetError),
    /// 有控制端连上来，正在验证。
    Peer {
        /// 对端地址。
        addr: SocketAddr,
        /// 对端指纹。
        fingerprint: String,
    },
    /// 控制端通过了验证，开始发画面。
    Established,
    /// 本机屏幕或键鼠打不开（多半是没有权限）：不提供远程，被控已关闭。带着原因。
    Unavailable(String),
    /// 有人来连，但这时屏幕打不开（例如锁屏、登录界面）：拒绝这一次，继续等待下一次。带着原因。
    Refused(String),
    /// 会话结束：结束原因，或者失败原因（例如密码错误）。
    SessionEnded(Result<String, NetError>),
    /// 连接握手失败。
    HandshakeFailed(NetError),
    /// 不再接受连接。出错退出时带着原因。
    Stopped(Option<NetError>),
}

/// 界面和被控端线程共享的东西。
#[derive(Debug, Default)]
struct HostShared {
    password: Mutex<String>,
    kicked: AtomicBool,
    tap: Arc<Tap>,
}

/// 被控端线程的句柄。丢弃时停止。
#[derive(Debug)]
pub struct HostHandle {
    events: Receiver<HostEvent>,
    stop: watch::Sender<bool>,
    shared: Arc<HostShared>,
}

impl HostHandle {
    /// 取走到目前为止的事件。
    pub fn poll(&self) -> Vec<HostEvent> {
        self.events.try_iter().collect()
    }

    /// 停止：断开当前会话，关闭端口。线程结束时发 [`HostEvent::Stopped`]。
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }

    /// 断开当前的控制端，继续等待下一个。
    pub fn kick(&self) {
        self.shared.kicked.store(true, Ordering::Relaxed);
    }

    /// 换密码。只影响之后的连接。
    pub fn set_password(&self, password: &str) {
        password.clone_into(
            &mut self
                .shared
                .password
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    /// 收到的输入。
    #[must_use]
    pub fn tap(&self) -> &Tap {
        &self.shared.tap
    }
}

impl Drop for HostHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 开始作为被控端等待连接。
pub fn start_host(config: HostConfig, password: &str, repaint: Repaint) -> HostHandle {
    let (tx, events) = mpsc::channel();
    let (stop, stop_rx) = watch::channel(false);
    let shared = Arc::new(HostShared::default());
    password.clone_into(
        &mut shared
            .password
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
    );
    let emitter = Emitter { tx, repaint };
    let task = host_main(config, Arc::clone(&shared), stop_rx, emitter.clone());
    spawn("peerdesk-host", emitter, task, |r| {
        HostEvent::Stopped(r.err())
    });
    HostHandle {
        events,
        stop,
        shared,
    }
}

/// 绑定监听端口：IPv4 必须成功；同一端口上再试一个 IPv6 端点。
/// Linux 和 macOS 的 IPv6 套接字默认也收 IPv4，和 IPv4 端点冲突而绑定失败，这时只用 IPv4；
/// Windows 上两者互不影响，IPv6 地址也能连。
fn bind_nodes(
    port: u16,
    identity: &Identity,
    policy: &Policy,
) -> Result<(Vec<Arc<Node>>, bool), NetError> {
    // 优先一个双栈端口（IPv4 和 IPv6 都收发，打洞和 IPv6 直连用同一个端口）；
    // 系统不支持 IPv6 时只开 IPv4。
    let tries = [
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
    ];
    let node = Node::bind_first(&tries, identity, policy)?;
    let ipv6 = node.local_addr()?.is_ipv6();
    Ok((vec![Arc::new(node)], ipv6))
}

/// 画面来源的包装：第一次取画面时告诉界面“验证通过了”；界面要求断开时返回 `None` 结束会话。
struct Watched {
    inner: Box<dyn ScreenSource>,
    shared: Arc<HostShared>,
    announce: Option<Emitter<HostEvent>>,
}

impl ScreenSource for Watched {
    fn frame(&mut self) -> Option<Frame> {
        if let Some(emitter) = self.announce.take() {
            emitter.send(HostEvent::Established);
        }
        if self.shared.kicked.load(Ordering::Relaxed) {
            return None;
        }
        self.inner.frame()
    }

    fn native_size(&self) -> Option<(u16, u16)> {
        self.inner.native_size()
    }

    fn set_target_size(&mut self, size: (u16, u16)) {
        self.inner.set_target_size(size);
    }

    fn change_seq(&self) -> Option<u64> {
        self.inner.change_seq()
    }

    fn stats_line(&mut self) -> Option<String> {
        self.inner.stats_line()
    }
}

async fn host_main(
    config: HostConfig,
    shared: Arc<HostShared>,
    mut stop: watch::Receiver<bool>,
    ev: Emitter<HostEvent>,
) -> Result<(), NetError> {
    // 没有权限就不提供远程：先确认屏幕和键鼠都能用，再开始监听。
    if let Err(why) = check_local_in(&config.data_dir) {
        ev.send(HostEvent::Unavailable(why));
        return Ok(());
    }
    let policy = Policy::recommended();
    let identity = Identity::load_or_create(&config.data_dir)?;
    let (nodes, ipv6) = bind_nodes(config.port, &identity, &policy)?;
    let mut port = config.port;
    for node in &nodes {
        port = node.local_addr()?.port();
    }
    ev.send(HostEvent::Listening { port, ipv6 });

    if let Some(server) = config.server {
        // 与服务器同一地址族的端点优先（Windows 上 IPv6 套接字不收 IPv4）。
        let same_family = nodes.iter().find(|n| {
            n.local_addr()
                .is_ok_and(|a| a.is_ipv4() == server.addr.is_ipv4())
        });
        if let Some(node) = same_family.or(nodes.first()) {
            // 先体检、请路由器映射端口（映射出来的地址也报告给服务器），结果给界面显示。
            let (report, extra) = prepare_host(node, Some(&server)).await;
            for (item, value) in report.lines() {
                peerdesk_net::note!(info, "网络体检 {item}：{value}");
            }
            ev.send(HostEvent::Network(report));
            match register_with(node, &server, extra.clone()).await {
                Ok(registration) => {
                    peerdesk_net::note!(
                        info,
                        "已在服务器 {} 登记：设备码 {}，服务器看到的本机地址 {}",
                        server.addr,
                        format_code(registration.code),
                        registration.observed
                    );
                    ev.send(HostEvent::Registered {
                        observed: registration.observed,
                        code: registration.code,
                    });
                    tokio::spawn(stay_registered(
                        Arc::clone(node),
                        server,
                        Some(registration),
                        extra.clone(),
                    ));
                    tokio::spawn(join_cluster(
                        Arc::clone(node),
                        server,
                        extra,
                        config.data_dir.clone(),
                    ));
                }
                Err(e) => {
                    peerdesk_net::note!(warn, "在服务器 {} 登记失败：{e}", server.addr);
                    ev.send(HostEvent::RegisterFailed(e));
                }
            }
        }
    }

    // 所有端点收到的连接汇到一处，一次只服务一个控制端。
    let (tx, mut incoming) = tokio::sync::mpsc::channel(8);
    for node in &nodes {
        let (node, tx) = (Arc::clone(node), tx.clone());
        tokio::spawn(async move {
            while let Some(link) = node.accept().await {
                if tx.send(link).await.is_err() {
                    break;
                }
            }
        });
    }
    drop(tx);

    let limiter = Arc::new(Mutex::new(Limiter::default()));
    let recv_root = config.data_dir.join("received");
    let mut seed = 1u64;
    // 会话进行中来的其他控制端的连接，按顺序排着（一次只服务一个）
    let mut waiting: std::collections::VecDeque<peerdesk_net::Link> =
        std::collections::VecDeque::new();
    'serve: loop {
        let link = if let Some(link) = waiting.pop_front() {
            link
        } else {
            tokio::select! {
            () = stopped(&mut stop) => break,
            next = incoming.recv() => match next {
                Some(Ok(link)) => link,
                Some(Err(e)) => {
                    peerdesk_net::note!(warn, "被控：握手失败：{e}");
                    ev.send(HostEvent::HandshakeFailed(e));
                    continue;
                }
                None => break,
            },
            }
        };
        peerdesk_net::note!(
            info,
            "被控：{} 连上来（{}）",
            link.remote_addr(),
            to_hex(link.peer_fingerprint())
                .get(..16)
                .unwrap_or_default()
        );
        ev.send(HostEvent::Peer {
            addr: link.remote_addr(),
            fingerprint: to_hex(link.peer_fingerprint()),
        });
        shared.kicked.store(false, Ordering::Relaxed);
        let password = shared
            .password
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let opts = HostOptions {
            record: crate::diag_record::setup(&config.data_dir),
            limiter: Arc::clone(&limiter),
            clipboard: system_clipboard(Box::new(|_| {})),
            audio: SYSTEM_AUDIO.0(),
            name: crate::local::device_name(),
            ..HostOptions::new(
                session(&identity, &policy, Auth::Password(password)),
                config.fps,
                recv_root.clone(),
            )
        };
        seed = seed.wrapping_add(1);
        // 这时屏幕或键鼠打不开（锁屏、登录界面、权限被撤销……）：拒绝这一次连接并告诉对方原因，
        // 继续等待下一次（原来会直接停止被控，设备从此离线）。
        let opened = local_screen_in(&config.data_dir)
            .and_then(|s| Ok((s, local_input(Arc::clone(&shared.tap))?)));
        let (screen, input) = match opened {
            Ok(opened) => opened,
            Err(why) => {
                peerdesk_net::note!(warn, "被控：拒绝 {}：{why}", link.remote_addr());
                link.close(&why);
                ev.send(HostEvent::Refused(why));
                continue;
            }
        };
        let source = Watched {
            inner: screen,
            shared: Arc::clone(&shared),
            announce: Some(ev.clone()),
        };
        let current = *link.peer_fingerprint();
        let mut incoming_open = true;
        let serving = host::serve(link, opts, Box::new(source), input);
        tokio::pin!(serving);
        let result = loop {
            tokio::select! {
                () = stopped(&mut stop) => break 'serve,
                r = &mut serving => break r.map(|report| report.end),
                // 会话中同一个控制端又连上来：它换了通道（中继升级直连、网络变了重连），
                // 旧会话马上结束、服务新连接，不等旧连接超时（以前要等十几秒，新连接的握手也跟着超时）
                next = incoming.recv(), if incoming_open => match next {
                    Some(Ok(newer)) if *newer.peer_fingerprint() == current => {
                        waiting.push_front(newer);
                        break Ok("同一个控制端换了通道".to_owned());
                    }
                    Some(Ok(other)) => waiting.push_back(other),
                    Some(Err(e)) => {
                        peerdesk_net::note!(warn, "被控：握手失败：{e}");
                        ev.send(HostEvent::HandshakeFailed(e));
                    }
                    // 端点都关了：不再等新连接（否则这里会一直立刻返回、空转）
                    None => incoming_open = false,
                },
            }
        };
        match &result {
            Ok(end) => peerdesk_net::note!(info, "被控：会话结束：{end}"),
            Err(e) => peerdesk_net::note!(warn, "被控：会话出错：{e}"),
        }
        ev.send(HostEvent::SessionEnded(result));
    }
    close_nodes(&nodes).await;
    Ok(())
}

// ---------------------------------------------------------------- 控制端

/// 最新一帧画面，界面取走后清空。存解码器给的格式（NV12 或 RGB），界面在显卡上转颜色。
#[derive(Debug, Default)]
pub struct FrameSlot {
    image: Mutex<Option<Picture>>,
    frames: AtomicU64,
    size: AtomicU32,
}

impl FrameSlot {
    /// 取走最新的画面（没有新画面时为 `None`）。
    pub fn take(&self) -> Option<Picture> {
        self.image
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// 一共收到的画面数。
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// 最近一帧的尺寸。
    pub fn size(&self) -> [u16; 2] {
        let [w0, w1, h0, h1] = self.size.load(Ordering::Relaxed).to_be_bytes();
        [u16::from_be_bytes([w0, w1]), u16::from_be_bytes([h0, h1])]
    }

    fn put(&self, picture: Picture) {
        let (w, h) = picture.size();
        let [w0, w1] = w.to_be_bytes();
        let [h0, h1] = h.to_be_bytes();
        self.size
            .store(u32::from_be_bytes([w0, w1, h0, h1]), Ordering::Relaxed);
        *self.image.lock().unwrap_or_else(PoisonError::into_inner) = Some(picture);
        self.frames.fetch_add(1, Ordering::Relaxed);
    }
}

/// 画面（像素是 `0x00RRGGBB`）转成 egui 的图片。
#[must_use]
pub fn to_color_image(screen: &Screen) -> ColorImage {
    let pixels = screen
        .pixels()
        .iter()
        .map(|p| {
            let [_, r, g, b] = p.to_be_bytes();
            Color32::from_rgb(r, g, b)
        })
        .collect();
    ColorImage::new(
        [usize::from(screen.width()), usize::from(screen.height())],
        pixels,
    )
}

#[derive(Clone)]
struct GuiSink {
    slot: Arc<FrameSlot>,
    repaint: Repaint,
    /// 声音开关（助手里切换，默认关）；重连后的新会话也看同一个开关。
    sound: watch::Receiver<bool>,
    /// 显示画面的区域（像素，0×0 表示不限）：被控端按这个大小编码；重连后的新会话也看它。
    view: watch::Receiver<(u16, u16)>,
    /// 当前连接（对方支持文件管理时有），文件管理在上面开浏览流；重连后换成新连接。
    files: FilesLink,
}

/// 文件管理用的当前连接。
type FilesLink = Arc<Mutex<Option<Arc<peerdesk_net::Link>>>>;

impl ScreenSink for GuiSink {
    fn present(&mut self, picture: &Picture) {
        // 只是多一个引用，不拷贝像素、不转颜色
        self.slot.put(picture.clone());
        (self.repaint)();
    }
}

/// 控制端会话线程发给界面的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewEvent {
    /// 连上了，开始握手。
    Connected {
        /// 对端指纹。
        peer: String,
        /// 对端地址。
        remote: SocketAddr,
        /// 直连还是中继。
        route: Route,
        /// 路径的具体说明（种类和往返时间），直接连地址时为空。
        detail: String,
    },
    /// 会话结束。
    Ended(Result<Summary, NetError>),
    /// 网络断了，正在换通道重连（第几次，断开的原因）。
    Reconnecting {
        /// 第几次重连。
        attempt: u32,
        /// 断开的原因（原文）。
        why: String,
        /// 为什么在重连（网络断了，或者重拨失败的原因）。
        reason: peerdesk_net::reason::Reason,
    },
    /// 服务器的通知（例如免费额度用完），会话随之结束。
    Notice(String),
    /// 剪贴板事件（内容太大没有同步等），界面短暂提示，会话继续。
    Clipboard(ClipNote),
    /// 每秒一次的链路质量（往返时间、丢包、排队），工具条据此提示网络状况。
    Quality(controller::LinkQuality),
    /// 握手完成，得知被控端的系统和电脑名（老版本为未知、空）。助手据此选 Ctrl 还是 Cmd，
    /// 设备列表据此显示电脑名。
    Peer(PeerOs, String),
}

impl From<viewer::ViewEvent> for ViewEvent {
    fn from(event: viewer::ViewEvent) -> Self {
        match event {
            viewer::ViewEvent::Connected {
                peer,
                remote,
                route,
                detail,
            } => Self::Connected {
                peer,
                remote,
                route,
                detail,
            },
            viewer::ViewEvent::Reconnecting {
                attempt,
                why,
                reason,
            } => Self::Reconnecting {
                attempt,
                why,
                reason,
            },
            viewer::ViewEvent::Notice(text) => Self::Notice(text),
            viewer::ViewEvent::Clipboard(note) => Self::Clipboard(note),
            viewer::ViewEvent::Quality(q) => Self::Quality(q),
            viewer::ViewEvent::Peer(os, name) => Self::Peer(os, name),
        }
    }
}

/// 打开本机系统剪贴板。测试构建里没有（测试不碰开发者电脑的剪贴板）。
const SYSTEM_CLIPBOARD: fn(peerdesk_platform::OnNote) -> Option<ClipboardHandle> = if cfg!(test) {
    no_clipboard
} else {
    peerdesk_platform::open_clipboard
};

fn no_clipboard(_on_note: peerdesk_platform::OnNote) -> Option<ClipboardHandle> {
    None
}

fn system_clipboard(on_note: peerdesk_platform::OnNote) -> Option<ClipboardHandle> {
    SYSTEM_CLIPBOARD(on_note)
}

type OpenCapture = fn() -> Option<peerdesk_net::audio::CaptureHandle>;
type OpenPlayback = fn() -> peerdesk_net::audio::PlaybackHandle;

/// 本机声卡：被控端的声音来源、控制端的声音去向。测试构建里换成合成声音和录音器（不碰开发者的声卡）。
const SYSTEM_AUDIO: (OpenCapture, OpenPlayback) = if cfg!(test) {
    (test_capture, test_playback)
} else {
    (
        peerdesk_platform::system_capture,
        peerdesk_platform::system_playback,
    )
};

fn test_capture() -> Option<peerdesk_net::audio::CaptureHandle> {
    Some(peerdesk_net::audio::CaptureHandle(Arc::new(
        peerdesk_net::audio::ToneCapture,
    )))
}

fn test_playback() -> peerdesk_net::audio::PlaybackHandle {
    peerdesk_net::audio::PlaybackHandle(Arc::new(peerdesk_net::audio::Recorder::default()))
}

/// 控制端会话的句柄。丢弃时断开。
#[derive(Debug)]
pub struct ViewHandle {
    events: Receiver<ViewEvent>,
    input: tokio::sync::mpsc::Sender<InputEvent>,
    stop: watch::Sender<bool>,
    slot: Arc<FrameSlot>,
    sound: watch::Sender<bool>,
    view: watch::Sender<(u16, u16)>,
    files: FilesLink,
}

impl ViewHandle {
    /// 打开或关闭远程声音（默认关）。
    pub fn set_sound(&self, on: bool) {
        let _ = self.sound.send(on);
    }

    /// 显示画面的区域变了（像素；0×0 表示不限，例如“原始大小”）。被控端按它编码。
    pub fn set_view(&self, size: (u16, u16)) {
        let _ = self.view.send_if_modified(|v| {
            let changed = *v != size;
            *v = size;
            changed
        });
    }

    /// 文件管理用的连接：已连上、对方支持文件管理时有。
    #[must_use]
    pub fn files_link(&self) -> Option<Arc<peerdesk_net::Link>> {
        self.files
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// 远程声音是否打开。
    #[must_use]
    pub fn sound(&self) -> bool {
        *self.sound.borrow()
    }

    /// 取走到目前为止的事件。
    pub fn poll(&self) -> Vec<ViewEvent> {
        self.events.try_iter().collect()
    }

    /// 发一个输入事件。队列满或会话已结束时返回 `false`（事件丢弃）。
    pub fn send(&self, event: InputEvent) -> bool {
        self.input.try_send(event).is_ok()
    }

    /// 断开。线程结束时发 [`ViewEvent::Ended`]。
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }

    /// 收到的画面。
    #[must_use]
    pub fn frames(&self) -> &FrameSlot {
        &self.slot
    }
}

impl Drop for ViewHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 开始一次观看会话。`password` 为空表示不带密码（对端信任本机时可用）。
pub fn start_view(
    target: Target,
    password: &str,
    data_dir: PathBuf,
    repaint: Repaint,
) -> ViewHandle {
    let (tx, events) = mpsc::channel();
    let (stop, stop_rx) = watch::channel(false);
    let (input, input_rx) = tokio::sync::mpsc::channel(1024);
    let slot = Arc::new(FrameSlot::default());
    let auth = if password.is_empty() {
        Auth::Open
    } else {
        Auth::Password(password.to_owned())
    };
    let emitter = Emitter { tx, repaint };
    let (sound, sound_rx) = watch::channel(false);
    let (view, view_rx) = watch::channel((0, 0));
    let files = FilesLink::default();
    let sink = GuiSink {
        slot: Arc::clone(&slot),
        repaint: Arc::clone(&emitter.repaint),
        sound: sound_rx,
        view: view_rx,
        files: Arc::clone(&files),
    };
    let task = view_main(
        target,
        auth,
        data_dir,
        sink,
        input_rx,
        stop_rx,
        emitter.clone(),
    );
    spawn("peerdesk-view", emitter, task, ViewEvent::Ended);
    ViewHandle {
        events,
        input,
        stop,
        slot,
        sound,
        view,
        files,
    }
}

async fn view_main(
    target: Target,
    auth: Auth,
    data_dir: PathBuf,
    sink: GuiSink,
    input: tokio::sync::mpsc::Receiver<InputEvent>,
    mut stop: watch::Receiver<bool>,
    ev: Emitter<ViewEvent>,
) -> Result<Summary, NetError> {
    let policy = Policy::recommended();
    let identity = Identity::load_or_create(&data_dir)?;
    let bind = viewer::bind_for(&target);
    // 系统不支持 IPv6 时退回 IPv4。
    let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
    let node = Arc::new(Node::bind_first(&[bind, v4], &identity, &policy)?);
    let env: Arc<dyn ViewEnv> = Arc::new(GuiEnv { sink, ev, data_dir });
    let started = Instant::now();
    let result = tokio::select! {
        () = stopped(&mut stop) => Ok(Summary {
            cancelled: true,
            elapsed: started.elapsed(),
            ..Summary::default()
        }),
        r = viewer::drive(&node, &identity, &policy, target, auth, env, input) => r,
    };
    close_nodes(&[node]).await;
    match &result {
        Ok(summary) => peerdesk_net::note!(
            info,
            "控制：会话结束（{:?}），时长 {:?}，画面 {} KB，RTT {:?}",
            summary.end,
            summary.elapsed,
            summary.video_bytes / 1024,
            summary.rtt
        ),
        Err(e) => peerdesk_net::note!(warn, "控制：连接或会话失败：{e}"),
    }
    result
}

/// 桌面界面给访问驱动（[`viewer::drive`]）的东西：画面进界面的槽，事件进界面的通道。
struct GuiEnv {
    sink: GuiSink,
    ev: Emitter<ViewEvent>,
    data_dir: PathBuf,
}

impl ViewEnv for GuiEnv {
    fn event(&self, event: viewer::ViewEvent) {
        self.ev.send(ViewEvent::from(event));
    }

    fn sink(&self) -> Box<dyn ScreenSink> {
        Box::new(self.sink.clone())
    }

    fn session(&self, identity: &Identity, policy: &Policy, auth: Auth) -> SessionConfig {
        session(identity, policy, auth)
    }

    fn clipboard(&self) -> Option<ClipboardHandle> {
        let ev = self.ev.clone();
        system_clipboard(Box::new(move |note| ev.send(ViewEvent::Clipboard(note))))
    }

    fn audio(&self) -> Option<peerdesk_net::audio::AudioOut> {
        Some(peerdesk_net::audio::AudioOut {
            playback: SYSTEM_AUDIO.1(),
            on: self.sink.sound.clone(),
        })
    }

    fn view(&self) -> Option<watch::Receiver<(u16, u16)>> {
        Some(self.sink.view.clone())
    }

    fn on_link(&self) -> Option<LinkHook> {
        let files = Arc::clone(&self.sink.files);
        Some(LinkHook(Arc::new(move |link| {
            *files.lock().unwrap_or_else(PoisonError::into_inner) = link;
        })))
    }

    fn record(&self) -> Option<peerdesk_net::recorder::RecordSetup> {
        crate::diag_record::setup(&self.data_dir)
    }

    fn data_dir(&self) -> &std::path::Path {
        &self.data_dir
    }
}

#[cfg(test)]
mod tests;
