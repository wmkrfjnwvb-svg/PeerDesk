//! 被控端每秒发给控制端的状态（控制端“详情”面板显示对方的编码器、编码耗时等）。
//!
//! 走控制通道的数据报，开头一个标记字节区分于反馈。只在双方都声明 [`crate::proto::Caps::HOST_STATS`]
//! 时发（老版本收到不认识的数据报只会计为异常数据报，但我们不发给它）。解码容忍末尾多出的字段，
//! 以后可以往后加。

use crate::error::Error;
use crate::wire::{Reader, Writer};

/// 标记字节。
pub const TAG: u8 = 0xA5;
const MAX_TEXT: usize = 64;
/// 没有测到时的值。
pub const UNKNOWN: u16 = u16::MAX;

/// 被控端的状态。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostStats {
    /// 编码器（例如“硬件 NVIDIA H.264 Encoder MFT”“软件 OpenH264”）。
    pub encoder: String,
    /// 采集的分辨率。
    pub width: u16,
    /// 采集的分辨率。
    pub height: u16,
    /// 这一秒发出的帧数。
    pub fps: u16,
    /// 帧率上限（帧率调节给的）。
    pub fps_cap: u16,
    /// 编码一帧的耗时（毫秒）。
    pub encode_ms: u16,
    /// 目标码率（kbps）。
    pub target_kbps: u32,
    /// 这一秒实际发出的画面码率（kbps）。
    pub sent_kbps: u32,
    /// 纠错比例（千分比）。
    pub fec_permille: u16,
    /// 码率控制画像（stable、wireless 等）。
    pub profile: String,
    /// 被控端到它的路由器的往返时间（毫秒）；[`UNKNOWN`] 表示没测到。
    pub router_ms: u16,
}

fn clip(s: &str) -> &str {
    let mut end = s.len().min(MAX_TEXT);
    while !s.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    s.get(..end).unwrap_or_default()
}

impl HostStats {
    /// 编码成数据报载荷。
    ///
    /// # Errors
    /// 不会出错（文字已截到上限）；保留错误类型以便以后加字段。
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer::new();
        w.u8(TAG).u8(1);
        w.string(clip(&self.encoder), MAX_TEXT)?;
        w.u16(self.width)
            .u16(self.height)
            .u16(self.fps)
            .u16(self.fps_cap)
            .u16(self.encode_ms);
        w.u32(self.target_kbps)
            .u32(self.sent_kbps)
            .u16(self.fec_permille);
        w.string(clip(&self.profile), MAX_TEXT)?;
        w.u16(self.router_ms);
        Ok(w.into_vec())
    }

    /// 解码。不是状态数据报（标记不对）或数据不完整时出错；末尾多出的字段忽略。
    ///
    /// # Errors
    /// 见上。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let tag = r.u8()?;
        if tag != TAG {
            return Err(Error::UnknownMessage(tag));
        }
        let _version = r.u8()?;
        Ok(Self {
            encoder: r.string(MAX_TEXT)?,
            width: r.u16()?,
            height: r.u16()?,
            fps: r.u16()?,
            fps_cap: r.u16()?,
            encode_ms: r.u16()?,
            target_kbps: r.u32()?,
            sent_kbps: r.u32()?,
            fec_permille: r.u16()?,
            profile: r.string(MAX_TEXT)?,
            router_ms: r.u16()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tolerance() {
        let s = HostStats {
            encoder: "硬件 NVIDIA H.264 Encoder MFT".into(),
            width: 1920,
            height: 1080,
            fps: 58,
            fps_cap: 60,
            encode_ms: 4,
            target_kbps: 12_000,
            sent_kbps: 3_100,
            fec_permille: 150,
            profile: "wireless".into(),
            router_ms: 3,
        };
        let mut bytes = s.encode().unwrap();
        assert_eq!(HostStats::decode(&bytes).unwrap(), s);
        // 以后加的字段：老版本忽略
        bytes.extend_from_slice(&[1, 2, 3]);
        assert_eq!(HostStats::decode(&bytes).unwrap(), s);
        // 不是状态数据报 / 不完整
        assert!(HostStats::decode(&[0x00, 1]).is_err());
        assert!(HostStats::decode(&bytes[..10]).is_err());
        // 太长的名字截断（不切断字符）
        let long = HostStats {
            encoder: "编".repeat(40),
            ..HostStats::default()
        };
        let back = HostStats::decode(&long.encode().unwrap()).unwrap();
        assert!(back.encoder.len() <= MAX_TEXT && back.encoder.chars().all(|c| c == '编'));
    }
}
