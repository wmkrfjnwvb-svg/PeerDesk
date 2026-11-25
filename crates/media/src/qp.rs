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
//!   （Chromium：补到量化值最小为止）。

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
/// 画面变了这么多（千分比）以上算大面积变化：和关键帧一样，QP 不低于 [`KEY_MIN_QP`]。
const LARGE_CHANGE_PERMILLE: u64 = 500;
/// 补清晰时一帧的补充量最多是预算的几倍：1 倍，补清晰正好用满链路；再多会占满在途上限，画面一动新帧就得排在补清晰的帧后面。
const REFINE_BUDGET_FACTOR: u64 = 1;

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
    /// 多出来的大小”，至少降 1。
    fn refine_floor(&self, now: u8, budget: u64, pixels: u64) -> u8 {
        let mut q = now;
        while q > MIN_QP {
            let more = self
                .key_bytes(pixels, q - 1)
                .saturating_sub(self.key_bytes(pixels, now));
            if more > budget && q < now {
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
                        u8::try_from(q.clamp(i32::from(MIN_QP), i32::from(self.max_qp)))
                            .unwrap_or(self.max_qp)
                    }
                    // 静止后第一帧变化：按变化面积估计大小，装进大帧时限（大面积变化和关键帧一样，QP 不低于
                    // 关键帧的下限）；不知道面积时用上一次在变时的 QP
                    Some(_) | None => match changed_permille {
                        Some(area) => self.fit(
                            if area >= LARGE_CHANGE_PERMILLE {
                                KEY_MIN_QP
                            } else {
                                MIN_QP
                            },
                            key_allowed,
                            |q| {
                                self.key_bytes(pixels, q)
                                    .saturating_mul(area.min(1000))
                                    .saturating_mul(CHANGE_OF_KEY_PERMILLE)
                                    / 1_000_000
                            },
                        ),
                        None => self.change_qp.unwrap_or(now).max(now),
                    },
                };
                // 比当前画面的 QP 低：静止的部分也会跟着重编（等于补清晰），只能低到预算补得动的程度
                q.max(self.refine_floor(now, target, pixels))
            }
            Kind::Refine => self.refine_floor(
                self.qp.unwrap_or(BASE_QP),
                target.saturating_mul(REFINE_BUDGET_FACTOR),
                pixels,
            ),
        };
        qp.clamp(MIN_QP, self.max_qp)
    }

    /// 编完一帧：`bytes` 字节、用的 QP `qp`、`pixels` 个像素。
    pub fn on_frame(&mut self, kind: Kind, bytes: u64, qp: u8, pixels: u64) {
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
        c.on_frame(Kind::Key, 46_000, BASE_QP, P720);
        assert!(c.key_bytes(P720, BASE_QP) < 60_000);
        // 刚出过关键帧：沿用它的 QP
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), BASE_QP);
        // 一帧大了 8 倍：QP 加约 21，但每帧最多 +10
        c.on_frame(Kind::Change, 16_000, 30, P720);
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), 40);
        // 一帧比目标小很多：每帧最多 −2；但比当前画面的 QP 低就等于补清晰，只能低到预算补得动的程度（这里 1）
        c.on_frame(Kind::Change, 100, 40, P720);
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), 39);
        assert_eq!(c.next(Kind::Change, 1_000_000, 0, P720, None), 38);
        // 正好：不动
        c.on_frame(Kind::Change, 2_000, 39, P720);
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), 39);
        // 夹在范围里
        c.on_frame(Kind::Change, 1, MIN_QP, P720);
        assert_eq!(c.next(Kind::Change, 2_000, 0, P720, None), MIN_QP);
    }

    #[test]
    fn refine_steps_down_within_budget_until_converged() {
        let mut c = QpControl::new(MAX_QP);
        c.on_frame(Kind::Key, 14_000, 51, P720);
        assert!(!c.converged());
        // 预算很小：每帧至少降 1；预算大：一次降很多
        let small = c.next(Kind::Refine, 500, 0, P720, None);
        assert_eq!(small, 50);
        let big = c.next(Kind::Refine, 1_000_000, 0, P720, None);
        assert_eq!(big, MIN_QP);
        // 一直补：QP 单调降到最小
        let mut last = 51;
        for _ in 0..200 {
            let q = c.next(Kind::Refine, 2_000, 0, P720, None);
            assert!(q <= last);
            c.on_frame(Kind::Refine, 2_000, q, P720);
            last = q;
            if c.converged() {
                break;
            }
        }
        assert!(c.converged());
        // 补清晰的帧不会让画面“变糊”：用了更高的 QP 也记更好的那个
        c.on_frame(Kind::Refine, 10, 30, P720);
        assert_eq!(c.qp(), Some(MIN_QP));
        // 补完之后画面突然整屏变化：按面积估计、装进大帧时限，不按补到的 QP 12 编
        let whole = c.next(Kind::Change, 1_000, 10_000, P720, Some(1000));
        assert!(whole >= 40, "{whole}");
        // 只变了一小块（打字）：低 QP 也装得下
        assert_eq!(c.next(Kind::Change, 1_000, 10_000, P720, Some(2)), MIN_QP);
        // 不知道面积：用上一次在变时的 QP（这里还没有过，用当前的）
        assert_eq!(c.next(Kind::Change, 1_000, 10_000, P720, None), MIN_QP);
        c.on_frame(Kind::Change, 1_000, 33, P720);
        c.on_frame(Kind::Refine, 10, 20, P720);
        assert_eq!(c.next(Kind::Change, 1_000, 10_000, P720, None), 33);
        assert_eq!(QpControl::default().qp(), None);
    }
}
