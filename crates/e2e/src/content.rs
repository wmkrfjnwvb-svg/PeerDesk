//! 接近真实的桌面画面：照片质感的壁纸、带抗锯齿“文字”的窗口、视频区域，
//! 以及打字、滚动、拖窗口、切换窗口、播放视频这些操作。
//!
//! 全部由种子决定，同一个种子每次生成的画面逐像素相同，测试结果可以复现。
//! 仿真里的纯色画面太“干净”，编码器在上面的表现比真实桌面好得多，所以要用这里的画面。

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
}

impl Action {
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
    frame: u32,
    typed: u32,
    drag_dir: i32,
    rng: Rng,
    screen: Screen,
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
            frame: 0,
            typed: 0,
            drag_dir: 1,
            rng,
            screen: Screen::new(width, height, 0),
        };
        desk.compose()?;
        Ok(desk)
    }

    /// 当前画面。
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        &self.screen
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
                        self.video_pixel(x - vx, y - vy)
                    }
                    Some((_, c)) => c,
                    None => self.wallpaper.get(x, y),
                };
                out.push(color);
            }
        }
        let (w, h) = (u16::try_from(self.w), u16::try_from(self.h));
        if let (Ok(w), Ok(h)) = (w, h) {
            self.screen = Screen::from_pixels(w, h, out)?;
        }
        Ok(())
    }

    /// 走一帧。返回画面是否变了。
    pub fn step(&mut self, action: Action) -> Result<bool, Error> {
        self.frame = self.frame.wrapping_add(1);
        let changed = match action {
            Action::Idle => false,
            Action::Typing => {
                if self.frame % 3 == 0 {
                    if let Some(editor) = self.windows.get_mut(EDITOR) {
                        let per_line = editor.w.saturating_sub(24) / GLYPH_W;
                        let (line, col) =
                            (self.typed / per_line.max(1), self.typed % per_line.max(1));
                        let seed = u64::from(self.typed) << 8 | 0x5a;
                        draw_glyph(
                            &mut editor.doc,
                            12 + col * GLYPH_W,
                            6 + (3 + line % 20) * LINE_H,
                            seed,
                        );
                        self.typed += 1;
                    }
                    true
                } else {
                    false
                }
            }
            Action::Scrolling => {
                let doc = self.windows.get_mut(DOCUMENT).into_iter();
                doc.for_each(|d| d.scroll = (d.scroll + 4) % d.doc.h.max(1));
                true
            }
            Action::Dragging => {
                let limit = i32::try_from(self.w / 3).unwrap_or(0);
                let dir = &mut self.drag_dir;
                self.windows.get_mut(DOCUMENT).into_iter().for_each(|d| {
                    d.x += 8 * *dir;
                    if d.x > limit * 2 || d.x < limit / 4 {
                        *dir = -*dir;
                    }
                });
                true
            }
            Action::Switching => {
                // 第一次切换：把弹出的窗口放到最前面；之后保持不动。
                match self.windows.get_mut(POPUP) {
                    Some(popup) if !popup.visible => {
                        popup.visible = true;
                        true
                    }
                    Some(_) | None => false,
                }
            }
            Action::Video => true,
        };
        if changed {
            self.compose()?;
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
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
                Action::Scrolling | Action::Dragging | Action::Video => {
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
}
