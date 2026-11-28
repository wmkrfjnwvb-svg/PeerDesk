//! 编码耗时（帧出发时刻）模型：被控端一帧交给编码器以后，要过一段编码耗时，分片才交给节奏器发出。
//!
//! 默认关（[`EncodeTime::Zero`]，和 A1 以来一样按 0 算：帧严格按帧时钟出发）。为什么要它
//! （`docs/research/Q-redesign/a3-plan.md` 第 11 节）：多路共享窄瓶颈时，帧严格按时钟出发、各路相位固定，
//! 小流的包每帧都落在大流一帧突发的同一个位置，码率控制的常驻队列判断（100 ms 里的最小排队）被锁相，小流被饿死。
//! 真机上每帧的编码耗时不一样，帧出发时刻有抖动；这一层量的就是抖动能不能把锁相打散。
//!
//! 形状：本容器里真 openh264（产品同一个后端，`examples/measure.rs`）编 e2e 的真实合成内容
//! （指针、打字、滚动、拖动四类办公内容，各 2 个种子、各 150 帧，720p，QP 28，不含关键帧，共 1192 帧），
//! 逐帧耗时按 5% 一档取分位，除以中位（4.37 ms）得到下面的千分比表。
//! 尺度：中位按 Mac 被控端用户日志（`docs/research/data/user-logs/2026-11-27*-mac-host-wifi*/per-second.csv`
//! 的 `encode_ms`，每秒平均，p10/50/90 = 4/7/10 ms）取 7 ms；这样缩放后表的 p10/50/90 = 3.7/7.0/10.0 ms，和日志对得上。
//! 局限（写明）：日志是每秒平均，不是逐帧；Mac 上是硬件编码（VideoToolbox），Windows 的 Media Foundation 也是硬件，
//! 形状可能和 openh264 不同。产品日志以后带逐帧的编码耗时分布和出发抖动，到时候换成真机逐帧的表。

use peerdesk_core::path::rng::{stream_seed, Dir};
use peerdesk_core::sim::Rng;

/// 逐帧编码耗时 ÷ 中位（千分比），分位 0%, 5%, …, 100%（21 档，档间线性插值）。来源见模块文档。
pub const SHAPE_PERMILLE: [u32; 21] = [
    423, 504, 526, 594, 679, 742, 754, 784, 945, 970, 1000, 1025, 1041, 1057, 1101, 1176, 1229,
    1306, 1422, 1491, 2063,
];

/// Mac 被控端日志的编码耗时中位（微秒），见模块文档。
pub const MAC_MEDIAN_US: u32 = 7_000;

/// 编码耗时模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EncodeTime {
    /// 按 0 算：帧严格按帧时钟出发（默认，和以前逐字节相同）。
    #[default]
    Zero,
    /// openh264 办公内容的逐帧形状（[`SHAPE_PERMILLE`]），按这个中位（微秒）缩放。
    Openh264Office {
        /// 中位（微秒）。
        median_us: u32,
    },
}

impl EncodeTime {
    /// 世界文件里的写法：`zero` 或 `openh264-office/<中位微秒>`。
    #[must_use]
    pub fn name(self) -> String {
        match self {
            Self::Zero => "zero".to_owned(),
            Self::Openh264Office { median_us } => format!("openh264-office/{median_us}"),
        }
    }

    /// 从世界文件的写法读回；认不出时为 `None`。
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text == "zero" {
            return Some(Self::Zero);
        }
        let median_us = text.strip_prefix("openh264-office/")?.parse().ok()?;
        Some(Self::Openh264Office { median_us })
    }
}

/// 一路的编码耗时抽样器：自己一条随机数流（种子由（场景种子，第几路）得出），开关它不影响别的流。
#[derive(Debug, Clone)]
pub struct Sampler {
    rng: Rng,
    median_us: u32,
}

impl Sampler {
    /// `model` 是 [`EncodeTime::Zero`] 时为 `None`（不抽，不占随机数）。
    #[must_use]
    pub fn new(model: EncodeTime, seed: u64, pair: usize) -> Option<Self> {
        match model {
            EncodeTime::Zero => None,
            EncodeTime::Openh264Office { median_us } => Some(Self {
                rng: Rng::new(stream_seed(
                    seed,
                    Dir::Forward,
                    &format!("encode-time/{pair}"),
                )),
                median_us,
            }),
        }
    }

    /// 抽一帧的编码耗时（微秒）：均匀抽分位，在表里线性插值，乘中位。只用整数，跨平台逐位相同。
    pub fn next_us(&mut self) -> u64 {
        // 分位按 1/1_000_000 的精度抽：落在第 i 档和第 i+1 档之间（每档 50_000）
        let u = self.rng.below(1_000_000);
        let i = usize::try_from(u / 50_000).unwrap_or(0);
        let frac = u % 50_000;
        let lo = SHAPE_PERMILLE.get(i).copied().unwrap_or(0);
        let hi = SHAPE_PERMILLE.get(i + 1).copied().unwrap_or(lo);
        let permille = (u64::from(lo) * (50_000 - frac) + u64::from(hi) * frac) / 50_000;
        permille * u64::from(self.median_us) / 1000
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn names_round_trip() {
        for m in [
            EncodeTime::Zero,
            EncodeTime::Openh264Office { median_us: 7_000 },
        ] {
            assert_eq!(EncodeTime::parse(&m.name()), Some(m));
        }
        assert_eq!(EncodeTime::parse("openh264-office/x"), None);
        assert_eq!(EncodeTime::parse("lognormal"), None);
        assert_eq!(EncodeTime::default(), EncodeTime::Zero);
    }

    #[test]
    fn off_draws_nothing_and_quantiles_match_the_calibration() {
        assert!(Sampler::new(EncodeTime::Zero, 1, 0).is_none());
        let m = EncodeTime::Openh264Office {
            median_us: MAC_MEDIAN_US,
        };
        let mut s = Sampler::new(m, 1, 0).unwrap();
        let mut v: Vec<u64> = (0..20_000).map(|_| s.next_us()).collect();
        v.sort_unstable();
        let q = |p: usize| v[p * (v.len() - 1) / 100];
        // 表缩放到 7 ms 以后 p10/50/90 = 3.7/7.0/10.0 ms（模块文档），抽样误差内
        assert!((3_500..3_900).contains(&q(10)), "p10 {}", q(10));
        assert!((6_800..7_200).contains(&q(50)), "p50 {}", q(50));
        assert!((9_700..10_300).contains(&q(90)), "p90 {}", q(90));
        assert!(v[0] >= 423 * 7 && *v.last().unwrap() <= 2_063 * 7);
        // 确定：同样的（种子，路）同样的序列；不同的路不同
        let a: Vec<u64> = {
            let mut s = Sampler::new(m, 1, 0).unwrap();
            (0..8).map(|_| s.next_us()).collect()
        };
        let mut s0 = Sampler::new(m, 1, 0).unwrap();
        let mut s1 = Sampler::new(m, 1, 1).unwrap();
        assert_eq!(a, (0..8).map(|_| s0.next_us()).collect::<Vec<_>>());
        assert_ne!(a, (0..8).map(|_| s1.next_us()).collect::<Vec<_>>());
    }
}
