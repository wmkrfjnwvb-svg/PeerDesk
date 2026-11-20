//! 画面缓冲区与合成桌面。
//!
//! [`Screen`] 是采集、编码、解码、显示之间传递画面的统一格式。
//! [`SyntheticDesktop`] 按固定种子生成会变化的桌面画面（打字、滚动、拖动窗口、播放视频），
//! 让编解码和传输可以在没有真实屏幕的环境里（仿真、CI）用真实的画面内容测试。
//! 真实的屏幕采集接入后产出同样的 [`Screen`]，后面的环节不变。

use crate::sim::Rng;

/// 一幅画面。像素是 `0x00RRGGBB`，按行存放。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Screen {
    width: u16,
    height: u16,
    pixels: Vec<u32>,
}

impl Screen {
    /// 创建一幅纯色画面。
    #[must_use]
    pub fn new(width: u16, height: u16, color: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![color & 0x00ff_ffff; usize::from(width) * usize::from(height)],
        }
    }

    /// 用现成的像素创建。像素数必须等于宽乘高；每个像素只取低 24 位。
    pub fn from_pixels(
        width: u16,
        height: u16,
        mut pixels: Vec<u32>,
    ) -> Result<Self, crate::Error> {
        if pixels.len() != usize::from(width) * usize::from(height) {
            return Err(crate::Error::Inconsistent("像素数与画面尺寸不符"));
        }
        for p in &mut pixels {
            *p &= 0x00ff_ffff;
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// 宽度（像素）。
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.width
    }

    /// 高度（像素）。
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.height
    }

    /// 全部像素，按行存放。
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    /// 一行里从 `x` 开始的 `len` 个像素；越界部分被截掉。
    #[must_use]
    pub fn row(&self, x: u16, y: u16, len: u16) -> &[u32] {
        let w = usize::from(self.width);
        let start = usize::from(y) * w + usize::from(x.min(self.width));
        let end = start + usize::from(len.min(self.width.saturating_sub(x)));
        self.pixels.get(start..end).unwrap_or_default()
    }

    /// 一行里从 `x` 开始的 `len` 个像素（可写）；越界部分被截掉。
    pub fn row_mut(&mut self, x: u16, y: u16, len: u16) -> &mut [u32] {
        let w = usize::from(self.width);
        let start = usize::from(y) * w + usize::from(x.min(self.width));
        let end = start + usize::from(len.min(self.width.saturating_sub(x)));
        self.pixels.get_mut(start..end).unwrap_or_default()
    }

    /// 把一个矩形填成纯色；越界部分被截掉。
    pub fn fill(&mut self, x: u16, y: u16, w: u16, h: u16, color: u32) {
        for row in y..y.saturating_add(h).min(self.height) {
            self.row_mut(x, row, w).fill(color & 0x00ff_ffff);
        }
    }

    /// 与另一幅同尺寸画面不同的像素数；尺寸不同时返回全部像素数。
    #[must_use]
    pub fn diff(&self, other: &Screen) -> usize {
        if self.width != other.width || self.height != other.height {
            return self.pixels.len().max(other.pixels.len());
        }
        self.pixels
            .iter()
            .zip(&other.pixels)
            .filter(|(a, b)| a != b)
            .count()
    }
}

/// 桌面上正在发生的事。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Workload {
    /// 什么都不动。
    Idle,
    /// 在文本区里打字：每帧多一个字符。
    Typing,
    /// 文本区向上滚动一行并出现新的一行。
    Scrolling,
    /// 一个窗口在桌面上移动。
    Dragging,
    /// 一块区域在播放视频：每帧内容都变。
    Video,
}

const BACKGROUND: u32 = 0x1e_1e_2e;
const PAPER: u32 = 0xfa_fa_f5;
const INK: u32 = 0x20_20_20;
const WINDOW: u32 = 0x3b_82_f6;
const TITLE: u32 = 0x1d_4e_d8;
const GLYPH_W: u16 = 8;
const GLYPH_H: u16 = 14;
const LINE_H: u16 = 18;

/// 按种子生成的合成桌面。同一种子、同一串操作得到完全相同的画面序列。
#[derive(Debug, Clone)]
pub struct SyntheticDesktop {
    screen: Screen,
    rng: Rng,
    /// 文本区的位置和大小。
    text: (u16, u16, u16, u16),
    cursor: (u16, u16),
    /// 可拖动窗口的位置。
    window: (u16, u16),
    /// 窗口移动的方向。
    velocity: (i16, i16),
    /// 视频区的位置和大小。
    video: (u16, u16, u16, u16),
    tick: u32,
}

impl SyntheticDesktop {
    /// 创建。画面至少 160×120。
    #[must_use]
    pub fn new(width: u16, height: u16, seed: u64) -> Self {
        let (width, height) = (width.max(160), height.max(120));
        let mut screen = Screen::new(width, height, BACKGROUND);
        // 左半边是文本区，右上是视频区。
        let text = (16, 16, width / 2 - 24, height - 32);
        let video = (width / 2 + 8, 16, width / 2 - 24, height / 2 - 24);
        screen.fill(text.0, text.1, text.2, text.3, PAPER);
        screen.fill(video.0, video.1, video.2, video.3, 0);
        let mut desktop = Self {
            screen,
            rng: Rng::new(seed),
            text,
            cursor: (0, 0),
            window: (width / 2 + 16, height / 2 + 8),
            velocity: (7, 3),
            video,
            tick: 0,
        };
        desktop.draw_window(WINDOW, TITLE);
        desktop
    }

    /// 当前画面。
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        &self.screen
    }

    fn window_size(&self) -> (u16, u16) {
        (self.screen.width() / 5, self.screen.height() / 5)
    }

    fn draw_window(&mut self, body: u32, title: u32) {
        let (w, h) = self.window_size();
        let (x, y) = self.window;
        self.screen.fill(x, y, w, h, body);
        self.screen.fill(x, y, w, 10.min(h), title);
    }

    /// 画一个字符：一个随机的 8×14 点阵，看起来像文字，颜色只有纸和墨两种。
    fn draw_glyph(&mut self) {
        let (tx, ty, tw, th) = self.text;
        let cols = (tw / GLYPH_W).max(1);
        let lines = (th / LINE_H).max(1);
        if self.cursor.0 >= cols {
            self.cursor = (0, self.cursor.1 + 1);
        }
        if self.cursor.1 >= lines {
            self.scroll_text();
            self.cursor = (0, lines - 1);
        }
        let x = tx + self.cursor.0 * GLYPH_W;
        let y = ty + self.cursor.1 * LINE_H;
        for row in 2..GLYPH_H - 2 {
            let bits = self.rng.next_u64();
            for (col, pixel) in self
                .screen
                .row_mut(x + 1, y + row, GLYPH_W - 2)
                .iter_mut()
                .enumerate()
            {
                *pixel = if bits >> col & 1 == 1 { INK } else { PAPER };
            }
        }
        self.cursor.0 += 1;
    }

    /// 文本区整体上移一行，最后一行清空。
    fn scroll_text(&mut self) {
        let (tx, ty, tw, th) = self.text;
        let lines = (th / LINE_H).max(1);
        for y in ty..ty + (lines - 1) * LINE_H {
            let below: Vec<u32> = self.screen.row(tx, y + LINE_H, tw).to_vec();
            self.screen.row_mut(tx, y, tw).copy_from_slice(&below);
        }
        self.screen
            .fill(tx, ty + (lines - 1) * LINE_H, tw, LINE_H, PAPER);
    }

    fn drag_window(&mut self) {
        let (w, h) = self.window_size();
        self.draw_window(BACKGROUND, BACKGROUND);
        // 窗口只在右下的空白区域里移动，碰到边就反弹。
        let min = (self.screen.width() / 2 + 8, self.screen.height() / 2 + 4);
        let max = (
            self.screen.width().saturating_sub(w + 4),
            self.screen.height().saturating_sub(h + 4),
        );
        let step = |pos: u16, v: &mut i16, lo: u16, hi: u16| {
            let next = i32::from(pos) + i32::from(*v);
            if next < i32::from(lo) || next > i32::from(hi) {
                *v = -*v;
            }
            let moved = i32::from(pos) + i32::from(*v);
            u16::try_from(moved.clamp(i32::from(lo), i32::from(hi.max(lo)))).unwrap_or(lo)
        };
        self.window.0 = step(self.window.0, &mut self.velocity.0, min.0, max.0);
        self.window.1 = step(self.window.1, &mut self.velocity.1, min.1, max.1);
        self.draw_window(WINDOW, TITLE);
    }

    /// 视频区：平滑的渐变加一点噪声，每帧整体移动，像真实视频那样难压缩。
    fn play_video(&mut self) {
        let (vx, vy, vw, vh) = self.video;
        let t = self.tick;
        for y in 0..vh {
            let noise = self.rng.next_u64();
            for (x, pixel) in self.screen.row_mut(vx, vy + y, vw).iter_mut().enumerate() {
                let x = u32::try_from(x).unwrap_or(0);
                let r = (x + t * 3) & 0xff;
                let g = (u32::from(y) * 2 + t) & 0xff;
                let b = ((x + u32::from(y)) / 2
                    + (u32::try_from(noise >> (x % 60) & 7).unwrap_or(0)))
                    & 0xff;
                *pixel = r << 16 | g << 8 | b;
            }
        }
    }

    /// 前进一帧。
    pub fn step(&mut self, workload: Workload) {
        self.tick = self.tick.wrapping_add(1);
        match workload {
            Workload::Idle => {}
            Workload::Typing => self.draw_glyph(),
            Workload::Scrolling => {
                self.scroll_text();
                let lines = (self.text.3 / LINE_H).max(1);
                self.cursor = (0, lines - 1);
                for _ in 0..(self.text.2 / GLYPH_W).max(1) / 2 {
                    self.draw_glyph();
                }
            }
            Workload::Dragging => self.drag_window(),
            Workload::Video => self.play_video(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_pixels_checks_the_size_and_masks_alpha() {
        assert!(Screen::from_pixels(2, 2, vec![0; 3]).is_err());
        let s = Screen::from_pixels(1, 1, vec![0xff12_3456]).unwrap();
        assert_eq!(s.pixels(), &[0x0012_3456]);
    }

    #[test]
    fn screen_basics_and_clipping() {
        let mut s = Screen::new(10, 4, 0xff00_0000 | 0x11_22_33);
        assert_eq!((s.width(), s.height()), (10, 4));
        assert_eq!(s.pixels().len(), 40);
        assert!(
            s.pixels().iter().all(|p| *p == 0x11_22_33),
            "最高字节被清掉"
        );
        s.fill(8, 3, 5, 5, 0xabcdef);
        assert_eq!(
            s.row(0, 3, 10),
            &[
                0x11_22_33, 0x11_22_33, 0x11_22_33, 0x11_22_33, 0x11_22_33, 0x11_22_33, 0x11_22_33,
                0x11_22_33, 0xabcdef, 0xabcdef
            ]
        );
        assert_eq!(s.row(8, 3, 100).len(), 2);
        assert!(s.row(10, 0, 5).is_empty());
        assert!(s.row(50, 0, 5).is_empty());
        assert!(s.row(0, 9, 5).is_empty());
        assert!(s.row_mut(0, 9, 5).is_empty());
        s.fill(20, 20, 5, 5, 1); // 完全在外面：无事发生
        let clean = Screen::new(10, 4, 0x11_22_33);
        assert_eq!(s.diff(&clean), 2);
        assert_eq!(s.diff(&s), 0);
        assert_eq!(s.diff(&Screen::new(5, 4, 0)), 40);
    }

    #[test]
    fn same_seed_same_frames_and_minimum_size() {
        let run = |seed| {
            let mut d = SyntheticDesktop::new(320, 240, seed);
            for w in [
                Workload::Typing,
                Workload::Scrolling,
                Workload::Dragging,
                Workload::Video,
            ] {
                for _ in 0..5 {
                    d.step(w);
                }
            }
            d.screen().clone()
        };
        assert_eq!(run(1), run(1));
        assert_ne!(run(1), run(2));
        let tiny = SyntheticDesktop::new(1, 1, 0);
        assert_eq!((tiny.screen().width(), tiny.screen().height()), (160, 120));
    }

    #[test]
    fn each_workload_changes_what_it_should() {
        let mut d = SyntheticDesktop::new(640, 360, 7);
        let before = d.screen().clone();
        d.step(Workload::Idle);
        assert_eq!(d.screen().diff(&before), 0);

        d.step(Workload::Typing);
        let typed = d.screen().diff(&before);
        assert!(typed > 0 && typed <= 60, "一个字符只改几十个像素：{typed}");

        let before = d.screen().clone();
        d.step(Workload::Dragging);
        let dragged = d.screen().diff(&before);
        assert!(dragged > 100, "{dragged}");

        let before = d.screen().clone();
        d.step(Workload::Video);
        let (.., vw, vh) = d.video;
        let video = d.screen().diff(&before);
        assert!(video > usize::from(vw) * usize::from(vh) / 2, "{video}");

        let before = d.screen().clone();
        d.step(Workload::Scrolling);
        assert!(d.screen().diff(&before) > 0);
    }

    #[test]
    fn typing_wraps_lines_and_scrolls_at_the_bottom() {
        let mut d = SyntheticDesktop::new(160, 120, 3);
        let cols = d.text.2 / GLYPH_W;
        let lines = d.text.3 / LINE_H;
        for _ in 0..u32::from(cols) * u32::from(lines) + 3 {
            d.step(Workload::Typing);
        }
        assert_eq!(d.cursor.1, lines - 1, "写满后停在最后一行");
        // 窗口拖很久也不会跑出画面
        for _ in 0..2000 {
            d.step(Workload::Dragging);
        }
        let (w, h) = d.window_size();
        assert!(d.window.0 + w <= d.screen().width());
        assert!(d.window.1 + h <= d.screen().height());
    }
}
