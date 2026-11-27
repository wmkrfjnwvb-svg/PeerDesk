//! 软件编码（openh264）的逐帧 QP 控制：编码器缓冲约一帧（`docs/research/L-framework.md` T6）。
//!
//! 为什么不用 openh264 自己的码控（2026-11 对照源码和实测）：
//! - 屏幕内容模式下第一帧关键帧的 QP 按每像素比特查表（720p 低码率固定 34，最高 37），不跟目标码率，
//!   720p 桌面关键帧 92 KB，340 kbps 的链路上要 2 秒多。
//! - P 帧的基准 QP 被夹在 `[iMinQp, iMinQp]`（`WelRcPictureInitScc`），只在“场景大变 + 缓冲满”时才往上加；
//!   粗编一帧之后码控的缓冲状态里没有那一帧，下一帧按 QP 12 把整幅画面重编一遍（实测 46.8 KB）。
//! - 运行中改 QP 范围要重建编码器（必出关键帧）。
//!
//! 所以软件编码关掉 openh264 的码控（`RC_OFF_MODE`），每帧由这里定 QP（`iDLayerQp` 运行中可改、不重建）：
//! - **关键帧**：按“每百万像素多少字节”的估计和“大约每 7 个 QP 大小减半”，取装得进时限的最低 QP
//!   （大帧规则，照 Chromium 远程桌面 `ShouldDropQualityForLargeFrame`：发不完就粗编，之后补清晰）。
//! - **画面在变**：按上一帧实际大小和目标比，大了快加（每帧最多 +10）、小了慢减（每帧最多 −2），
//!   和“一帧缓冲”的码控一样（Sunshine 的 NVENC/FFmpeg 路径 `vbvBufferSize = 码率 / 帧率`）。
//! - **静止后第一帧变化**：按变化面积估计这一帧多大（整屏变化约等于关键帧的 0.8），和关键帧一样装进时限
//!   （Chromium 按“更新面积”判断大帧）。不知道面积时用上一次画面在变时的 QP，不用补清晰补到的低 QP
//!   （否则整屏切换按 QP 12 编，实测 49 KB；高熵内容甚至超出 openh264 的码流缓冲、编码失败）。
//! - **画面停下（补清晰）**：每帧把 QP 降一点，降多少按估计让这一帧的补充量不超过一帧的预算，一直补到最小 QP
//!   （Chromium：补到量化值最小为止）；但降一步的量超过链路约 200 ms 能传的（大帧时限的 2 倍）就停：窄带上
//!   低 QP 降一步就是几十 KB，传几秒，用户一动新画面就排在它后面。

/// 最小 QP：补清晰补到这里为止（再小几乎看不出区别，白费流量）。
pub const MIN_QP: u8 = 12;
/// 最大 QP。
pub const MAX_QP: u8 = 51;
/// 估计大小时的基准 QP：实测 720p 办公桌面关键帧在 QP 34 时约 100 KB / 百万像素。
const BASE_QP: u8 = 34;
/// 关键帧大小的初始估计（QP 34 时每百万像素多少字节）：实测 720p 桌面 92.5 KB（0.92 百万像素），
/// 和 Chromium 远程桌面的 `kEstimatedBytesPerMegapixel`（100 KB）一致。
pub const KEY_BYTES_PER_MEGAPIXEL: u64 = 100_000;
/// QP 每减 1 帧大约变大多少（千分比）：实测 QP 34→51 大小约每 7 个 QP 减半，2^(1/7) ≈ 1.104。
const QP_STEP_PERMILLE: u64 = 1104;
/// 画面在变时 QP 每帧最多加多少、减多少。
const MAX_UP: i32 = 10;
const MAX_DOWN: i32 = 2;
/// 关键帧最低 QP：再低关键帧大得不成比例，清晰度靠之后补。
const KEY_MIN_QP: u8 = 22;
/// 整幅画面都变了的增量帧大约是同 QP 关键帧的多少（千分比）：实测整屏切换的增量帧是关键帧的
/// 0.68–0.86（QP 34–51），取 0.8。
const CHANGE_OF_KEY_PERMILLE: u64 = 800;
/// 画面变了这么多（千分比）以上算整屏变化（编出来远超时限时重编成关键帧，见 [`QpControl::overshoot_qp`]）。
pub const LARGE_CHANGE_PERMILLE: u64 = 500;
/// 整屏变化编出来超过大帧时限这么多倍就重编（见 [`QpControl::overshoot_qp`]）。
const OVERSHOOT_TIMES: u64 = 2;
/// 画面一直在变时，变化面积比上一帧大出这么多（上一帧的 [`RENEWAL_TIMES`] 倍再加这么多千分比），算“突然换了一块”
/// （打字、拖小块之后切换窗口、打开对话框），按面积估计装进大帧时限，不封顶在 [`KEY_MIN_QP`]。
const RENEWAL_MIN_PERMILLE: u64 = 100;
const RENEWAL_TIMES: u64 = 4;
/// 补清晰时一帧的补充量最多是预算的几倍：1 倍，补清晰正好用满链路；再多会占满在途上限，画面一动新帧就得排在补清晰的帧后面。
const REFINE_BUDGET_FACTOR: u64 = 1;
/// 补清晰降一个 QP 的估计补充量超过大帧时限的这么多倍就不再降（停在当前清晰度）。
/// 以前预算再小也“至少降 1”：QP 越低降一步越贵（整幅画面重编，降一步约多出关键帧的十分之一），窄带上
/// 一帧补清晰编出 35–40 KB（107–161 kbps 的链路要传两三秒，2026-11 微软轨迹实测），这时用户一动，新画面就排在它后面。
pub const REFINE_STEP_KEY_TIMES: u64 = 2;

/// 这一帧是哪种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 关键帧。
    Key,
    /// 画面变了的增量帧。
    Change,
    /// 画面没变、补清晰的增量帧。
    Refine,
}

/// 逐帧 QP 控制。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QpControl {
    /// 当前画面大致的 QP（上一帧用的）。
    qp: Option<u8>,
    /// 上一帧：(字节数, QP, 种类)。
    last: Option<(u64, u8, Kind)>,
    /// 关键帧大小的估计（QP 34 时每百万像素多少字节），每出一个关键帧更新。
    key_density: u64,
    /// 上一次画面在变时用的 QP。
    change_qp: Option<u8>,
    /// 上一帧在变的增量帧变了多少（加权千分比，不知道为 `None`）：判断这一帧是不是“突然换了一块”。
    change_area: Option<u64>,
    max_qp: u8,
}

impl Default for QpControl {
    fn default() -> Self {
        Self::new(MAX_QP)
    }
}

/// `bytes` 在 QP `from` 时的大小，换算到 QP `to` 时大约多大（每 7 个 QP 减半）。
fn rescale(bytes: u64, from: u8, to: u8) -> u64 {
    let mut b = bytes;
    if to < from {
        for _ in to..from {
            b = b.saturating_mul(QP_STEP_PERMILLE) / 1000;
        }
    } else {
        for _ in from..to {
            b = b.saturating_mul(1000) / QP_STEP_PERMILLE;
        }
    }
    b
}

impl QpControl {
    /// 创建，QP 不超过 `max_qp`（夹在 [`MIN_QP`, `MAX_QP`]）。
    #[must_use]
    pub fn new(max_qp: u8) -> Self {
        Self {
            qp: None,
            last: None,
            key_density: KEY_BYTES_PER_MEGAPIXEL,
            change_qp: None,
            change_area: None,
            max_qp: max_qp.clamp(MIN_QP, MAX_QP),
        }
    }

    /// 当前画面的 QP（还没编过为 `None`）。
    #[must_use]
    pub const fn qp(&self) -> Option<u8> {
        self.qp
    }

    /// 补清晰补完了没有（QP 到了最小值）。
    #[must_use]
    pub fn converged(&self) -> bool {
        self.qp.is_some_and(|q| q <= MIN_QP)
    }

    /// `pixels` 个像素的关键帧在 QP `qp` 时估计多大。
    #[must_use]
    pub fn key_bytes(&self, pixels: u64, qp: u8) -> u64 {
        rescale(
            self.key_density.saturating_mul(pixels) / 1_000_000,
            BASE_QP,
            qp,
        )
    }

    /// 装得进 `allowed` 字节的最低 QP（不低于 `floor`）：`bytes_at(q)` 估计 QP 为 q 时多大。
    fn fit(&self, floor: u8, allowed: u64, bytes_at: impl Fn(u8) -> u64) -> u8 {
        let mut q = floor.max(MIN_QP);
        while q < self.max_qp && bytes_at(q) > allowed.max(1) {
            q += 1;
        }
        q
    }

    /// 从当前 QP `now` 往下补清晰，按预算 `budget` 字节一帧能补到多少：补充量约等于“整幅画面从 now 到 q
    /// 多出来的大小”，至少降 1——除非降一步的补充量就超过 `step_cap`（链路一次装不下这么大的一帧，停在 now）。
    fn refine_floor(&self, now: u8, budget: u64, pixels: u64, step_cap: u64) -> u8 {
        let mut q = now;
        while q > MIN_QP {
            let more = self
                .key_bytes(pixels, q - 1)
                .saturating_sub(self.key_bytes(pixels, now));
            if more > step_cap || more > budget && q < now {
                break;
            }
            q -= 1;
            if more > budget {
                break;
            }
        }
        q
    }

    /// 这一帧用的 QP。`target` 是这一帧的目标字节数；`key_allowed` 是大帧最多多大（大帧规则的时限
    /// 按目标码率换算的字节数）；`pixels` 是画面像素数；`changed_permille` 是画面变了多少（千分比，不知道为
    /// `None`）。
    #[must_use]
    pub fn next(
        &self,
        kind: Kind,
        target: u64,
        key_allowed: u64,
        pixels: u64,
        changed_permille: Option<u64>,
    ) -> u8 {
        let target = target.max(1);
        let qp = match kind {
            Kind::Key => self.fit(KEY_MIN_QP, key_allowed, |q| self.key_bytes(pixels, q)),
            Kind::Change => {
                let now = self.qp.unwrap_or(BASE_QP);
                // 变化的这块区域整个换成新内容（不能靠运动补偿）时，QP 为 q 估计多大
                let renewed = |area: u64, q: u8| {
                    self.key_bytes(pixels, q)
                        .saturating_mul(area.min(1000))
                        .saturating_mul(CHANGE_OF_KEY_PERMILLE)
                        / 1_000_000
                };
                let q = match self.last {
                    // 上一帧也是在变的增量帧：按它的实际大小和目标比修正
                    Some((bytes, q, Kind::Change)) => {
                        let mut delta = 0i32;
                        let mut b = bytes.max(1);
                        while b > target && delta < MAX_UP {
                            b = b.saturating_mul(1000) / QP_STEP_PERMILLE;
                            delta += 1;
                        }
                        while b.saturating_mul(QP_STEP_PERMILLE) / 1000 <= target
                            && delta > -MAX_DOWN
                        {
                            b = b.saturating_mul(QP_STEP_PERMILLE) / 1000;
                            delta -= 1;
                        }
                        let q = i32::from(q) + delta;
                        let q = u8::try_from(q.clamp(i32::from(MIN_QP), i32::from(self.max_qp)))
                            .unwrap_or(self.max_qp);
                        // 变化面积比上一帧突然大很多（打字之后切换窗口、打开对话框）：这块区域是整个换成新内容，
                        // 上一帧的大小推算不出它，按面积估计装进大帧时限，和静止后第一帧变化一样不封顶。
                        // 2026-11-27：打字时码率 13 Mbps、QP 12，静止中链路降到 2 Mbps，切换窗口（加权面积 38%）
                        // 按下面封顶的规则只提到 QP 22，编出 146 KB（大帧时限 9 KB 的 16 倍），2 Mbps 上传了 600 ms。
                        // 降到 KEY_MIN_QP 以下时：这块区域突然整个换掉（拖动窗口之后马上切换窗口）也要装得进
                        // 大帧时限，最多提到 KEY_MIN_QP。不按面积估计一直在变的帧：滚动、视频靠运动补偿，
                        // 比按面积估计的小得多（实测窄带上逐帧按面积估计，视频和拖动之后的画面明显更糊）
                        match changed_permille {
                            Some(area) if self.renewal(area) => q.max(self.fit(
                                if area >= LARGE_CHANGE_PERMILLE {
                                    KEY_MIN_QP
                                } else {
                                    MIN_QP
                                },
                                key_allowed,
                                |x| renewed(area, x),
                            )),
                            Some(area) if q < KEY_MIN_QP => q.max(
                                self.fit(MIN_QP, key_allowed, |x| renewed(area, x))
                                    .min(KEY_MIN_QP),
                            ),
                            Some(_) | None => q,
                        }
                    }
                    // 静止后第一帧变化：按变化面积估计大小，装进大帧时限（大面积和关键帧一样不低于 KEY_MIN_QP：
                    // 高熵画面按 QP 12 整屏编会超出 openh264 的码流缓冲）；不知道面积时用上一次在变时的 QP
                    Some(_) | None => match changed_permille {
                        Some(area) => self.fit(
                            if area >= LARGE_CHANGE_PERMILLE {
                                KEY_MIN_QP
                            } else {
                                MIN_QP
                            },
                            key_allowed,
                            |x| renewed(area, x),
                        ),
                        None => self.change_qp.unwrap_or(now).max(now),
                    },
                };
                // 比当前画面的 QP 低：静止的部分也会跟着重编（等于补清晰），只能低到预算补得动的程度
                q.max(self.refine_floor(now, target, pixels, u64::MAX))
            }
            Kind::Refine => self.refine_floor(
                self.qp.unwrap_or(BASE_QP),
                target.saturating_mul(REFINE_BUDGET_FACTOR),
                pixels,
                key_allowed.saturating_mul(REFINE_STEP_KEY_TIMES),
            ),
        };
        qp.clamp(MIN_QP, self.max_qp)
    }

    /// 一帧整屏变化编出来 `bytes` 字节（用的 QP 是 `qp`），超过大帧时限 `allowed` 的 [`OVERSHOOT_TIMES`] 倍时，
    /// 按它的实际大小推算装得进时限的 QP（不低于关键帧的下限）；不用重编（没超，或者 QP 已经到顶）时为 `None`。
    /// 照 libvpx 屏幕内容实时模式（`vp9_encodedframe_overshoot`）：场景切换超出太多，用更高的 QP 重编这一帧。
    #[must_use]
    pub fn overshoot_qp(&self, bytes: u64, qp: u8, allowed: u64) -> Option<u8> {
        if bytes <= allowed.saturating_mul(OVERSHOOT_TIMES) || qp >= self.max_qp {
            return None;
        }
        Some(
            self.fit(KEY_MIN_QP.max(qp.saturating_add(1)), allowed, |q| {
                rescale(bytes, qp, q)
            }),
        )
    }

    /// 画面一直在变时，这一帧变化的面积 `area`（加权千分比）比上一帧大很多：这块区域是突然整个换掉的。
    /// 上一帧的面积不知道时不算（按原来的规则）。
    fn renewal(&self, area: u64) -> bool {
        self.change_area.is_some_and(|last| {
            area >= last
                .saturating_mul(RENEWAL_TIMES)
                .saturating_add(RENEWAL_MIN_PERMILLE)
        })
    }

    /// 编完一帧：`bytes` 字节、用的 QP `qp`、`pixels` 个像素，画面变了多少 `area`（加权千分比，不知道为 `None`）。
    pub fn on_frame(&mut self, kind: Kind, bytes: u64, qp: u8, pixels: u64, area: Option<u64>) {
        if kind == Kind::Key {
            // 换算回 QP 34 的“每百万像素多少字节”，下一个关键帧按它估计
            let density = bytes.saturating_mul(1_000_000) / pixels.max(1);
            self.key_density = rescale(density, qp, BASE_QP).max(1);
        }
        // 补清晰的帧不会把画面变糊：补完的画面质量取两者的好的那个
        self.qp = Some(match (kind, self.qp) {
            (Kind::Refine, Some(now)) => now.min(qp),
            _ => qp,
        });
        if kind == Kind::Change {
            self.change_qp = Some(qp);
            self.change_area = area;
        }
        self.last = Some((bytes, qp, kind));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P720: u64 = 1280 * 720;

    #[test]
    fn keyframes_fit_the_time_limit() {
        let c = QpControl::new(MAX_QP);
        // 估计：QP 34 时 92 KB
        assert_eq!(c.key_bytes(P720, BASE_QP), 92_160);
        // 时限内能发 100 KB：不用粗编，取最低的 22（下限）以上能装下的
        let rich = c.next(Kind::Key, 3_000, 10_000_000, P720, None);
        assert_eq!(rich, KEY_MIN_QP);
        // 只能发 10 KB：要粗编到 51 附近
        let poor = c.next(Kind::Key, 1_000, 10_000, P720, None);
        assert!(poor >= 45, "{poor}");
        assert!(c.key_bytes(P720, poor) <= 10_000 || poor == MAX_QP);
        // 上限
        assert_eq!(QpControl::new(40).next(Kind::Key, 1, 1, P720, None), 40);
    }

    #[test]
    fn changing_frames_track_the_target_and_learn_keyframe_size() {
        let mut c = QpControl::new(MAX_QP);
        // 关键帧实际只有估计的一半：下一次估计跟着变小
        c.on_frame(Kind::Key, 46_000, BASE_QP, P720, None);
        assert!(c.key_bytes(P720, BASE_QP) < 60_000);
        // 刚出过关键帧：沿用它的 QP
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), BASE_QP);
        // 一帧大了 8 倍：QP 加约 21，但每帧最多 +10
        c.on_frame(Kind::Change, 16_000, 30, P720, None);
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), 40);
        // 一帧比目标小很多：每帧最多 −2；但比当前画面的 QP 低就等于补清晰，只能低到预算补得动的程度（这里 1）
        c.on_frame(Kind::Change, 100, 40, P720, None);
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), 39);
        assert_eq!(c.next(Kind::Change, 1_000_000, 0, P720, None), 38);
        // 正好：不动
        c.on_frame(Kind::Change, 2_000, 39, P720, None);
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), 39);
        // 夹在范围里
        c.on_frame(Kind::Change, 1, MIN_QP, P720, None);
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), MIN_QP);
    }

    #[test]
    fn refine_steps_down_within_budget_until_converged() {
        let mut c = QpControl::new(MAX_QP);
        c.on_frame(Kind::Key, 14_000, 51, P720, None);
        assert!(!c.converged());
        // 预算很小：每帧至少降 1；预算大：一次降很多
        let small = c.next(Kind::Refine, 500, u64::MAX, P720, None);
        assert_eq!(small, 50);
        let big = c.next(Kind::Refine, 1_000_000, u64::MAX, P720, None);
        assert_eq!(big, MIN_QP);
        // 一直补：QP 单调降到最小
        let mut last = 51;
        for _ in 0..200 {
            let q = c.next(Kind::Refine, 2_000, u64::MAX, P720, None);
            assert!(q <= last);
            c.on_frame(Kind::Refine, 2_000, q, P720, None);
            last = q;
            if c.converged() {
                break;
            }
        }
        assert!(c.converged());
        // 补清晰的帧不会让画面“变糊”：用了更高的 QP 也记更好的那个
        c.on_frame(Kind::Refine, 10, 30, P720, None);
        assert_eq!(c.qp(), Some(MIN_QP));
        // 补完之后画面突然整屏变化：按面积估计、装进大帧时限，不按补到的 QP 12 编
        let whole = c.next(Kind::Change, 1_000, 10_000, P720, Some(1000));
        assert!(whole >= 40, "{whole}");
        // 只变了一小块（打字）：低 QP 也装得下
        assert_eq!(c.next(Kind::Change, 1_000, 10_000, P720, Some(2)), MIN_QP);
        // 不知道面积：用上一次在变时的 QP（这里还没有过，用当前的）
        assert_eq!(c.next(Kind::Change, 1_000, 10_000, P720, None), MIN_QP);
        c.on_frame(Kind::Change, 1_000, 33, P720, None);
        c.on_frame(Kind::Refine, 10, 20, P720, None);
        assert_eq!(c.next(Kind::Change, 1_000, 10_000, P720, None), 33);
        assert_eq!(QpControl::default().qp(), None);
    }

    #[test]
    fn refine_stops_where_one_step_would_take_the_link_too_long() {
        let mut c = QpControl::new(MAX_QP);
        c.on_frame(Kind::Key, 14_000, 40, P720, None);
        // 窄带（大帧时限 500 字节，约 40 kbps）：降一步要多出约 1.5 KB，链路一次装不下，停在 40
        let step = c.key_bytes(P720, 39) - c.key_bytes(P720, 40);
        assert!(step > 500 * REFINE_STEP_KEY_TIMES, "{step}");
        assert_eq!(c.next(Kind::Refine, 100, 500, P720, None), 40);
        // 时限够装下这一步：照常至少降 1
        assert_eq!(c.next(Kind::Refine, 500, step, P720, None), 39);
        // 宽带：一直补到最小 QP
        for _ in 0..100 {
            let q = c.next(Kind::Refine, 20_000, 1_000_000, P720, None);
            c.on_frame(Kind::Refine, 20_000, q, P720, None);
        }
        assert!(c.converged());
    }

    #[test]
    fn low_qp_while_changing_only_if_the_changing_region_could_be_renewed_in_time() {
        // 拖动窗口：变化 20%，帧很小，按上一帧推算要降到 12
        let drag = |allowed, area| {
            let mut c = QpControl::new(MAX_QP);
            c.on_frame(Kind::Change, 500, 14, P720, None);
            c.next(Kind::Change, 15_000, allowed, P720, area)
        };
        let drag = |allowed| drag(allowed, Some(200));
        // 窄一些的链路（时限 46 KB）：这块区域突然整个换掉按 QP 12 要 120 KB 以上，提到 KEY_MIN_QP
        assert_eq!(drag(46_000), KEY_MIN_QP);
        // 宽的链路（时限 1 MB）：装得下，和不看面积一样往下降
        let mut c = QpControl::new(MAX_QP);
        c.on_frame(Kind::Change, 500, 14, P720, None);
        let free = c.next(Kind::Change, 15_000, 1_000_000, P720, None);
        assert!(free < 14);
        assert_eq!(drag(1_000_000), free);
        // 中间：提到刚好装得下的 QP
        let mid = drag(200_000);
        assert!(mid > MIN_QP && mid < KEY_MIN_QP, "{mid}");
        // QP 本来就不低于 KEY_MIN_QP（窄带）：不管面积，按上一帧推算
        let mut c = QpControl::new(MAX_QP);
        c.on_frame(Kind::Change, 15_000, 26, P720, None);
        assert_eq!(c.next(Kind::Change, 20_000, 1_000, P720, Some(1000)), 25);
    }

    #[test]
    fn a_region_suddenly_renewed_while_changing_fits_the_limit_without_the_cap() {
        // 2026-11-27 drop-while-idle：打字（变化不到 1‰、QP 12、1.5 KB）之后切换窗口（加权面积 381‰），
        // 时限 9 KB。以前按上一帧推算到 12、面积保护最多提到 KEY_MIN_QP，编出 146 KB
        let after = |last_area| {
            let mut c = QpControl::new(MAX_QP);
            c.on_frame(Kind::Key, 147_000, 26, P720, None);
            c.on_frame(Kind::Change, 1_500, MIN_QP, P720, last_area);
            c.next(Kind::Change, 3_000, 9_000, P720, Some(381))
        };
        let renewed = after(Some(0));
        assert!(renewed > KEY_MIN_QP + 10, "{renewed}");
        // 估计装得进时限
        let mut c = QpControl::new(MAX_QP);
        c.on_frame(Kind::Key, 147_000, 26, P720, None);
        let est = c.key_bytes(P720, renewed) * 381 * CHANGE_OF_KEY_PERMILLE / 1_000_000;
        assert!(est <= 9_000, "{est}");
        // 上一帧面积不知道：按原来的规则（封顶 KEY_MIN_QP）
        assert_eq!(after(None), KEY_MIN_QP);
        // 一直在变、面积差不多（拖动、滚动）：不算突然换了一块，按原来的规则
        assert_eq!(after(Some(300)), KEY_MIN_QP);
        // 整屏换掉（面积过半）：下限按关键帧
        let mut c = QpControl::new(MAX_QP);
        c.on_frame(Kind::Change, 1_500, MIN_QP, P720, Some(0));
        assert!(c.next(Kind::Change, 3_000, 10_000_000, P720, Some(900)) >= KEY_MIN_QP);
    }

    #[test]
    fn a_scene_change_far_over_the_limit_is_re_encoded_at_a_qp_from_its_real_size() {
        let c = QpControl::new(MAX_QP);
        // 拖动之后切换整个窗口：按 QP 12 编出 200 KB，时限 75 KB 的 2 倍都超了 → 按实际大小推 QP
        let q = c.overshoot_qp(200_000, MIN_QP, 75_000).unwrap();
        assert!(
            q >= KEY_MIN_QP && rescale(200_000, MIN_QP, q) <= 75_000,
            "{q}"
        );
        assert!(rescale(200_000, MIN_QP, q - 1) > 75_000 || q == KEY_MIN_QP);
        // 超得不多：不重编
        assert_eq!(c.overshoot_qp(140_000, MIN_QP, 75_000), None);
        // QP 已经到顶：重编也小不了
        assert_eq!(c.overshoot_qp(200_000, MAX_QP, 1_000), None);
        // 已经很高的 QP：至少再高 1
        assert_eq!(c.overshoot_qp(1_000_000, 50, 1), Some(MAX_QP));
    }
}
