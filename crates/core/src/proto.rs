//! 线上协议：数据报包头、视频分片头、控制消息。
//!
//! 数据报布局：`包头(20 字节) | 载荷`。视频载荷再以 [`VideoHeader`] 开头。
//! M1 接入 QUIC 后，这些数据报承载在 QUIC 数据报里，控制消息承载在 QUIC 流里，格式不变。

use crate::crypto::Suite;
use crate::wire::{Reader, Writer};
use crate::{Error, Time};

/// 包头魔数。
pub const MAGIC: u8 = 0x50;
/// 协议版本（包头和握手里的版本号）。
///
/// 兼容规则（见 `docs/compat.md`）：新功能一律用 [`Caps`] 里的能力位协商，两端都支持才开启，
/// 版本号不变，新旧版本照常互通。只有无法兼容的改动才升版本号，这时被控端仍接受
/// [`MIN_VERSION`] 及以上的所有版本（包括比自己新的），只有低于它才拒绝（要求升级）。
pub const VERSION: u8 = 1;
/// 还能连的最老版本。低于它才拒绝连接、提示升级。
pub const MIN_VERSION: u8 = 1;
/// 包头长度。
pub const HEADER_LEN: usize = 20;
/// 视频分片头长度。
pub const VIDEO_HEADER_LEN: usize = 12;
/// 单个数据报的最大长度，避免 IP 分片。
pub const MAX_DATAGRAM: usize = 1200;
/// 区域名的最大长度。
pub const MAX_REGION_LEN: usize = 32;
/// 电脑名最长字节数（UTF-8）。更长的在字符边界截断。
pub const MAX_NAME_LEN: usize = 64;

/// 把电脑名截到 [`MAX_NAME_LEN`] 字节以内，不切断字符。
#[must_use]
pub fn clip_name(name: &str) -> String {
    let mut end = name.len().min(MAX_NAME_LEN);
    while !name.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    name.get(..end).unwrap_or_default().to_owned()
}
/// 一条 Hello 最多携带的套件数。
pub const MAX_SUITES: usize = 16;

/// 逻辑通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Channel {
    /// 键鼠输入。
    Input,
    /// 控制与反馈。
    Control,
    /// 光标形状与位置。
    Cursor,
    /// 视频。
    Video,
    /// 剪贴板。
    Clipboard,
    /// 文件。
    File,
    /// 声音（不可靠，每 10 ms 一个包；只在双方协商出声音能力、控制端打开声音时才有）。
    Audio,
}

impl Channel {
    /// 全部通道，按优先级从高到低。
    pub const ALL: [Channel; 7] = [
        Channel::Input,
        Channel::Control,
        Channel::Cursor,
        Channel::Video,
        Channel::Clipboard,
        Channel::File,
        Channel::Audio,
    ];

    /// 线上编号。
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Channel::Input => 0,
            Channel::Control => 1,
            Channel::Cursor => 2,
            Channel::Video => 3,
            Channel::Clipboard => 4,
            Channel::File => 5,
            Channel::Audio => 6,
        }
    }

    /// 由线上编号解析。
    pub const fn from_id(id: u8) -> Result<Self, Error> {
        match id {
            0 => Ok(Channel::Input),
            1 => Ok(Channel::Control),
            2 => Ok(Channel::Cursor),
            3 => Ok(Channel::Video),
            4 => Ok(Channel::Clipboard),
            5 => Ok(Channel::File),
            6 => Ok(Channel::Audio),
            other => Err(Error::UnknownChannel(other)),
        }
    }

    /// 调度优先级，0 最高。
    #[must_use]
    pub const fn priority(self) -> u8 {
        // 声音排在画面前面：输入 > 控制 > 光标 > 声音 > 画面 > 剪贴板 > 文件
        match self {
            Channel::Input => 0,
            Channel::Control => 1,
            Channel::Cursor => 2,
            Channel::Audio => 3,
            Channel::Video => 4,
            Channel::Clipboard => 5,
            Channel::File => 6,
        }
    }

    /// 是否要求可靠有序送达。
    #[must_use]
    pub const fn reliable(self) -> bool {
        match self {
            Channel::Control | Channel::Clipboard | Channel::File => true,
            Channel::Input | Channel::Cursor | Channel::Video | Channel::Audio => false,
        }
    }
}

/// 数据报包头。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// 通道。
    pub channel: Channel,
    /// 标志位，目前保留。
    pub flags: u8,
    /// 连接标识。会话不绑定 IP 和端口，换路径时保持不变。
    pub conn_id: u32,
    /// 包序号。
    pub seq: u32,
    /// 发送时间，用于延迟趋势估计。
    pub send_time: Time,
}

impl Header {
    /// 编码到写入器。
    pub fn encode(&self, w: &mut Writer) {
        w.u8(MAGIC)
            .u8(VERSION)
            .u8(self.channel.id())
            .u8(self.flags)
            .u32(self.conn_id)
            .u32(self.seq)
            .u64(self.send_time.micros());
    }

    /// 从读取器解码。
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let magic = r.u8()?;
        if magic != MAGIC {
            return Err(Error::BadMagic(magic));
        }
        let version = r.u8()?;
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let channel = Channel::from_id(r.u8()?)?;
        let flags = r.u8()?;
        let conn_id = r.u32()?;
        let seq = r.u32()?;
        let send_time = Time(r.u64()?);
        Ok(Self {
            channel,
            flags,
            conn_id,
            seq,
            send_time,
        })
    }
}

/// 把包头和载荷拼成一个数据报，超过 [`MAX_DATAGRAM`] 报错。
pub fn encode_datagram(header: &Header, payload: &[u8]) -> Result<Vec<u8>, Error> {
    let len = HEADER_LEN.saturating_add(payload.len());
    if len > MAX_DATAGRAM {
        return Err(Error::PayloadTooLarge {
            len,
            max: MAX_DATAGRAM,
        });
    }
    let mut w = Writer::new();
    header.encode(&mut w);
    w.bytes(payload);
    Ok(w.into_vec())
}

/// 拆出包头和载荷。
pub fn decode_datagram(buf: &[u8]) -> Result<(Header, &[u8]), Error> {
    if buf.len() > MAX_DATAGRAM {
        return Err(Error::PayloadTooLarge {
            len: buf.len(),
            max: MAX_DATAGRAM,
        });
    }
    let mut r = Reader::new(buf);
    let header = Header::decode(&mut r)?;
    Ok((header, r.rest()))
}

/// 视频分片头。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoHeader {
    /// 帧号。
    pub frame_id: u32,
    /// 帧内分片序号，从 0 开始。
    pub frag_index: u16,
    /// 帧内分片总数，至少为 1。
    pub frag_count: u16,
    /// 本帧参考的帧号；关键帧参考自己。
    pub ref_frame: u32,
}

impl VideoHeader {
    /// 校验分片序号与总数。
    pub const fn validate(&self) -> Result<(), Error> {
        if self.frag_count == 0 || self.frag_index >= self.frag_count {
            return Err(Error::InvalidFragment {
                index: self.frag_index,
                count: self.frag_count,
            });
        }
        Ok(())
    }

    /// 编码。
    pub fn encode(&self, w: &mut Writer) {
        w.u32(self.frame_id)
            .u16(self.frag_index)
            .u16(self.frag_count)
            .u32(self.ref_frame);
    }

    /// 解码并校验。
    pub fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        let header = Self {
            frame_id: r.u32()?,
            frag_index: r.u16()?,
            frag_count: r.u16()?,
            ref_frame: r.u32()?,
        };
        header.validate()?;
        Ok(header)
    }
}

/// 编解码能力位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caps(pub u32);

impl Caps {
    /// H.264。
    pub const H264: Caps = Caps(1);
    /// H.265。
    pub const H265: Caps = Caps(1 << 1);
    /// AV1。
    pub const AV1: Caps = Caps(1 << 2);
    /// 4:4:4 色度。
    pub const YUV444: Caps = Caps(1 << 3);

    /// 帧上带输入序号（见 [`crate::reaction`]）：被控端随帧告诉控制端“这幅画面包含到第几个输入”，
    /// 控制端据此量“操作到画面”。对端没有这一位时被控端不发，控制端用旧算法（操作后的下一幅画面）。
    /// 用第 15 位：第 16–23 位用完了，24–26 是被控端的系统，27–31 在这个分支和其他分支上都占了（见 `PROBE_ID`）；
    /// 第 4–15 位原本留给编码格式，一直没用过，协商同样是取交集，老版本没有这一位。
    pub const INPUT_STAMP: Caps = Caps(1 << 15);

    // 第 16 位起是功能能力位。老版本不发这些位，协商后的交集里也就没有，对应功能自动关闭。
    /// 剪贴板同步。
    pub const CLIPBOARD: Caps = Caps(1 << 16);
    /// 声音。
    pub const AUDIO: Caps = Caps(1 << 17);
    /// 剪贴板里复制粘贴文件（20 MB 以内）。
    pub const CLIPBOARD_FILES: Caps = Caps(1 << 18);
    /// 视频纠错用 RS 码（老版本只认异或纠错）。
    pub const FEC_RS: Caps = Caps(1 << 19);
    /// 被控端每秒发状态（编码器、编码耗时等，控制端“详情”面板显示）。
    pub const HOST_STATS: Caps = Caps(1 << 20);
    /// 文件管理：浏览对方的目录、指定目录的上传下载（见 [`crate::browse`]）。
    pub const FILE_BROWSE: Caps = Caps(1 << 21);
    /// 控制端告诉被控端自己显示画面的区域有多大（像素），被控端按这个大小编码（见 [`crate::scale::fit`]）。
    pub const VIEW_SIZE: Caps = Caps(1 << 22);
    /// 发送端的网络控制器（见 [`crate::cc`]）：接收端在反馈里发到达报告、能收探测的填充包。
    pub const TRANSPORT_CC: Caps = Caps(1 << 23);
    /// 接收端按组测填充包的包串速率（包里带组号）、随反馈带回：给以后画面静止时的低成本探测用（链路在静止时
    /// 变差，发送端要能察觉，见 #26）。接收端先支持、先发出去，发送端开始探测时用户手上的版本已经能配合。
    /// 起步探测实测不如保守的起步码率，没用它（docs/research/L-framework.md T7）。
    pub const PROBE_TRAIN: Caps = Caps(1 << 24);
    /// 反馈里的探测结果带组号（见 [`crate::estimator::ProbeReport`]）：发送端能分清结果是哪一组探测的、不会和帧的
    /// 包串混，才做运行中探测（应用受限时降过码率之后核实、码率偏低时定期往上探）。对端没有这一位时反馈格式和
    /// 探测行为都照旧。
    /// 用第 31 位：24–26 是被控端的系统（见下，`PROBE_TRAIN` 和它撞了第 24 位，Mac 被控端的 Accept 里这一位是 0），
    /// 其他分支上 27 是音画同步、28 是模式、29 是挪过位置的 `PROBE_TRAIN`、30 是剪贴板按需取。
    pub const PROBE_ID: Caps = Caps(1 << 31);

    // 第 24–26 位不是能力，是被控端在 Accept 里告诉控制端自己的系统（见 [`PeerOs`]），
    // 控制端据此把快捷键里的 Ctrl 换成 Cmd 等。老版本只看能力位，会忽略这几位。
    const OS_SHIFT: u32 = 24;
    const OS_MASK: u32 = 0b111 << Self::OS_SHIFT;

    /// 带上本机系统（被控端发 Accept 时用）。
    #[must_use]
    pub const fn with_os(self, os: PeerOs) -> Caps {
        Caps((self.0 & !Self::OS_MASK) | ((os as u32) << Self::OS_SHIFT))
    }

    /// 对端的系统；老版本不发时为 [`PeerOs::Unknown`]。
    #[must_use]
    pub const fn os(self) -> PeerOs {
        match (self.0 & Self::OS_MASK) >> Self::OS_SHIFT {
            1 => PeerOs::Windows,
            2 => PeerOs::Mac,
            3 => PeerOs::Linux,
            _ => PeerOs::Unknown,
        }
    }

    /// 本版本支持的全部能力。
    pub const ALL: Caps = Caps(
        Self::H264.0
            | Self::CLIPBOARD.0
            | Self::AUDIO.0
            | Self::CLIPBOARD_FILES.0
            | Self::FEC_RS.0
            | Self::HOST_STATS.0
            | Self::FILE_BROWSE.0
            | Self::VIEW_SIZE.0
            | Self::TRANSPORT_CC.0
            | Self::PROBE_TRAIN.0
            | Self::PROBE_ID.0
            | Self::INPUT_STAMP.0,
    );

    /// 并集。
    #[must_use]
    pub const fn with(self, other: Caps) -> Caps {
        Caps(self.0 | other.0)
    }

    /// 交集。
    #[must_use]
    pub const fn common(self, other: Caps) -> Caps {
        Caps(self.0 & other.0)
    }

    /// 是否包含 `other` 的全部能力。
    #[must_use]
    pub const fn has(self, other: Caps) -> bool {
        self.0 & other.0 == other.0
    }
}

/// 本机的系统（编译时确定）。
#[cfg(windows)]
const CURRENT_OS: PeerOs = PeerOs::Windows;
#[cfg(target_os = "macos")]
const CURRENT_OS: PeerOs = PeerOs::Mac;
#[cfg(target_os = "linux")]
const CURRENT_OS: PeerOs = PeerOs::Linux;
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
const CURRENT_OS: PeerOs = PeerOs::Unknown;

/// 被控端的操作系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerOs {
    /// 不知道（老版本）。
    Unknown = 0,
    /// Windows。
    Windows = 1,
    /// macOS。
    Mac = 2,
    /// Linux。
    Linux = 3,
}

impl PeerOs {
    /// 本机的系统。
    #[must_use]
    pub const fn current() -> Self {
        CURRENT_OS
    }
}

/// 拒绝连接的原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// 协议版本不兼容。
    Version,
    /// 没有共同的加密套件。
    NoCommonSuite,
    /// 被控端正忙。
    Busy,
    /// 未授权。
    Unauthorized,
    /// 认证失败次数过多，暂时锁定。
    Locked,
}

impl RejectReason {
    /// 全部原因。
    pub const ALL: [RejectReason; 5] = [
        RejectReason::Version,
        RejectReason::NoCommonSuite,
        RejectReason::Busy,
        RejectReason::Unauthorized,
        RejectReason::Locked,
    ];

    /// 线上编号。
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            RejectReason::Version => 1,
            RejectReason::NoCommonSuite => 2,
            RejectReason::Busy => 3,
            RejectReason::Unauthorized => 4,
            RejectReason::Locked => 5,
        }
    }

    /// 由线上编号解析。
    pub const fn from_id(id: u8) -> Result<Self, Error> {
        match id {
            1 => Ok(RejectReason::Version),
            2 => Ok(RejectReason::NoCommonSuite),
            3 => Ok(RejectReason::Busy),
            4 => Ok(RejectReason::Unauthorized),
            5 => Ok(RejectReason::Locked),
            other => Err(Error::UnknownReason(other)),
        }
    }
}

/// 控制消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Control {
    /// 控制端发起。
    Hello {
        /// 发起方的协议版本。
        version: u8,
        /// 发起方的设备标识。
        device_id: u64,
        /// 发起方的编解码能力。
        caps: Caps,
        /// 发起方所在区域。
        region: String,
        /// 发起方允许的加密套件，按偏好排列。
        suites: Vec<Suite>,
    },
    /// 被控端接受。
    Accept {
        /// 选中的加密套件。
        suite: Suite,
        /// 会话标识。
        session_id: u32,
        /// 两端共同的编解码能力。
        caps: Caps,
        /// 被控端的电脑名（扩展字段，老版本不发；控制端用来在设备列表里显示）。空表示没有。
        name: String,
    },
    /// 被控端拒绝。
    Reject {
        /// 原因。
        reason: RejectReason,
    },
    /// 探活。
    Ping {
        /// 回显用的随机数。
        nonce: u64,
    },
    /// 探活应答。
    Pong {
        /// 原样返回的随机数。
        nonce: u64,
    },
    /// 结束会话。
    Bye,
    /// 密码交换消息。被控端先发（表示需要密码），控制端回一条。
    AuthStart {
        /// SPAKE2 消息。
        msg: Vec<u8>,
    },
    /// 密码交换完成后的确认码。
    AuthProof {
        /// HMAC-SHA256 确认码。
        proof: [u8; 32],
    },
}

impl Control {
    /// 消息名，用于日志和错误信息。
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Control::Hello { .. } => "Hello",
            Control::Accept { .. } => "Accept",
            Control::Reject { .. } => "Reject",
            Control::Ping { .. } => "Ping",
            Control::Pong { .. } => "Pong",
            Control::Bye => "Bye",
            Control::AuthStart { .. } => "AuthStart",
            Control::AuthProof { .. } => "AuthProof",
        }
    }

    /// 编码。
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer::new();
        match self {
            Control::Hello {
                version,
                device_id,
                caps,
                region,
                suites,
            } => {
                let count = u8::try_from(suites.len())
                    .ok()
                    .filter(|c| usize::from(*c) <= MAX_SUITES)
                    .ok_or(Error::TooManyItems {
                        len: suites.len(),
                        max: MAX_SUITES,
                    })?;
                w.u8(1).u8(*version).u64(*device_id).u32(caps.0);
                w.string(region, MAX_REGION_LEN)?;
                w.u8(count);
                for suite in suites {
                    w.u8(suite.id());
                }
            }
            Control::Accept {
                suite,
                session_id,
                caps,
                name,
            } => {
                w.u8(2).u8(suite.id()).u32(*session_id).u32(caps.0);
                // 没有名字时不写，字节和老版本完全一样
                if !name.is_empty() {
                    w.string(&clip_name(name), MAX_NAME_LEN)?;
                }
            }
            Control::Reject { reason } => {
                w.u8(3).u8(reason.id());
            }
            Control::Ping { nonce } => {
                w.u8(4).u64(*nonce);
            }
            Control::Pong { nonce } => {
                w.u8(5).u64(*nonce);
            }
            Control::Bye => {
                w.u8(6);
            }
            Control::AuthStart { msg } => {
                let len = u8::try_from(msg.len())
                    .ok()
                    .filter(|l| usize::from(*l) <= crate::auth::MAX_PAKE_MSG)
                    .ok_or(Error::PayloadTooLarge {
                        len: msg.len(),
                        max: crate::auth::MAX_PAKE_MSG,
                    })?;
                w.u8(7).u8(len).bytes(msg);
            }
            Control::AuthProof { proof } => {
                w.u8(8).bytes(proof);
            }
        }
        Ok(w.into_vec())
    }

    /// 解码。要求恰好用完全部字节。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let msg = match r.u8()? {
            1 => {
                let version = r.u8()?;
                let device_id = r.u64()?;
                let caps = Caps(r.u32()?);
                let region = r.string(MAX_REGION_LEN)?;
                let count = usize::from(r.u8()?);
                if count > MAX_SUITES {
                    return Err(Error::TooManyItems {
                        len: count,
                        max: MAX_SUITES,
                    });
                }
                let mut suites = Vec::with_capacity(count);
                for _ in 0..count {
                    suites.push(Suite::from_id(r.u8()?)?);
                }
                Control::Hello {
                    version,
                    device_id,
                    caps,
                    region,
                    suites,
                }
            }
            2 => Control::Accept {
                suite: Suite::from_id(r.u8()?)?,
                session_id: r.u32()?,
                caps: Caps(r.u32()?),
                // 读不成名字（更新版本换了扩展格式、或对方发错）就当没有：名字只用来显示，
                // 不能因此让握手失败。
                name: r.string(MAX_NAME_LEN).unwrap_or_default(),
            },
            3 => Control::Reject {
                reason: RejectReason::from_id(r.u8()?)?,
            },
            4 => Control::Ping { nonce: r.u64()? },
            5 => Control::Pong { nonce: r.u64()? },
            6 => Control::Bye,
            7 => {
                let len = usize::from(r.u8()?);
                if len > crate::auth::MAX_PAKE_MSG {
                    return Err(Error::PayloadTooLarge {
                        len,
                        max: crate::auth::MAX_PAKE_MSG,
                    });
                }
                Control::AuthStart {
                    msg: r.take(len)?.to_vec(),
                }
            }
            8 => {
                let mut proof = [0u8; 32];
                proof.copy_from_slice(r.take(32)?);
                Control::AuthProof { proof }
            }
            other => return Err(Error::UnknownMessage(other)),
        };
        // 握手消息可以带扩展字段：老版本读到自己认识的部分就够了，后面的忽略。
        // 其他消息要求恰好用完全部字节。
        if !matches!(msg, Control::Hello { .. } | Control::Accept { .. }) {
            r.finish()?;
        }
        Ok(msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_id_bit_survives_the_os_bits() {
        // 第 24–26 位是被控端的系统；探测组号的能力位在第 31 位，带上哪个系统都不受影响
        for os in [PeerOs::Unknown, PeerOs::Windows, PeerOs::Mac, PeerOs::Linux] {
            let caps = Caps::ALL.with_os(os);
            assert!(caps.has(Caps::PROBE_ID));
            assert_eq!(caps.os(), os);
        }
        assert_eq!(Caps::PROBE_ID.0, 1 << 31);
        // 握手的能力位怎么流：被控端取自己和控制端 Hello 的交集（Hello 不带系统位），Accept 里带上自己的系统；
        // 控制端取自己和 Accept 的交集、保留被控端的系统。被控端是哪个系统，两边看到的 PROBE_ID 都对；
        // PROBE_TRAIN（第 24 位）只有被控端读得准——控制端那边这一位是系统位，Mac 是 0（所以控制端不许读它，
        // 见 peerdesk_net::controller）。
        for os in [PeerOs::Windows, PeerOs::Mac, PeerOs::Linux] {
            let host = Caps::ALL.common(Caps::ALL);
            assert!(host.has(Caps::PROBE_TRAIN) && host.has(Caps::PROBE_ID));
            let accept = host.with_os(os);
            let controller = Caps::ALL.common(accept).with_os(accept.os());
            assert!(controller.has(Caps::PROBE_ID), "{os:?}");
            assert_eq!(controller.os(), os);
            assert_eq!(
                controller.has(Caps::PROBE_TRAIN),
                os != PeerOs::Mac,
                "控制端读到的第 24 位是系统位"
            );
            // 对端是老版本（没有 PROBE_ID）：两边都协商不出来
            let old = Caps(Caps::ALL.0 & !Caps::PROBE_ID.0);
            assert!(!old.common(Caps::ALL).has(Caps::PROBE_ID));
            let from_old_host = Caps::ALL.common(old.with_os(os)).with_os(os);
            assert!(!from_old_host.has(Caps::PROBE_ID));
        }
        // 老版本没有这一位：协商出来就没有
        let old = Caps(Caps::ALL.0 & !Caps::PROBE_ID.0);
        assert!(!Caps::ALL.common(old).has(Caps::PROBE_ID));
    }

    #[test]
    fn long_names_are_clipped_on_char_boundaries() {
        assert_eq!(clip_name("abc"), "abc");
        let long = "电脑".repeat(20); // 120 字节
        let clipped = clip_name(&long);
        assert!(clipped.len() <= MAX_NAME_LEN && clipped.len() == 63);
        let wire = Control::Accept {
            suite: Suite::Aes256Gcm,
            session_id: 1,
            caps: Caps(0),
            name: long,
        };
        let back = Control::decode(&wire.encode().unwrap()).unwrap();
        assert!(matches!(back, Control::Accept { name, .. } if name == clipped));
    }

    fn samples() -> Vec<Control> {
        vec![
            Control::Hello {
                version: VERSION,
                device_id: 42,
                caps: Caps::H264.with(Caps::AV1),
                region: "us-east".into(),
                suites: vec![Suite::Aes256Gcm, Suite::None],
            },
            Control::Accept {
                suite: Suite::ChaCha20Poly1305,
                session_id: 7,
                caps: Caps::H264,
                name: String::new(),
            },
            Control::Accept {
                suite: Suite::Aes256Gcm,
                session_id: 8,
                caps: Caps::H264,
                name: "张三的 MacBook Pro".into(),
            },
            Control::Reject {
                reason: RejectReason::Busy,
            },
            Control::Ping { nonce: 1 },
            Control::Pong { nonce: u64::MAX },
            Control::Bye,
            Control::AuthStart { msg: vec![7; 33] },
            Control::AuthStart { msg: vec![] },
            Control::AuthProof { proof: [9; 32] },
        ]
    }

    #[test]
    fn control_roundtrip() {
        let mut names = Vec::new();
        for msg in samples() {
            let bytes = msg.encode().unwrap();
            assert_eq!(Control::decode(&bytes).unwrap(), msg);
            names.push(msg.name());
        }
        assert_eq!(
            names,
            [
                "Hello",
                "Accept",
                "Accept",
                "Reject",
                "Ping",
                "Pong",
                "Bye",
                "AuthStart",
                "AuthStart",
                "AuthProof"
            ]
        );
        assert!(matches!(
            Control::AuthStart { msg: vec![0; 65] }.encode(),
            Err(Error::PayloadTooLarge { len: 65, max: 64 })
        ));
        assert!(matches!(
            Control::decode(&[7, 65]),
            Err(Error::PayloadTooLarge { len: 65, max: 64 })
        ));
        assert!(matches!(
            Control::decode(&[7, 5, 1]),
            Err(Error::Truncated { .. })
        ));
        assert!(matches!(
            Control::decode(&[8, 1, 2]),
            Err(Error::Truncated { .. })
        ));
    }

    #[test]
    fn channel_properties() {
        for (i, ch) in Channel::ALL.into_iter().enumerate() {
            assert_eq!(Channel::from_id(ch.id()).unwrap(), ch);
            assert_eq!(usize::from(ch.id()), i);
        }
        // 优先级：输入 > 控制 > 光标 > 声音 > 画面 > 剪贴板 > 文件
        let order = [
            Channel::Input,
            Channel::Control,
            Channel::Cursor,
            Channel::Audio,
            Channel::Video,
            Channel::Clipboard,
            Channel::File,
        ];
        for (i, ch) in order.into_iter().enumerate() {
            assert_eq!(usize::from(ch.priority()), i, "{ch:?}");
        }
        assert!(Channel::Audio.priority() < Channel::Video.priority());
        assert!(Channel::Input.priority() < Channel::Audio.priority());
        assert!(!Channel::Audio.reliable());
        assert!(Channel::File.reliable());
        assert!(Channel::Control.reliable());
        assert!(Channel::Clipboard.reliable());
        assert!(!Channel::Video.reliable());
        assert!(!Channel::Input.reliable());
        assert!(!Channel::Cursor.reliable());
        assert_eq!(Channel::from_id(7), Err(Error::UnknownChannel(7)));
    }

    #[test]
    fn reject_reason_ids() {
        for reason in RejectReason::ALL {
            assert_eq!(RejectReason::from_id(reason.id()).unwrap(), reason);
        }
        assert_eq!(RejectReason::from_id(0), Err(Error::UnknownReason(0)));
    }

    #[test]
    fn caps_set_operations() {
        let a = Caps::H264.with(Caps::H265);
        let b = Caps::H265.with(Caps::YUV444);
        assert_eq!(a.common(b), Caps::H265);
        assert!(a.has(Caps::H264));
        assert!(!a.has(Caps::AV1));
        assert!(a.has(Caps::default()));
    }

    #[test]
    fn datagram_roundtrip() {
        let header = Header {
            channel: Channel::Video,
            flags: 0,
            conn_id: 0xdead_beef,
            seq: 9,
            send_time: Time(123_456),
        };
        let bytes = encode_datagram(&header, b"abc").unwrap();
        assert_eq!(bytes.len(), HEADER_LEN + 3);
        let (decoded, payload) = decode_datagram(&bytes).unwrap();
        assert_eq!(decoded, header);
        assert_eq!(payload, b"abc");
    }

    #[test]
    fn video_header_roundtrip() {
        let vh = VideoHeader {
            frame_id: 1,
            frag_index: 2,
            frag_count: 3,
            ref_frame: 0,
        };
        let mut w = Writer::new();
        vh.encode(&mut w);
        let bytes = w.into_vec();
        assert_eq!(bytes.len(), VIDEO_HEADER_LEN);
        assert_eq!(VideoHeader::decode(&mut Reader::new(&bytes)).unwrap(), vh);
    }
}
