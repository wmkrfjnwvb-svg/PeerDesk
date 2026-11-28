//! PeerDesk 端到端测试平台（第一层：进程内仿真）。
//!
//! 被控端和控制端用的是产品里同一份发送端 / 接收端（[`peerdesk_core::media`]）和真实的编码器，
//! 中间是确定性的仿真网络，画面是接近真实的桌面（[`content`]）。每个场景输出：
//! - 延迟（采集到显示）、卡顿（两次画面更新之间的最大间隔、超过 200 ms 的次数）；
//! - 画质（显示的画面和源画面的亮度 PSNR）、停下之后多久变清晰；
//! - 每种操作的流量、关键帧数、丢失恢复次数；
//! - 每个阶段结束时的并排截图（源画面 | 显示的画面 | 放大的差异）。
//!
//! 同一个场景同一个种子，结果逐字节相同，可以放进 CI 当回归门槛。

pub mod content;
pub mod flow;
pub mod homes;
pub mod report;
pub mod rfc8867;
mod session;
pub use session::{run_multi, MultiScenario, Packet, PairSpec};
pub mod timeline;
pub mod world;

use peerdesk_core::adaptive::Profile;
use peerdesk_core::changes::{ChangeFeed, Changes};
use peerdesk_core::error::Error;
pub use peerdesk_core::estimator::EstimatorKind;
pub use peerdesk_core::estimator::RateState;
use peerdesk_core::frame::{Described, Picture};
pub use peerdesk_core::media::ProbeStats;
use peerdesk_core::media::{DecodeWorker, MediaReceiver, MediaSender, SenderConfig, SenderStats};
use peerdesk_core::path::segments::{Segment, SegmentRoute, Segmented, SegmentedConfig};
use peerdesk_core::path::{
    CrossSpec, CrossStats, Dir, DirConfig, Path, PathConfig, PathStats, RouteChange,
};
pub use peerdesk_core::policy::ContentPolicy;
use peerdesk_core::proto::{decode_datagram, encode_datagram, Channel, Header};
use peerdesk_core::record::{self, Meta, Record, Role, Sink};
use peerdesk_core::screen::Screen;
use peerdesk_core::sim::{Link, Net, NetStats, Network};
use peerdesk_core::time::Time;
use peerdesk_core::video::{TileDecoder, TileEncoder, VideoDecoder, VideoEncoder};
use peerdesk_media::fake::{
    ByteModel, DefaultByteModel, FakeBackend, FakeConfig, FakeDecoder, ManualClock, ModelInput,
};
use peerdesk_media::fitted::FittedByteModel;
use peerdesk_media::fitted_v2::RegionByteModel;
use peerdesk_media::{H264Decoder, H264Encoder};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub use content::{Action, RealisticDesktop};

const HOST: usize = 0;
const VIEWER: usize = 1;
/// 反馈间隔（微秒）。
const FEEDBACK_US: u64 = 20_000;
/// 没有新情况时多久也发一次反馈（微秒）。和产品的控制端一样（`crates/net/src/controller.rs`）：每 20 ms 看一次，
/// 收到了新的画面包、要请求重传或者帧有了定论才发，否则 1 秒一次。以前仿真每 20 ms 无条件发，真实程序里
/// 静止时 1 秒才一次：探测组靠“下一次报告没有新包”结束，仿真里晚 40 ms，真实程序里晚 2 秒（2026-11-26 审查发现）。
const IDLE_FEEDBACK_US: u64 = 1_000_000;
/// 亮度 PSNR 达到这个值算“清晰”（dB）。
pub const SHARP_DB: f64 = 38.0;
/// 两次画面更新间隔超过这么久算一次卡顿（微秒）。
pub const FREEZE_US: u64 = 200_000;

/// 真实排队（瓶颈里的数据要排多久）超过这么久算“真实队列高”（微秒）。取无线画像判拥塞的排队门限 30 ms：
/// 真实队列到了这个程度，发送端就不该还认为自己“应用受限”（见 [`StateShare`]）。
pub const TRUE_QUEUE_HIGH_US: u64 = 30_000;

/// 一对编码器和解码器。
type Codecs = (Box<dyn VideoEncoder>, Box<dyn VideoDecoder>);

/// 默认画面尺寸。调试构建（覆盖率）里画面合成慢得多，用小画面，逻辑一样。
const DEFAULT_SIZE: (u16, u16) = if cfg!(debug_assertions) {
    (480, 272)
} else {
    (1280, 720)
};

/// 假后端用哪个字节模型（只对 L0、L0.5 有意义；L1、块编码是真编码器，不用）。世界文件里的 `byte_model` 键。
/// 以后加新模型（例如输入改成脏区域和移动区域的）就在这里加一项，世界文件的键不用改。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteModelKind {
    /// 占位的 [`DefaultByteModel`]（默认，现有结果都是它）。
    Default,
    /// M2 实测拟合的 openh264 模型，各类合在一起的系数（[`FittedByteModel::openh264_v1`]）。
    Openh264V1,
    /// M2b（B3）按画面区域（脏区域、移动区域、宏块画质表）重新拟合的 openh264 模型（[`RegionByteModel::openh264_v2`]）。
    Openh264V2,
}

impl ByteModelKind {
    /// 全部，（文本里的名字，值）。
    pub const ALL: [(&'static str, Self); 3] = [
        ("default", Self::Default),
        ("openh264-v1", Self::Openh264V1),
        ("openh264-v2", Self::Openh264V2),
    ];

    /// 文本里的名字。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Openh264V1 => "openh264-v1",
            Self::Openh264V2 => "openh264-v2",
        }
    }

    /// 造模型。[`run_probed`] 在跑的时候，外面再套一层记录。
    fn build(self, noise: ByteNoise) -> Result<Box<dyn ByteModel>, Error> {
        let model = self.build_plain(noise)?;
        Ok(if PROBE.with(|p| p.borrow().is_some()) {
            Box::new(Probed(model))
        } else {
            model
        })
    }

    fn build_plain(self, noise: ByteNoise) -> Result<Box<dyn ByteModel>, Error> {
        use peerdesk_media::fitted::Noise;
        Ok(match self {
            // 占位模型没有扰动
            Self::Default => Box::new(DefaultByteModel),
            Self::Openh264V1 => {
                let model = FittedByteModel::openh264_v1()
                    .map_err(|_| Error::Inconsistent("随包的 openh264-v1 字节模型文件读不出来"))?;
                Box::new(match noise {
                    ByteNoise::Default => model,
                    ByteNoise::Within => model.with_within_sigma(OPENH264_V1_WITHIN_SIGMA_MICRO),
                    ByteNoise::Off => model.with_noise(false),
                })
            }
            Self::Openh264V2 => {
                let model = RegionByteModel::openh264_v2()
                    .map_err(|_| Error::Inconsistent("随包的 openh264-v2 字节模型文件读不出来"))?;
                Box::new(model.with_noise_kind(match noise {
                    ByteNoise::Default => Noise::Full,
                    ByteNoise::Within => Noise::Within,
                    ByteNoise::Off => Noise::Off,
                }))
            }
        })
    }
}

/// 字节模型的预测上加多少扰动（只对实测拟合的模型有意义；见 `peerdesk_media::fitted::Noise`）。世界文件里的 `byte_noise` 键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ByteNoise {
    /// 模型自己的默认：全部残差的标准差（会把总字节抬高，留着是为了现有结果不变；占位模型没有扰动，只能写这个）。
    Default,
    /// 只用序列内的残差标准差（帧间波动）。
    Within,
    /// 不加扰动：每帧就是模型的中位预测。
    Off,
}

/// openh264-v1 的序列内残差标准差（10⁻⁶ 的 log2）。v1 的模型文件里没有拆过残差（文件不动），数来自
/// `scripts/m2b-residuals.py` 在 M2b 拟合集上的结果：全部残差 2.219 = 序列之间 2.155 + 序列之内 0.527。
const OPENH264_V1_WITHIN_SIGMA_MICRO: u32 = 527_139;

impl ByteNoise {
    /// 全部，（文本里的名字，值）。
    pub const ALL: [(&'static str, Self); 3] = [
        ("default", Self::Default),
        ("within", Self::Within),
        ("off", Self::Off),
    ];

    /// 文本里的名字。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Within => "within",
            Self::Off => "off",
        }
    }
}

thread_local! {
    /// [`run_probed`] 正在记的模型输入（没在记为 `None`）。仿真是单线程的，一个线程一份。
    static PROBE: std::cell::RefCell<Option<Vec<ModelInput>>> = const { std::cell::RefCell::new(None) };
}

/// 套在真正的字节模型外面：把每一帧的模型输入记下来，字节数照旧由里面的模型定，所以会话和不记时逐字节相同。
#[derive(Debug)]
struct Probed(Box<dyn ByteModel>);

impl ByteModel for Probed {
    fn bytes(&self, input: &ModelInput) -> usize {
        PROBE.with(|p| {
            if let Some(log) = p.borrow_mut().as_mut() {
                log.push(*input);
            }
        });
        self.0.bytes(input)
    }
}

/// 和 [`run`] 一样，另外交回假后端的字节模型这一路上看到的每一帧的输入（只对 L0.5、L0 有，其他编码器是空的）。
/// 对账用：L0.5（像素）和 L0（内容描述）交给假后端的输入必须逐帧相同。
///
/// # Errors
/// 同 [`run`]。
pub fn run_probed(s: &Scenario) -> Result<(Outcome, Vec<ModelInput>), Error> {
    PROBE.with(|p| *p.borrow_mut() = Some(Vec::new()));
    let outcome = run(s);
    let log = PROBE.with(|p| p.borrow_mut().take()).unwrap_or_default();
    outcome.map(|o| (o, log))
}

/// 编码器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecKind {
    /// 块编码（对照组）。
    Tile,
    /// H.264（OpenH264）。
    H264,
    /// L0.5（见 `docs/research/Q-redesign/a1-plan.md` 第 4 节）：画面是真像素，编码用假后端（面积、细节从像素算，字节由字节模型定，
    /// 字节模型现在是占位），解码用假解码器；接收端走控制端用的“延后解码”那条路。显示端量尺按内容版本号和 QP 判清晰，
    /// 没有像素可比：[`PhaseResult::end_psnr`] 是 NaN（没量）。和 L1（[`Self::H264`]）比，只差“字节模型准不准”。
    H264L05,
    /// L0，全模型仿真（见 `a1-plan.md`）：没有像素。画面是操作时间线里的内容描述（从同一个合成桌面导出，
    /// 见 [`timeline::Timeline`]），编码用假后端，解码用假解码器。和 L0.5 交给假后端的数逐帧相同，快得多。
    H264L0,
}

/// L0.5 里 QP 不超过这个值算“清晰”。**占位**：M3 出数之前没有依据，结果只报告、不当门槛。
pub const L05_QP_SHARP: u8 = 28;

impl CodecKind {
    /// 名字。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            CodecKind::Tile => "tile",
            CodecKind::H264 => "h264",
            CodecKind::H264L05 => "h264-l05",
            CodecKind::H264L0 => "h264-l0",
        }
    }

    /// 是不是 H.264 一类（接收端要开关键帧宽限）。
    #[must_use]
    pub const fn is_h264(self) -> bool {
        matches!(
            self,
            CodecKind::H264 | CodecKind::H264L05 | CodecKind::H264L0
        )
    }

    /// 造一对编码器和解码器。`tag` 是源画面的内容版本，只有 L0.5 用（假后端每交一帧读一次）。
    fn make(self, s: &Scenario, tag: &Arc<AtomicU64>, feed: &ChangeFeed) -> Result<Codecs, Error> {
        let (fps, max_bps, policy) = (s.fps, s.max_bps, &s.policy);
        let (byte_model, byte_noise) = (s.byte_model, s.byte_noise);
        Ok(match self {
            CodecKind::H264L0 => {
                // 和 L0.5 同一个假后端，只多收“内容描述”这种输入；版本号从描述里来，不用拨
                let mut cfg = FakeConfig::like_openh264();
                cfg.inputs.described = true;
                let backend = FakeBackend::new(
                    cfg,
                    Arc::new(ManualClock::new()),
                    byte_model.build(byte_noise)?,
                )
                .with_change_feed(feed.clone());
                (
                    Box::new(
                        H264Encoder::with_max_bps(fps, max_bps)
                            .with_policy(policy)
                            .with_hardware(software_only)
                            .with_soft_backend(Box::new(backend)),
                    ),
                    Box::new(FakeDecoder::new()),
                )
            }
            CodecKind::H264L05 => {
                // 假后端的时钟不拨：A1 里编码耗时按 0 算（和 L1 一样）
                let backend = FakeBackend::new(
                    FakeConfig::like_openh264(),
                    Arc::new(ManualClock::new()),
                    byte_model.build(byte_noise)?,
                )
                .with_version_tag(Arc::clone(tag))
                .with_change_feed(feed.clone());
                (
                    Box::new(
                        H264Encoder::with_max_bps(fps, max_bps)
                            .with_policy(policy)
                            .with_hardware(software_only)
                            .with_soft_backend(Box::new(backend)),
                    ),
                    Box::new(FakeDecoder::new()),
                )
            }
            CodecKind::Tile => (Box::new(TileEncoder::new()), Box::new(TileDecoder::new())),
            CodecKind::H264 => (
                Box::new(
                    H264Encoder::with_max_bps(fps, max_bps)
                        .with_policy(policy)
                        .with_hardware(software_only),
                ),
                Box::new(H264Decoder::new()?),
            ),
        })
    }
}

/// 仿真只用软件编码：结果在任何机器上都一样（开发者电脑上有硬件编码器也不用）。
fn software_only(
    _: u32,
    _: u32,
    _: u32,
    _: u64,
) -> Result<Box<dyn peerdesk_media::HardwareEncoder>, String> {
    Err("仿真只用软件编码（结果可复现）".into())
}

/// 纠错方式的环境变量：`xor` 用异或（对照），其他用 RS。
fn fec_from(value: Option<&std::ffi::OsStr>) -> peerdesk_core::transport::Fec {
    if value.is_some_and(|v| v == "xor") {
        peerdesk_core::transport::Fec::Xor
    } else {
        peerdesk_core::transport::Fec::Rs
    }
}

/// 默认的编码器：环境变量 `PEERDESK_E2E_CODEC=h264-l05`、`h264-l0` 把所有默认场景换成 L0.5、L0（对账用），
/// 其他是 H.264（L1）。
fn default_codec(value: Option<&std::ffi::OsStr>) -> CodecKind {
    if value.is_some_and(|v| v == "h264-l05") {
        CodecKind::H264L05
    } else if value.is_some_and(|v| v == "h264-l0") {
        CodecKind::H264L0
    } else {
        CodecKind::H264
    }
}

/// 一个阶段：做什么，做多少帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Phase {
    /// 操作。
    pub action: Action,
    /// 帧数。
    pub frames: u32,
}

/// 一个场景。
#[derive(Debug, Clone)]
pub struct Scenario {
    /// 名字，也是截图目录名。
    pub name: String,
    /// 画面尺寸。
    pub width: u16,
    /// 画面高度。
    pub height: u16,
    /// 帧率。
    pub fps: u32,
    /// 编码器。
    pub codec: CodecKind,
    /// 依次执行的阶段。
    pub phases: Vec<Phase>,
    /// 网络：随机数布局 1（老的 `Net`）或布局 2（分层的 `Path`），见 [`NetworkPlan`]。
    pub network: NetworkPlan,
    /// 网络画像。
    pub profile: Profile,
    /// 起始码率。
    pub start_bps: u64,
    /// 码率上限。
    pub max_bps: u64,
    /// 随机种子。
    pub seed: u64,
    /// 带宽估计器。
    pub estimator: EstimatorKind,
    /// 发送节奏（实验，见 [`peerdesk_core::pacer`]）。默认关，环境变量 `PEERDESK_PACE=1` 打开。
    pub pacing: bool,
    /// 纠错方式。默认 RS（新版本之间）；`PEERDESK_FEC=xor` 对照。
    pub fec: peerdesk_core::transport::Fec,
    /// 调试：把发送端各模块的决定、每秒的网络控制状态、因为哪个闸门没编帧打到标准错误。
    /// 默认按环境变量 `PEERDESK_EVENTS` 打开。
    pub trace: bool,
    /// 起步探测（见 `MediaSender::set_start_probe`）。默认打开（和产品一样），环境变量 `PEERDESK_START_PROBE=0` 关掉做对照。
    pub start_probe: bool,
    /// 内容策略（现在只有办公；以后的游戏、电影模式各加场景守住）。
    pub policy: ContentPolicy,
    /// 反馈里的探测结果带组号（产品里由握手的 `PROBE_ID` 决定；新版本之间都打开）。关掉是模拟对端是老版本：
    /// 反馈格式和探测行为照旧，没有运行中探测。
    pub probe_ids: bool,
    /// 假后端的字节模型（只对 [`CodecKind::H264L05`]、[`CodecKind::H264L0`] 有意义）。默认是占位模型。
    pub byte_model: ByteModelKind,
    /// 字节模型的扰动（默认是模型自己的做法；占位模型只能是默认）。
    pub byte_noise: ByteNoise,
}

impl Scenario {
    /// 一个默认的场景：720p、30 帧、H.264、局域网。编码种类、节奏器、纠错、起步探测、调试输出的默认值
    /// 看环境变量（测试和对照用）；要“结果不受环境变量影响”的场景用 [`Self::fixed`]，世界文件读出来的就是它。
    #[must_use]
    pub fn new(name: &str, phases: Vec<Phase>) -> Self {
        Self {
            codec: default_codec(std::env::var_os("PEERDESK_E2E_CODEC").as_deref()),
            pacing: peerdesk_core::pacer::enabled(),
            fec: fec_from(std::env::var_os("PEERDESK_FEC").as_deref()),
            trace: std::env::var_os("PEERDESK_EVENTS").is_some(),
            start_probe: std::env::var("PEERDESK_START_PROBE").map_or(true, |v| v != "0"),
            ..Self::fixed(name, phases)
        }
    }

    /// 和 [`Self::new`] 一样的默认值，但一个环境变量也不读：L1 H.264、节奏器关、RS 纠错、起步探测开、不打调试输出。
    #[must_use]
    pub fn fixed(name: &str, phases: Vec<Phase>) -> Self {
        let (width, height) = DEFAULT_SIZE;
        Self {
            name: name.to_owned(),
            width,
            height,
            fps: 30,
            codec: CodecKind::H264,
            phases,
            network: NetworkPlan::Layout1(vec![(0, Link::lan())]),
            profile: Profile::STABLE,
            start_bps: ContentPolicy::OFFICE.start_bps,
            max_bps: 30_000_000,
            seed: 1,
            estimator: EstimatorKind::PRODUCT,
            pacing: false,
            fec: peerdesk_core::transport::Fec::Rs,
            trace: false,
            start_probe: true,
            policy: ContentPolicy::OFFICE,
            probe_ids: true,
            byte_model: ByteModelKind::Default,
            byte_noise: ByteNoise::Default,
        }
    }
}

/// 场景的网络，按随机数布局分（`docs/research/Q-redesign/a2-plan.md` 第 2、8 节）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPlan {
    /// 布局 1：老的 `Net`，从某个时刻（微秒）起换成某条链路，两个方向对称。第一项应当从 0 开始。
    /// 现有场景和 144 次基线都用它，结果逐字节不变。
    Layout1(Vec<(u64, Link)>),
    /// 布局 2：分层的路径模型 [`Path`]，每层一条随机数流，两个方向可以不同。
    Layout2(Box<Layout2>),
    /// 布局 2 的分段路径：两端之间五段（A 局域网、A 出口、公网、B 出口、B 局域网），每段一个 `Path`，
    /// 见 `peerdesk_core::path::segments`。
    Segmented(Box<SegmentedPlan>),
}

impl NetworkPlan {
    /// 随机数布局号（世界文件里的 `rng_layout`）。
    #[must_use]
    pub const fn layout(&self) -> u32 {
        match self {
            NetworkPlan::Layout1(_) => 1,
            NetworkPlan::Layout2(_) | NetworkPlan::Segmented(_) => 2,
        }
    }

    /// 指纹行身份里的网络部分：布局 1 是空的（和加布局 2 之前逐字节相同），布局 2 是 ` layout2`，分段是 ` layout2-segments`。
    #[must_use]
    pub const fn identity(&self) -> &'static str {
        match self {
            NetworkPlan::Layout1(_) => "",
            NetworkPlan::Layout2(_) => " layout2",
            NetworkPlan::Segmented(_) => " layout2-segments",
        }
    }

    /// 把布局 1 的链路搬到布局 2（A2-3 对照用）：每一项按 `DirConfig::from_link` 转成两个方向相同的参数，
    /// 在同样的时刻换（[`PathStep`]，不发“路径变了”事件，和 `set_link` 一样），`Link::up` 搬成接通或切断。
    /// `loss_first` 把丢包放回瓶颈之前（`Net` 的位置）。已经是布局 2 的原样返回。
    ///
    /// 和布局 1 仍然不同的地方：随机数（每层一条流）、容量误差按纳秒记、尖峰和接收端忙的时间表从换参数的时刻起算；
    /// 第一项不从 0 开始时，布局 1 在那之前没有链路（包全算 `unroutable`），布局 2 在那之前用第一项的参数。
    #[must_use]
    pub fn to_layout2(&self, loss_first: bool) -> NetworkPlan {
        let NetworkPlan::Layout1(links) = self else {
            return self.clone();
        };
        let cfg = |l: &Link| {
            PathConfig::symmetric(DirConfig {
                loss_before_bottleneck: loss_first,
                ..DirConfig::from_link(l)
            })
        };
        let first = links.first().map_or_else(Link::lan, |(_, l)| *l);
        NetworkPlan::Layout2(Box::new(Layout2 {
            start: cfg(&first),
            steps: links
                .iter()
                .map(|(at, l)| {
                    (
                        *at,
                        PathStep {
                            config: cfg(l),
                            up: l.up,
                        },
                    )
                })
                .collect(),
            routes: Vec::new(),
            cross: Vec::new(),
        }))
    }
}

/// 布局 2 的网络。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Layout2 {
    /// 起始的参数（两个方向各一份）。
    pub start: PathConfig,
    /// 从某个时刻（微秒）起立即换参数：和布局 1 的换链路一样在那一帧开头换，不中断、不发事件
    /// （容量按轨迹变这类“同一条路的参数变了”）。换路用 [`Self::routes`]。
    pub steps: Vec<(u64, PathStep)>,
    /// 按计划的中断和换路（`Path::schedule_route_change`）；带新参数的在恢复时刻发一次“路径变了”事件。
    pub routes: Vec<RouteChange>,
    /// 交叉流量：挂在哪个方向的瓶颈上（正向是被控端到控制端，画面走的方向），什么源。
    pub cross: Vec<(Dir, CrossSpec)>,
}

/// 分段路径的网络（布局 2）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SegmentedPlan {
    /// 五段起始的参数。
    pub start: SegmentedConfig,
    /// 从某个时刻（微秒）起立即换一段的参数（不中断、不发事件）。
    pub steps: Vec<(u64, SegmentStep)>,
    /// 按计划的中断和换路（可以同时涉及几段，带新参数的在恢复时刻发一次“路径变了”）。
    pub routes: Vec<SegmentRoute>,
    /// 交叉流量：挂在哪一段、哪个方向（正向是 A → B）的瓶颈上，什么源。
    pub cross: Vec<(Segment, Dir, CrossSpec)>,
}

/// 分段路径的一次立即换参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentStep {
    /// 换哪一段。
    pub segment: Segment,
    /// 这一段两个方向的新参数。
    pub config: PathConfig,
    /// 这一段接通还是切断。
    pub up: bool,
}

/// 分段路径一段的真值（不进指纹）。
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SegmentTruth {
    /// 这一段的计数（主流量和探测都在里面）。
    pub stats: PathStats,
    /// 正向（A → B）瓶颈排队的时间平均（毫秒，按帧末采样）。
    pub mean_queue_ms: f64,
    /// 正向瓶颈排队的最大值（毫秒，按帧末采样）。
    pub max_queue_ms: f64,
}

/// 布局 2 的一次立即换参数。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PathStep {
    /// 两个方向的新参数。
    pub config: PathConfig,
    /// 接通还是切断（`Path::set_up`，对应 `Link::up`）。
    pub up: bool,
}

/// 一个阶段的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseResult {
    /// 操作。
    pub action: Action,
    /// 这个阶段发出的数据报字节数。
    pub bytes: u64,
    /// 平均码率（kbps）。
    pub kbps: u64,
    /// 显示的帧数。
    pub frames_shown: u32,
    /// 采集到显示的延迟 p50 / p95（毫秒）。
    pub latency_ms: (f64, f64),
    /// 最长等待（毫秒）：源画面变了之后，过了多久控制端才看到新画面。
    /// 正常时约等于延迟；丢包、拥塞时画面停住，这个值就变大。
    pub max_wait_ms: f64,
    /// 卡顿次数（等待超过 [`FREEZE_US`]）。
    pub freezes: u32,
    /// 阶段结束时显示的画面的亮度 PSNR（dB）。
    pub end_psnr: f64,
    /// 停下来的阶段（[`Action::Idle`]）：从阶段开始到画面清晰（PSNR ≥ [`SHARP_DB`]）用了多久（毫秒）。
    /// `None` 表示阶段内没清晰，或者不是停下来的阶段。
    pub sharp_ms: Option<f64>,
    /// 阶段结束时的源画面和显示的画面，用于截图。
    pub snapshot: (Screen, Screen),
}

/// 一个场景的结果。
#[derive(Debug, Clone)]
pub struct Outcome {
    /// 场景名。
    pub name: String,
    /// 调试记录（[`Scenario::trace`] 打开时）：发送端各模块的决定、每秒的状态。
    pub trace: Vec<String>,
    /// 编码器。
    pub codec: CodecKind,
    /// 每个阶段。
    pub phases: Vec<PhaseResult>,
    /// 发出的帧数。
    pub frames: u32,
    /// 发出的关键帧数。
    pub keyframes: u32,
    /// 判定丢失、让编码器恢复的次数。
    pub recoveries: u32,
    /// 接收端因为缺参考帧没解的帧数。
    pub skipped: u32,
    /// 接收端解码失败的帧数。
    pub bad: u32,
    /// 网络计数。
    pub net: NetStats,
    /// 全程最长等待（毫秒），从看到第一幅画面之后算。
    pub max_wait_ms: f64,
    /// 会话开始到看到第一幅画面（毫秒）；一直没看到时为 `None`。
    pub first_picture_ms: Option<f64>,
    /// 全程卡顿次数。
    pub freezes: u32,
    /// 发送端重传的分片数（和 `net.lost` 比：重传远多于丢包说明接收端在要还在路上的分片）。
    pub retransmitted: u32,
    /// 码率控制各状态的时间，以及真实队列高时被判成应用受限的时间。
    pub states: StateShare,
    /// 探测的流量和结论，和发出的全部字节（含数据报头；算探测占比用）。
    pub probes: ProbeStats,
    /// 见 [`Self::probes`]。
    pub sent_bytes: u64,
    /// 带宽估计器的名字。
    pub estimator: &'static str,
    /// 每次反馈一行特征，带着仿真知道的真实情况（以后训练 AI 模型用）。
    pub features: Vec<FeatureRow>,
    /// 对账用的几样（见 [`Tally`]）。**不进指纹**：它们只是把会话里已经发生的事多数了一遍，加它们不改变任何行为，
    /// 指纹基准因此逐字节不变。
    pub tally: Tally,
    /// 布局 2 的细分计数（中断、限速器、丢尾、CoDel、丢包层、交叉流量各多少）；布局 1 没有。**不进指纹**。
    pub path: Option<PathStats>,
    /// “路径变了”事件送到两端的时刻（微秒），按先后。布局 1 没有事件。**不进指纹**。
    pub route_changes: Vec<u64>,
    /// 布局 2 每个交叉流量源的计数，按 [`Layout2::cross`]（或 [`SegmentedPlan::cross`]）的顺序。**不进指纹**。
    pub cross: Vec<CrossStats>,
    /// 分段路径每段的真值，按 A → B 的顺序；不是分段路径时为空。**不进指纹**。
    pub segments: Vec<SegmentTruth>,
    /// 控制端收到的画面数据报字节数（含数据报头）。几对共享网络时只数这一对的。**不进指纹**。
    pub received_bytes: u64,
    /// 控制端第一次收到画面数据报的时刻（微秒，这一对的本地时间）；没收到为 `None`。**不进指纹**。
    pub first_received_us: Option<u64>,
    /// 这一对按 100 ms 一格的流量记录（RFC 8868 的指标用，见 [`flow::FlowLog`]）。**不进指纹**。
    pub flow: flow::FlowLog,
}

/// 两秒一个窗口（对账时比每个窗口发了多少字节，见 `a1-plan.md` 第 4 节）。
pub const WINDOW_US: u64 = 2_000_000;

/// 三档对账要比、而 [`Outcome`] 原来没有的量。只读会话的状态，不影响会话。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tally {
    /// 每个两秒窗口里被控端发出的字节（含数据报头，和各阶段的字节数同一个口径），按发出的时刻分窗。
    pub window_bytes: Vec<u64>,
    /// 降码率的次数：处理完一份反馈或一次定时任务之后，码率比之前低。
    pub rate_cuts: u32,
    /// 其中降幅过半的次数。
    pub rate_deep_cuts: u32,
    /// 帧率换档的次数（升、降都算）。
    pub fps_changes: u32,
    /// 按时间加权的码率中位（比特每秒）：一半的时间码率不低于它。
    pub rate_time_p50_bps: u64,
    /// 按时间加权的码率 5% 分位：95% 的时间码率不低于它。
    pub rate_time_p5_bps: u64,
}

/// 会话进行中数这几样。
#[derive(Debug, Default)]
struct Counter {
    tally: Tally,
    last_rate: Option<u64>,
    last_fps: Option<u32>,
    /// （码率，在这个码率上停了多久）。
    held: Vec<(u64, u64)>,
}

impl Counter {
    /// 被控端在 `now` 发出了 `bytes` 字节。
    fn sent(&mut self, now: Time, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let index = usize::try_from(now.micros() / WINDOW_US).unwrap_or(usize::MAX);
        if self.tally.window_bytes.len() <= index {
            self.tally.window_bytes.resize(index.saturating_add(1), 0);
        }
        if let Some(slot) = self.tally.window_bytes.get_mut(index) {
            *slot = slot.saturating_add(bytes);
        }
    }

    /// 发送端刚处理完一件事（一份反馈、一次定时任务、编了一帧）：看码率、帧率变没变。
    fn after(&mut self, rate_bps: u64, fps: u32) {
        if let Some(before) = self.last_rate.replace(rate_bps) {
            if rate_bps < before {
                self.tally.rate_cuts = self.tally.rate_cuts.saturating_add(1);
                if rate_bps.saturating_mul(2) < before {
                    self.tally.rate_deep_cuts = self.tally.rate_deep_cuts.saturating_add(1);
                }
            }
        }
        if self
            .last_fps
            .replace(fps)
            .is_some_and(|before| before != fps)
        {
            self.tally.fps_changes = self.tally.fps_changes.saturating_add(1);
        }
    }

    /// 码率是 `rate_bps` 的状态又持续了 `us` 微秒。
    fn held(&mut self, rate_bps: u64, us: u64) {
        match self.held.last_mut() {
            Some((rate, total)) if *rate == rate_bps => *total = total.saturating_add(us),
            Some(_) | None => self.held.push((rate_bps, us)),
        }
    }

    /// 会话结束：算按时间加权的分位，补齐最后一个窗口（`end` 是会话结束的时刻）。
    fn finish(mut self, end: Time) -> Tally {
        let windows = usize::try_from(end.micros().div_ceil(WINDOW_US)).unwrap_or(usize::MAX);
        if self.tally.window_bytes.len() < windows {
            self.tally.window_bytes.resize(windows, 0);
        }
        self.held.sort_unstable();
        let total: u64 = self.held.iter().map(|(_, us)| *us).sum();
        // 从低到高累加时间：累计到总时间的 p% 时的那个码率
        let at = |percent: u64| {
            let need = total.saturating_mul(percent).div_ceil(100).max(1);
            let mut seen = 0u64;
            self.held
                .iter()
                .find(|(_, us)| {
                    seen = seen.saturating_add(*us);
                    seen >= need
                })
                .map_or(0, |(rate, _)| *rate)
        };
        self.tally.rate_time_p50_bps = at(50);
        self.tally.rate_time_p5_bps = at(5);
        self.tally
    }
}

/// 码率控制各状态占了多少时间，以及量尺的核对项：仿真知道瓶颈里真实排了多久，真实队列高
/// （超过 [`TRUE_QUEUE_HIGH_US`]）的时间里，发送端有多少时间认为自己是“应用受限”。
///
/// 这个比例应当接近 0：真有队列时还当成应用受限，拥塞反应就被放宽了。2026-11-26 的几次尝试按
/// “发出量 ÷ 码率”判应用受限，1 Mbps 链路上排队超过 30 ms 的反馈里 35–44% 被判成没用满，窄带卡顿变多。
///
/// 时间按仿真的事件精确累计（不是按反馈抽样）：两个事件之间状态不变，真实排队按瓶颈速率线性排掉。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StateShare {
    /// 每个状态的时间（微秒），下标见 [`RateState::index`]。
    pub state_us: [u64; RateState::ALL.len()],
    /// 真实队列高的时间（微秒）。
    pub queued_us: u64,
    /// 真实队列高、同时处于应用受限的时间（微秒）。
    pub queued_app_limited_us: u64,
    /// 进入“核实”的次数（平均停留 = 核实的时间 ÷ 次数）。
    pub verify_entries: u32,
    /// 上一段时间是不是在核实（数进入次数用）。
    in_verify: bool,
}

impl StateShare {
    /// 记一段时间：这段时间开始时处于 `state`、瓶颈里真实排着 `backlog_us`，之后 `dt_us` 里没有新的包进瓶颈。
    pub fn add(&mut self, state: RateState, backlog_us: u64, dt_us: u64) {
        if let Some(slot) = self.state_us.get_mut(state.index()) {
            *slot = slot.saturating_add(dt_us);
        }
        // 排队按瓶颈速率线性排掉：这段时间里排队高于门限的部分是开头的 backlog − 门限
        let high = backlog_us.saturating_sub(TRUE_QUEUE_HIGH_US).min(dt_us);
        self.queued_us = self.queued_us.saturating_add(high);
        if state == RateState::AppLimited {
            self.queued_app_limited_us = self.queued_app_limited_us.saturating_add(high);
        }
        let verify = state == RateState::Verify;
        if verify && !self.in_verify && dt_us > 0 {
            self.verify_entries = self.verify_entries.saturating_add(1);
        }
        if dt_us > 0 {
            self.in_verify = verify;
        }
    }

    /// 把另一次运行的时间加进来（多次运行合计）。
    pub fn merge(&mut self, other: &Self) {
        for (a, b) in self.state_us.iter_mut().zip(other.state_us) {
            *a = a.saturating_add(b);
        }
        self.queued_us = self.queued_us.saturating_add(other.queued_us);
        self.queued_app_limited_us = self
            .queued_app_limited_us
            .saturating_add(other.queued_app_limited_us);
        self.verify_entries = self.verify_entries.saturating_add(other.verify_entries);
    }

    /// 某个状态的时间占比（千分比）。
    #[must_use]
    pub fn share_permille(&self, state: RateState) -> u64 {
        let total: u64 = self.state_us.iter().sum();
        let us = self.state_us.get(state.index()).copied().unwrap_or(0);
        us.saturating_mul(1000) / total.max(1)
    }

    /// 真实队列高的时间里被判成应用受限的比例（千分比）；真实队列从没高过时为 0。
    #[must_use]
    pub fn queued_app_limited_permille(&self) -> u64 {
        self.queued_app_limited_us.saturating_mul(1000) / self.queued_us.max(1)
    }

    /// 一行报告。
    #[must_use]
    pub fn line(&self) -> String {
        let shares: Vec<String> = RateState::ALL
            .iter()
            .map(|s| {
                let p = self.share_permille(*s);
                format!("{} {}.{}%", s.name(), p / 10, p % 10)
            })
            .collect();
        let q = self.queued_app_limited_permille();
        let verify_us = self
            .state_us
            .get(RateState::Verify.index())
            .copied()
            .unwrap_or(0);
        format!(
            "状态时间占比：{}；真实队列 > {} ms 共 {} ms，其中被判为应用受限 {}.{}%；进核实 {} 次，平均停留 {} ms",
            shares.join("、"),
            TRUE_QUEUE_HIGH_US / 1000,
            self.queued_us / 1000,
            q / 10,
            q % 10,
            self.verify_entries,
            verify_us / 1000 / u64::from(self.verify_entries.max(1))
        )
    }
}

/// 一行特征：发送端这一刻看到的，以及仿真知道的真实链路。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeatureRow {
    /// 时间（毫秒）。
    pub t_ms: u64,
    /// 估计器给出的码率（比特每秒）。
    pub rate_bps: u64,
    /// 接收端报告的排队延迟（微秒）。
    pub queue_us: u32,
    /// 延迟趋势（0.1 毫秒）。
    pub trend: i32,
    /// 包串速率（字节每秒）。
    pub train: u32,
    /// 丢包率（千分比）。
    pub loss: u16,
    /// 接收速率（字节每秒）。
    pub recv: u32,
    /// 发送端在途字节。
    pub in_flight: u64,
    /// 真实瓶颈带宽（比特每秒，0 表示不限）。
    pub true_rate_bps: u64,
    /// 真实的在途包数（链路里排着的）。
    pub true_queue_packets: usize,
    /// 瓶颈里真实排着多久的数据（微秒）。
    pub true_queue_us: u64,
    /// 码率控制处理完这份反馈之后的状态。
    pub state: RateState,
}

impl FeatureRow {
    /// CSV 表头。
    pub const HEADER: &'static str =
        "t_ms,rate_bps,queue_us,trend_0.1ms,train_Bps,loss_permille,recv_Bps,in_flight_B,true_rate_bps,true_queue_pkts,true_queue_us,state";

    /// 一行 CSV。
    #[must_use]
    pub fn csv(&self) -> String {
        format!(
            "{},{},{},{},{},{},{},{},{},{},{},{}",
            self.t_ms,
            self.rate_bps,
            self.queue_us,
            self.trend,
            self.train,
            self.loss,
            self.recv,
            self.in_flight,
            self.true_rate_bps,
            self.true_queue_packets,
            self.true_queue_us,
            self.state.name()
        )
    }
}

/// 亮度 PSNR（dB）。`a` 比 `b` 小时（缩小编码）先放大到 `b` 的尺寸；画面为空时返回 0。
#[must_use]
pub fn luma_psnr(a: &Screen, b: &Screen) -> f64 {
    // 缩小编码的画面：像控制端显示时一样拉伸回 b 的尺寸再比（缩小损失的清晰度如实算进去）
    if (a.width(), a.height()) != (b.width(), b.height()) {
        return a
            .upscale(b.width(), b.height())
            .map_or(0.0, |up| luma_psnr(&up, b));
    }
    if a.pixels().is_empty() {
        return 0.0;
    }
    let luma = |p: u32| {
        0.299 * f64::from((p >> 16) & 0xff)
            + 0.587 * f64::from((p >> 8) & 0xff)
            + 0.114 * f64::from(p & 0xff)
    };
    let sum: f64 = a
        .pixels()
        .iter()
        .zip(b.pixels())
        .map(|(x, y)| (luma(*x) - luma(*y)).powi(2))
        .sum();
    let count = f64::from(u32::try_from(a.pixels().len()).unwrap_or(u32::MAX));
    let mse = sum / count;
    if mse <= 1e-9 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 / mse).log10()
}

fn ms(us: u64) -> f64 {
    f64::from(u32::try_from(us).unwrap_or(u32::MAX)) / 1000.0
}

fn percentile(sorted: &[u64], pct: usize) -> u64 {
    let index = sorted.len().saturating_sub(1).saturating_mul(pct) / 100;
    sorted.get(index).copied().unwrap_or(0)
}

/// 正在统计的阶段。
#[derive(Debug)]
struct PhaseAcc {
    start: Time,
    bytes: u64,
    shown: u32,
    latencies: Vec<u64>,
    max_wait: u64,
    freezes: u32,
    sharp_at: Option<Time>,
}

impl PhaseAcc {
    const fn new(start: Time) -> Self {
        Self {
            start,
            bytes: 0,
            shown: 0,
            latencies: Vec::new(),
            max_wait: 0,
            freezes: 0,
            sharp_at: None,
        }
    }
}

/// 画面来源和显示端量尺：会话循环只认这个接口，不碰像素。
///
/// 两样放在一起，是因为“显示得对不对、清不清晰”只能拿显示端的画面和源画面比。现在只有像素版（[`PixelStage`]：
/// 合成桌面、真编码、真解码、比亮度信噪比）；全模型仿真（L0，见 `docs/research/Q-redesign/a1-plan.md`）
/// 以后实现同一个接口，用内容描述代替像素。
trait Stage {
    /// 被控端的画面按 `action` 走一拍；返回画面变没变。
    fn step(&mut self, action: Action) -> Result<bool, Error>;

    /// 把被控端现在的画面交给发送端编一帧，返回要发的数据报。
    fn encode(
        &self,
        tx: &mut MediaSender,
        now: Time,
        changed: bool,
        rtt_us: u64,
    ) -> Result<Vec<peerdesk_core::media::Outgoing>, Error>;

    /// 控制端收到一个画面数据报。收齐并显示了一帧时返回它的帧号。
    fn receive(
        &mut self,
        rx: &mut MediaReceiver,
        now: Time,
        header: &Header,
        body: &[u8],
    ) -> Option<u32>;

    /// 控制端显示的画面和被控端现在的画面比，算不算清晰（还什么都没显示时不算）。
    fn sharp(&self) -> bool;

    /// 阶段结束：（显示的画面和源画面的亮度信噪比，（源画面，显示的画面））。
    fn snapshot(&self) -> (f64, (Screen, Screen));
}

/// 像素版：合成桌面出真像素，编解码器是真的，清晰度按亮度信噪比判。
struct PixelStage {
    desk: RealisticDesktop,
    /// 控制端现在显示的画面。
    shown: Option<Screen>,
}

impl Stage for PixelStage {
    fn step(&mut self, action: Action) -> Result<bool, Error> {
        self.desk.step(action)
    }

    fn encode(
        &self,
        tx: &mut MediaSender,
        now: Time,
        changed: bool,
        rtt_us: u64,
    ) -> Result<Vec<peerdesk_core::media::Outgoing>, Error> {
        tx.on_frame(now, self.desk.screen(), changed, rtt_us)
    }

    fn receive(
        &mut self,
        rx: &mut MediaReceiver,
        now: Time,
        header: &Header,
        body: &[u8],
    ) -> Option<u32> {
        let frame = rx
            .on_datagram(now, header.send_time, header.seq, header.flags, body)
            .ok()
            .flatten()?;
        self.shown = Some(frame.screen.clone());
        Some(frame.frame_id)
    }

    fn sharp(&self) -> bool {
        self.shown
            .as_ref()
            .is_some_and(|d| luma_psnr(d, self.desk.screen()) >= SHARP_DB)
    }

    fn snapshot(&self) -> (f64, (Screen, Screen)) {
        let source = self.desk.screen();
        let display = self
            .shown
            .clone()
            .unwrap_or_else(|| Screen::new(source.width(), source.height(), 0));
        (luma_psnr(&display, source), (source.clone(), display))
    }
}

/// L0.5 版：合成桌面出真像素，编码是假后端、解码是假解码器；清晰度按内容版本号和 QP 判。
///
/// 源画面每变一次版本加一，同时拨给假后端（[`FakeBackend::with_version_tag`]），后端把它写进码流；假解码器从码流里读出来
/// 交回，所以“显示的是第几版”是从传输、重组、解码这一整条链上走过来的，不是量尺自己猜的。
struct DescribedStage {
    desk: RealisticDesktop,
    /// 画面哪里变了：每变一版推进一次（假后端从上一帧编出来起累计）。
    feed: ChangeFeed,
    /// 源画面现在是第几版。
    version: u64,
    tag: Arc<AtomicU64>,
    /// 解码器放在这里（和产品控制端的解码线程里一样，用 [`DecodeWorker`]；仿真是单线程，当场执行）。
    worker: DecodeWorker,
    /// 控制端现在显示的是什么。
    shown: Option<Described>,
}

impl Stage for DescribedStage {
    fn step(&mut self, action: Action) -> Result<bool, Error> {
        let moved = self.desk.step(action)?;
        if moved {
            self.version = self.version.saturating_add(1);
            self.tag.store(self.version, Ordering::Relaxed);
            self.feed.push(self.desk.changes());
        }
        Ok(moved)
    }

    fn encode(
        &self,
        tx: &mut MediaSender,
        now: Time,
        changed: bool,
        rtt_us: u64,
    ) -> Result<Vec<peerdesk_core::media::Outgoing>, Error> {
        tx.on_frame(now, self.desk.screen(), changed, rtt_us)
    }

    fn receive(
        &mut self,
        rx: &mut MediaReceiver,
        now: Time,
        header: &Header,
        body: &[u8],
    ) -> Option<u32> {
        show_described(&mut self.worker, &mut self.shown, rx, now, header, body)
    }

    fn sharp(&self) -> bool {
        self.shown
            .is_some_and(|d| d.version == self.version && d.qp <= L05_QP_SHARP)
    }

    /// 没有显示的像素：信噪比是 NaN（没量），截图的显示那一半是黑的。
    fn snapshot(&self) -> (f64, (Screen, Screen)) {
        let source = self.desk.screen();
        let blank = Screen::new(source.width(), source.height(), 0);
        (f64::NAN, (source.clone(), blank))
    }
}

/// 控制端收一个画面数据报，走延后解码（和产品控制端一样：每个解码任务一个结果，成功了报给接收端，
/// 失败了让接收端等关键帧）。显示了新的一帧时更新 `shown`，返回它的帧号。
fn show_described(
    worker: &mut DecodeWorker,
    shown: &mut Option<Described>,
    rx: &mut MediaReceiver,
    now: Time,
    header: &Header,
    body: &[u8],
) -> Option<u32> {
    let jobs = rx
        .on_datagram_deferred(now, header.send_time, header.seq, header.flags, body)
        .ok()?;
    let mut last = None;
    for job in &jobs {
        match worker.decode(job) {
            Ok(Some(Picture::Described(d))) => {
                rx.decode_succeeded();
                *shown = Some(d);
                last = Some(job.frame_id);
            }
            // 假解码器只出内容描述；真实像素和“平台解码器自己画到屏幕上”（手机）在 L0 里不会出现
            Ok(Some(Picture::Rgb(_) | Picture::Nv12(_) | Picture::Presented { .. }) | None) => {}
            Err(_) => {
                rx.decode_failed();
                last = None;
            }
        }
    }
    last
}

/// L0 版：没有像素。被控端的画面是操作时间线里的内容描述，一拍一拍重放；编码是假后端，解码是假解码器；
/// 清晰度按内容版本号和 QP 判（和 L0.5 同一个判据）。
struct TimelineStage {
    timeline: Arc<timeline::Timeline>,
    /// 下一拍的序号。
    next: usize,
    /// 现在的画面（走过第一拍之后才有）。
    content: Option<Arc<peerdesk_core::frame::ContentDesc>>,
    /// 画面哪里变了：每走到一个变了的拍就把描述里的区域推进去（和 L0.5 一样）。
    feed: ChangeFeed,
    worker: DecodeWorker,
    shown: Option<Described>,
}

impl Stage for TimelineStage {
    /// 时间线是按场景的阶段导出的，这里只管往前走一拍（`_action` 已经体现在时间线里）。
    fn step(&mut self, _action: Action) -> Result<bool, Error> {
        let (moved, content) = self
            .timeline
            .tick(self.next)
            .ok_or(Error::InvalidArgument("操作时间线比场景的阶段短"))?;
        if moved {
            self.feed.push(&Changes {
                size: content.size,
                dirty: content.dirty.clone(),
                moves: content.moves.clone(),
            });
        }
        self.content = Some(Arc::clone(content));
        self.next = self.next.saturating_add(1);
        Ok(moved)
    }

    fn encode(
        &self,
        tx: &mut MediaSender,
        now: Time,
        changed: bool,
        rtt_us: u64,
    ) -> Result<Vec<peerdesk_core::media::Outgoing>, Error> {
        let content = self
            .content
            .as_ref()
            .ok_or(Error::InvalidArgument("还没走第一拍就要编码"))?;
        tx.on_described(now, content, changed, rtt_us)
    }

    fn receive(
        &mut self,
        rx: &mut MediaReceiver,
        now: Time,
        header: &Header,
        body: &[u8],
    ) -> Option<u32> {
        show_described(&mut self.worker, &mut self.shown, rx, now, header, body)
    }

    fn sharp(&self) -> bool {
        let version = self.content.as_ref().map(|c| c.version);
        self.shown
            .is_some_and(|d| Some(d.version) == version && d.qp <= L05_QP_SHARP)
    }

    /// 两端都没有像素：信噪比是 NaN（没量），截图是一个黑点（不按画面尺寸分配：每个阶段结束都要一份，
    /// 720p 的两幅空画面比这一阶段的全部仿真还贵）。
    fn snapshot(&self) -> (f64, (Screen, Screen)) {
        let blank = Screen::new(1, 1, 0);
        (f64::NAN, (blank.clone(), blank))
    }
}

/// 一次会话的墙上时间花在哪（测量用）：环境变量 `PEERDESK_E2E_PROFILE` 打开，会话结束时往标准错误打一行。
/// 只读时钟，不影响会话。用来回答“去掉像素之后仿真能多快”（`a1-plan.md` 第 3 节）。
#[derive(Debug, Default)]
struct Stopwatch {
    on: bool,
    /// 会话开始之前的准备（L0 导出操作时间线）花了多久；不算进各段和合计。
    prepared: std::time::Duration,
    spent: [std::time::Duration; Part::COUNT],
}

/// 计时的分段。
#[derive(Debug, Clone, Copy)]
enum Part {
    /// 生成被控端的画面。
    Render,
    /// 发送端编一帧：转颜色、编码、切片、纠错。
    Encode,
    /// 节奏器出包、封数据报、交给仿真网络。
    Send,
    /// 仿真网络推进时间、取出到达的数据报、解数据报头。
    Network,
    /// 控制端：结算、出反馈；被控端：处理反馈（码率控制、重传）、定时任务。
    Control,
    /// 控制端收画面数据报，没收齐帧（只有重组、纠错）。
    Reassemble,
    /// 控制端收画面数据报，收齐了一帧（重组加解码）。
    Decode,
    /// 显示端量尺：比画面、算清晰。
    Gauge,
}

impl Part {
    const COUNT: usize = 8;
    const ALL: [(Self, &'static str); Self::COUNT] = [
        (Self::Render, "生成画面"),
        (Self::Encode, "编码和切片"),
        (Self::Send, "出包"),
        (Self::Network, "仿真网络"),
        (Self::Control, "反馈和控制"),
        (Self::Reassemble, "收包重组"),
        (Self::Decode, "收齐并解码"),
        (Self::Gauge, "显示端量尺"),
    ];
}

impl Stopwatch {
    fn from_env() -> Self {
        Self {
            on: std::env::var_os("PEERDESK_E2E_PROFILE").is_some(),
            ..Self::default()
        }
    }

    /// 开始量一段（没打开时不读时钟）。
    fn start(&self) -> Option<std::time::Instant> {
        self.on.then(std::time::Instant::now)
    }

    /// 这一段结束，记到 `part` 名下。
    fn stop(&mut self, part: Part, started: Option<std::time::Instant>) {
        if let (Some(t), Some(slot)) = (started, self.spent.get_mut(part as usize)) {
            *slot += t.elapsed();
        }
    }

    /// 一行汇总：各段的毫秒数、其余、合计、仿真了多少秒、是实时的多少倍。
    fn line(&self, name: &str, total: std::time::Duration, simulated: Time) -> String {
        let parts: Vec<String> = Part::ALL
            .iter()
            .map(|(part, label)| {
                let ms = self
                    .spent
                    .get(*part as usize)
                    .map_or(0.0, std::time::Duration::as_secs_f64)
                    * 1000.0;
                format!("{label} {ms:.0}")
            })
            .collect();
        let known: std::time::Duration = self.spent.iter().sum();
        let wall = total.as_secs_f64().max(1e-9);
        format!(
            "计时 {name}：{}，其余 {:.0}，合计 {:.0} ms；仿真 {:.1} 秒，{:.2} 倍实时；会话之前的准备 {:.0} ms",
            parts.join("，"),
            total.saturating_sub(known).as_secs_f64() * 1000.0,
            wall * 1000.0,
            ms(simulated.micros()) / 1000.0,
            ms(simulated.micros()) / 1000.0 / wall,
            self.prepared.as_secs_f64() * 1000.0
        )
    }
}

/// 和产品（`peerdesk_net::host` 的 `send_paced`）一样：节奏器是唯一出口，发出的包记进发送历史。
/// 返回这一次发到链路上的字节数。
fn send_ready(
    pacer: &mut peerdesk_core::pacer::Pacer,
    tx: &mut MediaSender,
    pacing: bool,
    net: &mut session::Tagged<'_, impl Network<Packet>>,
    now: Time,
    seq: &mut u32,
) -> Result<u64, Error> {
    let mut bytes = 0u64;
    for o in pacer.pop_ready(now, tx.pacing_bps(pacing)) {
        let header = Header {
            channel: Channel::Video,
            flags: o.flags,
            conn_id: 0,
            seq: *seq,
            send_time: now,
        };
        tx.on_packet_sent(now, *seq, &o);
        *seq = seq.wrapping_add(1);
        let datagram = encode_datagram(&header, &o.body)?;
        let len = u64::try_from(datagram.len()).unwrap_or(u64::MAX);
        bytes = bytes.saturating_add(len);
        net.send(datagram.len(), datagram);
    }
    Ok(bytes)
}

/// 网络控制器的定时任务间隔（和产品一样 25 ms）。
const CC_TICK_US: u64 = 25_000;

pub fn run(s: &Scenario) -> Result<Outcome, Error> {
    run_inner(s, false).map(|(outcome, _)| outcome)
}

/// 结果的指纹：除了调试记录（`trace`，开不开看环境变量）以外的每一个字段，连同每个阶段结束时两端的画面像素。
/// 重构时用它证明“行为逐字节不变”（见 `scripts/e2e-fingerprints.sh`）。同一提交、同一场景，指纹相同；
/// 指纹和编码器的每个字节有关，所以只在同一种系统和处理器上可比。
#[must_use]
pub fn fingerprint(o: &Outcome) -> u64 {
    fn eat(h: &mut u64, bytes: &[u8]) {
        for b in bytes {
            *h = (*h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let head = format!(
        "{} {:?} {} {} {} {} {} {:?} {:?} {:?} {} {} {:?} {:?} {} {}",
        o.name,
        o.codec,
        o.frames,
        o.keyframes,
        o.recoveries,
        o.skipped,
        o.bad,
        o.net,
        o.max_wait_ms.to_bits(),
        o.first_picture_ms.map(f64::to_bits),
        o.freezes,
        o.retransmitted,
        o.states,
        o.probes,
        o.sent_bytes,
        o.estimator
    );
    eat(&mut h, head.as_bytes());
    for p in &o.phases {
        let line = format!(
            "{:?} {} {} {} {} {} {} {} {} {:?}",
            p.action,
            p.bytes,
            p.kbps,
            p.frames_shown,
            p.latency_ms.0.to_bits(),
            p.latency_ms.1.to_bits(),
            p.max_wait_ms.to_bits(),
            p.freezes,
            p.end_psnr.to_bits(),
            p.sharp_ms.map(f64::to_bits)
        );
        eat(&mut h, line.as_bytes());
        for screen in [&p.snapshot.0, &p.snapshot.1] {
            eat(&mut h, &screen.width().to_le_bytes());
            eat(&mut h, &screen.height().to_le_bytes());
            for px in screen.pixels() {
                eat(&mut h, &px.to_le_bytes());
            }
        }
    }
    for f in &o.features {
        eat(&mut h, format!("{f:?}").as_bytes());
    }
    h
}

/// 只管“会话是怎么走的”的指纹：每份反馈的特征行、各项计数、每个阶段的流量和等待；不含编码器的名字、
/// 画面的信噪比和像素。L0.5 和 L0 是同一份内容的两种表示，这个指纹必须相同（`tests/l0.rs`）。
#[must_use]
pub fn behaviour_fingerprint(o: &Outcome) -> u64 {
    let mut stripped = o.clone();
    stripped.name = String::new();
    stripped.trace = Vec::new();
    stripped.codec = CodecKind::H264;
    for p in &mut stripped.phases {
        p.end_psnr = 0.0;
        p.snapshot = (Screen::new(1, 1, 0), Screen::new(1, 1, 0));
    }
    fingerprint(&stripped)
}

/// 环境变量 `PEERDESK_E2E_FINGERPRINTS` 给出文件路径时，每次运行往里追加一行：场景的身份和结果的指纹。
/// 测试是并行跑的，行的顺序不定，比较前先排序（`scripts/e2e-fingerprints.sh`）。
fn note_fingerprint(s: &Scenario, o: &Outcome) {
    use std::io::Write as _;
    // 对账用：`PEERDESK_E2E_BEHAVIOUR` 给出文件路径时，每次运行追加一行“场景的身份（不含编码档）+ 行为指纹”。
    // 同一批场景分别按 L0.5、L0 跑，两个文件排序后应当相同。
    if let Some(path) = std::env::var_os("PEERDESK_E2E_BEHAVIOUR") {
        let line = format!(
            "{} {}x{} fps{} {} seed{} {:016x}\n",
            s.name,
            s.width,
            s.height,
            s.fps,
            o.estimator,
            s.seed,
            behaviour_fingerprint(o)
        );
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
    let Some(path) = std::env::var_os("PEERDESK_E2E_FINGERPRINTS") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        // 一次写一整行：并行的测试各写各的，不会互相截断
        let _ = file.write_all(fingerprint_line(s, o).as_bytes());
    }
}

/// 环境变量 `PEERDESK_E2E_WORLDS` 给出目录时，每次运行把这个场景的世界文件（[`world`]）写进去，文件名是内容哈希，
/// 同一个场景重复跑只留一份。跑一遍现有测试就能拿到全部回归场景和微软轨迹的世界文件，交给批量运行器
/// （`crates/e2e/examples/batch.rs`）。不设时什么都不做；写不出来（例如场景用了没命名的画像）会在标准错误里说明，不影响这次运行。
fn note_world(s: &Scenario) {
    use std::io::Write as _;
    let Some(dir) = std::env::var_os("PEERDESK_E2E_WORLDS") else {
        return;
    };
    if let Err(e) = world::export(std::path::Path::new(&dir), s) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "世界文件没写出来（场景 {}）：{e}",
            s.name
        );
    }
}

/// 环境变量 `PEERDESK_E2E_SUMMARY` 给出文件路径时，每次运行往里追加一行关键数字（用来比 L1 和 L0.5 在同一批场景上的差别，
/// 见 `scripts/e2e-tiers.sh`）：场景的身份（和指纹文件一样，不含编码器）、编码、帧数、关键帧数、冻结次数、最长等待、没解的帧、解码失败（花屏）、
/// 发出的字节、停下阶段变清晰用的时间（毫秒，没清晰写 -）、仿真秒数、墙上毫秒。
/// 墙上时间在并行的测试里会被抢占，量速度要单线程跑（`-- --test-threads=1`）。
fn note_summary(s: &Scenario, o: &Outcome, wall: std::time::Duration, sim: Time) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("PEERDESK_E2E_SUMMARY") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(summary_line(s, o, wall, sim).as_bytes());
    }
}

/// 摘要文件里的一行（见 [`note_summary`]）。
fn summary_line(s: &Scenario, o: &Outcome, wall: std::time::Duration, sim: Time) -> String {
    let sharp: Vec<String> = o
        .phases
        .iter()
        .filter_map(|p| p.sharp_ms.map(|ms| format!("{ms:.0}")))
        .collect();
    format!(
        "{} {}x{} fps{} {} seed{} max{} start{} pace{} probe{}{} {:?}|{}|frames {}|keys {}|freezes {}|maxwait {:.0}|skipped {}|bad {}|bytes {}|sharp {}|sim {:.1}|wall {:.0}\n",
        s.name,
        s.width,
        s.height,
        s.fps,
        o.estimator,
        s.seed,
        s.max_bps,
        s.start_bps,
        u8::from(s.pacing),
        u8::from(s.start_probe),
        u8::from(s.probe_ids),
        s.fec,
        o.codec.name(),
        o.frames,
        o.keyframes,
        o.freezes,
        o.max_wait_ms,
        o.skipped,
        o.bad,
        o.sent_bytes,
        if sharp.is_empty() {
            "-".to_owned()
        } else {
            sharp.join(",")
        },
        ms(sim.micros()) / 1000.0,
        wall.as_secs_f64() * 1000.0
    )
}

/// 指纹文件里的一行：场景的身份（名字、尺寸、帧率、编码、估计器、种子、码率、开关）和结果的指纹。
fn fingerprint_line(s: &Scenario, o: &Outcome) -> String {
    // 字节模型和扰动只在不是默认值时写进身份：默认的行和加这两项之前逐字节相同，
    // 用了别的模型的行也只多了身份里的字，哈希不变；同一个场景换模型跑，各行分得开。
    let mut model = String::new();
    if s.byte_model != ByteModelKind::Default {
        model.push_str(" model-");
        model.push_str(s.byte_model.name());
    }
    if s.byte_noise != ByteNoise::Default {
        model.push_str(" noise-");
        model.push_str(s.byte_noise.name());
    }
    // 布局 2 同理：布局 1 的行逐字节不变，布局 2 的行身份里多一个 layout2
    model.push_str(s.network.identity());
    format!(
        "{} {}x{} fps{} {} {} seed{} max{} start{} pace{} probe{}{}{model} {:?} {:016x}\n",
        s.name,
        s.width,
        s.height,
        s.fps,
        o.codec.name(),
        o.estimator,
        s.seed,
        s.max_bps,
        s.start_bps,
        u8::from(s.pacing),
        u8::from(s.start_probe),
        u8::from(s.probe_ids),
        s.fec,
        fingerprint(o)
    )
}

/// 运行一个场景，同时把被控端的会话记录（`peerdesk_core::record` 的格式）写进内存，一起交回。
/// 记录只读发送端的状态，不改变会话：结果和 [`run`] 逐字节相同（`tests/record.rs` 证明）。
pub fn run_recorded(s: &Scenario) -> Result<(Outcome, Vec<u8>), Error> {
    run_inner(s, true).map(|(outcome, bytes)| (outcome, bytes.unwrap_or_default()))
}

/// 被控端的会话记录（仿真的内存模式）：没有队列和写盘线程，直接写进内存，所以没有缺口，结果也是确定的。
/// 记录点和产品的被控端循环（`peerdesk_net::host::serve`）里不用改核心库的那几处对应；产品里取得到、
/// 仿真里取不到的字段（预算、各段耗时、编码尺寸）写“不知道”，由 R5 补。
#[derive(Debug)]
struct Journal {
    sink: Sink,
    out: Vec<u8>,
    /// 下一次每秒汇总的时间（微秒）。
    next_second: u64,
    /// 上一次汇总时的发送端计数。
    last: SenderStats,
}

impl Journal {
    fn new(s: &Scenario, estimator: &str) -> Self {
        let mut meta = Meta::new(Role::Host, 0);
        meta.codec = Some(s.codec.name().to_owned());
        meta.estimator = Some(estimator.to_owned());
        meta.policy = Some(s.policy.name.to_owned());
        meta.fps = Some(s.fps);
        meta.start_bps = Some(s.start_bps);
        meta.max_bps = Some(s.max_bps);
        Self {
            sink: Sink::new(),
            out: record::encode_header(&meta),
            next_second: 1_000_000,
            last: SenderStats::default(),
        }
    }

    fn push(&mut self, now: Time, record: &Record) {
        self.sink.push(now.micros(), record, || None, &mut self.out);
    }

    /// 处理完一份反馈：发送端看到的和算出的。
    fn feedback(
        &mut self,
        now: Time,
        tx: &MediaSender,
        obs: &peerdesk_core::estimator::Observation,
    ) {
        let f = record::Feedback::from_observation(obs, tx.rate_bps(), tx.rate_state());
        self.push(now, &Record::Feedback(f));
    }

    /// 一个编码任务结束。`before` 是调用 `on_frame` 之前的计数。
    fn frame(&mut self, now: Time, tx: &MediaSender, before: SenderStats, changed: bool) {
        let after = tx.stats();
        let key = after.keyframes != before.keyframes;
        // 出了帧就是出了帧；没出帧时仿真取不到编码器的内部状态（是被预算丢了，还是没东西要发），
        // 如实写“不知道”，不猜
        let outcome = if after.frames == before.frames {
            record::FrameOutcome::NoFrameUnknown
        } else {
            record::FrameOutcome::Sent {
                bytes: u32::try_from(after.video_bytes.saturating_sub(before.video_bytes))
                    .unwrap_or(u32::MAX),
            }
        };
        let f = record::Frame {
            frame_id: before.frames.saturating_add(1),
            captured_ago_us: Some(0),
            class: record::FrameClass::of(key, changed),
            key_reason: std::borrow::Cow::Borrowed(""),
            budget_bytes: None,
            outcome,
            scale_permille: None,
            size: None,
            timing: None,
            hardware: false,
        };
        self.push(now, &Record::Frame(f));
    }

    /// 每秒汇总：到点了就写重传记录。
    fn second(&mut self, now: Time, stats: SenderStats) {
        if now.micros() >= self.next_second {
            self.retransmit(now, stats);
            while self.next_second <= now.micros() {
                self.next_second += 1_000_000;
            }
        }
    }

    fn retransmit(&mut self, now: Time, stats: SenderStats) {
        let last = std::mem::replace(&mut self.last, stats);
        let r = record::Retransmit {
            requested: stats.nack_requests.saturating_sub(last.nack_requests),
            resent: stats.retransmitted.saturating_sub(last.retransmitted),
            capped: stats.resend_capped.saturating_sub(last.resend_capped),
            lost_frames: stats.lost.saturating_sub(last.lost),
        };
        self.push(now, &Record::Retransmit(r));
    }

    /// 会话结束：最后不足一秒的汇总，结束记录。
    fn finish(mut self, now: Time, stats: SenderStats) -> Vec<u8> {
        self.retransmit(now, stats);
        self.sink.finish(
            now.micros(),
            record::EndReason::Normal,
            now.micros(),
            None,
            &mut self.out,
        );
        self.out
    }
}

fn run_inner(s: &Scenario, record: bool) -> Result<(Outcome, Option<Vec<u8>>), Error> {
    let spec = session::Spec {
        s,
        start: Time::ZERO,
        reverse: false,
        extra: 0,
    };
    let mut done = session::with_network(&s.network, s.seed, &[spec], record)?;
    let Some(one) = done.pop() else {
        return Err(Error::InvalidArgument("会话没有结果"));
    };
    note_fingerprint(s, &one.outcome);
    note_summary(s, &one.outcome, one.wall, one.end);
    note_world(s);
    Ok((one.outcome, one.recorded))
}

/// 分段路径加上交叉流量源的序号（每个源挂在哪一段、在那一段里是第几个），主循环用它取计数。
struct SegmentedWire {
    net: Segmented<Packet>,
    cross: Vec<(Segment, usize)>,
}

impl Network<Packet> for SegmentedWire {
    fn send(&mut self, from: usize, to: usize, size_bytes: usize, payload: Packet) {
        self.net.send(from, to, size_bytes, payload);
    }
    fn peek_time(&self) -> Option<Time> {
        self.net.peek_time()
    }
    fn pop(&mut self) -> Option<peerdesk_core::sim::Delivery<Packet>> {
        self.net.pop()
    }
    fn advance_to(&mut self, t: Time) {
        self.net.advance_to(t);
    }
    fn in_flight(&self) -> usize {
        self.net.in_flight()
    }
    fn backlog_us(&self, from: usize, to: usize) -> u64 {
        self.net.backlog_us(from, to)
    }
    fn stats(&self) -> NetStats {
        self.net.stats()
    }
    fn next_event_time(&self) -> Option<Time> {
        self.net.next_event_time()
    }
    fn pop_event(&mut self) -> Option<peerdesk_core::path::PathEvent> {
        self.net.pop_event()
    }
}

impl Wire for SegmentedWire {
    type Step = SegmentStep;
    fn apply(&mut self, step: &SegmentStep) {
        self.net
            .set_dir(step.segment, Dir::Forward, step.config.forward.clone());
        self.net
            .set_dir(step.segment, Dir::Reverse, step.config.reverse.clone());
        self.net.set_up(step.segment, step.up);
    }
    /// 五段两个方向的名义传播延迟之和，加 1 ms。
    fn nominal_rtt_us(&self) -> u64 {
        let now = self.net.now();
        Segment::ALL
            .iter()
            .flat_map(|s| Dir::BOTH.map(|d| (*s, d)))
            .map(|(s, d)| self.net.segment(s).dir(d).config().delay.nominal_us(now))
            .fold(1_000u64, u64::saturating_add)
    }
    /// 正向五段里最窄的容量（不限速的段不算；都不限速为 0）。
    fn true_rate_bps(&self) -> u64 {
        let now = self.net.now();
        Segment::ALL
            .iter()
            .map(|s| {
                self.net
                    .segment(*s)
                    .dir(Dir::Forward)
                    .bottleneck()
                    .rate_at(now)
            })
            .filter(|r| *r > 0)
            .min()
            .unwrap_or(0)
    }
    fn cross(&self) -> Vec<CrossStats> {
        self.cross
            .iter()
            .map(|(s, id)| self.net.segment(*s).cross_stats(*id))
            .collect()
    }
    fn segment_queues_us(&self) -> Vec<u64> {
        Segment::ALL
            .iter()
            .map(|s| self.net.segment(*s).backlog_us(HOST, VIEWER))
            .collect()
    }
    fn segment_stats(&self) -> Vec<PathStats> {
        self.net.segment_stats()
    }
}

/// 主循环看网络的另一面：按场景的时间表换参数，和两样“仿真才知道的真值”。`Net` 和 `Path` 各实现一份。
trait Wire: Network<Packet> {
    /// 时间表里的一项。
    type Step;
    /// 在这一帧开头换成这一项（布局 1 是 `set_link`，两个方向对称）。
    fn apply(&mut self, step: &Self::Step);
    /// 名义往返（微秒），交给编码那一步（和以前一样是两个方向的传播延迟加 1 ms）。
    fn nominal_rtt_us(&self) -> u64;
    /// 正向瓶颈现在名义上的容量（比特每秒，不限速为 0），填特征行的真值。
    fn true_rate_bps(&self) -> u64;
    /// 每个交叉流量源的计数（布局 1 没有交叉流量）。
    fn cross(&self) -> Vec<CrossStats> {
        Vec::new()
    }
    /// 分段路径每段正向瓶颈现在的排队（微秒）；不是分段路径时为空。
    fn segment_queues_us(&self) -> Vec<u64> {
        Vec::new()
    }
    /// 分段路径每段的计数；不是分段路径时为空。
    fn segment_stats(&self) -> Vec<PathStats> {
        Vec::new()
    }
}

impl Wire for Net<Packet> {
    type Step = Link;
    fn apply(&mut self, step: &Link) {
        self.set_link(HOST, VIEWER, *step);
    }
    // 还没有链路时按局域网算（和以前主循环里 `link` 的初值一样）
    fn nominal_rtt_us(&self) -> u64 {
        let link = self.link(HOST, VIEWER).unwrap_or_else(Link::lan);
        link.delay_us.saturating_mul(2).saturating_add(1_000)
    }
    fn true_rate_bps(&self) -> u64 {
        let link = self.link(HOST, VIEWER).unwrap_or_else(Link::lan);
        link.rate_bps.unwrap_or(0)
    }
}

impl Wire for Path<Packet> {
    type Step = PathStep;
    fn apply(&mut self, step: &PathStep) {
        self.set_dir(Dir::Forward, step.config.forward.clone());
        self.set_dir(Dir::Reverse, step.config.reverse.clone());
        self.set_up(step.up);
    }
    fn nominal_rtt_us(&self) -> u64 {
        let now = self.now();
        let one = |dir| self.dir(dir).config().delay.nominal_us(now);
        one(Dir::Forward)
            .saturating_add(one(Dir::Reverse))
            .saturating_add(1_000)
    }
    fn true_rate_bps(&self) -> u64 {
        self.dir(Dir::Forward).bottleneck().rate_at(self.now())
    }
    fn cross(&self) -> Vec<CrossStats> {
        (0..self.cross_sources())
            .map(|i| self.cross_stats(i))
            .collect()
    }
}

/// “路径变了”事件交给两端的钩子。产品怎么反应（重新探测）是 B4 的事；A2-3 只保证事件按时刻送到这里，现在什么都不做。
const fn route_changed(_tx: &mut MediaSender, _rx: &mut MediaReceiver, _at: Time) {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    /// 一个很小的结果，给报告的测试用。
    pub(crate) fn tiny() -> Outcome {
        run(&Scenario::new(
            "tiny",
            vec![Phase {
                action: Action::Idle,
                frames: 1,
            }],
        ))
        .unwrap()
    }

    #[test]
    fn l05_runs_the_real_session_code_with_a_fake_codec_and_no_corrupt_frames() {
        let mut s = Scenario::new(
            "l05",
            vec![
                Phase {
                    action: Action::Typing,
                    frames: 30,
                },
                Phase {
                    action: Action::Idle,
                    frames: 60,
                },
            ],
        );
        s.codec = CodecKind::H264L05;
        let o = run(&s).unwrap();
        assert_eq!(o.codec.name(), "h264-l05");
        assert!(o.frames > 10 && o.keyframes >= 1, "{o:?}");
        // 假解码器和真解码器一样严：接不上的帧会算解码失败。没有丢包的局域网上一帧也不该有
        assert_eq!((o.bad, o.skipped), (0, 0));
        assert!(o.first_picture_ms.is_some());
        // 停下之后补清晰到 QP 够低；没有像素可比，信噪比是 NaN
        assert!(o.phases[1].sharp_ms.is_some(), "{:?}", o.phases[1]);
        assert!(o.phases[1].end_psnr.is_nan());
        // 同一个场景再跑一遍，结果相同
        assert_eq!(fingerprint(&o), fingerprint(&run(&s).unwrap()));
        // 摘要行
        let line = summary_line(
            &s,
            &o,
            std::time::Duration::from_millis(50),
            Time(2_000_000),
        );
        assert!(
            line.contains(" seed1 ") && line.contains("|h264-l05|frames "),
            "{line}"
        );
        assert!(line.contains("|sim 2.0|wall 50\n"), "{line}");
        let plain = summary_line(
            &s,
            &Outcome {
                phases: vec![],
                ..o
            },
            std::time::Duration::ZERO,
            Time::ZERO,
        );
        assert!(plain.contains("|sharp -|"), "{plain}");
    }

    #[test]
    fn codec_kinds_and_the_environment_choice() {
        assert!(CodecKind::H264.is_h264() && CodecKind::H264L05.is_h264());
        assert!(!CodecKind::Tile.is_h264());
        assert_eq!(default_codec(None), CodecKind::H264);
        assert_eq!(
            default_codec(Some(std::ffi::OsStr::new("h264-l05"))),
            CodecKind::H264L05
        );
        assert_eq!(
            default_codec(Some(std::ffi::OsStr::new("x"))),
            CodecKind::H264
        );
    }

    #[test]
    fn bad_scenarios_and_edge_cases() {
        let mut s = Scenario::new("x", vec![]);
        assert!(run(&s).is_err(), "没有阶段");
        s.phases = vec![Phase {
            action: Action::Idle,
            frames: 1,
        }];
        s.fps = 0;
        assert!(run(&s).is_err(), "帧率为 0");
        assert_eq!(percentile(&[], 50), 0);
        let a = Screen::new(4, 4, 0);
        assert!((luma_psnr(&a, &a) - 99.0).abs() < 1e-9);
        // 小的画面放大后再比（缩小编码）；空画面为 0
        assert!((luma_psnr(&Screen::new(2, 2, 0), &a) - 99.0).abs() < 1e-9);
        assert!(luma_psnr(&Screen::new(0, 0, 0), &a).abs() < 1e-9);
        assert!(luma_psnr(&Screen::new(0, 0, 0), &Screen::new(0, 0, 0)).abs() < 1e-9);
        assert_eq!(CodecKind::Tile.name(), "tile");
    }

    #[test]
    fn state_share_counts_time_exactly() {
        let mut a = StateShare::default();
        assert_eq!(a.queued_app_limited_permille(), 0, "真实队列从没高过");
        assert_eq!(a.share_permille(RateState::Tracking), 0);
        // 跟踪 100 ms，开头真实排队 50 ms：前 20 ms 高于 30 ms 门限
        a.add(RateState::Tracking, 50_000, 100_000);
        assert_eq!((a.queued_us, a.queued_app_limited_us), (20_000, 0));
        // 应用受限 10 ms，真实排队 200 ms：整段都高，全记在应用受限上
        a.add(RateState::AppLimited, 200_000, 10_000);
        assert_eq!((a.queued_us, a.queued_app_limited_us), (30_000, 10_000));
        // 排队不到门限：不算
        a.add(RateState::AppLimited, 30_000, 90_000);
        assert_eq!((a.queued_us, a.queued_app_limited_us), (30_000, 10_000));
        assert_eq!(a.share_permille(RateState::Tracking), 500);
        assert_eq!(a.share_permille(RateState::AppLimited), 500);
        assert_eq!(a.queued_app_limited_permille(), 333);
        let mut total = a;
        total.merge(&a);
        assert_eq!(total.state_us[RateState::Tracking.index()], 200_000);
        assert_eq!(total.queued_us, 60_000);
        assert_eq!(total.queued_app_limited_us, 20_000);
        assert_eq!(
            a.line(),
            "状态时间占比：起步 0.0%、跟踪 50.0%、排空（跟踪） 0.0%、排空（应用受限） 0.0%、应用受限 50.0%、\
             核实 0.0%；真实队列 > 30 ms 共 30 ms，其中被判为应用受限 33.3%；进核实 0 次，平均停留 0 ms"
        );
        // 一次真实运行：各状态时间加起来等于场景时长；特征行带真实排队和状态
        let o = run(&Scenario::new(
            "share",
            vec![Phase {
                action: Action::Typing,
                frames: 30,
            }],
        ))
        .unwrap();
        assert_eq!(o.states.state_us.iter().sum::<u64>(), 30 * 33_333);
        assert!(FeatureRow::HEADER.ends_with("true_queue_us,state"));
        let row = o.features.first().unwrap();
        assert!(row
            .csv()
            .ends_with(&format!(",{},{}", row.true_queue_us, row.state.name())));
    }

    #[test]
    fn fec_choice_from_the_environment() {
        use peerdesk_core::transport::Fec;
        assert_eq!(fec_from(Some(std::ffi::OsStr::new("xor"))), Fec::Xor);
        assert_eq!(fec_from(Some(std::ffi::OsStr::new("rs"))), Fec::Rs);
        assert_eq!(fec_from(None), Fec::Rs);
    }

    #[test]
    fn paced_run_on_a_narrow_link() {
        let mut s = Scenario::new(
            "paced",
            vec![
                Phase {
                    action: Action::Switching,
                    frames: 10,
                },
                Phase {
                    action: Action::Idle,
                    frames: 90,
                },
            ],
        );
        s.pacing = true;
        s.trace = true;
        s.estimator = EstimatorKind::GccV1;
        s.start_bps = 500_000;
        s.network = NetworkPlan::Layout1(vec![(
            0,
            Link {
                rate_bps: Some(1_000_000),
                delay_us: 20_000,
                ..Link::lan()
            },
        )]);
        let o = run(&s).unwrap();
        assert!(o.first_picture_ms.is_some());
    }

    #[test]
    fn a_short_outage_counts_as_a_freeze() {
        let mut s = Scenario::new(
            "outage",
            vec![Phase {
                action: Action::Typing,
                frames: 40,
            }],
        );
        // 第 200 ms 起断开 500 ms 再恢复：中间画面停住，算一次超过 200 ms 的冻结
        s.network = NetworkPlan::Layout1(vec![
            (0, Link::lan()),
            (
                200_000,
                Link {
                    up: false,
                    ..Link::lan()
                },
            ),
            (700_000, Link::lan()),
        ]);
        let o = run(&s).unwrap();
        assert!(o.freezes >= 1, "{}", o.freezes);
        assert!(o.max_wait_ms >= 400.0, "{}", o.max_wait_ms);
    }

    /// 对账用的计数：按发出的时刻分两秒一个窗口；码率变低算一次降码率、降幅过半另数；帧率变了算一次换档；
    /// 码率的分位按停留的时间加权，不按反馈的条数。
    #[test]
    fn the_counter_tallies_windows_cuts_fps_steps_and_time_weighted_rates() {
        let mut c = Counter::default();
        c.sent(Time(0), 100);
        c.sent(Time(1_999_999), 50);
        c.sent(Time(2_000_000), 7);
        c.sent(Time(6_500_000), 9);
        // 第一次只是记下起点，不算变化
        c.after(8_000_000, 60);
        c.after(8_000_000, 60);
        c.after(6_000_000, 60); // 降了，不到一半
        c.after(2_000_000, 30); // 降幅过半；帧率换档
        c.after(9_000_000, 30); // 升，不算
        c.after(9_000_000, 60); // 帧率换回去
                                // 8 Mbps 停 1 秒，1 Mbps 停 9 秒（分两段记），20 Mbps 停 10 秒
        c.held(8_000_000, 1_000_000);
        c.held(1_000_000, 4_000_000);
        c.held(1_000_000, 5_000_000);
        c.held(20_000_000, 10_000_000);
        assert_eq!(c.held.len(), 3, "同一个码率连着的两段合成一段");
        let t = c.finish(Time(9_000_000));
        // 会话 9 秒：5 个窗口，最后一个没有流量也补上
        assert_eq!(t.window_bytes, [150, 7, 0, 9, 0]);
        assert_eq!((t.rate_cuts, t.rate_deep_cuts, t.fps_changes), (2, 1, 2));
        // 总共 20 秒：最低的 5%（1 秒）落在 1 Mbps；一半（10 秒）正好是 1 Mbps 9 秒加 8 Mbps 1 秒
        assert_eq!(t.rate_time_p5_bps, 1_000_000);
        assert_eq!(t.rate_time_p50_bps, 8_000_000);
        // 什么都没发生：全是 0，不出错
        let empty = Counter::default().finish(Time::ZERO);
        assert_eq!(empty, Tally::default());
    }

    /// 真跑一个会话：窗口的字节加起来等于各阶段的字节；这些量不进指纹。
    #[test]
    fn the_tally_adds_up_and_stays_out_of_the_fingerprint() {
        let s = Scenario::new(
            "tally",
            vec![
                Phase {
                    action: Action::Typing,
                    frames: 45,
                },
                Phase {
                    action: Action::Idle,
                    frames: 45,
                },
            ],
        );
        let o = run(&s).unwrap();
        assert_eq!(o.tally.window_bytes.len(), 2, "3 秒的会话是两个窗口");
        assert_eq!(o.tally.window_bytes.iter().sum::<u64>(), o.sent_bytes);
        assert!(o.tally.rate_time_p50_bps >= o.tally.rate_time_p5_bps);
        assert!(o.tally.rate_time_p5_bps > 0);
        let mut other = o.clone();
        other.tally.rate_cuts += 5;
        other.tally.window_bytes.push(1);
        assert_eq!(fingerprint(&other), fingerprint(&o));
    }

    /// 字节模型的扰动三种写法：全 σ（模型的默认）、只用序列内 σ、不加。同一个会话三种的字节数各不相同；
    /// 占位模型没有扰动，怎么写都一样。
    #[test]
    fn byte_noise_changes_the_fitted_models_only() {
        let phases = vec![
            Phase {
                action: Action::Scrolling,
                frames: 30,
            },
            Phase {
                action: Action::Idle,
                frames: 15,
            },
        ];
        for model in [ByteModelKind::Openh264V1, ByteModelKind::Openh264V2] {
            let bytes: Vec<u64> = ByteNoise::ALL
                .iter()
                .map(|(name, noise)| {
                    assert_eq!(noise.name(), *name);
                    let mut s = Scenario::new("noise", phases.clone());
                    s.codec = CodecKind::H264L05;
                    s.byte_model = model;
                    s.byte_noise = *noise;
                    run(&s).unwrap().sent_bytes
                })
                .collect();
            assert!(
                bytes[0] != bytes[1] && bytes[1] != bytes[2] && bytes[0] != bytes[2],
                "{model:?} {bytes:?}"
            );
        }
        let placeholder: Vec<u64> = ByteNoise::ALL
            .iter()
            .map(|(_, noise)| {
                let mut s = Scenario::new("noise", phases.clone());
                s.codec = CodecKind::H264L05;
                s.byte_noise = *noise;
                run(&s).unwrap().sent_bytes
            })
            .collect();
        assert!(placeholder.iter().all(|b| *b == placeholder[0]));
    }

    /// 指纹：同一场景跑两遍相同；结果里任何一个数、任何一个像素变了都不同；调试记录不算在内。
    #[test]
    fn the_fingerprint_covers_every_number_and_pixel_but_not_the_trace() {
        let s = Scenario::new(
            "fp",
            vec![Phase {
                action: Action::Typing,
                frames: 8,
            }],
        );
        let a = run(&s).unwrap();
        let b = run(&s).unwrap();
        assert_eq!(fingerprint(&a), fingerprint(&b));
        let line = fingerprint_line(&s, &a);
        assert!(line.starts_with("fp ") && line.ends_with(&format!("{:016x}\n", fingerprint(&a))));
        let mut traced = a.clone();
        traced.trace.push("调试".into());
        assert_eq!(fingerprint(&traced), fingerprint(&a));
        let mut other = a.clone();
        other.sent_bytes += 1;
        assert_ne!(fingerprint(&other), fingerprint(&a));
        let mut other = a.clone();
        if let Some(f) = other.features.last_mut() {
            f.rate_bps += 1;
        }
        assert_ne!(fingerprint(&other), fingerprint(&a));
        let mut other = a.clone();
        if let Some(p) = other.phases.first_mut() {
            p.snapshot.1.fill(0, 0, 1, 1, 0x00ab_cdef);
        }
        assert_ne!(fingerprint(&other), fingerprint(&a));
        let mut other = a.clone();
        if let Some(p) = other.phases.first_mut() {
            p.freezes += 1;
        }
        assert_ne!(fingerprint(&other), fingerprint(&a));
    }

    /// 分段计时：没打开时不读时钟、什么都不记；打开时各段累加，汇总成一行。
    #[test]
    fn the_stopwatch_only_runs_when_asked_and_reports_one_line() {
        let mut off = Stopwatch::default();
        let t = off.start();
        assert!(t.is_none());
        off.stop(Part::Encode, t);
        assert!(off.spent.iter().all(std::time::Duration::is_zero));
        let mut on = Stopwatch {
            on: true,
            ..Stopwatch::default()
        };
        for (part, _) in Part::ALL {
            let t = on.start();
            assert!(t.is_some());
            on.stop(part, t);
        }
        on.spent[Part::Encode as usize] += std::time::Duration::from_millis(40);
        let line = on.line(
            "会话",
            std::time::Duration::from_millis(100),
            Time(2_000_000),
        );
        assert!(
            line.starts_with("计时 会话：生成画面 0，编码和切片 40，"),
            "{line}"
        );
        assert!(
            line.contains("合计 100 ms；仿真 2.0 秒，20.00 倍实时"),
            "{line}"
        );
        // 环境变量没设时关着（测试环境里不设它）
        assert_eq!(
            Stopwatch::from_env().on,
            std::env::var_os("PEERDESK_E2E_PROFILE").is_some()
        );
    }

    #[test]
    fn a_short_run_with_a_network_change_and_a_dead_link() {
        let mut s = Scenario::new(
            "short",
            vec![
                Phase {
                    action: Action::Typing,
                    frames: 6,
                },
                Phase {
                    action: Action::Idle,
                    frames: 6,
                },
            ],
        );
        // 第 100 ms 起链路断开：画面停住，阶段结束时还在等
        s.network = NetworkPlan::Layout1(vec![
            (0, Link::lan()),
            (
                100_000,
                Link {
                    up: false,
                    ..Link::lan()
                },
            ),
        ]);
        let o = run(&s).unwrap();
        assert_eq!(o.phases.len(), 2);
        assert!(o.phases.iter().all(|p| p.frames_shown < 10));
    }

    fn dir(rate_bps: u64, delay_us: u64) -> DirConfig {
        DirConfig {
            bottleneck: peerdesk_core::path::BottleneckConfig {
                capacity: peerdesk_core::path::Capacity::Constant { rate_bps },
                ..peerdesk_core::path::BottleneckConfig::default()
            },
            delay: peerdesk_core::path::Delay::Fixed(delay_us),
            ..DirConfig::default()
        }
    }

    #[test]
    fn layout1_converts_to_layout2_step_by_step() {
        let links = vec![
            (0, Link::wifi()),
            (
                500_000,
                Link {
                    up: false,
                    ..Link::lan()
                },
            ),
        ];
        let plan = NetworkPlan::Layout1(links);
        assert_eq!(plan.layout(), 1);
        let NetworkPlan::Layout2(two) = plan.to_layout2(true) else {
            panic!("应当是布局 2");
        };
        assert_eq!(two.start.forward, two.start.reverse);
        assert!(two.start.forward.loss_before_bottleneck);
        assert_eq!(
            two.start.forward.noise.jitter_us,
            Link::wifi().jitter_us,
            "按 from_link 搬"
        );
        assert_eq!(two.steps.len(), 2);
        assert_eq!(two.steps[0].0, 0);
        assert!(two.steps[0].1.up && !two.steps[1].1.up);
        assert_eq!(two.steps[1].0, 500_000);
        assert!(two.routes.is_empty() && two.cross.is_empty());
        let again = NetworkPlan::Layout2(two.clone());
        assert_eq!(again.layout(), 2);
        assert_eq!(again.to_layout2(false), again, "已经是布局 2 的原样返回");
        // 没有链路：起始按局域网
        let NetworkPlan::Layout2(empty) = NetworkPlan::Layout1(Vec::new()).to_layout2(false) else {
            panic!("应当是布局 2");
        };
        assert_eq!(
            empty.start,
            PathConfig::symmetric(DirConfig::from_link(&Link::lan()))
        );
    }

    #[test]
    fn both_wires_report_the_nominal_truth() {
        let mut net: Net<Packet> = Net::new(1);
        // 还没有链路：按局域网（以前主循环里 link 的初值）
        assert_eq!(net.nominal_rtt_us(), 300 * 2 + 1_000);
        assert_eq!(net.true_rate_bps(), 0);
        net.apply(&Link {
            delay_us: 20_000,
            rate_bps: Some(4_000_000),
            ..Link::lan()
        });
        assert_eq!(net.nominal_rtt_us(), 41_000);
        assert_eq!(net.true_rate_bps(), 4_000_000);

        let mut path: Path<Packet> = Path::new(
            1,
            PathConfig {
                forward: dir(8_000_000, 20_000),
                reverse: dir(1_000_000, 5_000),
            },
        );
        assert_eq!(path.nominal_rtt_us(), 26_000, "两个方向各算各的");
        assert_eq!(path.true_rate_bps(), 8_000_000, "正向的容量");
        path.apply(&PathStep {
            config: PathConfig::symmetric(dir(2_000_000, 1_000)),
            up: false,
        });
        assert_eq!(path.nominal_rtt_us(), 3_000);
        assert_eq!(path.true_rate_bps(), 2_000_000);
        path.send(
            HOST,
            VIEWER,
            100,
            Packet {
                pair: 0,
                bytes: vec![0],
            },
        );
        assert_eq!(path.stats().outage_dropped, 1, "up=false 切断");
    }

    /// 验收第 3 条的机制部分：两个方向参数不同、一次换路、一次纯中断、一条定速背景流、一条长 TCP，会话能跑完，
    /// “路径变了”只在换路恢复的时刻送到一次（纯中断不发），布局 2 的计数折算进 `net`。
    #[test]
    fn a_layout2_session_with_a_route_change_and_cross_traffic() {
        let p = |action, frames| Phase { action, frames };
        let mut s = Scenario::fixed(
            "layout2",
            vec![
                p(Action::Typing, 30),
                p(Action::Idle, 30),
                p(Action::Typing, 30),
            ],
        );
        s.codec = CodecKind::Tile;
        s.network = NetworkPlan::Layout2(Box::new(Layout2 {
            start: PathConfig {
                forward: dir(8_000_000, 20_000),
                reverse: dir(1_000_000, 5_000),
            },
            steps: Vec::new(),
            routes: vec![
                RouteChange {
                    at: Time(1_000_000),
                    down_us: 200_000,
                    new: Some(PathConfig::symmetric(dir(3_000_000, 40_000))),
                },
                RouteChange {
                    at: Time(2_500_000),
                    down_us: 100_000,
                    new: None,
                },
            ],
            cross: vec![
                (
                    Dir::Forward,
                    CrossSpec::Cbr {
                        rate_bps: 2_000_000,
                        packet_bytes: 1_200,
                        start: Time::ZERO,
                        stop: Some(Time(1_000_000)),
                    },
                ),
                (
                    Dir::Forward,
                    CrossSpec::Tcp {
                        cfg: peerdesk_core::path::TcpConfig::default(),
                        start: Time(1_500_000),
                        bytes: None,
                    },
                ),
            ],
        }));
        let o = run(&s).unwrap();
        assert_eq!(o.route_changes, vec![1_200_000]);
        let path = o.path.unwrap();
        assert_eq!(o.net, path.net());
        assert!(path.outage_dropped > 0, "两次中断都有包丢");
        assert_eq!(o.cross.len(), 2);
        // 定速流 1 秒 2 Mbps、1200 字节一个包：约 208 个
        assert!(
            (200..=210).contains(&o.cross[0].sent_packets),
            "{:?}",
            o.cross[0]
        );
        assert!(o.cross[1].delivered_bytes > 0, "{:?}", o.cross[1]);
        assert!(Net::<Packet>::new(1).cross().is_empty());
        assert!(o.frames > 0 && o.first_picture_ms.is_some());
        assert!(fingerprint_line(&s, &o).contains(" layout2 "));
        let mut one = s.clone();
        one.network = NetworkPlan::Layout1(vec![(0, Link::lan())]);
        assert!(!fingerprint_line(&one, &o).contains(" layout2 "));
    }
}
