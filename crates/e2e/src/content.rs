//! 接近真实的桌面画面：照片质感的壁纸、带抗锯齿“文字”的窗口、视频区域，
//! 以及打字、滚动、拖窗口、切换窗口、播放视频这些操作。
//!
//! 全部由种子决定，同一个种子每次生成的画面逐像素相同，测试结果可以复现。
//! 仿真里的纯色画面太“干净”，编码器在上面的表现比真实桌面好得多，所以要用这里的画面。

use peerdesk_core::changes::{intersect_all, subtract_all, Changes, Move, Rect};
use peerdesk_core::error::Error;
use peerdesk_core::screen::Screen;

/// 用户在做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// 什么都不做。
    Idle,
    /// 在编辑窗口里打字：每 3 帧出一个字。
    Typing,
    /// 滚动文档窗口：每帧 4 像素。
    Scrolling,
    /// 拖着一个窗口来回移动：每帧 8 像素。
    Dragging,
    /// 切换窗口：第一帧把一个新窗口切到最前面（大面积突变），之后不动。
    Switching,
    /// 播放视频：视频区域每帧都变。
    Video,
    /// 小动静：鼠标指针每帧移一点（光标、悬停效果、时钟这类），每帧只变一小块。
    /// 真实桌面很少完全静止：2026-11-26 用户日志里轻操作时每秒 22–28 帧、每帧 0–3 KB，发送端远用不满码率。
    Pointer,
}

impl Action {
    /// 全部操作，（名字，值），世界文件读写用。
    pub const ALL: [(&'static str, Action); 7] = [
        ("idle", Action::Idle),
        ("typing", Action::Typing),
        ("scrolling", Action::Scrolling),
        ("dragging", Action::Dragging),
        ("switching", Action::Switching),
        ("video", Action::Video),
        ("pointer", Action::Pointer),
    ];

    /// 名字，用于报告。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Action::Idle => "idle",
            Action::Typing => "typing",
            Action::Scrolling => "scrolling",
            Action::Dragging => "dragging",
            Action::Switching => "switching",
            Action::Video => "video",
            Action::Pointer => "pointer",
        }
    }
}

/// 画一个 12×19 的箭头鼠标指针（黑边白芯），左上角在 (x, y)。超出画面的部分不画。
fn draw_pointer(out: &mut [u32], w: u32, x: u32, y: u32) {
    const BLACK: u32 = 0xff00_0000;
    const WHITE: u32 = 0xffff_ffff;
    for dy in 0u32..19 {
        // 上面 12 行是三角形，下面是一小截柄
        let (from, to) = if dy < 12 { (0, dy) } else { (4, 7) };
        for dx in from..=to {
            let edge = dx == from || dx == to || dy == 11 || dy == 18;
            let at = usize::try_from((y + dy) * w + x + dx).unwrap_or(usize::MAX);
            if x + dx < w {
                if let Some(p) = out.get_mut(at) {
                    *p = if edge { BLACK } else { WHITE };
                }
            }
        }
    }
}

/// 简单的确定性随机数（splitmix64）。
#[derive(Debug, Clone)]
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

/// 一块像素缓冲。
#[derive(Debug, Clone)]
struct Canvas {
    w: u32,
    h: u32,
    px: Vec<u32>,
}

impl Canvas {
    fn new(w: u32, h: u32, color: u32) -> Self {
        let len = usize::try_from(w * h).unwrap_or(0);
        Self {
            w,
            h,
            px: vec![color; len],
        }
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        (x < self.w && y < self.h).then(|| usize::try_from(y * self.w + x).ok())?
    }

    fn get(&self, x: u32, y: u32) -> u32 {
        self.index(x, y)
            .and_then(|i| self.px.get(i).copied())
            .unwrap_or(0)
    }

    fn set(&mut self, x: u32, y: u32, color: u32) {
        if let Some(p) = self.index(x, y).and_then(|i| self.px.get_mut(i)) {
            *p = color;
        }
    }

    fn fill(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        for yy in y..y.saturating_add(h).min(self.h) {
            for xx in x..x.saturating_add(w).min(self.w) {
                self.set(xx, yy, color);
            }
        }
    }
}

fn gray(level: u32) -> u32 {
    let l = level.min(255);
    l << 16 | l << 8 | l
}

/// 字形宽高（含字间距）。
const GLYPH_W: u32 = 7;
const LINE_H: u32 = 14;

/// 画一个“字”：5×8 的点阵由种子决定，深色笔画两侧带浅灰的抗锯齿边，像真实的屏幕文字。
fn draw_glyph(c: &mut Canvas, x: u32, y: u32, seed: u64) {
    let mut r = Rng(seed);
    let bits = r.next();
    for row in 0..8u32 {
        for col in 0..5u32 {
            if bits >> (row * 5 + col) & 1 == 1 && (row + col) % 3 != 0 {
                c.set(x + col, y + row, gray(40));
                // 抗锯齿：左右两侧一半亮度
                if c.get(x + col + 1, y + row) == gray(255) {
                    c.set(x + col + 1, y + row, gray(150));
                }
                if col > 0 && c.get(x + col - 1, y + row) == gray(255) {
                    c.set(x + col - 1, y + row, gray(170));
                }
            }
        }
    }
}

/// 画一行“文字”：字和空格交替，像一段英文。
fn draw_line(c: &mut Canvas, x: u32, y: u32, chars: u32, seed: u64) {
    let mut r = Rng(seed);
    let mut cx = x;
    for _ in 0..chars {
        if r.below(6) != 0 {
            draw_glyph(c, cx, y, r.next());
        }
        cx += GLYPH_W;
    }
}

/// 一个窗口：标题栏 + 白底内容。内容是一张比窗口高的“文档”，滚动就是改偏移。
#[derive(Debug, Clone)]
struct Window {
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    title: u32,
    doc: Canvas,
    scroll: u32,
    visible: bool,
}

const TITLE_H: u32 = 24;

impl Window {
    fn new(x: i32, y: i32, w: u32, h: u32, title: u32, lines: u32, seed: u64) -> Self {
        let doc_h = (lines * LINE_H).max(h);
        let mut doc = Canvas::new(w, doc_h, gray(255));
        let chars = w.saturating_sub(24) / GLYPH_W;
        for i in 0..lines {
            draw_line(
                &mut doc,
                12,
                6 + i * LINE_H,
                chars,
                seed ^ u64::from(i) << 20,
            );
        }
        Self {
            x,
            y,
            w,
            h,
            title,
            doc,
            scroll: 0,
            visible: true,
        }
    }

    /// 窗口在 (px, py) 处的颜色；不在窗口内时返回 `None`。
    fn pixel(&self, px: i32, py: i32) -> Option<u32> {
        let (dx, dy) = (px - self.x, py - self.y);
        let (dx, dy) = (u32::try_from(dx).ok()?, u32::try_from(dy).ok()?);
        if !self.visible || dx >= self.w || dy >= self.h {
            return None;
        }
        if dy < TITLE_H {
            // 标题栏：带一点竖向渐变，右边三个按钮
            let shade = self.title.saturating_sub(dy * 0x0001_0101);
            let button = dx + 70 > self.w && (dx + 70 - self.w) % 22 < 14 && (6..18).contains(&dy);
            return Some(if button { gray(220) } else { shade });
        }
        let doc_y = dy - TITLE_H + self.scroll;
        Some(self.doc.get(dx, doc_y % self.doc.h.max(1)))
    }
}

/// 接近真实的桌面。
#[derive(Debug, Clone)]
pub struct RealisticDesktop {
    w: u32,
    h: u32,
    wallpaper: Canvas,
    windows: Vec<Window>,
    video: (u32, u32, u32, u32),
    /// 视频区域当前这一帧：只在播放视频时更新，否则保持不变（像暂停的视频）。
    video_frame: Canvas,
    frame: u32,
    typed: u32,
    drag_dir: i32,
    /// 鼠标指针的位置（还没出现时为 `None`）和移动方向。
    pointer: Option<(u32, u32)>,
    pointer_dir: (i32, i32),
    rng: Rng,
    screen: Screen,
    /// 最近一步画了哪里、搬了哪里（相对上一步的画面）。
    last: Changes,
}

/// 鼠标指针的外框：宽 12、高 19（和 [`draw_pointer`] 画的一致）。
const POINTER_W: u32 = 12;
const POINTER_H: u32 = 19;

/// 一层（一个窗口，或窗口里的内容区）从 `old` 变成 `new`、里面的内容整块平移 `shift` 之后的变化：
/// 被上面的东西（`occluders`，别的窗口）挡住的部分不算这一层的。鼠标指针（`blockers`）不是整块不透明的：
/// 它的外框里只有箭头本身盖住了画面，其余还是这一层的内容，所以外框里的像素仍算这一层的、会变，
/// 只是不能说成“搬来的”（来源或目的地在指针底下，像素不是拷贝），这些由新画的区域盖住。
///
/// - 搬来的：新画面里这一层看得见的、并且“往回平移 `shift` 之后在旧画面里也是这一层看得见的”那些，来源就是往回平移之后的位置；
/// - 新画的：旧、新两个位置里这一层看得见的，减去搬来的（露出来的底下的东西、新进来的一条）。
///
/// 这样每个搬来的区域都真的是旧画面里同一块的拷贝（采集接口的 MoveRects 也是这个意思），不会把被别的窗口盖住的
/// 部分说成是搬来的。
fn layer_shift(
    size: (u16, u16),
    old: (i32, i32, u32, u32),
    new: (i32, i32, u32, u32),
    shift: (i32, i32),
    occluders: &[Rect],
    blockers: &[Rect],
) -> Changes {
    let visible = |r: (i32, i32, u32, u32)| {
        let clipped: Vec<Rect> = Rect::clip(r.0, r.1, r.2, r.3, size).into_iter().collect();
        subtract_all(&clipped, occluders)
    };
    let (old_vis, new_vis) = (visible(old), visible(new));
    let shifted: Vec<Rect> = old_vis
        .iter()
        .filter_map(|r| {
            Rect::clip(
                i32::from(r.x) + shift.0,
                i32::from(r.y) + shift.1,
                u32::from(r.w),
                u32::from(r.h),
                size,
            )
        })
        .collect();
    // 来源在指针外框里的目的地：外框平移 `shift` 之后的位置
    let blocked: Vec<Rect> = blockers
        .iter()
        .copied()
        .chain(blockers.iter().filter_map(|r| {
            Rect::clip(
                i32::from(r.x) + shift.0,
                i32::from(r.y) + shift.1,
                u32::from(r.w),
                u32::from(r.h),
                size,
            )
        }))
        .collect();
    let targets = subtract_all(&intersect_all(&new_vis, &shifted), &blocked);
    let moves = targets
        .iter()
        .filter_map(|t| {
            Some(Move {
                from: (
                    u16::try_from(i32::from(t.x) - shift.0).ok()?,
                    u16::try_from(i32::from(t.y) - shift.1).ok()?,
                ),
                to: *t,
            })
        })
        .collect();
    let both: Vec<Rect> = old_vis.into_iter().chain(new_vis).collect();
    Changes {
        size,
        dirty: subtract_all(&both, &targets),
        moves,
    }
}

/// 窗口的下标。
const EDITOR: usize = 0;
const DOCUMENT: usize = 1;
const PLAYER: usize = 2;
const POPUP: usize = 3;

impl RealisticDesktop {
    /// 创建。
    pub fn new(width: u16, height: u16, seed: u64) -> Result<Self, Error> {
        let (w, h) = (u32::from(width), u32::from(height));
        if w < 320 || h < 240 {
            return Err(Error::InvalidArgument("画面至少 320×240"));
        }
        let mut rng = Rng(seed);
        // 壁纸：两个方向的渐变叠加几个柔和的亮斑，再加亮度噪点，像一张照片。
        let mut wallpaper = Canvas::new(w, h, 0);
        let blobs: Vec<(u32, u32, u32)> = (0..5)
            .map(|_| (rng.below(w), rng.below(h), 80 + rng.below(h / 2)))
            .collect();
        for y in 0..h {
            for x in 0..w {
                let mut light = 0u32;
                for (bx, by, r) in &blobs {
                    let d = x.abs_diff(*bx).max(y.abs_diff(*by));
                    light += r.saturating_sub(d) * 60 / r.max(&1);
                }
                let n = rng.below(10);
                let r = (30 + x * 120 / w + light + n).min(255);
                let g = (50 + y * 90 / h + light / 2 + n).min(255);
                let b = (110 + (w - x) * 80 / w + n).min(255);
                wallpaper.set(x, y, r << 16 | g << 8 | b);
            }
        }
        let (iw, ih) = (i32::try_from(w).unwrap_or(0), i32::try_from(h).unwrap_or(0));
        let mut windows = vec![
            Window::new(
                iw / 20,
                ih / 12,
                w * 9 / 20,
                h / 2,
                0x0040_6090,
                60,
                rng.next(),
            ),
            Window::new(
                iw / 2,
                ih / 10,
                w * 9 / 20,
                h * 7 / 10,
                0x0030_7050,
                400,
                rng.next(),
            ),
            Window::new(
                iw / 8,
                ih / 2,
                w * 2 / 5,
                h * 2 / 5,
                0x0050_5050,
                1,
                rng.next(),
            ),
            Window::new(
                iw / 5,
                ih / 6,
                w * 3 / 5,
                h * 3 / 5,
                0x0080_4040,
                80,
                rng.next(),
            ),
        ];
        if let Some(popup) = windows.get_mut(POPUP) {
            popup.visible = false;
        }
        // 编辑窗口一开始几乎是空的：打字时字一个个出现。
        if let Some(editor) = windows.get_mut(EDITOR) {
            editor
                .doc
                .fill(0, LINE_H * 3, editor.w, editor.doc.h, gray(255));
        }
        let player = windows.get(PLAYER).map_or((0, 0, 0, 0), |p| {
            let (x, y) = (
                u32::try_from(p.x).unwrap_or(0),
                u32::try_from(p.y).unwrap_or(0),
            );
            (x + 8, y + TITLE_H + 8, p.w - 16, p.h - TITLE_H - 16)
        });
        let mut desk = Self {
            w,
            h,
            wallpaper,
            windows,
            video: player,
            video_frame: Canvas::new(player.2, player.3, 0),
            frame: 0,
            typed: 0,
            drag_dir: 1,
            pointer: None,
            pointer_dir: (5, 3),
            rng,
            screen: Screen::new(width, height, 0),
            last: Changes::empty((width, height)),
        };
        desk.render_video();
        desk.compose()?;
        Ok(desk)
    }

    /// 当前画面。
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        &self.screen
    }

    /// 最近一步（[`Self::step`]）画了哪里、搬了哪里。画面没变的一步是空的。
    ///
    /// 合成桌面知道自己在画什么，所以区域是它自己交的，不是拿前后两幅像素比出来的（采集接口也是系统交的）：
    /// 打字是字和光标的小块，指针是旧位置加新位置，滚动是内容区的移动加露出来的一条，拖窗口是窗口的移动加露出来的底，
    /// 切窗口和视频是脏区域。被别的窗口和指针盖住的部分不算移动。单元测试逐像素核对它们盖得住实际变了的每一个像素。
    #[must_use]
    pub const fn changes(&self) -> &Changes {
        &self.last
    }

    fn size(&self) -> (u16, u16) {
        (self.screen.width(), self.screen.height())
    }

    /// 窗口下标比 `below` 大的（在它上面的）、看得见的窗口：它们会整块盖住 `below` 那个窗口的一部分。
    fn occluders(&self, below: usize) -> Vec<Rect> {
        let size = self.size();
        self.windows
            .iter()
            .enumerate()
            .filter(|(i, w)| *i > below && w.visible)
            .filter_map(|(_, w)| Rect::clip(w.x, w.y, w.w, w.h, size))
            .collect()
    }

    /// 鼠标指针的外框（还没出现为空）。
    fn blockers(&self) -> Vec<Rect> {
        self.pointer
            .into_iter()
            .filter_map(|p| self.pointer_box(p))
            .collect()
    }

    fn pointer_box(&self, (x, y): (u32, u32)) -> Option<Rect> {
        Rect::clip(
            i32::try_from(x).ok()?,
            i32::try_from(y).ok()?,
            POINTER_W,
            POINTER_H,
            self.size(),
        )
    }

    /// 视频区域某一帧的像素：缓慢移动的彩色波纹加上每帧不同的颗粒，压缩起来和真实视频一样费劲。
    fn video_pixel(&mut self, x: u32, y: u32) -> u32 {
        let t = self.frame;
        let wave =
            |a: u32, b: u32| -> u32 { (a.wrapping_mul(3) + b.wrapping_mul(5) + t * 7) % 256 };
        let grain = self.rng.below(24);
        let r = (wave(x, y) / 2 + 60 + grain).min(255);
        let g = (wave(y, x + t) / 3 + 40 + grain).min(255);
        let b = (wave(x + y, t) / 2 + 50 + grain).min(255);
        r << 16 | g << 8 | b
    }

    /// 生成视频区域的下一帧。
    fn render_video(&mut self) {
        let (_, _, vw, vh) = self.video;
        for y in 0..vh {
            for x in 0..vw {
                let p = self.video_pixel(x, y);
                self.video_frame.set(x, y, p);
            }
        }
    }

    fn compose(&mut self) -> Result<(), Error> {
        let mut out = Vec::with_capacity(usize::try_from(self.w * self.h).unwrap_or(0));
        let (vx, vy, vw, vh) = self.video;
        let playing = self.windows.get(PLAYER).is_some_and(|p| p.visible);
        for y in 0..self.h {
            for x in 0..self.w {
                let (ix, iy) = (i32::try_from(x).unwrap_or(0), i32::try_from(y).unwrap_or(0));
                // 从最上层的窗口往下找
                let top = self
                    .windows
                    .iter()
                    .rev()
                    .find_map(|win| win.pixel(ix, iy).map(|c| (win, c)));
                let color = match top {
                    Some((win, _))
                        if playing
                            && std::ptr::eq(win, self.windows.get(PLAYER).unwrap_or(win))
                            && (vx..vx + vw).contains(&x)
                            && (vy..vy + vh).contains(&y) =>
                    {
                        self.video_frame.get(x - vx, y - vy)
                    }
                    Some((_, c)) => c,
                    None => self.wallpaper.get(x, y),
                };
                out.push(color);
            }
        }
        if let Some((px, py)) = self.pointer {
            draw_pointer(&mut out, self.w, px, py);
        }
        let (w, h) = (u16::try_from(self.w), u16::try_from(self.h));
        if let (Ok(w), Ok(h)) = (w, h) {
            self.screen = Screen::from_pixels(w, h, out)?;
        }
        Ok(())
    }

    /// 走一帧。返回画面是否变了；这一步画了哪里、搬了哪里见 [`Self::changes`]。
    pub fn step(&mut self, action: Action) -> Result<bool, Error> {
        self.frame = self.frame.wrapping_add(1);
        let size = self.size();
        let mut ch = Changes::empty(size);
        let changed = match action {
            Action::Idle => false,
            Action::Typing => {
                if self.frame.is_multiple_of(3) {
                    if let Some(editor) = self.windows.get_mut(EDITOR) {
                        let per_line = editor.w.saturating_sub(24) / GLYPH_W;
                        let (line, col) =
                            (self.typed / per_line.max(1), self.typed % per_line.max(1));
                        let seed = u64::from(self.typed) << 8 | 0x5a;
                        let (gx, gy) = (12 + col * GLYPH_W, 6 + (3 + line % 20) * LINE_H);
                        draw_glyph(&mut editor.doc, gx, gy, seed);
                        self.typed += 1;
                        // 字的点阵 5×8，抗锯齿再各占左右一列：在文档里是 (gx, gy) 起 6×8；换到屏幕上要减去滚动，
                        // 只算窗口内容区里看得见的部分
                        let glyph = Rect::clip(
                            editor.x + i32::try_from(gx).unwrap_or(0),
                            editor.y + i32::try_from(TITLE_H + gy).unwrap_or(0)
                                - i32::try_from(editor.scroll).unwrap_or(0),
                            6,
                            8,
                            size,
                        );
                        let content = Rect::clip(
                            editor.x,
                            editor.y + i32::try_from(TITLE_H).unwrap_or(0),
                            editor.w,
                            editor.h.saturating_sub(TITLE_H),
                            size,
                        );
                        ch.dirty
                            .extend(glyph.zip(content).and_then(|(g, c)| g.intersect(c)));
                    }
                    true
                } else {
                    false
                }
            }
            Action::Scrolling => {
                let (occluders, blockers) = (self.occluders(DOCUMENT), self.blockers());
                if let Some(d) = self.windows.get_mut(DOCUMENT) {
                    d.scroll = (d.scroll + 4) % d.doc.h.max(1);
                    // 内容区整块上移 4 行：窗口不动，新露出来的是底下一条
                    let content = (
                        d.x,
                        d.y + i32::try_from(TITLE_H).unwrap_or(0),
                        d.w,
                        d.h.saturating_sub(TITLE_H),
                    );
                    ch = layer_shift(size, content, content, (0, -4), &occluders, &blockers);
                }
                true
            }
            Action::Dragging => {
                let limit = i32::try_from(self.w / 3).unwrap_or(0);
                let (occluders, blockers) = (self.occluders(DOCUMENT), self.blockers());
                let dir = &mut self.drag_dir;
                if let Some(d) = self.windows.get_mut(DOCUMENT) {
                    let old = (d.x, d.y, d.w, d.h);
                    d.x += 8 * *dir;
                    if d.x > limit * 2 || d.x < limit / 4 {
                        *dir = -*dir;
                    }
                    // 整个窗口（标题栏加内容）往旁边平移；露出来的是它底下的东西
                    ch = layer_shift(
                        size,
                        old,
                        (d.x, d.y, d.w, d.h),
                        (d.x - old.0, 0),
                        &occluders,
                        &blockers,
                    );
                }
                true
            }
            Action::Switching => {
                // 第一次切换：把弹出的窗口放到最前面；之后保持不动。
                match self.windows.get_mut(POPUP) {
                    Some(popup) if !popup.visible => {
                        popup.visible = true;
                        ch.dirty
                            .extend(Rect::clip(popup.x, popup.y, popup.w, popup.h, size));
                        true
                    }
                    Some(_) | None => false,
                }
            }
            Action::Video => {
                self.render_video();
                let (vx, vy, vw, vh) = self.video;
                ch.dirty.extend(Rect::clip(
                    i32::try_from(vx).unwrap_or(0),
                    i32::try_from(vy).unwrap_or(0),
                    vw,
                    vh,
                    size,
                ));
                true
            }
            Action::Pointer => {
                // 在屏幕中间一块区域里来回移动
                let old = self.pointer;
                let (x, y) = self.pointer.unwrap_or((self.w / 3, self.h / 3));
                let (dx, dy) = self.pointer_dir;
                let bounce = |v: u32, d: i32, lo: u32, hi: u32| -> (u32, i32) {
                    let next = i64::from(v) + i64::from(d);
                    if next < i64::from(lo) || next > i64::from(hi) {
                        (v, -d)
                    } else {
                        (u32::try_from(next).unwrap_or(v), d)
                    }
                };
                let (x, dx) = bounce(x, dx, self.w / 4, self.w * 3 / 4);
                let (y, dy) = bounce(y, dy, self.h / 4, self.h * 3 / 4);
                self.pointer = Some((x, y));
                self.pointer_dir = (dx, dy);
                // 旧位置要把底下的画面露出来，新位置要画指针
                ch.dirty.extend(old.and_then(|p| self.pointer_box(p)));
                ch.dirty.extend(self.pointer_box((x, y)));
                true
            }
        };
        if changed {
            self.compose()?;
        }
        self.last = ch;
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]
    use super::*;

    #[test]
    fn deterministic_and_every_action_does_what_it_says() {
        let a = RealisticDesktop::new(640, 360, 3).unwrap();
        let b = RealisticDesktop::new(640, 360, 3).unwrap();
        assert_eq!(a.screen(), b.screen(), "同一个种子同一幅画面");
        assert!(RealisticDesktop::new(100, 100, 1).is_err());
        let mut d = a;
        for action in [
            Action::Idle,
            Action::Typing,
            Action::Scrolling,
            Action::Dragging,
            Action::Switching,
            Action::Video,
            Action::Pointer,
        ] {
            assert!(!action.name().is_empty());
            let before = d.screen().clone();
            let mut changed = 0;
            for _ in 0..6 {
                if d.step(action).unwrap() {
                    changed += 1;
                }
            }
            let differs = d.screen() != &before;
            match action {
                Action::Idle => assert!(changed == 0 && !differs),
                Action::Typing => assert!(changed == 2 && differs, "每 3 帧一个字"),
                Action::Switching => assert!(changed == 1 && differs, "只在第一帧突变"),
                Action::Scrolling | Action::Dragging | Action::Video | Action::Pointer => {
                    assert!(changed == 6 && differs);
                }
            }
        }
        // 再切换一次：已经在最前面了，不变
        assert!(!d.step(Action::Switching).unwrap());
        // 来回拖：方向会反过来
        for _ in 0..200 {
            d.step(Action::Dragging).unwrap();
        }
    }

    #[test]
    fn canvas_ignores_out_of_range() {
        let mut c = Canvas::new(4, 4, 1);
        c.set(9, 9, 5);
        assert_eq!(c.get(9, 9), 0);
        c.fill(2, 2, 10, 10, 7);
        assert_eq!(c.get(3, 3), 7);
        assert_eq!(gray(999), 0x00ff_ffff);
    }
    /// 区域的核对：画面实际变了的每一个像素都在“脏区域 ∪ 移动目的地”里（区域可以比实际大，不能漏），
    /// 每个移动区域里的像素确实是上一版 `from` 那块的拷贝。返回（区域面积，实际变了的像素数）。
    fn check_regions(before: &Screen, after: &Screen, ch: &Changes, what: &str) -> (u64, u64) {
        let (w, h) = (usize::from(after.width()), usize::from(after.height()));
        assert_eq!(ch.size, (after.width(), after.height()), "{what}");
        let mut mask = vec![false; w * h];
        for r in ch
            .dirty
            .iter()
            .copied()
            .chain(ch.moves.iter().map(|m| m.to))
        {
            assert!(
                r.right() <= u32::try_from(w).unwrap() && r.bottom() <= u32::try_from(h).unwrap()
            );
            for y in usize::from(r.y)..usize::from(r.y) + usize::from(r.h) {
                for x in usize::from(r.x)..usize::from(r.x) + usize::from(r.w) {
                    mask[y * w + x] = true;
                }
            }
        }
        let mut changed = 0u64;
        for (i, (a, b)) in before.pixels().iter().zip(after.pixels()).enumerate() {
            if a != b {
                changed += 1;
                assert!(
                    mask[i],
                    "{what}：像素 ({}, {}) 变了，区域没盖住",
                    i % w,
                    i / w
                );
            }
        }
        for m in &ch.moves {
            for dy in 0..usize::from(m.to.h) {
                for dx in 0..usize::from(m.to.w) {
                    let to = (usize::from(m.to.y) + dy) * w + usize::from(m.to.x) + dx;
                    let (fx, fy) = (usize::from(m.from.0) + dx, usize::from(m.from.1) + dy);
                    assert!(fx < w && fy < h, "{what}：移动的来源伸出了画面");
                    assert_eq!(
                        after.pixels()[to],
                        before.pixels()[fy * w + fx],
                        "{what}：移动区域 {m:?} 里 ({dx}, {dy}) 不是上一版的拷贝"
                    );
                }
            }
        }
        (ch.covered_area(), changed)
    }

    /// 走一串操作，每一步核对区域；返回（操作，区域面积，实际变了的像素数）。
    fn walk(desk: &mut RealisticDesktop, script: &[(Action, usize)]) -> Vec<(Action, u64, u64)> {
        let mut out = Vec::new();
        for (action, n) in script {
            for k in 0..*n {
                let before = desk.screen().clone();
                let moved = desk.step(*action).unwrap();
                let what = format!("{} 第 {k} 步", action.name());
                let (area, changed) = check_regions(&before, desk.screen(), desk.changes(), &what);
                if !moved {
                    assert!(
                        desk.changes().dirty.is_empty()
                            && desk.changes().moves.is_empty()
                            && changed == 0,
                        "{what}：画面没变，区域应当是空的"
                    );
                }
                out.push((*action, area, changed));
            }
        }
        out
    }

    /// 每一种操作交出的区域都盖得住实际变了的每个像素（逐像素比较），移动区域是上一版的拷贝。
    /// 指针出现之后窗口在它底下滚、拖；弹窗出现之后文档在它底下滚、拖；整套在两种尺寸上都走一遍。
    #[test]
    fn regions_cover_every_changed_pixel_for_every_action() {
        for (w, h, seed) in [(320, 240, 1), (480, 270, 3)] {
            let mut d = RealisticDesktop::new(w, h, seed).unwrap();
            let script = [
                (Action::Idle, 2),
                (Action::Typing, 30),
                (Action::Scrolling, 40),
                (Action::Dragging, 120),
                (Action::Video, 4),
                (Action::Pointer, 60),
                // 指针在，窗口在它底下滚、拖、打字
                (Action::Scrolling, 20),
                (Action::Dragging, 120),
                (Action::Typing, 30),
                (Action::Switching, 3),
                // 弹窗在最上面，文档在它底下滚、拖
                (Action::Scrolling, 20),
                (Action::Dragging, 120),
                (Action::Video, 3),
                (Action::Pointer, 10),
            ];
            let log = walk(&mut d, &script);
            // 每种操作都有实际变化的帧
            for a in [
                Action::Typing,
                Action::Scrolling,
                Action::Dragging,
                Action::Video,
                Action::Pointer,
                Action::Switching,
            ] {
                assert!(
                    log.iter().any(|(x, _, c)| *x == a && *c > 0),
                    "{} 没有实际变化的帧",
                    a.name()
                );
            }
        }
    }

    /// 滚动绕过文档末尾回到开头：内容按文档高度取余，移动区域照样是真的拷贝。
    #[test]
    fn scrolling_past_the_end_of_the_document_still_moves_rigidly() {
        let mut d = RealisticDesktop::new(320, 240, 2).unwrap();
        let log = walk(&mut d, &[(Action::Scrolling, 1_450)]);
        assert!(log.iter().all(|(_, _, c)| *c > 0));
    }

    /// 整屏滚动：移动区域占了整帧的一大半以上，露出来的一条是脏区域；窗口滚动占不了整帧。
    #[test]
    fn a_window_scroll_is_a_move_plus_a_strip_not_a_full_screen_move() {
        let mut d = RealisticDesktop::new(640, 360, 3).unwrap();
        let _ = d.step(Action::Scrolling).unwrap();
        let ch = d.changes();
        assert!(!ch.moves.is_empty() && !ch.dirty.is_empty());
        assert!(!ch.full_move());
        let moved: u64 = ch.moves.iter().map(|m| m.to.area()).sum();
        let strip: u64 = ch.dirty.iter().map(|r| r.area()).sum();
        assert!(moved > strip * 10, "{moved} {strip}");
        // 来源在目的地下面 4 行
        assert!(ch
            .moves
            .iter()
            .all(|m| m.from.1 == m.to.y + 4 && m.from.0 == m.to.x));
        // 拖窗口：旁边平移 8 像素，露出来的是竖着的一条
        let _ = d.step(Action::Dragging).unwrap();
        assert!(d
            .changes()
            .moves
            .iter()
            .all(|m| m.from.1 == m.to.y && m.from.0.abs_diff(m.to.x) == 8));
        // 打字：只有一个小块；指针：新旧两个位置；视频：整个视频区域；切窗口：整个弹窗
        let mut e = RealisticDesktop::new(640, 360, 3).unwrap();
        for _ in 0..3 {
            let _ = e.step(Action::Typing).unwrap();
        }
        assert!(e.changes().moves.is_empty() && e.changes().dirty.len() == 1);
        assert!(e.changes().dirty[0].area() <= 48);
        let _ = e.step(Action::Pointer).unwrap();
        assert_eq!(e.changes().dirty.len(), 1, "第一次出现只有新位置");
        let _ = e.step(Action::Pointer).unwrap();
        assert_eq!(e.changes().dirty.len(), 2);
        let _ = e.step(Action::Video).unwrap();
        assert_eq!(e.changes().dirty.len(), 1);
        let _ = e.step(Action::Switching).unwrap();
        assert!(e.changes().dirty[0].area() > 640 * 360 / 5);
        let _ = e.step(Action::Idle).unwrap();
        assert_eq!(e.changes().covered_area(), 0);
    }

    /// 报告：区域面积 ÷ 实际变了的像素数（越接近 1 越紧；小于 1 是漏，上面的测试不允许）。
    /// `cargo test -p peerdesk-e2e --release regions_tightness -- --nocapture --ignored` 打印分布。
    #[test]
    #[ignore = "报告：打印区域面积和实际变化像素数之比的分布"]
    #[allow(clippy::print_stdout)]
    fn regions_tightness_report() {
        let script = [
            (Action::Typing, 90),
            (Action::Scrolling, 60),
            (Action::Dragging, 120),
            (Action::Video, 20),
            (Action::Pointer, 60),
            (Action::Switching, 1),
            (Action::Scrolling, 60),
            (Action::Dragging, 120),
        ];
        let mut by_action: std::collections::BTreeMap<&str, Vec<f64>> = Default::default();
        for (w, h) in [(1280, 720), (1920, 1080)] {
            for seed in [1, 2] {
                let mut d = RealisticDesktop::new(w, h, seed).unwrap();
                for (a, area, changed) in walk(&mut d, &script) {
                    if changed > 0 {
                        by_action
                            .entry(a.name())
                            .or_default()
                            .push(area as f64 / changed as f64);
                    }
                }
            }
        }
        for (name, mut v) in by_action {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
            println!(
                "{name:<10} n={:<5} 中位 {:>7.2}  90% {:>7.2}  最大 {:>8.2}  最小 {:>5.2}",
                v.len(),
                q(0.5),
                q(0.9),
                q(1.0),
                q(0.0)
            );
        }
    }
}
