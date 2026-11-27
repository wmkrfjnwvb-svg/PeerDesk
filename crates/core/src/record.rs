//! 会话记录文件的格式（M1 的 R1，设计见 `docs/research/Q-redesign/m1-recorder.md` 第 1 节）。
//!
//! 纯逻辑：不做 IO、不读时钟。写的一方（[`BlockWriter`]）把记录攒成块，交出字节；读的一方
//! （[`Recording::parse`]）从字节读回记录，遇到坏块、截断、不认识的内容都不报错，只计数。
//!
//! # 文件结构（全部整数大端，除非写明是变长整数）
//!
//! ```text
//! 文件头：魔数 "PDSR"(4) | 格式版本 u16 | 头长度 u16（字节，只算后面的元数据） | 元数据（TLV，见 [`Meta`]）
//! 块 × N：块长度 u32（字节，只算“块内容”） | CRC32 u32（对块内容算） | 块内容
//!         块内容 = 块的起始时间（变长整数，微秒，本端单调时钟，从会话开始算） | 记录 × M
//!         记录   = 种类（变长整数） | 长度（变长整数，字节，只算“记录内容”） | 记录内容
//!         记录内容 = 时间差（变长整数，微秒） | 各字段
//! ```
//!
//! - **变长整数**：每字节低 7 位是数据，高位为 1 表示后面还有，低位在前（LEB128），最长 10 字节。
//!   有符号数先做 zigzag（0、-1、1、-2……对应 0、1、2、3……）。
//! - **时间差**：这条记录的时间，减去“同一块里同一种类的上一条记录”的时间；同一种类的第一条减块的起始时间。
//!   所以每个块能单独解。同一种类的时间如果倒退，按上一条的时间写（差为 0）。
//! - **帧号、确认到的帧号**：同样用和同一块里上一条同类记录的差（有符号），块里第一条是绝对值。
//! - **可扩展**：读的一方遇到不认识的种类按长度跳过（计入 [`ReadStats::unknown_records`]）；认识的种类里
//!   多出来的尾部字段忽略。加字段只能加在尾部、加种类不用改版本号；版本号只在块结构变了时才加。
//! - **损坏和截断**：逐块校验 CRC。坏块跳过，从下一个能校验通过的块接着读（块长度字段本身坏了也一样：
//!   往后逐字节找下一个长度和 CRC 都对得上的位置）；文件尾部不完整的那一块丢掉。
//!   正常结束写一条 [`Record::End`]；没有它说明进程是崩掉或被杀的（[`Recording::end`] 为 `None`）。
//!
//! 文件头没有校验：它坏了整份文件读不出来（返回错误），不猜。

use std::borrow::Cow;
use std::fmt;

use crate::estimator::{NetCause, Observation, RateState, SendLimit};
use crate::proto::PeerOs;

/// 魔数。
pub const MAGIC: [u8; 4] = *b"PDSR";
/// 当前格式版本。
pub const VERSION: u16 = 1;
/// 一个块最多覆盖多久（微秒）：块里最早的记录和新来的记录差到这么多就封块。
pub const BLOCK_SPAN_US: u64 = 1_000_000;
/// 一个块的内容攒到这么多字节（含）就封块。
pub const BLOCK_SOFT_BYTES: usize = 64 * 1024;
/// 读的一方接受的块内容上限（字节）。写的一方在 [`BLOCK_SOFT_BYTES`] 后封块，一条记录最多再多出一点，
/// 远小于这个值；读的一方用它挡住长度字段损坏时的荒谬值。
pub const MAX_BLOCK_BYTES: usize = 1 << 20;
/// 文件名后缀。
pub const EXTENSION: &str = "pdsr";
/// 写的过程中的后缀，正常结束后去掉。
pub const PART_SUFFIX: &str = ".part";
/// 块头长度：块长度 u32 加 CRC u32。
const BLOCK_HEAD: usize = 8;
/// 文件头里固定部分的长度：魔数、版本、头长度。
const FIXED_HEAD: usize = 8;
/// 元数据里字符串的长度上限（字节）。
const META_TEXT_MAX: usize = 64;
/// 帧记录里关键帧原因的长度上限（字节）；超过的在字符边界处截断。
pub const KEY_REASON_MAX: usize = 96;

/// 读写出错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordError {
    /// 不是记录文件（魔数不对，或者连文件头都不完整）。
    NotARecording,
    /// 格式版本比本程序认识的新。
    UnsupportedVersion(u16),
    /// 文件头里的元数据坏了（缺必须的字段、长度对不上、字符串不是合法 UTF-8）。
    BadHeader,
}

impl fmt::Display for RecordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotARecording => write!(f, "不是会话记录文件（魔数不对或文件头不完整）"),
            Self::UnsupportedVersion(v) => write!(f, "会话记录的格式版本 {v} 比本程序认识的新"),
            Self::BadHeader => write!(f, "会话记录的文件头损坏"),
        }
    }
}

impl std::error::Error for RecordError {}

// ---------------------------------------------------------------------------
// 变长整数和游标
// ---------------------------------------------------------------------------

/// 写变长整数。
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let low = u8::try_from(v & 0x7f).unwrap_or(0);
        v >>= 7;
        if v == 0 {
            out.push(low);
            return;
        }
        out.push(low | 0x80);
    }
}

/// zigzag：有符号转无符号。
const fn zigzag(v: i64) -> u64 {
    let bits = u64::from_ne_bytes(v.to_ne_bytes());
    let sign = if v < 0 { u64::MAX } else { 0 };
    (bits << 1) ^ sign
}

/// zigzag 的逆。
const fn unzigzag(v: u64) -> i64 {
    let half = v >> 1;
    let bits = if v & 1 == 1 { !half } else { half };
    i64::from_ne_bytes(bits.to_ne_bytes())
}

fn put_signed(out: &mut Vec<u8>, v: i64) {
    put_varint(out, zigzag(v));
}

/// 写一个“可能不知道”的数：0 表示不知道，其他是值加 1。
fn put_opt(out: &mut Vec<u8>, v: Option<u64>) {
    put_varint(out, v.map_or(0, |v| v.saturating_add(1)));
}

fn put_text(out: &mut Vec<u8>, text: &str) {
    put_varint(out, u64::try_from(text.len()).unwrap_or(u64::MAX));
    out.extend_from_slice(text.as_bytes());
}

/// 在字符边界处截断到 `max` 字节。
fn clip(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.get(..end).unwrap_or("")
}

/// 顺序读取；任何一步读不出来返回 `None`（调用方把它当“这条记录坏了”）。
struct Cur<'a> {
    buf: &'a [u8],
}

impl<'a> Cur<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    const fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.buf.len() < n {
            return None;
        }
        let (head, tail) = self.buf.split_at(n);
        self.buf = tail;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    fn varint(&mut self) -> Option<u64> {
        let mut out = 0u64;
        for i in 0..10u32 {
            let byte = self.u8()?;
            let low = u64::from(byte & 0x7f);
            // 第 10 个字节只剩 1 位有用，其余位非 0 说明超过了 64 位
            if i == 9 && low > 1 {
                return None;
            }
            out |= low << (7 * i);
            if byte & 0x80 == 0 {
                return Some(out);
            }
        }
        None
    }

    fn signed(&mut self) -> Option<i64> {
        self.varint().map(unzigzag)
    }

    fn opt(&mut self) -> Option<Option<u64>> {
        Some(match self.varint()? {
            0 => None,
            n => Some(n - 1),
        })
    }

    fn u32(&mut self) -> Option<u32> {
        u32::try_from(self.varint()?).ok()
    }

    fn u16(&mut self) -> Option<u16> {
        u16::try_from(self.varint()?).ok()
    }

    fn text(&mut self) -> Option<String> {
        let len = usize::try_from(self.varint()?).ok()?;
        let bytes = self.take(len)?;
        std::str::from_utf8(bytes).ok().map(str::to_owned)
    }
}

// ---------------------------------------------------------------------------
// 元数据（文件头）
// ---------------------------------------------------------------------------

/// 这份记录是哪一端写的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 被控端。
    Host,
    /// 控制端。
    Controller,
}

impl Role {
    const fn code(self) -> u8 {
        match self {
            Self::Host => 0,
            Self::Controller => 1,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Host),
            1 => Some(Self::Controller),
            _ => None,
        }
    }

    /// 文件名里用的名字。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Controller => "controller",
        }
    }
}

/// 文件头里的元数据：会话开始时写一次。除了角色和开始时间，其他项没有来源时为 `None`（不是“空字符串”）。
///
/// 不放：IP 地址和端口、设备码、证书指纹、账户号、电脑名、用户名（见设计 1.7 节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    /// 会话号（16 字节随机数，两端相同，只用来把两端的两份记录对上）。还没有来源时为 `None`。
    pub session_id: Option<[u8; 16]>,
    /// 角色。
    pub role: Role,
    /// 会话开始时的墙上时间（Unix 微秒），只给人看和粗对；仿真里没有墙上时间，为 0。
    pub wall_start_us: u64,
    /// 程序版本号。
    pub program_version: Option<String>,
    /// 程序提交号。
    pub commit: Option<String>,
    /// 本机系统。
    pub os: Option<PeerOs>,
    /// 编码格式，例如 `h264`、`tile`。
    pub codec: Option<String>,
    /// 码率估计器的名字。
    pub estimator: Option<String>,
    /// 内容策略的名字。
    pub policy: Option<String>,
    /// 帧率设定（帧每秒）。
    pub fps: Option<u32>,
    /// 起步码率（比特每秒）。
    pub start_bps: Option<u64>,
    /// 码率上限（比特每秒）。
    pub max_bps: Option<u64>,
    /// 握手协商出来的共同能力位。
    pub caps: Option<u32>,
}

impl Meta {
    /// 只有必须项的元数据。
    #[must_use]
    pub const fn new(role: Role, wall_start_us: u64) -> Self {
        Self {
            session_id: None,
            role,
            wall_start_us,
            program_version: None,
            commit: None,
            os: None,
            codec: None,
            estimator: None,
            policy: None,
            fps: None,
            start_bps: None,
            max_bps: None,
            caps: None,
        }
    }

    fn put_tlv(out: &mut Vec<u8>, tag: u64, value: &[u8]) {
        put_varint(out, tag);
        put_varint(out, u64::try_from(value.len()).unwrap_or(u64::MAX));
        out.extend_from_slice(value);
    }

    fn put_tlv_text(out: &mut Vec<u8>, tag: u64, value: &Option<String>) {
        if let Some(text) = value {
            Self::put_tlv(out, tag, clip(text, META_TEXT_MAX).as_bytes());
        }
    }

    fn put_tlv_number(out: &mut Vec<u8>, tag: u64, value: Option<u64>) {
        if let Some(v) = value {
            let mut buf = Vec::new();
            put_varint(&mut buf, v);
            Self::put_tlv(out, tag, &buf);
        }
    }

    /// 元数据的 TLV 字节。
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(id) = &self.session_id {
            Self::put_tlv(&mut out, 1, id);
        }
        Self::put_tlv(&mut out, 2, &[self.role.code()]);
        Self::put_tlv(&mut out, 3, &self.wall_start_us.to_be_bytes());
        Self::put_tlv_text(&mut out, 4, &self.program_version);
        Self::put_tlv_text(&mut out, 5, &self.commit);
        Self::put_tlv_number(&mut out, 6, self.os.map(|os| u64::from(os_code(os))));
        Self::put_tlv_text(&mut out, 7, &self.codec);
        Self::put_tlv_text(&mut out, 8, &self.estimator);
        Self::put_tlv_text(&mut out, 9, &self.policy);
        Self::put_tlv_number(&mut out, 10, self.fps.map(u64::from));
        Self::put_tlv_number(&mut out, 11, self.start_bps);
        Self::put_tlv_number(&mut out, 12, self.max_bps);
        Self::put_tlv_number(&mut out, 13, self.caps.map(u64::from));
        out
    }

    /// 从 TLV 字节读回。不认识的标签跳过；缺角色或开始时间、长度对不上都算坏。
    fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let bad = || RecordError::BadHeader;
        let mut cur = Cur::new(bytes);
        let (mut role, mut wall) = (None, None);
        let mut meta = Self::new(Role::Host, 0);
        while !cur.is_empty() {
            let tag = cur.varint().ok_or_else(bad)?;
            let len = usize::try_from(cur.varint().ok_or_else(bad)?).map_err(|_| bad())?;
            let mut value = Cur::new(cur.take(len).ok_or_else(bad)?);
            match tag {
                1 => {
                    let id = value.take(16).ok_or_else(bad)?;
                    let mut out = [0u8; 16];
                    out.copy_from_slice(id);
                    meta.session_id = Some(out);
                }
                2 => role = Some(Role::from_code(value.u8().ok_or_else(bad)?).ok_or_else(bad)?),
                3 => {
                    let raw: [u8; 8] = value
                        .take(8)
                        .and_then(|b| b.try_into().ok())
                        .ok_or_else(bad)?;
                    wall = Some(u64::from_be_bytes(raw));
                }
                4 => meta.program_version = Some(text_of(value.buf)?),
                5 => meta.commit = Some(text_of(value.buf)?),
                6 => {
                    let code = u8::try_from(value.varint().ok_or_else(bad)?).map_err(|_| bad())?;
                    meta.os = Some(os_from_code(code).ok_or_else(bad)?);
                }
                7 => meta.codec = Some(text_of(value.buf)?),
                8 => meta.estimator = Some(text_of(value.buf)?),
                9 => meta.policy = Some(text_of(value.buf)?),
                10 => meta.fps = Some(value.u32().ok_or_else(bad)?),
                11 => meta.start_bps = Some(value.varint().ok_or_else(bad)?),
                12 => meta.max_bps = Some(value.varint().ok_or_else(bad)?),
                13 => meta.caps = Some(value.u32().ok_or_else(bad)?),
                // 不认识的标签：按长度跳过（上面已经取走了）
                _ => {}
            }
        }
        meta.role = role.ok_or_else(bad)?;
        meta.wall_start_us = wall.ok_or_else(bad)?;
        Ok(meta)
    }
}

fn text_of(bytes: &[u8]) -> Result<String, RecordError> {
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| RecordError::BadHeader)
}

const fn os_code(os: PeerOs) -> u8 {
    match os {
        PeerOs::Unknown => 0,
        PeerOs::Windows => 1,
        PeerOs::Mac => 2,
        PeerOs::Linux => 3,
    }
}

const fn os_from_code(code: u8) -> Option<PeerOs> {
    match code {
        0 => Some(PeerOs::Unknown),
        1 => Some(PeerOs::Windows),
        2 => Some(PeerOs::Mac),
        3 => Some(PeerOs::Linux),
        _ => None,
    }
}

/// 文件头的字节。元数据最长的情况（13 项，字符串各截到 [`META_TEXT_MAX`]）也只有几百字节，装得进头长度的 u16。
#[must_use]
pub fn encode_header(meta: &Meta) -> Vec<u8> {
    let body = meta.encode();
    let len = u16::try_from(body.len()).unwrap_or(u16::MAX);
    let mut out = Vec::with_capacity(FIXED_HEAD + body.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&VERSION.to_be_bytes());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&body);
    out
}

// ---------------------------------------------------------------------------
// 记录
// ---------------------------------------------------------------------------

/// 记录的种类。数值是写进文件的种类号，不能改。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 被控端每处理一份反馈。
    Feedback,
    /// 被控端每个编码任务结束。
    Frame,
    /// 被控端每秒的重传汇总。
    Retransmit,
    /// 每秒的传输（QUIC）计数。
    Transport,
    /// 每秒的输入活跃度。
    Input,
    /// 缺口：队列满了丢记录。
    Gap,
    /// 结束。
    End,
}

impl Kind {
    /// 全部种类，顺序也是 [`Self::slot`] 的顺序。
    pub const ALL: [Self; 7] = [
        Self::Feedback,
        Self::Frame,
        Self::Retransmit,
        Self::Transport,
        Self::Input,
        Self::Gap,
        Self::End,
    ];

    /// 种类号。
    #[must_use]
    pub const fn code(self) -> u64 {
        match self {
            Self::Feedback => 1,
            Self::Frame => 2,
            Self::Retransmit => 3,
            Self::Transport => 4,
            Self::Input => 5,
            Self::Gap => 14,
            Self::End => 15,
        }
    }

    /// 由种类号得到种类；不认识的返回 `None`。
    #[must_use]
    pub const fn from_code(code: u64) -> Option<Self> {
        match code {
            1 => Some(Self::Feedback),
            2 => Some(Self::Frame),
            3 => Some(Self::Retransmit),
            4 => Some(Self::Transport),
            5 => Some(Self::Input),
            14 => Some(Self::Gap),
            15 => Some(Self::End),
            _ => None,
        }
    }

    /// 在 [`Self::ALL`] 里的位置（计数数组的下标）。
    #[must_use]
    pub const fn slot(self) -> usize {
        match self {
            Self::Feedback => 0,
            Self::Frame => 1,
            Self::Retransmit => 2,
            Self::Transport => 3,
            Self::Input => 4,
            Self::Gap => 5,
            Self::End => 6,
        }
    }

    /// 名字（日志用）。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Feedback => "反馈",
            Self::Frame => "帧",
            Self::Retransmit => "重传",
            Self::Transport => "传输",
            Self::Input => "输入",
            Self::Gap => "缺口",
            Self::End => "结束",
        }
    }
}

/// 发送量被谁限制：0 是应用受限，1 加上网络受限的原因号。
const fn limit_code(limit: SendLimit) -> u8 {
    match limit {
        SendLimit::App => 0,
        SendLimit::Network(cause) => {
            1 + match cause {
                NetCause::Unknown => 0,
                NetCause::Start => 1,
                NetCause::FrameBlocked => 2,
                NetCause::KeyPending => 3,
                NetCause::FrameNearBudget => 4,
                NetCause::FpsReduced => 5,
                NetCause::ScaleReduced => 6,
                NetCause::ResendCapped => 7,
                NetCause::EncoderDropped => 8,
                NetCause::UsingRate => 9,
            }
        }
    }
}

const fn limit_from_code(code: u8) -> Option<SendLimit> {
    Some(match code {
        0 => SendLimit::App,
        1 => SendLimit::Network(NetCause::Unknown),
        2 => SendLimit::Network(NetCause::Start),
        3 => SendLimit::Network(NetCause::FrameBlocked),
        4 => SendLimit::Network(NetCause::KeyPending),
        5 => SendLimit::Network(NetCause::FrameNearBudget),
        6 => SendLimit::Network(NetCause::FpsReduced),
        7 => SendLimit::Network(NetCause::ScaleReduced),
        8 => SendLimit::Network(NetCause::ResendCapped),
        9 => SendLimit::Network(NetCause::EncoderDropped),
        10 => SendLimit::Network(NetCause::UsingRate),
        _ => return None,
    })
}

fn state_from_code(code: u8) -> Option<RateState> {
    RateState::ALL.get(usize::from(code)).copied()
}

/// 反馈记录：被控端处理完一份反馈之后，发送端看到的和算出的。时间是处理的时刻。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feedback {
    /// 确认到的帧号：不大于它的帧都有了定论。
    pub floor: u32,
    /// 接收端报告的排队延迟（微秒）。
    pub queue_delay_us: u32,
    /// 丢包率（千分比）。
    pub loss_permille: u16,
    /// 接收速率（字节每秒）。
    pub recv_bytes_per_s: u32,
    /// 延迟趋势（0.1 毫秒）。
    pub trend_tenth_ms: i32,
    /// 包串速率（字节每秒），0 表示没测到。
    pub train_bytes_per_s: u32,
    /// 往返时间（微秒）。
    pub rtt_us: u64,
    /// 还没确认的字节数。
    pub in_flight_bytes: u64,
    /// 发送量被谁限制。
    pub limit: SendLimit,
    /// 处理完这份反馈之后的码率（比特每秒）。
    pub rate_bps: u64,
    /// 处理完之后码率控制的状态。
    pub state: RateState,
}

impl Feedback {
    /// 由发送端的一次观测、处理完之后的码率和状态构成。
    #[must_use]
    pub fn from_observation(obs: &Observation, rate_bps: u64, state: RateState) -> Self {
        Self {
            floor: obs.feedback.floor,
            queue_delay_us: obs.feedback.queue_delay_us,
            loss_permille: obs.feedback.loss_permille,
            recv_bytes_per_s: obs.feedback.recv_bytes_per_s,
            trend_tenth_ms: obs.ext.trend_tenth_ms,
            train_bytes_per_s: obs.ext.train_bytes_per_s,
            rtt_us: obs.rtt_us,
            in_flight_bytes: obs.in_flight,
            limit: obs.limit,
            rate_bps,
            state,
        }
    }
}

/// 一帧属于哪一类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameClass {
    /// 画面变了，编变化。
    Content,
    /// 画面没变，补清晰。
    Refine,
    /// 关键帧。
    Key,
}

impl FrameClass {
    /// 由“出的是不是关键帧”和“交任务时画面变没变”得到类别。
    #[must_use]
    pub const fn of(key: bool, changed: bool) -> Self {
        if key {
            Self::Key
        } else if changed {
            Self::Content
        } else {
            Self::Refine
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Content => 0,
            Self::Refine => 1,
            Self::Key => 2,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Content),
            1 => Some(Self::Refine),
            2 => Some(Self::Key),
            _ => None,
        }
    }
}

/// 编码任务各段的耗时（微秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// 整个编码任务用的时间。
    pub took_us: u64,
    /// 缩放。
    pub scale_us: u64,
    /// 颜色转换（含等显卡读回的时间）。
    pub convert_us: u64,
    /// 其中在等显卡的时间（已包含在 `convert_us` 里）。
    pub wait_us: u64,
    /// 编码器本身。
    pub encode_us: u64,
}

/// 帧记录：一个编码任务结束。时间是任务结束（结果交回会话循环）的时刻。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// 帧号。没出帧时（`bytes` 为 0）是这个任务占用、下一个任务还会用的编号。
    pub frame_id: u32,
    /// 采集时间在记录时间之前多少微秒；不知道时为 `None`。
    pub captured_ago_us: Option<u64>,
    /// 类别。
    pub class: FrameClass,
    /// 关键帧的原因（编码器给的原文，最多 [`KEY_REASON_MAX`] 字节）；不是关键帧或没给原因时为空。
    pub key_reason: Cow<'static, str>,
    /// 这一帧的字节预算；不知道时为 `None`。
    pub budget_bytes: Option<u32>,
    /// 编出的字节数（码流，不含分片头和纠错分片）；0 表示这个任务没出帧。
    pub bytes: u32,
    /// 编码尺寸占源画面的比例（千分比）；不知道时为 `None`。
    pub scale_permille: Option<u16>,
    /// 编码尺寸（宽，高，像素）；不知道时为 `None`。
    pub size: Option<(u16, u16)>,
    /// 各段耗时；不知道时为 `None`。
    pub timing: Option<Timing>,
    /// 用的是硬件编码器。
    pub hardware: bool,
    /// 编码器因为预算放不下把这一帧丢了。
    pub dropped: bool,
}

/// 重传记录：每秒一条，这一秒（或最后不足一秒）里的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retransmit {
    /// 收到的重传请求，按分片数。
    pub requested: u32,
    /// 实际重传的分片数。
    pub resent: u32,
    /// 因为这一轮重传额度用完而没有重传的分片数。
    pub capped: u32,
    /// 判定丢失、让编码器恢复的帧数。
    pub lost_frames: u32,
}

/// 传输记录：每秒一条，QUIC 在这一秒里的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Transport {
    /// 应用交给 QUIC 的数据报数。
    pub app_sent: u64,
    /// QUIC 实际发出的 DATAGRAM 帧数（比 `app_sent` 少的部分是发送缓冲满时被挤掉的）。
    pub frames_sent: u64,
    /// 超过路径 MTU 没发出去的数据报数。
    pub too_large: u64,
    /// QUIC 判定在网络上丢了的包数。
    pub lost_packets: u64,
    /// 拥塞控制的拥塞事件次数。
    pub congestion_events: u64,
    /// 这一秒结束时的拥塞窗口（字节，不是差值）。
    pub cwnd_bytes: u64,
    /// 这一秒结束时的路径 MTU（字节，不是差值）。
    pub mtu: u16,
}

/// 输入活跃度：这一秒收到的输入事件数，只分三档，不记按了什么键、鼠标在哪。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputLevel {
    /// 0 个。
    None,
    /// 1 到 5 个。
    Few,
    /// 6 个以上。
    Many,
}

impl InputLevel {
    /// 由事件数分档。
    #[must_use]
    pub const fn of(count: u64) -> Self {
        match count {
            0 => Self::None,
            1..=5 => Self::Few,
            _ => Self::Many,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Few => 1,
            Self::Many => 2,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::None),
            1 => Some(Self::Few),
            2 => Some(Self::Many),
            _ => None,
        }
    }
}

/// 某个种类的条数：种类号和数目（读的一方可能遇到自己不认识的种类号，所以存号不存 [`Kind`]）。
pub type KindCount = (u64, u64);

/// 缺口记录：记录线程的队列满过，丢了一些记录。时间是最后一条被丢的记录的时间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// 缺口从最早一条被丢的记录到最后一条，跨了多久（微秒）。
    pub span_us: u64,
    /// 各种类丢了几条。
    pub dropped: Vec<KindCount>,
}

/// 会话怎么结束的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// 正常结束：一方发了结束消息，或者画面来源结束。
    Normal,
    /// 对端因为退出程序而关闭了连接。
    PeerQuit,
    /// 网络断了（超时、重置、传输错误）。
    LinkLost,
    /// 本端出错结束。
    Failed,
}

impl EndReason {
    const fn code(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::PeerQuit => 1,
            Self::LinkLost => 2,
            Self::Failed => 3,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Normal),
            1 => Some(Self::PeerQuit),
            2 => Some(Self::LinkLost),
            3 => Some(Self::Failed),
            _ => None,
        }
    }
}

/// 一个种类的条数账：写进文件多少条，被丢了多少条。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindTally {
    /// 种类号。
    pub kind: u64,
    /// 写进文件的条数。
    pub written: u64,
    /// 因为队列满被丢的条数。
    pub dropped: u64,
}

/// 结束记录：时间是会话结束的时刻。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    /// 怎么结束的。
    pub reason: EndReason,
    /// 会话从开始到结束多久（微秒）。
    pub duration_us: u64,
    /// 各种类的条数账（只列写过或丢过的种类，不含结束记录自己）。
    pub tallies: Vec<KindTally>,
}

/// 一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// 反馈。
    Feedback(Feedback),
    /// 帧。
    Frame(Frame),
    /// 重传。
    Retransmit(Retransmit),
    /// 传输。
    Transport(Transport),
    /// 输入。
    Input(InputLevel),
    /// 缺口。
    Gap(Gap),
    /// 结束。
    End(End),
}

/// 块内的差值状态：每个块开头清零重来，写和读各一份，规则相同。
#[derive(Debug, Clone)]
struct Deltas {
    /// 每个种类上一条记录的时间。
    last_at: [u64; Kind::ALL.len()],
    /// 上一条帧记录的帧号。
    frame_id: u32,
    /// 上一条反馈记录的确认帧号。
    floor: u32,
}

impl Deltas {
    const fn new(block_start_us: u64) -> Self {
        Self {
            last_at: [block_start_us; Kind::ALL.len()],
            frame_id: 0,
            floor: 0,
        }
    }
}

fn put_delta(out: &mut Vec<u8>, now: u32, last: &mut u32) {
    put_signed(out, i64::from(now) - i64::from(*last));
    *last = now;
}

fn get_delta(cur: &mut Cur<'_>, last: &mut u32) -> Option<u32> {
    let now = i64::from(*last).checked_add(cur.signed()?)?;
    let now = u32::try_from(now).ok()?;
    *last = now;
    Some(now)
}

impl Record {
    /// 种类。
    #[must_use]
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Feedback(_) => Kind::Feedback,
            Self::Frame(_) => Kind::Frame,
            Self::Retransmit(_) => Kind::Retransmit,
            Self::Transport(_) => Kind::Transport,
            Self::Input(_) => Kind::Input,
            Self::Gap(_) => Kind::Gap,
            Self::End(_) => Kind::End,
        }
    }

    /// 写字段（不含时间差）。
    fn put(&self, st: &mut Deltas, out: &mut Vec<u8>) {
        match self {
            Self::Feedback(f) => {
                put_delta(out, f.floor, &mut st.floor);
                put_varint(out, u64::from(f.queue_delay_us));
                put_varint(out, u64::from(f.loss_permille));
                put_varint(out, u64::from(f.recv_bytes_per_s));
                put_signed(out, i64::from(f.trend_tenth_ms));
                put_varint(out, u64::from(f.train_bytes_per_s));
                put_varint(out, f.rtt_us);
                put_varint(out, f.in_flight_bytes);
                out.push(limit_code(f.limit));
                put_varint(out, f.rate_bps);
                out.push(u8::try_from(f.state.index()).unwrap_or(u8::MAX));
            }
            Self::Frame(f) => {
                put_delta(out, f.frame_id, &mut st.frame_id);
                put_opt(out, f.captured_ago_us);
                out.push(f.class.code());
                put_text(out, clip(&f.key_reason, KEY_REASON_MAX));
                put_opt(out, f.budget_bytes.map(u64::from));
                put_varint(out, u64::from(f.bytes));
                put_opt(out, f.scale_permille.map(u64::from));
                put_opt(out, f.size.map(|(w, _)| u64::from(w)));
                put_opt(out, f.size.map(|(_, h)| u64::from(h)));
                match f.timing {
                    Some(t) => {
                        out.push(1);
                        for v in [t.took_us, t.scale_us, t.convert_us, t.wait_us, t.encode_us] {
                            put_varint(out, v);
                        }
                    }
                    None => out.push(0),
                }
                out.push(u8::from(f.hardware) | (u8::from(f.dropped) << 1));
            }
            Self::Retransmit(r) => {
                for v in [r.requested, r.resent, r.capped, r.lost_frames] {
                    put_varint(out, u64::from(v));
                }
            }
            Self::Transport(t) => {
                for v in [
                    t.app_sent,
                    t.frames_sent,
                    t.too_large,
                    t.lost_packets,
                    t.congestion_events,
                    t.cwnd_bytes,
                    u64::from(t.mtu),
                ] {
                    put_varint(out, v);
                }
            }
            Self::Input(level) => out.push(level.code()),
            Self::Gap(g) => {
                put_varint(out, g.span_us);
                put_varint(out, u64::try_from(g.dropped.len()).unwrap_or(u64::MAX));
                for (kind, count) in &g.dropped {
                    put_varint(out, *kind);
                    put_varint(out, *count);
                }
            }
            Self::End(e) => {
                out.push(e.reason.code());
                put_varint(out, e.duration_us);
                put_varint(out, u64::try_from(e.tallies.len()).unwrap_or(u64::MAX));
                for t in &e.tallies {
                    put_varint(out, t.kind);
                    put_varint(out, t.written);
                    put_varint(out, t.dropped);
                }
            }
        }
    }

    /// 读字段。多出来的尾部字节忽略；不够读或者值不合法返回 `None`。
    fn get(kind: Kind, st: &mut Deltas, cur: &mut Cur<'_>) -> Option<Self> {
        Some(match kind {
            Kind::Feedback => Self::Feedback(Feedback {
                floor: get_delta(cur, &mut st.floor)?,
                queue_delay_us: cur.u32()?,
                loss_permille: cur.u16()?,
                recv_bytes_per_s: cur.u32()?,
                trend_tenth_ms: i32::try_from(cur.signed()?).ok()?,
                train_bytes_per_s: cur.u32()?,
                rtt_us: cur.varint()?,
                in_flight_bytes: cur.varint()?,
                limit: limit_from_code(cur.u8()?)?,
                rate_bps: cur.varint()?,
                state: state_from_code(cur.u8()?)?,
            }),
            Kind::Frame => {
                let frame_id = get_delta(cur, &mut st.frame_id)?;
                let captured_ago_us = cur.opt()?;
                let class = FrameClass::from_code(cur.u8()?)?;
                let key_reason = Cow::Owned(cur.text()?);
                let budget_bytes = cur.opt()?.map(u32::try_from).transpose().ok()?;
                let bytes = cur.u32()?;
                let scale_permille = cur.opt()?.map(u16::try_from).transpose().ok()?;
                let width = cur.opt()?.map(u16::try_from).transpose().ok()?;
                let height = cur.opt()?.map(u16::try_from).transpose().ok()?;
                let size = match (width, height) {
                    (Some(w), Some(h)) => Some((w, h)),
                    (None, None) => None,
                    (Some(_), None) | (None, Some(_)) => return None,
                };
                let timing = match cur.u8()? {
                    0 => None,
                    1 => Some(Timing {
                        took_us: cur.varint()?,
                        scale_us: cur.varint()?,
                        convert_us: cur.varint()?,
                        wait_us: cur.varint()?,
                        encode_us: cur.varint()?,
                    }),
                    _ => return None,
                };
                let flags = cur.u8()?;
                Self::Frame(Frame {
                    frame_id,
                    captured_ago_us,
                    class,
                    key_reason,
                    budget_bytes,
                    bytes,
                    scale_permille,
                    size,
                    timing,
                    hardware: flags & 1 != 0,
                    dropped: flags & 2 != 0,
                })
            }
            Kind::Retransmit => Self::Retransmit(Retransmit {
                requested: cur.u32()?,
                resent: cur.u32()?,
                capped: cur.u32()?,
                lost_frames: cur.u32()?,
            }),
            Kind::Transport => Self::Transport(Transport {
                app_sent: cur.varint()?,
                frames_sent: cur.varint()?,
                too_large: cur.varint()?,
                lost_packets: cur.varint()?,
                congestion_events: cur.varint()?,
                cwnd_bytes: cur.varint()?,
                mtu: cur.u16()?,
            }),
            Kind::Input => Self::Input(InputLevel::from_code(cur.u8()?)?),
            Kind::Gap => {
                let span_us = cur.varint()?;
                let n = cur.varint()?;
                let mut dropped = Vec::new();
                for _ in 0..n {
                    dropped.push((cur.varint()?, cur.varint()?));
                }
                Self::Gap(Gap { span_us, dropped })
            }
            Kind::End => {
                let reason = EndReason::from_code(cur.u8()?)?;
                let duration_us = cur.varint()?;
                let n = cur.varint()?;
                let mut tallies = Vec::new();
                for _ in 0..n {
                    tallies.push(KindTally {
                        kind: cur.varint()?,
                        written: cur.varint()?,
                        dropped: cur.varint()?,
                    });
                }
                Self::End(End {
                    reason,
                    duration_us,
                    tallies,
                })
            }
        })
    }
}

// ---------------------------------------------------------------------------
// 写
// ---------------------------------------------------------------------------

/// 把记录攒成块。不做 IO：封好的块追加到调用方给的 `Vec<u8>` 里，由调用方写文件或留在内存。
///
/// 封块时机：块里的第一条和新来的一条差到 [`BLOCK_SPAN_US`]，或者块内容到了 [`BLOCK_SOFT_BYTES`]；
/// 调用方也可以随时 [`Self::flush`]（例如记录线程每秒醒来一次）。
#[derive(Debug)]
pub struct BlockWriter {
    /// 当前块里已经写好的记录。
    body: Vec<u8>,
    /// 当前块的起始时间；没有打开的块时为 `None`。
    start_us: Option<u64>,
    state: Deltas,
    /// 记录内容的临时区，重复使用。
    scratch: Vec<u8>,
    written: [u64; Kind::ALL.len()],
    blocks: u64,
}

impl Default for BlockWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl BlockWriter {
    /// 新的写入器。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            body: Vec::new(),
            start_us: None,
            state: Deltas::new(0),
            scratch: Vec::new(),
            written: [0; Kind::ALL.len()],
            blocks: 0,
        }
    }

    /// 当前有没有打开的块（里面有记录等着封）。
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.start_us.is_some()
    }

    /// 各种类已经交给写入器的条数，下标是 [`Kind::slot`]。
    #[must_use]
    pub const fn written(&self) -> &[u64; Kind::ALL.len()] {
        &self.written
    }

    /// 已经封好的块数。
    #[must_use]
    pub const fn blocks(&self) -> u64 {
        self.blocks
    }

    /// 如果当前块到了封块的时候（相对时间 `at_us`），封掉它。
    pub fn seal_if_due(&mut self, at_us: u64, out: &mut Vec<u8>) {
        let due = self
            .start_us
            .is_some_and(|start| at_us.saturating_sub(start) >= BLOCK_SPAN_US)
            || self.body.len() >= BLOCK_SOFT_BYTES;
        if due {
            self.flush(out);
        }
    }

    /// 打开一个新块，起始时间 `start_us`。已经有打开的块时什么也不做。
    /// 之后写入的记录时间都不应早于它（早的按差 0 写）。
    pub fn open(&mut self, start_us: u64) {
        if self.start_us.is_none() {
            self.start_us = Some(start_us);
            self.state = Deltas::new(start_us);
        }
    }

    /// 写一条记录，时间是 `at_us`。需要时先封上一块、再开新块。
    pub fn push(&mut self, at_us: u64, record: &Record, out: &mut Vec<u8>) {
        self.seal_if_due(at_us, out);
        self.open(at_us);
        let slot = record.kind().slot();
        let last = self.state.last_at.get(slot).copied().unwrap_or(at_us);
        let at = at_us.max(last);
        self.scratch.clear();
        put_varint(&mut self.scratch, at - last);
        record.put(&mut self.state, &mut self.scratch);
        if let Some(slot_at) = self.state.last_at.get_mut(slot) {
            *slot_at = at;
        }
        put_varint(&mut self.body, record.kind().code());
        put_varint(
            &mut self.body,
            u64::try_from(self.scratch.len()).unwrap_or(u64::MAX),
        );
        self.body.extend_from_slice(&self.scratch);
        if let Some(count) = self.written.get_mut(slot) {
            *count = count.saturating_add(1);
        }
    }

    /// 封掉当前块（没有打开的块时什么也不做）。
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        let Some(start) = self.start_us.take() else {
            return;
        };
        let mut content = Vec::with_capacity(self.body.len() + 10);
        put_varint(&mut content, start);
        content.extend_from_slice(&self.body);
        self.body.clear();
        // 块内容不可能超过 4 GiB：一条记录最多几百字节，攒到 64 KiB 就封
        let len = u32::try_from(content.len()).unwrap_or(u32::MAX);
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(&crc32fast::hash(&content).to_be_bytes());
        out.extend_from_slice(&content);
        self.blocks = self.blocks.saturating_add(1);
    }
}

// ---------------------------------------------------------------------------
// 缺口和结束：写入器之上的一层
// ---------------------------------------------------------------------------

/// 队列满过：记录线程取走的这一段时间里被丢的记录。下标是 [`Kind::slot`]。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GapInfo {
    /// 最早一条被丢的记录的时间（微秒）。
    pub first_us: u64,
    /// 最后一条被丢的记录的时间（微秒）。
    pub last_us: u64,
    /// 各种类丢了几条。
    pub dropped: [u64; Kind::ALL.len()],
}

/// 在 [`BlockWriter`] 之上管“缺口”和“结束”两种记录：
/// 队列满过的话，下一个块开头写一条缺口记录；结束时写结束记录（带各种类写了多少、丢了多少）。
/// 内存里的仿真和写文件的记录线程用同一份逻辑，区别只是缺口从哪来（仿真没有队列，永远没有缺口）。
#[derive(Debug, Default)]
pub struct Sink {
    writer: BlockWriter,
    /// 已经写进缺口记录的丢失条数，下标是 [`Kind::slot`]。
    dropped: [u64; Kind::ALL.len()],
}

impl Sink {
    /// 新的写入层。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            writer: BlockWriter::new(),
            dropped: [0; Kind::ALL.len()],
        }
    }

    /// 写一条记录。要开新块时调用 `take_gap` 取队列满过的情况（取走即清零，所以只在真的要写缺口记录时才调用）。
    pub fn push(
        &mut self,
        at_us: u64,
        record: &Record,
        take_gap: impl FnOnce() -> Option<GapInfo>,
        out: &mut Vec<u8>,
    ) {
        self.writer.seal_if_due(at_us, out);
        if !self.writer.is_open() {
            if let Some(gap) = take_gap() {
                self.write_gap(at_us, &gap, out);
            }
        }
        self.writer.push(at_us, record, out);
    }

    fn write_gap(&mut self, at_us: u64, gap: &GapInfo, out: &mut Vec<u8>) {
        // 块的起始时间取这一段里最早的：缺口记录的时间（最后一条被丢的）和后面的记录都不早于它
        self.writer.open(at_us.min(gap.first_us));
        let dropped = Kind::ALL
            .iter()
            .zip(gap.dropped)
            .filter(|(_, n)| *n > 0)
            .map(|(kind, n)| (kind.code(), n))
            .collect();
        for (total, n) in self.dropped.iter_mut().zip(gap.dropped) {
            *total = total.saturating_add(n);
        }
        let record = Record::Gap(Gap {
            span_us: gap.last_us.saturating_sub(gap.first_us),
            dropped,
        });
        self.writer.push(gap.last_us, &record, out);
    }

    /// 封掉当前块。
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        self.writer.flush(out);
    }

    /// 会话结束：先写还没写的缺口，再写结束记录，封块。`at_us` 是结束的时间，`duration_us` 是会话时长。
    pub fn finish(
        &mut self,
        at_us: u64,
        reason: EndReason,
        duration_us: u64,
        last_gap: Option<GapInfo>,
        out: &mut Vec<u8>,
    ) {
        self.writer.seal_if_due(at_us, out);
        if let Some(gap) = last_gap {
            self.write_gap(at_us, &gap, out);
        }
        let tallies = Kind::ALL
            .iter()
            .zip(self.writer.written())
            .zip(self.dropped)
            .filter(|((kind, written), dropped)| {
                **kind != Kind::End && (**written > 0 || *dropped > 0)
            })
            .map(|((kind, written), dropped)| KindTally {
                kind: kind.code(),
                written: *written,
                dropped,
            })
            .collect();
        let end = Record::End(End {
            reason,
            duration_us,
            tallies,
        });
        self.writer.push(at_us, &end, out);
        self.writer.flush(out);
    }

    /// 各种类交给写入器的条数，下标是 [`Kind::slot`]。
    #[must_use]
    pub const fn written(&self) -> &[u64; Kind::ALL.len()] {
        self.writer.written()
    }

    /// 各种类已经写进缺口记录的丢失条数，下标是 [`Kind::slot`]。
    #[must_use]
    pub const fn dropped(&self) -> &[u64; Kind::ALL.len()] {
        &self.dropped
    }

    /// 已经封好的块数。
    #[must_use]
    pub const fn blocks(&self) -> u64 {
        self.writer.blocks()
    }
}

// ---------------------------------------------------------------------------
// 读
// ---------------------------------------------------------------------------

/// 读的时候遇到的损坏和不认识的内容，全部计数，没有一项是静默的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReadStats {
    /// 读成功的块数。
    pub blocks: u64,
    /// 坏了被跳过的块数（CRC 对不上，或者长度字段坏了）。
    pub bad_blocks: u64,
    /// 因为坏块和截断而没读的字节数。
    pub skipped_bytes: u64,
    /// 文件尾部有不完整的一块，被丢掉了。
    pub truncated_tail: bool,
    /// CRC 通过的块里解不出来的记录数（缺字段、值不合法）；这一块里它后面的记录跳过。
    pub bad_records: u64,
    /// 种类号不认识、按长度跳过的记录数。
    pub unknown_records: u64,
}

/// 读出来的一条记录和它的时间。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// 时间（微秒，本端单调时钟，从会话开始算）。
    pub at_us: u64,
    /// 记录。
    pub record: Record,
}

/// 读回来的一份记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    /// 文件头里的元数据。
    pub meta: Meta,
    /// 记录，按它们在文件里的顺序（不同种类的时间不保证单调）。
    pub entries: Vec<Entry>,
    /// 损坏情况。
    pub stats: ReadStats,
}

/// 在 `pos` 处试着读一个块。
enum Probe {
    /// 块完整、CRC 对：内容的范围，和下一个块的位置。
    Good {
        content: (usize, usize),
        next: usize,
    },
    /// 剩下的字节装不下这个块（块头不完整，或长度字段指到了文件外）。
    Incomplete,
    /// 装得下，但长度字段不合理或 CRC 对不上。
    Bad,
}

fn probe(bytes: &[u8], pos: usize) -> Probe {
    let Some(head) = bytes.get(pos..pos.saturating_add(BLOCK_HEAD)) else {
        return Probe::Incomplete;
    };
    let (len_bytes, crc_bytes) = head.split_at(4);
    let len = u32::from_be_bytes(len_bytes.try_into().unwrap_or([0; 4]));
    let crc = u32::from_be_bytes(crc_bytes.try_into().unwrap_or([0; 4]));
    let Ok(len) = usize::try_from(len) else {
        return Probe::Bad;
    };
    if len == 0 || len > MAX_BLOCK_BYTES {
        return Probe::Bad;
    }
    let start = pos + BLOCK_HEAD;
    let Some(content) = bytes.get(start..start.saturating_add(len)) else {
        return Probe::Incomplete;
    };
    if crc32fast::hash(content) == crc {
        Probe::Good {
            content: (start, start + len),
            next: start + len,
        }
    } else {
        Probe::Bad
    }
}

impl Recording {
    /// 读一整份记录。文件头不对返回错误；之后的损坏都不报错，计入 [`ReadStats`]。
    pub fn parse(bytes: &[u8]) -> Result<Self, RecordError> {
        let fixed = bytes.get(..FIXED_HEAD).ok_or(RecordError::NotARecording)?;
        if fixed.get(..4) != Some(&MAGIC[..]) {
            return Err(RecordError::NotARecording);
        }
        let version = u16::from_be_bytes(
            fixed
                .get(4..6)
                .and_then(|b| b.try_into().ok())
                .ok_or(RecordError::NotARecording)?,
        );
        if version > VERSION {
            return Err(RecordError::UnsupportedVersion(version));
        }
        let meta_len = usize::from(u16::from_be_bytes(
            fixed
                .get(6..8)
                .and_then(|b| b.try_into().ok())
                .ok_or(RecordError::NotARecording)?,
        ));
        let meta_bytes = bytes
            .get(FIXED_HEAD..FIXED_HEAD + meta_len)
            .ok_or(RecordError::NotARecording)?;
        let meta = Meta::decode(meta_bytes)?;

        let mut rec = Self {
            meta,
            entries: Vec::new(),
            stats: ReadStats::default(),
        };
        let mut pos = FIXED_HEAD + meta_len;
        while pos < bytes.len() {
            match probe(bytes, pos) {
                Probe::Good { content, next } => {
                    if let Some(body) = bytes.get(content.0..content.1) {
                        rec.read_block(body);
                    }
                    pos = next;
                }
                first @ (Probe::Incomplete | Probe::Bad) => {
                    pos = rec.resync(bytes, pos, matches!(first, Probe::Incomplete));
                }
            }
        }
        Ok(rec)
    }

    /// `pos` 处的块读不出来：往后逐字节找下一个读得出来的块，计数，返回接着读的位置。
    /// 找不到：尾部不完整（`incomplete`）算截断，否则算一个坏块；都读到文件尾。
    fn resync(&mut self, bytes: &[u8], pos: usize, incomplete: bool) -> usize {
        let mut q = pos + 1;
        while q < bytes.len() {
            if matches!(probe(bytes, q), Probe::Good { .. }) {
                self.stats.bad_blocks += 1;
                self.stats.skipped_bytes += u64::try_from(q - pos).unwrap_or(u64::MAX);
                return q;
            }
            q += 1;
        }
        self.stats.skipped_bytes += u64::try_from(bytes.len() - pos).unwrap_or(u64::MAX);
        if incomplete {
            self.stats.truncated_tail = true;
        } else {
            self.stats.bad_blocks += 1;
        }
        bytes.len()
    }

    /// 读一个 CRC 通过的块的内容。
    fn read_block(&mut self, body: &[u8]) {
        let mut cur = Cur::new(body);
        let Some(start) = cur.varint() else {
            self.stats.bad_records += 1;
            return;
        };
        self.stats.blocks += 1;
        let mut st = Deltas::new(start);
        while !cur.is_empty() {
            let (Some(code), Some(len)) = (cur.varint(), cur.varint()) else {
                self.stats.bad_records += 1;
                return;
            };
            let Some(content) = usize::try_from(len).ok().and_then(|n| cur.take(n)) else {
                self.stats.bad_records += 1;
                return;
            };
            let Some(kind) = Kind::from_code(code) else {
                self.stats.unknown_records += 1;
                continue;
            };
            let mut fields = Cur::new(content);
            let decoded = fields.varint().and_then(|delta| {
                let last = st.last_at.get(kind.slot()).copied()?;
                let at = last.checked_add(delta)?;
                let record = Record::get(kind, &mut st, &mut fields)?;
                *st.last_at.get_mut(kind.slot())? = at;
                Some(Entry { at_us: at, record })
            });
            match decoded {
                Some(entry) => self.entries.push(entry),
                None => {
                    self.stats.bad_records += 1;
                    return;
                }
            }
        }
    }

    /// 结束记录；没有说明会话不是正常收尾的（崩溃或被杀）。
    #[must_use]
    pub fn end(&self) -> Option<&End> {
        self.entries.iter().find_map(|e| match &e.record {
            Record::End(end) => Some(end),
            Record::Feedback(_)
            | Record::Frame(_)
            | Record::Retransmit(_)
            | Record::Transport(_)
            | Record::Input(_)
            | Record::Gap(_) => None,
        })
    }

    /// 缺口记录（队列满过的地方）：时间和内容。
    pub fn gaps(&self) -> impl Iterator<Item = (u64, &Gap)> {
        self.entries.iter().filter_map(|e| match &e.record {
            Record::Gap(gap) => Some((e.at_us, gap)),
            Record::Feedback(_)
            | Record::Frame(_)
            | Record::Retransmit(_)
            | Record::Transport(_)
            | Record::Input(_)
            | Record::End(_) => None,
        })
    }

    /// 反馈记录：时间和内容。
    pub fn feedbacks(&self) -> impl Iterator<Item = (u64, &Feedback)> {
        self.entries.iter().filter_map(|e| match &e.record {
            Record::Feedback(f) => Some((e.at_us, f)),
            Record::Frame(_)
            | Record::Retransmit(_)
            | Record::Transport(_)
            | Record::Input(_)
            | Record::Gap(_)
            | Record::End(_) => None,
        })
    }

    /// 帧记录：时间和内容。
    pub fn frames(&self) -> impl Iterator<Item = (u64, &Frame)> {
        self.entries.iter().filter_map(|e| match &e.record {
            Record::Frame(f) => Some((e.at_us, f)),
            Record::Feedback(_)
            | Record::Retransmit(_)
            | Record::Transport(_)
            | Record::Input(_)
            | Record::Gap(_)
            | Record::End(_) => None,
        })
    }

    /// 重传记录：时间和内容。
    pub fn retransmits(&self) -> impl Iterator<Item = (u64, &Retransmit)> {
        self.entries.iter().filter_map(|e| match &e.record {
            Record::Retransmit(r) => Some((e.at_us, r)),
            Record::Feedback(_)
            | Record::Frame(_)
            | Record::Transport(_)
            | Record::Input(_)
            | Record::Gap(_)
            | Record::End(_) => None,
        })
    }

    /// 某个种类读出来多少条。
    #[must_use]
    pub fn count(&self, kind: Kind) -> usize {
        self.entries
            .iter()
            .filter(|e| e.record.kind() == kind)
            .count()
    }
}

// ---------------------------------------------------------------------------
// 文件名
// ---------------------------------------------------------------------------

/// 公历日期和时间（UTC）：Howard Hinnant 的 `civil_from_days`。
fn civil(unix_secs: u64) -> (u64, u64, u64, u64, u64, u64) {
    let days = unix_secs / 86_400;
    let rest = unix_secs % 86_400;
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day, rest / 3600, rest % 3600 / 60, rest % 60)
}

/// 记录文件的正式文件名：`<开始时间>-<会话号前 8 位>-<角色>.pdsr`，例如
/// `20261127T103045Z-1a2b3c4d-host.pdsr`。开始时间是 UTC，写成固定宽度，按字典序排就是按时间排。
/// 会话号还没有来源时，那一段写 `00000000`。
#[must_use]
pub fn file_name(meta: &Meta) -> String {
    let (y, mo, d, h, mi, s) = civil(meta.wall_start_us / 1_000_000);
    let id: String = meta.session_id.map_or_else(
        || "00000000".to_owned(),
        |id| id.iter().take(4).map(|b| format!("{b:02x}")).collect(),
    );
    format!(
        "{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z-{id}-{}.{EXTENSION}",
        meta.role.name()
    )
}

/// 写的过程中用的文件名：正式文件名加 [`PART_SUFFIX`]。正常结束后改成正式文件名。
#[must_use]
pub fn part_name(final_name: &str) -> String {
    format!("{final_name}{PART_SUFFIX}")
}

/// 一个目录项是不是记录文件，是的话是正常结束的还是遗留的 `.part`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// 正常结束，正式文件名。
    Finished,
    /// 还带 `.part`：正在写，或者上次进程没正常结束留下的。
    Part,
}

/// 按文件名判断；不是记录文件返回 `None`。
#[must_use]
pub fn classify(name: &str) -> Option<FileState> {
    let suffix = format!(".{EXTENSION}");
    if name
        .strip_suffix(&suffix)
        .is_some_and(|stem| !stem.is_empty())
    {
        Some(FileState::Finished)
    } else if name
        .strip_suffix(PART_SUFFIX)
        .and_then(|n| n.strip_suffix(&suffix))
        .is_some_and(|stem| !stem.is_empty())
    {
        Some(FileState::Part)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::adaptive::Feedback as Fb;
    use crate::estimator::Extension;

    fn feedback(n: u32) -> Record {
        Record::Feedback(Feedback {
            floor: n,
            queue_delay_us: n.saturating_mul(1_234),
            loss_permille: u16::try_from(n % 1000).unwrap(),
            recv_bytes_per_s: 987_654,
            trend_tenth_ms: i32::try_from(n).unwrap_or(i32::MAX).saturating_mul(-12),
            train_bytes_per_s: 5_000_000,
            rtt_us: 23_456,
            in_flight_bytes: 70_000,
            limit: SendLimit::Network(NetCause::FrameNearBudget),
            rate_bps: 3_500_000,
            state: RateState::Drain,
        })
    }

    fn frame(id: u32) -> Record {
        Record::Frame(Frame {
            frame_id: id,
            captured_ago_us: Some(4_200),
            class: FrameClass::Key,
            key_reason: Cow::Borrowed("有帧丢了（接收端解不了后面的帧）"),
            budget_bytes: Some(40_000),
            bytes: 38_123,
            scale_permille: Some(750),
            size: Some((1920, 1080)),
            timing: Some(Timing {
                took_us: 14_000,
                scale_us: 1_000,
                convert_us: 3_000,
                wait_us: 500,
                encode_us: 9_000,
            }),
            hardware: true,
            dropped: false,
        })
    }

    fn bare_frame(id: u32) -> Record {
        Record::Frame(bare(id))
    }

    fn bare(id: u32) -> Frame {
        Frame {
            frame_id: id,
            captured_ago_us: None,
            class: FrameClass::Refine,
            key_reason: Cow::Borrowed(""),
            budget_bytes: None,
            bytes: 0,
            scale_permille: None,
            size: None,
            timing: None,
            hardware: false,
            dropped: true,
        }
    }

    fn every_record() -> Vec<(u64, Record)> {
        vec![
            (1_000, feedback(1)),
            (1_500, frame(1)),
            (2_000, bare_frame(2)),
            (
                10_000,
                Record::Retransmit(Retransmit {
                    requested: 5,
                    resent: 4,
                    capped: 1,
                    lost_frames: 2,
                }),
            ),
            (
                10_001,
                Record::Transport(Transport {
                    app_sent: 900,
                    frames_sent: 890,
                    too_large: 1,
                    lost_packets: 3,
                    congestion_events: 1,
                    cwnd_bytes: 120_000,
                    mtu: 1_452,
                }),
            ),
            (10_002, Record::Input(InputLevel::Many)),
            (
                20_000,
                Record::Gap(Gap {
                    span_us: 800,
                    dropped: vec![(1, 7), (99, 2)],
                }),
            ),
            (
                30_000,
                Record::End(End {
                    reason: EndReason::PeerQuit,
                    duration_us: 29_000,
                    tallies: vec![KindTally {
                        kind: 1,
                        written: 3,
                        dropped: 7,
                    }],
                }),
            ),
        ]
    }

    fn meta() -> Meta {
        Meta {
            session_id: Some([7; 16]),
            role: Role::Host,
            wall_start_us: 1_795_000_000_000_000,
            program_version: Some("0.1.0".into()),
            commit: Some("19daac9".into()),
            os: Some(PeerOs::Linux),
            codec: Some("h264".into()),
            estimator: Some("gcc-v1".into()),
            policy: Some("office".into()),
            fps: Some(60),
            start_bps: Some(2_000_000),
            max_bps: Some(30_000_000),
            caps: Some(0xffff),
        }
    }

    /// 写成一份完整的文件字节。
    fn write_all(meta: &Meta, records: &[(u64, Record)]) -> Vec<u8> {
        let mut out = encode_header(meta);
        let mut w = BlockWriter::new();
        for (at, r) in records {
            w.push(*at, r, &mut out);
        }
        w.flush(&mut out);
        out
    }

    #[test]
    fn varints_round_trip_at_the_edges() {
        for v in [0u64, 1, 127, 128, 300, u64::from(u32::MAX), u64::MAX] {
            let mut buf = Vec::new();
            put_varint(&mut buf, v);
            let mut cur = Cur::new(&buf);
            assert_eq!(cur.varint(), Some(v));
            assert!(cur.is_empty());
        }
        for v in [0i64, -1, 1, i64::MIN, i64::MAX, -123_456] {
            assert_eq!(unzigzag(zigzag(v)), v);
        }
        assert_eq!((zigzag(0), zigzag(-1), zigzag(1), zigzag(-2)), (0, 1, 2, 3));
        // 11 个续位字节、第 10 个字节超过 1 位、没读完：都读不出来
        assert_eq!(Cur::new(&[0x80; 11]).varint(), None);
        assert_eq!(Cur::new(&[0xff; 9]).varint(), None);
        let mut too_big = vec![0xff; 9];
        too_big.push(0x02);
        assert_eq!(Cur::new(&too_big).varint(), None);
        assert_eq!(Cur::new(&[]).varint(), None);
    }

    #[test]
    fn every_kind_round_trips() {
        let records = every_record();
        let bytes = write_all(&meta(), &records);
        let rec = Recording::parse(&bytes).unwrap();
        assert_eq!(rec.meta, meta());
        let got: Vec<(u64, Record)> = rec
            .entries
            .iter()
            .map(|e| (e.at_us, e.record.clone()))
            .collect();
        assert_eq!(got, records);
        assert_eq!(
            rec.stats,
            ReadStats {
                blocks: 1,
                ..ReadStats::default()
            }
        );
        assert_eq!(rec.end().unwrap().reason, EndReason::PeerQuit);
        assert_eq!(rec.count(Kind::Feedback), 1);
        assert_eq!(rec.count(Kind::Frame), 2);
        assert_eq!(
            rec.feedbacks().map(|(at, _)| at).collect::<Vec<_>>(),
            vec![1_000]
        );
        assert_eq!(
            rec.frames().map(|(at, _)| at).collect::<Vec<_>>(),
            vec![1_500, 2_000]
        );
        assert_eq!(
            rec.retransmits().map(|(at, _)| at).collect::<Vec<_>>(),
            vec![10_000]
        );
        assert_eq!(
            rec.gaps().map(|(at, _)| at).collect::<Vec<_>>(),
            vec![20_000]
        );
    }

    #[test]
    fn every_enum_value_round_trips() {
        let causes = [
            NetCause::Unknown,
            NetCause::Start,
            NetCause::FrameBlocked,
            NetCause::KeyPending,
            NetCause::FrameNearBudget,
            NetCause::FpsReduced,
            NetCause::ScaleReduced,
            NetCause::ResendCapped,
            NetCause::EncoderDropped,
            NetCause::UsingRate,
        ];
        let mut limits = vec![SendLimit::App];
        limits.extend(causes.map(SendLimit::Network));
        for limit in limits {
            assert_eq!(limit_from_code(limit_code(limit)), Some(limit));
        }
        assert_eq!(limit_from_code(11), None);
        for state in RateState::ALL {
            assert_eq!(
                state_from_code(u8::try_from(state.index()).unwrap()),
                Some(state)
            );
        }
        for class in [FrameClass::Content, FrameClass::Refine, FrameClass::Key] {
            assert_eq!(FrameClass::from_code(class.code()), Some(class));
        }
        for level in [InputLevel::None, InputLevel::Few, InputLevel::Many] {
            assert_eq!(InputLevel::from_code(level.code()), Some(level));
        }
        for reason in [
            EndReason::Normal,
            EndReason::PeerQuit,
            EndReason::LinkLost,
            EndReason::Failed,
        ] {
            assert_eq!(EndReason::from_code(reason.code()), Some(reason));
        }
        for kind in Kind::ALL {
            assert_eq!(Kind::from_code(kind.code()), Some(kind));
            assert_eq!(Kind::ALL.get(kind.slot()), Some(&kind));
            assert!(!kind.name().is_empty());
        }
        assert_eq!(Kind::from_code(0), None);
        assert_eq!(Role::from_code(2), None);
        assert_eq!(Role::Controller.name(), "controller");
        assert_eq!(FrameClass::of(true, false), FrameClass::Key);
        assert_eq!(FrameClass::of(false, true), FrameClass::Content);
        assert_eq!(FrameClass::of(false, false), FrameClass::Refine);
        assert_eq!(InputLevel::of(0), InputLevel::None);
        assert_eq!(InputLevel::of(5), InputLevel::Few);
        assert_eq!(InputLevel::of(6), InputLevel::Many);
    }

    #[test]
    fn feedback_is_built_from_an_observation() {
        let obs = Observation {
            feedback: Fb {
                floor: 9,
                delivered: 1,
                queue_delay_us: 2_000,
                loss_permille: 30,
                recv_bytes_per_s: 400_000,
            },
            ext: Extension {
                trend_tenth_ms: -5,
                groups: 3,
                train_bytes_per_s: 800_000,
            },
            rtt_us: 25_000,
            in_flight: 12_000,
            limit: SendLimit::App,
        };
        let f = Feedback::from_observation(&obs, 2_000_000, RateState::Startup);
        assert_eq!(
            (
                f.floor,
                f.queue_delay_us,
                f.loss_permille,
                f.recv_bytes_per_s
            ),
            (9, 2_000, 30, 400_000)
        );
        assert_eq!(
            (f.trend_tenth_ms, f.train_bytes_per_s, f.rtt_us),
            (-5, 800_000, 25_000)
        );
        assert_eq!(
            (f.in_flight_bytes, f.limit, f.rate_bps, f.state),
            (12_000, SendLimit::App, 2_000_000, RateState::Startup)
        );
    }

    #[test]
    fn times_are_per_kind_deltas_and_never_go_backwards() {
        // 同一种类时间倒退：按上一条的时间写；不同种类互不影响
        let records = vec![
            (5_000, feedback(1)),
            (4_000, feedback(2)),
            (6_000, Record::Input(InputLevel::Few)),
        ];
        let rec = Recording::parse(&write_all(&meta(), &records)).unwrap();
        let times: Vec<u64> = rec.entries.iter().map(|e| e.at_us).collect();
        assert_eq!(times, vec![5_000, 5_000, 6_000]);
        // 记录比块起始时间早：按块起始时间读（差 0）
        let mut out = Vec::new();
        let mut w = BlockWriter::new();
        w.open(10_000);
        w.push(9_000, &Record::Input(InputLevel::Few), &mut out);
        w.flush(&mut out);
        let mut file = encode_header(&meta());
        file.extend_from_slice(&out);
        let rec = Recording::parse(&file).unwrap();
        assert_eq!(rec.entries[0].at_us, 10_000);
    }

    #[test]
    fn frame_ids_and_floors_are_deltas_that_reset_per_block() {
        // 帧号走到很大，再回到小的（例如会话重新计数）：有符号差都能还原
        let records = vec![
            (0, frame(1_000_000)),
            (10, frame(5)),
            (20, feedback(7)),
            (30, feedback(3)),
            // 超过 1 秒：新块，差值从 0 重来
            (2_000_000, frame(6)),
            (2_000_010, feedback(4_000_000_000)),
        ];
        let bytes = write_all(&meta(), &records);
        let rec = Recording::parse(&bytes).unwrap();
        assert_eq!(rec.stats.blocks, 2);
        let got: Vec<(u64, Record)> = rec
            .entries
            .iter()
            .map(|e| (e.at_us, e.record.clone()))
            .collect();
        assert_eq!(got, records);
    }

    #[test]
    fn blocks_seal_by_time_and_by_size_and_on_flush() {
        let mut out = Vec::new();
        let mut w = BlockWriter::default();
        assert!(!w.is_open());
        w.flush(&mut out);
        assert!(out.is_empty(), "没有打开的块时 flush 什么也不写");
        w.push(0, &feedback(1), &mut out);
        assert!(w.is_open());
        w.push(999_999, &feedback(2), &mut out);
        assert_eq!(w.blocks(), 0, "不到 1 秒不封");
        w.push(1_000_000, &feedback(3), &mut out);
        assert_eq!(w.blocks(), 1, "差到 1 秒封上一块");
        assert_eq!(w.written()[Kind::Feedback.slot()], 3);
        // 按大小：塞满 64 KiB 就封，不用等时间
        let mut w = BlockWriter::new();
        let mut out = Vec::new();
        let mut n = 0u32;
        while w.blocks() == 0 {
            n += 1;
            w.push(u64::from(n), &frame(n), &mut out);
            assert!(n < 10_000);
        }
        assert!(n > 100 && out.len() > BLOCK_SOFT_BYTES);
        w.flush(&mut out);
        let mut file = encode_header(&meta());
        file.extend_from_slice(&out);
        let rec = Recording::parse(&file).unwrap();
        assert_eq!(rec.count(Kind::Frame), usize::try_from(n).unwrap());
    }

    /// 手工拼一个块：记录是（种类号，内容）。
    fn raw_block(start: u64, records: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let mut content = Vec::new();
        put_varint(&mut content, start);
        for (kind, body) in records {
            put_varint(&mut content, *kind);
            put_varint(&mut content, u64::try_from(body.len()).unwrap());
            content.extend_from_slice(body);
        }
        let mut out = Vec::new();
        out.extend_from_slice(&u32::try_from(content.len()).unwrap().to_be_bytes());
        out.extend_from_slice(&crc32fast::hash(&content).to_be_bytes());
        out.extend_from_slice(&content);
        out
    }

    /// 一条记录的内容字节（时间差加字段），用来在后面追加尾部字段。
    fn content_of(record: &Record) -> Vec<u8> {
        let mut st = Deltas::new(0);
        let mut out = Vec::new();
        put_varint(&mut out, 0);
        record.put(&mut st, &mut out);
        out
    }

    #[test]
    fn unknown_kinds_and_extra_tail_fields_are_skipped() {
        let mut feedback_body = content_of(&feedback(3));
        // 未来版本在尾部加的字段：读的一方忽略
        feedback_body.extend_from_slice(&[0x81, 0x01, 0x05, 0x06]);
        let mut gap_body = content_of(&Record::Input(InputLevel::Few));
        gap_body.extend_from_slice(&[9, 9, 9]);
        let mut file = encode_header(&meta());
        file.extend_from_slice(&raw_block(
            50,
            &[
                (77, vec![1, 2, 3, 4]),
                (Kind::Feedback.code(), feedback_body),
                (200, vec![]),
                (Kind::Input.code(), gap_body),
            ],
        ));
        let rec = Recording::parse(&file).unwrap();
        assert_eq!(rec.stats.unknown_records, 2);
        assert_eq!(rec.stats.bad_records, 0);
        assert_eq!(rec.entries.len(), 2);
        assert_eq!(
            rec.entries[0],
            Entry {
                at_us: 50,
                record: feedback(3)
            }
        );
        assert_eq!(rec.entries[1].record, Record::Input(InputLevel::Few));
    }

    #[test]
    fn records_that_cannot_be_decoded_are_counted_not_silent() {
        let mut file = encode_header(&meta());
        // 反馈缺字段；输入的值不合法；长度指到块外
        let mut short = Vec::new();
        put_varint(&mut short, 0);
        file.extend_from_slice(&raw_block(0, &[(Kind::Feedback.code(), short)]));
        file.extend_from_slice(&raw_block(0, &[(Kind::Input.code(), vec![0, 9])]));
        let mut broken = Vec::new();
        put_varint(&mut broken, 0);
        put_varint(&mut broken, 5);
        put_varint(&mut broken, 99);
        file.extend_from_slice(&{
            let mut content = Vec::new();
            put_varint(&mut content, 0);
            content.extend_from_slice(&broken);
            let mut out = Vec::new();
            out.extend_from_slice(&u32::try_from(content.len()).unwrap().to_be_bytes());
            out.extend_from_slice(&crc32fast::hash(&content).to_be_bytes());
            out.extend_from_slice(&content);
            out
        });
        // 块内容连起始时间都没有
        let mut empty = Vec::new();
        empty.extend_from_slice(&1u32.to_be_bytes());
        empty.extend_from_slice(&crc32fast::hash(&[0x80]).to_be_bytes());
        empty.push(0x80);
        file.extend_from_slice(&empty);
        // 好块照常读
        file.extend_from_slice(&raw_block(
            9,
            &[(
                Kind::Input.code(),
                content_of(&Record::Input(InputLevel::None)),
            )],
        ));
        let rec = Recording::parse(&file).unwrap();
        assert_eq!(rec.stats.bad_records, 4);
        assert_eq!(rec.entries.len(), 1);
        assert_eq!(rec.entries[0].at_us, 9);
    }

    fn blocks_of(bytes: &[u8]) -> Vec<(usize, usize)> {
        let mut pos = FIXED_HEAD + usize::from(u16::from_be_bytes([bytes[6], bytes[7]]));
        let mut spans = Vec::new();
        while pos < bytes.len() {
            let len = usize::try_from(u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()))
                .unwrap();
            spans.push((pos, pos + BLOCK_HEAD + len));
            pos += BLOCK_HEAD + len;
        }
        spans
    }

    /// 四个块，每块 3 条反馈，时间隔得够远。
    fn four_blocks() -> (Vec<u8>, Vec<(u64, Record)>) {
        let mut records = Vec::new();
        for block in 0..4u32 {
            for i in 0..3u32 {
                records.push((
                    u64::from(block) * 2_000_000 + u64::from(i) * 1_000,
                    feedback(block * 10 + i + 1),
                ));
            }
        }
        (write_all(&meta(), &records), records)
    }

    fn read_pairs(rec: &Recording) -> Vec<(u64, Record)> {
        rec.entries
            .iter()
            .map(|e| (e.at_us, e.record.clone()))
            .collect()
    }

    #[test]
    fn truncating_at_every_byte_keeps_every_complete_block() {
        let (bytes, records) = four_blocks();
        let spans = blocks_of(&bytes);
        assert_eq!(spans.len(), 4);
        let header_len = spans[0].0;
        // 文件头被截断：不是记录文件，报错不 panic
        for cut in 0..header_len {
            assert!(Recording::parse(&bytes[..cut]).is_err(), "头里截断 {cut}");
        }
        for cut in header_len..=bytes.len() {
            let rec = Recording::parse(&bytes[..cut]).unwrap();
            let complete = spans.iter().filter(|(_, end)| *end <= cut).count();
            assert_eq!(
                read_pairs(&rec),
                records[..complete * 3].to_vec(),
                "截断在 {cut}"
            );
            assert_eq!(rec.stats.blocks, u64::try_from(complete).unwrap());
            let at_boundary = spans.iter().any(|(_, end)| *end == cut) || cut == header_len;
            assert_eq!(rec.stats.truncated_tail, !at_boundary, "截断在 {cut}");
            assert_eq!(rec.stats.bad_blocks, 0, "截断不是坏块：{cut}");
        }
    }

    #[test]
    fn flipping_any_byte_loses_only_that_block() {
        let (bytes, records) = four_blocks();
        let spans = blocks_of(&bytes);
        for (block, (from, to)) in spans.iter().enumerate() {
            for at in *from..*to {
                for mask in [0x01u8, 0xff] {
                    let mut damaged = bytes.clone();
                    damaged[at] ^= mask;
                    let rec = Recording::parse(&damaged).unwrap();
                    let expect: Vec<(u64, Record)> = records
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| i / 3 != block)
                        .map(|(_, r)| r.clone())
                        .collect();
                    assert_eq!(
                        read_pairs(&rec),
                        expect,
                        "块 {block} 字节 {at} 翻转 {mask:#x}"
                    );
                    // 最后一块的长度字段被改大、指到文件外时，和“文件被截断”分不开，记成截断；其余都是一个坏块
                    let looks_truncated =
                        block == 3 && at < spans[3].0 + 4 && rec.stats.truncated_tail;
                    if looks_truncated {
                        assert_eq!(rec.stats.bad_blocks, 0, "块 {block} 字节 {at}");
                    } else {
                        assert_eq!(rec.stats.bad_blocks, 1, "块 {block} 字节 {at}");
                        assert!(!rec.stats.truncated_tail, "块 {block} 字节 {at}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_damaged_last_block_is_a_bad_block_not_a_truncation() {
        let (bytes, records) = four_blocks();
        let mut damaged = bytes.clone();
        let last = blocks_of(&bytes)[3];
        damaged[last.1 - 1] ^= 0x55;
        let rec = Recording::parse(&damaged).unwrap();
        assert_eq!(read_pairs(&rec), records[..9].to_vec());
        assert_eq!((rec.stats.bad_blocks, rec.stats.truncated_tail), (1, false));
        assert_eq!(
            rec.stats.skipped_bytes,
            u64::try_from(last.1 - last.0).unwrap()
        );
        // 块长度被改成荒谬的大值，后面没有别的好块：同样算一个坏块
        let mut huge = bytes;
        huge[last.0] = 0xff;
        let rec = Recording::parse(&huge).unwrap();
        assert_eq!(read_pairs(&rec), records[..9].to_vec());
        assert_eq!(rec.stats.bad_blocks, 1);
    }

    #[test]
    fn an_end_record_tells_a_clean_finish_from_a_crash() {
        let records = vec![(0, feedback(1))];
        let crashed = Recording::parse(&write_all(&meta(), &records)).unwrap();
        assert!(crashed.end().is_none());
        let mut with_end = records;
        with_end.push((
            10,
            Record::End(End {
                reason: EndReason::Normal,
                duration_us: 10,
                tallies: Vec::new(),
            }),
        ));
        let clean = Recording::parse(&write_all(&meta(), &with_end)).unwrap();
        assert_eq!(clean.end().map(|e| e.reason), Some(EndReason::Normal));
        // 只有文件头也是一份合法的记录（刚开始就崩了）
        let empty = Recording::parse(&encode_header(&meta())).unwrap();
        assert!(empty.entries.is_empty() && empty.end().is_none());
        assert_eq!(empty.stats, ReadStats::default());
    }

    #[test]
    fn header_errors_are_reported() {
        let good = encode_header(&meta());
        assert_eq!(
            Recording::parse(b"").unwrap_err(),
            RecordError::NotARecording
        );
        let mut bad_magic = good.clone();
        bad_magic[0] = b'X';
        assert_eq!(
            Recording::parse(&bad_magic).unwrap_err(),
            RecordError::NotARecording
        );
        let mut newer = good.clone();
        newer[4..6].copy_from_slice(&(VERSION + 1).to_be_bytes());
        assert_eq!(
            Recording::parse(&newer).unwrap_err(),
            RecordError::UnsupportedVersion(VERSION + 1)
        );
        // 头长度指到文件外
        let mut long = good.clone();
        long[6..8].copy_from_slice(&u16::MAX.to_be_bytes());
        assert_eq!(
            Recording::parse(&long).unwrap_err(),
            RecordError::NotARecording
        );
        // 元数据缺角色
        let mut no_role = Vec::new();
        Meta::put_tlv(&mut no_role, 3, &0u64.to_be_bytes());
        // 元数据缺开始时间
        let mut no_wall = Vec::new();
        Meta::put_tlv(&mut no_wall, 2, &[0]);
        // 值的长度不对、字符串不是 UTF-8、角色和系统号不合法、标签的长度超出
        let mut short_id = Vec::new();
        Meta::put_tlv(&mut short_id, 1, &[1, 2, 3]);
        let mut bad_utf8 = Vec::new();
        Meta::put_tlv(&mut bad_utf8, 4, &[0xff, 0xfe]);
        let mut bad_role = Vec::new();
        Meta::put_tlv(&mut bad_role, 2, &[9]);
        let mut bad_os = Vec::new();
        Meta::put_tlv(&mut bad_os, 6, &[9]);
        let mut short_wall = Vec::new();
        Meta::put_tlv(&mut short_wall, 3, &[1, 2]);
        let overlong = vec![0x04, 0x09, b'a'];
        let cut_tag = vec![0x80];
        let cut_len = vec![0x04, 0x80];
        for body in [
            no_role, no_wall, short_id, bad_utf8, bad_role, bad_os, short_wall, overlong, cut_tag,
            cut_len,
        ] {
            assert_eq!(
                Meta::decode(&body).unwrap_err(),
                RecordError::BadHeader,
                "{body:?}"
            );
        }
        for e in [
            RecordError::NotARecording,
            RecordError::UnsupportedVersion(9),
            RecordError::BadHeader,
        ] {
            assert!(!e.to_string().is_empty());
        }
        // 元数据太长
        let mut huge = meta();
        huge.program_version = Some("x".repeat(META_TEXT_MAX));
        let ok = encode_header(&huge);
        assert!(ok.len() < usize::from(u16::MAX));
    }

    #[test]
    fn unknown_metadata_tags_are_skipped_and_text_is_clipped() {
        let mut body = Vec::new();
        Meta::put_tlv(&mut body, 2, &[1]);
        Meta::put_tlv(&mut body, 3, &42u64.to_be_bytes());
        Meta::put_tlv(&mut body, 500, &[1, 2, 3]);
        let meta = Meta::decode(&body).unwrap();
        assert_eq!(
            (meta.role, meta.wall_start_us, meta.session_id),
            (Role::Controller, 42, None)
        );
        // 超长的字符串在字符边界截断：72 个“中”（216 字节）截到 64 字节以内
        let mut m = Meta::new(Role::Host, 0);
        m.commit = Some("中".repeat(72));
        let bytes = encode_header(&m);
        let back = Recording::parse(&bytes).unwrap().meta;
        let commit = back.commit.unwrap();
        assert!(commit.len() <= META_TEXT_MAX && commit.chars().all(|c| c == '中'));
        assert_eq!(clip("abc", 10), "abc");
        assert_eq!(clip("中", 1), "");
        // 帧记录里的关键帧原因同样截断
        let long = Record::Frame(Frame {
            key_reason: Cow::Owned("因".repeat(100)),
            ..bare(1)
        });
        let rec = Recording::parse(&write_all(&Meta::new(Role::Host, 0), &[(0, long)])).unwrap();
        assert!(matches!(
            &rec.entries[0].record,
            Record::Frame(f) if f.key_reason.len() <= KEY_REASON_MAX && !f.key_reason.is_empty()
        ));
    }

    #[test]
    fn a_gap_opens_the_next_block_and_the_end_record_adds_everything_up() {
        let mut out = encode_header(&meta());
        let mut sink = Sink::new();
        let mut asked = 0;
        let gap = GapInfo {
            first_us: 900,
            last_us: 1_400,
            dropped: [3, 0, 0, 0, 0, 0, 0],
        };
        sink.push(
            1_000,
            &feedback(1),
            || {
                asked += 1;
                Some(gap)
            },
            &mut out,
        );
        // 块已经开着：不再问缺口（问了就会把计数取走）
        sink.push(
            1_100,
            &feedback(2),
            || {
                asked += 1;
                None
            },
            &mut out,
        );
        assert_eq!(asked, 1);
        // 超过 1 秒开新块，这次没有缺口
        sink.push(
            2_500_000,
            &Record::Input(InputLevel::Few),
            || None,
            &mut out,
        );
        let last = GapInfo {
            first_us: 2_600_000,
            last_us: 2_700_000,
            dropped: [0, 2, 0, 0, 0, 0, 0],
        };
        sink.finish(
            3_000_000,
            EndReason::LinkLost,
            3_000_000,
            Some(last),
            &mut out,
        );
        let rec = Recording::parse(&out).unwrap();
        let got: Vec<(u64, Record)> = read_pairs(&rec);
        assert_eq!(
            got[0],
            (
                1_400,
                Record::Gap(Gap {
                    span_us: 500,
                    dropped: vec![(1, 3)]
                })
            )
        );
        assert_eq!(got[1].0, 1_000);
        assert_eq!(got[3].0, 2_500_000);
        assert_eq!(
            got[4],
            (
                2_700_000,
                Record::Gap(Gap {
                    span_us: 100_000,
                    dropped: vec![(2, 2)]
                })
            )
        );
        let Record::End(end) = &got[5].1 else {
            assert_eq!(got[5].1.kind(), Kind::End, "最后一条应当是结束记录");
            return;
        };
        assert_eq!(
            (end.reason, end.duration_us),
            (EndReason::LinkLost, 3_000_000)
        );
        let tally = |kind: Kind| end.tallies.iter().find(|t| t.kind == kind.code()).copied();
        assert_eq!(
            tally(Kind::Feedback),
            Some(KindTally {
                kind: 1,
                written: 2,
                dropped: 3
            })
        );
        assert_eq!(
            tally(Kind::Frame),
            Some(KindTally {
                kind: 2,
                written: 0,
                dropped: 2
            })
        );
        assert_eq!(tally(Kind::Gap).map(|t| t.written), Some(2));
        assert_eq!(tally(Kind::End), None, "结束记录不数自己");
        assert_eq!(tally(Kind::Transport), None, "没写过也没丢过的不列");
        assert_eq!(sink.written()[Kind::End.slot()], 1);
        assert_eq!(sink.dropped(), &[3, 2, 0, 0, 0, 0, 0]);
        assert_eq!(sink.blocks(), rec.stats.blocks);
    }

    #[test]
    fn controller_files_and_every_os_round_trip() {
        for os in [PeerOs::Unknown, PeerOs::Windows, PeerOs::Mac, PeerOs::Linux] {
            let mut m = Meta::new(Role::Controller, 5);
            m.os = Some(os);
            let back = Recording::parse(&encode_header(&m)).unwrap().meta;
            assert_eq!(back, m);
        }
    }

    #[test]
    fn frames_with_inconsistent_fields_are_counted_as_bad() {
        // 只有宽没有高；耗时标志不是 0 或 1；字段值装不进目标类型
        let mut file = encode_header(&meta());
        let base = |tail: &dyn Fn(&mut Vec<u8>)| {
            let mut c = Vec::new();
            put_varint(&mut c, 0); // 时间差
            put_signed(&mut c, 1); // 帧号差
            put_opt(&mut c, None); // 采集时间
            c.push(0); // 类别
            put_text(&mut c, "");
            put_opt(&mut c, None); // 预算
            put_varint(&mut c, 10); // 字节
            put_opt(&mut c, None); // 缩放
            tail(&mut c);
            c
        };
        let half_size = base(&|c| {
            put_opt(c, Some(640));
            put_opt(c, None);
            c.extend_from_slice(&[0, 0]);
        });
        let bad_timing_flag = base(&|c| {
            put_opt(c, None);
            put_opt(c, None);
            c.push(2);
        });
        let too_wide = base(&|c| {
            put_opt(c, Some(70_000));
            put_opt(c, Some(1));
            c.extend_from_slice(&[0, 0]);
        });
        for body in [half_size, bad_timing_flag, too_wide] {
            file.extend_from_slice(&raw_block(0, &[(Kind::Frame.code(), body)]));
        }
        let rec = Recording::parse(&file).unwrap();
        assert_eq!((rec.stats.bad_records, rec.entries.len()), (3, 0));
    }

    #[test]
    fn nonsense_block_lengths_are_skipped() {
        // 块长度 0、长度超过上限、块内容的起始时间读不出来：都当坏块，后面的好块照读
        let mut file = encode_header(&meta());
        file.extend_from_slice(&[0; 8]);
        let mut huge = Vec::new();
        huge.extend_from_slice(&u32::try_from(MAX_BLOCK_BYTES + 1).unwrap().to_be_bytes());
        huge.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&huge);
        file.extend_from_slice(&raw_block(
            3,
            &[(
                Kind::Input.code(),
                content_of(&Record::Input(InputLevel::Few)),
            )],
        ));
        let rec = Recording::parse(&file).unwrap();
        assert_eq!(rec.entries.len(), 1);
        assert_eq!((rec.stats.bad_blocks, rec.stats.truncated_tail), (1, false));
    }

    #[test]
    fn file_names_sort_by_time_and_part_files_are_recognized() {
        let mut m = meta();
        m.wall_start_us = 1_795_000_000 * 1_000_000;
        // 2026-11-18 11:06:40 UTC
        let name = file_name(&m);
        assert_eq!(name, "20261118T110640Z-07070707-host.pdsr");
        let mut later = m.clone();
        later.wall_start_us += 1_000_000;
        later.role = Role::Controller;
        later.session_id = None;
        assert_eq!(
            file_name(&later),
            "20261118T110641Z-00000000-controller.pdsr"
        );
        assert!(file_name(&m) < file_name(&later));
        assert_eq!(part_name(&name), "20261118T110640Z-07070707-host.pdsr.part");
        assert_eq!(classify(&name), Some(FileState::Finished));
        assert_eq!(classify(&part_name(&name)), Some(FileState::Part));
        for other in [
            "",
            ".pdsr",
            ".pdsr.part",
            "notes.txt",
            "a.pdsr.part.bak",
            "a.part",
        ] {
            assert_eq!(classify(other), None, "{other}");
        }
        assert_eq!(civil(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(civil(951_782_400 + 3_661), (2000, 2, 29, 1, 1, 1));
    }
}
