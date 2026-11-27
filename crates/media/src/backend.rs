//! 编码后端的接口和契约（整体重新设计 C1，见 `docs/research/Q-redesign/design.md` 3.1 节）。
//!
//! 后端 = 一个具体的编码器实现：软件的 openh264、各系统的硬件编码器（VideoToolbox、Media Foundation、VA-API，
//! 以后的 NVENC、AMF 原生接口和 AV1），还有仿真里的假后端。它们都实现 [`EncoderBackend`]，上面一层
//! （[`crate::H264Encoder`]：码率控制、补清晰、换后端）只认这个接口。
//!
//! **码率控制归我们，后端只执行。** 每一帧给后端的是“这一帧的字节预算、可选的 QP、要不要关键帧”，
//! 码率、帧率、分辨率怎么定不是后端的事。和业内一致：WebRTC 的 `VideoEncoder` 默认
//! `has_trusted_rate_controller = false`，上层自己兜丢帧；Chromium 在不可靠的硬件编码器外面套一层软件码控，
//! 逐帧给 QP（`docs/research/Q-redesign/industry-codec-layer.md` 第 4 节）。
//!
//! # 契约
//!
//! 后端必须做到下面每一条；做不到的地方不许悄悄糊弄，要么用 [`BackendTraits`] 事先说明，要么当场用返回值说明。
//! C2 的体检脚本逐条检查，假后端按同样的条款模拟各家的毛病。
//!
//! 1. **每一帧都有下落。** [`EncoderBackend::submit`] 交的每一帧（用 [`FrameAsk::token`] 标识）最终恰好得到一个
//!    结果：编好的帧（[`Finished::Frame`]）或者“丢了，原因是……”（[`Finished::Dropped`]）。结果带着同一个 token，
//!    不靠“第几个出来的就是第几个交的”来对号（异步编码器攒帧、丢帧时顺序对号会错位）。
//! 2. **还没好要有上限。** `submit` 可以回答 [`Submitted::Pending`]（异步的硬件编码器），之后用
//!    [`EncoderBackend::poll`] 取。同时攒着的帧不超过 [`BackendTraits::max_held`]；
//!    [`EncoderBackend::flush`] 之后攒着的帧必须都有下落。同步的后端 `max_held` 是 0，从不回答 `Pending`。
//! 3. **要了关键帧就要给，或者明说给不了。** [`FrameAsk::keyframe`] 为真的那一帧起，
//!    [`BackendTraits::keyframe_within`] 帧之内必须出一个关键帧；被码控丢掉的帧要回 `Dropped`
//!    （调用方据此接着要）。没要求时不许自己出关键帧，两种情况除外：重新配置之后的第一帧；
//!    事先声明了 [`BackendTraits::scene_keyframes`] 的后端在整屏内容突变时（openh264 的屏幕内容模式会这样）。
//! 4. **预算是硬的。** 编出的帧不应超过 [`FrameAsk::budget`]。做不到分两种，都要如实：
//!    后端按 QP 编（[`RateControl::PerFrameQp`]）时字节数由调用方的 QP 决定，超不超由调用方负责；
//!    后端自己有码控（[`RateControl::Window`]）时放不下可以丢帧（回 `Dropped`），不许交一个空帧冒充编好了。
//! 5. **改码率、帧率不重建。** [`EncoderBackend::set_rates`] 立即返回、下一帧生效、不出关键帧。
//!    运行中改不了的后端回 [`BackendError::Rejected`]：状态不变、不复位，接着按旧的值编；
//!    调用方改用 [`EncoderBackend::reconfigure`]（代价是一个关键帧）或者换后端。
//! 6. **改分辨率可以重建，但要说代价。** 只有 [`EncoderBackend::reconfigure`] 会改尺寸；它返回
//!    [`Reconfigured`]，写明下一帧是不是一定是关键帧。`submit` 不会因为尺寸对不上而自己重建，尺寸不对是调用方的错。
//! 7. **停多久再交帧都不能出错。** 两次 `submit` 之间隔多久都一样；后端内部的“卡住”判定只能从交了活之后算起
//!    （见 [`crate::stall`]，2026-11-27 Windows 日志的教训）。
//! 8. **时间分开报。** [`Timing::compute_us`] 是 CPU（或编码器）真正在算的时间，[`Timing::wait_us`] 是等待
//!    （等显卡读回、等锁、等编码器事件），[`Timing::setup_us`] 是一次性的初始化。“CPU 跟不上”只看计算时间。
//! 9. **出错之后回到干净的状态。** `submit`、`poll` 返回 [`BackendError::Failed`] 之后，后端自己复位
//!    （[`EncoderBackend::size`] 变回 `None`），攒着的帧作废；调用方重新 `reconfigure` 就能接着用，
//!    或者换别的后端。
//!
//! # 为什么长这样（对照业内接口）
//!
//! | 这里 | WebRTC `VideoEncoder` | Chromium `VideoEncodeAccelerator` | 区别和原因 |
//! | --- | --- | --- | --- |
//! | `submit(input, ask)` | `Encode(frame, frame_types)` | `Encode(frame, force_keyframe)` | 多了每帧的字节预算和 QP：远程桌面的帧大小差几百倍，预算必须逐帧给 |
//! | 返回值 / `poll` | `EncodedImageCallback` 回调 | `BitstreamBufferReady` 回调 | 用返回值不用回调：编码线程是我们自己的，同步取更简单，也好在仿真里确定地重放 |
//! | `Finished::Dropped` | `OnFrameDropped` | 没有 | 丢帧必须带原因（发送端要区分“预算不够”和“没东西要发”） |
//! | `set_rates` | `SetRates` | `RequestEncodingParametersChange` | 一样 |
//! | `reconfigure` | `InitEncode`（重建） | 重建 | 一样，但把代价（必出关键帧）写在返回值里 |
//! | [`BackendTraits`] | `GetEncoderInfo` | `GetSupportedProfiles` | 多了“毛病”这一类属性（C2 的体检来填） |
//!
//! 参考帧失效、帧内刷新、长期参考帧这些恢复手段现在不在接口里：目前所有后端都只会“要关键帧”，
//! 等有了支持它们的后端（NVENC）再加一个可选的方法，不影响已有的后端。

use peerdesk_core::frame::GpuFrame;

/// 后端是软件还是硬件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// 在 CPU 上算。
    Software,
    /// 显卡或专用芯片。
    Hardware,
}

/// 编出来的码流是什么格式。只做开源、免专利费的两种（产品负责人 2026-11-27 定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// H.264（Annex B）。
    H264,
    /// AV1（C6 再接）。
    Av1,
}

/// 每帧的大小由谁、用什么手段控制。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    /// 后端按调用方给的 QP 编，自己没有码控：字节数由调用方负责（openh264 关掉码控后就是这样）。
    /// [`FrameAsk::qp`] 必须给。
    PerFrameQp,
    /// 后端有自己的码控，只能给它码率和窗口：按 [`FrameAsk::budget`] 和 [`Rates`] 设；放不下时它可能丢帧。
    /// [`FrameAsk::qp`] 只是提示，可以不理。
    Window,
}

/// 后端收哪些形式的画面。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Inputs {
    /// 内存里的 I420 三个平面。
    pub i420: bool,
    /// 内存里的 NV12。
    pub nv12: bool,
    /// 显卡画面，不拷贝（具体认不认某一帧还要问 [`EncoderBackend::accepts`]：可能不在同一块显卡上）。
    pub gpu: bool,
    /// 内容描述（没有像素，只有模型后端收）。
    pub described: bool,
}

/// 后端的属性：能力和毛病。事先说明的才算数，体检（C2）核对它们是不是真的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendTraits {
    /// 每帧大小怎么控制。
    pub rate_control: RateControl,
    /// 收哪些形式的画面。
    pub inputs: Inputs,
    /// 最多同时攒几帧没交出来（同步的后端是 0）。
    pub max_held: u8,
    /// 要了关键帧，最多几帧之内给（1 表示就是要的那一帧）。
    pub keyframe_within: u8,
    /// 新开（或重新配置）之后第一帧只能按这个 QP 编，除非先多编一次初始化；`None` 表示没有这个限制。
    pub first_frame_qp: Option<u8>,
    /// 画面整屏突变（场景切换）时会自己出关键帧，即使没要求。调用方要按“可能是关键帧”来算预算。
    pub scene_keyframes: bool,
}

/// 后端的身份和属性。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendInfo {
    /// 给人看的名字（写进日志和详情面板）。
    pub name: String,
    /// 软件还是硬件。
    pub kind: BackendKind,
    /// 码流格式。
    pub format: Format,
    /// 能力和毛病。
    pub traits: BackendTraits,
}

/// 码率和帧率（给有自己码控的后端设窗口用；按 QP 编的后端只用来选级别这类静态参数）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rates {
    /// 目标码率（比特每秒）。
    pub bps: u64,
    /// 帧率。
    pub fps: u32,
}

/// 交给后端的一帧画面。尺寸必须和 [`EncoderBackend::size`] 一致，宽高都是偶数。
#[derive(Clone, Copy)]
pub enum FrameInput<'a> {
    /// I420：亮度平面每行 `width` 字节，两个色度平面每行 `width / 2` 字节。
    I420 {
        /// 亮度。
        y: &'a [u8],
        /// 色度 U。
        u: &'a [u8],
        /// 色度 V。
        v: &'a [u8],
    },
    /// NV12：亮度平面后面接交错的色度平面，每行按宽度紧排。
    Nv12(&'a [u8]),
    /// 显卡画面。
    Gpu(&'a dyn GpuFrame),
    /// 内容描述（没有像素）：这一帧在当前编码尺寸下的亮度抽样和细节，以及源画面的版本。
    /// 只有模型后端（假后端）收：它本来就不看像素，只看这几样（见 `peerdesk_core::frame::ContentDesc`）。
    Described {
        /// 源画面的第几版。
        version: u64,
        /// 亮度平面的抽样（和 `content::sample_luma` 对同一幅画面的结果相同）。
        sample: &'a [u8],
        /// 细节（千分之几）。
        detail_permille: u32,
        /// 每个 16×16 宏块里有细节的抽样点数（`content::mb_edges`）：模型后端汇总脏区域的细节用。
        mb_edges: &'a [u16],
    },
}

impl std::fmt::Debug for FrameInput<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::I420 { .. } => "I420",
            Self::Nv12(_) => "NV12",
            Self::Gpu(_) => "显卡画面",
            Self::Described { .. } => "内容描述",
        })
    }
}

/// 对这一帧的要求。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameAsk {
    /// 调用方起的编号，结果里原样带回（契约第 1 条）。
    pub token: u64,
    /// 这一帧最多多少字节（契约第 4 条）。
    pub budget: usize,
    /// 按哪个 QP 编。[`RateControl::PerFrameQp`] 的后端必须有；其他后端当提示。
    pub qp: Option<u8>,
    /// 这一帧要编成关键帧。
    pub keyframe: bool,
}

/// 各段时间（微秒），契约第 8 条。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Timing {
    /// 真正在算的时间。
    pub compute_us: u64,
    /// 等待的时间（等显卡读回、等锁、等编码器事件）。
    pub wait_us: u64,
    /// 一次性初始化的时间（例如新开的编码器为了改第一帧的 QP 多编的那一次），不算进前两项。
    pub setup_us: u64,
}

/// 编好的一帧。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedOut {
    /// 是哪一帧（[`FrameAsk::token`]）。
    pub token: u64,
    /// 码流，不为空。
    pub data: Vec<u8>,
    /// 是不是关键帧。
    pub key: bool,
    /// 按哪个 QP 编的（后端知道的话）。
    pub qp: Option<u8>,
    /// 各段时间。
    pub timing: Timing,
}

/// 后端为什么把一帧丢了。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropReason {
    /// 码控：预算（或码率窗口）放不下这一帧。发送端记成网络受限。
    Budget,
    /// 后端这会儿不收帧（例如 Media Foundation 的 `MF_E_NOTACCEPTING`）：这一帧没进编码器，它带的要求也没送到。
    NotAccepting,
}

impl DropReason {
    /// 写进日志的说明。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Budget => "预算放不下这一帧",
            Self::NotAccepting => "编码器这会儿不收帧",
        }
    }
}

/// 一帧的下落。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finished {
    /// 编好了。
    Frame(EncodedOut),
    /// 丢了。
    Dropped {
        /// 是哪一帧。
        token: u64,
        /// 为什么。
        why: DropReason,
    },
}

impl Finished {
    /// 是哪一帧。
    #[must_use]
    pub const fn token(&self) -> u64 {
        match self {
            Self::Frame(f) => f.token,
            Self::Dropped { token, .. } => *token,
        }
    }
}

/// [`EncoderBackend::submit`] 的回答。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Submitted {
    /// 这一帧已经有下落了。
    Ready(Finished),
    /// 收下了，还没好：之后用 [`EncoderBackend::poll`] 取（契约第 2 条）。
    Pending,
}

/// [`EncoderBackend::reconfigure`] 的回答：换尺寸的代价。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reconfigured {
    /// 编码器是不是重建了（重建的话之前攒着的帧都作废）。
    pub rebuilt: bool,
    /// 下一帧是不是一定是关键帧（不管要没要）。
    pub keyframe_next: bool,
}

/// 后端出错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendError {
    /// 这次调用失败了。后端已经复位（契约第 9 条），调用方可以重新配置再试，或者换后端。
    Failed(String),
    /// 后端不接受这组值（例如运行中改不了码率），状态没有变、没有复位，接着按旧的值编（契约第 5 条）。
    /// 和 [`Self::Failed`] 分开：一个信号只表示一件事，“拒绝”不等于“出错复位”。
    Rejected(String),
    /// 调用方用错了（给了后端不收的画面形式、该给 QP 没给、尺寸不对、还没配置就交帧）。后端的状态没有变。
    Misuse(&'static str),
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(why) => write!(f, "编码器出错：{why}"),
            Self::Rejected(why) => write!(f, "编码器不接受：{why}"),
            Self::Misuse(why) => write!(f, "编码器用法不对：{why}"),
        }
    }
}

impl std::error::Error for BackendError {}

/// 编码后端。契约见模块说明。
pub trait EncoderBackend: Send + std::fmt::Debug {
    /// 身份和属性。
    fn info(&self) -> &BackendInfo;

    /// 现在按多大编（宽、高）。`None`：还没配置，或者出错之后复位了。
    fn size(&self) -> Option<(u16, u16)>;

    /// 配置（或改）编码尺寸，可以重建编码器（契约第 6 条）。尺寸和现在一样时也照做（调用方想从头开始时用）。
    ///
    /// # Errors
    /// 打不开这个尺寸的编码器。
    fn reconfigure(&mut self, size: (u16, u16), rates: Rates)
        -> Result<Reconfigured, BackendError>;

    /// 改码率、帧率：不重建、不出关键帧，下一帧生效（契约第 5 条）。
    ///
    /// # Errors
    /// 后端不接受这组值（[`BackendError::Rejected`]，状态不变）；或者编码器出错（[`BackendError::Failed`]，已复位）。
    fn set_rates(&mut self, rates: Rates) -> Result<(), BackendError>;

    /// 认不认这一帧显卡画面（[`Inputs::gpu`] 为真的后端才可能认；例如要在同一块显卡上）。
    fn accepts(&self, _frame: &dyn GpuFrame) -> bool {
        false
    }

    /// 交一帧（契约第 1–4、7 条）。
    ///
    /// # Errors
    /// 编码器出错（已复位），或者用法不对。
    fn submit(&mut self, input: FrameInput<'_>, ask: FrameAsk) -> Result<Submitted, BackendError>;

    /// 取一个之前回答了 [`Submitted::Pending`]、现在有下落的帧，不等。没有时返回 `None`。
    ///
    /// # Errors
    /// 编码器出错（已复位）。
    fn poll(&mut self) -> Result<Option<Finished>, BackendError> {
        Ok(None)
    }

    /// 让后端把攒着的帧都交出来（画面停下后用），之后用 [`Self::poll`] 取到 `None` 为止。
    ///
    /// # Errors
    /// 编码器出错（已复位）。
    fn flush(&mut self) -> Result<(), BackendError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_carry_their_token_and_reasons_have_text() {
        let frame = Finished::Frame(EncodedOut {
            token: 7,
            data: vec![1],
            key: true,
            qp: Some(26),
            timing: Timing::default(),
        });
        assert_eq!(frame.token(), 7);
        let dropped = Finished::Dropped {
            token: 8,
            why: DropReason::Budget,
        };
        assert_eq!(dropped.token(), 8);
        for why in [DropReason::Budget, DropReason::NotAccepting] {
            assert!(!why.name().is_empty());
        }
        assert_eq!(
            BackendError::Failed("显卡丢了".into()).to_string(),
            "编码器出错：显卡丢了"
        );
        assert_eq!(
            BackendError::Rejected("运行中改不了码率".into()).to_string(),
            "编码器不接受：运行中改不了码率"
        );
        assert_eq!(
            BackendError::Misuse("没给 QP").to_string(),
            "编码器用法不对：没给 QP"
        );
        let y = [0u8; 4];
        assert_eq!(
            format!(
                "{:?} {:?}",
                FrameInput::I420 {
                    y: &y,
                    u: &y,
                    v: &y
                },
                FrameInput::Nv12(&y)
            ),
            "I420 NV12"
        );
    }
}
