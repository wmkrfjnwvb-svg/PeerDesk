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

pub mod annexb;
pub mod audio;
mod color;
use openh264::OpenH264API;
use peerdesk_core::error::Error;
use peerdesk_core::screen::Screen;
use peerdesk_core::video::{Codec, EncodedFrame, FrameHeader, VideoDecoder, VideoEncoder};

/// 画面停下后最多再编几帧来补清晰。
pub const REFINE_FRAMES: u8 = 12;
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
        let dec: Box<dyn HardwareDecoder> = Box::new(vt::VtDecoder::new());
        Ok(dec)
    }
    #[cfg(windows)]
    {
        let dec: Box<dyn HardwareDecoder> = Box::new(mf::MfDecoder::open()?);
        Ok(dec)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Err("这个系统还没接硬件解码（目前有 macOS、Windows）".into())
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
        // Media Foundation 这边是一帧一帧等结果的（同步）
        mf::MfEncoder::encode(self, nv12, bps, force_key).map(Some)
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
        let enc: Box<dyn HardwareEncoder> = Box::new(mf::MfEncoder::open(width, height, fps, bps)?);
        Ok(enc)
    }
    #[cfg(target_os = "macos")]
    {
        let enc: Box<dyn HardwareEncoder> = Box::new(vt::VtEncoder::open(width, height, fps, bps)?);
        Ok(enc)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (width, height, fps, bps);
        Err("这个系统还没接硬件编码（目前只有 Windows 和 macOS）".into())
    }
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
    /// 硬件编码器（打开了就优先用）。
    hw: Option<Box<dyn HardwareEncoder>>,
    /// 硬件编码不能用（没有、打不开或失败过）：之后一直用软件编码。
    hw_off: bool,
    hw_open: HardwareOpener,
    /// 硬件编码器的输入（NV12），每帧复用。
    nv12: Vec<u8>,
    /// 上一次编出来的尺寸（不管用的哪个编码器）。
    encoded_size: Option<(u16, u16)>,
    /// 上一帧颜色转换的耗时（微秒）。
    convert_us: u64,
    /// 硬件编码器连续出了几个没要求的关键帧。
    unasked_keys: u32,
    /// 交给硬件编码器、还没取回的帧：每帧是否要求了关键帧（按顺序；异步编码器会攒帧）。
    hw_sent: std::collections::VecDeque<bool>,
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
            inner: None,
            bps: 0,
            force_key: false,
            refine_left: 0,
            still: 0,
            planes: (Vec::new(), Vec::new(), Vec::new()),
            hw: None,
            hw_off: false,
            hw_open: open_system_hardware,
            nv12: Vec::new(),
            encoded_size: None,
            convert_us: 0,
            unasked_keys: 0,
            hw_sent: std::collections::VecDeque::new(),
        }
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
        self.hw.as_ref().map_or_else(
            || "软件 OpenH264".to_owned(),
            |e| format!("硬件 {}", e.name()),
        )
    }

    /// 硬件出错：之后一直用软件编码（从关键帧开始）。
    fn hardware_failed(&mut self, why: &str) {
        log::warn!("硬件编码失败，改用软件编码（OpenH264）：{why}");
        self.hw = None;
        self.hw_off = true;
        self.hw_sent.clear();
        self.force_key = true;
    }

    /// 取回了一个硬件编好的帧：对上它交的时候是否要求了关键帧，检查“没要求却是关键帧”。
    fn hardware_output(&mut self, data: Vec<u8>, key: bool, fresh: bool) -> Hw {
        let asked = self.hw_sent.pop_front().unwrap_or(true);
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
            self.hw = None;
            self.hw_off = true;
            self.hw_sent.clear();
            self.force_key = true;
        }
        Hw::Frame(data, key, fresh)
    }

    /// 试着用硬件编一帧（交给编码器，取回一个编好的帧）。
    fn encode_hardware(&mut self, w: usize, h: usize, bps: u64) -> Hw {
        if self.hw_off {
            return Hw::Off;
        }
        let size = (u32::try_from(w).unwrap_or(0), u32::try_from(h).unwrap_or(0));
        let reopen = self.hw.as_ref().is_none_or(|e| e.size() != size);
        if reopen {
            self.hw = None;
            self.hw_sent.clear();
            match (self.hw_open)(size.0, size.1, self.fps, bps) {
                Ok(e) => {
                    log::info!("视频编码：硬件 {}（{}×{}）", e.name(), size.0, size.1);
                    self.hw = Some(e);
                }
                Err(why) => {
                    log::info!("视频编码：软件 OpenH264（不用硬件编码：{why}）");
                    self.hw_off = true;
                    return Hw::Off;
                }
            }
        }
        let packing = std::time::Instant::now();
        color::i420_to_nv12(
            &self.planes.0,
            &self.planes.1,
            &self.planes.2,
            &mut self.nv12,
        );
        self.convert_us = self.convert_us.saturating_add(micros_since(packing));
        let force_key = self.force_key || reopen;
        let Some(enc) = self.hw.as_mut() else {
            return Hw::Off;
        };
        match enc.encode(&self.nv12, bps, force_key) {
            Ok(got) => {
                self.hw_sent.push_back(force_key);
                // 要求已经交给编码器了：不再对后面的帧重复要求
                self.force_key = false;
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
        self.force_key = false;
        self.encoded_size = Some(size);
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
        EncodedFrame {
            payload: header.frame(data),
            key,
        }
    }

    /// 画面停下多少帧之后开始补清晰（约 200 ms）。
    const fn refine_after(&self) -> u32 {
        self.fps / 5
    }

    /// 按 `bps` 打开编码器。
    ///
    /// 第一帧（关键帧）的大小由打开时的码率决定：按上限打开的话，1.5 Mbps 的链路上第一帧有
    /// 130 KB 左右，要 0.7 秒才送得完，超过一帧的截止时间，永远收不齐，画面一直是黑的。
    /// 所以按当前预算打开；openh264 的封装把每层的最大码率设成初始码率，第一帧编完之后
    /// （编第一帧之前 OpenH264 不接受改）再把上限放开，之后码率才能往上调。
    fn open_at(&self, bps: u64) -> Result<Encoder, Error> {
        let bps = u32::try_from(bps).unwrap_or(u32::MAX);
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

    /// 每帧预算换算成码率。
    fn budget_bps(&self, budget: usize) -> u64 {
        u64::try_from(budget)
            .unwrap_or(u64::MAX)
            .saturating_mul(8)
            .saturating_mul(u64::from(self.fps))
            .clamp(MIN_BPS, self.max_bps)
    }

    /// 按每帧预算设码率。变化不到一成时不动，避免频繁调整。
    fn set_budget(&mut self, enc: &mut Encoder, budget: usize) {
        let bps = self.budget_bps(budget);
        let changed = bps.abs_diff(self.bps).saturating_mul(10) > self.bps;
        if !changed {
            return;
        }
        if set_bitrate(enc, bps) {
            self.bps = bps;
        }
    }
}

/// 放开码率上限（第一帧编完之后调用）：每层的上限和总的上限都设成 `bps`。
#[allow(unsafe_code)]
fn set_max_bitrate(enc: &mut Encoder, bps: u64) {
    for layer in [
        openh264_sys2::SPATIAL_LAYER_0,
        openh264_sys2::SPATIAL_LAYER_ALL,
    ] {
        let mut info = openh264_sys2::SBitrateInfo {
            iLayer: layer,
            iBitrate: i32::try_from(bps).unwrap_or(i32::MAX),
        };
        // SAFETY: 编码器已经初始化并编过一帧；ENCODER_OPTION_MAX_BITRATE 的参数就是 SBitrateInfo，
        // 指针指向本函数里的局部变量，调用期间有效。只改码率上限。
        let _ = unsafe {
            enc.raw_api().set_option(
                openh264_sys2::ENCODER_OPTION_MAX_BITRATE,
                std::ptr::addr_of_mut!(info).cast(),
            )
        };
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
        let size = (screen.width(), screen.height());
        // 按“上一次编出来的尺寸”判断，和用的是硬件还是软件编码器无关
        let fresh = self.encoded_size != Some(size);
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
        let converting = std::time::Instant::now();
        let (w, h, pixels) = even_pixels(screen);
        color::rgb_to_i420(
            &pixels,
            w,
            &mut self.planes.0,
            &mut self.planes.1,
            &mut self.planes.2,
        );
        self.convert_us = micros_since(converting);
        // 有硬件编码器就用它（快得多、几乎不占 CPU）；没有或失败时用 OpenH264
        let bps = self.budget_bps(budget);
        match self.encode_hardware(w, h, bps) {
            Hw::Frame(data, key, fresh_hw) if !data.is_empty() => {
                return Ok(Some(self.finish(&data, key, size, changed, fresh_hw)));
            }
            // 还没编好，或编码器丢了这一帧：这一次没有帧发（之后再取）
            Hw::Frame(..) | Hw::Pending => return Ok(None),
            Hw::Off => {}
        }
        let fresh = self.inner.as_ref().is_none_or(|(w, h, _)| (*w, *h) != size);
        // 编码器先拿出来用，编完放回去；编码出错时丢掉，下一帧重新打开（从关键帧开始）。
        let mut enc = match self.inner.take() {
            Some((_, _, mut enc)) if !fresh => {
                self.set_budget(&mut enc, budget);
                enc
            }
            Some(_) | None => {
                // 新编码器按当前预算打开（见 open_at）。
                self.bps = self.budget_bps(budget);
                self.open_at(self.bps)?
            }
        };
        if self.force_key && !fresh {
            enc.force_intra_frame();
        }
        let yuv = YUVSlices::new(
            (&self.planes.0, &self.planes.1, &self.planes.2),
            (w, h),
            (w, w / 2, w / 2),
        );
        let stream = enc.encode(&yuv).map_err(codec_error("H.264 编码失败"))?;
        let key = matches!(stream.frame_type(), FrameType::IDR | FrameType::I);
        let data = stream.to_vec();
        if fresh {
            // 第一帧编完：放开上限，之后码率才能往上调。
            set_max_bitrate(&mut enc, self.max_bps);
        }
        self.inner = Some((size.0, size.1, enc));
        Ok(Some(self.finish(&data, key, size, changed, fresh)))
    }

    fn on_feedback(&mut self, _floor: u32, _mask: u32) {}

    fn convert_micros(&self) -> u64 {
        self.convert_us
    }

    fn on_lost(&mut self, _frame_id: u32) {
        // 之后的增量帧都参考了丢的那一帧，接收端解不了：下一帧编关键帧。
        self.force_key = true;
    }

    fn wants_refine(&self) -> bool {
        // 硬件编码器还攒着帧时也要接着调用，把它们取回来
        self.force_key || self.refine_left > 0 || !self.hw_sent.is_empty()
    }

    fn backend(&self) -> String {
        self.backend_name()
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

    fn backend(&self) -> String {
        "软件 OpenH264".to_owned()
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
        }))
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
        assert_eq!(enc.backend_name(), "软件 OpenH264");
        assert!(keys <= 1 + UNASKED_KEY_LIMIT + 1, "{keys}");
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
        #[cfg(not(any(windows, target_os = "macos")))]
        assert!(open_system_hardware_decoder().is_err());
    }

    #[test]
    fn hardware_encoder_is_preferred_and_failures_fall_back_to_software() {
        let screen = picture(320, 192);
        // 能用：一直用硬件，画面不变时照样停发
        let mut enc = H264Encoder::new(30).with_hardware(fake_ok);
        let big = picture(1280, 720);
        let first = enc.encode(1, &big, true, 5000).unwrap().unwrap();
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
        assert_eq!(enc.backend_name(), "软件 OpenH264");
        decode(&mut dec, &f).unwrap();
        // 打不开：直接用软件
        let mut enc = H264Encoder::new(30).with_hardware(fake_none);
        assert!(enc.encode(1, &screen, true, 5000).unwrap().unwrap().key);
        assert_eq!(enc.backend_name(), "软件 OpenH264");
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
                for i in 0..5 {
                    let (data, key) = hw.encode(&nv12, 2_000_000, i == 0).unwrap().unwrap();
                    assert!(i > 0 || key, "第一帧要是关键帧");
                    let header = FrameHeader {
                        codec: Codec::H264,
                        key,
                        width: 640,
                        height: 360,
                    };
                    let _ = dec.decode(&header, &data).unwrap();
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
        assert_eq!(enc.bps, 240_000, "按当前预算打开");
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
