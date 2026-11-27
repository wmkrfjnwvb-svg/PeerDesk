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
use openh264::formats::YUVSource;

pub mod annexb;
pub mod audio;
pub mod backend;
mod color;
/// Linux 的显卡和 VA-API 能力（检测和给用户的说明）。
#[cfg(target_os = "linux")]
pub mod gpu;
pub mod qp;
pub mod soft;
pub mod stall;
#[cfg(target_os = "linux")]
mod vaapi;
use backend::{EncoderBackend, Finished, FrameAsk, FrameInput, Rates, Submitted};
use peerdesk_core::error::Error;
use peerdesk_core::frame::{Frame, GpuFrame, GpuHandle, Nv12, Picture};
use peerdesk_core::policy::ContentPolicy;
use peerdesk_core::scale::encode_size;
use peerdesk_core::screen::Screen;
use peerdesk_core::video::{
    Codec, EncodeTiming, EncodedFrame, FrameHeader, VideoDecoder, VideoEncoder,
};

/// 画面停下后最多再编几帧来补清晰（硬件编码：不知道 QP，按帧数和大小判断；软件编码补到最小 QP，见 [`qp`]）。
pub const REFINE_FRAMES: u8 = 12;
/// 补清晰的一帧小于这么多字节，说明已经补不出什么了，提前停下（硬件编码）。
const REFINE_DONE_BYTES: usize = 64;
/// 码率下限（比特每秒）。
const MIN_BPS: u64 = 64_000;
/// 硬件编码器运行中出错、改用软件编码之后，过这么久再试一次硬件。
/// 硬件出错多半是一时的（驱动忙、显卡在切换），整场会话都用软件编码代价太大（1080p 以上帧率、耗电都差一截）；
/// 但每次换编码器要出一个关键帧，不能试得太勤。RustDesk 是连续失败 3 次才停用硬件（`P-industry.md` 主题 2）。
const HW_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(30);
/// 一场会话里最多再试这么多次，还出错就一直用软件编码。
const HW_RETRY_MAX: u32 = 3;
/// 硬件编码器的码率不能低到放不下一帧：每个像素至少按这么多分之一字节算一帧的量（0.02 比特每像素，
/// 2880×1800 约 13 KB、1080p 约 5 KB），码控窗口（[`BIG_FRAME_US`]）里至少放得下这么多。
///
/// 为什么：硬件编码器的码控是硬上限，窗口里放不下就把整帧丢掉。2026-11-27 用户日志（Mac，2880×1800）：
/// 码率 344 kbps 时 100 ms 的窗口只有 4.3 KB，编码器每秒编 30–50 帧、出帧 0，画面停了 8 秒
/// （`docs/research/P-fps-bitrate.md` M2）。链路窄时该让的是帧率（帧大、发得慢，由发送端的在途上限和帧率调节管），
/// 不是“一帧也不出”。这个数是按日志里放不下、放得下的帧大小定的，真机上还要按分辨率核对。
const HW_FRAME_FLOOR_PIXELS_PER_BYTE: u64 = 400;
/// 码率上限的默认值。
pub const DEFAULT_MAX_BPS: u64 = 50_000_000;
/// 大帧规则默认的时限（微秒）：关键帧、大面积变化要在这么久里按目标码率发得完，发不完就粗编、之后补清晰
/// （照 Chromium 远程桌面 `ShouldDropQualityForLargeFrame`，它用一帧间隔）。办公策略的值（[`ContentPolicy::OFFICE`]）；
/// 硬件编码器的码控缓冲（VideoToolbox `DataRateLimits`、Media Foundation `AVEncCommonBufferSize`）也按它设。
///
/// 为什么 100 ms（2026-11 微软 8 条真实轨迹实测，经典估计器）：200 ms 正好压在“卡顿”的门限上，
/// 再加传输延迟就超过 200 ms，ms-09401 卡顿 7 次；50 ms 太粗，窄带上关键帧反复补；100 ms 时
/// 三条门槛轨迹最长等待 1519→457、1531→304、322→290 ms，首帧快 4–5 倍，卡顿也少了。
pub const BIG_FRAME_US: u64 = ContentPolicy::OFFICE.big_frame_us;
/// 最小 QP：补清晰时能达到的画质上限。
const MIN_QP: u8 = qp::MIN_QP;
/// 估计画面变了多少时每隔多少像素抽一个（横竖都是）：720p 约 3600 个，几乎不花时间。
const SAMPLE_STEP: usize = 16;
/// 抽样点细节量的底数：平坦的地方变了也要编一点。也是“按细节”和“按点数”之间的折中：底数越大越接近只数点数。
/// 2026-11 微软轨迹 12 条 × 3 个种子（冻结 / 最长等待平均 / 停下后最差 PSNR）：只数点数 145 / 642 ms / 27.3 dB；
/// 底数 8 77 / 366 ms / 25.8 dB；24 95 / 392 ms / 26.8 dB；64 112 / 447 ms / 26.7 dB。取 24：清晰度最后让步。
const DETAIL_FLOOR: u32 = 24;

/// 近似亮度（0–255）：(2R + 5G + B) / 8。
fn luma(p: u32) -> u32 {
    ((p >> 16 & 0xff) * 2 + (p >> 8 & 0xff) * 5 + (p & 0xff)) / 8
}

fn codec_error(what: &'static str) -> impl Fn(openh264::Error) -> Error {
    move |_| Error::Inconsistent(what)
}

/// OpenH264 要求宽高都是偶数：奇数时补一行（列），复制边上的像素。
/// 用 `open` 打开的硬件编码器编 `frames`：一行结果（Windows；macOS 用 `vt::probe` 对比几种设置）。
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn report_with(
    open: HardwareOpener,
    width: u32,
    height: u32,
    frames: &[Vec<u8>],
    decodes: &mut dyn FnMut(&[u8]) -> bool,
) -> Vec<String> {
    let mut enc = match open(width, height, 30, 4_000_000) {
        Ok(enc) => enc,
        Err(why) => return vec![format!("没有可用的硬件编码器：{why}")],
    };
    let name = enc.name().to_owned();
    let (mut keys, mut total, mut ok) = (0u32, 0u128, true);
    for (i, nv12) in frames.iter().enumerate() {
        let started = std::time::Instant::now();
        let (data, key) = match enc.encode(nv12, 4_000_000, i == 0) {
            Ok(done) => done.unwrap_or_default(),
            Err(why) => return vec![format!("{name}：第 {i} 帧编码失败：{why}")],
        };
        total += started.elapsed().as_micros();
        keys += u32::from(key);
        ok &= data.is_empty() || decodes(&data);
    }
    let n = u128::try_from(frames.len().max(1)).unwrap_or(1);
    vec![format!(
        "{name}：关键帧 {keys}/{}，编码平均 {} ms，软件解码 {}",
        frames.len(),
        total / n / 1000,
        if ok { "正常" } else { "失败" }
    )]
}

/// 画面转成 NV12（宽高补成偶数）：（宽, 高, NV12）。`hw-check` 用它准备硬件编码器的输入。
#[must_use]
pub fn screen_to_nv12(screen: &Screen) -> (u32, u32, Vec<u8>) {
    let (w, h, pixels) = even_pixels(screen);
    let (mut y, mut u, mut v, mut nv12) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    color::rgb_to_i420(&pixels, w, &mut y, &mut u, &mut v);
    color::i420_to_nv12(&y, &u, &v, &mut nv12);
    (
        u32::try_from(w).unwrap_or(0),
        u32::try_from(h).unwrap_or(0),
        nv12,
    )
}

/// 软件解码器解不解得了这一帧（Annex B）。
fn soft_decodes(dec: Option<&mut H264Decoder>, width: u32, height: u32, data: &[u8]) -> bool {
    let header = FrameHeader {
        codec: Codec::H264,
        key: annexb::has_idr(data),
        width: u16::try_from(width).unwrap_or(u16::MAX),
        height: u16::try_from(height).unwrap_or(u16::MAX),
    };
    dec.is_some_and(|d| d.decode(&header, data).is_ok())
}

/// 硬件编码器体检：用本系统的硬件编码器（macOS 对比几种设置）编 `frames`（NV12，第一帧要求关键帧），
/// 每种设置一行结果。没有硬件编码器时说明原因。
#[must_use]
pub fn hardware_report(width: u32, height: u32, frames: &[Vec<u8>]) -> Vec<String> {
    // 软件解码器能不能接上硬件编出的码流（接收端在 Linux 上用它）
    let mut soft = H264Decoder::new().ok();
    let mut decodes = |data: &[u8]| soft_decodes(soft.as_mut(), width, height, data);
    #[cfg(target_os = "macos")]
    {
        vt::probe(width, height, frames, &mut decodes)
    }
    #[cfg(not(target_os = "macos"))]
    {
        report_with(open_system_hardware, width, height, frames, &mut decodes)
    }
}

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

pub mod gl;
#[cfg(windows)]
mod mf;
#[cfg(target_os = "macos")]
mod vt;

#[cfg(target_os = "macos")]
impl HardwareEncoder for vt::VtEncoder {
    fn name(&self) -> &str {
        "VideoToolbox"
    }
    fn size(&self) -> (u32, u32) {
        vt::VtEncoder::size(self)
    }
    fn encode(
        &mut self,
        nv12: &[u8],
        bps: u64,
        force_key: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        vt::VtEncoder::encode(self, nv12, bps, force_key)
    }
    fn accepts(&self, handle: &GpuHandle) -> bool {
        matches!(handle, GpuHandle::CvPixelBuffer(p) if !p.is_null())
    }
    fn encode_gpu(
        &mut self,
        frame: &dyn GpuFrame,
        bps: u64,
        force_key: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        match frame.handle() {
            GpuHandle::CvPixelBuffer(image) => {
                vt::VtEncoder::encode_image(self, image, bps, force_key)
            }
            GpuHandle::D3d11Texture { .. } | GpuHandle::None => {
                Err("VideoToolbox 只认 CVPixelBuffer".into())
            }
        }
    }
    fn poll(&mut self) -> Result<Option<(Vec<u8>, bool)>, String> {
        vt::VtEncoder::poll(self)
    }
    fn drain(&mut self) -> Result<(), String> {
        vt::VtEncoder::complete(self)
    }
}

/// 硬件解码器（系统胶水实现，例如 macOS 的 VideoToolbox）。输入 Annex B 码流。
pub trait HardwareDecoder: Send + std::fmt::Debug {
    /// 名称（日志、详情面板用）。
    fn name(&self) -> &str;
    /// 解一帧，返回（宽, 高, `0x00RRGGBB` 像素）；还没收到关键帧时返回 `None`。
    ///
    /// # Errors
    /// 解码出错：调用方改用软件解码。
    fn decode(&mut self, data: &[u8]) -> Result<Option<(usize, usize, Vec<u32>)>, String>;

    /// 解一帧，交出 NV12（解码器本来就出 NV12：只拷贝，不在 CPU 上转颜色，界面在显卡上转）。
    /// 返回 `Ok(None)` 的情况同 [`Self::decode`]。默认不支持（交 RGB 的解码器，例如 macOS 直接出 BGRA）。
    ///
    /// # Errors
    /// 不支持，或解码出错。
    fn decode_nv12(&mut self, _data: &[u8]) -> Result<Option<Nv12>, String> {
        Err("这个解码器不出 NV12".into())
    }

    /// 能用 [`Self::decode_nv12`]。
    fn gives_nv12(&self) -> bool {
        false
    }
}

#[cfg(target_os = "macos")]
impl HardwareDecoder for vt::VtDecoder {
    fn name(&self) -> &str {
        "VideoToolbox"
    }
    fn decode(&mut self, data: &[u8]) -> Result<Option<(usize, usize, Vec<u32>)>, String> {
        vt::VtDecoder::decode(self, data)
    }
}

#[cfg(windows)]
impl HardwareDecoder for mf::MfDecoder {
    fn name(&self) -> &str {
        mf::MfDecoder::name(self)
    }
    fn decode(&mut self, data: &[u8]) -> Result<Option<(usize, usize, Vec<u32>)>, String> {
        mf::MfDecoder::decode(self, data)
    }
    fn decode_nv12(&mut self, data: &[u8]) -> Result<Option<Nv12>, String> {
        mf::MfDecoder::decode_nv12(self, data)
    }
    fn gives_nv12(&self) -> bool {
        true
    }
}

/// 本系统的硬件解码器（目前只有 macOS）。`PEERDESK_HW=0` 关掉。
///
/// # Errors
/// 被关掉，或者这个系统还没接硬件解码。
pub fn open_system_hardware_decoder() -> Result<Box<dyn HardwareDecoder>, String> {
    if hardware_disabled(std::env::var_os("PEERDESK_HW").as_deref()) {
        return Err("已用 PEERDESK_HW=0 关掉".into());
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(vt::VtDecoder::new()))
    }
    #[cfg(windows)]
    {
        Ok(Box::new(mf::MfDecoder::open()?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(vaapi::VaapiDecoder::open()?))
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        Err("这个系统还没接硬件解码".into())
    }
}

/// H.264 解码：有硬件解码器就用（macOS），出错时改用 OpenH264 并且不再换回。
pub struct AutoH264Decoder {
    hw: Option<Box<dyn HardwareDecoder>>,
    soft: H264Decoder,
    /// 硬件解出的最近一幅画面。
    screen: Option<Screen>,
}

impl std::fmt::Debug for AutoH264Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AutoH264Decoder")
            .field("hw", &self.hw)
            .finish_non_exhaustive()
    }
}

impl AutoH264Decoder {
    /// 用本系统的硬件解码器（没有就只用软件）。
    ///
    /// # Errors
    /// OpenH264 解码器创建失败。
    pub fn new() -> Result<Self, Error> {
        Self::with_hardware(open_system_hardware_decoder().ok())
    }

    /// 指定硬件解码器（测试用假的）。
    ///
    /// # Errors
    /// OpenH264 解码器创建失败。
    pub fn with_hardware(hw: Option<Box<dyn HardwareDecoder>>) -> Result<Self, Error> {
        if let Some(h) = &hw {
            log::info!("视频解码：{}", decoder_label(h.name()));
        }
        Ok(Self {
            hw,
            soft: H264Decoder::new()?,
            screen: None,
        })
    }
}

impl VideoDecoder for AutoH264Decoder {
    fn decode(&mut self, header: &FrameHeader, data: &[u8]) -> Result<Option<&Screen>, Error> {
        if let Some(hw) = self.hw.as_mut() {
            match hw.decode(data) {
                Ok(None) => return Ok(None),
                Ok(Some((w, h, pixels))) => {
                    let (hw_w, hh) = (usize::from(header.width), usize::from(header.height));
                    if w < hw_w || h < hh {
                        return Err(Error::Inconsistent("解出的画面比帧头说的小"));
                    }
                    // 硬件按 16 对齐时多出的边裁掉
                    let mut cropped = Vec::with_capacity(hw_w * hh);
                    for y in 0..hh {
                        cropped
                            .extend_from_slice(pixels.get(y * w..y * w + hw_w).unwrap_or_default());
                    }
                    self.screen = Some(Screen::from_pixels(header.width, header.height, cropped)?);
                    return Ok(self.screen.as_ref());
                }
                Err(why) => {
                    log::warn!("硬件解码失败，改用软件解码（OpenH264）：{why}");
                    self.hw = None;
                    self.screen = None;
                }
            }
        }
        self.soft.decode(header, data)
    }

    fn screen(&self) -> Option<&Screen> {
        self.screen.as_ref().or_else(|| self.soft.screen())
    }

    fn decode_picture(
        &mut self,
        header: &FrameHeader,
        data: &[u8],
    ) -> Result<Option<Picture>, Error> {
        if let Some(hw) = self.hw.as_mut().filter(|h| h.gives_nv12()) {
            match hw.decode_nv12(data) {
                Ok(None) => return Ok(None),
                Ok(Some(nv12)) => {
                    let (w, h) = (header.width & !1, header.height & !1);
                    if nv12.width() < w || nv12.height() < h {
                        return Err(Error::Inconsistent("解出的画面比帧头说的小"));
                    }
                    // 硬件按 16 对齐时多出的边裁掉（只拷贝）
                    let nv12 = if (nv12.width(), nv12.height()) == (w, h) {
                        nv12
                    } else {
                        let (y, uv) = nv12.planes();
                        let pitch = usize::from(nv12.width());
                        Nv12::from_planes(w, h, y, pitch, uv, pitch)?
                    };
                    return Ok(Some(Picture::Nv12(std::sync::Arc::new(nv12))));
                }
                Err(why) => {
                    log::warn!("硬件解码失败，改用软件解码（OpenH264）：{why}");
                    self.hw = None;
                    self.screen = None;
                }
            }
        }
        if self.hw.is_some() {
            // 出 RGB 的硬件解码器（macOS）
            return Ok(self
                .decode(header, data)?
                .map(|s| Picture::Rgb(std::sync::Arc::new(s.clone()))));
        }
        self.soft.decode_picture(header, data)
    }

    fn backend(&self) -> String {
        self.hw
            .as_ref()
            .map_or_else(|| "软件 OpenH264".to_owned(), |h| decoder_label(h.name()))
    }
}

/// 系统解码器的显示名：在 CPU 上解的（例如没有显卡解码时的 Media Foundation）不能叫“硬件”。
fn decoder_label(name: &str) -> String {
    if name.contains("CPU") {
        format!("系统 {name}")
    } else {
        format!("硬件 {name}")
    }
}

/// 硬件编码器（系统胶水实现，例如 Windows 的 Media Foundation）。输入 NV12，输出 H.264 Annex B。
pub trait HardwareEncoder: Send + std::fmt::Debug {
    /// 名称（日志用），例如“NVIDIA H.264 Encoder MFT”。
    fn name(&self) -> &str;
    /// 打开时的尺寸。
    fn size(&self) -> (u32, u32);
    /// 交一帧，取回一个编好的帧（码流和是否关键帧）。异步的编码器（VideoToolbox）可能攒着帧：
    /// 取回的可以是之前交的帧；还没编好时为 `None`，之后用 [`Self::poll`] 取。
    /// 码流为空表示编码器丢了这一帧。
    ///
    /// # Errors
    /// 编码器出错：调用方改用软件编码。
    fn encode(
        &mut self,
        nv12: &[u8],
        bps: u64,
        force_key: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String>;

    /// 取走一个之前交的、现在编好了的帧（同步的编码器没有，总是 `None`）。
    ///
    /// # Errors
    /// 编码器出错。
    fn poll(&mut self) -> Result<Option<(Vec<u8>, bool)>, String> {
        Ok(None)
    }

    /// 画面停下后让编码器交出攒着的帧，之后用 [`Self::poll`] 取。
    ///
    /// # Errors
    /// 编码器出错。
    fn drain(&mut self) -> Result<(), String> {
        Ok(())
    }

    /// 认得这个显卡句柄：能不拷贝、直接编系统交来的显卡画面（见 [`Self::encode_gpu`]）。
    fn accepts(&self, _handle: &GpuHandle) -> bool {
        false
    }

    /// 直接编一帧显卡画面（只在 [`Self::accepts`] 为真时调用），结果和 [`Self::encode`] 一样。
    ///
    /// # Errors
    /// 编码器出错：调用方改用软件编码。
    fn encode_gpu(
        &mut self,
        _frame: &dyn GpuFrame,
        _bps: u64,
        _force_key: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        Err("这个硬件编码器不能直接用显卡画面".into())
    }

    /// 上一次 [`Self::encode`] 交的这一帧编码器没收（例如 Media Foundation 的 `MF_E_NOTACCEPTING`）：
    /// 这一帧不会有输出，它带的关键帧要求也没有送到，调用方下一帧重新要求。
    fn last_rejected(&self) -> bool {
        false
    }
}

#[cfg(windows)]
impl HardwareEncoder for mf::MfEncoder {
    fn name(&self) -> &str {
        mf::MfEncoder::name(self)
    }
    fn size(&self) -> (u32, u32) {
        mf::MfEncoder::size(self)
    }
    fn encode(
        &mut self,
        nv12: &[u8],
        bps: u64,
        force_key: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        mf::MfEncoder::encode(self, nv12, bps, force_key)
    }
    fn poll(&mut self) -> Result<Option<(Vec<u8>, bool)>, String> {
        mf::MfEncoder::poll(self)
    }
    fn drain(&mut self) -> Result<(), String> {
        mf::MfEncoder::drain(self)
    }
    fn last_rejected(&self) -> bool {
        mf::MfEncoder::last_rejected(self)
    }
}

/// `PEERDESK_HW=0` 关掉硬件编码（对照测试用）。
fn hardware_disabled(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| v == "0")
}

/// 打开硬件编码器的函数：（宽, 高, 帧率, 码率）。
pub type HardwareOpener = fn(u32, u32, u32, u64) -> Result<Box<dyn HardwareEncoder>, String>;

/// 本系统的硬件编码器。`PEERDESK_HW=0` 关掉（对照测试用）。
///
/// # Errors
/// 没有硬件编码器、被关掉，或者这个系统还没接硬件编码（目前只有 Windows）。
pub fn open_system_hardware(
    width: u32,
    height: u32,
    fps: u32,
    bps: u64,
) -> Result<Box<dyn HardwareEncoder>, String> {
    if hardware_disabled(std::env::var_os("PEERDESK_HW").as_deref()) {
        return Err("已用 PEERDESK_HW=0 关掉".into());
    }
    #[cfg(windows)]
    {
        Ok(Box::new(mf::MfEncoder::open(width, height, fps, bps)?))
    }
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(vt::VtEncoder::open(width, height, fps, bps)?))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(vaapi::VaapiEncoder::open(
            width, height, fps, bps,
        )?))
    }
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    {
        let _ = (width, height, fps, bps);
        Err("这个系统还没接硬件编码".into())
    }
}

/// H.264 编码器（OpenH264）。
pub struct H264Encoder {
    fps: u32,
    max_bps: u64,
    /// 软件编码后端（现在是 openh264；硬件编码器搬到同一个接口之后，硬件、软件走同一条路）。
    soft: Box<dyn EncoderBackend>,
    /// 交给软件后端的帧的编号（对号用）。
    soft_token: u64,
    bps: u64,
    force_key: bool,
    refine_left: u8,
    /// 画面连续没变的帧数：够了 [`Self::refine_after`] 才开始补清晰。
    still: u32,
    /// 转换好的 Y、U、V 平面，每帧复用。
    planes: (Vec<u8>, Vec<u8>, Vec<u8>),
    /// 硬件编码器（打开了就优先用）。
    hw: Option<Box<dyn HardwareEncoder>>,
    /// 硬件编码现在不用（没有、打不开或失败过）；到了 `hw_retry_at` 会再试。
    hw_off: bool,
    /// 硬件编码器运行中出过几次错。
    hw_failures: u32,
    /// 什么时候再试硬件（`None`：不再试）。
    hw_retry_at: Option<std::time::Instant>,
    /// 出错后隔多久再试（测试里改短）。
    hw_retry_after: std::time::Duration,
    /// 硬件编码器丢过帧之后学到的码率下限（这个尺寸下低于它放不下一帧；0：还没丢过）。
    hw_floor: u64,
    /// 上一次交给硬件编码器的码率。
    hw_last_bps: u64,
    hw_open: HardwareOpener,
    /// 硬件编码器的输入（NV12），每帧复用。
    nv12: Vec<u8>,
    /// 上一次编出来的尺寸（不管用的哪个编码器）。
    encoded_size: Option<(u16, u16)>,
    /// 上一帧颜色转换的耗时（微秒）。
    convert_us: u64,
    /// 硬件编码器连续出了几个没要求的关键帧。
    unasked_keys: u32,
    /// 用软件编码的原因（打不开硬件编码器，或硬件中途出错），显示在详情面板里。
    soft_reason: Option<String>,
    /// 按源画面的多少编码（千分比，见 [`VideoEncoder::set_scale`]）。
    scale_permille: u16,
    /// 控制端显示画面的区域（见 [`VideoEncoder::set_view`]）。
    view: Option<(u16, u16)>,
    /// 为什么要求了关键帧（出了关键帧时记进 `timing`）。
    key_reason: &'static str,
    /// 编码分辨率或控制端窗口变了（原因）：CPU 画面的编码尺寸可能跟着变，下一次要编（真变了才出关键帧）。
    resize_reason: Option<&'static str>,
    /// 这一次编码各步骤的耗时（见 [`EncodeTiming`]）。
    timing: EncodeTiming,
    /// 交给硬件编码器、还没取回的帧：每帧是否要求了关键帧（按顺序；异步编码器会攒帧）。
    hw_sent: std::collections::VecDeque<bool>,
    /// 软件编码的逐帧 QP 控制（openh264 自己的码控关掉，见 [`qp`]）。
    qp: qp::QpControl,
    /// 下一帧强制用的 QP（见 [`H264Encoder::coarse_next`]）。
    coarse_qp: Option<u8>,
    /// 软件编码刚编完的一帧：补清晰补完了没有（交给 `finish` 定还要不要补）。
    soft_converged: Option<bool>,
    /// 大帧规则的时限（微秒）：关键帧要在这么久里按目标码率发得完。0：不限。
    big_frame_us: u64,
    /// 码率估计过期了（发送端判断，见 `VideoEncoder::set_rate_stale`）：大帧不按时限放宽，最多一帧的预算。
    rate_stale: bool,
    /// 上一次编的画面的抽样（每 [`SAMPLE_STEP`] 像素取一个），估计画面变了多少。
    samples: Vec<u32>,
    sampled: (u16, u16),
}

/// 硬件编码这一步的结果。
enum Hw {
    /// 不用硬件（没有、打不开或刚出错）：用软件编。
    Off,
    /// 交给硬件了，还没有编好的帧。
    Pending,
    /// 取回一个编好的帧：码流（空表示编码器丢了这一帧）、是否关键帧、是否刚打开。
    Frame(Vec<u8>, bool, bool),
}

/// 交来编码的一帧。
#[derive(Clone, Copy)]
enum Input<'a> {
    /// CPU 上的 RGB 画面：编码器缩放、转颜色。
    Rgb(&'a Screen),
    /// 显卡画面：已经是编码尺寸的 NV12。
    Gpu(&'a dyn GpuFrame),
}

/// 硬件编码器连续这么多帧给出没要求的关键帧，就认为它不能用。
const UNASKED_KEY_LIMIT: u32 = 5;

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
            soft: Box::new(soft::OpenH264Backend::new()),
            soft_token: 0,
            bps: 0,
            force_key: false,
            refine_left: 0,
            still: 0,
            planes: (Vec::new(), Vec::new(), Vec::new()),
            hw: None,
            hw_off: false,
            hw_failures: 0,
            hw_retry_at: None,
            hw_retry_after: HW_RETRY_AFTER,
            hw_floor: 0,
            hw_last_bps: 0,
            hw_open: open_system_hardware,
            nv12: Vec::new(),
            encoded_size: None,
            convert_us: 0,
            unasked_keys: 0,
            soft_reason: None,
            scale_permille: peerdesk_core::scale::FULL_PERMILLE,
            view: None,
            key_reason: "",
            resize_reason: None,
            timing: EncodeTiming::default(),
            hw_sent: std::collections::VecDeque::new(),
            qp: qp::QpControl::default(),
            coarse_qp: None,
            soft_converged: None,
            big_frame_us: BIG_FRAME_US,
            rate_stale: false,
            samples: Vec::new(),
            sampled: (0, 0),
        }
    }

    /// 大帧规则的时限（微秒，0 不限）：关键帧要在这么久里按目标码率发得完，发不完就粗编、之后补清晰。
    /// 默认 [`BIG_FRAME_US`]。
    #[must_use]
    pub const fn with_big_frame_us(mut self, us: u64) -> Self {
        self.big_frame_us = us;
        self
    }

    /// 画面和上一次编的相比变了多少（千分比），按抽样估计；尺寸变了算全变。顺便记下这一次的抽样。
    /// 画面变了多少（千分比）：`(按点数, 按细节加权)`。按细节加权的用来估计这一帧多大；
    /// 按点数的用来判断是不是整屏变化（滚动文字时细节集中在文字上，按细节加权常常过半，但有运动补偿，
    /// 不该当成整屏变化重编成关键帧：8 Mbps、12% 丢包时关键帧更难送到，实测卡顿变多）。
    fn changed_permille(&mut self, screen: &Screen) -> (u64, u64) {
        let width = usize::from(screen.width());
        let size = (screen.width(), screen.height());
        let pixels = screen.pixels();
        let rows: Vec<&[u32]> = pixels.chunks(width.max(1)).collect();
        // 每个抽样点：像素值，和它在新画面里的细节量（和右边、下边像素的亮度差，加一个底数）
        let mut now = Vec::with_capacity(self.samples.len());
        let mut detail = Vec::with_capacity(self.samples.len());
        for (y, row) in rows.iter().enumerate().step_by(SAMPLE_STEP) {
            let below = rows.get(y.saturating_add(1));
            for (x, p) in row.iter().enumerate().step_by(SAMPLE_STEP) {
                let l = luma(*p);
                let right = row.get(x.saturating_add(1)).map_or(l, |q| luma(*q));
                let down = below.and_then(|r| r.get(x)).map_or(l, |q| luma(*q));
                now.push(*p);
                detail.push(l.abs_diff(right) + l.abs_diff(down) + DETAIL_FLOOR);
            }
        }
        // 按细节加权的变化比例：编码出来的大小大致按细节分布（关键帧的大小估计也是整幅画面的细节），
        // 只数变了几个点会把“文字密集的弹窗盖在白底上”估得很小（弹窗占 36%，变了的点只有 19%，按点数估计
        // 比实际小 5 倍以上，2026-11 微软轨迹里切换窗口的帧在 1 Mbps 上编出 89–125 KB）
        let changed = if size == self.sampled && now.len() == self.samples.len() {
            let total: u64 = detail.iter().map(|d| u64::from(*d)).sum();
            let (count, weight) = now
                .iter()
                .zip(&self.samples)
                .zip(&detail)
                .filter(|((a, b), _)| a != b)
                .fold((0u64, 0u64), |(c, w), (_, d)| {
                    (c.saturating_add(1), w.saturating_add(u64::from(*d)))
                });
            let points = u64::try_from(now.len()).unwrap_or(u64::MAX).max(1);
            (
                count.saturating_mul(1000) / points,
                weight.saturating_mul(1000) / total.max(1),
            )
        } else {
            (1000, 1000)
        };
        self.samples = now;
        self.sampled = size;
        changed
    }

    /// 下一帧（软件编码）强制用 QP `qp` 编（测量、以后按画面变化面积判断大帧时用），之后照常逐帧控制。
    pub fn coarse_next(&mut self, qp: u8) {
        self.coarse_qp = Some(qp.clamp(MIN_QP, qp::MAX_QP));
    }

    /// 指定软件编码的最大 QP（夹在 [`MIN_QP`, 51]）。默认 51：大帧粗编、之后补清晰。
    #[must_use]
    pub fn with_max_qp(mut self, qp: u8) -> Self {
        self.qp = qp::QpControl::new(qp);
        self
    }

    /// 按内容策略设 QP 上限和大帧时限（见 [`ContentPolicy`]）。
    #[must_use]
    pub fn with_policy(self, policy: &ContentPolicy) -> Self {
        self.with_max_qp(policy.max_qp)
            .with_big_frame_us(policy.big_frame_us)
    }

    /// 指定打开硬件编码器的函数（测试用假的编码器）。
    #[must_use]
    pub fn with_hardware(mut self, open: HardwareOpener) -> Self {
        self.hw_open = open;
        self
    }

    /// 现在用的是哪个编码器（日志、统计用）。
    #[must_use]
    pub fn backend_name(&self) -> String {
        match (&self.hw, &self.soft_reason) {
            (Some(e), _) => format!("硬件 {}", e.name()),
            (None, Some(why)) => format!("软件 OpenH264（{why}）"),
            (None, None) => "软件 OpenH264".to_owned(),
        }
    }

    /// 交给硬件编码器的码率：不低于“放得下一帧”的下限（按尺寸算的，和丢过帧之后学到的，取大的）。
    fn hw_bps(&self, bps: u64, w: usize, h: usize) -> u64 {
        bps.max(hw_floor_bps(w, h)).max(self.hw_floor)
    }

    /// 硬件出错：改用软件编码（从关键帧开始）；过一阵再试硬件，试够了次数就一直用软件。
    fn hardware_failed(&mut self, why: &str) {
        self.hw_failures = self.hw_failures.saturating_add(1);
        if self.hw_failures <= HW_RETRY_MAX {
            self.hw_retry_at = std::time::Instant::now().checked_add(self.hw_retry_after);
            log::warn!(
                "硬件编码失败，改用软件编码（OpenH264），{} 秒后再试硬件（第 {} 次出错，最多再试 {HW_RETRY_MAX} 次）：{why}",
                self.hw_retry_after.as_secs(),
                self.hw_failures
            );
        } else {
            self.hw_retry_at = None;
            log::warn!(
                "硬件编码失败，改用软件编码（OpenH264），已经出错 {} 次，这次会话不再试硬件：{why}",
                self.hw_failures
            );
        }
        self.soft_reason = Some(format!("硬件出错：{why}"));
        self.hw = None;
        self.hw_off = true;
        self.hw_sent.clear();
        self.force_key = true;
        self.key_reason = "硬件编码出错，改用软件编码";
    }

    /// 取回了一个硬件编好的帧：对上它交的时候是否要求了关键帧，检查“没要求却是关键帧”。
    fn hardware_output(&mut self, data: Vec<u8>, key: bool, fresh: bool) -> Hw {
        let asked = self.hw_sent.pop_front().unwrap_or(true);
        // 编码器把这一帧丢了（码控放不下）：告诉发送端，这是预算不够，不是没东西要发；
        // 同时把给它的码率下限翻倍——丢帧必须有出口，不能一直丢下去（每丢一次翻一倍，几帧之内一定放得下）
        self.timing.dropped = data.is_empty();
        if data.is_empty() && self.hw_last_bps > 0 {
            let raised = self
                .hw_floor
                .max(self.hw_last_bps)
                .saturating_mul(2)
                .min(self.max_bps);
            if raised > self.hw_floor {
                log::info!(
                    "视频编码：硬件编码器在 {} kbps 下把帧丢了（码控放不下一帧），给它的码率下限抬到 {} kbps",
                    self.hw_last_bps / 1000,
                    raised / 1000
                );
                self.hw_floor = raised;
            }
        }
        // 要了关键帧、编码器却交出空帧（码率控制把它丢了：2026-11-25 CI 实测 VideoToolbox 低延迟码控
        // 300 kbps 时丢掉了强制要的关键帧）：要求还没满足，下一帧接着要；否则接收端一直等不到关键帧。
        if asked && data.is_empty() && !self.force_key {
            log::info!("视频编码：硬件编码器丢掉了要求的关键帧（码率控制），下一帧再要");
            self.force_key = true;
        }
        // 没要求却一直出关键帧：每帧都是整幅画面（几百 KB），码率和帧率都垮掉，画面忽糊忽清。
        // 这是用法或编码器的问题，记日志说明，改用软件编码（这一帧本身是完整的关键帧，照常发出）。
        self.unasked_keys = if key && !asked {
            self.unasked_keys + 1
        } else {
            0
        };
        if self.unasked_keys >= UNASKED_KEY_LIMIT {
            let name = self.backend_name();
            log::warn!(
                "硬件编码器 {name} 连续 {UNASKED_KEY_LIMIT} 帧都是没要求的关键帧，改用软件编码"
            );
            self.soft_reason = Some(format!("{name} 每帧都是关键帧"));
            self.hw = None;
            self.hw_off = true;
            self.hw_sent.clear();
            self.force_key = true;
            self.key_reason = "硬件编码器每帧都是关键帧，改用软件编码";
        }
        Hw::Frame(data, key, fresh)
    }

    /// 准备硬件编码器：没打开或尺寸变了就（重新）打开。返回 `None` 表示不用硬件，否则是否刚打开。
    fn hardware_ready(&mut self, w: usize, h: usize, bps: u64) -> Option<bool> {
        if self.hw_off {
            // 运行中出过错：到时间了再试一次（打不开、每帧都是关键帧的不试，那不是一时的毛病）
            match self.hw_retry_at {
                Some(at) if std::time::Instant::now() >= at => {
                    self.hw_retry_at = None;
                    self.hw_off = false;
                    log::info!(
                        "视频编码：再试一次硬件编码（之前出错 {} 次）",
                        self.hw_failures
                    );
                }
                Some(_) | None => return None,
            }
        }
        let size = (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0));
        // 码率不能低到放不下一帧（见 HW_FRAME_FLOOR_PIXELS_PER_BYTE）
        let bps = self.hw_bps(bps, w, h);
        let reopen = self.hw.as_ref().is_none_or(|e| e.size() != size);
        if reopen {
            self.hw = None;
            self.hw_sent.clear();
            // 学到的下限是这个尺寸的，换了尺寸重新学
            self.hw_floor = 0;
            match (self.hw_open)(size.0, size.1, self.fps, bps) {
                Ok(e) => {
                    log::info!("视频编码：硬件 {}（{}×{}）", e.name(), size.0, size.1);
                    self.hw = Some(e);
                    self.soft_reason = None;
                    if self.key_reason.is_empty() && self.hw_failures > 0 {
                        self.key_reason = "再试硬件编码";
                    }
                }
                Err(why) => {
                    log::info!("视频编码：软件 OpenH264（不用硬件编码：{why}）");
                    self.soft_reason = Some(why);
                    self.hw_off = true;
                    return None;
                }
            }
        }
        Some(reopen)
    }

    /// 交给硬件之后：记下要求了关键帧没有，取回的帧对上号；出错时改用软件编码。
    fn hardware_result(
        &mut self,
        reopen: bool,
        force_key: bool,
        got: Result<Option<(Vec<u8>, bool)>, String>,
    ) -> Hw {
        match got {
            Ok(got) => {
                if self.hw.as_ref().is_some_and(|e| e.last_rejected()) {
                    // 编码器没收这一帧：不会有它的输出；关键帧要求留到下一帧
                    self.force_key = force_key;
                } else {
                    self.hw_sent.push_back(force_key);
                    // 要求已经交给编码器了：不再对后面的帧重复要求
                    self.force_key = false;
                }
                match got {
                    Some((data, key)) => self.hardware_output(data, key, reopen),
                    None => Hw::Pending,
                }
            }
            Err(why) => {
                self.hardware_failed(&why);
                Hw::Off
            }
        }
    }

    /// 用硬件编 CPU 上转好的一帧（I420 打包成 NV12 交给编码器，取回一个编好的帧）。
    fn encode_hardware(&mut self, w: usize, h: usize, bps: u64) -> Hw {
        let Some(reopen) = self.hardware_ready(w, h, bps) else {
            return Hw::Off;
        };
        let bps = self.hw_bps(bps, w, h);
        self.hw_last_bps = bps;
        let packing = std::time::Instant::now();
        color::i420_to_nv12(
            &self.planes.0,
            &self.planes.1,
            &self.planes.2,
            &mut self.nv12,
        );
        self.convert_us = self.convert_us.saturating_add(micros_since(packing));
        let force_key = self.force_key || reopen;
        let nv12 = &self.nv12;
        let got = self
            .hw
            .as_mut()
            .map_or(Ok(None), |enc| enc.encode(nv12, bps, force_key));
        self.hardware_result(reopen, force_key, got)
    }

    /// 用硬件编一帧显卡画面：编码器认得这个句柄就直接交（不拷贝）；认不得（例如不在同一块显卡上）
    /// 读回一次 NV12 再交。
    ///
    /// # Errors
    /// 要读回时读不出来。
    fn encode_hardware_gpu(&mut self, frame: &dyn GpuFrame, bps: u64) -> Result<Hw, Error> {
        let (w, h) = frame.size();
        let Some(reopen) = self.hardware_ready(usize::from(w), usize::from(h), bps) else {
            return Ok(Hw::Off);
        };
        let bps = self.hw_bps(bps, usize::from(w), usize::from(h));
        self.hw_last_bps = bps;
        let force_key = self.force_key || reopen;
        let handle = frame.handle();
        let direct = self.hw.as_ref().is_some_and(|e| e.accepts(&handle));
        let got = if direct {
            self.hw
                .as_mut()
                .map_or(Ok(None), |enc| enc.encode_gpu(frame, bps, force_key))
        } else {
            let reading = std::time::Instant::now();
            let nv12 = frame.read_nv12()?;
            // 读回是在等显卡（和它的锁），不是 CPU 在算：单独记，CPU 过载的判断不算它
            self.timing.wait_us = self.timing.wait_us.saturating_add(micros_since(reading));
            self.convert_us = self.convert_us.saturating_add(micros_since(reading));
            self.hw
                .as_mut()
                .map_or(Ok(None), |enc| enc.encode(nv12.data(), bps, force_key))
        };
        Ok(self.hardware_result(reopen, force_key, got))
    }

    /// CPU 画面缩到编码尺寸、转成 I420（放进 `planes`）。返回编码用的宽高（奇数尺寸裁掉一行一列）。
    fn rgb_planes(&mut self, screen: &Screen, size: (u16, u16)) -> Result<(usize, usize), Error> {
        let shrunk;
        let screen = if size == (screen.width(), screen.height()) {
            screen
        } else {
            let scaling = std::time::Instant::now();
            shrunk = screen.downscale(size.0, size.1)?;
            self.timing.scale_us = micros_since(scaling);
            &shrunk
        };
        let (w, h, pixels) = even_pixels(screen);
        color::rgb_to_i420(
            &pixels,
            w,
            &mut self.planes.0,
            &mut self.planes.1,
            &mut self.planes.2,
        );
        Ok((w, h))
    }

    /// 编一帧（CPU 画面或显卡画面）。
    fn encode_input(
        &mut self,
        input: Input<'_>,
        changed: bool,
        budget: usize,
    ) -> Result<Option<EncodedFrame>, Error> {
        // 这一次没有转换（画面没变、只取硬件攒着的帧）时报 0，不沿用上一次的
        // （以前沿用：统计里出现“编码 31 ms（其中颜色转换 184 ms）”这种转换比总时间还大的数）
        self.convert_us = 0;
        self.timing = EncodeTiming::default();
        let size = match input {
            // CPU 画面：按发送端定的比例和控制端窗口算编码尺寸（见 peerdesk_core::scale::encode_size）
            Input::Rgb(screen) => encode_size(
                (screen.width(), screen.height()),
                self.scale_permille,
                self.view,
            ),
            // 显卡画面：采集端已经按编码尺寸准备好了
            Input::Gpu(frame) => frame.size(),
        };
        // 按“上一次编出来的尺寸”判断，和用的是硬件还是软件编码器无关
        let fresh = self.encoded_size != Some(size);
        // 编码分辨率、控制端窗口变了：尺寸真的变了，关键帧的原因写它
        if let Some(why) = self.resize_reason.take() {
            if fresh && self.key_reason.is_empty() {
                self.key_reason = why;
            }
        }
        if !fresh && !changed && !self.force_key {
            // 画面停下了：先等一会儿，确认真的停了再补清晰。
            self.still = self.still.saturating_add(1);
            // 硬件编码器还攒着之前交的帧：先把它们取回来
            if !self.hw_sent.is_empty() {
                return Ok(match self.poll_hardware() {
                    Hw::Frame(data, key, fresh) if !data.is_empty() => {
                        Some(self.finish(&data, key, size, false, fresh))
                    }
                    Hw::Frame(..) | Hw::Pending | Hw::Off => None,
                });
            }
            if self.refine_left == 0 || self.still < self.refine_after() {
                return Ok(None);
            }
        } else {
            self.still = 0;
        }
        self.timing.width = size.0;
        self.timing.height = size.1;
        let bps = self.budget_bps(budget);
        // 有硬件编码器就用它（快得多、几乎不占 CPU）；没有或失败时用 OpenH264
        let converting = std::time::Instant::now();
        let (w, h, hw) = match input {
            Input::Rgb(screen) => {
                let (w, h) = self.rgb_planes(screen, size)?;
                self.convert_us = micros_since(converting);
                let encoding = std::time::Instant::now();
                let hw = self.encode_hardware(w, h, bps);
                self.timing.encode_us = micros_since(encoding);
                (w, h, hw)
            }
            Input::Gpu(frame) => {
                self.timing.nv12_direct = true;
                let encoding = std::time::Instant::now();
                let hw = self.encode_hardware_gpu(frame, bps)?;
                // 要读回时读回的时间记在“转换”里，其余是编码器本身
                self.timing.encode_us = micros_since(encoding).saturating_sub(self.convert_us);
                (usize::from(size.0), usize::from(size.1), hw)
            }
        };
        self.timing.hardware = !matches!(hw, Hw::Off);
        match hw {
            Hw::Frame(data, key, fresh_hw) if !data.is_empty() => {
                self.timing.convert_us = self.convert_us.saturating_sub(self.timing.scale_us);
                return Ok(Some(self.finish(&data, key, size, changed, fresh_hw)));
            }
            // 还没编好，或编码器丢了这一帧：这一次没有帧发（之后再取）
            Hw::Frame(..) | Hw::Pending => {
                self.timing.convert_us = self.convert_us.saturating_sub(self.timing.scale_us);
                return Ok(None);
            }
            Hw::Off => {}
        }
        if let Input::Gpu(frame) = input {
            // 软件编码：显卡画面读回一次（只拷贝、拆平面，不转颜色、不缩放）
            let reading = std::time::Instant::now();
            let nv12 = frame.read_nv12()?;
            // 读回是在等显卡（和它的锁），不是 CPU 在算；后面拆平面才是 CPU 的活
            self.timing.wait_us = self.timing.wait_us.saturating_add(micros_since(reading));
            color::nv12_to_i420(
                nv12.data(),
                w,
                h,
                &mut self.planes.0,
                &mut self.planes.1,
                &mut self.planes.2,
            );
            self.convert_us = self.convert_us.saturating_add(micros_since(reading));
        }
        self.timing.convert_us = self.convert_us.saturating_sub(self.timing.scale_us);
        let encoding = std::time::Instant::now();
        // 后端按实际编的尺寸配置：奇数的宽高在转换时已经补成偶数（`w`、`h`），帧头里写的还是原来的 `size`
        let coded = (
            u16::try_from(w).unwrap_or(u16::MAX),
            u16::try_from(h).unwrap_or(u16::MAX),
        );
        let fresh = self.soft.size() != Some(coded);
        // 尺寸变了（或上一帧出错复位了）：重新配置，从关键帧开始；否则只告诉后端现在的码率、帧率。
        self.bps = self.budget_bps(budget);
        let rates = Rates {
            bps: self.bps,
            fps: self.fps,
        };
        if fresh {
            self.soft
                .reconfigure(coded, rates)
                .map_err(|_| Error::Inconsistent("创建 H.264 编码器失败"))?;
        } else {
            self.soft
                .set_rates(rates)
                .map_err(|_| Error::Inconsistent("H.264 编码失败"))?;
        }
        let area = match input {
            Input::Rgb(screen) if changed && !fresh && !self.force_key => {
                Some(self.changed_permille(screen))
            }
            Input::Rgb(screen) if fresh || self.force_key => {
                let _ = self.changed_permille(screen);
                None
            }
            Input::Rgb(_) | Input::Gpu(_) => None,
        };
        // 这一帧的 QP（见 qp 模块）：关键帧按大帧规则装进时限，在变的帧跟着目标，停下的帧往下补
        let kind = if fresh || self.force_key {
            qp::Kind::Key
        } else if changed {
            qp::Kind::Change
        } else {
            qp::Kind::Refine
        };
        let pixels = u64::from(size.0).saturating_mul(u64::from(size.1));
        let target = u64::try_from(budget).unwrap_or(u64::MAX);
        let key_allowed = if self.big_frame_us == 0 {
            u64::MAX
        } else if self.rate_stale && kind != qp::Kind::Refine {
            // 码率估计过期：静止时链路可能已经变差，不多押，最多一帧的预算（之后补清晰）。补清晰不在此列：
            // 它按一帧的预算一点点补，时限只用来限制单步不超过链路约 200 ms 的量（见 qp::REFINE_STEP_KEY_TIMES），
            // 停下两秒后估计就算过期，按一帧预算限的话补清晰几乎停住（微软轨迹停下后的画质 43 → 34 dB）
            target
        } else {
            self.bps.saturating_mul(self.big_frame_us) / 8_000_000
        };
        let qp = self.coarse_qp.take().unwrap_or_else(|| {
            self.qp.next(
                kind,
                target,
                key_allowed,
                pixels,
                area.map(|(_, weighted)| weighted),
            )
        });
        // 补清晰补不动了（降一步的量链路一次装不下，见 qp::REFINE_STEP_KEY_TIMES）：这个码率下就算补完，
        // 不再编（否则画面不动也一直按同一个 QP 编空帧）。画面再变时重新开始补。
        if kind == qp::Kind::Refine && !fresh && self.qp.qp().is_some_and(|now| qp >= now) {
            self.refine_left = 0;
            return Ok(None);
        }
        // 后端新开之后第一帧只能按它的默认 QP 编、要别的就得多编一次初始化时（见 BackendTraits::first_frame_qp，
        // openh264 就是这样）：只有要明显更粗（窄带宽上的大帧规则）才值得多编这一次；要更细或差不多时就按默认的编，
        // 之后补清晰补上（局域网上省掉一次编码：会话开头本来就忙）。
        let qp = match self.soft.info().traits.first_frame_qp {
            Some(default) if fresh && qp < default.saturating_add(PRIME_ABOVE) => default,
            Some(_) | None => qp,
        };
        let first = self.submit_soft(budget, qp, self.force_key || fresh)?;
        // 多编的那一次是初始化的开销（和打开编码器一样），不算进编码耗时：帧率调节按编码耗时判断 CPU
        // 跟不跟得上，算进去的话每次新开编码器都像 CPU 过载（实测 100 帧时被降到 50 帧）。
        let primed = first.timing.setup_us > 0;
        let mut computed = first.timing.compute_us;
        let (mut key, mut data) = (first.key, first.data);
        let mut qp = qp;
        // 整屏变化（切换窗口这类“场景切换”）编出来远超大帧时限：上一帧的大小推算不出这种帧。按它的实际大小
        // 推一个装得进的 QP 重编（照 libvpx 屏幕内容实时模式的 vp9_encodedframe_overshoot）。openh264 编完
        // 参考帧就更新了，不能原样重编，只能重编成关键帧——整屏都换了，关键帧和增量帧差不多大。openh264 自己
        // 检测到场景切换出的关键帧（用的还是我们给的 QP）也一样处理。
        let whole = area.is_none_or(|(count, _)| count >= qp::LARGE_CHANGE_PERMILLE);
        let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
        if let Some(q) = (kind == qp::Kind::Change && whole && self.big_frame_us != 0)
            .then(|| self.qp.overshoot_qp(bytes, qp, key_allowed))
            .flatten()
        {
            let again = self.submit_soft(budget, q, true)?;
            computed = computed.saturating_add(again.timing.compute_us);
            (key, data) = (again.key, again.data);
            qp = q;
            self.key_reason = "整屏变化编出来远超大帧时限，按实际大小重编成关键帧";
        }
        self.timing.encode_us = if primed {
            computed
        } else {
            micros_since(encoding)
        };
        let kind = if key { qp::Kind::Key } else { kind };
        self.qp.on_frame(
            kind,
            u64::try_from(data.len()).unwrap_or(u64::MAX),
            qp,
            pixels,
        );
        self.soft_converged = Some(self.qp.converged());
        Ok(Some(self.finish(&data, key, size, changed, fresh)))
    }

    /// 画面没变时取硬件攒着的帧；停了一阵还取不到，让编码器交出来。
    fn poll_hardware(&mut self) -> Hw {
        let idle = self.still >= self.refine_after();
        let Some(enc) = self.hw.as_mut() else {
            return Hw::Off;
        };
        let mut got = enc.poll();
        if matches!(got, Ok(None)) && idle {
            got = enc.drain().and_then(|()| enc.poll());
        }
        match got {
            Ok(Some((data, key))) => self.hardware_output(data, key, false),
            Ok(None) => Hw::Pending,
            Err(why) => {
                self.hardware_failed(&why);
                Hw::Off
            }
        }
    }

    /// 编好一帧之后：更新补清晰的状态，加上帧头。
    fn finish(
        &mut self,
        data: &[u8],
        key: bool,
        size: (u16, u16),
        changed: bool,
        fresh: bool,
    ) -> EncodedFrame {
        if key {
            // 为什么出了关键帧：要求的原因，或者第一帧 / 尺寸变了，或者编码器自己出的
            self.timing.key_reason = if !self.key_reason.is_empty() {
                std::mem::take(&mut self.key_reason)
            } else if fresh || self.encoded_size != Some(size) {
                "第一帧或尺寸变了"
            } else {
                "编码器自己出的"
            };
        }
        self.force_key = false;
        self.encoded_size = Some(size);
        // 画面在变：之后要补清晰；没变：这是补清晰的一帧。
        // 软件编码按 QP 判断补完没有（补到最小 QP，照 Chromium）；硬件编码不知道 QP，按大小判断。
        self.refine_left = match self.soft_converged.take() {
            Some(true) => 0,
            Some(false) => u8::MAX,
            None if changed || fresh || key => REFINE_FRAMES,
            None if data.len() < REFINE_DONE_BYTES => 0,
            None => self.refine_left.saturating_sub(1),
        };
        let header = FrameHeader {
            codec: Codec::H264,
            key,
            width: size.0,
            height: size.1,
        };
        EncodedFrame {
            payload: header.frame(data),
            key,
        }
    }

    /// 画面停下多少帧之后开始补清晰（约 200 ms）。
    const fn refine_after(&self) -> u32 {
        self.fps / 5
    }

    /// 把 `planes` 里转好的这一帧交给软件后端，按 `qp` 编；`keyframe` 要关键帧。
    ///
    /// # Errors
    /// 后端出错（它已经复位，下一帧会重新配置、从关键帧开始）。
    fn submit_soft(
        &mut self,
        budget: usize,
        qp: u8,
        keyframe: bool,
    ) -> Result<backend::EncodedOut, Error> {
        self.soft_token = self.soft_token.wrapping_add(1);
        let ask = FrameAsk {
            token: self.soft_token,
            budget,
            qp: Some(qp),
            keyframe,
        };
        let input = FrameInput::I420 {
            y: &self.planes.0,
            u: &self.planes.1,
            v: &self.planes.2,
        };
        match self.soft.submit(input, ask) {
            Ok(Submitted::Ready(Finished::Frame(frame))) if frame.token == ask.token => Ok(frame),
            // 软件后端按 QP 同步编：不攒帧、不丢帧。别的回答都是后端违约
            Ok(Submitted::Ready(_) | Submitted::Pending) | Err(_) => {
                Err(Error::Inconsistent("H.264 编码失败"))
            }
        }
    }

    /// 每帧预算换算成码率。
    fn budget_bps(&self, budget: usize) -> u64 {
        u64::try_from(budget)
            .unwrap_or(u64::MAX)
            .saturating_mul(8)
            .saturating_mul(u64::from(self.fps))
            .clamp(MIN_BPS, self.max_bps)
    }
}

/// 硬件编码器在这个尺寸下的码率下限：码控窗口里至少放得下一帧（见 [`HW_FRAME_FLOOR_PIXELS_PER_BYTE`]）。
fn hw_floor_bps(w: usize, h: usize) -> u64 {
    let pixels = u64::try_from(w.saturating_mul(h)).unwrap_or(u64::MAX);
    let frame_bytes = pixels / HW_FRAME_FLOOR_PIXELS_PER_BYTE;
    frame_bytes
        .saturating_mul(8)
        .saturating_mul(1_000_000)
        .checked_div(BIG_FRAME_US)
        .unwrap_or(0)
}

/// 后端新开之后的第一帧，要比它的默认 QP 粗这么多以上才多编一次去改第一帧的 QP。
const PRIME_ABOVE: u8 = 4;

fn micros_since(t: std::time::Instant) -> u64 {
    u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX)
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
        self.encode_input(Input::Rgb(screen), changed, budget)
    }

    fn encode_frame(
        &mut self,
        _frame_id: u32,
        frame: &Frame,
        changed: bool,
        budget: usize,
    ) -> Result<Option<EncodedFrame>, Error> {
        match frame {
            Frame::Rgb(screen) => self.encode_input(Input::Rgb(screen), changed, budget),
            Frame::Gpu(gpu) => self.encode_input(Input::Gpu(gpu.as_ref()), changed, budget),
        }
    }

    fn on_feedback(&mut self, _floor: u32, _mask: u32) {}

    fn convert_micros(&self) -> u64 {
        self.convert_us
    }

    fn timing(&self) -> EncodeTiming {
        self.timing
    }

    fn on_lost(&mut self, _frame_id: u32) {
        // 之后的增量帧都参考了丢的那一帧，接收端解不了：下一帧编关键帧。
        self.force_key = true;
        self.key_reason = "有帧丢了（接收端解不了后面的帧）";
    }

    fn wants_refine(&self) -> bool {
        // 硬件编码器还攒着帧时也要接着调用，把它们取回来
        self.force_key
            || self.resize_reason.is_some()
            || self.refine_left > 0
            || !self.hw_sent.is_empty()
    }

    fn backend(&self) -> String {
        self.backend_name()
    }

    fn set_scale(&mut self, permille: u16) {
        // 比例变了：CPU 画面的编码尺寸可能跟着变，画面不动也要编这一帧（例如升回原尺寸）；
        // 尺寸真的变了才出关键帧（显卡画面的尺寸由采集端决定，见 encode_input）
        if permille != self.scale_permille && self.encoded_size.is_some() {
            self.resize_reason = Some("编码分辨率变了");
        }
        self.scale_permille = permille;
    }

    fn set_view(&mut self, view: Option<(u16, u16)>) {
        let view = view.filter(|(w, h)| *w > 0 && *h > 0);
        if view != self.view && self.encoded_size.is_some() {
            self.resize_reason = Some("控制端窗口大小变了");
        }
        self.view = view;
    }

    fn set_rate_stale(&mut self, stale: bool) {
        self.rate_stale = stale;
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

    fn decode_picture(
        &mut self,
        header: &FrameHeader,
        data: &[u8],
    ) -> Result<Option<Picture>, Error> {
        let Some(yuv) = self
            .decoder
            .decode(data)
            .map_err(codec_error("H.264 解码失败"))?
        else {
            return Ok(None);
        };
        let (dw, dh) = yuv.dimensions();
        if dw < usize::from(header.width) || dh < usize::from(header.height) {
            return Err(Error::Inconsistent("解出的画面比帧头说的小"));
        }
        // OpenH264 出的是 I420：亮度照拷、色度交错成 NV12，不转颜色（界面在显卡上转）
        let (ys, us, vs) = yuv.strides();
        let nv12 = Nv12::from_i420(
            header.width,
            header.height,
            (yuv.y(), ys),
            (yuv.u(), us),
            (yuv.v(), vs),
        )?;
        Ok(Some(Picture::Nv12(std::sync::Arc::new(nv12))))
    }

    fn backend(&self) -> String {
        "软件 OpenH264".to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use peerdesk_core::frame::{Nv12, TestGpuFrame};
    use std::sync::Arc;

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

    /// 假的硬件编码器：第 `fail_at` 帧（从 1 数）出错；码流用 OpenH264 编，接收端照常能解。
    #[derive(Debug)]
    struct FakeHw {
        size: (u32, u32),
        frames: u32,
        fail_at: u32,
        soft: H264Encoder,
        /// 每帧都说是关键帧（模拟出问题的编码器）。
        always_key: bool,
        /// 攒一帧（异步编码器）。
        hold_back: bool,
        held: Option<(Vec<u8>, bool)>,
        ready: Option<(Vec<u8>, bool)>,
        /// 取攒着的帧时出错。
        poll_error: bool,
        /// 码控丢掉前几个要求的关键帧（输出空帧，像 VideoToolbox 低延迟码控在窄带上那样）。
        drop_keys: u32,
    }

    impl HardwareEncoder for FakeHw {
        fn name(&self) -> &str {
            "假硬件"
        }
        fn size(&self) -> (u32, u32) {
            self.size
        }
        fn encode(
            &mut self,
            nv12: &[u8],
            _bps: u64,
            force_key: bool,
        ) -> Result<Option<(Vec<u8>, bool)>, String> {
            self.frames += 1;
            if self.frames == self.fail_at {
                return Err("模拟显卡出错".into());
            }
            assert_eq!(
                nv12.len(),
                self.size.0 as usize * self.size.1 as usize * 3 / 2
            );
            // 关键帧要求必须传到
            assert!(self.frames > 1 || force_key);
            if force_key && self.drop_keys > 0 {
                self.drop_keys -= 1;
                return Ok(Some((Vec::new(), false)));
            }
            let screen = picture(
                u16::try_from(self.size.0).unwrap(),
                u16::try_from(self.size.1).unwrap(),
            );
            let f = self
                .soft
                .encode(self.frames, &screen, true, 100_000)
                .unwrap()
                .unwrap();
            let (_, data) = FrameHeader::parse(&f.payload).unwrap();
            if self.hold_back {
                // 像 Intel Mac 的编码器：收到下一帧才交出上一帧
                let out = self.held.take();
                self.held = Some((data.to_vec(), f.key));
                return Ok(out);
            }
            Ok(Some((data.to_vec(), f.key || self.always_key)))
        }
        fn poll(&mut self) -> Result<Option<(Vec<u8>, bool)>, String> {
            if self.poll_error {
                return Err("模拟取帧出错".into());
            }
            Ok(self.ready.take())
        }
        fn drain(&mut self) -> Result<(), String> {
            self.ready = self.held.take();
            Ok(())
        }
    }

    fn fake_ok(w: u32, h: u32, fps: u32, _bps: u64) -> Result<Box<dyn HardwareEncoder>, String> {
        Ok(Box::new(FakeHw {
            size: (w, h),
            frames: 0,
            fail_at: 0,
            soft: H264Encoder::new(fps).with_hardware(fake_none),
            always_key: false,
            hold_back: false,
            held: None,
            ready: None,
            poll_error: false,
            drop_keys: 0,
        }))
    }

    fn fake_always_key(
        w: u32,
        h: u32,
        fps: u32,
        _bps: u64,
    ) -> Result<Box<dyn HardwareEncoder>, String> {
        Ok(Box::new(FakeHw {
            size: (w, h),
            frames: 0,
            fail_at: 0,
            soft: H264Encoder::new(fps).with_hardware(fake_none),
            always_key: true,
            hold_back: false,
            held: None,
            ready: None,
            poll_error: false,
            drop_keys: 0,
        }))
    }

    #[test]
    fn hardware_report_lines() {
        let frames: Vec<Vec<u8>> = (0..4)
            .map(|_| screen_to_nv12(&picture(320, 192)).2)
            .collect();
        let mut yes = |_: &[u8]| true;
        let ok = report_with(fake_ok, 320, 192, &frames, &mut yes);
        assert!(ok[0].contains("假硬件") && ok[0].contains("正常"), "{ok:?}");
        let mut no = |_: &[u8]| false;
        assert!(report_with(fake_ok, 320, 192, &frames, &mut no)[0].contains("失败"));
        assert!(report_with(fake_none, 320, 192, &frames, &mut yes)[0].contains("没有可用"));
        assert!(report_with(fake_failing, 320, 192, &frames, &mut yes)[0].contains("第 2 帧"));
        // 软件解码器的检查：真的 H.264 帧解得了，没有解码器时算失败
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let f = enc
            .encode(1, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        let (_, data) = FrameHeader::parse(&f.payload).unwrap();
        let mut dec = H264Decoder::new().unwrap();
        assert!(soft_decodes(Some(&mut dec), 320, 192, data));
        assert!(!soft_decodes(None, 320, 192, data));
        // 本系统（CI 的 Linux 没有硬件编码器；Windows、macOS 上真的测）
        assert!(!hardware_report(320, 192, &frames).is_empty());
    }

    fn fake_holds_back(
        w: u32,
        h: u32,
        fps: u32,
        _bps: u64,
    ) -> Result<Box<dyn HardwareEncoder>, String> {
        Ok(Box::new(FakeHw {
            size: (w, h),
            frames: 0,
            fail_at: 0,
            soft: H264Encoder::new(fps).with_hardware(fake_none),
            always_key: false,
            hold_back: true,
            held: None,
            ready: None,
            poll_error: false,
            drop_keys: 0,
        }))
    }

    fn fake_poll_fails(
        w: u32,
        h: u32,
        fps: u32,
        _bps: u64,
    ) -> Result<Box<dyn HardwareEncoder>, String> {
        Ok(Box::new(FakeHw {
            size: (w, h),
            frames: 0,
            fail_at: 0,
            soft: H264Encoder::new(fps).with_hardware(fake_none),
            always_key: false,
            hold_back: true,
            held: None,
            ready: None,
            poll_error: true,
            drop_keys: 0,
        }))
    }

    /// 原尺寸时不看控制端窗口（控制端显卡缩放，文字最清楚）；按比例缩小时再放进窗口；尺寸真的变了才出关键帧。
    #[test]
    fn view_only_matters_when_scaled_down() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let size = |f: &EncodedFrame| {
            let (h, _) = FrameHeader::parse(&f.payload).unwrap();
            (h.width, h.height)
        };
        enc.set_view(Some((120, 72)));
        let f = enc
            .encode(1, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        assert_eq!(size(&f), (320, 192), "原尺寸");
        enc.set_view(Some((100, 60)));
        assert!(enc.wants_refine(), "窗口变了：画面不动也要编一次");
        let f = enc.encode(2, &picture(320, 192), false, 50_000).unwrap();
        assert!(f.is_none_or(|f| !f.key), "尺寸没变：不出关键帧");
        enc.set_scale(500);
        let f = enc
            .encode(3, &picture(320, 192), false, 50_000)
            .unwrap()
            .unwrap();
        assert!(f.key);
        assert_eq!(size(&f), (100, 60), "半分辨率 160×96 再放进窗口");
        assert_eq!(enc.timing().key_reason, "编码分辨率变了");
        enc.set_view(None);
        let f = enc
            .encode(4, &picture(320, 192), false, 50_000)
            .unwrap()
            .unwrap();
        assert!(f.key);
        assert_eq!(size(&f), (160, 96));
        assert_eq!(enc.timing().key_reason, "控制端窗口大小变了");
    }

    /// 第一帧不收（像 Media Foundation 的 `MF_E_NOTACCEPTING`），之后照常编。
    #[derive(Debug)]
    struct RejectsFirst {
        calls: u32,
        rejected: bool,
        soft: H264Encoder,
    }

    impl HardwareEncoder for RejectsFirst {
        fn name(&self) -> &str {
            "不收第一帧"
        }
        fn size(&self) -> (u32, u32) {
            (320, 192)
        }
        fn encode(
            &mut self,
            _nv12: &[u8],
            _bps: u64,
            force_key: bool,
        ) -> Result<Option<(Vec<u8>, bool)>, String> {
            self.calls += 1;
            self.rejected = self.calls == 1;
            if self.rejected {
                return Ok(None);
            }
            // 第一帧的关键帧要求没送到：第二帧必须重新要求
            assert!(self.calls != 2 || force_key, "关键帧要求丢了");
            let f = self
                .soft
                .encode(self.calls, &picture(320, 192), true, 100_000)
                .unwrap()
                .unwrap();
            let (_, data) = FrameHeader::parse(&f.payload).unwrap();
            Ok(Some((data.to_vec(), f.key)))
        }
        fn last_rejected(&self) -> bool {
            self.rejected
        }
    }

    fn fake_rejects_first(
        _w: u32,
        _h: u32,
        fps: u32,
        _bps: u64,
    ) -> Result<Box<dyn HardwareEncoder>, String> {
        Ok(Box::new(RejectsFirst {
            calls: 0,
            rejected: false,
            soft: H264Encoder::new(fps).with_hardware(fake_none),
        }))
    }

    /// 诊断日志用的分步耗时和关键帧原因。
    #[test]
    fn timing_and_keyframe_reasons() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let f = enc
            .encode(1, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        assert!(f.key);
        let t = enc.timing();
        assert_eq!((t.width, t.height, t.hardware), (320, 192, false));
        assert_eq!(t.key_reason, "第一帧或尺寸变了");
        enc.on_lost(1);
        enc.encode(2, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        assert_eq!(enc.timing().key_reason, "有帧丢了（接收端解不了后面的帧）");
        enc.encode(3, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        assert_eq!(enc.timing().key_reason, "", "不是关键帧");
        enc.set_scale(500);
        enc.encode(4, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        let t = enc.timing();
        assert_eq!(t.key_reason, "编码分辨率变了");
        assert_eq!((t.width, t.height), (160, 96));
        // 画面没变、什么都不编：耗时清零，不沿用上一次的
        let _ = enc.encode(5, &picture(320, 192), false, 50_000).unwrap();
        assert_eq!(enc.convert_micros(), 0);
    }

    /// 编码器没收的帧不等它的输出（不算攒着），它带的关键帧要求留给下一帧。
    #[test]
    fn rejected_frame_keeps_the_keyframe_request() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_rejects_first);
        assert_eq!(enc.backend_name(), "软件 OpenH264", "还没开始编码");
        assert!(enc
            .encode(1, &picture(320, 192), true, 50_000)
            .unwrap()
            .is_none());
        assert!(enc.hw_sent.is_empty(), "没收的帧不算攒着");
        let f = enc
            .encode(2, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        assert!(f.key);
        assert_eq!(enc.backend_name(), "硬件 不收第一帧");
    }

    /// 画面停下后取攒着的帧时编码器出错：改用软件编码（说明原因），之后照常出画面。
    #[test]
    fn hardware_error_while_draining_falls_back_to_software() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_poll_fails);
        assert!(enc
            .encode(1, &picture(320, 192), true, 50_000)
            .unwrap()
            .is_none());
        assert!(enc
            .encode(2, &picture(320, 192), true, 50_000)
            .unwrap()
            .is_some());
        // 画面不变：去取攒着的帧，出错
        assert!(enc
            .encode(3, &picture(320, 192), false, 50_000)
            .unwrap()
            .is_none());
        assert_eq!(
            enc.backend_name(),
            "软件 OpenH264（硬件出错：模拟取帧出错）"
        );
        let f = enc
            .encode(4, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        assert!(f.key, "改用软件后从关键帧开始");
    }

    /// 同步的编码器（Media Foundation）不攒帧：默认的 poll、drain 什么都不做。
    #[test]
    fn synchronous_hardware_has_nothing_pending() {
        #[derive(Debug)]
        struct Sync;
        impl HardwareEncoder for Sync {
            fn name(&self) -> &str {
                "同步"
            }
            fn size(&self) -> (u32, u32) {
                (2, 2)
            }
            fn encode(
                &mut self,
                _: &[u8],
                _: u64,
                _: bool,
            ) -> Result<Option<(Vec<u8>, bool)>, String> {
                Ok(Some((vec![1], true)))
            }
        }
        let mut e = Sync;
        assert_eq!(e.poll(), Ok(None));
        assert_eq!(e.drain(), Ok(()));
        assert_eq!(e.encode(&[], 0, true), Ok(Some((vec![1], true))));
        assert_eq!((e.name(), e.size()), ("同步", (2, 2)));
    }

    /// 异步编码器攒一帧（像 Intel Mac）：每次取回上一帧，只有第一帧是关键帧，软件解码接得上；
    /// 画面停下后把攒着的最后一帧取出来，不会丢。
    #[test]
    fn asynchronous_hardware_is_drained_in_order() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_holds_back);
        let mut dec = H264Decoder::new().unwrap();
        let mut got = Vec::new();
        for id in 1..8u32 {
            let f = enc.encode(id, &picture(320, 192), true, 50_000).unwrap();
            if id == 1 {
                assert!(f.is_none(), "第一帧还攒在编码器里");
                assert!(enc.wants_refine(), "有攒着的帧：要接着调用");
            }
            got.extend(f);
        }
        assert_eq!(got.len(), 6);
        assert!(
            got[0].key && got.iter().skip(1).all(|f| !f.key),
            "只有第一帧是关键帧"
        );
        for f in &got {
            decode(&mut dec, f).unwrap();
        }
        // 画面停下：先取不到，停够了让编码器交出最后一帧
        let mut last = None;
        for id in 8..40u32 {
            if let Some(f) = enc.encode(id, &picture(320, 192), false, 50_000).unwrap() {
                last = Some(f);
                break;
            }
        }
        decode(&mut dec, &last.expect("最后一帧要取出来")).unwrap();
        assert_eq!(enc.backend_name(), "硬件 假硬件");
    }

    #[test]
    fn hardware_that_only_makes_keyframes_is_dropped() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_always_key);
        let mut keys = 0;
        for id in 1..12u32 {
            // 画面一直在变（鼠标在动）
            let f = enc
                .encode(id, &picture(320, 192), true, 50_000)
                .unwrap()
                .unwrap();
            keys += u32::from(f.key);
        }
        // 第一帧（要求的）加 5 个没要求的之后改用软件，之后是增量帧
        assert!(enc.backend_name().starts_with("软件 OpenH264（"));
        assert!(keys <= 1 + UNASKED_KEY_LIMIT + 1, "{keys}");
    }

    /// 测试用的显卡画面：`picture` 转成 NV12。
    fn gpu_frame(w: u16, h: u16) -> Arc<TestGpuFrame> {
        let screen = picture(w, h);
        let (mut y, mut u, mut v, mut nv12) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        color::rgb_to_i420(screen.pixels(), usize::from(w), &mut y, &mut u, &mut v);
        color::i420_to_nv12(&y, &u, &v, &mut nv12);
        Arc::new(TestGpuFrame::new(Nv12::new(w, h, nv12).unwrap()))
    }

    /// 回归（2026-11-27 Windows 局域网日志，`P-fps-bitrate.md` W1）：硬件编码器运行中出错改用软件之后，过一阵再试硬件；
    /// 每次换编码器从关键帧开始，接收端一路都解得出；出错次数多了就一直用软件。
    #[test]
    fn hardware_is_tried_again_after_a_failure_and_given_up_after_a_few() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_failing);
        // 默认隔 30 秒才再试：出错后紧接着的帧还是软件
        let mut dec = H264Decoder::new().unwrap();
        let mut id = 0u32;
        let mut next = |enc: &mut H264Encoder, dec: &mut H264Decoder| {
            id += 1;
            let f = enc
                .encode(id, &picture(320, 192), true, 5000)
                .unwrap()
                .unwrap();
            decode(dec, &f).unwrap();
            (f.key, enc.timing().hardware)
        };
        assert_eq!(next(&mut enc, &mut dec), (true, true));
        assert_eq!(next(&mut enc, &mut dec), (false, true));
        assert_eq!(
            next(&mut enc, &mut dec),
            (true, false),
            "第 3 帧出错，软件从关键帧开始"
        );
        assert_eq!(next(&mut enc, &mut dec), (false, false), "没到时间，不试");
        assert!(enc.backend_name().starts_with("软件 OpenH264（硬件出错"));
        // 到时间了（测试里把间隔改成 0）：再试硬件，从关键帧开始
        enc.hw_retry_after = std::time::Duration::ZERO;
        enc.hw_retry_at = Some(std::time::Instant::now());
        assert_eq!(next(&mut enc, &mut dec), (true, true));
        assert_eq!(enc.timing().key_reason, "再试硬件编码");
        assert_eq!(enc.backend_name(), "硬件 假硬件");
        assert_eq!(next(&mut enc, &mut dec), (false, true));
        // 假硬件每次打开后的第 3 帧都出错：第 2、3、4 次出错
        for failures in 2..=4u32 {
            assert_eq!(
                next(&mut enc, &mut dec),
                (true, false),
                "第 {failures} 次出错"
            );
            assert_eq!(enc.hw_failures, failures);
            if failures <= HW_RETRY_MAX {
                assert_eq!(next(&mut enc, &mut dec), (true, true), "再试");
                assert_eq!(next(&mut enc, &mut dec), (false, true));
            }
        }
        // 出错 4 次（再试了 3 次）：这场会话一直用软件
        assert!(enc.hw_retry_at.is_none());
        for _ in 0..3 {
            assert_eq!(next(&mut enc, &mut dec), (false, false));
        }
    }

    /// 回归（2026-11-27 Mac 日志，`P-fps-bitrate.md` M2）：码率低到硬件编码器的码控放不下一帧时，它把帧整个丢掉；
    /// 不能一直丢下去——每丢一次把给它的码率下限翻倍，几帧之内一定出帧；丢的那几帧告诉发送端“是预算不够”。
    #[test]
    fn a_hardware_encoder_that_drops_frames_for_lack_of_bitrate_is_given_more() {
        use std::sync::atomic::{AtomicU64, Ordering};
        static LAST_BPS: AtomicU64 = AtomicU64::new(0);
        /// 码率不到 1 Mbps 就把帧丢掉的硬件编码器（像 VideoToolbox 带 100 ms 的 DataRateLimits）。
        #[derive(Debug)]
        struct Starved(FakeHw);
        impl HardwareEncoder for Starved {
            fn name(&self) -> &str {
                "码率不够就丢帧"
            }
            fn size(&self) -> (u32, u32) {
                self.0.size
            }
            fn encode(
                &mut self,
                nv12: &[u8],
                bps: u64,
                force_key: bool,
            ) -> Result<Option<(Vec<u8>, bool)>, String> {
                LAST_BPS.store(bps, Ordering::Relaxed);
                if bps < 1_000_000 {
                    return Ok(Some((Vec::new(), false)));
                }
                // 之前要的关键帧都被丢了：出帧的这一次必须还带着关键帧要求（假硬件的第一帧会检查）
                self.0.encode(nv12, bps, force_key)
            }
        }
        fn starved(w: u32, h: u32, fps: u32, _: u64) -> Result<Box<dyn HardwareEncoder>, String> {
            Ok(Box::new(Starved(FakeHw {
                size: (w, h),
                frames: 0,
                fail_at: 0,
                soft: H264Encoder::new(fps).with_hardware(fake_none),
                always_key: false,
                hold_back: false,
                held: None,
                ready: None,
                poll_error: false,
                drop_keys: 0,
            })))
        }
        let mut enc = H264Encoder::new(30).with_hardware(starved);
        let screen = picture(64, 64);
        // 预算 100 字节一帧：24 kbps，按下限是 64 kbps。64 → 128 → 256 → 512 kbps 都被丢，1024 kbps 出帧
        let mut seen = Vec::new();
        let mut out = None;
        for id in 1..=8 {
            let f = enc.encode(id, &screen, true, 100).unwrap();
            seen.push((LAST_BPS.load(Ordering::Relaxed), enc.timing().dropped));
            if f.is_some() {
                out = f;
                break;
            }
            assert!(enc.wants_refine(), "丢了的帧还要接着编");
        }
        assert_eq!(
            seen,
            [
                (64_000, true),
                (128_000, true),
                (256_000, true),
                (512_000, true),
                (1_024_000, false)
            ]
        );
        assert!(out.unwrap().key);
        assert!(enc.timing().hardware, "不因为丢帧换软件");
        // 学到的下限留着：之后预算再小也不低于它
        assert!(enc.encode(9, &screen, true, 100).unwrap().is_some());
        assert_eq!(LAST_BPS.load(Ordering::Relaxed), 1_024_000);
        // 按尺寸算的下限：码控的 100 ms 窗口里放得下 0.02 比特每像素的一帧
        assert_eq!(hw_floor_bps(2880, 1800), 1_036_800);
        assert_eq!(hw_floor_bps(1920, 1080), 414_720);
        assert_eq!(hw_floor_bps(0, 0), 0);
    }

    /// 读回显卡画面的时间单独记（等显卡，不是 CPU 在算）；CPU 上转换的画面没有这一项。
    #[test]
    fn gpu_readback_time_is_reported_separately_from_cpu_work() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let frame = Frame::Gpu(gpu_frame(320, 192));
        enc.encode_frame(1, &frame, true, 50_000).unwrap().unwrap();
        let t = enc.timing();
        assert!(t.wait_us <= t.convert_us, "读回包含在转换里：{t:?}");
        assert!(!t.dropped);
        enc.encode(2, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        assert_eq!(enc.timing().wait_us, 0, "CPU 画面没有读回");
        // 硬件编码器不认这个句柄：读回一次再交，也记在等待里
        let mut hw = H264Encoder::new(30).with_hardware(fake_ok);
        hw.encode_frame(1, &frame, true, 50_000).unwrap().unwrap();
        let t = hw.timing();
        assert!(t.hardware && t.wait_us <= t.convert_us, "{t:?}");
    }

    /// 软件编码一段固定的操作，返回每一帧的（是否出帧、是否关键帧、字节数、码流的哈希）。
    /// 覆盖软件路径的每个分支：新开编码器（按默认 QP、窄带上先初始化再按粗 QP 编）、在变的帧、静止后补清晰、
    /// 丢帧后要关键帧、整屏变化超出大帧时限重编、换尺寸、显卡画面读回、码率估计过期。
    fn software_script() -> Vec<(bool, bool, usize, u64)> {
        fn hash(data: &[u8]) -> u64 {
            data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
                (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3)
            })
        }
        let mut out = Vec::new();
        let mut push = |f: Option<EncodedFrame>| {
            out.push(f.map_or((false, false, 0, 0), |f| {
                (true, f.key, f.payload.len(), hash(&f.payload))
            }));
        };
        let a = picture(640, 360);
        let mut b = a.clone();
        b.fill(40, 40, 200, 120, 0x0020_40ff);
        let mut c = Screen::new(640, 360, 0x00f0_f0f0);
        for i in 0..40u16 {
            c.fill(
                i * 16,
                (i * 7) % 300,
                12,
                40,
                0x0010_1010 + u32::from(i) * 0x0003_0507,
            );
        }
        // 宽带：新开按默认 QP；变化；静止补清晰直到不再出帧
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        push(enc.encode(1, &a, true, 50_000).unwrap());
        push(enc.encode(2, &b, true, 50_000).unwrap());
        for id in 3..30 {
            push(enc.encode(id, &b, false, 50_000).unwrap());
        }
        // 丢帧后要关键帧；整屏变化（预算小，触发按实际大小重编）；再变回来
        enc.on_lost(30);
        push(enc.encode(31, &b, true, 20_000).unwrap());
        push(enc.encode(32, &c, true, 1_500).unwrap());
        push(enc.encode(33, &a, true, 1_500).unwrap());
        // 码率估计过期时的变化和补清晰
        enc.set_rate_stale(true);
        push(enc.encode(34, &b, true, 3_000).unwrap());
        for id in 35..45 {
            push(enc.encode(id, &b, false, 3_000).unwrap());
        }
        enc.set_rate_stale(false);
        // 换尺寸（编码比例减半）：重新打开；再换回来
        enc.set_scale(500);
        push(enc.encode(45, &a, true, 8_000).unwrap());
        push(enc.encode(46, &b, true, 8_000).unwrap());
        enc.set_scale(peerdesk_core::scale::FULL_PERMILLE);
        push(enc.encode(47, &b, true, 8_000).unwrap());
        // 窄带：新开时要明显更粗的 QP（先初始化一次再编）；之后在变、补清晰
        let mut narrow = H264Encoder::new(30).with_hardware(fake_none);
        push(narrow.encode(1, &a, true, 300).unwrap());
        push(narrow.encode(2, &b, true, 300).unwrap());
        for id in 3..12 {
            push(narrow.encode(id, &b, false, 300).unwrap());
        }
        // 奇数的宽高（转换时补成偶数再编）
        let mut odd = H264Encoder::new(30).with_hardware(fake_none);
        let o = picture(641, 361);
        push(odd.encode(1, &o, true, 20_000).unwrap());
        let mut o2 = o.clone();
        o2.fill(100, 100, 301, 151, 0x00a0_2020);
        push(odd.encode(2, &o2, true, 20_000).unwrap());
        push(odd.encode(3, &o2, true, 400).unwrap());
        // 显卡画面：读回 NV12 再编
        let mut gpu = H264Encoder::new(60).with_hardware(fake_none);
        let g = Frame::Gpu(gpu_frame(320, 192));
        push(gpu.encode_frame(1, &g, true, 20_000).unwrap());
        push(gpu.encode_frame(2, &g, true, 20_000).unwrap());
        push(gpu.encode_frame(3, &g, false, 20_000).unwrap());
        out
    }

    /// 搬到后端接口（整体重新设计 C1）前后，软件编码逐字节不变。基准是在搬迁之前的提交（dev 2513645）上
    /// 用同一段 [`software_script`] 录的：每一帧是否出帧、是否关键帧、字节数、码流的哈希。
    /// 只在 Linux x86_64 上比：openh264 在别的系统、别的指令集上不保证每个字节都一样，那里只检查跑得通。
    #[test]
    fn software_encoding_is_byte_identical_to_before_the_backend_interface() {
        let got = software_script();
        assert_eq!(got.len(), 63);
        assert!(
            got.iter().filter(|r| r.0).count() > 20,
            "多数分支都要真的出帧"
        );
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            let before: [(bool, bool, usize, u64); 63] = [
                (true, true, 10140, 12467316667972714207),
                (true, false, 64957, 11245756257944888205),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (true, true, 28622, 14855844185393296398),
                (true, true, 1730, 8188028110477398655),
                (true, true, 2442, 9569077134605663632),
                (true, false, 3879, 2594102178545637662),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (true, false, 14623, 8189313834945198341),
                (true, false, 14839, 11486710695334218289),
                (true, false, 13989, 18086222159814628434),
                (true, false, 12649, 12899486267850091652),
                (true, false, 25497, 14568573672304195533),
                (true, true, 1384, 13416966219816767039),
                (true, false, 10580, 14652061378435444301),
                (true, true, 10175, 14646049696086142616),
                (true, true, 1352, 5247130174107880173),
                (true, false, 244, 10110679835351519157),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (false, false, 0, 0),
                (true, false, 35, 16337137600208355028),
                (true, false, 185, 10120661390380055447),
                (true, false, 78, 9882511896813594477),
                (true, false, 123, 17342484993396303408),
                (true, true, 10556, 10426616145667090634),
                (true, false, 40928, 8284539310635470557),
                (true, false, 18, 16216706742597887763),
                (true, true, 3254, 2722084851986203129),
                (true, false, 18, 12542253289644810899),
                (false, false, 0, 0),
            ];
            for (i, (now, was)) in got.iter().zip(before.iter()).enumerate() {
                assert_eq!(now, was, "第 {i} 帧和搬迁之前不一样");
            }
        }
    }

    /// 显卡画面 + 软件编码：读回一次 NV12（只拷贝、拆平面，不转颜色、不缩放），尺寸就是画面的尺寸
    /// （采集端已经按编码尺寸采好，比例和窗口不再管）；画面没变不读；读不出来报错。
    #[test]
    fn gpu_frames_with_software_encoding_read_back_once() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        enc.set_scale(500);
        enc.set_view(Some((100, 60)));
        let g = gpu_frame(320, 192);
        let frame = Frame::Gpu(g.clone());
        let f = enc.encode_frame(1, &frame, true, 50_000).unwrap().unwrap();
        let (h, data) = FrameHeader::parse(&f.payload).unwrap();
        assert_eq!((h.width, h.height), (320, 192));
        assert!(f.key);
        assert_eq!(g.reads(), 1);
        let t = enc.timing();
        assert!(t.nv12_direct && !t.hardware, "{t:?}");
        assert_eq!(t.scale_us, 0, "不缩放");
        // 解出来和原画面一致（只是 H.264 的损失）
        let mut dec = H264Decoder::new().unwrap();
        let seen = dec.decode(&h, data).unwrap().unwrap().clone();
        assert!(psnr(&picture(320, 192), &seen) > 30.0);
        let _ = enc.encode_frame(2, &frame, false, 50_000).unwrap();
        assert_eq!(g.reads(), 1, "画面没变不读");
        let bad = Frame::Gpu(Arc::new(
            TestGpuFrame::new(Nv12::new(320, 192, vec![16; 320 * 192 * 3 / 2]).unwrap()).failing(),
        ));
        assert!(enc.encode_frame(3, &bad, true, 50_000).is_err());
    }

    /// 认得显卡句柄的硬件编码器：直接交，不读回；认不得的：读回一次 NV12 交给它。
    #[test]
    fn gpu_frames_go_straight_to_hardware_that_accepts_them() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_direct);
        let g = gpu_frame(320, 192);
        let frame = Frame::Gpu(g.clone());
        let f = enc.encode_frame(1, &frame, true, 50_000).unwrap().unwrap();
        assert!(f.key);
        assert_eq!(g.reads(), 0, "直接交给硬件，不读回");
        assert!(enc.timing().hardware && enc.timing().nv12_direct);
        assert!(enc.backend_name().contains("假硬件"));
        assert!(
            enc.encode_frame(2, &frame, true, 50_000).is_ok(),
            "同一个编码器接着编"
        );
        // 认不得句柄
        let mut enc = H264Encoder::new(30).with_hardware(fake_ok);
        let g = gpu_frame(320, 192);
        let frame = Frame::Gpu(g.clone());
        assert!(enc.encode_frame(1, &frame, true, 50_000).unwrap().is_some());
        assert_eq!(g.reads(), 1);
        assert!(enc.timing().hardware);
        // 默认的硬件编码器不接显卡画面
        let mut hw = fake_ok(2, 2, 30, 1).unwrap();
        assert!(!hw.accepts(&GpuHandle::None));
        assert!(hw.encode_gpu(g.as_ref(), 1, false).is_err());
        // CPU 画面照常
        let mut enc = H264Encoder::new(30).with_hardware(fake_direct);
        let rgb = Frame::rgb(picture(320, 192));
        assert!(enc.encode_frame(1, &rgb, true, 50_000).unwrap().is_some());
    }

    /// 认得显卡句柄的假硬件编码器（像 VideoToolbox 认 CVPixelBuffer）。
    #[derive(Debug)]
    struct FakeDirect {
        size: (u32, u32),
        soft: H264Encoder,
        frames: u32,
    }

    impl FakeDirect {
        fn out(&mut self) -> Option<(Vec<u8>, bool)> {
            self.frames += 1;
            let screen = picture(
                u16::try_from(self.size.0).unwrap(),
                u16::try_from(self.size.1).unwrap(),
            );
            let f = self
                .soft
                .encode(self.frames, &screen, true, 100_000)
                .unwrap()?;
            let (_, data) = FrameHeader::parse(&f.payload).unwrap();
            Some((data.to_vec(), f.key))
        }
    }

    impl HardwareEncoder for FakeDirect {
        fn name(&self) -> &str {
            "假硬件（直接收显卡画面）"
        }
        fn size(&self) -> (u32, u32) {
            self.size
        }
        fn encode(
            &mut self,
            _nv12: &[u8],
            _bps: u64,
            _force_key: bool,
        ) -> Result<Option<(Vec<u8>, bool)>, String> {
            Ok(self.out())
        }
        fn accepts(&self, handle: &GpuHandle) -> bool {
            *handle == GpuHandle::None
        }
        fn encode_gpu(
            &mut self,
            frame: &dyn GpuFrame,
            _bps: u64,
            force_key: bool,
        ) -> Result<Option<(Vec<u8>, bool)>, String> {
            assert_eq!(
                frame.size(),
                (
                    u16::try_from(self.size.0).unwrap(),
                    u16::try_from(self.size.1).unwrap()
                )
            );
            assert!(self.frames > 0 || force_key, "第一帧要求关键帧");
            Ok(self.out())
        }
    }

    fn fake_direct(
        w: u32,
        h: u32,
        fps: u32,
        _bps: u64,
    ) -> Result<Box<dyn HardwareEncoder>, String> {
        Ok(Box::new(FakeDirect {
            size: (w, h),
            soft: H264Encoder::new(fps).with_hardware(fake_none),
            frames: 0,
        }))
    }

    fn fake_failing(
        w: u32,
        h: u32,
        fps: u32,
        _bps: u64,
    ) -> Result<Box<dyn HardwareEncoder>, String> {
        Ok(Box::new(FakeHw {
            size: (w, h),
            frames: 0,
            fail_at: 3,
            soft: H264Encoder::new(fps).with_hardware(fake_none),
            always_key: false,
            hold_back: false,
            held: None,
            ready: None,
            poll_error: false,
            drop_keys: 0,
        }))
    }

    fn fake_none(_: u32, _: u32, _: u32, _: u64) -> Result<Box<dyn HardwareEncoder>, String> {
        Err("没有".into())
    }

    /// 假的硬件解码器：先说“还没关键帧”，再给一幅比帧头大的画面（对齐），之后出错。
    #[derive(Debug)]
    struct FakeHwDec(u32);

    impl HardwareDecoder for FakeHwDec {
        fn name(&self) -> &str {
            "假硬件"
        }
        fn decode(&mut self, _data: &[u8]) -> Result<Option<(usize, usize, Vec<u32>)>, String> {
            self.0 += 1;
            match self.0 {
                1 => Ok(None),
                2 => Ok(Some((32, 18, vec![0x00ff_0000; 32 * 18]))),
                3 => Ok(Some((8, 8, vec![0; 64]))),
                _ => Err("模拟出错".into()),
            }
        }
    }

    /// 出 NV12 的假硬件解码器（像 Media Foundation）：第一次没画面，第二次按 16 对齐多出边，
    /// 第三次正好，第四次比帧头小，之后出错。
    #[derive(Debug)]
    struct FakeNv12Dec(u32);

    impl HardwareDecoder for FakeNv12Dec {
        fn name(&self) -> &str {
            "假硬件 NV12"
        }
        fn decode(&mut self, _data: &[u8]) -> Result<Option<(usize, usize, Vec<u32>)>, String> {
            Err("应当用 decode_nv12".into())
        }
        fn gives_nv12(&self) -> bool {
            true
        }
        fn decode_nv12(&mut self, _data: &[u8]) -> Result<Option<Nv12>, String> {
            self.0 += 1;
            let gray = |w: u16, h: u16| {
                let n = usize::from(w) * usize::from(h);
                Nv12::new(w, h, [vec![200; n], vec![128; n / 2]].concat()).unwrap()
            };
            match self.0 {
                1 => Ok(None),
                2 => Ok(Some(gray(32, 18))),
                3 => Ok(Some(gray(30, 16))),
                4 => Ok(Some(gray(8, 8))),
                _ => Err("模拟出错".into()),
            }
        }
    }

    /// 解码交给界面的画面：硬件出 NV12 的直接交（只裁对齐多出的边，不转颜色）；出 RGB 的交 RGB；
    /// 软件解码（OpenH264）把 I420 拼成 NV12。
    #[test]
    fn decoders_hand_nv12_to_the_display() {
        let header = FrameHeader {
            codec: Codec::H264,
            key: true,
            width: 30,
            height: 16,
        };
        let mut dec = AutoH264Decoder::with_hardware(Some(Box::new(FakeNv12Dec(0)))).unwrap();
        assert!(dec.decode_picture(&header, b"x").unwrap().is_none());
        for _ in 0..2 {
            let shown = dec.decode_picture(&header, b"x").unwrap().unwrap();
            assert!(matches!(shown, Picture::Nv12(_)), "应当是 NV12");
            assert_eq!(shown.size(), (30, 16), "对齐多出的边裁掉");
        }
        assert!(dec.decode_picture(&header, b"x").is_err(), "比帧头小");
        // 硬件出错：改用软件，真的码流解出 NV12
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let frame = enc
            .encode(1, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        let (h, data) = FrameHeader::parse(&frame.payload).unwrap();
        let shown = dec.decode_picture(&h, data).unwrap().unwrap();
        assert!(matches!(shown, Picture::Nv12(_)));
        assert!(psnr(&picture(320, 192), &shown.to_screen().unwrap()) > 30.0);
        assert_eq!(dec.backend(), "软件 OpenH264");
        // 帧头说的比解出来的大：报错
        let mut soft = H264Decoder::new().unwrap();
        let big = FrameHeader {
            width: 640,
            height: 384,
            ..h
        };
        assert!(soft.decode_picture(&big, data).is_err());
        // 没有完整的帧：没有画面
        assert!(soft.decode_picture(&h, &[]).unwrap().is_none());
        // 出 RGB 的硬件解码器（macOS）：交 RGB
        let mut rgb = AutoH264Decoder::with_hardware(Some(Box::new(FakeHwDec(1)))).unwrap();
        assert!(matches!(
            rgb.decode_picture(&header, b"x").unwrap(),
            Some(Picture::Rgb(_))
        ));
        let mut fake = FakeHwDec(0);
        assert!(!fake.gives_nv12());
        assert!(fake.decode_nv12(b"x").is_err());
        assert!(FakeNv12Dec(0).decode(b"x").is_err());
    }

    #[test]
    fn hardware_decoder_then_software_fallback() {
        let header = FrameHeader {
            codec: Codec::H264,
            key: true,
            width: 30,
            height: 16,
        };
        let mut dec = AutoH264Decoder::with_hardware(Some(Box::new(FakeHwDec(0)))).unwrap();
        assert_eq!(dec.backend(), "硬件 假硬件");
        assert_eq!(
            decoder_label("Media Foundation（CPU）"),
            "系统 Media Foundation（CPU）"
        );
        assert!(format!("{dec:?}").contains("AutoH264Decoder"));
        // 增量帧可以暂时没有画面
        let delta = FrameHeader {
            key: false,
            ..header
        };
        assert!(dec.decode(&delta, b"x").unwrap().is_none());
        // 对齐多出的边被裁掉
        let shown = dec.decode(&header, b"x").unwrap().unwrap();
        assert_eq!((shown.width(), shown.height()), (30, 16));
        assert!(dec.screen().is_some());
        // 比帧头小：报错
        assert!(dec.decode(&header, b"x").is_err());
        // 硬件出错：改用软件，用真的 OpenH264 码流能解出来，之后一直是软件
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let frame = enc
            .encode(1, &picture(320, 192), true, 50_000)
            .unwrap()
            .unwrap();
        let (h, data) = FrameHeader::parse(&frame.payload).unwrap();
        assert!(dec.decode(&h, data).unwrap().is_some());
        assert_eq!(dec.backend(), "软件 OpenH264");
        // 没有硬件解码器：直接软件
        let mut soft = AutoH264Decoder::with_hardware(None).unwrap();
        assert_eq!(soft.backend(), "软件 OpenH264");
        assert!(soft.decode(&h, data).unwrap().is_some());
        assert_eq!(H264Decoder::new().unwrap().backend(), "软件 OpenH264");
        // 本系统的入口：Linux 上没有硬件解码；macOS 上有（实机测试另测）
        let _ = AutoH264Decoder::new().unwrap();
        // CI 的 Linux 没有显卡：打不开并说明原因
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(open_system_hardware_decoder().is_err());
    }

    #[test]
    fn hardware_encoder_is_preferred_and_failures_fall_back_to_software() {
        let screen = picture(320, 192);
        // 能用：一直用硬件，画面不变时照样停发
        let mut enc = H264Encoder::new(30).with_hardware(fake_ok);
        let big = picture(1280, 720);
        let first = enc.encode(1, &big, true, 50_000).unwrap().unwrap();
        assert!(first.key);
        assert_eq!(enc.backend_name(), "硬件 假硬件");
        // 颜色转换单独计时（RGB → I420 → NV12），720p 一定要花时间
        assert!(enc.convert_micros() > 0);
        assert!(enc.convert_micros() < 5_000_000);
        let mut quiet = false;
        for id in 2..80 {
            if enc.encode(id, &screen, false, 5000).unwrap().is_none() {
                quiet = true;
                break;
            }
        }
        assert!(quiet, "画面不变时应当停发");
        // 第 3 帧出错：这一帧改用软件编码，从关键帧开始，之后一直用软件
        let mut enc = H264Encoder::new(30).with_hardware(fake_failing);
        let mut dec = H264Decoder::new().unwrap();
        for id in 1..3 {
            let f = enc
                .encode(id, &picture(320, 192), true, 5000)
                .unwrap()
                .unwrap();
            decode(&mut dec, &f).unwrap();
        }
        let f = enc
            .encode(3, &picture(320, 192), true, 5000)
            .unwrap()
            .unwrap();
        assert!(f.key, "换成软件编码要从关键帧开始");
        assert!(enc.backend_name().starts_with("软件 OpenH264（"));
        decode(&mut dec, &f).unwrap();
        // 打不开：直接用软件
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        assert!(enc.encode(1, &screen, true, 5000).unwrap().unwrap().key);
        assert!(enc.backend_name().starts_with("软件 OpenH264（"));
        // 这个系统的硬件编码入口：Linux 上没有（Windows、macOS 的实机测试另测）
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(open_system_hardware(64, 64, 30, 1_000_000).is_err());
        assert!(hardware_disabled(Some(std::ffi::OsStr::new("0"))));
        assert!(!hardware_disabled(Some(std::ffi::OsStr::new("1"))) && !hardware_disabled(None));
    }

    /// macOS 实机：VideoToolbox 解 OpenH264 的码流（验证格式转换、颜色）；有硬件编码器时
    /// 编几帧并用 OpenH264 解出来（验证是 Baseline，接收端解得了）。
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_videotoolbox_decode_and_encode() {
        let screen = picture(640, 360);
        let mut soft_enc = H264Encoder::new(30).with_hardware(fake_none);
        let frame = soft_enc.encode(1, &screen, true, 200_000).unwrap().unwrap();
        let (header, data) = FrameHeader::parse(&frame.payload).unwrap();
        let mut hw = open_system_hardware_decoder().unwrap();
        let decoded = hw.decode(data).unwrap();
        assert!(decoded.is_some(), "关键帧应当解出画面");
        let (w, h, pixels) = decoded.unwrap();
        assert!(w >= 640 && h >= 360);
        // 和 OpenH264 解出来的差不多（颜色转换一致）
        let mut soft = H264Decoder::new().unwrap();
        let reference = soft.decode(&header, data).unwrap().unwrap().clone();
        let cropped: Vec<u32> = (0..360)
            .flat_map(|y| pixels[y * w..y * w + 640].to_vec())
            .collect();
        let hw_screen = Screen::from_pixels(640, 360, cropped).unwrap();
        assert!(
            psnr(&hw_screen, &reference) > 30.0,
            "{}",
            psnr(&hw_screen, &reference)
        );
        match open_system_hardware(640, 360, 30, 2_000_000) {
            Ok(mut enc) => {
                let (mut y, mut u, mut v, mut nv12) =
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                let (pw, _, px) = even_pixels(&screen);
                color::rgb_to_i420(&px, pw, &mut y, &mut u, &mut v);
                color::i420_to_nv12(&y, &u, &v, &mut nv12);
                // 异步：这一次没取到就让编码器交出来
                let (bytes, key) = match enc.encode(&nv12, 2_000_000, true).unwrap() {
                    Some(got) => got,
                    None => {
                        enc.drain().unwrap();
                        enc.poll().unwrap().expect("交出攒着的帧")
                    }
                };
                assert!(key && annexb::has_idr(&bytes));
                let header = FrameHeader {
                    codec: Codec::H264,
                    key,
                    width: 640,
                    height: 360,
                };
                let mut dec = H264Decoder::new().unwrap();
                assert!(
                    dec.decode(&header, &bytes).unwrap().is_some(),
                    "OpenH264 要解得了硬件编出的码流"
                );
                // 画面一直在变（模拟鼠标移动）：之后必须是增量帧，并且软件解码器接得上。
                // 以前每帧都调 CompleteFrames，Intel Mac 上每帧都成了关键帧。
                let mut keys = 0;
                for i in 1..10u16 {
                    // 一个小方块（光标）每帧挪一点
                    let mut px = screen.pixels().to_vec();
                    let left = usize::from(i) * 24;
                    for row in 100..116 {
                        for p in px.iter_mut().skip(row * 640 + left).take(16) {
                            *p = 0x00ff_ffff;
                        }
                    }
                    color::rgb_to_i420(&px, 640, &mut y, &mut u, &mut v);
                    color::i420_to_nv12(&y, &u, &v, &mut nv12);
                    let (bytes, key) = enc
                        .encode(&nv12, 2_000_000, false)
                        .unwrap()
                        .unwrap_or_default();
                    keys += u32::from(key);
                    if !bytes.is_empty() {
                        let header = FrameHeader {
                            codec: Codec::H264,
                            key,
                            width: 640,
                            height: 360,
                        };
                        assert!(dec.decode(&header, &bytes).is_ok(), "第 {i} 帧软件解不了");
                    }
                }
                eprintln!("硬件编码器 {}：9 个变化帧里关键帧 {keys} 个", enc.name());
                assert!(keys <= 1, "没要求却出了 {keys} 个关键帧");
                log::info!("硬件编码器可用：{}", enc.name());
            }
            Err(why) => assert!(!why.is_empty()),
        }
    }

    /// Windows 实机回归（2026-11-27 两台 Intel 核显的日志，`P-fps-bitrate.md` W1）：画面静止超过卡住的时限（2 秒）
    /// 之后再交帧，硬件编码器不能被判成“没有响应”。没有硬件编码器的机器（CI 的虚拟机）什么都不测。
    #[cfg(windows)]
    #[test]
    fn windows_hardware_encoder_survives_a_pause_longer_than_the_stall_limit() {
        let Ok(mut hw) = open_system_hardware(640, 360, 30, 2_000_000) else {
            return;
        };
        let screen = picture(640, 360);
        let (mut y, mut u, mut v, mut nv12) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (w, _, pixels) = even_pixels(&screen);
        color::rgb_to_i420(&pixels, w, &mut y, &mut u, &mut v);
        color::i420_to_nv12(&y, &u, &v, &mut nv12);
        let mut got = 0usize;
        // 三轮之间各静止 2.5 秒、其间不碰编码器：第一次停之前不排空，第二次像会话里一样先把攒着的帧取走
        for round in 0..3 {
            for i in 0..5 {
                let out = hw
                    .encode(&nv12, 2_000_000, round == 0 && i == 0)
                    .map_err(|e| format!("第 {round} 轮第 {i} 帧：{e}"));
                got += usize::from(out.unwrap().is_some());
            }
            if round == 1 {
                hw.drain().unwrap();
                while hw.poll().unwrap().is_some() {
                    got += 1;
                }
            }
            if round < 2 {
                std::thread::sleep(std::time::Duration::from_millis(2_500));
            }
        }
        assert!(got > 0, "编码器一帧都没交出");
    }

    /// Windows 实机：真的调 Media Foundation。有硬件编码器就编几帧并用 OpenH264 解出来（验证是 Baseline、
    /// 接收端解得了）；没有（例如 CI 的虚拟机没有显卡）就要给出原因，并且会话照常用软件编码。
    #[cfg(windows)]
    #[test]
    fn windows_hardware_encoder_or_a_clear_reason() {
        match open_system_hardware(640, 360, 30, 2_000_000) {
            Ok(mut hw) => {
                let mut dec = H264Decoder::new().unwrap();
                let screen = picture(640, 360);
                let (mut y, mut u, mut v, mut nv12) =
                    (Vec::new(), Vec::new(), Vec::new(), Vec::new());
                let (w, _, pixels) = even_pixels(&screen);
                color::rgb_to_i420(&pixels, w, &mut y, &mut u, &mut v);
                color::i420_to_nv12(&y, &u, &v, &mut nv12);
                // 异步：交 5 帧，取回多少算多少，最后让编码器交出攒着的；第一帧是关键帧，软件解码接得上
                let mut got = Vec::new();
                for i in 0..5 {
                    got.extend(hw.encode(&nv12, 2_000_000, i == 0).unwrap());
                }
                hw.drain().unwrap();
                while let Some(frame) = hw.poll().unwrap() {
                    got.push(frame);
                }
                assert!(!got.is_empty(), "编码器一帧都没交出");
                assert!(got[0].1, "第一帧要是关键帧");
                for (data, key) in &got {
                    let header = FrameHeader {
                        codec: Codec::H264,
                        key: *key,
                        width: 640,
                        height: 360,
                    };
                    let _ = dec.decode(&header, data).unwrap();
                }
                log::info!("硬件编码器可用：{}", hw.name());
            }
            Err(why) => assert!(!why.is_empty()),
        }
        // 无论有没有硬件编码器，编码器都能编出画面
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        assert!(enc
            .encode(1, &picture(640, 360), true, 50_000)
            .unwrap()
            .is_some());
    }

    /// 720p 文字画面（白底、一行行深色小块，接近真实的办公桌面），NV12。
    #[cfg(any(windows, target_os = "macos"))]
    fn text_nv12() -> Vec<u8> {
        let mut screen = Screen::new(1280, 720, 0x00f0_f0f0);
        for row in 0..40u16 {
            for col in 0..150u16 {
                if (u32::from(row) * 7 + u32::from(col) * 13) % 5 != 0 {
                    screen.fill(20 + col * 8, 12 + row * 17, 5, 9, 0x0020_2020);
                }
            }
        }
        let (mut y, mut u, mut v, mut nv12) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let (w, _, pixels) = even_pixels(&screen);
        color::rgb_to_i420(&pixels, w, &mut y, &mut u, &mut v);
        color::i420_to_nv12(&y, &u, &v, &mut nv12);
        nv12
    }

    /// 实机（macOS VideoToolbox、Windows Media Foundation）测量：低码率下 720p 文字画面的关键帧多大，
    /// 和大帧规则的时限（[`BIG_FRAME_US`] 按码率能发的量）对照；会话第一帧和中途要的关键帧分别量。
    ///
    /// 只量不卡：2026-11-25 CI 的 Apple 芯片虚拟机上 300 kbps 时第一个关键帧 94786 字节（100 ms 的量
    /// 3750 字节），VideoToolbox 的码控缓冲管不住关键帧。各种码率上限组合的对照见 `vt_keyframe_under_rate_limits`。
    /// 没有硬件编码器（CI 的 Windows 虚拟机）时说明原因、跳过。
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn hardware_keyframe_size_at_low_bitrate() {
        let nv12 = text_nv12();
        let keys = |bps: u64| -> Option<(String, Vec<usize>)> {
            let mut hw = match open_system_hardware(1280, 720, 30, bps) {
                Ok(hw) => hw,
                Err(why) => {
                    eprintln!("没有硬件编码器，跳过：{why}");
                    return None;
                }
            };
            let mut got = Vec::new();
            for i in 0..31 {
                got.extend(hw.encode(&nv12, bps, i == 0 || i == 30).unwrap());
            }
            hw.drain().unwrap();
            while let Some(frame) = hw.poll().unwrap() {
                got.push(frame);
            }
            assert!(got.first().is_some_and(|(_, key)| *key), "第一帧要是关键帧");
            let sizes = got
                .iter()
                .filter(|(_, key)| *key)
                .map(|(d, _)| d.len())
                .collect();
            Some((hw.name().to_owned(), sizes))
        };
        let Some((name, low)) = keys(300_000) else {
            return;
        };
        let high = keys(20_000_000).map(|(_, s)| s).unwrap_or_default();
        let window = 300_000 * BIG_FRAME_US / 8_000_000;
        eprintln!(
            "{name}：720p 文字画面关键帧（第一帧、第 30 帧）300 kbps 时 {low:?} 字节（100 ms 的量 {window} 字节），\
             20 Mbps 时 {high:?} 字节"
        );
    }

    /// macOS 实机测量：VideoToolbox 的码率上限 `DataRateLimits` 几种设法下，300 kbps 时 720p 文字画面的
    /// 关键帧多大、系统接不接受这个设置（结果码 0 为接受）。低延迟码控开、关各量一遍。
    #[cfg(target_os = "macos")]
    #[test]
    fn vt_keyframe_under_rate_limits() {
        let nv12 = text_nv12();
        let bps = 300_000u64;
        let window = bps * BIG_FRAME_US / 8_000_000;
        let product = [(bps * 3 / 16, 1_000_000), (window, BIG_FRAME_US)];
        let short = [(window, BIG_FRAME_US)];
        let webrtc = [(bps * 3 / 16, 1_000_000)];
        let variants: [(&str, &[(u64, u64)]); 3] = [
            ("1 秒 1.5 倍 + 100 ms（产品）", &product),
            ("只有 100 ms", &short),
            ("只有 1 秒 1.5 倍（WebRTC）", &webrtc),
        ];
        for low_latency in [true, false] {
            for (name, pairs) in variants {
                let opts = vt::VtOptions {
                    low_latency,
                    ..vt::VtOptions::DEFAULT
                };
                let mut enc = match vt::VtEncoder::open_with(1280, 720, 30, bps, opts) {
                    Ok(enc) => enc,
                    Err(why) => {
                        eprintln!("低延迟码控 {low_latency}，{name}：打不开（{why}）");
                        continue;
                    }
                };
                let status = enc.set_rate_limits(pairs);
                for i in 0..31 {
                    enc.submit(&nv12, bps, i == 0 || i == 30).unwrap();
                }
                let (mut keys, mut total) = (Vec::new(), 0usize);
                for one in enc.flush().unwrap() {
                    let (data, key) = one.unwrap();
                    total += data.len();
                    if key {
                        keys.push(data.len());
                    }
                }
                eprintln!(
                    "VideoToolbox 300 kbps，低延迟码控 {low_latency}，{name}：设置结果 {status}（开会话时 {}），\
                     关键帧 {keys:?} 字节（100 ms 的量 {window}），31 帧共 {total} 字节",
                    enc.limits_status()
                );
            }
        }
    }

    /// macOS 实机测量：300 kbps 下编 90 帧（第 0、30、60 帧要关键帧），看码率控制丢了哪些帧、要的关键帧出没出来、
    /// 每帧最低 QP（`MinAllowedFrameQP`）能不能把关键帧压小。低延迟码控开、关各量一遍。
    #[cfg(target_os = "macos")]
    #[test]
    fn vt_keyframe_rate_control_and_qp_floor() {
        let nv12 = text_nv12();
        let bps = 300_000u64;
        for low_latency in [true, false] {
            for floor in [None, Some(30u8), Some(40), Some(48)] {
                let opts = vt::VtOptions {
                    low_latency,
                    ..vt::VtOptions::DEFAULT
                };
                let mut enc = match vt::VtEncoder::open_with(1280, 720, 30, bps, opts) {
                    Ok(enc) => enc,
                    Err(why) => {
                        eprintln!("VideoToolbox 低延迟码控 {low_latency}：打不开（{why}）");
                        continue;
                    }
                };
                let status = floor.map(|q| enc.set_min_frame_qp(q));
                for i in 0..90 {
                    enc.submit(&nv12, bps, i % 30 == 0).unwrap();
                }
                let out = enc.flush().unwrap();
                let (mut dropped, mut keys, mut total) = (Vec::new(), Vec::new(), 0usize);
                for (i, one) in out.iter().enumerate() {
                    let (data, key) = one.as_ref().unwrap();
                    total += data.len();
                    if data.is_empty() {
                        dropped.push(i);
                    }
                    if *key {
                        keys.push((i, data.len()));
                    }
                }
                eprintln!(
                    "VideoToolbox 300 kbps 90 帧（第 0/30/60 帧要关键帧），低延迟码控 {low_latency}，最低 QP {floor:?}\
                     （设置结果 {status:?}）：交出 {} 帧，丢 {} 帧 {dropped:?}，关键帧（帧号, 字节）{keys:?}，共 {total} 字节",
                    out.len(),
                    dropped.len()
                );
            }
        }
    }

    /// Windows 实机：系统 H.264 解码器解 OpenH264 的码流（验证 NV12 读取、对齐裁剪、颜色），
    /// 连着解几帧；尺寸不是 16 的倍数（360 行 → 解码器输出 368 行）。打不开时要给出原因。
    #[cfg(windows)]
    #[test]
    fn windows_media_foundation_decodes() {
        let mut hw = match open_system_hardware_decoder() {
            Ok(hw) => hw,
            Err(why) => {
                assert!(!why.is_empty());
                eprintln!("没有系统 H.264 解码器：{why}");
                return;
            }
        };
        eprintln!("解码器：{}", hw.name());
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let mut soft = H264Decoder::new().unwrap();
        for id in 1..6u32 {
            let screen = picture(640, 360);
            let frame = enc.encode(id, &screen, true, 200_000).unwrap().unwrap();
            let (header, data) = FrameHeader::parse(&frame.payload).unwrap();
            let reference = soft.decode(&header, data).unwrap().unwrap().clone();
            let started = std::time::Instant::now();
            let Some((w, h, pixels)) = hw.decode(data).unwrap() else {
                continue;
            };
            eprintln!("第 {id} 帧 {w}×{h} 解码 {:?}", started.elapsed());
            assert_eq!((w, h), (640, 360), "要裁掉对齐的行");
            let hw_screen = Screen::from_pixels(640, 360, pixels).unwrap();
            let quality = psnr(&hw_screen, &reference);
            assert!(quality > 30.0, "和软件解码的差太多：{quality}");
        }
    }

    #[test]
    fn big_keyframes_are_coarse_then_topped_off_until_sharp() {
        let size = (320u16, 192u16);
        // 低码率下的第一帧：比不限时（大帧规则关掉）时小得多，仍是关键帧
        let screen = picture(size.0, size.1);
        let mut plain = H264Encoder::new(30)
            .with_hardware(fake_none)
            .with_big_frame_us(0);
        let big = plain.encode(1, &screen, true, 300).unwrap().unwrap();
        let mut enc = H264Encoder::new(30)
            .with_hardware(fake_none)
            .with_max_qp(51);
        let mut dec = H264Decoder::new().unwrap();
        let first = enc.encode(1, &screen, true, 300).unwrap().unwrap();
        assert!(first.key);
        assert!(
            first.payload.len() * 5 < big.payload.len() * 4,
            "粗编 {} B，照常 {} B",
            first.payload.len(),
            big.payload.len()
        );
        let mut shown = decode(&mut dec, &first).unwrap();
        let coarse = psnr(&shown, &screen);
        // 画面停下：一直补到收敛（超过普通的 REFINE_FRAMES 帧也继续），之后不再发
        let mut frames = 0;
        for id in 2..400 {
            if let Some(f) = enc.encode(id, &screen, false, 300).unwrap() {
                assert!(!f.key, "补清晰不出关键帧");
                shown = decode(&mut dec, &f).unwrap_or(shown);
                frames += 1;
            }
            if !enc.wants_refine() {
                break;
            }
        }
        assert!(!enc.wants_refine(), "补到收敛就停");
        let sharp = psnr(&shown, &screen);
        assert!(
            sharp > coarse + 3.0,
            "补了 {frames} 帧：{coarse:.1} → {sharp:.1} dB"
        );
        // 已有的编码器上要求粗编下一帧（码控照常之后恢复）：这一帧小得多，不出关键帧
        let noise = |salt: u32| {
            let px = (0..u32::from(size.0) * u32::from(size.1))
                .map(|i| (i.wrapping_mul(2_654_435_761) ^ salt) & 0x00ff_ffff)
                .collect();
            Screen::from_pixels(size.0, size.1, px).unwrap()
        };
        let normal = enc.encode(500, &noise(1), true, 50_000).unwrap().unwrap();
        enc.coarse_next(51);
        let small = enc.encode(501, &noise(2), true, 50_000).unwrap().unwrap();
        assert!(!small.key && small.payload.len() < normal.payload.len());
    }

    #[test]
    fn encodes_decodes_refines_and_goes_quiet() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
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
        assert!(fresh.screen().is_none());
        assert!(psnr(&decode(&mut fresh, &key).unwrap(), &screen) > 30.0);
        assert!(fresh.screen().is_some());
    }

    #[test]
    fn odd_sizes_and_resizes_work() {
        let mut enc = H264Encoder::new(60).with_hardware(fake_none);
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
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let f = enc
            .encode(1, &picture(32, 32), true, 100_000)
            .unwrap()
            .unwrap();
        let (_, data) = FrameHeader::parse(&f.payload).unwrap();
        assert!(dec.decode(&header, data).is_err());
    }

    #[test]
    fn budget_changes_reach_the_encoder() {
        let mut enc = H264Encoder::with_max_bps(30, 1_000_000).with_hardware(fake_none);
        let _ = enc.encode(1, &picture(64, 64), true, 1000).unwrap();
        assert_eq!(enc.bps, 240_000, "预算换算成码率");
        let _ = enc.encode(2, &picture(64, 64), true, usize::MAX).unwrap();
        assert_eq!(enc.bps, 1_000_000, "不超过上限");
        let _ = enc.encode(3, &picture(64, 64), true, 0).unwrap();
        assert_eq!(enc.bps, MIN_BPS);
        // 简单画面静止后补清晰：补到最小 QP 就停
        let plain = Screen::new(64, 64, 0x0012_3456);
        let _ = enc.encode(2, &plain, true, 10_000).unwrap();
        let mut n = 0;
        for id in 3..60 {
            if enc.encode(id, &plain, false, 10_000).unwrap().is_some() {
                n += 1;
            }
        }
        assert!(n < 30, "补了 {n} 帧");
        assert!(!enc.wants_refine());
    }

    /// 码率估计过期（发送端判断）：关键帧不按大帧时限放宽，最多一帧的预算，比新鲜时小。
    #[test]
    fn a_stale_rate_does_not_stretch_the_keyframe() {
        let screen = picture(320, 192);
        let size = |stale: bool| {
            let mut enc = H264Encoder::new(30).with_hardware(fake_none);
            enc.set_rate_stale(stale);
            enc.encode(1, &screen, true, 2_000)
                .unwrap()
                .unwrap()
                .payload
                .len()
        };
        let (fresh, stale) = (size(false), size(true));
        assert!(stale < fresh, "过期 {stale} 字节，新鲜 {fresh} 字节");
    }

    /// 拖动小块（预算宽裕，QP 降到很低）之后突然整屏换成新内容、预算又很小：编出来远超大帧时限，
    /// 按实际大小重编成关键帧，写明原因。
    #[test]
    fn a_scene_change_far_over_the_limit_is_re_encoded_as_a_keyframe() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        let mut screen = Screen::new(320, 192, 0x00f0_f0f0);
        let first = enc.encode(1, &screen, true, 100_000).unwrap().unwrap();
        assert!(first.key);
        for id in 2..12u16 {
            screen.fill(id * 8, 20, 6, 6, 0x0020_2020);
            let frame = enc.encode(u32::from(id), &screen, true, 100_000).unwrap();
            assert!(frame.is_some_and(|f| !f.key));
        }
        let changed = enc
            .encode(12, &picture(320, 192), true, 300)
            .unwrap()
            .unwrap();
        assert!(changed.key, "{} 字节", changed.payload.len());
        assert_eq!(
            enc.timing().key_reason,
            "整屏变化编出来远超大帧时限，按实际大小重编成关键帧"
        );
    }

    /// 有硬件编码器就一直用硬件，码率再低也不换软件（软件编码只在硬件打不开或出错时兜底）。
    /// 以前窄带时换软件：2026-11-26 用户实测 Mac 2880×1800，起步码率 1 Mbps 还没测过就判成窄带，
    /// 软件编码每帧 100–250 ms、4–9 帧每秒，48 秒后才换回硬件。窄带上关键帧太大交给降分辨率。
    #[test]
    fn hardware_stays_even_when_keyframes_are_too_big_for_the_rate() {
        let mut enc = H264Encoder::new(30).with_hardware(fake_ok);
        let screen = picture(640, 360);
        assert!(enc.encode(1, &screen, true, 100_000).unwrap().unwrap().key);
        assert!(enc.timing().hardware);
        // 丢了帧要关键帧，预算只有 100 字节一帧：照样用硬件
        enc.on_lost(1);
        assert!(enc.encode(2, &screen, true, 100).unwrap().is_some());
        assert!(enc.timing().hardware);
        assert!(
            enc.backend_name().starts_with("硬件"),
            "{}",
            enc.backend_name()
        );
    }

    /// 硬件编码器的码控丢掉了要求的关键帧（交出空帧）：下一帧接着要，直到真的出了关键帧。
    #[test]
    fn a_keyframe_dropped_by_hardware_rate_control_is_asked_again() {
        fn dropping(
            w: u32,
            h: u32,
            fps: u32,
            _bps: u64,
        ) -> Result<Box<dyn HardwareEncoder>, String> {
            Ok(Box::new(FakeHw {
                size: (w, h),
                frames: 0,
                fail_at: 0,
                soft: H264Encoder::new(fps).with_hardware(fake_none),
                always_key: false,
                hold_back: false,
                held: None,
                ready: None,
                poll_error: false,
                drop_keys: 2,
            }))
        }
        let mut enc = H264Encoder::new(30).with_hardware(dropping);
        let screen = picture(64, 64);
        // 第一帧（要关键帧）被丢、第二帧又要也被丢，第三帧出来的是关键帧
        assert!(enc.encode(1, &screen, true, 10_000).unwrap().is_none());
        assert!(enc.wants_refine(), "还要接着编");
        assert!(enc.encode(2, &screen, false, 10_000).unwrap().is_none());
        let third = enc.encode(3, &screen, false, 10_000).unwrap().unwrap();
        assert!(third.key);
    }

    /// 720p 连续编码几帧：不能卡住，也不能 panic。在单独的线程里跑，10 秒没结束就判失败，
    /// 这样卡死和崩溃都能在 CI 上明确地报出来，而不是让会话测试莫名超时。
    #[test]
    fn hd_frames_encode_without_hanging() {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut enc = H264Encoder::new(60).with_hardware(fake_none);
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
        // Timeout：720p 编码 10 秒没有结束（卡住了）；Disconnected：编码线程 panic 了
        let (took, sizes) = rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
        assert!(sizes.iter().all(|s| *s > 0), "{took:?} {sizes:?}");
    }
}

/// Windows 的系统 H.264 解码器只对“和 Main 兼容”的码流用显卡（DXVA）：Baseline 要带
/// constraint_set1_flag（受约束的 Baseline），否则悄悄用 CPU 解。确认我们编出来的码流是这样。
#[cfg(test)]
mod sps_profile {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn openh264_output_is_constrained_baseline() {
        let mut enc = H264Encoder::new(30).with_hardware(|_, _, _, _| Err("只测软件".into()));
        let screen = Screen::new(320, 192, 0x0012_3456);
        let f = enc.encode(1, &screen, true, 50_000).unwrap().unwrap();
        let (_, data) = FrameHeader::parse(&f.payload).unwrap();
        let sps = annexb::nal_units(data)
            .into_iter()
            .find(|n| n.first().map(|b| b & 0x1f) == Some(7))
            .unwrap();
        assert_eq!(sps[1], 66, "Baseline");
        assert_ne!(
            sps[2] & 0b0100_0000,
            0,
            "constraint_set1_flag：和 Main 兼容"
        );
    }
}
