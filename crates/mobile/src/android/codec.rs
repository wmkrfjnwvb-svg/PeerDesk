//! MediaCodec 解码到 Surface：系统硬件解码器，解完直接交给系统合成到屏幕，像素不经过我们（见 `R-android.md` 1.1）。
//!
//! - 打开：先试系统默认的 H.264 解码器（一般是硬件的），打不开再试系统自带的软件解码器（厂商付过专利许可）。
//!   每个解码器按 [`decoder_setup::options`] 从最激进的低延迟设置试起，`configure` 失败就退一档。
//! - 关键帧带着 SPS、PPS：用它打开解码器（`csd-0`、`csd-1`）；画面尺寸变了就重开。没打开前的增量帧不解（等关键帧）。
//! - 解一帧：送进输入缓冲，然后等一小会儿取输出、立刻画到屏幕上（`render = true`）。出错时关掉解码器、报错，
//!   控制端会请求关键帧，下一个关键帧重新打开。
#![allow(unsafe_code)]

use std::time::Duration;

use ndk::media::media_codec::{
    DequeuedInputBufferResult, DequeuedOutputBufferInfoResult, MediaCodec, MediaCodecDirection,
    MediaFormat,
};
use ndk::native_window::NativeWindow;
use peerdesk_core::error::Error;
use peerdesk_core::frame::Picture;
use peerdesk_core::screen::Screen;
use peerdesk_core::video::{Codec, FrameHeader, VideoDecoder};

use crate::decoder_setup::{self, codec_params, is_software, SOFTWARE_DECODERS, TRIES};

const MIME: &str = "video/avc";
/// 等输入缓冲最多多久：解码器满了说明它跟不上，等太久会拖住整个接收。
const INPUT_WAIT: Duration = Duration::from_millis(20);
/// 送进一帧后等它出来最多多久（硬件解码一帧一般几毫秒）；没等到就下一帧再取，不丢。
const OUTPUT_WAIT: Duration = Duration::from_millis(10);
/// 时间戳每帧加多少（微秒）：解码器只要求单调递增。
const FRAME_US: u64 = 16_667;

/// 打开了的解码器。
struct Open {
    codec: MediaCodec,
    name: String,
    size: (u16, u16),
}

// SAFETY: `AMediaCodec` 没有线程亲和性要求；它整个归这个解码器所有、只在解码线程上用（DecodeWorker 独占），
// 从创建它的线程移交到解码线程是安全的。不实现 Sync：从不跨线程共享。
unsafe impl Send for Open {}

impl Drop for Open {
    fn drop(&mut self) {
        let _ = self.codec.stop();
    }
}

/// 系统解码器，画面直接画到 `window`（界面的 SurfaceView）。
pub struct SurfaceDecoder {
    window: NativeWindow,
    open: Option<Open>,
    pts_us: u64,
    /// 打开失败的原因（日志和工具条用）。
    last_error: Option<String>,
}

impl std::fmt::Debug for SurfaceDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurfaceDecoder")
            .field("decoder", &self.open.as_ref().map(|o| o.name.as_str()))
            .finish_non_exhaustive()
    }
}

impl SurfaceDecoder {
    /// 画到 `window` 上。解码器在第一个关键帧来时才打开。
    #[must_use]
    pub const fn new(window: NativeWindow) -> Self {
        Self {
            window,
            open: None,
            pts_us: 0,
            last_error: None,
        }
    }

    /// 打开一个解码器：先默认的，再系统软件解码器；每个按低延迟设置从多到少试。
    fn open(&self, size: (u16, u16), sps: &[u8], pps: &[u8]) -> Result<Open, String> {
        let default_name = MediaCodec::from_decoder_type(MIME).and_then(|c| c.name().ok());
        let names: Vec<String> = default_name
            .into_iter()
            .chain(SOFTWARE_DECODERS.iter().map(|n| (*n).to_owned()))
            .collect();
        let mut why = String::from("系统没有 H.264 解码器");
        for name in names {
            for attempt in 0..TRIES {
                let Some(codec) = MediaCodec::from_codec_name(&name) else {
                    why = format!("打不开解码器 {name}");
                    break;
                };
                let mut format = MediaFormat::new();
                format.set_str("mime", MIME);
                format.set_i32("width", i32::from(size.0));
                format.set_i32("height", i32::from(size.1));
                format.set_buffer("csd-0", sps);
                format.set_buffer("csd-1", pps);
                for (key, value) in decoder_setup::options(&name, attempt) {
                    format.set_i32(key, value);
                }
                let started = codec
                    .configure(&format, Some(&self.window), MediaCodecDirection::Decoder)
                    .and_then(|()| codec.start());
                match started {
                    Ok(()) => {
                        log::info!("视频解码：{name}（低延迟设置第 {attempt} 档）");
                        return Ok(Open { codec, name, size });
                    }
                    Err(e) => why = format!("{name} 第 {attempt} 档打不开：{e:?}"),
                }
            }
        }
        Err(why)
    }

    /// 取出所有已经解完的帧画到屏幕上，返回画了几帧。第一次等一小会儿，之后不等。
    fn drain(open: &Open) -> Result<u32, String> {
        let mut shown = 0u32;
        let mut wait = OUTPUT_WAIT;
        loop {
            match open.codec.dequeue_output_buffer(wait) {
                Ok(DequeuedOutputBufferInfoResult::Buffer(buffer)) => {
                    open.codec
                        .release_output_buffer(buffer, true)
                        .map_err(|e| format!("画到屏幕失败：{e:?}"))?;
                    shown = shown.saturating_add(1);
                    wait = Duration::ZERO;
                }
                Ok(DequeuedOutputBufferInfoResult::TryAgainLater) => return Ok(shown),
                Ok(
                    DequeuedOutputBufferInfoResult::OutputFormatChanged
                    | DequeuedOutputBufferInfoResult::OutputBuffersChanged,
                ) => {}
                Err(e) => return Err(format!("取解码结果失败：{e:?}")),
            }
        }
    }

    fn feed(&mut self, data: &[u8]) -> Result<u32, String> {
        let Some(open) = self.open.as_ref() else {
            return Ok(0);
        };
        match open.codec.dequeue_input_buffer(INPUT_WAIT) {
            Ok(DequeuedInputBufferResult::Buffer(mut buffer)) => {
                let dst = buffer.buffer_mut();
                if dst.len() < data.len() {
                    return Err(format!(
                        "一帧 {} 字节，输入缓冲只有 {}",
                        data.len(),
                        dst.len()
                    ));
                }
                for (d, s) in dst.iter_mut().zip(data) {
                    d.write(*s);
                }
                open.codec
                    .queue_input_buffer(buffer, 0, data.len(), self.pts_us, 0)
                    .map_err(|e| format!("送进解码器失败：{e:?}"))?;
                self.pts_us = self.pts_us.saturating_add(FRAME_US);
            }
            // 解码器满了：这一帧送不进去，后面的帧也就没法接上，报错让控制端要关键帧
            Ok(DequeuedInputBufferResult::TryAgainLater) => {
                return Err("解码器忙，送不进去".to_owned());
            }
            Err(e) => return Err(format!("取输入缓冲失败：{e:?}")),
        }
        Self::drain(open)
    }
}

impl VideoDecoder for SurfaceDecoder {
    fn decode(&mut self, header: &FrameHeader, data: &[u8]) -> Result<Option<&Screen>, Error> {
        // 画面直接到屏幕上，没有像素可以交出去
        self.decode_picture(header, data).map(|_| None)
    }

    fn screen(&self) -> Option<&Screen> {
        None
    }

    fn decode_picture(
        &mut self,
        header: &FrameHeader,
        data: &[u8],
    ) -> Result<Option<Picture>, Error> {
        if header.codec != Codec::H264 {
            return Err(Error::InvalidArgument("手机端只解 H.264"));
        }
        let size = (header.width, header.height);
        if self.open.as_ref().is_none_or(|o| o.size != size) {
            if !header.key {
                // 还没打开（或者尺寸变了）：等关键帧
                return Ok(None);
            }
            let Some((sps, pps)) = codec_params(data) else {
                return Err(Error::Inconsistent("关键帧里没有 SPS、PPS"));
            };
            self.open = None;
            match self.open(size, &sps, &pps) {
                Ok(open) => {
                    self.open = Some(open);
                    self.last_error = None;
                }
                Err(why) => {
                    log::warn!("视频解码：{why}");
                    self.last_error = Some(why);
                    return Err(Error::Inconsistent("打不开系统的 H.264 解码器"));
                }
            }
        }
        match self.feed(data) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(Picture::Presented {
                width: size.0,
                height: size.1,
            })),
            Err(why) => {
                log::warn!("视频解码：{why}，关掉解码器，等关键帧重新打开");
                self.open = None;
                self.last_error = Some(why);
                Err(Error::Inconsistent("系统解码器出错"))
            }
        }
    }

    fn backend(&self) -> String {
        match &self.open {
            Some(o) if is_software(&o.name) => format!("系统软件解码 {}", o.name),
            Some(o) => format!("硬件解码 {}", o.name),
            None => self.last_error.clone().map_or_else(
                || "系统解码器（还没打开）".to_owned(),
                |e| format!("解码器打不开：{e}"),
            ),
        }
    }
}
