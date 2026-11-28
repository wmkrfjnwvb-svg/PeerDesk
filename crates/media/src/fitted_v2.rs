//! 按画面区域拟合的字节模型（M2b 第三步 B3，见 `docs/research/Q-redesign/m2b-plan.md` 第 3 节、`data-m2b.md` 第 4 节）。
//!
//! 系数在 `models/openh264-v2.tsv` 里。和 M2 的 [`crate::fitted`] 的区别：输入不再是“变化面积千分比”，而是假后端交的
//! [`Regions`](crate::fake::Regions)——脏区域和移动区域碰到的宏块数、脏区域细节、整屏移动标记、静止宏块的画质差分布。
//! 形式 F3，**各部分相加**：
//!
//! ```text
//! 字节 = 帧头 + D（新内容） + M（搬来的） + S（静止部分补清晰） + 地板
//! D = 2^(a_d + b_d·QP) · 脏面积^c_d · 脏区域细节^d_d                      脏面积 = 脏区域碰到的宏块数 × 256
//! M = 2^(a_m + b_m·QP) · 移动面积^c_m · 2^(f·整屏移动)                    移动面积 = 移动区域碰到、脏区域没碰到的宏块数 × 256
//! S = 2^(a_s + b_s·QP) · 全帧细节^d_s · Σ_g n_g · 256 · 2^(e·g)           n_g = 现在画质比这一帧粗 g 档的静止宏块数
//! 地板 = 2^(a_f + b_f·QP) · 静止像素数       （QP ≤ 地板最大QP 时才有；B2 的结论：和历史无关，只看 QP）
//! ```
//!
//! 关键帧沿用 M2 的行（`log2(字节 − 帧头) = a + b·QP + c·log2(总像素) + d·log2(细节)`），不重拟。
//!
//! 运算全是整数定点（对数、指数查表加移位，和 [`crate::fitted`] 共用），不用浮点：仿真要求同输入逐字节相同。
//! 对 QP 单调：每一项的系数 `b ≤ 0`；静止宏块的画质差随 QP 变大只会变小；地板只在 QP 小的一侧有；扰动不依赖 QP。
//!
//! **不是默认模型**：换掉 [`crate::fake::DefaultByteModel`] 要先和 L1 对账（A1 第五步）。

use std::collections::HashMap;

use crate::fake::{ByteModel, ModelInput, MODEL_MAX_QP};
use crate::fitted::{exp2_q32, log2_q16, micro, ModelError};

/// 随包带的 openh264 模型文件（屏幕内容实时模式，复杂度低），输入是画面区域。
pub const OPENH264_V2: &str = include_str!("../models/openh264-v2.tsv");

/// 定点小数 Q16 的 1。
const ONE_Q16: i128 = 1 << 16;
/// 系数的单位：10⁻⁶。
const MICRO: i128 = 1_000_000;
/// 一个宏块的像素数（面积 = 宏块数 × 这个数）。
const MB_PIXELS: u64 = 256;
/// 求和前把总字节数压在 2^80 以内（Q32），免得乘扰动时溢出；2^48 字节远大于任何真实的帧。
const TOTAL_CAP_Q32: u128 = 1 << 80;

/// 形式 F3 的 14 个参数名，文件里 `参数` 行的顺序不限，缺一个都不行。
const PARAMS: [&str; 14] = [
    "a_d", "b_d", "c_d", "d_d", "a_m", "b_m", "c_m", "f", "a_s", "b_s", "d_s", "e", "a_f", "b_f",
];

/// 关键帧的一行系数（10⁻⁶ 的整数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct KeyRow {
    head: u64,
    a: i128,
    b: i128,
    c: i128,
    d: i128,
    sigma: i128,
}

/// F3 的系数（10⁻⁶ 的整数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Coeffs {
    a_d: i128,
    b_d: i128,
    c_d: i128,
    d_d: i128,
    a_m: i128,
    b_m: i128,
    c_m: i128,
    f: i128,
    a_s: i128,
    b_s: i128,
    d_s: i128,
    e: i128,
    a_f: i128,
    b_f: i128,
}

/// 解析好的模型文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionTable {
    /// 普通帧的帧头字节数。
    head: u64,
    /// 普通帧的残差标准差（log2 单位，10⁻⁶）。
    sigma: i128,
    key: KeyRow,
    c: Coeffs,
    /// 地板只在 QP 不超过这个数时有。
    floor_max_qp: u8,
    scene_key_permille: u32,
}

fn bad<T>(why: impl Into<String>) -> Result<T, ModelError> {
    Err(ModelError(why.into()))
}

impl RegionTable {
    /// 读模型文件（格式见 `models/openh264-v2.tsv` 的注释）。
    ///
    /// # Errors
    /// 格式不对、缺行缺参数、形式不是 F3，或系数会让字节数随 QP 增大（任何一项的 `b > 0`、指数为负、残差标准差为负）。
    pub fn parse(text: &str) -> Result<Self, ModelError> {
        let mut form = None;
        let mut head = None;
        let mut sigma = None;
        let mut key = None;
        let mut floor_max = None;
        let mut scene = None;
        let mut params: HashMap<String, i128> = HashMap::new();
        for line in text.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let f: Vec<&str> = line.split('\t').collect();
            let at = |i: usize| {
                f.get(i)
                    .copied()
                    .ok_or_else(|| ModelError(format!("字段不够：{line}")))
            };
            let int = |i: usize| -> Result<u64, ModelError> {
                at(i)?
                    .parse()
                    .map_err(|_| ModelError(format!("不是整数：{line}")))
            };
            match at(0)? {
                "形式" => form = Some(at(1)?.to_owned()),
                "帧头" => head = Some(int(1)?),
                "残差标准差" => sigma = Some(micro(at(1)?)?),
                "关键帧" => {
                    key = Some(KeyRow {
                        head: int(1)?,
                        a: micro(at(2)?)?,
                        b: micro(at(3)?)?,
                        c: micro(at(4)?)?,
                        d: micro(at(5)?)?,
                        sigma: micro(at(6)?)?,
                    });
                }
                "地板最大QP" => {
                    floor_max = Some(
                        u8::try_from(int(1)?)
                            .map_err(|_| ModelError(format!("QP 太大：{line}")))?,
                    );
                }
                "scene_key_permille" => {
                    scene = Some(
                        u32::try_from(int(1)?)
                            .map_err(|_| ModelError(format!("场景门槛太大：{line}")))?,
                    );
                }
                "参数" => {
                    let name = at(1)?;
                    if !PARAMS.contains(&name) {
                        return bad(format!("不认识的参数：{name}"));
                    }
                    if params.insert(name.to_owned(), micro(at(2)?)?).is_some() {
                        return bad(format!("重复的参数：{name}"));
                    }
                }
                other => return bad(format!("不认识的行：{other}")),
            }
        }
        let missing = |what: &str| ModelError(format!("缺 {what}"));
        if form.ok_or_else(|| missing("形式"))? != "F3" {
            return bad("形式只认 F3");
        }
        let p = |name: &str| {
            params
                .get(name)
                .copied()
                .ok_or_else(|| ModelError(format!("缺参数 {name}")))
        };
        let c = Coeffs {
            a_d: p("a_d")?,
            b_d: p("b_d")?,
            c_d: p("c_d")?,
            d_d: p("d_d")?,
            a_m: p("a_m")?,
            b_m: p("b_m")?,
            c_m: p("c_m")?,
            f: p("f")?,
            a_s: p("a_s")?,
            b_s: p("b_s")?,
            d_s: p("d_s")?,
            e: p("e")?,
            a_f: p("a_f")?,
            b_f: p("b_f")?,
        };
        let key = key.ok_or_else(|| missing("关键帧"))?;
        let sigma = sigma.ok_or_else(|| missing("残差标准差"))?;
        // 对 QP 单调的条件：每一项随 QP 增大不增；画质差的权重 2^(e·g) 随 g 不减（g 随 QP 变小）
        if c.b_d > 0 || c.b_m > 0 || c.b_s > 0 || c.b_f > 0 || key.b > 0 {
            return bad("系数会让字节数随 QP 增大");
        }
        if c.e < 0 || c.c_d < 0 || c.c_m < 0 || sigma < 0 || key.sigma < 0 {
            return bad("系数会让字节数随 QP 增大或残差标准差为负");
        }
        Ok(Self {
            head: head.ok_or_else(|| missing("帧头"))?,
            sigma,
            key,
            c,
            floor_max_qp: floor_max.ok_or_else(|| missing("地板最大QP"))?,
            scene_key_permille: scene.ok_or_else(|| missing("scene_key_permille"))?,
        })
    }

    /// 随包带的 openh264 模型（区域输入）。
    ///
    /// # Errors
    /// 文件有误（有单元测试守着，不会发生）。
    pub fn openh264_v2() -> Result<Self, ModelError> {
        Self::parse(OPENH264_V2)
    }

    /// 后端自己出关键帧的变化面积门槛（千分比）。
    #[must_use]
    pub const fn scene_key_permille(&self) -> u32 {
        self.scene_key_permille
    }
}

/// 按画面区域估字节数的模型（形式 F3）。
#[derive(Debug, Clone)]
pub struct RegionByteModel {
    table: RegionTable,
    noise: bool,
}

impl RegionByteModel {
    /// 用 `table`，打开残差扰动。
    #[must_use]
    pub const fn new(table: RegionTable) -> Self {
        Self { table, noise: true }
    }

    /// 随包带的 openh264 模型。
    ///
    /// # Errors
    /// 同 [`RegionTable::openh264_v2`]。
    pub fn openh264_v2() -> Result<Self, ModelError> {
        Ok(Self::new(RegionTable::openh264_v2()?))
    }

    /// 打开或关掉残差扰动。
    #[must_use]
    pub const fn with_noise(mut self, on: bool) -> Self {
        self.noise = on;
        self
    }

    /// 后端自己出关键帧的变化面积门槛（千分比）。
    #[must_use]
    pub const fn scene_key_permille(&self) -> u32 {
        self.table.scene_key_permille
    }

    /// 标准正态的近似（12 个均匀数之和减 6），Q16；只由帧的序号和形状决定，不看 QP（和 [`crate::fitted`] 同一个做法，
    /// 但扰动的种子另取，免得两个模型的“偶尔一个大帧”撞在同一帧上）。
    fn gauss_q16(input: &ModelInput) -> i128 {
        let mut state = u64::from(input.serial).wrapping_mul(0xd1b5_4a32_d192_ed03)
            ^ input.pixels.rotate_left(23)
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

    /// 关键帧：和 [`crate::fitted::FittedByteModel`] 的关键帧行同一个式子。
    fn key_body_q32(&self, input: &ModelInput, qp: i128) -> u128 {
        let r = &self.table.key;
        let x = r.a
            + r.b * qp
            + r.c * q16_to_micro(log2_q16(input.pixels)) / MICRO
            + r.d * q16_to_micro(log2_q16(u64::from(input.detail_permille.min(1000)))) / MICRO;
        exp2_q32(micro_to_q16(x))
    }

    /// 普通帧的正文（Q32）：D + M + S + 地板。
    fn normal_body_q32(&self, input: &ModelInput, qp: i128) -> u128 {
        let c = &self.table.c;
        let r = &input.regions;
        let log = |n: u64| q16_to_micro(log2_q16(n));
        let mut total: u128 = 0;
        // D：新内容
        if r.dirty_mbs > 0 {
            let area = u64::from(r.dirty_mbs) * MB_PIXELS;
            let detail = u64::from(r.dirty_detail_permille.clamp(1, 1000));
            let x = c.a_d + c.b_d * qp + c.c_d * log(area) / MICRO + c.d_d * log(detail) / MICRO;
            total += exp2_q32(micro_to_q16(x));
        }
        // M：搬来的（整屏移动再加 f，f 预计为负）
        if r.moved_mbs > 0 {
            let area = u64::from(r.moved_mbs) * MB_PIXELS;
            let full = if r.full_move { c.f } else { 0 };
            let x = c.a_m + c.b_m * qp + c.c_m * log(area) / MICRO + full;
            total += exp2_q32(micro_to_q16(x));
        }
        // S：静止的宏块补清晰，每个宏块按画质差 g 档计，2^(e·g) 倍
        let base = c.a_s
            + c.b_s * qp
            + c.d_s * log(u64::from(input.detail_permille.clamp(1, 1000))) / MICRO;
        for (g, n) in r.static_gap.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            let g = i128::try_from(g + 1).unwrap_or(0);
            let x = base + c.e * g;
            total += exp2_q32(micro_to_q16(x)) * u128::from(*n) * u128::from(MB_PIXELS);
        }
        // 地板：QP 小的时候，画面一点没动也要这么多（只和 QP 有关）
        if qp <= i128::from(self.table.floor_max_qp) {
            let still = input
                .pixels
                .saturating_sub(MB_PIXELS * u64::from(r.dirty_mbs.saturating_add(r.moved_mbs)));
            if still > 0 {
                let x = c.a_f + c.b_f * qp + log(still);
                total += exp2_q32(micro_to_q16(x));
            }
        }
        total
    }
}

/// Q16 的 log2 换成 10⁻⁶ 单位。
const fn q16_to_micro(v: i128) -> i128 {
    v * MICRO / ONE_Q16
}

/// 10⁻⁶ 单位换成 Q16。
const fn micro_to_q16(v: i128) -> i128 {
    v.saturating_mul(ONE_Q16).div_euclid(MICRO)
}

impl ByteModel for RegionByteModel {
    fn bytes(&self, input: &ModelInput) -> usize {
        let qp = i128::from(input.qp.min(MODEL_MAX_QP));
        let (head, sigma, mut total) = if input.key {
            (
                self.table.key.head,
                self.table.key.sigma,
                self.key_body_q32(input, qp),
            )
        } else {
            (
                self.table.head,
                self.table.sigma,
                self.normal_body_q32(input, qp),
            )
        };
        if self.noise {
            let x = sigma * Self::gauss_q16(input) / MICRO;
            total = (total.min(TOTAL_CAP_Q32) * exp2_q32(x)) >> 32;
        }
        let body = (total.min(TOTAL_CAP_Q32) + (1 << 31)) >> 32;
        usize::try_from(u128::from(head).saturating_add(body))
            .unwrap_or(usize::MAX)
            .max(1)
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
    use crate::fake::{Regions, GAP_BINS};

    /// 手写的一份小系数：数都取得好算（指数里的系数多是 0.5、1、−0.1 这种）。
    const SAMPLE: &str = "# 注释\n形式\tF3\n帧头\t13\n残差标准差\t0.5\n\
        关键帧\t40\t0.8\t-0.15\t1\t0.15\t0.25\n地板最大QP\t17\n\
        参数\ta_d\t2\n参数\tb_d\t-0.1\n参数\tc_d\t0.5\n参数\td_d\t0.25\n\
        参数\ta_m\t1\n参数\tb_m\t-0.1\n参数\tc_m\t0.5\n参数\tf\t-3\n\
        参数\ta_s\t-6\n参数\tb_s\t-0.05\n参数\td_s\t0.5\n参数\te\t0.125\n\
        参数\ta_f\t-8\n参数\tb_f\t-0.25\n\
        scene_key_permille\t700\n";

    fn table() -> RegionTable {
        RegionTable::parse(SAMPLE).unwrap()
    }

    fn model() -> RegionByteModel {
        RegionByteModel::new(table()).with_noise(false)
    }

    fn input() -> ModelInput {
        ModelInput {
            qp: 24,
            key: false,
            pixels: 1920 * 1080,
            changed_permille: 100,
            detail_permille: 120,
            prev_qp: Some(24),
            serial: 5,
            regions: Regions::default(),
        }
    }

    fn with(regions: Regions) -> ModelInput {
        ModelInput { regions, ..input() }
    }

    fn close(got: usize, want: f64) {
        let got = got as f64;
        // 正文四舍五入成整数字节，小帧上差不到 1 字节
        assert!((got - want).abs() <= 1.0 + 0.01 * want, "{got} 对 {want}");
    }

    /// 每一项单独拿出来对手算的数（用浮点公式），再看它们是相加的。
    #[test]
    fn each_part_matches_the_formula_and_they_add_up() {
        let m = model();
        // 只有新内容：40 个宏块，脏区域细节 200‰，QP 24，面积 10240
        let d = Regions {
            dirty_mbs: 40,
            dirty_detail_permille: 200,
            ..Regions::default()
        };
        let want_d = (2.0 - 0.1 * 24.0 + 0.5 * 10_240f64.log2() + 0.25 * 200f64.log2()).exp2();
        close(m.bytes(&with(d)), 13.0 + want_d);
        // 只有移动：100 个宏块；整屏移动时多乘 2^f
        let mv = Regions {
            moved_mbs: 100,
            ..Regions::default()
        };
        let want_m = (1.0 - 2.4 + 0.5 * 25_600f64.log2()).exp2();
        close(m.bytes(&with(mv)), 13.0 + want_m);
        let full = Regions {
            full_move: true,
            ..mv
        };
        close(m.bytes(&with(full)), 13.0 + want_m / 8.0);
        // 只有静止宏块补清晰：10 个比这一帧粗 4 档、2 个粗 20 档；全帧细节 120‰
        let mut gap = [0u32; GAP_BINS];
        gap[3] = 10;
        gap[19] = 2;
        let s = Regions {
            static_gap: gap,
            ..Regions::default()
        };
        let base = -6.0 - 0.05 * 24.0 + 0.5 * 120f64.log2();
        let want_s =
            256.0 * (10.0 * (base + 0.125 * 4.0).exp2() + 2.0 * (base + 0.125 * 20.0).exp2());
        close(m.bytes(&with(s)), 13.0 + want_s);
        // 地板：QP 24 没有；QP 12 有，像素数是整屏减去编码的宏块
        assert_eq!(m.bytes(&with(Regions::default())), 13);
        let floor12 = |regions| {
            m.bytes(&ModelInput {
                qp: 12,
                regions,
                ..input()
            })
        };
        let still: f64 = 1920.0 * 1080.0 - 256.0 * 40.0;
        let want_f = (-8.0 - 0.25 * 12.0 + still.log2()).exp2();
        let want_d12 = (2.0 - 0.1 * 12.0 + 0.5 * 10_240f64.log2() + 0.25 * 200f64.log2()).exp2();
        close(floor12(d), 13.0 + want_d12 + want_f);
        // 全部加在一起
        let all = Regions {
            dirty_mbs: 40,
            dirty_detail_permille: 200,
            moved_mbs: 100,
            static_gap: gap,
            ..Regions::default()
        };
        close(m.bytes(&with(all)), 13.0 + want_d + want_m + want_s);
    }

    /// 关键帧用关键帧行（总像素、全帧细节），不看区域。
    #[test]
    fn key_frames_use_the_key_row_and_ignore_regions() {
        let m = model();
        let key = ModelInput {
            key: true,
            ..input()
        };
        let want = 40.0 + (0.8 - 0.15 * 24.0 + 2_073_600f64.log2() + 0.15 * 120f64.log2()).exp2();
        close(m.bytes(&key), want);
        let busy = Regions {
            dirty_mbs: 10,
            moved_mbs: 10,
            ..Regions::default()
        };
        assert_eq!(
            m.bytes(&key),
            m.bytes(&ModelInput {
                regions: busy,
                ..key
            })
        );
    }

    #[test]
    fn bytes_never_grow_with_qp_even_with_noise() {
        let noisy = RegionByteModel::new(table());
        let mut gap = [0u32; GAP_BINS];
        gap[0] = 30;
        gap[10] = 500;
        gap[40] = 7;
        let busy = Regions {
            dirty_mbs: 400,
            moved_mbs: 900,
            dirty_detail_permille: 300,
            full_move: true,
            static_gap: gap,
            ..Regions::default()
        };
        for model in [noisy, model()] {
            for regions in [Regions::default(), busy] {
                for key in [false, true] {
                    let mut prev = usize::MAX;
                    for qp in 0..=60 {
                        // 画质差分布是对这一帧的 QP 算的：QP 越大，比它粗的宏块越少。这里用同一份分布，是更苛刻的检验
                        let b = model.bytes(&ModelInput {
                            qp,
                            key,
                            regions,
                            ..input()
                        });
                        assert!(b <= prev && b > 0, "QP {qp}：{b} 比 {prev} 多");
                        prev = b;
                    }
                }
            }
        }
    }

    #[test]
    fn noise_is_deterministic_log_normal_and_off_switchable() {
        let m = RegionByteModel::new(table());
        let quiet = model();
        let busy = with(Regions {
            dirty_mbs: 40,
            dirty_detail_permille: 200,
            ..Regions::default()
        });
        assert_eq!(m.bytes(&busy), m.bytes(&busy), "同输入同输出");
        // σ = 0.5：正文远大于帧头，2000 帧对数偏差的标准差约 0.5、均值约 0
        let base = quiet.bytes(&busy) as f64;
        let logs: Vec<f64> = (0..2000u32)
            .map(|serial| (m.bytes(&ModelInput { serial, ..busy }) as f64 / base).log2())
            .collect();
        let mean = logs.iter().sum::<f64>() / logs.len() as f64;
        let var = logs.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / logs.len() as f64;
        assert!(mean.abs() < 0.08, "均值 {mean}");
        assert!((var.sqrt() - 0.5).abs() < 0.06, "标准差 {}", var.sqrt());
        assert_ne!(
            m.bytes(&ModelInput { serial: 3, ..busy }),
            m.bytes(&ModelInput { serial: 4, ..busy })
        );
        assert_eq!(
            quiet.bytes(&ModelInput { serial: 3, ..busy }),
            quiet.bytes(&ModelInput { serial: 4, ..busy })
        );
    }

    #[test]
    fn extreme_inputs_do_not_overflow() {
        let m = RegionByteModel::new(table());
        let huge = ModelInput {
            qp: 0,
            pixels: u64::MAX,
            regions: Regions {
                dirty_px: u64::MAX,
                moved_px: u64::MAX,
                dirty_mbs: u32::MAX,
                moved_mbs: u32::MAX,
                dirty_detail_permille: u32::MAX,
                full_move: true,
                static_gap: [u32::MAX; GAP_BINS],
                repeat: 255,
            },
            ..input()
        };
        assert!(m.bytes(&huge) > 1_000_000);
        for key in [false, true] {
            assert!(m.bytes(&ModelInput { key, ..huge }) > 0);
        }
    }

    #[test]
    fn parse_rejects_broken_files() {
        assert_eq!(table().scene_key_permille(), 700);
        assert_eq!(model().scene_key_permille(), 700);
        let cases = [
            SAMPLE.replace("形式\tF3\n", ""),
            SAMPLE.replace("形式\tF3", "形式\tF9"),
            SAMPLE.replace("帧头\t13\n", ""),
            SAMPLE.replace("帧头\t13", "帧头\tx"),
            SAMPLE.replace("帧头\t13", "帧头"),
            SAMPLE.replace("残差标准差\t0.5\n", ""),
            SAMPLE.replace("残差标准差\t0.5", "残差标准差\t-0.5"),
            SAMPLE.replace("关键帧\t40\t0.8\t-0.15\t1\t0.15\t0.25\n", ""),
            SAMPLE.replace("-0.15\t1\t0.15", "0.15\t1\t0.15"),
            SAMPLE.replace("0.15\t0.25\n", "0.15\t-0.25\n"),
            SAMPLE.replace("地板最大QP\t17\n", ""),
            SAMPLE.replace("地板最大QP\t17", "地板最大QP\t999"),
            SAMPLE.replace("scene_key_permille\t700\n", ""),
            SAMPLE.replace("scene_key_permille\t700", "scene_key_permille\t99999999999"),
            SAMPLE.replace("参数\ta_d\t2\n", ""),
            SAMPLE.replace("参数\ta_d\t2\n", "参数\ta_d\t2\n参数\ta_d\t2\n"),
            SAMPLE.replace("参数\ta_d\t2", "参数\tz_z\t2"),
            SAMPLE.replace("参数\tb_d\t-0.1", "参数\tb_d\t0.1"),
            SAMPLE.replace("参数\tb_m\t-0.1", "参数\tb_m\t0.1"),
            SAMPLE.replace("参数\tb_s\t-0.05", "参数\tb_s\t0.05"),
            SAMPLE.replace("参数\tb_f\t-0.25", "参数\tb_f\t0.25"),
            SAMPLE.replace("参数\te\t0.125", "参数\te\t-0.125"),
            SAMPLE.replace("参数\tc_d\t0.5", "参数\tc_d\t-0.5"),
            SAMPLE.replace("参数\ta_d\t2", "参数\ta_d\tx"),
            SAMPLE.replace("参数\ta_d\t2", "参数\ta_d"),
            format!("{SAMPLE}乱写\t1\n"),
        ];
        for text in cases {
            assert!(RegionTable::parse(&text).is_err(), "{text}");
        }
    }

    /// 定点运算和用浮点按公式算的结果对得上：随包带的文件里的系数，用浮点把 F3 的式子再写一遍（只在测试里），
    /// 拿一批形状不同的输入比，差在 1% 加 1 字节以内。
    #[test]
    fn the_fixed_point_model_matches_the_float_formula() {
        let text = OPENH264_V2;
        let num = |name: &str| -> f64 {
            text.lines()
                .filter_map(|l| l.strip_prefix(&format!("参数\t{name}\t")))
                .next()
                .unwrap()
                .parse()
                .unwrap()
        };
        let head = 13.0;
        let reference = |qp: u8, r: &Regions, pixels: u64, detail: u32| -> f64 {
            let q = f64::from(qp);
            let mut total = 0.0;
            if r.dirty_mbs > 0 {
                let area = f64::from(r.dirty_mbs) * 256.0;
                let dd = f64::from(r.dirty_detail_permille.clamp(1, 1000));
                total += (num("a_d")
                    + num("b_d") * q
                    + num("c_d") * area.log2()
                    + num("d_d") * dd.log2())
                .exp2();
            }
            if r.moved_mbs > 0 {
                let area = f64::from(r.moved_mbs) * 256.0;
                let full = if r.full_move { num("f") } else { 0.0 };
                total += (num("a_m") + num("b_m") * q + num("c_m") * area.log2() + full).exp2();
            }
            let base =
                num("a_s") + num("b_s") * q + num("d_s") * f64::from(detail.clamp(1, 1000)).log2();
            for (g, n) in r.static_gap.iter().enumerate() {
                total += f64::from(*n) * 256.0 * (base + num("e") * (g as f64 + 1.0)).exp2();
            }
            if qp <= 17 {
                let still = pixels as f64 - 256.0 * f64::from(r.dirty_mbs + r.moved_mbs);
                total += (num("a_f") + num("b_f") * q + still.log2()).exp2();
            }
            head + total
        };
        let m = RegionByteModel::openh264_v2().unwrap().with_noise(false);
        let mut gap = [0u32; GAP_BINS];
        gap[0] = 8000;
        gap[11] = 300;
        gap[29] = 20;
        let cases = [
            Regions::default(),
            Regions {
                dirty_mbs: 1,
                dirty_detail_permille: 200,
                ..Regions::default()
            },
            Regions {
                dirty_mbs: 3600,
                dirty_detail_permille: 140,
                ..Regions::default()
            },
            Regions {
                dirty_mbs: 36,
                moved_mbs: 1018,
                dirty_detail_permille: 250,
                ..Regions::default()
            },
            Regions {
                dirty_mbs: 100,
                moved_mbs: 7900,
                dirty_detail_permille: 90,
                full_move: true,
                ..Regions::default()
            },
            Regions {
                static_gap: gap,
                ..Regions::default()
            },
            Regions {
                dirty_mbs: 400,
                moved_mbs: 900,
                dirty_detail_permille: 300,
                static_gap: gap,
                ..Regions::default()
            },
        ];
        for r in &cases {
            for qp in [12u8, 17, 18, 24, 30, 40, 51] {
                let input = ModelInput {
                    qp,
                    regions: *r,
                    ..input()
                };
                let want = reference(qp, r, input.pixels, input.detail_permille);
                let got = m.bytes(&input) as f64;
                assert!(
                    (got - want).abs() <= 1.0 + 0.01 * want,
                    "QP {qp} {r:?}：{got} 对 {want}"
                );
            }
        }
    }

    /// 随包带的文件能读；形状对：关键帧远大于普通帧，QP 每加 6 字节数差不多减半，补清晰按画质差越大越大。
    #[test]
    fn the_shipped_model_file_loads_and_has_the_right_shape() {
        let table = RegionTable::openh264_v2().unwrap();
        assert!((100..=1001).contains(&table.scene_key_permille()));
        let m = RegionByteModel::openh264_v2().unwrap().with_noise(false);
        assert_eq!(m.scene_key_permille(), table.scene_key_permille());
        let busy = Regions {
            dirty_mbs: 600,
            moved_mbs: 300,
            dirty_detail_permille: 150,
            ..Regions::default()
        };
        let normal = ModelInput {
            regions: busy,
            ..input()
        };
        let key = m.bytes(&ModelInput {
            key: true,
            ..normal
        });
        assert!(key > m.bytes(&normal));
        let (lo, hi) = (
            m.bytes(&ModelInput { qp: 28, ..normal }),
            m.bytes(&ModelInput { qp: 34, ..normal }),
        );
        assert!(hi < lo && hi * 4 > lo, "{lo} → {hi}");
        // 补清晰：画质差越大越大
        let gap = |g: usize| {
            let mut a = [0u32; GAP_BINS];
            a[g - 1] = 8000;
            m.bytes(&with(Regions {
                static_gap: a,
                ..Regions::default()
            }))
        };
        assert!(gap(20) > gap(10) && gap(10) > gap(2));
    }
}
