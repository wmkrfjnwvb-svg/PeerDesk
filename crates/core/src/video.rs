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
use crate::frame::{Frame, Picture};
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

    /// 编一帧 [`Frame`]：CPU 画面交给 [`Self::encode`]；显卡画面只有能直接用它的编码器才支持
    /// （默认报错，不悄悄在 CPU 上转换）。
    ///
    /// # Errors
    /// 编码出错，或者这个编码器不能用显卡画面。
    fn encode_frame(
        &mut self,
        frame_id: u32,
        frame: &Frame,
        changed: bool,
        budget: usize,
    ) -> Result<Option<EncodedFrame>, Error> {
        match frame {
            Frame::Rgb(screen) => self.encode(frame_id, screen, changed, budget),
            Frame::Gpu(_) => Err(Error::InvalidArgument(
                "这个编码器不能直接用显卡画面（对方版本太旧、不支持 H.264？）",
            )),
        }
    }

    /// 接收端的确认：帧号不大于 `floor` 的帧都有了定论，`mask` 的第 i 位表示第 `floor - i` 帧已送达。
    fn on_feedback(&mut self, floor: u32, mask: u32);

    /// 发送端判定这一帧丢了。
    fn on_lost(&mut self, frame_id: u32);

    /// 画面静止之后是否还要继续编（补清晰度）。为真时即使画面没变，发送端也要按时调用 [`Self::encode`]。
    fn wants_refine(&self) -> bool;

    /// 按源画面的多少（千分比，1000 为原尺寸）编码。由发送端按网络决定（见 [`crate::media::MediaSender`]）；
    /// 尺寸变了的那一帧是关键帧。不支持缩放的编码器忽略。
    fn set_scale(&mut self, _permille: u16) {}

    /// 控制端显示画面的区域（像素）；编码尺寸不超过它（等比缩小）。`None` 表示不限。
    /// 尺寸变了的那一帧是关键帧。不支持缩放的编码器忽略。
    fn set_view(&mut self, _view: Option<(u16, u16)>) {}

    /// 码率估计是不是“过期”了：好一阵没有测量证实它（静止时链路可能已经变差，发送端不知道）。
    /// 过期时编码器不按大帧时限放宽（关键帧、静止后第一帧变化最多一帧的预算），先发粗的、之后补清晰。
    /// 照 TCP 拥塞窗口验证（RFC 2861、Linux `tcp_slow_start_after_idle`）：闲置后不信闲置前的窗口。
    fn set_rate_stale(&mut self, _stale: bool) {}

    /// 现在用的编码器（“详情”面板、日志用）。
    fn backend(&self) -> String {
        format!("{:?}", self.codec())
    }

    /// 上一帧花在颜色转换（RGB → YUV，交给硬件时还有 NV12 打包）上的微秒数。
    /// 和编码总耗时对照，看时间花在转换还是编码器本身。没有单独转换步骤的编码器为 0。
    fn convert_micros(&self) -> u64 {
        0
    }

    /// 上一次编码各步骤的耗时和情况（诊断日志用）。
    fn timing(&self) -> EncodeTiming {
        EncodeTiming::default()
    }
}

/// 一次编码各步骤的耗时和情况（诊断日志用：看时间花在缩放、颜色转换还是编码器本身）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EncodeTiming {
    /// 缩放（CPU 上把采集的画面缩到要编的尺寸）的微秒数；采集时已经缩好时为 0。
    pub scale_us: u64,
    /// 颜色转换（RGB → YUV、打包 NV12）的微秒数；采集直接交 NV12 时只是拷贝。
    pub convert_us: u64,
    /// 编码器本身（交给硬件编码器并取回、或软件编码）的微秒数。
    pub encode_us: u64,
    /// 其中在等显卡的微秒数（把显卡画面读回内存：等显卡把活干完、等它的锁），已经包含在 `convert_us` 里。
    /// 这段时间 CPU 是闲的，判断“CPU 跟不上”时要扣掉（2026-11-27 用户日志：读回等锁 200 ms 一帧，
    /// 被算成“编码占用 94%”，帧率 60 → 30 → 20）。
    pub wait_us: u64,
    /// 用的是硬件编码器。
    pub hardware: bool,
    /// 硬件编码器的码控把这一帧丢了（预算放不下）：发送端记成网络受限，不能当成“没东西要发”。
    pub dropped: bool,
    /// 采集直接交来尺寸正好的 NV12，没有在 CPU 上转换。
    pub nv12_direct: bool,
    /// 编出来的尺寸。
    pub width: u16,
    /// 编出来的高度。
    pub height: u16,
    /// 这次出了关键帧的话，为什么（空表示不是关键帧）。
    pub key_reason: &'static str,
}

impl EncodeTiming {
    /// 这一帧 CPU 真正在忙的时间：整个任务用了 `took_us`，扣掉等显卡的部分。
    /// “CPU 跟不上”只按它判断（一个信号只表示一件事：等锁、等读回不是算不过来）。
    #[must_use]
    pub const fn busy_us(&self, took_us: u64) -> u64 {
        took_us.saturating_sub(self.wait_us)
    }
}

/// 视频解码器。
pub trait VideoDecoder: Send {
    /// 解一帧（不带帧头的码流）。返回解出的画面；`None` 表示这一帧没有可显示的新画面。
    fn decode(&mut self, header: &FrameHeader, data: &[u8]) -> Result<Option<&Screen>, Error>;

    /// 最近解出的画面。
    fn screen(&self) -> Option<&Screen>;

    /// 解一帧，交出要显示的画面：能给 NV12 的解码器直接给（界面在显卡上转颜色，CPU 不转），
    /// 其他给 RGB。默认按 [`Self::decode`] 的 RGB 结果。
    ///
    /// # Errors
    /// 同 [`Self::decode`]。
    fn decode_picture(
        &mut self,
        header: &FrameHeader,
        data: &[u8],
    ) -> Result<Option<Picture>, Error> {
        Ok(self
            .decode(header, data)?
            .map(|s| Picture::Rgb(std::sync::Arc::new(s.clone()))))
    }

    /// 现在用的解码器（“详情”面板、日志用）。
    fn backend(&self) -> String {
        "软件".to_owned()
    }
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

    /// 默认的 `encode_frame`：CPU 画面照常编；显卡画面报错，不悄悄在 CPU 上转换。
    #[test]
    fn default_encode_frame_takes_cpu_frames_only() {
        use crate::frame::{Frame, Nv12, TestGpuFrame};
        let mut enc = TileEncoder::default();
        let rgb = Frame::rgb(Screen::new(32, 32, 0x10_2030));
        assert!(enc.encode_frame(1, &rgb, true, 100_000).unwrap().is_some());
        let nv12 = Nv12::new(2, 2, vec![16; 6]).unwrap();
        let gpu = Frame::Gpu(std::sync::Arc::new(TestGpuFrame::new(nv12)));
        assert!(enc.encode_frame(2, &gpu, true, 100_000).is_err());
    }

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
        // 块编码没有单独的颜色转换步骤
        assert_eq!(TileEncoder::new().convert_micros(), 0);
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
        assert!(dec.screen().is_none());
        let (h, data) = FrameHeader::parse(&first.payload).unwrap();
        assert_eq!(dec.decode(&h, data).unwrap().unwrap(), &screen);
        assert_eq!(dec.screen(), Some(&screen));
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
