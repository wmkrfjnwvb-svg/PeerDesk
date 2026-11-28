//! 编码后端的体检（整体重新设计 C2，见 `docs/research/Q-redesign/design.md` 3.2 节）。
//!
//! 对任意 [`EncoderBackend`] 跑同一套检查，逐条对照契约（`backend.rs` 模块说明九条）给结论，
//! 再把实测到的属性和后端声明的 [`BackendTraits`] 对比：**声明和实测不符就算违约**。
//! 做成库函数：[`run`]（以后 `peerdesk hw-check` 在真机上调它，C4）。
//!
//! # 检查项
//!
//! | 步骤 | 做什么 | 主要对照的契约 |
//! | --- | --- | --- |
//! | 正常连续编码 | 打字式的小变化、一次整屏突变（换主题）、几帧静止 | 1、2、3（没要求不许出关键帧） |
//! | 要关键帧 | 三次要求，每次后面跟足够多的帧；被丢了的要求按契约接着要 | 3 |
//! | 码率阶跃 | 高 → 低 → 高，每次后面编一段 | 5、2 |
//! | 极低预算 | 先量一帧不受限的大小，再给它的 1/3、1/8、1/400 的预算，和 16 字节，最后给足预算恢复 | 4、3 |
//! | 停 5 秒再编 | 静止之后再交帧 | 7、8（静止后第一帧慢） |
//! | 换尺寸 | 换尺寸、同尺寸重配、交尺寸不对的画面（大的、小的）看后端会不会自己重建 | 6 |
//! | 声明核对 | QP 缺省、各种画面形式、QP 和字节数的关系 | 声明 |
//! | 连续高速 | 一帧接一帧不停顿 | 1、2 |
//!
//! 每个步骤结束都排空（`flush`、`poll`、再等一会儿），还没有下落的帧算违约。
//!
//! # 结论的格式
//!
//! [`Report`] 里每条契约一个 [`Verdict`]：守约、违约加证据（每条证据写明是哪一步、哪一帧、实测什么）、
//! 不适用（写明原因）。没有“没结论”：九条契约和“声明核对”各有且只有一个。
//! 另有实测到的属性（[`Measured`]：实际攒几帧、关键帧几帧内给、超发比例、静止后第一帧多慢……）。
//!
//! # 时间
//!
//! 体检不读系统时间，也不自己睡：“停 5 秒”“等一会儿”都通过调用方给的 `idle` 回调。
//! 仿真里它拨注入的时钟（[`crate::fake::ManualClock`]），瞬间完成；真机上它真的睡。
//! 后端自己用的是真实时钟（例如 openh264）又是同步的，回调什么也不做就行。
//!
//! # 做不到的
//!
//! - 契约 9（出错后复位）要后端出过错才能查，体检造不出错：前面的步骤里后端出过错才有结论，否则“不适用”。
//! - 契约 5 里的“立即返回”没有计时（计时要读系统时间）。
//! - 契约 8 只能查“有没有报时间”，查不出计算和等待有没有分错。

use std::time::Duration;

use crate::backend::{
    BackendError, BackendTraits, DropReason, EncoderBackend, Finished, FrameAsk, FrameInput,
    RateControl, Rates, Submitted,
};

/// 契约的条数。
const CLAUSES: usize = 9;
/// 每条契约最多列这么多条证据（够定位，免得一个毛病刷屏）。
const MAX_EVIDENCE: usize = 6;
/// 超预算容许的比例（千分之几）：码控有点误差，超过这个才算违约。
pub const BUDGET_TOLERANCE_PERMILLE: u32 = 50;
/// 关键帧的预算是普通帧的几倍（大帧规则，见 `lib.rs` 的 `BIG_FRAME_US`）。
const KEY_BUDGET_FACTOR: usize = 8;
/// 普通帧给的 QP（按 QP 编的后端用；有码控的当提示）。
const DEFAULT_QP: u8 = 30;
/// 等结果时每次往前拨的时间（微秒）。
const PATIENCE_STEP_US: u64 = 100_000;
/// 一次 `poll` 循环最多取几个结果（防后端一直有结果）。
const POLL_CAP: usize = 4096;

/// 体检的参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// 主要的编码尺寸（宽高都是偶数）。
    pub size: (u16, u16),
    /// 换尺寸时换成的尺寸。
    pub other_size: (u16, u16),
    /// 比 `other_size` 小的尺寸，用来交“尺寸不对的画面”。
    pub small_size: (u16, u16),
    /// 帧间隔（微秒）。
    pub frame_gap_us: u64,
    /// “停下”多久再编（微秒）。
    pub idle_us: u64,
    /// 等一帧出结果最多等多久（微秒）。
    pub patience_us: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            size: (640, 360),
            other_size: (480, 270),
            small_size: (320, 180),
            frame_gap_us: 33_333,
            idle_us: 5_000_000,
            patience_us: 5_000_000,
        }
    }
}

/// 一条契约的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// 守约。
    Holds,
    /// 违约，附证据。
    Breach(Vec<String>),
    /// 这条对这个后端不适用（或体检造不出检查的条件），附原因。
    NotApplicable(String),
}

/// 一条契约和它的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractResult {
    /// 第几条（1 到 9）。
    pub clause: u8,
    /// 这条契约一句话说什么。
    pub title: &'static str,
    /// 结论。
    pub verdict: Verdict,
}

const TITLES: [&str; CLAUSES] = [
    "每一帧都有下落",
    "还没好要有上限",
    "要了关键帧就要给",
    "预算是硬的",
    "改码率、帧率不重建",
    "改分辨率要说代价",
    "停多久再交帧都不能出错",
    "时间分开报",
    "出错之后回到干净的状态",
];

/// 实测到的属性（和后端声明的 [`BackendTraits`] 对比；不是结论，是数据）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Measured {
    /// 交帧并取完能取的结果之后，同时还没有下落的帧最多几个（实际攒几帧）。
    pub max_held_seen: u8,
    /// 要了关键帧，最多几帧之内给了（1 表示就是要的那一帧）；没有一次要求得到过关键帧时是 `None`。
    pub keyframe_within_seen: Option<u32>,
    /// 没要求、也不是配置后第一帧、也不是整屏突变，却出了关键帧的次数。
    pub unasked_keyframes: u32,
    /// 整屏突变时自己出了关键帧。
    pub scene_keyframe_seen: bool,
    /// Window 后端：编出的帧比预算最多多多少（千分之几，没超过是 0）。
    pub overshoot_permille: Option<u32>,
    /// 因预算放不下丢的帧数。
    pub budget_drops: u32,
    /// 因后端不收丢的帧数。
    pub not_accepting_drops: u32,
    /// `set_rates` 是否都被接受（没调过是 `None`）。
    pub set_rates_accepted: Option<bool>,
    /// 停下之后第一帧的计算时间比平时多多少（微秒）；没测到是 `None`。
    pub idle_first_frame_extra_us: Option<u64>,
    /// 平时每帧的计算时间中位数（微秒）。
    pub compute_median_us: Option<u64>,
    /// 单帧最长的等待时间（微秒）。
    pub wait_max_us: u64,
    /// 单帧最长的初始化时间（微秒）。
    pub setup_max_us: u64,
    /// 卡住的时间：最老的没出结果的那一帧交出之后，到后端报错用了多久（微秒，来自 `idle` 回调的时间）。
    pub stall_us: Option<u64>,
    /// 体检过程中后端报过的错（含步骤名）。契约允许出错，但干净的后端不该有。
    pub errors: Vec<String>,
    /// 编出了几帧（供判断“有没有数据”）。
    pub frames_encoded: u32,
}

/// 体检报告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// 后端名字。
    pub backend: String,
    /// 后端声明的属性。
    pub declared: BackendTraits,
    /// 体检没法开始的原因（打不开编码器、后端不收内存画面）。有值时所有契约都是“不适用”。
    pub not_started: Option<String>,
    /// 九条契约各一个结论，按条号排。
    pub contracts: Vec<ContractResult>,
    /// 声明和实测的核对：守约 = 声明都对得上。
    pub traits_check: Verdict,
    /// 实测的属性。
    pub measured: Measured,
}

impl Report {
    /// 第几条契约的结论。
    #[must_use]
    pub fn contract(&self, clause: u8) -> Option<&ContractResult> {
        self.contracts.iter().find(|c| c.clause == clause)
    }

    /// 第几条契约是不是违约。
    #[must_use]
    pub fn breached(&self, clause: u8) -> bool {
        self.contract(clause)
            .is_some_and(|c| matches!(c.verdict, Verdict::Breach(_)))
    }

    /// 哪些条违约（条号）。
    #[must_use]
    pub fn breaches(&self) -> Vec<u8> {
        self.contracts
            .iter()
            .filter(|c| matches!(c.verdict, Verdict::Breach(_)))
            .map(|c| c.clause)
            .collect()
    }

    /// 体检跑完了、没有违约、声明和实测对得上。
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.not_started.is_none()
            && self.breaches().is_empty()
            && !matches!(self.traits_check, Verdict::Breach(_))
    }

    /// 写成给人看的几行（日志、`hw-check` 输出）。
    #[must_use]
    pub fn render(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "编码后端“{}”体检：{}",
            self.backend,
            if self.is_clean() {
                "通过"
            } else {
                "有问题"
            }
        )];
        if let Some(why) = &self.not_started {
            lines.push(format!("没能开始：{why}"));
        }
        let rows = self
            .contracts
            .iter()
            .map(|c| (format!("契约 {} {}", c.clause, c.title), &c.verdict))
            .chain(std::iter::once(("声明核对".to_owned(), &self.traits_check)));
        for (name, verdict) in rows {
            match verdict {
                Verdict::Holds => lines.push(format!("{name}：守约")),
                Verdict::NotApplicable(why) => lines.push(format!("{name}：不适用（{why}）")),
                Verdict::Breach(evidence) => {
                    lines.push(format!("{name}：违约"));
                    lines.extend(evidence.iter().map(|e| format!("    证据：{e}")));
                }
            }
        }
        let m = &self.measured;
        lines.push(format!(
            "实测：声明攒 {} 帧、实测 {} 帧；关键帧声明 {} 帧内给、实测 {}；没要求的关键帧 {} 次，整屏突变自出关键帧 {}；超发 {}；丢帧 预算 {} 次、不收 {} 次",
            self.declared.max_held,
            m.max_held_seen,
            self.declared.keyframe_within,
            m.keyframe_within_seen
                .map_or("没测到".to_owned(), |n| format!("{n} 帧内")),
            m.unasked_keyframes,
            if m.scene_keyframe_seen { "是" } else { "否" },
            m.overshoot_permille
                .map_or("不适用".to_owned(), |o| format!("{o}‰")),
            m.budget_drops,
            m.not_accepting_drops,
        ));
        lines.push(format!(
            "实测：改码率{}；静止后第一帧多算 {}；平时计算 {}；最长等待 {} 微秒、最长初始化 {} 微秒；卡住 {}；共编出 {} 帧",
            match m.set_rates_accepted {
                Some(true) => "都接受",
                Some(false) => "被拒绝",
                None => "没测",
            },
            m.idle_first_frame_extra_us
                .map_or("没测到".to_owned(), |us| format!("{us} 微秒")),
            m.compute_median_us
                .map_or("没测到".to_owned(), |us| format!("{us} 微秒")),
            m.wait_max_us,
            m.setup_max_us,
            m.stall_us
                .map_or("没有".to_owned(), |us| format!("{us} 微秒后报错")),
            m.frames_encoded,
        ));
        lines.extend(m.errors.iter().map(|e| format!("后端报错：{e}")));
        lines
    }
}

/// 用默认参数体检。
pub fn run(backend: &mut dyn EncoderBackend, idle: &mut dyn FnMut(Duration)) -> Report {
    run_with(Options::default(), backend, idle)
}

/// 体检。`idle(d)`：让时间过去 `d`（见模块说明）。
pub fn run_with(
    options: Options,
    backend: &mut dyn EncoderBackend,
    idle: &mut dyn FnMut(Duration),
) -> Report {
    let info = backend.info().clone();
    let declared = info.traits;
    let form = if declared.inputs.i420 {
        Some(Form::I420)
    } else if declared.inputs.nv12 {
        Some(Form::Nv12)
    } else {
        None
    };
    let hi = Rates {
        bps: 4_000_000,
        fps: 30,
    };
    let mut run = Run::new(backend, idle, options, declared, hi);
    let not_started = match form {
        None => Some("后端既不收 I420 也不收 NV12，体检只给内存画面".to_owned()),
        Some(form) => {
            run.form = form;
            run.fresh(options.size, hi)
                .err()
                .map(|e| format!("配置 {}×{} 失败：{e}", options.size.0, options.size.1))
        }
    };
    if not_started.is_none() {
        run.normal();
        run.keyframes();
        run.rate_steps();
        run.tiny_budgets();
        run.after_idle();
        run.resize();
        run.declaration();
        run.burst();
    }
    run.report(info.name, not_started)
}

// ---------------------------------------------------------------------------------------------
// 确定性的合成画面

/// 画面形式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    I420,
    Nv12,
}

/// 一幅合成的“桌面”：窗口标题栏、一页页的文字、左边一条侧栏；`dark` 换主题（整屏突变）；
/// `typing` 在光标处改一小块（打字、小变化）。像素完全由参数决定。
struct Pic {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    nv12: Vec<u8>,
}

fn mix(a: u64, b: u64, c: u64) -> u64 {
    let mut x = a.wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ b.wrapping_mul(0xBF58_476D_1CE4_E5B9)
        ^ c.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    x = x.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    x ^ x >> 29
}

fn wide(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

impl Pic {
    fn new(size: (u16, u16), scene: u32, typing: u32) -> Self {
        let (w, h) = (usize::from(size.0), usize::from(size.1));
        let dark = scene % 2 == 1;
        let (bg, ink, bar) = if dark { (30, 200, 70) } else { (235, 40, 180) };
        let seed = u64::from(scene);
        let caret_row = h / 2;
        let caret_col =
            40 + usize::try_from(typing).unwrap_or(0) * 7 % w.saturating_sub(100).max(1);
        let y: Vec<u8> = (0..h)
            .flat_map(|row| {
                (0..w).map(move |col| {
                    if row < 20 {
                        return bar;
                    }
                    if typing > 0
                        && (caret_row..caret_row + 10).contains(&row)
                        && (caret_col..caret_col + 24).contains(&col)
                    {
                        let on = mix(u64::from(typing), wide(row), wide(col)) & 1 == 1;
                        return if on { ink } else { bg };
                    }
                    // 文字：每 16 行一行字（字高 10 行），每 6 列一个字（宽 5 列）
                    let (band, line, cell) = (row / 16, row % 16, col / 6);
                    let glyph = (3..13).contains(&line)
                        && col % 6 < 5
                        && col >= 8
                        && col + 8 < w
                        && (mix(seed, wide(band), wide(cell)) >> line) & 1 == 1;
                    if glyph {
                        ink
                    } else {
                        bg
                    }
                })
            })
            .collect();
        let chroma = |value: u8| -> Vec<u8> {
            (0..h / 2)
                .flat_map(|_| (0..w / 2).map(move |c| if c < w / 16 { value } else { 128 }))
                .collect()
        };
        let (u, v) = (chroma(110), chroma(140));
        let nv12 = y
            .iter()
            .copied()
            .chain(u.iter().zip(&v).flat_map(|(a, b)| [*a, *b]))
            .collect();
        Self { y, u, v, nv12 }
    }

    fn input(&self, form: Form) -> FrameInput<'_> {
        match form {
            Form::I420 => FrameInput::I420 {
                y: &self.y,
                u: &self.u,
                v: &self.v,
            },
            Form::Nv12 => FrameInput::Nv12(&self.nv12),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// 记录和分析

/// 对一帧的要求（体检这边的写法）。
#[derive(Clone, Copy)]
struct Req {
    keyframe: bool,
    budget: usize,
    qp: Option<u8>,
    /// 这一帧相对上一帧是整屏突变。
    scene: bool,
    /// 这一帧是停下之后的第一帧。
    after_idle: bool,
    /// 这一帧用来核对“首帧 QP 限制”的声明（要一个不同的 QP）。
    probe_first: bool,
}

/// 交过的一帧和它的下落。
struct Rec {
    token: u64,
    ask: FrameAsk,
    scene: bool,
    after_set_rates: bool,
    after_idle: bool,
    probe_first: bool,
    /// 这是重新配置之后交的第一帧，值是 `Reconfigured::keyframe_next`。
    reconfig: Option<bool>,
    /// 交帧时体检的时间（`idle` 回调累计，微秒）。
    at_us: u64,
    out: Option<Finished>,
}

/// 交一帧的结局。
enum Sent {
    /// 记录号。
    Rec(usize),
    /// 后端回了用法错。
    Misuse(&'static str),
    /// 后端回了“不接受”：这个信号只该来自 set_rates（契约 5），交帧时回它是用错了信号（已经记录，这一帧按没进后端退回）。
    Rejected,
    /// 后端出错（已经记录、等着复位）。
    Failed,
    /// 前面复位失败，这一步没法继续。
    Skipped,
}

/// 一个步骤的分析结果。
#[derive(Default)]
struct Findings {
    evidence: Vec<(usize, String)>,
    traits: Vec<String>,
    budget_drops: u32,
    not_accepting_drops: u32,
    overshoot: Option<u32>,
    latency: Option<u32>,
    scene_seen: bool,
    unasked: u32,
    steady_compute: Vec<u64>,
    idle_compute: Vec<u64>,
    wait_max: u64,
    setup_max: u64,
    frames: u32,
    all_zero_timing: bool,
}

/// 千分之几（`part` 比 `whole` 多多少）。
fn over_permille(bytes: usize, budget: usize) -> u32 {
    let over = u128::try_from(bytes.saturating_sub(budget)).unwrap_or(0) * 1000
        / u128::try_from(budget.max(1)).unwrap_or(1);
    u32::try_from(over).unwrap_or(u32::MAX)
}

/// 逐帧分析一个步骤的记录（只读，结论放进 [`Findings`]）。
fn analyze(recs: &[Rec], declared: &BackendTraits, step: &str) -> Findings {
    let mut f = Findings {
        all_zero_timing: true,
        ..Findings::default()
    };
    let window = declared.rate_control == RateControl::Window;
    // 欠着的关键帧：从第几帧要的
    let mut owed: Option<usize> = None;
    // 配置之后的第一个编出的帧：Some(重新配置说下一帧是不是一定关键帧)
    let mut awaiting_first: Option<bool> = None;
    for (pos, r) in recs.iter().enumerate() {
        if let Some(says_key) = r.reconfig {
            awaiting_first = Some(says_key);
            owed = None;
        }
        let label = format!("{step}：第 {pos} 帧（token {}）", r.token);
        let Some(out) = &r.out else { continue };
        match out {
            Finished::Dropped { why, .. } => {
                match why {
                    DropReason::Budget => f.budget_drops += 1,
                    DropReason::NotAccepting => f.not_accepting_drops += 1,
                }
                if r.ask.keyframe {
                    // 要求没送到，调用方接着要（体检的调用方会）
                    owed = None;
                }
            }
            Finished::Frame(out) => {
                f.frames += 1;
                let t = out.timing;
                if t.compute_us + t.wait_us + t.setup_us > 0 {
                    f.all_zero_timing = false;
                }
                f.wait_max = f.wait_max.max(t.wait_us);
                f.setup_max = f.setup_max.max(t.setup_us);
                if r.after_idle {
                    f.idle_compute.push(t.compute_us);
                } else {
                    f.steady_compute.push(t.compute_us);
                }
                if out.data.is_empty() {
                    f.evidence.push((
                        4,
                        format!("{label}交了 0 字节的“帧”，没有说丢帧，空帧冒充编好了"),
                    ));
                } else if window {
                    let over = over_permille(out.data.len(), r.ask.budget);
                    f.overshoot = Some(f.overshoot.unwrap_or(0).max(over));
                    if over > BUDGET_TOLERANCE_PERMILLE {
                        f.evidence.push((
                            4,
                            format!(
                                "{label}编出 {} 字节，预算 {}，超 {over}‰（容许 {BUDGET_TOLERANCE_PERMILLE}‰），既没丢帧也没守预算",
                                out.data.len(),
                                r.ask.budget
                            ),
                        ));
                    }
                }
                if r.probe_first {
                    probe_first_qp(declared, r, out, &label, &mut f);
                }
                let first = awaiting_first.take();
                if out.key {
                    // 要了的、欠着的、配置后第一帧、改码率之后、整屏突变，其余都是没要求的关键帧
                    if let Some(from) = owed.take().or_else(|| r.ask.keyframe.then_some(pos)) {
                        let frames = u32::try_from(pos - from + 1).unwrap_or(u32::MAX);
                        f.latency = Some(f.latency.unwrap_or(0).max(frames));
                    } else if first.is_some() {
                        // 重新配置之后第一帧是关键帧，不算没要求
                    } else if r.after_set_rates {
                        f.evidence.push((5, format!("{label}是改码率之后的第一帧，没要求却出了关键帧：改码率不许出关键帧")));
                    } else if r.scene {
                        f.scene_seen = true;
                        if !declared.scene_keyframes {
                            f.evidence.push((
                                3,
                                format!(
                                    "{label}整屏突变时自己出了关键帧，但没有声明 scene_keyframes"
                                ),
                            ));
                        }
                    } else {
                        f.unasked += 1;
                        f.evidence
                            .push((3, format!("{label}没要求、画面也只有小变化，却出了关键帧")));
                    }
                } else {
                    if first == Some(true) {
                        f.evidence.push((
                            6,
                            format!(
                                "{label}：reconfigure 说下一帧一定是关键帧，但配置之后的第一帧不是"
                            ),
                        ));
                    }
                    if r.ask.keyframe && owed.is_none() {
                        owed = Some(pos);
                    }
                }
            }
        }
    }
    if let Some(from) = owed {
        f.evidence.push((
            3,
            format!(
                "{step}：第 {from} 帧要了关键帧，之后 {} 帧过去了还没给，也没有回丢帧",
                recs.len() - from
            ),
        ));
    }
    f
}

/// 核对“首帧 QP 限制”的声明（只对按 QP 编的后端；探测帧要的 QP 和声明的限制不同）。
fn probe_first_qp(
    declared: &BackendTraits,
    r: &Rec,
    out: &crate::backend::EncodedOut,
    label: &str,
    f: &mut Findings,
) {
    if declared.rate_control != RateControl::PerFrameQp {
        return;
    }
    let (asked, setup) = (r.ask.qp, out.timing.setup_us);
    match declared.first_frame_qp {
        None => {
            if setup > 0 || out.qp.is_some_and(|q| Some(q) != asked) {
                f.traits.push(format!(
                    "{label}：声明首帧没有 QP 限制，但要 QP {asked:?} 的首帧实际按 QP {:?} 编、初始化花了 {setup} 微秒",
                    out.qp
                ));
            }
        }
        Some(pinned) => {
            if asked != Some(pinned) && setup == 0 && out.qp != Some(pinned) {
                f.traits.push(format!(
                    "{label}：声明首帧只能按 QP {pinned}，但要 QP {asked:?} 的首帧既没有多编一次初始化，也不是按 {pinned} 编的"
                ));
            }
        }
    }
}

fn median(values: &mut [u64]) -> Option<u64> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

// ---------------------------------------------------------------------------------------------
// 跑体检

struct Run<'a> {
    b: &'a mut dyn EncoderBackend,
    idle: &'a mut dyn FnMut(Duration),
    opts: Options,
    declared: BackendTraits,
    form: Form,
    step: &'static str,
    /// 体检自己的时间（`idle` 回调累计，微秒）。
    now_us: u64,
    next: u64,
    cur_size: (u16, u16),
    cur_rates: Rates,
    budget: usize,
    recs: Vec<Rec>,
    /// 交了、还没有下落的记录号。
    pending: Vec<usize>,
    /// 上一次要的关键帧被丢了，下一帧接着要。
    reask: bool,
    set_rates_mark: bool,
    reconfig_mark: Option<bool>,
    /// 出过错，下一帧之前要重新配置。
    needs_recover: bool,
    /// 复位后重新配置失败，这一步没法继续。
    broken: bool,
    /// 复位后的第一帧还没有结果。
    after_recover: bool,
    failed_seen: bool,
    evidence: Vec<Vec<String>>,
    traits: Vec<String>,
    measured: Measured,
    steady_compute: Vec<u64>,
    idle_compute: Vec<u64>,
    all_zero_timing: bool,
    scene: u32,
    typing: u32,
    /// 收到“没交过的 token”的次数（证据只记前几次）。
    strays: u32,
}

impl<'a> Run<'a> {
    fn new(
        b: &'a mut dyn EncoderBackend,
        idle: &'a mut dyn FnMut(Duration),
        opts: Options,
        declared: BackendTraits,
        rates: Rates,
    ) -> Self {
        Self {
            b,
            idle,
            opts,
            declared,
            form: Form::I420,
            step: "开始",
            now_us: 0,
            next: 0,
            cur_size: opts.size,
            cur_rates: rates,
            budget: budget_of(rates),
            recs: Vec::new(),
            pending: Vec::new(),
            reask: false,
            set_rates_mark: false,
            reconfig_mark: None,
            needs_recover: false,
            broken: false,
            after_recover: false,
            failed_seen: false,
            evidence: vec![Vec::new(); CLAUSES],
            traits: Vec::new(),
            measured: Measured::default(),
            steady_compute: Vec::new(),
            idle_compute: Vec::new(),
            all_zero_timing: true,
            scene: 0,
            typing: 0,
            strays: 0,
        }
    }

    fn wait(&mut self, us: u64) {
        (self.idle)(Duration::from_micros(us));
        self.now_us = self.now_us.saturating_add(us);
    }

    /// 记一条违约证据（同样的不重复，每条契约最多 [`MAX_EVIDENCE`] 条）。
    fn note(&mut self, clause: usize, text: String) {
        let slot = clause.checked_sub(1).and_then(|i| self.evidence.get_mut(i));
        for list in slot.into_iter() {
            if list.len() < MAX_EVIDENCE && !list.contains(&text) {
                list.push(text.clone());
            }
        }
    }

    fn note_trait(&mut self, text: String) {
        if self.traits.len() < MAX_EVIDENCE && !self.traits.contains(&text) {
            self.traits.push(text);
        }
    }

    // ---- 配置和复位 ----

    /// 排空，然后（重新）配置。失败时返回原因。
    fn fresh(&mut self, size: (u16, u16), rates: Rates) -> Result<(), String> {
        self.drain();
        match self.b.reconfigure(size, rates) {
            Ok(done) => {
                self.cur_size = size;
                self.cur_rates = rates;
                self.budget = budget_of(rates);
                self.broken = false;
                self.needs_recover = false;
                self.after_recover = false;
                self.reask = false;
                self.reconfig_mark = Some(done.keyframe_next);
                if self.b.size() != Some(size) {
                    let got = self.b.size();
                    self.note(
                        6,
                        format!(
                            "{}：reconfigure 成功，size() 却是 {got:?}，不是 {size:?}",
                            self.step
                        ),
                    );
                }
                Ok(())
            }
            Err(e) => {
                self.broken = true;
                let why = e.to_string();
                self.note(
                    6,
                    format!("{}：reconfigure {size:?} 失败：{why}", self.step),
                );
                Err(why)
            }
        }
    }

    /// 出错之后重新配置（契约 9：调用方重新 `reconfigure` 就能接着用）。
    fn recover(&mut self) {
        self.needs_recover = false;
        let (size, rates) = (self.cur_size, self.cur_rates);
        match self.b.reconfigure(size, rates) {
            Ok(done) => {
                self.reconfig_mark = Some(done.keyframe_next);
                self.after_recover = true;
            }
            Err(e) => {
                self.broken = true;
                self.note(9, format!("{}：出错复位之后重新配置失败：{e}", self.step));
            }
        }
    }

    /// 后端报错：记下来，核对契约 9，攒着的帧作废。
    fn on_error(&mut self, error: BackendError) {
        let msg = match error {
            BackendError::Misuse(m) => {
                self.measured
                    .errors
                    .push(format!("{}：意外的用法错：{m}", self.step));
                return;
            }
            // “不接受”只能来自 set_rates（契约 5）；别处回它说明后端用错了信号
            BackendError::Rejected(m) => {
                self.measured
                    .errors
                    .push(format!("{}：不该在这里回“不接受”：{m}", self.step));
                return;
            }
            BackendError::Failed(m) => m,
        };
        self.measured.errors.push(format!("{}：{msg}", self.step));
        self.failed_seen = true;
        if let Some(size) = self.b.size() {
            self.note(
                9,
                format!(
                    "{}：出错（{msg}）之后 size() 还是 {size:?}，没有复位",
                    self.step
                ),
            );
        }
        if self.after_recover {
            self.note(
                9,
                format!("{}：复位、重新配置之后的第一帧又出错：{msg}", self.step),
            );
            self.after_recover = false;
        }
        let oldest = self
            .pending
            .iter()
            .filter_map(|i| self.recs.get(*i))
            .map(|r| r.at_us)
            .min();
        if let Some(since) = oldest {
            let stuck = self.now_us.saturating_sub(since);
            self.measured.stall_us = Some(self.measured.stall_us.unwrap_or(0).max(stuck));
        }
        // 攒着的帧作废（契约 9）
        self.pending.clear();
        self.needs_recover = true;
    }

    // ---- 交帧和取结果 ----

    fn req(&self) -> Req {
        Req {
            keyframe: false,
            budget: self.budget,
            qp: Some(DEFAULT_QP),
            scene: false,
            after_idle: false,
            probe_first: false,
        }
    }

    /// 要关键帧的一帧（预算按大帧给）。
    fn key_req(&self) -> Req {
        Req {
            keyframe: true,
            budget: self.budget.saturating_mul(KEY_BUDGET_FACTOR),
            ..self.req()
        }
    }

    fn deliver(&mut self, finished: Finished) {
        let token = finished.token();
        let found = self
            .pending
            .iter()
            .position(|i| self.recs.get(*i).is_some_and(|r| r.token == token));
        let Some(at) = found else {
            self.strays += 1;
            if self.strays <= 2 {
                self.note(
                    1,
                    format!(
                        "{}：收到一个没交过、或已经有过下落的 token {token} 的结果",
                        self.step
                    ),
                );
            }
            return;
        };
        let idx = self.pending.remove(at);
        let dropped = matches!(finished, Finished::Dropped { .. });
        let asked_key = self.recs.get_mut(idx).is_some_and(|r| {
            r.out = Some(finished);
            r.ask.keyframe
        });
        // 丢了的关键帧要求没送到：下一帧接着要（契约 3）
        if dropped && asked_key {
            self.reask = true;
        }
    }

    fn poll_all(&mut self) {
        for _ in 0..POLL_CAP {
            match self.b.poll() {
                Ok(Some(f)) => self.deliver(f),
                Ok(None) => break,
                Err(e) => {
                    self.on_error(e);
                    break;
                }
            }
        }
    }

    fn note_held(&mut self) {
        let n = u8::try_from(self.pending.len()).unwrap_or(u8::MAX);
        self.measured.max_held_seen = self.measured.max_held_seen.max(n);
        if n > self.declared.max_held {
            self.note(
                2,
                format!(
                    "{}：实测同时攒着 {n} 帧没有下落，超过声明的 max_held {}",
                    self.step, self.declared.max_held
                ),
            );
        }
    }

    /// 交一帧；用法错、出错的结局交给调用方。
    fn send_raw(&mut self, pic: &Pic, req: Req) -> Sent {
        if self.needs_recover {
            self.recover();
        }
        if self.broken {
            return Sent::Skipped;
        }
        let token = 100 + 7 * self.next;
        self.next += 1;
        let ask = FrameAsk {
            token,
            budget: req.budget,
            qp: req.qp,
            keyframe: req.keyframe || std::mem::take(&mut self.reask),
        };
        let idx = self.recs.len();
        self.recs.push(Rec {
            token,
            ask,
            scene: req.scene,
            after_set_rates: std::mem::take(&mut self.set_rates_mark),
            after_idle: req.after_idle,
            probe_first: req.probe_first,
            reconfig: self.reconfig_mark.take(),
            at_us: self.now_us,
            out: None,
        });
        self.pending.push(idx);
        match self.b.submit(pic.input(self.form), ask) {
            Ok(Submitted::Ready(f)) => self.deliver(f),
            Ok(Submitted::Pending) => {
                if self.declared.max_held == 0 {
                    self.note(
                        2,
                        format!(
                            "{}：声明同步（max_held 0），交帧却回答了 Pending",
                            self.step
                        ),
                    );
                }
            }
            Err(BackendError::Failed(m)) => {
                if req.after_idle {
                    let idle = self.opts.idle_us;
                    self.note(
                        7,
                        format!("{}：停了 {idle} 微秒再交帧，后端报错：{m}", self.step),
                    );
                }
                self.on_error(BackendError::Failed(m));
                return Sent::Failed;
            }
            Err(BackendError::Misuse(m)) => {
                self.unsend(idx, req);
                return Sent::Misuse(m);
            }
            Err(e @ BackendError::Rejected(_)) => {
                self.on_error(e);
                self.unsend(idx, req);
                return Sent::Rejected;
            }
        }
        self.after_recover = false;
        self.poll_all();
        self.note_held();
        Sent::Rec(idx)
    }

    /// 交一帧，用法错算声明不符（交的是合法的画面）。
    fn send(&mut self, pic: &Pic, req: Req) -> Option<usize> {
        match self.send_raw(pic, req) {
            Sent::Rec(i) => Some(i),
            Sent::Misuse(m) => {
                self.note_trait(format!(
                    "{}：交了合法的画面（声明收的形式、给了 QP），后端回用法错：{m}",
                    self.step
                ));
                None
            }
            Sent::Rejected | Sent::Failed | Sent::Skipped => None,
        }
    }

    /// 这一帧没进后端：记录退回去，下一帧要带的标记还给下一帧。
    fn unsend(&mut self, idx: usize, req: Req) {
        self.pending.retain(|i| *i != idx);
        let (reconfig, rates_mark, asked) = self
            .recs
            .get(idx)
            .map(|r| (r.reconfig, r.after_set_rates, r.ask.keyframe))
            .unwrap_or_default();
        self.recs.truncate(idx);
        self.reconfig_mark = reconfig;
        self.set_rates_mark = rates_mark;
        self.reask = self.reask || (asked && !req.keyframe);
    }

    /// 交一帧，再等一个帧间隔。
    fn frame(&mut self, pic: &Pic, req: Req) -> Option<usize> {
        let got = self.send(pic, req);
        self.wait(self.opts.frame_gap_us);
        got
    }

    /// 打字式的一帧：在当前场景上改一小块。
    fn typing_pic(&mut self) -> Pic {
        self.typing += 1;
        Pic::new(self.cur_size, self.scene, self.typing)
    }

    /// 换一个场景（切主题），返回它的第一幅画面。
    fn new_scene(&mut self) -> Pic {
        self.scene += 1;
        self.typing = 0;
        Pic::new(self.cur_size, self.scene, 0)
    }

    /// 编 `n` 帧打字式的变化。
    fn type_frames(&mut self, n: usize) {
        for _ in 0..n {
            let pic = self.typing_pic();
            self.frame(&pic, self.req());
        }
    }

    // ---- 排空和分析 ----

    /// 让攒着的帧都有下落：`flush`、取干净、再等一会儿；还没下落的算违约。
    fn drain(&mut self) {
        if let Err(e) = self.b.flush() {
            self.on_error(e);
        }
        self.poll_all();
        let mut waited = 0;
        while !self.pending.is_empty() && waited < self.opts.patience_us {
            self.wait(PATIENCE_STEP_US);
            waited += PATIENCE_STEP_US;
            self.poll_all();
        }
        if !self.pending.is_empty() {
            let tokens: Vec<u64> = self
                .pending
                .iter()
                .filter_map(|i| self.recs.get(*i))
                .map(|r| r.token)
                .collect();
            let text = format!(
                "{}：flush 之后又等了 {waited} 微秒，还有 {} 帧没有下落（token {tokens:?}），也没报错",
                self.step,
                tokens.len()
            );
            self.note(1, text.clone());
            if self.declared.max_held > 0 {
                self.note(2, text);
            }
            self.pending.clear();
        }
    }

    /// 步骤收尾：排空、分析、清记录。
    fn finish(&mut self) {
        self.drain();
        let found = analyze(&self.recs, &self.declared, self.step);
        for (clause, text) in found.evidence {
            self.note(clause, text);
        }
        for text in found.traits {
            self.note_trait(text);
        }
        let m = &mut self.measured;
        m.budget_drops += found.budget_drops;
        m.not_accepting_drops += found.not_accepting_drops;
        m.overshoot_permille = match (m.overshoot_permille, found.overshoot) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        m.keyframe_within_seen = match (m.keyframe_within_seen, found.latency) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        m.scene_keyframe_seen |= found.scene_seen;
        m.unasked_keyframes += found.unasked;
        m.wait_max_us = m.wait_max_us.max(found.wait_max);
        m.setup_max_us = m.setup_max_us.max(found.setup_max);
        m.frames_encoded += found.frames;
        self.steady_compute.extend(found.steady_compute);
        self.idle_compute.extend(found.idle_compute);
        self.all_zero_timing &= found.all_zero_timing;
        if let Some(within) = self.measured.keyframe_within_seen {
            if within > u32::from(self.declared.keyframe_within) {
                let declared = self.declared.keyframe_within;
                self.note(
                    3,
                    format!(
                        "{}：要了关键帧，实测最晚 {within} 帧内才给，超过声明的 keyframe_within {declared}",
                        self.step
                    ),
                );
            }
        }
        self.recs.clear();
    }

    // ---- 步骤 ----

    /// 正常连续编码：打字式的小变化、一次整屏突变、几帧静止。第一帧用来核对“首帧 QP 限制”。
    fn normal(&mut self) {
        self.step = "正常连续编码";
        let pic = Pic::new(self.cur_size, self.scene, 0);
        let probe_qp = if self.declared.first_frame_qp == Some(40) {
            38
        } else {
            40
        };
        let probe = Req {
            probe_first: true,
            qp: Some(probe_qp),
            ..self.req()
        };
        self.frame(&pic, probe);
        self.type_frames(20);
        // 整屏突变：换主题。预算按大帧给
        let scene = self.new_scene();
        let change = Req {
            scene: true,
            budget: self.budget.saturating_mul(KEY_BUDGET_FACTOR),
            ..self.req()
        };
        self.frame(&scene, change);
        self.type_frames(10);
        // 静止：同一幅画面再交几次
        let still = Pic::new(self.cur_size, self.scene, self.typing);
        for _ in 0..5 {
            self.frame(&still, self.req());
        }
        self.finish();
    }

    /// 要关键帧：三次，每次后面跟足够多的帧。
    fn keyframes(&mut self) {
        self.step = "要关键帧";
        let follow = usize::from(self.declared.keyframe_within) + 8;
        self.type_frames(6);
        for _ in 0..3 {
            let pic = self.typing_pic();
            self.frame(&pic, self.key_req());
            self.type_frames(follow);
        }
        self.finish();
    }

    /// 码率阶跃：高 → 低 → 高。
    fn rate_steps(&mut self) {
        self.step = "码率阶跃";
        self.type_frames(4);
        for bps in [400_000, 4_000_000] {
            let rates = Rates { bps, fps: 30 };
            let mut reset = false;
            match self.b.set_rates(rates) {
                Ok(()) => {
                    self.measured.set_rates_accepted =
                        Some(self.measured.set_rates_accepted.unwrap_or(true));
                    self.cur_rates = rates;
                    self.budget = budget_of(rates);
                    self.set_rates_mark = true;
                }
                // 如实回“不接受”是允许的（契约 5）：状态不能变，接着用旧的码率编
                Err(BackendError::Rejected(_)) => self.measured.set_rates_accepted = Some(false),
                // 用法错只记下来，状态不该变，下面照常核对尺寸
                Err(e @ BackendError::Misuse(_)) => self.on_error(e),
                // 改码率时出错：后端应已复位（契约 9 由 on_error 核对），下一帧前重新配置
                Err(e @ BackendError::Failed(_)) => {
                    self.on_error(e);
                    reset = true;
                }
            }
            if !reset && self.b.size() != Some(self.cur_size) {
                let got = self.b.size();
                let size = self.cur_size;
                self.note(
                    5,
                    format!(
                        "{}：改码率之后 size() 是 {got:?}，不是 {size:?}，等于重建了",
                        self.step
                    ),
                );
            }
            self.type_frames(8);
        }
        self.finish();
    }

    /// 极低预算：先量一帧不受限的关键帧，再给它的 1/3、1/8、1/400 的预算，和 16 字节，最后给足预算恢复。
    fn tiny_budgets(&mut self) {
        self.step = "极低预算";
        let roomy = self.budget.saturating_mul(KEY_BUDGET_FACTOR * 8);
        let pic = self.new_scene();
        let free = self.frame(
            &pic,
            Req {
                keyframe: true,
                budget: roomy,
                ..self.req()
            },
        );
        let free_bytes = free
            .and_then(|i| self.recs.get(i))
            .and_then(|r| match &r.out {
                Some(Finished::Frame(f)) => Some(f.data.len()),
                Some(Finished::Dropped { .. }) | None => None,
            })
            .unwrap_or(roomy / 8);
        for divisor in [3, 8, 400] {
            let pic = self.new_scene();
            self.frame(
                &pic,
                Req {
                    keyframe: true,
                    budget: free_bytes / divisor,
                    scene: true,
                    ..self.req()
                },
            );
        }
        for _ in 0..3 {
            let pic = self.typing_pic();
            self.frame(
                &pic,
                Req {
                    budget: 16,
                    ..self.req()
                },
            );
        }
        // 恢复：丢了的要求接着要，预算足够就一定给
        let pic = self.typing_pic();
        self.frame(&pic, self.key_req());
        self.type_frames(self.declared.keyframe_within.into());
        self.type_frames(4);
        self.finish();
    }

    /// 停下之后再编。
    fn after_idle(&mut self) {
        self.step = "停下再编";
        let (size, rates) = (self.cur_size, self.cur_rates);
        if self.fresh(size, rates).is_err() {
            return self.finish();
        }
        self.type_frames(6);
        self.wait(self.opts.idle_us);
        let pic = self.typing_pic();
        let req = Req {
            after_idle: true,
            ..self.req()
        };
        self.frame(&pic, req);
        self.type_frames(3);
        self.finish();
    }

    /// 换尺寸、同尺寸重配、交尺寸不对的画面。
    fn resize(&mut self) {
        self.step = "换尺寸";
        let rates = self.cur_rates;
        let (main, other, small) = (self.opts.size, self.opts.other_size, self.opts.small_size);
        for size in [main, other, other] {
            if self.fresh(size, rates).is_err() {
                return self.finish();
            }
            self.type_frames(4);
        }
        // 尺寸不对的画面：一幅比配置的大（主尺寸），一幅小。后端不许自己重建，之后正常的帧还能编
        for wrong in [main, small] {
            let pic = Pic::new(wrong, self.scene, 1);
            let got = self.send_raw(
                &pic,
                Req {
                    scene: true,
                    ..self.req()
                },
            );
            let after = self.b.size();
            let outcome = match got {
                Sent::Rec(_) => "收下了",
                Sent::Misuse(_) => "回了用法错",
                Sent::Rejected => "回了“不接受”",
                Sent::Failed | Sent::Skipped => "出错了",
            };
            if after != Some(other) && !self.needs_recover {
                self.note(
                    6,
                    format!(
                        "{}：交了 {}×{} 的画面（配置的是 {}×{}），后端{outcome}，size() 变成 {after:?}：submit 不许自己重建",
                        self.step, wrong.0, wrong.1, other.0, other.1
                    ),
                );
            }
            // 悄悄换了尺寸的后端：体检换回来，免得后面的帧都错
            if after != Some(other) && !self.needs_recover && self.fresh(other, rates).is_err() {
                return self.finish();
            }
            self.type_frames(2);
        }
        // 回到主尺寸，后面的步骤用
        if self.fresh(main, rates).is_ok() {
            self.type_frames(2);
        }
        self.finish();
    }

    /// 声明核对：QP 缺省、各种画面形式、QP 和字节数的关系。
    fn declaration(&mut self) {
        self.step = "声明核对";
        let (size, rates) = (self.cur_size, self.cur_rates);
        if self.fresh(size, rates).is_err() {
            return self.finish();
        }
        let pic = self.new_scene();
        let no_qp = Req {
            qp: None,
            ..self.req()
        };
        match (self.declared.rate_control, self.send_raw(&pic, no_qp)) {
            (RateControl::PerFrameQp, Sent::Misuse(_)) | (RateControl::Window, Sent::Rec(_)) => {}
            (RateControl::PerFrameQp, _) => self.note_trait(format!(
                "{}：声明按 QP 编（每帧必须给 QP），不给 QP 却没有回用法错",
                self.step
            )),
            (RateControl::Window, _) => self.note_trait(format!(
                "{}：声明有自己的码控（QP 只是提示），不给 QP 却没有正常编出或丢弃",
                self.step
            )),
        }
        // 声明收的每种画面形式都要真的收
        for form in [Form::I420, Form::Nv12] {
            let declared = match form {
                Form::I420 => self.declared.inputs.i420,
                Form::Nv12 => self.declared.inputs.nv12,
            };
            if declared {
                let before = self.form;
                self.form = form;
                let pic = self.typing_pic();
                self.frame(&pic, self.req());
                self.form = before;
            }
        }
        // 按 QP 编：QP 小的帧必须比 QP 大的大（同一幅画面、都要关键帧、预算给足）
        if self.declared.rate_control == RateControl::PerFrameQp {
            let pic = self.new_scene();
            let sizes: Vec<Option<usize>> = [22, 44]
                .into_iter()
                .map(|qp| {
                    let got = self.frame(
                        &pic,
                        Req {
                            qp: Some(qp),
                            ..self.key_req()
                        },
                    );
                    // 只比关键帧（要的关键帧晚几帧才给的后端，这一帧可能不是）
                    got.and_then(|i| self.recs.get(i))
                        .and_then(|r| match &r.out {
                            Some(Finished::Frame(f)) if f.key => Some(f.data.len()),
                            Some(Finished::Frame(_) | Finished::Dropped { .. }) | None => None,
                        })
                })
                .collect();
            if let [Some(fine), Some(coarse)] = sizes.as_slice() {
                if fine <= coarse {
                    self.note_trait(format!(
                        "{}：声明按 QP 编，但同一幅画面 QP 22 编出 {fine} 字节、QP 44 编出 {coarse} 字节，QP 没有起作用",
                        self.step
                    ));
                }
            }
            // 后面跟几帧：要的关键帧晚到也有地方落
            self.type_frames(usize::from(self.declared.keyframe_within) + 2);
        }
        self.finish();
    }

    /// 连续高速：一帧接一帧，中间不停顿。
    fn burst(&mut self) {
        self.step = "连续高速";
        let (size, rates) = (self.cur_size, self.cur_rates);
        if self.fresh(size, rates).is_err() {
            return self.finish();
        }
        for i in 0..60 {
            let pic = self.typing_pic();
            let req = if i == 30 { self.key_req() } else { self.req() };
            self.send(&pic, req);
        }
        self.type_frames(usize::from(self.declared.keyframe_within) + 2);
        self.finish();
    }

    // ---- 结论 ----

    fn report(mut self, backend: String, not_started: Option<String>) -> Report {
        let declared = self.declared;
        let mut measured = std::mem::take(&mut self.measured);
        measured.compute_median_us = median(&mut self.steady_compute);
        measured.idle_first_frame_extra_us = self
            .idle_compute
            .iter()
            .max()
            .zip(measured.compute_median_us)
            .map(|(idle, steady)| idle.saturating_sub(steady));
        if measured.frames_encoded > 0 && self.all_zero_timing {
            self.note(
                8,
                format!(
                    "编出的 {} 帧，计算、等待、初始化时间全是 0：没有报时间",
                    measured.frames_encoded
                ),
            );
        }
        let contracts = (1..=CLAUSES)
            .map(|clause| {
                let evidence = self.evidence.get(clause - 1).cloned().unwrap_or_default();
                let verdict = if let Some(why) = &not_started {
                    Verdict::NotApplicable(format!("体检没能开始：{why}"))
                } else if !evidence.is_empty() {
                    Verdict::Breach(evidence)
                } else {
                    match clause {
                        4 if declared.rate_control == RateControl::PerFrameQp => {
                            Verdict::NotApplicable(
                                "按 QP 编，字节数由调用方的 QP 决定，超不超预算由调用方负责"
                                    .to_owned(),
                            )
                        }
                        8 if measured.frames_encoded == 0 => {
                            Verdict::NotApplicable("没有编出任何帧，没有时间可查".to_owned())
                        }
                        9 if !self.failed_seen => Verdict::NotApplicable(
                            "体检期间后端没有出过错，造不出出错的条件，没法查复位".to_owned(),
                        ),
                        _ => Verdict::Holds,
                    }
                };
                ContractResult {
                    clause: u8::try_from(clause).unwrap_or(u8::MAX),
                    title: TITLES.get(clause - 1).copied().unwrap_or(""),
                    verdict,
                }
            })
            .collect();
        let traits_check = if self.traits.is_empty() || not_started.is_some() {
            Verdict::Holds
        } else {
            Verdict::Breach(self.traits.clone())
        };
        Report {
            backend,
            declared,
            not_started,
            contracts,
            traits_check,
            measured,
        }
    }
}

/// 一帧的预算：码率按帧率平分。
fn budget_of(rates: Rates) -> usize {
    usize::try_from(rates.bps / 8 / u64::from(rates.fps.max(1))).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use std::sync::Arc;

    use super::*;
    use crate::backend::{BackendInfo, Inputs, Reconfigured};
    use crate::fake::{
        Breaches, FakeBackend, FakeConfig, Faults, ManualClock, SceneKeyframes, SlowAfterIdle,
        Stall, PRIME_SETUP_US,
    };
    use crate::soft::OpenH264Backend;

    /// 小尺寸，跑得快（默认尺寸在 openh264 的测试里用）。
    fn small() -> Options {
        Options {
            size: (192, 128),
            other_size: (160, 112),
            small_size: (128, 96),
            ..Options::default()
        }
    }

    fn check(cfg: FakeConfig) -> Report {
        check_with(small(), cfg)
    }

    fn check_with(options: Options, cfg: FakeConfig) -> Report {
        let clock = ManualClock::new();
        let mut b = FakeBackend::with_default_model(cfg, Arc::new(clock.clone()));
        run_with(options, &mut b, &mut |d| clock.advance(d))
    }

    fn evidence(report: &Report, clause: u8) -> String {
        let verdict = report.contract(clause).map(|c| &c.verdict);
        assert!(
            matches!(verdict, Some(Verdict::Breach(_))),
            "契约 {clause} 应当违约，实际是 {verdict:?}"
        );
        verdict.map_or_else(String::new, breach_text)
    }

    fn breach_text(verdict: &Verdict) -> String {
        match verdict {
            Verdict::Breach(list) => list.join("\n"),
            Verdict::Holds | Verdict::NotApplicable(_) => String::new(),
        }
    }

    fn faults(f: Faults) -> FakeConfig {
        FakeConfig {
            faults: f,
            ..FakeConfig::default()
        }
    }

    fn window(f: Faults) -> FakeConfig {
        FakeConfig {
            faults: Faults {
                window_drop: true,
                ..f
            },
            ..FakeConfig::window()
        }
    }

    fn breach(b: Breaches) -> FakeConfig {
        FakeConfig {
            breaches: b,
            ..FakeConfig::default()
        }
    }

    fn assert_clean(report: &Report) {
        assert!(report.is_clean(), "应当通过：{:#?}", report.render());
        assert!(report.measured.errors.is_empty() || report.measured.stall_us.is_some());
        assert_eq!(report.contracts.len(), 9);
    }

    // ---- 守约的后端过体检 ----

    #[test]
    fn the_default_fake_backend_passes_the_checkup() {
        let report = check(FakeConfig::default());
        assert_clean(&report);
        assert!(report.measured.errors.is_empty());
        let m = &report.measured;
        assert_eq!((m.max_held_seen, m.keyframe_within_seen), (0, Some(1)));
        assert_eq!((m.unasked_keyframes, m.scene_keyframe_seen), (0, false));
        assert_eq!((m.budget_drops, m.not_accepting_drops), (0, 0));
        assert!(m.frames_encoded > 100 && m.compute_median_us.is_some());
        assert_eq!(m.idle_first_frame_extra_us, Some(0));
        assert_eq!(m.set_rates_accepted, Some(true));
        // 按 QP 编：预算不适用；没出过错：复位没法查
        assert!(matches!(
            report.contract(4).unwrap().verdict,
            Verdict::NotApplicable(_)
        ));
        assert!(matches!(
            report.contract(9).unwrap().verdict,
            Verdict::NotApplicable(_)
        ));
        assert_eq!(report.contract(1).unwrap().title, "每一帧都有下落");
        assert!(report.contract(10).is_none());
        assert!(!report.breached(1));
        // 报告写得出来
        let text = report.render().join("\n");
        assert!(text.contains("体检：通过") && text.contains("契约 4 预算是硬的：不适用"));
        let rejected = check(faults(Faults {
            set_rates_fails: true,
            ..Faults::default()
        }));
        assert!(rejected.render().join("\n").contains("改码率被拒绝"));
        assert!(text.contains("契约 1 每一帧都有下落：守约") && text.contains("声明核对：守约"));
    }

    /// 换上实测拟合的字节模型（M2，打开残差扰动和关掉各一次；按 QP 编和按码率窗口编各一次），体检照样全过：
    /// 模型对 QP 单调、扰动不依赖 QP，所以“按 QP 编”“预算是硬的”这些判断不受影响。
    #[test]
    fn the_fitted_byte_model_passes_the_checkup_too() {
        use crate::fitted::FittedByteModel;
        for noise in [true, false] {
            for cfg in [FakeConfig::default(), FakeConfig::window()] {
                let clock = ManualClock::new();
                let model = FittedByteModel::openh264_v1().unwrap().with_noise(noise);
                let mut b = FakeBackend::new(cfg, Arc::new(clock.clone()), Box::new(model));
                let report = run_with(small(), &mut b, &mut |d| clock.advance(d));
                assert_clean(&report);
            }
        }
    }

    /// 按画面区域拟合的字节模型（M2b 的 openh264-v2）同样过体检，扰动开关、按 QP 编和按码率窗口编各一次。
    /// 体检里没有接区域信道，区域类输入只有“第一帧整屏是新的”，所以另外用一份带区域的输入把模型扫一遍 QP。
    #[test]
    fn the_region_byte_model_passes_the_checkup_too() {
        use crate::fitted_v2::RegionByteModel;
        for noise in [true, false] {
            for cfg in [FakeConfig::default(), FakeConfig::window()] {
                let clock = ManualClock::new();
                let model = RegionByteModel::openh264_v2().unwrap().with_noise(noise);
                let mut b = FakeBackend::new(cfg, Arc::new(clock.clone()), Box::new(model));
                let report = run_with(small(), &mut b, &mut |d| clock.advance(d));
                assert_clean(&report);
            }
        }
    }

    /// 同一配置体检两次，报告逐项相同（确定性）。
    #[test]
    fn the_checkup_is_deterministic() {
        let cfg = window(Faults {
            held_frames: 2,
            ..Faults::default()
        });
        assert_eq!(check(cfg.clone()), check(cfg));
    }

    #[test]
    fn a_window_backend_that_drops_when_over_budget_passes() {
        let report = check(FakeConfig::window());
        assert_clean(&report);
        let m = &report.measured;
        assert!(m.budget_drops > 0, "极低预算时一定丢了帧");
        assert_eq!(m.overshoot_permille, Some(0));
        assert_eq!(report.contract(4).unwrap().verdict, Verdict::Holds);
        // 丢掉的关键帧要求接着要，最后给了
        assert!(m.keyframe_within_seen.is_some());
    }

    #[test]
    fn quirks_the_contract_allows_are_measured_and_pass() {
        // 攒帧
        let report = check(faults(Faults {
            held_frames: 2,
            ..Faults::default()
        }));
        assert_clean(&report);
        assert_eq!(report.measured.max_held_seen, 2);
        // 要关键帧晚几帧给
        let report = check(faults(Faults {
            keyframe_delay: 2,
            ..Faults::default()
        }));
        assert_clean(&report);
        assert_eq!(report.measured.keyframe_within_seen, Some(3));
        // 整屏突变自己出关键帧（声明了）
        let report = check(faults(Faults {
            scene_keyframes: SceneKeyframes::Declared,
            ..Faults::default()
        }));
        assert_clean(&report);
        assert!(report.measured.scene_keyframe_seen);
        assert_eq!(report.measured.unasked_keyframes, 0);
        // 静止后第一帧慢
        let report = check(faults(Faults {
            slow_after_idle: Some(SlowAfterIdle {
                idle_us: 1_000_000,
                extra_us: 30_000,
            }),
            ..Faults::default()
        }));
        assert_clean(&report);
        assert_eq!(report.measured.idle_first_frame_extra_us, Some(30_000));
        // 运行中改不了码率：如实回失败，不算违约
        let report = check(faults(Faults {
            set_rates_fails: true,
            ..Faults::default()
        }));
        assert_clean(&report);
        assert_eq!(report.measured.set_rates_accepted, Some(false));
        // 不收帧
        let report = check(faults(Faults {
            not_accepting_every: Some(5),
            ..Faults::default()
        }));
        assert_clean(&report);
        assert!(report.measured.not_accepting_drops > 10);
        // 首帧 QP 限制（声明了）：多编一次初始化，时间记在 setup
        let report = check(faults(Faults {
            first_frame_prime: Some(26),
            ..Faults::default()
        }));
        assert_clean(&report);
        assert_eq!(report.measured.setup_max_us, PRIME_SETUP_US);
        // 只有等待时间
        let report = check(FakeConfig {
            wait_us: 900,
            ..FakeConfig::default()
        });
        assert_clean(&report);
        assert_eq!(report.measured.wait_max_us, 900);
        // 只丢要求的关键帧：预算放不下时回 Dropped，要求接着要，最后给了（VideoToolbox）
        let report = check(FakeConfig {
            faults: Faults {
                drop_requested_keyframe: true,
                ..Faults::default()
            },
            ..FakeConfig::window()
        });
        assert_clean(&report);
        assert!(report.measured.budget_drops > 0);
        assert_eq!(report.contract(3).unwrap().verdict, Verdict::Holds);
        // 卡住：这一帧不出，之后不收帧，到时限报错复位；复位之后能接着用
        let report = check(faults(Faults {
            stall: Some(Stall {
                after_frames: 3,
                limit_us: 400_000,
            }),
            ..Faults::default()
        }));
        assert_clean(&report);
        assert!(report.measured.stall_us.unwrap() >= 400_000);
        assert!(!report.measured.errors.is_empty());
        assert_eq!(report.contract(9).unwrap().verdict, Verdict::Holds);
        assert_eq!(report.measured.max_held_seen, 1);
    }

    #[test]
    fn openh264_passes_the_checkup() {
        let mut b = OpenH264Backend::new();
        let report = run(&mut b, &mut |_| {});
        for line in report.render() {
            println!("{line}");
        }
        assert_clean(&report);
        let m = &report.measured;
        assert_eq!(m.max_held_seen, 0);
        assert_eq!(m.keyframe_within_seen, Some(1));
        assert_eq!(m.unasked_keyframes, 0);
        assert!(m.errors.is_empty() && m.setup_max_us > 0);
        assert!(m.compute_median_us.is_some());
    }

    // ---- 违约：每一种单独打开，体检都抓得到，证据对得上 ----

    #[test]
    fn unasked_keyframes_are_caught() {
        let report = check(breach(Breaches {
            periodic_keyframes: Some(7),
            ..Breaches::default()
        }));
        assert!(evidence(&report, 3).contains("没要求、画面也只有小变化，却出了关键帧"));
        assert!(report.measured.unasked_keyframes > 3);
        // 整屏突变自己出关键帧，但没声明
        let report = check(faults(Faults {
            scene_keyframes: SceneKeyframes::Undeclared,
            ..Faults::default()
        }));
        assert!(evidence(&report, 3).contains("没有声明 scene_keyframes"));
        assert!(report.measured.scene_keyframe_seen);
        assert_eq!(report.measured.unasked_keyframes, 0);
    }

    #[test]
    fn ignored_and_late_keyframes_are_caught() {
        let report = check(breach(Breaches {
            ignore_keyframe_requests: true,
            ..Breaches::default()
        }));
        assert!(evidence(&report, 3).contains("要了关键帧，之后"));
        // 实际晚给，声明说 1 帧
        let mut cfg = faults(Faults {
            keyframe_delay: 3,
            ..Faults::default()
        });
        cfg.declared = Some(FakeConfig::default().honest_traits());
        let report = check(cfg);
        assert!(evidence(&report, 3).contains("实测最晚 4 帧内才给，超过声明的 keyframe_within 1"));
        assert_eq!(report.measured.keyframe_within_seen, Some(4));
    }

    #[test]
    fn lost_frames_are_caught() {
        let report = check(breach(Breaches {
            lose_frame_every: Some(25),
            ..Breaches::default()
        }));
        assert!(evidence(&report, 1).contains("没有下落"));
        // 同步的后端不许回 Pending
        assert!(evidence(&report, 2).contains("声明同步（max_held 0），交帧却回答了 Pending"));
        // 异步的后端：flush 之后还没有下落，同时算违约 2
        let mut cfg = breach(Breaches {
            lose_frame_every: Some(25),
            ..Breaches::default()
        });
        cfg.faults.held_frames = 1;
        let report = check(cfg);
        assert!(evidence(&report, 1).contains("还有 1 帧没有下落"));
        assert!(evidence(&report, 2).contains("还有 1 帧没有下落"));
    }

    #[test]
    fn budget_violations_are_caught() {
        // 码控放不下又不丢：超预算的帧
        let report = check(FakeConfig::window().tap(|c| c.faults.window_drop = false));
        let text = evidence(&report, 4);
        assert!(text.contains("既没丢帧也没守预算") && text.contains("容许 50‰"));
        // 超发：比预算多 30%
        let report = check(window(Faults {
            overshoot_permille: 300,
            ..Faults::default()
        }));
        assert!(evidence(&report, 4).contains("容许 50‰"));
        let over = report.measured.overshoot_permille.unwrap();
        assert!((290..=300).contains(&over), "实测超发 {over}‰");
        // 空帧冒充
        let report =
            check(window(Faults::default()).tap(|c| c.breaches.empty_frame_on_drop = true));
        assert!(evidence(&report, 4).contains("交了 0 字节的“帧”"));
    }

    #[test]
    fn rate_and_size_violations_are_caught() {
        let report = check(breach(Breaches {
            set_rates_forces_keyframe: true,
            ..Breaches::default()
        }));
        assert!(evidence(&report, 5).contains("改码率之后的第一帧，没要求却出了关键帧"));
        let report = check(breach(Breaches {
            first_frame_not_key: true,
            ..Breaches::default()
        }));
        assert!(evidence(&report, 6).contains("一定是关键帧，但配置之后的第一帧不是"));
        let report = check(breach(Breaches {
            resize_silently: true,
            ..Breaches::default()
        }));
        let text = evidence(&report, 6);
        assert!(text.contains("submit 不许自己重建") && text.contains("160×112"));
    }

    #[test]
    fn idle_timing_and_reset_violations_are_caught() {
        let report = check(breach(Breaches {
            error_after_idle_us: Some(4_000_000),
            ..Breaches::default()
        }));
        assert!(evidence(&report, 7).contains("停了 5000000 微秒再交帧，后端报错"));
        // 出过错，复位之后能接着用：契约 9 守约
        assert_eq!(report.contract(9).unwrap().verdict, Verdict::Holds);
        let report = check(breach(Breaches {
            zero_timing: true,
            ..Breaches::default()
        }));
        assert!(evidence(&report, 8).contains("全是 0：没有报时间"));
        let report = check(
            faults(Faults {
                stall: Some(Stall {
                    after_frames: 3,
                    limit_us: 400_000,
                }),
                ..Faults::default()
            })
            .tap(|c| c.breaches.keep_state_after_error = true),
        );
        assert!(evidence(&report, 9).contains("没有复位"));
    }

    #[test]
    fn stuck_without_an_error_and_flush_that_forgets_are_caught() {
        let report = check(faults(Faults {
            stall: Some(Stall {
                after_frames: 3,
                limit_us: u64::MAX,
            }),
            ..Faults::default()
        }));
        let text = evidence(&report, 1);
        assert!(
            text.contains("又等了 5000000 微秒，还有 1 帧没有下落") && text.contains("也没报错")
        );
        assert!(evidence(&report, 2).contains("还有 1 帧没有下落"));
        assert_eq!(report.measured.stall_us, None);
        let report = check(
            faults(Faults {
                held_frames: 1,
                ..Faults::default()
            })
            .tap(|c| c.breaches.flush_forgets = true),
        );
        assert!(evidence(&report, 1).contains("还有 1 帧没有下落"));
        assert!(evidence(&report, 2).contains("还有 1 帧没有下落"));
    }

    // ---- 声明和实测不符 ----

    fn lying(real: FakeConfig, declared: BackendTraits) -> FakeConfig {
        FakeConfig {
            declared: Some(declared),
            ..real
        }
    }

    fn traits_breach(report: &Report) -> String {
        assert!(
            matches!(report.traits_check, Verdict::Breach(_)),
            "声明核对应当违约：{:?}",
            report.traits_check
        );
        breach_text(&report.traits_check)
    }

    #[test]
    fn declarations_that_do_not_match_are_breaches() {
        let honest = FakeConfig::default().honest_traits();
        // 攒帧比声明的多
        let report = check(lying(
            faults(Faults {
                held_frames: 2,
                ..Faults::default()
            }),
            honest,
        ));
        assert!(evidence(&report, 2).contains("实测同时攒着 1 帧没有下落，超过声明的 max_held 0"));
        assert!(evidence(&report, 2).contains("实测同时攒着 2 帧"));
        assert!(!report.is_clean());
        // 按 QP 编，声明有码控：不给 QP 回了用法错
        let report = check(lying(
            FakeConfig::default(),
            BackendTraits {
                rate_control: RateControl::Window,
                ..honest
            },
        ));
        assert!(traits_breach(&report).contains("声明有自己的码控"));
        // 有码控，声明按 QP 编：不给 QP 也收了
        let report = check(lying(FakeConfig::window(), honest));
        assert!(traits_breach(&report)
            .contains("声明按 QP 编（每帧必须给 QP），不给 QP 却没有回用法错"));
        // 声明收 NV12，其实只收 I420 之外的……：声明收的形式回了用法错
        let report = check(lying(
            FakeConfig {
                inputs: Inputs {
                    i420: false,
                    nv12: true,
                    gpu: false,
                    described: false,
                },
                ..FakeConfig::default()
            },
            honest,
        ));
        assert!(traits_breach(&report).contains("后端回用法错"));
        // 首帧 QP 限制没声明，其实有
        let report = check(lying(
            faults(Faults {
                first_frame_prime: Some(26),
                ..Faults::default()
            }),
            honest,
        ));
        assert!(traits_breach(&report).contains("声明首帧没有 QP 限制"));
        // 声明首帧只能按 QP 26，其实没有限制：要 QP 40 的首帧既没多编、也没按 26
        let report = check(lying(
            FakeConfig::default(),
            BackendTraits {
                first_frame_qp: Some(26),
                ..honest
            },
        ));
        assert!(traits_breach(&report).contains("声明首帧只能按 QP 26"));
        // 声明的限制正好是探测用的 40：换 38 探测
        let report = check(lying(
            faults(Faults {
                first_frame_prime: Some(40),
                ..Faults::default()
            }),
            BackendTraits {
                first_frame_qp: Some(40),
                ..honest
            },
        ));
        assert!(report.is_clean(), "{:?}", report.render());
    }

    // ---- 体检自己的边角 ----

    #[test]
    fn the_checkup_does_not_start_without_a_usable_backend() {
        // 打不开：奇数尺寸
        let report = check_with(
            Options {
                size: (63, 64),
                ..small()
            },
            FakeConfig::default(),
        );
        assert!(report
            .not_started
            .as_deref()
            .unwrap()
            .contains("配置 63×64 失败"));
        assert!(report
            .contracts
            .iter()
            .all(|c| matches!(c.verdict, Verdict::NotApplicable(_))));
        assert!(!report.is_clean());
        let text = report.render().join("\n");
        assert!(text.contains("没能开始") && text.contains("体检没能开始"));
        // 只收显卡画面：体检不给显卡画面
        let report = check(FakeConfig {
            inputs: Inputs {
                i420: false,
                nv12: false,
                gpu: true,
                described: false,
            },
            ..FakeConfig::default()
        });
        assert!(report
            .not_started
            .as_deref()
            .unwrap()
            .contains("既不收 I420 也不收 NV12"));
    }

    /// 包一层，造出假后端造不出的毛病（用来走体检自己的各个出口）。
    #[derive(Debug)]
    struct Odd {
        inner: FakeBackend,
        poll_misuse: bool,
        stray_once: bool,
        shift_tokens: bool,
        fail_reconfig_from: Option<u32>,
        fail_after_reconfig: bool,
        size_lie: Option<(u16, u16)>,
        set_rates_error: Option<BackendError>,
        submit_rejects_once: bool,
        reconfigs: u32,
        just_reconfigured: bool,
        flush_fails: bool,
    }

    impl Odd {
        fn new(cfg: FakeConfig) -> (Self, ManualClock) {
            let clock = ManualClock::new();
            let odd = Self {
                inner: FakeBackend::with_default_model(cfg, Arc::new(clock.clone())),
                poll_misuse: false,
                stray_once: false,
                shift_tokens: false,
                fail_reconfig_from: None,
                fail_after_reconfig: false,
                size_lie: None,
                set_rates_error: None,
                submit_rejects_once: false,
                reconfigs: 0,
                just_reconfigured: false,
                flush_fails: false,
            };
            (odd, clock)
        }

        fn check(mut self, clock: &ManualClock) -> Report {
            run_with(small(), &mut self, &mut |d| clock.advance(d))
        }
    }

    impl EncoderBackend for Odd {
        fn info(&self) -> &BackendInfo {
            self.inner.info()
        }
        fn size(&self) -> Option<(u16, u16)> {
            self.size_lie.or_else(|| self.inner.size())
        }
        fn reconfigure(
            &mut self,
            size: (u16, u16),
            rates: Rates,
        ) -> Result<Reconfigured, BackendError> {
            self.reconfigs += 1;
            if self.fail_reconfig_from.is_some_and(|n| self.reconfigs >= n) {
                return Err(BackendError::Failed("打不开".to_owned()));
            }
            self.just_reconfigured = true;
            self.inner.reconfigure(size, rates)
        }
        fn set_rates(&mut self, rates: Rates) -> Result<(), BackendError> {
            if let Some(e) = self.set_rates_error.take() {
                return Err(e);
            }
            self.inner.set_rates(rates)
        }
        fn submit(
            &mut self,
            input: FrameInput<'_>,
            ask: FrameAsk,
        ) -> Result<Submitted, BackendError> {
            if std::mem::take(&mut self.submit_rejects_once) {
                return Err(BackendError::Rejected("交帧时不该回这个".to_owned()));
            }
            if self.fail_after_reconfig
                && self.reconfigs >= 2
                && std::mem::take(&mut self.just_reconfigured)
            {
                return Err(BackendError::Failed("复位之后第一帧就坏".to_owned()));
            }
            let got = self.inner.submit(input, ask)?;
            Ok(match got {
                Submitted::Ready(Finished::Frame(mut f)) if self.shift_tokens => {
                    f.token += 1;
                    Submitted::Ready(Finished::Frame(f))
                }
                other @ (Submitted::Ready(_) | Submitted::Pending) => other,
            })
        }
        fn poll(&mut self) -> Result<Option<Finished>, BackendError> {
            if self.poll_misuse {
                return Err(BackendError::Misuse("poll 不该回用法错"));
            }
            if std::mem::take(&mut self.stray_once) {
                return Ok(Some(Finished::Dropped {
                    token: 1,
                    why: DropReason::Budget,
                }));
            }
            self.inner.poll()
        }
        fn flush(&mut self) -> Result<(), BackendError> {
            if std::mem::take(&mut self.flush_fails) {
                return Err(BackendError::Failed("flush 时出错".to_owned()));
            }
            self.inner.flush()
        }
    }

    fn stalls() -> FakeConfig {
        faults(Faults {
            stall: Some(Stall {
                after_frames: 3,
                limit_us: 400_000,
            }),
            ..Faults::default()
        })
    }

    #[test]
    fn tokens_are_matched_not_ordered() {
        // 交一个体检没交过的 token 的结果
        let (mut odd, clock) = Odd::new(FakeConfig::default());
        odd.stray_once = true;
        assert!(evidence(&odd.check(&clock), 1).contains("没交过、或已经有过下落的 token 1"));
        // 带回的 token 不对：对不上号，这一帧也一直没有下落
        let (mut odd, clock) = Odd::new(FakeConfig::default());
        odd.shift_tokens = true;
        let text = evidence(&odd.check(&clock), 1);
        assert!(text.contains("没交过") && text.contains("没有下落"));
    }

    #[test]
    fn rejected_failed_and_misuse_from_set_rates_mean_different_things() {
        // 出错：按契约 9 核对复位（这层包装不复位，所以记违约），之后重新配置接着编
        let (mut odd, clock) = Odd::new(FakeConfig::default());
        odd.set_rates_error = Some(BackendError::Failed("改码率时坏了".to_owned()));
        let report = odd.check(&clock);
        assert!(report
            .measured
            .errors
            .iter()
            .any(|e| e.contains("改码率时坏了")));
        assert!(evidence(&report, 9).contains("没有复位"));
        // 用法错：只记下来，状态不变，不算违约
        let (mut odd, clock) = Odd::new(FakeConfig::default());
        odd.set_rates_error = Some(BackendError::Misuse("码率是 0"));
        let report = odd.check(&clock);
        assert!(report
            .measured
            .errors
            .iter()
            .any(|e| e.contains("意外的用法错：码率是 0")));
        assert!(report.is_clean());
        // 交帧时回“不接受”：用错了信号，记下来
        let (mut odd, clock) = Odd::new(FakeConfig::default());
        odd.submit_rejects_once = true;
        let report = odd.check(&clock);
        assert!(report
            .measured
            .errors
            .iter()
            .any(|e| e.contains("不该在这里回“不接受”：交帧时不该回这个")));
    }

    #[test]
    fn an_error_from_flush_is_recorded_and_recovered_from() {
        let (mut odd, clock) = Odd::new(FakeConfig::default());
        odd.flush_fails = true;
        let report = odd.check(&clock);
        assert!(report
            .measured
            .errors
            .iter()
            .any(|e| e.contains("flush 时出错")));
        assert!(report.is_clean());
    }

    #[test]
    fn a_backend_that_only_takes_nv12_passes() {
        let report = check(FakeConfig {
            inputs: Inputs {
                i420: false,
                nv12: true,
                gpu: false,
                described: false,
            },
            ..FakeConfig::default()
        });
        assert_clean(&report);
        assert!(report.measured.frames_encoded > 100);
    }

    #[test]
    fn unexpected_misuse_from_poll_is_reported_not_hidden() {
        let (mut odd, clock) = Odd::new(FakeConfig::default());
        odd.poll_misuse = true;
        let report = odd.check(&clock);
        assert!(report
            .measured
            .errors
            .iter()
            .any(|e| e.contains("意外的用法错")));
        assert!(report.is_clean(), "用法错只记录，不算违约");
    }

    #[test]
    fn failing_to_come_back_after_an_error_is_caught() {
        // 出错之后重新配置失败：后面每一步都没法继续，但每条契约仍有结论
        let (mut odd, clock) = Odd::new(stalls());
        odd.fail_reconfig_from = Some(2);
        let report = odd.check(&clock);
        assert!(evidence(&report, 9).contains("出错复位之后重新配置失败"));
        assert!(evidence(&report, 6).contains("失败"));
        assert_eq!(report.contracts.len(), 9);
        // 复位之后第一帧又出错
        let (mut odd, clock) = Odd::new(stalls());
        odd.fail_after_reconfig = true;
        let report = odd.check(&clock);
        assert!(evidence(&report, 9).contains("复位、重新配置之后的第一帧又出错"));
    }

    #[test]
    fn a_failing_reconfigure_in_a_later_step_ends_that_step_only() {
        // 第 3 次配置（换尺寸那一步的第一次）开始都失败
        for from in [2, 3, 4, 6, 8, 9, 10] {
            let (mut odd, clock) = Odd::new(breach(Breaches {
                resize_silently: true,
                ..Breaches::default()
            }));
            odd.fail_reconfig_from = Some(from);
            let report = odd.check(&clock);
            assert_eq!(report.contracts.len(), 9, "从第 {from} 次配置起失败");
            assert!(
                report.breached(6),
                "从第 {from} 次配置起失败：{:?}",
                report.render()
            );
        }
    }

    #[test]
    fn a_backend_that_reports_a_wrong_size_is_caught() {
        let (mut odd, clock) = Odd::new(FakeConfig::default());
        odd.size_lie = Some((2, 2));
        let report = odd.check(&clock);
        assert!(evidence(&report, 6).contains("reconfigure 成功，size() 却是 Some((2, 2))"));
        assert!(evidence(&report, 5).contains("改码率之后 size() 是 Some((2, 2))"));
    }

    #[test]
    fn errors_on_every_frame_do_not_stop_the_checkup() {
        // 隔 1 微秒再交帧就出错：每一帧都错，体检仍然给出每条契约的结论
        let report = check(breach(Breaches {
            error_after_idle_us: Some(1),
            ..Breaches::default()
        }));
        assert_eq!(report.contracts.len(), 9);
        assert!(report.measured.errors.len() > 10);
        assert_eq!(report.contract(9).unwrap().verdict, Verdict::Holds);
    }

    #[test]
    fn report_text_lists_every_verdict_and_the_evidence() {
        let report = check(breach(Breaches {
            zero_timing: true,
            ..Breaches::default()
        }));
        let text = report.render().join("\n");
        assert!(text.contains("体检：有问题"));
        assert!(text.contains("契约 8 时间分开报：违约\n    证据："));
        assert_eq!(report.breaches(), vec![8]);
        assert!(text.contains("改码率都接受"));
        let none = Report {
            measured: Measured::default(),
            ..report
        };
        let text = none.render().join("\n");
        assert!(text.contains("实测 没测到") || text.contains("关键帧声明 1 帧内给、实测 没测到"));
        assert!(text.contains("改码率没测") && text.contains("超发 不适用"));
    }

    #[test]
    fn synthetic_pictures_are_deterministic_and_behave_like_a_desktop() {
        let a = Pic::new((192, 128), 0, 3);
        let b = Pic::new((192, 128), 0, 3);
        assert_eq!((a.y.clone(), a.nv12.clone()), (b.y.clone(), b.nv12.clone()));
        assert_eq!(a.nv12.len(), 192 * 128 * 3 / 2);
        let differ = |x: &[u8], y: &[u8]| x.iter().zip(y).filter(|(p, q)| p != q).count();
        // 打字只改一小块；换主题几乎整屏都变
        let typed = Pic::new((192, 128), 0, 4);
        assert!(differ(&a.y, &typed.y) < 600);
        let themed = Pic::new((192, 128), 1, 0);
        assert!(differ(&a.y, &themed.y) > 192 * 128 * 9 / 10);
        assert!(matches!(a.input(Form::I420), FrameInput::I420 { .. }));
        assert!(matches!(a.input(Form::Nv12), FrameInput::Nv12(_)));
    }

    /// 给配置在链式写法里改一处。
    trait Tap: Sized {
        fn tap(self, f: impl FnOnce(&mut Self)) -> Self;
    }
    impl Tap for FakeConfig {
        fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
            f(&mut self);
            self
        }
    }
}
