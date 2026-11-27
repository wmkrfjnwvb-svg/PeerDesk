//! M2 测量工具（只在本地跑，不进 CI、不进安装包；说明见 `docs/research/Q-redesign/m2-m3-protocol.md` 第 0、1 节）。
//!
//! 直接用产品里的 `OpenH264Backend` 按固定 QP 编，每帧输出一行 CSV：要量的是“后端本身给什么 QP 出多少字节”，
//! 不经过 `H264Encoder` 上面那层码率控制。颜色转换用产品同一套（`peerdesk_media::screen_to_i420`），
//! 变化面积和细节用 `peerdesk_media::content` 里假后端（L0）用的同一组函数在亮度平面上算：
//! 模型的输入和 L0 里假后端拿到的量逐位相同。
//!
//! 用法：`measure <清单> <输出目录> <第几份> <共几份>`。清单里每行一个任务，按行号对份数取余分给各份，
//! 每份写自己的 `<输出目录>/shard-<第几份>.csv`；并行就是同时起几个进程。
//!
//! 清单每行（空格分隔，`#` 开头是注释）：
//! - `seq <集> <类别> <种子> <宽> <高> <帧数> <QP,QP,…> <强制关键帧的帧号,…|->`：一个序列，每个 QP 各开一个新后端从头编；
//! - `refine <集> <种子> <宽> <高> <链;链;…>`：静止画面补清晰，一条链是 `QP1,QP2,…`，同一幅画面依次按这些 QP 编；
//! - `scene <集> <种子> <宽> <高> <QP> <面积千分比,…>`：整屏换窗口的门槛：前一帧是桌面，后一帧把其中一块（占这么多千分比）换成另一个桌面的内容；
//!   `scenev` 同上，换进来的是视频噪声（变化面积能到 100%）。
//!
//! 另有一个子命令 `measure floor <输出目录> <第几份> <共几份>`：低 QP 地板的根因实验（`docs/research/Q-redesign/m2b-plan.md` 第 2 节），
//! 不读清单，任务（12 个种子）写死在代码里，每帧多记解码后对原图的亮度 PSNR。
//!
//! 固定：单线程；每个（序列, QP）新开一个后端；每个任务的第一个 QP 编两遍，字节数必须相同（自检，不同就报错退出）。

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use peerdesk_core::frame::Picture;
use peerdesk_core::screen::Screen;
use peerdesk_core::video::{Codec, FrameHeader, VideoDecoder};
use peerdesk_e2e::content::{Action, RealisticDesktop};
use peerdesk_media::backend::{EncoderBackend, Finished, FrameAsk, FrameInput, Rates, Submitted};
use peerdesk_media::content::{detail_permille, luma_changed_permille, sample_luma};
use peerdesk_media::soft::OpenH264Backend;
use peerdesk_media::H264Decoder;

type Res<T> = Result<T, String>;

const HEADER: &str = "backend,seq,class,split,w,h,frame,qp,asked_key,got_key,bytes,changed_permille,detail_permille,prev_qp,enc_us";

/// 一帧：I420 三个平面，和上一帧比的变化面积、细节（都是亮度平面上算的，千分之几）。
struct Frame {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    changed: u32,
    detail: u32,
}

/// 编码结果的一行。
#[derive(PartialEq, Eq)]
struct Out {
    asked_key: bool,
    got_key: bool,
    bytes: usize,
    us: u64,
    prev_qp: Option<u8>,
}

/// 小的确定性随机数（splitmix64）：只用来摆放测量画面，和产品无关。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u32) -> u32 {
        u32::try_from(self.next() % u64::from(n.max(1))).unwrap_or(0)
    }
}

fn num<T: std::str::FromStr>(s: &str, what: &str) -> Res<T> {
    s.parse().map_err(|_| format!("{what} 不是数：{s}"))
}

fn list<T: std::str::FromStr>(s: &str, what: &str) -> Res<Vec<T>> {
    if s == "-" {
        return Ok(Vec::new());
    }
    s.split(',').map(|p| num(p, what)).collect()
}

/// 清单里的一个任务。
enum Job {
    Seq {
        split: String,
        class: String,
        seed: u64,
        size: (u16, u16),
        frames: usize,
        qps: Vec<u8>,
        forced: Vec<usize>,
    },
    Refine {
        split: String,
        seed: u64,
        size: (u16, u16),
        chains: Vec<Vec<u8>>,
    },
    /// 目录里按序号排好的 PNG 当一个序列（真字体测试页）。
    Pngseq {
        split: String,
        class: String,
        name: String,
        dir: String,
        qps: Vec<u8>,
        forced: Vec<usize>,
    },
    Scene {
        split: String,
        seed: u64,
        size: (u16, u16),
        qp: u8,
        areas: Vec<u32>,
        /// 换进来的是视频噪声（变化面积能到 100%），不是另一个桌面（桌面之间有大片相同的底色，变化面积最多约 60%）。
        noise: bool,
    },
}

fn parse_job(line: &str) -> Res<Job> {
    let f: Vec<&str> = line.split_whitespace().collect();
    let at = |i: usize| f.get(i).copied().ok_or_else(|| format!("字段不够：{line}"));
    let size = |i: usize| -> Res<(u16, u16)> { Ok((num(at(i)?, "宽")?, num(at(i + 1)?, "高")?)) };
    match at(0)? {
        "seq" => Ok(Job::Seq {
            split: at(1)?.to_owned(),
            class: at(2)?.to_owned(),
            seed: num(at(3)?, "种子")?,
            size: size(4)?,
            frames: num(at(6)?, "帧数")?,
            qps: list(at(7)?, "QP")?,
            forced: list(at(8)?, "帧号")?,
        }),
        "refine" => Ok(Job::Refine {
            split: at(1)?.to_owned(),
            seed: num(at(2)?, "种子")?,
            size: size(3)?,
            chains: at(5)?
                .split(';')
                .map(|c| list(c, "QP"))
                .collect::<Res<_>>()?,
        }),
        kind @ ("scene" | "scenev") => Ok(Job::Scene {
            noise: kind == "scenev",
            split: at(1)?.to_owned(),
            seed: num(at(2)?, "种子")?,
            size: size(3)?,
            qp: num(at(5)?, "QP")?,
            areas: list(at(6)?, "面积")?,
        }),
        "pngseq" => Ok(Job::Pngseq {
            split: at(1)?.to_owned(),
            class: at(2)?.to_owned(),
            name: at(3)?.to_owned(),
            dir: at(4)?.to_owned(),
            qps: list(at(5)?, "QP")?,
            forced: list(at(6)?, "帧号")?,
        }),
        other => Err(format!("不认识的任务：{other}")),
    }
}

/// 画面风格：种子 8、9 是“大片纯色”（壁纸的噪点和渐变压成色块），10、11 是“密纹理”（画面上叠一层细条纹，边缘很多），
/// 其余是原样的“照片壁纸”。合成桌面的细节量变化范围很窄（亮度边缘占 9%–14%），补这两档把（面积, 细节）的范围撑开；
/// 它们是合成的滤镜，不是真画面，报告里单独标明。
fn style_of(seed: u64) -> u8 {
    match seed {
        8 | 9 => 1,
        10 | 11 => 2,
        _ => 0,
    }
}

fn styled(screen: &Screen, style: u8) -> Res<Screen> {
    if style == 0 {
        return Ok(screen.clone());
    }
    let w = usize::from(screen.width()).max(1);
    let px: Vec<u32> = screen
        .pixels()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            if style == 1 {
                p & 0x00f0_f0f0
            } else if (i % w / 2 + i / w / 3) % 3 == 0 {
                p ^ 0x0040_4040
            } else {
                *p
            }
        })
        .collect();
    Screen::from_pixels(screen.width(), screen.height(), px).map_err(|e| e.to_string())
}

/// 把一幅画面变成 I420 和（相对上一帧的）内容特征。`prev` 是上一帧的亮度抽样，用完换成这一帧的。
fn to_frame(screen: &Screen, style: u8, prev: &mut Option<Vec<u8>>) -> Res<Frame> {
    let (_, _, y, u, v) = peerdesk_media::screen_to_i420(&styled(screen, style)?);
    let sample = sample_luma(&y);
    let changed = luma_changed_permille(&sample, prev.as_ref());
    let detail = detail_permille(&y);
    *prev = Some(sample);
    Ok(Frame {
        y,
        u,
        v,
        changed,
        detail,
    })
}

/// 开场前先让桌面处在不同的状态（窗口叠放、文字多少不同），免得所有种子的第一帧都差不多。
fn warm(desk: &mut RealisticDesktop, seed: u64, class: &str) -> Res<()> {
    let e = |e: peerdesk_core::error::Error| e.to_string();
    // 切换类自己摆弹窗；其他类里单数种子先把弹窗放出来（更多文字叠在壁纸上）
    if seed % 2 == 1 && class != "switching" {
        let _ = desk.step(Action::Switching).map_err(e)?;
    }
    if seed % 4 >= 2 && class != "scrolling" && class != "typing" {
        for _ in 0..40 {
            let _ = desk.step(Action::Typing).map_err(e)?;
        }
    }
    Ok(())
}

/// 全屏视频的一帧：和 `content.rs` 视频区域同一个公式（缓慢移动的彩色波纹加每帧不同的颗粒），铺满整个画面。
fn full_video(w: u16, h: u16, t: u32, rng: &mut Rng) -> Res<Screen> {
    let (wu, hu) = (u32::from(w), u32::from(h));
    let mut px = Vec::with_capacity(usize::from(w) * usize::from(h));
    let wave = |a: u32, b: u32| -> u32 { (a.wrapping_mul(3) + b.wrapping_mul(5) + t * 7) % 256 };
    for y in 0..hu {
        for x in 0..wu {
            let grain = rng.below(24);
            let r = (wave(x, y) / 2 + 60 + grain).min(255);
            let g = (wave(y, x + t) / 3 + 40 + grain).min(255);
            let b = (wave(x + y, t) / 2 + 50 + grain).min(255);
            px.push(r << 16 | g << 8 | b);
        }
    }
    Screen::from_pixels(w, h, px).map_err(|e| e.to_string())
}

/// 把 `donor` 里左上角在 (x, y)、`rw`×`rh` 的一块贴到 `base` 上。
fn paste(base: &mut Screen, donor: &Screen, x: u16, y: u16, rw: u16, rh: u16) {
    for row in y..y.saturating_add(rh).min(base.height()) {
        let len = rw.min(base.width().saturating_sub(x));
        let src = donor.row(x, row, len).to_vec();
        base.row_mut(x, row, len).copy_from_slice(&src);
    }
}

/// 占画面 `permille`‰ 的矩形（宽高比和画面相同），位置由随机数定。
fn rect_of(w: u16, h: u16, permille: u32, rng: &mut Rng) -> (u16, u16, u16, u16) {
    // 宽、高各占 sqrt(面积比)：整数开方，单位千分之一
    let side = u32::try_from(u64::from(permille * 1000).isqrt()).unwrap_or(1000);
    let part = |whole: u16| {
        u16::try_from(u32::from(whole) * side / 1000)
            .unwrap_or(whole)
            .clamp(2, whole)
    };
    let (rw, rh) = (part(w), part(h));
    let x = u16::try_from(rng.below(u32::from(w - rw) + 1)).unwrap_or(0);
    let y = u16::try_from(rng.below(u32::from(h - rh) + 1)).unwrap_or(0);
    (x, y, rw, rh)
}

/// 生成一个序列的帧：第 0 帧是起始画面，之后每帧走一步操作（只收画面真变了的帧，和产品一样不编没变的）。
fn make_frames(class: &str, seed: u64, size: (u16, u16), n: usize) -> Res<Vec<Frame>> {
    let (w, h) = size;
    let style = style_of(seed);
    let mut prev = None;
    let mut out = Vec::with_capacity(n);
    let err = |e: peerdesk_core::error::Error| e.to_string();
    if class == "fullvideo" {
        let mut rng = Rng(seed ^ 0xf011);
        for t in 0..n {
            let s = full_video(w, h, u32::try_from(t).unwrap_or(0), &mut rng)?;
            out.push(to_frame(&s, 0, &mut prev)?);
        }
        return Ok(out);
    }
    let mut desk = RealisticDesktop::new(w, h, seed).map_err(err)?;
    warm(&mut desk, seed, class)?;
    if class == "switching" {
        // 切窗口：基底是桌面，每一帧把一块（占 10%–100%，顺序打乱）换成另一个桌面（弹窗在最上面）同一位置的内容。
        // 真实的 `Action::Switching` 只切一次，凑不出序列，所以这样摆；换掉的是真合成桌面的内容，不是随机噪声。
        let base = desk.screen().clone();
        out.push(to_frame(&base, style, &mut prev)?);
        let mut donors = Vec::new();
        for k in 0..3u64 {
            let mut d = RealisticDesktop::new(w, h, seed.wrapping_add(1000 + k)).map_err(err)?;
            let _ = d.step(Action::Switching).map_err(err)?;
            donors.push(d.screen().clone());
        }
        let mut rng = Rng(seed ^ 0x5717c4);
        let mut areas: Vec<u32> = (1..=10).map(|i| i * 100).collect();
        for i in 1..n {
            // 每十帧把 10%–100% 各用一次，顺序每轮重新打乱
            if (i - 1) % 10 == 0 {
                for j in (1..areas.len()).rev() {
                    let k =
                        usize::try_from(rng.below(u32::try_from(j + 1).unwrap_or(1))).unwrap_or(0);
                    areas.swap(j, k);
                }
            }
            let permille = areas.get((i - 1) % 10).copied().unwrap_or(1000);
            let (x, y, rw, rh) = rect_of(w, h, permille, &mut rng);
            let mut s = base.clone();
            if let Some(d) = donors.get(i % donors.len().max(1)) {
                paste(&mut s, d, x, y, rw, rh);
            }
            out.push(to_frame(&s, style, &mut prev)?);
        }
        return Ok(out);
    }
    let action = match class {
        "pointer" => Action::Pointer,
        "typing" => Action::Typing,
        "scrolling" => Action::Scrolling,
        "dragging" => Action::Dragging,
        "video" => Action::Video,
        other => return Err(format!("不认识的类别：{other}")),
    };
    out.push(to_frame(desk.screen(), style, &mut prev)?);
    while out.len() < n {
        let mut moved = false;
        for _ in 0..12 {
            if desk.step(action).map_err(err)? {
                moved = true;
                break;
            }
        }
        if !moved {
            return Err(format!("{class} 连走 12 步画面都没变"));
        }
        out.push(to_frame(desk.screen(), style, &mut prev)?);
    }
    Ok(out)
}

/// 读一个目录里的 PNG（按文件名排序，RGB 或 RGBA、8 位），每张是一帧；变化面积相对前一张算。
fn read_pngs(dir: &Path) -> Res<(Vec<Frame>, (u16, u16))> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("读目录 {} 失败：{e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "png"))
        .collect();
    names.sort();
    let mut prev = None;
    let mut frames = Vec::new();
    let mut size = (0u16, 0u16);
    for path in &names {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let mut reader = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .map_err(|e| e.to_string())?;
        let mut buf = vec![0; reader.output_buffer_size().ok_or("图片太大")?];
        let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
        let step = match info.color_type {
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
            png::ColorType::Grayscale
            | png::ColorType::Indexed
            | png::ColorType::GrayscaleAlpha => return Err("只收 RGB、RGBA 的 PNG".to_owned()),
        };
        let px: Vec<u32> = buf
            .chunks_exact(step)
            .map(|c| {
                let at = |i: usize| u32::from(c.get(i).copied().unwrap_or(0));
                at(0) << 16 | at(1) << 8 | at(2)
            })
            .collect();
        let (w, h) = (
            u16::try_from(info.width).map_err(|e| e.to_string())?,
            u16::try_from(info.height).map_err(|e| e.to_string())?,
        );
        if size != (0, 0) && size != (w, h) {
            return Err("同一个目录里的图片尺寸不一样".to_owned());
        }
        size = (w, h);
        let screen = Screen::from_pixels(w, h, px).map_err(|e| e.to_string())?;
        frames.push(to_frame(&screen, 0, &mut prev)?);
    }
    Ok((frames, size))
}

/// 用一个新开的后端把 `frames` 依次编掉，第 i 帧按 `qps[i]` 编。
fn encode_run(frames: &[Frame], size: (u16, u16), qps: &[u8], forced: &[usize]) -> Res<Vec<Out>> {
    let mut backend = OpenH264Backend::new();
    let rates = Rates {
        bps: 8_000_000,
        fps: 30,
    };
    let _ = backend
        .reconfigure(size, rates)
        .map_err(|e| format!("配置后端失败：{e:?}"))?;
    let mut outs = Vec::with_capacity(frames.len());
    let mut prev_qp = None;
    for (i, frame) in frames.iter().enumerate() {
        let qp = qps.get(i).copied().ok_or("QP 列表比帧数短")?;
        let asked_key = forced.contains(&i);
        let ask = FrameAsk {
            token: u64::try_from(i).unwrap_or(0),
            budget: usize::MAX / 2,
            qp: Some(qp),
            keyframe: asked_key,
        };
        let input = FrameInput::I420 {
            y: &frame.y,
            u: &frame.u,
            v: &frame.v,
        };
        let started = Instant::now();
        let done = backend
            .submit(input, ask)
            .map_err(|e| format!("第 {i} 帧编码失败：{e:?}"))?;
        let us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        match done {
            Submitted::Ready(Finished::Frame(f)) => {
                outs.push(Out {
                    asked_key,
                    got_key: f.key,
                    bytes: f.data.len(),
                    us,
                    prev_qp,
                });
            }
            Submitted::Ready(Finished::Dropped { .. }) | Submitted::Pending => {
                return Err(format!("第 {i} 帧没有立刻编出来"));
            }
        }
        prev_qp = Some(qp);
    }
    Ok(outs)
}

/// 一次任务里写 CSV 要用到的不变部分。
struct Rows<'a> {
    w: &'a mut BufWriter<File>,
    name: String,
    class: &'a str,
    split: &'a str,
    size: (u16, u16),
}

impl Rows<'_> {
    fn write(&mut self, frames: &[Frame], qps: &[u8], outs: &[Out]) -> Res<()> {
        for (i, (frame, out)) in frames.iter().zip(outs).enumerate() {
            let qp = qps.get(i).copied().unwrap_or(0);
            let prev = out
                .prev_qp
                .map_or_else(|| "-".to_owned(), |p| p.to_string());
            writeln!(
                self.w,
                "openh264,{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                self.name,
                self.class,
                self.split,
                self.size.0,
                self.size.1,
                i,
                qp,
                u8::from(out.asked_key),
                u8::from(out.got_key),
                out.bytes,
                frame.changed,
                frame.detail,
                prev,
                out.us
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// 一组 QP 逐个跑；第一个 QP 编两遍自检（同输入同输出）。
fn run_qps(
    rows: &mut Rows<'_>,
    frames: &[Frame],
    per_frame_qps: &[Vec<u8>],
    forced: &[usize],
) -> Res<()> {
    for (k, qps) in per_frame_qps.iter().enumerate() {
        let outs = encode_run(frames, rows.size, qps, forced)?;
        if k == 0 {
            let again = encode_run(frames, rows.size, qps, forced)?;
            let same = outs
                .iter()
                .zip(&again)
                .all(|(a, b)| (a.bytes, a.got_key) == (b.bytes, b.got_key));
            if !same || outs.len() != again.len() {
                return Err(format!("自检失败：{} 同输入两次输出不同", rows.name));
            }
        }
        rows.write(frames, qps, &outs)?;
    }
    Ok(())
}

fn run_job(job: &Job, out: &mut BufWriter<File>) -> Res<()> {
    match job {
        Job::Seq {
            split,
            class,
            seed,
            size,
            frames,
            qps,
            forced,
        } => {
            let fr = make_frames(class, *seed, *size, *frames)?;
            let mut rows = Rows {
                w: out,
                name: format!("{class}-s{seed}-{}x{}", size.0, size.1),
                class,
                split,
                size: *size,
            };
            let plan: Vec<Vec<u8>> = qps.iter().map(|q| vec![*q; fr.len()]).collect();
            run_qps(&mut rows, &fr, &plan, forced)
        }
        Job::Refine {
            split,
            seed,
            size,
            chains,
        } => {
            // 起始画面：种子决定窗口叠放和文字多少；链上每一步编的是同一幅
            let mut desk =
                RealisticDesktop::new(size.0, size.1, *seed).map_err(|e| e.to_string())?;
            warm(&mut desk, *seed, "refine")?;
            for _ in 0..(seed % 3) * 30 {
                let _ = desk.step(Action::Scrolling).map_err(|e| e.to_string())?;
            }
            for chain in chains {
                let mut prev = None;
                let fr = chain
                    .iter()
                    .map(|_| to_frame(desk.screen(), style_of(*seed), &mut prev))
                    .collect::<Res<Vec<Frame>>>()?;
                let tag: Vec<String> = chain.iter().map(ToString::to_string).collect();
                let mut rows = Rows {
                    w: out,
                    name: format!("refine-s{seed}-{}x{}-{}", size.0, size.1, tag.join("_")),
                    class: "refine",
                    split,
                    size: *size,
                };
                run_qps(&mut rows, &fr, std::slice::from_ref(chain), &[])?;
            }
            Ok(())
        }
        Job::Pngseq {
            split,
            class,
            name,
            dir,
            qps,
            forced,
        } => {
            let (fr, size) = read_pngs(Path::new(dir))?;
            let mut rows = Rows {
                w: out,
                name: format!("{name}-{}x{}", size.0, size.1),
                class,
                split,
                size,
            };
            let plan: Vec<Vec<u8>> = qps.iter().map(|q| vec![*q; fr.len()]).collect();
            run_qps(&mut rows, &fr, &plan, forced)
        }
        Job::Scene {
            split,
            seed,
            size,
            qp,
            areas,
            noise,
        } => {
            let mut base =
                RealisticDesktop::new(size.0, size.1, *seed).map_err(|e| e.to_string())?;
            let _ = base.step(Action::Typing).map_err(|e| e.to_string())?;
            let mut donor = RealisticDesktop::new(size.0, size.1, seed.wrapping_add(777))
                .map_err(|e| e.to_string())?;
            let _ = donor.step(Action::Switching).map_err(|e| e.to_string())?;
            let donor_screen = if *noise {
                full_video(size.0, size.1, 0, &mut Rng(*seed ^ 0xf011))?
            } else {
                donor.screen().clone()
            };
            let mut rng = Rng(*seed ^ 0x5ce4e);
            for permille in areas {
                let (x, y, rw, rh) = rect_of(size.0, size.1, *permille, &mut rng);
                let mut prev = None;
                let mut shown = base.screen().clone();
                paste(&mut shown, &donor_screen, x, y, rw, rh);
                // 前一帧是桌面，后一帧换掉一块；之后再回到桌面，看“换回来”会不会也被当成场景切换
                let fr = [base.screen(), &shown, base.screen()]
                    .into_iter()
                    .map(|s| to_frame(s, 0, &mut prev))
                    .collect::<Res<Vec<Frame>>>()?;
                let mut rows = Rows {
                    w: out,
                    name: format!(
                        "scene{}-s{seed}-{}x{}-a{permille}",
                        if *noise { "v" } else { "" },
                        size.0,
                        size.1
                    ),
                    class: "scene",
                    split,
                    size: *size,
                };
                run_qps(&mut rows, &fr, &[vec![*qp; fr.len()]], &[])?;
            }
            Ok(())
        }
    }
}

/// 低 QP 地板实验的 CSV 表头。`chain` 是 `hold` 或 `desc`；`idx` 0 是先按 QP 30 编的那一帧。
const FLOOR_HEADER: &str = "seed,w,h,chain,q,idx,qp,bytes,got_key,psnr";

/// 解码后的亮度和原图亮度比，PSNR（分贝）；完全相同记 99。
fn luma_psnr(orig: &[u8], decoded: &[u8]) -> Option<f64> {
    if orig.len() != decoded.len() || orig.is_empty() {
        return None;
    }
    let sum: f64 = orig
        .iter()
        .zip(decoded)
        .map(|(a, b)| {
            let d = i32::from(*a) - i32::from(*b);
            f64::from(d * d)
        })
        .sum();
    let n = f64::from(u32::try_from(orig.len()).ok()?);
    let mse = sum / n;
    Some(if mse <= 0.0 {
        99.0
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    })
}

/// 同一幅画面依次按 `qps` 编，每帧解码后量亮度 PSNR。返回（字节数, 是不是关键帧, PSNR）。
fn floor_chain(
    frame: &Frame,
    size: (u16, u16),
    qps: &[u8],
) -> Res<Vec<(usize, bool, Option<f64>)>> {
    let mut backend = OpenH264Backend::new();
    let rates = Rates {
        bps: 8_000_000,
        fps: 30,
    };
    let _ = backend
        .reconfigure(size, rates)
        .map_err(|e| format!("配置后端失败：{e:?}"))?;
    let mut dec = H264Decoder::new().map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(qps.len());
    for (i, qp) in qps.iter().enumerate() {
        let ask = FrameAsk {
            token: u64::try_from(i).unwrap_or(0),
            budget: usize::MAX / 2,
            qp: Some(*qp),
            keyframe: false,
        };
        let input = FrameInput::I420 {
            y: &frame.y,
            u: &frame.u,
            v: &frame.v,
        };
        let Submitted::Ready(Finished::Frame(f)) = backend
            .submit(input, ask)
            .map_err(|e| format!("第 {i} 帧编码失败：{e:?}"))?
        else {
            return Err(format!("第 {i} 帧没有立刻编出来"));
        };
        let header = FrameHeader {
            codec: Codec::H264,
            key: f.key,
            width: size.0,
            height: size.1,
        };
        let psnr = match dec
            .decode_picture(&header, &f.data)
            .map_err(|e| e.to_string())?
        {
            Some(Picture::Nv12(nv)) => {
                let n = usize::from(size.0) * usize::from(size.1);
                nv.data().get(..n).and_then(|y| luma_psnr(&frame.y, y))
            }
            _ => None,
        };
        out.push((f.data.len(), f.key, psnr));
    }
    Ok(out)
}

/// `measure floor`：每个种子一幅 1080p 画面（和 refine 同一套摆法）。
/// 链 1（`hold`）：先按 QP 30 编一帧，再按同一个 q 连编 20 帧，q = 12…20；链 2（`desc`）：从 30 每帧降 1 到 12。
fn run_floor(args: &[String]) -> Res<()> {
    let arg = |i: usize| {
        args.get(i)
            .ok_or("用法：measure floor <输出目录> <第几份> <共几份>")
    };
    let dir = Path::new(arg(2)?);
    let (shard, shards): (u64, u64) = (num(arg(3)?, "第几份")?, num(arg(4)?, "共几份")?);
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = File::create(dir.join(format!("floor-{shard}.csv"))).map_err(|e| e.to_string())?;
    let mut w = BufWriter::new(file);
    writeln!(w, "{FLOOR_HEADER}").map_err(|e| e.to_string())?;
    let size = (1920u16, 1080u16);
    for seed in 0..12u64 {
        if seed % shards.max(1) != shard {
            continue;
        }
        let mut desk = RealisticDesktop::new(size.0, size.1, seed).map_err(|e| e.to_string())?;
        warm(&mut desk, seed, "refine")?;
        for _ in 0..(seed % 3) * 30 {
            let _ = desk.step(Action::Scrolling).map_err(|e| e.to_string())?;
        }
        let frame = to_frame(desk.screen(), style_of(seed), &mut None)?;
        let mut chains: Vec<(&str, u8, Vec<u8>)> = (12..=20u8)
            .map(|q| {
                let mut v = vec![30u8];
                v.extend(std::iter::repeat_n(q, 20));
                ("hold", q, v)
            })
            .collect();
        chains.push(("desc", 0, (12..=30u8).rev().collect()));
        for (name, q, qps) in chains {
            let rows = floor_chain(&frame, size, &qps)?;
            for (i, ((bytes, key, psnr), qp)) in rows.iter().zip(&qps).enumerate() {
                let p = psnr.map_or_else(|| "-".to_owned(), |p| format!("{p:.3}"));
                writeln!(
                    w,
                    "{seed},{},{},{name},{q},{i},{qp},{bytes},{},{p}",
                    size.0,
                    size.1,
                    u8::from(*key)
                )
                .map_err(|e| e.to_string())?;
            }
        }
        w.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn run() -> Res<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "floor") {
        return run_floor(&args);
    }
    let arg = |i: usize| {
        args.get(i)
            .ok_or("用法：measure <清单> <输出目录> <第几份> <共几份>")
    };
    let manifest = fs::read_to_string(arg(1)?).map_err(|e| format!("读清单失败：{e}"))?;
    let dir = Path::new(arg(2)?);
    let (shard, shards): (usize, usize) = (num(arg(3)?, "第几份")?, num(arg(4)?, "共几份")?);
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = File::create(dir.join(format!("shard-{shard}.csv"))).map_err(|e| e.to_string())?;
    let mut out = BufWriter::new(file);
    writeln!(out, "{HEADER}").map_err(|e| e.to_string())?;
    let started = Instant::now();
    let mut done = 0usize;
    let jobs = manifest
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'));
    for (i, line) in jobs.enumerate() {
        if i % shards.max(1) != shard {
            continue;
        }
        let job = parse_job(line)?;
        run_job(&job, &mut out).map_err(|e| format!("{e}（任务：{line}）"))?;
        out.flush().map_err(|e| e.to_string())?;
        done += 1;
        if done.is_multiple_of(10) {
            let mut err = std::io::stderr();
            let _ = writeln!(
                err,
                "份 {shard}：完成 {done} 个任务，用时 {} 秒",
                started.elapsed().as_secs()
            );
        }
    }
    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "份 {shard}：全部 {done} 个任务完成，用时 {} 秒",
        started.elapsed().as_secs()
    );
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(why) => {
            let _ = writeln!(std::io::stderr(), "失败：{why}");
            ExitCode::FAILURE
        }
    }
}
