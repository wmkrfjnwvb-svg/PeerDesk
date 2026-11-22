//! 声音的编解码：Opus，48 kHz 立体声，10 ms 一帧（设计见 `docs/research/H-audio.md` 第 7 节）。
//!
//! 远程桌面的声音以音乐、视频、提示音为主，用 Opus 的低延迟模式（CELT），码率 96 kbps 受限 VBR，
//! 不开 DTX（静音时仍发很小的帧，接收端的时间线保持连续）。丢包用 Opus 自带的补偿（PLC）。

use opus::{Application, Bitrate, Channels, Decoder, Encoder};

/// 采样率。
pub const RATE: u32 = 48_000;
/// 声道数。
pub const CHANNELS: usize = 2;
/// 每帧每声道的采样数（10 ms）。
pub const FRAME: usize = 480;
/// 一帧交错后的采样数。
pub const FRAME_SAMPLES: usize = FRAME * CHANNELS;
/// 默认码率（比特每秒）。
pub const DEFAULT_BPS: i32 = 96_000;
/// 一帧编码后的最大字节数（510 kbps 的 10 ms 也只有约 640 字节）。
const MAX_PACKET: usize = 1500;

/// 编解码出错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioError(pub String);

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "声音编解码失败：{}", self.0)
    }
}

impl std::error::Error for AudioError {}

fn err(e: &opus::Error) -> AudioError {
    AudioError(e.to_string())
}

/// 编码器：每次一帧（10 ms，交错的 16 位立体声）。
pub struct AudioEncoder {
    inner: Encoder,
}

impl std::fmt::Debug for AudioEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioEncoder")
    }
}

impl AudioEncoder {
    /// 创建，码率 `bps`。
    pub fn new(bps: i32) -> Result<Self, AudioError> {
        let mut inner =
            Encoder::new(RATE, Channels::Stereo, Application::LowDelay).map_err(|e| err(&e))?;
        inner.set_bitrate(Bitrate::Bits(bps)).map_err(|e| err(&e))?;
        // 受限 VBR：码率围绕目标小幅波动，不突发
        inner.set_vbr(true).map_err(|e| err(&e))?;
        inner.set_vbr_constraint(true).map_err(|e| err(&e))?;
        Ok(Self { inner })
    }

    /// 改码率（拥塞时降码率）。
    pub fn set_bitrate(&mut self, bps: i32) -> Result<(), AudioError> {
        self.inner
            .set_bitrate(Bitrate::Bits(bps))
            .map_err(|e| err(&e))
    }

    /// 编码一帧；`pcm` 必须正好是 [`FRAME_SAMPLES`] 个采样。
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, AudioError> {
        if pcm.len() != FRAME_SAMPLES {
            return Err(AudioError(format!(
                "一帧应为 {FRAME_SAMPLES} 个采样，实际 {}",
                pcm.len()
            )));
        }
        self.inner.encode_vec(pcm, MAX_PACKET).map_err(|e| err(&e))
    }
}

/// 解码器。
pub struct AudioDecoder {
    inner: Decoder,
}

impl std::fmt::Debug for AudioDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AudioDecoder")
    }
}

impl AudioDecoder {
    /// 创建。
    pub fn new() -> Result<Self, AudioError> {
        Ok(Self {
            inner: Decoder::new(RATE, Channels::Stereo).map_err(|e| err(&e))?,
        })
    }

    /// 解码一帧；`packet` 为 `None` 表示这一帧丢了，用 Opus 的补偿生成一帧。
    /// 返回交错的 16 位立体声，正好一帧。
    pub fn decode(&mut self, packet: Option<&[u8]>) -> Result<Vec<i16>, AudioError> {
        let mut out = vec![0i16; FRAME_SAMPLES];
        let n = self
            .inner
            .decode(packet.unwrap_or_default(), &mut out, false)
            .map_err(|e| err(&e))?;
        out.truncate(n.saturating_mul(CHANNELS));
        out.resize(FRAME_SAMPLES, 0);
        Ok(out)
    }
}

/// 交叉淡化的长度（每声道采样数，2.5 ms）。
const OVERLAP: usize = 120;
/// 找拼接点时的步长（每声道采样数）。
const SEARCH_STEP: usize = 4;

/// 加速：把连续的两帧 `a`、`b`（各 10 ms）压成一帧（10 ms），听不出接缝。
///
/// 输出 = `a` 的前 p 个采样 + `a[p..]` 与 `b[p..]` 交叉淡化 + `b` 的剩余部分，正好跳过 10 ms。
/// p 选在两段波形最相似的地方（WSOLA 的简化版），这样淡化时两段的相位对得上，不会出现咔哒声。
#[must_use]
pub fn merge_drop(a: &[i16], b: &[i16]) -> Vec<i16> {
    if a.len() != FRAME_SAMPLES || b.len() != FRAME_SAMPLES {
        return b.to_vec();
    }
    let mono = |pcm: &[i16], at: usize| -> i64 {
        let l = pcm.get(at * 2).copied().unwrap_or(0);
        let r = pcm.get(at * 2 + 1).copied().unwrap_or(0);
        i64::from(l) + i64::from(r)
    };
    // 归一化互相关最大的位置
    let mut best = (0usize, i128::MIN);
    for p in (0..=FRAME - OVERLAP).step_by(SEARCH_STEP) {
        let (mut dot, mut ea, mut eb) = (0i128, 0i128, 0i128);
        for k in 0..OVERLAP {
            let (x, y) = (i128::from(mono(a, p + k)), i128::from(mono(b, p + k)));
            dot += x * y;
            ea += x * x;
            eb += y * y;
        }
        // 比较 dot / sqrt(ea·eb)：用 dot·|dot| / (ea·eb) 避免开方，保留符号
        let score = if ea == 0 || eb == 0 {
            0
        } else {
            dot.saturating_mul(dot.abs()).saturating_mul(1_000_000) / ea.saturating_mul(eb).max(1)
        };
        if score > best.1 {
            best = (p, score);
        }
    }
    let p = best.0;
    let mut out = Vec::with_capacity(FRAME_SAMPLES);
    out.extend_from_slice(a.get(..p * 2).unwrap_or_default());
    for k in 0..OVERLAP {
        let w = i64::try_from(k).unwrap_or(0);
        let n = i64::try_from(OVERLAP).unwrap_or(1);
        for ch in 0..2 {
            let x = i64::from(a.get((p + k) * 2 + ch).copied().unwrap_or(0));
            let y = i64::from(b.get((p + k) * 2 + ch).copied().unwrap_or(0));
            let v = (x * (n - w) + y * w) / n;
            out.push(i16::try_from(v).unwrap_or(0));
        }
    }
    out.extend_from_slice(b.get((p + OVERLAP) * 2..).unwrap_or_default());
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::cast_possible_truncation)]
    use super::*;

    /// 一帧 440 Hz（左）/ 660 Hz（右）的正弦，`start` 是起始采样。
    fn tone(start: usize) -> Vec<i16> {
        let mut out = Vec::with_capacity(FRAME_SAMPLES);
        for i in start..start + FRAME {
            let t = i as f64 / f64::from(RATE);
            let l = (t * 440.0 * std::f64::consts::TAU).sin() * 8000.0;
            let r = (t * 660.0 * std::f64::consts::TAU).sin() * 8000.0;
            out.push(l as i16);
            out.push(r as i16);
        }
        out
    }

    fn energy(pcm: &[i16]) -> f64 {
        pcm.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / pcm.len() as f64
    }

    #[test]
    fn round_trip_keeps_the_sound_and_concealment_fills_gaps() {
        let mut enc = AudioEncoder::new(DEFAULT_BPS).unwrap();
        let mut dec = AudioDecoder::new().unwrap();
        let mut sizes = Vec::new();
        let mut last = Vec::new();
        for k in 0..50 {
            let packet = enc.encode(&tone(k * FRAME)).unwrap();
            sizes.push(packet.len());
            last = dec.decode(Some(&packet)).unwrap();
            assert_eq!(last.len(), FRAME_SAMPLES);
        }
        // 96 kbps × 10 ms ≈ 120 字节，受限 VBR 波动不大
        let mean = sizes.iter().sum::<usize>() / sizes.len();
        assert!((60..=200).contains(&mean), "平均每帧 {mean} 字节");
        // 解码出来有和原始声音相当的能量（编码器有几毫秒延迟，不逐点比较）
        let ratio = energy(&last) / energy(&tone(49 * FRAME));
        assert!((0.5..2.0).contains(&ratio), "能量比 {ratio}");
        // 丢一帧：补偿出一整帧，而且不是静音
        let plc = dec.decode(None).unwrap();
        assert_eq!(plc.len(), FRAME_SAMPLES);
        assert!(energy(&plc) > 0.1 * energy(&last));
    }

    /// 相邻采样之间最大的跳变（同一声道）。
    fn max_jump(pcm: &[i16]) -> i32 {
        pcm.windows(4)
            .step_by(2)
            .map(|w| (i32::from(w[2]) - i32::from(w[0])).abs())
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn merging_two_frames_has_no_click() {
        let (a, b) = (tone(0), tone(FRAME));
        let merged = merge_drop(&a, &b);
        assert_eq!(merged.len(), FRAME_SAMPLES);
        // 原始正弦里相邻采样的最大跳变
        let smooth = max_jump(&a);
        // 直接丢掉一帧时：前一帧的结尾接下下一帧的开头（再下一帧），相位错开会有跳变
        let naive: Vec<i16> = a.iter().chain(tone(2 * FRAME).iter()).copied().collect();
        let seam = max_jump(&naive);
        assert!(
            seam > smooth * 3,
            "直接丢帧的接缝跳变 {seam}，原始 {smooth}"
        );
        // 拼接后的跳变不超过原始波形的两倍（没有咔哒声）
        assert!(
            max_jump(&merged) <= smooth * 2,
            "{} vs {}",
            max_jump(&merged),
            smooth
        );
        // 长度不对时原样返回后一帧
        assert_eq!(merge_drop(&a[..10], &b), b);
        // 静音也能拼
        let silent = vec![0; FRAME_SAMPLES];
        assert_eq!(merge_drop(&silent, &silent), silent);
    }

    #[test]
    fn bitrate_changes_and_bad_input() {
        let mut enc = AudioEncoder::new(DEFAULT_BPS).unwrap();
        enc.set_bitrate(32_000).unwrap();
        let small: usize = (0..20)
            .map(|k| enc.encode(&tone(k * FRAME)).unwrap().len())
            .sum();
        assert!(
            small / 20 < 80,
            "32 kbps 每帧应约 40 字节，实际 {}",
            small / 20
        );
        assert!(enc.encode(&[0; 10]).is_err());
        // 非法码率、坏数据都明确报错
        assert!(AudioEncoder::new(-5).is_err());
        assert!(AudioEncoder::new(0).is_err());
        let mut dec = AudioDecoder::new().unwrap();
        assert!(dec.decode(Some(&[0xff; 3])).is_err());
        let e = AudioError("x".into());
        assert!(e.to_string().contains('x'));
        assert!(format!("{enc:?}{dec:?}").contains("AudioEncoder"));
    }
}
