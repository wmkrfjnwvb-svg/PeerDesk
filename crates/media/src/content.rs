//! 画面内容的两个度量：**变化面积**和**细节**。
//!
//! 它们是产品里决定“这一帧大约多大”的依据，也是假编码后端（[`crate::fake`]）和 M2 测量工具要用的同一份定义。
//! 单独提成函数，是为了让测量、仿真和产品用同一个算法，不各写各的（见 `docs/research/Q-redesign/m2-m3-protocol.md` 第 0 节）。
//!
//! - [`AreaSampler`]：编码器用。从 RGB 画面里每隔 [`SAMPLE_STEP`] 个像素抽一个，和上一次抽的比，给出
//!   按点数和按细节加权的变化比例。
//! - [`detail_permille`]、[`sample_luma`]、[`luma_changed_permille`]：假后端用。从亮度平面抽样，算细节和变化比例。

use peerdesk_core::error::Error;
use peerdesk_core::frame::{ContentDesc, LumaDesc};
use peerdesk_core::scale::encode_size;
use peerdesk_core::screen::Screen;

/// 估计画面变了多少时每隔多少像素抽一个（横竖都是）：720p 约 3600 个，几乎不花时间。
pub const SAMPLE_STEP: usize = 16;
/// 抽样点细节量的底数：平坦的地方变了也要编一点。也是“按细节”和“按点数”之间的折中：底数越大越接近只数点数。
/// 2026-11 微软轨迹 12 条 × 3 个种子（冻结 / 最长等待平均 / 停下后最差 PSNR）：只数点数 145 / 642 ms / 27.3 dB；
/// 底数 8 77 / 366 ms / 25.8 dB；24 95 / 392 ms / 26.8 dB；64 112 / 447 ms / 26.7 dB。取 24：清晰度最后让步。
pub const DETAIL_FLOOR: u32 = 24;

/// 近似亮度（0–255）：(2R + 5G + B) / 8。
#[must_use]
pub const fn luma(p: u32) -> u32 {
    ((p >> 16 & 0xff) * 2 + (p >> 8 & 0xff) * 5 + (p & 0xff)) / 8
}

/// 画面和上一次比变了多少（千分比，0 到 1000）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    /// 按点数：变了的抽样点占全部抽样点的比例。用来判断是不是整屏变化（滚动文字时细节集中在文字上，按细节加权常常过半，
    /// 但有运动补偿，不该当成整屏变化重编成关键帧：8 Mbps、12% 丢包时关键帧更难送到，实测卡顿变多）。
    pub by_points: u64,
    /// 按细节加权：变了的点的细节量占总细节量的比例。用来估计这一帧多大。
    pub by_detail: u64,
}

impl Area {
    /// 整屏都是新的（第一次、尺寸变了）。
    pub const FULL: Self = Self {
        by_points: 1000,
        by_detail: 1000,
    };
}

/// 抽样估计画面变了多少。记着上一次抽的点，每次 [`Self::observe`] 和它比，再换成这一次的。
#[derive(Debug, Clone, Default)]
pub struct AreaSampler {
    /// 上一次编的画面的抽样（每 [`SAMPLE_STEP`] 像素取一个）。
    samples: Vec<u32>,
    sampled: (u16, u16),
}

impl AreaSampler {
    /// 创建（还没有上一次的抽样：第一次 [`Self::observe`] 算整屏都变了）。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            samples: Vec::new(),
            sampled: (0, 0),
        }
    }

    /// 画面和上一次 `observe` 的相比变了多少；尺寸变了算全变。顺便记下这一次的抽样。
    ///
    /// 按细节加权的变化比例：编码出来的大小大致按细节分布（关键帧的大小估计也是整幅画面的细节），
    /// 只数变了几个点会把“文字密集的弹窗盖在白底上”估得很小（弹窗占 36%，变了的点只有 19%，按点数估计
    /// 比实际小 5 倍以上，2026-11 微软轨迹里切换窗口的帧在 1 Mbps 上编出 89–125 KB）。
    pub fn observe(&mut self, screen: &Screen) -> Area {
        let sample = AreaSample::of(screen);
        self.observe_sample(sample.size, &sample.pixels, &sample.detail)
    }

    /// 和 [`Self::observe`] 一样，交来的是已经抽好的点（[`AreaSample::of`] 的结果）。全模型仿真没有像素，
    /// 从内容描述里拿抽样走这里；和拿同一幅画面的像素走 `observe` 结果相同。
    pub fn observe_sample(&mut self, size: (u16, u16), pixels: &[u32], detail: &[u32]) -> Area {
        let changed = if size == self.sampled && pixels.len() == self.samples.len() {
            let total: u64 = detail.iter().map(|d| u64::from(*d)).sum();
            let (count, weight) = pixels
                .iter()
                .zip(&self.samples)
                .zip(detail)
                .filter(|((a, b), _)| a != b)
                .fold((0u64, 0u64), |(c, w), (_, d)| {
                    (c.saturating_add(1), w.saturating_add(u64::from(*d)))
                });
            let points = u64::try_from(pixels.len()).unwrap_or(u64::MAX).max(1);
            Area {
                by_points: count.saturating_mul(1000) / points,
                by_detail: weight.saturating_mul(1000) / total.max(1),
            }
        } else {
            Area::FULL
        };
        self.samples.clear();
        self.samples.extend_from_slice(pixels);
        self.sampled = size;
        changed
    }
}

/// 一幅画面给 [`AreaSampler`] 用的抽样：每隔 [`SAMPLE_STEP`] 个像素取一个点，记下像素值和它的细节量。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaSample {
    /// 画面的尺寸。
    pub size: (u16, u16),
    /// 抽样点的像素值。
    pub pixels: Vec<u32>,
    /// 每个抽样点的细节量（和右边、下边像素的亮度差，加一个底数 [`DETAIL_FLOOR`]）。
    pub detail: Vec<u32>,
}

impl AreaSample {
    /// 从画面抽样。
    #[must_use]
    pub fn of(screen: &Screen) -> Self {
        let width = usize::from(screen.width());
        let rows: Vec<&[u32]> = screen.pixels().chunks(width.max(1)).collect();
        let mut pixels = Vec::new();
        let mut detail = Vec::new();
        for (y, row) in rows.iter().enumerate().step_by(SAMPLE_STEP) {
            let below = rows.get(y.saturating_add(1));
            for (x, p) in row.iter().enumerate().step_by(SAMPLE_STEP) {
                let l = luma(*p);
                let right = row.get(x.saturating_add(1)).map_or(l, |q| luma(*q));
                let down = below.and_then(|r| r.get(x)).map_or(l, |q| luma(*q));
                pixels.push(*p);
                detail.push(l.abs_diff(right) + l.abs_diff(down) + DETAIL_FLOOR);
            }
        }
        Self {
            size: (screen.width(), screen.height()),
            pixels,
            detail,
        }
    }
}

/// 把一幅画面变成内容描述（[`ContentDesc`]）：编码器和假后端从像素里会算出来的那几样抽样，一次算好。
/// `scales` 是可能用到的编码比例（千分比，例如原尺寸 1000 和降分辨率时的 500），每个比例一份亮度抽样；
/// 亮度平面的算法和编码器转换画面时完全一样（缩到编码尺寸、补成偶数、RGB 转 I420），所以拿描述去编和拿像素去编，
/// 模型后端看到的数相同。
///
/// # Errors
/// 缩放失败（尺寸为 0）。
pub fn describe(screen: &Screen, version: u64, scales: &[u16]) -> Result<ContentDesc, Error> {
    let area = AreaSample::of(screen);
    let native = (screen.width(), screen.height());
    let mut luma: Vec<LumaDesc> = Vec::new();
    let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
    for permille in scales {
        let size = encode_size(native, *permille, None);
        let shrunk;
        let source = if size == native {
            screen
        } else {
            shrunk = screen.downscale(size.0, size.1)?;
            &shrunk
        };
        let (w, h, pixels) = crate::even_pixels(source);
        let coded = (
            u16::try_from(w).unwrap_or(u16::MAX),
            u16::try_from(h).unwrap_or(u16::MAX),
        );
        if luma.iter().any(|l| l.size == coded) {
            continue;
        }
        crate::color::rgb_to_i420(&pixels, w, &mut y, &mut u, &mut v);
        luma.push(LumaDesc {
            size: coded,
            sample: sample_luma(&y).into(),
            detail_permille: detail_permille(&y),
            mb_edges: mb_edges(&y, coded).into(),
        });
    }
    Ok(ContentDesc {
        version,
        size: native,
        area_pixels: area.pixels.into(),
        area_detail: area.detail.into(),
        luma,
        // 区域不是从像素里算出来的：画面的来源（合成桌面、采集接口）知道哪里变了，由它填
        dirty: Vec::new(),
        moves: Vec::new(),
    })
}

/// 亮度平面的抽样步长（字节）：每隔这么多个字节取一个点（取质数，免得和行宽对齐）。
pub const LUMA_SAMPLE_STEP: usize = 13;
/// 相邻两点亮度差超过这个值算“有细节”。
pub const DETAIL_EDGE: u8 = 24;

/// `part` 占 `whole` 的千分之几（不超过 1000）。
#[must_use]
pub fn permille(part: usize, whole: usize) -> u32 {
    u32::try_from(part.saturating_mul(1000) / whole.max(1))
        .unwrap_or(1000)
        .min(1000)
}

/// 从亮度平面抽样（每隔 [`LUMA_SAMPLE_STEP`] 个字节一个点）。
#[must_use]
pub fn sample_luma(luma: &[u8]) -> Vec<u8> {
    luma.iter().copied().step_by(LUMA_SAMPLE_STEP).collect()
}

/// 这一次的抽样和上一次的比，变了多少（千分之几）；没有上一次、或点数对不上（尺寸变了）算整屏都变了（1000）。
#[must_use]
pub fn luma_changed_permille(sample: &[u8], prev: Option<&Vec<u8>>) -> u32 {
    match prev {
        Some(p) if p.len() == sample.len() => {
            let diff = sample.iter().zip(p).filter(|(a, b)| a != b).count();
            permille(diff, sample.len().max(1))
        }
        _ => 1000,
    }
}

/// 亮度平面有多少细节（千分之几）：抽样点里和后一个字节的亮度差超过 [`DETAIL_EDGE`] 的占多少。
#[must_use]
pub fn detail_permille(luma: &[u8]) -> u32 {
    let n = luma.len().div_ceil(LUMA_SAMPLE_STEP).max(1);
    let edges = luma
        .iter()
        .zip(luma.iter().skip(1))
        .step_by(LUMA_SAMPLE_STEP)
        .filter(|(a, b)| a.abs_diff(**b) > DETAIL_EDGE)
        .count();
    permille(edges, n)
}

/// 宏块边长（像素）。H.264 的宏块是 16×16；宏块画质表、脏区域的细节都按它汇总。
pub const MB_SIZE: usize = 16;

/// 编码尺寸为 `size` 时宏块的列数和行数（边上不满 16 的也算一个）。
#[must_use]
pub fn mb_dims(size: (u16, u16)) -> (usize, usize) {
    (
        usize::from(size.0).div_ceil(MB_SIZE),
        usize::from(size.1).div_ceil(MB_SIZE),
    )
}

/// 抽样点（亮度平面里每隔 [`LUMA_SAMPLE_STEP`] 个字节的那些，和 [`detail_permille`] 的点一样）所在的宏块下标。
fn sample_mbs(len: usize, size: (u16, u16)) -> impl Iterator<Item = usize> {
    let (cols, _) = mb_dims(size);
    let w = usize::from(size.0).max(1);
    (0..len.div_ceil(LUMA_SAMPLE_STEP)).map(move |k| {
        let i = k * LUMA_SAMPLE_STEP;
        i / w / MB_SIZE * cols + i % w / MB_SIZE
    })
}

/// 每个宏块里有几个抽样点（只和编码尺寸有关）。
#[must_use]
pub fn mb_points(size: (u16, u16)) -> Vec<u16> {
    let (cols, rows) = mb_dims(size);
    let mut out = vec![0u16; cols * rows];
    for mb in sample_mbs(usize::from(size.0) * usize::from(size.1), size) {
        if let Some(n) = out.get_mut(mb) {
            *n = n.saturating_add(1);
        }
    }
    out
}

/// 每个宏块里有细节的抽样点数（和 [`detail_permille`] 同一个判断：该点和后一个字节的亮度差超过 [`DETAIL_EDGE`]）。
/// 所有宏块加起来除以抽样点总数，就是整幅的 [`detail_permille`]。
#[must_use]
pub fn mb_edges(luma: &[u8], size: (u16, u16)) -> Vec<u16> {
    let (cols, rows) = mb_dims(size);
    let mut out = vec![0u16; cols * rows];
    let pairs = luma
        .iter()
        .zip(luma.iter().skip(1))
        .step_by(LUMA_SAMPLE_STEP);
    for (mb, (a, b)) in sample_mbs(luma.len(), size).zip(pairs) {
        if a.abs_diff(*b) > DETAIL_EDGE {
            if let Some(n) = out.get_mut(mb) {
                *n = n.saturating_add(1);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    /// 先抽样再比较，和直接拿画面比是同一回事；内容描述里的抽样就是这两步的中间结果，
    /// 亮度抽样和编码器转换出来的亮度平面上取的一样，每种编码比例一份。
    #[test]
    fn a_description_holds_the_same_samples_the_pixels_would_give() {
        let mut a = Screen::new(320, 240, 0x0020_2020);
        a.fill(10, 10, 120, 60, 0x00f0_f0f0);
        let mut b = a.clone();
        b.fill(200, 100, 64, 64, 0x0000_80ff);
        let (mut by_screen, mut by_sample) = (AreaSampler::new(), AreaSampler::new());
        for s in [&a, &b, &b, &a] {
            let sample = AreaSample::of(s);
            assert_eq!(
                by_screen.observe(s),
                by_sample.observe_sample(sample.size, &sample.pixels, &sample.detail)
            );
        }
        let d = describe(&b, 3, &[1000, 500, 1000]).unwrap();
        assert_eq!((d.version, d.size), (3, (320, 240)));
        let sample = AreaSample::of(&b);
        assert_eq!(
            (&*d.area_pixels, &*d.area_detail),
            (&*sample.pixels, &*sample.detail)
        );
        assert_eq!(d.luma.len(), 2, "同一个编码尺寸只留一份");
        // 原尺寸：和编码器的转换（补偶数、RGB 转 I420）得到的亮度平面一致
        let (w, _, pixels) = crate::even_pixels(&b);
        let (mut y, mut u, mut v) = (Vec::new(), Vec::new(), Vec::new());
        crate::color::rgb_to_i420(&pixels, w, &mut y, &mut u, &mut v);
        let full = d.luma_at((320, 240)).unwrap();
        assert_eq!(&*full.sample, &*sample_luma(&y));
        assert_eq!(full.detail_permille, detail_permille(&y));
        let half = d.luma_at((160, 120)).unwrap();
        assert_eq!(
            half.sample.len(),
            (160usize * 120).div_ceil(LUMA_SAMPLE_STEP)
        );
        assert!(d.luma_at((64, 64)).is_none());
    }

    fn flat(w: u16, h: u16, rgb: u32) -> Screen {
        Screen::new(w, h, rgb)
    }

    #[test]
    fn first_and_resized_frames_count_as_all_changed() {
        let mut s = AreaSampler::new();
        assert_eq!(s.observe(&flat(64, 64, 0x20_2020)), Area::FULL);
        assert_eq!(s.observe(&flat(64, 64, 0x20_2020)).by_points, 0);
        assert_eq!(s.observe(&flat(32, 32, 0x20_2020)), Area::FULL);
    }

    #[test]
    fn weighted_area_follows_detail_not_just_points() {
        let mut s = AreaSampler::new();
        let _ = s.observe(&flat(128, 128, 0xff_ffff));
        // 左半边换成条纹：点数变了一半，细节加权的比例比点数大（条纹处细节多）
        let mut striped = flat(128, 128, 0xff_ffff);
        for x in (0..64u16).step_by(2) {
            striped.fill(x, 0, 1, 128, 0);
        }
        let a = s.observe(&striped);
        assert!(a.by_points > 0 && a.by_points < 1000, "{a:?}");
        assert!(a.by_detail >= a.by_points, "{a:?}");
    }

    #[test]
    fn luma_measures_match_their_definitions() {
        // 全平：没有细节
        let flat_plane = vec![100u8; 1000];
        assert_eq!(detail_permille(&flat_plane), 0);
        // 相邻点亮度差很大：抽到的点都算细节
        let edgy: Vec<u8> = (0..1000)
            .map(|i| if i % 2 == 0 { 0 } else { 255 })
            .collect();
        assert!(detail_permille(&edgy) > 900);
        // 没有上一次、点数对不上：整屏都变
        let sample = sample_luma(&flat_plane);
        assert_eq!(luma_changed_permille(&sample, None), 1000);
        assert_eq!(luma_changed_permille(&sample, Some(&vec![0; 3])), 1000);
        assert_eq!(luma_changed_permille(&sample, Some(&sample)), 0);
        let mut other = sample.clone();
        other[0] ^= 1;
        assert!(luma_changed_permille(&sample, Some(&other)) > 0);
        assert_eq!(permille(5, 0), 1000);
    }
    /// 宏块汇总和整幅的定义对得上：每个宏块的抽样点数加起来是抽样点总数，有细节的点数加起来除以总数就是 `detail_permille`；
    /// 细节落在哪个宏块，就记在哪个宏块里。
    #[test]
    fn per_macroblock_detail_adds_up_to_the_whole_plane() {
        let size = (100u16, 50u16);
        assert_eq!(mb_dims(size), (7, 4));
        let (w, h) = (usize::from(size.0), usize::from(size.1));
        let pts = mb_points(size);
        assert_eq!(pts.len(), 28);
        let n = (w * h).div_ceil(LUMA_SAMPLE_STEP);
        assert_eq!(pts.iter().map(|p| usize::from(*p)).sum::<usize>(), n);
        // 一块 20×20 的亮块放在 (32, 16)：只有它边上的点有细节
        let mut luma = vec![10u8; w * h];
        for y in 16..36 {
            for x in 32..52 {
                luma[y * w + x] = 200;
            }
        }
        let edges = mb_edges(&luma, size);
        let total: usize = edges.iter().map(|e| usize::from(*e)).sum();
        assert!(total > 0);
        assert_eq!(
            u32::try_from(total * 1000 / n).unwrap(),
            detail_permille(&luma)
        );
        // 抽样点比的是它和右边一个字节：左边缘的点在块外一格（宏块列 1），右边缘的点在块里（列 3）；
        // 块占宏块行 1..=2。其余宏块为 0
        for (i, e) in edges.iter().enumerate() {
            let (col, row) = (i % 7, i / 7);
            if !(1..=3).contains(&col) || !(1..=2).contains(&row) {
                assert_eq!(*e, 0, "宏块 ({col}, {row}) 不该有细节");
            }
            assert!(e <= &pts[i]);
        }
        // 平面比尺寸短（后面没有了）：不越界，多出来的宏块没有点
        assert_eq!(mb_edges(&luma[..40], size).len(), 28);
        assert_eq!(mb_points((16, 16)), vec![20]);
    }
}
