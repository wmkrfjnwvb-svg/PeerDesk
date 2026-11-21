//! 服务器一侧的信令服务和中继服务。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use peerdesk_core::paths::Candidate;
use peerdesk_core::relay::{Record, Relay, SessionId, Usage};
use peerdesk_core::signal::{format_code, parse_observe, Caller, DeviceId, Registry, SignalMsg};
use peerdesk_core::Time;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::identity::to_hex;
use crate::stream::{read_kind, read_signal, write_signal, StreamKind};
use crate::{Link, NetError, Node};

/// 中继会话空闲多久后作废（微秒）。
const RELAY_IDLE_US: u64 = 30_000_000;
/// 同时存在的中继会话数上限。
const RELAY_MAX_SESSIONS: usize = 10_000;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 中继服务：一个 UDP 端口，转发并计量。
#[derive(Debug)]
pub struct RelayService {
    addr: SocketAddr,
    relay: Arc<Mutex<Relay>>,
    records: Arc<Mutex<Vec<Record>>>,
    /// NAT 探测：每台设备最近一次在这个端口上被看到的地址。
    observed: Arc<Mutex<BTreeMap<DeviceId, SocketAddr>>>,
    started: Instant,
    task: JoinHandle<()>,
}

/// 记下一台设备的探测结果；表满了就清空重来（只影响打洞方法的选择，下次探测又会补上）。
fn remember(
    seen: &mut BTreeMap<DeviceId, SocketAddr>,
    device: DeviceId,
    from: SocketAddr,
    max: usize,
) {
    if seen.len() >= max {
        seen.clear();
    }
    seen.insert(device, from);
}

/// 等控制端探测包的步数和每步时长（最多 200 毫秒）。
const PROBE_WAIT_STEPS: u32 = 20;
const PROBE_WAIT_STEP: Duration = Duration::from_millis(10);
/// 最多记住这么多台设备的探测结果；满了就清空重来（只影响打洞方法的选择）。
const MAX_OBSERVED: usize = 100_000;

impl RelayService {
    /// 在 `bind` 上启动。`advertise` 是告诉客户端的地址（服务器在 NAT 或负载均衡后面时与 `bind` 不同）。
    pub async fn start(bind: SocketAddr, advertise: Option<SocketAddr>) -> Result<Self, NetError> {
        let io = |e: std::io::Error| NetError::Io(format!("启动中继失败：{e}"));
        let socket = UdpSocket::bind(bind).await.map_err(io)?;
        let addr = advertise.unwrap_or(socket.local_addr().map_err(io)?);
        let relay = Arc::new(Mutex::new(Relay::new(RELAY_IDLE_US, RELAY_MAX_SESSIONS)));
        let records = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        let observed = Arc::new(Mutex::new(BTreeMap::new()));
        let (relay2, records2, observed2) = (
            Arc::clone(&relay),
            Arc::clone(&records),
            Arc::clone(&observed),
        );
        let task = tokio::spawn(async move {
            let now = || Time(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
            let mut sweep = tokio::time::interval(Duration::from_secs(1));
            let mut buf = vec![0u8; 2048];
            loop {
                tokio::select! {
                    _ = sweep.tick() => {
                        let ended = lock(&relay2).expire(now());
                        lock(&records2).extend(ended);
                    }
                    received = socket.recv_from(&mut buf) => {
                        let Ok((len, from)) = received else { continue };
                        let packet = buf.get(..len).unwrap_or_default();
                        if let Some(device) = parse_observe(packet) {
                            remember(&mut lock(&observed2), device, from, MAX_OBSERVED);
                            continue;
                        }
                        let to = lock(&relay2).on_packet(now(), from, packet);
                        if let Some(to) = to {
                            let _ = socket.send_to(packet, to).await;
                        }
                    }
                }
            }
        });
        Ok(Self {
            addr,
            relay,
            records,
            observed,
            started,
            task,
        })
    }

    /// 设备 `device` 最近一次的 NAT 探测包是从哪个地址来的。
    #[must_use]
    pub fn observed(&self, device: &DeviceId) -> Option<SocketAddr> {
        lock(&self.observed).get(device).copied()
    }

    /// 忘掉设备 `device` 的探测结果（马上要重新探测）。
    pub fn forget(&self, device: &DeviceId) {
        let _ = lock(&self.observed).remove(device);
    }

    /// 客户端应当使用的中继地址。
    #[must_use]
    pub const fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// 为一次连接开一条通道。满了时返回 false。
    pub fn allocate(&self, session: SessionId, account: [u8; 32]) -> bool {
        let now = Time(u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX));
        lock(&self.relay).allocate(now, session, account)
    }

    /// 某个进行中的会话的用量。
    #[must_use]
    pub fn usage(&self, session: &SessionId) -> Option<Usage> {
        lock(&self.relay).usage(session)
    }

    /// 启动以来转发的总量、当前会话数、丢弃的包数。
    #[must_use]
    pub fn totals(&self) -> (Usage, usize, u64) {
        let relay = lock(&self.relay);
        (relay.total(), relay.sessions(), relay.dropped())
    }

    /// 取走已结束会话的账单记录。
    #[must_use]
    pub fn take_records(&self) -> Vec<Record> {
        std::mem::take(&mut *lock(&self.records))
    }
}

impl Drop for RelayService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct SignalState {
    registry: Mutex<Registry>,
    /// 每条已登记连接的发件箱。
    outboxes: Mutex<BTreeMap<u64, mpsc::Sender<SignalMsg>>>,
    relay: Option<Arc<RelayService>>,
    next_conn: AtomicU64,
    /// 设备码簿的存盘位置；`None` 表示只在内存里（测试用）。
    codes: Option<PathBuf>,
}

/// 设备码用的随机数。
fn random_u32() -> u32 {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 4];
    // 取随机数失败极少见；退回到时钟，码仍然会做冲突检查。
    let ok = ring::rand::SystemRandom::new().fill(&mut b).is_ok();
    let clock = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    if ok {
        u32::from_le_bytes(b)
    } else {
        clock
    }
}

/// 把码簿写到磁盘：先写临时文件再改名，写一半断电也不会坏掉原文件。
fn save_codes(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// 信令服务：接受设备的连接，登记在线设备，为控制端和被控端牵线。
pub struct SignalService {
    state: Arc<SignalState>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for SignalService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalService")
            .field("online", &self.online())
            .finish()
    }
}

fn new_session_id() -> Option<SessionId> {
    use ring::rand::SecureRandom;
    let mut id = [0u8; 16];
    ring::rand::SystemRandom::new().fill(&mut id).ok()?;
    Some(id)
}

/// 控制端要连 `target`（`None` 表示找不到这台设备）：分配中继，通知被控端，回复控制端。
async fn introduce(
    state: &SignalState,
    link: &Link,
    target: Option<DeviceId>,
    locals: Vec<Candidate>,
    probed: &mut bool,
    send: &mut quinn::SendStream,
) -> Result<(), NetError> {
    let online = target.filter(|t| lock(&state.registry).is_online(t));
    let Some(target) = online else {
        return write_signal(send, &SignalMsg::NotFound).await;
    };
    let me = *link.peer_fingerprint();
    // 先让控制端从连接用的端口向探测地址发包，好判断它的 NAT 类型（每条信令连接问一次）。
    if let (Some(relay), false) = (state.relay.as_ref(), *probed) {
        *probed = true;
        relay.forget(&me);
        return write_signal(
            send,
            &SignalMsg::Probe {
                relay: relay.addr(),
            },
        )
        .await;
    }
    if let Some(relay) = state.relay.as_ref() {
        // 探测包走 UDP，可能比这条请求晚到一点。
        for _ in 0..PROBE_WAIT_STEPS {
            if relay.observed(&me).is_some() {
                break;
            }
            tokio::time::sleep(PROBE_WAIT_STEP).await;
        }
    }
    let nonce = new_session_id().unwrap_or_default();
    let probe_of = |d: &DeviceId| state.relay.as_ref().and_then(|r| r.observed(d));
    // 中继通道记在发起连接的设备名下。
    let relay = state.relay.as_ref().and_then(|relay| {
        let session = new_session_id()?;
        relay
            .allocate(session, me)
            .then_some((relay.addr(), session))
    });
    let caller = Caller {
        device: me,
        addr: link.remote_addr(),
        locals,
    };
    let intro = lock(&state.registry).connect(&caller, &target, relay, nonce, &probe_of);
    let plan = intro
        .to_controller
        .punch()
        .map(|p| format!("控制端 {:?}、被控端 {:?}", p.me, p.peer))
        .unwrap_or_default();
    crate::note!(
        info,
        "牵线 {} → {}（NAT：{plan}），中继{}",
        link.remote_addr(),
        to_hex(&target).get(..16).unwrap_or_default(),
        if relay.is_some() {
            "已分配"
        } else {
            "未开启"
        },
    );
    if let Some((host_conn, msg)) = intro.to_host {
        let host = lock(&state.outboxes).get(&host_conn).cloned();
        if let Some(host) = host {
            let _ = host.send(msg).await;
        }
    }
    write_signal(send, &intro.to_controller).await
}

async fn serve_device(state: Arc<SignalState>, link: Link) -> Result<(), NetError> {
    let conn = state.next_conn.fetch_add(1, Ordering::Relaxed);
    let (mut send, mut recv) = link.accept_stream().await?;
    if read_kind(&mut recv).await? != StreamKind::Signal {
        return Err(NetError::Protocol("信令端口只接受信令流"));
    }
    let (tx, mut outbox) = mpsc::channel::<SignalMsg>(16);
    let mut probed = false;
    let result = loop {
        tokio::select! {
            Some(msg) = outbox.recv() => {
                if let Err(e) = write_signal(&mut send, &msg).await { break Err(e); }
            }
            msg = read_signal(&mut recv) => match msg {
                Err(_) => break Ok(()),
                Ok(SignalMsg::Register { locals }) => {
                    let (done, text) = {
                        let mut registry = lock(&state.registry);
                        let done = registry
                            .register(conn, *link.peer_fingerprint(), link.remote_addr(), &locals, state.relay.as_ref().map(|r| r.addr()), &mut random_u32);
                        let text = done.as_ref().filter(|d| d.new_code).map(|_| registry.codes_text());
                        (done, text)
                    };
                    let Some(done) = done else { break Err(NetError::Protocol("设备码已分配完")); };
                    if let (Some(path), Some(text)) = (&state.codes, text) {
                        if let Err(e) = save_codes(path, &text) {
                            crate::note!(error, "保存设备码簿 {} 失败：{e}", path.display());
                        }
                    }
                    crate::note!(info,
                        "设备登记 {} 码 {} 地址 {}",
                        to_hex(link.peer_fingerprint()).get(..16).unwrap_or_default(),
                        format_code(done.code),
                        link.remote_addr(),
                    );
                    lock(&state.outboxes).insert(conn, tx.clone());
                    if let Err(e) = write_signal(&mut send, &done.reply).await { break Err(e); }
                }
                Ok(SignalMsg::Check) => {
                    let me = *link.peer_fingerprint();
                    let checked = SignalMsg::Checked {
                        observed: link.remote_addr(),
                        probe: state.relay.as_ref().map(|r| r.addr()),
                        seen: state.relay.as_ref().and_then(|r| r.observed(&me)),
                    };
                    // 下一次体检看的是这次之后的探测包。
                    if let Some(relay) = state.relay.as_ref() {
                        relay.forget(&me);
                    }
                    if let Err(e) = write_signal(&mut send, &checked).await { break Err(e); }
                }
                Ok(SignalMsg::Connect { target, locals }) => {
                    if let Err(e) = introduce(&state, &link, Some(target), locals, &mut probed, &mut send).await { break Err(e); }
                }
                Ok(SignalMsg::ConnectCode { code, locals }) => {
                    let target = lock(&state.registry).device_of(code);
                    if target.is_none() {
                        crate::note!(info, "按设备码 {} 连接：码不存在，来自 {}", format_code(code), link.remote_addr());
                    }
                    if let Err(e) = introduce(&state, &link, target, locals, &mut probed, &mut send).await { break Err(e); }
                }
                Ok(
                    SignalMsg::Registered { .. }
                    | SignalMsg::Checked { .. }
                    | SignalMsg::Probe { .. }
                    | SignalMsg::Peer { .. }
                    | SignalMsg::Incoming { .. }
                    | SignalMsg::NotFound,
                ) => break Err(NetError::Protocol("设备不该发这种信令")),
            },
        }
    };
    lock(&state.registry).disconnect(conn);
    lock(&state.outboxes).remove(&conn);
    link.close("bye");
    result
}

impl SignalService {
    /// 在一个已绑定的端点上启动。`relay` 是可选的中继服务。设备码只记在内存里。
    #[must_use]
    pub fn start(node: Arc<Node>, relay: Option<Arc<RelayService>>) -> Self {
        Self::start_with_codes(node, relay, None)
    }

    /// 同 [`SignalService::start`]，设备码簿存在 `codes` 文件里，服务器重启后设备码不变。
    #[must_use]
    pub fn start_with_codes(
        node: Arc<Node>,
        relay: Option<Arc<RelayService>>,
        codes: Option<PathBuf>,
    ) -> Self {
        let mut registry = Registry::new();
        if let Some(path) = &codes {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    let bad = registry.load_codes(&text);
                    crate::note!(
                        info,
                        "读取设备码簿 {}：{} 台设备，跳过 {bad} 行",
                        path.display(),
                        registry.known()
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => crate::note!(error, "读取设备码簿 {} 失败：{e}", path.display()),
            }
        }
        let state = Arc::new(SignalState {
            registry: Mutex::new(registry),
            outboxes: Mutex::new(BTreeMap::new()),
            relay,
            next_conn: AtomicU64::new(1),
            codes,
        });
        let accept_state = Arc::clone(&state);
        let task = tokio::spawn(async move {
            while let Some(incoming) = node.accept().await {
                if let Ok(link) = incoming {
                    tokio::spawn(serve_device(Arc::clone(&accept_state), link));
                }
            }
        });
        Self { state, task }
    }

    /// 在线设备数。
    #[must_use]
    pub fn online(&self) -> usize {
        lock(&self.state.registry).online()
    }
}

impl Drop for SignalService {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_clears_when_full() {
        let a: SocketAddr = "1.2.3.4:5"
            .parse()
            .unwrap_or(SocketAddr::from(([0, 0, 0, 0], 0)));
        let mut seen = BTreeMap::new();
        remember(&mut seen, [1; 32], a, 2);
        remember(&mut seen, [2; 32], a, 2);
        remember(&mut seen, [3; 32], a, 2);
        assert_eq!(seen.len(), 1);
        assert_eq!(seen.get(&[3; 32]), Some(&a));
    }
}
