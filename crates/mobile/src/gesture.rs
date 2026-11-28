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
/// 双指判断：一个量（两指距离的变化、中点走过的距离）是另一个的这么多倍，就定成它（见 `classify`）。
pub const DOMINANCE: f32 = 2.0;
/// 双指只有一根在动时，最多等多久（毫秒）再定（另一根可能只是晚一点动；照 libinput 的做法）。
pub const ONE_FINGER_WAIT_MS: u64 = 150;
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
    /// 在界面的 (`x`, `y`) 点了一下（`button` 左键或右键）：界面画一个点击的水波；
    /// 左键点的地方也是用户要输入的位置，弹出软键盘时界面把它让到键盘上面。
    Tap {
        /// 横坐标（界面像素）。
        x: f32,
        /// 纵坐标（界面像素）。
        y: f32,
        /// [`LEFT`] 或 [`RIGHT`]。
        button: u8,
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
    /// 一根手指，还没到长按。`moved` 是超过了防抖距离（之后就是滑动，不再算点击和长按）。
    One { f: Finger, moved: bool },
    /// 长按到点了、手指还没动：松开是右键，动了就开始拖拽。
    Held { f: Finger },
    /// 拖拽中：左键按着，跟着手指走。
    Drag { f: Finger },
    /// 两根手指。两根的 `start` 是第二根按下那一刻的位置（判断和“定了以后补上”都从这里量），
    /// `since` 是那一刻的时间；`dist` 是上一次两指的距离，`scroll` 是没发出去的滚动余数（滚轮单位）。
    Two {
        a: Finger,
        b: Finger,
        what: Two,
        dist: f32,
        scroll: (f32, f32),
        since: u64,
    },
    /// 手势已经结束（例如双指点完了），等还按着的这几根手指（编号）都抬起，期间什么也不发。
    /// 记编号不记个数：系统取消时给的事件里手指数不一定对（真机上见过三指被系统拿走，取消事件只带一根）。
    Done { ids: Vec<u32> },
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

    /// 同一个系统触摸事件里所有手指的移动（Android 的一个 `ACTION_MOVE` 带着每根手指的新位置）。
    ///
    /// 必须一起交：两根手指一根一根地更新，中间会出现“一根动了、另一根还在旧位置”的假状态，
    /// 两指距离和中点在一次事件里来回跳，缩放就抖（Android 的 `ScaleGestureDetector` 也是一次拿整个事件算）。
    /// 不是双指状态时逐个按 [`Self::touch`] 处理。阶段不是移动的触点忽略。
    pub fn moves(&mut self, touches: &[Touch]) -> Vec<Action> {
        if let State::Two {
            mut a,
            mut b,
            what,
            dist,
            scroll,
            since,
        } = self.state.clone()
        {
            let now = touches.iter().map(|t| t.t_ms).max().unwrap_or(since);
            let before = midpoint(a.last, b.last);
            let mut ours = false;
            for t in touches.iter().filter(|t| t.phase == Phase::Move) {
                if t.id == a.id {
                    a.last = (t.x, t.y);
                    ours = true;
                } else if t.id == b.id {
                    b.last = (t.x, t.y);
                    ours = true;
                }
            }
            // 这两根手指都不在里面（别的手指、抬起）：什么也不做
            if !ours {
                return Vec::new();
            }
            let mut out = Vec::new();
            self.state = self.two_moved(a, b, what, dist, scroll, since, now, before, &mut out);
            return out;
        }
        touches
            .iter()
            .filter(|t| t.phase == Phase::Move)
            .flat_map(|t| self.touch(t))
            .collect()
    }

    /// 整个手势作废，回到没有手指的状态：系统取消（来电、系统手势拿走）、或者界面知道这是新手势的第一根手指
    /// （Android 的 `ACTION_DOWN` 说明之前的手指都已经抬起，留下的状态一定是丢了事件）。拖拽中就先松开左键。
    pub fn reset(&mut self) -> Vec<Action> {
        let out = if matches!(self.state, State::Drag { .. }) {
            vec![button_action(LEFT, false)]
        } else {
            Vec::new()
        };
        self.state = State::Idle;
        out
    }

    /// 处理一个触点。取消（[`Phase::Cancel`]）不管是哪根手指，都是整个手势作废（Android 的取消就是这个意思），见 [`Self::reset`]。
    pub fn touch(&mut self, t: &Touch) -> Vec<Action> {
        if t.phase == Phase::Cancel {
            return self.reset();
        }
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
                    since,
                },
                phase,
            ) => self.two(a, b, what, dist, scroll, since, t, phase, &mut out),
            (State::Done { mut ids }, Phase::Down) => {
                if !ids.contains(&t.id) {
                    ids.push(t.id);
                }
                State::Done { ids }
            }
            (State::Done { ids }, Phase::Move | Phase::Cancel) => State::Done { ids },
            (State::Done { mut ids }, Phase::Up) => {
                ids.retain(|id| *id != t.id);
                if ids.is_empty() {
                    State::Idle
                } else {
                    State::Done { ids }
                }
            }
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
                // 第二根手指：进入双指。第一根的起点挪到现在的位置：两指距离、中点都从两根都按下的这一刻量
                // （ScaleGestureDetector 也是这样；真机上见过两根几乎同时按下时，系统先把它们报成一根、跳了 240 像素，
                // 拿第一根当初按下的地方当起点，两指距离就像从 8 涨到 234，被当成捏合）
                let b = Finger::new(t);
                let a = Finger { start: f.last, ..f };
                return State::Two {
                    a,
                    b,
                    what: Two::Undecided,
                    dist: distance(a.last, b.last),
                    scroll: (0.0, 0.0),
                    since: t.t_ms,
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
        out.push(Action::Tap {
            x: at.0,
            y: at.1,
            button,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn two(
        &mut self,
        mut a: Finger,
        mut b: Finger,
        what: Two,
        dist: f32,
        scroll: (f32, f32),
        since: u64,
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
            (Phase::Down, None) => State::Done {
                ids: vec![a.id, b.id, t.id],
            },
            (Phase::Down, Some(_)) | (Phase::Move | Phase::Up | Phase::Cancel, None) => {
                State::Two {
                    a,
                    b,
                    what,
                    dist,
                    scroll,
                    since,
                }
            }
            (Phase::Move, Some(first)) => {
                let before = midpoint(a.last, b.last);
                if first {
                    a.last = (t.x, t.y);
                } else {
                    b.last = (t.x, t.y);
                }
                self.two_moved(a, b, what, dist, scroll, since, t.t_ms, before, out)
            }
            (Phase::Up | Phase::Cancel, Some(first)) => {
                let quick = t.t_ms.saturating_sub(a.down_ms) <= TWO_FINGER_TAP_MS
                    && b.down_ms.saturating_sub(a.down_ms) <= TWO_FINGER_GAP_MS;
                if phase == Phase::Up && what == Two::Undecided && quick {
                    self.click(RIGHT, midpoint(a.last, b.last), out);
                }
                State::Done {
                    ids: vec![if first { b.id } else { a.id }],
                }
            }
        }
    }

    /// 两根手指的位置都更新好了（`before` 是更新前的中点）：判断是捏合还是滚动，再照做。
    ///
    /// 判断（定了就不再改，直到手指抬起）：两指距离的变化和中点走过的距离，哪个先超过防抖距离、
    /// 并且明显比另一个大，就是哪个。双指滚动时两指间距总会有些变化（手指不会完全平行），
    /// 所以不能只看距离变了百分之几；捏合时中点也会漂（常见一根手指不动、另一根动），
    /// 所以也不能只看中点动了多少。
    #[allow(clippy::too_many_arguments)]
    fn two_moved(
        &self,
        a: Finger,
        b: Finger,
        mut what: Two,
        mut dist: f32,
        mut scroll: (f32, f32),
        since: u64,
        now_ms: u64,
        mut before: (f32, f32),
        out: &mut Vec<Action>,
    ) -> State {
        let after = midpoint(a.last, b.last);
        let now_dist = distance(a.last, b.last);
        if what == Two::Undecided {
            what = classify(a, b, self.slop, now_ms.saturating_sub(since));
            if what != Two::Undecided {
                // 刚定下来：把没定之前走过的也补上（从两根都按下那一刻量），画面和滚动不落后手指
                before = midpoint(a.start, b.start);
                dist = distance(a.start, b.start);
            }
            if what == Two::Scroll {
                out.push(Action::Input(self.pointer_at(midpoint(a.start, b.start))));
            }
        }
        match what {
            Two::Undecided => {}
            Two::Pinch => {
                if dist > 0.0 && now_dist > 0.0 && (now_dist - dist).abs() > f32::EPSILON {
                    // 照看图软件的做法：旧中点下面的内容缩放后跟到新中点下面
                    // （以旧中点为中心缩放，再平移中点走过的距离），画面贴着手指走
                    out.push(Action::Zoom {
                        factor: now_dist / dist,
                        cx: before.0,
                        cy: before.1,
                    });
                    let (dx, dy) = (after.0 - before.0, after.1 - before.1);
                    if dx != 0.0 || dy != 0.0 {
                        out.push(Action::Pan { dx, dy });
                    }
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
            since,
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

/// 双指还没定时看一眼：捏合、滚动，还是再等等。`elapsed_ms` 是两根都按下以后过了多久。
///
/// 量的都是从两根都按下那一刻起：两指距离变了多少（`span`），中点走了多远（`travel`），两根各自走了多远、往哪走。
/// 业内做法：Android 的 `ScaleGestureDetector` 两指距离变化超过防抖距离的 2 倍才算开始捏合；
/// libinput（Linux 触摸板）看两根手指的方向，同向是滚动、反向是捏合，只有一根在动时先等一等。
///
/// 1. 都还没动够（距离变化不到 2 倍防抖距离、中点没走过防抖距离）：再等。
/// 2. 两根都动了：方向相近（夹角小于 45°）或者中点走的是距离变化的 [`DOMINANCE`] 倍以上，是滚动；方向相反（大于 90°）是捏合。
///    （夹角在中间时“距离变化是中点走的 2 倍以上”几何上不会出现：两根都动、又不反向，中点总会跟着走。）
/// 3. 还分不出（只有一根在动，或者夹角在中间）：先等。真机上两指上下叠着滚动时，一根常比另一根早动 40 毫秒，
///    这 40 毫秒里两指距离缩了 49 像素、中点只走了 24，看起来就是捏合。等到有一根走过 4 倍防抖距离、
///    或者过了 [`ONE_FINGER_WAIT_MS`]：距离变化大于中点走的是捏合（一指按着一指拉开），否则是滚动。
fn classify(a: Finger, b: Finger, slop: f32, elapsed_ms: u64) -> Two {
    let span = (distance(a.last, b.last) - distance(a.start, b.start)).abs();
    let travel = distance(midpoint(a.start, b.start), midpoint(a.last, b.last));
    if span <= slop * 2.0 && travel <= slop {
        return Two::Undecided;
    }
    let da = (a.last.0 - a.start.0, a.last.1 - a.start.1);
    let db = (b.last.0 - b.start.0, b.last.1 - b.start.1);
    let (la, lb) = (da.0.hypot(da.1), db.0.hypot(db.1));
    let still = slop / 2.0;
    if la > still && lb > still {
        let cos = (da.0 * db.0 + da.1 * db.1) / (la * lb);
        if cos > std::f32::consts::FRAC_1_SQRT_2 || travel > span * DOMINANCE {
            return Two::Scroll;
        }
        if cos < 0.0 {
            return Two::Pinch;
        }
    }
    // 还分不出（只有一根在动，或者两根的方向在 45°–90° 之间）：等到走远了或者到了时限，按哪个量大定，不会一直不定
    if la.max(lb) <= slop * 4.0 && elapsed_ms < ONE_FINGER_WAIT_MS {
        return Two::Undecided;
    }
    if span > travel {
        Two::Pinch
    } else {
        Two::Scroll
    }
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
            vec![
                mv(32768, 32768),
                btn(LEFT, true),
                btn(LEFT, false),
                Action::Tap {
                    x: 500.0,
                    y: 350.0,
                    button: LEFT
                }
            ]
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
        assert_eq!(
            out,
            vec![
                mv(0, 0),
                btn(RIGHT, true),
                btn(RIGHT, false),
                Action::Tap {
                    x: 0.0,
                    y: 100.0,
                    button: RIGHT
                }
            ]
        );
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
            vec![
                mv(32768, 32768),
                btn(RIGHT, true),
                btn(RIGHT, false),
                Action::Tap {
                    x: 500.0,
                    y: 350.0,
                    button: RIGHT
                }
            ]
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
        // 只有一根往下 30：另一根可能只是晚一点动，先等
        assert!(s.touch(&t(1, Phase::Move, 400.0, 330.0, 20)).is_empty());
        // 另一根也往下 30：同向，是滚动。先把光标放到起点的中点，再把这 30 一次补上（30/40 格向上）
        let out = s.touch(&t(2, Phase::Move, 600.0, 330.0, 30));
        assert_eq!(
            out,
            vec![
                mv(32768, 26214),
                Action::Input(InputEvent::Wheel { dx: 0, dy: 90 })
            ]
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
        // 一根不动、一根拉开 40：只有一根在动，还没走过 4 倍防抖距离，先等另一根
        assert!(s.touch(&t(2, Phase::Move, 640.0, 300.0, 20)).is_empty());
        // 拉到 60：另一根还不动，距离变化（60）大于中点走的（30），是一指按着一指拉开的捏合。
        // 从两根都按下那一刻补上：以那时的中点为中心放大 260/200，再跟着中点平移
        let out = s.touch(&t(2, Phase::Move, 660.0, 300.0, 30));
        assert_eq!(
            out,
            vec![
                Action::Zoom {
                    factor: 1.3,
                    cx: 500.0,
                    cy: 300.0
                },
                Action::Pan { dx: 30.0, dy: 0.0 }
            ]
        );
        // 之后每次相对上一次
        let out = s.touch(&t(1, Phase::Move, 340.0, 300.0, 40));
        assert_eq!(
            out,
            vec![
                Action::Zoom {
                    factor: 320.0 / 260.0,
                    cx: 530.0,
                    cy: 300.0
                },
                Action::Pan { dx: -30.0, dy: 0.0 }
            ]
        );
        // 第三根手指：作废，等三根都抬起
        assert!(s.touch(&t(3, Phase::Down, 10.0, 10.0, 45)).is_empty());
        assert!(s.touch(&t(3, Phase::Move, 20.0, 10.0, 50)).is_empty());
        for id in [1, 2] {
            assert!(s.touch(&t(id, Phase::Up, 0.0, 0.0, 60)).is_empty());
            assert_ne!(s.state, State::Idle);
        }
        s.touch(&t(3, Phase::Up, 0.0, 0.0, 70));
        assert_eq!(s.state, State::Idle);
    }

    /// 一次事件里两根手指的新位置（批量交）。
    fn both(s: &mut Gestures, a: (f32, f32), b: (f32, f32), t_ms: u64) -> Vec<Action> {
        s.moves(&[
            t(1, Phase::Move, a.0, a.1, t_ms),
            t(2, Phase::Move, b.0, b.1, t_ms),
        ])
    }

    /// 两指从 `a0`、`b0` 按下，按 `steps` 次事件线性走到 `a1`、`b1`（每次事件两指一起），收集所有输出。
    fn glide(
        a0: (f32, f32),
        b0: (f32, f32),
        a1: (f32, f32),
        b1: (f32, f32),
        steps: u16,
    ) -> Vec<Action> {
        let mut s = g();
        s.touch(&t(1, Phase::Down, a0.0, a0.1, 0));
        s.touch(&t(2, Phase::Down, b0.0, b0.1, 10));
        let lerp =
            |p: (f32, f32), q: (f32, f32), k: f32| (p.0 + (q.0 - p.0) * k, p.1 + (q.1 - p.1) * k);
        let mut out = Vec::new();
        for i in 1..=steps {
            let k = f32::from(i) / f32::from(steps);
            out.extend(both(
                &mut s,
                lerp(a0, a1, k),
                lerp(b0, b1, k),
                20 + u64::from(i) * 8,
            ));
        }
        out
    }

    fn zooms(out: &[Action]) -> usize {
        out.iter()
            .filter(|a| matches!(a, Action::Zoom { .. }))
            .count()
    }

    fn wheels(out: &[Action]) -> usize {
        out.iter()
            .filter(|a| matches!(a, Action::Input(InputEvent::Wheel { .. })))
            .count()
    }

    #[test]
    fn scroll_and_pinch_are_told_apart_with_real_hands() {
        // 双指往下滚 150，手指间距从 200 收到 176（12%：真实的手不会完全平行；旧判据“距离变 6% 就是捏合”会判错）
        let out = glide(
            (400.0, 200.0),
            (600.0, 200.0),
            (412.0, 350.0),
            (588.0, 350.0),
            15,
        );
        assert_eq!(zooms(&out), 0, "{out:?}");
        assert!(wheels(&out) > 0, "{out:?}");
        // 斜着滚：两指方向差一点
        let out = glide(
            (400.0, 200.0),
            (600.0, 200.0),
            (430.0, 340.0),
            (610.0, 350.0),
            15,
        );
        assert_eq!(zooms(&out), 0, "{out:?}");
        assert!(wheels(&out) > 0, "{out:?}");
        // 对称捏开 200 → 320，中点同时往下漂 25（手在动）
        let out = glide(
            (400.0, 300.0),
            (600.0, 300.0),
            (340.0, 325.0),
            (660.0, 325.0),
            15,
        );
        assert_eq!(wheels(&out), 0, "{out:?}");
        assert!(zooms(&out) > 0, "{out:?}");
        // 拇指按着不动，食指斜着拉开：中点走了一半的距离，照样是捏合
        let out = glide(
            (400.0, 400.0),
            (500.0, 300.0),
            (400.0, 400.0),
            (600.0, 200.0),
            15,
        );
        assert_eq!(wheels(&out), 0, "{out:?}");
        assert!(zooms(&out) > 0, "{out:?}");
        // 捏小
        let out = glide(
            (300.0, 300.0),
            (700.0, 300.0),
            (420.0, 300.0),
            (580.0, 300.0),
            15,
        );
        assert_eq!(wheels(&out), 0, "{out:?}");
        assert!(zooms(&out) > 0, "{out:?}");
    }

    #[test]
    fn pinching_keeps_the_picture_under_the_fingers() {
        // 拇指不动、食指拉开再挪：每次把缩放、平移作用到“按下时两根手指下面的内容点”，最后要还在两根手指下面
        let mut s = g();
        s.touch(&t(1, Phase::Down, 400.0, 400.0, 0));
        s.touch(&t(2, Phase::Down, 500.0, 300.0, 10));
        let mut under = [(400.0_f32, 400.0_f32), (500.0, 300.0)];
        // 不转（两指连线方向不变）：缩放加平移跟得上；我们不旋转远程画面，转的那部分本来就跟不上
        let path = [
            ((400.0, 400.0), (530.0, 270.0)),
            ((400.0, 400.0), (580.0, 220.0)),
            ((380.0, 420.0), (620.0, 180.0)),
        ];
        for (i, (a, b)) in path.into_iter().enumerate() {
            for act in both(&mut s, a, b, 20 + u64::try_from(i).unwrap_or(0) * 10) {
                for p in &mut under {
                    match act {
                        Action::Zoom { factor, cx, cy } => {
                            *p = (cx + (p.0 - cx) * factor, cy + (p.1 - cy) * factor);
                        }
                        Action::Pan { dx, dy } => *p = (p.0 + dx, p.1 + dy),
                        Action::Input(_) | Action::Haptic | Action::Tap { .. } => {}
                    }
                }
            }
            let close = |p: (f32, f32), q: (f32, f32)| distance(p, q) < 0.5;
            assert!(
                close(under[0], a) && close(under[1], b),
                "{i}: {under:?} vs {a:?} {b:?}"
            );
        }
    }

    #[test]
    fn one_event_moves_both_fingers_at_once() {
        // 对称捏开：一次事件两根手指一起走，一次缩放，中心不动（逐根交的话中心会来回跳）。
        // 定下来的那次从两根按下时（距离 200）补上
        let mut s = g();
        s.touch(&t(1, Phase::Down, 400.0, 300.0, 0));
        s.touch(&t(2, Phase::Down, 600.0, 300.0, 10));
        assert!(both(&mut s, (395.0, 300.0), (605.0, 300.0), 20).is_empty());
        let out = both(&mut s, (380.0, 300.0), (620.0, 300.0), 30);
        assert_eq!(
            out,
            vec![Action::Zoom {
                factor: 1.2,
                cx: 500.0,
                cy: 300.0
            }]
        );
        // 不是移动的触点、别的手指：不管
        assert!(s
            .moves(&[
                t(1, Phase::Up, 0.0, 0.0, 40),
                t(9, Phase::Move, 0.0, 0.0, 40)
            ])
            .is_empty());
        // 一根手指时逐个处理（单指滑动是平移）
        let mut s = g();
        s.touch(&t(1, Phase::Down, 100.0, 300.0, 0));
        let out = s.moves(&[t(1, Phase::Move, 150.0, 300.0, 20)]);
        assert_eq!(out, vec![Action::Pan { dx: 50.0, dy: 0.0 }]);
    }

    #[test]
    fn a_cancel_from_the_system_ends_the_whole_gesture() {
        // 真机（2026-11-28）：三根手指按下后被系统拿走，取消事件只带一根手指。以前按个数等三根都抬起，从此卡住
        let mut s = g();
        s.touch(&t(0, Phase::Down, 228.0, 300.0, 0));
        s.touch(&t(1, Phase::Down, 736.0, 400.0, 11));
        s.touch(&t(2, Phase::Down, 721.0, 300.0, 45));
        assert!(matches!(s.state, State::Done { .. }));
        assert!(s.touch(&t(0, Phase::Cancel, 0.0, 0.0, 62)).is_empty());
        assert_eq!(s.state, State::Idle);
        // 下一个捏合照常
        s.touch(&t(0, Phase::Down, 400.0, 300.0, 900));
        s.touch(&t(1, Phase::Down, 600.0, 300.0, 910));
        assert_eq!(zooms(&both(&mut s, (350.0, 300.0), (650.0, 300.0), 930)), 1);
    }

    #[test]
    fn leftover_fingers_are_counted_by_id() {
        // 双指点完：等剩下那根抬起；同一根重复报按下不多算，不认识的抬起不管
        let mut s = g();
        s.touch(&t(1, Phase::Down, 400.0, 350.0, 0));
        s.touch(&t(2, Phase::Down, 600.0, 350.0, 40));
        s.touch(&t(1, Phase::Up, 400.0, 350.0, 150));
        assert_eq!(s.state, State::Done { ids: vec![2] });
        s.touch(&t(2, Phase::Down, 600.0, 350.0, 160));
        s.touch(&t(9, Phase::Up, 0.0, 0.0, 170));
        assert!(s.touch(&t(2, Phase::Move, 610.0, 350.0, 175)).is_empty());
        assert_eq!(s.state, State::Done { ids: vec![2] });
        s.touch(&t(3, Phase::Down, 100.0, 100.0, 180));
        s.touch(&t(2, Phase::Up, 600.0, 350.0, 190));
        assert_eq!(s.state, State::Done { ids: vec![3] });
        s.touch(&t(3, Phase::Up, 100.0, 100.0, 200));
        assert_eq!(s.state, State::Idle);
        // 新手势的第一根手指：界面先 reset，留下的状态清掉（没在拖拽就什么也不发）
        s.touch(&t(5, Phase::Down, 100.0, 100.0, 300));
        s.touch(&t(6, Phase::Down, 200.0, 100.0, 310));
        s.touch(&t(7, Phase::Down, 300.0, 100.0, 320));
        assert!(s.reset().is_empty());
        assert_eq!(s.state, State::Idle);
    }

    #[test]
    fn the_two_finger_rules_cover_the_corner_cases() {
        let f = |start: (f32, f32), last: (f32, f32)| Finger {
            id: 0,
            start,
            last,
            down_ms: 0,
        };
        let slop = 10.0;
        // 两根都动、夹角在 45°–90° 之间：中点走得远（距离变化的 2 倍以上）是滚动；分不出先等，到时限按哪个量大定
        let a = f((0.0, 0.0), (-40.0, -40.0));
        assert_eq!(
            classify(a, f((200.0, 0.0), (160.0, 10.0)), slop, 20),
            Two::Scroll
        );
        let a = f((0.0, 0.0), (0.0, 30.0));
        let b = f((200.0, 0.0), (230.0, 5.0));
        assert_eq!(classify(a, b, slop, 20), Two::Undecided);
        assert_eq!(classify(a, b, slop, ONE_FINGER_WAIT_MS), Two::Pinch);
        // 只有一根动、还没走远：150 毫秒内再等，过了就定（距离变化大于中点走的是捏合，否则滚动）
        let still = f((200.0, 0.0), (200.0, 0.0));
        let along = f((0.0, 0.0), (-30.0, 0.0));
        assert_eq!(classify(along, still, slop, 100), Two::Undecided);
        assert_eq!(classify(along, still, slop, ONE_FINGER_WAIT_MS), Two::Pinch);
        let across = f((0.0, 0.0), (0.0, 30.0));
        assert_eq!(
            classify(across, still, slop, ONE_FINGER_WAIT_MS),
            Two::Scroll
        );
        // 都没动够
        assert_eq!(
            classify(f((0.0, 0.0), (3.0, 3.0)), still, slop, 500),
            Two::Undecided
        );
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
