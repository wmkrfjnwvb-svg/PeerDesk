//! 触控到鼠标、键盘事件的状态机（纯逻辑，Android 和 iOS 共用）。
//!
//! 界面只转发原始触点（编号、按下/移动/抬起、坐标、时间），并在有手指按着时每隔几十毫秒调一次 [`Gestures::tick`]
//! （长按要靠时间判断，手指不动时系统不一定发移动事件）。这里决定它是点击、拖拽、滚动还是缩放。
//!
//! 手势表（两种模式共用，区别只在“光标在哪”）：
//!
//! | 手势 | 直接点击模式（点哪是哪） | 触控板模式（手指移动光标） |
//! |---|---|---|
//! | 单指点 | 在手指处左键单击 | 在光标处左键单击 |
//! | 单指滑动 | 画面放大时平移画面；没放大时移动光标（不按键） | 移动光标 |
//! | 长按后拖动 | 在手指处按下左键并拖拽 | 在光标处按下左键并拖拽 |
//! | 双指点 | 在两指中点右键单击 | 在光标处右键单击 |
//! | 双指滑动 | 滚动（先把光标移到两指中点） | 滚动 |
//! | 双指捏合 | 缩放画面 | 缩放画面 |
//!
//! 滚动是“自然方向”：手指往下，内容跟着往下（等于滚轮向上）。三指及以上不处理，留给系统。

use peerdesk_core::input::InputEvent;

/// 点击最长多久（毫秒）：按下到抬起超过它就不算点击。
pub const TAP_MS: u64 = 250;
/// 长按多久（毫秒）开始拖拽。
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
const LEFT: u8 = 0;
const RIGHT: u8 = 1;

/// 触控模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 点哪是哪。
    Direct,
    /// 手指移动光标，像笔记本的触控板。
    Trackpad,
}

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
    /// 画面是不是放大了（单指滑动在直接点击模式下平移画面还是移动光标）。
    pub zoomed: bool,
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
    /// 一根手指，还没定是点击、滑动还是长按。`moved` 是超过了防抖距离。
    One { f: Finger, moved: bool },
    /// 长按之后：左键按着，跟着手指拖。
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
    mode: Mode,
    view: View,
    /// 防抖距离（像素）：手指移动不超过它还算“没动”。界面按屏幕密度给（大约 8 dp）。
    slop: f32,
    /// 滚轮一格对应手指移动多少像素。
    notch_px: f32,
    /// 触控板模式的光标位置（被控端画面上的比例，0..=1）。
    cursor: (f32, f32),
    state: State,
}

impl Gestures {
    /// 新建。`slop_px` 是防抖距离（像素），`notch_px` 是滚一格要移动多少像素。
    #[must_use]
    pub fn new(mode: Mode, view: View, slop_px: f32, notch_px: f32) -> Self {
        Self {
            mode,
            view,
            slop: slop_px.max(1.0),
            notch_px: notch_px.max(1.0),
            cursor: (0.5, 0.5),
            state: State::Idle,
        }
    }

    /// 换模式（例如工具条上切换）。正在进行的手势照常结束。
    pub const fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
    }

    /// 现在的模式。
    #[must_use]
    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// 画面位置变了（缩放、平移、横竖屏）。
    pub const fn set_view(&mut self, view: View) {
        self.view = view;
    }

    /// 触控板模式的光标位置（被控端画面上的比例），界面据此画光标。
    #[must_use]
    pub const fn cursor(&self) -> (f32, f32) {
        self.cursor
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

    /// 时间到了：手指按着不动够久就开始拖拽。有手指按着时界面每隔几十毫秒调一次。
    pub fn tick(&mut self, now_ms: u64) -> Vec<Action> {
        let mut out = Vec::new();
        if let State::One { f, moved: false } = self.state {
            if now_ms.saturating_sub(f.down_ms) >= LONG_PRESS_MS {
                let at = self.pointer_at(f.start);
                out.push(Action::Input(at));
                out.push(button(LEFT, true));
                self.state = State::Drag { f };
            }
        }
        out
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
            if phase == Phase::Down
                && t.t_ms.saturating_sub(f.down_ms) <= TWO_FINGER_GAP_MS.max(LONG_PRESS_MS)
            {
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
                    self.slide(prev, f.last, out);
                }
                State::One { f, moved }
            }
            Phase::Up => {
                f.last = (t.x, t.y);
                if !moved && t.t_ms.saturating_sub(f.down_ms) <= TAP_MS {
                    out.push(Action::Input(self.pointer_at(f.start)));
                    out.push(button(LEFT, true));
                    out.push(button(LEFT, false));
                }
                State::Idle
            }
            Phase::Cancel => State::Idle,
        }
    }

    /// 单指滑动：直接点击模式下放大了就平移画面、没放大就移动光标；触控板模式移动光标。
    fn slide(&mut self, prev: (f32, f32), now: (f32, f32), out: &mut Vec<Action>) {
        match self.mode {
            Mode::Direct if self.view.zoomed => out.push(Action::Pan {
                dx: now.0 - prev.0,
                dy: now.1 - prev.1,
            }),
            Mode::Direct => out.push(Action::Input(self.pointer_at(now))),
            Mode::Trackpad => {
                self.nudge(prev, now);
                out.push(Action::Input(self.cursor_event()));
            }
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
                let prev = f.last;
                f.last = (t.x, t.y);
                let at = match self.mode {
                    Mode::Direct => self.pointer_at(f.last),
                    Mode::Trackpad => {
                        self.nudge(prev, f.last);
                        self.cursor_event()
                    }
                };
                out.push(Action::Input(at));
                State::Drag { f }
            }
            Phase::Up | Phase::Cancel => {
                out.push(button(LEFT, false));
                State::Idle
            }
        }
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
                        if self.mode == Mode::Direct {
                            out.push(Action::Input(self.pointer_at(midpoint(a.start, b.start))));
                        }
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
                    let at = match self.mode {
                        Mode::Direct => self.pointer_at(midpoint(a.last, b.last)),
                        Mode::Trackpad => self.cursor_event(),
                    };
                    out.push(Action::Input(at));
                    out.push(button(RIGHT, true));
                    out.push(button(RIGHT, false));
                }
                State::Done { left: 1 }
            }
        }
    }

    /// 触控板模式：光标跟着手指的位移走（按画面大小换算成比例，手指在画面上划过整个宽度 = 光标划过整个宽度）。
    fn nudge(&mut self, prev: (f32, f32), now: (f32, f32)) {
        let w = self.view.width.max(1.0);
        let h = self.view.height.max(1.0);
        self.cursor.0 = (self.cursor.0 + (now.0 - prev.0) / w).clamp(0.0, 1.0);
        self.cursor.1 = (self.cursor.1 + (now.1 - prev.1) / h).clamp(0.0, 1.0);
    }

    fn cursor_event(&self) -> InputEvent {
        InputEvent::MouseMove {
            x: unit(self.cursor.0),
            y: unit(self.cursor.1),
        }
    }

    /// 界面上的一点对应被控端屏幕的哪里（画面外的点贴到边上）。触控板模式下是光标的位置。
    fn pointer_at(&self, p: (f32, f32)) -> InputEvent {
        if self.mode == Mode::Trackpad {
            return self.cursor_event();
        }
        let nx = (p.0 - self.view.left) / self.view.width.max(1.0);
        let ny = (p.1 - self.view.top) / self.view.height.max(1.0);
        InputEvent::MouseMove {
            x: unit(nx),
            y: unit(ny),
        }
    }
}

const fn button(button: u8, down: bool) -> Action {
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
        zoomed: false,
    };

    fn g(mode: Mode) -> Gestures {
        Gestures::new(mode, VIEW, 10.0, 40.0)
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
        let mut s = g(Mode::Direct);
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
    }

    #[test]
    fn a_slow_release_or_a_slide_is_not_a_click() {
        let mut s = g(Mode::Direct);
        s.touch(&t(1, Phase::Down, 500.0, 350.0, 0));
        assert!(s
            .touch(&t(1, Phase::Up, 500.0, 350.0, TAP_MS + 1))
            .is_empty());
        // 没放大：单指滑动移动光标，不按键
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 1000));
        assert!(
            s.touch(&t(1, Phase::Move, 5.0, 100.0, 1010)).is_empty(),
            "没超过防抖距离"
        );
        assert_eq!(
            s.touch(&t(1, Phase::Move, 500.0, 350.0, 1020)),
            vec![mv(32768, 32768)]
        );
        assert!(s.touch(&t(1, Phase::Up, 500.0, 350.0, 1030)).is_empty());
        // 放大了：单指滑动平移画面
        s.set_view(View {
            zoomed: true,
            ..VIEW
        });
        s.touch(&t(1, Phase::Down, 100.0, 200.0, 2000));
        assert_eq!(
            s.touch(&t(1, Phase::Move, 130.0, 180.0, 2010)),
            vec![Action::Pan {
                dx: 30.0,
                dy: -20.0
            }]
        );
        // 系统取消：不点击
        assert!(s.touch(&t(1, Phase::Cancel, 130.0, 180.0, 2020)).is_empty());
        assert!(
            s.touch(&t(1, Phase::Up, 130.0, 180.0, 2030)).is_empty(),
            "已经回到空闲"
        );
    }

    #[test]
    fn a_long_press_drags_until_the_finger_lifts() {
        let mut s = g(Mode::Direct);
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 0));
        assert!(s.tick(LONG_PRESS_MS - 1).is_empty());
        assert_eq!(s.tick(LONG_PRESS_MS), vec![mv(0, 0), btn(LEFT, true)]);
        // 再 tick 不重复按下
        assert!(s.tick(LONG_PRESS_MS + 100).is_empty());
        assert_eq!(
            s.touch(&t(1, Phase::Move, 1000.0, 600.0, 700)),
            vec![mv(65535, 65535)]
        );
        // 拖拽中别的手指不打断
        assert!(s.touch(&t(2, Phase::Down, 10.0, 10.0, 710)).is_empty());
        assert!(s.touch(&t(2, Phase::Up, 10.0, 10.0, 720)).is_empty());
        assert!(s.touch(&t(1, Phase::Down, 0.0, 0.0, 730)).is_empty());
        assert_eq!(
            s.touch(&t(1, Phase::Up, 1000.0, 600.0, 800)),
            vec![btn(LEFT, false)]
        );
        // 取消也要松开左键，不能让被控端一直按着
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 1000));
        s.tick(1000 + LONG_PRESS_MS);
        assert_eq!(
            s.touch(&t(1, Phase::Cancel, 0.0, 100.0, 1600)),
            vec![btn(LEFT, false)]
        );
        // 动过了就不算长按
        s.touch(&t(1, Phase::Down, 0.0, 100.0, 2000));
        s.touch(&t(1, Phase::Move, 50.0, 100.0, 2010));
        assert!(s.tick(2000 + LONG_PRESS_MS).is_empty());
    }

    #[test]
    fn two_fingers_tap_for_a_right_click_at_their_middle() {
        let mut s = g(Mode::Direct);
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
        // 第二根手指在长按时间之后才来：已经不是单指等待了（长按先发生），这里不进入双指
        s.touch(&t(1, Phase::Down, 400.0, 350.0, 3000));
        assert!(s
            .touch(&t(2, Phase::Down, 600.0, 350.0, 3000 + LONG_PRESS_MS + 1))
            .is_empty());
        assert!(
            s.touch(&t(2, Phase::Move, 650.0, 350.0, 3600)).is_empty(),
            "别的手指的移动不算"
        );
        s.touch(&t(1, Phase::Up, 400.0, 350.0, 3700));
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn two_fingers_scroll_naturally_and_keep_the_remainder() {
        let mut s = g(Mode::Direct);
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
        let mut s = g(Mode::Direct);
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
    fn the_trackpad_moves_a_cursor_and_clicks_where_it_is() {
        let mut s = g(Mode::Trackpad);
        assert_eq!(s.mode(), Mode::Trackpad);
        assert_eq!(s.cursor(), (0.5, 0.5));
        // 手指在哪不重要：往右划 250 像素 = 光标往右走四分之一个画面宽
        s.touch(&t(1, Phase::Down, 750.0, 550.0, 0));
        let out = s.touch(&t(1, Phase::Move, 1000.0, 550.0, 10));
        assert_eq!(out, vec![mv(49151, 32768)]);
        s.touch(&t(1, Phase::Up, 1000.0, 550.0, 20));
        // 单指点：在光标处点击
        s.touch(&t(1, Phase::Down, 10.0, 10.0, 100));
        let out = s.touch(&t(1, Phase::Up, 10.0, 10.0, 150));
        assert_eq!(
            out,
            vec![mv(49151, 32768), btn(LEFT, true), btn(LEFT, false)]
        );
        // 光标不出画面
        s.touch(&t(1, Phase::Down, 0.0, 0.0, 200));
        s.touch(&t(1, Phase::Move, 5000.0, -5000.0, 210));
        assert_eq!(s.cursor(), (1.0, 0.0));
        s.touch(&t(1, Phase::Up, 5000.0, -5000.0, 220));
        // 长按拖拽：从光标处拖
        s.touch(&t(1, Phase::Down, 500.0, 300.0, 1000));
        assert_eq!(
            s.tick(1000 + LONG_PRESS_MS),
            vec![mv(65535, 0), btn(LEFT, true)]
        );
        assert_eq!(
            s.touch(&t(1, Phase::Move, 250.0, 425.0, 1600)),
            vec![mv(49151, 16384)]
        );
        assert_eq!(
            s.touch(&t(1, Phase::Up, 250.0, 425.0, 1700)),
            vec![btn(LEFT, false)]
        );
        // 双指点：在光标处右键；双指滚动不移动光标
        s.touch(&t(1, Phase::Down, 400.0, 300.0, 2000));
        s.touch(&t(2, Phase::Down, 600.0, 300.0, 2010));
        let out = s.touch(&t(2, Phase::Up, 600.0, 300.0, 2100));
        assert_eq!(
            out,
            vec![mv(49151, 16384), btn(RIGHT, true), btn(RIGHT, false)]
        );
        s.touch(&t(1, Phase::Up, 400.0, 300.0, 2110));
        s.touch(&t(1, Phase::Down, 400.0, 300.0, 3000));
        s.touch(&t(2, Phase::Down, 600.0, 300.0, 3010));
        let out = s.touch(&t(1, Phase::Move, 400.0, 340.0, 3020));
        assert_eq!(
            out,
            vec![Action::Input(InputEvent::Wheel { dx: 0, dy: 60 })]
        );
        // 换回直接点击模式
        s.set_mode(Mode::Direct);
        assert_eq!(s.mode(), Mode::Direct);
    }

    #[test]
    fn stray_events_are_ignored() {
        let mut s = g(Mode::Direct);
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
        // 工具函数的边界
        assert_eq!(whole(1e9), i16::MAX);
        assert_eq!(whole(-1e9), i16::MIN);
        assert_eq!(unit(2.0), 65535);
        assert_eq!(unit(-1.0), 0);
        // 防抖距离、滚一格的距离至少 1 像素
        let s = Gestures::new(Mode::Direct, VIEW, 0.0, 0.0);
        assert!((s.slop - 1.0).abs() < f32::EPSILON && (s.notch_px - 1.0).abs() < f32::EPSILON);
    }
}
