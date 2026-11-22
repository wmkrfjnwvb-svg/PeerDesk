//! 声卡和会话之间的格式转换、播放队列（与系统无关，有单元测试）。
//!
//! 会话里的声音固定是 48 kHz 立体声、10 ms 一帧（交错的 16 位）。声卡可能是 44.1 kHz、单声道、
//! 5.1 声道、浮点格式……这里负责两边互转。真实设备的打开在 `audio_sys.rs`（系统胶水）。

use std::collections::VecDeque;

use peerdesk_media::audio::{FRAME, FRAME_SAMPLES, RATE};

/// 把声卡送来的采样（任意采样率、任意声道数，浮点）转成会话用的帧（48 kHz 立体声 16 位，10 ms）。
#[derive(Debug, Clone)]
pub struct ToFrames {
    channels: usize,
    /// 每个输入采样前进多少个输出采样（输出率 / 输入率）。
    step: f32,
    /// 下一个输出采样在输入里的位置（相对 `prev`）。
    pos: f32,
    /// 上一个输入采样（左、右），线性插值用。
    prev: [f32; 2],
    pending: Vec<i16>,
}

impl ToFrames {
    /// 声卡是 `rate` Hz、`channels` 个声道。
    #[must_use]
    pub fn new(rate: u32, channels: u16) -> Self {
        Self {
            channels: usize::from(channels.max(1)),
            step: ratio(rate, RATE),
            pos: 0.0,
            prev: [0.0; 2],
            pending: Vec::with_capacity(FRAME_SAMPLES * 2),
        }
    }

    /// 放入一段交错的采样，取出凑满的整帧。
    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<i16>> {
        for frame in samples.chunks_exact(self.channels) {
            let cur = stereo(frame);
            // 在 prev 和 cur 之间插出所有落在这一段里的输出采样
            while self.pos < 1.0 {
                let t = self.pos;
                for ch in 0..2 {
                    let a = self.prev.get(ch).copied().unwrap_or(0.0);
                    let b = cur.get(ch).copied().unwrap_or(0.0);
                    self.pending.push(to_i16(a + (b - a) * t));
                }
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
        let mut out = Vec::new();
        while self.pending.len() >= FRAME_SAMPLES {
            let rest = self.pending.split_off(FRAME_SAMPLES);
            out.push(std::mem::replace(&mut self.pending, rest));
        }
        out
    }
}

/// 多声道混成立体声：单声道复制到两边，多于两个声道时取前两个（左、右）。
fn stereo(frame: &[f32]) -> [f32; 2] {
    match frame {
        [mono] => [*mono, *mono],
        [l, r, ..] => [*l, *r],
        [] => [0.0, 0.0],
    }
}

/// 两个采样率的比（`a / b`）。采样率都在几十万以内，f32 足够精确。
#[allow(clippy::cast_precision_loss)]
fn ratio(a: u32, b: u32) -> f32 {
    a.max(1) as f32 / b.max(1) as f32
}

fn to_i16(v: f32) -> i16 {
    let scaled = (v.clamp(-1.0, 1.0) * 32767.0).round();
    // 已经夹在 ±32767 之内
    #[allow(clippy::cast_possible_truncation)]
    let s = scaled as i16;
    s
}

/// 播放队列：会话每 10 ms 放进一帧，声卡回调按自己的节奏取。
///
/// 队列里的声音超过 `max_ms` 时丢掉最老的部分，保证延迟不会越积越多（声卡时钟比被控端快或慢时）；
/// 取的时候没有就补静音并计数（欠载）。
#[derive(Debug, Clone)]
pub struct PlayQueue {
    samples: VecDeque<f32>,
    channels: usize,
    /// 输出采样 / 会话采样。
    step: f32,
    pos: f32,
    prev: [f32; 2],
    max_samples: usize,
    /// 欠载（声卡要数据时队列是空的）次数。
    pub underruns: u64,
    /// 因为积压太多丢掉的采样数。
    pub dropped: u64,
}

impl PlayQueue {
    /// 声卡是 `rate` Hz、`channels` 个声道，队列最多积 `max_ms` 毫秒。
    #[must_use]
    pub fn new(rate: u32, channels: u16, max_ms: u32) -> Self {
        let channels = usize::from(channels.max(1));
        let per_ms = usize::try_from(rate / 1000).unwrap_or(48) * channels;
        Self {
            samples: VecDeque::new(),
            channels,
            step: ratio(RATE, rate),
            pos: 0.0,
            prev: [0.0; 2],
            max_samples: per_ms * usize::try_from(max_ms).unwrap_or(100),
            underruns: 0,
            dropped: 0,
        }
    }

    /// 放入会话的一帧（48 kHz 立体声 16 位），转成声卡的格式。
    pub fn push(&mut self, pcm: &[i16]) {
        for pair in pcm.chunks_exact(2) {
            let cur = [
                f32::from(pair.first().copied().unwrap_or(0)) / 32768.0,
                f32::from(pair.get(1).copied().unwrap_or(0)) / 32768.0,
            ];
            while self.pos < 1.0 {
                let t = self.pos;
                let l = self.prev[0] + (cur[0] - self.prev[0]) * t;
                let r = self.prev[1] + (cur[1] - self.prev[1]) * t;
                match self.channels {
                    1 => self.samples.push_back((l + r) / 2.0),
                    n => {
                        self.samples.push_back(l);
                        self.samples.push_back(r);
                        // 多出来的声道（中置、低音……）放静音
                        self.samples.extend(std::iter::repeat_n(0.0, n - 2));
                    }
                }
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
        if self.samples.len() > self.max_samples {
            // 按整个声道组丢，不打乱声道顺序
            let excess = self.samples.len() - self.max_samples;
            let excess = excess.div_ceil(self.channels) * self.channels;
            self.samples.drain(..excess.min(self.samples.len()));
            self.dropped = self
                .dropped
                .saturating_add(u64::try_from(excess).unwrap_or(0));
        }
    }

    /// 声卡回调：填满 `out`；不够的部分补静音。
    pub fn fill(&mut self, out: &mut [f32]) {
        let available = self.samples.len().min(out.len());
        for (dst, src) in out.iter_mut().zip(self.samples.drain(..available)) {
            *dst = src;
        }
        if available < out.len() {
            for dst in out.iter_mut().skip(available) {
                *dst = 0.0;
            }
            self.underruns = self.underruns.saturating_add(1);
        }
    }

    /// 队列里现有的时长（采样数，按声道组计）。
    #[must_use]
    pub fn queued_frames(&self) -> usize {
        self.samples.len() / self.channels
    }
}

/// 一帧的采样数（每声道），给胶水层用。
pub const FRAME_LEN: usize = FRAME;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::cast_precision_loss)]
    use super::*;

    fn sine(rate: u32, channels: u16, ms: u32, hz: f32) -> Vec<f32> {
        let n = rate * ms / 1000;
        let mut out = Vec::new();
        for i in 0..n {
            let v = (i as f32 / rate as f32 * hz * std::f32::consts::TAU).sin() * 0.5;
            for _ in 0..channels {
                out.push(v);
            }
        }
        out
    }

    #[test]
    fn capture_at_48k_stereo_is_framed_exactly() {
        let mut c = ToFrames::new(48_000, 2);
        // 25 ms：两帧，剩 5 ms 留到下次
        let frames = c.push(&sine(48_000, 2, 25, 440.0));
        assert_eq!(frames.len(), 2);
        assert!(frames.iter().all(|f| f.len() == FRAME_SAMPLES));
        assert_eq!(c.push(&sine(48_000, 2, 5, 440.0)).len(), 1);
    }

    #[test]
    fn capture_resamples_and_mixes_channels() {
        // 44.1 kHz 单声道 1 秒 → 48 kHz 立体声约 100 帧
        let mut c = ToFrames::new(44_100, 1);
        let frames = c.push(&sine(44_100, 1, 1000, 440.0));
        assert!((99..=101).contains(&frames.len()), "{}", frames.len());
        // 单声道复制到左右两边，有声音
        let f = &frames[50];
        assert!(f.chunks(2).all(|p| p[0] == p[1]));
        assert!(f.iter().any(|&s| s.abs() > 10_000));
        // 5.1 声道取前两个
        let mut six = ToFrames::new(48_000, 6);
        let mut block = Vec::new();
        for _ in 0..FRAME {
            block.extend_from_slice(&[0.25, -0.25, 0.9, 0.9, 0.9, 0.9]);
        }
        let f = six.push(&block).pop().unwrap();
        assert_eq!(f[2], to_i16(0.25));
        assert_eq!(f[3], to_i16(-0.25));
        // 超出范围的值被夹住
        assert_eq!(to_i16(3.0), 32767);
        assert_eq!(to_i16(-3.0), -32767);
        assert_eq!(stereo(&[]), [0.0, 0.0]);
    }

    #[test]
    fn playback_converts_and_bounds_latency() {
        // 声卡 44.1 kHz 立体声，队列最多 100 ms
        let mut q = PlayQueue::new(44_100, 2, 100);
        let frame: Vec<i16> = (0..FRAME_SAMPLES)
            .map(|i| if i % 2 == 0 { 8000 } else { -8000 })
            .collect();
        q.push(&frame);
        // 10 ms 的 48k 变成约 441 个 44.1k 采样
        assert!(
            (440..=442).contains(&q.queued_frames()),
            "{}",
            q.queued_frames()
        );
        let mut out = vec![0.0; 200];
        q.fill(&mut out);
        assert!(out[100] > 0.2 && out[101] < -0.2);
        assert_eq!(q.underruns, 0);
        // 积压：推 50 帧（500 ms），队列被限制在 100 ms 左右
        for _ in 0..50 {
            q.push(&frame);
        }
        assert!(q.queued_frames() <= 4410 + 1, "{}", q.queued_frames());
        assert!(q.dropped > 0);
        // 取空了：补静音并计欠载
        let mut big = vec![1.0; 20_000];
        q.fill(&mut big);
        assert_eq!(big[19_999], 0.0);
        assert_eq!(q.underruns, 1);
    }

    #[test]
    fn playback_to_mono_and_surround() {
        let frame = vec![10_000i16; FRAME_SAMPLES];
        let mut mono = PlayQueue::new(48_000, 1, 100);
        mono.push(&frame);
        assert_eq!(mono.queued_frames(), FRAME);
        let mut six = PlayQueue::new(48_000, 6, 100);
        six.push(&frame);
        let mut out = vec![0.0; 12];
        six.fill(&mut out);
        // 后 6 个采样是第二个声道组：左右有声音，其余静音
        assert!(out[6] > 0.2 && out[7] > 0.2);
        assert!(out[8..].iter().all(|&s| s == 0.0));
        assert_eq!(FRAME_LEN, FRAME);
    }
}
