//! 用真机导出的手势记录回放手势识别（`tests/data/` 里的记录，格式见文件开头）。
//! 喂法和 Android 界面一样：移动整个事件一起交（`moves`），按下、抬起、取消逐根交（`touch`），画面矩形变了调 `set_view`。
// 打印每个手势的识别结果，`--nocapture` 时看得到（排查真机问题用）。
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use peerdesk_core::input::InputEvent;
use peerdesk_mobile::gesture::{Action, Gestures, Phase, Touch, View};

/// 记录里的一行。
enum Line {
    /// 触摸事件：时间、动作、序号、每根手指 (编号, x, y)。
    Motion(u64, u8, usize, Vec<(u32, f32, f32)>),
    /// 画面矩形。
    Rect(View),
}

fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            match f[0] {
                "M" => {
                    let pts = f[4..]
                        .chunks(3)
                        .map(|c| {
                            (
                                c[0].parse().unwrap(),
                                c[1].parse().unwrap(),
                                c[2].parse().unwrap(),
                            )
                        })
                        .collect();
                    Line::Motion(
                        f[1].parse().unwrap(),
                        f[2].parse().unwrap(),
                        f[3].parse().unwrap(),
                        pts,
                    )
                }
                "R" => Line::Rect(View {
                    left: f[2].parse().unwrap(),
                    top: f[3].parse().unwrap(),
                    width: f[4].parse().unwrap(),
                    height: f[5].parse().unwrap(),
                }),
                other => panic!("不认识的行 {other}"),
            }
        })
        .collect()
}

/// 一次手势（第一根按下到最后一根抬起）里识别出来的东西。
#[derive(Default, Debug)]
struct Seen {
    start_ms: u64,
    fingers: usize,
    zooms: usize,
    wheels: usize,
    pans: usize,
    taps: usize,
    /// 两指从第二根按下到抬起：两指距离的变化、中点走过的距离（像素）。
    span: f32,
    travel: f32,
}

const SLOP: f32 = 8.0 * 3.0;
const NOTCH: f32 = 40.0 * 3.0;

fn replay(lines: &[Line]) -> Vec<Seen> {
    let mut g = Gestures::new(
        View {
            left: 0.0,
            top: 0.0,
            width: 1080.0,
            height: 675.0,
        },
        SLOP,
        NOTCH,
    );
    let mut out: Vec<Seen> = Vec::new();
    let mut two: Option<[(f32, f32); 2]> = None;
    for line in lines {
        match line {
            Line::Rect(v) => g.set_view(*v),
            Line::Motion(t, a, i, pts) => {
                let mut acts = g.tick(*t);
                let at = |k: usize, phase| Touch {
                    id: pts[k].0,
                    phase,
                    x: pts[k].1,
                    y: pts[k].2,
                    t_ms: *t,
                };
                match a {
                    0 => {
                        out.push(Seen {
                            start_ms: *t,
                            fingers: 1,
                            ..Seen::default()
                        });
                        two = None;
                        // 界面：新手势的第一根手指（ACTION_DOWN）先把留下的状态清掉
                        acts.extend(g.reset());
                        acts.extend(g.touch(&at(*i, Phase::Down)));
                    }
                    5 => acts.extend(g.touch(&at(*i, Phase::Down))),
                    1 | 6 => acts.extend(g.touch(&at(*i, Phase::Up))),
                    2 => {
                        let touches: Vec<Touch> =
                            (0..pts.len()).map(|k| at(k, Phase::Move)).collect();
                        acts.extend(g.moves(&touches));
                    }
                    3 => {
                        for k in 0..pts.len() {
                            acts.extend(g.touch(&at(k, Phase::Cancel)));
                        }
                    }
                    _ => {}
                }
                if let Some(s) = out.last_mut() {
                    s.fingers = s.fingers.max(pts.len());
                    if pts.len() == 2 {
                        let p = [(pts[0].1, pts[0].2), (pts[1].1, pts[1].2)];
                        let a0 = *two.get_or_insert(p);
                        let d = |p: [(f32, f32); 2]| (p[0].0 - p[1].0).hypot(p[0].1 - p[1].1);
                        let m =
                            |p: [(f32, f32); 2]| ((p[0].0 + p[1].0) / 2.0, (p[0].1 + p[1].1) / 2.0);
                        s.span = d(p) - d(a0);
                        s.travel = (m(p).0 - m(a0).0).hypot(m(p).1 - m(a0).1);
                    }
                    for act in &acts {
                        match act {
                            Action::Zoom { .. } => s.zooms += 1,
                            Action::Pan { .. } => s.pans += 1,
                            Action::Tap { .. } => s.taps += 1,
                            Action::Input(InputEvent::Wheel { .. }) => s.wheels += 1,
                            Action::Input(_) | Action::Haptic => {}
                        }
                    }
                }
            }
        }
    }
    out
}

fn print(seen: &[Seen]) {
    for s in seen {
        println!(
            "{:6.2}s p{} span {:+5.0} travel {:4.0} zoom {:3} wheel {:3} pan {:3} tap {}",
            s.start_ms as f32 / 1000.0,
            s.fingers,
            s.span,
            s.travel,
            s.zooms,
            s.wheels,
            s.pans,
            s.taps
        );
    }
}

/// 用户的意图从手指本身看得出来：中点走得远、两指间距基本不变（走的超过间距变化的 3 倍）是滚动；
/// 间距变化大、中点不怎么动（变化超过走的 2 倍）是捏合。滚动不能缩放、要滚；捏合不能滚、要缩放。
/// 返回（滚动次数，捏合次数）。
fn check(seen: &[Seen]) -> (usize, usize) {
    let two: Vec<&Seen> = seen.iter().filter(|s| s.fingers == 2).collect();
    let scrolls: Vec<&&Seen> = two
        .iter()
        .filter(|s| s.travel > 200.0 && s.travel > s.span.abs() * 3.0)
        .collect();
    let pinches: Vec<&&Seen> = two
        .iter()
        .filter(|s| s.span.abs() > 200.0 && s.span.abs() > s.travel * 2.0)
        .collect();
    let bad_scroll: Vec<_> = scrolls
        .iter()
        .filter(|s| s.zooms > 0 || s.wheels == 0)
        .map(|s| s.start_ms)
        .collect();
    let bad_pinch: Vec<_> = pinches
        .iter()
        .filter(|s| s.wheels > 0 || s.zooms == 0)
        .map(|s| s.start_ms)
        .collect();
    println!(
        "滚动 {} 次，判错 {:?}；捏合 {} 次，判错 {:?}",
        scrolls.len(),
        bad_scroll,
        pinches.len(),
        bad_pinch
    );
    assert!(
        bad_scroll.is_empty(),
        "滚动被当成了缩放（或者没滚）：{bad_scroll:?}"
    );
    assert!(
        bad_pinch.is_empty(),
        "捏合被当成了滚动（或者没缩放）：{bad_pinch:?}"
    );
    (scrolls.len(), pinches.len())
}

/// 第一份（11-28 9 点）：三指被系统取消后卡死、双指滚动被当成缩放。
#[test]
fn the_real_trace_from_2026_11_28() {
    let seen = replay(&parse(include_str!(
        "data/trace-2026-11-28-pinch-scroll.txt"
    )));
    print(&seen);
    let (scrolls, pinches) = check(&seen);
    assert!(scrolls >= 20 && pinches >= 15, "记录里的手势数不对");
}

/// 第二份：两指上下叠着快速上滑，领先的手指 35 毫秒走了 97 像素、另一根才开始动（14.72 秒那次）。
#[test]
fn the_second_trace_from_2026_11_28() {
    let seen = replay(&parse(include_str!("data/trace-2026-11-28-scroll-end.txt")));
    print(&seen);
    let (scrolls, pinches) = check(&seen);
    assert!(scrolls >= 2 && pinches >= 4, "记录里的手势数不对");
}
