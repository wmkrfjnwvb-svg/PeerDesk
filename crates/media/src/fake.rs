//! 假编码后端（整体重新设计 C2，见 `docs/research/Q-redesign/design.md` 1.6、3.2 节）：给仿真（L0）和测试用。
//!
//! 编出的“码流”不是真 H.264，是确定性的假字节：长度由调用方给的预算和一个可替换的[`ByteModel`]（字节模型）定，
//! 内容只由（帧编号、QP、是否关键帧、第几帧）决定。**同一配置、同一输入序列，输出逐字节相同。**
//! 计时用调用方注入的[`Clock`]，从不读系统时间，所以仿真里可以确定地重放；“停了多久”也由调用方拨时钟。
//!
//! # 行为只由毛病表决定
//!
//! 后端的每一种脾气都是 [`FakeConfig`] 里的一个开关，没有隐式规则。分两张表：
//! - [`Faults`]：**守约的脾气**。这两周真机上遇到的、契约允许的行为（码率窗口放不下就丢帧、攒帧、静止后第一帧慢……），
//!   出处见每一项的说明。后端的 [`BackendTraits`] 按它们自动算出来（声明是诚实的）。
//! - [`Breaches`]：**违约**。契约（`backend.rs` 模块说明九条）不允许的行为，给体检（[`crate::checkup`]）证明它抓得到违约用；
//!   默认全关，要显式打开。
//!
//! 默认配置（[`FakeConfig::default`]）守约、什么毛病也没有：按 QP 编、同步、不攒帧、不丢帧。
//!
//! # 字节模型不是真数据
//!
//! [`DefaultByteModel`] 按 QP 和“内容复杂度”估字节数，**系数是占位**（形状对：QP 每加 6 字节数减半、关键帧比普通帧大得多、
//! 变化越多越大），数值不对应任何真实编码器，以后由 M2 的实测数据标定。不要拿它的绝对数字当结论。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::backend::{
    BackendError, BackendInfo, BackendKind, BackendTraits, DropReason, EncodedOut, EncoderBackend,
    Finished, Format, FrameAsk, FrameInput, Inputs, RateControl, Rates, Reconfigured, Submitted,
    Timing,
};
use crate::content;
use peerdesk_core::changes::{union_area, Accum, ChangeFeed, Rect};
use peerdesk_core::error::Error;
use peerdesk_core::frame::{Described, Picture};
use peerdesk_core::screen::Screen;
use peerdesk_core::video::{FrameHeader, VideoDecoder};

/// 最小 QP：假后端能编到的最细（和 [`crate::qp::MIN_QP`] 无关，只是模型的范围）。
pub const MODEL_MIN_QP: u8 = 10;
/// 最大 QP（H.264 的上限）。
pub const MODEL_MAX_QP: u8 = 51;
/// 画面变了这么多（千分之几）以上算整屏突变，[`SceneKeyframes`] 打开时会自己出关键帧。
pub const SCENE_PERMILLE: u32 = 900;
/// [`Faults::first_frame_prime`] 打开时，多编一次初始化花的时间（微秒，占位）。
pub const PRIME_SETUP_US: u64 = 6_000;

/// 时钟：调用方注入，后端不读系统时间。
pub trait Clock: Send + Sync + std::fmt::Debug {
    /// 现在是第几微秒（只有差值有意义）。
    fn now_us(&self) -> u64;
}

/// 手拨的时钟：克隆出来的几份共用同一个读数，调用方拨它，假后端读它。
#[derive(Debug, Clone, Default)]
pub struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    /// 从 0 开始。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 往前拨若干微秒。
    pub fn advance_us(&self, us: u64) {
        let _ = self.0.fetch_add(us, Ordering::Relaxed);
    }

    /// 往前拨一段时间。
    pub fn advance(&self, by: Duration) {
        self.advance_us(u64::try_from(by.as_micros()).unwrap_or(u64::MAX));
    }
}

impl Clock for ManualClock {
    fn now_us(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// 字节模型的输入：这一帧的编码条件和内容特征。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelInput {
    /// 按哪个 QP 编。
    pub qp: u8,
    /// 是不是关键帧。
    pub key: bool,
    /// 画面像素数。
    pub pixels: u64,
    /// 和上一帧比变了多少（千分之几，0 到 1000；第一帧算 1000）。抽样比出来的，小变化（指针、打字）量成 0；
    /// 新的字节模型该看 [`Self::regions`]，这个只用来判场景切换。
    pub changed_permille: u32,
    /// 画面有多少细节（千分之几：相邻点亮度差大的点占多少）。
    pub detail_permille: u32,
    /// 上一帧编出来时用的 QP（配置之后还没编出过帧为 `None`）。静止画面补清晰（同一幅画面按更小的 QP 再编一次）
    /// 要靠它看出来：这时变化面积是 0，字节数却由“从多粗补到多细”决定（M2 实测，见 `data-m2.md`）。
    pub prev_qp: Option<u8>,
    /// 配置之后这是第几帧（从 0 数）。字节模型要给帧大小加随机扰动时用它：同一个画面连着编几帧，扰动也该不同；
    /// 又因为它不依赖 QP，同一帧在不同 QP 下的扰动相同，“QP 越大不会越大”仍然成立。
    pub serial: u32,
    /// 区域类的输入：从上一帧编出来起，画面哪里变了、静止的部分画质怎么样（见 [`Regions`]）。
    /// 现有的字节模型（占位模型、`FittedByteModel`）都不看它；`changed_permille` 也不再用来算字节，只用来判场景切换。
    pub regions: Regions,
}

/// [`Regions`] 里静止宏块画质差分布的格数：画质差 1 到 51。
pub const GAP_BINS: usize = MODEL_MAX_QP as usize;

/// 区域类的输入（`docs/research/Q-redesign/m2b-plan.md` 第 1 节）。像素数都是编码尺寸上的。
///
/// 来源：画面的来源每变一版推进脏区域和移动区域（[`peerdesk_core::changes`]），后端从上一帧编出来起累计；
/// 再加上它自己维护的 16×16 宏块“现在画质”表：每个宏块记它最后一次被编时的 QP。
/// 脏区域、移动区域碰到的宏块是“编码的”，其余是“静止的”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regions {
    /// 脏区域（新画的）的像素数。刚配置之后的第一帧整屏都算新的。
    pub dirty_px: u64,
    /// 移动区域（搬来的）的像素数：各次移动目的地的并集，减去脏区域。
    pub moved_px: u64,
    /// 累计的变化里，有一次移动的目的地占整帧 [`peerdesk_core::changes::FULL_MOVE_PERMILLE`]‰ 以上（整屏滚动）。
    pub full_move: bool,
    /// 脏区域碰到的宏块数（编码器按整块编，改一个像素也要编整个宏块；B3 拟合用它 ×256 当“新内容的面积”）。
    pub dirty_mbs: u32,
    /// 移动区域碰到、脏区域没碰到的宏块数（和 `moved_px` 同一个口径：目的地并集减去脏区域，按宏块数）。
    pub moved_mbs: u32,
    /// 脏区域里的细节（千分之几）：脏区域碰到的宏块里，有细节的抽样点占全部抽样点的比例。没有脏区域为 0。
    pub dirty_detail_permille: u32,
    /// 静止宏块的画质差分布：`static_gap[g - 1]` 是“现在画质比这一帧的 QP 粗 g 档”的静止宏块个数（g = 1…51）。
    /// 这些宏块这一帧会被补清晰；画质不比这一帧粗的不计（它们保持原样）。
    pub static_gap: [u32; GAP_BINS],
    /// 静止宏块在这一帧的 QP 上已经连着编了几帧（现在画质恰好等于这一帧的 QP 的静止宏块里，连续次数最大的；没有为 0，最多 255）。
    /// 只有低 QP 地板的实验（`data-m2b.md` 第 1 节）证明需要时才用；那个实验的结论是不需要。
    pub repeat: u32,
}

impl Default for Regions {
    fn default() -> Self {
        Self {
            dirty_px: 0,
            moved_px: 0,
            full_move: false,
            dirty_mbs: 0,
            moved_mbs: 0,
            dirty_detail_permille: 0,
            static_gap: [0; GAP_BINS],
            repeat: 0,
        }
    }
}

/// 字节模型：给定编码条件，这一帧大约多少字节。要求：QP 越大不会越大（体检按这个判断“按 QP 编”）。
pub trait ByteModel: Send + std::fmt::Debug {
    /// 这一帧的字节数（不为 0）。
    fn bytes(&self, input: &ModelInput) -> usize;
}

/// 默认的字节模型（系数是占位，见模块说明）。
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultByteModel;

/// QP 每加 1 步长放大的倍数（千分之几），一个周期 6 档，之后翻倍。
const QP_STEP_X1000: [u64; 6] = [1000, 1122, 1260, 1414, 1587, 1782];
/// QP 为 0 时每个像素的字节数（占位，千分之一字节）。
const MILLI_BYTES_PER_PIXEL_QP0: u64 = 1400;
/// 每帧固定开销（占位）。
const FRAME_OVERHEAD: u64 = 24;
/// 关键帧多出来的固定开销（占位）。
const KEY_OVERHEAD: u64 = 40;

impl ByteModel for DefaultByteModel {
    fn bytes(&self, input: &ModelInput) -> usize {
        let qp = u64::from(input.qp.min(MODEL_MAX_QP));
        let step = QP_STEP_X1000
            .get(usize::try_from(qp % 6).unwrap_or(0))
            .copied()
            .unwrap_or(1000);
        // 步长 = 周期内的倍数 × 2^(QP / 6)
        let divisor = u128::from(step) << (qp / 6);
        let area = if input.key {
            1000
        } else {
            u64::from(input.changed_permille.min(1000))
        };
        let detail = 500 + u128::from(input.detail_permille.min(1000));
        // 千分之一字节 = 像素 × 系数 × 细节 × 面积（后两个各是千分之几）
        let milli = u128::from(input.pixels)
            * u128::from(MILLI_BYTES_PER_PIXEL_QP0)
            * detail
            * u128::from(area)
            / 1_000_000;
        let body = milli * 1000 / divisor / 1000;
        let overhead = FRAME_OVERHEAD + if input.key { KEY_OVERHEAD } else { 0 };
        usize::try_from(u128::from(overhead) + body).unwrap_or(usize::MAX)
    }
}

/// 整屏突变时会不会自己出关键帧（没要求也出）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SceneKeyframes {
    /// 不会。
    #[default]
    Off,
    /// 会，并且在 [`BackendTraits::scene_keyframes`] 里声明了（openh264 屏幕内容模式就是这样，守约）。
    Declared,
    /// 会，但没声明（违约：调用方没法按“可能是关键帧”算预算）。
    Undeclared,
}

/// 静止后第一帧慢：两次交帧隔了至少 `idle_us`，这一帧的计算时间多 `extra_us`
/// （日志：硬件编码器睡着之后第一帧要醒一下）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlowAfterIdle {
    /// 静止多久之后算“睡着了”（微秒）。
    pub idle_us: u64,
    /// 多出来的计算时间（微秒）。
    pub extra_us: u64,
}

/// 卡住：交到第 `after_frames` 帧时（从重新配置算起，从 0 数）编码器卡住，这一帧永远不出；
/// 之后交帧一律回 `Dropped { NotAccepting }`；卡了 `limit_us` 之后，下一次 `submit` 或 `poll` 报错并复位
/// （契约第 9 条）。只卡一次。`limit_us = u64::MAX` 表示一直不报错（体检抓“卡住又不吭声”用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stall {
    /// 第几帧卡住。
    pub after_frames: u32,
    /// 卡多久之后报错（微秒）。
    pub limit_us: u64,
}

/// 守约的脾气：契约允许、但各家编码器有的行为。每一项都对应日志里遇到过的情况。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Faults {
    /// 码率窗口放不下就丢帧，回 `Dropped { Budget }`（VideoToolbox、Media Foundation 的码控）。只对 [`RateControl::Window`] 有意义。
    pub window_drop: bool,
    /// 预算放不下时，把要求的关键帧丢掉（回 `Dropped { Budget }`，要求没送到，调用方接着要）
    /// （VideoToolbox：苹果文档说“不保证都能满足”，2026-11-27 Mac 日志）。只对 Window 有意义。
    pub drop_requested_keyframe: bool,
    /// 超发：码控“够不着”预算时（要的比预算大），编出来的字节比预算多这么多（千分之几）。只对 Window 有意义。
    pub overshoot_permille: u32,
    /// 攒帧：交了这么多帧之后才出第一个（异步硬件编码器）。声明在 `max_held` 里。
    pub held_frames: u8,
    /// 静止后第一帧慢。
    pub slow_after_idle: Option<SlowAfterIdle>,
    /// 整屏突变时自己出关键帧。
    pub scene_keyframes: SceneKeyframes,
    /// 要了关键帧，晚这么多帧才给（声明在 `keyframe_within` 里）。
    pub keyframe_delay: u8,
    /// 运行中改不了码率：`set_rates` 回失败（有的 Media Foundation 编码器），状态不变。
    pub set_rates_fails: bool,
    /// 卡住不出帧，然后报错复位（2026-11-27 Windows 日志）。
    pub stall: Option<Stall>,
    /// 每隔这么多次交帧有一次不收（`Dropped { NotAccepting }`，Media Foundation 的 `MF_E_NOTACCEPTING`）。
    pub not_accepting_every: Option<u32>,
    /// 新开之后第一帧只能按这个 QP 编，别的 QP 要多编一次初始化（openh264）；声明在 `first_frame_qp` 里。
    pub first_frame_prime: Option<u8>,
}

/// 违约：契约不允许的行为。默认全关，要显式打开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Breaches {
    /// 契约 3：没要求也每隔这么多帧出一个关键帧（固定 GOP，个别厂商的 Media Foundation 编码器）。
    pub periodic_keyframes: Option<u32>,
    /// 契约 3：要了关键帧也不给，又不回丢帧（悄悄丢掉要求）。
    pub ignore_keyframe_requests: bool,
    /// 契约 1：每隔这么多帧吞掉一帧：回 `Pending`，之后永远没有下落（Media Foundation 被码控丢掉的帧没有通知）。
    pub lose_frame_every: Option<u32>,
    /// 契约 4：放不下要丢的时候，交一个空帧冒充编好了（改造前的 VideoToolbox 路径）。
    pub empty_frame_on_drop: bool,
    /// 契约 5：改码率时顺带出一个关键帧。
    pub set_rates_forces_keyframe: bool,
    /// 契约 6：重新配置时说下一帧一定是关键帧，实际不是。
    pub first_frame_not_key: bool,
    /// 契约 6：交的画面尺寸和配置的不一样时，不报用法错，自己悄悄换尺寸（宽度不变，高度按数据量推）。
    pub resize_silently: bool,
    /// 契约 7：两次交帧隔了这么久（微秒）就报错。
    pub error_after_idle_us: Option<u64>,
    /// 契约 8：不报时间（全是 0）。
    pub zero_timing: bool,
    /// 契约 9：出错之后不复位（`size()` 还是 `Some`）。
    pub keep_state_after_error: bool,
    /// 契约 2：`flush` 什么也不做，攒着的帧一直没有下落。
    pub flush_forgets: bool,
}

/// 假后端的配置。
#[derive(Debug, Clone)]
pub struct FakeConfig {
    /// 名字（日志和体检报告里显示）。
    pub name: String,
    /// 每帧大小怎么控制。
    pub rate_control: RateControl,
    /// 收哪些形式的画面（显卡画面假后端不收）。
    pub inputs: Inputs,
    /// 每帧的基础计算时间（微秒，占位）；关键帧翻倍。
    pub compute_us: u64,
    /// 每帧的等待时间（微秒，例如读回）。
    pub wait_us: u64,
    /// 守约的脾气。
    pub faults: Faults,
    /// 违约。
    pub breaches: Breaches,
    /// 谎报的属性：不为 `None` 时，[`EncoderBackend::info`] 给这个，不按上面的脾气算
    /// （体检抓“声明和实测不符”用）。
    pub declared: Option<BackendTraits>,
}

impl Default for FakeConfig {
    fn default() -> Self {
        Self {
            name: "假后端".to_owned(),
            rate_control: RateControl::PerFrameQp,
            inputs: Inputs {
                i420: true,
                nv12: true,
                gpu: false,
                described: false,
            },
            compute_us: 2_000,
            wait_us: 0,
            faults: Faults::default(),
            breaches: Breaches::default(),
            declared: None,
        }
    }
}

impl FakeConfig {
    /// 有自己码控的后端：放不下就丢帧（契约第 4 条允许的做法）。
    #[must_use]
    pub fn window() -> Self {
        Self {
            name: "假后端（码控窗口）".to_owned(),
            rate_control: RateControl::Window,
            faults: Faults {
                window_drop: true,
                ..Faults::default()
            },
            ..Self::default()
        }
    }

    /// 和软件编码后端（openh264）声明的属性一样的假后端：按 QP 编、只收 I420、同步、要了关键帧就是这一帧、
    /// 新开之后第一帧按默认 QP、整屏突变会自己出关键帧。仿真里它代替 openh264，上面的码控、补清晰、大帧规则
    /// 看到的后端脾气不变。计算时间按 0 算（A1 里编码耗时不进仿真，和 L1 一样）。
    #[must_use]
    pub fn like_openh264() -> Self {
        Self {
            name: "假后端（照 openh264 的属性）".to_owned(),
            inputs: Inputs {
                i420: true,
                nv12: false,
                gpu: false,
                described: false,
            },
            compute_us: 0,
            faults: Faults {
                first_frame_prime: Some(crate::soft::DEFAULT_LAYER_QP),
                scene_keyframes: SceneKeyframes::Declared,
                ..Faults::default()
            },
            ..Self::default()
        }
    }

    /// 按脾气算出来的诚实声明。
    #[must_use]
    pub fn honest_traits(&self) -> BackendTraits {
        let f = &self.faults;
        BackendTraits {
            rate_control: self.rate_control,
            inputs: self.inputs,
            // 卡住的那一帧也算攒着的
            max_held: f.held_frames.max(u8::from(f.stall.is_some())),
            keyframe_within: f.keyframe_delay.saturating_add(1),
            first_frame_qp: f.first_frame_prime,
            scene_keyframes: f.scene_keyframes == SceneKeyframes::Declared,
        }
    }
}

/// 假编码后端。
#[derive(Debug)]
pub struct FakeBackend {
    cfg: FakeConfig,
    info: BackendInfo,
    clock: Arc<dyn Clock>,
    model: Box<dyn ByteModel>,
    rates: Rates,
    size: Option<(u16, u16)>,
    /// 配置之后还没编出过帧。
    first: bool,
    /// 配置之后编出了几帧（卡住、吞帧按它数）。
    index: u32,
    /// 配置之后收到几次有效的交帧（不收帧按它数）。
    calls: u32,
    /// 上一帧抽样的亮度点。
    prev: Option<Vec<u8>>,
    /// 上一帧编出来时用的 QP。
    last_qp: Option<u8>,
    /// 上一次交帧的时间。
    last_submit_us: Option<u64>,
    /// 欠着的关键帧：还要再过几帧才给。
    key_countdown: Option<u8>,
    /// 攒着、还没出的帧。
    inflight: VecDeque<Finished>,
    /// 已经出了、等 `poll` 取的帧。
    ready: VecDeque<Finished>,
    /// 卡住开始的时间。
    stalled_since: Option<u64>,
    /// 卡住已经发生过了（只卡一次）。
    stall_spent: bool,
    /// 画面内容的版本，由外面（仿真的画面来源）拨；每一帧的码流里写着交帧时的读数。
    version: Option<Arc<AtomicU64>>,
    /// 画面哪里变了，由外面（仿真的画面来源）每变一版推进来；没接的话区域类输入只有“刚配置的第一帧整屏都是新的”。
    feed: Option<ChangeFeed>,
    /// 区域类输入（宏块的“现在画质”表和累计变化的换算）。
    regions: RegionTracker,
}

impl FakeBackend {
    /// 创建（还没配置，先 [`EncoderBackend::reconfigure`]）。
    #[must_use]
    pub fn new(cfg: FakeConfig, clock: Arc<dyn Clock>, model: Box<dyn ByteModel>) -> Self {
        let info = BackendInfo {
            name: cfg.name.clone(),
            kind: BackendKind::Software,
            format: Format::H264,
            traits: cfg.declared.unwrap_or_else(|| cfg.honest_traits()),
        };
        Self {
            cfg,
            info,
            clock,
            model,
            rates: Rates { bps: 0, fps: 1 },
            size: None,
            first: true,
            index: 0,
            calls: 0,
            prev: None,
            last_qp: None,
            last_submit_us: None,
            key_countdown: None,
            inflight: VecDeque::new(),
            ready: VecDeque::new(),
            stalled_since: None,
            stall_spent: false,
            version: None,
            feed: None,
            regions: RegionTracker::default(),
        }
    }

    /// 画面哪里变了从 `feed` 读（每交一帧读一次累计，编出来之后清零）。
    ///
    /// 为什么用旁路而不是跟着画面一起交（和 [`Self::with_version_tag`] 同一个理由）：吃真像素的路径（L0.5）像素里没有
    /// “哪里变了”，产品里的编码器接口（`VideoEncoder::encode`）也没有这个参数；吃内容描述的路径（L0）里，发送端在途满了
    /// 跳过的那些版本，后端根本看不到，只能由画面来源每变一版推进一次。两条路都由来源推进，交给后端的累计就相同。
    #[must_use]
    pub fn with_change_feed(mut self, feed: ChangeFeed) -> Self {
        self.feed = Some(feed);
        self
    }

    /// 画面内容的版本从 `tag` 读（每交一帧读一次，写进这一帧的码流）。
    ///
    /// 吃真像素的假后端（L0.5）没有别的办法知道“这是源画面的第几版”：像素里没有版本。仿真的画面来源每变一次拨一下 `tag`，
    /// 假解码器（[`FakeDecoder`]）把它交给显示端量尺。吃内容描述的假后端（L0）从描述里读版本，不用这个。
    #[must_use]
    pub fn with_version_tag(mut self, tag: Arc<AtomicU64>) -> Self {
        self.version = Some(tag);
        self
    }

    /// 用默认字节模型。
    #[must_use]
    pub fn with_default_model(cfg: FakeConfig, clock: Arc<dyn Clock>) -> Self {
        Self::new(cfg, clock, Box::new(DefaultByteModel))
    }

    /// 最近一次设的码率和帧率。
    #[must_use]
    pub const fn rates(&self) -> Rates {
        self.rates
    }

    /// 回到“刚配置”的码流状态（不动尺寸）。
    fn reset_stream(&mut self) {
        self.first = true;
        self.index = 0;
        self.calls = 0;
        self.prev = None;
        self.last_qp = None;
        self.last_submit_us = None;
        self.key_countdown = None;
        self.regions.reset();
    }

    /// 出错：按契约第 9 条复位（违约的后端不复位），攒着的帧作废。
    fn fail(&mut self, why: &str) -> BackendError {
        self.stall_spent = true;
        self.stalled_since = None;
        self.inflight.clear();
        self.ready.clear();
        self.key_countdown = None;
        if !self.cfg.breaches.keep_state_after_error {
            self.size = None;
            self.reset_stream();
        }
        BackendError::Failed(why.to_owned())
    }

    /// 卡住了的话，到时限没有？到了就出错复位。
    fn stall_timeout(&mut self, now: u64) -> Option<BackendError> {
        let since = self.stalled_since?;
        let limit = self.cfg.faults.stall.map_or(u64::MAX, |s| s.limit_us);
        (now.saturating_sub(since) >= limit)
            .then(|| self.fail("编码器卡住了，一直没有出帧（假后端的毛病表）"))
    }

    /// 检查画面和配置的尺寸对得上；对不上时，违约的后端自己换尺寸，守约的回用法错。返回亮度平面。
    fn luma<'a>(&mut self, input: FrameInput<'a>) -> Result<(&'a [u8], (u16, u16)), BackendError> {
        let Some(size) = self.size else {
            return Err(BackendError::Misuse("还没配置编码尺寸就交帧"));
        };
        let (w, h) = (usize::from(size.0), usize::from(size.1));
        let (luma, total, ok) = match input {
            FrameInput::I420 { y, u, v } => {
                if !self.cfg.inputs.i420 {
                    return Err(BackendError::Misuse("这个后端不收 I420 画面"));
                }
                let chroma = w / 2 * (h / 2);
                (
                    y,
                    y.len(),
                    y.len() == w * h && u.len() == chroma && v.len() == chroma,
                )
            }
            FrameInput::Nv12(data) => {
                if !self.cfg.inputs.nv12 {
                    return Err(BackendError::Misuse("这个后端不收 NV12 画面"));
                }
                let luma = data.get(..w * h).unwrap_or(data);
                (luma, data.len() * 2 / 3, data.len() == w * h * 3 / 2)
            }
            FrameInput::Gpu(_) => return Err(BackendError::Misuse("这个后端不收显卡画面")),
            // 内容描述在 submit 里单独处理，到不了这里
            FrameInput::Described { .. } => {
                return Err(BackendError::Misuse("内容描述没有亮度平面"))
            }
        };
        if ok {
            return Ok((luma, size));
        }
        if !self.cfg.breaches.resize_silently {
            return Err(BackendError::Misuse("画面的大小和配置的尺寸对不上"));
        }
        // 违约：悄悄换尺寸。宽度不变，高度按亮度数据量推，取偶数、至少 2
        let new_h = u16::try_from(total / w.max(1) / 2 * 2).unwrap_or(2).max(2);
        self.size = Some((size.0, new_h));
        self.prev = None;
        Ok((luma, (size.0, new_h)))
    }
}

/// 抽样亮度，算“和上一帧比变了多少、有多少细节”（千分之几）；定义在 [`crate::content`]（产品、M2 测量共用）。
fn features(luma: &[u8], prev: Option<&Vec<u8>>) -> (Vec<u8>, u32, u32) {
    let sample = content::sample_luma(luma);
    let changed = content::luma_changed_permille(&sample, prev);
    (sample, changed, content::detail_permille(luma))
}

/// 宏块还没编过。
const MB_UNSET: u8 = u8::MAX;
/// 按 QP 分格的格数（0 到 [`MODEL_MAX_QP`]）。
const QP_BINS: usize = MODEL_MAX_QP as usize + 1;

/// 宏块（16×16）的“现在画质”表：每个宏块记它最后一次被编时的 QP，和这个 QP 上已经连着编了几帧。
///
/// 规则（`m2b-plan.md` 第 1 节）：脏区域和移动区域碰到的宏块，编完后设成这一帧的 QP；静止的宏块补清晰时取较小值
/// （画质只会变好，比这一帧细的保持不变）；关键帧整帧设成这一帧的 QP；重新配置之后整张表作废。
#[derive(Debug, Default)]
struct MbTable {
    size: (u16, u16),
    qp: Vec<u8>,
    run: Vec<u8>,
    /// 每个宏块里有几个抽样点（只和尺寸有关）。
    points: Vec<u16>,
}

/// 一帧编之前对宏块的盘点（[`RegionTracker::survey`] 的结果）。
#[derive(Debug)]
pub struct Survey {
    /// 脏区域或移动区域碰到的宏块。
    touched: Vec<bool>,
    dirty_px: u64,
    moved_px: u64,
    dirty_mbs: u32,
    moved_mbs: u32,
    full_move: bool,
    detail_permille: u32,
    /// 静止宏块按现在画质（QP）分的个数、连续次数的最大值。
    count: [u32; QP_BINS],
    run_max: [u8; QP_BINS],
}

impl MbTable {
    /// 尺寸变了（或第一次）：重建，宏块都没编过。
    fn ensure(&mut self, size: (u16, u16)) {
        if self.size == size {
            return;
        }
        let (cols, rows) = content::mb_dims(size);
        self.size = size;
        self.qp = vec![MB_UNSET; cols * rows];
        self.run = vec![0; cols * rows];
        self.points = content::mb_points(size);
    }

    /// 把 `rects` 碰到的宏块标出来。
    fn mark(&self, rects: &[Rect], out: &mut [bool]) {
        let (cols, rows) = content::mb_dims(self.size);
        let mb = u32::try_from(content::MB_SIZE).unwrap_or(16);
        for r in rects.iter().filter(|r| !r.is_empty()) {
            let (c0, c1) = (usize::from(r.x) / content::MB_SIZE, (r.right() - 1) / mb);
            let (r0, r1) = (usize::from(r.y) / content::MB_SIZE, (r.bottom() - 1) / mb);
            for row in r0..=usize::try_from(r1).unwrap_or(0).min(rows.saturating_sub(1)) {
                for col in c0..=usize::try_from(c1).unwrap_or(0).min(cols.saturating_sub(1)) {
                    if let Some(t) = out.get_mut(row * cols + col) {
                        *t = true;
                    }
                }
            }
        }
    }

    fn survey(&self, dirty: &[Rect], moved: &[Rect], edges: &[u16], full_move: bool) -> Survey {
        let n = self.qp.len();
        let mut by_dirty = vec![false; n];
        self.mark(dirty, &mut by_dirty);
        let mut touched = by_dirty.clone();
        self.mark(moved, &mut touched);
        let count_of =
            |v: &[bool]| u32::try_from(v.iter().filter(|b| **b).count()).unwrap_or(u32::MAX);
        let dirty_mbs = count_of(&by_dirty);
        let moved_mbs = count_of(&touched).saturating_sub(dirty_mbs);
        // 脏区域里的细节：碰到的宏块里有细节的抽样点 ÷ 全部抽样点
        let (mut hit, mut all) = (0u64, 0u64);
        for (i, _) in by_dirty.iter().enumerate().filter(|(_, d)| **d) {
            hit += u64::from(edges.get(i).copied().unwrap_or(0));
            all += u64::from(self.points.get(i).copied().unwrap_or(0));
        }
        let mut count = [0u32; QP_BINS];
        let mut run_max = [0u8; QP_BINS];
        for (i, _) in touched.iter().enumerate().filter(|(_, t)| !**t) {
            let q = self.qp.get(i).copied().unwrap_or(MB_UNSET);
            if q == MB_UNSET {
                continue;
            }
            let bin = usize::from(q).min(QP_BINS - 1);
            if let Some(c) = count.get_mut(bin) {
                *c += 1;
            }
            if let (Some(m), Some(r)) = (run_max.get_mut(bin), self.run.get(i)) {
                *m = (*m).max(*r);
            }
        }
        let dirty_px = union_area(dirty);
        let both: Vec<Rect> = dirty.iter().chain(moved).copied().collect();
        Survey {
            touched,
            dirty_px,
            moved_px: union_area(&both) - dirty_px,
            dirty_mbs,
            moved_mbs,
            full_move,
            detail_permille: content::permille(
                usize::try_from(hit).unwrap_or(usize::MAX),
                usize::try_from(all).unwrap_or(usize::MAX),
            ),
            count,
            run_max,
        }
    }

    /// 这一帧编出来了：按 QP `qp` 更新表。
    fn commit(&mut self, touched: &[bool], qp: u8, key: bool) {
        for (i, (q, run)) in self.qp.iter_mut().zip(self.run.iter_mut()).enumerate() {
            if key || touched.get(i).copied().unwrap_or(false) || *q > qp {
                *q = qp;
                *run = 1;
            } else if *q == qp {
                *run = run.saturating_add(1);
            }
        }
    }
}

impl Survey {
    /// 按这一帧的 QP 算区域类输入（字节模型的 [`ModelInput::regions`]）。
    #[must_use]
    pub fn regions(&self, qp: u8) -> Regions {
        let q = usize::from(qp.min(MODEL_MAX_QP));
        let mut static_gap = [0u32; GAP_BINS];
        for (g, slot) in static_gap.iter_mut().enumerate() {
            *slot = self.count.get(q + g + 1).copied().unwrap_or(0);
        }
        Regions {
            dirty_px: self.dirty_px,
            moved_px: self.moved_px,
            full_move: self.full_move,
            dirty_mbs: self.dirty_mbs,
            moved_mbs: self.moved_mbs,
            dirty_detail_permille: self.detail_permille,
            static_gap,
            repeat: u32::from(self.run_max.get(q).copied().unwrap_or(0)),
        }
    }
}

/// 区域类输入的跟踪：宏块画质表加上“累计的变化 → 这一帧的 [`Regions`]”的全部计算。
///
/// 假后端（L0、L0.5）和 M2b 的测量工具（`crates/e2e/examples/measure.rs`）用的是这同一份代码：拟合时每帧的特征和
/// 假后端运行时算出来的逐位相同，不是另写一份（`docs/research/Q-redesign/m2b-plan.md` 第 5 节）。
/// 用法：每交一帧先 [`Self::survey`]，编出来之后 [`Self::commit`]；重新配置时 [`Self::reset`]。
#[derive(Debug, Default)]
pub struct RegionTracker {
    mbs: MbTable,
}

impl RegionTracker {
    /// 重新配置：宏块画质表作废。
    pub fn reset(&mut self) {
        self.mbs = MbTable::default();
    }

    /// 盘点这一帧：`size` 是编码尺寸；`first` 是配置之后的第一帧（此前没有“上一帧”，整屏都算新的，不看 `acc`）；
    /// `acc` 是从上一帧编出来起累计的变化（没接信道为 `None`，区域类输入就只有“第一帧整屏是新的”）；
    /// `edges` 是每个宏块里有细节的抽样点数（[`content::mb_edges`]，L0 里在内容描述中）。
    pub fn survey(
        &mut self,
        size: (u16, u16),
        first: bool,
        acc: Option<&Accum>,
        edges: &[u16],
    ) -> Survey {
        self.mbs.ensure(size);
        let (dirty, moved, full_move) = if first {
            (vec![Rect::whole(size)], Vec::new(), false)
        } else if let Some(acc) = acc {
            let (d, m) = acc.rects(size);
            (d, m, acc.full_move())
        } else {
            (Vec::new(), Vec::new(), false)
        };
        self.mbs.survey(&dirty, &moved, edges, full_move)
    }

    /// 这一帧编出来了：按 QP `qp` 更新宏块画质表（`key` 是不是关键帧：关键帧整表设成这一帧的 QP）。
    pub fn commit(&mut self, survey: &Survey, qp: u8, key: bool) {
        self.mbs.commit(&survey.touched, qp, key);
    }
}

/// 假码流开头的固定部分的长度：起始码 4 字节 + NAL 头 1 字节 + 内容版本 8 + 帧序号 4 + QP 1。
pub const STREAM_HEAD_LEN: usize = 18;

/// 假码流开头的内容：假解码器（[`FakeDecoder`]）靠它知道“这一帧显示的是什么、接不接得上”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StreamHead {
    key: bool,
    /// 画面内容的版本（见 [`FakeBackend::with_version_tag`]）。
    version: u64,
    /// 配置之后编出的第几帧（参考链按它接：增量帧要接在上一帧后面）。
    serial: u32,
    qp: u8,
}

/// 确定性的假码流：Annex B 起始码 + NAL 头（IDR 或普通片）+ [`StreamHead`] + 按种子生成的字节。
/// 长度至少 [`STREAM_HEAD_LEN`]。
fn fake_stream(len: usize, head: StreamHead, seed: u64) -> Vec<u8> {
    let mut data = vec![0, 0, 0, 1, if head.key { 0x65 } else { 0x41 }];
    data.extend_from_slice(&head.version.to_le_bytes());
    data.extend_from_slice(&head.serial.to_le_bytes());
    data.push(head.qp);
    let mut x = seed | 1;
    while data.len() < len {
        // xorshift64
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        data.extend_from_slice(&x.to_le_bytes());
    }
    data.truncate(len.max(STREAM_HEAD_LEN));
    data
}

/// 拆开假码流的开头；不是假码流（太短、起始码或 NAL 头不对）返回 `None`。
fn parse_head(data: &[u8]) -> Option<StreamHead> {
    let head = data.get(..STREAM_HEAD_LEN)?;
    let key = match head.get(..5)? {
        [0, 0, 0, 1, 0x65] => true,
        [0, 0, 0, 1, 0x41] => false,
        _ => return None,
    };
    Some(StreamHead {
        key,
        version: u64::from_le_bytes(*head.get(5..13)?.first_chunk::<8>()?),
        serial: u32::from_le_bytes(*head.get(13..17)?.first_chunk::<4>()?),
        qp: *head.get(17)?,
    })
}

impl EncoderBackend for FakeBackend {
    fn info(&self) -> &BackendInfo {
        &self.info
    }

    fn size(&self) -> Option<(u16, u16)> {
        self.size
    }

    fn reconfigure(
        &mut self,
        size: (u16, u16),
        rates: Rates,
    ) -> Result<Reconfigured, BackendError> {
        if size.0 == 0 || size.1 == 0 || !size.0.is_multiple_of(2) || !size.1.is_multiple_of(2) {
            return Err(BackendError::Misuse("尺寸要是偶数，不能为 0"));
        }
        self.size = Some(size);
        self.rates = rates;
        self.reset_stream();
        self.inflight.clear();
        self.ready.clear();
        // 重建把卡住的编码器也换掉了
        if self.stalled_since.take().is_some() {
            self.stall_spent = true;
        }
        Ok(Reconfigured {
            rebuilt: true,
            keyframe_next: true,
        })
    }

    fn set_rates(&mut self, rates: Rates) -> Result<(), BackendError> {
        if self.cfg.faults.set_rates_fails {
            return Err(BackendError::Rejected(
                "运行中改不了码率（假后端的毛病表）".to_owned(),
            ));
        }
        self.rates = rates;
        if self.cfg.breaches.set_rates_forces_keyframe {
            self.key_countdown = Some(0);
        }
        Ok(())
    }

    fn submit(&mut self, input: FrameInput<'_>, ask: FrameAsk) -> Result<Submitted, BackendError> {
        let rc = self.cfg.rate_control;
        if rc == RateControl::PerFrameQp && ask.qp.is_none() {
            // 先于尺寸检查：用法错不改变状态
            return Err(BackendError::Misuse("按 QP 编，每帧都要给 QP"));
        }
        // 内容描述：没有像素，面积抽样、细节、版本都在描述里；其余形式从亮度平面算
        let described = match input {
            FrameInput::Described {
                version,
                sample,
                detail_permille,
                mb_edges,
            } => {
                if !self.cfg.inputs.described {
                    return Err(BackendError::Misuse("这个后端不收内容描述"));
                }
                Some((version, sample, detail_permille, mb_edges))
            }
            FrameInput::I420 { .. } | FrameInput::Nv12(_) | FrameInput::Gpu(_) => None,
        };
        let (luma, (w, h)) = match described {
            Some(_) => {
                let Some(size) = self.size else {
                    return Err(BackendError::Misuse("还没配置编码尺寸就交帧"));
                };
                (&[][..], size)
            }
            None => self.luma(input)?,
        };
        let now = self.clock.now_us();
        let gap = self.last_submit_us.map(|t| now.saturating_sub(t));
        if let (Some(limit), Some(gap)) = (self.cfg.breaches.error_after_idle_us, gap) {
            if gap >= limit {
                return Err(self.fail("隔太久再交帧就出错（假后端的违约）"));
            }
        }
        self.last_submit_us = Some(now);
        if let Some(err) = self.stall_timeout(now) {
            return Err(err);
        }
        if self.stalled_since.is_some() {
            return Ok(Submitted::Ready(Finished::Dropped {
                token: ask.token,
                why: DropReason::NotAccepting,
            }));
        }
        self.calls = self.calls.saturating_add(1);
        if let Some(n) = self.cfg.faults.not_accepting_every {
            if n > 0 && self.calls.is_multiple_of(n) {
                return Ok(Submitted::Ready(Finished::Dropped {
                    token: ask.token,
                    why: DropReason::NotAccepting,
                }));
            }
        }

        // 画面特征
        let (sample, changed, detail) = match described {
            Some((_, sample, detail, _)) => (
                sample.to_vec(),
                content::luma_changed_permille(sample, self.prev.as_ref()),
                detail,
            ),
            None => features(luma, self.prev.as_ref()),
        };
        let pixels = u64::from(w) * u64::from(h);

        // 区域类输入：累计的变化换算到编码尺寸，和宏块画质表一起盘点
        let acc = self.feed.as_ref().map(ChangeFeed::peek);
        let edges_owned;
        let edges: &[u16] = match described {
            Some((_, _, _, e)) => e,
            None => {
                edges_owned = content::mb_edges(luma, (w, h));
                &edges_owned
            }
        };
        let survey = self
            .regions
            .survey((w, h), self.prev.is_none(), acc.as_ref(), edges);

        // 是不是关键帧：要了（可能晚几帧给）、第一帧、整屏突变、固定 GOP、改码率时顺带
        let faults = self.cfg.faults;
        if ask.keyframe
            && !self.cfg.breaches.ignore_keyframe_requests
            && self.key_countdown.is_none()
        {
            self.key_countdown = Some(faults.keyframe_delay);
        }
        let mut key = self.first && !self.cfg.breaches.first_frame_not_key;
        if self.key_countdown == Some(0) {
            key = true;
        }
        if self.prev.is_some()
            && faults.scene_keyframes != SceneKeyframes::Off
            && changed >= SCENE_PERMILLE
        {
            key = true;
        }
        if let Some(n) = self.cfg.breaches.periodic_keyframes {
            if n > 0 && self.index > 0 && self.index.is_multiple_of(n) {
                key = true;
            }
        }

        // 多大、按哪个 QP
        let bytes_at = |qp: u8| {
            self.model.bytes(&ModelInput {
                qp,
                key,
                pixels,
                changed_permille: changed,
                detail_permille: detail,
                prev_qp: self.last_qp,
                serial: self.index,
                regions: survey.regions(qp),
            })
        };
        let (qp, mut bytes, engaged, fits) = match (rc, ask.qp) {
            (RateControl::PerFrameQp, Some(qp)) => {
                let qp = qp.min(MODEL_MAX_QP);
                (qp, bytes_at(qp), false, true)
            }
            // 上面已经排除了 PerFrameQp 不给 QP；Window 不看 QP，自己挑最细的放得下的
            (RateControl::PerFrameQp | RateControl::Window, _) => {
                match (MODEL_MIN_QP..=MODEL_MAX_QP).find(|q| bytes_at(*q) <= ask.budget) {
                    Some(q) => (q, bytes_at(q), bytes_at(MODEL_MIN_QP) > ask.budget, true),
                    None => (MODEL_MAX_QP, bytes_at(MODEL_MAX_QP), true, false),
                }
            }
        };

        // 放不下：丢（守约的做法），或者不丢、交超预算的帧（违约，体检会抓）
        if !fits && (faults.window_drop || (key && faults.drop_requested_keyframe)) {
            if ask.keyframe || key {
                // 这一帧带的关键帧要求没送到，调用方接着要
                self.key_countdown = None;
            }
            return Ok(Submitted::Ready(if self.cfg.breaches.empty_frame_on_drop {
                Finished::Frame(EncodedOut {
                    token: ask.token,
                    data: Vec::new(),
                    key: false,
                    qp: None,
                    timing: self.timing(false, gap, false),
                })
            } else {
                Finished::Dropped {
                    token: ask.token,
                    why: DropReason::Budget,
                }
            }));
        }
        if fits && engaged && faults.overshoot_permille > 0 {
            let over = u128::from(1000 + faults.overshoot_permille);
            bytes = usize::try_from(u128::try_from(ask.budget).unwrap_or(0) * over / 1000)
                .unwrap_or(usize::MAX);
        }

        // 编出来了
        if key {
            self.key_countdown = None;
        } else if let Some(c) = self.key_countdown {
            self.key_countdown = Some(c.saturating_sub(1));
        }
        let primed = self.first && faults.first_frame_prime.is_some_and(|p| ask.qp != Some(p));
        let index = self.index;
        let timing = self.timing(key, gap, primed);
        self.prev = Some(sample);
        self.regions.commit(&survey, qp, key);
        if let Some(feed) = &self.feed {
            feed.clear();
        }
        self.last_qp = Some(qp);
        self.first = false;
        self.index = self.index.saturating_add(1);
        let seed = ask.token.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ u64::from(qp) << 8
            ^ u64::from(index) << 16
            ^ u64::from(key);
        let head = StreamHead {
            key,
            // 内容描述自带版本；像素输入时读调用方拨的版本（没拨就是 0）
            version: described.map_or_else(
                || {
                    self.version
                        .as_ref()
                        .map_or(0, |v| v.load(Ordering::Relaxed))
                },
                |(version, _, _, _)| version,
            ),
            serial: index,
            qp,
        };
        let done = Finished::Frame(EncodedOut {
            token: ask.token,
            data: fake_stream(bytes, head, seed),
            key,
            qp: Some(qp),
            timing,
        });

        // 违约：吞帧、卡住：这一帧永远没有下落
        if let Some(n) = self.cfg.breaches.lose_frame_every {
            if n > 0 && self.index.is_multiple_of(n) {
                return Ok(Submitted::Pending);
            }
        }
        if let Some(stall) = faults.stall {
            if !self.stall_spent && index == stall.after_frames {
                self.stalled_since = Some(now);
                return Ok(Submitted::Pending);
            }
        }
        if faults.held_frames == 0 {
            return Ok(Submitted::Ready(done));
        }
        // 攒帧：超过 held_frames 的最老的那帧才出
        self.inflight.push_back(done);
        if self.inflight.len() > usize::from(faults.held_frames) {
            if let Some(oldest) = self.inflight.pop_front() {
                self.ready.push_back(oldest);
            }
        }
        Ok(Submitted::Pending)
    }

    fn poll(&mut self) -> Result<Option<Finished>, BackendError> {
        if let Some(err) = self.stall_timeout(self.clock.now_us()) {
            return Err(err);
        }
        Ok(self.ready.pop_front())
    }

    fn flush(&mut self) -> Result<(), BackendError> {
        if !self.cfg.breaches.flush_forgets {
            self.ready.append(&mut self.inflight);
        }
        Ok(())
    }
}

impl FakeBackend {
    /// 这一帧的各段时间：计算（关键帧翻倍，静止之后第一帧加上额外的）、等待、初始化。
    fn timing(&self, key: bool, gap: Option<u64>, primed: bool) -> Timing {
        if self.cfg.breaches.zero_timing {
            return Timing::default();
        }
        let base = self.cfg.compute_us.saturating_mul(if key { 2 } else { 1 });
        let slow = self
            .cfg
            .faults
            .slow_after_idle
            .zip(gap)
            .filter(|(s, gap)| *gap >= s.idle_us)
            .map_or(0, |(s, _)| s.extra_us);
        Timing {
            compute_us: base.saturating_add(slow),
            wait_us: self.cfg.wait_us,
            setup_us: if primed { PRIME_SETUP_US } else { 0 },
        }
    }
}

/// 假解码器：读假码流（[`FakeBackend`] 编的）开头的内容，交出 [`Picture::Described`]（显示的是源画面的第几版、QP 多少），
/// 不出像素。
///
/// **参考链和真解码器一样严：缺了参考帧就不出画面，要等关键帧。** 关键帧随时能接；增量帧只能接在上一帧后面
/// （帧序号差一），否则报错、并且之后的增量帧一律拒绝，直到下一个关键帧。不能出现“接不上也照样显示”：
/// 那样仿真里的花屏会被悄悄吞掉，量尺就失真了（真解码器接不上的增量帧要么报错、要么出花屏，两种都不能算好画面）。
/// 码流格式不对（太短、起始码不对、NAL 头和帧头说的关键帧对不上）也报错、等关键帧。
#[derive(Debug, Default)]
pub struct FakeDecoder {
    /// 最近一个成功解出的帧的序号；`None`：还没有，或者出过错，要等关键帧。
    last: Option<u32>,
    /// 解出的帧数。
    decoded: u32,
    /// 因为接不上（缺参考帧）或码流不对而拒绝的帧数。
    refused: u32,
}

impl FakeDecoder {
    /// 创建（等第一个关键帧）。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            last: None,
            decoded: 0,
            refused: 0,
        }
    }

    /// 解出了几帧。
    #[must_use]
    pub const fn decoded(&self) -> u32 {
        self.decoded
    }

    /// 拒绝了几帧（缺参考帧、码流不对）。
    #[must_use]
    pub const fn refused(&self) -> u32 {
        self.refused
    }

    fn refuse(&mut self, why: &'static str) -> Error {
        self.last = None;
        self.refused = self.refused.saturating_add(1);
        Error::Inconsistent(why)
    }
}

impl VideoDecoder for FakeDecoder {
    fn decode(&mut self, _header: &FrameHeader, _data: &[u8]) -> Result<Option<&Screen>, Error> {
        Err(Error::InvalidArgument(
            "假解码器不出像素：用 decode_picture 取描述",
        ))
    }

    fn screen(&self) -> Option<&Screen> {
        None
    }

    fn decode_picture(
        &mut self,
        header: &FrameHeader,
        data: &[u8],
    ) -> Result<Option<Picture>, Error> {
        let Some(head) = parse_head(data) else {
            return Err(self.refuse("假码流格式不对"));
        };
        if head.key != header.key {
            return Err(self.refuse("假码流的 NAL 头和帧头说的关键帧对不上"));
        }
        if !head.key && self.last != Some(head.serial.wrapping_sub(1)) {
            return Err(self.refuse("缺参考帧，等关键帧"));
        }
        self.last = Some(head.serial);
        self.decoded = self.decoded.saturating_add(1);
        Ok(Some(Picture::Described(Described {
            version: head.version,
            qp: head.qp,
            key: head.key,
            size: (header.width, header.height),
        })))
    }

    fn backend(&self) -> String {
        "假解码器".to_owned()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use peerdesk_core::video::Codec;

    const RATES: Rates = Rates {
        bps: 2_000_000,
        fps: 30,
    };
    const SIZE: (u16, u16) = (64, 64);

    fn planes(patch: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let y = (0..64usize * 64)
            .map(|i| {
                if patch != 0 && i % 64 < 8 && i / 64 < 8 {
                    patch
                } else {
                    u8::try_from((i % 64) * 3 % 200).unwrap()
                }
            })
            .collect();
        (y, vec![128; 32 * 32], vec![128; 32 * 32])
    }

    fn backend(cfg: FakeConfig) -> (FakeBackend, ManualClock) {
        let clock = ManualClock::new();
        let mut b = FakeBackend::with_default_model(cfg, Arc::new(clock.clone()));
        b.reconfigure(SIZE, RATES).unwrap();
        (b, clock)
    }

    fn ask(token: u64, qp: Option<u8>, budget: usize, keyframe: bool) -> FrameAsk {
        FrameAsk {
            token,
            budget,
            qp,
            keyframe,
        }
    }

    fn ready(got: Result<Submitted, BackendError>) -> Finished {
        match got.unwrap() {
            Submitted::Ready(f) => f,
            Submitted::Pending => panic!("应当当场有结果"),
        }
    }

    fn frame(f: Finished) -> EncodedOut {
        match f {
            Finished::Frame(out) => out,
            Finished::Dropped { .. } => panic!("应当编出帧"),
        }
    }

    fn i420<'a>(p: &'a (Vec<u8>, Vec<u8>, Vec<u8>)) -> FrameInput<'a> {
        FrameInput::I420 {
            y: &p.0,
            u: &p.1,
            v: &p.2,
        }
    }

    #[test]
    fn manual_clock_is_shared_between_clones() {
        let clock = ManualClock::new();
        let other = clock.clone();
        clock.advance_us(5);
        other.advance(Duration::from_millis(2));
        assert_eq!(clock.now_us(), 2_005);
    }

    /// 默认模型的形状：QP 每加 6 字节数约减半，关键帧远大于普通帧，变得越多越大，细节越多越大。
    #[test]
    fn default_model_has_the_expected_shape() {
        let m = DefaultByteModel;
        let base = ModelInput {
            qp: 20,
            key: true,
            pixels: 640 * 360,
            changed_permille: 1000,
            detail_permille: 150,
            prev_qp: None,
            serial: 0,
            regions: Regions::default(),
        };
        let key20 = m.bytes(&base);
        let key26 = m.bytes(&ModelInput { qp: 26, ..base });
        assert!(key26 * 2 <= key20 + key20 / 20 && key26 * 2 >= key20 - key20 / 20);
        let mut prev = usize::MAX;
        for qp in 0..=60 {
            let b = m.bytes(&ModelInput { qp, ..base });
            assert!(b <= prev, "QP {qp} 的字节数不该比更小的 QP 多");
            prev = b;
        }
        let small = ModelInput {
            key: false,
            changed_permille: 5,
            ..base
        };
        assert!(m.bytes(&small) < key20 / 50);
        assert!(
            m.bytes(&ModelInput {
                changed_permille: 500,
                ..small
            }) > m.bytes(&small)
        );
        assert!(
            m.bytes(&ModelInput {
                detail_permille: 900,
                ..base
            }) > key20
        );
        assert!(m.bytes(&ModelInput { pixels: 0, ..small }) >= 24);
    }

    /// 字节模型要的两样上下文由后端给：上一帧编出来的 QP、配置之后的第几帧；重新配置后从头数。
    #[test]
    fn the_model_is_told_the_previous_qp_and_the_frame_serial() {
        type Seen = Arc<std::sync::Mutex<Vec<(Option<u8>, u32)>>>;
        #[derive(Debug)]
        struct Recorder(Seen);
        impl ByteModel for Recorder {
            fn bytes(&self, input: &ModelInput) -> usize {
                self.0.lock().unwrap().push((input.prev_qp, input.serial));
                100
            }
        }
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut b = FakeBackend::new(
            FakeConfig::default(),
            Arc::new(ManualClock::new()),
            Box::new(Recorder(Arc::clone(&seen))),
        );
        b.reconfigure(SIZE, RATES).unwrap();
        let p = planes(0);
        for (i, qp) in [30u8, 20, 20].into_iter().enumerate() {
            let _ = ready(b.submit(i420(&p), ask(i as u64, Some(qp), 1 << 20, false)));
        }
        b.reconfigure(SIZE, RATES).unwrap();
        let _ = ready(b.submit(i420(&p), ask(9, Some(26), 1 << 20, false)));
        assert_eq!(
            *seen.lock().unwrap(),
            [(None, 0), (Some(30), 1), (Some(20), 2), (None, 0)]
        );
    }

    /// 同一配置、同一输入序列，输出逐字节相同；数据是 Annex B 的样子；不同的 token 数据不同。
    #[test]
    fn same_config_and_inputs_give_identical_bytes() {
        let run = || {
            let (mut b, clock) = backend(FakeConfig::default());
            let mut out = Vec::new();
            for i in 0..6u64 {
                clock.advance_us(33_000);
                let p = planes(u8::try_from(i * 7).unwrap());
                out.push(frame(ready(
                    b.submit(i420(&p), ask(i, Some(26), 1 << 20, i == 3)),
                )));
            }
            out
        };
        let (a, b) = (run(), run());
        assert_eq!(a, b);
        assert!(a[0].key && a[3].key && !a[1].key);
        assert_eq!(&a[0].data[..5], &[0, 0, 0, 1, 0x65]);
        assert_eq!(&a[1].data[..5], &[0, 0, 0, 1, 0x41]);
        assert_ne!(a[1].data, a[2].data);
        assert!(a.iter().all(|f| f.timing.compute_us > 0));
    }

    /// 内容描述和像素是同一幅画面的两种交法：抽样、细节取自同一个算法时，编出来的字节逐帧相同，
    /// 版本号从描述里来。没声明收内容描述的后端拒收，状态不变；还没配置时也拒收。
    #[test]
    fn a_described_frame_encodes_exactly_like_its_pixels() {
        let mut cfg = FakeConfig::default();
        cfg.inputs.described = true;
        let (mut by_pixels, _) = backend(cfg.clone());
        let (mut by_desc, _) = backend(cfg.clone());
        for i in 0..6u64 {
            let p = planes(u8::try_from(i / 2 * 9).unwrap());
            let sample = content::sample_luma(&p.0);
            let described = FrameInput::Described {
                version: 0,
                sample: &sample,
                detail_permille: content::detail_permille(&p.0),
                mb_edges: &content::mb_edges(&p.0, SIZE),
            };
            let a = frame(ready(
                by_pixels.submit(i420(&p), ask(i, Some(30), 1 << 20, i == 4)),
            ));
            let b = frame(ready(
                by_desc.submit(described, ask(i, Some(30), 1 << 20, i == 4)),
            ));
            assert_eq!(a, b, "第 {i} 帧");
        }
        // 版本号写进码流，假解码器读得回
        let p = planes(77);
        let sample = content::sample_luma(&p.0);
        let tagged = FrameInput::Described {
            version: 42,
            sample: &sample,
            detail_permille: 100,
            mb_edges: &[],
        };
        assert_eq!(format!("{tagged:?}"), "内容描述");
        let out = frame(ready(
            by_desc.submit(tagged, ask(9, Some(30), 1 << 20, true)),
        ));
        assert_eq!(parse_head(&out.data).map(|h| h.version), Some(42));
        // 没声明的后端不收；没配置尺寸的也不收
        let (mut plain, _) = backend(FakeConfig::default());
        assert!(matches!(
            plain.submit(tagged, ask(1, Some(30), 1 << 20, false)),
            Err(BackendError::Misuse(_))
        ));
        assert!(
            frame(ready(
                plain.submit(i420(&p), ask(2, Some(30), 1 << 20, false))
            ))
            .key
        );
        let mut unset = FakeBackend::with_default_model(cfg, Arc::new(ManualClock::new()));
        assert!(matches!(
            unset.submit(tagged, ask(1, Some(30), 1 << 20, false)),
            Err(BackendError::Misuse(_))
        ));
    }

    /// 默认假后端守约：属性诚实、按 QP 编（QP 大字节少）、同步、从不丢帧；用法错不改变状态。
    #[test]
    fn default_backend_is_contract_abiding() {
        let (mut b, _) = backend(FakeConfig::default());
        let traits = b.info().traits;
        assert_eq!(traits.rate_control, RateControl::PerFrameQp);
        assert_eq!((traits.max_held, traits.keyframe_within), (0, 1));
        assert!(!traits.scene_keyframes && traits.first_frame_qp.is_none());
        assert_eq!(b.info().name, "假后端");
        assert_eq!(b.size(), Some(SIZE));
        assert_eq!(b.rates(), RATES);
        let p = planes(0);
        // 没给 QP、不收的形式、尺寸不对：用法错
        assert!(matches!(
            b.submit(i420(&p), ask(1, None, 1000, false)),
            Err(BackendError::Misuse(_))
        ));
        assert!(matches!(
            b.submit(FrameInput::Gpu(&gpu_frame()), ask(1, Some(30), 1000, false)),
            Err(BackendError::Misuse(_))
        ));
        let short = FrameInput::I420 {
            y: &p.0[..100],
            u: &p.1,
            v: &p.2,
        };
        assert!(matches!(
            b.submit(short, ask(1, Some(30), 1000, false)),
            Err(BackendError::Misuse(_))
        ));
        assert_eq!(b.size(), Some(SIZE));
        let fine = frame(ready(b.submit(i420(&p), ask(1, Some(20), 10, false))));
        let coarse = frame(ready(b.submit(i420(&p), ask(2, Some(44), 10, true))));
        assert!(fine.key, "第一帧是关键帧");
        assert!(coarse.key && coarse.data.len() < fine.data.len());
        assert_eq!((fine.qp, coarse.qp), (Some(20), Some(44)));
        assert_eq!(b.poll().unwrap(), None);
        b.flush().unwrap();
        b.set_rates(Rates { bps: 1, fps: 1 }).unwrap();
        // 重新配置：奇数、0 不行；正常的话从头来，又是关键帧
        assert!(matches!(
            b.reconfigure((63, 64), RATES),
            Err(BackendError::Misuse(_))
        ));
        assert!(matches!(
            b.reconfigure((0, 64), RATES),
            Err(BackendError::Misuse(_))
        ));
        assert!(matches!(
            b.reconfigure((64, 0), RATES),
            Err(BackendError::Misuse(_))
        ));
        let r = b.reconfigure(SIZE, RATES).unwrap();
        assert!(r.rebuilt && r.keyframe_next);
        assert!(frame(ready(b.submit(i420(&p), ask(3, Some(20), 10, false)))).key);
        // 没配置就交帧
        let mut fresh =
            FakeBackend::with_default_model(FakeConfig::default(), Arc::new(ManualClock::new()));
        assert_eq!(fresh.size(), None);
        assert!(matches!(
            fresh.submit(i420(&p), ask(1, Some(30), 1000, false)),
            Err(BackendError::Misuse(_))
        ));
        assert!(!fresh.accepts(&gpu_frame()));
    }

    fn gpu_frame() -> peerdesk_core::frame::TestGpuFrame {
        peerdesk_core::frame::TestGpuFrame::new(
            peerdesk_core::frame::Nv12::new(64, 64, vec![16; 64 * 64 * 3 / 2]).unwrap(),
        )
    }

    /// NV12 收（配置里打开才收）；只收 NV12 时 I420 回用法错。
    #[test]
    fn input_forms_follow_the_config() {
        let p = planes(0);
        let nv12: Vec<u8> =
            p.0.iter()
                .copied()
                .chain(p.1.iter().zip(&p.2).flat_map(|(a, b)| [*a, *b]))
                .collect();
        let (mut b, _) = backend(FakeConfig::default());
        assert!(
            frame(ready(b.submit(
                FrameInput::Nv12(&nv12),
                ask(1, Some(30), 1000, false)
            )))
            .key
        );
        assert!(matches!(
            b.submit(
                FrameInput::Nv12(&nv12[..100]),
                ask(2, Some(30), 1000, false)
            ),
            Err(BackendError::Misuse(_))
        ));
        let (mut only_nv12, _) = backend(FakeConfig {
            inputs: Inputs {
                i420: false,
                nv12: true,
                gpu: false,
                described: false,
            },
            ..FakeConfig::default()
        });
        assert!(matches!(
            only_nv12.submit(i420(&p), ask(1, Some(30), 1000, false)),
            Err(BackendError::Misuse(_))
        ));
        let (mut only_i420, _) = backend(FakeConfig {
            inputs: Inputs {
                i420: true,
                nv12: false,
                gpu: false,
                described: false,
            },
            ..FakeConfig::default()
        });
        assert!(matches!(
            only_i420.submit(FrameInput::Nv12(&nv12), ask(1, Some(30), 1000, false)),
            Err(BackendError::Misuse(_))
        ));
    }

    /// 码控窗口：自己挑 QP 放进预算；放不下按毛病表丢（只丢关键帧 / 全丢），回 Budget。
    #[test]
    fn window_mode_fits_the_budget_or_drops() {
        let (mut b, _) = backend(FakeConfig::window());
        let p = planes(0);
        let roomy = frame(ready(b.submit(i420(&p), ask(1, None, 1 << 20, false))));
        assert!(roomy.qp == Some(MODEL_MIN_QP) && roomy.data.len() <= 1 << 20);
        let tight = frame(ready(
            b.submit(i420(&p), ask(2, Some(10), roomy.data.len() / 4, true)),
        ));
        assert!(tight.data.len() <= roomy.data.len() / 4 && tight.qp > roomy.qp);
        let dropped = ready(b.submit(i420(&p), ask(3, None, 1, true)));
        assert_eq!(
            dropped,
            Finished::Dropped {
                token: 3,
                why: DropReason::Budget
            }
        );
        // 只丢要求的关键帧：普通帧放不下就交超预算的
        let (mut vt, _) = backend(FakeConfig {
            faults: Faults {
                drop_requested_keyframe: true,
                ..Faults::default()
            },
            ..FakeConfig::window()
        });
        let first = frame(ready(vt.submit(i420(&p), ask(1, None, 1 << 20, false))));
        assert!(first.key);
        let over = frame(ready(vt.submit(i420(&p), ask(2, None, 1, false))));
        assert!(over.data.len() > 1 && !over.key);
        assert_eq!(
            ready(vt.submit(i420(&p), ask(3, None, 1, true))),
            Finished::Dropped {
                token: 3,
                why: DropReason::Budget
            }
        );
        // 丢了之后接着要：放得下就给
        assert!(frame(ready(vt.submit(i420(&p), ask(4, None, 1 << 20, true)))).key);
    }

    /// 超发：码控够不着预算时，字节数是预算的 (1000 + 比例) / 1000。
    #[test]
    fn overshoot_only_when_the_rate_control_is_engaged() {
        let (mut b, _) = backend(FakeConfig {
            faults: Faults {
                overshoot_permille: 300,
                ..Faults::default()
            },
            ..FakeConfig::window()
        });
        let p = planes(0);
        let roomy = frame(ready(b.submit(i420(&p), ask(1, None, 1 << 20, false))));
        assert!(roomy.data.len() < 1 << 20, "放得下、没卡住码控，不超发");
        let tight = frame(ready(
            b.submit(i420(&p), ask(2, None, roomy.data.len() / 2, true)),
        ));
        assert_eq!(tight.data.len(), roomy.data.len() / 2 * 13 / 10);
    }

    /// 攒帧：交了 held_frames 帧之后才出第一个；flush 全部交出；顺序按交帧。
    #[test]
    fn held_frames_come_out_late_and_flush_releases_them() {
        let (mut b, _) = backend(FakeConfig {
            faults: Faults {
                held_frames: 2,
                ..Faults::default()
            },
            ..FakeConfig::default()
        });
        assert_eq!(b.info().traits.max_held, 2);
        let p = planes(0);
        for t in 0..2 {
            assert_eq!(
                b.submit(i420(&p), ask(t, Some(30), 1000, false)).unwrap(),
                Submitted::Pending
            );
            assert_eq!(b.poll().unwrap(), None);
        }
        assert_eq!(
            b.submit(i420(&p), ask(2, Some(30), 1000, false)).unwrap(),
            Submitted::Pending
        );
        assert_eq!(b.poll().unwrap().map(|f| f.token()), Some(0));
        assert_eq!(b.poll().unwrap(), None);
        b.flush().unwrap();
        assert_eq!(b.poll().unwrap().map(|f| f.token()), Some(1));
        assert_eq!(b.poll().unwrap().map(|f| f.token()), Some(2));
        assert_eq!(b.poll().unwrap(), None);
    }

    /// 静止后第一帧慢：只有隔得够久的那一帧多出额外的计算时间；时间来自注入的时钟。
    #[test]
    fn the_first_frame_after_idle_is_slow() {
        let (mut b, clock) = backend(FakeConfig {
            faults: Faults {
                slow_after_idle: Some(SlowAfterIdle {
                    idle_us: 1_000_000,
                    extra_us: 30_000,
                }),
                ..Faults::default()
            },
            wait_us: 700,
            ..FakeConfig::default()
        });
        let p = planes(0);
        let steady = frame(ready(b.submit(i420(&p), ask(0, Some(30), 1000, false))));
        clock.advance_us(33_000);
        let again = frame(ready(b.submit(i420(&p), ask(1, Some(30), 1000, false))));
        clock.advance(Duration::from_secs(5));
        let slow = frame(ready(b.submit(i420(&p), ask(2, Some(30), 1000, false))));
        assert_eq!(again.timing.compute_us, 2_000);
        assert_eq!(steady.timing.wait_us, 700);
        assert_eq!(slow.timing.compute_us, 32_000);
    }

    /// 整屏突变自己出关键帧（声明与否在属性里）；局部变化不出；固定 GOP 是违约，按帧数出。
    #[test]
    fn unasked_keyframes_follow_the_table() {
        let flat = |v: u8| (vec![v; 64 * 64], vec![128; 32 * 32], vec![128; 32 * 32]);
        for (scene, declared) in [
            (SceneKeyframes::Declared, true),
            (SceneKeyframes::Undeclared, false),
        ] {
            let (mut b, _) = backend(FakeConfig {
                faults: Faults {
                    scene_keyframes: scene,
                    ..Faults::default()
                },
                ..FakeConfig::default()
            });
            assert_eq!(b.info().traits.scene_keyframes, declared);
            assert!(
                frame(ready(
                    b.submit(i420(&flat(50)), ask(0, Some(30), 1000, false))
                ))
                .key
            );
            assert!(
                !frame(ready(
                    b.submit(i420(&flat(50)), ask(1, Some(30), 1000, false))
                ))
                .key
            );
            assert!(
                frame(ready(
                    b.submit(i420(&flat(200)), ask(2, Some(30), 1000, false))
                ))
                .key
            );
        }
        let (mut gop, _) = backend(FakeConfig {
            breaches: Breaches {
                periodic_keyframes: Some(3),
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        let keys: Vec<bool> = (0..7)
            .map(|t| {
                frame(ready(
                    gop.submit(i420(&planes(0)), ask(t, Some(30), 1000, false)),
                ))
                .key
            })
            .collect();
        assert_eq!(keys, [true, false, false, true, false, false, true]);
    }

    /// 要了关键帧晚几帧给：属性里声明的就是 1 + 延迟。
    #[test]
    fn a_requested_keyframe_can_arrive_late() {
        let (mut b, _) = backend(FakeConfig {
            faults: Faults {
                keyframe_delay: 2,
                ..Faults::default()
            },
            ..FakeConfig::default()
        });
        assert_eq!(b.info().traits.keyframe_within, 3);
        let p = planes(0);
        let keys: Vec<bool> = (0..6)
            .map(|t| frame(ready(b.submit(i420(&p), ask(t, Some(30), 1000, t == 1)))).key)
            .collect();
        assert_eq!(keys, [true, false, false, true, false, false]);
        // 忽略要求（违约）：永远不给
        let (mut ignore, _) = backend(FakeConfig {
            breaches: Breaches {
                ignore_keyframe_requests: true,
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        let keys: Vec<bool> = (0..4)
            .map(|t| {
                frame(ready(
                    ignore.submit(i420(&p), ask(t, Some(30), 1000, t == 1)),
                ))
                .key
            })
            .collect();
        assert_eq!(keys, [true, false, false, false]);
    }

    /// 改码率：守约的改了就生效；毛病表里“改不了”的回失败，状态不变；违约的顺带出关键帧。
    #[test]
    fn set_rates_follows_the_table() {
        let p = planes(0);
        let (mut stuck, _) = backend(FakeConfig {
            faults: Faults {
                set_rates_fails: true,
                ..Faults::default()
            },
            ..FakeConfig::default()
        });
        assert!(matches!(
            stuck.set_rates(Rates { bps: 5, fps: 5 }),
            Err(BackendError::Rejected(_))
        ));
        assert_eq!((stuck.rates(), stuck.size()), (RATES, Some(SIZE)));
        let (mut rude, _) = backend(FakeConfig {
            breaches: Breaches {
                set_rates_forces_keyframe: true,
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        assert!(frame(ready(rude.submit(i420(&p), ask(0, Some(30), 1000, false)))).key);
        assert!(!frame(ready(rude.submit(i420(&p), ask(1, Some(30), 1000, false)))).key);
        rude.set_rates(Rates { bps: 5, fps: 5 }).unwrap();
        assert!(frame(ready(rude.submit(i420(&p), ask(2, Some(30), 1000, false)))).key);
    }

    /// 不收帧：每 N 次有一次，帧没进编码器。
    #[test]
    fn not_accepting_drops_every_nth_call() {
        let (mut b, _) = backend(FakeConfig {
            faults: Faults {
                not_accepting_every: Some(3),
                ..Faults::default()
            },
            ..FakeConfig::default()
        });
        let p = planes(0);
        let kinds: Vec<bool> = (0..6)
            .map(|t| {
                matches!(
                    ready(b.submit(i420(&p), ask(t, Some(30), 1000, false))),
                    Finished::Dropped {
                        why: DropReason::NotAccepting,
                        ..
                    }
                )
            })
            .collect();
        assert_eq!(kinds, [false, false, true, false, false, true]);
    }

    /// 卡住：这一帧不出，之后不收帧，过了时限报错并复位（只卡一次）；到时限也可以由 poll 报。
    #[test]
    fn a_stalled_encoder_errors_out_and_resets() {
        let p = planes(0);
        for via_poll in [false, true] {
            let (mut b, clock) = backend(FakeConfig {
                faults: Faults {
                    stall: Some(Stall {
                        after_frames: 1,
                        limit_us: 500_000,
                    }),
                    ..Faults::default()
                },
                ..FakeConfig::default()
            });
            assert_eq!(b.info().traits.max_held, 1);
            assert!(
                !frame(ready(b.submit(i420(&p), ask(0, Some(30), 1000, false))))
                    .data
                    .is_empty()
            );
            assert_eq!(
                b.submit(i420(&p), ask(1, Some(30), 1000, false)).unwrap(),
                Submitted::Pending
            );
            clock.advance_us(100_000);
            assert_eq!(b.poll().unwrap(), None);
            assert!(matches!(
                ready(b.submit(i420(&p), ask(2, Some(30), 1000, false))),
                Finished::Dropped {
                    why: DropReason::NotAccepting,
                    ..
                }
            ));
            clock.advance_us(400_000);
            if via_poll {
                assert!(matches!(b.poll(), Err(BackendError::Failed(_))));
            } else {
                assert!(matches!(
                    b.submit(i420(&p), ask(3, Some(30), 1000, false)),
                    Err(BackendError::Failed(_))
                ));
            }
            assert_eq!(b.size(), None, "出错之后复位");
            b.reconfigure(SIZE, RATES).unwrap();
            for t in 4..8 {
                assert!(
                    !frame(ready(b.submit(i420(&p), ask(t, Some(30), 1000, false))))
                        .data
                        .is_empty()
                );
            }
        }
        // 违约：出错不复位
        let (mut keep, clock) = backend(FakeConfig {
            faults: Faults {
                stall: Some(Stall {
                    after_frames: 0,
                    limit_us: 10,
                }),
                ..Faults::default()
            },
            breaches: Breaches {
                keep_state_after_error: true,
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        assert_eq!(
            keep.submit(i420(&p), ask(0, Some(30), 1000, false))
                .unwrap(),
            Submitted::Pending
        );
        clock.advance_us(10);
        assert!(keep.poll().is_err());
        assert_eq!(keep.size(), Some(SIZE));
        // 重新配置也把卡住的换掉了
        let (mut re, _) = backend(FakeConfig {
            faults: Faults {
                stall: Some(Stall {
                    after_frames: 0,
                    limit_us: u64::MAX,
                }),
                ..Faults::default()
            },
            ..FakeConfig::default()
        });
        assert_eq!(
            re.submit(i420(&p), ask(0, Some(30), 1000, false)).unwrap(),
            Submitted::Pending
        );
        re.reconfigure(SIZE, RATES).unwrap();
        assert!(
            !frame(ready(re.submit(i420(&p), ask(1, Some(30), 1000, false))))
                .data
                .is_empty()
        );
    }

    /// 首帧 QP 限制：要的 QP 不是限制的那个，多花一次初始化（记在 setup）；是的话不花。
    #[test]
    fn first_frame_prime_costs_setup_time() {
        let (mut b, _) = backend(FakeConfig {
            faults: Faults {
                first_frame_prime: Some(26),
                ..Faults::default()
            },
            ..FakeConfig::default()
        });
        assert_eq!(b.info().traits.first_frame_qp, Some(26));
        let p = planes(0);
        let first = frame(ready(b.submit(i420(&p), ask(0, Some(40), 1000, false))));
        let next = frame(ready(b.submit(i420(&p), ask(1, Some(40), 1000, false))));
        assert_eq!(
            (first.timing.setup_us, next.timing.setup_us),
            (PRIME_SETUP_US, 0)
        );
        b.reconfigure(SIZE, RATES).unwrap();
        assert_eq!(
            frame(ready(b.submit(i420(&p), ask(2, Some(26), 1000, false))))
                .timing
                .setup_us,
            0
        );
    }

    /// 违约一组：吞帧、空帧、第一帧不是关键帧、悄悄换尺寸、隔太久出错、不报时间、flush 不交出、谎报属性。
    #[test]
    fn breaches_behave_as_described() {
        let p = planes(0);
        // 吞帧：第 N 帧回 Pending 又永远不出
        let (mut lose, _) = backend(FakeConfig {
            breaches: Breaches {
                lose_frame_every: Some(2),
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        assert!(matches!(
            lose.submit(i420(&p), ask(0, Some(30), 1000, false))
                .unwrap(),
            Submitted::Ready(_)
        ));
        assert_eq!(
            lose.submit(i420(&p), ask(1, Some(30), 1000, false))
                .unwrap(),
            Submitted::Pending
        );
        lose.flush().unwrap();
        assert_eq!(lose.poll().unwrap(), None);
        // 空帧冒充
        let (mut empty, _) = backend(FakeConfig {
            breaches: Breaches {
                empty_frame_on_drop: true,
                ..Breaches::default()
            },
            ..FakeConfig::window()
        });
        let e = frame(ready(empty.submit(i420(&p), ask(0, None, 1, false))));
        assert!(e.data.is_empty() && !e.key);
        // 第一帧不是关键帧；要了就是
        let (mut nokey, _) = backend(FakeConfig {
            breaches: Breaches {
                first_frame_not_key: true,
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        assert!(!frame(ready(nokey.submit(i420(&p), ask(0, Some(30), 1000, false)))).key);
        assert!(frame(ready(nokey.submit(i420(&p), ask(1, Some(30), 1000, true)))).key);
        // 悄悄换尺寸：I420 和 NV12 都推得出新高度
        let (mut resize, _) = backend(FakeConfig {
            breaches: Breaches {
                resize_silently: true,
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        let small = (vec![9u8; 64 * 32], vec![128; 32 * 16], vec![128; 32 * 16]);
        assert!(resize
            .submit(i420(&small), ask(0, Some(30), 1000, false))
            .is_ok());
        assert_eq!(resize.size(), Some((64, 32)));
        let nv12 = vec![9u8; 64 * 16 * 3 / 2];
        assert!(resize
            .submit(FrameInput::Nv12(&nv12), ask(1, Some(30), 1000, false))
            .is_ok());
        assert_eq!(resize.size(), Some((64, 16)));
        assert!(resize
            .submit(FrameInput::Nv12(&[1, 2]), ask(2, Some(30), 1000, false))
            .is_ok());
        assert_eq!(resize.size(), Some((64, 2)));
        // 隔太久出错
        let (mut idle, clock) = backend(FakeConfig {
            breaches: Breaches {
                error_after_idle_us: Some(1_000_000),
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        idle.submit(i420(&p), ask(0, Some(30), 1000, false))
            .unwrap();
        clock.advance_us(999_999);
        idle.submit(i420(&p), ask(1, Some(30), 1000, false))
            .unwrap();
        clock.advance_us(1_000_000);
        assert!(matches!(
            idle.submit(i420(&p), ask(2, Some(30), 1000, false)),
            Err(BackendError::Failed(_))
        ));
        assert_eq!(idle.size(), None);
        // 不报时间
        let (mut zero, _) = backend(FakeConfig {
            breaches: Breaches {
                zero_timing: true,
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        assert_eq!(
            frame(ready(zero.submit(i420(&p), ask(0, Some(30), 1000, false)))).timing,
            Timing::default()
        );
        // flush 不交出
        let (mut forget, _) = backend(FakeConfig {
            faults: Faults {
                held_frames: 1,
                ..Faults::default()
            },
            breaches: Breaches {
                flush_forgets: true,
                ..Breaches::default()
            },
            ..FakeConfig::default()
        });
        forget
            .submit(i420(&p), ask(0, Some(30), 1000, false))
            .unwrap();
        forget.flush().unwrap();
        assert_eq!(forget.poll().unwrap(), None);
        // 谎报属性
        let lie = BackendTraits {
            max_held: 9,
            ..FakeConfig::default().honest_traits()
        };
        let liar = FakeBackend::with_default_model(
            FakeConfig {
                declared: Some(lie),
                ..FakeConfig::default()
            },
            Arc::new(ManualClock::new()),
        );
        assert_eq!(liar.info().traits.max_held, 9);
    }

    /// 编一帧，交出码流和它的帧头。
    fn encode_one(
        b: &mut FakeBackend,
        token: u64,
        qp: u8,
        keyframe: bool,
    ) -> (FrameHeader, Vec<u8>) {
        let p = planes(0);
        let Finished::Frame(f) = ready(b.submit(i420(&p), ask(token, Some(qp), 100_000, keyframe)))
        else {
            panic!("应当编出帧");
        };
        let header = FrameHeader {
            codec: Codec::H264,
            key: f.key,
            width: SIZE.0,
            height: SIZE.1,
        };
        (header, f.data)
    }

    fn described(p: Option<Picture>) -> Described {
        match p {
            Some(Picture::Described(d)) => d,
            other => panic!("应当是描述，不是 {other:?}"),
        }
    }

    #[test]
    fn fake_decoder_follows_the_reference_chain_like_the_real_one() {
        let (mut b, _) = backend(FakeConfig::like_openh264());
        let mut dec = FakeDecoder::new();
        let frames: Vec<_> = (0..8u64)
            .map(|i| encode_one(&mut b, i, 30, false))
            .collect();
        // 第一帧是关键帧（新配置），接着的增量帧一帧接一帧
        assert!(frames[0].0.key && !frames[1].0.key);
        for (h, d) in &frames[..3] {
            assert!(dec.decode_picture(h, d).unwrap().is_some());
        }
        // 丢了第 3 帧：第 4 帧接不上，不出画面
        assert!(dec.decode_picture(&frames[4].0, &frames[4].1).is_err());
        // 之后的增量帧哪怕恰好接得上第 4 帧，也一律拒绝：要等关键帧
        assert!(dec.decode_picture(&frames[5].0, &frames[5].1).is_err());
        assert!(dec.decode_picture(&frames[6].0, &frames[6].1).is_err());
        assert_eq!((dec.decoded(), dec.refused()), (3, 3));
        // 关键帧随时能接，之后的增量帧又能解
        let key = encode_one(&mut b, 8, 30, true);
        assert!(key.0.key);
        assert!(dec.decode_picture(&key.0, &key.1).unwrap().is_some());
        let next = encode_one(&mut b, 9, 30, false);
        assert!(dec.decode_picture(&next.0, &next.1).unwrap().is_some());
        assert_eq!((dec.decoded(), dec.refused()), (5, 3));
    }

    #[test]
    fn fake_decoder_refuses_a_stream_that_starts_without_a_keyframe_and_garbage() {
        let (mut b, _) = backend(FakeConfig::like_openh264());
        let first = encode_one(&mut b, 0, 30, false);
        let second = encode_one(&mut b, 1, 30, false);
        let mut dec = FakeDecoder::new();
        // 没见过关键帧就来了增量帧
        assert!(dec.decode_picture(&second.0, &second.1).is_err());
        // 不是假码流 / 太短 / 帧头说是关键帧、NAL 头不是
        assert!(dec.decode_picture(&first.0, &[1, 2, 3]).is_err());
        let mut bad_start = first.1.clone();
        bad_start[0] = 9;
        assert!(dec.decode_picture(&first.0, &bad_start).is_err());
        let wrong_key = FrameHeader {
            key: false,
            ..first.0
        };
        assert!(dec.decode_picture(&wrong_key, &first.1).is_err());
        assert_eq!(dec.decoded(), 0);
        assert_eq!(dec.refused(), 4);
        // 不出像素
        assert!(dec.decode(&first.0, &first.1).is_err());
        assert!(dec.screen().is_none());
        assert_eq!(dec.backend(), "假解码器");
        // 正常的关键帧能接
        assert!(dec.decode_picture(&first.0, &first.1).unwrap().is_some());
    }

    #[test]
    fn fake_decoder_hands_over_version_qp_and_size() {
        let tag = Arc::new(AtomicU64::new(7));
        let clock = ManualClock::new();
        let mut b = FakeBackend::with_default_model(FakeConfig::like_openh264(), Arc::new(clock))
            .with_version_tag(tag.clone());
        b.reconfigure(SIZE, RATES).unwrap();
        let mut dec = FakeDecoder::new();
        let (h, d) = encode_one(&mut b, 0, 33, false);
        assert!(d.len() >= STREAM_HEAD_LEN);
        assert_eq!(
            described(dec.decode_picture(&h, &d).unwrap()),
            Described {
                version: 7,
                qp: 33,
                key: true,
                size: SIZE
            }
        );
        // 内容变了一版，下一帧写新的版本；补清晰（版本不变）QP 变细
        tag.store(8, Ordering::Relaxed);
        let (h, d) = encode_one(&mut b, 1, 40, false);
        let got = described(dec.decode_picture(&h, &d).unwrap());
        assert_eq!((got.version, got.qp, got.key), (8, 40, false));
        let (h, d) = encode_one(&mut b, 2, 20, false);
        let got = described(dec.decode_picture(&h, &d).unwrap());
        assert_eq!((got.version, got.qp), (8, 20));
    }

    #[test]
    fn fake_decoder_through_the_decode_worker_waits_for_a_keyframe() {
        use peerdesk_core::media::{DecodeJob, DecodeWorker};
        let (mut b, _) = backend(FakeConfig::like_openh264());
        let mut worker = DecodeWorker::new(Box::new(FakeDecoder::new()));
        let job = |frame_id: u32, (header, data): (FrameHeader, Vec<u8>)| DecodeJob {
            frame_id,
            header,
            data,
        };
        let f0 = encode_one(&mut b, 0, 30, false);
        let _lost = encode_one(&mut b, 1, 30, false);
        let f2 = encode_one(&mut b, 2, 30, false);
        let f3 = encode_one(&mut b, 3, 30, false);
        assert!(worker.decode(&job(0, f0)).unwrap().is_some());
        assert!(worker.decode(&job(2, f2)).is_err());
        assert!(worker.decode(&job(3, f3)).is_err());
        let key = encode_one(&mut b, 4, 30, true);
        assert!(worker.decode(&job(4, key)).unwrap().is_some());
    }

    #[test]
    fn like_openh264_declares_what_openh264_declares() {
        let fake = FakeBackend::with_default_model(
            FakeConfig::like_openh264(),
            Arc::new(ManualClock::new()),
        );
        let real = crate::soft::OpenH264Backend::new();
        assert_eq!(fake.info().traits, real.info().traits);
    }
    type Seen = Arc<std::sync::Mutex<Vec<ModelInput>>>;

    /// 把每一帧的模型输入记下来，字节数固定。
    #[derive(Debug)]
    struct Probe(Seen);
    impl ByteModel for Probe {
        fn bytes(&self, input: &ModelInput) -> usize {
            self.0.lock().unwrap().push(*input);
            100
        }
    }

    fn probed(cfg: FakeConfig, feed: Option<&ChangeFeed>) -> (FakeBackend, Seen) {
        let seen = Seen::default();
        let mut b = FakeBackend::new(
            cfg,
            Arc::new(ManualClock::new()),
            Box::new(Probe(Arc::clone(&seen))),
        );
        if let Some(f) = feed {
            b = b.with_change_feed(f.clone());
        }
        b.reconfigure(SIZE, RATES).unwrap();
        (b, seen)
    }

    fn last(seen: &Seen) -> ModelInput {
        *seen.lock().unwrap().last().unwrap()
    }

    fn changes(
        dirty: &[Rect],
        moves: &[peerdesk_core::changes::Move],
    ) -> peerdesk_core::changes::Changes {
        peerdesk_core::changes::Changes {
            size: SIZE,
            dirty: dirty.to_vec(),
            moves: moves.to_vec(),
        }
    }

    /// 区域类输入的全过程（64×64 = 4×4 个宏块）：刚配置整屏是新的；画质表按“编码的设成这一帧的 QP、静止的只会变细”走；
    /// 画质差分布和连续次数；跳过的版本累计；关键帧整屏重设；重新配置作废。
    #[test]
    fn region_inputs_follow_the_change_feed_and_the_quality_table() {
        use peerdesk_core::changes::Move;
        let feed = ChangeFeed::default();
        let (mut b, seen) = probed(FakeConfig::default(), Some(&feed));
        let p = planes(0);
        let go = |b: &mut FakeBackend, i: u64, qp: u8, key: bool| {
            frame(ready(b.submit(i420(&p), ask(i, Some(qp), 1 << 20, key))));
            last(&seen).regions
        };
        // 第一帧：整屏都是新的，没有静止宏块
        let r = go(&mut b, 0, 30, false);
        assert_eq!((r.dirty_px, r.moved_px, r.full_move), (64 * 64, 0, false));
        assert_eq!((r.dirty_mbs, r.moved_mbs), (16, 0), "整屏 4×4 个宏块");
        assert_eq!(r.static_gap, [0; GAP_BINS]);
        // 补清晰：什么都没变，16 个静止宏块都是 QP 30，比这一帧（20）粗 10 档
        let r = go(&mut b, 1, 20, false);
        assert_eq!((r.dirty_px, r.moved_px, r.dirty_detail_permille), (0, 0, 0));
        assert_eq!(r.static_gap.iter().sum::<u32>(), 16);
        assert_eq!(r.static_gap[9], 16);
        assert_eq!(r.repeat, 0, "没有静止宏块已经在 QP 20 上");
        // 再按 20 编：都补到 20 了，连着编了 1 帧；比 20 更粗的没有
        let r = go(&mut b, 2, 20, false);
        assert_eq!(r.static_gap, [0; GAP_BINS]);
        assert_eq!(r.repeat, 1);
        assert_eq!(go(&mut b, 3, 20, false).repeat, 2);
        // 变化：脏区域（碰到宏块 (0,0) 和 (1,0)）和移动（目的地 (16,16)–(48,32)，碰到 2×1 个宏块，和脏区域不重叠）
        feed.push(&changes(
            &[Rect::new(10, 2, 10, 4)],
            &[Move {
                from: (0, 0),
                to: Rect::new(16, 16, 32, 16),
            }],
        ));
        let r = go(&mut b, 4, 28, false);
        assert_eq!((r.dirty_px, r.moved_px), (40, 512));
        assert_eq!(
            (r.dirty_mbs, r.moved_mbs),
            (2, 2),
            "按碰到的宏块数，不是像素数"
        );
        assert!(!r.full_move);
        // 静止宏块都是 20，比这一帧（28）细：不计入画质差
        assert_eq!(r.static_gap, [0; GAP_BINS]);
        // 编码的那些宏块现在是 28：补到 20 时，碰到的宏块（脏 2 个 + 移动 2 个）比 20 粗 8 档
        let r = go(&mut b, 5, 20, false);
        assert_eq!(r.static_gap[7], 4);
        assert_eq!(r.static_gap.iter().sum::<u32>(), 4);
        assert_eq!(
            r.repeat, 3,
            "另外 12 个宏块一直在 QP 20 上，QP 28 那一帧它们没动"
        );
        // 画面来源每变一版推进一次，发送端跳过的版本也累计：两版合起来
        feed.push(&changes(&[Rect::new(0, 0, 8, 8)], &[]));
        feed.push(&changes(&[Rect::new(4, 4, 8, 8)], &[]));
        let r = go(&mut b, 6, 20, false);
        assert_eq!(r.dirty_px, 64 + 64 - 16);
        assert_eq!(
            (r.dirty_mbs, r.moved_mbs),
            (1, 0),
            "两块都落在宏块 (0,0) 里"
        );
        // 整屏移动一次就记着
        feed.push(&changes(
            &[Rect::new(0, 60, 64, 4)],
            &[Move {
                from: (0, 4),
                to: Rect::new(0, 0, 64, 60),
            }],
        ));
        let r = go(&mut b, 7, 20, false);
        assert!(r.full_move);
        assert_eq!((r.dirty_px, r.moved_px), (256, 64 * 60));
        assert_eq!(
            (r.dirty_mbs, r.moved_mbs),
            (4, 12),
            "脏的一条 4 个宏块，移动的 4×4 里除去它们（宏块 (c,3) 被条碰到）"
        );
        // 关键帧：整屏设成这一帧的 QP；之后补清晰按它算
        let _ = go(&mut b, 8, 40, true);
        assert_eq!(go(&mut b, 9, 30, false).static_gap[9], 16);
        // 帧编出来之前累计不清零：放不下被丢的帧，变化还欠着
        feed.push(&changes(&[Rect::new(0, 0, 4, 4)], &[]));
        let mut win = FakeBackend::new(
            FakeConfig::window(),
            Arc::new(ManualClock::new()),
            Box::new(Probe(Arc::default())),
        )
        .with_change_feed(feed.clone());
        win.reconfigure(SIZE, RATES).unwrap();
        assert!(matches!(
            ready(win.submit(i420(&p), ask(0, None, 1, false))),
            Finished::Dropped { .. }
        ));
        assert!(!feed.peek().is_empty(), "丢了的帧不消耗累计");
        // 编出来的帧清零
        let _ = go(&mut b, 10, 30, false);
        assert!(feed.peek().is_empty());
        // 重新配置：表作废，第一帧又是整屏新的
        b.reconfigure(SIZE, RATES).unwrap();
        assert_eq!(go(&mut b, 11, 30, false).dirty_px, 64 * 64);
        // 画质表按编码尺寸建：换尺寸之后宏块数跟着变
        let (mut small, seen2) = probed(FakeConfig::default(), None);
        small.reconfigure((32, 32), RATES).unwrap();
        let q = (vec![9u8; 32 * 32], vec![128; 16 * 16], vec![128; 16 * 16]);
        frame(ready(
            small.submit(i420(&q), ask(0, Some(30), 1 << 20, false)),
        ));
        frame(ready(
            small.submit(i420(&q), ask(1, Some(20), 1 << 20, false)),
        ));
        assert_eq!(last(&seen2).regions.static_gap[9], 4);
        // 没接信道：只有第一帧整屏是新的，之后没有区域
        assert_eq!(last(&seen2).regions.dirty_px, 0);
    }

    /// 脏区域里的细节按宏块汇总：细节在哪个宏块，就只算碰到那个宏块的脏区域。
    #[test]
    fn dirty_detail_counts_only_the_macroblocks_the_dirty_region_touches() {
        let feed = ChangeFeed::default();
        let (mut b, seen) = probed(FakeConfig::default(), Some(&feed));
        // 左上 16×16 满是细节（条纹），其余是平的
        let y: Vec<u8> = (0..64usize * 64)
            .map(|i| {
                if i % 64 < 16 && i / 64 < 16 && i % 2 == 0 {
                    255
                } else {
                    0
                }
            })
            .collect();
        let p = (y, vec![128u8; 32 * 32], vec![128u8; 32 * 32]);
        frame(ready(b.submit(i420(&p), ask(0, Some(30), 1 << 20, false))));
        let whole = last(&seen).regions.dirty_detail_permille;
        assert_eq!(
            whole,
            content::detail_permille(&p.0),
            "整屏都是脏的就是整幅的细节"
        );
        feed.push(&changes(&[Rect::new(2, 2, 4, 4)], &[]));
        frame(ready(b.submit(i420(&p), ask(1, Some(30), 1 << 20, false))));
        let inside = last(&seen).regions.dirty_detail_permille;
        feed.push(&changes(&[Rect::new(40, 40, 4, 4)], &[]));
        frame(ready(b.submit(i420(&p), ask(2, Some(30), 1 << 20, false))));
        let outside = last(&seen).regions.dirty_detail_permille;
        assert!(inside > 400, "{inside}");
        assert_eq!(outside, 0);
    }

    /// 同一幅画面，交像素和交内容描述，区域类输入逐帧相同；描述里的宏块细节是从同一个算法来的。
    #[test]
    fn described_and_pixel_frames_give_the_same_region_inputs() {
        let mut cfg = FakeConfig::default();
        cfg.inputs.described = true;
        let (fa, fb) = (ChangeFeed::default(), ChangeFeed::default());
        let (mut by_pixels, seen_a) = probed(cfg.clone(), Some(&fa));
        let (mut by_desc, seen_b) = probed(cfg, Some(&fb));
        for i in 0..6u64 {
            let p = planes(u8::try_from(i / 2 * 9).unwrap());
            let sample = content::sample_luma(&p.0);
            let edges = content::mb_edges(&p.0, SIZE);
            if i > 0 {
                let c = changes(&[Rect::new(0, 0, 8, 8)], &[]);
                fa.push(&c);
                fb.push(&c);
            }
            let described = FrameInput::Described {
                version: 0,
                sample: &sample,
                detail_permille: content::detail_permille(&p.0),
                mb_edges: &edges,
            };
            let qp = if i == 3 { 20 } else { 30 };
            frame(ready(
                by_pixels.submit(i420(&p), ask(i, Some(qp), 1 << 20, false)),
            ));
            frame(ready(
                by_desc.submit(described, ask(i, Some(qp), 1 << 20, false)),
            ));
        }
        let (a, b) = (seen_a.lock().unwrap(), seen_b.lock().unwrap());
        assert_eq!(a.len(), 6);
        assert_eq!(*a, *b);
        assert!(a.iter().skip(1).any(|m| m.regions.dirty_px == 64));
    }
}
