//! 画面内容的两个度量：**变化面积**和**细节**。
//!
//! 它们是产品里决定“这一帧大约多大”的依据，也是假编码后端（[`crate::fake`]）和 M2 测量工具要用的同一份定义。
//! 单独提成函数，是为了让测量、仿真和产品用同一个算法，不各写各的（见 `docs/research/Q-redesign/m2-m3-protocol.md` 第 0 节）。
//!
//! - [`AreaSampler`]：编码器用。从 RGB 画面里每隔 [`SAMPLE_STEP`] 个像素抽一个，和上一次抽的比，给出
//!   按点数和按细节加权的变化比例。
//! - [`detail_permille`]、[`sample_luma`]、[`luma_changed_permille`]：假后端用。从亮度平面抽样，算细节和变化比例。

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
            Area {
                by_points: count.saturating_mul(1000) / points,
                by_detail: weight.saturating_mul(1000) / total.max(1),
            }
        } else {
            Area::FULL
        };
        self.samples = now;
        self.sampled = size;
        changed
    }
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

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
}
