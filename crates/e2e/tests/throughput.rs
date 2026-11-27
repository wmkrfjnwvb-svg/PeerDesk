//! 高码率守护（只测传输层）：4K、144 帧、约 100 Mbps 的流量——每帧约 87 KB、七十多个分片——完整走一遍
//! 发送端分片和纠错、1% 随机丢包、接收端重组、重传请求和补发。
//!
//! 编码器按这个帧大小交出数据，不编真实画面：软件编码 4K144 在仿真里太慢，这里考的是传输层（分片、RS 纠错、
//! 重组、重传缓存、确认）能不能跟上每秒一万多个包。以后做游戏模式（4K、144 帧）之前的底线，见
//! `docs/research/L-framework.md` 4.5。网络本身（节奏器、估计器）由 e2e 的其他场景覆盖。
#![allow(clippy::unwrap_used, clippy::print_stdout, clippy::cast_precision_loss)]

use std::time::Instant;

use peerdesk_core::adaptive::Profile;
use peerdesk_core::error::Error;
use peerdesk_core::media::{MediaReceiver, MediaSender, Outgoing, SenderConfig};
use peerdesk_core::screen::Screen;
use peerdesk_core::time::Time;
use peerdesk_core::video::{Codec, EncodedFrame, FrameHeader, VideoEncoder};

/// 帧率和码率：4K144 游戏的常见设置。
const FPS: u64 = 144;
const BPS: u64 = 100_000_000;
/// 一帧多大（字节）。
const FRAME_BYTES: usize = (BPS / 8 / FPS) as usize;

/// 按固定大小交出“编码好的”帧（伪随机内容，纠错要真算），第一帧是关键帧。
struct SizedEncoder {
    seed: u64,
    started: bool,
}

impl VideoEncoder for SizedEncoder {
    fn codec(&self) -> Codec {
        Codec::H264
    }
    fn encode(
        &mut self,
        _id: u32,
        _screen: &Screen,
        _changed: bool,
        _budget: usize,
    ) -> Result<Option<EncodedFrame>, Error> {
        let key = !self.started;
        self.started = true;
        let body: Vec<u8> = (0..FRAME_BYTES)
            .map(|_| {
                self.seed ^= self.seed << 13;
                self.seed ^= self.seed >> 7;
                self.seed ^= self.seed << 17;
                self.seed.to_le_bytes()[0]
            })
            .collect();
        let header = FrameHeader {
            codec: Codec::H264,
            key,
            width: 3840,
            height: 2160,
        };
        Ok(Some(EncodedFrame {
            payload: header.frame(&body),
            key,
        }))
    }
    fn on_feedback(&mut self, _floor: u32, _mask: u32) {}
    fn on_lost(&mut self, _frame_id: u32) {}
    fn wants_refine(&self) -> bool {
        false
    }
}

#[test]
fn transport_keeps_up_with_4k144_at_100_mbps() {
    // 发布构建（CI 的端到端任务）跑 2 秒的流量并计时；调试构建（覆盖率）只跑一小段、不计时
    let seconds: u64 = if cfg!(debug_assertions) { 1 } else { 2 };
    let frames = if cfg!(debug_assertions) {
        20
    } else {
        FPS * seconds
    };
    let mut tx = MediaSender::new(
        SenderConfig {
            fps: u32::try_from(FPS).unwrap(),
            start_bps: BPS,
            max_bps: BPS * 2,
            profile: Profile::WIRELESS,
            ..SenderConfig::default()
        },
        Box::new(SizedEncoder {
            seed: 0x9e37_79b9_7f4a_7c15,
            started: false,
        }),
    );
    let mut rx = MediaReceiver::detached(Time::ZERO, 250_000);
    rx.set_keyframe_grace(true);
    let screen = Screen::new(64, 36, 0x0080_8080);
    let interval = 1_000_000 / FPS;
    let mut net = Lossy {
        seq: 0,
        rng: 0x2545_f491_4f6c_dd1d,
        packets: 0,
        lost: 0,
    };
    let mut delivered = 0u64;
    let started = Instant::now();
    for i in 0..frames {
        let now = Time(i * interval);
        let out = tx.on_frame(now, &screen, true, 20_000).unwrap();
        // 每帧一份反馈（144 帧时约 7 ms 一次，比产品的 20 ms 更勤）：重传请求、补发
        let fb_at = now.plus(interval / 2);
        for o in &out {
            delivered += net.send(&mut tx, &mut rx, now, o);
        }
        let fb = rx.feedback_datagram(fb_at);
        let again = tx.on_feedback_datagram(fb_at, &fb).unwrap();
        for o in &again {
            delivered += net.send(&mut tx, &mut rx, fb_at, o);
        }
    }
    let took = started.elapsed();
    let stats = tx.stats();
    println!(
        "4K144 100 Mbps：{frames} 帧、{} 个数据报（丢 {}），交出 {delivered} 帧，关键帧 {}，重传 {}；\
         传输层耗时 {:.0} ms（流量时长 {} ms）",
        net.packets,
        net.lost,
        stats.keyframes,
        stats.retransmitted,
        took.as_secs_f64() * 1000.0,
        frames * interval / 1000
    );
    // 丢包靠纠错和重传补回来：不出新的关键帧，几乎每帧都交出（最后几帧可能还在等重传）
    assert_eq!(stats.keyframes, 1, "丢包不该变成关键帧");
    assert!(delivered + 3 >= frames, "交出 {delivered} / {frames} 帧");
    if !cfg!(debug_assertions) {
        // 传输层（两端合计）处理 2 秒的流量要远少于 2 秒：本机实测见下，CI 的机器慢一些，留足余量
        assert!(
            took.as_millis() < 1_000,
            "传输层处理 {} ms 的流量用了 {} ms",
            frames * interval / 1000,
            took.as_millis()
        );
    }
}

/// 发送端到接收端：记“已发出”、编序号，1% 随机丢包。返回接收端交出了几帧。
struct Lossy {
    seq: u32,
    rng: u64,
    packets: u64,
    lost: u64,
}

impl Lossy {
    fn send(
        &mut self,
        tx: &mut MediaSender,
        rx: &mut MediaReceiver,
        now: Time,
        o: &Outgoing,
    ) -> u64 {
        tx.on_packet_sent(now, self.seq, o);
        self.seq = self.seq.wrapping_add(1);
        self.packets += 1;
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        if self.rng.is_multiple_of(100) {
            self.lost += 1;
            return 0;
        }
        let jobs = rx
            .on_datagram_deferred(now, now, self.seq, o.flags, &o.body)
            .unwrap();
        u64::try_from(jobs.len()).unwrap()
    }
}
