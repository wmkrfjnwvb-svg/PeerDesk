//! 诊断日志：每个模块每秒一行（平均值 / 最大值），加上各模块的决定和原因。
//!
//! 为什么：2026-11-25 用户实测画面糊、帧率低，原来的日志只有一行汇总，
//! 看不出是采集、编码、码率估计、发送、网络还是接收端的哪一步出的问题，也看不出模块之间怎么互相影响
//! （编码慢 → 帧率降 → 每帧预算变大 → 大帧突发丢包 → 码率估计降到底）。
//! 这里只负责汇总和格式化（可以单独测试），数据由被控端、控制端的会话循环交进来。

use peerdesk_core::video::EncodeTiming;

use crate::node::QuicStats;

/// 一个量在这一秒里的总和、最大值、次数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stat {
    sum: u64,
    max: u64,
    n: u64,
}

impl Stat {
    /// 记一个值。
    pub fn add(&mut self, v: u64) {
        self.sum = self.sum.saturating_add(v);
        self.max = self.max.max(v);
        self.n = self.n.saturating_add(1);
    }

    /// 次数。
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.n
    }

    /// 平均值（没有值时为 0）。
    #[must_use]
    pub fn avg(&self) -> u64 {
        self.sum.checked_div(self.n).unwrap_or(0)
    }

    /// 最大值。
    #[must_use]
    pub const fn max(&self) -> u64 {
        self.max
    }

    /// 微秒写成“平均/最大 ms”（保留一位小数）。
    #[must_use]
    pub fn ms(&self) -> String {
        format!("{}/{}", tenth_ms(self.avg()), tenth_ms(self.max))
    }
}

/// 微秒写成毫秒，一位小数。
#[must_use]
pub fn tenth_ms(us: u64) -> String {
    let tenths = (us + 50) / 100;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// 十分之一毫秒（可以为负，例如延迟趋势在下降）写成“-1.5”这样的毫秒。
#[must_use]
pub fn signed_tenths(v: i32) -> String {
    let a = v.unsigned_abs();
    format!("{}{}.{}", if v < 0 { "-" } else { "" }, a / 10, a % 10)
}

/// 被控端编码这一秒的汇总。
#[derive(Debug, Clone, Default)]
pub struct EncodeWindow {
    /// 编码线程处理一个任务的总时间（含转换、缩放、编码、等硬件交帧）。
    total: Stat,
    scale: Stat,
    convert: Stat,
    encode: Stat,
    bytes: Stat,
    keys: Vec<&'static str>,
    hardware: Option<bool>,
    direct: Option<bool>,
    size: (u16, u16),
}

impl EncodeWindow {
    /// 记一次编码任务。`bytes` 是编出来的帧大小（这次没有出帧时为 `None`）。
    pub fn add(&mut self, took_us: u64, t: &EncodeTiming, bytes: Option<usize>) {
        self.total.add(took_us);
        if t.width > 0 {
            self.scale.add(t.scale_us);
            self.convert.add(t.convert_us);
            self.encode.add(t.encode_us);
            self.hardware = Some(t.hardware);
            self.direct = Some(t.nv12_direct);
            self.size = (t.width, t.height);
        }
        if let Some(b) = bytes {
            self.bytes.add(u64::try_from(b).unwrap_or(u64::MAX));
        }
        if !t.key_reason.is_empty() {
            self.keys.push(t.key_reason);
        }
    }

    /// 这一秒的一行；这一秒没有编码任务时为 `None`。取完清零。
    pub fn take_line(&mut self) -> Option<String> {
        if self.total.count() == 0 {
            return None;
        }
        let w = std::mem::take(self);
        let yes_no = |v: Option<bool>, yes: &str, no: &str| match v {
            Some(true) => yes.to_owned(),
            Some(false) => no.to_owned(),
            None => "—".to_owned(),
        };
        let keys = if w.keys.is_empty() {
            "无".to_owned()
        } else {
            w.keys.join("、")
        };
        Some(format!(
            "编码 任务 {} 次（其中转换并编码 {} 次）：总耗时 {} ms，缩放 {} ms，颜色转换 {} ms，编码器 {} ms；\
             出帧 {} 个，帧大小 {}/{} KB；尺寸 {}×{}，{}，{}；关键帧 {} 个（{}）",
            w.total.count(),
            w.encode.count(),
            w.total.ms(),
            w.scale.ms(),
            w.convert.ms(),
            w.encode.ms(),
            w.bytes.count(),
            w.bytes.avg() / 1024,
            w.bytes.max() / 1024,
            w.size.0,
            w.size.1,
            yes_no(w.hardware, "硬件编码", "软件编码"),
            yes_no(w.direct, "采集直接交 NV12", "CPU 转换颜色"),
            w.keys.len(),
            keys,
        ))
    }
}

/// QUIC 层这一秒的变化：应用交了多少、QUIC 实际发了多少（差的是发送缓冲满被挤掉的）、
/// 收到多少、应用读了多少（差的是读得慢被丢的）、QUIC 判定网络丢了多少。
#[must_use]
pub fn quic_line(prev: &QuicStats, now: &QuicStats) -> String {
    let d = |a: u64, b: u64| b.saturating_sub(a);
    format!(
        "QUIC 应用交出 {} 个数据报，实际发出 {}（发送缓冲挤掉约 {}，超 MTU {}）；收到 {}，应用读走 {}；\
         网络丢包 {} 个（{} KB），拥塞事件 {} 次，拥塞窗口 {} KB，UDP 发 {} 收 {}，MTU {}",
        d(prev.app_sent, now.app_sent),
        d(prev.datagram_frames_sent, now.datagram_frames_sent),
        d(prev.app_sent, now.app_sent)
            .saturating_sub(d(prev.datagram_frames_sent, now.datagram_frames_sent))
            .saturating_sub(d(prev.app_too_large, now.app_too_large)),
        d(prev.app_too_large, now.app_too_large),
        d(prev.datagram_frames_received, now.datagram_frames_received),
        d(prev.app_read, now.app_read),
        d(prev.lost_packets, now.lost_packets),
        d(prev.lost_bytes, now.lost_bytes) / 1024,
        d(prev.congestion_events, now.congestion_events),
        now.cwnd / 1024,
        d(prev.udp_sent, now.udp_sent),
        d(prev.udp_received, now.udp_received),
        now.mtu,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_average_and_max() {
        let mut s = Stat::default();
        assert_eq!((s.avg(), s.max(), s.ms()), (0, 0, "0.0/0.0".to_owned()));
        s.add(1_000);
        s.add(3_050);
        assert_eq!((s.count(), s.avg(), s.max()), (2, 2_025, 3_050));
        assert_eq!(s.ms(), "2.0/3.1");
        assert_eq!(tenth_ms(149), "0.1");
        assert_eq!(
            (signed_tenths(-15), signed_tenths(32)),
            ("-1.5".to_owned(), "3.2".to_owned())
        );
    }

    #[test]
    fn encode_window_line() {
        let mut w = EncodeWindow::default();
        assert!(w.take_line().is_none(), "这一秒没有编码");
        let t = EncodeTiming {
            scale_us: 30_000,
            convert_us: 20_000,
            encode_us: 10_000,
            wait_us: 0,
            dropped: false,
            hardware: true,
            nv12_direct: false,
            width: 1872,
            height: 1170,
            key_reason: "控制端窗口大小变了",
        };
        w.add(65_000, &t, Some(80 * 1024));
        // 只取硬件攒着的帧（没有转换）
        w.add(1_000, &EncodeTiming::default(), None);
        let line = w.take_line().unwrap();
        for part in [
            "任务 2 次（其中转换并编码 1 次）",
            "缩放 30.0/30.0 ms",
            "颜色转换 20.0/20.0 ms",
            "编码器 10.0/10.0 ms",
            "出帧 1 个，帧大小 80/80 KB",
            "尺寸 1872×1170，硬件编码，CPU 转换颜色",
            "关键帧 1 个（控制端窗口大小变了）",
        ] {
            assert!(line.contains(part), "{part} 不在：{line}");
        }
        assert!(w.take_line().is_none(), "取完清零");
        let mut soft = EncodeWindow::default();
        soft.add(
            5_000,
            &EncodeTiming {
                width: 2,
                height: 2,
                nv12_direct: true,
                ..EncodeTiming::default()
            },
            None,
        );
        let line = soft.take_line().unwrap();
        assert!(
            line.contains("软件编码，采集直接交 NV12") && line.contains("关键帧 0 个（无）"),
            "{line}"
        );
        let mut empty = EncodeWindow::default();
        empty.add(1, &EncodeTiming::default(), None);
        assert!(empty.take_line().unwrap().contains("尺寸 0×0，—，—"));
    }

    #[test]
    fn quic_line_shows_where_datagrams_went() {
        let prev = QuicStats::default();
        let now = QuicStats {
            app_sent: 100,
            app_too_large: 2,
            datagram_frames_sent: 90,
            datagram_frames_received: 80,
            app_read: 75,
            lost_packets: 5,
            lost_bytes: 6_000,
            congestion_events: 1,
            cwnd: 64 * 1024,
            udp_sent: 95,
            udp_received: 85,
            mtu: 1200,
        };
        let line = quic_line(&prev, &now);
        for part in [
            "应用交出 100 个数据报，实际发出 90（发送缓冲挤掉约 8，超 MTU 2）",
            "收到 80，应用读走 75",
            "网络丢包 5 个（5 KB），拥塞事件 1 次，拥塞窗口 64 KB",
            "MTU 1200",
        ] {
            assert!(line.contains(part), "{part} 不在：{line}");
        }
    }
}
