//! 视频编解码的统一接口和帧格式。
//!
//! 传输层只认这里的接口：每帧给编码器一个字节预算，告诉它哪些帧送达了、哪些丢了；
//! 编码器自己决定怎么在预算内编、丢了之后怎么恢复。这样块编码、H.264、以后的 AV1
//! 都能放进同一条管线、同一套端到端测试里比较。
//!
//! 每一帧的载荷前面有 [`FRAME_HEADER_LEN`] 字节的帧头：编码格式、是否关键帧、画面尺寸。
//! 接收端靠它在丢包之后找到能重新开始解码的帧。

use crate::codec;
use crate::error::Error;
use crate::screen::Screen;
use crate::wire::{Reader, Writer};

/// 帧头长度。
pub const FRAME_HEADER_LEN: usize = 6;

/// 编码格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Codec {
    /// 自研的块编码：无损、可以按块补发。用于仿真对照和不支持视频编码的场合。
    Tile,
    /// H.264。
    H264,
}

impl Codec {
    const fn id(self) -> u8 {
        match self {
            Codec::Tile => 0,
            Codec::H264 => 1,
        }
    }

    const fn from_id(id: u8) -> Result<Self, Error> {
        match id {
            0 => Ok(Codec::Tile),
            1 => Ok(Codec::H264),
            _ => Err(Error::UnknownMessage(id)),
        }
    }

    /// 名字，用于日志和报告。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Codec::Tile => "tile",
            Codec::H264 => "h264",
        }
    }
}

/// 帧头。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    /// 编码格式。
    pub codec: Codec,
    /// 是否关键帧：不依赖之前的任何帧，丢包之后可以从这里重新开始解码。
    pub key: bool,
    /// 画面宽度。
    pub width: u16,
    /// 画面高度。
    pub height: u16,
}

impl FrameHeader {
    /// 编码帧头，后面接着码流。
    #[must_use]
    pub fn frame(&self, data: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u8(self.codec.id())
            .u8(u8::from(self.key))
            .u16(self.width)
            .u16(self.height)
            .bytes(data);
        w.into_vec()
    }

    /// 拆出帧头和码流。
    pub fn parse(frame: &[u8]) -> Result<(Self, &[u8]), Error> {
        let mut r = Reader::new(frame);
        let codec = Codec::from_id(r.u8()?)?;
        let key = match r.u8()? {
            0 => false,
            1 => true,
            other => return Err(Error::UnknownMessage(other)),
        };
        let (width, height) = (r.u16()?, r.u16()?);
        if width == 0 || height == 0 {
            return Err(Error::Inconsistent("画面尺寸为 0"));
        }
        let data = frame.get(FRAME_HEADER_LEN..).unwrap_or_default();
        Ok((
            Self {
                codec,
                key,
                width,
                height,
            },
            data,
        ))
    }
}

/// 编码器的输出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// 带帧头的整帧载荷。
    pub payload: Vec<u8>,
    /// 是否关键帧。
    pub key: bool,
}

/// 视频编码器。
pub trait VideoEncoder: Send {
    /// 编码格式。
    fn codec(&self) -> Codec;

    /// 编一帧，尽量不超过 `budget` 字节。`changed` 表示画面和上一次交来的相比变了。
    /// 返回 `None` 表示这一帧不用发（画面没变，清晰度也已经补完）。
    fn encode(
        &mut self,
        frame_id: u32,
        screen: &Screen,
        changed: bool,
        budget: usize,
    ) -> Result<Option<EncodedFrame>, Error>;

    /// 接收端的确认：帧号不大于 `floor` 的帧都有了定论，`mask` 的第 i 位表示第 `floor - i` 帧已送达。
    fn on_feedback(&mut self, floor: u32, mask: u32);

    /// 发送端判定这一帧丢了。
    fn on_lost(&mut self, frame_id: u32);

    /// 画面静止之后是否还要继续编（补清晰度）。为真时即使画面没变，发送端也要按时调用 [`Self::encode`]。
    fn wants_refine(&self) -> bool;
}

/// 视频解码器。
pub trait VideoDecoder: Send {
    /// 解一帧（不带帧头的码流）。返回解出的画面；`None` 表示这一帧没有可显示的新画面。
    fn decode(&mut self, header: &FrameHeader, data: &[u8]) -> Result<Option<&Screen>, Error>;

    /// 最近解出的画面。
    fn screen(&self) -> Option<&Screen>;
}

/// 块编码的适配。
#[derive(Debug)]
pub struct TileEncoder {
    inner: Option<(u16, u16, codec::Encoder)>,
}

impl TileEncoder {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self { inner: None }
    }
}

impl Default for TileEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoEncoder for TileEncoder {
    fn codec(&self) -> Codec {
        Codec::Tile
    }

    fn encode(
        &mut self,
        frame_id: u32,
        screen: &Screen,
        _changed: bool,
        budget: usize,
    ) -> Result<Option<EncodedFrame>, Error> {
        let size = (screen.width(), screen.height());
        // 尺寸变了（或第一帧）：换一个新的编码器，从整屏开始。
        let fresh = self.inner.as_ref().is_none_or(|(w, h, _)| (*w, *h) != size);
        if fresh {
            self.inner = None;
        }
        let (_, _, enc) = self
            .inner
            .get_or_insert_with(|| (size.0, size.1, codec::Encoder::new(size.0, size.1)));
        let header = FrameHeader {
            codec: Codec::Tile,
            key: fresh,
            width: size.0,
            height: size.1,
        };
        Ok(enc
            .encode(frame_id, screen, budget.saturating_sub(FRAME_HEADER_LEN))
            .map(|encoded| EncodedFrame {
                payload: header.frame(&encoded.data),
                key: fresh,
            }))
    }

    fn on_feedback(&mut self, floor: u32, mask: u32) {
        if let Some((_, _, enc)) = self.inner.as_mut() {
            enc.on_feedback(floor, mask);
        }
    }

    fn on_lost(&mut self, frame_id: u32) {
        if let Some((_, _, enc)) = self.inner.as_mut() {
            enc.mark_lost(frame_id);
        }
    }

    fn wants_refine(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|(_, _, enc)| enc.pending() > 0)
    }
}

/// 块解码的适配。块编码的每一帧都能独立应用，所以不需要等关键帧。
#[derive(Debug, Default)]
pub struct TileDecoder {
    inner: Option<codec::Decoder>,
}

impl TileDecoder {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self { inner: None }
    }
}

impl VideoDecoder for TileDecoder {
    fn decode(&mut self, header: &FrameHeader, data: &[u8]) -> Result<Option<&Screen>, Error> {
        let size_ok = self.inner.as_ref().is_some_and(|d| {
            (d.screen().width(), d.screen().height()) == (header.width, header.height)
        });
        if !size_ok {
            self.inner = None;
        }
        let dec = self
            .inner
            .get_or_insert_with(|| codec::Decoder::new(header.width, header.height));
        dec.apply(data)?;
        Ok(Some(dec.screen()))
    }

    fn screen(&self) -> Option<&Screen> {
        self.inner.as_ref().map(codec::Decoder::screen)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn header_round_trip_and_rejects_garbage() {
        for codec in [Codec::Tile, Codec::H264] {
            let h = FrameHeader {
                codec,
                key: codec == Codec::H264,
                width: 1920,
                height: 1080,
            };
            let framed = h.frame(b"abc");
            assert_eq!(framed.len(), FRAME_HEADER_LEN + 3);
            let (back, data) = FrameHeader::parse(&framed).unwrap();
            assert_eq!((back, data), (h, &b"abc"[..]));
            assert!(!codec.name().is_empty());
        }
        assert!(FrameHeader::parse(&[9, 0, 0, 1, 0, 1]).is_err(), "未知编码");
        assert!(
            FrameHeader::parse(&[0, 2, 0, 1, 0, 1]).is_err(),
            "关键帧标志只能是 0/1"
        );
        assert!(FrameHeader::parse(&[0, 0, 0, 0, 0, 1]).is_err(), "宽为 0");
        assert!(FrameHeader::parse(&[0, 0, 0, 1]).is_err(), "截断");
    }

    #[test]
    fn tile_codec_through_the_common_interface() {
        let mut enc = TileEncoder::default();
        let mut dec = TileDecoder::new();
        assert_eq!(enc.codec(), Codec::Tile);
        assert!(!enc.wants_refine());
        enc.on_feedback(0, 0);
        enc.on_lost(1);
        let mut screen = Screen::new(64, 64, 0x0011_2233);
        let first = enc
            .encode(1, &screen, true, usize::MAX / 2)
            .unwrap()
            .unwrap();
        assert!(first.key);
        let (h, data) = FrameHeader::parse(&first.payload).unwrap();
        assert_eq!(dec.decode(&h, data).unwrap().unwrap(), &screen);
        // 没变：不用发
        assert!(enc
            .encode(2, &screen, false, usize::MAX / 2)
            .unwrap()
            .is_none());
        // 变了一块：增量帧
        screen.fill(0, 0, 8, 8, 0x00ff_ffff);
        let delta = enc
            .encode(2, &screen, true, usize::MAX / 2)
            .unwrap()
            .unwrap();
        assert!(!delta.key);
        let (h, data) = FrameHeader::parse(&delta.payload).unwrap();
        assert_eq!(dec.decode(&h, data).unwrap().unwrap(), &screen);
        // 丢了这一帧：那一块重新待发
        enc.on_lost(2);
        assert!(enc.wants_refine());
        enc.on_feedback(2, 1);
        // 尺寸变了：新的关键帧，解码器跟着换
        let bigger = Screen::new(96, 64, 0x0044_5566);
        let k = enc
            .encode(3, &bigger, true, usize::MAX / 2)
            .unwrap()
            .unwrap();
        assert!(k.key);
        let (h, data) = FrameHeader::parse(&k.payload).unwrap();
        assert_eq!(dec.decode(&h, data).unwrap().unwrap(), &bigger);
        // 坏码流报错
        assert!(dec.decode(&h, &[1, 2, 3]).is_err());
    }
}
