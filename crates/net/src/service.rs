//! 服务器一侧的信令服务和中继服务。

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use peerdesk_core::billing::{verify_enroll, EnrollTokenError};
use peerdesk_core::billing::{AccountId, PUBLIC_ACCOUNT};
use peerdesk_core::devices::{DeviceInfo, DeviceStore, Refusal, Role, Row};
use peerdesk_core::paths::Candidate;
use peerdesk_core::relay::{
    unwrap, FreePolicy, Record, Relay, SessionId, Ticket, Usage, TICKET_LEN, TICKET_MAGIC,
};
use peerdesk_core::signal::{
    format_code, parse_observe, Caller, DeviceId, DeviceRow, Features, Registry, SignalMsg,
    MAX_ROWS,
};
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
    /// 验证中继票据的密钥（集群共享）。
    key: Vec<u8>,
    started: Instant,
    task: JoinHandle<()>,
}

/// 中继的设置。
#[derive(Debug, Clone)]
pub struct RelayOptions {
    /// 签发和验证中继票据的密钥。集群里所有节点相同，信令节点签的票据其他节点才认。
    pub key: Vec<u8>,
    /// 免费通道的额度。
    pub free: FreePolicy,
    /// 记账间隔。
    pub bill_every: Duration,
}

impl RelayOptions {
    /// 单机用：随机密钥，免费通道不限额。
    #[must_use]
    pub fn local() -> Self {
        let mut key = vec![0u8; 32];
        {
            use ring::rand::SecureRandom;
            // 取随机数失败极少见；那样密钥全 0，票据仍然只在本进程内使用。
            let _ = ring::rand::SystemRandom::new().fill(&mut key);
        }
        Self {
            key,
            free: FreePolicy::UNLIMITED,
            bill_every: BILL_EVERY,
        }
    }
}

/// 多久记一次账（崩溃最多丢这么久的用量）。
pub const BILL_EVERY: Duration = Duration::from_secs(10);

/// 现在的 Unix 毫秒。
pub(crate) fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// 处理中继端口收到的一个包，返回要转给谁。带票据的包（转发器的登记和保活）：
/// 会话还没开就凭票据开通，然后只登记发送方的地址，不转发、不计费。
fn handle(
    relay: &mut Relay,
    key: &[u8],
    now: Time,
    from: SocketAddr,
    packet: &[u8],
) -> Option<SocketAddr> {
    let ticket = unwrap(packet).filter(|(_, _, payload)| payload.starts_with(TICKET_MAGIC));
    let Some((session, _, payload)) = ticket else {
        return relay.on_packet(now, from, packet);
    };
    if !relay.knows(&session) {
        let valid = Ticket::open(key, payload, wall_ms()).filter(|t| t.session == session)?;
        if !relay.admit(now, &valid) {
            return None;
        }
    }
    relay.touch(now, from, packet);
    None
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
    /// 在 `bind` 上启动（单机：随机票据密钥，免费通道不限额）。`advertise` 是告诉客户端的地址
    /// （服务器在 NAT 或负载均衡后面时与 `bind` 不同）。
    pub async fn start(bind: SocketAddr, advertise: Option<SocketAddr>) -> Result<Self, NetError> {
        Self::start_with(bind, advertise, RelayOptions::local()).await
    }

    /// 同 [`RelayService::start`]，指定票据密钥和免费额度。
    pub async fn start_with(
        bind: SocketAddr,
        advertise: Option<SocketAddr>,
        options: RelayOptions,
    ) -> Result<Self, NetError> {
        let io = |e: std::io::Error| NetError::Io(format!("启动中继失败：{e}"));
        let socket = UdpSocket::bind(bind).await.map_err(io)?;
        let addr = advertise.unwrap_or(socket.local_addr().map_err(io)?);
        let relay = Arc::new(Mutex::new(
            Relay::new(RELAY_IDLE_US, RELAY_MAX_SESSIONS).with_free_policy(options.free),
        ));
        let records = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        let observed = Arc::new(Mutex::new(BTreeMap::new()));
        let (relay2, records2, observed2, key) = (
            Arc::clone(&relay),
            Arc::clone(&records),
            Arc::clone(&observed),
            options.key.clone(),
        );
        let task = tokio::spawn(async move {
            let now = || Time(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
            let mut sweep = tokio::time::interval(Duration::from_secs(1));
            let mut bill = tokio::time::interval(options.bill_every);
            let mut buf = vec![0u8; 2048];
            loop {
                tokio::select! {
                    _ = sweep.tick() => {
                        let ended = lock(&relay2).expire(now());
                        lock(&records2).extend(ended);
                    }
                    _ = bill.tick() => {
                        let deltas = lock(&relay2).bill();
                        lock(&records2).extend(deltas);
                    }
                    received = socket.recv_from(&mut buf) => {
                        let Ok((len, from)) = received else { continue };
                        let packet = buf.get(..len).unwrap_or_default();
                        if let Some(device) = parse_observe(packet) {
                            remember(&mut lock(&observed2), device, from, MAX_OBSERVED);
                            continue;
                        }
                        let (to, notices) = {
                            let mut relay = lock(&relay2);
                            (handle(&mut relay, &key, now(), from, packet), relay.take_notices())
                        };
                        if let Some(to) = to {
                            let _ = socket.send_to(packet, to).await;
                        }
                        for (to, notice) in notices {
                            let _ = socket.send_to(&notice, to).await;
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
            key: options.key,
            started,
            task,
        })
    }

    /// 签发中继票据用的密钥。
    #[must_use]
    pub fn key(&self) -> &[u8] {
        &self.key
    }

    /// 被限流的设备数（当前窗口里免费额度用完的）和启动以来停掉的免费通道数。
    #[must_use]
    pub fn limited(&self) -> (usize, u64) {
        let now = Time(u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX));
        let relay = lock(&self.relay);
        (relay.limited_now(now), relay.limited())
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
    /// 中继放在哪、集群里有哪些节点。
    directory: Arc<dyn Directory>,
    next_conn: AtomicU64,
    /// 设备码簿的存盘位置；`None` 表示只在内存里（测试用）。
    codes: Option<PathBuf>,
    /// 每台设备上一次传崩溃报告的时间（限频）。
    crash_seen: Mutex<BTreeMap<[u8; 32], Instant>>,
    /// 账户设备表；没配就不提供（功能字段里不报 [`Features::DEVICES`]）。
    accounts: Option<Accounts>,
}

/// 账户设备表的配置：表存在哪、用哪个密钥验证入表令牌（就是计费密钥，集群共用）。
#[derive(Debug, Clone)]
pub struct Accounts {
    /// 存储。
    pub store: Arc<dyn DeviceStore>,
    /// 验证入表令牌的密钥。
    pub key: Vec<u8>,
}

/// 现在的 Unix 秒。
fn wall_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn wire_row(r: &Row) -> DeviceRow {
    DeviceRow {
        row: r.row,
        fingerprint: r.fingerprint,
        name: r.name.clone(),
        os: r.os,
        role: r.role.id(),
        last_seen_s: r.last_seen_s,
        removed: r.removed_s.is_some(),
    }
}

/// 回答账户设备表的请求（入表、列出、改名、移除）。`device` 是这条连接的证书指纹：除了入表，别的请求都要求它
/// 是账户成员，而且只能动自己账户里的设备——不是成员一律回“不在账户里”，不泄露别的账户有什么。
/// 不是设备表的消息返回空。
fn account_reply(
    accounts: Option<&Accounts>,
    device: &DeviceId,
    msg: &SignalMsg,
    now_s: u64,
) -> Vec<SignalMsg> {
    let refused = |why: Refusal| vec![SignalMsg::Refused { code: why.code() }];
    let Some(accounts) = accounts else {
        return refused(Refusal::Unsupported);
    };
    let store = &accounts.store;
    let changed = |result: Result<u64, Refusal>| match result {
        Ok(rev) => vec![SignalMsg::Changed { rev }],
        Err(why) => refused(why),
    };
    match msg {
        SignalMsg::Enroll {
            token,
            name,
            os,
            role,
        } => {
            let account = match verify_enroll(&accounts.key, token, now_s) {
                Ok(account) => account,
                Err(EnrollTokenError::Invalid) => return refused(Refusal::BadToken),
                Err(EnrollTokenError::Expired) => return refused(Refusal::ExpiredToken),
            };
            let info = DeviceInfo {
                fingerprint: *device,
                name: name.clone(),
                os: *os,
                role: Role::from_id(*role),
            };
            match store.enroll(account, &info, now_s) {
                Ok(e) => vec![SignalMsg::Enrolled {
                    account: e.account,
                    row: e.row,
                    rev: e.rev,
                }],
                Err(why) => refused(why),
            }
        }
        SignalMsg::Devices { since } => {
            let Some((account, _)) = store.member(device) else {
                return refused(Refusal::NotMember);
            };
            let changes = store.list(&account, *since);
            let rows: Vec<DeviceRow> = changes.rows.iter().map(wire_row).collect();
            let mut chunks: Vec<&[DeviceRow]> = rows.chunks(MAX_ROWS).collect();
            if chunks.is_empty() {
                chunks.push(&[]);
            }
            let last = chunks.len().saturating_sub(1);
            chunks
                .into_iter()
                .enumerate()
                .map(|(i, chunk)| SignalMsg::DeviceRows {
                    rev: changes.rev,
                    full: changes.full,
                    more: i < last,
                    rows: chunk.to_vec(),
                })
                .collect()
        }
        SignalMsg::Rename { row, name } => {
            let Some((account, _)) = store.member(device) else {
                return refused(Refusal::NotMember);
            };
            changed(store.rename(account, *row, name))
        }
        SignalMsg::Remove { row } => {
            let Some((account, own)) = store.member(device) else {
                return refused(Refusal::NotMember);
            };
            // 编号 0 是自己退出
            let row = if *row == 0 { own } else { *row };
            changed(store.remove(account, row, now_s))
        }
        SignalMsg::Register { .. }
        | SignalMsg::Registered { .. }
        | SignalMsg::Probe { .. }
        | SignalMsg::Connect { .. }
        | SignalMsg::ConnectCode { .. }
        | SignalMsg::Peer { .. }
        | SignalMsg::Incoming { .. }
        | SignalMsg::NotFound
        | SignalMsg::Nodes
        | SignalMsg::NodeList { .. }
        | SignalMsg::Check
        | SignalMsg::CheckQuiet
        | SignalMsg::Checked { .. }
        | SignalMsg::Enrolled { .. }
        | SignalMsg::Refused { .. }
        | SignalMsg::DeviceRows { .. }
        | SignalMsg::Changed { .. } => Vec::new(),
    }
}

/// 这台服务器实际开着的功能：代码会、配置也开了的。
fn features_of(accounts: Option<&Accounts>) -> Features {
    if accounts.is_some() {
        Features::DEVICES
    } else {
        Features::NONE
    }
}

impl SignalState {
    fn features(&self) -> Features {
        features_of(self.accounts.as_ref())
    }
}

/// 同一台设备两次崩溃报告至少隔这么久。
const CRASH_INTERVAL: Duration = Duration::from_secs(60);
/// 服务器最多保存这么多份崩溃报告（每份不超过 260 KB，总共约 500 MB）。
const MAX_CRASH_FILES: usize = 2000;

/// 收一份崩溃报告，存到设备码簿旁边的 `crashes/` 里。没有数据目录（测试）时只记日志。
async fn serve_crash(
    state: &SignalState,
    link: &Link,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
) -> Result<(), NetError> {
    let device = *link.peer_fingerprint();
    let now = Instant::now();
    {
        let mut seen = lock(&state.crash_seen);
        if seen
            .get(&device)
            .is_some_and(|t| now.duration_since(*t) < CRASH_INTERVAL)
        {
            return Err(NetError::Protocol("崩溃报告太频繁"));
        }
        let _ = seen.insert(device, now);
    }
    let report = crate::stream::read_crash(&mut recv).await?;
    crate::note!(
        warn,
        "收到崩溃报告：{}（{}，日志 {} 字节）",
        link.remote_addr(),
        report.summary.lines().next().unwrap_or_default(),
        report.log.len()
    );
    if let Some(dir) = state
        .codes
        .as_ref()
        .and_then(|p| p.parent())
        .map(|d| d.join("crashes"))
    {
        let count = std::fs::read_dir(&dir).map_or(0, Iterator::count);
        if count >= MAX_CRASH_FILES {
            return Err(NetError::Protocol("崩溃报告已存满"));
        }
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let path = dir.join(peerdesk_core::crash::file_name(secs, &device));
        let mut text = format!("{}\n来自 {}\n\n", report.summary, link.remote_addr()).into_bytes();
        text.extend_from_slice(&report.log);
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, text))
            .map_err(|e| NetError::Io(format!("保存崩溃报告 {}：{e}", path.display())))?;
    }
    send.write_all(&[1])
        .await
        .map_err(|e| NetError::Closed(e.to_string()))?;
    let _ = send.finish();
    // 等确认送到再返回：返回后连接随之关闭，没送到的确认会被丢掉
    let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;
    Ok(())
}

/// 一次连接的中继安排。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayPlan {
    /// 中继地址、会话号和票据；`None` 表示没有中继可用。
    pub relay: Option<(SocketAddr, SessionId, [u8; TICKET_LEN])>,
    /// 给用户看的说明：走会员通道还是免费通道，或者为什么没有中继。
    pub note: String,
}

/// 信令服务向“目录”查询：这次连接的中继放在哪台节点上、集群里有哪些节点。
/// 服务端的集群实现它（按节点角色、负载、会员额度选中继）；单机部署用 [`LocalDirectory`]。
pub trait Directory: Send + Sync {
    /// 为发起连接的 `caller`（带着会员令牌 `account`，空表示免费用户）安排中继。
    fn plan_relay(&self, caller: &DeviceId, account: &str) -> RelayPlan;
    /// 集群里存活的节点：（信令地址，证书指纹，是否付费节点）。
    fn nodes(&self) -> Vec<(SocketAddr, DeviceId, bool)>;
}

/// 签一张中继票据，返回会话号和票据。票据 60 秒内有效（只用来开通道，开了以后不再检查）。
#[must_use]
pub fn sign_ticket(
    key: &[u8],
    caller: &DeviceId,
    account: AccountId,
    vip: bool,
) -> (SessionId, [u8; TICKET_LEN]) {
    let session = new_session_id().unwrap_or_default();
    let mut subject = [0u8; 8];
    for (d, s) in subject.iter_mut().zip(caller.iter()) {
        *d = *s;
    }
    let ticket = Ticket {
        session,
        account,
        vip,
        subject,
        expires_ms: wall_ms().saturating_add(TICKET_TTL_MS),
    }
    .seal(key);
    let mut fixed = [0u8; TICKET_LEN];
    for (d, s) in fixed.iter_mut().zip(ticket.iter()) {
        *d = *s;
    }
    (session, fixed)
}

/// 票据的有效期。
const TICKET_TTL_MS: u64 = 60_000;

/// 单机部署的目录：中继就是本机的中继；会员令牌用本机的密钥验证；集群只有自己。
pub struct LocalDirectory {
    relay: Option<Arc<RelayService>>,
    billing_key: Option<Vec<u8>>,
}

impl std::fmt::Debug for LocalDirectory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalDirectory")
            .field("relay", &self.relay.as_ref().map(|r| r.addr()))
            .finish_non_exhaustive()
    }
}

impl LocalDirectory {
    /// 本机的中继（可以没有）和会员令牌的密钥（可以没有：那样所有人都按免费用户）。
    #[must_use]
    pub fn new(relay: Option<Arc<RelayService>>, billing_key: Option<Vec<u8>>) -> Self {
        Self { relay, billing_key }
    }
}

/// 验证会员令牌：有效时返回账户；空令牌、无效令牌返回 `None`，并说明原因（空令牌没有说明）。
#[must_use]
pub fn member(
    key: Option<&[u8]>,
    token: &str,
) -> (Option<peerdesk_core::billing::Account>, Option<String>) {
    if token.trim().is_empty() {
        return (None, None);
    }
    match key.map(|k| peerdesk_core::billing::verify(k, token)) {
        Some(Ok(account)) => (Some(account), None),
        Some(Err(_)) | None => (None, Some("会员令牌无效，按免费用户处理".into())),
    }
}

impl Directory for LocalDirectory {
    fn plan_relay(&self, caller: &DeviceId, account: &str) -> RelayPlan {
        let (member, why) = member(self.billing_key.as_deref(), account);
        let Some(relay) = &self.relay else {
            return RelayPlan {
                relay: None,
                note: "服务器没有开中继".into(),
            };
        };
        let vip = member.is_some();
        let id = member.map_or(PUBLIC_ACCOUNT, |m| m.id);
        let (session, ticket) = sign_ticket(relay.key(), caller, id, vip);
        let channel = if vip { "会员通道" } else { "免费通道" };
        RelayPlan {
            relay: Some((relay.addr(), session, ticket)),
            note: why.map_or_else(|| channel.to_owned(), |w| format!("{channel}（{w}）")),
        }
    }

    fn nodes(&self) -> Vec<(SocketAddr, DeviceId, bool)> {
        Vec::new()
    }
}

/// 设备码的候选：由设备指纹算出（第 `k` 个候选，冲突时试下一个）。集群里每个节点对同一台设备
/// 算出同一个码，不用互相协调；只有极少见的哈希冲突时，不同节点上的码才可能不同。
fn code_candidate(device: &DeviceId, k: u32) -> u32 {
    let mut input = device.to_vec();
    input.extend_from_slice(&k.to_be_bytes());
    let digest = ring::digest::digest(&ring::digest::SHA256, &input);
    let mut b = [0u8; 4];
    for (d, s) in b.iter_mut().zip(digest.as_ref()) {
        *d = *s;
    }
    u32::from_be_bytes(b)
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
    account: String,
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
    // 中继记在发起连接一端的账户上（会员令牌），没有令牌时是免费通道。
    let plan = state.directory.plan_relay(&me, &account);
    let relay = plan.relay;
    let caller = Caller {
        device: me,
        addr: link.remote_addr(),
        locals,
    };
    let mut intro = lock(&state.registry).connect(&caller, &target, relay, nonce, &probe_of);
    if let SignalMsg::Peer { note, .. } = &mut intro.to_controller {
        note.clone_from(&plan.note);
    }
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
    match read_kind(&mut recv).await? {
        StreamKind::Signal => {}
        StreamKind::Crash => return serve_crash(&state, &link, send, recv).await,
        StreamKind::Control
        | StreamKind::File
        | StreamKind::Clipboard
        | StreamKind::Audio
        | StreamKind::Browse
        | StreamKind::View => {
            return Err(NetError::Protocol("信令端口只接受信令流和崩溃报告"));
        }
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
                    let device = *link.peer_fingerprint();
                    let mut k = 0u32;
                    let mut next_code = || {
                        k = k.wrapping_add(1);
                        code_candidate(&device, k)
                    };
                    let (done, text) = {
                        let mut registry = lock(&state.registry);
                        let done = registry
                            .register(conn, *link.peer_fingerprint(), link.remote_addr(), &locals, state.relay.as_ref().map(|r| r.addr()), &mut next_code);
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
                Ok(msg @ (SignalMsg::Check | SignalMsg::CheckQuiet)) => {
                    let me = *link.peer_fingerprint();
                    let checked = SignalMsg::Checked {
                        observed: link.remote_addr(),
                        probe: state.relay.as_ref().map(|r| r.addr()),
                        seen: state.relay.as_ref().and_then(|r| r.observed(&me)),
                        features: state.features(),
                    };
                    // 下一次体检看的是这次之后的探测包。只问功能的（CheckQuiet）不动探测结果：
                    // 它可能夹在“探测”和“发起连接”之间，忘掉的话打洞用的 NAT 判断就没了。
                    if let (SignalMsg::Check, Some(relay)) = (&msg, state.relay.as_ref()) {
                        relay.forget(&me);
                    }
                    if let Err(e) = write_signal(&mut send, &checked).await { break Err(e); }
                }
                Ok(
                    msg @ (SignalMsg::Enroll { .. }
                    | SignalMsg::Devices { .. }
                    | SignalMsg::Rename { .. }
                    | SignalMsg::Remove { .. }),
                ) => {
                    let replies =
                        account_reply(state.accounts.as_ref(), link.peer_fingerprint(), &msg, wall_s());
                    let mut failed = None;
                    for reply in &replies {
                        if let Err(e) = write_signal(&mut send, reply).await {
                            failed = Some(e);
                            break;
                        }
                    }
                    if let Some(e) = failed { break Err(e); }
                }
                Ok(SignalMsg::Connect { target, locals, account }) => {
                    if let Err(e) = introduce(&state, &link, Some(target), locals, account, &mut probed, &mut send).await { break Err(e); }
                }
                Ok(SignalMsg::Nodes) => {
                    let list = SignalMsg::NodeList { nodes: state.directory.nodes() };
                    if let Err(e) = write_signal(&mut send, &list).await { break Err(e); }
                }
                Ok(SignalMsg::ConnectCode { code, locals, account }) => {
                    let target = lock(&state.registry).device_of(code);
                    if target.is_none() {
                        crate::note!(info, "按设备码 {} 连接：码不存在，来自 {}", format_code(code), link.remote_addr());
                    }
                    if let Err(e) = introduce(&state, &link, target, locals, account, &mut probed, &mut send).await { break Err(e); }
                }
                Ok(
                    SignalMsg::Registered { .. }
                    | SignalMsg::NodeList { .. }
                    | SignalMsg::Checked { .. }
                    | SignalMsg::Probe { .. }
                    | SignalMsg::Peer { .. }
                    | SignalMsg::Incoming { .. }
                    | SignalMsg::NotFound
                    | SignalMsg::Enrolled { .. }
                    | SignalMsg::Refused { .. }
                    | SignalMsg::DeviceRows { .. }
                    | SignalMsg::Changed { .. },
                ) => break Err(NetError::Protocol("设备不该发这种信令")),
            },
        }
    };
    let registered = lock(&state.outboxes).remove(&conn).is_some();
    lock(&state.registry).disconnect(conn);
    // 登记过的设备下线了：记上次在线时间（账户成员才记）
    if let (true, Some(accounts)) = (registered, state.accounts.as_ref()) {
        accounts.store.seen(link.peer_fingerprint(), wall_s());
    }
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
        let directory = Arc::new(LocalDirectory::new(relay.clone(), None));
        Self::start_with_directory(node, relay, directory, codes)
    }

    /// 同 [`SignalService::start_with_codes`]，中继安排和节点列表由 `directory` 决定（集群用）。
    /// `relay` 是本机的中继，只用来做 NAT 探测。
    #[must_use]
    pub fn start_with_directory(
        node: Arc<Node>,
        relay: Option<Arc<RelayService>>,
        directory: Arc<dyn Directory>,
        codes: Option<PathBuf>,
    ) -> Self {
        Self::start_with_accounts(node, relay, directory, codes, None)
    }

    /// 同 [`SignalService::start_with_directory`]，另外提供账户设备表（`accounts` 为 `None` 时不提供）。
    #[must_use]
    pub fn start_with_accounts(
        node: Arc<Node>,
        relay: Option<Arc<RelayService>>,
        directory: Arc<dyn Directory>,
        codes: Option<PathBuf>,
        accounts: Option<Accounts>,
    ) -> Self {
        let mut registry = Registry::new();
        registry.set_features(features_of(accounts.as_ref()));
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
            directory,
            next_conn: AtomicU64::new(1),
            codes,
            crash_seen: Mutex::new(BTreeMap::new()),
            accounts,
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
    #![allow(clippy::indexing_slicing, clippy::unreachable)]
    use super::*;

    /// 设备表的请求：除了入表都要求是账户成员，只能动自己账户里的设备。
    #[test]
    fn account_requests_are_answered_for_members_only() {
        use peerdesk_core::billing::issue_enroll;
        use peerdesk_core::devices::MemoryStore;
        let key = b"billing".to_vec();
        let accounts = Accounts {
            store: Arc::new(MemoryStore::new()),
            key: key.clone(),
        };
        let ask = |device: u8, msg: SignalMsg, now: u64| {
            account_reply(Some(&accounts), &[device; 32], &msg, now)
        };
        let refused = |why: Refusal| vec![SignalMsg::Refused { code: why.code() }];
        let enroll = |token: &str, name: &str| SignalMsg::Enroll {
            token: token.to_owned(),
            name: name.to_owned(),
            os: 2,
            role: Role::Host.id(),
        };
        let (a, b) = ([1u8; 8], [2u8; 8]);
        let token_a = issue_enroll(&key, a, 1_000, 1);
        let token_b = issue_enroll(&key, b, 1_000, 2);
        // 入表：令牌不对、过期、名字不合适都拒绝并说明
        assert_eq!(
            ask(1, enroll("junk", "mac"), 10),
            refused(Refusal::BadToken)
        );
        assert_eq!(
            ask(1, enroll(&issue_enroll(b"other", a, 1_000, 1), "mac"), 10),
            refused(Refusal::BadToken)
        );
        assert_eq!(
            ask(1, enroll(&token_a, "mac"), 1_000),
            refused(Refusal::ExpiredToken)
        );
        assert_eq!(ask(1, enroll(&token_a, ""), 10), refused(Refusal::BadName));
        // 没入表：列表、改名、移除都拒绝
        for msg in [
            SignalMsg::Devices { since: 0 },
            SignalMsg::Rename {
                row: 1,
                name: "x".into(),
            },
            SignalMsg::Remove { row: 1 },
        ] {
            assert_eq!(ask(1, msg, 10), refused(Refusal::NotMember));
        }
        // 三台设备入账户 a，一台入账户 b
        for (device, name) in [(1u8, "mac-mini"), (2, "办公室 PC"), (3, "linux-box")] {
            assert_eq!(
                ask(device, enroll(&token_a, name), 10),
                vec![SignalMsg::Enrolled {
                    account: a,
                    row: u32::from(device),
                    rev: u64::from(device),
                }]
            );
        }
        assert_eq!(
            ask(9, enroll(&token_b, "别人的"), 10),
            vec![SignalMsg::Enrolled {
                account: b,
                row: 1,
                rev: 1
            }]
        );
        // 已经在别的账户里：拒绝
        assert_eq!(
            ask(9, enroll(&token_a, "x"), 10),
            refused(Refusal::OtherAccount)
        );
        // 成员看到自己账户的全部设备，看不到别的账户的
        let listed = ask(2, SignalMsg::Devices { since: 0 }, 10);
        assert_eq!(listed.len(), 1);
        let SignalMsg::DeviceRows {
            rev,
            full,
            more,
            rows,
        } = &listed[0]
        else {
            unreachable!("应当是设备列表");
        };
        assert_eq!((*rev, *full, *more), (3, true, false));
        assert_eq!(
            rows.iter()
                .map(|r| (r.row, r.name.as_str(), r.fingerprint[0]))
                .collect::<Vec<_>>(),
            [(1, "mac-mini", 1), (2, "办公室 PC", 2), (3, "linux-box", 3)]
        );
        let other = ask(9, SignalMsg::Devices { since: 0 }, 10);
        assert!(matches!(
            &other[0],
            SignalMsg::DeviceRows { rows, .. } if rows.len() == 1 && rows[0].name == "别人的"
        ));
        // 改名：只能改自己账户里的；别的账户的编号 1 是另一台设备
        assert_eq!(
            ask(
                2,
                SignalMsg::Rename {
                    row: 1,
                    name: "书房的 Mac".into()
                },
                10
            ),
            vec![SignalMsg::Changed { rev: 4 }]
        );
        assert_eq!(
            ask(
                2,
                SignalMsg::Rename {
                    row: 7,
                    name: "x".into()
                },
                10
            ),
            refused(Refusal::NoSuchDevice)
        );
        assert!(matches!(
            &ask(9, SignalMsg::Devices { since: 0 }, 10)[0],
            SignalMsg::DeviceRows { rows, .. } if rows[0].name == "别人的"
        ));
        // 增量：只有改过的那一行
        assert!(matches!(
            &ask(3, SignalMsg::Devices { since: 3 }, 10)[0],
            SignalMsg::DeviceRows { rev: 4, full: false, rows, .. }
                if rows.len() == 1 && rows[0].name == "书房的 Mac"
        ));
        // 移除别的设备；被移除的那台不再是成员
        assert_eq!(
            ask(1, SignalMsg::Remove { row: 3 }, 20),
            vec![SignalMsg::Changed { rev: 5 }]
        );
        assert_eq!(
            ask(3, SignalMsg::Devices { since: 0 }, 20),
            refused(Refusal::NotMember)
        );
        assert!(matches!(
            &ask(1, SignalMsg::Devices { since: 4 }, 20)[0],
            SignalMsg::DeviceRows { rows, .. } if rows.len() == 1 && rows[0].removed
        ));
        // 编号 0：自己退出
        assert_eq!(
            ask(2, SignalMsg::Remove { row: 0 }, 30),
            vec![SignalMsg::Changed { rev: 6 }]
        );
        assert_eq!(
            ask(2, SignalMsg::Devices { since: 0 }, 30),
            refused(Refusal::NotMember)
        );
        // 设备多了分页：每条最多 MAX_ROWS 台，最后一条 more 为假
        for device in 20..50u8 {
            ask(device, enroll(&token_a, "批量"), 40);
        }
        let pages = ask(1, SignalMsg::Devices { since: 0 }, 40);
        assert_eq!(pages.len(), 3, "31 台分 3 页");
        let counts: Vec<(usize, bool)> = pages
            .iter()
            .map(|p| match p {
                SignalMsg::DeviceRows { rows, more, .. } => (rows.len(), *more),
                other => unreachable!("{}", other.name()),
            })
            .collect();
        assert_eq!(counts, [(MAX_ROWS, true), (MAX_ROWS, true), (7, false)]);
        // 服务器没开设备表：一律“不支持”；不是设备表的消息不回
        assert_eq!(
            account_reply(None, &[1; 32], &SignalMsg::Devices { since: 0 }, 1),
            refused(Refusal::Unsupported)
        );
        assert!(ask(1, SignalMsg::Check, 1).is_empty());
        assert_eq!(features_of(None), Features::NONE);
        assert_eq!(features_of(Some(&accounts)), Features::DEVICES);
        assert!(wall_s() > 1_700_000_000);
    }

    #[test]
    fn codes_derive_from_the_fingerprint() {
        assert_eq!(code_candidate(&[1; 32], 1), code_candidate(&[1; 32], 1));
        assert_ne!(code_candidate(&[1; 32], 1), code_candidate(&[1; 32], 2));
        assert_ne!(code_candidate(&[1; 32], 1), code_candidate(&[2; 32], 1));
    }

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
