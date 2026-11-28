//! 控制端的一次“访问”：从点连接到最终断开。桌面界面和手机端共用。
//!
//! 一次访问里可能有好几次会话（几条连接）：
//! - 网络断了自动重连，最多 [`RECONNECT_TRIES`] 次；
//! - 走中继时在后台反复试直连，通了就在直连上重新开始（不算重连）；
//! - 中继发来通知（例如免费额度用完）就结束，并把原因告诉用户。
//!
//! 和界面有关的东西（画面画到哪、剪贴板、声音、显示区域、文件管理要的连接、会话记录的开关）都由调用方通过
//! [`ViewEnv`] 提供；这里只管连接和会话的先后。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use peerdesk_core::auth::Auth;
use peerdesk_core::crypto::Policy;
use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::{Caps, PeerOs};
use peerdesk_core::record::{Previous, PreviousReason};
use peerdesk_core::session::SessionConfig;
use tokio::sync::{mpsc, watch};

use crate::audio::AudioOut;
use crate::clipsync::{ClipNote, ClipboardHandle};
use crate::controller::{self, ControllerOptions, LinkHook, LinkQuality, ScreenSink};
use crate::identity::to_hex;
use crate::recorder::{new_visit_id, RecordSetup, ViewRecordSetup};
use crate::rendezvous::{
    dial_via, fetch_nodes, Dialed, Lookup, Path as Route, PathPolicy, Server, Shim,
};
use crate::stream::FRAME_DEADLINE_US;
use crate::{Fingerprint, Identity, NetError, Node};

/// 网络断开后最多自动重连几次。
pub const RECONNECT_TRIES: u32 = 3;
/// 两次重连之间等多久。
const RECONNECT_GAP: Duration = Duration::from_secs(1);
/// 多久看一次中继有没有发来通知。
const NOTICE_POLL: Duration = Duration::from_millis(500);
/// 走中继时，过多久试一次直连（中继是成本，直连更快）：先试得勤一些，之后每 5 分钟一次。
/// 打洞失败多半是 NAT 本身的限制，频繁重试没用，也给服务器添负担。
const UPGRADE_SCHEDULE: [Duration; 3] = [
    Duration::from_secs(15),
    Duration::from_secs(45),
    Duration::from_secs(120),
];
/// 之后每隔多久试一次。
const UPGRADE_EVERY: Duration = Duration::from_secs(300);
/// [`PathPolicy::RelayFirst`]：先走中继出画面，这么久之后就试直连。
const RELAY_FIRST_UPGRADE: Duration = Duration::from_secs(1);
/// 集群节点缓存文件：上次从服务器得到的节点列表，服务器崩了也能换到别的节点。
pub const NODES_FILE: &str = "cluster-nodes.txt";

/// 连谁。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewTarget {
    /// 直接连这个地址；给了指纹就核对。
    Addr {
        /// 地址。
        addr: SocketAddr,
        /// 要核对的指纹。
        pin: Option<Fingerprint>,
    },
    /// 通过服务器找到这台设备（按设备码或指纹），先试直连，不通走中继。连接时核对设备指纹。
    Device {
        /// 信令服务器。
        server: Server,
        /// 被控端。
        device: Lookup,
        /// 路径策略。
        policy: PathPolicy,
        /// 会员令牌（空表示免费用户）。
        account: String,
    },
}

/// 一次访问的结果。
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

/// 访问过程中发给界面的事件（结束由 [`drive`] 的返回值给出）。
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
    /// 网络断了，正在换通道重连（第几次，断开的原因）。
    Reconnecting {
        /// 第几次重连。
        attempt: u32,
        /// 断开的原因。
        why: String,
    },
    /// 服务器的通知（例如免费额度用完），访问随之结束。
    Notice(String),
    /// 剪贴板事件（内容太大没有同步等），界面短暂提示，会话继续。
    Clipboard(ClipNote),
    /// 每秒一次的链路质量（往返时间、丢包、排队），工具条据此提示网络状况。
    Quality(LinkQuality),
    /// 握手完成，得知被控端的系统和电脑名（老版本为未知、空）。
    Peer(PeerOs, String),
}

/// 调用方（桌面界面、手机端）提供的东西。每次会话（重连、升级直连之后）都重新要一遍。
pub trait ViewEnv: Send + Sync {
    /// 发一个事件给界面。
    fn event(&self, event: ViewEvent);
    /// 这一次会话的画面去向。
    fn sink(&self) -> Box<dyn ScreenSink>;
    /// 握手配置（本端的能力、认证方式）。
    fn session(&self, identity: &Identity, policy: &Policy, auth: Auth) -> SessionConfig;
    /// 本机剪贴板；`None` 表示不同步剪贴板。
    fn clipboard(&self) -> Option<ClipboardHandle>;
    /// 声音去向；`None` 表示本机不播放声音。
    fn audio(&self) -> Option<AudioOut>;
    /// 显示画面的区域（像素），变了被控端按它编码；`None` 表示不告诉被控端。
    fn view(&self) -> Option<watch::Receiver<(u16, u16)>>;
    /// 握手完成后交出连接（文件管理用），会话结束时交出 `None`。
    fn on_link(&self) -> Option<LinkHook>;
    /// 会话记录的设置；开关关着时为 `None`。
    fn record(&self) -> Option<RecordSetup>;
    /// 数据目录：身份、集群节点缓存。
    fn data_dir(&self) -> &Path;
    /// H.264 解码器从哪来；默认 `None`，用控制端默认的（见 [`ControllerOptions::decoder`]）。
    fn decoder(&self) -> Option<controller::DecoderFactory> {
        None
    }
}

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

/// 读缓存的集群节点（每行 `地址 指纹`），读不懂的行跳过。
#[must_use]
pub fn load_nodes(dir: &Path) -> Vec<Server> {
    std::fs::read_to_string(dir.join(NODES_FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (addr, pin) = line.split_once(' ')?;
            Some(Server {
                addr: addr.parse().ok()?,
                pin: Some(crate::identity::parse_hex(pin.trim()).ok()?),
            })
        })
        .collect()
}

/// 存集群节点缓存。
///
/// # Errors
/// 写文件失败。
pub fn save_nodes(dir: &Path, nodes: &[Server]) -> std::io::Result<()> {
    let text: String = nodes
        .iter()
        .filter_map(|n| Some(format!("{} {}\n", n.addr, to_hex(n.pin.as_ref()?))))
        .collect();
    std::fs::write(dir.join(NODES_FILE), text)
}

/// 控制端绑定的地址：直接连地址时跟它同一地址族；通过服务器连接时用双栈端口
/// （IPv4 和 IPv6 的候选路径都能探测）。
#[must_use]
pub const fn bind_for(target: &ViewTarget) -> SocketAddr {
    match target {
        ViewTarget::Addr { addr, .. } if addr.is_ipv6() => {
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0)
        }
        ViewTarget::Addr { .. } => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        ViewTarget::Device { .. } => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    }
}

/// 控制端的一次访问：访问号，以及上一份记录的会话号和“为什么有下一份”。
///
/// 升级直连、断线重连会换新连接和新会话号；新的那份会话记录的文件头写上一份的会话号和原因，两份能接上。
#[derive(Debug)]
pub struct Visit {
    id: Option<[u8; 16]>,
    last_session: Option<[u8; 16]>,
    pending: Option<PreviousReason>,
}

impl Visit {
    /// 开始一次访问。生成不出访问号（系统随机数不可用）时写日志，这次访问不记录。
    #[must_use]
    pub fn new() -> Self {
        let id = new_visit_id()
            .inspect_err(|e| log::warn!("生成访问号失败，这次访问不记录会话：{e}"))
            .ok();
        Self {
            id,
            last_session: None,
            pending: None,
        }
    }

    /// 一次新的会话（新连接握手完成）开始：交出这份记录的设置，并记下它的会话号给下一份用。
    /// 记录关着（`setup` 为 `None`）、或没有访问号时为 `None`。
    pub fn begin(
        &mut self,
        session_id: Option<[u8; 16]>,
        setup: Option<RecordSetup>,
    ) -> Option<ViewRecordSetup> {
        let previous = self
            .last_session
            .zip(self.pending.take())
            .map(|(session_id, reason)| Previous { session_id, reason });
        self.last_session = session_id;
        Some(ViewRecordSetup {
            setup: setup?,
            visit_id: self.id?,
            previous,
        })
    }

    /// 走中继时打通了直连，要在直连上重新开始。
    pub const fn note_upgrade(&mut self) {
        self.pending = Some(PreviousReason::Upgraded);
    }

    /// 网络断了，要重连。
    pub const fn note_lost(&mut self) {
        self.pending = Some(PreviousReason::Reconnected);
    }
}

impl Default for Visit {
    fn default() -> Self {
        Self::new()
    }
}

/// 一次会话的结果：正常结束、网络断了（可以重连）、服务器通知（例如免费额度用完）。
enum Outcome {
    Done(Summary),
    Lost(String),
    Notice(String),
    /// 走中继时在后台打通了直连：结束这一次（中继）会话，在这条直连上重新开始。
    Upgrade(Box<Dialed>),
}

/// 走中继时在后台反复试直连，通了就交出这条连接（不返回就是一直没通）。
async fn find_direct(
    node: &Node,
    servers: &[Server],
    device: &Lookup,
    policy: PathPolicy,
    account: &str,
) -> Dialed {
    let first = if policy == PathPolicy::RelayFirst {
        vec![RELAY_FIRST_UPGRADE]
    } else {
        UPGRADE_SCHEDULE.to_vec()
    };
    let mut waits = first.into_iter().chain(std::iter::repeat(UPGRADE_EVERY));
    loop {
        let wait = waits.next().unwrap_or(UPGRADE_EVERY);
        tokio::time::sleep(wait).await;
        match dial_via(node, servers, device, PathPolicy::DirectOnly, account).await {
            Ok(dialed) if dialed.shim.is_none() => {
                crate::note!(
                    info,
                    "控制：直连打通了（{}），从中继切过去",
                    dialed.route.label()
                );
                return dialed;
            }
            Ok(_) => {}
            Err(e) => crate::note!(info, "控制：试直连没通（{e}），继续走中继"),
        }
    }
}

/// 跑一次访问：连上、跑会话；网络断了重连，走中继时打通直连就切过去，直到正常结束、
/// 收到服务器通知或重连次数用完。`input` 里的事件转给当前这一次会话（重连后转给新会话）。
///
/// # Errors
/// 第一次就连不上，或者重连次数用完。
pub async fn drive(
    node: &Node,
    identity: &Identity,
    policy: &Policy,
    target: ViewTarget,
    auth: Auth,
    env: Arc<dyn ViewEnv>,
    input: mpsc::Receiver<InputEvent>,
) -> Result<Summary, NetError> {
    let current: Arc<Mutex<Option<mpsc::Sender<InputEvent>>>> = Arc::new(Mutex::new(None));
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
    let mut upgraded: Option<Box<Dialed>> = None;
    // 一次访问（从点连接到最终断开）：升级直连和断线重连换了连接，记录靠它串起来
    let visit = Mutex::new(Visit::new());
    let result = loop {
        let (tx, rx) = mpsc::channel(256);
        *current.lock().unwrap_or_else(PoisonError::into_inner) = Some(tx);
        let outcome = one_session(
            node,
            identity,
            policy,
            &target,
            &auth,
            &env,
            rx,
            upgraded.take(),
            &visit,
        )
        .await;
        match outcome {
            Ok(Outcome::Done(summary)) => break Ok(summary),
            // 换到直连：不算重连次数
            Ok(Outcome::Upgrade(dialed)) => {
                visit
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .note_upgrade();
                upgraded = Some(dialed);
            }
            Ok(Outcome::Notice(text)) => {
                env.event(ViewEvent::Notice(text.clone()));
                break Ok(Summary {
                    end: text,
                    elapsed: started.elapsed(),
                    ..Summary::default()
                });
            }
            Ok(Outcome::Lost(why)) if attempt < RECONNECT_TRIES => {
                attempt += 1;
                visit
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .note_lost();
                crate::note!(warn, "控制：网络断开（{why}），第 {attempt} 次重连");
                env.event(ViewEvent::Reconnecting { attempt, why });
                tokio::time::sleep(RECONNECT_GAP).await;
            }
            Ok(Outcome::Lost(why)) => break Err(NetError::Closed(why)),
            // 第一次就连不上：直接报错；重连时连不上：再试，用完次数再报错。
            Err(e) if attempt > 0 && attempt < RECONNECT_TRIES => {
                attempt += 1;
                env.event(ViewEvent::Reconnecting {
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
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn one_session(
    node: &Node,
    identity: &Identity,
    policy: &Policy,
    target: &ViewTarget,
    auth: &Auth,
    env: &Arc<dyn ViewEnv>,
    input: mpsc::Receiver<InputEvent>,
    upgraded: Option<Box<Dialed>>,
    visit: &Mutex<Visit>,
) -> Result<Outcome, NetError> {
    let data_dir = env.data_dir();
    // 走中继时，会话中在后台试直连（用户没要求只走中继时）
    let mut direct_search = None;
    // 走中继时的本机转发器要保持到会话结束。
    let (link, shim, route, detail): (_, Option<Shim>, _, _) = match target {
        ViewTarget::Addr { addr, pin } => (
            node.connect(*addr, pin.as_ref()).await?,
            None,
            Route::Direct,
            String::new(),
        ),
        ViewTarget::Device {
            server,
            device,
            policy,
            account,
        } => {
            let servers = server_order(*server, load_nodes(data_dir));
            let dialed = match upgraded {
                Some(dialed) => *dialed,
                None => dial_via(node, &servers, device, *policy, account).await?,
            };
            if dialed.shim.is_some() && policy.upgrades() {
                direct_search = Some((servers.clone(), *device, *policy, account.clone()));
            }
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
    crate::note!(
        info,
        "控制：已连上 {}（{}），路径 {route:?} {detail}",
        link.remote_addr(),
        to_hex(link.peer_fingerprint())
            .get(..16)
            .unwrap_or_default()
    );
    env.event(ViewEvent::Connected {
        peer: to_hex(link.peer_fingerprint()),
        remote: link.remote_addr(),
        route,
        detail,
    });
    let started = Instant::now();
    // 会话记录：会话号在 link 交给会话之前取
    let record = visit
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .begin(link.session_id().ok(), env.record());
    let session = controller::run(
        link,
        ControllerOptions {
            decoder: env.decoder(),
            record,
            session: env.session(identity, policy, auth.clone()),
            deadline_us: FRAME_DEADLINE_US,
            duration: None,
            clipboard: env.clipboard(),
            on_established: {
                let env = Arc::clone(env);
                Some(controller::EstablishedHook(Arc::new(
                    move |caps: Caps, name: &str| {
                        env.event(ViewEvent::Peer(caps.os(), name.to_owned()));
                    },
                )))
            },
            on_link: env.on_link(),
            on_quality: {
                let env = Arc::clone(env);
                Some(controller::QualityHook(Arc::new(move |q| {
                    env.event(ViewEvent::Quality(q));
                })))
            },
            audio: env.audio(),
            view: env.view(),
        },
        env.sink(),
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
    let upgrade = async {
        match &direct_search {
            Some((servers, device, policy, account)) => {
                find_direct(node, servers, device, *policy, account).await
            }
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        // 旧连接随会话一起放开；被控端看到同一个控制端的新连接就结束旧会话（见 host_main）
        dialed = upgrade => Ok(Outcome::Upgrade(Box::new(dialed))),
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
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::recorder::Retention;

    fn setup() -> Option<RecordSetup> {
        Some(RecordSetup {
            dir: std::env::temp_dir().join("peerdesk-viewer-visit"),
            retention: Retention::default(),
            program_version: None,
            commit: None,
        })
    }

    #[test]
    fn a_visit_chains_its_sessions() {
        let mut visit = Visit::new();
        // 第一份：没有上一份
        let first = visit.begin(Some([1; 16]), setup()).unwrap();
        assert_eq!(first.previous, None);
        assert_ne!(first.visit_id, [0; 16]);
        // 升级直连：第二份接着第一份
        visit.note_upgrade();
        let second = visit.begin(Some([2; 16]), setup()).unwrap();
        assert_eq!(second.visit_id, first.visit_id, "同一次访问");
        assert_eq!(
            second.previous,
            Some(Previous {
                session_id: [1; 16],
                reason: PreviousReason::Upgraded
            })
        );
        // 断线重连：第三份接着第二份；原因只用一次
        visit.note_lost();
        let third = visit.begin(Some([3; 16]), setup()).unwrap();
        assert_eq!(third.previous.unwrap().session_id, [2; 16]);
        assert_eq!(third.previous.unwrap().reason, PreviousReason::Reconnected);
        let again = visit.begin(Some([4; 16]), setup()).unwrap();
        assert_eq!(again.previous, None);
        // 会话号导不出来：下一份没有上一份可接
        visit.begin(None, setup()).unwrap();
        visit.note_lost();
        assert_eq!(visit.begin(Some([5; 16]), setup()).unwrap().previous, None);
        // 记录关着：不给设置，但串联的状态照常走
        visit.note_lost();
        assert!(visit.begin(Some([6; 16]), None).is_none());
        assert_eq!(
            visit.begin(Some([7; 16]), setup()).unwrap().previous,
            None,
            "原因在关着的那一份上用掉了"
        );
        // 没有访问号：不记录
        let mut broken = Visit {
            id: None,
            ..Visit::default()
        };
        assert!(broken.begin(Some([8; 16]), setup()).is_none());
    }

    #[test]
    fn helpers_explain_and_order() {
        assert!(network_lost("timed out") && network_lost("connection lost"));
        assert!(!network_lost("对端结束会话：timed out") && !network_lost("用户结束"));
        assert!(free_limit_text(0).contains("约 1 分钟"));
        assert!(free_limit_text(61).contains("约 2 分钟"));
        let s = |port: u16| Server {
            addr: SocketAddr::from(([1, 2, 3, 4], port)),
            pin: None,
        };
        let order = server_order(s(1), vec![s(2), s(1), s(3), s(2)]);
        assert_eq!(order, vec![s(1), s(2), s(3)]);
        let device = ViewTarget::Device {
            server: s(1),
            device: Lookup::Device([0; 32]),
            policy: PathPolicy::Auto,
            account: String::new(),
        };
        assert!(bind_for(&device).is_ipv6());
        let v4 = ViewTarget::Addr {
            addr: s(1).addr,
            pin: None,
        };
        let v6 = ViewTarget::Addr {
            addr: "[::1]:1".parse().unwrap(),
            pin: None,
        };
        assert!(bind_for(&v4).is_ipv4() && bind_for(&v6).is_ipv6());
    }
}
