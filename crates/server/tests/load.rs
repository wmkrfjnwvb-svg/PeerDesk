//! 服务端并发压力测试（默认忽略，手动或 CI 的 `[load]` 触发）：
//! 三个真实的服务端节点（一个付费、两个免费）跑在本进程里，大量被控端登记，
//! 许多控制端同时连接（一半只走中继、一半自动选路），经中继推送数据报。
//! 测量并输出 Markdown 表格：登记成功率和延迟、连接成功率和延迟、中继吞吐、CPU、错误。
//!
//! 运行：`cargo test --release -p peerdesk-server --test load -- --ignored --nocapture`
//!
//! 环境变量：
//! - `PEERDESK_LOAD_DEVICES`：登记的被控端数量，默认 200；
//! - `PEERDESK_LOAD_SESSIONS`：同时连接的控制端数量，默认 50（不超过设备数）；
//! - `PEERDESK_LOAD_SECONDS`：每个会话推送数据的秒数，默认 5。
//!
//! 说明：服务端和模拟客户端在同一个进程里，所以 CPU 时间是两者之和，只能用来比较前后变化，
//! 不能当成服务端单独的占用。环回网络没有丢包和时延，吞吐是服务端转发能力的上限参考。

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stdout,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use peerdesk_core::billing::issue;
use peerdesk_core::cluster::ClusterConfig;
use peerdesk_core::config::ServerConfig;
use peerdesk_core::crypto::Policy;
use peerdesk_core::proto::Channel;
use peerdesk_net::rendezvous::{
    answer, dial_via, register, Lookup, Path as Route, PathPolicy, Server,
};
use peerdesk_net::{Identity, Node};
use peerdesk_server::node::Node as ServerNode;

const SECRET: &str = "load-cluster-secret-0123456789abcdef";

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn free_udp() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn free_tcp() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn node_toml(id: u64, role: &str, peers: &[u16], cluster: u16, dir: &Path) -> String {
    let peers: Vec<String> = peers.iter().map(|p| format!("\"127.0.0.1:{p}\"")).collect();
    format!(
        r#"node_id = {id}
region = "load"
role = "{role}"
http_listen = "127.0.0.1:{http}"
cluster_listen = "127.0.0.1:{cluster}"
advertise = "127.0.0.1:{cluster}"
peers = [{peers}]
cluster_secret = "{SECRET}"
signal_listen = "127.0.0.1:{signal}"
relay_listen = "127.0.0.1:{relay}"
data_dir = '{dir}'
free_device_mb = 0
free_ip_mb = 0
bill_every_ms = 500
"#,
        http = free_tcp(),
        signal = free_udp(),
        relay = free_udp(),
        dir = dir.display(),
        peers = peers.join(","),
    )
}

fn start_node(toml: String) -> ServerNode {
    let cfg = ServerConfig::from_toml(&toml).unwrap();
    let timing = ClusterConfig {
        heartbeat_us: 200_000,
        suspect_after_us: 1_000_000,
        dead_after_us: 2_000_000,
    };
    // 节点有自己的异步运行时，要在测试的运行时之外（普通线程里）启动和停止。
    std::thread::spawn(move || ServerNode::start(&cfg, timing).unwrap())
        .join()
        .unwrap()
}

fn any() -> SocketAddr {
    ([127, 0, 0, 1], 0).into()
}

/// 进程累计的 CPU 时间（用户态加内核态），单位秒。读不到时返回 `None`。
fn cpu_seconds() -> Option<f64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // comm 字段可能带空格，从最后一个右括号之后数。
    let rest = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // 右括号之后第 1 个字段是 state（总第 3 个），utime 是总第 14 个，stime 是第 15 个。
    let utime: f64 = fields.get(11)?.parse().ok()?;
    let stime: f64 = fields.get(12)?.parse().ok()?;
    // Linux 的时钟节拍固定按 100 Hz 对外报告（USER_HZ）。
    Some((utime + stime) / 100.0)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

/// 一次连接的结果。
struct DialResult {
    ok: bool,
    ms: f64,
    relay: bool,
    sent: u64,
    delivered: u64,
    error: Option<String>,
}

fn add_error(errors: &Mutex<BTreeMap<String, u64>>, what: &str) {
    let mut key: String = what.chars().take(120).collect();
    key = key.replace('|', "/").replace('\n', " ");
    *errors.lock().unwrap().entry(key).or_insert(0) += 1;
}

#[test]
#[ignore = "压力测试：用 --ignored 手动运行，CI 里提交信息带 [load] 时运行"]
fn server_cluster_under_load() {
    let devices = env_num("PEERDESK_LOAD_DEVICES", 200) as usize;
    let sessions = (env_num("PEERDESK_LOAD_SESSIONS", 50) as usize).min(devices);
    let seconds = env_num("PEERDESK_LOAD_SECONDS", 5);
    assert!(devices > 0, "PEERDESK_LOAD_DEVICES 必须大于 0");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .enable_all()
        .build()
        .unwrap();
    let summary = runtime.block_on(run(devices, sessions, seconds));
    runtime.shutdown_background();

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/load");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("summary.md"), &summary.table).unwrap();
    println!("{}", summary.table);
    for failure in &summary.failures {
        println!("门槛未达标：{failure}");
    }
    assert!(summary.failures.is_empty(), "{:?}", summary.failures);
}

struct Summary {
    table: String,
    failures: Vec<String>,
}

async fn run(devices: usize, sessions: usize, seconds: u64) -> Summary {
    let base = std::env::temp_dir().join(format!("peerdesk-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dirs: Vec<PathBuf> = (1..=3).map(|i| base.join(format!("node{i}"))).collect();
    let cluster_ports = [free_udp(), free_udp(), free_udp()];
    let roles = ["paid", "free", "free"];
    let nodes: Vec<ServerNode> = (0..3)
        .map(|i| {
            let peers: Vec<u16> = (0..3)
                .filter(|j| *j != i)
                .map(|j| cluster_ports[j])
                .collect();
            start_node(node_toml(
                i as u64 + 1,
                roles[i],
                &peers,
                cluster_ports[i],
                &dirs[i],
            ))
        })
        .collect();
    let servers: Vec<Server> = nodes
        .iter()
        .map(|n| {
            let info = n.signal().unwrap();
            Server {
                addr: info.addr,
                pin: Some(info.fingerprint),
            }
        })
        .collect();
    for _ in 0..100 {
        if nodes.iter().all(|n| n.alive_count() == 3) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let policy = Policy::recommended();
    let errors: Arc<Mutex<BTreeMap<String, u64>>> = Arc::default();
    let received: Arc<Vec<AtomicU64>> = Arc::new((0..devices).map(|_| AtomicU64::new(0)).collect());
    let cpu_start = cpu_seconds();
    let wall_start = Instant::now();

    println!("节点已启动，开始登记");
    // ---- 1. 被控端登记：每台设备在一个“归属节点”上登记，所有设备同时发起 ----
    let (reg_tx, mut reg_rx) = tokio::sync::mpsc::unbounded_channel::<(bool, f64)>();
    let mut fingerprints = Vec::with_capacity(devices);
    let mut keep = Vec::new();
    for index in 0..devices {
        let identity = Identity::generate().unwrap();
        fingerprints.push(identity.fingerprint());
        let host = Arc::new(Node::bind(any(), &identity, &policy).unwrap());
        let server = servers[index % servers.len()];
        let tx = reg_tx.clone();
        let errors = Arc::clone(&errors);
        let received = Arc::clone(&received);
        // 接受连接并统计收到的数据报。
        let acceptor = Arc::clone(&host);
        keep.push(tokio::spawn(async move {
            while let Some(link) = acceptor.accept().await {
                if let Ok(link) = link {
                    let received = Arc::clone(&received);
                    tokio::spawn(async move {
                        while link.recv().await.is_ok() {
                            if let Some(counter) = received.get(index) {
                                let _ = counter.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    });
                }
            }
        }));
        keep.push(tokio::spawn(async move {
            let started = Instant::now();
            let registration = register(&host, &server).await;
            let elapsed = ms(started.elapsed());
            let mut registration = match registration {
                Ok(r) => {
                    let _ = tx.send((true, elapsed));
                    r
                }
                Err(e) => {
                    add_error(&errors, &format!("登记失败：{e}"));
                    let _ = tx.send((false, elapsed));
                    return;
                }
            };
            let mut shims = Vec::new();
            while let Some(incoming) = registration.incoming.recv().await {
                match answer(&host, &incoming).await {
                    Ok(shim) => shims.push(shim),
                    Err(e) => add_error(&errors, &format!("应答失败：{e}")),
                }
            }
        }));
    }
    drop(reg_tx);
    let mut reg_ok = 0u64;
    let mut reg_ms = Vec::new();
    let mut reg_failed = 0u64;
    for _ in 0..devices {
        let Some((ok, elapsed)) = reg_rx.recv().await else {
            break;
        };
        if ok {
            reg_ok += 1;
            reg_ms.push(elapsed);
        } else {
            reg_failed += 1;
        }
    }
    let reg_wall = wall_start.elapsed();
    println!("登记完成：成功 {reg_ok}，失败 {reg_failed}");
    reg_ms.sort_by(f64::total_cmp);
    // 让服务端把登记都处理完。
    tokio::time::sleep(Duration::from_millis(500)).await;

    // ---- 2. 控制端同时连接：偶数号只走中继（一半带会员令牌），奇数号自动选路 ----
    let dial_start = Instant::now();
    let mut tasks = Vec::new();
    for (session, fp) in fingerprints.iter().copied().enumerate().take(sessions) {
        let identity = Identity::generate().unwrap();
        let ctrl = Node::bind(any(), &identity, &policy).unwrap();
        // 归属节点排第一，其余节点作为备选。
        let mut order = servers.clone();
        order.rotate_left(session % servers.len());
        let relay_only = session % 2 == 0;
        let token = if relay_only && session % 4 == 0 {
            let mut id = [0u8; 8];
            id[..8].copy_from_slice(&(session as u64 + 1).to_be_bytes());
            issue(SECRET.as_bytes(), id, 50)
        } else {
            String::new()
        };
        let received = Arc::clone(&received);
        tasks.push(tokio::spawn(async move {
            let path_policy = if relay_only {
                PathPolicy::RelayOnly
            } else {
                PathPolicy::Auto
            };
            let started = Instant::now();
            let dialed = dial_via(&ctrl, &order, &Lookup::Device(fp), path_policy, &token).await;
            let elapsed = ms(started.elapsed());
            let dialed = match dialed {
                Ok(d) => d,
                Err(e) => {
                    return DialResult {
                        ok: false,
                        ms: elapsed,
                        relay: false,
                        sent: 0,
                        delivered: 0,
                        error: Some(format!("连接失败：{e}")),
                    }
                }
            };
            let relay = dialed.path == Route::Relay;
            let before = received
                .get(session)
                .map_or(0, |c| c.load(Ordering::Relaxed));
            // 约 1000 个数据报每秒，每个 1000 字节（约 8 Mbps），推送 `seconds` 秒。
            let mut sent = 0u64;
            let mut tick = tokio::time::interval(Duration::from_millis(1));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let end = Instant::now() + Duration::from_secs(seconds);
            while Instant::now() < end {
                let _ = tick.tick().await;
                if dialed.link.send(Channel::Video, &[7u8; 1000]).is_ok() {
                    sent += 1;
                }
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
            let delivered = received
                .get(session)
                .map_or(0, |c| c.load(Ordering::Relaxed))
                .saturating_sub(before);
            dialed.link.close("done");
            DialResult {
                ok: true,
                ms: elapsed,
                relay,
                sent,
                delivered,
                error: None,
            }
        }));
    }
    println!("连接已发起：{sessions} 个");
    let mut results = Vec::new();
    for task in tasks {
        match task.await {
            Ok(r) => results.push(r),
            Err(e) => add_error(&errors, &format!("会话任务异常：{e}")),
        }
    }
    let dial_wall = dial_start.elapsed();
    for r in &results {
        if let Some(e) = &r.error {
            add_error(&errors, e);
        }
    }
    let cpu_used = cpu_seconds().zip(cpu_start).map(|(end, start)| end - start);
    let total_wall = wall_start.elapsed();

    for handle in &keep {
        handle.abort();
    }
    for node in nodes {
        std::thread::spawn(move || node.stop()).join().unwrap();
    }
    let _ = std::fs::remove_dir_all(&base);

    // ---- 3. 汇总 ----
    let dial_ok = results.iter().filter(|r| r.ok).count();
    let mut dial_ms: Vec<f64> = results.iter().filter(|r| r.ok).map(|r| r.ms).collect();
    dial_ms.sort_by(f64::total_cmp);
    let relayed: Vec<&DialResult> = results.iter().filter(|r| r.ok && r.relay).collect();
    let direct = results.iter().filter(|r| r.ok && !r.relay).count();
    let sent: u64 = relayed.iter().map(|r| r.sent).sum();
    let delivered: u64 = relayed.iter().map(|r| r.delivered).sum();
    let push_secs = seconds.max(1) as f64;
    let mbps = delivered as f64 * 1000.0 * 8.0 / 1e6 / push_secs;
    let reg_rate = reg_ok as f64 / devices as f64 * 100.0;
    let dial_rate = if sessions == 0 {
        100.0
    } else {
        dial_ok as f64 / sessions as f64 * 100.0
    };
    let delivery = if sent == 0 {
        0.0
    } else {
        delivered as f64 / sent as f64 * 100.0
    };

    let mut table = String::new();
    table.push_str("## 服务端并发压力测试\n\n");
    table.push_str(&format!(
        "3 个节点（1 付费、2 免费），{devices} 台被控端登记，{sessions} 个控制端同时连接（一半只走中继、一半自动选路，\
         中继会话里一半带会员令牌），每个会话推送 {seconds} 秒。服务端和模拟客户端在同一进程，环回网络。\n\n"
    ));
    table.push_str("| 指标 | 结果 |\n|---|---|\n");
    table.push_str(&format!(
        "| 登记成功 | {reg_ok} / {devices}（{reg_rate:.1}%），失败 {reg_failed} |\n"
    ));
    table.push_str(&format!(
        "| 登记延迟 p50 / p95 / max | {:.1} / {:.1} / {:.1} ms |\n",
        percentile(&reg_ms, 0.5),
        percentile(&reg_ms, 0.95),
        percentile(&reg_ms, 1.0)
    ));
    table.push_str(&format!(
        "| 登记总耗时（全部同时发起） | {:.2} s |\n",
        reg_wall.as_secs_f64()
    ));
    table.push_str(&format!(
        "| 连接成功 | {dial_ok} / {sessions}（{dial_rate:.1}%），其中中继 {}、直连 {direct} |\n",
        relayed.len()
    ));
    table.push_str(&format!(
        "| 连接延迟 p50 / p95 / max | {:.1} / {:.1} / {:.1} ms |\n",
        percentile(&dial_ms, 0.5),
        percentile(&dial_ms, 0.95),
        percentile(&dial_ms, 1.0)
    ));
    table.push_str(&format!(
        "| 中继数据报 | 发出 {sent}，对端收到 {delivered}（{delivery:.1}%） |\n"
    ));
    table.push_str(&format!(
        "| 中继吞吐（对端收到） | {mbps:.1} Mbps（{} 个中继会话合计） |\n",
        relayed.len()
    ));
    match cpu_used {
        Some(cpu) => table.push_str(&format!(
            "| 进程 CPU 时间（服务端加模拟客户端） | {cpu:.1} s / 墙钟 {:.1} s = {:.2} 核 |\n",
            total_wall.as_secs_f64(),
            cpu / total_wall.as_secs_f64().max(0.001)
        )),
        None => table.push_str("| 进程 CPU 时间 | 读不到 /proc/self/stat |\n"),
    }
    table.push_str(&format!(
        "| 连接阶段墙钟 | {:.1} s |\n",
        dial_wall.as_secs_f64()
    ));
    let errors = errors.lock().unwrap().clone();
    if errors.is_empty() {
        table.push_str("| 错误 | 无 |\n");
    } else {
        let list: Vec<String> = errors.iter().map(|(k, v)| format!("{k} ×{v}")).collect();
        table.push_str(&format!("| 错误 | {} |\n", list.join("；")));
    }

    let mut failures = Vec::new();
    if reg_rate < 99.0 {
        failures.push(format!("登记成功率 {reg_rate:.1}% < 99%"));
    }
    if dial_rate < 98.0 {
        failures.push(format!("连接成功率 {dial_rate:.1}% < 98%"));
    }
    if !relayed.is_empty() && delivery < 80.0 {
        failures.push(format!("中继数据报送达率 {delivery:.1}% < 80%"));
    }
    if sessions >= 2 && relayed.is_empty() {
        failures.push("没有任何会话走中继，测不到中继吞吐".to_owned());
    }
    Summary { table, failures }
}
