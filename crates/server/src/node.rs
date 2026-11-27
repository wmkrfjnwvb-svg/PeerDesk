//! 一个运行中的服务端节点：集群心跳线程加管理接口线程。
//!
//! 这里是服务端唯一做 IO 的地方。决策全部来自核心库的 [`Cluster`]，
//! 这一层只负责收发字节和计时。

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use peerdesk_core::billing::{
    account_hex, issue, issue_enroll, month_of, parse_account, ENROLL_MAX_DAYS,
};
use peerdesk_core::cluster::{self, Cluster, ClusterConfig, ClusterMsg, NodeInfo};
use peerdesk_core::config::{ServerConfig, SignalConfig};
use peerdesk_core::crypto::Policy;
use peerdesk_core::region::NodeId;
use peerdesk_core::relay::FreePolicy;
use peerdesk_core::{Error, Time};
use peerdesk_net::identity::to_hex;
use peerdesk_net::service::{Accounts, Directory, RelayOptions, RelayService, SignalService};
use peerdesk_net::{Fingerprint, Identity, NetError};

use crate::accounting::{hex, Accounting, ClusterDirectory};
use crate::http::{self, Admin, Api, Response, MAX_REQUEST_HEAD};
use crate::status;

/// 线程轮询停止标志和超时的间隔。
const POLL: Duration = Duration::from_millis(20);
/// 读取一个 HTTP 请求头的时间上限。
const HTTP_READ_TIMEOUT: Duration = Duration::from_secs(2);
/// 发完响应后等待对方收尾的时间上限。
const DRAIN_TIMEOUT: Duration = Duration::from_millis(200);
/// 发完响应后最多再读掉这么多字节。
const MAX_DRAIN: usize = 256 * 1024;

/// 带认证的集群消息允许的时钟偏差。节点之间需要大致对时。
const MAX_CLOCK_SKEW_MS: u64 = 30_000;

fn wall_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// 信令服务的对外信息。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalInfo {
    /// 信令实际监听的地址。
    pub addr: SocketAddr,
    /// 服务器的证书指纹，客户端用 `--server-pin` 核对。
    pub fingerprint: Fingerprint,
    /// 告诉客户端的中继地址；`None` 表示没有开启中继。
    pub relay: Option<SocketAddr>,
}

/// 信令和中继。它们是异步的，跑在自己的运行时上；其余部分仍然是普通线程。
struct Services {
    info: SignalInfo,
    signal: SignalService,
    relay: Option<Arc<RelayService>>,
    /// 放在最后：先停服务，再关运行时。
    _runtime: tokio::runtime::Runtime,
}

impl Services {
    fn start(
        cfg: &SignalConfig,
        policy: &Policy,
        options: RelayOptions,
        directory: ClusterDirectory,
        accounts: Accounts,
    ) -> Result<Self, Error> {
        let fail = |e: NetError| Error::Config(format!("启动信令服务失败：{e}"));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| io_error("启动异步运行时失败", &e))?;
        let identity = Identity::load_or_create(&cfg.data_dir).map_err(fail)?;
        let (signal, relay, addr) = runtime
            .block_on(async {
                let relay = match cfg.relay {
                    Some(r) => Some(Arc::new(
                        RelayService::start_with(r.listen, r.advertise, options.clone()).await?,
                    )),
                    None => None,
                };
                let node = Arc::new(peerdesk_net::Node::bind(cfg.listen, &identity, policy)?);
                let addr = node.local_addr()?;
                let directory: Arc<dyn Directory> = Arc::new(directory);
                Ok::<_, NetError>((
                    SignalService::start_with_accounts(
                        node,
                        relay.clone(),
                        directory,
                        Some(cfg.data_dir.join("device-codes.txt")),
                        Some(accounts),
                    ),
                    relay,
                    addr,
                ))
            })
            .map_err(fail)?;
        Ok(Self {
            info: SignalInfo {
                addr,
                fingerprint: identity.fingerprint(),
                relay: relay.as_ref().map(|r| r.addr()),
            },
            signal,
            relay,
            _runtime: runtime,
        })
    }
}

struct Shared {
    /// 集群共享密钥；`None` 表示心跳不带认证。
    secret: Option<Vec<u8>>,
    cluster: Arc<Mutex<Cluster>>,
    /// 中继账本。
    accounting: Arc<Accounting>,
    /// 计费密钥（签发会员令牌）。
    billing_key: Vec<u8>,
    /// 管理令牌；`None` 表示不能做写操作。
    admin_token: Option<String>,
    /// 中继单价（分/GB）。
    price: u64,
    /// 免费通道额度（管理中心显示）。
    free: FreePolicy,
    /// 中继带宽（Mbps），算负载用；0 表示按会话数算。
    capacity_mbps: u64,
    /// 上次算负载时的转发总字节数。
    last_bytes: Mutex<u64>,
    /// 记账间隔。
    bill_every: Duration,
    policy: Policy,
    fallback: Vec<String>,
    started: Instant,
    stop: AtomicBool,
    services: Option<Services>,
}

impl Shared {
    fn rendezvous(&self) -> status::Rendezvous {
        self.services
            .as_ref()
            .map_or_else(status::Rendezvous::default, |s| status::Rendezvous {
                online: Some(s.signal.online()),
                relay: s.relay.as_ref().map(|r| r.totals()),
            })
    }

    fn cluster(&self) -> MutexGuard<'_, Cluster> {
        // 持锁期间不会 panic；即便发生，成员表仍然可用，继续服务比整体停摆好。
        self.cluster.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> Time {
        Time(u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX))
    }
}

impl Shared {
    /// 处理一个集群数据包。验证不过、解析不了、重放的包都被静默丢弃。
    fn on_packet(&self, packet: &[u8]) {
        let (stamp, body) = match &self.secret {
            Some(secret) => match cluster::open(secret, packet, wall_ms(), MAX_CLOCK_SKEW_MS) {
                Ok((stamp, body)) => (Some(stamp), body),
                Err(_) => return,
            },
            None => (None, packet),
        };
        let Ok(msg) = ClusterMsg::decode(body) else {
            return;
        };
        let from = match &msg {
            ClusterMsg::Heartbeat { from, .. } => from.id,
            ClusterMsg::Usage { from, .. } => *from,
        };
        let now = self.now();
        let mut cluster = self.cluster();
        if stamp.is_some_and(|stamp| !cluster.fresh(from, stamp)) {
            return;
        }
        if let ClusterMsg::Usage { totals, .. } = &msg {
            self.accounting.merge(from.0, totals);
        }
        cluster.on_message(now, &msg);
    }
}

impl Api for Shared {
    fn status(&self) -> String {
        status::status_json(
            &self.cluster(),
            &self.policy,
            self.now(),
            self.secret.is_some(),
            &self.rendezvous(),
        )
    }

    fn route(&self, region: &str) -> Option<String> {
        self.cluster()
            .route(region, &self.fallback)
            .map(|node| status::node_json(&node))
    }

    fn cluster(&self) -> String {
        status::cluster_json(&self.cluster(), wall_ms())
    }

    fn usage(&self, month: Option<u32>) -> String {
        let now = wall_ms();
        let month = month.unwrap_or_else(|| month_of(now));
        let limited = self
            .services
            .as_ref()
            .and_then(|s| s.relay.as_ref())
            .map_or(0, |r| r.limited().0);
        let free = format!(
            "{{\"window_secs\":{},\"limit_bytes\":{},\"limited_now\":{limited}}}",
            self.free.window_us / 1_000_000,
            self.free.device_bytes
        );
        self.accounting.usage_json(month, self.price, &free)
    }

    fn admin(&self, token: Option<&str>, action: Admin) -> Result<String, (u16, String)> {
        let Some(expected) = &self.admin_token else {
            return Err((403, "这个节点没有配置 admin_token，不能做管理操作".into()));
        };
        // 逐字节比较全部内容，不在第一个不同处提前返回。
        let given = token.unwrap_or_default().as_bytes();
        let same = given.len() == expected.len()
            && given
                .iter()
                .zip(expected.as_bytes())
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0;
        if !same {
            return Err((401, "管理令牌不对".into()));
        }
        match action {
            Admin::AddPeer(addr) => {
                if addr.parse::<SocketAddr>().is_err() {
                    return Err((400, format!("节点地址应为 IP:端口：{addr}")));
                }
                peerdesk_net::note!(info, "管理中心：加入节点 {addr}");
                self.cluster().add_seed(addr);
                Ok("{\"ok\":true}".into())
            }
            Admin::IssueAccount(quota_gb) => {
                let mut id = [0u8; 8];
                {
                    use ring::rand::SecureRandom;
                    ring::rand::SystemRandom::new()
                        .fill(&mut id)
                        .map_err(|_| (500, "取随机数失败".to_owned()))?;
                }
                let token = issue(&self.billing_key, id, quota_gb);
                peerdesk_net::note!(
                    info,
                    "管理中心：签发会员账户 {}，额度 {quota_gb} GB/月",
                    hex(&id)
                );
                Ok(format!(
                    "{{\"account\":\"{}\",\"token\":{}}}",
                    hex(&id),
                    crate::json::string(&token)
                ))
            }
            Admin::IssueEnroll { account, days } => {
                let id = parse_account(&account)
                    .ok_or((400, "account 应为 16 位十六进制的账户号".to_owned()))?;
                if days == 0 || days > ENROLL_MAX_DAYS {
                    return Err((400, format!("days 应在 1 到 {ENROLL_MAX_DAYS} 之间")));
                }
                let mut nonce = [0u8; 4];
                {
                    use ring::rand::SecureRandom;
                    ring::rand::SystemRandom::new()
                        .fill(&mut nonce)
                        .map_err(|_| (500, "取随机数失败".to_owned()))?;
                }
                let expires_s = (wall_ms() / 1000).saturating_add(days.saturating_mul(86_400));
                let token =
                    issue_enroll(&self.billing_key, id, expires_s, u32::from_be_bytes(nonce));
                peerdesk_net::note!(
                    info,
                    "管理中心：给账户 {} 签发入表令牌，{days} 天有效",
                    account_hex(&id)
                );
                Ok(format!(
                    "{{\"account\":\"{}\",\"enroll_token\":{},\"expires\":{expires_s}}}",
                    account_hex(&id),
                    crate::json::string(&token)
                ))
            }
        }
    }
}

/// 随机的 32 字节密钥。
fn random_key() -> Vec<u8> {
    use ring::rand::SecureRandom;
    let mut key = vec![0u8; 32];
    // 取随机数失败极少见；那样密钥全 0，只是本机签发的东西可以被猜到，单机部署仍然能用。
    let _ = ring::rand::SystemRandom::new().fill(&mut key);
    key
}

fn io_error(what: &str, e: &std::io::Error) -> Error {
    Error::Config(format!("{what}：{e}"))
}

/// 记账、报告用量、更新负载：每个记账间隔一次。
fn bookkeeping(shared: &Shared, socket: &UdpSocket) {
    let now_ms = wall_ms();
    let Some(relay) = shared.services.as_ref().and_then(|s| s.relay.as_ref()) else {
        return;
    };
    let records = relay.take_records();
    if let Err(e) = shared.accounting.record(&records, now_ms) {
        peerdesk_net::note!(error, "{e}");
    }
    let (total, sessions, _) = relay.totals();
    let delta = {
        let mut last = shared
            .last_bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let delta = total.bytes.saturating_sub(*last);
        *last = total.bytes;
        delta
    };
    let millis = u64::try_from(shared.bill_every.as_millis()).unwrap_or(u64::MAX);
    let load = load_permille(shared.capacity_mbps, sessions, delta, millis);
    shared
        .cluster()
        .set_load(load, u32::try_from(sessions).unwrap_or(u32::MAX));
    let msg = ClusterMsg::Usage {
        from: NodeId(shared.cluster().me().id.0),
        totals: shared.accounting.payload(now_ms),
    };
    let Ok(body) = msg.encode() else { return };
    let bytes = match &shared.secret {
        Some(secret) => cluster::seal(secret, now_ms, &body),
        None => body,
    };
    let peers: Vec<String> = shared
        .cluster()
        .members_with_state()
        .map(|(info, _)| info.addr.clone())
        .collect();
    for addr in peers {
        if let Ok(addr) = addr.parse::<SocketAddr>() {
            let _ = socket.send_to(&bytes, addr);
        }
    }
}

/// 负载（千分比）：配置了带宽时按最近一段的转发速率算，否则每 100 个会话算满。
/// `delta_bytes` 是最近 `millis` 毫秒里转发的字节数。
fn load_permille(capacity_mbps: u64, sessions: usize, delta_bytes: u64, millis: u64) -> u16 {
    let by_sessions = u16::try_from(sessions.saturating_mul(10).min(1000)).unwrap_or(1000);
    if capacity_mbps == 0 {
        return by_sessions;
    }
    let delta = delta_bytes;
    let millis = millis.max(1);
    let bits_per_sec = delta.saturating_mul(8_000) / millis;
    let capacity = capacity_mbps.saturating_mul(1_000_000).max(1);
    u16::try_from((bits_per_sec.saturating_mul(1000) / capacity).min(1000)).unwrap_or(1000)
}

fn cluster_loop(shared: &Shared, socket: &UdpSocket) {
    let mut buf = [0u8; 2048];
    let mut last_bill = Instant::now();
    while !shared.stop.load(Ordering::Relaxed) {
        if last_bill.elapsed() >= shared.bill_every {
            last_bill = Instant::now();
            bookkeeping(shared, socket);
        }
        // 超时或瞬时错误都只是“这一轮没有收到包”。
        if let Ok((len, _)) = socket.recv_from(&mut buf) {
            shared.on_packet(buf.get(..len).unwrap_or_default());
        }
        let now = shared.now();
        let (_, outbound) = shared.cluster().tick(now);
        for out in outbound {
            let bytes = out.msg.encode().map(|body| match &shared.secret {
                Some(secret) => cluster::seal(secret, wall_ms(), &body),
                None => body,
            });
            if let (Ok(bytes), Ok(addr)) = (bytes, out.to_addr.parse::<SocketAddr>()) {
                // 对端不在线时发送失败是常态，下一次心跳会重试。
                let _ = socket.send_to(&bytes, addr);
            }
        }
    }
}

fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        head.extend_from_slice(chunk.get(..n)?);
        if let Some(end) = head.windows(4).position(|w| w == b"\r\n\r\n") {
            // 头读完了：按 Content-Length 再读请求体（有上限）。
            let text = String::from_utf8_lossy(head.get(..end).unwrap_or_default()).into_owned();
            let want = end + 4 + http::content_length(&text).min(http::MAX_BODY);
            if head.len() >= want {
                break;
            }
        }
        if head.len() > MAX_REQUEST_HEAD {
            return None;
        }
    }
    String::from_utf8(head).ok()
}

fn handle_connection(shared: &Shared, mut stream: TcpStream) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(HTTP_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(HTTP_READ_TIMEOUT));
    let response = match read_head(&mut stream) {
        None => Response::text(431, "request header too large or unreadable\n"),
        Some(head) => match http::parse_request(&head) {
            Some(request) => http::respond(&request, shared, crate::assets::ASSETS),
            None => Response::text(400, "bad request\n"),
        },
    };
    let _ = stream.write_all(&response.to_bytes());
    let _ = stream.flush();
    // 先关写端，再把对方还没发完的数据读掉（有上限），然后才关闭连接。
    // 否则内核会因为有未读数据而发 RST，客户端可能来不及读到响应。
    let _ = stream.shutdown(Shutdown::Write);
    let _ = stream.set_read_timeout(Some(DRAIN_TIMEOUT));
    let mut sink = [0u8; 4096];
    let mut drained = 0usize;
    while drained < MAX_DRAIN {
        match stream.read(&mut sink) {
            Ok(n) if n > 0 => drained = drained.saturating_add(n),
            Ok(_) | Err(_) => break,
        }
    }
}

fn http_loop(shared: &Arc<Shared>, listener: &TcpListener) {
    while !shared.stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = Arc::clone(shared);
                // 每个连接一个短命线程：管理接口的并发量很小。
                thread::spawn(move || handle_connection(&shared, stream));
            }
            Err(_) => thread::sleep(POLL),
        }
    }
}

/// 运行中的节点。
#[derive(Debug)]
pub struct Node {
    shared: Arc<Shared>,
    http_addr: SocketAddr,
    cluster_addr: SocketAddr,
    threads: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

impl Node {
    /// 按配置启动：绑定端口，拉起集群线程和管理接口线程；配置了信令时同时启动信令和中继。
    pub fn start(cfg: &ServerConfig, timing: ClusterConfig) -> Result<Self, Error> {
        let policy = cfg.crypto.policy()?;
        let socket =
            UdpSocket::bind(&cfg.cluster_listen).map_err(|e| io_error("绑定集群端口失败", &e))?;
        socket
            .set_read_timeout(Some(POLL))
            .map_err(|e| io_error("设置集群端口超时失败", &e))?;
        let listener =
            TcpListener::bind(&cfg.http_listen).map_err(|e| io_error("绑定管理端口失败", &e))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| io_error("设置管理端口失败", &e))?;
        let cluster_addr = socket
            .local_addr()
            .map_err(|e| io_error("读取集群端口失败", &e))?;
        let http_addr = listener
            .local_addr()
            .map_err(|e| io_error("读取管理端口失败", &e))?;
        let me = NodeInfo {
            id: NodeId(cfg.node_id),
            region: cfg.region.clone(),
            addr: cfg.advertise.clone(),
            load_permille: 0,
            sessions: 0,
            signal: String::new(),
            fingerprint: [0; 32],
            role: cfg.role,
            relay: String::new(),
        };
        let cluster = Arc::new(Mutex::new(Cluster::new(me, timing, cfg.peers.clone())));
        let usage_dir = cfg
            .data_dir
            .as_ref()
            .map(|d| PathBuf::from(d).join("usage"));
        let accounting = Arc::new(Accounting::open(cfg.node_id, usage_dir, wall_ms()));
        // 计费密钥：集群共用（billing_secret 或 cluster_secret）；单机没配时用随机密钥，
        // 只有本机签发的会员令牌和中继票据有效，重启后失效。
        let billing_key = cfg.billing_key().map_or_else(random_key, <[u8]>::to_vec);
        let bill_every = Duration::from_millis(cfg.bill_every_ms);
        let options = RelayOptions {
            key: billing_key.clone(),
            free: cfg.free_policy(),
            bill_every,
        };
        let directory = ClusterDirectory {
            cluster: Arc::clone(&cluster),
            accounting: Arc::clone(&accounting),
            key: billing_key.clone(),
            paid_limit: cfg.paid_load_limit_permille,
        };
        let services = cfg
            .signal()?
            .map(|signal| {
                // 账户设备表存在信令的数据目录里；入表令牌用计费密钥验证（集群共用）
                let dir = signal.data_dir.join("devices");
                let (store, skipped) =
                    crate::devices::FileDeviceStore::open(&dir, wall_ms() / 1000);
                peerdesk_net::note!(
                    info,
                    "账户设备表 {}：{} 个账户，跳过 {skipped} 行",
                    dir.display(),
                    store.counts().len()
                );
                let accounts = Accounts {
                    store: Arc::new(store),
                    key: billing_key.clone(),
                };
                Services::start(&signal, &policy, options, directory, accounts)
            })
            .transpose()?;
        if let Some(services) = &services {
            let info = services.info;
            let advertised = cfg
                .signal_advertise
                .clone()
                .or_else(|| (!info.addr.ip().is_unspecified()).then(|| info.addr.to_string()));
            let mut c = cluster.lock().unwrap_or_else(PoisonError::into_inner);
            c.update_me(|me| {
                me.signal = advertised.unwrap_or_default();
                me.fingerprint = info.fingerprint;
                me.relay = info.relay.map(|r| r.to_string()).unwrap_or_default();
            });
        }
        let shared = Arc::new(Shared {
            secret: cfg.cluster_secret.as_ref().map(|s| s.as_bytes().to_vec()),
            cluster,
            accounting,
            billing_key,
            admin_token: cfg.admin_token.clone(),
            price: cfg.relay_price_cents_per_gb,
            free: cfg.free_policy(),
            capacity_mbps: cfg.relay_capacity_mbps,
            last_bytes: Mutex::new(0),
            bill_every,
            policy,
            fallback: cfg.region_fallback.clone(),
            started: Instant::now(),
            stop: AtomicBool::new(false),
            services,
        });

        let for_cluster = Arc::clone(&shared);
        let for_http = Arc::clone(&shared);
        let threads = vec![
            thread::spawn(move || cluster_loop(&for_cluster, &socket)),
            thread::spawn(move || http_loop(&for_http, &listener)),
        ];
        Ok(Self {
            shared,
            http_addr,
            cluster_addr,
            threads,
        })
    }

    /// 管理接口实际监听的地址。
    #[must_use]
    pub const fn http_addr(&self) -> SocketAddr {
        self.http_addr
    }

    /// 集群心跳实际监听的地址。
    #[must_use]
    pub const fn cluster_addr(&self) -> SocketAddr {
        self.cluster_addr
    }

    /// 信令服务的地址和指纹；没有开启信令时是 `None`。
    #[must_use]
    pub fn signal(&self) -> Option<SignalInfo> {
        self.shared.services.as_ref().map(|s| s.info)
    }

    /// 信令和中继的运行数据。
    #[must_use]
    pub fn rendezvous(&self) -> status::Rendezvous {
        self.shared.rendezvous()
    }

    /// 存活节点数，包含自己。
    #[must_use]
    pub fn alive_count(&self) -> usize {
        self.shared.cluster().alive_count()
    }

    /// 更新本节点负载。
    pub fn set_load(&self, load_permille: u16, sessions: u32) {
        self.shared.cluster().set_load(load_permille, sessions);
    }

    /// 停止并等待线程退出。
    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }

    /// 取得一个可以从别的线程发出停止请求的句柄。
    #[must_use]
    pub fn stop_handle(&self) -> StopHandle {
        StopHandle {
            shared: Arc::clone(&self.shared),
        }
    }

    /// 一直运行，直到有人通过 [`StopHandle`] 请求停止。
    pub fn wait(mut self) {
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

/// 停止请求句柄。
#[derive(Debug, Clone)]
pub struct StopHandle {
    shared: Arc<Shared>,
}

impl StopHandle {
    /// 请求节点停止。线程会在一个轮询间隔内退出。
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

/// 进程退出码：用法错误。
pub const EXIT_USAGE: u8 = 2;
/// 进程退出码：启动失败。
pub const EXIT_FAILED: u8 = 1;

/// 解析命令行并启动节点。成功时返回节点，失败时返回退出码；提示信息写到 `out`。
///
/// 用法：`peerdesk-server <配置文件> [--exit-after-ms <毫秒>]`。
/// `--exit-after-ms` 让节点运行一段时间后自行退出，用于冒烟测试。
pub fn launch(args: &[String], out: &mut dyn Write) -> Result<Node, u8> {
    let (path, exit_after) = match args {
        [path] => (path, None),
        [path, flag, ms] if flag == "--exit-after-ms" => match ms.parse::<u64>() {
            Ok(ms) => (path, Some(Duration::from_millis(ms))),
            Err(_) => {
                let _ = writeln!(out, "错误：--exit-after-ms 的值不是合法数字：{ms}");
                return Err(EXIT_USAGE);
            }
        },
        _ => {
            let _ = writeln!(
                out,
                "用法：peerdesk-server <配置文件> [--exit-after-ms <毫秒>]"
            );
            return Err(EXIT_USAGE);
        }
    };
    let started = std::fs::read_to_string(path)
        .map_err(|e| Error::Config(format!("读取 {path} 失败：{e}")))
        .and_then(|text| ServerConfig::from_toml(&text))
        .and_then(|cfg| Node::start(&cfg, ClusterConfig::default()));
    match started {
        Ok(node) => {
            let _ = writeln!(
                out,
                "peerdesk-server {} 已启动：管理页面 http://{}，集群 {}",
                peerdesk_core::VERSION,
                node.http_addr(),
                node.cluster_addr()
            );
            match node.signal() {
                Some(info) => {
                    let relay = info
                        .relay
                        .map_or_else(|| "未开启".to_owned(), |a| a.to_string());
                    let _ = writeln!(
                        out,
                        "信令 {}，服务器指纹 {}\n中继 {relay}",
                        info.addr,
                        to_hex(&info.fingerprint)
                    );
                }
                None => {
                    let _ = writeln!(out, "信令未开启（配置里没有 signal_listen）。");
                }
            }
            if node.shared.secret.is_none() {
                let _ = writeln!(
                    out,
                    "警告：没有设置 cluster_secret，集群心跳不带认证，只能用于受信任的内网。"
                );
            }
            let _ = out.flush();
            if let Some(after) = exit_after {
                let handle = node.stop_handle();
                thread::spawn(move || {
                    thread::sleep(after);
                    handle.stop();
                });
            }
            Ok(node)
        }
        Err(e) => {
            let _ = writeln!(out, "错误：{e}");
            Err(EXIT_FAILED)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::load_permille;

    #[test]
    fn load_by_sessions_or_bandwidth() {
        // 没配带宽：每 100 个会话算满
        assert_eq!(load_permille(0, 0, 999, 1000), 0);
        assert_eq!(load_permille(0, 50, 0, 1000), 500);
        assert_eq!(load_permille(0, 5000, 0, 1000), 1000);
        // 100 Mbps 容量，1 秒内转发 6.25 MB = 50 Mbps → 500‰
        assert_eq!(load_permille(100, 0, 6_250_000, 1000), 500);
        assert_eq!(load_permille(100, 0, u64::MAX, 1000), 1000);
        // 间隔为 0 时不除零
        assert_eq!(load_permille(100, 0, 0, 0), 0);
    }
}
