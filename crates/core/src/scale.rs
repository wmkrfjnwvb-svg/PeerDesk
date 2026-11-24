//! 编码分辨率：带宽不明时先用半分辨率出第一幅画面，确认带宽够了再升回原尺寸。
//!
//! 为什么（2026-11-24 实测）：720p 满屏文字的关键帧，OpenH264 第一帧怎么也压不到 90 KB 以下
//! （初始 QP 由打开时的码率决定，放开 QP 上限、降低起始码率都只到 93 KB）。400 kbps 的链路上
//! 光传就要 1.9 秒以上，真实二进制 8 秒 0 帧、仿真里第一幅画面要 3.4–4.8 秒。半分辨率的关键帧
//! 约四分之一大小，零点几秒就能看到画面。
//!
//! 规则（v1）：
//! - 开始时用 [`START_PERMILLE`]（半分辨率）：连接时还不知道带宽（设计文档 3.4 节“估计值不可信时，
//!   大画面先用较低画质发出”）。
//! - 当前尺寸的关键帧送到之后，按它的大小推算原尺寸关键帧有多大（按面积），按当前码率
//!   [`UP_WITHIN_US`] 内送得完就升回原尺寸。局域网上几百毫秒内就升上去。
//! - 升上去之后不再降（v1）。降分辨率作为最后手段、按结果（冻结、关键帧送不到）判断，以后加。
//!
//! 接收端不用知道缩放：帧头写的就是编码后的尺寸，控制端按窗口拉伸显示；键鼠坐标按画面归一化，不受影响。

use crate::time::Time;

/// 原尺寸。
pub const FULL_PERMILLE: u16 = 1000;
/// 开始时的比例：半分辨率。
pub const START_PERMILLE: u16 = 500;
/// 画面在变时：推算的原尺寸关键帧能在这么长时间内送完才升回原尺寸（微秒）。
/// 0.5 秒时仿真里蜂窝链路一直停在半分辨率（文字看不清）；0.8 秒时窄带宽看视频最长等待 919 ms，0.65 秒时 868 ms。
pub const UP_WITHIN_US: u64 = 650_000;
/// 画面停下这么久之后（微秒），可以在后台慢慢送原尺寸的关键帧：画面本来就不动，慢一点不造成卡顿。
pub const IDLE_AFTER_US: u64 = 500_000;
/// 画面停下时：原尺寸关键帧在这么长时间内送得完就升（微秒）。“静止后必须补清晰”，半分辨率的字看不清。
pub const IDLE_UP_WITHIN_US: u64 = 5_000_000;

/// 编码分辨率的决定。不做 IO，时间由调用方传入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalePolicy {
    permille: u16,
    /// 最近一个关键帧：(按什么比例编的, 字节数)。
    last_key: Option<(u16, u64)>,
    /// 最近一个关键帧从发出到确认的实测速度（比特每秒）：链路真实能送多快（码率估计在会话开始时
    /// 还是起始值，真实 400 kbps 的链路上会按 4 Mbps 算、过早升回原尺寸）。
    delivered_bps: Option<u64>,
    since: Option<Time>,
    /// 画面上一次变化的时间（不是编帧的时间：窄带宽上画面在变时也会因为在途上限、降帧率而一阵子不编帧）。
    last_change: Option<Time>,
}

impl Default for ScalePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl ScalePolicy {
    /// 从半分辨率开始。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            permille: START_PERMILLE,
            last_key: None,
            delivered_bps: None,
            since: None,
            last_change: None,
        }
    }

    /// 一直用原尺寸（不缩放的编码器，例如块编码）。
    #[must_use]
    pub const fn full() -> Self {
        Self {
            permille: FULL_PERMILLE,
            last_key: None,
            delivered_bps: None,
            since: None,
            last_change: None,
        }
    }

    /// 现在的比例（千分比）。
    #[must_use]
    pub const fn permille(&self) -> u16 {
        self.permille
    }

    /// 发出了一个关键帧：按 `permille` 编的，`bytes` 字节。
    pub fn on_keyframe(&mut self, permille: u16, bytes: u64) {
        self.last_key = Some((permille, bytes));
        self.delivered_bps = None;
    }

    /// 最近一个关键帧送到了，从发出到确认用了 `took_us`。
    pub fn on_key_delivered(&mut self, took_us: u64) {
        if let Some((_, bytes)) = self.last_key {
            self.delivered_bps = Some(bytes.saturating_mul(8_000_000) / took_us.max(1));
        }
    }

    /// 编下一帧之前决定比例。`rate_bps` 是当前码率估计，`key_pending` 表示还有关键帧没送到
    /// （没送到之前不知道链路真实的速度，也不能再塞一个大关键帧）。
    pub fn decide(&mut self, now: Time, rate_bps: u64, key_pending: bool) -> u16 {
        self.since.get_or_insert(now);
        if self
            .full_transfer_us(rate_bps, key_pending)
            .is_some_and(|t| t <= UP_WITHIN_US)
        {
            self.permille = FULL_PERMILLE;
            self.since = Some(now);
        }
        self.permille
    }

    /// 采集到的画面变了。
    pub fn on_change(&mut self, now: Time) {
        self.last_change = Some(now);
    }

    /// 画面停下时调用（例如每次收到反馈）：停够了、原尺寸关键帧也送得完，就升回原尺寸。
    /// 返回真表示要编一帧（调用方让编码器出原尺寸的关键帧）。
    pub fn on_idle(&mut self, now: Time, rate_bps: u64, key_pending: bool) -> bool {
        let idle = self
            .last_change
            .is_some_and(|t| now.since(t) >= IDLE_AFTER_US);
        if idle
            && self
                .full_transfer_us(rate_bps, key_pending)
                .is_some_and(|t| t <= IDLE_UP_WITHIN_US)
        {
            self.permille = FULL_PERMILLE;
            self.since = Some(now);
            return true;
        }
        false
    }

    /// 推算按原尺寸出关键帧要传多久（微秒）。已经是原尺寸、有关键帧没送到、还没有这个比例的关键帧时为 `None`。
    fn full_transfer_us(&self, rate_bps: u64, key_pending: bool) -> Option<u64> {
        if self.permille >= FULL_PERMILLE || key_pending {
            return None;
        }
        let (at, bytes) = self.last_key.filter(|(p, _)| *p == self.permille)?;
        // 要等这个比例的关键帧送到、测出链路实际多快
        let delivered = self.delivered_bps?;
        // 关键帧大小大致和面积成正比：比例的平方
        let at = u64::from(at.max(1));
        let full_bytes = bytes.saturating_mul(1_000_000) / (at * at);
        Some(full_bytes.saturating_mul(8_000_000) / rate_bps.min(delivered).max(1))
    }
}

/// 把 `size` 等比缩小到放得进 `view`（不放大）；缩小时取偶数，至少 16×16。`view` 为 `None` 时不变。
#[must_use]
pub fn fit(size: (u16, u16), view: Option<(u16, u16)>) -> (u16, u16) {
    let Some((vw, vh)) = view.filter(|(w, h)| *w > 0 && *h > 0) else {
        return size;
    };
    let (w, h) = (u32::from(size.0), u32::from(size.1));
    if w <= u32::from(vw) && h <= u32::from(vh) {
        return size;
    }
    // 按更紧的那一边缩：那一边正好等于窗口，另一边等比
    let (nw, nh) = if w * u32::from(vh) >= h * u32::from(vw) {
        (u32::from(vw), h * u32::from(vw) / w.max(1))
    } else {
        (w * u32::from(vh) / h.max(1), u32::from(vh))
    };
    let even = |v: u32, max: u16| u16::try_from((v & !1).max(16)).unwrap_or(max).min(max);
    (even(nw, size.0), even(nh, size.1))
}

/// 按比例算编码尺寸：缩小时取偶数（H.264 的 4:2:0 要求），至少 16×16，不超过原尺寸；原尺寸不变。
#[must_use]
pub fn scaled_size(width: u16, height: u16, permille: u16) -> (u16, u16) {
    // 原尺寸不动（奇数尺寸由编码器自己补齐，帧头照样写原尺寸）
    if permille >= FULL_PERMILLE {
        return (width, height);
    }
    let one = |v: u16| {
        let scaled = u32::from(v) * u32::from(permille.min(FULL_PERMILLE)) / 1000;
        let even = scaled & !1;
        u16::try_from(even.max(16)).unwrap_or(v).min(v)
    };
    (one(width), one(height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_half_and_goes_up_when_a_full_keyframe_fits() {
        let mut p = ScalePolicy::new();
        assert_eq!(p.decide(Time(0), 4_000_000, false), START_PERMILLE);
        // 半分辨率关键帧 30 KB 还没送到：不动
        p.on_keyframe(START_PERMILLE, 30_000);
        assert_eq!(p.decide(Time(100_000), 4_000_000, true), START_PERMILLE);
        assert_eq!(
            p.decide(Time(100_000), 4_000_000, false),
            START_PERMILLE,
            "还没测出链路速度"
        );
        // 送了 0.6 秒（约 400 kbps）：码率估计还是 4 Mbps 也不升（原尺寸约 120 KB 要 2.4 秒）
        p.on_key_delivered(600_000);
        assert_eq!(p.decide(Time(700_000), 4_000_000, false), START_PERMILLE);
        // 码率估计 400 kbps 也一样
        assert_eq!(p.decide(Time(700_000), 400_000, false), START_PERMILLE);
        // 送得快（60 ms，4 Mbps）：0.24 秒，升
        p.on_key_delivered(60_000);
        assert_eq!(p.decide(Time(800_000), 4_000_000, false), FULL_PERMILLE);
        // 升上去之后不再降
        assert_eq!(p.decide(Time(900_000), 100_000, false), FULL_PERMILLE);
        assert_eq!(p.permille(), FULL_PERMILLE);
    }

    #[test]
    fn needs_a_keyframe_at_the_current_scale() {
        let mut p = ScalePolicy::default();
        assert_eq!(
            p.decide(Time(0), 50_000_000, false),
            START_PERMILLE,
            "还没有关键帧"
        );
        p.on_keyframe(FULL_PERMILLE, 100);
        p.on_key_delivered(1);
        assert_eq!(
            p.decide(Time(1), 50_000_000, false),
            START_PERMILLE,
            "不是这个比例的"
        );
        assert_eq!(ScalePolicy::full().permille(), FULL_PERMILLE);
        let mut q = ScalePolicy::new();
        q.on_key_delivered(1_000);
        assert_eq!(
            q.decide(Time(0), 50_000_000, false),
            START_PERMILLE,
            "没有关键帧时的确认不算"
        );
    }

    #[test]
    fn goes_up_after_the_screen_stays_still() {
        let mut p = ScalePolicy::new();
        assert!(!p.on_idle(Time(0), 400_000, false), "还没有画面");
        p.on_change(Time(0));
        p.decide(Time(0), 400_000, false);
        p.on_keyframe(START_PERMILLE, 30_000);
        p.on_key_delivered(600_000);
        // 400 kbps：原尺寸 120 KB 要 2.4 秒，画面在变时不升
        assert_eq!(p.decide(Time(100_000), 400_000, false), START_PERMILLE);
        assert!(!p.on_idle(Time(300_000), 400_000, false), "停得还不够久");
        assert!(!p.on_idle(Time(700_000), 400_000, true), "关键帧还没送到");
        assert!(!p.on_idle(Time(700_000), 100_000, false), "10 秒送不完");
        assert!(p.on_idle(Time(700_000), 400_000, false));
        assert_eq!(p.permille(), FULL_PERMILLE);
        assert!(!p.on_idle(Time(2_000_000), 400_000, false), "已经是原尺寸");
    }

    #[test]
    fn fits_inside_the_view() {
        // Retina 屏 2880×1800，控制端窗口 1440×900：编 1440×900
        assert_eq!(fit((2880, 1800), Some((1440, 900))), (1440, 900));
        // 窗口比例不同：按更紧的一边
        assert_eq!(fit((1920, 1080), Some((1000, 1000))), (1000, 562));
        // 窗口比画面大、不限：不变
        assert_eq!(fit((1280, 720), Some((2560, 1440))), (1280, 720));
        assert_eq!(fit((1280, 720), None), (1280, 720));
        assert_eq!(fit((1280, 720), Some((0, 0))), (1280, 720));
    }

    #[test]
    fn scaled_sizes_are_even_and_bounded() {
        assert_eq!(scaled_size(1280, 720, 500), (640, 360));
        assert_eq!(scaled_size(2241, 1401, 500), (1120, 700));
        assert_eq!(scaled_size(1280, 720, 1000), (1280, 720));
        assert_eq!(scaled_size(1280, 720, 2000), (1280, 720));
        assert_eq!(scaled_size(33, 17, 1000), (33, 17), "原尺寸是奇数也不动");
        assert_eq!(scaled_size(20, 10, 100), (16, 10), "至少 16，不超过原尺寸");
    }
}
