//! 一帧里画面哪里变了：脏区域和移动区域。
//!
//! 业内的采集接口都把一帧的变化拆成这两种（见 `docs/research/Q-redesign/m2b-plan.md` 第 1 节）：
//! - **脏区域**：和上一版相比新画的区域（Windows 桌面复制的 `GetFrameDirtyRects`、macOS ScreenCaptureKit 的 `dirtyRects`、
//!   WebRTC 的 `updated_region()`）；
//! - **移动区域**：从别处搬过来的区域（Windows 的 `GetFrameMoveRects`、VNC 的 CopyRect）。
//!
//! 这里只有区域本身和几个集合运算，没有任何编码器的概念。假后端（`peerdesk_media::fake`）用它累计“从上一帧编出来以后”
//! 变了哪里，字节模型据此估字节数；产品的真后端不用它（把系统给的区域交给编码器当提示，以后单独立项）。

use std::sync::{Arc, Mutex, PoisonError};

/// 一个移动区域的目的地占整帧千分之几以上，算“整屏移动”（整页滚动、整屏平移）。
pub const FULL_MOVE_PERMILLE: u64 = 900;
/// 累计时最多留这么多个矩形：超过就把最相近的两个并成它们的外接矩形（区域只会变大，不会漏）。
pub const MAX_RECTS: usize = 48;

/// 矩形（像素），左上角加宽高。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    /// 左边。
    pub x: u16,
    /// 上边。
    pub y: u16,
    /// 宽。
    pub w: u16,
    /// 高。
    pub h: u16,
}

impl Rect {
    /// 创建。
    #[must_use]
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    /// 整个画面。
    #[must_use]
    pub const fn whole(size: (u16, u16)) -> Self {
        Self::new(0, 0, size.0, size.1)
    }

    /// 把任意位置的矩形（可以是负的、可以伸出画面）裁到画面里；裁完是空的返回 `None`。
    #[must_use]
    pub fn clip(x: i32, y: i32, w: u32, h: u32, bounds: (u16, u16)) -> Option<Self> {
        let x0 = i64::from(x).max(0);
        let y0 = i64::from(y).max(0);
        let x1 = (i64::from(x) + i64::from(w)).min(i64::from(bounds.0));
        let y1 = (i64::from(y) + i64::from(h)).min(i64::from(bounds.1));
        Self::from_edges(x0, y0, x1, y1)
    }

    fn from_edges(x0: i64, y0: i64, x1: i64, y1: i64) -> Option<Self> {
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Self {
            x: u16::try_from(x0).ok()?,
            y: u16::try_from(y0).ok()?,
            w: u16::try_from(x1 - x0).ok()?,
            h: u16::try_from(y1 - y0).ok()?,
        })
    }

    /// 右边（不含）。
    #[must_use]
    pub fn right(self) -> u32 {
        u32::from(self.x) + u32::from(self.w)
    }

    /// 下边（不含）。
    #[must_use]
    pub fn bottom(self) -> u32 {
        u32::from(self.y) + u32::from(self.h)
    }

    /// 面积（像素）。
    #[must_use]
    pub fn area(self) -> u64 {
        u64::from(self.w) * u64::from(self.h)
    }

    /// 是空的。
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// 包含 `other`。
    #[must_use]
    pub fn contains(self, other: Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// 两个矩形的重叠部分。
    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        Self::from_edges(
            i64::from(self.x.max(other.x)),
            i64::from(self.y.max(other.y)),
            i64::from(self.right().min(other.right())),
            i64::from(self.bottom().min(other.bottom())),
        )
    }

    /// 外接矩形。
    #[must_use]
    pub fn hull(self, other: Self) -> Self {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        Self::from_edges(
            i64::from(x0),
            i64::from(y0),
            i64::from(self.right().max(other.right())),
            i64::from(self.bottom().max(other.bottom())),
        )
        .unwrap_or(self)
    }

    /// 减去 `cut` 之后剩下的部分，拆成互不重叠的至多四块（上、下、左、右）。
    #[must_use]
    pub fn subtract(self, cut: Self) -> Vec<Self> {
        let Some(mid) = self.intersect(cut) else {
            return vec![self];
        };
        let (sx0, sy0) = (i64::from(self.x), i64::from(self.y));
        let (sx1, sy1) = (i64::from(self.right()), i64::from(self.bottom()));
        let (mx0, my0) = (i64::from(mid.x), i64::from(mid.y));
        let (mx1, my1) = (i64::from(mid.right()), i64::from(mid.bottom()));
        [
            (sx0, sy0, sx1, my0),
            (sx0, my1, sx1, sy1),
            (sx0, my0, mx0, my1),
            (mx1, my0, sx1, my1),
        ]
        .into_iter()
        .filter_map(|(x0, y0, x1, y1)| Self::from_edges(x0, y0, x1, y1))
        .collect()
    }

    /// 换算到另一个画面尺寸：向外取整，换算后只会更大，不会漏。
    #[must_use]
    pub fn scaled(self, from: (u16, u16), to: (u16, u16)) -> Self {
        if from == to || from.0 == 0 || from.1 == 0 {
            return self;
        }
        let map = |v: u32, num: u16, den: u16, ceil: bool| -> i64 {
            let n = u64::from(v) * u64::from(num);
            let d = u64::from(den);
            i64::try_from(if ceil { n.div_ceil(d) } else { n / d }).unwrap_or(i64::MAX)
        };
        Self::from_edges(
            map(u32::from(self.x), to.0, from.0, false),
            map(u32::from(self.y), to.1, from.1, false),
            map(self.right(), to.0, from.0, true).min(i64::from(to.0)),
            map(self.bottom(), to.1, from.1, true).min(i64::from(to.1)),
        )
        .unwrap_or(Self::new(0, 0, 0, 0))
    }
}

/// 一块从别处搬来的区域：`to` 里的像素和上一版 `from` 起同样大小的那块一样。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Move {
    /// 来源的左上角。
    pub from: (u16, u16),
    /// 目的地。
    pub to: Rect,
}

/// `list` 里每个矩形都减去 `cut` 里的每一块。
#[must_use]
pub fn subtract_all(list: &[Rect], cut: &[Rect]) -> Vec<Rect> {
    let mut out: Vec<Rect> = list.to_vec();
    for c in cut {
        out = out.into_iter().flat_map(|r| r.subtract(*c)).collect();
    }
    out
}

/// 两组矩形两两相交。
#[must_use]
pub fn intersect_all(a: &[Rect], b: &[Rect]) -> Vec<Rect> {
    a.iter()
        .flat_map(|x| b.iter().filter_map(|y| x.intersect(*y)))
        .collect()
}

/// 一组矩形（可以重叠）盖住的面积（像素）：按竖线切成条，每条里把盖到它的矩形的纵向区间并起来。
#[must_use]
pub fn union_area(list: &[Rect]) -> u64 {
    let mut xs: Vec<u32> = list
        .iter()
        .filter(|r| !r.is_empty())
        .flat_map(|r| [u32::from(r.x), r.right()])
        .collect();
    xs.sort_unstable();
    xs.dedup();
    let mut total = 0u64;
    for pair in xs.windows(2) {
        let (Some(&xa), Some(&xb)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let mut spans: Vec<(u32, u32)> = list
            .iter()
            .filter(|r| !r.is_empty() && u32::from(r.x) <= xa && r.right() >= xb)
            .map(|r| (u32::from(r.y), r.bottom()))
            .collect();
        spans.sort_unstable();
        let mut covered = 0u64;
        let mut end = 0u32;
        for (a, b) in spans {
            let a = a.max(end);
            if b > a {
                covered += u64::from(b - a);
                end = b;
            }
        }
        total += covered * u64::from(xb - xa);
    }
    total
}

/// 加一块进去：已经被某一块盖住的不加；加完超过 [`MAX_RECTS`] 就把最相近的两块并成外接矩形。
fn push_capped(list: &mut Vec<Rect>, r: Rect) {
    if r.is_empty() || list.iter().any(|e| e.contains(r)) {
        return;
    }
    list.retain(|e| !r.contains(*e));
    list.push(r);
    while list.len() > MAX_RECTS {
        // 并起来多出来的面积最小的一对（重叠的一对多出来的是负数）
        let mut best: Option<(i64, usize, usize)> = None;
        for (i, a) in list.iter().enumerate() {
            for (j, b) in list.iter().enumerate().skip(i + 1) {
                let waste = i64::try_from(a.hull(*b).area()).unwrap_or(i64::MAX)
                    - i64::try_from(a.area() + b.area()).unwrap_or(i64::MAX);
                if best.is_none_or(|(w, _, _)| waste < w) {
                    best = Some((waste, i, j));
                }
            }
        }
        let Some((_, i, j)) = best else { break };
        let merged = match (list.get(i), list.get(j)) {
            (Some(a), Some(b)) => a.hull(*b),
            _ => break,
        };
        let _ = list.remove(j);
        if let Some(slot) = list.get_mut(i) {
            *slot = merged;
        }
    }
}

/// 一版画面相对上一版的变化（原始尺寸上）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    /// 画面尺寸。
    pub size: (u16, u16),
    /// 新画的区域。
    pub dirty: Vec<Rect>,
    /// 搬来的区域。
    pub moves: Vec<Move>,
}

impl Changes {
    /// 没有变化。
    #[must_use]
    pub const fn empty(size: (u16, u16)) -> Self {
        Self {
            size,
            dirty: Vec::new(),
            moves: Vec::new(),
        }
    }

    /// 整个画面都是新的。
    #[must_use]
    pub fn whole(size: (u16, u16)) -> Self {
        Self {
            size,
            dirty: vec![Rect::whole(size)],
            moves: Vec::new(),
        }
    }

    /// 脏区域和移动目的地盖住的面积（像素）。
    #[must_use]
    pub fn covered_area(&self) -> u64 {
        let all: Vec<Rect> = self
            .dirty
            .iter()
            .copied()
            .chain(self.moves.iter().map(|m| m.to))
            .collect();
        union_area(&all)
    }

    /// 移动的目的地占了整帧的 [`FULL_MOVE_PERMILLE`]‰ 以上（整屏滚动、整屏平移）。
    #[must_use]
    pub fn full_move(&self) -> bool {
        let dests: Vec<Rect> = self.moves.iter().map(|m| m.to).collect();
        let whole = u64::from(self.size.0) * u64::from(self.size.1);
        whole > 0 && union_area(&dests) * 1000 >= FULL_MOVE_PERMILLE * whole
    }
}

/// 从上一帧编出来以后累计的变化：脏区域取并集，移动区域只留目的地（减去脏区域是用的时候算，见 [`Self::rects`]）。
/// 中间跳过的版本也累计进来，所以累计的是矩形本身；矩形太多时并成外接矩形，区域只会变大。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Accum {
    size: (u16, u16),
    dirty: Vec<Rect>,
    moved: Vec<Rect>,
    full_move: bool,
}

impl Accum {
    /// 累计一版的变化。画面尺寸变了，之前的累计作废：整个画面都算新的。
    pub fn add(&mut self, c: &Changes) {
        if self.size == (0, 0) {
            self.size = c.size;
        } else if self.size != c.size {
            *self = Self {
                size: c.size,
                dirty: vec![Rect::whole(c.size)],
                ..Self::default()
            };
        }
        for r in &c.dirty {
            push_capped(&mut self.dirty, *r);
        }
        for m in &c.moves {
            push_capped(&mut self.moved, m.to);
        }
        self.full_move |= c.full_move();
    }

    /// 累计时有没有任何一版的移动占了整帧。
    #[must_use]
    pub const fn full_move(&self) -> bool {
        self.full_move
    }

    /// 什么都没累计。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.dirty.is_empty() && self.moved.is_empty()
    }

    /// 换算到编码尺寸的（脏区域，移动目的地）。
    #[must_use]
    pub fn rects(&self, to: (u16, u16)) -> (Vec<Rect>, Vec<Rect>) {
        let scale = |l: &[Rect]| -> Vec<Rect> {
            l.iter()
                .map(|r| r.scaled(self.size, to))
                .filter(|r| !r.is_empty())
                .collect()
        };
        (scale(&self.dirty), scale(&self.moved))
    }
}

/// 变化的信道：画面的来源（仿真里的合成桌面、操作时间线）每变一版就往里推一份，假后端每交一帧读一次累计，
/// 编出来之后清零。和 `FakeBackend::with_version_tag` 是同一种做法：吃真像素的路径（L0.5）像素里没有“哪里变了”，
/// 吃内容描述的路径（L0）里被跳过的版本后端也看不到，所以由来源在每一版推进来，两条路交给后端的累计相同。
#[derive(Debug, Clone, Default)]
pub struct ChangeFeed(Arc<Mutex<Accum>>);

impl ChangeFeed {
    /// 推进一版的变化。
    pub fn push(&self, c: &Changes) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).add(c);
    }

    /// 到现在为止的累计（不清零：这一帧没编出来的话，变化还欠着）。
    #[must_use]
    pub fn peek(&self) -> Accum {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// 清零（一帧编出来了）。
    pub fn clear(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Accum::default();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn r(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn rect_basics() {
        let a = r(10, 20, 30, 40);
        assert_eq!((a.right(), a.bottom(), a.area()), (40, 60, 1200));
        assert!(!a.is_empty() && r(1, 1, 0, 5).is_empty() && r(1, 1, 5, 0).is_empty());
        assert!(a.contains(r(10, 20, 30, 40)) && a.contains(r(15, 25, 5, 5)));
        assert!(!a.contains(r(5, 25, 5, 5)) && !a.contains(r(15, 25, 50, 5)));
        assert_eq!(Rect::whole((8, 6)), r(0, 0, 8, 6));
        assert_eq!(a.intersect(r(30, 50, 30, 30)), Some(r(30, 50, 10, 10)));
        assert_eq!(a.intersect(r(40, 20, 5, 5)), None, "只挨着不算重叠");
        assert_eq!(a.hull(r(0, 0, 5, 5)), r(0, 0, 40, 60));
    }

    #[test]
    fn clip_cuts_to_the_screen() {
        let b = (100, 50);
        assert_eq!(Rect::clip(10, 10, 20, 20, b), Some(r(10, 10, 20, 20)));
        assert_eq!(Rect::clip(-5, -5, 20, 20, b), Some(r(0, 0, 15, 15)));
        assert_eq!(Rect::clip(90, 40, 50, 50, b), Some(r(90, 40, 10, 10)));
        assert_eq!(Rect::clip(100, 0, 5, 5, b), None);
        assert_eq!(Rect::clip(-50, 0, 20, 5, b), None);
        assert_eq!(Rect::clip(0, 0, 0, 5, b), None);
        // 位置、尺寸大到超过 u16 也不出错
        assert_eq!(
            Rect::clip(i32::MIN, i32::MIN, u32::MAX, u32::MAX, b),
            Some(r(0, 0, 100, 50))
        );
    }

    /// 减法拆成的块互不重叠、面积相加正好等于原面积减去重叠。
    #[test]
    fn subtract_pieces_are_disjoint_and_exact() {
        let a = r(10, 10, 40, 30);
        for cut in [
            r(0, 0, 5, 5),
            r(20, 15, 10, 10),
            r(0, 0, 100, 100),
            r(30, 0, 5, 100),
            r(0, 20, 100, 3),
            r(40, 30, 30, 30),
            r(10, 10, 40, 30),
        ] {
            let pieces = a.subtract(cut);
            let overlap = a.intersect(cut).map_or(0, Rect::area);
            let sum: u64 = pieces.iter().map(|p| p.area()).sum();
            assert_eq!(sum, a.area() - overlap, "{cut:?}");
            assert_eq!(union_area(&pieces), sum, "{cut:?} 的块互不重叠");
            for p in &pieces {
                assert!(a.contains(*p) && p.intersect(cut).is_none());
            }
        }
        assert_eq!(a.subtract(r(200, 200, 5, 5)), vec![a]);
        assert!(a.subtract(a).is_empty());
    }

    #[test]
    fn scaling_never_loses_a_pixel() {
        let a = r(3, 5, 7, 9);
        assert_eq!(a.scaled((100, 100), (100, 100)), a);
        assert_eq!(a.scaled((0, 0), (50, 50)), a);
        // 缩小到一半：向外取整
        assert_eq!(a.scaled((100, 100), (50, 50)), r(1, 2, 4, 5));
        // 放大
        assert_eq!(a.scaled((100, 100), (200, 200)), r(6, 10, 14, 18));
        // 伸到边上的不超过目标尺寸
        assert_eq!(
            r(90, 90, 10, 10).scaled((100, 100), (51, 51)),
            r(45, 45, 6, 6)
        );
        // 缩没了
        assert!(r(0, 0, 1, 1).scaled((100, 100), (1, 1)).area() <= 1);
        assert!(r(99, 99, 1, 1).scaled((100, 100), (100, 100)).area() == 1);
    }

    #[test]
    fn list_operations() {
        let a = [r(0, 0, 10, 10), r(5, 5, 10, 10)];
        assert_eq!(union_area(&a), 100 + 100 - 25);
        assert_eq!(union_area(&[]), 0);
        assert_eq!(union_area(&[r(0, 0, 0, 5), r(1, 1, 5, 5)]), 25);
        assert_eq!(union_area(&[r(0, 0, 10, 10), r(0, 20, 10, 10)]), 200);
        let left = subtract_all(&a, &[r(0, 0, 10, 3)]);
        assert_eq!(union_area(&left), 175 - 30);
        let both = intersect_all(&[r(0, 0, 10, 10)], &[r(5, 5, 10, 10), r(50, 50, 1, 1)]);
        assert_eq!(both, vec![r(5, 5, 5, 5)]);
    }

    #[test]
    fn too_many_rects_are_merged_but_never_lose_area() {
        let mut acc = Accum::default();
        let mut covered = Vec::new();
        for i in 0..200u16 {
            let rect = r(i * 3 % 400, i * 7 % 300, 6, 6);
            covered.push(rect);
            acc.add(&Changes {
                size: (500, 400),
                dirty: vec![rect],
                moves: vec![Move {
                    from: (0, 0),
                    to: rect,
                }],
            });
        }
        assert!(acc.dirty.len() <= MAX_RECTS && acc.moved.len() <= MAX_RECTS);
        for c in &covered {
            assert!(acc.dirty.iter().any(|d| d.contains(*c)), "{c:?} 被漏掉");
        }
        // 被已有的盖住的、空的不加
        let n = acc.dirty.len();
        let whole = Rect::whole((500, 400));
        acc.add(&Changes {
            size: (500, 400),
            dirty: vec![r(1, 1, 0, 0)],
            moves: vec![],
        });
        assert_eq!(acc.dirty.len(), n);
        // 整屏的一块盖住所有小块
        acc.add(&Changes::whole((500, 400)));
        assert_eq!(acc.dirty, vec![whole]);
    }

    #[test]
    fn accumulation_keeps_every_skipped_version() {
        let size = (200, 100);
        let mut acc = Accum::default();
        assert!(acc.is_empty() && !acc.full_move());
        acc.add(&Changes {
            size,
            dirty: vec![r(0, 0, 10, 10)],
            moves: vec![Move {
                from: (0, 4),
                to: r(0, 0, 200, 96),
            }],
        });
        acc.add(&Changes {
            size,
            dirty: vec![r(50, 50, 10, 10)],
            moves: vec![],
        });
        assert!(!acc.is_empty() && acc.full_move(), "整屏移动一旦出现就记着");
        let (dirty, moved) = acc.rects(size);
        assert_eq!(dirty, vec![r(0, 0, 10, 10), r(50, 50, 10, 10)]);
        assert_eq!(moved, vec![r(0, 0, 200, 96)]);
        // 换算到半尺寸
        let (dirty, moved) = acc.rects((100, 50));
        assert_eq!(dirty, vec![r(0, 0, 5, 5), r(25, 25, 5, 5)]);
        assert_eq!(moved, vec![r(0, 0, 100, 48)]);
        // 画面尺寸变了：之前的作废，整屏算新的
        acc.add(&Changes::empty((300, 100)));
        assert_eq!(acc.rects((300, 100)), (vec![r(0, 0, 300, 100)], vec![]));
        assert!(!acc.full_move());
    }

    #[test]
    fn changes_measure_themselves() {
        let size = (100, 100);
        let c = Changes {
            size,
            dirty: vec![r(0, 96, 100, 4)],
            moves: vec![Move {
                from: (0, 4),
                to: r(0, 0, 100, 96),
            }],
        };
        assert_eq!(c.covered_area(), 10_000);
        assert!(c.full_move(), "96% 的移动算整屏");
        let small = Changes {
            moves: vec![Move {
                from: (0, 0),
                to: r(0, 0, 10, 10),
            }],
            ..c.clone()
        };
        assert!(!small.full_move());
        assert_eq!(Changes::empty(size).covered_area(), 0);
        assert!(!Changes::empty(size).full_move());
        assert!(!Changes::empty((0, 0)).full_move());
        assert_eq!(Changes::whole(size).covered_area(), 10_000);
    }

    #[test]
    fn the_feed_accumulates_until_cleared() {
        let feed = ChangeFeed::default();
        let other = feed.clone();
        assert!(feed.peek().is_empty());
        other.push(&Changes::whole((10, 10)));
        assert!(!feed.peek().is_empty());
        assert_eq!(feed.peek(), feed.peek(), "看不会清零");
        feed.clear();
        assert!(other.peek().is_empty());
    }
}
