//! 触控到鼠标、键盘事件的状态机（纯逻辑，Android 和 iOS 共用）。
//!
//! 界面只转发原始触点（编号、按下/移动/抬起、坐标、时间），并在有手指按着时每隔几十毫秒调一次 [`Gestures::tick`]
//! （长按要靠时间判断，手指不动时系统不一定发移动事件）。这里决定它是点击、拖拽、滚动还是缩放。
//!
//! 手势表（2026-11-28 按产品负责人用 ToDesk 的习惯定）：
//!
//! | 手势 | 效果 |
//! |---|---|
//! | 单指点 | 在手指处左键单击 |
//! | 单指滑动 | 移动画面（平移） |
//! | 长按（震一下）后松开 | 在手指处右键单击 |
//! | 长按（震一下）后拖动 | 从手指处按住左键拖拽 |
//! | 双指点 | 在两指中点右键单击 |
//! | 双指滑动 | 滚动页面（先把指针移到两指中点） |
//! | 双指捏合 | 缩放画面 |
//!
//! 精细操作用屏幕上的虚拟鼠标（界面画）：平时是一只可以拖到任意位置的小鼠标，点它变成大鼠标，大鼠标右上角关掉
//! 又变回小鼠标。大鼠标头上的箭头尖就是指针：拖住身体移动指针，按住左键拖就是拖拽、选中文字，右键就是右键。
//! 界面只要把箭头尖的位置交给 [`Gestures::point`]、按键交给 [`Gestures::button`]。大鼠标以外照常是上面的触屏手势。
//!
//! 滚动是“自然方向”：手指往下，内容跟着往下（等于滚轮向上）。三指及以上不处理，留给系统。

use peerdesk_core::input::InputEvent;

/// 点击最长多久（毫秒）：按下到抬起超过它就不算点击。
pub const TAP_MS: u64 = 250;
/// 长按多久（毫秒）：到点震一下，之后松开是右键、拖动是拖拽。
pub const LONG_PRESS_MS: u64 = 500;
/// 双指点：第二根手指要在第一根按下后这么久内按下（毫秒）。
pub const TWO_FINGER_GAP_MS: u64 = 150;
/// 双指点：从第一根按下到第一根抬起最长多久（毫秒）。
pub const TWO_FINGER_TAP_MS: u64 = 300;
/// 两指距离变化超过这个比例就当作捏合。
pub const PINCH_START: f32 = 0.06;
/// 滚轮一格的高精度增量（和桌面端、Windows 的 `WHEEL_DELTA` 一样）。
const NOTCH: f32 = 120.0;
/// 鼠标左键、右键的编号（见 [`InputEvent::MouseButton`]）。
pub const LEFT: u8 = 0;
/// 见 [`LEFT`]。
pub const RIGHT: u8 = 1;

/// 触点的阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// 按下。
    Down,
    /// 移动。
    Move,
    /// 抬起。
    Up,
    /// 系统取消（例如来电、手势被系统拿走）：按抬起处理，不触发点击。
    Cancel,
}

impl Phase {
    /// 界面传来的编号：0 按下、1 移动、2 抬起、3 取消；别的数是 `None`。
    #[must_use]
    pub const fn from_code(code: i32) -> Option<Self> {
        match code {
            0 => Some(Self::Down),
            1 => Some(Self::Move),
            2 => Some(Self::Up),
            3 => Some(Self::Cancel),
            _ => None,
        }
    }
}

/// 一个原始触点。坐标是界面上的像素。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    /// 手指编号（同一根手指按下到抬起不变）。
    pub id: u32,
    /// 阶段。
    pub phase: Phase,
    /// 横坐标（像素）。
    pub x: f32,
    /// 纵坐标（像素）。
    pub y: f32,
    /// 时间（毫秒，单调递增即可）。
    pub t_ms: u64,
}

/// 远程画面现在画在界面的哪里（像素，已经算上缩放和平移）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// 画面左边。
    pub left: f32,
    /// 画面上边。
    pub top: f32,
    /// 画面宽。
    pub width: f32,
    /// 画面高。
    pub height: f32,
}

/// 状态机的输出。
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// 发给被控端的输入。
    Input(InputEvent),
    /// 缩放画面：相对上一次的倍数，缩放中心（界面像素）。界面照做，再用新的 [`View`] 调 [`Gestures::set_view`]。
    Zoom {
        /// 倍数（大于 1 放大）。
        factor: f32,
        /// 中心横坐标。
        cx: f32,
        /// 中心纵坐标。
        cy: f32,
    },
    /// 平移画面（界面像素）。
    Pan {
        /// 横向。
        dx: f32,
        /// 纵向。
        dy: f32,
    },
    /// 长按到点了：界面震一下，告诉用户“现在松开是右键、拖动是拖拽”。
    Haptic,
}

/// 一根手指：编号、按下时的位置和时间、上一次的位置。
#[derive(Debug, Clone, Copy, PartialEq)]
struct Finger {
    id: u32,
    start: (f32, f32),
    last: (f32, f32),
    down_ms: u64,
}

impl Finger {
    const fn new(t: &Touch) -> Self {
        Self {
            id: t.id,
            start: (t.x, t.y),
            last: (t.x, t.y),
            down_ms: t.t_ms,
        }
    }

    fn moved_from_start(&self) -> f32 {
        distance(self.start, self.last)
    }
}

/// 两根手指在做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Two {
    /// 还没定（可能是双指点）。
    Undecided,
    /// 滚动。
    Scroll,
    /// 捏合缩放。
    Pinch,
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    /// 没有手指。
    Idle,
    /// 一根手指，还没到长按。`moved` 是超过了防抖距离（之后就是滑动，不再算点击和长按）。
    One { f: Finger, moved: bool },
    /// 长按到点了、手指还没动：松开是右键，动了就开始拖拽。
    Held { f: Finger },
    /// 拖拽中：左键按着，跟着手指走。
    Drag { f: Finger },
    /// 两根手指。`dist` 是上一次两指的距离，`scroll` 是没发出去的滚动余数（滚轮单位）。
    Two {
        a: Finger,
        b: Finger,
        what: Two,
        dist: f32,
        scroll: (f32, f32),
    },
    /// 手势已经结束（例如双指点完了），等剩下的手指都抬起，期间什么也不发。
    Done { left: usize },
}

/// 触控状态机。
#[derive(Debug, Clone)]
pub struct Gestures {
    view: View,
    /// 防抖距离（像素）：手指移动不超过它还算“没动”。界面按屏幕密度给（大约 8 dp）。
    slop: f32,
    /// 滚轮一格对应手指移动多少像素。
    notch_px: f32,
    state: State,
}

impl Gestures {
    /// 新建。`slop_px` 是防抖距离（像素），`notch_px` 是滚一格要移动多少像素。
    #[must_use]
    pub fn new(view: View, slop_px: f32, notch_px: f32) -> Self {
        Self {
            view,
            slop: slop_px.max(1.0),
            notch_px: notch_px.max(1.0),
            state: State::Idle,
        }
    }

    /// 画面位置变了（缩放、平移、横竖屏）。
    pub const fn set_view(&mut self, view: View) {
        self.view = view;
    }

    /// 大鼠标的箭头尖到了界面上的 (`x`, `y`)（像素，和触点同一个坐标系）：把被控端的指针移过去。
    /// 拖鼠标身体、按住左键拖、按下按键之前、画面缩放平移之后，界面都调一次。
    #[must_use]
    pub fn point(&self, x: f32, y: f32) -> Vec<Action> {
        vec![Action::Input(self.pointer_at((x, y)))]
    }

    /// 大鼠标的左键、右键按下或抬起，按在指针现在的位置（界面先调 [`Self::point`]）。
    #[must_use]
    pub fn button(&self, button: u8, down: bool) -> Vec<Action> {
        vec![button_action(button, down)]
    }

    /// 处理一个触点。
    pub fn touch(&mut self, t: &Touch) -> Vec<Action> {
        let mut out = Vec::new();
        let state = std::mem::replace(&mut self.state, State::Idle);
        self.state = match (state, t.phase) {
            (State::Idle, Phase::Down) => State::One {
                f: Finger::new(t),
                moved: false,
            },
            // 没按着就来的移动、抬起（例如上一个手势中途换了模式）：忽略
            (State::Idle, Phase::Move | Phase::Up | Phase::Cancel) => State::Idle,
            (State::One { f, moved }, phase) => self.one(f, moved, t, phase, &mut out),
            (State::Held { f }, phase) => self.held(f, t, phase, &mut out),
            (State::Drag { f }, phase) => self.drag(f, t, phase, &mut out),
            (
                State::Two {
                    a,
                    b,
                    what,
                    dist,
                    scroll,
                },
                phase,
            ) => self.two(a, b, what, dist, scroll, t, phase, &mut out),
            (State::Done { left }, Phase::Down) => State::Done {
                left: left.saturating_add(1),
            },
            (State::Done { left }, Phase::Move) => State::Done { left },
            (State::Done { left }, Phase::Up | Phase::Cancel) => match left.saturating_sub(1) {
                0 => State::Idle,
                left => State::Done { left },
            },
        };
        out
    }

    /// 时间到了：手指按着不动够久就震一下（之后松开是右键、拖动是拖拽）。有手指按着时界面每隔几十毫秒调一次。
    pub fn tick(&mut self, now_ms: u64) -> Vec<Action> {
        if let State::One { f, moved: false } = self.state {
            if now_ms.saturating_sub(f.down_ms) >= LONG_PRESS_MS {
                self.state = State::Held { f };
                return vec![Action::Haptic];
            }
        }
        Vec::new()
    }

    fn one(
        &mut self,
        mut f: Finger,
        mut moved: bool,
        t: &Touch,
        phase: Phase,
        out: &mut Vec<Action>,
    ) -> State {
        if t.id != f.id {
            if phase == Phase::Down && t.t_ms.saturating_sub(f.down_ms) <= LONG_PRESS_MS {
                // 第二根手指：进入双指
                let b = Finger::new(t);
                return State::Two {
                    a: f,
                    b,
                    what: Two::Undecided,
                    dist: distance(f.last, b.last),
                    scroll: (0.0, 0.0),
                };
            }
            return State::One { f, moved };
        }
        match phase {
            Phase::Down => State::One { f, moved },
            Phase::Move => {
                let prev = f.last;
                f.last = (t.x, t.y);
                if !moved && f.moved_from_start() > self.slop {
                    moved = true;
                }
                if moved {
                    Self::slide(prev, f.last, out);
                }
                State::One { f, moved }
            }
            Phase::Up => {
                if !moved && t.t_ms.saturating_sub(f.down_ms) <= TAP_MS {
                    self.click(LEFT, f.start, out);
                }
                State::Idle
            }
            Phase::Cancel => State::Idle,
        }
    }

    /// 单指滑动：平移画面。
    fn slide(prev: (f32, f32), now: (f32, f32), out: &mut Vec<Action>) {
        out.push(Action::Pan {
            dx: now.0 - prev.0,
            dy: now.1 - prev.1,
        });
    }

    /// 长按到点之后：松开是右键；动了（超过防抖距离）就从按下的地方开始拖拽。
    fn held(&mut self, mut f: Finger, t: &Touch, phase: Phase, out: &mut Vec<Action>) -> State {
        if t.id != f.id {
            return State::Held { f };
        }
        match phase {
            Phase::Down => State::Held { f },
            Phase::Move => {
                f.last = (t.x, t.y);
                if f.moved_from_start() <= self.slop {
                    return State::Held { f };
                }
                out.push(Action::Input(self.pointer_at(f.start)));
                out.push(button_action(LEFT, true));
                out.push(Action::Input(self.pointer_at(f.last)));
                State::Drag { f }
            }
            Phase::Up => {
                self.click(RIGHT, f.start, out);
                State::Idle
            }
            Phase::Cancel => State::Idle,
        }
    }

    fn drag(&mut self, mut f: Finger, t: &Touch, phase: Phase, out: &mut Vec<Action>) -> State {
        if t.id != f.id {
            // 拖拽中别的手指按下、抬起：不管（拖拽要一直进行到这根手指抬起）
            return State::Drag { f };
        }
        match phase {
            Phase::Down => State::Drag { f },
            Phase::Move => {
                f.last = (t.x, t.y);
                out.push(Action::Input(self.pointer_at(f.last)));
                State::Drag { f }
            }
            Phase::Up | Phase::Cancel => {
                out.push(button_action(LEFT, false));
                State::Idle
            }
        }
    }

    /// 在 `at`（界面像素）单击 `button`。
    fn click(&self, button: u8, at: (f32, f32), out: &mut Vec<Action>) {
        out.push(Action::Input(self.pointer_at(at)));
        out.push(button_action(button, true));
        out.push(button_action(button, false));
    }

    #[allow(clippy::too_many_arguments)]
    fn two(
        &mut self,
        mut a: Finger,
        mut b: Finger,
        mut what: Two,
        mut dist: f32,
        mut scroll: (f32, f32),
        t: &Touch,
        phase: Phase,
        out: &mut Vec<Action>,
    ) -> State {
        let which = if t.id == a.id {
            Some(true)
        } else if t.id == b.id {
            Some(false)
        } else {
            None
        };
        match (phase, which) {
            // 第三根手指：留给系统，这个手势作废（已经开始的滚动、缩放就停在那儿）
            (Phase::Down, None) => State::Done { left: 3 },
            (Phase::Down, Some(_)) | (Phase::Move | Phase::Up | Phase::Cancel, None) => {
                State::Two {
                    a,
                    b,
                    what,
                    dist,
                    scroll,
                }
            }
            (Phase::Move, Some(first)) => {
                let before = midpoint(a.last, b.last);
                if first {
                    a.last = (t.x, t.y);
                } else {
                    b.last = (t.x, t.y);
                }
                let after = midpoint(a.last, b.last);
                let now_dist = distance(a.last, b.last);
                if what == Two::Undecided {
                    let start_dist = distance(a.start, b.start).max(1.0);
                    if (now_dist / start_dist - 1.0).abs() > PINCH_START {
                        what = Two::Pinch;
                    } else if distance(midpoint(a.start, b.start), after) > self.slop {
                        what = Two::Scroll;
                        out.push(Action::Input(self.pointer_at(midpoint(a.start, b.start))));
                    }
                }
                match what {
                    Two::Undecided => {}
                    Two::Pinch => {
                        if dist > 0.0 && now_dist > 0.0 {
                            out.push(Action::Zoom {
                                factor: now_dist / dist,
                                cx: after.0,
                                cy: after.1,
                            });
                        }
                    }
                    Two::Scroll => {
                        // 自然方向：手指往下 = 滚轮向上（正）；手指往右 = 往左滚（负）
                        scroll.0 -= (after.0 - before.0) / self.notch_px * NOTCH;
                        scroll.1 += (after.1 - before.1) / self.notch_px * NOTCH;
                        let (dx, dy) = (whole(scroll.0), whole(scroll.1));
                        if dx != 0 || dy != 0 {
                            scroll.0 -= f32::from(dx);
                            scroll.1 -= f32::from(dy);
                            out.push(Action::Input(InputEvent::Wheel { dx, dy }));
                        }
                    }
                }
                dist = now_dist;
                State::Two {
                    a,
                    b,
                    what,
                    dist,
                    scroll,
                }
            }
            (Phase::Up | Phase::Cancel, Some(_)) => {
                let quick = t.t_ms.saturating_sub(a.down_ms) <= TWO_FINGER_TAP_MS
                    && b.down_ms.saturating_sub(a.down_ms) <= TWO_FINGER_GAP_MS;
                if phase == Phase::Up && what == Two::Undecided && quick {
                    self.click(RIGHT, midpoint(a.last, b.last), out);
                }
                State::Done { left: 1 }
            }
        }
    }

    /// 界面上的一点对应被控端屏幕的哪里（画面外的点贴到边上）。
    fn pointer_at(&self, p: (f32, f32)) -> InputEvent {
        let nx = (p.0 - self.view.left) / self.view.width.max(1.0);
        let ny = (p.1 - self.view.top) / self.view.height.max(1.0);
        InputEvent::MouseMove {
            x: unit(nx),
            y: unit(ny),
        }
    }
}

const fn button_action(button: u8, down: bool) -> Action {
    Action::Input(InputEvent::MouseButton { button, down })
}

fn distance(a: (f32, f32), b: (f32, f32)) -> f32 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn midpoint(a: (f32, f32), b: (f32, f32)) -> (f32, f32) {
    ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0)
}

/// 0..=1 的比例换成 0..=65535（被控端屏幕坐标）。
fn unit(v: f32) -> u16 {
    let scaled = (v.clamp(0.0, 1.0) * 65535.0).round();
    // 已经夹在 0..=65535 里，换算不会溢出
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let out = scaled as u16;
    out
}

/// 滚动余数里的整数部分（向零取整），夹在 i16 里。
fn whole(v: f32) -> i16 {
    let t = v.trunc().clamp(f32::from(i16::MIN), f32::from(i16::MAX));
    // 已经夹在 i16 的范围里
    #[allow(clippy::cast_possible_truncation)]
    let out = t as i16;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: View = View {
        left: 0.0,
        top: 100.0,
        width: 1000.0,
        height: 500.0,
    };

    fn g() -> Gestures {
        Gestures::new(VIEW, 10.0, 40.0)
    }

    fn t(id: u32, phase: Phase, x: f32, y: f32, t_ms: u64) -> Touch {
        Touch {
            id,
            phase,
            x,
            y,
            t_ms,
        }
    }

    fn mv(x: u16, y: u16) -> Action {
        Action::Input(InputEvent::MouseMove { x, y })
    }

    fn btn(button: u8, down: bool) -> Action {
        Action::Input(InputEvent::MouseButton { button, down })
    }

    #[test]
    fn a_tap_clicks_where_the_finger_is() {
        let mut s = g();
        assert!(s.touch(&t(1, Phase::Down, 500.0, 350.0, 0)).is_empty());
        // 抖一点不算动
        assert!(s.touch(&t(1, Phase::Move, 504.0, 352.0, 50)).is_empty());
        let out = s.touch(&t(1, Phase::Up, 504.0, 352.0, 120));
        assert_eq!(
            out,
            vec![mv(32768, 32768), btn(LEFT, true), btn(LEFT, false)]
        );
        // 画面外的点贴到边上
        s.touch(&t(2, Phase::Down, -50.0, 50.0, 1000));
        assert_eq!(
            s.touch(&t(2, Phase::Up, -50.0, 50.0, 1100)).first(),
            Some(&mv(0, 0))
        );
        // 慢慢松开（没到长按）不算点击
        s.touch(&t(1, Phase::Down, 500.0, 350.0, 2000));
        assert!(s
            .touch(&t(1, Phase::Up, 500.0, 350.0, 2000 + TAP_MS + 1))
            .is_empty());
    }

    #[test]
    fn one_finger_slides_the_picture_in_touch_mode() {
        let mut s = g();
        s.touch(&t(1, Phase::Down, 100.0, 200.0, 0));
        assert!(
            s.touch(&t(1, Phase::Move, 105.0, 200.0, 10)).is_empty(),
            "没超过防抖距离"
        );
        assert_eq!(
            s.touch(&t(1, Phase::Move, 130.0, 180.0, 20)),
            vec![Action::Pan {
                dx: 25.0,
                dy: -20.0
            }]
        );
        assert_eq!(
            s.touch(&t(1, Phase::Move, 140.0, 180.0, 30)),
            vec![Action::Pan { dx: 10.0, dy: 0.0 }]
        );
        // 滑过之后抬起不点击，也不会再变成长按
        assert!(s.tick(30 + LONG_PRESS_MS).is_empty());
        assert!(s.touch(&t(1, Phase::Up, 140.0, 180.0, 40)).is_empty());
        // 系统取消：什么也不发
        s.touch(&t(1, Phase::Down, 100.0, 200.0, 100));
        assert!(s.touch(&t(1, Phase::Cancel, 100.0, 200.0, 110)).is_empty());
        assert!(
            s.touch(&t(1, Phase::Up, 100.0, 200.0, 120)).is_empty(),
            "已经回到空闲"
        );
    }

    #[test]
    fn a_long_press_buzzes_then_right_clicks_or_drags() {
        let mut s = g();
        // 长按后松开：右键（在手指处）
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 0));
        assert!(s.tick(LONG_PRESS_MS - 1).is_empty());
        assert_eq!(s.tick(LONG_PRESS_MS), vec![Action::Haptic]);
        assert!(s.tick(LONG_PRESS_MS + 100).is_empty(), "只震一次");
        assert!(
            s.touch(&t(1, Phase::Move, 4.0, 100.0, 600)).is_empty(),
            "抖一点还是长按"
        );
        assert!(
            s.touch(&t(2, Phase::Down, 9.0, 9.0, 610)).is_empty(),
            "别的手指不管"
        );
        assert!(s.touch(&t(1, Phase::Down, 0.0, 100.0, 620)).is_empty());
        let out = s.touch(&t(1, Phase::Up, 4.0, 100.0, 700));
        assert_eq!(out, vec![mv(0, 0), btn(RIGHT, true), btn(RIGHT, false)]);
        // 长按后拖动：从按下的地方按住左键，跟着手指走，抬起松开
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 1000));
        s.tick(1000 + LONG_PRESS_MS);
        let out = s.touch(&t(1, Phase::Move, 500.0, 350.0, 1600));
        assert_eq!(out, vec![mv(0, 0), btn(LEFT, true), mv(32768, 32768)]);
        assert_eq!(
            s.touch(&t(1, Phase::Move, 1000.0, 600.0, 1700)),
            vec![mv(65535, 65535)]
        );
        // 拖拽中别的手指不打断
        assert!(s.touch(&t(2, Phase::Down, 10.0, 10.0, 1710)).is_empty());
        assert!(s.touch(&t(2, Phase::Up, 10.0, 10.0, 1720)).is_empty());
        assert!(s.touch(&t(1, Phase::Down, 0.0, 0.0, 1730)).is_empty());
        assert_eq!(
            s.touch(&t(1, Phase::Up, 1000.0, 600.0, 1800)),
            vec![btn(LEFT, false)]
        );
        // 拖拽中被系统取消：也要松开左键，不能让被控端一直按着
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 2000));
        s.tick(2000 + LONG_PRESS_MS);
        s.touch(&t(1, Phase::Move, 100.0, 100.0, 2600));
        assert_eq!(
            s.touch(&t(1, Phase::Cancel, 100.0, 100.0, 2700)),
            vec![btn(LEFT, false)]
        );
        // 长按后被系统取消：不右键
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 3000));
        s.tick(3000 + LONG_PRESS_MS);
        assert!(s.touch(&t(1, Phase::Cancel, 0.0, 100.0, 3600)).is_empty());
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn two_fingers_tap_for_a_right_click_at_their_middle() {
        let mut s = g();
        s.touch(&t(1, Phase::Down, 400.0, 350.0, 0));
        assert!(s.touch(&t(2, Phase::Down, 600.0, 350.0, 40)).is_empty());
        let out = s.touch(&t(1, Phase::Up, 400.0, 350.0, 150));
        assert_eq!(
            out,
            vec![mv(32768, 32768), btn(RIGHT, true), btn(RIGHT, false)]
        );
        // 剩下那根抬起时什么也不发
        assert!(s.touch(&t(2, Phase::Up, 600.0, 350.0, 160)).is_empty());
        // 太慢不算
        s.touch(&t(1, Phase::Down, 400.0, 350.0, 1000));
        s.touch(&t(2, Phase::Down, 600.0, 350.0, 1040));
        assert!(s
            .touch(&t(1, Phase::Up, 400.0, 350.0, 1000 + TWO_FINGER_TAP_MS + 1))
            .is_empty());
        s.touch(&t(2, Phase::Up, 600.0, 350.0, 1400));
        // 第二根手指按得太晚：不算双指点
        s.touch(&t(1, Phase::Down, 400.0, 350.0, 2000));
        s.touch(&t(
            2,
            Phase::Down,
            600.0,
            350.0,
            2000 + TWO_FINGER_GAP_MS + 1,
        ));
        assert!(s.touch(&t(1, Phase::Up, 400.0, 350.0, 2200)).is_empty());
        s.touch(&t(2, Phase::Up, 600.0, 350.0, 2210));
        // 第二根手指在长按时间之后才来：不进入双指，别的手指的移动也不算
        s.touch(&t(1, Phase::Down, 400.0, 350.0, 3000));
        assert!(s
            .touch(&t(2, Phase::Down, 600.0, 350.0, 3000 + LONG_PRESS_MS + 1))
            .is_empty());
        assert!(s.touch(&t(2, Phase::Move, 650.0, 350.0, 3600)).is_empty());
        s.touch(&t(1, Phase::Up, 400.0, 350.0, 3700));
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn two_fingers_scroll_naturally_and_keep_the_remainder() {
        let mut s = g();
        s.touch(&t(1, Phase::Down, 400.0, 300.0, 0));
        s.touch(&t(2, Phase::Down, 600.0, 300.0, 10));
        // 一根手指往下 30：中点往下 15，超过防抖距离，开始滚动。先把光标放到起点的中点，再滚（15/40 格向上）
        let out = s.touch(&t(1, Phase::Move, 400.0, 330.0, 20));
        assert_eq!(
            out,
            vec![
                mv(32768, 26214),
                Action::Input(InputEvent::Wheel { dx: 0, dy: 45 })
            ]
        );
        let out = s.touch(&t(2, Phase::Move, 600.0, 330.0, 30));
        assert_eq!(
            out,
            vec![Action::Input(InputEvent::Wheel { dx: 0, dy: 45 })]
        );
        // 往右划：往左滚（负）
        s.touch(&t(1, Phase::Move, 440.0, 330.0, 40));
        let out = s.touch(&t(2, Phase::Move, 640.0, 330.0, 50));
        assert_eq!(
            out,
            vec![Action::Input(InputEvent::Wheel { dx: -60, dy: 0 })]
        );
        // 小于一个单位的位移攒着
        assert!(s.touch(&t(1, Phase::Move, 440.0, 330.2, 60)).is_empty());
        // 滚动之后抬起：不右键
        assert!(s.touch(&t(1, Phase::Up, 440.0, 330.0, 70)).is_empty());
        assert!(s.touch(&t(2, Phase::Up, 640.0, 330.0, 80)).is_empty());
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn pinching_zooms_around_the_fingers() {
        let mut s = g();
        s.touch(&t(1, Phase::Down, 400.0, 300.0, 0));
        s.touch(&t(2, Phase::Down, 600.0, 300.0, 10));
        // 距离 200 → 240：超过 6%，算捏合
        let out = s.touch(&t(2, Phase::Move, 640.0, 300.0, 20));
        assert_eq!(
            out,
            vec![Action::Zoom {
                factor: 1.2,
                cx: 520.0,
                cy: 300.0
            }]
        );
        // 之后每次相对上一次
        let out = s.touch(&t(1, Phase::Move, 340.0, 300.0, 30));
        assert_eq!(
            out,
            vec![Action::Zoom {
                factor: 1.25,
                cx: 490.0,
                cy: 300.0
            }]
        );
        // 第三根手指：作废，等三根都抬起
        assert!(s.touch(&t(3, Phase::Down, 10.0, 10.0, 40)).is_empty());
        assert!(s.touch(&t(3, Phase::Move, 20.0, 10.0, 50)).is_empty());
        for id in [1, 2] {
            assert!(s.touch(&t(id, Phase::Up, 0.0, 0.0, 60)).is_empty());
            assert_ne!(s.state, State::Idle);
        }
        s.touch(&t(3, Phase::Up, 0.0, 0.0, 70));
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn the_big_mouse_points_and_presses() {
        let s = g();
        // 箭头尖在画面正中、画面外贴边
        assert_eq!(s.point(500.0, 350.0), vec![mv(32768, 32768)]);
        assert_eq!(s.point(-10.0, 9999.0), vec![mv(0, 65535)]);
        // 左键、右键按下抬起：只发按键，位置由 point 先放好
        assert_eq!(s.button(LEFT, true), vec![btn(LEFT, true)]);
        assert_eq!(s.button(LEFT, false), vec![btn(LEFT, false)]);
        assert_eq!(s.button(RIGHT, true), vec![btn(RIGHT, true)]);
    }

    #[test]
    fn codes_from_the_ui_are_read() {
        assert_eq!(Phase::from_code(0), Some(Phase::Down));
        assert_eq!(Phase::from_code(1), Some(Phase::Move));
        assert_eq!(Phase::from_code(2), Some(Phase::Up));
        assert_eq!(Phase::from_code(3), Some(Phase::Cancel));
        assert_eq!(Phase::from_code(4), None);
    }

    #[test]
    fn stray_events_are_ignored() {
        let mut s = g();
        // 没按下就来的移动、抬起、取消
        for phase in [Phase::Move, Phase::Up, Phase::Cancel] {
            assert!(s.touch(&t(1, phase, 0.0, 0.0, 0)).is_empty());
        }
        // 同一根手指重复按下、别的手指在单指等待时抬起
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 0));
        assert!(s.touch(&t(1, Phase::Down, 0.0, 100.0, 10)).is_empty());
        assert!(s.touch(&t(9, Phase::Up, 0.0, 100.0, 20)).is_empty());
        // 双指时重复按下、第三根手指的抬起
        s.touch(&t(2, Phase::Down, 100.0, 100.0, 30));
        assert!(s.touch(&t(2, Phase::Down, 100.0, 100.0, 40)).is_empty());
        assert!(s.touch(&t(7, Phase::Up, 100.0, 100.0, 50)).is_empty());
        // 双指时取消：不右键
        assert!(s.touch(&t(1, Phase::Cancel, 0.0, 100.0, 60)).is_empty());
        // 剩一根：再按一根也只是等它们都抬起
        assert!(s.touch(&t(5, Phase::Down, 0.0, 100.0, 70)).is_empty());
        s.touch(&t(2, Phase::Up, 0.0, 100.0, 80));
        s.touch(&t(5, Phase::Up, 0.0, 100.0, 90));
        assert_eq!(s.state, State::Idle);
        // 换画面位置之后，点击按新位置换算
        s.set_view(View {
            left: 100.0,
            top: 0.0,
            width: 200.0,
            height: 100.0,
        });
        s.touch(&t(1, Phase::Down, 300.0, 100.0, 1000));
        assert_eq!(
            s.touch(&t(1, Phase::Up, 300.0, 100.0, 1050)).first(),
            Some(&mv(65535, 65535))
        );
        // 工具函数的边界
        assert_eq!(whole(1e9), i16::MAX);
        assert_eq!(whole(-1e9), i16::MIN);
        assert_eq!(unit(2.0), 65535);
        assert_eq!(unit(-1.0), 0);
        // 防抖距离、滚一格的距离至少 1 像素
        let s = Gestures::new(VIEW, 0.0, 0.0);
        assert!((s.slop - 1.0).abs() < f32::EPSILON && (s.notch_px - 1.0).abs() < f32::EPSILON);
    }
}
