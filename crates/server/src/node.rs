//! 一个运行中的服务端节点：集群心跳线程加管理接口线程。
//!
//! 这里是服务端唯一做 IO 的地方。决策全部来自核心库的 [`Cluster`]，
//! 这一层只负责收发字节和计时。

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use peerdesk_core::cluster::{self, Cluster, ClusterConfig, ClusterMsg, NodeInfo};
use peerdesk_core::config::ServerConfig;
use peerdesk_core::crypto::Policy;
use peerdesk_core::region::NodeId;
use peerdesk_core::{Error, Time};

use crate::http::{self, Api, Response, MAX_REQUEST_HEAD};
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

struct Shared {
    /// 集群共享密钥；`None` 表示心跳不带认证。
    secret: Option<Vec<u8>>,
    cluster: Mutex<Cluster>,
    policy: Policy,
    fallback: Vec<String>,
    started: Instant,
    stop: AtomicBool,
}

impl Shared {
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
        let ClusterMsg::Heartbeat { from, .. } = &msg;
        let now = self.now();
        let mut cluster = self.cluster();
        if stamp.is_some_and(|stamp| !cluster.fresh(from.id, stamp)) {
            return;
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
        )
    }

    fn route(&self, region: &str) -> Option<String> {
        self.cluster()
            .route(region, &self.fallback)
            .map(|node| status::node_json(&node))
    }
}

fn io_error(what: &str, e: &std::io::Error) -> Error {
    Error::Config(format!("{what}：{e}"))
}

fn cluster_loop(shared: &Shared, socket: &UdpSocket) {
    let mut buf = [0u8; 2048];
    while !shared.stop.load(Ordering::Relaxed) {
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
        if head.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
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
    /// 按配置启动：绑定端口，拉起集群线程和管理接口线程。
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
        };
        let shared = Arc::new(Shared {
            secret: cfg.cluster_secret.as_ref().map(|s| s.as_bytes().to_vec()),
            cluster: Mutex::new(Cluster::new(me, timing, cfg.peers.clone())),
            policy,
            fallback: cfg.region_fallback.clone(),
            started: Instant::now(),
            stop: AtomicBool::new(false),
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
