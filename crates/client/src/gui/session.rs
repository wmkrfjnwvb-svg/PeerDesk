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
use peerdesk_core::input::InputEvent;
use peerdesk_core::screen::Screen;
use peerdesk_core::signal::format_code;
use peerdesk_net::controller::{self, ControllerOptions, ScreenSink};
use peerdesk_net::host::{self, HostOptions, ScreenSource, FRAME_DEADLINE_US};
use peerdesk_net::identity::to_hex;
use peerdesk_net::netcheck::{prepare_host, NetReport};
use peerdesk_net::rendezvous::{dial_via, fetch_nodes, register_with, Path as Route, Server, Shim};
use peerdesk_net::{Identity, NetError, Node};
use tokio::sync::watch;

use super::form::Target;
use crate::local::{check_local, local_input, local_screen, Tap};
use crate::remote::{join_cluster, load_nodes, runtime, save_nodes, session, stay_registered};

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
    fn capture(&mut self) -> Option<&Screen> {
        if let Some(emitter) = self.announce.take() {
            emitter.send(HostEvent::Established);
        }
        if self.shared.kicked.load(Ordering::Relaxed) {
            return None;
        }
        self.inner.capture()
    }

    fn change_seq(&self) -> Option<u64> {
        self.inner.change_seq()
    }
}

async fn host_main(
    config: HostConfig,
    shared: Arc<HostShared>,
    mut stop: watch::Receiver<bool>,
    ev: Emitter<HostEvent>,
) -> Result<(), NetError> {
    // 没有权限就不提供远程：先确认屏幕和键鼠都能用，再开始监听。
    if let Err(why) = check_local() {
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
    loop {
        let link = tokio::select! {
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
            limiter: Arc::clone(&limiter),
            ..HostOptions::new(
                session(&identity, &policy, Auth::Password(password)),
                config.fps,
                recv_root.clone(),
            )
        };
        seed = seed.wrapping_add(1);
        // 会话开始时屏幕或键鼠打不开了（例如权限被撤销）：拒绝这次连接，停止被控。
        let opened = local_screen().and_then(|s| Ok((s, local_input(Arc::clone(&shared.tap))?)));
        let (screen, input) = match opened {
            Ok(opened) => opened,
            Err(why) => {
                link.close("host cannot share its screen");
                ev.send(HostEvent::Unavailable(why));
                break;
            }
        };
        let source = Watched {
            inner: screen,
            shared: Arc::clone(&shared),
            announce: Some(ev.clone()),
        };
        let result = tokio::select! {
            () = stopped(&mut stop) => break,
            r = host::serve(link, opts, Box::new(source), input) => r.map(|report| report.end),
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

/// 最新一帧画面，界面取走后清空。
#[derive(Debug, Default)]
pub struct FrameSlot {
    image: Mutex<Option<ColorImage>>,
    frames: AtomicU64,
    size: AtomicU32,
}

impl FrameSlot {
    /// 取走最新的画面（没有新画面时为 `None`）。
    pub fn take(&self) -> Option<ColorImage> {
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

    fn put(&self, screen: &Screen) {
        let image = to_color_image(screen);
        let [w0, w1] = screen.width().to_be_bytes();
        let [h0, h1] = screen.height().to_be_bytes();
        self.size
            .store(u32::from_be_bytes([w0, w1, h0, h1]), Ordering::Relaxed);
        *self.image.lock().unwrap_or_else(PoisonError::into_inner) = Some(image);
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
}

impl ScreenSink for GuiSink {
    fn present(&mut self, screen: &Screen) {
        self.slot.put(screen);
        (self.repaint)();
    }
}

/// 一次观看会话的总结。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// 会话为什么结束。
    pub end: String,
    /// 是不是本端主动断开的。
    pub cancelled: bool,
    /// 结束时的往返延迟。
    pub rtt: Option<Duration>,
    /// 收到的画面字节数。
    pub video_bytes: u64,
    /// 会话时长。
    pub elapsed: Duration,
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
        /// 断开的原因。
        why: String,
    },
    /// 服务器的通知（例如免费额度用完），会话随之结束。
    Notice(String),
}

/// 网络断开后最多自动重连几次。
pub const RECONNECT_TRIES: u32 = 3;
/// 两次重连之间等多久。
const RECONNECT_GAP: Duration = Duration::from_secs(1);
/// 多久看一次中继有没有发来通知。
const NOTICE_POLL: Duration = Duration::from_millis(500);

/// 会话结束的原因是不是网络断了（应当换通道重连），而不是对方或自己结束的。
#[must_use]
pub fn network_lost(end: &str) -> bool {
    [
        "timed out",
        "超时",
        "connection lost",
        "reset",
        "closed by peer: 0",
    ]
    .iter()
    .any(|k| end.contains(k))
        && !end.contains("对端结束会话")
}

/// 免费额度用完时给用户看的话。
#[must_use]
pub fn free_limit_text(retry_secs: u32) -> String {
    let minutes = retry_secs.div_ceil(60).max(1);
    format!("免费服务器拥挤，请约 {minutes} 分钟后重试（免费用户每 10 分钟的中继流量有限额）")
}

/// 控制端会话的句柄。丢弃时断开。
#[derive(Debug)]
pub struct ViewHandle {
    events: Receiver<ViewEvent>,
    input: tokio::sync::mpsc::Sender<InputEvent>,
    stop: watch::Sender<bool>,
    slot: Arc<FrameSlot>,
}

impl ViewHandle {
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
    let sink = GuiSink {
        slot: Arc::clone(&slot),
        repaint: Arc::clone(&emitter.repaint),
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
    }
}

/// 控制端绑定的地址：直接连地址时跟它同一地址族；通过服务器连接时用双栈端口
/// （IPv4 和 IPv6 的候选路径都能探测）。
const fn view_bind(target: &Target) -> SocketAddr {
    match target {
        Target::Addr { addr, .. } if addr.is_ipv6() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        }
        Target::Addr { .. } => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        Target::Device { .. } => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
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
    let bind = view_bind(&target);
    // 系统不支持 IPv6 时退回 IPv4。
    let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
    let node = Arc::new(Node::bind_first(&[bind, v4], &identity, &policy)?);
    let started = Instant::now();
    let result = tokio::select! {
        () = stopped(&mut stop) => Ok(Summary {
            cancelled: true,
            elapsed: started.elapsed(),
            ..Summary::default()
        }),
        r = watch_remote(&node, &identity, &policy, target, auth, sink, input, &ev, &data_dir) => r,
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

/// 要试的服务器：用户配置的那个在前，然后是缓存的其他节点（去重）。
#[must_use]
pub fn server_order(seed: Server, cached: Vec<Server>) -> Vec<Server> {
    let mut list = vec![seed];
    for n in cached {
        if !list.iter().any(|s| s.addr == n.addr) {
            list.push(n);
        }
    }
    list
}

/// 一次会话的结果：正常结束、网络断了（可以重连）、服务器通知（例如免费额度用完）。
enum Outcome {
    Done(Summary),
    Lost(String),
    Notice(String),
}

#[allow(clippy::too_many_arguments)]
async fn watch_remote(
    node: &Node,
    identity: &Identity,
    policy: &Policy,
    target: Target,
    auth: Auth,
    sink: GuiSink,
    input: tokio::sync::mpsc::Receiver<InputEvent>,
    ev: &Emitter<ViewEvent>,
    data_dir: &std::path::Path,
) -> Result<Summary, NetError> {
    // 输入事件先进这里，再转给当前这一次会话（重连后转给新会话）。
    let current: Arc<Mutex<Option<tokio::sync::mpsc::Sender<InputEvent>>>> =
        Arc::new(Mutex::new(None));
    let forward = Arc::clone(&current);
    let mut input = input;
    let forwarder = tokio::spawn(async move {
        while let Some(event) = input.recv().await {
            let tx = forward
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            if let Some(tx) = tx {
                let _ = tx.send(event).await;
            }
        }
    });
    let started = Instant::now();
    let mut attempt = 0u32;
    let result = loop {
        let (tx, rx) = tokio::sync::mpsc::channel(256);
        *current.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
        let outcome = one_session(
            node,
            identity,
            policy,
            &target,
            &auth,
            sink.clone(),
            rx,
            ev,
            data_dir,
        )
        .await;
        match outcome {
            Ok(Outcome::Done(summary)) => break Ok(summary),
            Ok(Outcome::Notice(text)) => {
                ev.send(ViewEvent::Notice(text.clone()));
                break Ok(Summary {
                    end: text,
                    elapsed: started.elapsed(),
                    ..Summary::default()
                });
            }
            Ok(Outcome::Lost(why)) if attempt < RECONNECT_TRIES => {
                attempt += 1;
                peerdesk_net::note!(warn, "控制：网络断开（{why}），第 {attempt} 次重连");
                ev.send(ViewEvent::Reconnecting { attempt, why });
                tokio::time::sleep(RECONNECT_GAP).await;
            }
            Ok(Outcome::Lost(why)) => break Err(NetError::Closed(why)),
            // 第一次就连不上：直接报错；重连时连不上：再试，用完次数再报错。
            Err(e) if attempt > 0 && attempt < RECONNECT_TRIES => {
                attempt += 1;
                ev.send(ViewEvent::Reconnecting {
                    attempt,
                    why: e.to_string(),
                });
                tokio::time::sleep(RECONNECT_GAP).await;
            }
            Err(e) => break Err(e),
        }
    };
    forwarder.abort();
    result
}

/// 连一次并跑一次会话。
#[allow(clippy::too_many_arguments)]
async fn one_session(
    node: &Node,
    identity: &Identity,
    policy: &Policy,
    target: &Target,
    auth: &Auth,
    sink: GuiSink,
    input: tokio::sync::mpsc::Receiver<InputEvent>,
    ev: &Emitter<ViewEvent>,
    data_dir: &std::path::Path,
) -> Result<Outcome, NetError> {
    // 走中继时的本机转发器要保持到会话结束。
    let (link, shim, route, detail) = match target {
        Target::Addr { addr, pin } => (
            node.connect(*addr, pin.as_ref()).await?,
            None,
            Route::Direct,
            String::new(),
        ),
        Target::Device {
            server,
            device,
            policy,
            account,
        } => {
            let servers = server_order(*server, load_nodes(data_dir));
            let dialed = dial_via(node, &servers, device, *policy, account).await?;
            // 顺便更新集群节点缓存，下次这个服务器崩了还能换别的节点。
            if let Ok(nodes) = fetch_nodes(node, server).await {
                if !nodes.is_empty() {
                    let _ = save_nodes(data_dir, &nodes);
                }
            }
            let rtt = dialed.rtt_us.map_or_else(String::new, |us| {
                format!(
                    " {:.1} ms",
                    Duration::from_micros(us).as_secs_f64() * 1000.0
                )
            });
            let note = if dialed.shim.is_some() && !dialed.relay_note.is_empty() {
                format!("，{}", dialed.relay_note)
            } else {
                String::new()
            };
            let detail = format!("{}{rtt}{note}", dialed.route.label());
            (dialed.link, dialed.shim, dialed.path, detail)
        }
    };
    peerdesk_net::note!(
        info,
        "控制：已连上 {}（{}），路径 {route:?} {detail}",
        link.remote_addr(),
        to_hex(link.peer_fingerprint())
            .get(..16)
            .unwrap_or_default()
    );
    ev.send(ViewEvent::Connected {
        peer: to_hex(link.peer_fingerprint()),
        remote: link.remote_addr(),
        route,
        detail,
    });
    let started = Instant::now();
    let session = controller::run(
        link,
        ControllerOptions {
            session: session(identity, policy, auth.clone()),
            deadline_us: FRAME_DEADLINE_US,
            duration: None,
        },
        Box::new(sink),
        input,
        None,
    );
    // 走中继时盯着中继的通知：免费额度用完就结束会话并告诉用户原因。
    let notice = async {
        loop {
            tokio::time::sleep(NOTICE_POLL).await;
            if let Some((_, retry)) = shim.as_ref().and_then(Shim::notice) {
                return free_limit_text(retry);
            }
        }
    };
    tokio::select! {
        report = session => {
            let report = report?;
            if network_lost(&report.end) {
                return Ok(Outcome::Lost(report.end));
            }
            Ok(Outcome::Done(Summary {
                end: report.end,
                cancelled: false,
                rtt: Some(report.rtt),
                video_bytes: report.video_bytes,
                elapsed: started.elapsed(),
            }))
        }
        text = notice => Ok(Outcome::Notice(text)),
    }
}

#[cfg(test)]
mod tests;
