//! 交给编码器的一帧画面：CPU 上的 RGB 画面，或者系统交来的显卡画面（NV12）。
//!
//! 为什么（2026-11-25，`docs/research/K-pipeline.md` P15）：用户 Intel Mac 上一帧要在 CPU 上缩放 134 ms、
//! 转颜色 20 ms，硬件编码器本身只要 17 ms，帧率被压到每秒两三帧。业内（苹果 WWDC22 10155、Chromium、
//! FFmpeg、Sunshine）整条“采集 → 缩放 → 转颜色 → 编码”都在显卡上：系统采集时直接输出编码尺寸的 NV12，
//! 显存里的缓冲区原样交给硬件编码器，CPU 一遍都不碰。
//!
//! 分工：
//! - 采集端决定画面怎么来：能在显卡上缩放、转颜色的系统交 [`Frame::Gpu`]（已经是编码尺寸的 NV12）；
//!   做不到的（合成桌面、测试、X11）交 [`Frame::Rgb`]，由编码器在 CPU 上处理。
//! - 编码器只管编：硬件编码器认得这个显卡句柄就直接用；软件编码（OpenH264）用
//!   [`GpuFrame::read_nv12`] 读回一次——只拷贝，不转颜色、不缩放。
//! - [`Frame::to_screen`] 会在 CPU 上转成 RGB，只给体检、截图检查和测试用，会话里不用。

use std::ffi::c_void;
use std::sync::Arc;

use crate::changes::{Move, Rect};
use crate::screen::Screen;
use crate::Error;

/// 一帧画面。
#[derive(Debug, Clone)]
pub enum Frame {
    /// CPU 上的 RGB 画面（合成桌面、测试、X11 采集）：编码器自己缩放、转颜色。
    Rgb(Arc<Screen>),
    /// 系统交来的显卡画面：已经是编码尺寸的 NV12（采集端在显卡上缩放、转颜色）。
    Gpu(Arc<dyn GpuFrame>),
}

impl Frame {
    /// 用 CPU 画面创建。
    #[must_use]
    pub fn rgb(screen: Screen) -> Self {
        Self::Rgb(Arc::new(screen))
    }

    /// 尺寸（像素）。
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        match self {
            Self::Rgb(s) => (s.width(), s.height()),
            Self::Gpu(g) => g.size(),
        }
    }

    /// 内容和 `other` 一样。只比较 CPU 画面；显卡画面没法便宜地比较，由来源的变化计数判断，这里总是不一样。
    #[must_use]
    pub fn same_content(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Rgb(a), Self::Rgb(b)) => Arc::ptr_eq(a, b) || a == b,
            (Self::Rgb(_) | Self::Gpu(_), _) => false,
        }
    }

    /// 读成 RGB 画面。显卡画面会读回内存、在 CPU 上转颜色：只给体检、截图检查和测试用，会话里不用。
    ///
    /// # Errors
    /// 显卡画面读不出来。
    pub fn to_screen(&self) -> Result<Screen, Error> {
        match self {
            Self::Rgb(s) => Ok(Screen::clone(s)),
            Self::Gpu(g) => {
                let nv12 = g.read_nv12()?;
                let (w, h) = (usize::from(nv12.width()), usize::from(nv12.height()));
                let (y, uv) = nv12.planes();
                let mut pixels = Vec::new();
                nv12_to_rgb(y, uv, w, w, h, &mut pixels);
                Screen::from_pixels(nv12.width(), nv12.height(), pixels)
            }
        }
    }
}

/// 系统交来的一帧显卡画面（NV12，BT.601 有限范围，已经是编码尺寸）。由各系统的采集实现。
pub trait GpuFrame: Send + Sync + std::fmt::Debug {
    /// 尺寸（像素，宽高都是偶数）。
    fn size(&self) -> (u16, u16);

    /// 系统句柄：硬件编码器认得就直接用，不拷贝。
    fn handle(&self) -> GpuHandle;

    /// 读回内存（软件编码用）：亮度平面后面接交错的色度平面，每行按宽度紧排。只拷贝，不转颜色、不缩放。
    ///
    /// # Errors
    /// 系统不让读（缓冲区锁不住、格式不对）。
    fn read_nv12(&self) -> Result<Nv12, Error>;
}

/// 一幅画面的**内容描述**：没有像素，只有编码这条路上真正用到的那几样抽样。全模型仿真（L0，见
/// `docs/research/Q-redesign/a1-plan.md`）用它代替像素——编码器的码率控制、大帧规则、补清晰照常跑真代码，
/// 只是“变了多少、有多少细节”不再从像素算，而是从这里取。
///
/// 抽样和产品里的算法是同一份定义（`peerdesk_media::content`），所以拿同一幅画面的像素和它的描述去编，
/// 码率控制看到的数完全一样。带的是抽样本身而不是“变了千分之几”这样的结果：变了多少要和**上一次编的那一幅**比，
/// 而上一次编的是哪一幅由发送端当时的状态决定（在途满了会跳帧），事先算不出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentDesc {
    /// 源画面的第几版（每变一次加一）。假码流里带着它，显示端据此知道显示的是哪一版。
    pub version: u64,
    /// 原始尺寸。
    pub size: (u16, u16),
    /// 编码器估计变化面积用的抽样点（像素值），在原始尺寸上取。
    pub area_pixels: Arc<[u32]>,
    /// 每个抽样点的细节量。
    pub area_detail: Arc<[u32]>,
    /// 每种可能的编码尺寸各一份亮度抽样（原尺寸、降分辨率时的尺寸）。
    pub luma: Vec<LumaDesc>,
    /// 和上一版比新画的区域（原始尺寸上）。采集接口给的就是这个（见 [`crate::changes`]）；合成桌面自己知道自己画了什么。
    /// 只管这一版相对上一版：编码器从“上一帧编出来”起累计，中间跳过的版本由画面来源推进 [`crate::changes::ChangeFeed`]。
    pub dirty: Vec<Rect>,
    /// 和上一版比搬来的区域（原始尺寸上）。
    pub moves: Vec<Move>,
}

/// 某个编码尺寸下亮度平面的抽样和细节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LumaDesc {
    /// 编码尺寸（宽高都是偶数）。
    pub size: (u16, u16),
    /// 亮度平面的抽样。
    pub sample: Arc<[u8]>,
    /// 细节（千分之几）。
    pub detail_permille: u32,
    /// 每个 16×16 宏块里有细节的抽样点数（`peerdesk_media::content::mb_edges`）：脏区域里的细节要按宏块汇总，
    /// 只有整幅的千分比算不出一小块的。
    pub mb_edges: Arc<[u16]>,
}

impl ContentDesc {
    /// 编码尺寸为 `size` 时的亮度描述；没有准备这个尺寸时为 `None`。
    #[must_use]
    pub fn luma_at(&self, size: (u16, u16)) -> Option<&LumaDesc> {
        self.luma.iter().find(|l| l.size == size)
    }
}

/// 显卡画面的系统句柄。指针只在拿到它的那个 [`GpuFrame`] 活着时有效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuHandle {
    /// macOS：`CVPixelBufferRef`（像素格式 `420v`）。
    CvPixelBuffer(*const c_void),
    /// Windows：`ID3D11Texture2D*`（NV12）和它所在显卡的 LUID（硬件编码器要在同一块显卡上）。
    D3d11Texture {
        /// 纹理。
        texture: *const c_void,
        /// 显卡的 LUID（高 32 位 + 低 32 位）。
        adapter_luid: u64,
    },
    /// 没有系统句柄（测试用的假画面）：只能读回内存。
    None,
}

/// 内存里的一帧 NV12：亮度平面（宽 × 高）后面接交错的色度平面（宽 × 高 / 2），每行按宽度紧排。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nv12 {
    width: u16,
    height: u16,
    data: Vec<u8>,
}

impl Nv12 {
    /// 用现成的数据创建。宽高必须是正偶数，数据长度必须是 `宽 × 高 × 3 / 2`。
    ///
    /// # Errors
    /// 尺寸或数据长度不对。
    pub fn new(width: u16, height: u16, data: Vec<u8>) -> Result<Self, Error> {
        let (w, h) = (usize::from(width), usize::from(height));
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(Error::InvalidArgument("NV12 的宽高必须是正偶数"));
        }
        if data.len() != w * h * 3 / 2 {
            return Err(Error::Inconsistent("NV12 数据长度与尺寸不符"));
        }
        Ok(Self {
            width,
            height,
            data,
        })
    }

    /// 宽度。
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// 高度。
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// 全部数据。
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// 亮度平面和交错的色度平面。
    #[must_use]
    pub fn planes(&self) -> (&[u8], &[u8]) {
        self.data
            .split_at((usize::from(self.width) * usize::from(self.height)).min(self.data.len()))
    }

    /// 从带行宽的两个平面拷出左上角 `width`×`height`（宽高取偶数）：硬件解码器的输出常按 16 或 64 对齐。
    /// 只拷贝，不转颜色；平面不够长时缺的部分补黑。
    ///
    /// # Errors
    /// 宽高取偶数后为 0。
    pub fn from_planes(
        width: u16,
        height: u16,
        y: &[u8],
        y_pitch: usize,
        uv: &[u8],
        uv_pitch: usize,
    ) -> Result<Self, Error> {
        let (width, height) = (width & !1, height & !1);
        let (w, h) = (usize::from(width), usize::from(height));
        let mut data = Vec::with_capacity(w * h * 3 / 2);
        copy_rows(&mut data, y, y_pitch, w, h, 16);
        copy_rows(&mut data, uv, uv_pitch, w, h / 2, 128);
        Self::new(width, height, data)
    }

    /// 从 I420 的三个平面（OpenH264 的输出）拼成 NV12：亮度照拷，两个色度平面交错。不转颜色。
    ///
    /// # Errors
    /// 宽高取偶数后为 0。
    pub fn from_i420(
        width: u16,
        height: u16,
        y: (&[u8], usize),
        u: (&[u8], usize),
        v: (&[u8], usize),
    ) -> Result<Self, Error> {
        let (width, height) = (width & !1, height & !1);
        let (w, h) = (usize::from(width), usize::from(height));
        let mut data = Vec::with_capacity(w * h * 3 / 2);
        copy_rows(&mut data, y.0, y.1, w, h, 16);
        for row in 0..h / 2 {
            let us = u.0.get(row * u.1..).unwrap_or_default();
            let vs = v.0.get(row * v.1..).unwrap_or_default();
            for col in 0..w / 2 {
                data.push(us.get(col).copied().unwrap_or(128));
                data.push(vs.get(col).copied().unwrap_or(128));
            }
        }
        Self::new(width, height, data)
    }
}

/// 从每行 `pitch` 字节的平面里拷 `rows` 行、每行 `width` 字节到 `out` 后面；不够的补 `fill`。
fn copy_rows(out: &mut Vec<u8>, plane: &[u8], pitch: usize, width: usize, rows: usize, fill: u8) {
    for row in 0..rows {
        let line = plane
            .get(row * pitch..)
            .and_then(|r| r.get(..width.min(r.len())))
            .unwrap_or_default();
        out.extend_from_slice(line);
        out.extend(std::iter::repeat_n(fill, width - line.len()));
    }
}

/// 解码出来、要显示的一幅画面。
///
/// 为什么不全是 RGB（2026-11-25）：控制端一帧 1872×1170 的硬件解码后在 CPU 上转 RGB、再转成界面的格式，
/// 每帧 30–50 ms；被控端改按原分辨率编码后像素还多一倍多。业内（WebRTC、Chromium）解码出的 YUV
/// 直接交给显卡，用着色器转颜色显示。所以解码器能给 NV12 就给 NV12，界面在显卡上转；
/// [`Picture::to_screen`] 只给测试和截图检查用。
#[derive(Debug, Clone)]
pub enum Picture {
    /// RGB（块编码、macOS 硬件解码直接出的）。
    Rgb(Arc<Screen>),
    /// NV12（BT.601 有限范围）：界面用显卡转颜色。
    Nv12(Arc<Nv12>),
    /// 只有“显示的是什么”、没有像素（仿真里的假解码器出的，见 `docs/research/Q-redesign/a1-plan.md`）。
    /// 界面用不了它：产品里不会出现。
    Described(Described),
    /// 平台的解码器已经直接画到屏幕上了（手机端 MediaCodec 解到 Surface，见 `docs/research/R-android.md`），
    /// 没有像素交给界面；控制端照常按它统计帧率、冻结。
    Presented {
        /// 画面宽（像素）。
        width: u16,
        /// 画面高（像素）。
        height: u16,
    },
}

/// 假解码器交出的“显示出来的是什么”（没有像素）：仿真的显示端量尺按它判清晰、判有没有追上源画面。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Described {
    /// 显示的是源画面的第几版（源画面每变一次加一）。
    pub version: u64,
    /// 这一帧按哪个 QP 编的。
    pub qp: u8,
    /// 是不是关键帧。
    pub key: bool,
    /// 画面尺寸（像素）。
    pub size: (u16, u16),
}

impl Picture {
    /// 尺寸（像素）。
    #[must_use]
    pub fn size(&self) -> (u16, u16) {
        match self {
            Self::Rgb(s) => (s.width(), s.height()),
            Self::Nv12(n) => (n.width(), n.height()),
            Self::Described(d) => d.size,
            Self::Presented { width, height } => (*width, *height),
        }
    }

    /// 转成 RGB 画面（在 CPU 上转颜色：只给测试、截图检查用，界面不用）。
    ///
    /// # Errors
    /// 尺寸不对（不会发生：NV12 创建时已经检查过）；或者是 [`Self::Described`]、[`Self::Presented`]（没有像素）。
    pub fn to_screen(&self) -> Result<Screen, Error> {
        match self {
            Self::Rgb(s) => Ok(Screen::clone(s)),
            Self::Nv12(n) => {
                let (w, h) = (usize::from(n.width()), usize::from(n.height()));
                let (y, uv) = n.planes();
                let mut pixels = Vec::new();
                nv12_to_rgb(y, uv, w, w, h, &mut pixels);
                Screen::from_pixels(n.width(), n.height(), pixels)
            }
            Self::Described(_) | Self::Presented { .. } => Err(Error::InvalidArgument(
                "只有描述、没有像素的画面不能转成 RGB",
            )),
        }
    }
}

/// NV12 转成 `0x00RRGGBB` 像素，写进 `out`（先清空）。
///
/// `y`、`uv` 是两个平面，每行 `pitch` 字节（显卡对齐，常大于宽度）；只取左上角 `width`×`height`。
/// 系数是 BT.601 有限范围（×256 的整数），和编码端的 RGB → YUV 互逆、和软件解码出来的颜色一致。
/// 平面不够长时缺的部分当黑色。
pub fn nv12_to_rgb(
    y: &[u8],
    uv: &[u8],
    pitch: usize,
    width: usize,
    height: usize,
    out: &mut Vec<u32>,
) {
    out.clear();
    out.reserve(width * height);
    let clamp = |v: i32| u32::try_from((v >> 8).clamp(0, 255)).unwrap_or(0);
    for row in 0..height {
        let luma = y.get(row * pitch..).unwrap_or_default();
        let chroma = uv.get((row / 2) * pitch..).unwrap_or_default();
        for col in 0..width {
            let c = 298 * (i32::from(luma.get(col).copied().unwrap_or(16)) - 16) + 128;
            let pair = col & !1;
            let d = i32::from(chroma.get(pair).copied().unwrap_or(128)) - 128;
            let e = i32::from(chroma.get(pair + 1).copied().unwrap_or(128)) - 128;
            let r = clamp(c + 409 * e);
            let g = clamp(c - 100 * d - 208 * e);
            let b = clamp(c + 516 * d);
            out.push((r << 16) | (g << 8) | b);
        }
    }
}

/// 测试用的显卡画面：没有系统句柄，只能读回内存（读的次数记下来，测试据此确认读了几次）。
#[derive(Debug)]
pub struct TestGpuFrame {
    nv12: Nv12,
    reads: std::sync::atomic::AtomicU32,
    fail: bool,
}

impl TestGpuFrame {
    /// 用一帧 NV12 创建。
    #[must_use]
    pub fn new(nv12: Nv12) -> Self {
        Self {
            nv12,
            reads: std::sync::atomic::AtomicU32::new(0),
            fail: false,
        }
    }

    /// 读回总是失败（模拟系统不让读）。
    #[must_use]
    pub fn failing(mut self) -> Self {
        self.fail = true;
        self
    }

    /// 被读回了几次。
    #[must_use]
    pub fn reads(&self) -> u32 {
        self.reads.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl GpuFrame for TestGpuFrame {
    fn size(&self) -> (u16, u16) {
        (self.nv12.width(), self.nv12.height())
    }

    fn handle(&self) -> GpuHandle {
        GpuHandle::None
    }

    fn read_nv12(&self) -> Result<Nv12, Error> {
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.fail {
            return Err(Error::Inconsistent("测试：读回失败"));
        }
        Ok(self.nv12.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gray(w: u16, h: u16, luma: u8) -> Nv12 {
        let (wu, hu) = (usize::from(w), usize::from(h));
        let mut data = vec![luma; wu * hu];
        data.extend(std::iter::repeat_n(128, wu * hu / 2));
        Nv12::new(w, h, data).unwrap()
    }

    #[test]
    fn nv12_checks_its_shape() {
        assert!(Nv12::new(3, 2, vec![0; 9]).is_err(), "奇数宽");
        assert!(Nv12::new(0, 2, Vec::new()).is_err());
        assert!(Nv12::new(4, 2, vec![0; 11]).is_err(), "长度不对");
        let n = gray(4, 2, 16);
        assert_eq!((n.width(), n.height(), n.data().len()), (4, 2, 12));
        let (y, uv) = n.planes();
        assert_eq!((y.len(), uv.len()), (8, 4));
    }

    #[test]
    fn frames_report_size_and_compare_only_cpu_content() {
        let a = Frame::rgb(Screen::new(4, 2, 0x10_2030));
        let b = Frame::rgb(Screen::new(4, 2, 0x10_2030));
        let c = Frame::rgb(Screen::new(4, 2, 0));
        assert_eq!(a.size(), (4, 2));
        assert!(a.same_content(&a.clone()) && a.same_content(&b));
        assert!(!a.same_content(&c));
        let g = Frame::Gpu(Arc::new(TestGpuFrame::new(gray(4, 2, 16))));
        assert_eq!(g.size(), (4, 2));
        assert!(!g.same_content(&g.clone()), "显卡画面靠变化计数判断");
        assert!(!a.same_content(&g));
        assert_eq!(a.to_screen().unwrap(), Screen::new(4, 2, 0x10_2030));
    }

    #[test]
    fn gpu_frames_read_back_for_checks_only() {
        // 亮度 16 = 黑，235 = 白（有限范围）
        let black = Frame::Gpu(Arc::new(TestGpuFrame::new(gray(4, 2, 16))));
        assert_eq!(black.to_screen().unwrap(), Screen::new(4, 2, 0));
        let white = Frame::Gpu(Arc::new(TestGpuFrame::new(gray(4, 2, 235))));
        assert_eq!(white.to_screen().unwrap(), Screen::new(4, 2, 0x00ff_ffff));
        let t = TestGpuFrame::new(gray(2, 2, 16)).failing();
        assert_eq!(t.handle(), GpuHandle::None);
        assert!(t.read_nv12().is_err());
        assert_eq!(t.reads(), 1);
        assert!(Frame::Gpu(Arc::new(t)).to_screen().is_err());
    }

    #[test]
    fn nv12_from_pitched_and_planar_sources() {
        // 4×2，行宽 6：多出的两个字节要丢掉
        let y = [1, 2, 3, 4, 9, 9, 5, 6, 7, 8, 9, 9];
        let uv = [10, 11, 12, 13, 9, 9];
        let n = Nv12::from_planes(4, 2, &y, 6, &uv, 6).unwrap();
        assert_eq!(n.data(), &[1, 2, 3, 4, 5, 6, 7, 8, 10, 11, 12, 13]);
        // 奇数尺寸取偶数；平面不够长时补黑
        let n = Nv12::from_planes(5, 3, &[1; 4], 4, &[], 4).unwrap();
        assert_eq!((n.width(), n.height()), (4, 2));
        assert_eq!(n.data(), &[1, 1, 1, 1, 16, 16, 16, 16, 128, 128, 128, 128]);
        assert!(Nv12::from_planes(1, 1, &[], 0, &[], 0).is_err());
        // I420：两个色度平面交错
        let n = Nv12::from_i420(4, 2, (&[7; 8], 4), (&[1, 2], 2), (&[3, 4], 2)).unwrap();
        assert_eq!(&n.data()[8..], &[1, 3, 2, 4]);
        let n = Nv12::from_i420(4, 2, (&[7; 8], 4), (&[], 2), (&[], 2)).unwrap();
        assert_eq!(&n.data()[8..], &[128, 128, 128, 128]);
        assert!(Nv12::from_i420(1, 1, (&[], 0), (&[], 0), (&[], 0)).is_err());
    }

    #[test]
    fn pictures_report_size_and_convert_for_checks() {
        let rgb = Picture::Rgb(Arc::new(Screen::new(4, 2, 0x10_2030)));
        assert_eq!(rgb.size(), (4, 2));
        assert_eq!(rgb.to_screen().unwrap(), Screen::new(4, 2, 0x10_2030));
        let white = Picture::Nv12(Arc::new(gray(4, 2, 235)));
        assert_eq!(white.size(), (4, 2));
        assert_eq!(white.to_screen().unwrap(), Screen::new(4, 2, 0x00ff_ffff));
        // 只有描述的画面：有尺寸，没有像素
        let described = Picture::Described(Described {
            version: 3,
            qp: 30,
            key: false,
            size: (8, 6),
        });
        assert_eq!(described.size(), (8, 6));
        assert!(described.to_screen().is_err());
        // 平台解码器已经画到屏幕上的：有尺寸，没有像素
        let shown = Picture::Presented {
            width: 1280,
            height: 720,
        };
        assert_eq!(shown.size(), (1280, 720));
        assert!(shown.to_screen().is_err());
    }

    #[test]
    fn nv12_to_rgb_pads_missing_data_with_black() {
        let mut out = Vec::new();
        nv12_to_rgb(&[], &[], 4, 2, 2, &mut out);
        assert_eq!(out, vec![0; 4]);
    }
}
