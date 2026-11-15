//! 分块编解码器：没有硬件编码器时的通用软件方案。
//!
//! 画面切成 32×32 的块。每一帧只编码“需要发送”的块，每个块自成一体（不依赖别的帧），
//! 所以丢了一帧不会让后面的帧解不出来，只会让那几个块暂时停留在旧内容；
//! 接收端报告丢帧后，编码器把那些块重新标记为待发送，发的是它们**当前**的内容。
//!
//! 它直接实现了技术方案里的三条规则：
//! - **静止不发**：没有块需要发送时不产生任何数据。
//! - **每帧字节预算**：一帧的大小不超过给定预算；装不下时先降画质，再不够就分几帧发。
//! - **渐进清晰**：降了画质的块在预算有富余时会自动以无损重发，最终画面与源完全一致。
//!
//! 文字和界面（颜色少、大片纯色）压缩得很好；视频内容压缩率远不如 H.264 这类编码器，
//! 那是硬件编码器接入后的事。

use std::collections::VecDeque;

use crate::screen::Screen;
use crate::wire::{Reader, Writer};
use crate::Error;

/// 块的边长（像素）。
pub const TILE: u16 = 32;
/// 码流版本。
const VERSION: u8 = 1;
/// 帧头长度：版本(1) + 宽(2) + 高(2) + 块数(2)。
pub const FRAME_HEADER: usize = 7;
/// 调色板模式最多的颜色数。
const MAX_PALETTE: usize = 16;
/// 编码器记住最近多少帧里各发了哪些块，用于丢帧后重发。
const LOG_FRAMES: usize = 128;

/// 画质档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    /// 无损。
    Lossless,
    /// 每个颜色通道保留高 5 位。
    Reduced,
    /// 每个颜色通道保留高 4 位，并且横竖各减半采样。
    Coarse,
}

impl Quality {
    const ALL: [Quality; 3] = [Quality::Lossless, Quality::Reduced, Quality::Coarse];

    const fn id(self) -> u8 {
        match self {
            Quality::Lossless => 0,
            Quality::Reduced => 1,
            Quality::Coarse => 2,
        }
    }

    const fn from_id(id: u8) -> Result<Self, Error> {
        match id {
            0 => Ok(Quality::Lossless),
            1 => Ok(Quality::Reduced),
            2 => Ok(Quality::Coarse),
            _ => Err(Error::Inconsistent("未知的画质档位")),
        }
    }

    const fn mask(self) -> u32 {
        match self {
            Quality::Lossless => 0x00ff_ffff,
            Quality::Reduced => 0x00f8_f8f8,
            Quality::Coarse => 0x00f0_f0f0,
        }
    }

    const fn step(self) -> u16 {
        match self {
            Quality::Lossless | Quality::Reduced => 1,
            Quality::Coarse => 2,
        }
    }
}

/// 块的位置和大小。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Grid {
    width: u16,
    height: u16,
    cols: u16,
    rows: u16,
}

impl Grid {
    fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            cols: width.div_ceil(TILE),
            rows: height.div_ceil(TILE),
        }
    }

    fn count(&self) -> usize {
        usize::from(self.cols) * usize::from(self.rows)
    }

    fn rect(&self, index: u16) -> Option<Rect> {
        if self.cols == 0 || usize::from(index) >= self.count() {
            return None;
        }
        let (x, y) = ((index % self.cols) * TILE, (index / self.cols) * TILE);
        Some(Rect {
            x,
            y,
            w: TILE.min(self.width - x),
            h: TILE.min(self.height - y),
        })
    }
}

/// 取出一个块在某个画质下的像素（降采样时取每个 2×2 的左上角）。
fn sample(screen: &Screen, rect: Rect, quality: Quality) -> Vec<u32> {
    let step = usize::from(quality.step());
    let mask = quality.mask();
    let mut out = Vec::with_capacity(usize::from(rect.w) * usize::from(rect.h) / (step * step));
    for y in (rect.y..rect.y + rect.h).step_by(step) {
        out.extend(
            screen
                .row(rect.x, y, rect.w)
                .iter()
                .step_by(step)
                .map(|p| p & mask),
        );
    }
    out
}

fn sampled_len(rect: Rect, quality: Quality) -> usize {
    let step = quality.step();
    usize::from(rect.w.div_ceil(step)) * usize::from(rect.h.div_ceil(step))
}

fn tile_hash(screen: &Screen, rect: Rect) -> u64 {
    // FNV-1a。只用来判断“这个块变了没有”，不用于安全目的。
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for y in rect.y..rect.y + rect.h {
        for pixel in screen.row(rect.x, y, rect.w) {
            hash = (hash ^ u64::from(*pixel)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    // 0 留给“还没算过”。
    hash | 1
}

fn put_color(w: &mut Writer, color: u32) {
    let [_, r, g, b] = color.to_be_bytes();
    w.u8(r).u8(g).u8(b);
}

fn get_color(r: &mut Reader<'_>) -> Result<u32, Error> {
    let bytes = r.take(3)?;
    Ok(bytes.iter().fold(0u32, |acc, b| acc << 8 | u32::from(*b)))
}

const MODE_SOLID: u8 = 0;
const MODE_PALETTE: u8 = 1;
const MODE_RUNS: u8 = 2;
const MODE_RAW: u8 = 3;

const fn palette_bits(colors: usize) -> usize {
    if colors <= 2 {
        1
    } else if colors <= 4 {
        2
    } else {
        4
    }
}

/// 把一个块的像素编码成最省的形式，写入 `w`（不含块序号和模式字节之前的部分）。
fn encode_pixels(w: &mut Writer, quality: Quality, pixels: &[u32]) {
    let mut palette: Vec<u32> = Vec::with_capacity(MAX_PALETTE + 1);
    let mut runs = 0usize;
    let mut previous = None;
    for pixel in pixels {
        if palette.len() <= MAX_PALETTE && !palette.contains(pixel) {
            palette.push(*pixel);
        }
        if previous != Some(*pixel) {
            runs += 1;
            previous = Some(*pixel);
        }
    }
    // 一段最多 255 个像素，长的段要拆开。
    let run_records = runs + pixels.len() / 255;
    let raw_cost = pixels.len() * 3;
    let runs_cost = 2 + run_records * 4;
    let palette_cost = if palette.len() <= MAX_PALETTE {
        1 + palette.len() * 3 + (pixels.len() * palette_bits(palette.len())).div_ceil(8)
    } else {
        usize::MAX
    };
    let mode_byte = |mode: u8| mode | quality.id() << 4;

    if let [only] = palette.as_slice() {
        w.u8(mode_byte(MODE_SOLID));
        put_color(w, *only);
    } else if palette_cost <= runs_cost && palette_cost <= raw_cost {
        w.u8(mode_byte(MODE_PALETTE));
        w.u8(u8::try_from(palette.len()).unwrap_or(0));
        for color in &palette {
            put_color(w, *color);
        }
        let bits = palette_bits(palette.len());
        let (mut acc, mut filled) = (0u8, 0usize);
        for pixel in pixels {
            let index = palette.iter().position(|c| c == pixel).unwrap_or(0);
            acc |= u8::try_from(index).unwrap_or(0) << filled;
            filled += bits;
            if filled == 8 {
                w.u8(acc);
                (acc, filled) = (0, 0);
            }
        }
        if filled > 0 {
            w.u8(acc);
        }
    } else if runs_cost <= raw_cost {
        w.u8(mode_byte(MODE_RUNS));
        let mut records: Vec<(u8, u32)> = Vec::with_capacity(run_records);
        for pixel in pixels {
            match records.last_mut() {
                Some((len, color)) if color == pixel && *len < 255 => *len += 1,
                Some(_) | None => records.push((1, *pixel)),
            }
        }
        w.u16(u16::try_from(records.len()).unwrap_or(u16::MAX));
        for (len, color) in records {
            w.u8(len);
            put_color(w, color);
        }
    } else {
        w.u8(mode_byte(MODE_RAW));
        for pixel in pixels {
            put_color(w, *pixel);
        }
    }
}

fn decode_pixels(r: &mut Reader<'_>, mode: u8, count: usize) -> Result<Vec<u32>, Error> {
    let mut out = Vec::with_capacity(count);
    match mode {
        MODE_SOLID => out.resize(count, get_color(r)?),
        MODE_PALETTE => {
            let n = usize::from(r.u8()?);
            if !(2..=MAX_PALETTE).contains(&n) {
                return Err(Error::Inconsistent("调色板大小不合法"));
            }
            let mut palette = Vec::with_capacity(n);
            for _ in 0..n {
                palette.push(get_color(r)?);
            }
            let bits = palette_bits(n);
            let packed = r.take((count * bits).div_ceil(8))?;
            let mask = (1u8 << bits) - 1;
            for i in 0..count {
                let byte = packed.get(i * bits / 8).copied().unwrap_or(0);
                let index = usize::from(byte >> (i * bits % 8) & mask);
                let color = palette
                    .get(index)
                    .ok_or(Error::Inconsistent("调色板索引越界"))?;
                out.push(*color);
            }
        }
        MODE_RUNS => {
            let records = usize::from(r.u16()?);
            for _ in 0..records {
                let len = usize::from(r.u8()?);
                let color = get_color(r)?;
                if len == 0 || out.len() + len > count {
                    return Err(Error::Inconsistent("游程长度不合法"));
                }
                out.resize(out.len() + len, color);
            }
            if out.len() != count {
                return Err(Error::Inconsistent("游程总长度与块大小不符"));
            }
        }
        MODE_RAW => {
            for _ in 0..count {
                out.push(get_color(r)?);
            }
        }
        _ => return Err(Error::Inconsistent("未知的块编码模式")),
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy)]
struct TileState {
    /// 源画面里这个块最近一次的哈希；0 表示还没看过。
    hash: u64,
    /// 需要发送：内容变了，或者上次发的那一帧丢了。
    dirty: bool,
    /// 接收端现在拿到的画质；`None` 表示从没发过。
    sent: Option<Quality>,
    /// 最近一次发送它的帧号。
    last_frame: u32,
}

/// 一次编码的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// 码流。
    pub data: Vec<u8>,
    /// 这一帧包含的块数。
    pub tiles: usize,
    /// 变化的块用的画质。
    pub quality: Quality,
    /// 这一帧之后仍然待发送或待补清晰的块数。
    pub pending: usize,
}

/// 编码器。
#[derive(Debug, Clone)]
pub struct Encoder {
    grid: Grid,
    tiles: Vec<TileState>,
    log: VecDeque<(u32, Vec<u16>)>,
    /// 下一帧从哪个块开始挑，轮转，避免靠后的块总是排不上。
    cursor: usize,
}

impl Encoder {
    /// 创建。第一帧会发送整幅画面。
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        let grid = Grid::new(width, height);
        Self {
            grid,
            tiles: vec![
                TileState {
                    hash: 0,
                    dirty: true,
                    sent: None,
                    last_frame: 0,
                };
                grid.count()
            ],
            log: VecDeque::new(),
            cursor: 0,
        }
    }

    /// 待发送或待补清晰的块数。
    #[must_use]
    pub fn pending(&self) -> usize {
        self.tiles
            .iter()
            .filter(|t| t.dirty || t.sent != Some(Quality::Lossless))
            .count()
    }

    /// 把所有块标记为待发送（例如换了观看者）。
    pub fn request_full(&mut self) {
        for tile in &mut self.tiles {
            tile.dirty = true;
        }
    }

    /// 接收端的反馈：帧号不大于 `floor` 的帧都已有定论，`mask` 的第 i 位表示第 `floor - i` 帧已送达。
    /// 没送达的帧里的块会被重新标记为待发送。
    pub fn on_feedback(&mut self, floor: u32, mask: u32) {
        while let Some((frame_id, _)) = self.log.front() {
            if *frame_id > floor {
                break;
            }
            let Some((frame_id, tiles)) = self.log.pop_front() else {
                break;
            };
            let age = floor - frame_id;
            let delivered = age < 32 && mask >> age & 1 == 1;
            if !delivered {
                self.redirty(frame_id, &tiles);
            }
        }
    }

    fn redirty(&mut self, frame_id: u32, tiles: &[u16]) {
        for index in tiles {
            if let Some(tile) = self.tiles.get_mut(usize::from(*index)) {
                // 这个块之后又发过的话，以那一次的结果为准。
                if tile.last_frame == frame_id {
                    tile.dirty = true;
                    tile.sent = None;
                }
            }
        }
    }

    /// 发送端自己判定某一帧丢了（例如迟迟等不到它的反馈）：把它带的块重新标记为待发送。
    /// 这一帧之后即使送达了也没有坏处，只是多发了一次。
    pub fn mark_lost(&mut self, frame_id: u32) {
        if let Some(at) = self.log.iter().position(|(id, _)| *id == frame_id) {
            if let Some((_, tiles)) = self.log.remove(at) {
                self.redirty(frame_id, &tiles);
            }
        }
    }

    fn encode_tile(&self, screen: &Screen, index: u16, quality: Quality) -> Vec<u8> {
        let mut w = Writer::new();
        if let Some(rect) = self.grid.rect(index) {
            w.u16(index);
            encode_pixels(&mut w, quality, &sample(screen, rect, quality));
        }
        w.into_vec()
    }

    /// 编码一帧，大小不超过 `budget` 字节（但至少会发一个块，保证总有进展）。
    /// 没有任何块需要发送时返回 `None`。画面尺寸与创建时不同也返回 `None`。
    pub fn encode(&mut self, frame_id: u32, screen: &Screen, budget: usize) -> Option<Encoded> {
        if screen.width() != self.grid.width || screen.height() != self.grid.height {
            return None;
        }
        let count = self.tiles.len();
        // 1. 找出内容变了的块。
        for index in 0..count {
            let rect = u16::try_from(index).ok().and_then(|i| self.grid.rect(i))?;
            let hash = tile_hash(screen, rect);
            if let Some(tile) = self.tiles.get_mut(index) {
                if tile.hash != hash {
                    tile.hash = hash;
                    tile.dirty = true;
                }
            }
        }
        let order = (0..count).map(|i| (i + self.cursor) % count);
        let dirty: Vec<u16> = order
            .clone()
            .filter(|i| self.tiles.get(*i).is_some_and(|t| t.dirty))
            .filter_map(|i| u16::try_from(i).ok())
            .collect();
        let refine: Vec<u16> = order
            .filter(|i| {
                self.tiles
                    .get(*i)
                    .is_some_and(|t| !t.dirty && t.sent.is_some_and(|q| q != Quality::Lossless))
            })
            .filter_map(|i| u16::try_from(i).ok())
            .collect();
        if dirty.is_empty() && refine.is_empty() {
            return None;
        }

        // 2. 给变化的块选画质：能装下的最高画质；最低画质也装不下就分几帧发。
        let room = budget.saturating_sub(FRAME_HEADER);
        let mut chosen: Vec<(u16, Quality, Vec<u8>)> = Vec::new();
        let mut quality = Quality::Lossless;
        for q in Quality::ALL {
            quality = q;
            let encoded: Vec<Vec<u8>> = dirty
                .iter()
                .map(|i| self.encode_tile(screen, *i, q))
                .collect();
            let total: usize = encoded.iter().map(Vec::len).sum();
            let last = q == Quality::Coarse;
            if total <= room || last {
                let mut used = 0usize;
                for (index, bytes) in dirty.iter().zip(encoded) {
                    if used + bytes.len() > room && !chosen.is_empty() {
                        break;
                    }
                    used += bytes.len();
                    chosen.push((*index, q, bytes));
                }
                break;
            }
        }
        // 3. 预算有富余：把之前降了画质的块补成无损。
        let mut used: usize = chosen.iter().map(|(_, _, b)| b.len()).sum();
        if chosen.len() == dirty.len() {
            for index in refine {
                let bytes = self.encode_tile(screen, index, Quality::Lossless);
                if used + bytes.len() > room && !chosen.is_empty() {
                    break;
                }
                used += bytes.len();
                chosen.push((index, Quality::Lossless, bytes));
            }
        }

        // 4. 组帧，更新状态。
        let mut w = Writer::new();
        w.u8(VERSION)
            .u16(self.grid.width)
            .u16(self.grid.height)
            .u16(u16::try_from(chosen.len()).unwrap_or(u16::MAX));
        let mut sent = Vec::with_capacity(chosen.len());
        for (index, q, bytes) in &chosen {
            w.bytes(bytes);
            if let Some(tile) = self.tiles.get_mut(usize::from(*index)) {
                tile.dirty = false;
                tile.sent = Some(*q);
                tile.last_frame = frame_id;
            }
            sent.push(*index);
        }
        self.cursor = sent
            .last()
            .map_or(self.cursor, |i| (usize::from(*i) + 1) % count);
        if self.log.len() >= LOG_FRAMES {
            self.log.pop_front();
        }
        self.log.push_back((frame_id, sent));
        Some(Encoded {
            data: w.into_vec(),
            tiles: chosen.len(),
            quality,
            pending: self.pending(),
        })
    }
}

/// 解码器：把收到的帧应用到自己的画面上。
#[derive(Debug, Clone)]
pub struct Decoder {
    grid: Grid,
    screen: Screen,
}

impl Decoder {
    /// 创建，初始画面全黑。
    #[must_use]
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            grid: Grid::new(width, height),
            screen: Screen::new(width, height, 0),
        }
    }

    /// 当前画面。
    #[must_use]
    pub const fn screen(&self) -> &Screen {
        &self.screen
    }

    /// 应用一帧，返回更新的块数。
    ///
    /// 码流来自网络，按不可信输入处理：任何不合法的地方都返回错误，
    /// 而且出错的帧整帧不生效，不会留下应用了一半的画面。
    pub fn apply(&mut self, data: &[u8]) -> Result<usize, Error> {
        let mut r = Reader::new(data);
        let version = r.u8()?;
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let (width, height) = (r.u16()?, r.u16()?);
        if width != self.grid.width || height != self.grid.height {
            return Err(Error::Inconsistent("画面尺寸与解码器不一致"));
        }
        let count = usize::from(r.u16()?);
        if count > self.grid.count() {
            return Err(Error::TooManyItems {
                len: count,
                max: self.grid.count(),
            });
        }
        let mut tiles: Vec<(Rect, Quality, Vec<u32>)> = Vec::with_capacity(count);
        for _ in 0..count {
            let index = r.u16()?;
            let rect = self
                .grid
                .rect(index)
                .ok_or(Error::Inconsistent("块序号越界"))?;
            let mode_byte = r.u8()?;
            let quality = Quality::from_id(mode_byte >> 4)?;
            let pixels = decode_pixels(&mut r, mode_byte & 0x0f, sampled_len(rect, quality))?;
            tiles.push((rect, quality, pixels));
        }
        r.finish()?;

        for (rect, quality, pixels) in &tiles {
            let step = usize::from(quality.step());
            let sampled_w = usize::from(rect.w.div_ceil(quality.step()));
            for dy in 0..rect.h {
                let source_row = usize::from(dy) / step * sampled_w;
                let row = self.screen.row_mut(rect.x, rect.y + dy, rect.w);
                for (dx, pixel) in row.iter_mut().enumerate() {
                    *pixel = pixels.get(source_row + dx / step).copied().unwrap_or(0);
                }
            }
        }
        Ok(tiles.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::{SyntheticDesktop, Workload};
    use crate::sim::Rng;

    const BIG: usize = usize::MAX / 2;

    /// 编码并立刻解码，直到编码器没有东西可发。返回 (帧数, 总字节数)。
    fn drain(
        enc: &mut Encoder,
        dec: &mut Decoder,
        screen: &Screen,
        budget: usize,
        first_id: u32,
    ) -> (u32, usize) {
        let (mut frames, mut bytes) = (0u32, 0usize);
        while let Some(e) = enc.encode(first_id + frames, screen, budget) {
            dec.apply(&e.data).unwrap();
            frames += 1;
            bytes += e.data.len();
            assert!(frames < 10_000, "编码器不收敛");
        }
        (frames, bytes)
    }

    #[test]
    fn first_frame_is_complete_then_static_screen_sends_nothing() {
        let desktop = SyntheticDesktop::new(640, 360, 1);
        let mut enc = Encoder::new(640, 360);
        let mut dec = Decoder::new(640, 360);
        let first = enc.encode(1, desktop.screen(), BIG).unwrap();
        assert_eq!(first.tiles, 20 * 12);
        assert_eq!(first.quality, Quality::Lossless);
        assert_eq!(first.pending, 0);
        dec.apply(&first.data).unwrap();
        assert_eq!(dec.screen(), desktop.screen());
        // 大片纯色的桌面：640×360 的整屏不到 10 KB
        assert!(first.data.len() < 10_000, "{}", first.data.len());
        // 静止：不产生任何数据
        assert_eq!(enc.encode(2, desktop.screen(), BIG), None);
        assert_eq!(enc.pending(), 0);
    }

    #[test]
    fn every_workload_roundtrips_losslessly_and_costs_what_it_should() {
        let mut desktop = SyntheticDesktop::new(640, 360, 2);
        let mut enc = Encoder::new(640, 360);
        let mut dec = Decoder::new(640, 360);
        drain(&mut enc, &mut dec, desktop.screen(), BIG, 0);
        let mut id = 100;
        let mut cost =
            |w: Workload, enc: &mut Encoder, dec: &mut Decoder, desktop: &mut SyntheticDesktop| {
                let mut total = 0;
                for _ in 0..30 {
                    desktop.step(w);
                    id += 1;
                    if let Some(e) = enc.encode(id, desktop.screen(), BIG) {
                        dec.apply(&e.data).unwrap();
                        total += e.data.len();
                    }
                    assert_eq!(dec.screen(), desktop.screen(), "{w:?}");
                }
                total / 30
            };
        let idle = cost(Workload::Idle, &mut enc, &mut dec, &mut desktop);
        let typing = cost(Workload::Typing, &mut enc, &mut dec, &mut desktop);
        let dragging = cost(Workload::Dragging, &mut enc, &mut dec, &mut desktop);
        let video = cost(Workload::Video, &mut enc, &mut dec, &mut desktop);
        cost(Workload::Scrolling, &mut enc, &mut dec, &mut desktop);
        assert_eq!(idle, 0);
        assert!(typing > 0 && typing < 400, "打字每帧 {typing} 字节");
        assert!(dragging < 2_500, "拖窗口每帧 {dragging} 字节");
        assert!(
            video > 20_000,
            "视频每帧 {video} 字节：这个编码器不擅长视频"
        );
        assert!(typing < dragging && dragging < video);
    }

    #[test]
    fn budget_is_respected_quality_drops_then_refines_to_lossless() {
        let mut desktop = SyntheticDesktop::new(640, 360, 3);
        let mut enc = Encoder::new(640, 360);
        let mut dec = Decoder::new(640, 360);
        drain(&mut enc, &mut dec, desktop.screen(), BIG, 0);
        desktop.step(Workload::Video);

        // 预算只够降画质的版本
        let lossless = Encoder::new(640, 360)
            .encode(1, desktop.screen(), BIG)
            .unwrap()
            .data
            .len();
        let budget = 40_000;
        assert!(lossless > budget);
        let e = enc.encode(500, desktop.screen(), budget).unwrap();
        assert!(e.data.len() <= budget, "{} > {budget}", e.data.len());
        assert!(e.quality > Quality::Lossless);
        assert!(e.pending > 0);
        dec.apply(&e.data).unwrap();
        let rough = dec.screen().diff(desktop.screen());
        assert!(rough > 0, "降画质后与源有差别");

        // 画面不再变化：后续帧逐步补成无损，每帧都不超预算
        let mut id = 501;
        while let Some(e) = enc.encode(id, desktop.screen(), budget) {
            assert!(e.data.len() <= budget);
            dec.apply(&e.data).unwrap();
            id += 1;
            assert!(id < 600);
        }
        assert!(id > 502, "需要不止一帧来补清晰");
        assert_eq!(dec.screen(), desktop.screen(), "最终与源完全一致");
        assert_eq!(enc.pending(), 0);
    }

    #[test]
    fn tiny_budget_still_makes_progress_one_tile_at_a_time() {
        let desktop = SyntheticDesktop::new(160, 120, 4);
        let mut enc = Encoder::new(160, 120);
        let mut dec = Decoder::new(160, 120);
        let (frames, _) = drain(&mut enc, &mut dec, desktop.screen(), 1, 0);
        assert_eq!(dec.screen(), desktop.screen());
        assert!(frames >= 20, "预算极小时一帧一个块：{frames} 帧");
    }

    #[test]
    fn lost_frames_are_repaired_with_current_content() {
        let mut desktop = SyntheticDesktop::new(320, 240, 5);
        let mut enc = Encoder::new(320, 240);
        let mut dec = Decoder::new(320, 240);
        let first = enc.encode(1, desktop.screen(), BIG).unwrap();
        dec.apply(&first.data).unwrap();
        enc.on_feedback(1, 0b1);

        // 第 2、3 帧在路上丢了，第 4 帧送达
        desktop.step(Workload::Typing);
        let lost_a = enc.encode(2, desktop.screen(), BIG).unwrap();
        desktop.step(Workload::Dragging);
        let lost_b = enc.encode(3, desktop.screen(), BIG).unwrap();
        desktop.step(Workload::Typing);
        let ok = enc.encode(4, desktop.screen(), BIG).unwrap();
        dec.apply(&ok.data).unwrap();
        assert_ne!(dec.screen(), desktop.screen(), "丢的帧里的块还是旧的");
        assert_eq!(
            enc.encode(5, desktop.screen(), BIG),
            None,
            "编码器还不知道丢了"
        );

        // 反馈：到第 4 帧为止，只有第 4 帧和第 1 帧送达
        enc.on_feedback(4, 0b1001);
        let repair = enc.encode(6, desktop.screen(), BIG).unwrap();
        assert!(repair.tiles > 0);
        assert!(repair.tiles <= lost_a.tiles + lost_b.tiles);
        dec.apply(&repair.data).unwrap();
        assert_eq!(dec.screen(), desktop.screen());
        // 同一份反馈再来一次不会重复补发
        enc.on_feedback(4, 0b1001);
        assert_eq!(enc.encode(7, desktop.screen(), BIG), None);
    }

    #[test]
    fn feedback_edge_cases() {
        let desktop = SyntheticDesktop::new(160, 120, 6);
        let mut enc = Encoder::new(160, 120);
        enc.encode(1, desktop.screen(), BIG).unwrap();
        // 还没有定论的帧：不动
        enc.on_feedback(0, 0);
        assert_eq!(enc.pending(), 0);
        // 丢失的帧比窗口还老（超过 32 帧之前）：按丢失处理
        enc.on_feedback(100, u32::MAX);
        assert_eq!(enc.pending(), enc.tiles.len());
        // 块在丢失的帧之后又发过：以后一次为准，不重复标记
        let mut enc = Encoder::new(160, 120);
        enc.encode(1, desktop.screen(), BIG).unwrap();
        enc.request_full();
        enc.encode(2, desktop.screen(), BIG).unwrap();
        enc.on_feedback(2, 0b01); // 第 1 帧丢了，第 2 帧到了
        assert_eq!(enc.pending(), 0);
        // 发送端自己判定丢失
        let mut enc = Encoder::new(160, 120);
        enc.encode(1, desktop.screen(), BIG).unwrap();
        enc.mark_lost(9); // 不认识的帧号：无事发生
        assert_eq!(enc.pending(), 0);
        enc.mark_lost(1);
        assert_eq!(enc.pending(), enc.tiles.len());
        enc.mark_lost(1); // 已经处理过
                          // 日志有上限
        let mut enc = Encoder::new(160, 120);
        for id in 0..300 {
            enc.request_full();
            enc.encode(id, desktop.screen(), 1);
        }
        assert_eq!(enc.log.len(), LOG_FRAMES);
        // 尺寸不符
        assert_eq!(enc.encode(1, &Screen::new(10, 10, 0), BIG), None);
    }

    #[test]
    fn all_tile_modes_and_odd_sizes_roundtrip() {
        // 33×35：右边和下边各有一条不满 32 的块
        let mut rng = Rng::new(9);
        let mut screen = Screen::new(70, 67, 0x112233);
        // 两色（1 位）、四色（2 位）、十六色（4 位）、长游程、噪声
        for (i, pixel) in screen.row_mut(0, 0, 32).iter_mut().enumerate() {
            *pixel = if i % 2 == 0 { 0xffffff } else { 0 };
        }
        for y in 32..64 {
            for (i, pixel) in screen.row_mut(0, y, 32).iter_mut().enumerate() {
                *pixel = [0xff0000, 0x00ff00, 0x0000ff, 0x123456][(i + usize::from(y)) % 4];
            }
            for (i, pixel) in screen.row_mut(32, y, 32).iter_mut().enumerate() {
                *pixel = u32::try_from((i + usize::from(y)) % 16).unwrap() * 0x101010;
            }
        }
        for y in 0..32 {
            for pixel in screen.row_mut(32, y, 32).iter_mut() {
                *pixel = u32::try_from(rng.below(1 << 24)).unwrap();
            }
        }
        screen.fill(64, 0, 6, 67, 0xabcdef);
        screen.fill(0, 64, 20, 3, 0x654321);

        for budget in [BIG, 2_000, 300] {
            let mut enc = Encoder::new(70, 67);
            let mut dec = Decoder::new(70, 67);
            drain(&mut enc, &mut dec, &screen, budget, 0);
            assert_eq!(dec.screen(), &screen, "预算 {budget}");
        }
    }

    #[test]
    fn decoder_rejects_malformed_frames_atomically() {
        let desktop = SyntheticDesktop::new(160, 120, 8);
        let good = Encoder::new(160, 120)
            .encode(1, desktop.screen(), BIG)
            .unwrap()
            .data;
        let mut dec = Decoder::new(160, 120);
        let blank = dec.screen().clone();

        let mut bad_version = good.clone();
        bad_version[0] = 9;
        assert_eq!(dec.apply(&bad_version), Err(Error::UnsupportedVersion(9)));
        assert!(matches!(
            Decoder::new(100, 100).apply(&good),
            Err(Error::Inconsistent(_))
        ));
        let mut too_many = good.clone();
        too_many[5..7].copy_from_slice(&[0xff, 0xff]);
        assert!(matches!(
            dec.apply(&too_many),
            Err(Error::TooManyItems { .. })
        ));
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(dec.apply(&trailing), Err(Error::TrailingBytes(1)));
        for len in 0..good.len().min(200) {
            assert!(dec.apply(&good[..len]).is_err(), "截断到 {len}");
        }
        assert_eq!(dec.screen(), &blank, "出错的帧不留下任何痕迹");

        // 手工构造的非法块
        let frame = |tile: &[u8]| {
            let mut w = Writer::new();
            w.u8(VERSION).u16(160).u16(120).u16(1).bytes(tile);
            w.into_vec()
        };
        for (tile, why) in [
            (&[0xff, 0xff, 0][..], "块序号越界"),
            (&[0, 0, 0x30][..], "未知的画质档位"),
            (&[0, 0, 0x09][..], "未知的块编码模式"),
            (&[0, 0, 1, 1][..], "调色板大小不合法"),
            (&[0, 0, 1, 17][..], "调色板大小不合法"),
            (&[0, 0, 2, 0, 1, 0, 1, 2, 3][..], "游程长度不合法"),
            (&[0, 0, 2, 0, 1, 5, 1, 2, 3][..], "游程总长度与块大小不符"),
        ] {
            assert_eq!(dec.apply(&frame(tile)), Err(Error::Inconsistent(why)));
        }
        // 游程总长超过块大小
        let mut over = vec![0, 0, 2, 0, 5];
        for _ in 0..5 {
            over.extend([255, 1, 2, 3]);
        }
        assert_eq!(
            dec.apply(&frame(&over)),
            Err(Error::Inconsistent("游程长度不合法"))
        );
        // 调色板索引越界：3 色调色板用 2 位索引，索引 3 不存在
        let mut index = vec![0, 0, 1, 3, 0, 0, 0, 1, 1, 1, 2, 2, 2];
        index.extend(vec![0xff; 256]);
        assert_eq!(
            dec.apply(&frame(&index)),
            Err(Error::Inconsistent("调色板索引越界"))
        );

        // 随机字节和比特翻转都不会 panic
        let mut rng = Rng::new(1);
        for _ in 0..3_000 {
            let mut fuzz = good.clone();
            let at = usize::try_from(rng.below(fuzz.len() as u64)).unwrap();
            fuzz[at] ^= 1 << rng.below(8);
            let _ = dec.apply(&fuzz);
        }
        dec.apply(&good).unwrap();
        assert_eq!(dec.screen(), desktop.screen());
    }

    #[test]
    fn empty_and_degenerate_sizes() {
        let mut enc = Encoder::new(0, 0);
        assert_eq!(enc.pending(), 0);
        assert_eq!(enc.encode(1, &Screen::new(0, 0, 0), BIG), None);
        let mut dec = Decoder::new(0, 0);
        assert_eq!(dec.apply(&[VERSION, 0, 0, 0, 0, 0, 0]).unwrap(), 0);
        assert!(Grid::new(0, 0).rect(0).is_none());
        assert_eq!(Quality::from_id(2).unwrap(), Quality::Coarse);
    }
}
