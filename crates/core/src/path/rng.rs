//! 每层一条随机数流，以及不依赖浮点库的指数分布抽样。
//!
//! 流的种子由（世界种子，方向，层名）的 blake3 哈希取前 8 个字节得到，交给 `sim.rs` 里的 SplitMix64。
//! 开关一层、改一层的参数，只会改变这一层自己的流；别的层的流不受影响。

use crate::sim::Rng;

/// 方向，哈希里用一个字节表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Dir {
    /// 正向：节点 0 到节点 1（媒体）。
    Forward,
    /// 反向：节点 1 到节点 0（反馈）。
    Reverse,
}

impl Dir {
    /// 两个方向，正向在前。
    pub const BOTH: [Dir; 2] = [Dir::Forward, Dir::Reverse];

    /// 哈希里用的编号，也是 [`crate::path::Path`] 内部数组的下标。
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Dir::Forward => 0,
            Dir::Reverse => 1,
        }
    }

    /// 由（发送节点，接收节点）得到方向；不是 0↔1 时为 `None`。
    #[must_use]
    pub const fn from_nodes(from: usize, to: usize) -> Option<Dir> {
        match (from, to) {
            (0, 1) => Some(Dir::Forward),
            (1, 0) => Some(Dir::Reverse),
            _ => None,
        }
    }

    /// 另一个方向。
    #[must_use]
    pub const fn other(self) -> Dir {
        match self {
            Dir::Forward => Dir::Reverse,
            Dir::Reverse => Dir::Forward,
        }
    }
}

/// 层名：用作随机数流的标识。名字一旦定下就不能改，否则所有布局 2 的结果都会变。
pub mod layer {
    /// 瓶颈的泊松放行机会。
    pub const BOTTLENECK: &str = "bottleneck";
    /// 丢包层。
    pub const LOSS: &str = "loss";
    /// 传播延迟（周期性阶跃的取值）。
    pub const DELAY: &str = "delay";
    /// 均匀抖动。
    pub const JITTER: &str = "jitter";
    /// 延迟尖峰。
    pub const SPIKES: &str = "spikes";
    /// 省电唤醒。
    pub const DOZE: &str = "doze";
    /// 接收端忙。
    pub const BUSY: &str = "busy";
}

/// 流的种子：blake3（世界种子 8 字节小端 ‖ 方向 1 字节 ‖ 层名）的前 8 个字节（小端）。
#[must_use]
pub fn stream_seed(world_seed: u64, dir: Dir, layer: &str) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&world_seed.to_le_bytes());
    hasher.update(&[u8::try_from(dir.index()).unwrap_or(0)]);
    hasher.update(layer.as_bytes());
    let hash = hasher.finalize();
    let mut first = [0u8; 8];
    first.copy_from_slice(hash.as_bytes().get(..8).unwrap_or(&[0; 8]));
    u64::from_le_bytes(first)
}

/// 一层自己的随机数流。
#[derive(Debug, Clone)]
pub struct LayerRng {
    rng: Rng,
    /// 已经抽了多少个数。
    drawn: u64,
    /// 测试里记下每次抽到的数，用来证明别的层的流没被动过。
    #[cfg(test)]
    pub(crate) log: Vec<u64>,
}

impl LayerRng {
    /// 由（世界种子，方向，层名）创建。
    #[must_use]
    pub fn new(world_seed: u64, dir: Dir, layer: &str) -> Self {
        Self {
            rng: Rng::new(stream_seed(world_seed, dir, layer)),
            drawn: 0,
            #[cfg(test)]
            log: Vec::new(),
        }
    }

    /// 下一个 64 位随机数。
    pub fn next_u64(&mut self) -> u64 {
        let value = self.rng.next_u64();
        self.drawn = self.drawn.saturating_add(1);
        #[cfg(test)]
        self.log.push(value);
        value
    }

    /// `[0, n)` 内的随机数；`n` 为 0 时返回 0（也抽一个数，保证每个包抽的个数固定）。
    pub fn below(&mut self, n: u64) -> u64 {
        let value = self.next_u64();
        if n == 0 {
            0
        } else {
            value % n
        }
    }

    /// 已经抽了多少个数。
    #[must_use]
    pub const fn drawn(&self) -> u64 {
        self.drawn
    }

    /// 均值为 `mean_ns` 纳秒的指数分布随机数（纳秒），至少 1。
    pub fn exp_ns(&mut self, mean_ns: u64) -> u64 {
        let v = (self.next_u64() >> 32).saturating_add(1);
        let x = neg_ln_q24(v);
        let scaled = (u128::from(mean_ns) * u128::from(x)) >> 24;
        u64::try_from(scaled).unwrap_or(u64::MAX).max(1)
    }
}

/// ln 2 的 Q24 定点数。
const LN2_Q24: u128 = 11_629_080;

/// `-ln(v / 2^32)` 的 Q24 定点数，`v` 在 `1..=2^32`。
///
/// 为什么不用 `f64::ln`：浮点库在不同平台上最后一位可能不一样，仿真要跨平台逐字节相同（指纹）。
/// 做法：先用整数求 log2（整数部分取最高位，小数部分用逐次平方的办法一位一位取），再乘 ln 2。
#[must_use]
pub fn neg_ln_q24(v: u64) -> u64 {
    let v = v.clamp(1, 1 << 32);
    let n = 63 - v.leading_zeros(); // floor(log2 v)，0..=32
                                    // 尾数 m 取 [1, 2)，Q32 定点
    let mut m: u64 = if n >= 32 {
        v >> (n - 32)
    } else {
        v << (32 - n)
    };
    let mut frac: u64 = 0;
    for _ in 0..24 {
        let sq = (u128::from(m) * u128::from(m)) >> 32;
        m = u64::try_from(sq).unwrap_or(u64::MAX);
        frac <<= 1;
        if m >= 1 << 33 {
            frac |= 1;
            m >>= 1;
        }
    }
    let log2_q24 = (u64::from(n) << 24) | frac;
    let neg_log2 = (32u64 << 24).saturating_sub(log2_q24);
    u64::try_from((u128::from(neg_log2) * LN2_Q24) >> 24).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_helpers() {
        assert_eq!(Dir::Forward.index(), 0);
        assert_eq!(Dir::Reverse.index(), 1);
        assert_eq!(Dir::Forward.other(), Dir::Reverse);
        assert_eq!(Dir::Reverse.other(), Dir::Forward);
        assert_eq!(Dir::BOTH, [Dir::Forward, Dir::Reverse]);
    }

    /// 种子随（世界种子、方向、层名）变化，且和固定值一致：固定值变了就说明布局 2 的随机数布局被改了。
    #[test]
    fn stream_seed_depends_on_all_three_inputs_and_is_pinned() {
        let base = stream_seed(7, Dir::Forward, layer::LOSS);
        assert_eq!(base, stream_seed(7, Dir::Forward, layer::LOSS));
        assert_ne!(base, stream_seed(8, Dir::Forward, layer::LOSS));
        assert_ne!(base, stream_seed(7, Dir::Reverse, layer::LOSS));
        assert_ne!(base, stream_seed(7, Dir::Forward, layer::JITTER));
        // 手算对照：blake3(7 的 8 字节小端 ‖ 0x00 ‖ "loss") 的前 8 字节
        let mut input = 7u64.to_le_bytes().to_vec();
        input.push(0);
        input.extend_from_slice(b"loss");
        let h = blake3::hash(&input);
        let mut first = [0u8; 8];
        first.copy_from_slice(&h.as_bytes()[..8]);
        assert_eq!(base, u64::from_le_bytes(first));
        assert_eq!(base, PINNED_LOSS_SEED_7, "随机数布局 2 的种子推导被改了");
    }

    const PINNED_LOSS_SEED_7: u64 = 1_523_254_759_069_973_538;

    #[test]
    fn layer_rng_counts_and_logs_draws() {
        let mut a = LayerRng::new(1, Dir::Forward, layer::JITTER);
        let mut b = LayerRng::new(1, Dir::Forward, layer::JITTER);
        let xs: Vec<u64> = (0..5).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..5).map(|_| b.next_u64()).collect();
        assert_eq!(xs, ys);
        assert_eq!(a.drawn(), 5);
        assert_eq!(a.log, xs);
        // below(0) 也抽一个数
        assert_eq!(a.below(0), 0);
        assert_eq!(a.drawn(), 6);
        assert!((0..100).all(|_| a.below(7) < 7));
    }

    /// 定点 ln 和浮点 ln 对照：绝对误差在 2^-20 以内。
    #[test]
    fn neg_ln_matches_the_float_logarithm() {
        let two32 = 4_294_967_296.0_f64;
        for v in [
            1u64,
            2,
            3,
            100,
            65_535,
            1 << 20,
            (1 << 31) - 1,
            1 << 31,
            3 << 30,
            (1 << 32) - 1,
            1 << 32,
        ] {
            let want = -(v as f64 / two32).ln();
            let got = neg_ln_q24(v) as f64 / 16_777_216.0;
            assert!((got - want).abs() < 1e-6, "v={v} got={got} want={want}");
        }
        assert_eq!(neg_ln_q24(1 << 32), 0);
        // 越界的输入被夹住，不会 panic
        assert_eq!(neg_ln_q24(0), neg_ln_q24(1));
        assert_eq!(neg_ln_q24(u64::MAX), 0);
    }

    /// 指数分布：均值等于参数，变异系数约为 1。
    #[test]
    fn exponential_has_the_requested_mean_and_unit_cv() {
        let mut rng = LayerRng::new(3, Dir::Forward, layer::BOTTLENECK);
        let n = 200_000u32;
        let xs: Vec<f64> = (0..n).map(|_| rng.exp_ns(1_000_000) as f64).collect();
        let mean = xs.iter().sum::<f64>() / f64::from(n);
        let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / f64::from(n);
        let cv = var.sqrt() / mean;
        // 均值的标准误 = mean / sqrt(n)
        assert!(
            (mean - 1e6).abs() < 2.0 * 1e6 / f64::from(n).sqrt(),
            "mean={mean}"
        );
        assert!((cv - 1.0).abs() < 0.02, "cv={cv}");
        assert_eq!(
            LayerRng::new(3, Dir::Forward, layer::BOTTLENECK).exp_ns(0),
            1
        );
    }
}
