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
