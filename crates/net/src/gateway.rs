//! 默认网关（家用路由器）的地址查询，以及到路由器的往返时延（RTT）测量。
//!
//! 用途：
//! - 端口映射（[`crate::portmap`]）要知道把请求发给谁；
//! - 到路由器的 RTT 反映本机到出口的"最后一跳"质量（Wi-Fi 信号差时这里就已经很慢），
//!   诊断卡顿时用它把"本地网络"和"广域网"分开。
//!
//! 全部不需要管理员权限，也不用 `unsafe`：
//! - Linux：读 `/proc/net/route` 和 `/proc/net/ipv6_route`；
//! - macOS：执行 `route -n get default`；
//! - Windows：执行 `route print -4` / `route print -6`（不经过 PowerShell）。
//!
//! 解析都是纯函数，样例文本直接写在测试里。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;

/// 测量 RTT 用的目标端口起点（traceroute 的惯用端口）：路由器上这些端口都是关着的，
/// 会回 ICMP 端口不可达。
const PROBE_PORT_BASE: u16 = 33434;
/// 端口轮换的范围：每次尝试换一个端口，避开路由器对同一流的 ICMP 限速。
const PROBE_PORT_SPAN: u32 = 100;
/// 两次尝试之间的间隔：路由器普遍给 ICMP 回复限速，连发会被丢。
const PROBE_GAP: Duration = Duration::from_millis(60);

/// Linux 路由标志：路由可用。
const RTF_UP: u32 = 0x1;
/// Linux 路由标志：下一跳是网关。
const RTF_GATEWAY: u32 = 0x2;

// ---------------------------------------------------------------------------
// 解析（纯函数）
// ---------------------------------------------------------------------------

/// 解析 `/proc/net/route`，返回默认路由的网关，按 metric 从小到大（优先的在前）。
///
/// 列：`Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT`，
/// 地址是 8 位十六进制，内容是网络字节序的 32 位数按本机字节序打印的结果，所以要按本机字节序还原。
#[must_use]
pub fn parse_linux_route(text: &str) -> Vec<Ipv4Addr> {
    let mut found: Vec<(u32, Ipv4Addr)> = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (Some(dest), Some(gw), Some(flags), Some(metric), Some(mask)) = (
            cols.get(1).and_then(|s| u32::from_str_radix(s, 16).ok()),
            cols.get(2).and_then(|s| u32::from_str_radix(s, 16).ok()),
            cols.get(3).and_then(|s| u32::from_str_radix(s, 16).ok()),
            cols.get(6).and_then(|s| s.parse::<u32>().ok()),
            cols.get(7).and_then(|s| u32::from_str_radix(s, 16).ok()),
        ) else {
            continue;
        };
        if dest != 0 || mask != 0 || flags & RTF_UP == 0 || flags & RTF_GATEWAY == 0 || gw == 0 {
            continue;
        }
        found.push((metric, Ipv4Addr::from(gw.to_ne_bytes())));
    }
    sorted_unique(found)
}

/// 解析 `/proc/net/ipv6_route`，返回默认路由的下一跳，按 metric 从小到大。
///
/// 列：`目的(32 位十六进制) 前缀长 源 源前缀长 下一跳 metric 引用 使用 标志 接口`，
/// 全部是十六进制。默认路由是目的全 0、前缀长 0、带网关标志、下一跳非 0。
#[must_use]
pub fn parse_linux_ipv6_route(text: &str) -> Vec<Ipv6Addr> {
    let mut found: Vec<(u32, Ipv6Addr)> = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let (Some(dest), Some(plen), Some(next), Some(metric), Some(flags)) = (
            cols.first().and_then(|s| parse_hex_ipv6(s)),
            cols.get(1).and_then(|s| u32::from_str_radix(s, 16).ok()),
            cols.get(4).and_then(|s| parse_hex_ipv6(s)),
            cols.get(5).and_then(|s| u32::from_str_radix(s, 16).ok()),
            cols.get(8).and_then(|s| u32::from_str_radix(s, 16).ok()),
        ) else {
            continue;
        };
        if !dest.is_unspecified()
            || plen != 0
            || flags & RTF_UP == 0
            || flags & RTF_GATEWAY == 0
            || next.is_unspecified()
        {
            continue;
        }
        found.push((metric, next));
    }
    sorted_unique(found)
}

/// 32 位十六进制 → IPv6 地址（`/proc/net/ipv6_route` 里按字节顺序直接写，没有字节序问题）。
fn parse_hex_ipv6(s: &str) -> Option<Ipv6Addr> {
    if s.len() != 32 || !s.is_ascii() {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        let pair = s.get(i * 2..i * 2 + 2)?;
        *byte = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(Ipv6Addr::from(bytes))
}

/// 解析 macOS `route -n get default`（或 `-inet6`）的输出里的 `gateway:` 行。
///
/// IPv6 网关带接口后缀（`fe80::1%en0`），后缀去掉；`link#4` 这样的链路标识不是地址，返回 `None`。
#[must_use]
pub fn parse_macos_route(text: &str) -> Option<IpAddr> {
    text.lines().find_map(|line| {
        let value = line.trim().strip_prefix("gateway:")?.trim();
        let addr = value.split('%').next().unwrap_or(value);
        addr.parse::<IpAddr>().ok()
    })
}

/// 解析 Windows `route print -4` 的输出，返回默认路由的网关，按 metric 从小到大。
///
/// 不依赖表头（表头随系统语言变化），只认数据行：`0.0.0.0 0.0.0.0 <网关> <接口地址> <metric>`。
/// "永久路由"表只有四列，不会被误认；网关是 `On-link` 的行不是网关，跳过。
#[must_use]
pub fn parse_windows_route4(text: &str) -> Vec<Ipv4Addr> {
    let mut found: Vec<(u32, Ipv4Addr)> = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() != 5 || cols.first() != Some(&"0.0.0.0") || cols.get(1) != Some(&"0.0.0.0") {
            continue;
        }
        let (Some(gw), Some(metric)) = (
            cols.get(2).and_then(|s| s.parse::<Ipv4Addr>().ok()),
            cols.get(4).and_then(|s| s.parse::<u32>().ok()),
        ) else {
            continue;
        };
        found.push((metric, gw));
    }
    sorted_unique(found)
}

/// 解析 Windows `route print -6` 的输出，返回默认路由（`::/0`）的网关，按 metric 从小到大。
///
/// 数据行：`<接口号> <metric> ::/0 <网关>`；网关是 `On-link` 的跳过。
#[must_use]
pub fn parse_windows_route6(text: &str) -> Vec<Ipv6Addr> {
    let mut found: Vec<(u32, Ipv6Addr)> = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() != 4 || cols.get(2) != Some(&"::/0") {
            continue;
        }
        let (Some(metric), Some(gw)) = (
            cols.get(1).and_then(|s| s.parse::<u32>().ok()),
            cols.get(3)
                .and_then(|s| s.split('%').next())
                .and_then(|s| s.parse::<Ipv6Addr>().ok()),
        ) else {
            continue;
        };
        found.push((metric, gw));
    }
    sorted_unique(found)
}

/// 按 metric 稳定排序并去重（同一个网关出现在多条路由里只留 metric 最小的那条）。
fn sorted_unique<T: PartialEq + Copy>(mut found: Vec<(u32, T)>) -> Vec<T> {
    found.sort_by_key(|(metric, _)| *metric);
    let mut out: Vec<T> = Vec::new();
    for (_, gw) in found {
        if !out.contains(&gw) {
            out.push(gw);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 取系统路由表
// ---------------------------------------------------------------------------

/// 本机的默认网关：IPv4 在前、IPv6 在后，各自按 metric 排序。
///
/// 查不到（没联网、没有默认路由、系统命令失败）时返回空列表；调用方据此说明
/// "找不到路由器，无法做端口映射"，而不是猜一个地址。
#[must_use]
pub fn default_gateways() -> Vec<IpAddr> {
    platform_gateways()
}

#[cfg(target_os = "linux")]
fn platform_gateways() -> Vec<IpAddr> {
    let v4 = std::fs::read_to_string("/proc/net/route").unwrap_or_default();
    let v6 = std::fs::read_to_string("/proc/net/ipv6_route").unwrap_or_default();
    parse_linux_route(&v4)
        .into_iter()
        .map(IpAddr::V4)
        .chain(parse_linux_ipv6_route(&v6).into_iter().map(IpAddr::V6))
        .collect()
}

#[cfg(target_os = "macos")]
fn platform_gateways() -> Vec<IpAddr> {
    ["-n get default", "-n get -inet6 default"]
        .iter()
        .filter_map(|args| {
            let args: Vec<&str> = args.split(' ').collect();
            run_command("route", &args).and_then(|out| parse_macos_route(&out))
        })
        .collect()
}

#[cfg(target_os = "windows")]
fn platform_gateways() -> Vec<IpAddr> {
    let v4 = run_command("route", &["print", "-4"]).unwrap_or_default();
    let v6 = run_command("route", &["print", "-6"]).unwrap_or_default();
    parse_windows_route4(&v4)
        .into_iter()
        .map(IpAddr::V4)
        .chain(parse_windows_route6(&v6).into_iter().map(IpAddr::V6))
        .collect()
}

/// 执行系统命令并取标准输出；命令不存在、失败或输出不是文本都当作没有结果。
#[cfg(any(target_os = "macos", target_os = "windows"))]
fn run_command(program: &str, args: &[&str]) -> Option<String> {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    #[cfg(target_os = "windows")]
    {
        // 不弹控制台窗口（图形界面进程里启动控制台程序会闪一下黑框）。
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

// ---------------------------------------------------------------------------
// 到路由器的 RTT
// ---------------------------------------------------------------------------

/// 无特权测量到路由器的往返时延。
///
/// 做法：给路由器的一个关着的 UDP 端口发一个小包，路由器会回 ICMP"端口不可达"，
/// 内核把它变成本 socket 上的 `ConnectionRefused`（Windows 上是 `ConnectionReset`），
/// 从发送到收到这个错误的时间就是到路由器的 RTT。不需要原始套接字，也不需要管理员权限。
///
/// 尝试 `tries` 次取最小值（最小值最接近真实的链路时延，排队和 Wi-Fi 重传只会让它变大）。
/// 每次换一个目标端口，并且间隔一小会儿，因为路由器会给 ICMP 回复限速。
/// 路由器不回 ICMP（或本机防火墙拦了），每次都超时，返回 `None`——这时只能说"测不出来"，
/// 不能说"时延很高"。
pub async fn gateway_rtt(gw: IpAddr, tries: u32, timeout: Duration) -> Option<Duration> {
    let mut best: Option<Duration> = None;
    for i in 0..tries {
        if i > 0 {
            tokio::time::sleep(PROBE_GAP).await;
        }
        let port = probe_port(i);
        for target in probe_targets(gw, port) {
            if let Some(rtt) = probe_once(target, timeout).await {
                best = Some(best.map_or(rtt, |b| b.min(rtt)));
                break;
            }
        }
    }
    best
}

/// 第 `i` 次尝试用的目标端口。
fn probe_port(i: u32) -> u16 {
    let offset = u16::try_from(i % PROBE_PORT_SPAN).unwrap_or(0);
    PROBE_PORT_BASE.saturating_add(offset)
}

/// 给定网关和端口，要尝试的目标地址。
///
/// IPv6 链路本地地址（`fe80::/10`）必须带接口编号才能发包，而路由表里的网关不带，
/// 所以按本机每个有链路本地地址的接口各试一个。
fn probe_targets(gw: IpAddr, port: u16) -> Vec<SocketAddr> {
    match gw {
        IpAddr::V4(_) => vec![SocketAddr::new(gw, port)],
        IpAddr::V6(v6) => {
            if is_link_local_v6(v6) {
                let scopes = link_local_scopes(
                    if_addrs::get_if_addrs()
                        .unwrap_or_default()
                        .iter()
                        .map(|i| (i.index, i.ip())),
                );
                scopes
                    .into_iter()
                    .map(|scope| SocketAddr::V6(SocketAddrV6::new(v6, port, 0, scope)))
                    .collect()
            } else {
                vec![SocketAddr::new(gw, port)]
            }
        }
    }
}

/// 是否 IPv6 链路本地地址 `fe80::/10`。
fn is_link_local_v6(ip: Ipv6Addr) -> bool {
    ip.segments().first().is_some_and(|s| s & 0xffc0 == 0xfe80)
}

/// 从（接口编号, 地址）列表里挑出有链路本地 IPv6 地址的接口编号，去重、保持顺序。
fn link_local_scopes(addrs: impl Iterator<Item = (Option<u32>, IpAddr)>) -> Vec<u32> {
    let mut scopes: Vec<u32> = Vec::new();
    for (index, ip) in addrs {
        if let (Some(index), IpAddr::V6(v6)) = (index, ip) {
            if is_link_local_v6(v6) && !scopes.contains(&index) {
                scopes.push(index);
            }
        }
    }
    scopes
}

/// 向 `target` 发一个小包，计时到收到"端口不可达"为止。超时、发不出去、没有收到错误都返回 `None`。
async fn probe_once(target: SocketAddr, timeout: Duration) -> Option<Duration> {
    let bind: SocketAddr = if target.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let socket = UdpSocket::bind(bind).await.ok()?;
    socket.connect(target).await.ok()?;
    let start = Instant::now();
    if let Err(e) = socket.send(&[0u8]).await {
        // 回环等场景下，ICMP 错误可能在发送时就报出来。
        return is_unreachable(&e).then(|| start.elapsed());
    }
    let mut buf = [0u8; 16];
    match tokio::time::timeout(timeout, socket.recv(&mut buf)).await {
        Ok(Err(e)) if is_unreachable(&e) => Some(start.elapsed()),
        // 对端居然回了数据：也是一次有效的往返。
        Ok(Ok(_)) => Some(start.elapsed()),
        // 其他错误、超时：这次没测到。
        Ok(Err(_)) | Err(_) => None,
    }
}

/// 这个 IO 错误是不是"对端回了 ICMP 不可达"：Linux/macOS 是 `ConnectionRefused`，
/// Windows 是 `ConnectionReset`（WSAECONNRESET）。
pub(crate) fn is_unreachable(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一台典型 Linux 的 /proc/net/route：eth0 上有默认路由（metric 100）和 wlan0 上的备用默认路由（metric 600）。
    const LINUX_ROUTE: &str = "\
Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT
eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0
wlan0\t00000000\t0100A8C0\t0003\t0\t0\t600\t00000000\t0\t0\t0
eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0
docker0\t000011AC\t00000000\t0001\t0\t0\t0\t0000FFFF\t0\t0\t0
";

    #[test]
    fn linux_v4_picks_default_routes_by_metric() {
        let gws = parse_linux_route(LINUX_ROUTE);
        assert_eq!(
            gws,
            vec![Ipv4Addr::new(192, 168, 1, 1), Ipv4Addr::new(192, 168, 0, 1)]
        );
    }

    #[test]
    fn linux_v4_sorts_and_dedups() {
        // metric 大的写在前面；同一个网关出现两次只留一份。
        let text = "\
Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT
wlan0 00000000 0100A8C0 0003 0 0 600 00000000 0 0 0
eth0 00000000 0101A8C0 0003 0 0 100 00000000 0 0 0
eth1 00000000 0101A8C0 0003 0 0 300 00000000 0 0 0
";
        assert_eq!(
            parse_linux_route(text),
            vec![Ipv4Addr::new(192, 168, 1, 1), Ipv4Addr::new(192, 168, 0, 1)]
        );
    }

    #[test]
    fn linux_v4_ignores_non_default_down_and_gatewayless_lines() {
        let text = "\
Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT
eth0 00000000 0101A8C0 0001 0 0 100 00000000 0 0 0
eth0 00000000 0101A8C0 0002 0 0 100 00000000 0 0 0
eth0 00000000 00000000 0003 0 0 100 00000000 0 0 0
eth0 0001A8C0 0101A8C0 0003 0 0 100 00FFFFFF 0 0 0
eth0 00000000 0101A8C0 0003 0 0 100 0000FFFF 0 0 0
eth0 00000000 ZZZZZZZZ 0003 0 0 100 00000000 0 0 0
short line
";
        assert!(parse_linux_route(text).is_empty());
        assert!(parse_linux_route("").is_empty());
    }

    #[test]
    fn linux_v6_parses_default_route_next_hop() {
        // fe80::1 经 eth0 的默认路由（标志 0x0003 = UP|GATEWAY），以及无关的路由行。
        let text = "\
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000003 eth0
fe800000000000000000000000000000 40 00000000000000000000000000000000 00 00000000000000000000000000000000 00000100 00000001 00000000 00000001 eth0
00000000000000000000000000000001 80 00000000000000000000000000000000 00 00000000000000000000000000000000 00000000 00000002 00000000 80200001 lo
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000002 00000258 00000001 00000000 00000003 wlan0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo
";
        let gws = parse_linux_ipv6_route(text);
        assert_eq!(
            gws,
            vec![
                "fe80::1".parse::<Ipv6Addr>().unwrap(),
                "fe80::2".parse::<Ipv6Addr>().unwrap()
            ]
        );
    }

    #[test]
    fn linux_v6_rejects_malformed_lines() {
        let text = "\
nothexnothexnothexnothexnothexno 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000003 eth0
0000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000003 eth0
00000000000000000000000000000000 40 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000003 eth0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000001 eth0
00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe800000000000000000000000000001 00000064 00000001 00000000 00000002 eth0
too short
";
        assert!(parse_linux_ipv6_route(text).is_empty());
        // 非 ASCII 不能让切片 panic。
        let weird = "é".repeat(16);
        assert_eq!(parse_hex_ipv6(&weird), None);
    }

    #[test]
    fn macos_gateway_line() {
        let v4 = "\
   route to: default
destination: default
       mask: default
    gateway: 192.168.1.1
  interface: en0
      flags: <UP,GATEWAY,DONE,STATIC,PRCLONING,GLOBAL>
 recvpipe  sendpipe  ssthresh  rtt,msec    rttvar  hopcount      mtu     expire
       0         0         0         0         0         0      1500         0
";
        assert_eq!(
            parse_macos_route(v4),
            Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)))
        );
        let v6 = "\
   route to: default
destination: default
       mask: default
    gateway: fe80::1%en0
  interface: en0
";
        assert_eq!(parse_macos_route(v6), Some("fe80::1".parse().unwrap()));
        // 直连接口的默认路由：网关是链路标识，不是地址。
        assert_eq!(parse_macos_route("    gateway: link#4\n"), None);
        // 没有默认路由时 route 会报错，stdout 里没有 gateway 行。
        assert_eq!(
            parse_macos_route("route: writing to routing socket: not in table\n"),
            None
        );
    }

    /// Windows 10 英文版 `route print -4` 的输出（节选：接口列表 + 活动路由 + 永久路由）。
    const WIN_ROUTE4: &str = "\
===========================================================================
Interface List
  8...00 15 5d 01 02 03 ......Hyper-V Virtual Ethernet Adapter
 12...a4 bb 6d 11 22 33 ......Intel(R) Wi-Fi 6 AX201 160MHz
  1...........................Software Loopback Interface 1
===========================================================================

IPv4 Route Table
===========================================================================
Active Routes:
Network Destination        Netmask          Gateway       Interface  Metric
          0.0.0.0          0.0.0.0      192.168.1.1    192.168.1.100     35
          0.0.0.0          0.0.0.0     172.20.208.1     172.20.214.5   5256
        127.0.0.0        255.0.0.0         On-link         127.0.0.1    331
        127.0.0.1  255.255.255.255         On-link         127.0.0.1    331
      192.168.1.0    255.255.255.0         On-link     192.168.1.100    291
    192.168.1.100  255.255.255.255         On-link     192.168.1.100    291
      224.0.0.0        240.0.0.0         On-link         127.0.0.1    331
===========================================================================
Persistent Routes:
  Network Address          Netmask  Gateway Address  Metric
          0.0.0.0          0.0.0.0     192.168.9.9  Default
";

    #[test]
    fn windows_v4_active_default_routes() {
        let gws = parse_windows_route4(WIN_ROUTE4);
        // 永久路由表只有四列，不能混进来；两条活动默认路由按 metric 排序。
        assert_eq!(
            gws,
            vec![
                Ipv4Addr::new(192, 168, 1, 1),
                Ipv4Addr::new(172, 20, 208, 1)
            ]
        );
    }

    #[test]
    fn windows_v4_skips_on_link_and_garbage() {
        let text = "\
          0.0.0.0          0.0.0.0         On-link     192.168.1.100     35
          0.0.0.0          0.0.0.0      192.168.1.1    192.168.1.100    abc
          0.0.0.0        255.0.0.0      192.168.1.1    192.168.1.100     35
";
        assert!(parse_windows_route4(text).is_empty());
    }

    /// Windows 10 中文版同一命令的输出：表头是中文，数据行格式不变，所以解析不依赖表头。
    #[test]
    fn windows_v4_localized_header() {
        let text = "\
IPv4 路由表
===========================================================================
活动路由:
网络目标        网络掩码          网关       接口   跃点数
          0.0.0.0          0.0.0.0      10.0.0.1      10.0.0.23     25
";
        assert_eq!(parse_windows_route4(text), vec![Ipv4Addr::new(10, 0, 0, 1)]);
    }

    /// Windows 10 英文版 `route print -6` 的输出（节选）。
    const WIN_ROUTE6: &str = "\
IPv6 Route Table
===========================================================================
Active Routes:
 If Metric Network Destination      Gateway
 12    291 ::/0                     fe80::1
  1    331 ::1/128                  On-link
 12    291 2408:8207:1234:5600::/64 On-link
 12    291 2408:8207:1234:5600:a1b2:c3d4:e5f6:1234/128
                                    On-link
 12    291 fe80::/64                On-link
  8    271 ::/0                     fe80::2%8
 14    256 ::/0                     On-link
===========================================================================
Persistent Routes:
  None
";

    #[test]
    fn windows_v6_default_routes() {
        let gws = parse_windows_route6(WIN_ROUTE6);
        assert_eq!(
            gws,
            vec![
                "fe80::2".parse::<Ipv6Addr>().unwrap(),
                "fe80::1".parse::<Ipv6Addr>().unwrap()
            ]
        );
        assert!(parse_windows_route6(" 12 abc ::/0 fe80::1\n").is_empty());
    }

    #[test]
    fn default_gateways_does_not_panic() {
        // 沙箱里可能没有默认路由，只要求不 panic、且返回的都是具体地址。
        for gw in default_gateways() {
            assert!(!gw.is_unspecified());
        }
    }

    #[test]
    fn probe_ports_rotate_and_wrap() {
        assert_eq!(probe_port(0), 33434);
        assert_eq!(probe_port(1), 33435);
        assert_eq!(probe_port(99), 33533);
        assert_eq!(probe_port(100), 33434);
    }

    #[test]
    fn link_local_detection_and_scopes() {
        assert!(is_link_local_v6("fe80::1".parse().unwrap()));
        assert!(is_link_local_v6("febf::1".parse().unwrap()));
        assert!(!is_link_local_v6("fec0::1".parse().unwrap()));
        assert!(!is_link_local_v6("2001:db8::1".parse().unwrap()));
        let addrs = vec![
            (Some(2), "fe80::aaaa".parse::<IpAddr>().unwrap()),
            (Some(2), "fe80::bbbb".parse().unwrap()),
            (Some(3), "2001:db8::1".parse().unwrap()),
            (None, "fe80::cccc".parse().unwrap()),
            (Some(4), "10.0.0.2".parse().unwrap()),
            (Some(5), "fe80::dddd".parse().unwrap()),
        ];
        assert_eq!(link_local_scopes(addrs.into_iter()), vec![2, 5]);
    }

    #[test]
    fn probe_targets_by_family() {
        let v4 = probe_targets("192.168.1.1".parse().unwrap(), 33434);
        assert_eq!(v4, vec!["192.168.1.1:33434".parse::<SocketAddr>().unwrap()]);
        let v6 = probe_targets("2001:db8::1".parse().unwrap(), 33434);
        assert_eq!(
            v6,
            vec!["[2001:db8::1]:33434".parse::<SocketAddr>().unwrap()]
        );
        // 链路本地：每个目标都带接口编号（沙箱里可能没有，只检查带的都是链路本地地址 + 正确端口）。
        for t in probe_targets("fe80::1".parse().unwrap(), 33440) {
            let SocketAddr::V6(t) = t else {
                panic!("应为 IPv6 目标");
            };
            assert_eq!(t.port(), 33440);
            assert_ne!(t.scope_id(), 0);
        }
    }

    #[tokio::test]
    async fn rtt_on_loopback_closed_port() {
        // 回环上关着的端口：Linux 上立刻 ConnectionRefused，这就是"路由器的回复"。
        let rtt = gateway_rtt(IpAddr::V4(Ipv4Addr::LOCALHOST), 3, Duration::from_secs(1)).await;
        let rtt = rtt.expect("回环必须有回复");
        assert!(rtt < Duration::from_secs(1), "回环 RTT 不该这么大：{rtt:?}");
    }

    #[tokio::test]
    async fn rtt_on_ipv6_loopback() {
        // 本机有 IPv6 回环就必须测得到；没有（某些沙箱）时 bind 失败，返回 None，也不能 panic。
        let rtt = gateway_rtt(IpAddr::V6(Ipv6Addr::LOCALHOST), 1, Duration::from_secs(1)).await;
        let has_v6 = UdpSocket::bind("[::1]:0").await.is_ok();
        assert_eq!(rtt.is_some(), has_v6);
    }

    #[tokio::test]
    async fn rtt_zero_tries_is_none() {
        assert_eq!(
            gateway_rtt(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                0,
                Duration::from_millis(50)
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn rtt_times_out_when_nobody_replies() {
        // 端口是开着的并且不回话：既没有 ICMP 也没有数据，只能超时。
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = silent.local_addr().unwrap();
        assert_eq!(probe_once(target, Duration::from_millis(80)).await, None);
    }

    #[tokio::test]
    async fn rtt_counts_a_data_reply() {
        // 对端回了数据（不是 ICMP）：也算一次往返。
        let echo = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = echo.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 16];
            if let Ok((_, from)) = echo.recv_from(&mut buf).await {
                let _ = echo.send_to(b"x", from).await;
            }
        });
        assert!(probe_once(target, Duration::from_secs(1)).await.is_some());
    }

    #[tokio::test]
    async fn rtt_send_failure_is_none() {
        // 本机没有 IPv6 路由时 connect/send 直接失败（有路由时超时）：都不是"端口不可达"，都是 None。
        let target: SocketAddr = "[2001:db8::1]:33434".parse().unwrap();
        assert_eq!(probe_once(target, Duration::from_millis(50)).await, None);
    }

    #[test]
    fn unreachable_error_kinds() {
        use std::io::{Error, ErrorKind};
        assert!(is_unreachable(&Error::from(ErrorKind::ConnectionRefused)));
        assert!(is_unreachable(&Error::from(ErrorKind::ConnectionReset)));
        assert!(!is_unreachable(&Error::from(ErrorKind::TimedOut)));
    }
}
