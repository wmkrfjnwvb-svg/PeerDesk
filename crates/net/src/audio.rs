//! 在会话里传声音（设计见 `docs/research/H-audio.md` 第 7 节）。
//!
//! - 默认关闭。控制端打开声音时向被控端开一条“声音”流；流开着，被控端就采集、编码、发声音数据报，
//!   流关了就停（不用新的控制消息，老版本不受影响）。只有双方都协商出 [`Caps::AUDIO`] 才会开流。
//! - 被控端：每 10 ms 一帧 → Opus → [`AudioPacket`]（带前一帧的副本，丢一个包能补回）→ 数据报。
//! - 控制端：数据报 → [`JitterBuffer`] → 每 10 ms 取一帧解码（缺包用 Opus 补偿）→ 播放。
//!
//! 采集和播放是接口：真实设备在平台层，测试里用合成的声音和录音器（测试不碰开发者电脑的声卡）。
//!
//! [`Caps::AUDIO`]: peerdesk_core::proto::Caps::AUDIO

use std::sync::{Arc, Mutex, PoisonError};

use peerdesk_core::audio::{
    loss_permille, AudioPacket, AudioStats, JitterBuffer, Play, Redundancy,
};
use peerdesk_core::proto::Channel;
use peerdesk_media::audio::{AudioDecoder, AudioEncoder, DEFAULT_BPS, FRAME, FRAME_SAMPLES};
use tokio::sync::mpsc;

use crate::{Link, NetError};

/// 被控端的声音来源：打开后每 10 ms 送来一帧（交错的 16 位立体声，[`FRAME_SAMPLES`] 个采样）。
pub trait AudioCapture: Send + Sync {
    /// 开始采集。打不开（没有声卡、没有权限）时返回给用户看的原因。
    fn start(&self) -> Result<mpsc::Receiver<Vec<i16>>, String>;
}

/// 控制端的声音去向。
pub trait AudioPlayback: Send + Sync {
    /// 播放一帧（10 ms）。
    fn play(&self, pcm: &[i16]);
}

/// 声音来源的句柄，可以放进会话选项里。
#[derive(Clone)]
pub struct CaptureHandle(pub Arc<dyn AudioCapture>);

/// 声音去向的句柄，可以放进会话选项里。
#[derive(Clone)]
pub struct PlaybackHandle(pub Arc<dyn AudioPlayback>);

impl std::fmt::Debug for CaptureHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CaptureHandle")
    }
}

impl std::fmt::Debug for PlaybackHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PlaybackHandle")
    }
}

/// 控制端的声音设置：去向和开关（助手里切换，默认关）。
#[derive(Clone)]
pub struct AudioOut {
    /// 去向。
    pub playback: PlaybackHandle,
    /// 开关：`true` 时向被控端要声音。
    pub on: tokio::sync::watch::Receiver<bool>,
}

impl std::fmt::Debug for AudioOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioOut")
            .field("on", &*self.on.borrow())
            .finish_non_exhaustive()
    }
}

/// 控制端多久报告一次丢包率（毫秒）。
pub const REPORT_MS: u64 = 500;

/// 被控端：采集、编码并发送，直到声音流关闭（控制端关掉声音）或 `stop` 完成（会话结束）。
/// 控制端在声音流上每 [`REPORT_MS`] 报告一次丢包率（2 字节，千分比），据此决定带不带前一帧的副本。
pub async fn send(
    link: &Link,
    capture: &dyn AudioCapture,
    mut reports: quinn::RecvStream,
    stop: impl std::future::Future<Output = ()>,
) -> Result<(), NetError> {
    let started = std::time::Instant::now();
    let mut redundancy = Redundancy::default();
    let mut report = Vec::with_capacity(2);
    let mut buf = [0u8; 64];
    let mut frames = capture.start().map_err(NetError::Io)?;
    let mut encoder = AudioEncoder::new(DEFAULT_BPS).map_err(|e| NetError::Io(e.to_string()))?;
    let mut seq = 0u32;
    let mut previous: Option<Vec<u8>> = None;
    tokio::pin!(stop);
    crate::note!(info, "声音：开始发送");
    loop {
        let pcm = tokio::select! {
            () = &mut stop => break,
            read = reports.read(&mut buf) => match read {
                // 控制端的丢包报告：2 字节一个
                Ok(Some(n)) => {
                    for &b in buf.get(..n).unwrap_or_default() {
                        report.push(b);
                        if let [hi, lo] = report[..] {
                            let loss = u16::from_be_bytes([hi, lo]);
                            let now = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                            let was = redundancy.on();
                            if redundancy.report(loss, now) != was {
                                crate::note!(info, "声音：丢包 {}.{}%，冗余{}", loss / 10, loss % 10, if was { "关闭" } else { "打开" });
                            }
                            report.clear();
                        }
                    }
                    continue;
                }
                // 控制端关掉了声音
                Ok(None) | Err(_) => break,
            },
            pcm = frames.recv() => match pcm {
                Some(pcm) => pcm,
                None => break,
            },
        };
        let frame = encoder
            .encode(&pcm)
            .map_err(|e| NetError::Io(e.to_string()))?;
        let packet = AudioPacket {
            seq,
            capture_us: u64::from(seq) * 10_000,
            frame: frame.clone(),
            // 有丢包时带上前一帧：丢一个包时接收端用副本补回
            redundant: previous
                .take()
                .filter(|_| redundancy.on())
                .into_iter()
                .collect(),
        };
        let bytes = packet.encode()?;
        // 链路断了交给会话处理；声音这里只是不再发
        if link.send(Channel::Audio, &bytes).is_err() {
            break;
        }
        previous = Some(frame);
        seq = seq.wrapping_add(1);
    }
    crate::note!(info, "声音：停止发送");
    Ok(())
}

/// 控制端：收声音数据报，每 10 ms 取一帧解码播放。
pub struct Receiver {
    jitter: JitterBuffer,
    decoder: AudioDecoder,
    out: PlaybackHandle,
    bad: u64,
    /// 上次报告丢包率时的统计。
    reported: AudioStats,
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Receiver")
            .field("stats", &self.jitter.stats())
            .finish_non_exhaustive()
    }
}

impl Receiver {
    /// 创建。
    pub fn new(out: PlaybackHandle) -> Result<Self, NetError> {
        Ok(Self {
            jitter: JitterBuffer::new(),
            decoder: AudioDecoder::new().map_err(|e| NetError::Io(e.to_string()))?,
            out,
            bad: 0,
            reported: AudioStats::default(),
        })
    }

    /// 收到一个声音数据报（`now_us` 是本机时刻）。
    pub fn on_datagram(&mut self, now_us: u64, payload: &[u8]) {
        match AudioPacket::decode(payload) {
            Ok(packet) => self.jitter.push(now_us, packet),
            Err(_) => self.bad = self.bad.saturating_add(1),
        }
    }

    /// 每 10 ms 调用一次：取一帧解码播放。
    pub fn tick(&mut self) {
        let pcm = match self.jitter.pop() {
            Play::Normal(frame) => self.decoder.decode(Some(&frame)),
            Play::Expand => self.decoder.decode(None),
            Play::Wait => Ok(vec![0; FRAME_SAMPLES]),
        };
        match pcm {
            Ok(pcm) => self.out.0.play(&pcm),
            // 坏包：这一帧放静音，计数
            Err(_) => {
                self.bad = self.bad.saturating_add(1);
                self.out.0.play(&[0; FRAME_SAMPLES]);
            }
        }
    }

    /// 统计。
    #[must_use]
    pub fn stats(&self) -> AudioStats {
        self.jitter.stats()
    }

    /// 自上次调用以来的丢包率（千分比），发给被控端决定要不要冗余。
    pub fn loss_report(&mut self) -> u16 {
        let now = self.jitter.stats();
        let loss = loss_permille(&self.reported, &now);
        self.reported = now;
        loss
    }

    /// 解不开的包数。
    #[must_use]
    pub const fn bad(&self) -> u64 {
        self.bad
    }

    /// 当前目标延迟（毫秒）。
    #[must_use]
    pub fn target_ms(&self) -> u64 {
        self.jitter.target_ms()
    }
}

/// 合成的声音来源（测试和自测用）：约 440/660 Hz 的三角波，每 10 ms 一帧。
#[derive(Debug, Default)]
pub struct ToneCapture;

impl AudioCapture for ToneCapture {
    fn start(&self) -> Result<mpsc::Receiver<Vec<i16>>, String> {
        let (tx, rx) = mpsc::channel(8);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(10));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Burst);
            let mut n = 0usize;
            loop {
                tick.tick().await;
                let mut pcm = Vec::with_capacity(FRAME_SAMPLES);
                for i in n..n + FRAME {
                    // 两个声道各一个三角波（约 440 Hz 和 660 Hz），振幅 ±8000，整数运算
                    for period in [109usize, 73] {
                        let phase = i % period;
                        let tri = if phase < period / 2 {
                            phase
                        } else {
                            period - phase
                        };
                        let v = tri * 32_000 / period;
                        pcm.push(i16::try_from(v).unwrap_or(0).saturating_sub(8000));
                    }
                }
                n += FRAME;
                if tx.send(pcm).await.is_err() {
                    break;
                }
            }
        });
        Ok(rx)
    }
}

/// 录下播放的声音（测试用）。
#[derive(Debug, Default)]
pub struct Recorder {
    frames: Mutex<Vec<Vec<i16>>>,
}

impl Recorder {
    /// 播放过的帧。
    #[must_use]
    pub fn frames(&self) -> Vec<Vec<i16>> {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl AudioPlayback for Recorder {
    fn play(&self, pcm: &[i16]) {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(pcm.to_vec());
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn receiver_handles_good_bad_and_missing_packets() {
        let rec = Arc::new(Recorder::default());
        let mut rx = Receiver::new(PlaybackHandle(rec.clone())).unwrap();
        // 还没收到包：播静音等待
        rx.tick();
        assert_eq!(rec.frames().len(), 1);
        assert!(rec.frames()[0].iter().all(|&s| s == 0));
        // 解不开的数据报计入坏包
        rx.on_datagram(0, &[1, 2]);
        assert_eq!(rx.bad(), 1);
        // 正常的包
        let mut enc = AudioEncoder::new(DEFAULT_BPS).unwrap();
        for seq in 0..8u32 {
            let frame = enc.encode(&[1000; FRAME_SAMPLES]).unwrap();
            let p = AudioPacket {
                seq,
                capture_us: u64::from(seq) * 10_000,
                frame,
                redundant: vec![],
            };
            rx.on_datagram(u64::from(seq) * 10_000, &p.encode().unwrap());
        }
        // Opus 数据是坏的：这一帧放静音并计数
        let broken = AudioPacket {
            seq: 8,
            capture_us: 80_000,
            frame: vec![0xff; 3],
            redundant: vec![],
        };
        rx.on_datagram(80_000, &broken.encode().unwrap());
        for _ in 0..12 {
            rx.tick();
        }
        // 9 个包一起到，超过目标的部分加速丢掉，其余正常播放
        let st = rx.stats();
        assert!(st.normal + st.accelerated >= 8, "{st:?}");
        assert!(st.accelerated > 0, "{st:?}");
        assert_eq!(rx.bad(), 2);
        assert!(rx.target_ms() >= peerdesk_core::audio::MIN_TARGET_MS);
        assert!(format!("{rx:?}").contains("Receiver"));
    }

    #[test]
    fn loss_report_counts_missing_frames() {
        let rec = Arc::new(Recorder::default());
        let mut rx = Receiver::new(PlaybackHandle(rec)).unwrap();
        let mut enc = AudioEncoder::new(DEFAULT_BPS).unwrap();
        // 每 10 ms 到一个包，序号 5 和 15 丢了；按真实节奏一边收一边播
        for seq in 0..40u32 {
            if seq % 10 != 5 {
                let frame = enc.encode(&[1000; FRAME_SAMPLES]).unwrap();
                let p = AudioPacket {
                    seq,
                    capture_us: u64::from(seq) * 10_000,
                    frame,
                    redundant: vec![],
                };
                rx.on_datagram(u64::from(seq) * 10_000, &p.encode().unwrap());
            }
            rx.tick();
        }
        let loss = rx.loss_report();
        assert!(loss > 0, "丢了包却报告 {loss}");
        // 报告后重新计数
        assert_eq!(rx.loss_report(), 0);
    }

    #[test]
    fn handles_are_printable() {
        let cap = CaptureHandle(Arc::new(ToneCapture));
        let play = PlaybackHandle(Arc::new(Recorder::default()));
        let (_tx, on) = tokio::sync::watch::channel(false);
        let out = AudioOut {
            playback: play.clone(),
            on,
        };
        assert_eq!(format!("{cap:?}"), "CaptureHandle");
        assert_eq!(format!("{play:?}"), "PlaybackHandle");
        assert!(format!("{out:?}").contains("on: false"));
    }
}
