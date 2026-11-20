//! PeerDesk 的视频编解码：实现 [`peerdesk_core::video`] 里的接口。
//!
//! 现在只有 H.264（OpenH264 软件编码，屏幕内容模式）。硬件编码（Media Foundation、VideoToolbox、
//! VA-API）和 AV1 以后加在这里，接口不变。
//!
//! 编码器听传输层的指挥：
//! - 每帧的字节预算换算成目标码率，码率变化超过一成才通知编码器（避免频繁调整）。
//! - 画面不变就不编；画面停下约 200 ms 后把最后一帧再编几次，只补残差，几帧之内补清晰
//!   （[`REFINE_FRAMES`]）。画面一直在变（打字、拖动）时不补：补了也马上被新内容盖掉，白费流量。
//! - 丢了帧：下一帧编关键帧。以后改为从长期参考帧恢复。

use openh264::decoder::Decoder;
use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Level,
    QpRange, RateControlMode, UsageType,
};
use openh264::formats::{YUVSlices, YUVSource};

mod color;
use openh264::OpenH264API;
use peerdesk_core::error::Error;
use peerdesk_core::screen::Screen;
use peerdesk_core::video::{Codec, EncodedFrame, FrameHeader, VideoDecoder, VideoEncoder};

/// 画面停下后最多再编几帧来补清晰。
pub const REFINE_FRAMES: u8 = 8;
/// 补清晰的一帧小于这么多字节，说明已经补不出什么了，提前停下。
const REFINE_DONE_BYTES: usize = 64;
/// 码率下限（比特每秒）：再低 OpenH264 的码率控制就不稳定了。
const MIN_BPS: u64 = 64_000;
/// 码率上限的默认值。
pub const DEFAULT_MAX_BPS: u64 = 50_000_000;
/// 画质下限对应的最大 QP：再糊就不像话了，宁可这一帧超出预算。
const MAX_QP: u8 = 42;
/// 最小 QP：补清晰时能达到的画质上限。
const MIN_QP: u8 = 12;

fn codec_error(what: &'static str) -> impl Fn(openh264::Error) -> Error {
    move |_| Error::Inconsistent(what)
}

/// OpenH264 要求宽高都是偶数：奇数时补一行（列），复制边上的像素。
fn even_pixels(screen: &Screen) -> (usize, usize, Vec<u32>) {
    let (w, h) = (usize::from(screen.width()), usize::from(screen.height()));
    let (ew, eh) = (w + w % 2, h + h % 2);
    if (ew, eh) == (w, h) {
        return (w, h, screen.pixels().to_vec());
    }
    let src = screen.pixels();
    let mut out = Vec::with_capacity(ew * eh);
    for y in 0..eh {
        let row = src.get(y.min(h - 1) * w..).unwrap_or_default();
        let row = row.get(..w).unwrap_or_default();
        out.extend_from_slice(row);
        if ew > w {
            out.push(row.last().copied().unwrap_or(0));
        }
    }
    (ew, eh, out)
}

/// H.264 编码器（OpenH264）。
pub struct H264Encoder {
    fps: u32,
    max_bps: u64,
    inner: Option<(u16, u16, Encoder)>,
    bps: u64,
    force_key: bool,
    refine_left: u8,
    /// 画面连续没变的帧数：够了 [`Self::refine_after`] 才开始补清晰。
    still: u32,
    /// 转换好的 Y、U、V 平面，每帧复用。
    planes: (Vec<u8>, Vec<u8>, Vec<u8>),
}

impl std::fmt::Debug for H264Encoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H264Encoder")
            .field("fps", &self.fps)
            .field("bps", &self.bps)
            .field("force_key", &self.force_key)
            .field("refine_left", &self.refine_left)
            .finish_non_exhaustive()
    }
}

impl H264Encoder {
    /// 创建。`fps` 是发送端的帧率，用来把每帧预算换算成码率。
    #[must_use]
    pub fn new(fps: u32) -> Self {
        Self::with_max_bps(fps, DEFAULT_MAX_BPS)
    }

    /// 创建，指定码率上限。
    #[must_use]
    pub fn with_max_bps(fps: u32, max_bps: u64) -> Self {
        Self {
            fps: fps.max(1),
            max_bps: max_bps.clamp(MIN_BPS, u64::from(u32::MAX)),
            inner: None,
            bps: 0,
            force_key: false,
            refine_left: 0,
            still: 0,
            planes: (Vec::new(), Vec::new(), Vec::new()),
        }
    }

    /// 打开编码器。
    ///
    /// openh264 的封装把每层的最大码率设成初始码率，之后码率只能往下调；所以按上限打开，
    /// 第一帧编完再按预算调下来（编第一帧之前 OpenH264 不接受调码率）。
    /// 画面停下多少帧之后开始补清晰（约 200 ms）。
    const fn refine_after(&self) -> u32 {
        self.fps / 5
    }

    fn open(&self) -> Result<Encoder, Error> {
        let bps = u32::try_from(self.max_bps).unwrap_or(u32::MAX);
        let fps = u16::try_from(self.fps).unwrap_or(u16::MAX);
        let config = EncoderConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(bps))
            .max_frame_rate(FrameRate::from_hz(f32::from(fps)))
            // 帧率由发送端决定；编码器自己跳帧会让画面停住。
            .skip_frames(false)
            // 屏幕内容模式不支持这两项，关掉免得 OpenH264 打警告。
            .adaptive_quantization(false)
            .background_detection(false)
            // 不定期插关键帧：关键帧只在开始和丢包恢复时出现。
            .intra_frame_period(IntraFramePeriod::from_num_frames(u32::MAX))
            .qp(QpRange::new(MIN_QP, MAX_QP))
            // 级别决定码率上限；不指定时按分辨率自动选，小画面的上限很低。
            .level(Level::Level_5_2)
            .complexity(Complexity::Low)
            .debug(false);
        Encoder::with_api_config(OpenH264API::from_source(), config)
            .map_err(codec_error("创建 H.264 编码器失败"))
    }

    /// 按每帧预算设码率。变化不到一成时不动，避免频繁调整。
    fn set_budget(&mut self, enc: &mut Encoder, budget: usize) {
        let bps = u64::try_from(budget)
            .unwrap_or(u64::MAX)
            .saturating_mul(8)
            .saturating_mul(u64::from(self.fps))
            .clamp(MIN_BPS, self.max_bps);
        let changed = bps.abs_diff(self.bps).saturating_mul(10) > self.bps;
        if !changed {
            return;
        }
        if set_bitrate(enc, bps) {
            self.bps = bps;
        }
    }
}

/// 运行中改码率：openh264 的安全接口没有这一项，只能走底层的 `SetOption`。返回是否生效。
#[allow(unsafe_code)]
fn set_bitrate(enc: &mut Encoder, bps: u64) -> bool {
    let mut info = openh264_sys2::SBitrateInfo {
        iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
        iBitrate: i32::try_from(bps).unwrap_or(i32::MAX),
    };
    // SAFETY: 编码器已经初始化；ENCODER_OPTION_BITRATE 的参数就是 SBitrateInfo，
    // 指针指向本函数里的局部变量，调用期间有效。这一项只改目标码率，不改编码器依赖的其他参数。
    let status = unsafe {
        enc.raw_api().set_option(
            openh264_sys2::ENCODER_OPTION_BITRATE,
            std::ptr::addr_of_mut!(info).cast(),
        )
    };
    status == 0
}

impl VideoEncoder for H264Encoder {
    fn codec(&self) -> Codec {
        Codec::H264
    }

    fn encode(
        &mut self,
        _frame_id: u32,
        screen: &Screen,
        changed: bool,
        budget: usize,
    ) -> Result<Option<EncodedFrame>, Error> {
        let size = (screen.width(), screen.height());
        let fresh = self.inner.as_ref().is_none_or(|(w, h, _)| (*w, *h) != size);
        if !fresh && !changed && !self.force_key {
            // 画面停下了：先等一会儿，确认真的停了再补清晰。
            self.still = self.still.saturating_add(1);
            if self.refine_left == 0 || self.still < self.refine_after() {
                return Ok(None);
            }
        } else {
            self.still = 0;
        }
        // 编码器先拿出来用，编完放回去；编码出错时丢掉，下一帧重新打开（从关键帧开始）。
        let mut enc = match self.inner.take() {
            Some((_, _, mut enc)) if !fresh => {
                self.set_budget(&mut enc, budget);
                enc
            }
            Some(_) | None => {
                // 新编码器按上限打开：第一帧编完才能调码率。
                self.bps = 0;
                self.open()?
            }
        };
        if self.force_key && !fresh {
            enc.force_intra_frame();
        }
        let (w, h, pixels) = even_pixels(screen);
        color::rgb_to_i420(
            &pixels,
            w,
            &mut self.planes.0,
            &mut self.planes.1,
            &mut self.planes.2,
        );
        let yuv = YUVSlices::new(
            (&self.planes.0, &self.planes.1, &self.planes.2),
            (w, h),
            (w, w / 2, w / 2),
        );
        let stream = enc.encode(&yuv).map_err(codec_error("H.264 编码失败"))?;
        let key = matches!(stream.frame_type(), FrameType::IDR | FrameType::I);
        let data = stream.to_vec();
        self.force_key = false;
        if fresh {
            self.set_budget(&mut enc, budget);
        }
        self.inner = Some((size.0, size.1, enc));
        // 画面在变：之后要补清晰；没变：这是补清晰的一帧。
        self.refine_left = if changed || fresh || key {
            REFINE_FRAMES
        } else if data.len() < REFINE_DONE_BYTES {
            0
        } else {
            self.refine_left.saturating_sub(1)
        };
        let header = FrameHeader {
            codec: Codec::H264,
            key,
            width: size.0,
            height: size.1,
        };
        Ok(Some(EncodedFrame {
            payload: header.frame(&data),
            key,
        }))
    }

    fn on_feedback(&mut self, _floor: u32, _mask: u32) {}

    fn on_lost(&mut self, _frame_id: u32) {
        // 之后的增量帧都参考了丢的那一帧，接收端解不了：下一帧编关键帧。
        self.force_key = true;
    }

    fn wants_refine(&self) -> bool {
        self.force_key || self.refine_left > 0
    }
}

/// H.264 解码器（OpenH264）。
pub struct H264Decoder {
    decoder: Decoder,
    rgb: Vec<u8>,
    screen: Option<Screen>,
}

impl std::fmt::Debug for H264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("H264Decoder").finish_non_exhaustive()
    }
}

impl H264Decoder {
    /// 创建。
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            decoder: Decoder::new().map_err(codec_error("创建 H.264 解码器失败"))?,
            rgb: Vec::new(),
            screen: None,
        })
    }
}

impl VideoDecoder for H264Decoder {
    fn decode(&mut self, header: &FrameHeader, data: &[u8]) -> Result<Option<&Screen>, Error> {
        let Some(yuv) = self
            .decoder
            .decode(data)
            .map_err(codec_error("H.264 解码失败"))?
        else {
            return Ok(None);
        };
        let (dw, dh) = yuv.dimensions();
        let (w, h) = (usize::from(header.width), usize::from(header.height));
        if dw < w || dh < h {
            return Err(Error::Inconsistent("解出的画面比帧头说的小"));
        }
        self.rgb.resize(dw * dh * 3, 0);
        yuv.write_rgb8(&mut self.rgb);
        let mut pixels = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = self
                .rgb
                .get(y * dw * 3..(y * dw + w) * 3)
                .unwrap_or_default();
            color::pack_rgb(row, &mut pixels);
        }
        self.screen = Some(Screen::from_pixels(header.width, header.height, pixels)?);
        Ok(self.screen.as_ref())
    }

    fn screen(&self) -> Option<&Screen> {
        self.screen.as_ref()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    /// 亮度的 PSNR：H.264 用 4:2:0，色度的损失不算在内。
    fn psnr(a: &Screen, b: &Screen) -> f64 {
        let luma = |p: u32| {
            0.299 * f64::from((p >> 16) & 0xff)
                + 0.587 * f64::from((p >> 8) & 0xff)
                + 0.114 * f64::from(p & 0xff)
        };
        let sum: f64 = a
            .pixels()
            .iter()
            .zip(b.pixels())
            .map(|(x, y)| (luma(*x) - luma(*y)).powi(2))
            .sum();
        let mse = sum / a.pixels().len() as f64;
        10.0 * (255.0f64 * 255.0 / mse.max(1e-9)).log10()
    }

    /// 带纹理的画面：渐变加噪点（像照片），上面一块白底“文字”。
    fn picture(w: u16, h: u16) -> Screen {
        let mut px = Vec::new();
        let mut seed = 7u64;
        for y in 0..u32::from(h) {
            for x in 0..u32::from(w) {
                seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let n = u32::try_from(seed >> 60).unwrap();
                let r = (x * 255 / u32::from(w) + n).min(255);
                let g = (y * 255 / u32::from(h) + n).min(255);
                px.push(r << 16 | g << 8 | (0x80 + n));
            }
        }
        let mut s = Screen::from_pixels(w, h, px).unwrap();
        s.fill(4, 4, w / 2, h / 2, 0x00ff_ffff);
        for c in 0..(w / 2 - 8) / 7 {
            s.fill(8 + c * 7, 8, 4, 6, 0x0030_3030);
        }
        s
    }

    fn decode(dec: &mut H264Decoder, frame: &EncodedFrame) -> Option<Screen> {
        let (h, data) = FrameHeader::parse(&frame.payload).unwrap();
        dec.decode(&h, data).unwrap().cloned()
    }

    #[test]
    fn encodes_decodes_refines_and_goes_quiet() {
        let mut enc = H264Encoder::new(30);
        let mut dec = H264Decoder::new().unwrap();
        assert_eq!(enc.codec(), Codec::H264);
        assert!(format!("{enc:?} {dec:?}").contains("H264"));
        let screen = picture(320, 192);
        // 先在很小的预算下把画面编几帧：糊
        let first = enc.encode(1, &screen, true, 300).unwrap().unwrap();
        assert!(first.key);
        let mut shown = decode(&mut dec, &first).unwrap();
        let moving = picture(320, 192);
        for id in 2..5 {
            if let Some(f) = enc.encode(id, &moving, true, 300).unwrap() {
                shown = decode(&mut dec, &f).unwrap_or(shown);
            }
        }
        let blurry = psnr(&shown, &screen);
        // 画面不再变，预算放宽：补清晰，几帧之后停发
        let mut frames = 0;
        let mut waited = 0;
        for id in 2..60 {
            match enc.encode(id, &screen, false, 50_000).unwrap() {
                Some(f) => {
                    assert!(!f.key);
                    shown = decode(&mut dec, &f).unwrap_or(shown);
                    frames += 1;
                }
                None if frames == 0 => waited += 1,
                None => break,
            }
        }
        assert_eq!(waited, 30 / 5 - 1, "停下约 200 ms 才开始补");
        assert!(
            frames <= usize::from(REFINE_FRAMES),
            "补清晰最多 {REFINE_FRAMES} 帧，实际 {frames}"
        );
        assert!(!enc.wants_refine());
        let sharp = psnr(&shown, &screen);
        assert!(
            sharp > blurry + 1.0 && sharp > 36.0,
            "补清晰之后 {sharp:.1} dB，之前 {blurry:.1} dB"
        );
        assert!(
            enc.encode(99, &screen, false, 50_000).unwrap().is_none(),
            "静止且已清晰：不发"
        );
        // 丢包：下一帧是关键帧
        enc.on_feedback(1, 1);
        enc.on_lost(5);
        assert!(enc.wants_refine());
        let key = enc.encode(100, &screen, false, 50_000).unwrap().unwrap();
        assert!(key.key);
        // 新解码器只从关键帧开始也能解
        let mut fresh = H264Decoder::new().unwrap();
        assert!(psnr(&decode(&mut fresh, &key).unwrap(), &screen) > 30.0);
    }

    #[test]
    fn odd_sizes_and_resizes_work() {
        let mut enc = H264Encoder::new(60);
        let mut dec = H264Decoder::new().unwrap();
        for (w, h) in [(33u16, 17u16), (64, 48)] {
            let screen = picture(w, h);
            let f = enc.encode(1, &screen, true, 100_000).unwrap().unwrap();
            assert!(f.key, "尺寸变了要从关键帧开始");
            let shown = decode(&mut dec, &f).unwrap();
            assert_eq!((shown.width(), shown.height()), (w, h));
            assert!(psnr(&shown, &screen) > 30.0);
        }
    }

    #[test]
    fn garbage_and_inconsistent_frames_are_rejected() {
        let mut dec = H264Decoder::new().unwrap();
        let header = FrameHeader {
            codec: Codec::H264,
            key: true,
            width: 64,
            height: 48,
        };
        // 不是 H.264、空的：解码器不出画面或报错，都不能崩
        let _ = dec.decode(&header, &[0, 0, 0, 1, 0xff, 0xff, 0xff]);
        assert!(dec.decode(&header, &[]).is_ok_and(|s| s.is_none()));
        // 帧头说的尺寸比码流大
        let mut enc = H264Encoder::new(30);
        let f = enc
            .encode(1, &picture(32, 32), true, 100_000)
            .unwrap()
            .unwrap();
        let (_, data) = FrameHeader::parse(&f.payload).unwrap();
        assert!(dec.decode(&header, data).is_err());
    }

    #[test]
    fn budget_changes_reach_the_encoder() {
        let mut enc = H264Encoder::with_max_bps(30, 1_000_000);
        let _ = enc.encode(1, &picture(64, 64), true, 1000).unwrap();
        assert_eq!(enc.bps, 240_000, "第一帧编完按预算调下来");
        let (w, h, mut inner) = enc.inner.take().unwrap();
        enc.set_budget(&mut inner, 1050); // 变化不到一成：不动
        assert_eq!(enc.bps, 240_000);
        enc.set_budget(&mut inner, 3000); // 往上调也要生效
        assert_eq!(enc.bps, 720_000);
        enc.set_budget(&mut inner, usize::MAX); // 不超过上限
        assert_eq!(enc.bps, 1_000_000);
        enc.set_budget(&mut inner, 0);
        assert_eq!(enc.bps, MIN_BPS);
        enc.inner = Some((w, h, inner));
        // 简单画面静止后补清晰的帧很小：提前停
        let plain = Screen::new(64, 64, 0x0012_3456);
        let _ = enc.encode(2, &plain, true, 10_000).unwrap();
        let mut n = 0;
        for id in 3..60 {
            if enc.encode(id, &plain, false, 10_000).unwrap().is_some() {
                n += 1;
            }
        }
        assert!(n < u32::from(REFINE_FRAMES), "补了 {n} 帧");
    }

    /// 720p 连续编码几帧：不能卡住，也不能 panic。在单独的线程里跑，10 秒没结束就判失败，
    /// 这样卡死和崩溃都能在 CI 上明确地报出来，而不是让会话测试莫名超时。
    #[test]
    fn hd_frames_encode_without_hanging() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut enc = H264Encoder::new(60);
            let mut dec = H264Decoder::new().unwrap();
            let start = std::time::Instant::now();
            let mut sizes = Vec::new();
            for id in 1..=6u32 {
                let mut screen = picture(1280, 720);
                screen.fill(0, 0, 40 * u16::try_from(id).unwrap(), 40, 0x00ff_0000);
                let f = enc.encode(id, &screen, true, 200_000).unwrap().unwrap();
                decode(&mut dec, &f).unwrap();
                sizes.push(f.payload.len());
            }
            tx.send((start.elapsed(), sizes)).unwrap();
        });
        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok((took, sizes)) => assert!(sizes.iter().all(|s| *s > 0), "{took:?} {sizes:?}"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                panic!("720p 编码 10 秒没有结束：卡住了")
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => panic!("编码线程 panic 了"),
        }
    }
}
