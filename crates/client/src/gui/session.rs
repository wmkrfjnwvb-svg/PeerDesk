//! 在后台线程里运行会话：每个会话一个线程、一个 tokio 运行时。
//!
//! 界面线程和会话线程之间只有通道：会话线程发事件过来并请求重绘，界面每帧取走；
//! 界面通过句柄发输入、断开或停止。句柄被丢弃时会话自己结束。

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
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
use peerdesk_net::controller::{self, ControllerOptions, ScreenSink};
use peerdesk_net::host::{self, HostOptions, ScreenSource, FRAME_DEADLINE_US};
use peerdesk_net::identity::to_hex;
use peerdesk_net::rendezvous::{dial, register, Path as Route, Server};
use peerdesk_net::{Identity, NetError, Node};
use tokio::sync::watch;

use super::form::Target;
use crate::local::{local_input, local_screen, Tap};
use crate::remote::{runtime, session, stay_registered};

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
    },
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

/// 绑定监听端口：先试 IPv6（多数系统上同时接受 IPv4），再在同一端口上试 IPv4。
/// IPv6 不可用、或者 IPv4 已经由 IPv6 套接字一并接收时，只用其中一个。
fn bind_nodes(
    port: u16,
    identity: &Identity,
    policy: &Policy,
) -> Result<(Vec<Arc<Node>>, bool), NetError> {
    let v6 = Node::bind((Ipv6Addr::UNSPECIFIED, port).into(), identity, policy);
    let port = match &v6 {
        Ok(node) => node.local_addr()?.port(),
        Err(_) => port,
    };
    let v4 = Node::bind((Ipv4Addr::UNSPECIFIED, port).into(), identity, policy);
    let has_v6 = v6.is_ok();
    let nodes: Vec<Arc<Node>> = match (v6, v4) {
        (Err(_), Err(e)) => return Err(e),
        (v6, v4) => [v6.ok(), v4.ok()]
            .into_iter()
            .flatten()
            .map(Arc::new)
            .collect(),
    };
    Ok((nodes, has_v6))
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
            match register(node, &server).await {
                Ok(registration) => {
                    ev.send(HostEvent::Registered {
                        observed: registration.observed,
                    });
                    tokio::spawn(stay_registered(Arc::clone(node), server, registration));
                }
                Err(e) => ev.send(HostEvent::RegisterFailed(e)),
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
                    ev.send(HostEvent::HandshakeFailed(e));
                    continue;
                }
                None => break,
            },
        };
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
        let source = Watched {
            inner: local_screen(seed),
            shared: Arc::clone(&shared),
            announce: Some(ev.clone()),
        };
        let input = local_input(Arc::clone(&shared.tap));
        let result = tokio::select! {
            () = stopped(&mut stop) => break,
            r = host::serve(link, opts, Box::new(source), input) => r.map(|report| report.end),
        };
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
    },
    /// 会话结束。
    Ended(Result<Summary, NetError>),
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

const fn wants_ipv6(target: &Target) -> bool {
    match target {
        Target::Addr { addr, .. } => addr.is_ipv6(),
        Target::Device { server, .. } => server.addr.is_ipv6(),
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
    let bind: SocketAddr = if wants_ipv6(&target) {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    };
    let node = Arc::new(Node::bind(bind, &identity, &policy)?);
    let started = Instant::now();
    let result = tokio::select! {
        () = stopped(&mut stop) => Ok(Summary {
            cancelled: true,
            elapsed: started.elapsed(),
            ..Summary::default()
        }),
        r = watch_remote(&node, &identity, &policy, target, auth, sink, input, &ev) => r,
    };
    close_nodes(&[node]).await;
    result
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
) -> Result<Summary, NetError> {
    // 走中继时的本机转发器要保持到会话结束。
    let (link, _shim, route) = match target {
        Target::Addr { addr, pin } => {
            (node.connect(addr, pin.as_ref()).await?, None, Route::Direct)
        }
        Target::Device {
            server,
            device,
            policy,
        } => {
            let dialed = dial(node, &server, &device, policy).await?;
            (dialed.link, dialed.shim, dialed.path)
        }
    };
    ev.send(ViewEvent::Connected {
        peer: to_hex(link.peer_fingerprint()),
        remote: link.remote_addr(),
        route,
    });
    let started = Instant::now();
    let report = controller::run(
        link,
        ControllerOptions {
            session: session(identity, policy, auth),
            deadline_us: FRAME_DEADLINE_US,
            duration: None,
        },
        Box::new(sink),
        input,
        None,
    )
    .await?;
    Ok(Summary {
        end: report.end,
        cancelled: false,
        rtt: Some(report.rtt),
        video_bytes: report.video_bytes,
        elapsed: started.elapsed(),
    })
}

#[cfg(test)]
mod tests;
