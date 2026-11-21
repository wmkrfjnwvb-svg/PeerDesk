//! 让家用路由器把一个 UDP 端口转发给本机（端口映射），使对称型 NAT / 被挡住的网络变成可直连。
//!
//! 三种协议并行尝试，谁先成功用谁：
//! - **PCP**（RFC 6887）：新路由器；
//! - **NAT-PMP**（RFC 6886）：苹果系和不少开源固件；
//! - **UPnP IGD**：最普及，用 `igd-next` 库。
//!
//! 线路格式（PCP / NAT-PMP）自己实现成纯编解码函数，字节布局直接对照 RFC 写在测试里；
//! 网络部分只是"发包、重传、等回应"的薄壳，测试时用 127.0.0.1 上的假路由器应答。
//!
//! 诚实原则：失败时给出每种方法各自的原因（中文），不静默降级；
//! 路由器自己的外网地址是私网地址时（双层 NAT / 运营商 NAT），映射在公网上没有意义，明确报错。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use igd_next::{AddAnyPortError, PortMappingProtocol, RequestError, SearchOptions};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::net::UdpSocket;
use tokio::sync::watch;
use tokio::task::JoinSet;

use crate::gateway::is_unreachable;

/// NAT-PMP / PCP 在路由器上的端口（两者共用）。
pub const PMP_PORT: u16 = 5351;
/// 向路由器申请的租期（秒）。路由器可以给得更短，以返回值为准。
const REQUEST_LIFETIME_SECS: u32 = 7200;
/// NAT-PMP / PCP 第一次重传前的等待（RFC 6886 建议 250 ms，之后每次翻倍）。
const FIRST_WAIT: Duration = Duration::from_millis(250);
/// 续租失败后的重试间隔。
const RETRY_DELAY: Duration = Duration::from_secs(30);
/// 续租前至少等这么久（防止路由器给了很短的租期时空转）。
const MIN_RENEW_DELAY: Duration = Duration::from_secs(1);
/// UPnP 映射的描述文字（路由器管理页里能看到）。
const UPNP_DESCRIPTION: &str = "PeerDesk";
/// 协议号：UDP。
const PROTO_UDP: u8 = 17;

// ---------------------------------------------------------------------------
// 公共类型
// ---------------------------------------------------------------------------

/// 一个已建立的端口映射。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mapping {
    /// 外网地址和端口：对端往这里发包，路由器转给本机。
    pub external: SocketAddr,
    /// 用哪种协议建立的：`"UPnP"`、`"NAT-PMP"` 或 `"PCP"`。
    pub protocol: &'static str,
    /// 路由器实际给的租期，到期前必须续租（见 [`keep_mapped`]）。
    pub lifetime: Duration,
}

/// 路由器的外网地址能不能用于公网可达。
///
/// 外网地址是私网地址（RFC 1918 的 10/8、172.16/12、192.168/16，运营商级 NAT 的 100.64/10，
/// 链路本地 169.254/16，IPv6 的唯一本地 fc00::/7）说明路由器自己还在另一层 NAT 后面
/// （双层 NAT / 运营商 NAT），映射只在内层有效，公网上的对端打不进来；
/// 全 0 地址说明路由器的外网口还没有拿到地址。
///
/// # Errors
/// 外网地址不可用时，返回给用户看的原因。
pub fn check_public(ip: IpAddr) -> Result<(), String> {
    if ip.is_unspecified() {
        return Err("路由器还没有外网地址（外网口没有连上），端口映射无效".to_string());
    }
    let private = match ip {
        IpAddr::V4(v4) => is_private_v4(v4),
        IpAddr::V6(v6) => v6.segments().first().is_some_and(|s| s & 0xfe00 == 0xfc00),
    };
    if private {
        return Err(format!(
            "路由器的外网地址是私网地址（双层 NAT / 运营商 NAT），端口映射无效（外网地址 {ip}）"
        ));
    }
    Ok(())
}

/// IPv4 私网：RFC 1918、运营商级 NAT（100.64.0.0/10）、链路本地。
fn is_private_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private() || (a == 100 && b & 0xc0 == 64) || ip.is_link_local()
}

// ---------------------------------------------------------------------------
// 线路格式：公共部分
// ---------------------------------------------------------------------------

/// 解码失败的原因。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// 报文太短。
    Truncated,
    /// 协议版本不对（NAT-PMP 应为 0，PCP 应为 2）。
    Version(u8),
    /// 操作码不是期望的应答。
    Opcode(u8),
    /// PCP 应答里的 nonce 和请求不一致（不是对我们这次请求的应答）。
    Nonce,
    /// PCP 应答里的协议号不是 UDP。
    Protocol(u8),
    /// 路由器返回了非零结果码。
    Result(u16),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "报文太短"),
            Self::Version(v) => write!(f, "协议版本 {v} 不符"),
            Self::Opcode(op) => write!(f, "操作码 {op} 不符"),
            Self::Nonce => write!(f, "nonce 不符"),
            Self::Protocol(p) => write!(f, "协议号 {p} 不是 UDP"),
            Self::Result(code) => write!(f, "结果码 {code}"),
        }
    }
}

/// 按顺序读字节，越界时返回 `None`（不用下标，不会 panic）。
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Option<[u8; N]> {
        let (head, rest) = self.0.split_first_chunk::<N>()?;
        self.0 = rest;
        Some(*head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|[b]| b)
    }

    fn u16(&mut self) -> Option<u16> {
        self.take::<2>().map(u16::from_be_bytes)
    }

    fn u32(&mut self) -> Option<u32> {
        self.take::<4>().map(u32::from_be_bytes)
    }
}

// ---------------------------------------------------------------------------
// NAT-PMP（RFC 6886）
// ---------------------------------------------------------------------------

/// NAT-PMP 操作码：查询外网地址。
const NATPMP_OP_EXTERNAL: u8 = 0;
/// NAT-PMP 操作码：UDP 映射。
const NATPMP_OP_MAP_UDP: u8 = 1;
/// 应答的操作码 = 请求的操作码 + 128。
const NATPMP_REPLY_BIT: u8 = 128;

/// NAT-PMP 外网地址应答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatPmpExternal {
    /// 路由器的"启动后秒数"（epoch）：变小说明路由器重启过，映射已丢失。
    pub epoch: u32,
    /// 路由器的外网地址。
    pub addr: Ipv4Addr,
}

/// NAT-PMP UDP 映射应答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatPmpMap {
    /// 路由器的"启动后秒数"。
    pub epoch: u32,
    /// 内部端口（和请求一致）。
    pub internal_port: u16,
    /// 路由器分配的外部端口。
    pub external_port: u16,
    /// 路由器给的租期（秒）。
    pub lifetime_secs: u32,
}

/// 编码"查询外网地址"请求：版本 0、操作码 0，共 2 字节。
#[must_use]
pub fn natpmp_encode_external_request() -> [u8; 2] {
    [0, NATPMP_OP_EXTERNAL]
}

/// 编码 UDP 映射请求（12 字节）：
/// 版本 0、操作码 1、保留 2 字节、内部端口、建议的外部端口、租期（秒），多字节整数均为大端。
/// 租期为 0 且建议外部端口为 0 表示删除该内部端口的映射。
#[must_use]
pub fn natpmp_encode_map_request(
    internal_port: u16,
    suggested_external: u16,
    lifetime_secs: u32,
) -> [u8; 12] {
    let [p0, p1] = internal_port.to_be_bytes();
    let [e0, e1] = suggested_external.to_be_bytes();
    let [l0, l1, l2, l3] = lifetime_secs.to_be_bytes();
    // 第 2、3 字节是保留位，保持 0。
    [0, NATPMP_OP_MAP_UDP, 0, 0, p0, p1, e0, e1, l0, l1, l2, l3]
}

/// 读 NAT-PMP 应答头（版本、操作码、结果码），返回头之后的读取器。
fn natpmp_header(buf: &[u8], expect_op: u8) -> Result<Reader<'_>, DecodeError> {
    let mut r = Reader(buf);
    let version = r.u8().ok_or(DecodeError::Truncated)?;
    if version != 0 {
        return Err(DecodeError::Version(version));
    }
    let op = r.u8().ok_or(DecodeError::Truncated)?;
    if op != expect_op.wrapping_add(NATPMP_REPLY_BIT) {
        return Err(DecodeError::Opcode(op));
    }
    let code = r.u16().ok_or(DecodeError::Truncated)?;
    if code != 0 {
        return Err(DecodeError::Result(code));
    }
    Ok(r)
}

/// 解码"查询外网地址"应答（12 字节）。
///
/// # Errors
/// 报文太短、版本或操作码不符、路由器返回了错误码。
pub fn natpmp_decode_external(buf: &[u8]) -> Result<NatPmpExternal, DecodeError> {
    let mut r = natpmp_header(buf, NATPMP_OP_EXTERNAL)?;
    let epoch = r.u32().ok_or(DecodeError::Truncated)?;
    let addr = r.take::<4>().ok_or(DecodeError::Truncated)?;
    Ok(NatPmpExternal {
        epoch,
        addr: Ipv4Addr::from(addr),
    })
}

/// 解码 UDP 映射应答（16 字节）。
///
/// # Errors
/// 报文太短、版本或操作码不符、路由器返回了错误码。
pub fn natpmp_decode_map(buf: &[u8]) -> Result<NatPmpMap, DecodeError> {
    let mut r = natpmp_header(buf, NATPMP_OP_MAP_UDP)?;
    let epoch = r.u32().ok_or(DecodeError::Truncated)?;
    let internal_port = r.u16().ok_or(DecodeError::Truncated)?;
    let external_port = r.u16().ok_or(DecodeError::Truncated)?;
    let lifetime_secs = r.u32().ok_or(DecodeError::Truncated)?;
    Ok(NatPmpMap {
        epoch,
        internal_port,
        external_port,
        lifetime_secs,
    })
}

/// NAT-PMP 结果码的中文说明（RFC 6886 第 3.5 节）。
#[must_use]
pub fn natpmp_result_text(code: u16) -> &'static str {
    match code {
        1 => "路由器不支持这个版本的 NAT-PMP",
        2 => "路由器拒绝了请求（端口映射功能没开，或不允许本机使用）",
        3 => "路由器网络故障（外网口没有连上）",
        4 => "路由器资源不足（映射表满了）",
        5 => "路由器不支持这个操作",
        _ => "路由器返回了未知的错误码",
    }
}

// ---------------------------------------------------------------------------
// PCP（RFC 6887）
// ---------------------------------------------------------------------------

/// PCP 版本号。
const PCP_VERSION: u8 = 2;
/// PCP 操作码：MAP。
const PCP_OP_MAP: u8 = 1;
/// 应答的 R 位：操作码字节最高位为 1。
const PCP_REPLY_BIT: u8 = 0x80;
/// PCP 请求 / 应答的 MAP 报文长度：公共头 24 字节 + MAP 载荷 36 字节。
pub const PCP_MAP_LEN: usize = 60;

/// PCP MAP 应答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PcpMap {
    /// 路由器给的租期（秒）。
    pub lifetime_secs: u32,
    /// 路由器的"启动后秒数"。
    pub epoch: u32,
    /// 内部端口（和请求一致）。
    pub internal_port: u16,
    /// 路由器分配的外部端口。
    pub external_port: u16,
    /// 路由器分配的外部地址。
    pub external_ip: IpAddr,
}

/// IP 地址转 16 字节：IPv4 用 IPv4 映射的 IPv6 形式（`::ffff:a.b.c.d`）。
fn ip_to_16(ip: IpAddr) -> [u8; 16] {
    match ip {
        IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
        IpAddr::V6(v6) => v6.octets(),
    }
}

/// 编码 PCP MAP 请求（60 字节）。
///
/// 公共头（24 字节）：版本 2、R=0 + 操作码 1、保留 2 字节、请求租期（秒）、客户端 IP（16 字节）；
/// MAP 载荷（36 字节）：nonce（12 字节）、协议 17（UDP）、保留 3 字节、内部端口、建议外部端口、
/// 建议外部地址（16 字节，全 0 表示不指定）。多字节整数均为大端。
/// 租期为 0 表示删除（nonce、协议、内部端口必须和创建时一致）。
#[must_use]
pub fn pcp_encode_map_request(
    client_ip: IpAddr,
    nonce: [u8; 12],
    internal_port: u16,
    suggested_external: u16,
    lifetime_secs: u32,
) -> [u8; PCP_MAP_LEN] {
    let mut out: Vec<u8> = Vec::with_capacity(PCP_MAP_LEN);
    // 公共头：版本、R=0|操作码、保留 2 字节、租期、客户端 IP。
    out.extend_from_slice(&[PCP_VERSION, PCP_OP_MAP, 0, 0]);
    out.extend_from_slice(&lifetime_secs.to_be_bytes());
    out.extend_from_slice(&ip_to_16(client_ip));
    // MAP 载荷：nonce、协议、保留 3 字节、内部端口、建议外部端口、建议外部地址（全 0）。
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&[PROTO_UDP, 0, 0, 0]);
    out.extend_from_slice(&internal_port.to_be_bytes());
    out.extend_from_slice(&suggested_external.to_be_bytes());
    out.extend_from_slice(&[0u8; 16]);
    // 长度恒为 60；转换失败不可能发生，退回全 0 只是为了不 panic。
    <[u8; PCP_MAP_LEN]>::try_from(out).unwrap_or([0u8; PCP_MAP_LEN])
}

/// 解码 PCP MAP 应答（60 字节）。
///
/// 应答公共头（24 字节）：版本、R=1 + 操作码、保留 1 字节、结果码 1 字节、租期、epoch、保留 12 字节；
/// 后面是和请求同结构的 MAP 载荷，内部端口之后是路由器分配的外部端口和外部地址。
/// `expect_nonce` 用来确认这是对我们这次请求的应答。
///
/// # Errors
/// 报文太短、版本或操作码不符、结果码非零、nonce 或协议不符。
pub fn pcp_decode_map_response(buf: &[u8], expect_nonce: &[u8; 12]) -> Result<PcpMap, DecodeError> {
    let mut r = Reader(buf);
    let version = r.u8().ok_or(DecodeError::Truncated)?;
    // 只懂 NAT-PMP 的路由器收到 PCP 请求会回版本 0，这里据此报"不支持 PCP"。
    if version != PCP_VERSION {
        return Err(DecodeError::Version(version));
    }
    let op = r.u8().ok_or(DecodeError::Truncated)?;
    if op != PCP_REPLY_BIT | PCP_OP_MAP {
        return Err(DecodeError::Opcode(op));
    }
    let _reserved = r.u8().ok_or(DecodeError::Truncated)?;
    let code = r.u8().ok_or(DecodeError::Truncated)?;
    if code != 0 {
        return Err(DecodeError::Result(u16::from(code)));
    }
    let lifetime_secs = r.u32().ok_or(DecodeError::Truncated)?;
    let epoch = r.u32().ok_or(DecodeError::Truncated)?;
    let _reserved = r.take::<12>().ok_or(DecodeError::Truncated)?;
    let nonce = r.take::<12>().ok_or(DecodeError::Truncated)?;
    if &nonce != expect_nonce {
        return Err(DecodeError::Nonce);
    }
    let protocol = r.u8().ok_or(DecodeError::Truncated)?;
    if protocol != PROTO_UDP {
        return Err(DecodeError::Protocol(protocol));
    }
    let _reserved = r.take::<3>().ok_or(DecodeError::Truncated)?;
    let internal_port = r.u16().ok_or(DecodeError::Truncated)?;
    let external_port = r.u16().ok_or(DecodeError::Truncated)?;
    let ip = r.take::<16>().ok_or(DecodeError::Truncated)?;
    Ok(PcpMap {
        lifetime_secs,
        epoch,
        internal_port,
        external_port,
        external_ip: Ipv6Addr::from(ip).to_canonical(),
    })
}

/// PCP 结果码的中文说明（RFC 6887 第 7.4 节）。
#[must_use]
pub fn pcp_result_text(code: u16) -> &'static str {
    match code {
        1 => "路由器不支持这个版本的 PCP",
        2 => "路由器拒绝了请求（端口映射功能没开，或不允许本机使用）",
        3 => "请求格式有误",
        4 => "路由器不支持这个操作",
        5 | 6 => "路由器不支持请求里的选项",
        7 => "路由器网络故障（外网口没有连上）",
        8 => "路由器资源不足（映射表满了）",
        9 => "路由器不支持 UDP 映射",
        10 => "超过了本机在路由器上的映射数量上限",
        11 => "路由器没法提供外部地址或端口",
        12 => "路由器看到的本机地址和请求里的不符（中间还有一层 NAT）",
        13 => "远端对端过多",
        _ => "路由器返回了未知的错误码",
    }
}

/// 一次 PCP 映射用的 nonce：同一进程里同一个内部端口永远是同一个值（续租和删除要用同一个），
/// 不同进程之间不同（进程级随机盐 + 端口，取 SHA-256 的前 12 字节）。
#[must_use]
pub fn pcp_nonce(internal_port: u16) -> [u8; 12] {
    static SALT: OnceLock<[u8; 16]> = OnceLock::new();
    let salt = SALT.get_or_init(|| {
        let mut salt = [0u8; 16];
        // 随机数取不到时盐保持全 0：nonce 只是防误删，不是密钥。
        let _ = SystemRandom::new().fill(&mut salt);
        salt
    });
    let mut ctx = digest::Context::new(&digest::SHA256);
    ctx.update(salt);
    ctx.update(&internal_port.to_be_bytes());
    let hash = ctx.finish();
    hash.as_ref()
        .first_chunk::<12>()
        .copied()
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// 收发：发请求、重传、等应答
// ---------------------------------------------------------------------------

/// 创建一个连到 `target` 的 UDP socket（`connect` 不发包，只是让内核过滤来源并报告 ICMP 错误）。
async fn connect_to(target: SocketAddr) -> Result<UdpSocket, String> {
    let bind: SocketAddr = if target.is_ipv4() {
        (Ipv4Addr::UNSPECIFIED, 0).into()
    } else {
        (Ipv6Addr::UNSPECIFIED, 0).into()
    };
    let sock = UdpSocket::bind(bind)
        .await
        .map_err(ctx("无法创建 UDP 套接字"))?;
    sock.connect(target)
        .await
        .map_err(ctx(format!("无法连接到 {target}")))?;
    Ok(sock)
}

/// 发请求并等应答：按 `first_wait`、翻倍……的间隔重传，总共不超过 `within`。
///
/// `decode` 判断收到的报文：`None` 表示不是我们要的（旧应答、别的请求的应答），继续等；
/// `Some(结果)` 表示有结论。路由器回 ICMP 端口不可达说明它没开这个端口，立刻放弃，不必等到超时。
async fn exchange<T>(
    sock: &UdpSocket,
    request: &[u8],
    within: Duration,
    first_wait: Duration,
    decode: impl Fn(&[u8]) -> Option<Result<T, String>>,
) -> Result<T, String> {
    let deadline = Instant::now() + within;
    let mut wait = first_wait;
    let mut buf = [0u8; 1100];
    loop {
        if let Err(e) = sock.send(request).await {
            return Err(send_error(&e));
        }
        let attempt_end = (Instant::now() + wait).min(deadline);
        loop {
            // 时间已到时 timeout(0) 仍会先轮询一次 recv，随后返回超时，所以不用单独判断。
            let left = attempt_end.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, sock.recv(&mut buf)).await {
                Err(_) => break,
                Ok(Err(e)) => return Err(send_error(&e)),
                Ok(Ok(n)) => {
                    if let Some(verdict) = decode(buf.get(..n).unwrap_or_default()) {
                        return verdict;
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{:.1} 秒内没有收到回应（路由器不支持，或端口映射功能没开）",
                within.as_secs_f32()
            ));
        }
        wait = wait.saturating_mul(2);
    }
}

/// 给错误加上"做什么时出的错"的前缀。
fn ctx<E: std::fmt::Display>(what: impl Into<String>) -> impl FnOnce(E) -> String {
    let what = what.into();
    move |e| format!("{what}：{e}")
}

/// 把收发错误翻译成人话。
fn send_error(e: &std::io::Error) -> String {
    if is_unreachable(e) {
        "路由器回了\"端口不可达\"：它没有开放端口映射服务（不支持这个协议）".to_string()
    } else {
        format!("网络错误：{e}")
    }
}

/// 把解码结果变成 `exchange` 要的判定：
/// 不是对本次请求的应答（操作码、nonce 对不上，或截断）就忽略；版本不符和路由器的错误码是明确的结论。
fn verdict<T>(
    result: Result<T, DecodeError>,
    name: &str,
    result_text: fn(u16) -> &'static str,
) -> Option<Result<T, String>> {
    match result {
        Ok(v) => Some(Ok(v)),
        Err(DecodeError::Result(code)) => Some(Err(format!(
            "{name}：{}（结果码 {code}）",
            result_text(code)
        ))),
        Err(DecodeError::Version(v)) => Some(Err(format!(
            "{name}：路由器回复的协议版本是 {v}，不是 {name}"
        ))),
        Err(
            DecodeError::Truncated
            | DecodeError::Opcode(_)
            | DecodeError::Nonce
            | DecodeError::Protocol(_),
        ) => None,
    }
}

/// 剩余时间（不会小于 0）。
fn remaining(start: Instant, within: Duration) -> Duration {
    within.saturating_sub(start.elapsed())
}

/// 校验路由器给的映射结果，组装成 [`Mapping`]。
fn finish(
    ip: IpAddr,
    port: u16,
    lifetime_secs: u32,
    protocol: &'static str,
) -> Result<Mapping, String> {
    check_public(ip)?;
    if port == 0 || lifetime_secs == 0 {
        return Err(format!(
            "{protocol}：路由器回复了无效的映射（端口 {port}，租期 {lifetime_secs} 秒）"
        ));
    }
    Ok(Mapping {
        external: SocketAddr::new(ip, port),
        protocol,
        lifetime: Duration::from_secs(u64::from(lifetime_secs)),
    })
}

/// NAT-PMP：先问外网地址，再申请 UDP 映射。`gw` 是路由器的 NAT-PMP 地址（正常是网关:5351）。
async fn natpmp_map(
    gw: SocketAddr,
    internal_port: u16,
    suggested: u16,
    within: Duration,
    first_wait: Duration,
) -> Result<Mapping, String> {
    let start = Instant::now();
    let sock = connect_to(gw).await?;
    let external = exchange(
        &sock,
        &natpmp_encode_external_request(),
        within,
        first_wait,
        |b| verdict(natpmp_decode_external(b), "NAT-PMP", natpmp_result_text),
    )
    .await?;
    // 先查外网地址：它是私网地址就不用再申请映射了。
    check_public(IpAddr::V4(external.addr))?;
    let request = natpmp_encode_map_request(internal_port, suggested, REQUEST_LIFETIME_SECS);
    let map = exchange(&sock, &request, remaining(start, within), first_wait, |b| {
        match natpmp_decode_map(b) {
            // 别的内部端口的应答（旧请求的迟到应答）：忽略。
            Ok(m) if m.internal_port != internal_port => None,
            other => verdict(other, "NAT-PMP", natpmp_result_text),
        }
    })
    .await?;
    finish(
        IpAddr::V4(external.addr),
        map.external_port,
        map.lifetime_secs,
        "NAT-PMP",
    )
}

/// PCP：申请 UDP 映射。外部地址由路由器在应答里给出。
async fn pcp_map(
    gw: SocketAddr,
    internal_port: u16,
    suggested: u16,
    within: Duration,
    first_wait: Duration,
) -> Result<Mapping, String> {
    let sock = connect_to(gw).await?;
    let client_ip = sock.local_addr().map_err(ctx("无法确定本机地址"))?.ip();
    let nonce = pcp_nonce(internal_port);
    let request = pcp_encode_map_request(
        client_ip,
        nonce,
        internal_port,
        suggested,
        REQUEST_LIFETIME_SECS,
    );
    let map = exchange(
        &sock,
        &request,
        within,
        first_wait,
        |b| match pcp_decode_map_response(b, &nonce) {
            Ok(m) if m.internal_port != internal_port => None,
            other => verdict(other, "PCP", pcp_result_text),
        },
    )
    .await?;
    finish(map.external_ip, map.external_port, map.lifetime_secs, "PCP")
}

/// NAT-PMP 删除映射（租期 0、建议外部端口 0）。
async fn natpmp_unmap(
    gw: SocketAddr,
    internal_port: u16,
    within: Duration,
    first_wait: Duration,
) -> Result<(), String> {
    let sock = connect_to(gw).await?;
    let request = natpmp_encode_map_request(internal_port, 0, 0);
    exchange(&sock, &request, within, first_wait, |b| {
        match natpmp_decode_map(b) {
            Ok(m) if m.internal_port != internal_port => None,
            other => verdict(other, "NAT-PMP", natpmp_result_text),
        }
        .map(|r| r.map(|_| ()))
    })
    .await
}

/// PCP 删除映射（租期 0，nonce 和创建时一致）。
async fn pcp_unmap(
    gw: SocketAddr,
    internal_port: u16,
    within: Duration,
    first_wait: Duration,
) -> Result<(), String> {
    let sock = connect_to(gw).await?;
    let client_ip = sock.local_addr().map_err(ctx("无法确定本机地址"))?.ip();
    let nonce = pcp_nonce(internal_port);
    let request = pcp_encode_map_request(client_ip, nonce, internal_port, 0, 0);
    exchange(&sock, &request, within, first_wait, |b| {
        verdict(pcp_decode_map_response(b, &nonce), "PCP", pcp_result_text).map(|r| r.map(|_| ()))
    })
    .await
}

// ---------------------------------------------------------------------------
// UPnP IGD
// ---------------------------------------------------------------------------

/// UPnP：发现网关、查外网地址、添加映射。`local` 必须是本机的局域网地址（路由器把包转到它）。
async fn upnp_map(
    local: SocketAddr,
    options: SearchOptions,
    suggested: u16,
    within: Duration,
) -> Result<Mapping, String> {
    let mut options = options;
    options.timeout = Some(within);
    let gateway = igd_next::aio::tokio::search_gateway(options)
        .await
        .map_err(ctx("没有发现 UPnP 路由器"))?;
    let ip = gateway
        .get_external_ip()
        .await
        .map_err(ctx("查询外网地址失败"))?;
    // 外网地址是私网地址就不必添加映射了。
    check_public(ip)?;

    // 指定了外部端口（续租）时先试同一个端口，这样对端拿到的地址不变。
    if suggested != 0
        && gateway
            .add_port(
                PortMappingProtocol::UDP,
                suggested,
                local,
                REQUEST_LIFETIME_SECS,
                UPNP_DESCRIPTION,
            )
            .await
            .is_ok()
    {
        return finish(ip, suggested, REQUEST_LIFETIME_SECS, "UPnP");
    }
    let mut lease = REQUEST_LIFETIME_SECS;
    let port = loop {
        match gateway
            .add_any_port(PortMappingProtocol::UDP, local, lease, UPNP_DESCRIPTION)
            .await
        {
            Ok(port) => break port,
            // 有些路由器只接受永久租约（租期 0）：退一步照做。我们仍按原租期续租，续租只是重复添加，无害。
            Err(
                AddAnyPortError::OnlyPermanentLeasesSupported
                | AddAnyPortError::RequestError(RequestError::ErrorCode(725, _)),
            ) if lease != 0 => {
                lease = 0;
            }
            Err(e) => return Err(format!("添加端口映射失败：{e}")),
        }
    };
    finish(ip, port, REQUEST_LIFETIME_SECS, "UPnP")
}

/// UPnP 删除映射。
async fn upnp_unmap(
    options: SearchOptions,
    external_port: u16,
    within: Duration,
) -> Result<(), String> {
    let mut options = options;
    options.timeout = Some(within);
    let gateway = igd_next::aio::tokio::search_gateway(options)
        .await
        .map_err(ctx("没有发现 UPnP 路由器"))?;
    gateway
        .remove_port(PortMappingProtocol::UDP, external_port)
        .await
        .map_err(ctx("删除端口映射失败"))
}

// ---------------------------------------------------------------------------
// 组合：三种协议并行
// ---------------------------------------------------------------------------

/// 各协议的目标，正常使用时是路由器的标准端口；测试里换成本机上的假路由器。
#[derive(Clone)]
struct Endpoints {
    natpmp_port: u16,
    pcp_port: u16,
    upnp: Arc<dyn Fn() -> SearchOptions + Send + Sync>,
    first_wait: Duration,
    retry_delay: Duration,
    /// 没有网关地址时，用来推断本机局域网地址的探测目标（TEST-NET，不会真的发包）。
    lan_probe: IpAddr,
}

impl std::fmt::Debug for Endpoints {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoints")
            .field("natpmp_port", &self.natpmp_port)
            .field("pcp_port", &self.pcp_port)
            .finish_non_exhaustive()
    }
}

impl Endpoints {
    fn real() -> Self {
        Self {
            natpmp_port: PMP_PORT,
            pcp_port: PMP_PORT,
            upnp: Arc::new(SearchOptions::default),
            first_wait: FIRST_WAIT,
            retry_delay: RETRY_DELAY,
            lan_probe: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        }
    }
}

/// 请路由器把 UDP 端口转发给 `local`，PCP、NAT-PMP、UPnP 并行尝试，返回最先成功的。
///
/// - `local`：要映射的本机地址和端口（端口必须非 0；地址可以是 `0.0.0.0`，这时用到路由器的出口地址）；
/// - `gateways`：路由器地址，来自 [`crate::gateway::default_gateways`]；
/// - `within`：每种方法的最长等待时间。
///
/// # Errors
/// 全部失败时，错误信息按方法逐条写明原因（中文）。路由器的外网地址是私网地址
///（双层 NAT / 运营商 NAT）也算失败，因为这样的映射对公网上的对端没有用。
pub async fn map_udp(
    local: SocketAddr,
    gateways: &[IpAddr],
    within: Duration,
) -> Result<Mapping, String> {
    map_udp_with(local, gateways, within, 0, &Endpoints::real()).await
}

/// [`map_udp`] 的实现。`suggested` 是希望得到的外部端口（续租时用上次的，0 表示随意）。
async fn map_udp_with(
    local: SocketAddr,
    gateways: &[IpAddr],
    within: Duration,
    suggested: u16,
    ep: &Endpoints,
) -> Result<Mapping, String> {
    if local.port() == 0 {
        return Err("端口映射失败：本机端口不能是 0".to_string());
    }
    let mut tasks: JoinSet<(String, Result<Mapping, String>)> = JoinSet::new();
    for &gw in gateways {
        let pcp_target = SocketAddr::new(gw, ep.pcp_port);
        let (port, first_wait) = (local.port(), ep.first_wait);
        tasks.spawn(async move {
            (
                format!("PCP({gw})"),
                pcp_map(pcp_target, port, suggested, within, first_wait).await,
            )
        });
        // NAT-PMP 只有 IPv4。
        if gw.is_ipv4() {
            let target = SocketAddr::new(gw, ep.natpmp_port);
            tasks.spawn(async move {
                (
                    format!("NAT-PMP({gw})"),
                    natpmp_map(target, port, suggested, within, first_wait).await,
                )
            });
        }
    }
    // UPnP 不需要知道网关地址（组播发现），但要知道本机的局域网地址。
    let upnp_local = lan_addr(local, gateways.first().copied(), ep.lan_probe).await;
    let options = (ep.upnp)();
    tasks.spawn(async move {
        let timeout_msg = format!("{:.1} 秒内没有完成", within.as_secs_f32());
        let result = match upnp_local {
            Ok(addr) => tokio::time::timeout(within, upnp_map(addr, options, suggested, within))
                .await
                .unwrap_or(Err(timeout_msg)),
            Err(e) => Err(e),
        };
        ("UPnP".to_string(), result)
    });

    let mut failures: Vec<String> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        // 任务 panic 只可能是内部错误，和别的失败一起列出。
        let outcome = joined
            .map_err(ctx("内部错误"))
            .and_then(|(label, result)| result.map_err(|e| format!("{label}：{e}")));
        match outcome {
            Ok(mapping) => return Ok(mapping),
            Err(e) => failures.push(e),
        }
    }
    failures.sort();
    let hint = if gateways.is_empty() {
        "（没有找到默认网关）"
    } else {
        ""
    };
    Err(format!("端口映射失败{hint}：{}", failures.join("；")))
}

/// 本机的局域网地址（UPnP 要告诉路由器把包转给谁）。
/// `local` 已经指定了地址就直接用；否则看"发往路由器时内核会选哪个源地址"
///（UDP `connect` 只查路由表，不发包）。
async fn lan_addr(
    local: SocketAddr,
    gateway: Option<IpAddr>,
    fallback: IpAddr,
) -> Result<SocketAddr, String> {
    if !local.ip().is_unspecified() {
        return Ok(local);
    }
    let probe_target = gateway.unwrap_or(fallback);
    let sock = connect_to(SocketAddr::new(probe_target, 9))
        .await
        .map_err(ctx("无法确定本机的局域网地址"))?;
    let ip = sock
        .local_addr()
        .map_err(ctx("无法确定本机的局域网地址"))?
        .ip();
    Ok(SocketAddr::new(ip, local.port()))
}

/// 尽力删除映射（退出前调用，让路由器的映射表保持干净）。失败不影响正确性：租期到了路由器自己会清。
///
/// # Errors
/// 删除失败的原因（中文）。调用方通常只记日志。
pub async fn unmap(
    local: SocketAddr,
    gateways: &[IpAddr],
    mapping: &Mapping,
    within: Duration,
) -> Result<(), String> {
    unmap_with(local, gateways, mapping, within, &Endpoints::real()).await
}

async fn unmap_with(
    local: SocketAddr,
    gateways: &[IpAddr],
    mapping: &Mapping,
    within: Duration,
    ep: &Endpoints,
) -> Result<(), String> {
    if mapping.protocol == "UPnP" {
        return upnp_unmap((ep.upnp)(), mapping.external.port(), within).await;
    }
    let mut failures: Vec<String> = Vec::new();
    for &gw in gateways {
        let result = match mapping.protocol {
            "PCP" => {
                pcp_unmap(
                    SocketAddr::new(gw, ep.pcp_port),
                    local.port(),
                    within,
                    ep.first_wait,
                )
                .await
            }
            "NAT-PMP" if gw.is_ipv4() => {
                natpmp_unmap(
                    SocketAddr::new(gw, ep.natpmp_port),
                    local.port(),
                    within,
                    ep.first_wait,
                )
                .await
            }
            "NAT-PMP" => continue,
            other => return Err(format!("未知的端口映射协议：{other}")),
        };
        match result {
            Ok(()) => return Ok(()),
            Err(e) => failures.push(format!("{gw}：{e}")),
        }
    }
    if failures.is_empty() {
        return Err("没有可用的网关地址，无法删除端口映射".to_string());
    }
    Err(format!("删除端口映射失败：{}", failures.join("；")))
}

// ---------------------------------------------------------------------------
// 续租
// ---------------------------------------------------------------------------

/// 到该续租的时间：租期的一半（RFC 6886 / 6887 的建议），至少 1 秒。
#[must_use]
pub fn renew_delay(lifetime: Duration) -> Duration {
    (lifetime / 2).max(MIN_RENEW_DELAY)
}

/// 在后台保持映射不过期：每到租期一半就重新申请（优先要回同一个外部端口），
/// 把最新的映射通过 `tx` 发布出去；续租失败发布 `None`（对端的候选地址要撤掉），30 秒后重试。
/// 所有接收端都丢弃后自动结束。调用方 `tokio::spawn` 它即可；要停止就丢掉 `watch::Receiver`
///（并视需要调用 [`unmap`]）。
pub async fn keep_mapped(
    local: SocketAddr,
    gateways: Vec<IpAddr>,
    first: Mapping,
    within: Duration,
    tx: watch::Sender<Option<Mapping>>,
) {
    keep_mapped_with(local, gateways, first, within, tx, &Endpoints::real()).await;
}

async fn keep_mapped_with(
    local: SocketAddr,
    gateways: Vec<IpAddr>,
    first: Mapping,
    within: Duration,
    tx: watch::Sender<Option<Mapping>>,
    ep: &Endpoints,
) {
    let mut port = first.external.port();
    let mut wait = renew_delay(first.lifetime);
    loop {
        tokio::select! {
            () = tx.closed() => return,
            () = tokio::time::sleep(wait) => {}
        }
        let published = match map_udp_with(local, &gateways, within, port, ep).await {
            Ok(mapping) => {
                crate::note!(
                    info,
                    "端口映射已续租：{} 经 {}",
                    mapping.external,
                    mapping.protocol
                );
                port = mapping.external.port();
                wait = renew_delay(mapping.lifetime);
                tx.send(Some(mapping))
            }
            Err(e) => {
                crate::note!(
                    warn,
                    "端口映射续租失败，{} 秒后重试：{e}",
                    ep.retry_delay.as_secs()
                );
                wait = ep.retry_delay;
                tx.send(None)
            }
        };
        if published.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    // ------------------------- 编解码 -------------------------

    #[test]
    fn natpmp_external_request_layout() {
        assert_eq!(natpmp_encode_external_request(), [0, 0]);
    }

    #[test]
    fn natpmp_map_request_layout() {
        // RFC 6886 3.3：版本 0，操作码 1，保留 0，内部端口 5000(0x1388)，建议外部端口 6000(0x1770)，租期 7200(0x1c20)。
        assert_eq!(
            natpmp_encode_map_request(5000, 6000, 7200),
            [0, 1, 0, 0, 0x13, 0x88, 0x17, 0x70, 0, 0, 0x1c, 0x20]
        );
        // 删除：租期 0、建议端口 0。
        assert_eq!(
            natpmp_encode_map_request(5000, 0, 0),
            [0, 1, 0, 0, 0x13, 0x88, 0, 0, 0, 0, 0, 0]
        );
    }

    fn natpmp_external_reply(code: u16, epoch: u32, ip: [u8; 4]) -> Vec<u8> {
        let mut v = vec![0, 128];
        v.extend_from_slice(&code.to_be_bytes());
        v.extend_from_slice(&epoch.to_be_bytes());
        v.extend_from_slice(&ip);
        v
    }

    fn natpmp_map_reply(
        code: u16,
        epoch: u32,
        internal: u16,
        external: u16,
        lifetime: u32,
    ) -> Vec<u8> {
        let mut v = vec![0, 129];
        v.extend_from_slice(&code.to_be_bytes());
        v.extend_from_slice(&epoch.to_be_bytes());
        v.extend_from_slice(&internal.to_be_bytes());
        v.extend_from_slice(&external.to_be_bytes());
        v.extend_from_slice(&lifetime.to_be_bytes());
        v
    }

    #[test]
    fn natpmp_external_decode() {
        let reply = natpmp_external_reply(0, 123_456, [203, 0, 113, 7]);
        assert_eq!(reply.len(), 12);
        assert_eq!(
            natpmp_decode_external(&reply),
            Ok(NatPmpExternal {
                epoch: 123_456,
                addr: Ipv4Addr::new(203, 0, 113, 7)
            })
        );
    }

    #[test]
    fn natpmp_map_decode() {
        let reply = natpmp_map_reply(0, 99, 5000, 6001, 3600);
        assert_eq!(reply.len(), 16);
        assert_eq!(
            natpmp_decode_map(&reply),
            Ok(NatPmpMap {
                epoch: 99,
                internal_port: 5000,
                external_port: 6001,
                lifetime_secs: 3600
            })
        );
    }

    #[test]
    fn natpmp_decode_errors() {
        // 结果码：每个都带出来；错误应答可能只有 8 字节，不能因为"太短"盖住结果码。
        for code in 1..=5u16 {
            let mut short = natpmp_map_reply(code, 1, 5000, 0, 0);
            short.truncate(8);
            assert_eq!(natpmp_decode_map(&short), Err(DecodeError::Result(code)));
        }
        assert_eq!(natpmp_decode_map(&[]), Err(DecodeError::Truncated));
        assert_eq!(natpmp_decode_map(&[0]), Err(DecodeError::Truncated));
        assert_eq!(natpmp_decode_map(&[0, 129, 0]), Err(DecodeError::Truncated));
        // 头对但载荷缺。
        assert_eq!(
            natpmp_decode_map(&[0, 129, 0, 0, 0, 0, 0, 1]),
            Err(DecodeError::Truncated)
        );
        let mut v = natpmp_map_reply(0, 1, 5000, 6000, 10);
        v.truncate(15);
        assert_eq!(natpmp_decode_map(&v), Err(DecodeError::Truncated));
        let mut v = natpmp_external_reply(0, 1, [1, 2, 3, 4]);
        v.truncate(11);
        assert_eq!(natpmp_decode_external(&v), Err(DecodeError::Truncated));
        // 版本不是 0。
        assert_eq!(
            natpmp_decode_map(&[2, 129, 0, 0]),
            Err(DecodeError::Version(2))
        );
        // 操作码不符：拿外网地址的应答当映射应答。
        assert_eq!(
            natpmp_decode_map(&natpmp_external_reply(0, 1, [1, 2, 3, 4])),
            Err(DecodeError::Opcode(128))
        );
        assert_eq!(
            natpmp_decode_external(&natpmp_map_reply(0, 1, 1, 1, 1)),
            Err(DecodeError::Opcode(129))
        );
    }

    #[test]
    fn natpmp_result_texts_are_distinct() {
        let texts: Vec<_> = (1..=6).map(natpmp_result_text).collect();
        for (i, a) in texts.iter().enumerate() {
            for b in texts.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn pcp_map_request_layout() {
        let nonce = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let req = pcp_encode_map_request(
            IpAddr::V4(Ipv4Addr::new(192, 168, 1, 100)),
            nonce,
            5000,
            6000,
            7200,
        );
        assert_eq!(req.len(), 60);
        // 公共头。
        assert_eq!(&req[0..4], &[2, 1, 0, 0]); // 版本 2，R=0|操作码 1，保留
        assert_eq!(&req[4..8], &7200u32.to_be_bytes());
        assert_eq!(&req[8..18], &[0u8; 10]); // IPv4 映射地址前 80 位为 0
        assert_eq!(&req[18..20], &[0xff, 0xff]);
        assert_eq!(&req[20..24], &[192, 168, 1, 100]);
        // MAP 载荷。
        assert_eq!(&req[24..36], &nonce);
        assert_eq!(req[36], 17); // UDP
        assert_eq!(&req[37..40], &[0, 0, 0]);
        assert_eq!(&req[40..42], &5000u16.to_be_bytes());
        assert_eq!(&req[42..44], &6000u16.to_be_bytes());
        assert_eq!(&req[44..60], &[0u8; 16]);
    }

    #[test]
    fn pcp_map_request_with_ipv6_client() {
        let ip: Ipv6Addr = "2001:db8::1234".parse().unwrap();
        let req = pcp_encode_map_request(IpAddr::V6(ip), [0; 12], 1, 0, 0);
        assert_eq!(&req[8..24], &ip.octets());
        assert_eq!(&req[4..8], &[0, 0, 0, 0]); // 删除：租期 0
    }

    /// 按 RFC 6887 的布局造一个 MAP 应答。
    fn pcp_reply(
        code: u8,
        nonce: [u8; 12],
        proto: u8,
        internal: u16,
        external: u16,
        ip: IpAddr,
    ) -> Vec<u8> {
        let mut v = vec![2, 0x81, 0, code];
        v.extend_from_slice(&3600u32.to_be_bytes()); // 租期
        v.extend_from_slice(&777u32.to_be_bytes()); // epoch
        v.extend_from_slice(&[0u8; 12]);
        v.extend_from_slice(&nonce);
        v.push(proto);
        v.extend_from_slice(&[0, 0, 0]);
        v.extend_from_slice(&internal.to_be_bytes());
        v.extend_from_slice(&external.to_be_bytes());
        v.extend_from_slice(&ip_to_16(ip));
        v
    }

    #[test]
    fn pcp_decode_ok_v4_and_v6() {
        let nonce = [9u8; 12];
        let ip4 = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));
        let reply = pcp_reply(0, nonce, 17, 5000, 6000, ip4);
        assert_eq!(reply.len(), PCP_MAP_LEN);
        assert_eq!(
            pcp_decode_map_response(&reply, &nonce),
            Ok(PcpMap {
                lifetime_secs: 3600,
                epoch: 777,
                internal_port: 5000,
                external_port: 6000,
                external_ip: ip4
            })
        );
        let ip6: IpAddr = "2001:db8::77".parse().unwrap();
        let reply = pcp_reply(0, nonce, 17, 5000, 5000, ip6);
        assert_eq!(
            pcp_decode_map_response(&reply, &nonce).unwrap().external_ip,
            ip6
        );
    }

    #[test]
    fn pcp_decode_errors() {
        let nonce = [9u8; 12];
        let ip = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5));
        // 每个结果码都原样带出。
        for code in 1..=13u8 {
            let reply = pcp_reply(code, nonce, 17, 5000, 0, ip);
            assert_eq!(
                pcp_decode_map_response(&reply, &nonce),
                Err(DecodeError::Result(u16::from(code)))
            );
        }
        // 版本 0：只懂 NAT-PMP 的路由器的回复（只有 8 字节）。
        assert_eq!(
            pcp_decode_map_response(&[0, 129, 0, 1, 0, 0, 0, 0], &nonce),
            Err(DecodeError::Version(0))
        );
        assert_eq!(
            pcp_decode_map_response(&[], &nonce),
            Err(DecodeError::Truncated)
        );
        // 操作码：没有 R 位（那是请求，不是应答）。
        let mut reply = pcp_reply(0, nonce, 17, 5000, 6000, ip);
        reply[1] = 1;
        assert_eq!(
            pcp_decode_map_response(&reply, &nonce),
            Err(DecodeError::Opcode(1))
        );
        // nonce、协议不符。
        let reply = pcp_reply(0, [1; 12], 17, 5000, 6000, ip);
        assert_eq!(
            pcp_decode_map_response(&reply, &nonce),
            Err(DecodeError::Nonce)
        );
        let reply = pcp_reply(0, nonce, 6, 5000, 6000, ip);
        assert_eq!(
            pcp_decode_map_response(&reply, &nonce),
            Err(DecodeError::Protocol(6))
        );
        // 在每个字段边界截断都报 Truncated，不 panic。
        let full = pcp_reply(0, nonce, 17, 5000, 6000, ip);
        for len in 1..PCP_MAP_LEN {
            let r = pcp_decode_map_response(full.get(..len).unwrap(), &nonce);
            assert_eq!(r, Err(DecodeError::Truncated), "长度 {len}");
        }
    }

    #[test]
    fn pcp_result_texts() {
        for code in 1..=13u16 {
            assert!(!pcp_result_text(code).is_empty());
        }
        assert_ne!(pcp_result_text(2), pcp_result_text(8));
        assert_eq!(pcp_result_text(200), natpmp_result_text(200));
    }

    #[test]
    fn decode_error_display() {
        for e in [
            DecodeError::Truncated,
            DecodeError::Version(1),
            DecodeError::Opcode(2),
            DecodeError::Nonce,
            DecodeError::Protocol(6),
            DecodeError::Result(3),
        ] {
            assert!(!e.to_string().is_empty());
        }
    }

    #[test]
    fn nonce_is_stable_per_port_and_differs_between_ports() {
        assert_eq!(pcp_nonce(5000), pcp_nonce(5000));
        assert_ne!(pcp_nonce(5000), pcp_nonce(5001));
    }

    #[test]
    fn double_nat_detection() {
        let bad = [
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.0.1",
            "100.64.0.1",
            "100.127.255.255",
            "169.254.1.1",
            "fd00::1",
            "fc00::1",
        ];
        for ip in bad {
            let err = check_public(ip.parse().unwrap()).unwrap_err();
            assert!(err.contains("双层 NAT / 运营商 NAT"), "{ip}: {err}");
            assert!(err.contains(ip));
        }
        for ip in [
            "8.8.8.8",
            "203.0.113.7",
            "100.63.255.255",
            "100.128.0.0",
            "172.15.0.1",
            "172.32.0.1",
            "2001:db8::1",
        ] {
            assert!(check_public(ip.parse().unwrap()).is_ok(), "{ip}");
        }
        assert!(check_public("0.0.0.0".parse().unwrap())
            .unwrap_err()
            .contains("还没有外网地址"));
        assert!(check_public("::".parse().unwrap()).is_err());
    }

    #[test]
    fn renew_at_half_lifetime() {
        assert_eq!(
            renew_delay(Duration::from_secs(7200)),
            Duration::from_secs(3600)
        );
        assert_eq!(renew_delay(Duration::from_secs(1)), Duration::from_secs(1));
        assert_eq!(renew_delay(Duration::ZERO), Duration::from_secs(1));
    }

    #[test]
    fn mapping_debug_and_clone() {
        let m = Mapping {
            external: "203.0.113.7:6000".parse().unwrap(),
            protocol: "PCP",
            lifetime: Duration::from_secs(60),
        };
        assert_eq!(m.clone(), m);
        assert!(format!("{m:?}").contains("PCP"));
        assert!(format!("{:?}", Endpoints::real()).contains("5351"));
    }

    // ------------------------- 假路由器 -------------------------

    /// 在 127.0.0.1 上起一个 UDP 假路由器：每收到一个包就调用 `handler(第几个包, 包内容)`，
    /// 把返回的每个报文依次发回去。
    async fn fake_router<F>(handler: F) -> SocketAddr
    where
        F: Fn(usize, &[u8]) -> Vec<Vec<u8>> + Send + 'static,
    {
        let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(serve_udp(sock, handler));
        addr
    }

    /// 假路由器的收发循环（不会结束，任务随测试运行时一起丢弃）。
    #[rustfmt::skip]
    async fn serve_udp<F: Fn(usize, &[u8]) -> Vec<Vec<u8>>>(sock: UdpSocket, handler: F) { let (mut buf, mut n) = ([0u8; 1500], 0usize); while let Ok((len, from)) = sock.recv_from(&mut buf).await { for reply in handler(n, &buf[..len]) { let _ = sock.send_to(&reply, from).await; } n += 1; } }

    /// 一个有 NAT-PMP 的路由器：外网地址 `ip`，映射给 `external_port`，租期 `lifetime`。
    fn natpmp_handler(
        ip: [u8; 4],
        external_port: u16,
        lifetime: u32,
    ) -> impl Fn(usize, &[u8]) -> Vec<Vec<u8>> {
        move |_, req| {
            if req[1] == 0 {
                return vec![natpmp_external_reply(0, 5, ip)];
            }
            let internal = u16::from_be_bytes([req[4], req[5]]);
            let life = u32::from_be_bytes([req[8], req[9], req[10], req[11]]);
            // 删除请求：租期 0，回 0 租期。
            let (port, lifetime) = if life == 0 {
                (0, 0)
            } else {
                (external_port, lifetime)
            };
            vec![natpmp_map_reply(0, 5, internal, port, lifetime)]
        }
    }

    /// 一个有 PCP 的路由器。
    fn pcp_handler(
        ip: IpAddr,
        external_port: u16,
        lifetime: u32,
    ) -> impl Fn(usize, &[u8]) -> Vec<Vec<u8>> {
        move |_, req| {
            let nonce: [u8; 12] = req[24..36].try_into().unwrap();
            let internal = u16::from_be_bytes([req[40], req[41]]);
            let life = u32::from_be_bytes([req[4], req[5], req[6], req[7]]);
            let mut reply = pcp_reply(0, nonce, 17, internal, external_port, ip);
            reply[4..8].copy_from_slice(&(if life == 0 { 0 } else { lifetime }).to_be_bytes());
            vec![reply]
        }
    }

    const SHORT: Duration = Duration::from_secs(3);
    const FAST: Duration = Duration::from_millis(40);

    fn public_ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))
    }

    /// 本机上一个没人听的 UDP 端口（对它发包会收到 ICMP 端口不可达）。
    async fn closed_port() -> u16 {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        s.local_addr().unwrap().port()
    }

    #[tokio::test]
    async fn natpmp_client_happy_path() {
        let gw = fake_router(natpmp_handler([203, 0, 113, 7], 40000, 3600)).await;
        let m = natpmp_map(gw, 5000, 0, SHORT, FAST).await.unwrap();
        assert_eq!(m.external, "203.0.113.7:40000".parse().unwrap());
        assert_eq!(m.protocol, "NAT-PMP");
        assert_eq!(m.lifetime, Duration::from_secs(3600));
    }

    #[tokio::test]
    async fn natpmp_client_retries_after_lost_packet() {
        // 前两个包（第一次外网地址请求和它的重传）被"丢"，第三个才回。
        let inner = natpmp_handler([203, 0, 113, 7], 40001, 60);
        let gw = fake_router(move |n, req| if n < 2 { vec![] } else { inner(n, req) }).await;
        let m = natpmp_map(gw, 5000, 0, SHORT, FAST).await.unwrap();
        assert_eq!(m.external.port(), 40001);
    }

    #[tokio::test]
    async fn natpmp_client_ignores_stale_replies() {
        // 先回一个别的端口的映射应答、一个操作码不对的应答、一个截断的，再回正确的。
        let gw = fake_router(|_, req| {
            if req[1] == 0 {
                return vec![vec![0, 128], natpmp_external_reply(0, 1, [203, 0, 113, 7])];
            }
            vec![
                natpmp_map_reply(0, 1, 9999, 1234, 60),
                natpmp_external_reply(0, 1, [1, 1, 1, 1]),
                natpmp_map_reply(0, 1, 5000, 40002, 60),
            ]
        })
        .await;
        let m = natpmp_map(gw, 5000, 0, SHORT, FAST).await.unwrap();
        assert_eq!(m.external.port(), 40002);
    }

    #[tokio::test]
    async fn natpmp_client_reports_router_refusal() {
        let gw = fake_router(|_, req| match req.get(1) {
            Some(0) => vec![natpmp_external_reply(0, 1, [203, 0, 113, 7])],
            _ => vec![natpmp_map_reply(2, 1, 5000, 0, 0)],
        })
        .await;
        let err = natpmp_map(gw, 5000, 0, SHORT, FAST).await.unwrap_err();
        assert!(err.contains("拒绝") && err.contains("结果码 2"), "{err}");
    }

    #[tokio::test]
    async fn natpmp_client_rejects_wrong_version() {
        let gw = fake_router(|_, _| vec![vec![9, 128, 0, 0, 0, 0, 0, 0]]).await;
        let err = natpmp_map(gw, 5000, 0, SHORT, FAST).await.unwrap_err();
        assert!(err.contains("协议版本是 9"), "{err}");
    }

    #[tokio::test]
    async fn natpmp_client_rejects_double_nat() {
        let gw = fake_router(natpmp_handler([100, 64, 3, 4], 40000, 3600)).await;
        let err = natpmp_map(gw, 5000, 0, SHORT, FAST).await.unwrap_err();
        assert!(err.contains("双层 NAT / 运营商 NAT"), "{err}");
    }

    #[tokio::test]
    async fn natpmp_client_rejects_empty_mapping() {
        let gw = fake_router(natpmp_handler([203, 0, 113, 7], 0, 3600)).await;
        let err = natpmp_map(gw, 5000, 0, SHORT, FAST).await.unwrap_err();
        assert!(err.contains("无效的映射"), "{err}");
    }

    #[tokio::test]
    async fn natpmp_client_times_out_on_silence() {
        let gw = fake_router(|_, _| vec![]).await;
        let started = Instant::now();
        let err = natpmp_map(gw, 5000, 0, Duration::from_millis(300), FAST)
            .await
            .unwrap_err();
        assert!(err.contains("没有收到回应"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn natpmp_client_gives_up_on_icmp_refused() {
        let port = closed_port().await;
        let started = Instant::now();
        let err = natpmp_map(
            SocketAddr::new("127.0.0.1".parse().unwrap(), port),
            5000,
            0,
            SHORT,
            FAST,
        )
        .await
        .unwrap_err();
        assert!(err.contains("端口不可达"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(1), "不该等到超时");
    }

    #[tokio::test]
    async fn pcp_client_happy_path_and_request_contents() {
        let seen = Arc::new(AtomicUsize::new(0));
        let seen2 = seen.clone();
        let inner = pcp_handler(public_ip(), 41000, 1800);
        let gw = fake_router(move |n, req| {
            // 客户端 IP 必须是本机发包用的地址（回环），内部端口 5000。
            assert_eq!(&req[8..18], &[0u8; 10]);
            assert_eq!(&req[20..24], &[127, 0, 0, 1]);
            assert_eq!(u16::from_be_bytes([req[40], req[41]]), 5000);
            assert_eq!(u16::from_be_bytes([req[42], req[43]]), 4242);
            seen2.fetch_add(1, Ordering::SeqCst);
            inner(n, req)
        })
        .await;
        let m = pcp_map(gw, 5000, 4242, SHORT, FAST).await.unwrap();
        assert_eq!(m.external, "203.0.113.7:41000".parse().unwrap());
        assert_eq!(m.protocol, "PCP");
        assert_eq!(m.lifetime, Duration::from_secs(1800));
        assert_eq!(seen.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn pcp_client_ignores_foreign_replies_and_reports_errors() {
        let ip = public_ip();
        // 先来一个别人的 nonce 的应答，再来正确的。
        let gw = fake_router(move |_, req| {
            let nonce: [u8; 12] = req[24..36].try_into().unwrap();
            vec![
                pcp_reply(0, [0; 12], 17, 5000, 1, ip),
                pcp_reply(0, nonce, 17, 5000, 41001, ip),
            ]
        })
        .await;
        assert_eq!(
            pcp_map(gw, 5000, 0, SHORT, FAST)
                .await
                .unwrap()
                .external
                .port(),
            41001
        );

        // 路由器拒绝。
        let gw = fake_router(move |_, req| {
            let nonce: [u8; 12] = req[24..36].try_into().unwrap();
            vec![pcp_reply(2, nonce, 17, 5000, 0, ip)]
        })
        .await;
        let err = pcp_map(gw, 5000, 0, SHORT, FAST).await.unwrap_err();
        assert!(err.contains("PCP") && err.contains("拒绝"), "{err}");

        // 只懂 NAT-PMP 的路由器：回版本 0。
        let gw = fake_router(|_, _| vec![vec![0, 129, 0, 1, 0, 0, 0, 0]]).await;
        let err = pcp_map(gw, 5000, 0, SHORT, FAST).await.unwrap_err();
        assert!(err.contains("协议版本是 0"), "{err}");

        // 双层 NAT。
        let gw = fake_router(pcp_handler(
            IpAddr::V4(Ipv4Addr::new(10, 9, 8, 7)),
            41000,
            60,
        ))
        .await;
        let err = pcp_map(gw, 5000, 0, SHORT, FAST).await.unwrap_err();
        assert!(err.contains("双层 NAT / 运营商 NAT"), "{err}");

        // 静默、端口关闭。
        let gw = fake_router(|_, _| vec![]).await;
        assert!(pcp_map(gw, 5000, 0, Duration::from_millis(200), FAST)
            .await
            .unwrap_err()
            .contains("没有收到回应"));
        let closed = SocketAddr::new("127.0.0.1".parse().unwrap(), closed_port().await);
        assert!(pcp_map(closed, 5000, 0, SHORT, FAST)
            .await
            .unwrap_err()
            .contains("端口不可达"));
    }

    #[tokio::test]
    async fn unmap_clients() {
        // NAT-PMP 删除：路由器收到租期 0。
        let gw = fake_router(natpmp_handler([203, 0, 113, 7], 40000, 3600)).await;
        natpmp_unmap(gw, 5000, SHORT, FAST).await.unwrap();
        // 别的内部端口的应答被忽略，直到超时。
        let gw = fake_router(|_, _| vec![natpmp_map_reply(0, 1, 1, 0, 0)]).await;
        assert!(natpmp_unmap(gw, 5000, Duration::from_millis(150), FAST)
            .await
            .is_err());
        // PCP 删除。
        let gw = fake_router(pcp_handler(public_ip(), 40000, 3600)).await;
        pcp_unmap(gw, 5000, SHORT, FAST).await.unwrap();
        let closed = SocketAddr::new("127.0.0.1".parse().unwrap(), closed_port().await);
        assert!(pcp_unmap(closed, 5000, SHORT, FAST).await.is_err());
    }

    // ------------------------- 假 UPnP 路由器 -------------------------

    /// 假 IGD 的行为开关。
    #[derive(Clone, Copy)]
    struct IgdBehavior {
        external_ip: &'static str,
        /// 租期非 0 时回 725（只支持永久租约）。
        permanent_only: bool,
        /// AddPortMapping 直接失败（续租时同端口不可用）。
        reject_fixed_port: bool,
        /// AddAnyPortMapping 失败（722：没有可用端口）。
        reject_any_port: bool,
    }

    const OK_IGD: IgdBehavior = IgdBehavior {
        external_ip: "203.0.113.9",
        permanent_only: false,
        reject_fixed_port: false,
        reject_any_port: false,
    };

    const DEVICE_XML: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
<deviceList><device><deviceType>urn:schemas-upnp-org:device:WANConnectionDevice:1</deviceType>
<serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
<serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId><SCPDURL>/scpd.xml</SCPDURL><controlURL>/ctl</controlURL></service></serviceList>
</device></deviceList></device></deviceList></device></root>"#;

    fn scpd_xml() -> String {
        let action = |name: &str, args: &[&str]| {
            let args: String = args
                .iter()
                .map(|a| format!("<argument><name>{a}</name><direction>in</direction></argument>"))
                .collect();
            format!("<action><name>{name}</name><argumentList>{args}</argumentList></action>")
        };
        let add = [
            "NewRemoteHost",
            "NewExternalPort",
            "NewProtocol",
            "NewInternalPort",
            "NewInternalClient",
            "NewEnabled",
            "NewPortMappingDescription",
            "NewLeaseDuration",
        ];
        format!(
            "<?xml version=\"1.0\"?><scpd xmlns=\"urn:schemas-upnp-org:service-1-0\"><actionList>{}{}{}{}</actionList></scpd>",
            action("AddAnyPortMapping", &add),
            action("AddPortMapping", &add),
            action("GetExternalIPAddress", &[]),
            action("DeletePortMapping", &["NewRemoteHost", "NewExternalPort", "NewProtocol"]),
        )
    }

    fn soap(body: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>{body}</s:Body></s:Envelope>"
        )
    }

    fn soap_fault(code: u16, text: &str) -> String {
        soap(&format!(
            "<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode><errorDescription>{text}</errorDescription></UPnPError></detail></s:Fault>"
        ))
    }

    /// 假 IGD 的路由表：按请求行（GET）或 SOAP 动作名（POST）查应答；查不到就是 404。
    #[rustfmt::skip]
    fn igd_route(b: IgdBehavior, first: &str, name: &str, text: &str) -> (&'static str, String) {
        const NS: &str = "xmlns:u=\"urn:schemas-upnp-org:service:WANIPConnection:1\"";
        let permanent_refused = b.permanent_only && !text.contains("<NewLeaseDuration>0<");
        let ok = |n: &str, inner: &str| ("200 OK", soap(&format!("<u:{n}Response {NS}>{inner}</u:{n}Response>")));
        let fault = |code, what| ("500 Internal Server Error", soap_fault(code, what));
        let any = if b.reject_any_port { fault(728, "NoPortMapsAvailable") } else if permanent_refused { fault(725, "OnlyPermanentLeasesSupported") } else { ok("AddAnyPortMapping", "<NewReservedPort>45678</NewReservedPort>") };
        let fixed = if b.reject_fixed_port { fault(718, "ConflictInMappingEntry") } else { ok("AddPortMapping", "") };
        let routes = [
            ("GET /scpd.xml", ("200 OK", scpd_xml())),
            ("GET /desc.xml", ("200 OK", DEVICE_XML.to_string())),
            ("GetExternalIPAddress", ok("GetExternalIPAddress", &format!("<NewExternalIPAddress>{}</NewExternalIPAddress>", b.external_ip))),
            ("AddAnyPortMapping", any),
            ("AddPortMapping", fixed),
            ("DeletePortMapping", ok("DeletePortMapping", "")),
        ];
        routes.into_iter().find(|(k, _)| first.starts_with(k) || *k == name).map_or(("404 Not Found", String::new()), |(_, v)| v)
    }

    /// 假 IGD 的接受循环（不会结束，任务随测试运行时一起丢弃）。
    #[rustfmt::skip]
    async fn accept_loop(http: TcpListener, b: IgdBehavior, log: Arc<std::sync::Mutex<Vec<String>>>) { while let Ok((conn, _)) = http.accept().await { tokio::spawn(serve_http(conn, b, log.clone())); } }

    /// HTTP 请求是否已经收完：头结束，并且按 Content-Length 的正文也到齐。
    fn request_complete(data: &[u8]) -> bool {
        let text = String::from_utf8_lossy(data);
        let length = |t: &str| {
            t.lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0)
        };
        text.find("\r\n\r\n")
            .is_some_and(|pos| data.len() >= pos + 4 + length(&text))
    }

    /// 处理一个连接上的一次 HTTP 请求。
    async fn serve_http(
        mut conn: tokio::net::TcpStream,
        b: IgdBehavior,
        log: Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        // 读到请求收完；连接提前关了（n == 0）也往下走，按已收到的内容应答。
        let mut n = 1;
        while n != 0 && !request_complete(&data) {
            n = conn.read(&mut buf).await.unwrap_or(0);
            data.extend_from_slice(&buf[..n]);
        }
        let text = String::from_utf8_lossy(&data).to_string();
        let first = text.lines().next().unwrap_or_default().to_string();
        let action = text
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("soapaction:"))
            .unwrap_or_default();
        let name = [
            "GetExternalIPAddress",
            "AddAnyPortMapping",
            "AddPortMapping",
            "DeletePortMapping",
        ]
        .into_iter()
        .find(|n| action.contains(&format!("#{n}\"")))
        .unwrap_or("-");
        if first.starts_with("POST") {
            log.lock().unwrap().push(name.to_string());
        }
        let (status, body) = igd_route(b, &first, name, &text);
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Type: text/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = conn.write_all(reply.as_bytes()).await;
        let _ = conn.shutdown().await;
    }

    /// 启动假 IGD：返回给 `SearchOptions` 用的 SSDP 地址，以及记录收到的 SOAP 动作的列表。
    async fn fake_igd(b: IgdBehavior) -> (SocketAddr, Arc<std::sync::Mutex<Vec<String>>>) {
        let http = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let http_port = http.local_addr().unwrap().port();
        let log: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
        tokio::spawn(accept_loop(http, b, log.clone()));

        let ssdp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let ssdp_addr = ssdp.local_addr().unwrap();
        let reply = format!("HTTP/1.1 200 OK\r\nLOCATION: http://127.0.0.1:{http_port}/desc.xml\r\nST: urn:schemas-upnp-org:device:InternetGatewayDevice:1\r\n\r\n");
        tokio::spawn(serve_udp(ssdp, move |_, _| {
            vec![reply.clone().into_bytes()]
        }));
        (ssdp_addr, log)
    }

    fn igd_options(ssdp: SocketAddr) -> impl Fn() -> SearchOptions + Send + Sync + 'static {
        move || {
            let mut o = SearchOptions::default();
            o.bind_addr = "127.0.0.1:0".parse().unwrap();
            o.broadcast_address = ssdp;
            // 默认只信任和 SSDP 应答者同一地址的 URL；回环地址要显式放行。
            o.allowed_gateway_ips = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
            o.single_search_timeout = Some(Duration::from_millis(500));
            o
        }
    }

    fn local_lan() -> SocketAddr {
        "127.0.0.1:5000".parse().unwrap()
    }

    #[tokio::test]
    async fn upnp_client_happy_path() {
        let (ssdp, log) = fake_igd(OK_IGD).await;
        let m = upnp_map(local_lan(), igd_options(ssdp)(), 0, SHORT)
            .await
            .unwrap();
        assert_eq!(m.external, "203.0.113.9:45678".parse().unwrap());
        assert_eq!(m.protocol, "UPnP");
        assert_eq!(
            m.lifetime,
            Duration::from_secs(u64::from(REQUEST_LIFETIME_SECS))
        );
        let log = log.lock().unwrap().clone();
        assert_eq!(log, vec!["GetExternalIPAddress", "AddAnyPortMapping"]);
    }

    #[tokio::test]
    async fn upnp_client_renews_same_port_and_falls_back() {
        // 续租：先用同一个外部端口。
        let (ssdp, log) = fake_igd(OK_IGD).await;
        let m = upnp_map(local_lan(), igd_options(ssdp)(), 45000, SHORT)
            .await
            .unwrap();
        assert_eq!(m.external.port(), 45000);
        assert_eq!(
            log.lock().unwrap().clone(),
            vec!["GetExternalIPAddress", "AddPortMapping"]
        );

        // 同端口被占：退回让路由器挑端口。
        let (ssdp, _) = fake_igd(IgdBehavior {
            reject_fixed_port: true,
            ..OK_IGD
        })
        .await;
        let m = upnp_map(local_lan(), igd_options(ssdp)(), 45000, SHORT)
            .await
            .unwrap();
        assert_eq!(m.external.port(), 45678);
    }

    #[tokio::test]
    async fn upnp_client_retries_permanent_lease() {
        let (ssdp, log) = fake_igd(IgdBehavior {
            permanent_only: true,
            ..OK_IGD
        })
        .await;
        let m = upnp_map(local_lan(), igd_options(ssdp)(), 0, SHORT)
            .await
            .unwrap();
        assert_eq!(m.external.port(), 45678);
        // 先带租期（被 725 拒绝），再用永久租约。
        let adds = log
            .lock()
            .unwrap()
            .iter()
            .filter(|n| *n == "AddAnyPortMapping")
            .count();
        assert_eq!(adds, 2);
    }

    #[tokio::test]
    async fn upnp_client_errors() {
        // 双层 NAT：外网地址是私网地址，根本不去添加映射。
        let (ssdp, log) = fake_igd(IgdBehavior {
            external_ip: "192.168.100.2",
            ..OK_IGD
        })
        .await;
        let err = upnp_map(local_lan(), igd_options(ssdp)(), 0, SHORT)
            .await
            .unwrap_err();
        assert!(err.contains("双层 NAT / 运营商 NAT"), "{err}");
        assert_eq!(log.lock().unwrap().clone(), vec!["GetExternalIPAddress"]);

        // 路由器没有可用端口。
        let (ssdp, _) = fake_igd(IgdBehavior {
            reject_any_port: true,
            ..OK_IGD
        })
        .await;
        let err = upnp_map(local_lan(), igd_options(ssdp)(), 0, SHORT)
            .await
            .unwrap_err();
        assert!(err.contains("添加端口映射失败"), "{err}");

        // 没有 UPnP 路由器：SSDP 没人应答。
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let err = upnp_map(
            local_lan(),
            igd_options(silent.local_addr().unwrap())(),
            0,
            Duration::from_millis(300),
        )
        .await
        .unwrap_err();
        assert!(err.contains("没有发现 UPnP 路由器"), "{err}");
    }

    #[tokio::test]
    async fn upnp_unmap_client() {
        let (ssdp, log) = fake_igd(OK_IGD).await;
        upnp_unmap(igd_options(ssdp)(), 45678, SHORT).await.unwrap();
        assert_eq!(log.lock().unwrap().clone(), vec!["DeletePortMapping"]);
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let err = upnp_unmap(
            igd_options(silent.local_addr().unwrap())(),
            1,
            Duration::from_millis(200),
        )
        .await
        .unwrap_err();
        assert!(err.contains("没有发现 UPnP 路由器"));
    }

    // ------------------------- 组合 -------------------------

    /// 测试用的端点：NAT-PMP / PCP 指向给定的假路由器端口，UPnP 指向给定的 SSDP 地址。
    fn endpoints(natpmp_port: u16, pcp_port: u16, ssdp: SocketAddr) -> Endpoints {
        Endpoints {
            natpmp_port,
            pcp_port,
            upnp: Arc::new(igd_options(ssdp)),
            first_wait: FAST,
            retry_delay: Duration::from_millis(100),
            lan_probe: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
        }
    }

    async fn dead_ssdp() -> SocketAddr {
        // 一个不回话的 SSDP 地址（绑定后保持占用）。
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = s.local_addr().unwrap();
        std::mem::forget(s);
        addr
    }

    const LO: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    #[tokio::test]
    async fn map_udp_returns_first_success_whichever_protocol() {
        // 只有 NAT-PMP 能用。
        let natpmp = fake_router(natpmp_handler([203, 0, 113, 7], 40000, 3600)).await;
        let dead = closed_port().await;
        let ep = endpoints(natpmp.port(), dead, dead_ssdp().await);
        let m = map_udp_with(local_lan(), &[LO], SHORT, 0, &ep)
            .await
            .unwrap();
        assert_eq!((m.protocol, m.external.port()), ("NAT-PMP", 40000));

        // 只有 PCP 能用。
        let pcp = fake_router(pcp_handler(public_ip(), 41000, 3600)).await;
        let ep = endpoints(dead, pcp.port(), dead_ssdp().await);
        let m = map_udp_with(local_lan(), &[LO], SHORT, 0, &ep)
            .await
            .unwrap();
        assert_eq!((m.protocol, m.external.port()), ("PCP", 41000));

        // 只有 UPnP 能用，本机地址没指定（0.0.0.0）时从路由表推出：这里网关是回环，推出 127.0.0.1。
        let (ssdp, _) = fake_igd(OK_IGD).await;
        let ep = endpoints(dead, dead, ssdp);
        let m = map_udp_with("0.0.0.0:5000".parse().unwrap(), &[LO], SHORT, 0, &ep)
            .await
            .unwrap();
        assert_eq!((m.protocol, m.external.port()), ("UPnP", 45678));

        // 没有网关地址也能走 UPnP（组播发现）。
        let (ssdp, _) = fake_igd(OK_IGD).await;
        let ep = endpoints(dead, dead, ssdp);
        let m = map_udp_with(local_lan(), &[], SHORT, 0, &ep).await.unwrap();
        assert_eq!(m.protocol, "UPnP");
    }

    #[tokio::test]
    async fn map_udp_explains_every_failure() {
        let dead = closed_port().await;
        let ep = endpoints(dead, dead, dead_ssdp().await);
        let err = map_udp_with(local_lan(), &[LO], Duration::from_millis(400), 0, &ep)
            .await
            .unwrap_err();
        assert!(err.starts_with("端口映射失败"), "{err}");
        for label in ["PCP(127.0.0.1)", "NAT-PMP(127.0.0.1)", "UPnP"] {
            assert!(err.contains(label), "缺少 {label}：{err}");
        }
        assert!(err.contains("端口不可达"), "{err}");
        assert!(err.contains("没有发现 UPnP 路由器"), "{err}");

        // 没有网关。
        let err = map_udp_with(local_lan(), &[], Duration::from_millis(300), 0, &ep)
            .await
            .unwrap_err();
        assert!(err.contains("没有找到默认网关"), "{err}");

        // 本机端口 0。
        let err = map_udp_with("127.0.0.1:0".parse().unwrap(), &[LO], SHORT, 0, &ep)
            .await
            .unwrap_err();
        assert!(err.contains("不能是 0"));

        // 双层 NAT：三种协议都报同一个原因。
        let natpmp = fake_router(natpmp_handler([192, 168, 8, 2], 40000, 3600)).await;
        let pcp = fake_router(pcp_handler(
            IpAddr::V4(Ipv4Addr::new(100, 80, 1, 1)),
            41000,
            3600,
        ))
        .await;
        let (ssdp, _) = fake_igd(IgdBehavior {
            external_ip: "10.0.0.2",
            ..OK_IGD
        })
        .await;
        let ep = endpoints(natpmp.port(), pcp.port(), ssdp);
        let err = map_udp_with(local_lan(), &[LO], SHORT, 0, &ep)
            .await
            .unwrap_err();
        assert_eq!(err.matches("双层 NAT / 运营商 NAT").count(), 3, "{err}");
    }

    #[tokio::test]
    async fn map_udp_with_ipv6_gateway_skips_natpmp() {
        // 没有 IPv6 的机器上 PCP 在 connect 阶段失败，同样带着 PCP(::1) 标签；NAT-PMP 都不会被尝试。
        let dead = closed_port().await;
        let ep = endpoints(dead, dead, dead_ssdp().await);
        let err = map_udp_with(
            local_lan(),
            &[IpAddr::V6(Ipv6Addr::LOCALHOST)],
            Duration::from_millis(300),
            0,
            &ep,
        )
        .await
        .unwrap_err();
        assert!(err.contains("PCP(::1)"), "{err}");
        assert!(!err.contains("NAT-PMP"), "{err}");
    }

    #[tokio::test]
    async fn public_map_udp_without_router_reports_reasons() {
        // 真实入口：127.0.0.1:5351 上没有路由器（端口不可达），UPnP 组播在沙箱里也没人应答。
        let err = map_udp(local_lan(), &[LO], Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(err.starts_with("端口映射失败"), "{err}");
    }

    #[tokio::test]
    async fn lan_addr_resolution() {
        // 已指定地址：原样返回。
        assert_eq!(
            lan_addr(local_lan(), None, public_ip()).await.unwrap(),
            local_lan()
        );
        // 未指定：按去路由器的源地址推出，端口保留。
        let got = lan_addr("0.0.0.0:6000".parse().unwrap(), Some(LO), public_ip())
            .await
            .unwrap();
        assert_eq!(got, "127.0.0.1:6000".parse().unwrap());
        // 没有网关时用探测目标推断（这里是回环）；探测目标连不上（广播地址被系统拒绝）就明确报错。
        let got = lan_addr("0.0.0.0:6000".parse().unwrap(), None, LO)
            .await
            .unwrap();
        assert_eq!(got.port(), 6000);
        // Windows、macOS 允许 UDP 套接字 connect 到广播地址，下面的失败只在 Linux 上出现。
        #[cfg(target_os = "linux")]
        {
            let bad: IpAddr = "255.255.255.255".parse().unwrap();
            let err = lan_addr("0.0.0.0:6000".parse().unwrap(), None, bad)
                .await
                .unwrap_err();
            assert!(err.contains("无法确定本机的局域网地址"), "{err}");
            // 同样的失败经 map_udp_with 变成 UPnP 的失败原因，其他协议不受影响。
            let dead = closed_port().await;
            let mut ep = endpoints(dead, dead, dead_ssdp().await);
            ep.lan_probe = bad;
            let err = map_udp_with("0.0.0.0:6000".parse().unwrap(), &[], SHORT, 0, &ep)
                .await
                .unwrap_err();
            assert!(err.contains("UPnP：无法确定本机的局域网地址"), "{err}");
        }
    }

    #[tokio::test]
    async fn unmap_dispatches_by_protocol() {
        let natpmp = fake_router(natpmp_handler([203, 0, 113, 7], 40000, 3600)).await;
        let pcp = fake_router(pcp_handler(public_ip(), 41000, 3600)).await;
        let (ssdp, log) = fake_igd(OK_IGD).await;
        let ep = endpoints(natpmp.port(), pcp.port(), ssdp);
        let mapping = |protocol| Mapping {
            external: "203.0.113.7:40000".parse().unwrap(),
            protocol,
            lifetime: Duration::from_secs(60),
        };
        unmap_with(local_lan(), &[LO], &mapping("NAT-PMP"), SHORT, &ep)
            .await
            .unwrap();
        unmap_with(local_lan(), &[LO], &mapping("PCP"), SHORT, &ep)
            .await
            .unwrap();
        unmap_with(local_lan(), &[LO], &mapping("UPnP"), SHORT, &ep)
            .await
            .unwrap();
        assert_eq!(log.lock().unwrap().clone(), vec!["DeletePortMapping"]);

        // 未知协议、没有网关、NAT-PMP 遇到只有 IPv6 的网关、删除失败。
        let err = unmap_with(local_lan(), &[LO], &mapping("FOO"), SHORT, &ep)
            .await
            .unwrap_err();
        assert!(err.contains("未知的端口映射协议"));
        let err = unmap_with(local_lan(), &[], &mapping("PCP"), SHORT, &ep)
            .await
            .unwrap_err();
        assert!(err.contains("没有可用的网关地址"));
        let err = unmap_with(
            local_lan(),
            &[IpAddr::V6(Ipv6Addr::LOCALHOST)],
            &mapping("NAT-PMP"),
            SHORT,
            &ep,
        )
        .await
        .unwrap_err();
        assert!(err.contains("没有可用的网关地址"));
        let dead = closed_port().await;
        let ep = endpoints(dead, dead, dead_ssdp().await);
        let err = unmap_with(local_lan(), &[LO], &mapping("PCP"), SHORT, &ep)
            .await
            .unwrap_err();
        assert!(
            err.contains("删除端口映射失败") && err.contains("127.0.0.1"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn public_unmap_without_router_fails_politely() {
        let m = Mapping {
            external: "203.0.113.7:40000".parse().unwrap(),
            protocol: "PCP",
            lifetime: Duration::from_secs(60),
        };
        assert!(unmap(local_lan(), &[LO], &m, Duration::from_millis(200))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn keep_mapped_renews_publishes_failure_and_stops() {
        // 路由器给 2 秒租期：1 秒后续租。续租要回同一个外部端口（建议端口）。
        let requested = Arc::new(std::sync::Mutex::new(Vec::<u16>::new()));
        let requested2 = requested.clone();
        let ok = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ok2 = ok.clone();
        let inner = pcp_handler(public_ip(), 41000, 2);
        let pcp = fake_router(move |n, req| {
            requested2
                .lock()
                .unwrap()
                .push(u16::from_be_bytes([req[42], req[43]]));
            if ok2.load(Ordering::SeqCst) {
                inner(n, req)
            } else {
                vec![]
            }
        })
        .await;
        let dead = closed_port().await;
        let ep = endpoints(dead, pcp.port(), dead_ssdp().await);
        let first = Mapping {
            external: "203.0.113.7:41000".parse().unwrap(),
            protocol: "PCP",
            lifetime: Duration::from_secs(2),
        };
        let (tx, mut rx) = watch::channel(Some(first.clone()));
        let task = tokio::spawn(async move {
            keep_mapped_with(
                local_lan(),
                vec![LO],
                first,
                Duration::from_millis(400),
                tx,
                &ep,
            )
            .await;
        });

        // 第一次续租成功。
        tokio::time::timeout(Duration::from_secs(5), rx.changed())
            .await
            .unwrap()
            .unwrap();
        let renewed = rx.borrow_and_update().clone().unwrap();
        assert_eq!(renewed.external.port(), 41000);
        assert_eq!(
            requested.lock().unwrap().first().copied(),
            Some(41000),
            "续租应请求同一个外部端口"
        );

        // 路由器不再应答：发布 None。
        ok.store(false, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(8), rx.wait_for(Option::is_none))
            .await
            .unwrap()
            .unwrap();
        // 恢复后重试成功，重新发布映射。
        ok.store(true, Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(8), rx.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap();
        // 接收端全部丢弃：任务自己结束。
        drop(rx);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn keep_mapped_stops_when_receiver_dropped_during_sleep_or_before_publish() {
        let dead = closed_port().await;
        let ep = endpoints(dead, dead, dead_ssdp().await);
        let first = Mapping {
            external: "203.0.113.7:41000".parse().unwrap(),
            protocol: "PCP",
            lifetime: Duration::from_secs(3600),
        };
        // 睡眠期间接收端被丢弃：立即结束，不用等一小时。
        let (tx, rx) = watch::channel(Some(first.clone()));
        let ep2 = ep.clone();
        let first2 = first.clone();
        let task = tokio::spawn(async move {
            keep_mapped_with(local_lan(), vec![LO], first2, SHORT, tx, &ep2).await;
        });
        drop(rx);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();

        // 续租失败要发布 None 时发现接收端已经没了：也结束。
        let (tx, rx) = watch::channel(Some(first.clone()));
        let short = Mapping {
            lifetime: Duration::from_secs(2),
            ..first
        };
        let task = tokio::spawn(async move {
            keep_mapped_with(
                local_lan(),
                vec![LO],
                short,
                Duration::from_millis(200),
                tx,
                &ep,
            )
            .await;
        });
        // 1 秒后开始续租（必失败，约 0.2 秒），在这期间丢掉接收端。
        tokio::time::sleep(Duration::from_millis(1100)).await;
        drop(rx);
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn public_keep_mapped_stops_when_receiver_dropped() {
        let first = Mapping {
            external: "203.0.113.7:41000".parse().unwrap(),
            protocol: "PCP",
            lifetime: Duration::from_secs(3600),
        };
        let (tx, rx) = watch::channel(Some(first.clone()));
        let task = tokio::spawn(keep_mapped(local_lan(), vec![LO], first, SHORT, tx));
        drop(rx);
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn connect_to_covers_both_families() {
        // IPv6 目标：本机有 IPv6 就连上，没有就是明确的错误，都不能 panic。
        match connect_to("[2001:db8::1]:5351".parse().unwrap()).await {
            Ok(sock) => assert!(sock.local_addr().unwrap().is_ipv6()),
            Err(e) => assert!(e.contains("无法"), "{e}"),
        }
    }

    // Windows、macOS 允许 UDP 套接字 connect 到广播地址，这个失败只在 Linux 上出现。
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn connect_failure_is_reported() {
        // 没开广播权限的 UDP socket 连广播地址：系统直接拒绝（EACCES），不是"端口不可达"。
        let err = natpmp_map(
            "255.255.255.255:5351".parse().unwrap(),
            5000,
            0,
            SHORT,
            FAST,
        )
        .await
        .unwrap_err();
        assert!(err.contains("无法连接"), "{err}");
    }

    #[tokio::test]
    async fn send_failure_is_reported_as_network_error() {
        // 超过 UDP 上限的报文：发送直接失败（EMSGSIZE），不是"端口不可达"。
        let target = fake_router(|_, _| vec![]).await;
        let sock = connect_to(target).await.unwrap();
        let huge = vec![0u8; 70_000];
        let err = exchange(&sock, &huge, SHORT, FAST, |_| Some(Ok(())))
            .await
            .unwrap_err();
        assert!(err.contains("网络错误"), "{err}");
    }
}
