//! 按实测拟合的字节模型（M2，见 `docs/research/Q-redesign/m2-m3-protocol.md` 第 1 节、`data-m2.md`）。
//!
//! 系数在 `models/openh264-v1.tsv` 里：用 `crates/e2e/examples/measure.rs` 让真的 `OpenH264Backend` 按固定 QP 编合成桌面，
//! 再用 `scripts/m2-fit.py` 对 `log2(字节 − 帧头)` 做最小二乘得到。形式：
//!
//! ```text
//! 普通帧：  log2(字节 − 帧头) = a + b·QP + c·log2(变化的像素数) + d·log2(细节)
//! 关键帧：  同上，变化的像素数换成总像素数
//! 补清晰帧：同普通帧，再加 e·(上一帧 QP − QP)，变化的像素数换成总像素数（画面没变，变化面积是 0，大小由“从多粗补到多细”决定）
//! ```
//!
//! 所有运算用整数定点（对数和指数都是查表加移位），不用浮点：仿真要求同一输入逐字节相同，浮点库函数在不同平台上的最后一位
//! 不保证一致。系数在文件里是十进制文本，按 10⁻⁶ 的整数读，不经过浮点。
//!
//! 残差：[`FittedByteModel::with_noise`] 打开时按残差标准差给字节数乘一个对数正态的扰动，免得帧大小太整齐、
//! 测不出“偶尔一个大帧”引起的行为。扰动只由（第几帧、是不是关键帧、像素数）决定，**不依赖 QP**，所以对 QP 仍然单调。
//!
//! **不是默认模型**：换掉 [`crate::fake::DefaultByteModel`] 要先和 L1 对账（下一步）。

use std::collections::HashMap;

use crate::fake::{ByteModel, ModelInput};

/// 随包带的 openh264 模型文件（屏幕内容实时模式，复杂度低）。
pub const OPENH264_V1: &str = include_str!("../models/openh264-v1.tsv");

/// 定点小数 Q16 的 1。
const ONE_Q16: i128 = 1 << 16;
/// 系数的单位：10⁻⁶。
const MICRO: i128 = 1_000_000;
/// 补清晰行的类别名。
pub const REFINE: &str = "refine";
/// 所有普通帧合在一起拟合的类别名（也是关键帧行的类别名）。
pub const ANY: &str = "any";

/// 2^(2^-k) × 2^32，k = 1 到 16（指数的小数部分按位相乘）。
const EXP2_STEPS: [u128; 16] = [
    6_074_001_000,
    5_107_605_667,
    4_683_695_048,
    4_485_121_744,
    4_389_014_833,
    4_341_736_423,
    4_318_288_544,
    4_306_612_134,
    4_300_785_774,
    4_297_875_550,
    4_296_421_177,
    4_295_694_175,
    4_295_330_720,
    4_295_149_004,
    4_295_058_149,
    4_295_012_722,
];

/// `log2(n)`，Q16 定点（`n` 为 0 当 1）。
#[must_use]
pub fn log2_q16(n: u64) -> i128 {
    let n = n.max(1);
    let lead = i128::from(n.leading_zeros());
    let int = 63 - lead;
    // 尾数在 [1, 2)，Q63
    let mut m = u128::from(n << n.leading_zeros());
    let mut frac = 0i128;
    for _ in 0..16 {
        m = (m * m) >> 63;
        frac <<= 1;
        if m >= 1 << 64 {
            frac |= 1;
            m >>= 1;
        }
    }
    int * ONE_Q16 + frac
}

/// `2^x`，`x` 是 Q16 定点，结果是 Q32（超过 2^60 当 2^60）。
#[must_use]
pub fn exp2_q32(x: i128) -> u128 {
    let int = x.div_euclid(ONE_Q16).clamp(-64, 60);
    let frac = u32::try_from(x.rem_euclid(ONE_Q16)).unwrap_or(0);
    let mut m: u128 = 1 << 32;
    for (k, step) in EXP2_STEPS.iter().enumerate() {
        if frac >> (15 - k) & 1 == 1 {
            m = (m * step) >> 32;
        }
    }
    if int >= 0 {
        m << u32::try_from(int).unwrap_or(0)
    } else {
        m >> u32::try_from(-int).unwrap_or(0)
    }
}

/// 帧种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Normal,
    Key,
}

/// 一行系数（都是 10⁻⁶ 的整数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row {
    /// 帧头字节数（取不出内容的固定开销）。
    head: u64,
    a: i128,
    b: i128,
    c: i128,
    d: i128,
    e: i128,
    /// 残差标准差（log2 单位）。
    sigma: i128,
}

/// 读模型文件出的错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelError(pub String);

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "字节模型文件有误：{}", self.0)
    }
}

impl std::error::Error for ModelError {}

fn bad<T>(why: impl Into<String>) -> Result<T, ModelError> {
    Err(ModelError(why.into()))
}

/// 十进制文本读成 10⁻⁶ 的整数（最多读 6 位小数，多的截掉）。
pub(crate) fn micro(text: &str) -> Result<i128, ModelError> {
    let (neg, digits) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    let all_digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if int.is_empty() || !all_digits(int) || !all_digits(frac) {
        return bad(format!("不是十进制数：{text}"));
    }
    let int: i128 = int
        .parse()
        .map_err(|_| ModelError(format!("数太大：{text}")))?;
    let frac6: String = frac.chars().chain(std::iter::repeat('0')).take(6).collect();
    // 上面验过全是数字，读得出来
    let frac: i128 = frac6.parse().unwrap_or(0);
    let v = int
        .checked_mul(MICRO)
        .and_then(|i| i.checked_add(frac))
        .ok_or_else(|| ModelError(format!("数太大：{text}")))?;
    Ok(if neg { -v } else { v })
}

/// 解析好的模型文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTable {
    rows: HashMap<(String, Kind), Row>,
    /// 变化面积达到这么多（千分比），后端自己出关键帧。
    scene_key_permille: u32,
}

impl ModelTable {
    /// 读模型文件（格式见 `models/openh264-v1.tsv` 的注释）。
    ///
    /// # Errors
    /// 格式不对，或系数不满足“QP 越大字节不增”（`b <= 0`、`e >= 0`）。
    pub fn parse(text: &str) -> Result<Self, ModelError> {
        let mut rows = HashMap::new();
        let mut scene = None;
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.starts_with("类别") {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            if f.first() == Some(&"scene_key_permille") {
                let v = f
                    .get(1)
                    .ok_or_else(|| ModelError("场景门槛没有值".into()))?;
                scene = Some(
                    v.parse::<u32>()
                        .map_err(|_| ModelError(format!("场景门槛不是整数：{v}")))?,
                );
                continue;
            }
            let at = |i: usize| {
                f.get(i)
                    .copied()
                    .ok_or_else(|| ModelError(format!("字段不够：{line}")))
            };
            let kind = match at(1)? {
                "normal" => Kind::Normal,
                "key" => Kind::Key,
                other => return bad(format!("不认识的帧种类：{other}")),
            };
            let row = Row {
                head: at(2)?
                    .parse()
                    .map_err(|_| ModelError(format!("帧头不是整数：{line}")))?,
                a: micro(at(3)?)?,
                b: micro(at(4)?)?,
                c: micro(at(5)?)?,
                d: micro(at(6)?)?,
                e: micro(at(7)?)?,
                sigma: micro(at(8)?)?,
            };
            if row.b > 0 || row.e < 0 || row.sigma < 0 {
                return bad(format!("系数会让字节数随 QP 增大：{line}"));
            }
            if rows.insert((at(0)?.to_owned(), kind), row).is_some() {
                return bad(format!("重复的行：{line}"));
            }
        }
        let scene_key_permille = scene.ok_or_else(|| ModelError("缺 scene_key_permille".into()))?;
        Ok(Self {
            rows,
            scene_key_permille,
        })
    }

    /// 随包带的 openh264 模型。
    ///
    /// # Errors
    /// 文件有误（有单元测试守着，不会发生）。
    pub fn openh264_v1() -> Result<Self, ModelError> {
        Self::parse(OPENH264_V1)
    }

    /// 后端自己出关键帧的变化面积门槛（千分比）：假后端的 [`crate::fake::SCENE_PERMILLE`] 应当取这个数。
    #[must_use]
    pub const fn scene_key_permille(&self) -> u32 {
        self.scene_key_permille
    }

    /// 有没有这个类别的普通帧系数。
    #[must_use]
    pub fn has_class(&self, class: &str) -> bool {
        self.rows.contains_key(&(class.to_owned(), Kind::Normal))
    }
}

/// 按实测系数估字节数的模型。
#[derive(Debug, Clone)]
pub struct FittedByteModel {
    table: ModelTable,
    /// 普通帧用哪一类的系数（[`ANY`] 是各类合在一起拟合的）。
    class: String,
    noise: Noise,
    /// 序列内残差标准差（10⁻⁶ 的 log2）：[`Noise::Within`] 时普通帧用它。模型文件里没有这个数，由调用方给
    /// （[`Self::with_within_sigma`]）；没给是 0。
    within: i128,
}

impl FittedByteModel {
    /// 用 `class` 这一类的普通帧系数；关键帧用 [`ANY`] 的，补清晰用 [`REFINE`] 的。打开残差扰动。
    ///
    /// # Errors
    /// 表里缺这一类，或缺关键帧、补清晰的行。
    pub fn new(table: ModelTable, class: &str) -> Result<Self, ModelError> {
        let need = [
            (class, Kind::Normal),
            (ANY, Kind::Key),
            (REFINE, Kind::Normal),
        ];
        for (c, k) in need {
            if !table.rows.contains_key(&(c.to_owned(), k)) {
                return bad(format!("缺 {c} {k:?} 的行"));
            }
        }
        Ok(Self {
            table,
            class: class.to_owned(),
            noise: Noise::Full,
            within: 0,
        })
    }

    /// 随包带的 openh264 模型，普通帧用各类合在一起的系数。
    ///
    /// # Errors
    /// 同 [`Self::new`]。
    pub fn openh264_v1() -> Result<Self, ModelError> {
        Self::new(ModelTable::openh264_v1()?, ANY)
    }

    /// 打开或关掉残差扰动。
    #[must_use]
    pub const fn with_noise(mut self, on: bool) -> Self {
        self.noise = if on { Noise::Full } else { Noise::Off };
        self
    }

    /// 扰动只用序列内的残差标准差 `sigma_micro`（10⁻⁶ 的 log2，见 [`Noise::Within`]）。
    /// 这个模型的文件里没有拆过残差，数由调用方按 `scripts/m2b-residuals.py` 的结果给。
    #[must_use]
    pub const fn with_within_sigma(mut self, sigma_micro: u32) -> Self {
        self.noise = Noise::Within;
        self.within = sigma_micro as i128;
        self
    }

    /// 后端自己出关键帧的变化面积门槛（千分比）。
    #[must_use]
    pub const fn scene_key_permille(&self) -> u32 {
        self.table.scene_key_permille
    }

    /// 这一帧用哪一行：关键帧、补清晰（画面一点没变、QP 比上一帧小）、普通帧。
    fn row(&self, input: &ModelInput) -> (Row, bool) {
        let (class, kind, refine) = if input.key {
            (ANY, Kind::Key, false)
        } else if input.changed_permille == 0 && input.prev_qp.is_some_and(|p| p > input.qp) {
            (REFINE, Kind::Normal, true)
        } else {
            (self.class.as_str(), Kind::Normal, false)
        };
        let row = self
            .table
            .rows
            .get(&(class.to_owned(), kind))
            .copied()
            .unwrap_or(Row {
                head: 24,
                a: 0,
                b: 0,
                c: 0,
                d: 0,
                e: 0,
                sigma: 0,
            });
        (row, refine)
    }

    /// 标准正态的近似（12 个均匀数之和减 6），Q16；只由帧的序号和形状决定，不看 QP。
    fn gauss_q16(input: &ModelInput) -> i128 {
        let mut state = u64::from(input.serial).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ input.pixels.rotate_left(17)
            ^ u64::from(input.key) << 63;
        let mut sum = 0i128;
        for _ in 0..3 {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^= z >> 31;
            for shift in [0, 16, 32, 48] {
                sum += i128::from((z >> shift) & 0xffff);
            }
        }
        sum - 6 * ONE_Q16
    }
}

/// 给模型的预测加多少扰动。
///
/// 模型是在 log2 上做最小二乘拟合的，预测的是每帧字节数的**中位**。扰动是乘一个 2^(σZ)（Z 标准正态）：中位不变，
/// 均值变成 2^(σ²·ln2/2) 倍。所以 σ 取多大决定了一段内容的总字节被抬高多少：
/// - 模型文件里的“残差标准差”是全部拟合行合在一起的，主要是结构误差（这一类内容、这个 QP 整段偏大或偏小），
///   不是帧与帧之间的波动。拿它当每帧独立的扰动，总字节被抬高 1.77 倍（v2）、3.23 倍（v1），还凭空多出大帧。
/// - 帧与帧之间真正的波动是“序列之内”的那一部分（同一段画面按同一个 QP 连着编，每帧相对这一段的平均偏多少），
///   v2 是 0.573、v1 是 0.527（`scripts/m2b-residuals.py`，M2b 的拟合集），总字节只抬高约 1.08 倍。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noise {
    /// 不加：每帧就是模型的中位预测。
    Off,
    /// 只用序列内的残差标准差：帧间波动，不含结构误差。
    Within,
    /// 用全部残差的标准差（原来的做法，留着对照；会把总字节抬高，见上）。
    Full,
}

/// Q16 的 log2 换成 10⁻⁶ 单位。
const fn q16_to_micro(v: i128) -> i128 {
    v * MICRO / ONE_Q16
}

impl ByteModel for FittedByteModel {
    fn bytes(&self, input: &ModelInput) -> usize {
        let (row, refine) = self.row(input);
        let qp = i128::from(input.qp.min(crate::fake::MODEL_MAX_QP));
        // 面积项：普通帧是变化的像素数，关键帧和补清晰是总像素数
        let area = if input.key || refine {
            input.pixels
        } else {
            input
                .pixels
                .saturating_mul(u64::from(input.changed_permille.min(1000)))
                / 1000
        };
        let detail = u64::from(input.detail_permille.min(1000));
        let up = i128::from(input.prev_qp.map_or(0, |p| {
            p.saturating_sub(input.qp.min(crate::fake::MODEL_MAX_QP))
        }));
        let mut x_micro = row.a
            + row.b * qp
            + row.c * q16_to_micro(log2_q16(area)) / MICRO
            + row.d * q16_to_micro(log2_q16(detail)) / MICRO;
        if refine {
            x_micro += row.e * up;
        }
        let mut x_q16 = x_micro.saturating_mul(ONE_Q16).div_euclid(MICRO);
        let sigma = match self.noise {
            Noise::Off => 0,
            Noise::Full => row.sigma,
            // 关键帧、补清晰一步的残差没有序列内的部分可拆（一个序列只有一帧），不加
            Noise::Within if input.key || refine => 0,
            Noise::Within => self.within,
        };
        if sigma != 0 {
            x_q16 += sigma * Self::gauss_q16(input) / MICRO;
        }
        let body = (exp2_q32(x_q16) + (1 << 31)) >> 32;
        let total = u128::from(row.head).saturating_add(body);
        usize::try_from(total).unwrap_or(usize::MAX).max(1)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation
    )]
    use super::*;

    const SAMPLE: &str =
        "# 注释\n类别\t帧种类\t帧头\ta\tb\tc\td\te\t残差标准差\t样本数\t留出中位误差\n\
        any\tnormal\t6\t-2.5\t-0.15\t0.5\t0.25\t0\t0.5\t10\t0.1\n\
        typing\tnormal\t7\t-1\t-0.1\t0.4\t0.2\t0\t0\t10\t0.1\n\
        any\tkey\t40\t3\t-0.12\t0.8\t0.3\t0\t0.25\t10\t0.1\n\
        refine\tnormal\t6\t1\t-0.1\t0.6\t0.3\t0.05\t0.25\t10\t0.1\n\
        scene_key_permille\t700\n";

    fn input() -> ModelInput {
        ModelInput {
            qp: 28,
            key: false,
            pixels: 1920 * 1080,
            changed_permille: 100,
            detail_permille: 120,
            prev_qp: Some(28),
            serial: 5,
            regions: crate::fake::Regions::default(),
        }
    }

    #[test]
    fn fixed_point_log_and_exp_match_floating_point() {
        for n in [
            1u64,
            2,
            3,
            7,
            100,
            1000,
            12345,
            2_073_600,
            8_294_400,
            u64::MAX,
        ] {
            let got = log2_q16(n) as f64 / 65536.0;
            let want = (n as f64).log2();
            assert!((got - want).abs() < 2e-4, "log2({n}) = {got}，应为 {want}");
        }
        assert_eq!(log2_q16(0), 0);
        for x in [-20.5f64, -3.25, -0.5, 0.0, 0.3, 1.0, 7.77, 13.9, 20.0] {
            let q = (x * 65536.0).round() as i128;
            let got = exp2_q32(q) as f64 / 4_294_967_296.0;
            let want = x.exp2();
            assert!(
                (got / want - 1.0).abs() < 1e-3 || (got - want).abs() < 1e-8,
                "2^{x}"
            );
        }
        // 上限：不溢出
        assert!(exp2_q32(1000 * 65536) > 0);
        assert_eq!(exp2_q32(-1000 * 65536), 0);
    }

    #[test]
    fn decimal_text_reads_as_integers_without_floats() {
        assert_eq!(micro("0.5").unwrap(), 500_000);
        assert_eq!(micro("-0.153846").unwrap(), -153_846);
        assert_eq!(micro("12").unwrap(), 12_000_000);
        assert_eq!(micro("1.2345678").unwrap(), 1_234_567, "多的位数截掉");
        for bad in [
            "",
            "-",
            "a",
            "1.x",
            ".5",
            "1e3",
            "99999999999999999999999999999999999999999",
            "9999999999999999999999999999999999999",
        ] {
            assert!(micro(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parse_rejects_broken_files() {
        let t = ModelTable::parse(SAMPLE).unwrap();
        assert_eq!(t.scene_key_permille(), 700);
        assert!(t.has_class("typing") && !t.has_class("video"));
        let cases = [
            SAMPLE.replace("scene_key_permille\t700\n", ""),
            SAMPLE.replace("scene_key_permille\t700", "scene_key_permille\tx"),
            SAMPLE.replace("scene_key_permille\t700", "scene_key_permille"),
            SAMPLE.replace("normal\t7", "weird\t7"),
            SAMPLE.replace("typing\tnormal\t7\t-1", "typing\tnormal\tx\t-1"),
            SAMPLE.replace("-0.1\t0.4", "0.1\t0.4"),
            SAMPLE.replace("0\t0\t10\t0.1\nany", "-1\t0\t10\t0.1\nany"),
            SAMPLE.replace("0.05\t0.25", "-0.05\t0.25"),
            SAMPLE.replace("typing\tnormal", "any\tnormal"),
            "any\tnormal\t7\n".to_owned(),
        ];
        for text in cases {
            assert!(ModelTable::parse(&text).is_err(), "{text}");
        }
        assert!(format!("{}", ModelError("x".into())).contains("字节模型文件有误"));
    }

    #[test]
    fn model_picks_rows_and_checks_the_table_is_complete() {
        let t = ModelTable::parse(SAMPLE).unwrap();
        assert!(FittedByteModel::new(t.clone(), "video").is_err());
        let no_refine = SAMPLE.replace("refine\tnormal", "refine\tkey");
        assert!(FittedByteModel::new(ModelTable::parse(&no_refine).unwrap(), "any").is_err());
        let m = FittedByteModel::new(t, "typing").unwrap().with_noise(false);
        assert_eq!(m.scene_key_permille(), 700);
        // 手算：typing，变化像素 207360，细节 120：2^(-1 - 0.1×28 + 0.4×log2(207360) + 0.2×log2(120)) + 7
        let want = 7.0 + (-1.0 - 2.8 + 0.4 * 207_360f64.log2() + 0.2 * 120f64.log2()).exp2();
        let got = m.bytes(&input()) as f64;
        assert!((got / want - 1.0).abs() < 0.01, "{got} 对 {want}");
        // 关键帧：用总像素数
        let key = m.bytes(&ModelInput {
            key: true,
            ..input()
        });
        let want =
            40.0 + (3.0 - 0.12 * 28.0 + 0.8 * 2_073_600f64.log2() + 0.3 * 120f64.log2()).exp2();
        assert!((key as f64 / want - 1.0).abs() < 0.01);
        // 补清晰：画面没变、QP 比上一帧小；多补得越多越大
        let refine = ModelInput {
            changed_permille: 0,
            prev_qp: Some(40),
            ..input()
        };
        let far = m.bytes(&refine);
        let near = m.bytes(&ModelInput {
            prev_qp: Some(32),
            ..refine
        });
        assert!(far > near);
        let want = 6.0
            + (1.0 - 2.8 + 0.6 * 2_073_600f64.log2() + 0.3 * 120f64.log2() + 0.05 * 12.0).exp2();
        assert!((far as f64 / want - 1.0).abs() < 0.01);
        // 画面没变、QP 没有往下：不是补清晰，按普通帧（变化像素 0，当 1 个）
        let still = m.bytes(&ModelInput {
            changed_permille: 0,
            ..input()
        });
        assert!(still < near);
        // 没有上一帧的 QP（刚配置）也不算补清晰
        assert!(
            m.bytes(&ModelInput {
                changed_permille: 0,
                prev_qp: None,
                ..input()
            }) < near
        );
    }

    #[test]
    fn bytes_never_grow_with_qp_even_with_noise() {
        let noisy = FittedByteModel::new(ModelTable::parse(SAMPLE).unwrap(), "any").unwrap();
        for class_input in [
            input(),
            ModelInput {
                key: true,
                ..input()
            },
            ModelInput {
                changed_permille: 0,
                prev_qp: Some(51),
                ..input()
            },
        ] {
            let mut prev = usize::MAX;
            for qp in 10..=51 {
                let b = noisy.bytes(&ModelInput { qp, ..class_input });
                assert!(b <= prev && b > 0, "QP {qp}：{b} 比 {prev} 多");
                prev = b;
            }
        }
    }

    #[test]
    fn noise_is_deterministic_log_normal_and_off_switchable() {
        let m = FittedByteModel::new(ModelTable::parse(SAMPLE).unwrap(), "any").unwrap();
        let quiet = m.clone().with_noise(false);
        assert_eq!(m.bytes(&input()), m.bytes(&input()), "同输入同输出");
        // 关键帧行的 sigma = 0.25（log2 单位）：帧头和正文比可以忽略，2000 帧的对数偏差标准差约 0.25、均值约 0
        let input = ModelInput {
            key: true,
            ..input()
        };
        let base = quiet.bytes(&input);
        let logs: Vec<f64> = (0..2000u32)
            .map(|serial| {
                let b = m.bytes(&ModelInput { serial, ..input });
                (b as f64 / base as f64).log2()
            })
            .collect();
        let mean = logs.iter().sum::<f64>() / logs.len() as f64;
        let var = logs.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / logs.len() as f64;
        assert!(mean.abs() < 0.05, "均值 {mean}");
        assert!((var.sqrt() - 0.25).abs() < 0.03, "标准差 {}", var.sqrt());
        assert!(logs.iter().any(|l| *l > 0.5), "偶尔有一个大帧");
        // 不同的帧序号，扰动不同
        assert_ne!(
            m.bytes(&ModelInput { serial: 3, ..input }),
            m.bytes(&ModelInput { serial: 4, ..input })
        );
        // 关掉扰动：和帧序号无关
        assert_eq!(
            quiet.bytes(&ModelInput { serial: 3, ..input }),
            quiet.bytes(&ModelInput { serial: 4, ..input })
        ); // 只用序列内 σ（由调用方给，这里 0.3）：关键帧不加；普通帧的波动是 0.3，中位不变
        let within = m.clone().with_within_sigma(300_000);
        assert_eq!(
            within.bytes(&ModelInput { serial: 3, ..input }),
            quiet.bytes(&ModelInput { serial: 4, ..input }),
            "序列内扰动不动关键帧"
        );
        let normal = ModelInput {
            key: false,
            changed_permille: 400,
            ..input
        };
        let base = quiet.bytes(&normal) as f64;
        let logs: Vec<f64> = (0..2000u32)
            .map(|serial| (within.bytes(&ModelInput { serial, ..normal }) as f64 / base).log2())
            .collect();
        let mean = logs.iter().sum::<f64>() / logs.len() as f64;
        let var = logs.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / logs.len() as f64;
        assert!(mean.abs() < 0.05, "均值 {mean}");
        // 样例表里普通帧的正文不比帧头大多少，帧头不参与扰动，所以整帧的波动比 0.3 小一点
        assert!((0.2..0.33).contains(&var.sqrt()), "标准差 {}", var.sqrt());
        // 补清晰一步也不加（一个序列只有一帧，没有序列内的部分）
        let refine = ModelInput {
            key: false,
            changed_permille: 0,
            prev_qp: Some(normal.qp.saturating_add(6)),
            ..input
        };
        assert_eq!(
            within.bytes(&ModelInput {
                serial: 3,
                ..refine
            }),
            quiet.bytes(&ModelInput {
                serial: 8,
                ..refine
            })
        );
    }

    /// 随包带的文件能读，类别齐全，关键帧远大于普通帧，QP 每加 6 字节数差不多减半。
    #[test]
    fn the_shipped_model_file_loads_and_has_the_right_shape() {
        let table = ModelTable::openh264_v1().unwrap();
        for class in [
            "any",
            "pointer",
            "typing",
            "scrolling",
            "dragging",
            "switching",
            "video",
            REFINE,
        ] {
            assert!(table.has_class(class), "缺 {class}");
        }
        assert!((100..=1001).contains(&table.scene_key_permille()));
        let m = FittedByteModel::openh264_v1().unwrap().with_noise(false);
        let normal = ModelInput {
            changed_permille: 300,
            ..input()
        };
        let key = m.bytes(&ModelInput {
            key: true,
            ..normal
        });
        assert!(key > m.bytes(&normal));
        let (lo, hi) = (
            m.bytes(&ModelInput { qp: 20, ..normal }),
            m.bytes(&ModelInput { qp: 26, ..normal }),
        );
        assert!(hi < lo && hi * 4 > lo, "{lo} → {hi}");
    }
}
