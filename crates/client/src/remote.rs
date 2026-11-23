//! 真实连接的命令：`host`、`connect`、`bench-transport`。

use std::collections::VecDeque;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use peerdesk_core::auth::{Auth, Limiter};
use peerdesk_core::crypto::Policy;
use peerdesk_core::input::InputEvent;
use peerdesk_core::paths::Candidate;
use peerdesk_core::proto::Caps;
use peerdesk_core::session::SessionConfig;
use peerdesk_core::signal::{format_code, parse_code};
use peerdesk_net::controller::{self, ControllerOptions, Upload};
use peerdesk_net::demo::{to_bmp, CountSink};
use peerdesk_net::fs_store::scan;
use peerdesk_net::host::{self, HostOptions, InputSink, Permissions};
use peerdesk_net::identity::{parse_hex, to_hex};
use peerdesk_net::netcheck::prepare_host;
use peerdesk_net::rendezvous::{
    answer, dial_via, fetch_nodes, register_with, Incoming, Lookup, Path as PathKind, PathPolicy,
    Registration, Server,
};
use peerdesk_net::{bench, Fingerprint, Identity, NetError, Node};
use peerdesk_platform::CaptureOptions;

use crate::source::{Opener, SourceKind, Sources};
use crate::{parse_number, usage_error, EXIT_FAILED, EXIT_OK};

/// 默认的数据目录：优先用系统的每用户配置目录。
pub(crate) fn default_data_dir() -> PathBuf {
    let from_env = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    from_env("PEERDESK_DATA_DIR")
        .or_else(|| from_env("APPDATA").map(|p| p.join("PeerDesk")))
        .or_else(|| from_env("XDG_CONFIG_HOME").map(|p| p.join("peerdesk")))
        .or_else(|| from_env("HOME").map(|p| p.join(".config").join("peerdesk")))
        .unwrap_or_else(|| PathBuf::from(".peerdesk"))
}

pub(crate) fn runtime() -> Result<tokio::runtime::Runtime, NetError> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| NetError::Io(format!("启动运行时失败：{e}")))
}

fn finish(out: &mut dyn Write, result: Result<u8, NetError>) -> u8 {
    result.unwrap_or_else(|e| {
        let _ = writeln!(out, "错误：{e}");
        EXIT_FAILED
    })
}

pub(crate) fn session(identity: &Identity, policy: &Policy, auth: Auth) -> SessionConfig {
    SessionConfig {
        device_id: identity.device_id(),
        region: String::new(),
        // 剪贴板能力：双方都支持、两端都有剪贴板时才真正同步（命令行不同步剪贴板）。
        caps: Caps::H264
            .with(Caps::CLIPBOARD)
            .with(Caps::AUDIO)
            .with(Caps::CLIPBOARD_FILES),
        policy: policy.clone(),
        auth,
        // 由网络层替换成真实连接的通道绑定。
        binding: Vec::new(),
    }
}

/// 输入注入接入之前：只计数。
struct CountInput(Arc<AtomicU64>);

impl InputSink for CountInput {
    fn inject(&mut self, _event: &InputEvent) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

/// 被控端最多同时保留这么多条中继转发器（每个连接请求一条）；更早的被丢弃。
const MAX_SHIMS: usize = 16;
/// 与服务器的登记断开后，隔这么久重新登记一次，直到成功。
const REREGISTER_EVERY: Duration = Duration::from_secs(2);

fn parse_fingerprint(flag: &str, value: Option<&String>) -> Result<Fingerprint, String> {
    let hex = value.ok_or_else(|| format!("{flag} 需要一个值"))?;
    parse_hex(hex).map_err(|e| e.to_string())
}

/// `--server` 与 `--server-pin` 组合成服务器。只给了指纹时报错。
fn server_of(addr: Option<SocketAddr>, pin: Option<Fingerprint>) -> Result<Option<Server>, String> {
    match (addr, pin) {
        (None, Some(_)) => Err("--server-pin 只能和 --server 一起用".into()),
        (addr, pin) => Ok(addr.map(|addr| Server { addr, pin })),
    }
}

fn warn_unpinned_server(out: &mut dyn Write, server: &Server) {
    if server.pin.is_none() {
        let _ = writeln!(
            out,
            "警告：没有用 --server-pin 指定服务器指纹，无法确认连到的是你的服务器。"
        );
    }
}

struct HostArgs {
    listen: SocketAddr,
    server: Option<Server>,
    data_dir: PathBuf,
    recv_dir: Option<PathBuf>,
    fps: u32,
    once: bool,
    auth: Auth,
    trusted: Vec<Fingerprint>,
    open: bool,
    permissions: Permissions,
    source: SourceKind,
    display: usize,
}

/// 密码从环境变量读，避免出现在进程列表和命令历史里；`--password` 仅为方便测试。
fn password_from_env() -> Option<String> {
    std::env::var("PEERDESK_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
}

fn parse_host(args: &[String]) -> Result<HostArgs, String> {
    let mut parsed = HostArgs {
        listen: ([0, 0, 0, 0], 4433).into(),
        server: None,
        data_dir: default_data_dir(),
        recv_dir: None,
        fps: 60,
        once: false,
        auth: password_from_env().map_or(Auth::Open, Auth::Password),
        trusted: Vec::new(),
        open: false,
        permissions: Permissions::default(),
        source: SourceKind::Auto,
        display: 0,
    };
    let (mut server, mut server_pin) = (None, None);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--listen" => parsed.listen = parse_number(flag, it.next())?,
            "--server" => server = Some(parse_number(flag, it.next())?),
            "--server-pin" => server_pin = Some(parse_fingerprint(flag, it.next())?),
            "--data-dir" => parsed.data_dir = it.next().ok_or("--data-dir 需要一个值")?.into(),
            "--recv-dir" => {
                parsed.recv_dir = Some(it.next().ok_or("--recv-dir 需要一个值")?.into())
            }
            "--fps" => parsed.fps = parse_number(flag, it.next())?,
            "--once" => parsed.once = true,
            "--password" => {
                parsed.auth = Auth::Password(it.next().ok_or("--password 需要一个值")?.clone());
            }
            "--trust" => parsed.trusted.push(parse_fingerprint(flag, it.next())?),
            "--open" => parsed.open = true,
            "--no-input" => parsed.permissions.input = false,
            "--no-files" => parsed.permissions.files = false,
            "--source" => parsed.source = SourceKind::parse(it.next())?,
            "--display" => parsed.display = parse_number(flag, it.next())?,
            other => return Err(format!("未知选项：{other}")),
        }
    }
    parsed.server = server_of(server, server_pin)?;
    if parsed.fps == 0 || parsed.fps > 240 {
        return Err("--fps 的范围是 1 到 240".into());
    }
    match (&parsed.auth, parsed.open) {
        (Auth::Password(p), false) if p.len() < 8 => {
            return Err("密码至少 8 个字符".into());
        }
        (Auth::Password(_), true) => return Err("--open 不能和密码一起用".into()),
        (Auth::Open, false) if parsed.trusted.is_empty() => {
            return Err(
                "必须指定谁可以控制这台设备：设置环境变量 PEERDESK_PASSWORD，\
                 或用 --trust <指纹> 指定受信任的设备，或用 --open 明确表示任何人都可以连"
                    .into(),
            );
        }
        (Auth::Password(_) | Auth::Open, _) => {}
    }
    if parsed.open {
        // 完全开放时，受信任列表没有意义。
        parsed.trusted.clear();
    } else if matches!(parsed.auth, Auth::Open) {
        // 只有受信任的设备可以连：其他设备面对的是一个没人知道的随机密码。
        let random = Identity::generate().map_err(|e| e.to_string())?;
        parsed.auth = Auth::Password(to_hex(&random.fingerprint()));
    }
    Ok(parsed)
}

/// 下一个连接请求。没有登记或登记已断开时返回 `None`。
async fn next_request(registration: &mut Option<Registration>) -> Option<Incoming> {
    registration.as_mut()?.incoming.recv().await
}

/// 保持在服务器上的登记：对每个连接请求打洞并接上中继；断开后定期重新登记，直到成功。
/// 中继转发器留在这里，直到被更新的请求挤掉。
pub(crate) async fn stay_registered(
    node: Arc<Node>,
    server: Server,
    registration: Option<Registration>,
    extra: Vec<Candidate>,
) {
    let mut registration = registration;
    let mut shims = VecDeque::new();
    loop {
        while let Some(request) = next_request(&mut registration).await {
            // 中继通道接不上时，控制端仍可能直连成功，所以这里不报错。
            shims.extend(answer(&node, &request).await.ok().flatten());
            let _ = shims.drain(..shims.len().saturating_sub(MAX_SHIMS));
        }
        tokio::time::sleep(REREGISTER_EVERY).await;
        registration = register_with(&node, &server, extra.clone()).await.ok();
    }
}

/// 隔多久重新问一次集群节点列表（新加入的节点也登记上）。
const CLUSTER_REFRESH: Duration = Duration::from_secs(60);

/// 集群节点缓存文件：上次从服务器得到的节点列表，服务器崩了也能换到别的节点。
const NODES_FILE: &str = "cluster-nodes.txt";

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
                pin: Some(parse_hex(pin.trim()).ok()?),
            })
        })
        .collect()
}

/// 存集群节点缓存。
pub fn save_nodes(dir: &Path, nodes: &[Server]) -> std::io::Result<()> {
    let text: String = nodes
        .iter()
        .filter_map(|n| Some(format!("{} {}\n", n.addr, to_hex(n.pin.as_ref()?))))
        .collect();
    std::fs::write(dir.join(NODES_FILE), text)
}

/// 也在集群里其他节点上登记：任何一个节点崩了，设备仍然在线，别的节点照样能找到它。
/// 节点列表从 `seed` 问来（问不到时用缓存），每隔一段时间再问一次，新加入的节点也登记上。
pub(crate) async fn join_cluster(
    node: Arc<Node>,
    seed: Server,
    extra: Vec<Candidate>,
    data_dir: PathBuf,
) {
    let mut joined = vec![seed.addr];
    loop {
        let nodes = match fetch_nodes(&node, &seed).await {
            Ok(nodes) => {
                if !nodes.is_empty() {
                    let _ = save_nodes(&data_dir, &nodes);
                }
                nodes
            }
            Err(_) => load_nodes(&data_dir),
        };
        for server in nodes {
            if joined.contains(&server.addr) {
                continue;
            }
            joined.push(server.addr);
            peerdesk_net::note!(info, "也在集群节点 {} 上登记", server.addr);
            tokio::spawn(stay_registered(
                Arc::clone(&node),
                server,
                None,
                extra.clone(),
            ));
        }
        tokio::time::sleep(CLUSTER_REFRESH).await;
    }
}

async fn host_async(args: HostArgs, out: &mut dyn Write) -> Result<u8, NetError> {
    let policy = Policy::recommended();
    let identity = Identity::load_or_create(&args.data_dir)?;
    // 系统不支持 IPv6 时，“所有 IPv6 地址”退回“所有 IPv4 地址”，端口不变。
    let fallback = SocketAddr::from(([0, 0, 0, 0], args.listen.port()));
    let tries = if args.listen.is_ipv6() && args.listen.ip().is_unspecified() {
        vec![args.listen, fallback]
    } else {
        vec![args.listen]
    };
    let node = Arc::new(Node::bind_first(&tries, &identity, &policy)?);
    let recv_root = args
        .recv_dir
        .unwrap_or_else(|| args.data_dir.join("received"));
    let _ = writeln!(out, "监听 {}", node.local_addr()?);
    let _ = writeln!(out, "指纹 {}", to_hex(&identity.fingerprint()));
    let _ = writeln!(out, "控制端连接时请核对这个指纹。按 Ctrl+C 退出。");
    if args.open {
        let _ = writeln!(
            out,
            "警告：--open 模式，任何能连到这个端口的人都可以控制这台设备。"
        );
    }
    if let Some(server) = args.server {
        warn_unpinned_server(out, &server);
        // 先体检、请路由器映射端口（映射出来的地址也报告给服务器）。
        let (report, extra) = prepare_host(&node, Some(&server)).await;
        for (item, value) in report.lines() {
            let _ = writeln!(out, "{item:<12}{value}");
            peerdesk_net::note!(info, "网络体检 {item}：{value}");
        }
        let registration = register_with(&node, &server, extra.clone()).await?;
        let _ = writeln!(
            out,
            "已在服务器 {} 登记，服务器看到的本机地址 {}",
            server.addr, registration.observed
        );
        let _ = writeln!(out, "设备码 {}", format_code(registration.code));
        tokio::spawn(stay_registered(
            Arc::clone(&node),
            server,
            Some(registration),
            extra.clone(),
        ));
        tokio::spawn(join_cluster(
            Arc::clone(&node),
            server,
            extra,
            args.data_dir.clone(),
        ));
    }
    let limiter = Arc::new(Mutex::new(Limiter::default()));
    let capture = CaptureOptions {
        display: args.display,
        max_fps: args.fps,
    };
    let sources = Sources::choose(args.source, capture, Opener::SYSTEM, args.permissions.input)
        .map_err(NetError::Io)?;
    let _ = writeln!(out, "画面来源：{}", sources.describe());
    let _ = out.flush();

    while let Some(incoming) = node.accept().await {
        let link = match incoming {
            Ok(link) => link,
            Err(e) => {
                let _ = writeln!(out, "连接握手失败：{e}");
                continue;
            }
        };
        let _ = writeln!(
            out,
            "已连接 {}，对端指纹 {}",
            link.remote_addr(),
            to_hex(link.peer_fingerprint())
        );
        let _ = out.flush();
        let inputs = Arc::new(AtomicU64::new(0));
        // 没有权限就不提供远程：拒绝这次连接并停止被控。
        let (source, sink) = match sources.open(Box::new(CountInput(Arc::clone(&inputs)))) {
            Ok(opened) => opened,
            Err(why) => {
                link.close("host cannot share its screen");
                return Err(NetError::Io(why));
            }
        };
        let result = host::serve(
            link,
            HostOptions {
                trusted: args.trusted.clone(),
                permissions: args.permissions,
                limiter: Arc::clone(&limiter),
                ..HostOptions::new(
                    session(&identity, &policy, args.auth.clone()),
                    args.fps,
                    recv_root.clone(),
                )
            },
            source,
            sink,
        )
        .await;
        match result {
            Ok(report) => {
                let _ = writeln!(
                    out,
                    "会话结束（{}）：发出 {} 帧共 {} 字节，结束时码率 {} kbps（{}），\
                     注入 {} 个输入事件（拒绝 {} 个），收到 {} 个文件",
                    report.end,
                    report.frames_sent,
                    report.video_bytes,
                    report.final_rate_bps / 1000,
                    report.profile,
                    report.input_events,
                    report.input_denied,
                    report.files.len()
                );
                for file in &report.files {
                    let _ = writeln!(out, "  {} {:?}", file.path, file.outcome);
                }
            }
            Err(e) => {
                // 没建立起来的会话（例如对端核对指纹后放弃）不算数，继续等下一个。
                let _ = writeln!(out, "会话失败：{e}");
                let _ = out.flush();
                continue;
            }
        }
        let _ = out.flush();
        if args.once {
            break;
        }
    }
    node.close();
    node.wait_idle().await;
    Ok(EXIT_OK)
}

/// 连 `addr` 时本机绑定的地址：同一地址族的任意地址、任意端口。
fn any_for(addr: SocketAddr) -> SocketAddr {
    if addr.is_ipv6() {
        ([0u16; 8], 0).into()
    } else {
        ([0, 0, 0, 0], 0).into()
    }
}

/// 把崩溃报告传给服务器（用本机身份连接，服务器按身份限频）。
pub(crate) fn upload_crash(
    data_dir: &Path,
    server: &Server,
    report: &peerdesk_core::crash::Report,
) -> Result<(), NetError> {
    let identity = Identity::load_or_create(data_dir)?;
    runtime()?.block_on(async {
        let node = Node::bind(any_for(server.addr), &identity, &Policy::recommended())?;
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            peerdesk_net::rendezvous::upload_crash(&node, server, report),
        )
        .await
        .unwrap_or(Err(NetError::Timeout("上传崩溃报告")));
        node.close();
        result
    })
}

/// `peerdesk host`。
pub fn host(args: &[String], out: &mut dyn Write) -> u8 {
    let parsed = match parse_host(args) {
        Ok(parsed) => parsed,
        Err(why) => return usage_error(out, &why),
    };
    let result = runtime().and_then(|rt| rt.block_on(host_async(parsed, out)));
    finish(out, result)
}

/// 控制端连到哪里。
enum Target {
    /// 直接连这个地址。
    Addr(SocketAddr),
    /// 通过服务器找到这台设备。
    Device {
        server: Server,
        device: Lookup,
        policy: PathPolicy,
    },
}

/// `--device` 的值：9 位设备码，或者 64 位十六进制指纹。
fn parse_device(flag: &str, value: Option<&String>) -> Result<Lookup, String> {
    let text = value.ok_or_else(|| format!("{flag} 需要一个值"))?;
    if let Some(code) = parse_code(text) {
        return Ok(Lookup::Code(code));
    }
    parse_hex(text)
        .map(Lookup::Device)
        .map_err(|_| format!("{flag} 要么是 9 位设备码，要么是 64 位十六进制指纹"))
}

struct ConnectArgs {
    target: Target,
    pin: Option<Fingerprint>,
    data_dir: PathBuf,
    seconds: u64,
    send: Option<PathBuf>,
    auth: Auth,
    snapshot: Option<PathBuf>,
    /// 看到第一帧之后输入的一行字（后面自动回车）。
    type_line: Option<String>,
    /// 会员令牌（空表示免费用户）。
    account: String,
}

fn parse_connect(args: &[String]) -> Result<ConnectArgs, String> {
    let (addr, rest) = match args.split_first() {
        Some((addr, rest)) if !addr.starts_with("--") => {
            let addr = addr
                .parse()
                .map_err(|_| format!("不是合法的 地址:端口：{addr}"))?;
            (Some(addr), rest)
        }
        Some(_) | None => (None, args),
    };
    let (mut server, mut server_pin, mut device) = (None, None, None);
    let (mut relay_only, mut direct_only) = (false, false);
    let mut parsed = ConnectArgs {
        target: Target::Addr(([0, 0, 0, 0], 0).into()),
        pin: None,
        data_dir: default_data_dir(),
        seconds: 5,
        send: None,
        auth: password_from_env().map_or(Auth::Open, Auth::Password),
        snapshot: None,
        account: String::new(),
        type_line: None,
    };
    let mut it = rest.iter();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--pin" => parsed.pin = Some(parse_fingerprint(flag, it.next())?),
            "--server" => server = Some(parse_number(flag, it.next())?),
            "--server-pin" => server_pin = Some(parse_fingerprint(flag, it.next())?),
            "--device" => device = Some(parse_device(flag, it.next())?),
            "--relay-only" => relay_only = true,
            "--account" => parsed.account = it.next().ok_or("--account 需要一个值")?.clone(),
            "--direct-only" => direct_only = true,
            "--data-dir" => parsed.data_dir = it.next().ok_or("--data-dir 需要一个值")?.into(),
            "--seconds" => parsed.seconds = parse_number(flag, it.next())?,
            "--send" => parsed.send = Some(it.next().ok_or("--send 需要一个值")?.into()),
            "--snapshot" => {
                parsed.snapshot = Some(it.next().ok_or("--snapshot 需要一个值")?.into());
            }
            "--password" => {
                parsed.auth = Auth::Password(it.next().ok_or("--password 需要一个值")?.clone());
            }
            "--type-line" => {
                parsed.type_line = Some(it.next().ok_or("--type-line 需要一个值")?.clone());
            }
            other => return Err(format!("未知选项：{other}")),
        }
    }
    let policy = match (relay_only, direct_only) {
        (true, true) => return Err("--relay-only 和 --direct-only 只能用一个".into()),
        (true, false) => PathPolicy::RelayOnly,
        (false, true) => PathPolicy::DirectOnly,
        (false, false) => PathPolicy::Auto,
    };
    parsed.target = match (addr, server_of(server, server_pin)?, device) {
        (Some(addr), None, None) if policy == PathPolicy::Auto => Target::Addr(addr),
        (None, Some(server), Some(device)) if parsed.pin.is_none() => {
            // 给的是指纹时，它就是要核对的指纹；给的是设备码时，由服务器告知指纹并在连接时核对。
            if let Lookup::Device(fp) = device {
                parsed.pin = Some(fp);
            }
            Target::Device {
                server,
                device,
                policy,
            }
        }
        (None, None, None) => {
            return Err("connect 需要被控端的 地址:端口，或者 --server 加 --device".into())
        }
        _ => {
            return Err(
                "直连用 <地址:端口> [--pin]；通过服务器连接用 --server 加 --device，\
                 可选 --server-pin、--relay-only、--direct-only，两种方式不能混用"
                    .into(),
            )
        }
    };
    Ok(parsed)
}

fn plan_upload(path: &Path) -> Result<Upload, NetError> {
    let bad = || NetError::Io(format!("无法上传：{}", path.display()));
    let name = path.file_name().and_then(|n| n.to_str()).ok_or_else(bad)?;
    let root = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        Some(_) | None => PathBuf::from("."),
    };
    let files = scan(&root, name)?;
    Ok(Upload { root, files })
}

async fn connect_async(args: ConnectArgs, out: &mut dyn Write) -> Result<u8, NetError> {
    let policy = Policy::recommended();
    let identity = Identity::load_or_create(&args.data_dir)?;
    let upload = args.send.as_deref().map(plan_upload).transpose()?;
    // 通过服务器连接时用双栈端口（IPv6 的候选路径也能探测）；直接连地址时跟它同一地址族。
    // 系统不支持 IPv6 时退回 IPv4。
    let v4: SocketAddr = ([0, 0, 0, 0], 0).into();
    let bind = match &args.target {
        Target::Addr(addr) if addr.is_ipv4() => v4,
        Target::Addr(_) | Target::Device { .. } => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
    };
    let node = Node::bind_first(&[bind, v4], &identity, &policy)?;
    let result = connect_session(&node, &identity, &policy, args, upload, out).await;
    // 无论成功失败，都等关闭通知送到对端再退出：
    // 否则被控端要等到超时才知道这条连接没了，期间不能接待下一个连接。
    node.close();
    let _ = tokio::time::timeout(Duration::from_millis(500), node.wait_idle()).await;
    result
}

async fn connect_session(
    node: &Node,
    identity: &Identity,
    policy: &Policy,
    args: ConnectArgs,
    upload: Option<Upload>,
    out: &mut dyn Write,
) -> Result<u8, NetError> {
    // 走中继时的本机转发器要保持到会话结束。
    let (link, _shim) = match args.target {
        Target::Addr(addr) => (node.connect(addr, args.pin.as_ref()).await?, None),
        Target::Device {
            server,
            device,
            policy,
        } => {
            warn_unpinned_server(out, &server);
            // 配置的服务器在前，缓存的其他集群节点在后：前面的连不上就换下一个。
            let mut servers = vec![server];
            servers.extend(
                load_nodes(&args.data_dir)
                    .into_iter()
                    .filter(|n| n.addr != server.addr),
            );
            let dialed = dial_via(node, &servers, &device, policy, &args.account).await?;
            if let Ok(nodes) = fetch_nodes(node, &server).await {
                if !nodes.is_empty() {
                    let _ = save_nodes(&args.data_dir, &nodes);
                }
            }
            let _ = writeln!(
                out,
                "信令节点      {}",
                servers
                    .iter()
                    .map(|s| s.addr.to_string())
                    .collect::<Vec<_>>()
                    .join(" → ")
            );
            let _ = writeln!(out, "中继说明      {}", dialed.relay_note);
            let path = match dialed.path {
                PathKind::Direct => "直连",
                PathKind::Relay => "中继",
            };
            let _ = writeln!(out, "连接路径      {path}");
            let _ = writeln!(out, "路径种类      {}", dialed.route.label());
            let rtt = dialed.rtt_us.map_or_else(
                || "-".to_owned(),
                |us| format!("{:.1} ms", Duration::from_micros(us).as_secs_f64() * 1000.0),
            );
            let _ = writeln!(out, "路径往返      {rtt}");
            let _ = writeln!(out, "打洞方式      {:?}", dialed.plan);
            let _ = writeln!(out, "路径探测      {}", dialed.report);
            let t = dialed.timing;
            let _ = writeln!(
                out,
                "连接耗时      服务器 {} ms，查询 {} ms，探测 {} ms，直连 {} ms，中继 {} ms",
                t.server_connect.as_millis(),
                t.lookup.as_millis(),
                t.probe.as_millis(),
                t.direct.as_millis(),
                t.relay.as_millis()
            );
            let why = dialed.direct_error.as_deref().unwrap_or("-");
            let _ = writeln!(out, "直连失败原因  {why}");
            (dialed.link, dialed.shim)
        }
    };
    let _ = writeln!(out, "已连接 {}", link.remote_addr());
    let _ = writeln!(out, "对端指纹 {}", to_hex(link.peer_fingerprint()));
    if args.pin.is_none() {
        let _ = writeln!(
            out,
            "警告：没有用 --pin 指定指纹，无法确认连到的是不是你的设备。"
        );
    }
    let _ = out.flush();

    let sink = CountSink::new();
    // 命令行没有键鼠可采集；`--type-line` 时在看到第一帧之后输入一行字并回车（端到端测试用它验证
    // 输入真的到了被控端）。发送端保持打开直到会话结束。
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(64);
    let typing = args.type_line.clone().map(|line| {
        let seen = sink.clone();
        tokio::spawn(async move {
            while seen.count() == 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            for (i, event) in type_line_events(&line).into_iter().enumerate() {
                if input_tx.send(event).await.is_err() {
                    break;
                }
                // 指针移过去之后等一下：窗口要先拿到键盘焦点（没有窗口管理器时焦点跟着指针）。
                if i == 0 {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                }
            }
            input_tx
        })
    });
    let report = controller::run(
        link,
        ControllerOptions {
            session: session(identity, policy, args.auth.clone()),
            deadline_us: 100_000,
            duration: Some(Duration::from_secs(args.seconds)),
            clipboard: None,
            on_established: None,
            on_quality: None,
            audio: None,
        },
        Box::new(sink.clone()),
        input_rx,
        upload,
    )
    .await?;
    if let Some(task) = typing {
        task.abort();
    }

    let lines = [
        format!("会话结束      {}", report.end),
        format!("加密套件      {}", report.suite),
        format!(
            "帧            送达 {}，作废 {}，解码失败 {}，纠错恢复 {} 个分片",
            report.frames_delivered, report.frames_expired, report.bad_frames, report.recovered
        ),
        format!("画面更新      {} 次", sink.count()),
        format!("画面数据      {} 字节", report.video_bytes),
        format!("往返延迟      {} us", report.rtt.as_micros()),
        format!("异常数据报    {}", report.bad_datagrams),
    ];
    for line in lines {
        let _ = writeln!(out, "{line}");
    }
    if let Some(screen) = &report.screen {
        let _ = writeln!(out, "画面尺寸      {}×{}", screen.width(), screen.height());
        if let Some(path) = &args.snapshot {
            std::fs::write(path, to_bmp(screen))
                .map_err(|e| NetError::Io(format!("写入 {} 失败：{e}", path.display())))?;
            let _ = writeln!(out, "画面已保存    {}", path.display());
        }
    }
    if !report.files.is_empty() {
        let _ = writeln!(out, "上传          {} 字节", report.file_bytes);
        for file in &report.files {
            let _ = writeln!(out, "  {} {:?}", file.path, file.outcome);
        }
    }
    Ok(EXIT_OK)
}

/// `--type-line` 发出的事件：指针移到画面中央（让那里的窗口收到键盘输入），输入文字，回车。
fn type_line_events(line: &str) -> Vec<InputEvent> {
    /// 回车键的 USB HID 用途码。
    const ENTER: u16 = 0x28;
    vec![
        InputEvent::MouseMove {
            x: u16::MAX / 2,
            y: u16::MAX / 2,
        },
        InputEvent::Text(line.to_owned()),
        InputEvent::Key {
            code: ENTER,
            down: true,
        },
        InputEvent::Key {
            code: ENTER,
            down: false,
        },
    ]
}

/// `peerdesk connect`。
pub fn connect(args: &[String], out: &mut dyn Write) -> u8 {
    let parsed = match parse_connect(args) {
        Ok(parsed) => parsed,
        Err(why) => return usage_error(out, &why),
    };
    let result = runtime().and_then(|rt| rt.block_on(connect_async(parsed, out)));
    finish(out, result)
}

/// `peerdesk bench-transport`。
/// `netcheck`：网络体检（NAT 类型、路由器和服务器延迟、IPv6）。不请求端口映射。
pub fn netcheck(args: &[String], out: &mut dyn Write) -> u8 {
    let mut it = args.iter();
    let (mut server, mut pin) = (None, None);
    while let Some(flag) = it.next() {
        let parsed = match flag.as_str() {
            "--server" => parse_number(flag, it.next()).map(|a| server = Some(a)),
            "--server-pin" => parse_fingerprint(flag, it.next()).map(|p| pin = Some(p)),
            other => Err(format!("未知选项：{other}")),
        };
        if let Err(e) = parsed {
            return usage_error(out, &e);
        }
    }
    let server = match server_of(server, pin) {
        Ok(server) => server,
        Err(e) => return usage_error(out, &e),
    };
    let result = runtime().and_then(|rt| {
        rt.block_on(async {
            let identity = Identity::generate()?;
            let tries: [SocketAddr; 2] = [
                (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
                ([0, 0, 0, 0], 0).into(),
            ];
            let node = Node::bind_first(&tries, &identity, &Policy::recommended())?;
            let report = peerdesk_net::netcheck::run(&node, server.as_ref()).await;
            node.close();
            Ok::<_, NetError>(report)
        })
    });
    match result {
        Ok(report) => {
            for (item, value) in report.lines() {
                let _ = writeln!(out, "{item:<12}{value}");
            }
            EXIT_OK
        }
        Err(e) => {
            let _ = writeln!(out, "错误：{e}");
            EXIT_FAILED
        }
    }
}

pub fn bench(args: &[String], out: &mut dyn Write) -> u8 {
    let packets = match args {
        [] => 1200usize,
        [flag, value] if flag == "--packets" => match value.parse::<usize>() {
            Ok(n) if (1..=100_000).contains(&n) => n,
            Ok(_) | Err(_) => return usage_error(out, "--packets 的范围是 1 到 100000"),
        },
        _ => return usage_error(out, "bench-transport 只接受 --packets <N>"),
    };
    let result = runtime()
        .and_then(|rt| rt.block_on(bench::compare(packets, 1100)))
        .map(|c| {
            let _ = writeln!(
                out,
                "本机回环，{} 个包，每包 {} 字节（单向延迟，微秒）",
                c.packets, c.payload_bytes
            );
            let _ = writeln!(out, "              收到    p50    p99    最大");
            for (name, l) in [("裸 UDP", c.udp), ("QUIC 数据报", c.quic)] {
                let _ = writeln!(
                    out,
                    "{name:<12}{:>6} {:>6} {:>6} {:>7}",
                    l.received, l.p50_us, l.p99_us, l.max_us
                );
            }
            let _ = writeln!(
                out,
                "QUIC 相对裸 UDP 的中位数差值：{} us",
                i128::from(c.quic.p50_us) - i128::from(c.udp.p50_us)
            );
            EXIT_OK
        });
    finish(out, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_the_same_address_family() {
        assert!(any_for("[::1]:4433".parse().unwrap()).is_ipv6());
        assert!(any_for("1.2.3.4:4433".parse().unwrap()).is_ipv4());
    }

    #[test]
    fn netcheck_command() {
        let run = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
            let mut out = Vec::new();
            let code = netcheck(&args, &mut out);
            (code, String::from_utf8(out).unwrap())
        };
        let (code, text) = run(&[]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(
            text.contains("NAT 类型") && text.contains("本机 → 路由器"),
            "{text}"
        );
        assert_eq!(run(&["--bogus"]).0, crate::EXIT_USAGE);
        assert_eq!(run(&["--server", "nope"]).0, crate::EXIT_USAGE);
        assert_eq!(
            run(&["--server-pin", &"ab".repeat(32)]).0,
            crate::EXIT_USAGE
        );
        let (code, text) = run(&["--server", "127.0.0.1:9"]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("连不上"), "{text}");
    }
    use peerdesk_net::service::{RelayService, SignalService};
    use std::sync::Mutex;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peerdesk-cli-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 可以在另一个线程里边写边读的输出。
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);
    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Shared {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
        fn wait_for(&self, needle: &str) -> String {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            loop {
                let text = self.text();
                if let Some(line) = text.lines().find(|l| l.starts_with(needle)) {
                    return line.trim_start_matches(needle).trim().to_owned();
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "等待 {needle:?} 超时：{text}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    fn run(args: &[&str]) -> (u8, String) {
        let mut out = Vec::new();
        let code = crate::run(&strings(args), &mut out);
        (code, String::from_utf8(out).unwrap())
    }

    #[test]
    fn host_and_connect_end_to_end_with_upload() {
        let (host_dir, ctrl_dir, files) = (temp_dir("host"), temp_dir("ctrl"), temp_dir("files"));
        std::fs::create_dir_all(files.join("docs")).unwrap();
        std::fs::write(files.join("docs/a.txt"), b"alpha").unwrap();
        std::fs::write(files.join("docs/b.bin"), vec![7u8; 200_000]).unwrap();

        let host_out = Shared::default();
        let mut writer = host_out.clone();
        let host_args = strings(&[
            "host",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            host_dir.to_str().unwrap(),
            "--fps",
            "100",
            "--once",
            "--open",
            "--source",
            "demo",
        ]);
        let host_thread = std::thread::spawn(move || crate::run(&host_args, &mut writer));
        let addr = host_out.wait_for("监听");
        let pin = host_out.wait_for("指纹");

        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            ctrl_dir.to_str().unwrap(),
            "--snapshot",
            ctrl_dir.join("seen.bmp").to_str().unwrap(),
            // 足够解出第一帧：覆盖率构建（插桩、未优化）里第一帧要一两秒。
            "--seconds",
            "3",
            "--send",
            files.join("docs").to_str().unwrap(),
            "--type-line",
            "ls",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains(&format!("对端指纹 {pin}")));
        assert!(text.contains("加密套件      aes-256-gcm"));
        assert!(text.contains("解码失败 0"));
        assert!(text.contains("画面尺寸      1280×720"));
        // 看到的画面存成了一张 1280×720 的 BMP
        let bmp = std::fs::read(ctrl_dir.join("seen.bmp")).unwrap();
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(bmp.len(), 54 + 1280 * 720 * 3);
        assert!(text.contains("docs/a.txt Some(Done)"));
        assert!(text.contains("docs/b.bin Some(Done)"));
        assert!(!text.contains("警告"));

        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        let host_text = host_out.text();
        assert!(host_text.contains("已连接"), "{host_text}");
        assert!(host_text.contains("收到 2 个文件"), "{host_text}");
        // --type-line 的 4 个事件（移动、文字、回车按下、抬起）都到了被控端
        assert!(host_text.contains("注入 4 个输入事件"), "{host_text}");
        assert_eq!(
            std::fs::read(host_dir.join("received/docs/a.txt")).unwrap(),
            b"alpha"
        );
        assert_eq!(
            std::fs::read(host_dir.join("received/docs/b.bin"))
                .unwrap()
                .len(),
            200_000
        );

        // 身份是持久的：重启被控端后指纹不变
        let again = Identity::load_or_create(&host_dir).unwrap();
        assert_eq!(to_hex(&again.fingerprint()), pin);
        for dir in [host_dir, ctrl_dir, files] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn connect_without_pin_warns_and_wrong_pin_is_refused() {
        let (host_dir, ctrl_dir) = (temp_dir("host2"), temp_dir("ctrl2"));
        let host_out = Shared::default();
        let mut writer = host_out.clone();
        let host_args = strings(&[
            "host",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            host_dir.to_str().unwrap(),
            "--recv-dir",
            host_dir.join("in").to_str().unwrap(),
            "--once",
            "--open",
            "--source",
            "demo",
        ]);
        let host_thread = std::thread::spawn(move || crate::run(&host_args, &mut writer));
        let addr = host_out.wait_for("监听");

        // 指纹不对：拒绝，不进入会话
        let wrong = "ab".repeat(32);
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &wrong,
            "--data-dir",
            ctrl_dir.to_str().unwrap(),
        ]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("对端身份与预期不符"), "{text}");

        // 不给指纹：能连，但有警告
        let (code, text) = run(&[
            "connect",
            &addr,
            "--data-dir",
            ctrl_dir.to_str().unwrap(),
            // 足够解出第一帧：覆盖率构建（插桩、未优化）里第一帧要一两秒。
            "--seconds",
            "3",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("警告"));
        assert!(!text.contains("上传"));
        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        let host_text = host_out.text();
        assert!(host_text.contains("会话失败"), "{host_text}");
        assert!(host_text.contains("会话结束"), "{host_text}");
        for dir in [host_dir, ctrl_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn start_host(
        extra: &[&str],
        dir: &Path,
    ) -> (Shared, std::thread::JoinHandle<u8>, String, String) {
        let out = Shared::default();
        let mut writer = out.clone();
        let mut args = strings(&[
            "host",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            dir.to_str().unwrap(),
            "--once",
            "--source",
            "demo",
        ]);
        args.extend(strings(extra));
        let thread = std::thread::spawn(move || crate::run(&args, &mut writer));
        let addr = out.wait_for("监听");
        let pin = out.wait_for("指纹");
        (out, thread, addr, pin)
    }

    #[test]
    fn password_protected_host_end_to_end() {
        let (host_dir, ctrl_dir) = (temp_dir("pw-host"), temp_dir("pw-ctrl"));
        let (host_out, host_thread, addr, pin) = start_host(
            &["--password", "correct horse", "--no-input", "--no-files"],
            &host_dir,
        );
        let c = ctrl_dir.to_str().unwrap();

        // 密码不对：进不去
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            c,
            "--password",
            "wrong guess",
        ]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("密码错误"), "{text}");
        // 没带密码：进不去
        let (code, _) = run(&["connect", &addr, "--pin", &pin, "--data-dir", c]);
        assert_eq!(code, EXIT_FAILED);
        // 密码正确
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            c,
            "--password",
            "correct horse",
            // 足够解出第一帧：覆盖率构建（插桩、未优化）里第一帧要一两秒。
            "--seconds",
            "3",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        let host_text = host_out.text();
        assert!(host_text.contains("会话失败"), "{host_text}");
        assert!(host_text.contains("会话结束"), "{host_text}");
        assert!(!host_text.contains("警告：--open"));
        for dir in [host_dir, ctrl_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn trusted_device_connects_without_password_and_strangers_cannot() {
        let (host_dir, friend_dir, stranger_dir) = (
            temp_dir("tr-host"),
            temp_dir("tr-friend"),
            temp_dir("tr-stranger"),
        );
        let friend = Identity::load_or_create(&friend_dir).unwrap();
        let friend_fp = to_hex(&friend.fingerprint());
        let (host_out, host_thread, addr, pin) = start_host(&["--trust", &friend_fp], &host_dir);

        // 陌生设备：没有密码可用（被控端内部是一个随机密码）
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            stranger_dir.to_str().unwrap(),
            "--password",
            "anything at all",
        ]);
        assert_eq!(code, EXIT_FAILED, "{text}");
        // 受信任的设备：不需要密码
        let (code, text) = run(&[
            "connect",
            &addr,
            "--pin",
            &pin,
            "--data-dir",
            friend_dir.to_str().unwrap(),
            // 足够解出第一帧：覆盖率构建（插桩、未优化）里第一帧要一两秒。
            "--seconds",
            "3",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        assert!(host_out.text().contains(&friend_fp));
        for dir in [host_dir, friend_dir, stranger_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn host_refuses_to_start_without_an_access_rule() {
        use crate::EXIT_USAGE;
        let d = temp_dir("norule");
        let dir = d.to_str().unwrap();
        let (code, text) = run(&["host", "--data-dir", dir]);
        assert_eq!(code, EXIT_USAGE);
        assert!(text.contains("必须指定谁可以控制这台设备"), "{text}");
        for bad in [
            vec!["host", "--data-dir", dir, "--password", "short"],
            vec![
                "host",
                "--data-dir",
                dir,
                "--password",
                "long enough",
                "--open",
            ],
            vec!["host", "--data-dir", dir, "--password"],
            vec!["host", "--data-dir", dir, "--trust"],
            vec!["host", "--data-dir", dir, "--trust", "nothex"],
            vec!["connect", "127.0.0.1:1", "--password"],
        ] {
            assert_eq!(run(&bad).0, EXIT_USAGE, "{bad:?}");
        }
        // --open 时受信任列表被清空，并给出警告
        let parsed = parse_host(&strings(&["--open", "--trust", &"ab".repeat(32)])).unwrap();
        assert!(parsed.trusted.is_empty() && parsed.open);
        // 只有 --trust 时，内部生成一个随机密码
        let parsed = parse_host(&strings(&["--trust", &"ab".repeat(32)])).unwrap();
        assert!(matches!(parsed.auth, Auth::Password(ref p) if p.len() == 64));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn argument_errors() {
        use crate::EXIT_USAGE;
        let fp = "ab".repeat(32);
        for bad in [
            vec!["host", "--bogus"],
            vec!["host", "--listen"],
            vec!["host", "--listen", "nope"],
            vec!["host", "--data-dir"],
            vec!["host", "--recv-dir"],
            vec!["host", "--fps", "0"],
            vec!["host", "--fps", "999"],
            vec!["host", "--source"],
            vec!["host", "--source", "camera"],
            vec!["host", "--display"],
            vec!["host", "--display", "x"],
            vec!["connect"],
            vec!["connect", "nope"],
            vec!["connect", "127.0.0.1:1", "--pin"],
            vec!["connect", "127.0.0.1:1", "--pin", "short"],
            vec!["connect", "127.0.0.1:1", "--data-dir"],
            vec!["connect", "127.0.0.1:1", "--seconds", "x"],
            vec!["connect", "127.0.0.1:1", "--send"],
            vec!["connect", "127.0.0.1:1", "--snapshot"],
            vec!["connect", "127.0.0.1:1", "--bogus"],
            vec!["bench-transport", "--packets", "0"],
            vec!["host", "--open", "--server"],
            vec!["host", "--open", "--server", "nope"],
            vec!["host", "--open", "--server-pin"],
            vec!["host", "--open", "--server-pin", "short"],
            vec!["host", "--open", "--server-pin", &fp],
            vec!["connect", "--relay-only"],
            vec!["connect", "127.0.0.1:1", "--relay-only"],
            vec!["connect", "127.0.0.1:1", "--server", "127.0.0.1:2"],
            vec!["connect", "127.0.0.1:1", "--device", &fp],
            vec!["connect", "--server", "127.0.0.1:2"],
            vec!["connect", "--device", &fp],
            vec!["connect", "--server-pin", &fp, "--device", &fp],
            vec![
                "connect",
                "--server",
                "127.0.0.1:2",
                "--device",
                &fp,
                "--pin",
                &fp,
            ],
            vec![
                "connect",
                "--server",
                "127.0.0.1:2",
                "--device",
                &fp,
                "--relay-only",
                "--direct-only",
            ],
            vec!["connect", "--server"],
            vec!["connect", "--server", "nope"],
            vec!["connect", "--device"],
            vec!["connect", "--device", "short"],
            vec!["connect", "--server-pin"],
            vec!["bench-transport", "--packets", "x"],
            vec!["bench-transport", "extra"],
        ] {
            assert_eq!(run(&bad).0, EXIT_USAGE, "{bad:?}");
        }
    }

    #[test]
    fn server_options_are_parsed() {
        let fp = "cd".repeat(32);
        let host = parse_host(&strings(&[
            "--open",
            "--server",
            "127.0.0.1:7",
            "--server-pin",
            &fp,
        ]))
        .unwrap();
        let server = host.server.unwrap();
        assert_eq!(server.addr, "127.0.0.1:7".parse().unwrap());
        assert_eq!(server.pin, Some(parse_hex(&fp).unwrap()));
        assert!(parse_host(&strings(&["--open"])).unwrap().server.is_none());
        for (flag, expected) in [
            (None, PathPolicy::Auto),
            (Some("--relay-only"), PathPolicy::RelayOnly),
            (Some("--direct-only"), PathPolicy::DirectOnly),
        ] {
            let mut args = strings(&["--server", "127.0.0.1:7", "--device", &fp]);
            args.extend(flag.map(str::to_owned));
            let parsed = parse_connect(&args).unwrap();
            // 设备的指纹同时是要核对的指纹
            assert_eq!(parsed.pin, Some(parse_hex(&fp).unwrap()));
            assert!(matches!(
                parsed.target,
                Target::Device { server, policy, .. } if policy == expected && server.pin.is_none()
            ));
        }
        assert!(matches!(
            parse_connect(&strings(&["127.0.0.1:7"])).unwrap().target,
            Target::Addr(_)
        ));
    }

    /// 把节点列表固定为 `nodes` 的目录，中继交给单机目录。
    struct ListDirectory {
        nodes: Vec<(SocketAddr, Fingerprint, bool)>,
        local: peerdesk_net::service::LocalDirectory,
    }

    impl peerdesk_net::service::Directory for ListDirectory {
        fn plan_relay(
            &self,
            caller: &Fingerprint,
            account: &str,
        ) -> peerdesk_net::service::RelayPlan {
            self.local.plan_relay(caller, account)
        }
        fn nodes(&self) -> Vec<(SocketAddr, Fingerprint, bool)> {
            self.nodes.clone()
        }
    }

    #[test]
    fn device_by_code_or_fingerprint() {
        let code = "123456789".to_owned();
        assert!(matches!(
            parse_device("--device", Some(&code)),
            Ok(Lookup::Code(123_456_789))
        ));
        let fp = "ab".repeat(32);
        assert!(matches!(
            parse_device("--device", Some(&fp)),
            Ok(Lookup::Device(_))
        ));
        assert!(parse_device("--device", None).is_err());
        assert!(parse_device("--device", Some(&"zz".to_owned())).is_err());
    }

    #[test]
    fn node_cache_round_trip() {
        let dir = temp_dir("nodes-cache");
        assert!(load_nodes(&dir).is_empty());
        let nodes = vec![
            Server {
                addr: "127.0.0.1:4433".parse().unwrap(),
                pin: Some([7; 32]),
            },
            // 没有指纹的不存
            Server {
                addr: "127.0.0.1:4434".parse().unwrap(),
                pin: None,
            },
        ];
        save_nodes(&dir, &nodes).unwrap();
        // 读不懂的行跳过
        let mut text = std::fs::read_to_string(dir.join(NODES_FILE)).unwrap();
        text.push_str("garbage\n1.2.3.4:5 zz\nnope 00\n");
        std::fs::write(dir.join(NODES_FILE), text).unwrap();
        let back = load_nodes(&dir);
        assert_eq!(back.len(), 1);
        assert_eq!(
            back.first().map(|n| (n.addr, n.pin)),
            Some((nodes[0].addr, Some([7; 32])))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn host_joins_every_cluster_node_and_falls_back_to_the_cache() {
        let (a_dir, b_dir, host_dir) = (
            temp_dir("join-a"),
            temp_dir("join-b"),
            temp_dir("join-host"),
        );
        let b_id = Identity::load_or_create(&b_dir).unwrap();
        let b = TestServer::start(&b_id, ([127, 0, 0, 1], 0).into()).unwrap();
        let b_addr: SocketAddr = b.addr().parse().unwrap();

        // 节点 A：节点列表里有 A 自己和 B
        let a_id = Identity::load_or_create(&a_dir).unwrap();
        let rt = runtime().unwrap();
        let (a_node, _a_signal) = rt.block_on(async {
            let node = Arc::new(
                Node::bind(([127, 0, 0, 1], 0).into(), &a_id, &Policy::recommended()).unwrap(),
            );
            let a_addr = node.local_addr().unwrap();
            let directory = Arc::new(ListDirectory {
                nodes: vec![
                    (a_addr, a_id.fingerprint(), false),
                    (b_addr, b_id.fingerprint(), false),
                ],
                local: peerdesk_net::service::LocalDirectory::new(None, None),
            });
            // 没有中继的单机目录：不安排中继
            let plan = peerdesk_net::service::Directory::plan_relay(&*directory, &[0; 32], "");
            assert!(plan.relay.is_none(), "{plan:?}");
            assert!(format!("{:?}", directory.local).contains("LocalDirectory"));
            let signal =
                SignalService::start_with_directory(Arc::clone(&node), None, directory, None);
            (node, signal)
        });
        let seed = Server {
            addr: a_node.local_addr().unwrap(),
            pin: Some(a_id.fingerprint()),
        };

        // 被控端只知道 A，也会登记到 B 上，并把节点列表存下来
        let host_id = Identity::load_or_create(&host_dir).unwrap();
        let host_rt = runtime().unwrap();
        let host = host_rt.block_on(async {
            Arc::new(
                Node::bind(([127, 0, 0, 1], 0).into(), &host_id, &Policy::recommended()).unwrap(),
            )
        });
        let task = host_rt.spawn(join_cluster(
            Arc::clone(&host),
            seed,
            Vec::new(),
            host_dir.clone(),
        ));
        b.wait_online(1);
        assert_eq!(load_nodes(&host_dir).len(), 2);
        task.abort();
        host.close();
        drop(host_rt);
        b.shut_down();

        // A 下线后再开一个被控端：问不到节点列表，用缓存里的 B
        a_node.close();
        drop(rt);
        let b = TestServer::start(&b_id, b_addr).unwrap();
        let host_rt = runtime().unwrap();
        let host = host_rt.block_on(async {
            Arc::new(
                Node::bind(([127, 0, 0, 1], 0).into(), &host_id, &Policy::recommended()).unwrap(),
            )
        });
        let task = host_rt.spawn(join_cluster(
            Arc::clone(&host),
            seed,
            Vec::new(),
            host_dir.clone(),
        ));
        b.wait_online(1);
        task.abort();
        host.close();
        drop(host_rt);
        b.shut_down();
        for dir in [a_dir, b_dir, host_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 测试用的信令服务器（带中继），跑在自己的运行时上。
    struct TestServer {
        node: Arc<Node>,
        signal: SignalService,
        relay: Arc<RelayService>,
        rt: tokio::runtime::Runtime,
    }

    impl TestServer {
        fn start(identity: &Identity, bind: SocketAddr) -> Result<Self, NetError> {
            let rt = runtime()?;
            let (node, signal, relay) = rt.block_on(async {
                let relay = Arc::new(RelayService::start(([127, 0, 0, 1], 0).into(), None).await?);
                let node = Arc::new(Node::bind(bind, identity, &Policy::recommended())?);
                let signal = SignalService::start(Arc::clone(&node), Some(Arc::clone(&relay)));
                Ok::<_, NetError>((node, signal, relay))
            })?;
            Ok(Self {
                node,
                signal,
                relay,
                rt,
            })
        }

        fn addr(&self) -> String {
            self.node.local_addr().unwrap().to_string()
        }

        fn wait_online(&self, n: usize) {
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while self.signal.online() != n {
                assert!(
                    std::time::Instant::now() < deadline,
                    "等待 {n} 台设备在线超时"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        /// 关掉服务器，并等关闭通知送到各个设备。
        fn shut_down(self) {
            self.node.close();
            let node = &self.node;
            let _ = self.rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(2), node.wait_idle()).await
            });
        }
    }

    #[test]
    fn connect_through_the_server_over_the_relay() {
        let (server_dir, host_dir, ctrl_dir) = (
            temp_dir("rv-server"),
            temp_dir("rv-host"),
            temp_dir("rv-ctrl"),
        );
        let server_id = Identity::load_or_create(&server_dir).unwrap();
        let server = TestServer::start(&server_id, ([127, 0, 0, 1], 0).into()).unwrap();
        let (addr, server_pin) = (server.addr(), to_hex(&server_id.fingerprint()));
        let h = host_dir.to_str().unwrap();
        let c = ctrl_dir.to_str().unwrap();

        // 服务器指纹不对：被控端不登记，直接报错
        let wrong = "ab".repeat(32);
        let (code, text) = run(&[
            "host",
            "--listen",
            "127.0.0.1:0",
            "--data-dir",
            h,
            "--open",
            "--server",
            &addr,
            "--server-pin",
            &wrong,
        ]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("对端身份与预期不符"), "{text}");

        // 被控端不核对服务器指纹：有警告
        let (host_out, host_thread, _, pin) = start_host(&["--open", "--server", &addr], &host_dir);
        assert!(!host_out.wait_for("已在服务器").is_empty());
        assert!(host_out.text().contains("--server-pin"));
        server.wait_online(1);

        // 不在线的设备
        let (code, text) = run(&[
            "connect",
            "--server",
            &addr,
            "--server-pin",
            &server_pin,
            "--device",
            &wrong,
            "--data-dir",
            c,
        ]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("设备不在线"), "{text}");

        // 只走中继；控制端也不核对服务器指纹
        let (code, text) = run(&[
            "connect",
            "--server",
            &addr,
            "--device",
            &pin,
            "--relay-only",
            "--data-dir",
            c,
            // 足够解出第一帧：覆盖率构建（插桩、未优化）里第一帧要一两秒。
            "--seconds",
            "3",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("连接路径      中继"), "{text}");
        assert!(text.contains("警告：没有用 --server-pin"), "{text}");
        assert!(text.contains(&format!("对端指纹 {pin}")), "{text}");
        assert!(text.contains("画面尺寸      1280×720"), "{text}");
        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        assert!(host_out.text().contains("会话结束"));
        let (total, _, _) = server.relay.totals();
        assert!(total.bytes > 10_000, "会话的数据经过了中继：{total:?}");
        drop(server);
        for dir in [server_dir, host_dir, ctrl_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// 服务器重启后，被控端自己重新登记；之后控制端照常找到它，并且直连。
    #[test]
    fn host_registers_again_after_the_server_restarts() {
        let (server_dir, host_dir, ctrl_dir) = (
            temp_dir("rr-server"),
            temp_dir("rr-host"),
            temp_dir("rr-ctrl"),
        );
        let server_id = Identity::load_or_create(&server_dir).unwrap();
        let server = TestServer::start(&server_id, ([127, 0, 0, 1], 0).into()).unwrap();
        let (addr, server_pin) = (server.addr(), to_hex(&server_id.fingerprint()));
        let (host_out, host_thread, _, pin) = start_host(
            &["--open", "--server", &addr, "--server-pin", &server_pin],
            &host_dir,
        );
        host_out.wait_for("已在服务器");
        assert!(!host_out.text().contains("--server-pin"));
        server.wait_online(1);
        server.shut_down();

        // 在同一个地址上重新启动（端口释放可能要一点时间）
        let server = (0..200)
            .find_map(|_| {
                std::thread::sleep(Duration::from_millis(50));
                TestServer::start(&server_id, addr.parse().unwrap()).ok()
            })
            .unwrap();
        server.wait_online(1);

        let (code, text) = run(&[
            "connect",
            "--server",
            &addr,
            "--server-pin",
            &server_pin,
            "--device",
            &pin,
            "--data-dir",
            ctrl_dir.to_str().unwrap(),
            // 足够解出第一帧：覆盖率构建（插桩、未优化）里第一帧要一两秒。
            "--seconds",
            "3",
        ]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("连接路径      直连"), "{text}");
        assert!(!text.contains("警告"), "{text}");
        assert_eq!(host_thread.join().unwrap(), EXIT_OK);
        drop(server);
        for dir in [server_dir, host_dir, ctrl_dir] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn runtime_failures_are_reported() {
        let dir = temp_dir("fail");
        let d = dir.to_str().unwrap();
        // 连不上
        let (code, text) = run(&["connect", "127.0.0.1:9", "--data-dir", d]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("错误"));
        // 要上传的路径不存在
        let (code, text) = run(&[
            "connect",
            "127.0.0.1:9",
            "--data-dir",
            d,
            "--send",
            "/nonexistent/x.bin",
        ]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("错误"), "{text}");
        // 没有文件名的路径
        assert!(plan_upload(Path::new("/")).is_err());
        // 监听端口被占
        let taken = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap().to_string();
        let (code, text) = run(&["host", "--listen", &addr, "--data-dir", d, "--open"]);
        assert_eq!(code, EXIT_FAILED);
        assert!(text.contains("绑定"), "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// 指定了 `--source screen` 而屏幕打不开：报错退出；没有指定时改用演示桌面并提示。
    #[test]
    fn host_refuses_when_the_screen_cannot_be_opened() {
        let dir = temp_dir("source");
        let d = dir.to_str().unwrap();
        let base = ["host", "--listen", "127.0.0.1:0", "--data-dir", d, "--open"];
        // 不管写不写 --source screen：屏幕打不开就报错，不拿演示画面顶替
        for extra in [
            &["--source", "screen", "--display", "999"][..],
            &["--display", "999"][..],
        ] {
            let mut args = base.to_vec();
            args.extend(extra);
            let (code, text) = run(&args);
            assert_eq!(code, EXIT_FAILED, "{text}");
            assert!(text.contains("无法采集屏幕"), "{text}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn relative_upload_path_uses_current_directory_as_root() {
        let name = format!("peerdesk-rel-{}.tmp", std::process::id());
        std::fs::write(&name, b"x").unwrap();
        let upload = plan_upload(Path::new(&name)).unwrap();
        std::fs::remove_file(&name).unwrap();
        assert_eq!(upload.root, PathBuf::from("."));
        assert_eq!(upload.files.len(), 1);
    }

    #[test]
    fn bench_prints_both_transports() {
        let (code, text) = run(&["bench-transport", "--packets", "120"]);
        assert_eq!(code, EXIT_OK, "{text}");
        assert!(text.contains("裸 UDP"));
        assert!(text.contains("QUIC 数据报"));
        assert!(text.contains("中位数差值"));
    }

    #[test]
    fn data_dir_has_a_default() {
        assert!(!default_data_dir().as_os_str().is_empty());
        let count = Arc::new(AtomicU64::new(0));
        CountInput(Arc::clone(&count)).inject(&InputEvent::ReleaseAll);
        assert_eq!(count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn type_line_moves_types_and_presses_enter() {
        let events = type_line_events("ls");
        assert!(matches!(events[0], InputEvent::MouseMove { .. }));
        assert_eq!(events[1], InputEvent::Text("ls".into()));
        assert_eq!(
            events[2],
            InputEvent::Key {
                code: 0x28,
                down: true
            }
        );
        assert_eq!(
            events[3],
            InputEvent::Key {
                code: 0x28,
                down: false
            }
        );
        let parsed = parse_connect(&strings(&["127.0.0.1:1", "--type-line", "echo hi"])).unwrap();
        assert_eq!(parsed.type_line.as_deref(), Some("echo hi"));
        assert!(parse_connect(&strings(&["127.0.0.1:1", "--type-line"])).is_err());
    }
}
