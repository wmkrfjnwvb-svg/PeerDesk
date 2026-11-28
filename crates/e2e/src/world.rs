//! 世界文件：一次仿真会话的**全部输入**，文本，一行一个键值（读写的通用部分在 [`peerdesk_core::kv`]），
//! 按内容哈希命名（`<16 位十六进制>.world`）。见 `docs/research/Q-redesign/a1-plan.md` 第 2 节和“S4 的结果”。
//!
//! 规矩：
//! - **结果只由文件内容决定**。[`Scenario::new`] 里从环境变量取默认值的几项（编码种类、节奏器、纠错、起步探测）在文件里都写明；
//!   读出来的场景用 [`Scenario::fixed`] 打底、每一项都由文件填，不读任何环境变量。`trace`（调试输出）不是输入，不写，读出来是关。
//!   仅剩的例外是节奏器的倍数 `PEERDESK_PACE_FACTOR`（产品代码里的全局变量，节奏器开着时才起作用）：[`env_conflict`] 发现
//!   它被改过就拒绝运行，不静默用错。
//! - **读**：不认识的键、重复的键、缺的键、值不对都报清楚的中文错误，不用默认值顶替。
//! - **写**：键的顺序固定，同一个 [`Scenario`] 写出来逐字节相同；读回来再写，逐字节相同。
//! - **网络内联**：微软轨迹有几十段，每段一行写在文件里，不引用外部文件。引用要多一套“文件找不到、改过了没有、相对路径”的错误，
//!   世界文件要能单独拷走、按哈希核对就是自包含；几十行的体积（约 10 KB）不是问题。
//! - **随机数布局**：`rng_layout` 写在文件里（[`RNG_LAYOUTS`]），读到别的号就报错，不会用错的布局跑。
//!   - 布局 1（`a1-plan.md` 第 2 节）：老的 `Net`，网络是 `links N` 加每段一行 `link.<i>`，两个方向对称；
//!   - 布局 2（`a2-plan.md` 第 6、8 节）：分层的 `Path`，两个方向各一组层参数，加按时刻换参数、换路、交叉流量，
//!     键见 [`layout2`] 模块。两种布局的网络键互斥：布局 2 的键出现在布局 1 的文件里（或反过来）都报错。
//!   - 布局 1 的文件写法和以前逐字节相同，哈希不变。

mod layout2;

use crate::content::Action;
use crate::{
    ByteModelKind, ByteNoise, CodecKind, EstimatorKind, NetworkPlan, Outcome, Phase, Scenario,
};
use peerdesk_core::adaptive::Profile;
use peerdesk_core::error::Error;
use peerdesk_core::kv::{content_hash, join_fields, Kv, KvError, KvWriter};
use peerdesk_core::policy::ContentPolicy;
use peerdesk_core::sim::{Busy, Doze, Link, Spikes};
use peerdesk_core::transport::Fec;
use std::fmt;
use std::path::{Path, PathBuf};

/// 文件格式的版本。
pub const FORMAT_VERSION: u32 = 1;
/// 布局 1 的随机数布局号：现有场景的随机数取用顺序（所有网络毛病共用一条流，合成桌面另有一条）。
pub const RNG_LAYOUT: u32 = 1;
/// 认识的随机数布局号：1 是老的 `Net`，2 是每层一条随机数流的 `Path`（A2）。
pub const RNG_LAYOUTS: [u32; 2] = [1, 2];
/// 世界文件的扩展名。
pub const EXTENSION: &str = "world";

/// 读写、运行世界文件出的错。
#[derive(Debug)]
pub enum WorldError {
    /// 文件内容不对，或场景写不成文件（消息是中文，说明哪里不对）。
    Format(String),
    /// 世界读对了，仿真跑不下去。
    Run(Error),
    /// 读写文件失败。
    Io(String),
}

impl fmt::Display for WorldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorldError::Format(m) | WorldError::Io(m) => f.write_str(m),
            WorldError::Run(e) => write!(f, "仿真出错：{e}"),
        }
    }
}

impl std::error::Error for WorldError {}

impl From<KvError> for WorldError {
    fn from(e: KvError) -> Self {
        WorldError::Format(e.0)
    }
}

fn bad<T>(msg: String) -> Result<T, WorldError> {
    Err(WorldError::Format(msg))
}

const CODECS: [(&str, CodecKind); 4] = [
    ("tile", CodecKind::Tile),
    ("h264", CodecKind::H264),
    ("h264-l05", CodecKind::H264L05),
    ("h264-l0", CodecKind::H264L0),
];
const PROFILES: [(&str, Profile); 4] = [
    ("stable", Profile::STABLE),
    ("wireless", Profile::WIRELESS),
    ("cellular", Profile::CELLULAR),
    ("long-haul", Profile::LONG_HAUL),
];
const ESTIMATORS: [(&str, EstimatorKind); 3] = [
    ("legacy", EstimatorKind::Legacy),
    ("v1", EstimatorKind::ClassicV1),
    ("gcc", EstimatorKind::GccV1),
];
const POLICIES: [(&str, ContentPolicy); 1] = [("office", ContentPolicy::OFFICE)];
const FECS: [(&str, Fec); 2] = [("rs", Fec::Rs), ("xor", Fec::Xor)];

fn name_of<'a, T: Copy + PartialEq>(table: &'a [(&'a str, T)], v: T) -> Option<&'a str> {
    table.iter().find(|(_, x)| *x == v).map(|(n, _)| *n)
}

/// 运行环境会不会让世界文件的结果不一样。现在只有一项：节奏器开着、而环境变量改了倍数。
/// 有就返回中文原因（运行前先查，不静默用错）。
#[must_use]
pub fn env_conflict(s: &Scenario) -> Option<String> {
    let factor = peerdesk_core::pacer::factor_permille();
    (s.pacing && factor != peerdesk_core::pacer::PACE_FACTOR_PERMILLE).then(|| {
        format!(
            "环境变量 PEERDESK_PACE_FACTOR 把节奏器倍数改成了 {factor}（默认 {}），世界文件的结果会和别处跑的不一样；请去掉这个环境变量再跑",
            peerdesk_core::pacer::PACE_FACTOR_PERMILLE
        )
    })
}

fn opt_fields(v: &[u64]) -> String {
    v.iter().map(u64::to_string).collect::<Vec<_>>().join(":")
}

fn parse_opt_fields(
    kv: &mut Kv,
    key: &str,
    n: usize,
    ctx: &str,
) -> Result<Option<Vec<u64>>, WorldError> {
    let text = kv.take(key)?;
    if text == "-" {
        return Ok(None);
    }
    let nums: Result<Vec<u64>, _> = text.split(':').map(str::parse::<u64>).collect();
    match nums {
        Ok(v) if v.len() == n => Ok(Some(v)),
        _ => bad(format!(
            "{ctx}：{key} 的值 {text:?} 不对，应当是 - 或 {n} 个用冒号隔开的非负整数"
        )),
    }
}

fn link_fields(at: u64, l: &Link) -> Result<String, WorldError> {
    // 这里解构 Link：以后 Link 加了字段，这里编译不过，逼着把它写进文件
    let Link {
        delay_us,
        jitter_us,
        fifo_jitter,
        spikes,
        doze,
        busy,
        batch_us,
        loss_permille,
        rate_bps,
        max_queue_us,
        up,
    } = l;
    let flag = |b: bool| if b { "1" } else { "0" }.to_owned();
    Ok(join_fields(&[
        ("at", at.to_string()),
        ("delay_us", delay_us.to_string()),
        ("jitter_us", jitter_us.to_string()),
        ("fifo_jitter", flag(*fifo_jitter)),
        ("batch_us", batch_us.to_string()),
        ("loss_permille", loss_permille.to_string()),
        (
            "rate_bps",
            rate_bps.map_or("-".to_owned(), |r| r.to_string()),
        ),
        ("max_queue_us", max_queue_us.to_string()),
        ("up", flag(*up)),
        (
            "spikes",
            spikes.map_or("-".to_owned(), |s| {
                opt_fields(&[s.every_us, s.length_us, s.max_us])
            }),
        ),
        (
            "doze",
            doze.map_or("-".to_owned(), |d| opt_fields(&[d.idle_us, d.wake_us])),
        ),
        (
            "busy",
            busy.map_or("-".to_owned(), |b| opt_fields(&[b.every_us, b.length_us])),
        ),
    ])?)
}

fn parse_link(text: &str, ctx: &str) -> Result<(u64, Link), WorldError> {
    let mut f = Kv::parse_fields(text, ctx)?;
    let at = f.take_u64("at")?;
    let delay_us = f.take_u64("delay_us")?;
    let jitter_us = f.take_u64("jitter_us")?;
    let fifo_jitter = f.take_bool("fifo_jitter")?;
    let batch_us = f.take_u64("batch_us")?;
    let loss_permille = f.take_u16("loss_permille")?;
    let rate_bps = f.take_opt_u64("rate_bps")?;
    let max_queue_us = f.take_u64("max_queue_us")?;
    let up = f.take_bool("up")?;
    let spikes = parse_opt_fields(&mut f, "spikes", 3, ctx)?.map(|v| {
        let mut it = v.into_iter();
        Spikes {
            every_us: it.next().unwrap_or(0),
            length_us: it.next().unwrap_or(0),
            max_us: it.next().unwrap_or(0),
        }
    });
    let doze = parse_opt_fields(&mut f, "doze", 2, ctx)?.map(|v| {
        let mut it = v.into_iter();
        Doze {
            idle_us: it.next().unwrap_or(0),
            wake_us: it.next().unwrap_or(0),
        }
    });
    let busy = parse_opt_fields(&mut f, "busy", 2, ctx)?.map(|v| {
        let mut it = v.into_iter();
        Busy {
            every_us: it.next().unwrap_or(0),
            length_us: it.next().unwrap_or(0),
        }
    });
    f.finish()?;
    Ok((
        at,
        Link {
            delay_us,
            jitter_us,
            fifo_jitter,
            spikes,
            doze,
            busy,
            batch_us,
            loss_permille,
            rate_bps,
            max_queue_us,
            up,
        },
    ))
}

/// 场景 → 世界文件的文本。同一个场景永远写出同样的字节。
///
/// # Errors
/// 场景里有写不进文件的东西：名字有换行或两端有空白、画像或内容策略不是命名的那几套、字节模型配了不用它的编码。
pub fn to_text(s: &Scenario) -> Result<String, WorldError> {
    // 解构 Scenario：以后加了字段，这里编译不过，逼着决定写不写进文件
    let Scenario {
        name,
        width,
        height,
        fps,
        codec,
        phases,
        network,
        profile,
        start_bps,
        max_bps,
        seed,
        estimator,
        pacing,
        fec,
        trace: _,
        start_probe,
        policy,
        probe_ids,
        byte_model,
        byte_noise,
    } = s;
    check_byte_model(*codec, *byte_model)?;
    check_byte_noise(*byte_model, *byte_noise)?;
    let named = |what: &str, found: Option<&str>| {
        found
            .map(str::to_owned)
            .ok_or_else(|| WorldError::Format(format!("{what}不是命名的那几套，世界文件写不出来")))
    };
    let mut w = KvWriter::new();
    w.comment("PeerDesk 世界文件：一次仿真会话的全部输入（格式见 crates/e2e/src/world.rs）。")?;
    w.comment("文件名是内容哈希；结果只由这里的内容决定，不受环境变量影响。")?;
    w.put_display("world", FORMAT_VERSION)?;
    w.put("name", name)?;
    w.put_display("width", width)?;
    w.put_display("height", height)?;
    w.put_display("fps", fps)?;
    w.put(
        "codec",
        named("编码种类", name_of(&CODECS, *codec))?.as_str(),
    )?;
    w.put("byte_model", byte_model.name())?;
    // 扰动是后来加的键：只在不是默认时写。这样没有它的老世界文件照样读得进来，默认的场景写出来和以前逐字节相同（哈希不变）
    if *byte_noise != ByteNoise::Default {
        w.put("byte_noise", byte_noise.name())?;
    }
    w.put_display("rng_layout", network.layout())?;
    w.put_display("seed", seed)?;
    w.put(
        "estimator",
        named("估计器", name_of(&ESTIMATORS, *estimator))?.as_str(),
    )?;
    w.put(
        "profile",
        named("网络画像", name_of(&PROFILES, *profile))?.as_str(),
    )?;
    w.put(
        "policy",
        named("内容策略", name_of(&POLICIES, *policy))?.as_str(),
    )?;
    w.put_display("start_bps", start_bps)?;
    w.put_display("max_bps", max_bps)?;
    w.put_bool("pacing", *pacing)?;
    w.put("fec", named("纠错方式", name_of(&FECS, *fec))?.as_str())?;
    w.put_bool("start_probe", *start_probe)?;
    w.put_bool("probe_ids", *probe_ids)?;
    w.put_display("phases", phases.len())?;
    for (i, p) in phases.iter().enumerate() {
        let fields = join_fields(&[
            ("action", p.action.name().to_owned()),
            ("frames", p.frames.to_string()),
        ])?;
        w.put(&format!("phase.{i}"), &fields)?;
    }
    match network {
        NetworkPlan::Layout1(links) => {
            w.put_display("links", links.len())?;
            for (i, (at, link)) in links.iter().enumerate() {
                w.put(&format!("link.{i}"), &link_fields(*at, link)?)?;
            }
        }
        NetworkPlan::Layout2(plan) => layout2::write(&mut w, plan)?,
        NetworkPlan::Segmented(plan) => layout2::write_segmented(&mut w, plan)?,
    }
    Ok(w.finish())
}

/// 占位的字节模型没有扰动：配了别的写法就拒绝（否则同样的会话有两个哈希）。
fn check_byte_noise(model: ByteModelKind, noise: ByteNoise) -> Result<(), WorldError> {
    if model == ByteModelKind::Default && noise != ByteNoise::Default {
        return bad(format!(
            "字节模型 default（占位）没有扰动，byte_noise 只能不写或写 default，不能写 {}",
            noise.name()
        ));
    }
    Ok(())
}

fn check_byte_model(codec: CodecKind, model: ByteModelKind) -> Result<(), WorldError> {
    let fake = matches!(codec, CodecKind::H264L05 | CodecKind::H264L0);
    if !fake && model != ByteModelKind::Default {
        return bad(format!(
            "字节模型 {} 只对 h264-l05、h264-l0 有意义，编码种类 {} 要写 default（不同的写法会让同样的会话有两个哈希）",
            model.name(),
            codec.name()
        ));
    }
    Ok(())
}

/// 世界文件的文本 → 场景。
///
/// # Errors
/// 文件内容不对：不认识的键、重复的键、缺的键、值不对、随机数布局或格式版本不认识、帧率为 0、没有阶段。
pub fn from_text(text: &str) -> Result<Scenario, WorldError> {
    let mut kv = Kv::parse_text(text)?;
    let version = kv.take_u32("world")?;
    if version != FORMAT_VERSION {
        return bad(format!(
            "世界文件的格式版本是 {version}，这个程序只认 {FORMAT_VERSION}"
        ));
    }
    let name = kv.take("name")?;
    let width = kv.take_u16("width")?;
    let height = kv.take_u16("height")?;
    let fps = kv.take_u32("fps")?;
    let codec = kv.take_choice("codec", &CODECS)?;
    let byte_model = kv.take_choice("byte_model", &ByteModelKind::ALL)?;
    let byte_noise = if kv.has("byte_noise") {
        kv.take_choice("byte_noise", &ByteNoise::ALL)?
    } else {
        ByteNoise::Default
    };
    let rng_layout = kv.take_u32("rng_layout")?;
    if !RNG_LAYOUTS.contains(&rng_layout) {
        return bad(format!(
            "随机数布局是 {rng_layout}，这个程序只会 {RNG_LAYOUTS:?}（用别的布局跑，结果就不是文件描述的那个世界）"
        ));
    }
    let seed = kv.take_u64("seed")?;
    let estimator = kv.take_choice("estimator", &ESTIMATORS)?;
    let profile = kv.take_choice("profile", &PROFILES)?;
    let policy = kv.take_choice("policy", &POLICIES)?;
    let start_bps = kv.take_u64("start_bps")?;
    let max_bps = kv.take_u64("max_bps")?;
    let pacing = kv.take_bool("pacing")?;
    let fec = kv.take_choice("fec", &FECS)?;
    let start_probe = kv.take_bool("start_probe")?;
    let probe_ids = kv.take_bool("probe_ids")?;
    if width == 0 || height == 0 {
        return bad("width、height 不能为 0".to_owned());
    }
    if fps == 0 {
        return bad("fps 不能为 0".to_owned());
    }
    check_byte_model(codec, byte_model)?;
    check_byte_noise(byte_model, byte_noise)?;

    let count = kv.take_u32("phases")?;
    if count == 0 {
        return bad("phases 不能为 0：会话至少要有一个阶段".to_owned());
    }
    let mut phases = Vec::new();
    for i in 0..count {
        let key = format!("phase.{i}");
        let ctx = key.clone();
        let mut f = Kv::parse_fields(&kv.take(&key)?, &ctx)?;
        let action = f.take_choice("action", &Action::ALL)?;
        let frames = f.take_u32("frames")?;
        f.finish()?;
        phases.push(Phase { action, frames });
    }
    let network = if rng_layout == RNG_LAYOUT {
        if let Some(key) = layout2::KEYS.into_iter().find(|k| kv.has(k)) {
            return bad(format!(
                "{key} 是随机数布局 2 的键，这个文件写的是 rng_layout 1（两种布局的网络不能混写）"
            ));
        }
        let count = kv.take_u32("links")?;
        let mut links = Vec::new();
        for i in 0..count {
            let key = format!("link.{i}");
            let text = kv.take(&key)?;
            links.push(parse_link(&text, &key)?);
        }
        NetworkPlan::Layout1(links)
    } else {
        if kv.has("links") {
            return bad(
                "links 是随机数布局 1 的键，这个文件写的是 rng_layout 2（两种布局的网络不能混写）"
                    .to_owned(),
            );
        }
        if kv.has(layout2::SEGMENTED_KEY) {
            if kv.has("path.fwd") {
                return bad(
                    "path.fwd 和 seg.a-lan.fwd 不能同时出现：一个文件要么是单段的路径，要么是五段的分段路径"
                        .to_owned(),
                );
            }
            NetworkPlan::Segmented(Box::new(layout2::read_segmented(&mut kv)?))
        } else {
            NetworkPlan::Layout2(Box::new(layout2::read(&mut kv)?))
        }
    };
    kv.finish()?;

    let mut s = Scenario::fixed(&name, phases);
    s.width = width;
    s.height = height;
    s.fps = fps;
    s.codec = codec;
    s.network = network;
    s.profile = profile;
    s.start_bps = start_bps;
    s.max_bps = max_bps;
    s.seed = seed;
    s.estimator = estimator;
    s.pacing = pacing;
    s.fec = fec;
    s.start_probe = start_probe;
    s.policy = policy;
    s.probe_ids = probe_ids;
    s.byte_model = byte_model;
    s.byte_noise = byte_noise;
    Ok(s)
}

/// 编码种类的名字 → 值（`tile`、`h264`、`h264-l05`、`h264-l0`）。
#[must_use]
pub fn codec_named(name: &str) -> Option<CodecKind> {
    CODECS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// 会话的身份：不管用哪一档仿真（L1、L0.5、L0）、哪个字节模型、哪种扰动，同一份画面、网络、算法配置得到同一个键。
/// 三档对账靠它把同一个会话的几次运行对上。算法：把这三项换成固定值（`h264`、`default`、默认扰动）之后的世界文件的哈希。
/// 块编码不参与对账，保持原样（键和它自己的世界哈希相同）。
///
/// # Errors
/// 同 [`to_text`]。
pub fn session_key(s: &Scenario) -> Result<String, WorldError> {
    let mut base = s.clone();
    if base.codec.is_h264() {
        base.codec = CodecKind::H264;
    }
    base.byte_model = ByteModelKind::Default;
    base.byte_noise = ByteNoise::Default;
    Ok(hash(&to_text(&base)?))
}

/// 世界文件文本的哈希（16 位十六进制）。
#[must_use]
pub fn hash(text: &str) -> String {
    content_hash(text.as_bytes())
}

/// 世界文件的文件名：`<哈希>.world`。
#[must_use]
pub fn file_name(text: &str) -> String {
    format!("{}.{EXTENSION}", hash(text))
}

/// 把场景的世界文件写进 `dir`（目录不存在就建），文件名是内容哈希。已有同名文件（内容必然相同）就不再写。
/// 先写临时文件再改名：并行的测试写同一个场景、批量运行器同时在读，都不会看到写了一半的文件。
///
/// # Errors
/// 场景写不成文件，或读写磁盘失败。
pub fn export(dir: &Path, s: &Scenario) -> Result<PathBuf, WorldError> {
    let text = to_text(s)?;
    let io = |what: &str, e: std::io::Error| WorldError::Io(format!("{what}：{e}"));
    std::fs::create_dir_all(dir).map_err(|e| io(&format!("建目录 {} 失败", dir.display()), e))?;
    let path = dir.join(file_name(&text));
    if path.exists() {
        return Ok(path);
    }
    let tmp = dir.join(format!(
        ".{}.{}.{:?}.tmp",
        hash(&text),
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::write(&tmp, text.as_bytes())
        .map_err(|e| io(&format!("写 {} 失败", tmp.display()), e))?;
    std::fs::rename(&tmp, &path).map_err(|e| io(&format!("改名 {} 失败", path.display()), e))?;
    Ok(path)
}

/// 一个读进来的世界文件。
#[derive(Debug, Clone)]
pub struct World {
    /// 文件内容的哈希（16 位十六进制）。
    pub hash: String,
    /// 场景。
    pub scenario: Scenario,
}

impl World {
    /// 读一个世界文件。如果文件名是 `<16 位十六进制>.world`，核对它和内容的哈希一致（对不上说明文件被改过或拷坏了）；
    /// 别的名字（手写的世界文件）不核对。
    ///
    /// # Errors
    /// 读不出文件、内容不对、文件名上的哈希和内容对不上。
    pub fn load(path: &Path) -> Result<Self, WorldError> {
        let shown = path.display();
        let bytes =
            std::fs::read(path).map_err(|e| WorldError::Io(format!("{shown}：读不出来：{e}")))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| WorldError::Format(format!("{shown}：不是 UTF-8 文本")))?;
        let hash = hash(&text);
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            let named_by_hash = path.extension().is_some_and(|e| e == EXTENSION)
                && stem.len() == hash.len()
                && stem.bytes().all(|b| b.is_ascii_hexdigit());
            if named_by_hash && stem != hash {
                return bad(format!(
                    "{shown}：文件名里的哈希 {stem} 和内容的哈希 {hash} 对不上（文件被改过或拷坏了）"
                ));
            }
        }
        let scenario = from_text(&text).map_err(|e| WorldError::Format(format!("{shown}：{e}")))?;
        Ok(Self { hash, scenario })
    }

    /// 同一个会话换一档仿真：编码种类、字节模型、扰动换成给定的（`None` 的不换），哈希按新内容重算。
    /// 对账时用一批 L1 的世界文件跑出 L0.5、L0 各种配置，不用另外导出。
    ///
    /// # Errors
    /// 换完之后的组合不合法（真编码器配了字节模型、占位模型配了扰动）。
    pub fn retiered(
        &self,
        codec: Option<CodecKind>,
        byte_model: Option<ByteModelKind>,
        byte_noise: Option<ByteNoise>,
    ) -> Result<Self, WorldError> {
        let mut scenario = self.scenario.clone();
        scenario.codec = codec.unwrap_or(scenario.codec);
        scenario.byte_model = byte_model.unwrap_or(scenario.byte_model);
        scenario.byte_noise = byte_noise.unwrap_or(scenario.byte_noise);
        Ok(Self {
            hash: hash(&to_text(&scenario)?),
            scenario,
        })
    }

    /// 布局 1 的世界换成布局 2（`NetworkPlan::to_layout2`，A2-3 对照用），哈希按新的内容重算；已经是布局 2 的不变。
    ///
    /// # Errors
    /// 换了之后写不成文件（同 [`to_text`]）。
    pub fn with_layout2(&self, loss_first: bool) -> Result<Self, WorldError> {
        let mut scenario = self.scenario.clone();
        scenario.network = scenario.network.to_layout2(loss_first);
        Ok(Self {
            hash: hash(&to_text(&scenario)?),
            scenario,
        })
    }

    /// 跑这个世界。
    ///
    /// # Errors
    /// 环境会让结果不一样（见 [`env_conflict`]），或仿真出错。
    pub fn run(&self) -> Result<Outcome, WorldError> {
        if let Some(why) = env_conflict(&self.scenario) {
            return bad(why);
        }
        crate::run(&self.scenario).map_err(WorldError::Run)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use peerdesk_core::sim::Link;

    fn phases() -> Vec<Phase> {
        vec![
            Phase {
                action: Action::Idle,
                frames: 5,
            },
            Phase {
                action: Action::Typing,
                frames: 10,
            },
            Phase {
                action: Action::Pointer,
                frames: 3,
            },
        ]
    }

    /// 带多段网络、全部可选字段都有的场景。
    fn busy_scenario() -> Scenario {
        let mut s = Scenario::fixed("多段 网络", phases());
        s.width = 640;
        s.height = 360;
        s.fps = 60;
        s.seed = 99;
        s.profile = Profile::WIRELESS;
        s.estimator = EstimatorKind::GccV1;
        s.fec = Fec::Xor;
        s.pacing = true;
        s.start_probe = false;
        s.probe_ids = false;
        s.start_bps = 2_000_000;
        s.max_bps = 50_000_000;
        s.network = NetworkPlan::Layout1(vec![
            (
                0,
                Link {
                    delay_us: 12_000,
                    jitter_us: 2_000,
                    fifo_jitter: true,
                    spikes: Some(Spikes {
                        every_us: 1_000_000,
                        length_us: 50_000,
                        max_us: 20_000,
                    }),
                    doze: Some(Doze {
                        idle_us: 100_000,
                        wake_us: 15_000,
                    }),
                    busy: Some(Busy {
                        every_us: 2_000_000,
                        length_us: 10_000,
                    }),
                    batch_us: 4_000,
                    loss_permille: 12,
                    rate_bps: Some(30_000_000),
                    max_queue_us: 150_000,
                    up: true,
                },
            ),
            (
                2_500_000,
                Link {
                    up: false,
                    rate_bps: None,
                    ..Link::lan()
                },
            ),
            (3_000_000, Link::cellular()),
        ]);
        s
    }

    fn ms_trace_scenario() -> Scenario {
        let csv = include_str!("../traces/ms-02560.csv");
        let mut s = Scenario::fixed("ms-02560", phases());
        s.network = NetworkPlan::Layout1(
            csv.lines()
                .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
                .map(|l| {
                    let f: Vec<u64> = l.split(',').map(|x| x.trim().parse().unwrap()).collect();
                    let link = Link {
                        delay_us: 25_000,
                        jitter_us: 3_000,
                        loss_permille: u16::try_from(f[2]).unwrap(),
                        rate_bps: Some(f[1]),
                        max_queue_us: 200_000,
                        ..Link::lan()
                    };
                    (f[0] * 1000, link)
                })
                .collect(),
        );
        s
    }

    fn l0_scenario() -> Scenario {
        let mut s = Scenario::fixed("l0", phases());
        s.codec = CodecKind::H264L0;
        s.byte_model = ByteModelKind::Openh264V1;
        s
    }

    fn assert_round_trips(s: &Scenario) -> String {
        let text = to_text(s).unwrap();
        let back = from_text(&text).unwrap();
        assert_eq!(to_text(&back).unwrap(), text, "再写出来逐字节相同");
        assert_eq!(to_text(s).unwrap(), text, "同一个场景写两次相同");
        // 每一项都还原
        assert_eq!(back.name, s.name);
        assert_eq!(
            (back.width, back.height, back.fps),
            (s.width, s.height, s.fps)
        );
        assert_eq!(back.codec, s.codec);
        assert_eq!(back.network, s.network);
        assert_eq!(back.profile, s.profile);
        assert_eq!(
            (back.start_bps, back.max_bps, back.seed),
            (s.start_bps, s.max_bps, s.seed)
        );
        assert_eq!(back.estimator, s.estimator);
        assert_eq!((back.pacing, back.fec), (s.pacing, s.fec));
        assert_eq!(
            (back.start_probe, back.probe_ids),
            (s.start_probe, s.probe_ids)
        );
        assert_eq!(back.policy, s.policy);
        assert_eq!(back.byte_model, s.byte_model);
        assert_eq!(back.phases, s.phases);
        assert!(!back.trace);
        text
    }

    #[test]
    fn round_trips_are_byte_identical() {
        let default = assert_round_trips(&Scenario::fixed("default", phases()));
        assert!(default.contains("\nbyte_model default\n") && default.contains("\nrng_layout 1\n"));
        let multi = assert_round_trips(&busy_scenario());
        assert!(multi.contains("\nlinks 3\n") && multi.contains("spikes=1000000:50000:20000"));
        let ms = assert_round_trips(&ms_trace_scenario());
        assert!(ms.lines().filter(|l| l.starts_with("link.")).count() > 10);
        let l0 = assert_round_trips(&l0_scenario());
        assert!(l0.contains("\ncodec h264-l0\n") && l0.contains("\nbyte_model openh264-v1\n"));
        // 全部编码种类、估计器、画像都能来回
        for (_, codec) in CODECS {
            let mut s = Scenario::fixed("c", phases());
            s.codec = codec;
            assert_round_trips(&s);
        }
        for (_, e) in ESTIMATORS {
            let mut s = Scenario::fixed("e", phases());
            s.estimator = e;
            assert_round_trips(&s);
        }
        for (_, p) in PROFILES {
            let mut s = Scenario::fixed("p", phases());
            s.profile = p;
            assert_round_trips(&s);
        }
        for (_, a) in Action::ALL {
            let mut s = Scenario::fixed(
                "a",
                vec![Phase {
                    action: a,
                    frames: 1,
                }],
            );
            s.network = NetworkPlan::Layout1(Vec::new());
            assert_round_trips(&s);
        }
    }

    #[test]
    fn different_inputs_get_different_hashes() {
        let a = to_text(&Scenario::fixed("x", phases())).unwrap();
        let mut s = Scenario::fixed("x", phases());
        s.seed = 2;
        let b = to_text(&s).unwrap();
        assert_ne!(hash(&a), hash(&b));
        assert_eq!(file_name(&a), format!("{}.world", hash(&a)));
        assert_eq!(hash(&a).len(), 16);
    }

    #[test]
    fn the_written_text_does_not_depend_on_the_environment_defaults() {
        // fixed 不读环境变量：和 new 在“没设环境变量”时的默认值一致（测试环境不设它们）
        let n = Scenario::new("x", phases());
        let f = Scenario::fixed("x", phases());
        if std::env::var_os("PEERDESK_E2E_CODEC").is_none()
            && std::env::var_os("PEERDESK_PACE").is_none()
            && std::env::var_os("PEERDESK_FEC").is_none()
            && std::env::var_os("PEERDESK_START_PROBE").is_none()
        {
            assert_eq!(to_text(&n).unwrap(), to_text(&f).unwrap());
        }
        assert_eq!(f.codec, CodecKind::H264);
        assert!(!f.pacing && f.start_probe && !f.trace);
    }

    fn good() -> String {
        to_text(&busy_scenario()).unwrap()
    }

    fn err_of(text: &str) -> String {
        from_text(text).unwrap_err().to_string()
    }

    #[test]
    fn reading_rejects_unknown_duplicate_missing_and_bad_values() {
        let g = good();
        // 不认识的键
        let e = err_of(&format!("{g}colour red\n"));
        assert!(e.contains("不认识的键：colour"), "{e}");
        // 重复的键
        let e = err_of(&format!("{g}seed 5\n"));
        assert!(e.contains("键 seed 重复"), "{e}");
        // 缺的键：逐个删掉每个必须有的键
        for key in [
            "world",
            "name",
            "width",
            "height",
            "fps",
            "codec",
            "byte_model",
            "rng_layout",
            "seed",
            "estimator",
            "profile",
            "policy",
            "start_bps",
            "max_bps",
            "pacing",
            "fec",
            "start_probe",
            "probe_ids",
            "phases",
            "links",
            "phase.1",
            "link.2",
        ] {
            let cut: String = g
                .lines()
                .filter(|l| l.split(' ').next() != Some(key))
                .map(|l| format!("{l}\n"))
                .collect();
            let e = err_of(&cut);
            assert!(e.contains(key), "{key}: {e}");
        }
        // 值不对
        for (from, to, want) in [
            ("\nwidth 640\n", "\nwidth 99999\n", "width"),
            ("\nfps 60\n", "\nfps x\n", "fps"),
            (
                "\ncodec h264\n",
                "\ncodec vp9\n",
                "可选：tile、h264、h264-l05、h264-l0",
            ),
            (
                "\nbyte_model default\n",
                "\nbyte_model other\n",
                "可选：default、openh264-v1",
            ),
            ("\nestimator gcc\n", "\nestimator x\n", "estimator"),
            ("\nprofile wireless\n", "\nprofile x\n", "profile"),
            ("\npolicy office\n", "\npolicy x\n", "policy"),
            ("\nfec xor\n", "\nfec x\n", "fec"),
            ("\npacing 1\n", "\npacing yes\n", "应当是 0 或 1"),
            (
                "\nphase.1 action=typing frames=10\n",
                "\nphase.1 action=dance frames=10\n",
                "可选：idle、typing",
            ),
            (
                "\nphase.1 action=typing frames=10\n",
                "\nphase.1 action=typing frames=-1\n",
                "frames",
            ),
            (
                "\nphase.1 action=typing frames=10\n",
                "\nphase.1 action=typing\n",
                "缺少字段 frames",
            ),
            (
                "\nphase.1 action=typing frames=10\n",
                "\nphase.1 action=typing frames=1 x=2\n",
                "不认识的键：x",
            ),
        ] {
            assert!(g.contains(from), "{from}");
            let e = err_of(&g.replace(from, to));
            assert!(e.contains(want), "{to:?}: {e}");
        }
    }

    #[test]
    fn link_lines_are_checked_field_by_field() {
        let g = good();
        let line = g
            .lines()
            .find(|l| l.starts_with("link.0 "))
            .unwrap()
            .to_owned();
        for (from, to, want) in [
            ("spikes=1000000:50000:20000", "spikes=1:2", "3 个用冒号隔开"),
            (
                "spikes=1000000:50000:20000",
                "spikes=a:b:c",
                "3 个用冒号隔开",
            ),
            ("doze=100000:15000", "doze=1:2:3", "2 个用冒号隔开"),
            ("busy=2000000:10000", "busy=x", "2 个用冒号隔开"),
            ("loss_permille=12", "loss_permille=70000", "loss_permille"),
            ("rate_bps=30000000", "rate_bps=fast", "rate_bps"),
            ("up=1", "up=2", "应当是 0 或 1"),
            ("at=0", "at=-5", "at"),
        ] {
            assert!(line.contains(from), "{from}");
            let e = err_of(&g.replace(&line, &line.replace(from, to)));
            assert!(e.contains("link.0") && e.contains(want), "{to}: {e}");
        }
        // 缺字段、多字段、字段重复、格式不对
        let no_up = line.replace(" up=1", "");
        assert!(err_of(&g.replace(&line, &no_up)).contains("link.0：缺少字段 up"));
        assert!(err_of(&g.replace(&line, &format!("{line} extra=1"))).contains("不认识的键：extra"));
        assert!(err_of(&g.replace(&line, &format!("{line} at=1"))).contains("字段 at 重复"));
        assert!(err_of(&g.replace(&line, "link.0 junk")).contains("不是 名=值"));
    }

    #[test]
    fn semantic_checks_have_clear_messages() {
        let g = good();
        let cases = [
            ("\nworld 1\n", "\nworld 2\n", "格式版本是 2"),
            ("\nrng_layout 1\n", "\nrng_layout 9\n", "随机数布局是 9"),
            (
                "\nrng_layout 1\n",
                "\nrng_layout 2\n",
                "links 是随机数布局 1 的键",
            ),
            ("\nwidth 640\n", "\nwidth 0\n", "不能为 0"),
            ("\nfps 60\n", "\nfps 0\n", "fps 不能为 0"),
            (
                "\nbyte_model default\n",
                "\nbyte_model openh264-v1\n",
                "只对 h264-l05、h264-l0 有意义",
            ),
        ];
        for (from, to, want) in cases {
            assert!(g.contains(from), "{from}");
            let e = err_of(&g.replace(from, to));
            assert!(e.contains(want), "{e}");
        }
        // 阶段数为 0
        let zero = g.replace("\nphases 3\n", "\nphases 0\n");
        assert!(err_of(&zero).contains("至少要有一个阶段"));
        // 阶段数写少了：多出来的 phase.N 是不认识的键
        let few = g.replace("\nphases 3\n", "\nphases 2\n");
        assert!(err_of(&few).contains("phase.2"));
        // 空文件、乱码
        assert!(err_of("").contains("缺少键 world"));
        assert!(err_of("world\n").contains("没有值"));
    }

    #[test]
    fn writing_rejects_what_the_file_cannot_hold() {
        let mut s = Scenario::fixed("a\nb", phases());
        assert!(to_text(&s).unwrap_err().to_string().contains("name"));
        s.name = " 前面有空格".into();
        assert!(to_text(&s).is_err());
        // 没命名的画像
        let mut s = Scenario::fixed("x", phases());
        s.profile = Profile {
            name: "mine",
            ..Profile::STABLE
        };
        assert!(to_text(&s)
            .unwrap_err()
            .to_string()
            .contains("网络画像不是命名的"));
        let mut s = Scenario::fixed("x", phases());
        s.policy = ContentPolicy {
            max_qp: 40,
            ..ContentPolicy::OFFICE
        };
        assert!(to_text(&s)
            .unwrap_err()
            .to_string()
            .contains("内容策略不是命名的"));
        // 字节模型配了不用它的编码
        let mut s = Scenario::fixed("x", phases());
        s.byte_model = ByteModelKind::Openh264V1;
        assert!(to_text(&s)
            .unwrap_err()
            .to_string()
            .contains("只对 h264-l05、h264-l0 有意义"));
        // 名字里的 # 和 = 没问题
        let s = Scenario::fixed("a#b=c", phases());
        assert_round_trips(&s);
    }

    #[test]
    fn export_names_files_by_hash_and_load_checks_them() {
        let dir = std::env::temp_dir().join(format!("peerdesk-world-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = busy_scenario();
        let path = export(&dir, &s).unwrap();
        let text = to_text(&s).unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            file_name(&text)
        );
        // 再导出一次：同一个文件，不出错
        assert_eq!(export(&dir, &s).unwrap(), path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let world = World::load(&path).unwrap();
        assert_eq!(world.hash, hash(&text));
        assert_eq!(to_text(&world.scenario).unwrap(), text);
        // 改一个字节：文件名上的哈希对不上
        let tampered = path.with_file_name(path.file_name().unwrap());
        std::fs::write(&tampered, text.replace("seed 99", "seed 98")).unwrap();
        let e = World::load(&tampered).unwrap_err().to_string();
        assert!(e.contains("对不上"), "{e}");
        // 手写的名字不核对哈希，但内容照样检查
        let hand = dir.join("hand.world");
        std::fs::write(&hand, text.replace("seed 99", "seed 98")).unwrap();
        assert_eq!(World::load(&hand).unwrap().scenario.seed, 98);
        std::fs::write(&hand, "# 空\n").unwrap();
        let e = World::load(&hand).unwrap_err().to_string();
        assert!(
            e.contains("hand.world") && e.contains("缺少键 world"),
            "{e}"
        );
        std::fs::write(&hand, [0xff, 0xfe]).unwrap();
        assert!(World::load(&hand)
            .unwrap_err()
            .to_string()
            .contains("不是 UTF-8"));
        assert!(World::load(&dir.join("none.world"))
            .unwrap_err()
            .to_string()
            .contains("读不出来"));
        // 不能建目录：把一个文件当目录
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, "x").unwrap();
        assert!(export(&blocker.join("sub"), &s)
            .unwrap_err()
            .to_string()
            .contains("建目录"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_world_runs_to_the_same_result_and_matches_the_scenario() {
        let mut s = Scenario::fixed(
            "world-run",
            vec![
                Phase {
                    action: Action::Typing,
                    frames: 20,
                },
                Phase {
                    action: Action::Idle,
                    frames: 15,
                },
            ],
        );
        s.width = 320;
        s.height = 240;
        s.codec = CodecKind::H264L0;
        s.byte_model = ByteModelKind::Openh264V1;
        let text = to_text(&s).unwrap();
        let world = World {
            hash: hash(&text),
            scenario: from_text(&text).unwrap(),
        };
        let a = world.run().unwrap();
        let b = world.run().unwrap();
        assert_eq!(crate::fingerprint(&a), crate::fingerprint(&b));
        // 和直接跑原场景一样
        let direct = crate::run(&s).unwrap();
        assert_eq!(crate::fingerprint(&a), crate::fingerprint(&direct));
    }

    #[test]
    fn the_fitted_model_changes_the_bytes_and_the_default_does_not() {
        let mk = |model| {
            let mut s = Scenario::fixed(
                "model",
                vec![Phase {
                    action: Action::Typing,
                    frames: 20,
                }],
            );
            s.width = 320;
            s.height = 240;
            s.codec = CodecKind::H264L05;
            s.byte_model = model;
            crate::run(&s).unwrap().sent_bytes
        };
        let default = mk(ByteModelKind::Default);
        let fitted = mk(ByteModelKind::Openh264V1);
        let regional = mk(ByteModelKind::Openh264V2);
        assert_ne!(default, fitted);
        assert_ne!(default, regional);
        assert_ne!(fitted, regional);
        assert_eq!(default, mk(ByteModelKind::Default));
    }

    #[test]
    fn env_conflict_is_only_about_the_pacer_factor() {
        let mut s = Scenario::fixed("x", phases());
        assert_eq!(env_conflict(&s), None);
        s.pacing = true;
        // 测试环境不设 PEERDESK_PACE_FACTOR
        if std::env::var_os("PEERDESK_PACE_FACTOR").is_none() {
            assert_eq!(env_conflict(&s), None);
        }
    }

    #[test]
    fn errors_display() {
        assert_eq!(WorldError::Format("a".into()).to_string(), "a");
        assert_eq!(WorldError::Io("b".into()).to_string(), "b");
        assert!(WorldError::Run(Error::InvalidArgument("c"))
            .to_string()
            .starts_with("仿真出错："));
    }

    /// 扰动的键：默认时不写（老的世界文件、默认场景的哈希都不变），不是默认时写出来能读回；
    /// 占位模型配了扰动、不认识的写法都拒绝。
    #[test]
    fn byte_noise_is_written_only_when_it_is_not_the_default() {
        let mut s = l0_scenario();
        let plain = to_text(&s).unwrap();
        assert!(!plain.contains("byte_noise"), "{plain}");
        assert_eq!(from_text(&plain).unwrap().byte_noise, ByteNoise::Default);
        for (name, noise) in [("within", ByteNoise::Within), ("off", ByteNoise::Off)] {
            s.byte_noise = noise;
            let text = to_text(&s).unwrap();
            assert!(text.contains(&format!("\nbyte_noise {name}\n")), "{text}");
            let back = from_text(&text).unwrap();
            assert_eq!(back.byte_noise, noise);
            assert_eq!(to_text(&back).unwrap(), text);
            assert_ne!(hash(&text), hash(&plain));
        }
        // 明写 default 也读得进来
        let explicit = plain.replace("\nrng_layout", "\nbyte_noise default\nrng_layout");
        assert_eq!(from_text(&explicit).unwrap().byte_noise, ByteNoise::Default);
        assert!(
            from_text(&plain.replace("\nrng_layout", "\nbyte_noise loud\nrng_layout")).is_err()
        );
        // 占位模型没有扰动
        let mut placeholder = Scenario::fixed("p", s.phases.clone());
        placeholder.codec = CodecKind::H264L0;
        placeholder.byte_noise = ByteNoise::Off;
        assert!(to_text(&placeholder).is_err());
        placeholder.byte_noise = ByteNoise::Default;
        let text = to_text(&placeholder).unwrap();
        assert!(from_text(&text.replace("\nrng_layout", "\nbyte_noise off\nrng_layout")).is_err());
    }

    /// 会话的身份不随仿真的档、字节模型、扰动变；画面、网络、种子任何一样变了就变。换档之后的世界哈希跟着内容走。
    #[test]
    fn the_session_key_ignores_the_tier_and_follows_everything_else() {
        let l1 = World {
            hash: String::new(),
            scenario: busy_scenario(),
        };
        let key = session_key(&l1.scenario).unwrap();
        assert_eq!(
            key,
            hash(&to_text(&l1.scenario).unwrap()),
            "L1 的键就是它的世界哈希"
        );
        let l0 = l1
            .retiered(
                Some(CodecKind::H264L0),
                Some(ByteModelKind::Openh264V2),
                Some(ByteNoise::Within),
            )
            .unwrap();
        assert_eq!(l0.scenario.codec, CodecKind::H264L0);
        assert_eq!(l0.hash, hash(&to_text(&l0.scenario).unwrap()));
        assert_ne!(l0.hash, key);
        assert_eq!(session_key(&l0.scenario).unwrap(), key);
        let l05 = l1.retiered(Some(CodecKind::H264L05), None, None).unwrap();
        assert_eq!(session_key(&l05.scenario).unwrap(), key);
        let mut other = l1.scenario.clone();
        other.seed += 1;
        assert_ne!(session_key(&other).unwrap(), key);
        // 不合法的组合：真编码器配字节模型、占位模型配扰动
        assert!(l1
            .retiered(None, Some(ByteModelKind::Openh264V1), None)
            .is_err());
        assert!(l1
            .retiered(Some(CodecKind::H264L0), None, Some(ByteNoise::Off))
            .is_err());
        // 块编码不参与对账：键就是它自己
        let mut tile = l1.scenario.clone();
        tile.codec = CodecKind::Tile;
        assert_eq!(session_key(&tile).unwrap(), hash(&to_text(&tile).unwrap()));
        assert_eq!(codec_named("h264-l05"), Some(CodecKind::H264L05));
        assert_eq!(codec_named("mpeg2"), None);
    }

    /// 布局 2 的每一种写法都用上：四种容量、两种队列策略、三种丢包、三种延迟、限速器、三种交叉流量、
    /// 带新参数和纯中断的换路、按时刻换参数。
    fn layout2_scenario() -> Scenario {
        use crate::{Layout2, PathStep};
        use peerdesk_core::path::{
            Aqm, BottleneckConfig, Capacity, CrossSpec, Delay, Dir, DirConfig, Loss, Noise,
            PathConfig, Policer, QueueLimit, Receiver, RouteChange, TcpConfig, Trace,
        };
        use peerdesk_core::time::Time;
        let full = DirConfig {
            policer: Some(Policer {
                rate_bps: 30_000_000,
                burst_bytes: 30_000,
            }),
            bottleneck: BottleneckConfig {
                capacity: Capacity::Trace(Trace {
                    segments: vec![(1_000_000, 5_000_000), (0, 1), (500_000, 0)],
                    repeat: true,
                }),
                limit: QueueLimit {
                    max_delay_us: Some(100_000),
                    max_bytes: Some(60_000),
                },
                aqm: Aqm::codel(),
            },
            loss: Loss::Burst {
                loss_ppm: 30_000,
                avg_burst_milli: 3_000,
            },
            delay: Delay::Steps {
                initial_us: 20_000,
                steps: vec![(Time(1_000_000), 30_000), (Time(2_000_000), 25_000)],
            },
            noise: Noise {
                jitter_us: 3_000,
                fifo_jitter: true,
                spikes: Some(Spikes {
                    every_us: 400_000,
                    length_us: 30_000,
                    max_us: 20_000,
                }),
                doze: Some(Doze {
                    idle_us: 30_000,
                    wake_us: 20_000,
                }),
            },
            receiver: Receiver {
                busy: Some(Busy {
                    every_us: 100_000,
                    length_us: 5_000,
                }),
                batch_us: 4_000,
            },
            loss_before_bottleneck: true,
        };
        let plain = |capacity, loss, delay| DirConfig {
            bottleneck: BottleneckConfig {
                capacity,
                limit: QueueLimit {
                    max_delay_us: None,
                    max_bytes: None,
                },
                aqm: Aqm::TailDrop,
            },
            loss,
            delay,
            ..DirConfig::default()
        };
        let poisson = plain(
            Capacity::Poisson {
                rate_bps: 12_000_000,
                opportunity_bytes: 1500,
            },
            Loss::Independent { ppm: 1_000 },
            Delay::Periodic {
                initial_us: 20_000,
                period_us: 15_000_000,
                phase_us: 3_000_000,
                low_us: 25_000,
                high_us: 60_000,
            },
        );
        let constant = plain(
            Capacity::Constant {
                rate_bps: 8_000_000,
            },
            Loss::None,
            Delay::Fixed(5_000),
        );
        let empty_steps = plain(
            Capacity::Unlimited,
            Loss::None,
            Delay::Steps {
                initial_us: 1,
                steps: Vec::new(),
            },
        );
        let empty_trace = plain(
            Capacity::Trace(Trace {
                segments: Vec::new(),
                repeat: false,
            }),
            Loss::None,
            Delay::Fixed(0),
        );
        let tcp = TcpConfig::default();
        let mut s = Scenario::fixed("布局 2", phases());
        s.network = NetworkPlan::Layout2(Box::new(Layout2 {
            start: PathConfig {
                forward: full,
                reverse: poisson,
            },
            steps: vec![(
                700_000,
                PathStep {
                    config: PathConfig {
                        forward: constant.clone(),
                        reverse: empty_steps,
                    },
                    up: false,
                },
            )],
            routes: vec![
                RouteChange {
                    at: Time(1_000_000),
                    down_us: 300_000,
                    new: Some(PathConfig {
                        forward: empty_trace,
                        reverse: constant,
                    }),
                },
                RouteChange {
                    at: Time(2_000_000),
                    down_us: 50_000,
                    new: None,
                },
            ],
            cross: vec![
                (
                    Dir::Forward,
                    CrossSpec::Cbr {
                        rate_bps: 2_000_000,
                        packet_bytes: 1_200,
                        start: Time(100),
                        stop: Some(Time(900_000)),
                    },
                ),
                (
                    Dir::Reverse,
                    CrossSpec::Cbr {
                        rate_bps: 1,
                        packet_bytes: 100,
                        start: Time::ZERO,
                        stop: None,
                    },
                ),
                (
                    Dir::Reverse,
                    CrossSpec::Tcp {
                        cfg: tcp,
                        start: Time(5),
                        bytes: Some(1_000_000),
                    },
                ),
                (
                    Dir::Forward,
                    CrossSpec::Tcp {
                        cfg: tcp,
                        start: Time(6),
                        bytes: None,
                    },
                ),
                (
                    Dir::Forward,
                    CrossSpec::Bursts {
                        cfg: tcp,
                        connections: 30,
                        min_bytes: 30_000,
                        max_bytes: 50_000,
                        mean_off_us: 10_000_000,
                        start_on: true,
                        start: Time(7),
                    },
                ),
            ],
        }));
        s
    }

    #[test]
    fn layout2_round_trips_byte_identical() {
        let s = layout2_scenario();
        let text = assert_round_trips(&s);
        assert!(text.contains("\nrng_layout 2\n"));
        assert!(!text.contains("\nlinks "), "布局 2 不写布局 1 的键");
        assert!(text.contains("capacity=trace:repeat:1000000@5000000,0@1,500000@0"));
        assert!(text.contains("\nroute.1 at=2000000 down_us=50000 new=0\n"));
        assert!(!text.contains("route.1.fwd"));
        // 布局 1 转过来的也能来回；布局 1 的写法和以前一样（哈希不变）由 round_trips_are_byte_identical 守着
        let mut converted = busy_scenario();
        converted.network = converted.network.to_layout2(true);
        let t = assert_round_trips(&converted);
        assert!(t.contains("loss_first=1"));
        assert_ne!(hash(&t), hash(&to_text(&busy_scenario()).unwrap()));
        assert!(!to_text(&busy_scenario()).unwrap().contains("loss_first"));
        let w = World {
            hash: hash(&to_text(&busy_scenario()).unwrap()),
            scenario: busy_scenario(),
        };
        let two = w.with_layout2(false).unwrap();
        assert_eq!(two.scenario.network.layout(), 2);
        assert_eq!(two.hash, hash(&to_text(&two.scenario).unwrap()));
        assert_eq!(
            two.with_layout2(true).unwrap().hash,
            two.hash,
            "已经是布局 2 的不变"
        );
    }

    /// 把 `text` 里的 `from` 换成 `to` 再读，应当报错，消息里有 `want`。
    fn rejects(text: &str, from: &str, to: &str, want: &str) {
        assert!(text.contains(from), "测试用的文本里没有 {from:?}");
        let bad = text.replacen(from, to, 1);
        let e = from_text(&bad).map(|_| ()).unwrap_err().to_string();
        assert!(e.contains(want), "{from:?} → {to:?}：{e}");
    }

    #[test]
    fn layout2_reports_every_bad_value() {
        let text = to_text(&layout2_scenario()).unwrap();
        let cases = [
            // 两种布局的键不能混写
            ("\nrng_layout 2\n", "\nrng_layout 3\n", "随机数布局是 3"),
            (
                "\nsteps 1\n",
                "\nlinks 0\nsteps 1\n",
                "links 是随机数布局 1 的键",
            ),
            (
                "capacity=trace:repeat:",
                "capacity=trace:twice:",
                "trace:once",
            ),
            (
                "capacity=trace:repeat:1000000@5000000",
                "capacity=trace:repeat:1000000@x",
                "非负整数@非负整数",
            ),
            (
                "capacity=trace:repeat:1000000@5000000",
                "capacity=trace:repeat:1000000",
                "非负整数@非负整数",
            ),
            (
                "capacity=poisson:12000000:1500",
                "capacity=poisson:12000000",
                "2 个用冒号隔开",
            ),
            (
                "capacity=const:8000000",
                "capacity=const:x",
                "1 个用冒号隔开",
            ),
            (
                "capacity=unlimited",
                "capacity=unlimited:5",
                "capacity 的值",
            ),
            ("capacity=unlimited", "capacity=fast", "capacity 的值"),
            ("aqm=codel:5000:100000", "aqm=codel:5000", "aqm 的值"),
            ("aqm=tail", "aqm=red", "aqm 的值"),
            ("loss=burst:30000:3000", "loss=burst:30000", "loss 的值"),
            ("loss=ind:1000", "loss=ind:5000000000", "超出范围"),
            ("loss=none", "loss=some", "loss 的值"),
            ("delay=fixed:5000", "delay=fixed:5:6", "delay 的值"),
            ("delay=steps:20000:", "delay=steps:x:", "delay 的值"),
            ("delay=periodic:20000:", "delay=periodic:x:", "delay 的值"),
            ("delay=fixed:5000", "delay=slow", "delay 的值"),
            ("policer=30000000:30000", "policer=30000000", "policer"),
            ("step.0 at=700000 up=0", "step.0 at=700000 up=0 x=1", "x"),
            ("new=0", "new=2", "new"),
            ("kind=bursts", "kind=video", "kind"),
            ("dir=rev", "dir=up", "dir"),
            ("connections=30", "connections=x", "connections"),
            ("mss_bytes=1250", "mss_bytes=-1", "mss_bytes"),
            ("packet_bytes=1200", "packet_bytes=x", "packet_bytes"),
        ];
        for (from, to, want) in cases {
            rejects(&text, from, to, want);
        }
        // 布局 2 的键出现在布局 1 的文件里
        let one = to_text(&Scenario::fixed("x", phases())).unwrap();
        for key in layout2::KEYS {
            rejects(
                &one,
                "\nlinks ",
                &format!("\n{key} 0\nlinks "),
                "是随机数布局 2 的键",
            );
        }
    }

    fn segmented_scenario() -> Scenario {
        use crate::homes;
        use crate::SegmentStep;
        use peerdesk_core::path::segments::{Segment, SegmentRoute};
        use peerdesk_core::path::PathConfig;
        use peerdesk_core::time::Time;
        let mut plan = homes::quiet();
        homes::b_watching_video(&mut plan, Time(1_000_000));
        homes::a_uploading(&mut plan, Time(2_000_000), true);
        plan.steps.push((
            3_000_000,
            SegmentStep {
                segment: Segment::Public,
                config: PathConfig::symmetric(homes::cable_up()),
                up: false,
            },
        ));
        plan.routes = vec![
            SegmentRoute {
                at: Time(4_000_000),
                down_us: 300_000,
                down: vec![Segment::ALan, Segment::AEgress],
                new: vec![
                    (Segment::ALan, PathConfig::symmetric(homes::wifi())),
                    (Segment::AEgress, PathConfig::symmetric(homes::cable_down())),
                ],
            },
            SegmentRoute {
                at: Time(5_000_000),
                down_us: 10,
                down: vec![Segment::BLan],
                new: Vec::new(),
            },
        ];
        let mut s = Scenario::fixed("分段", phases());
        s.network = NetworkPlan::Segmented(Box::new(plan));
        s
    }

    #[test]
    fn segmented_round_trips_byte_identical() {
        let text = assert_round_trips(&segmented_scenario());
        assert!(text.contains("\nrng_layout 2\n"));
        assert!(text.contains("\nseg.b-egress.fwd "));
        assert!(text.contains(
            "\nseg_route.0 at=4000000 down_us=300000 down=a-lan,a-egress new=a-lan,a-egress\n"
        ));
        assert!(text.contains("\nseg_route.0.a-egress.rev "));
        assert!(text.contains("\nseg_route.1 at=5000000 down_us=10 down=b-lan new=-\n"));
        assert!(text.contains("\nseg_cross.0 seg=b-egress dir=fwd kind=bursts "));
        assert!(text.contains("\nseg_step.0 at=3000000 seg=public up=0\n"));
        assert!(!text.contains("\npath.fwd") && !text.contains("\nlinks "));
        let quiet = Scenario {
            network: NetworkPlan::Segmented(Box::new(crate::homes::quiet())),
            ..Scenario::fixed("安静", phases())
        };
        let q = assert_round_trips(&quiet);
        assert!(q.contains("\nseg_routes 0\n") && q.contains("\nseg_cross 0\n"));
        assert!(!q.contains("down=-"));
    }

    #[test]
    fn segmented_reports_bad_values() {
        let text = to_text(&segmented_scenario()).unwrap();
        let cases = [
            ("seg=public up=0", "seg=lan up=0", "段名 \"lan\" 不认识"),
            ("down=a-lan,a-egress", "down=a-lan,x", "段名 \"x\" 不认识"),
            (
                "\nseg_cross.0 seg=b-egress",
                "\nseg_cross.0 seg=c-egress",
                "段名",
            ),
            (
                "\nseg_cross.0 seg=b-egress dir=fwd",
                "\nseg_cross.0 seg=b-egress dir=fwd x=1",
                "x",
            ),
            (
                "\nseg_steps 1\n",
                "\nseg_steps 1\npath.fwd x=1\n",
                "不能同时出现",
            ),
        ];
        for (from, to, want) in cases {
            rejects(&text, from, to, want);
        }
        // 分段的键也不能出现在布局 1 的文件里
        let one = to_text(&Scenario::fixed("x", phases())).unwrap();
        rejects(
            &one,
            "\nlinks ",
            "\nseg_cross 0\nlinks ",
            "是随机数布局 2 的键",
        );
    }
}
