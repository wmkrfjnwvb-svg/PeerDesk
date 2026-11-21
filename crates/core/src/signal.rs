//! 信令：设备向服务器登记自己在线，控制端通过服务器找到被控端。
//!
//! 服务器从每条信令连接上看到设备的公网地址（设备用同一个 UDP 端口做信令和点对点连接，
//! 所以这个地址就是它在 NAT 上的映射），在两端之间交换地址，并为它们开一条中继通道备用。
//! 设备的身份是它的证书指纹，由加密连接本身证明，消息里不需要再声明。

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::paths::{Candidate, PathKind};
use crate::relay::{SessionId, TICKET_LEN};
use crate::wire::{Reader, Writer};
use crate::Error;

/// 设备指纹。
pub type DeviceId = [u8; 32];

/// 一条中继通道的使用凭据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayGrant {
    /// 中继的地址。
    pub addr: SocketAddr,
    /// 会话号。
    pub session: SessionId,
    /// 本端在这条通道里的方向（0 或 1）。
    pub side: u8,
    /// 中继票据（见 [`crate::relay::Ticket`]），转发器在第一个包里交给中继。
    pub ticket: [u8; TICKET_LEN],
}

/// 信令消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalMsg {
    /// 设备 → 服务器：我在线，可以被连接。附上本机网卡上的地址（端口是同一个），
    /// 同一个局域网里的控制端可以直接连，不用绕到公网（很多路由器不支持回环）。
    Register {
        /// 本机的候选地址（局域网、IPv6、路由器映射的端口），最多 [`MAX_LOCALS`] 个。
        locals: Vec<Candidate>,
    },
    /// 服务器 → 设备：登记成功，这是我看到的你的公网地址和你的设备码。
    Registered {
        /// 服务器看到的设备地址。
        observed: SocketAddr,
        /// 服务器给这台设备分配的 9 位设备码，同一台设备每次登记都一样。
        code: u32,
        /// 探测地址：设备从同一个端口向这里发 [`observe_packet`]，服务器比较两次看到的端口，
        /// 判断设备的 NAT 是不是“换目的地就换端口”（难打洞）。服务器没开中继时为 `None`。
        probe: Option<SocketAddr>,
    },
    /// 服务器 → 控制端：先从连接用的端口向 `relay` 发几个 [`observe_packet`]，再重发连接请求。
    Probe {
        /// 探测地址。
        relay: SocketAddr,
    },
    /// 控制端 → 服务器：我要连这台设备。
    Connect {
        /// 目标设备的指纹。
        target: DeviceId,
        /// 控制端的候选地址，转给被控端，两端同时互相探测。
        locals: Vec<Candidate>,
        /// 会员账户令牌（空表示免费用户）。
        account: String,
    },
    /// 控制端 → 服务器：我要连设备码是 `code` 的设备。
    ConnectCode {
        /// 设备码。
        code: u32,
        /// 控制端的候选地址。
        locals: Vec<Candidate>,
        /// 会员账户令牌（空表示免费用户）。
        account: String,
    },
    /// 服务器 → 控制端：对方的身份和地址，以及备用的中继通道。
    Peer {
        /// 被控端的指纹。控制端用它核对连上的是不是这台设备。
        device: DeviceId,
        /// 被控端的公网地址。
        addr: SocketAddr,
        /// 被控端的候选地址。
        locals: Vec<Candidate>,
        /// 服务器看到的控制端自己的地址：和被控端公网地址的 IP 相同时，两端在同一个出口后面。
        you: SocketAddr,
        /// 打洞方式。
        punch: PunchInfo,
        /// 中继通道。
        relay: Option<RelayGrant>,
        /// 中继的说明（走会员通道还是免费通道，或者为什么没有中继），给用户看。
        note: String,
    },
    /// 服务器 → 被控端：有人要连你，请向这个地址打洞。
    Incoming {
        /// 控制端的指纹。
        peer: DeviceId,
        /// 控制端的公网地址。
        addr: SocketAddr,
        /// 控制端的候选地址。
        locals: Vec<Candidate>,
        /// 打洞方式。
        punch: PunchInfo,
        /// 中继通道。
        relay: Option<RelayGrant>,
    },
    /// 服务器 → 控制端：目标设备不在线。
    NotFound,
    /// 设备 → 服务器：集群里有哪些节点（用于故障切换）。
    Nodes,
    /// 服务器 → 设备：集群里存活的节点：（信令地址，证书指纹，是否付费节点）。
    NodeList {
        /// 节点。
        nodes: Vec<(SocketAddr, DeviceId, bool)>,
    },
    /// 设备 → 服务器：网络体检，问服务器看到的本机地址。
    Check,
    /// 服务器 → 设备：体检结果。
    Checked {
        /// 服务器在信令端口看到的本机地址。
        observed: SocketAddr,
        /// 探测地址（服务器没开中继时为 `None`）：从同一个端口向这里发 [`observe_packet`]，
        /// 再体检一次，`seen` 就是这次探测看到的地址。
        probe: Option<SocketAddr>,
        /// 上一次体检以后，服务器在探测地址看到的本机地址。
        seen: Option<SocketAddr>,
    },
}

fn put_addr(w: &mut Writer, addr: &SocketAddr) {
    match addr.ip() {
        IpAddr::V4(ip) => w.u8(4).bytes(&ip.octets()),
        IpAddr::V6(ip) => w.u8(6).bytes(&ip.octets()),
    };
    w.u16(addr.port());
}

fn get_addr(r: &mut Reader<'_>) -> Result<SocketAddr, Error> {
    let ip = match r.u8()? {
        4 => {
            let mut b = [0u8; 4];
            b.copy_from_slice(r.take(4)?);
            IpAddr::V4(Ipv4Addr::from(b))
        }
        6 => {
            let mut b = [0u8; 16];
            b.copy_from_slice(r.take(16)?);
            IpAddr::V6(Ipv6Addr::from(b))
        }
        _ => return Err(Error::Inconsistent("未知的地址族")),
    };
    Ok(SocketAddr::new(ip, r.u16()?))
}

fn put_opt_addr(w: &mut Writer, addr: Option<&SocketAddr>) {
    match addr {
        None => {
            w.u8(0);
        }
        Some(a) => {
            w.u8(1);
            put_addr(w, a);
        }
    }
}

fn get_opt_addr(r: &mut Reader<'_>) -> Result<Option<SocketAddr>, Error> {
    match r.u8()? {
        0 => Ok(None),
        1 => Ok(Some(get_addr(r)?)),
        _ => Err(Error::Inconsistent("地址标志不合法")),
    }
}

/// 一台设备的 NAT 好不好打洞。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nat {
    /// 不知道（服务器没开中继，或者设备的探测包没到）。
    Unknown,
    /// 映射与目的地无关：发往不同目的地用同一个公网端口（各种锥形 NAT，或者没有 NAT）。
    Easy,
    /// 映射随目的地变化（对称型）：对方看到的端口和服务器看到的不一样。
    Hard,
}

impl Nat {
    /// 比较服务器在信令端口和探测端口看到的同一个设备端口的地址。
    #[must_use]
    pub fn classify(signal: SocketAddr, probe: Option<SocketAddr>) -> Self {
        match probe {
            None => Nat::Unknown,
            Some(p) if p == signal => Nat::Easy,
            Some(_) => Nat::Hard,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Nat::Unknown => 0,
            Nat::Easy => 1,
            Nat::Hard => 2,
        }
    }

    fn from_code(code: u8) -> Result<Self, Error> {
        match code {
            0 => Ok(Nat::Unknown),
            1 => Ok(Nat::Easy),
            2 => Ok(Nat::Hard),
            _ => Err(Error::Inconsistent("未知的 NAT 类型")),
        }
    }
}

/// 一次连接的打洞方式：两端的 NAT 类型和这次的随机暗号（探测包里带上，收到时核对）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PunchInfo {
    /// 暗号。
    pub nonce: [u8; 16],
    /// 收到这条消息的一端的 NAT。
    pub me: Nat,
    /// 对端的 NAT。
    pub peer: Nat,
}

/// 根据两端的 NAT 选的打洞方法（每一端各自算，结果互相对应）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunchPlan {
    /// 普通打洞：向对方的公网地址发包。
    Plain,
    /// 对方是难打洞的一端：向对方公网 IP 的随机端口撒探测包，等它的某个端口撞上。
    Spray,
    /// 本端是难打洞的一端：开很多个端口，都向对方发探测包，用最先收到对方探测包的那个端口连接。
    Sockets,
    /// 两端都难：打洞几乎不可能成功（两端各撒 256 个包时 20 秒成功率约万分之一），直接走中继。
    Skip,
}

impl PunchInfo {
    /// 这一端该用的打洞方法。
    #[must_use]
    pub const fn plan(&self) -> PunchPlan {
        match (self.me, self.peer) {
            (Nat::Hard, Nat::Hard) => PunchPlan::Skip,
            (Nat::Hard, Nat::Easy | Nat::Unknown) => PunchPlan::Sockets,
            (Nat::Easy | Nat::Unknown, Nat::Hard) => PunchPlan::Spray,
            (Nat::Easy | Nat::Unknown, Nat::Easy | Nat::Unknown) => PunchPlan::Plain,
        }
    }

    fn put(&self, w: &mut Writer) {
        w.bytes(&self.nonce).u8(self.me.code()).u8(self.peer.code());
    }

    fn get(r: &mut Reader<'_>) -> Result<Self, Error> {
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(r.take(16)?);
        Ok(Self {
            nonce,
            me: Nat::from_code(r.u8()?)?,
            peer: Nat::from_code(r.u8()?)?,
        })
    }
}

/// 探测包的开头。
pub const OBSERVE_MAGIC: &[u8; 8] = b"PDOBSV01";
/// 打洞探测包的开头。
pub const PROBE_MAGIC: &[u8; 8] = b"PDPROBE1";

/// NAT 探测包：从连接用的端口发往服务器的探测地址，带上设备指纹。
#[must_use]
pub fn observe_packet(device: &DeviceId) -> Vec<u8> {
    let mut p = OBSERVE_MAGIC.to_vec();
    p.extend_from_slice(device);
    p
}

/// 解析 NAT 探测包，返回设备指纹。
#[must_use]
pub fn parse_observe(packet: &[u8]) -> Option<DeviceId> {
    let rest = packet.strip_prefix(OBSERVE_MAGIC.as_slice())?;
    DeviceId::try_from(rest).ok()
}

/// 打洞探测包：带上这次连接的暗号和发往的端口（对方据此知道自己的公网端口）。
#[must_use]
pub fn probe_packet(nonce: &[u8; 16], to_port: u16) -> Vec<u8> {
    let mut p = PROBE_MAGIC.to_vec();
    p.extend_from_slice(nonce);
    p.extend_from_slice(&to_port.to_be_bytes());
    p
}

/// 解析打洞探测包：暗号对得上时返回包里写的目的端口。
#[must_use]
pub fn parse_probe(packet: &[u8], nonce: &[u8; 16]) -> Option<u16> {
    let rest = packet.strip_prefix(PROBE_MAGIC.as_slice())?;
    let (got, port) = rest.split_at_checked(16)?;
    if got != nonce {
        return None;
    }
    Some(u16::from_be_bytes(<[u8; 2]>::try_from(port).ok()?))
}

/// 节点列表最多这么多个节点。
pub const MAX_NODES: usize = 64;
/// 文本字段（账户令牌、说明）的最大长度。
const MAX_TEXT: usize = 256;

fn put_text(w: &mut Writer, text: &str) {
    // 截断时不切开一个字符。
    let n = (0..=text.len().min(MAX_TEXT))
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    let bytes = text.as_bytes().get(..n).unwrap_or_default();
    w.u16(u16::try_from(n).unwrap_or(0)).bytes(bytes);
}

fn get_text(r: &mut Reader<'_>) -> Result<String, Error> {
    let n = usize::from(r.u16()?);
    if n > MAX_TEXT {
        return Err(Error::Inconsistent("文本太长"));
    }
    String::from_utf8(r.take(n)?.to_vec()).map_err(|_| Error::BadUtf8)
}

/// 一台设备最多报告这么多个候选地址。
pub const MAX_LOCALS: usize = 12;

fn put_addrs(w: &mut Writer, cands: &[Candidate]) {
    let cands = cands.get(..cands.len().min(MAX_LOCALS)).unwrap_or_default();
    w.u8(u8::try_from(cands.len()).unwrap_or(0));
    for c in cands {
        w.u8(c.kind.to_code());
        put_addr(w, &c.addr);
    }
}

fn get_addrs(r: &mut Reader<'_>) -> Result<Vec<Candidate>, Error> {
    let n = usize::from(r.u8()?);
    if n > MAX_LOCALS {
        return Err(Error::Inconsistent("本机地址太多"));
    }
    (0..n)
        .map(|_| {
            let kind =
                PathKind::from_code(r.u8()?).ok_or(Error::Inconsistent("未知的候选地址种类"))?;
            Ok(Candidate {
                addr: get_addr(r)?,
                kind,
            })
        })
        .collect()
}

fn put_grant(w: &mut Writer, grant: Option<&RelayGrant>) {
    match grant {
        None => {
            w.u8(0);
        }
        Some(g) => {
            w.u8(1);
            put_addr(w, &g.addr);
            w.bytes(&g.session).u8(g.side).bytes(&g.ticket);
        }
    }
}

fn get_grant(r: &mut Reader<'_>) -> Result<Option<RelayGrant>, Error> {
    match r.u8()? {
        0 => Ok(None),
        1 => {
            let addr = get_addr(r)?;
            let mut session = [0u8; 16];
            session.copy_from_slice(r.take(16)?);
            let side = r.u8()?;
            if side > 1 {
                return Err(Error::Inconsistent("中继方向只能是 0 或 1"));
            }
            let mut ticket = [0u8; TICKET_LEN];
            ticket.copy_from_slice(r.take(TICKET_LEN)?);
            Ok(Some(RelayGrant {
                addr,
                session,
                side,
                ticket,
            }))
        }
        _ => Err(Error::Inconsistent("中继凭据标志不合法")),
    }
}

fn get_id(r: &mut Reader<'_>) -> Result<DeviceId, Error> {
    let mut id = [0u8; 32];
    id.copy_from_slice(r.take(32)?);
    Ok(id)
}

impl SignalMsg {
    /// 消息里的打洞方式（只有 `Peer` 和 `Incoming` 带）。
    #[must_use]
    pub const fn punch(&self) -> Option<&PunchInfo> {
        match self {
            SignalMsg::Peer { punch, .. } | SignalMsg::Incoming { punch, .. } => Some(punch),
            SignalMsg::Register { .. }
            | SignalMsg::Registered { .. }
            | SignalMsg::Probe { .. }
            | SignalMsg::Connect { .. }
            | SignalMsg::ConnectCode { .. }
            | SignalMsg::NotFound
            | SignalMsg::Check
            | SignalMsg::Nodes
            | SignalMsg::NodeList { .. }
            | SignalMsg::Checked { .. } => None,
        }
    }

    /// 消息名。
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            SignalMsg::Register { .. } => "Register",
            SignalMsg::Registered { .. } => "Registered",
            SignalMsg::Connect { .. } => "Connect",
            SignalMsg::ConnectCode { .. } => "ConnectCode",
            SignalMsg::Peer { .. } => "Peer",
            SignalMsg::Incoming { .. } => "Incoming",
            SignalMsg::Probe { .. } => "Probe",
            SignalMsg::NotFound => "NotFound",
            SignalMsg::Check => "Check",
            SignalMsg::Nodes => "Nodes",
            SignalMsg::NodeList { .. } => "NodeList",
            SignalMsg::Checked { .. } => "Checked",
        }
    }

    /// 编码。
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::new();
        match self {
            SignalMsg::Register { locals } => {
                w.u8(1);
                put_addrs(&mut w, locals);
            }
            SignalMsg::Registered {
                observed,
                code,
                probe,
            } => {
                w.u8(2);
                put_addr(&mut w, observed);
                w.u32(*code);
                put_opt_addr(&mut w, probe.as_ref());
            }
            SignalMsg::Probe { relay } => {
                w.u8(8);
                put_addr(&mut w, relay);
            }
            SignalMsg::Connect {
                target,
                locals,
                account,
            } => {
                w.u8(3).bytes(target);
                put_addrs(&mut w, locals);
                put_text(&mut w, account);
            }
            SignalMsg::Nodes => {
                w.u8(11);
            }
            SignalMsg::NodeList { nodes } => {
                let n = nodes.len().min(MAX_NODES);
                w.u8(12).u8(u8::try_from(n).unwrap_or(0));
                for (addr, fp, paid) in nodes.iter().take(n) {
                    put_addr(&mut w, addr);
                    w.bytes(fp).u8(u8::from(*paid));
                }
            }
            SignalMsg::Peer {
                device,
                addr,
                locals,
                you,
                punch,
                relay,
                note,
            } => {
                w.u8(4).bytes(device);
                put_addr(&mut w, addr);
                put_addrs(&mut w, locals);
                put_addr(&mut w, you);
                punch.put(&mut w);
                put_grant(&mut w, relay.as_ref());
                put_text(&mut w, note);
            }
            SignalMsg::Incoming {
                peer,
                addr,
                locals,
                punch,
                relay,
            } => {
                w.u8(5).bytes(peer);
                put_addr(&mut w, addr);
                put_addrs(&mut w, locals);
                punch.put(&mut w);
                put_grant(&mut w, relay.as_ref());
            }
            SignalMsg::NotFound => {
                w.u8(6);
            }
            SignalMsg::Check => {
                w.u8(9);
            }
            SignalMsg::Checked {
                observed,
                probe,
                seen,
            } => {
                w.u8(10);
                put_addr(&mut w, observed);
                put_opt_addr(&mut w, probe.as_ref());
                put_opt_addr(&mut w, seen.as_ref());
            }
            SignalMsg::ConnectCode {
                code,
                locals,
                account,
            } => {
                w.u8(7).u32(*code);
                put_addrs(&mut w, locals);
                put_text(&mut w, account);
            }
        }
        w.into_vec()
    }

    /// 解码。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let msg = match r.u8()? {
            1 => SignalMsg::Register {
                locals: get_addrs(&mut r)?,
            },
            2 => SignalMsg::Registered {
                observed: get_addr(&mut r)?,
                code: r.u32()?,
                probe: get_opt_addr(&mut r)?,
            },
            8 => SignalMsg::Probe {
                relay: get_addr(&mut r)?,
            },
            3 => SignalMsg::Connect {
                target: get_id(&mut r)?,
                locals: get_addrs(&mut r)?,
                account: get_text(&mut r)?,
            },
            11 => SignalMsg::Nodes,
            12 => {
                let n = usize::from(r.u8()?);
                if n > MAX_NODES {
                    return Err(Error::Inconsistent("节点太多"));
                }
                let mut nodes = Vec::with_capacity(n);
                for _ in 0..n {
                    let addr = get_addr(&mut r)?;
                    let fp = get_id(&mut r)?;
                    nodes.push((addr, fp, r.u8()? == 1));
                }
                SignalMsg::NodeList { nodes }
            }
            4 => SignalMsg::Peer {
                device: get_id(&mut r)?,
                addr: get_addr(&mut r)?,
                locals: get_addrs(&mut r)?,
                you: get_addr(&mut r)?,
                punch: PunchInfo::get(&mut r)?,
                relay: get_grant(&mut r)?,
                note: get_text(&mut r)?,
            },
            5 => SignalMsg::Incoming {
                peer: get_id(&mut r)?,
                addr: get_addr(&mut r)?,
                locals: get_addrs(&mut r)?,
                punch: PunchInfo::get(&mut r)?,
                relay: get_grant(&mut r)?,
            },
            6 => SignalMsg::NotFound,
            9 => SignalMsg::Check,
            10 => SignalMsg::Checked {
                observed: get_addr(&mut r)?,
                probe: get_opt_addr(&mut r)?,
                seen: get_opt_addr(&mut r)?,
            },
            7 => SignalMsg::ConnectCode {
                code: r.u32()?,
                locals: get_addrs(&mut r)?,
                account: get_text(&mut r)?,
            },
            other => return Err(Error::UnknownMessage(other)),
        };
        // 兼容：新版本可以在消息末尾追加字段，老版本读完认识的部分，忽略后面的。
        let _ = r;
        Ok(msg)
    }
}

/// 设备码的范围：9 位数，首位不为 0。
pub const CODE_MIN: u32 = 100_000_000;
/// 设备码的上限（含）。
pub const CODE_MAX: u32 = 999_999_999;

/// 把设备码显示成 `123 456 789`。
#[must_use]
pub fn format_code(code: u32) -> String {
    format!(
        "{:03} {:03} {:03}",
        code / 1_000_000,
        code / 1000 % 1000,
        code % 1000
    )
}

/// 解析用户输入的设备码：忽略空格和短横线，必须正好是 9 位数。
#[must_use]
pub fn parse_code(text: &str) -> Option<u32> {
    let digits: String = text.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    if digits.len() != 9 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits
        .parse()
        .ok()
        .filter(|c| (CODE_MIN..=CODE_MAX).contains(c))
}

/// 服务器端的在线设备表和设备码簿（不做 IO）。连接用一个由 IO 层分配的编号标识。
///
/// 设备码一经分配就固定在这台设备（指纹）上，码簿由 IO 层存盘，服务器重启后不变。
#[derive(Debug, Clone, Default)]
pub struct Registry {
    devices: BTreeMap<DeviceId, Online>,
    codes: BTreeMap<DeviceId, u32>,
    owners: BTreeMap<u32, DeviceId>,
}

/// 一台在线设备。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Online {
    conn: u64,
    observed: SocketAddr,
    locals: Vec<Candidate>,
}

/// 发起连接的一端：指纹、服务器看到的地址、它自己报告的候选地址。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// 指纹。
    pub device: DeviceId,
    /// 服务器看到的地址。
    pub addr: SocketAddr,
    /// 候选地址。
    pub locals: Vec<Candidate>,
}

/// [`Registry::connect`] 的结果：要发给谁什么消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Introduction {
    /// 回给控制端的消息。
    pub to_controller: SignalMsg,
    /// 被控端的连接编号和要发给它的消息；目标不在线时为 `None`。
    pub to_host: Option<(u64, SignalMsg)>,
}

/// [`Registry::register`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registered {
    /// 回给设备的消息。
    pub reply: SignalMsg,
    /// 设备码。
    pub code: u32,
    /// 这次登记新分配了设备码，码簿需要存盘。
    pub new_code: bool,
}

impl Registry {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            codes: BTreeMap::new(),
            owners: BTreeMap::new(),
        }
    }

    /// 从存盘的码簿恢复（每行 `设备码 指纹十六进制`）。返回读不懂的行数，这些行被跳过。
    pub fn load_codes(&mut self, text: &str) -> usize {
        let mut bad = 0;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let mut parts = line.split_whitespace();
            let code = parts.next().and_then(|c| c.parse::<u32>().ok());
            let id = parts.next().and_then(parse_id);
            match (code, id, parts.next()) {
                (Some(code), Some(id), None)
                    if (CODE_MIN..=CODE_MAX).contains(&code)
                        && !self.owners.contains_key(&code)
                        && !self.codes.contains_key(&id) =>
                {
                    self.codes.insert(id, code);
                    self.owners.insert(code, id);
                }
                _ => bad += 1,
            }
        }
        bad
    }

    /// 码簿的存盘文本，格式见 [`Registry::load_codes`]。
    #[must_use]
    pub fn codes_text(&self) -> String {
        let mut out = String::new();
        for (code, id) in &self.owners {
            out.push_str(&format!("{code} {}\n", hex(id)));
        }
        out
    }

    /// 已分配的设备码数。
    #[must_use]
    pub fn known(&self) -> usize {
        self.codes.len()
    }

    /// 登记设备在线。同一台设备重新登记时以最新的连接为准。
    ///
    /// 设备还没有设备码时，从 `random` 取随机数分配一个不冲突的；取 64 次都冲突
    /// （码簿接近装满，实际不会发生）时返回 `None`。
    pub fn register(
        &mut self,
        conn: u64,
        device: DeviceId,
        observed: SocketAddr,
        locals: &[Candidate],
        probe: Option<SocketAddr>,
        random: &mut dyn FnMut() -> u32,
    ) -> Option<Registered> {
        let (code, new_code) = match self.codes.get(&device) {
            Some(code) => (*code, false),
            None => {
                let span = CODE_MAX - CODE_MIN + 1;
                let code = (0..64)
                    .map(|_| CODE_MIN + random() % span)
                    .find(|c| !self.owners.contains_key(c))?;
                self.codes.insert(device, code);
                self.owners.insert(code, device);
                (code, true)
            }
        };
        let locals = locals
            .get(..locals.len().min(MAX_LOCALS))
            .unwrap_or_default()
            .to_vec();
        self.devices.insert(
            device,
            Online {
                conn,
                observed,
                locals,
            },
        );
        Some(Registered {
            reply: SignalMsg::Registered {
                observed,
                code,
                probe,
            },
            code,
            new_code,
        })
    }

    /// 设备是否在线。
    #[must_use]
    pub fn is_online(&self, device: &DeviceId) -> bool {
        self.devices.contains_key(device)
    }

    /// 设备码对应的设备。
    #[must_use]
    pub fn device_of(&self, code: u32) -> Option<DeviceId> {
        self.owners.get(&code).copied()
    }

    /// 一条连接断开：如果它是某台设备当前的登记连接，把设备标为离线。
    pub fn disconnect(&mut self, conn: u64) {
        self.devices.retain(|_, d| d.conn != conn);
    }

    /// 在线设备数。
    #[must_use]
    pub fn online(&self) -> usize {
        self.devices.len()
    }

    /// `caller` 请求连接 `target`。`relay` 是为这次连接准备的中继地址和会话号（没有中继时为 `None`）；
    /// `nonce` 是这次连接的暗号；`probe_of` 给出服务器在探测地址上看到的某台设备的地址。
    #[must_use]
    pub fn connect(
        &self,
        caller: &Caller,
        target: &DeviceId,
        relay: Option<(SocketAddr, SessionId, [u8; TICKET_LEN])>,
        nonce: [u8; 16],
        probe_of: &dyn Fn(&DeviceId) -> Option<SocketAddr>,
    ) -> Introduction {
        let Some(host) = self.devices.get(target) else {
            return Introduction {
                to_controller: SignalMsg::NotFound,
                to_host: None,
            };
        };
        let grant = |side| {
            relay.map(|(addr, session, ticket)| RelayGrant {
                addr,
                session,
                side,
                ticket,
            })
        };
        let (controller, controller_addr) = (caller.device, caller.addr);
        let ctrl_nat = Nat::classify(controller_addr, probe_of(&controller));
        let host_nat = Nat::classify(host.observed, probe_of(target));
        Introduction {
            to_controller: SignalMsg::Peer {
                device: *target,
                addr: host.observed,
                locals: host.locals.clone(),
                you: controller_addr,
                punch: PunchInfo {
                    nonce,
                    me: ctrl_nat,
                    peer: host_nat,
                },
                relay: grant(0),
                note: String::new(),
            },
            to_host: Some((
                host.conn,
                SignalMsg::Incoming {
                    peer: controller,
                    addr: controller_addr,
                    locals: caller
                        .locals
                        .get(..caller.locals.len().min(MAX_LOCALS))
                        .unwrap_or_default()
                        .to_vec(),
                    punch: PunchInfo {
                        nonce,
                        me: host_nat,
                        peer: ctrl_nat,
                    },
                    relay: grant(1),
                },
            )),
        }
    }
}

fn hex(id: &DeviceId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_id(text: &str) -> Option<DeviceId> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut id = [0u8; 32];
    for (i, byte) in id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lan(a: &str) -> Candidate {
        Candidate {
            addr: a.parse().unwrap(),
            kind: PathKind::Lan,
        }
    }

    fn caller(device: DeviceId, a: SocketAddr) -> Caller {
        Caller {
            device,
            addr: a,
            locals: vec![lan("10.9.9.9:1")],
        }
    }

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    fn grant(side: u8) -> RelayGrant {
        RelayGrant {
            addr: addr("[2001:db8::1]:3478"),
            session: [5; 16],
            side,
            ticket: [4; TICKET_LEN],
        }
    }

    #[test]
    fn messages_roundtrip() {
        let all = [
            SignalMsg::Register { locals: vec![] },
            SignalMsg::Register {
                locals: vec![lan("192.168.1.5:4433"), lan("[fd00::5]:4433")],
            },
            SignalMsg::Registered {
                observed: addr("203.0.113.9:40000"),
                code: 123_456_789,
                probe: Some(addr("203.0.113.1:3478")),
            },
            SignalMsg::Registered {
                observed: addr("203.0.113.9:40000"),
                code: 123_456_789,
                probe: None,
            },
            SignalMsg::Probe {
                relay: addr("203.0.113.1:3478"),
            },
            SignalMsg::Check,
            SignalMsg::Nodes,
            SignalMsg::NodeList {
                nodes: vec![(addr("203.0.113.1:4433"), [1; 32], true)],
            },
            SignalMsg::Checked {
                observed: addr("203.0.113.9:4000"),
                probe: Some(addr("203.0.113.1:3478")),
                seen: None,
            },
            SignalMsg::Connect {
                target: [1; 32],
                locals: vec![lan("10.0.0.3:9")],
                account: "pdk1-token".into(),
            },
            SignalMsg::ConnectCode {
                code: 987_654_321,
                locals: vec![],
                account: String::new(),
            },
            SignalMsg::Peer {
                device: [3; 32],
                addr: addr("[2001:db8::2]:1"),
                locals: vec![lan("10.0.0.2:1")],
                you: addr("[2001:db8::3]:2"),
                punch: PunchInfo {
                    nonce: [6; 16],
                    me: Nat::Easy,
                    peer: Nat::Hard,
                },
                relay: Some(grant(0)),
                note: "免费通道".into(),
            },
            SignalMsg::Peer {
                device: [4; 32],
                addr: addr("1.2.3.4:5"),
                locals: vec![],
                you: addr("1.2.3.4:6"),
                punch: PunchInfo {
                    nonce: [6; 16],
                    me: Nat::Easy,
                    peer: Nat::Hard,
                },
                relay: None,
                note: "免费通道".into(),
            },
            SignalMsg::Incoming {
                peer: [2; 32],
                addr: addr("1.2.3.4:5"),
                locals: vec![lan("10.0.0.4:1")],
                punch: PunchInfo {
                    nonce: [6; 16],
                    me: Nat::Easy,
                    peer: Nat::Hard,
                },
                relay: Some(grant(1)),
            },
            SignalMsg::NotFound,
        ];
        let mut names = Vec::new();
        for msg in all {
            assert_eq!(SignalMsg::decode(&msg.encode()).unwrap(), msg);
            names.push(msg.name());
        }
        assert_eq!(
            names,
            [
                "Register",
                "Register",
                "Registered",
                "Registered",
                "Probe",
                "Check",
                "Nodes",
                "NodeList",
                "Checked",
                "Connect",
                "ConnectCode",
                "Peer",
                "Peer",
                "Incoming",
                "NotFound"
            ]
        );
    }

    #[test]
    fn text_fields_are_bounded_and_utf8() {
        let long = "中".repeat(200);
        let msg = SignalMsg::ConnectCode {
            code: 1,
            locals: vec![],
            account: long,
        };
        let SignalMsg::ConnectCode { account, .. } = SignalMsg::decode(&msg.encode()).unwrap()
        else {
            panic!("应为 ConnectCode")
        };
        assert_eq!(account.chars().count(), MAX_TEXT / 3, "截断时不切开字符");
        // 长度超过上限、内容不是 UTF-8 都拒绝
        let mut too_long = vec![7u8, 0, 0, 0, 1, 0];
        too_long.extend_from_slice(&(u16::try_from(MAX_TEXT).unwrap() + 1).to_be_bytes());
        assert_eq!(
            SignalMsg::decode(&too_long),
            Err(Error::Inconsistent("文本太长"))
        );
        assert_eq!(
            SignalMsg::decode(&[7, 0, 0, 0, 1, 0, 0, 1, 0xff]),
            Err(Error::BadUtf8)
        );
        assert_eq!(
            SignalMsg::decode(&[12, u8::try_from(MAX_NODES).unwrap() + 1]),
            Err(Error::Inconsistent("节点太多"))
        );
    }

    #[test]
    fn malformed_messages_are_rejected() {
        assert_eq!(
            SignalMsg::decode(&[1u8, 13]),
            Err(Error::Inconsistent("本机地址太多"))
        );
        assert_eq!(
            SignalMsg::decode(&[1u8, 1, 9, 4, 1, 2, 3, 4, 0, 1]),
            Err(Error::Inconsistent("未知的候选地址种类"))
        );
        // 编码时多出来的地址截掉
        let many = SignalMsg::Register {
            locals: vec![lan("10.0.0.1:1"); 20],
        };
        assert_eq!(
            SignalMsg::decode(&many.encode()),
            Ok(SignalMsg::Register {
                locals: vec![lan("10.0.0.1:1"); MAX_LOCALS]
            })
        );
        assert_eq!(SignalMsg::decode(&[0]), Err(Error::UnknownMessage(0)));
        // 末尾多出来的字节是新版本追加的字段，忽略
        assert_eq!(
            SignalMsg::decode(&[1, 0, 0]),
            Ok(SignalMsg::Register { locals: vec![] })
        );
        assert_eq!(
            SignalMsg::decode(&[2, 9]),
            Err(Error::Inconsistent("未知的地址族"))
        );
        assert!(matches!(
            SignalMsg::decode(&[2, 4, 1, 2]),
            Err(Error::Truncated { .. })
        ));
        assert!(matches!(
            SignalMsg::decode(&[3, 1, 2]),
            Err(Error::Truncated { .. })
        ));
        let mut bad_flag = SignalMsg::Peer {
            device: [4; 32],
            addr: addr("1.2.3.4:5"),
            locals: vec![],
            you: addr("1.2.3.4:6"),
            punch: PunchInfo {
                nonce: [6; 16],
                me: Nat::Easy,
                peer: Nat::Hard,
            },
            relay: None,
            note: String::new(),
        }
        .encode();
        // 最后是说明的长度（2 字节），前面一个字节是中继凭据标志
        let flag = bad_flag.len() - 3;
        bad_flag[flag] = 7;
        assert_eq!(
            SignalMsg::decode(&bad_flag),
            Err(Error::Inconsistent("中继凭据标志不合法"))
        );
        let mut bad_side = SignalMsg::Peer {
            device: [4; 32],
            addr: addr("1.2.3.4:5"),
            locals: vec![],
            you: addr("1.2.3.4:6"),
            punch: PunchInfo {
                nonce: [6; 16],
                me: Nat::Easy,
                peer: Nat::Hard,
            },
            relay: Some(grant(0)),
            note: String::new(),
        }
        .encode();
        // 方向后面是票据（65 字节）和说明的长度（2 字节）
        let side = bad_side.len() - 3 - TICKET_LEN;
        bad_side[side] = 2;
        assert_eq!(
            SignalMsg::decode(&bad_side),
            Err(Error::Inconsistent("中继方向只能是 0 或 1"))
        );
        let good = SignalMsg::Incoming {
            peer: [2; 32],
            addr: addr("1.2.3.4:5"),
            locals: vec![lan("10.0.0.4:1")],
            punch: PunchInfo {
                nonce: [6; 16],
                me: Nat::Easy,
                peer: Nat::Hard,
            },
            relay: Some(grant(1)),
        }
        .encode();
        for len in 0..good.len() {
            assert!(SignalMsg::decode(&good[..len]).is_err(), "{len}");
        }
    }

    #[test]
    fn registry_introduces_peers_with_matching_relay_sides() {
        let mut reg = Registry::new();
        let (host, ctrl) = ([1; 32], [2; 32]);
        let mut next = || 5;
        assert_eq!(
            reg.register(
                10,
                host,
                addr("198.51.100.1:5000"),
                &[lan("192.168.0.9:5000"); 20],
                None,
                &mut next
            ),
            Some(Registered {
                reply: SignalMsg::Registered {
                    observed: addr("198.51.100.1:5000"),
                    code: CODE_MIN + 5,
                    probe: None,
                },
                code: CODE_MIN + 5,
                new_code: true,
            })
        );
        assert_eq!(reg.online(), 1);

        let relay = Some((addr("203.0.113.1:3478"), [8; 16], [4; TICKET_LEN]));
        let intro = reg.connect(
            &caller(ctrl, addr("192.0.2.7:6000")),
            &host,
            relay,
            [6; 16],
            &|_| None,
        );
        match (&intro.to_controller, &intro.to_host) {
            (
                SignalMsg::Peer {
                    device,
                    addr: host_addr,
                    locals,
                    you,
                    punch,
                    relay: Some(c),
                    ..
                },
                Some((
                    10,
                    SignalMsg::Incoming {
                        peer,
                        addr: ctrl_addr,
                        relay: Some(h),
                        ..
                    },
                )),
            ) => {
                assert_eq!(*device, host);
                assert_eq!(punch.plan(), PunchPlan::Plain, "不知道 NAT 类型时普通打洞");
                assert_eq!(locals.len(), MAX_LOCALS, "多报的地址截掉");
                assert_eq!(*you, addr("192.0.2.7:6000"));
                assert_eq!(*host_addr, addr("198.51.100.1:5000"));
                assert_eq!(*ctrl_addr, addr("192.0.2.7:6000"));
                assert_eq!(*peer, ctrl);
                assert_eq!((c.session, h.session), ([8; 16], [8; 16]));
                assert_eq!((c.side, h.side), (0, 1), "两端拿到同一条通道的两个方向");
            }
            other => panic!("{other:?}"),
        }
        // 没有中继可用
        let intro = reg.connect(
            &caller(ctrl, addr("192.0.2.7:6000")),
            &host,
            None,
            [6; 16],
            &|_| None,
        );
        assert!(matches!(
            intro.to_controller,
            SignalMsg::Peer { relay: None, .. }
        ));

        // 目标不在线
        let intro = reg.connect(
            &caller(ctrl, addr("192.0.2.7:6000")),
            &[9; 32],
            relay,
            [6; 16],
            &|_| None,
        );
        assert_eq!(
            intro,
            Introduction {
                to_controller: SignalMsg::NotFound,
                to_host: None
            }
        );

        // 重新登记：以新连接为准；旧连接断开不影响
        let again = reg
            .register(11, host, addr("198.51.100.1:5001"), &[], None, &mut next)
            .unwrap();
        assert_eq!(
            (again.code, again.new_code),
            (CODE_MIN + 5, false),
            "设备码不变"
        );
        reg.disconnect(10);
        assert_eq!(reg.online(), 1);
        reg.disconnect(11);
        assert_eq!(reg.online(), 0);
    }

    #[test]
    fn codes_are_unique_and_survive_a_restart() {
        let mut reg = Registry::new();
        // 随机数先给一个会冲突的值，再给新值
        let mut seq = [7u32, 7, 7, 9].into_iter();
        let mut next = || seq.next().unwrap_or(0);
        let a = reg
            .register(1, [1; 32], addr("1.1.1.1:1"), &[], None, &mut next)
            .unwrap();
        let b = reg
            .register(2, [2; 32], addr("1.1.1.1:2"), &[], None, &mut next)
            .unwrap();
        assert_eq!((a.code, b.code), (CODE_MIN + 7, CODE_MIN + 9));
        assert_eq!(reg.device_of(b.code), Some([2; 32]));
        assert_eq!(reg.device_of(CODE_MIN), None);
        // 码簿装不下（随机数一直冲突）时拒绝登记
        let mut stuck = || 7;
        assert_eq!(
            reg.register(3, [3; 32], addr("1.1.1.1:3"), &[], None, &mut stuck),
            None
        );

        let text = reg.codes_text();
        let mut restarted = Registry::new();
        let junk = format!(
            "{text}\nnot a line\n1 {}\n{} zz\n{text}",
            "00".repeat(32),
            CODE_MIN + 1
        );
        assert_eq!(restarted.load_codes(&junk), 5, "坏行、越界、重复的都跳过");
        assert_eq!(restarted.known(), 2);
        assert_eq!(restarted.online(), 0);
        let again = restarted
            .register(5, [1; 32], addr("2.2.2.2:2"), &[], None, &mut stuck)
            .unwrap();
        assert_eq!((again.code, again.new_code), (CODE_MIN + 7, false));
        assert_eq!(
            restarted.load_codes(&format!("{} {} extra", CODE_MIN + 2, "ab".repeat(32))),
            1
        );
        assert_eq!(parse_id("é".repeat(32).as_str()), None);
    }

    #[test]
    fn codes_are_shown_and_typed_like_phone_numbers() {
        assert_eq!(format_code(123_045_009), "123 045 009");
        assert_eq!(parse_code(" 123 045-009 "), Some(123_045_009));
        assert_eq!(parse_code("12345678"), None);
        assert_eq!(parse_code("1234567890"), None);
        assert_eq!(parse_code("12345678x"), None);
        assert_eq!(parse_code("012345678"), None, "首位不能是 0");
    }

    #[test]
    fn nat_classes_pick_matching_plans() {
        let a = addr("203.0.113.9:4000");
        assert_eq!(Nat::classify(a, None), Nat::Unknown);
        assert_eq!(Nat::classify(a, Some(a)), Nat::Easy);
        assert_eq!(Nat::classify(a, Some(addr("203.0.113.9:4001"))), Nat::Hard);
        let plan = |me, peer| {
            PunchInfo {
                nonce: [0; 16],
                me,
                peer,
            }
            .plan()
        };
        assert_eq!(plan(Nat::Hard, Nat::Hard), PunchPlan::Skip);
        assert_eq!(plan(Nat::Hard, Nat::Easy), PunchPlan::Sockets);
        assert_eq!(plan(Nat::Hard, Nat::Unknown), PunchPlan::Sockets);
        assert_eq!(plan(Nat::Easy, Nat::Hard), PunchPlan::Spray);
        assert_eq!(plan(Nat::Unknown, Nat::Hard), PunchPlan::Spray);
        assert_eq!(plan(Nat::Easy, Nat::Unknown), PunchPlan::Plain);
        assert_eq!(
            Nat::from_code(3),
            Err(Error::Inconsistent("未知的 NAT 类型"))
        );

        // 服务器按探测结果给两端算出对应的方法：难的一端开很多端口，容易的一端撒包
        let mut reg = Registry::new();
        let (host, ctrl) = ([1; 32], [2; 32]);
        let host_seen = addr("198.51.100.1:5000");
        let mut next = || 1;
        reg.register(1, host, host_seen, &[], None, &mut next)
            .unwrap();
        let probes = move |d: &DeviceId| match d {
            [1, ..] => Some(host_seen),
            _ => Some(addr("192.0.2.7:6001")),
        };
        let intro = reg.connect(
            &caller(ctrl, addr("192.0.2.7:6000")),
            &host,
            None,
            [9; 16],
            &probes,
        );
        let ctrl_plan = intro.to_controller.punch().map(PunchInfo::plan);
        let host_punch = intro.to_host.as_ref().and_then(|(_, m)| m.punch());
        assert_eq!(ctrl_plan, Some(PunchPlan::Sockets));
        assert_eq!(host_punch.map(PunchInfo::plan), Some(PunchPlan::Spray));
        assert_eq!(host_punch.map(|p| p.nonce), Some([9; 16]));
        assert_eq!(SignalMsg::NotFound.punch(), None);
    }

    #[test]
    fn probe_packets_carry_nonce_and_port() {
        let fp = [7u8; 32];
        assert_eq!(parse_observe(&observe_packet(&fp)), Some(fp));
        assert_eq!(parse_observe(b"PDOBSV01short"), None);
        assert_eq!(parse_observe(b"other"), None);
        let nonce = [3u8; 16];
        let p = probe_packet(&nonce, 40_123);
        assert_eq!(parse_probe(&p, &nonce), Some(40_123));
        assert_eq!(parse_probe(&p, &[4; 16]), None, "暗号不对");
        assert_eq!(parse_probe(&p[..p.len() - 1], &nonce), None);
        assert_eq!(parse_probe(&p[..10], &nonce), None);
        assert_eq!(parse_probe(b"junk", &nonce), None);
        // 地址标志不合法
        let mut bad = SignalMsg::Registered {
            observed: addr("1.2.3.4:5"),
            code: 1,
            probe: None,
        }
        .encode();
        *bad.last_mut().unwrap() = 9;
        assert_eq!(
            SignalMsg::decode(&bad),
            Err(Error::Inconsistent("地址标志不合法"))
        );
    }
}
