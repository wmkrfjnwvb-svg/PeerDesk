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
    check_dyn(&what, &expected, &mut |seed| f(seed).to_vec());
}

/// 两个模型（旧的、新的）每个种子各测 K 个量，每个量的均值之差要在两倍合并标准误以内。
pub fn check_pair<const K: usize>(
    what: [&str; K],
    a: impl Fn(u64) -> [f64; K],
    b: impl Fn(u64) -> [f64; K],
) {
    check_pair_dyn(&what, &|seed| a(seed).to_vec(), &|seed| b(seed).to_vec());
}

fn collect(seeds: Range<u64>, f: &mut dyn FnMut(u64) -> Vec<f64>, s: &mut [Samples]) {
    for seed in seeds {
        for (sample, x) in s.iter_mut().zip(f(seed)) {
            sample.push(x);
        }
    }
}

fn check_dyn(what: &[&str], expected: &[f64], f: &mut dyn FnMut(u64) -> Vec<f64>) {
    let mut s = vec![Samples::default(); what.len()];
    collect(SEEDS, f, &mut s);
    let mut again = false;
    for ((name, want), sample) in what.iter().zip(expected).zip(&s) {
        let (m, se) = (sample.mean(), sample.se());
        let z = (m - want) / se.max(1e-300);
        println!("[统计] {name}: 12 个种子测得 {m:.6} ± {se:.6}，公式/参数 {want:.6}，差 {z:.2} 个标准误");
        again |= z.abs() > 2.0;
    }
    if !again {
        return;
    }
    collect(MORE, f, &mut s);
    for ((name, want), sample) in what.iter().zip(expected).zip(&s) {
        let (m, se) = (sample.mean(), sample.se());
        let z = (m - want) / se.max(1e-300);
        println!("[复核] {name}: 132 个种子测得 {m:.6} ± {se:.6}，公式/参数 {want:.6}，差 {z:.2} 个标准误");
        assert!(
            z.abs() <= 2.0,
            "{name}: 132 个种子测得 {m} ± {se}，公式值 {want} 不在两倍标准误以内"
        );
    }
}

fn check_pair_dyn(what: &[&str], a: &dyn Fn(u64) -> Vec<f64>, b: &dyn Fn(u64) -> Vec<f64>) {
    let mut sa = vec![Samples::default(); what.len()];
    let mut sb = vec![Samples::default(); what.len()];
    let feed = |seeds: Range<u64>, sa: &mut [Samples], sb: &mut [Samples]| {
        collect(seeds.clone(), &mut |seed| a(seed), sa);
        collect(seeds, &mut |seed| b(seed), sb);
    };
    feed(SEEDS, &mut sa, &mut sb);
    let report = |tag: &str, n: usize, sa: &[Samples], sb: &[Samples], assert_it: bool| {
        let mut again = false;
        for ((name, x), y) in what.iter().zip(sa).zip(sb) {
            let se = (x.se().powi(2) + y.se().powi(2)).sqrt();
            let diff = x.mean() - y.mean();
            let z = diff / se.max(1e-300);
            println!(
                "[{tag}] {name}: {n} 个种子 Net {:.3} ± {:.3}，Path {:.3} ± {:.3}，差 {diff:.3}（合并标准误 {se:.3}，{z:.2} 倍）",
                x.mean(),
                x.se(),
                y.mean(),
                y.se()
            );
            assert!(
                !assert_it || z.abs() <= 2.0,
                "{name}: 两个模型的均值相差 {diff}，超过两倍合并标准误 {}",
                2.0 * se
            );
            again |= z.abs() > 2.0;
        }
        again
    };
    if !report("对照", 12, &sa, &sb, false) {
        return;
    }
    feed(MORE, &mut sa, &mut sb);
    report("复核", 132, &sa, &sb, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 前 12 个种子偏高（1.0 左右），之后的种子围着 0 对称：第一轮判不过，复核通过。
    fn skewed_then_fair(seed: u64) -> [f64; 1] {
        let odd = (seed % 2) as f64;
        if seed < 12 {
            [1.0 + 0.2 * odd]
        } else {
            [(odd - 0.5) * 2.0]
        }
    }

    #[test]
    fn a_chance_miss_is_rechecked_with_more_seeds() {
        check(["偶然偏出去"], [0.0], skewed_then_fair);
        check_pair(["偶然偏出去（对照）"], skewed_then_fair, |_| [0.0]);
        // 一个种子算不出标准误：按 0
        let mut s = Samples::default();
        s.push(3.0);
        assert_eq!((s.mean(), s.se()), (3.0, 0.0));
    }

    #[test]
    #[should_panic(expected = "不在两倍标准误以内")]
    fn a_real_bias_still_fails_after_the_recheck() {
        check(["真有偏差"], [0.0], |seed| {
            [1.0 + 0.2 * (seed % 2) as f64]
        });
    }

    #[test]
    #[should_panic(expected = "超过两倍合并标准误")]
    fn a_real_difference_between_models_still_fails_after_the_recheck() {
        check_pair(
            ["真有差别"],
            |seed| [1.0 + 0.2 * (seed % 2) as f64],
            |_| [0.0],
        );
    }
}
