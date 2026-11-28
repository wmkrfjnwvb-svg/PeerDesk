//! 丢包层：独立丢包，或两态突发丢包（Gilbert-Elliott）。放在瓶颈之后，丢掉的包已经占过容量。
//!
//! 两态的参数换算照 WebRTC `SimulatedNetwork`：
//! - 坏状态保持的概率 `prob_loss_bursting = 1 - 1/平均突发长度`；
//! - 进入坏状态的概率 `prob_start_bursting = p / (1 - p) / 平均突发长度`，`p` 是长期丢包率。
//!
//! 每个包到达这一层时，先按状态转移（坏状态里以 `prob_loss_bursting` 留下，好状态里以 `prob_start_bursting` 进入），
//! 转移后在坏状态就丢这个包。所以丢包 ⇔ 处在坏状态，离开坏状态的那个包是好的，
//! 连续丢包的长度正好是几何分布，均值就是平均突发长度；长期丢包率 = 坏状态的稳态概率 = `p`。

/// 丢包模型。概率用百万分比（ppm）表示，避免浮点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Loss {
    /// 不丢。
    #[default]
    None,
    /// 每个包独立地以 `ppm`/10^6 的概率丢。
    Independent {
        /// 丢包率（百万分比）。
        ppm: u32,
    },
    /// 两态突发丢包。
    Burst {
        /// 长期丢包率（百万分比）。
        loss_ppm: u32,
        /// 平均突发长度，单位千分之一个包：1000 = 1 个包，4000 = 4 个包。小于 1000 时按 1000 算。
        avg_burst_milli: u32,
    },
}

const ONE: u64 = 1_000_000;

/// 两态模型的转移概率（百万分比）：（进入坏状态，留在坏状态）。
/// 进入概率超过 1 时（`p > L/(L+1)`，表示要求的丢包率在这个突发长度下做不到）按 1 算，长期丢包率会比参数低。
#[must_use]
pub fn burst_probabilities(loss_ppm: u32, avg_burst_milli: u32) -> (u64, u64) {
    let p = u64::from(loss_ppm).min(ONE - 1);
    let l = u64::from(avg_burst_milli).max(1000);
    // stay = 1 - 1/L = 1 - 1000/l
    let stay = ONE - ONE * 1000 / l;
    // start = p/(1-p)/L，u128 防溢出
    let start = u128::from(p) * u128::from(ONE) * 1000 / u128::from(ONE - p) / u128::from(l);
    (u64::try_from(start).unwrap_or(ONE).min(ONE), stay)
}

impl Loss {
    /// 这一层有没有开。
    #[must_use]
    pub const fn enabled(&self) -> bool {
        match self {
            Loss::None => false,
            Loss::Independent { .. } | Loss::Burst { .. } => true,
        }
    }
}

/// 丢包层的状态：是否在坏状态。
#[derive(Debug, Clone, Copy, Default)]
pub struct LossState {
    bad: bool,
}

impl LossState {
    /// 用一个随机数 `draw` 判断这个包丢不丢。
    pub fn lost(&mut self, loss: Loss, draw: u64) -> bool {
        let r = draw % ONE;
        match loss {
            Loss::None => false,
            Loss::Independent { ppm } => r < u64::from(ppm),
            Loss::Burst {
                loss_ppm,
                avg_burst_milli,
            } => {
                let (start, stay) = burst_probabilities(loss_ppm, avg_burst_milli);
                self.bad = r < if self.bad { stay } else { start };
                self.bad
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::rng::{layer, Dir, LayerRng};
    use crate::path::testkit::check;

    #[test]
    fn probabilities_follow_the_webrtc_formulas() {
        // p = 2%，平均突发 4 个包
        let (start, stay) = burst_probabilities(20_000, 4_000);
        let p = 0.02_f64;
        let l = 4.0_f64;
        assert_eq!(stay, 750_000); // 1 - 1/4
        let want_start = p / (1.0 - p) / l * 1e6;
        assert!(
            (start as f64 - want_start).abs() <= 1.0,
            "{start} vs {want_start}"
        );
        // 突发长度小于 1 按 1 算：留在坏状态的概率为 0
        assert_eq!(burst_probabilities(10_000, 500).1, 0);
        // 要求的丢包率在这个突发长度下做不到：进入概率封顶为 1
        assert_eq!(burst_probabilities(900_000, 1_000).0, ONE);
        // 丢包率 100% 也不除以 0
        assert!(burst_probabilities(1_000_000, 2_000).0 <= ONE);
    }

    #[test]
    fn none_never_loses_and_is_disabled() {
        let mut s = LossState::default();
        assert!(!Loss::None.enabled());
        assert!(Loss::Independent { ppm: 1 }.enabled());
        assert!(Loss::Burst {
            loss_ppm: 1,
            avg_burst_milli: 2000
        }
        .enabled());
        assert!((0..1000).all(|i| !s.lost(Loss::None, i)));
    }

    /// 一个种子上的（丢包率，平均连续丢包长度）。
    fn run(loss: Loss, seed: u64, n: u32) -> [f64; 2] {
        let mut rng = LayerRng::new(seed, Dir::Forward, layer::LOSS);
        let mut s = LossState::default();
        let (mut lost, mut runs, mut cur) = (0u32, 0u32, 0u32);
        for _ in 0..n {
            if s.lost(loss, rng.next_u64()) {
                lost += 1;
                cur += 1;
            } else if cur > 0 {
                runs += 1;
                cur = 0;
            }
        }
        if cur > 0 {
            runs += 1;
        }
        [
            f64::from(lost) / f64::from(n),
            f64::from(lost) / f64::from(runs.max(1)),
        ]
    }

    /// 独立丢包：长期丢包率等于参数；连续丢包的平均长度等于 1/(1-p)（几何分布）。
    #[test]
    fn independent_loss_rate_matches() {
        check(
            ["独立丢包率", "独立丢包的平均连丢长度"],
            [0.05, 1.0 / 0.95],
            |seed| run(Loss::Independent { ppm: 50_000 }, seed, 400_000),
        );
    }

    /// 两态突发丢包：长期丢包率 = p，平均突发长度 = L。
    #[test]
    fn burst_loss_matches_rate_and_mean_burst_length() {
        for (p_ppm, l_milli) in [(20_000u32, 4_000u32), (100_000, 8_000), (10_000, 2_500)] {
            let tag = format!("p={p_ppm}ppm L={l_milli}/1000");
            check(
                [&format!("{tag} 长期丢包率"), &format!("{tag} 平均突发长度")],
                [f64::from(p_ppm) / 1e6, f64::from(l_milli) / 1e3],
                |seed| {
                    run(
                        Loss::Burst {
                            loss_ppm: p_ppm,
                            avg_burst_milli: l_milli,
                        },
                        seed,
                        500_000,
                    )
                },
            );
        }
    }
}
