//! 硬件能力探测与档位划分。
//!
//! CPU 指令集在运行时检测。编码器基准测试的执行在平台层（M1），
//! 这里只定义基准结果的结构和“结果 → 档位”的判定规则。

use std::fmt::Write as _;

/// CPU 指令集。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuFeatures {
    /// SSE4.1。
    pub sse41: bool,
    /// AVX2。
    pub avx2: bool,
    /// AVX-512 基础指令。
    pub avx512f: bool,
    /// AES 硬件指令（影响 AES-GCM 的速度）。
    pub aes: bool,
    /// ARM NEON。
    pub neon: bool,
}

impl CpuFeatures {
    /// 检测当前 CPU。
    #[must_use]
    pub fn detect() -> Self {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            Self {
                sse41: std::arch::is_x86_feature_detected!("sse4.1"),
                avx2: std::arch::is_x86_feature_detected!("avx2"),
                avx512f: std::arch::is_x86_feature_detected!("avx512f"),
                aes: std::arch::is_x86_feature_detected!("aes"),
                neon: false,
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            Self {
                sse41: false,
                avx2: false,
                avx512f: false,
                aes: std::arch::is_aarch64_feature_detected!("aes"),
                neon: std::arch::is_aarch64_feature_detected!("neon"),
            }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self::default()
        }
    }

    /// 是否有适合做区块哈希和软件编码的宽向量指令。
    #[must_use]
    pub const fn has_wide_simd(&self) -> bool {
        self.avx2 || self.avx512f || self.neon
    }

    /// 一行文字描述。
    #[must_use]
    pub fn describe(&self) -> String {
        let flags = [
            ("sse4.1", self.sse41),
            ("avx2", self.avx2),
            ("avx512f", self.avx512f),
            ("aes", self.aes),
            ("neon", self.neon),
        ];
        let mut out = String::new();
        for (name, on) in flags {
            if on {
                if !out.is_empty() {
                    out.push(' ');
                }
                // 向 String 写入不会失败。
                let _ = write!(out, "{name}");
            }
        }
        if out.is_empty() {
            out.push_str("(无)");
        }
        out
    }
}

/// 一次编码器基准测试的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderBench {
    /// 是否有可用的硬件编码器。
    pub hardware: bool,
    /// 单帧编码耗时的中位数（微秒）。
    pub median_frame_us: u32,
    /// 是否支持参考帧控制（长期参考帧或参考帧失效）。
    pub reference_control: bool,
    /// 是否支持单帧字节上限。
    pub frame_size_cap: bool,
}

/// 硬件档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// 硬件编码快且支持参考帧控制。
    A,
    /// 有硬件编码，但较慢或缺特性。
    B,
    /// 只能软件编码。
    C,
}

/// 档位 A 允许的单帧编码耗时上限（微秒）。
pub const TIER_A_MAX_FRAME_US: u32 = 6_000;

/// 根据基准结果划分档位。
#[must_use]
pub const fn classify_tier(bench: &EncoderBench) -> Tier {
    if !bench.hardware {
        Tier::C
    } else if bench.median_frame_us <= TIER_A_MAX_FRAME_US
        && bench.reference_control
        && bench.frame_size_cap
    {
        Tier::A
    } else {
        Tier::B
    }
}

/// 某个档位的默认编码上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierLimits {
    /// 最大宽度（像素）。
    pub max_width: u32,
    /// 最大高度（像素）。
    pub max_height: u32,
    /// 最高帧率。
    pub max_fps: u32,
    /// 丢包恢复是否可以换参考帧；否则用帧内刷新。
    pub reference_recovery: bool,
}

impl Tier {
    /// 该档位的默认上限。软件编码档再按 CPU 是否有宽向量指令细分。
    #[must_use]
    pub const fn limits(self, cpu: &CpuFeatures) -> TierLimits {
        match self {
            Tier::A => TierLimits {
                max_width: 3840,
                max_height: 2160,
                max_fps: 60,
                reference_recovery: true,
            },
            Tier::B => TierLimits {
                max_width: 1920,
                max_height: 1080,
                max_fps: 60,
                reference_recovery: false,
            },
            Tier::C => {
                if cpu.has_wide_simd() {
                    TierLimits {
                        max_width: 1920,
                        max_height: 1080,
                        max_fps: 30,
                        reference_recovery: false,
                    }
                } else {
                    TierLimits {
                        max_width: 1280,
                        max_height: 720,
                        max_fps: 30,
                        reference_recovery: false,
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_matches_compiler_view() {
        let cpu = CpuFeatures::detect();
        // 检测结果至少不能与编译目标保证的特性矛盾。
        #[cfg(target_arch = "aarch64")]
        assert!(cpu.neon);
        #[cfg(target_arch = "x86_64")]
        assert!(!cpu.neon);
        assert!(!cpu.describe().is_empty());
    }

    #[test]
    fn describe_lists_enabled_flags() {
        assert_eq!(CpuFeatures::default().describe(), "(无)");
        let cpu = CpuFeatures {
            sse41: true,
            avx2: true,
            avx512f: true,
            aes: true,
            neon: true,
        };
        assert_eq!(cpu.describe(), "sse4.1 avx2 avx512f aes neon");
        assert!(cpu.has_wide_simd());
        assert!(!CpuFeatures::default().has_wide_simd());
        assert!(CpuFeatures {
            neon: true,
            ..CpuFeatures::default()
        }
        .has_wide_simd());
        assert!(CpuFeatures {
            avx512f: true,
            ..CpuFeatures::default()
        }
        .has_wide_simd());
    }

    const GOOD: EncoderBench = EncoderBench {
        hardware: true,
        median_frame_us: TIER_A_MAX_FRAME_US,
        reference_control: true,
        frame_size_cap: true,
    };

    #[test]
    fn tier_boundaries() {
        assert_eq!(classify_tier(&GOOD), Tier::A);
        assert_eq!(
            classify_tier(&EncoderBench {
                median_frame_us: TIER_A_MAX_FRAME_US + 1,
                ..GOOD
            }),
            Tier::B
        );
        assert_eq!(
            classify_tier(&EncoderBench {
                reference_control: false,
                ..GOOD
            }),
            Tier::B
        );
        assert_eq!(
            classify_tier(&EncoderBench {
                frame_size_cap: false,
                ..GOOD
            }),
            Tier::B
        );
        assert_eq!(
            classify_tier(&EncoderBench {
                hardware: false,
                ..GOOD
            }),
            Tier::C
        );
        assert!(Tier::A < Tier::B && Tier::B < Tier::C);
    }

    #[test]
    fn tier_limits() {
        let none = CpuFeatures::default();
        let avx = CpuFeatures { avx2: true, ..none };
        assert!(Tier::A.limits(&none).reference_recovery);
        assert_eq!(Tier::A.limits(&none).max_width, 3840);
        assert!(!Tier::B.limits(&none).reference_recovery);
        assert_eq!(Tier::B.limits(&none).max_fps, 60);
        assert_eq!(Tier::C.limits(&avx).max_height, 1080);
        assert_eq!(Tier::C.limits(&none).max_height, 720);
        assert_eq!(Tier::C.limits(&none).max_fps, 30);
    }
}
