//! PeerDesk 手机端的 Rust 核心（Android 和 iOS 共用）。界面用原生写，通过很薄的一层绑定调这里（见
//! `docs/research/R-android.md`）。
//!
//! - [`Session`]：一次访问（连上、断线重连、走中继时找机会切直连），用的是桌面端同一套驱动
//!   `peerdesk_net::viewer`。会话在自己的线程和运行时里跑，界面只拿到一个句柄。
//! - 事件用队列交给界面：界面在后台线程调 [`Session::next_event`] 取（状态、质量、错误），
//!   不从 Rust 的线程回调界面，生命周期简单。
//! - [`gesture`]：触控到鼠标的状态机。
//! - 解码器由平台层提供（Android 的 MediaCodec），这里不带 OpenH264。

#[cfg(target_os = "android")]
mod android;
pub mod decoder_setup;
pub mod gesture;
pub mod keymap;

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use peerdesk_core::auth::Auth;
use peerdesk_core::crypto::Policy;
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::Caps;
use peerdesk_core::session::SessionConfig;
use peerdesk_core::signal::parse_code;
use peerdesk_net::clipsync::ClipboardHandle;
use peerdesk_net::controller::{LinkHook, LinkQuality};
use peerdesk_net::recorder::{RecordSetup, Retention};
use peerdesk_net::rendezvous::{Lookup, PathPolicy, Server};
use peerdesk_net::viewer::{self, ViewEnv, ViewTarget};
use peerdesk_net::{Identity, NetError, Node};
use tokio::sync::watch;

pub use peerdesk_net::controller::{DecoderFactory, ScreenSink};
pub use peerdesk_net::viewer::Summary;

/// 服务器地址没写端口时用的端口（和桌面端一样）。
pub const DEFAULT_PORT: u16 = 4433;
/// 输入事件队列的长度：满了就丢（界面那边按键不会快到这个程度）。
const INPUT_QUEUE: usize = 1024;

/// 每次会话（重连、升级直连之后）造一个画面去向。
pub type SinkFactory = Arc<dyn Fn() -> Box<dyn ScreenSink> + Send + Sync>;

/// 发给界面的事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// 连上了，开始握手。`route` 是直连还是中继，`detail` 是路径的说明（种类和往返时间）。
    Connected {
        /// 直连还是中继。
        route: String,
        /// 路径说明。
        detail: String,
    },
    /// 握手完成：对方的系统和电脑名。
    Peer {
        /// 系统（Windows、macOS、Linux，老版本为未知）。
        os: String,
        /// 电脑名（老版本为空）。
        name: String,
    },
    /// 每秒一次的链路质量。
    Quality(LinkQuality),
    /// 网络断了，正在重连（第几次，原因）。
    Reconnecting {
        /// 第几次。
        attempt: u32,
        /// 原因。
        why: String,
    },
    /// 服务器的通知（例如免费额度用完），访问随之结束。
    Notice(String),
    /// 访问结束。`ok` 为假时 `text` 是失败的原因（白话），为真时是结束的原因。
    Ended {
        /// 是不是正常结束（包括自己断开）。
        ok: bool,
        /// 原因。
        text: String,
    },
}

impl Event {
    /// 写成一行 JSON 给界面（Kotlin、Swift 解析）。字段名固定，界面按 `type` 分。
    #[must_use]
    pub fn to_json(&self) -> String {
        match self {
            Self::Connected { route, detail } => format!(
                "{{\"type\":\"connected\",\"route\":{},\"detail\":{}}}",
                json_string(route),
                json_string(detail)
            ),
            Self::Peer { os, name } => format!(
                "{{\"type\":\"peer\",\"os\":{},\"name\":{}}}",
                json_string(os),
                json_string(name)
            ),
            Self::Quality(q) => format!(
                "{{\"type\":\"quality\",\"rtt_ms\":{},\"loss_permille\":{},\"queue_ms\":{},\"fps\":{},\
                 \"recv_kbps\":{},\"decode_ms\":{},\"width\":{},\"height\":{},\"router_ms\":{},\
                 \"decoder\":{},\"fec\":{}}}",
                q.rtt_ms,
                q.loss_permille,
                q.queue_ms,
                q.fps,
                q.recv_kbps,
                q.decode_ms,
                q.width,
                q.height,
                q.router_ms
                    .map_or_else(|| "null".to_owned(), |ms| ms.to_string()),
                json_string(&q.decoder),
                json_string(q.fec)
            ),
            Self::Reconnecting { attempt, why } => format!(
                "{{\"type\":\"reconnecting\",\"attempt\":{attempt},\"why\":{}}}",
                json_string(why)
            ),
            Self::Notice(text) => format!(
                "{{\"type\":\"notice\",\"text\":{}}}",
                json_string(text)
            ),
            Self::Ended { ok, text } => format!(
                "{{\"type\":\"ended\",\"ok\":{ok},\"text\":{}}}",
                json_string(text)
            ),
        }
    }
}

impl From<viewer::ViewEvent> for Event {
    fn from(event: viewer::ViewEvent) -> Self {
        match event {
            viewer::ViewEvent::Connected { route, detail, .. } => Self::Connected {
                route: format!("{route:?}"),
                detail,
            },
            viewer::ViewEvent::Peer(os, name) => Self::Peer {
                os: format!("{os:?}"),
                name,
            },
            viewer::ViewEvent::Quality(q) => Self::Quality(q),
            viewer::ViewEvent::Reconnecting { attempt, why } => Self::Reconnecting { attempt, why },
            viewer::ViewEvent::Notice(text) => Self::Notice(text),
            // 手机端第一版不同步剪贴板（ViewEnv::clipboard 给的是 None），不会有这个事件；
            // 真来了也只是提示，按通知处理
            viewer::ViewEvent::Clipboard(note) => Self::Notice(format!("{note:?}")),
        }
    }
}

/// JSON 字符串（带引号），控制字符、引号、反斜杠转义。
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if u32::from(c) < 0x20 => out.push_str(&format!("\\u{:04x}", u32::from(c))),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 按用户填的服务器地址和设备码，得到要连的目标。服务器可以写域名，没写端口用 [`DEFAULT_PORT`]。
///
/// # Errors
/// 设备码不是 9 位数字、服务器地址解析不了（原因写成白话）。
pub fn device_target(server: &str, code: &str) -> Result<ViewTarget, String> {
    let code = parse_code(code).ok_or_else(|| "设备码应为 9 位数字".to_owned())?;
    let server = server.trim();
    if server.is_empty() {
        return Err("请填写服务器地址".to_owned());
    }
    let with_port = if server.parse::<SocketAddr>().is_ok()
        || server
            .rsplit_once(':')
            .is_some_and(|(h, p)| !h.ends_with(']') && !h.contains(':') && p.parse::<u16>().is_ok())
    {
        server.to_owned()
    } else if server.starts_with('[') {
        format!("{server}:{DEFAULT_PORT}")
    } else if server.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv6()) {
        format!("[{server}]:{DEFAULT_PORT}")
    } else {
        format!("{server}:{DEFAULT_PORT}")
    };
    let addr = with_port
        .to_socket_addrs()
        .map_err(|e| format!("找不到服务器 {server}：{e}"))?
        .next()
        .ok_or_else(|| format!("找不到服务器 {server}"))?;
    Ok(ViewTarget::Device {
        server: Server { addr, pin: None },
        device: Lookup::Code(code),
        policy: PathPolicy::Auto,
        account: String::new(),
    })
}

/// 手机端给访问驱动的东西。
struct MobileEnv {
    events: mpsc::Sender<Event>,
    sink: SinkFactory,
    decoder: Option<DecoderFactory>,
    view: watch::Receiver<(u16, u16)>,
    data_dir: PathBuf,
}

/// 手机端控制端声明的能力。第一版不带剪贴板、声音、文件管理（界面还没有），其余和桌面端一样。
fn caps() -> Caps {
    Caps::H264
        .with(Caps::FEC_RS)
        .with(Caps::HOST_STATS)
        .with(Caps::VIEW_SIZE)
        .with(Caps::TRANSPORT_CC)
        .with(Caps::PROBE_TRAIN)
        .with(Caps::PROBE_ID)
}

impl ViewEnv for MobileEnv {
    fn event(&self, event: viewer::ViewEvent) {
        let _ = self.events.send(Event::from(event));
    }

    fn sink(&self) -> Box<dyn ScreenSink> {
        (self.sink)()
    }

    fn session(&self, identity: &Identity, policy: &Policy, auth: Auth) -> SessionConfig {
        SessionConfig {
            device_id: identity.device_id(),
            region: String::new(),
            caps: caps(),
            policy: policy.clone(),
            auth,
            binding: Vec::new(),
        }
    }

    fn clipboard(&self) -> Option<ClipboardHandle> {
        None
    }

    fn audio(&self) -> Option<peerdesk_net::audio::AudioOut> {
        None
    }

    fn view(&self) -> Option<watch::Receiver<(u16, u16)>> {
        Some(self.view.clone())
    }

    fn on_link(&self) -> Option<LinkHook> {
        None
    }

    fn record(&self) -> Option<RecordSetup> {
        Some(RecordSetup {
            dir: self.data_dir.join("records"),
            retention: Retention::default(),
            program_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
            commit: None,
        })
    }

    fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn decoder(&self) -> Option<DecoderFactory> {
        self.decoder.clone()
    }
}

/// 一次访问的句柄。丢弃时断开。
#[derive(Debug)]
pub struct Session {
    /// 放在锁里：界面可能从不同的线程调句柄（取事件在后台线程、触控在界面线程）。
    events: Mutex<mpsc::Receiver<Event>>,
    input: tokio::sync::mpsc::Sender<InputEvent>,
    stop: watch::Sender<bool>,
    view: watch::Sender<(u16, u16)>,
}

impl Session {
    /// 开始一次访问：在自己的线程里连接并跑会话。`data_dir` 放身份（第一次自动生成）、集群节点缓存和会话记录；
    /// `password` 为空表示不带密码。结果和过程都从 [`Self::next_event`] 取，最后一定有一个 [`Event::Ended`]。
    #[must_use]
    pub fn start(
        data_dir: PathBuf,
        target: ViewTarget,
        password: &str,
        sink: SinkFactory,
        decoder: Option<DecoderFactory>,
    ) -> Self {
        Self::start_with(data_dir, move || Ok(target), password, sink, decoder)
    }

    /// 同 [`Self::start`]，但目标在会话线程里才算出来：按服务器名连接时要查 DNS，不能卡住调用的线程
    /// （手机端是界面线程）。`resolve` 失败时，第一个事件就是带原因的 [`Event::Ended`]。
    #[must_use]
    pub fn start_with(
        data_dir: PathBuf,
        resolve: impl FnOnce() -> Result<ViewTarget, String> + Send + 'static,
        password: &str,
        sink: SinkFactory,
        decoder: Option<DecoderFactory>,
    ) -> Self {
        let (tx, events) = mpsc::channel();
        let (input, input_rx) = tokio::sync::mpsc::channel(INPUT_QUEUE);
        let (stop, stop_rx) = watch::channel(false);
        let (view, view_rx) = watch::channel((0, 0));
        let auth = if password.is_empty() {
            Auth::Open
        } else {
            Auth::Password(password.to_owned())
        };
        let env = MobileEnv {
            events: tx.clone(),
            sink,
            decoder,
            view: view_rx,
            data_dir,
        };
        let spawned = std::thread::Builder::new()
            .name("peerdesk-session".to_owned())
            .spawn(move || {
                let event = match resolve() {
                    Ok(target) => ended(run(target, auth, env, input_rx, stop_rx)),
                    Err(text) => Event::Ended { ok: false, text },
                };
                let _ = tx.send(event);
            });
        if let Err(e) = spawned {
            // 线程都起不来：直接告诉界面，不装作在连
            return Self::failed(format!("启动会话失败：{e}"));
        }
        Self {
            events: Mutex::new(events),
            input,
            stop,
            view,
        }
    }

    /// 一个还没开始就失败了的访问（例如设备码填错）：第一个事件就是 [`Event::Ended`]，原因是 `text`。
    /// 界面照常取事件、显示原因，不用另写一条出错的路径。
    #[must_use]
    pub fn failed(text: String) -> Self {
        let (tx, events) = mpsc::channel();
        let _ = tx.send(Event::Ended { ok: false, text });
        let (input, _) = tokio::sync::mpsc::channel(1);
        let (stop, _) = watch::channel(false);
        let (view, _) = watch::channel((0, 0));
        Self {
            events: Mutex::new(events),
            input,
            stop,
            view,
        }
    }

    /// 取下一个事件，最多等 `timeout`；没有就返回 `None`。[`Event::Ended`] 之后不会再有事件。
    #[must_use]
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recv_timeout(timeout)
            .ok()
    }

    /// 发一个输入事件。队列满或会话已结束时返回 `false`（事件丢弃）。
    pub fn send(&self, event: InputEvent) -> bool {
        self.input.try_send(event).is_ok()
    }

    /// 显示画面的区域变了（像素）：被控端按这个大小编码，手机屏幕小就不用传大画面。0×0 表示不限。
    pub fn set_view(&self, width: u16, height: u16) {
        let _ = self.view.send_if_modified(|v| {
            let changed = *v != (width, height);
            *v = (width, height);
            changed
        });
    }

    /// 断开。之后会收到 [`Event::Ended`]。
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 句柄（堆上对象的地址）换成交给 Java 的 `long`：按位原样放进去，不当数值换算。
///
/// 为什么不用 `i64::try_from`：Android 11 起堆指针的最高字节带标记（Scudo 的 pointer tagging，例如 `0xb4…`），
/// 当成无符号数大于 `i64::MAX`，换算会失败（第一版就是因此返回 0，界面报“显示界面不可用”）。
#[must_use]
pub fn handle_from_addr(addr: usize) -> i64 {
    let bits = u64::try_from(addr).unwrap_or(0);
    i64::from_ne_bytes(bits.to_ne_bytes())
}

/// [`handle_from_addr`] 的反过程。0 是“没有句柄”，返回 `None`。
#[must_use]
pub fn addr_from_handle(handle: i64) -> Option<usize> {
    let bits = u64::from_ne_bytes(handle.to_ne_bytes());
    usize::try_from(bits).ok().filter(|&a| a != 0)
}

/// 手势状态机的输出：输入事件发给被控端，缩放、平移、震动交回界面（一行 JSON 数组，没有就是 `None`）。
pub fn apply(session: &Session, actions: Vec<gesture::Action>) -> Option<String> {
    let mut view = Vec::new();
    for action in actions {
        match action {
            gesture::Action::Input(event) => {
                let _ = session.send(event);
            }
            gesture::Action::Zoom { factor, cx, cy } => view.push(format!(
                "{{\"type\":\"zoom\",\"factor\":{factor},\"cx\":{cx},\"cy\":{cy}}}"
            )),
            gesture::Action::Pan { dx, dy } => {
                view.push(format!("{{\"type\":\"pan\",\"dx\":{dx},\"dy\":{dy}}}"));
            }
            gesture::Action::Haptic => view.push("{\"type\":\"haptic\"}".to_owned()),
            gesture::Action::Tap { x, y, button } => view.push(format!(
                "{{\"type\":\"tap\",\"x\":{x},\"y\":{y},\"button\":{button}}}"
            )),
        }
    }
    (!view.is_empty()).then(|| format!("[{}]", view.join(",")))
}

fn ended(result: Result<Summary, NetError>) -> Event {
    match result {
        Ok(summary) => Event::Ended {
            ok: true,
            text: if summary.cancelled {
                "已断开".to_owned()
            } else {
                summary.end
            },
        },
        Err(e) => Event::Ended {
            ok: false,
            text: e.to_string(),
        },
    }
}

fn run(
    target: ViewTarget,
    auth: Auth,
    env: MobileEnv,
    input: tokio::sync::mpsc::Receiver<InputEvent>,
    mut stop: watch::Receiver<bool>,
) -> Result<Summary, NetError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| NetError::Io(format!("启动运行时失败：{e}")))?;
    let policy = Policy::recommended();
    let identity = Identity::load_or_create(&env.data_dir)?;
    runtime.block_on(async move {
        let bind = viewer::bind_for(&target);
        // 系统不支持 IPv6 时退回 IPv4
        let v4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let node = Arc::new(Node::bind_first(&[bind, v4], &identity, &policy)?);
        let env: Arc<dyn ViewEnv> = Arc::new(env);
        let started = Instant::now();
        let result = tokio::select! {
            () = stopped(&mut stop) => Ok(Summary {
                cancelled: true,
                elapsed: started.elapsed(),
                ..Summary::default()
            }),
            r = viewer::drive(&node, &identity, &policy, target, auth, env, input) => r,
        };
        node.close();
        let _ = tokio::time::timeout(Duration::from_millis(500), node.wait_idle()).await;
        result
    })
}

/// 等到界面要求断开（句柄被丢弃也算）。
async fn stopped(stop: &mut watch::Receiver<bool>) {
    loop {
        if *stop.borrow_and_update() {
            return;
        }
        if stop.changed().await.is_err() {
            return;
        }
    }
}
