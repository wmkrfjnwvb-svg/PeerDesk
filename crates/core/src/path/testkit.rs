//! 测试用的统计小工具：12 个种子，按“均值 ± 两倍标准误”判断。
//!
//! 一个补充：用 12 个样本估计标准误，本身就不稳（样本均值与公式值之差除以估计出的标准误服从 t 分布，
//! 自由度 11，`±2` 倍标准误之外的概率约 7%，不是 5%），一个测试里有十几个判断，每次总有判断因为抽样波动落在外面
//! （2026-11-28 做 A2-1 时，泊松放行的变异系数和两态丢包的长期丢包率就这样落到 2 倍以外，换 240 个种子复核，均值都对）。
//! 所以 12 个种子判不过时，不调阈值，也不换种子：加 120 个种子（共 132 个）再判一次，仍在两倍标准误之外才算失败，
//! 真有偏差的话 132 个种子的标准误缩到约 1/3，偏差藏不住。两次的数都会打出来。

use std::ops::Range;

/// 第一轮的 12 个种子。
pub const SEEDS: Range<u64> = 0..12;
/// 复核时多加的种子。
const MORE: Range<u64> = 12..132;

/// 一组样本（每个种子一个数）。
#[derive(Debug, Default, Clone)]
pub struct Samples(Vec<f64>);

impl Samples {
    pub fn push(&mut self, x: f64) {
        self.0.push(x);
    }

    pub fn mean(&self) -> f64 {
        self.0.iter().sum::<f64>() / self.0.len().max(1) as f64
    }

    /// 均值的标准误 = 样本标准差 / sqrt(n)。
    pub fn se(&self) -> f64 {
        let n = self.0.len() as f64;
        if self.0.len() < 2 {
            return 0.0;
        }
        let m = self.mean();
        let var = self.0.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
        (var / n).sqrt()
    }
}

/// 每个种子测 K 个量，每个量的均值要在公式值（`expected`）± 两倍标准误以内。
pub fn check<const K: usize>(
    what: [&str; K],
    expected: [f64; K],
    mut f: impl FnMut(u64) -> [f64; K],
) {
    let mut s: [Samples; K] = std::array::from_fn(|_| Samples::default());
    for seed in SEEDS {
        for (i, x) in f(seed).into_iter().enumerate() {
            s[i].push(x);
        }
    }
    let mut again = false;
    for i in 0..K {
        let (m, se) = (s[i].mean(), s[i].se());
        let z = (m - expected[i]) / se.max(1e-300);
        println!(
            "[统计] {}: 12 个种子测得 {m:.6} ± {se:.6}，公式/参数 {:.6}，差 {z:.2} 个标准误",
            what[i], expected[i]
        );
        again |= z.abs() > 2.0;
    }
    if !again {
        return;
    }
    for seed in MORE {
        for (i, x) in f(seed).into_iter().enumerate() {
            s[i].push(x);
        }
    }
    for i in 0..K {
        let (m, se) = (s[i].mean(), s[i].se());
        let z = (m - expected[i]) / se.max(1e-300);
        println!(
            "[复核] {}: 132 个种子测得 {m:.6} ± {se:.6}，公式/参数 {:.6}，差 {z:.2} 个标准误",
            what[i], expected[i]
        );
        assert!(
            z.abs() <= 2.0,
            "{}: 132 个种子测得 {m} ± {se}，公式值 {} 不在两倍标准误以内",
            what[i],
            expected[i]
        );
    }
}

/// 两个模型（旧的、新的）每个种子各测 K 个量，每个量的均值之差要在两倍合并标准误以内。
pub fn check_pair<const K: usize>(
    what: [&str; K],
    a: impl Fn(u64) -> [f64; K],
    b: impl Fn(u64) -> [f64; K],
) {
    let mut sa: [Samples; K] = std::array::from_fn(|_| Samples::default());
    let mut sb: [Samples; K] = std::array::from_fn(|_| Samples::default());
    let feed = |seeds: Range<u64>, sa: &mut [Samples; K], sb: &mut [Samples; K]| {
        for seed in seeds {
            for (i, x) in a(seed).into_iter().enumerate() {
                sa[i].push(x);
            }
            for (i, x) in b(seed).into_iter().enumerate() {
                sb[i].push(x);
            }
        }
    };
    feed(SEEDS, &mut sa, &mut sb);
    let z = |sa: &Samples, sb: &Samples| {
        let se = (sa.se().powi(2) + sb.se().powi(2)).sqrt();
        ((sa.mean() - sb.mean()) / se.max(1e-300), se)
    };
    let mut again = false;
    for i in 0..K {
        let (zi, se) = z(&sa[i], &sb[i]);
        println!(
            "[对照] {}: Net {:.3} ± {:.3}，Path {:.3} ± {:.3}，差 {:.3}（合并标准误 {se:.3}，{zi:.2} 倍）",
            what[i],
            sa[i].mean(),
            sa[i].se(),
            sb[i].mean(),
            sb[i].se(),
            sa[i].mean() - sb[i].mean()
        );
        again |= zi.abs() > 2.0;
    }
    if !again {
        return;
    }
    feed(MORE, &mut sa, &mut sb);
    for i in 0..K {
        let (zi, se) = z(&sa[i], &sb[i]);
        println!(
            "[复核] {}: 132 个种子 Net {:.3} ± {:.3}，Path {:.3} ± {:.3}，差 {:.3}（合并标准误 {se:.3}，{zi:.2} 倍）",
            what[i],
            sa[i].mean(),
            sa[i].se(),
            sb[i].mean(),
            sb[i].se(),
            sa[i].mean() - sb[i].mean()
        );
        assert!(
            zi.abs() <= 2.0,
            "{}: 两个模型的均值相差 {}，超过两倍合并标准误 {}",
            what[i],
            sa[i].mean() - sb[i].mean(),
            2.0 * se
        );
    }
}
