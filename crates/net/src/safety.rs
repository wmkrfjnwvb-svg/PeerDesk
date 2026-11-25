//! QUIC 层的拥塞控制：只做安全网，不和媒体层的码率控制抢。
//!
//! 为什么（2026-11-25，`docs/research/K-pipeline.md` P1、`L-framework.md` 第 4 节）：
//! 以前用 quinn 的 BBR。本机 1.5 Mbps 限速实测：BBR 把窗口压到 11 KB，我们的数据报在 QUIC 里积压、
//! 被静默挤掉，媒体层的码率估计只看到“排队越来越高”，接收端当网络丢包要重传。两套完整的拥塞控制叠在一起
//! 互相干扰，RTP over QUIC 草案（draft-ietf-avtcore-rtp-over-quic §8.2）建议只用媒体层的估计、
//! 按低于容量的速率节奏发送，让 QUIC 的拥塞控制基本不触发。BBR 每 10 秒的 PROBE_RTT 还会把窗口压到
//! 4 个包（draft-huitema-ccwg-bbr-realtime），对实时画面是定期卡顿。
//!
//! 规则：
//! - 窗口按实测送达速率给：约 [`BDP_MULTIPLE`] 倍“送达速率 × 往返时间”，下限 [`MIN_WINDOW`]、
//!   上限 [`MAX_WINDOW`]。媒体层按估计节奏发送时在途量约等于一个往返的量，远低于窗口，QUIC 不挡；
//!   只有媒体层的估计严重出错（发得比网络能送的快好几倍）时才拦住，防止把队列堆满。
//! - 送达速率取最近 [`RATE_MEMORY`] 里的最大值，网络变好时窗口跟着放大；
//! - 单个丢包不收窗口：Wi-Fi 的随机丢包不是拥塞，丢没丢由媒体层判断（重传、纠错、码率控制）；
//! - 持续拥塞（RFC 9002 的 persistent congestion：好几个往返一个确认都没有）时窗口减半。
//!
//! 文件传输走同一条连接的流，也受这个窗口约束；画面和文件之间怎么分带宽由媒体层的分配器决定。

use std::time::{Duration, Instant};

/// 窗口下限（字节）：约 45 个满包，慢速链路上一帧也能分几批发出。
pub const MIN_WINDOW: u64 = 64 * 1024;
/// 窗口上限（字节）：30 Mbps × 2 秒。
pub const MAX_WINDOW: u64 = 8 * 1024 * 1024;
/// 还没收到确认时的窗口（字节）：第一个关键帧能直接发出，由媒体层的节奏器控制速度。
pub const INITIAL_WINDOW: u64 = 512 * 1024;
/// 窗口是“送达速率 × 往返时间”的几倍。
pub const BDP_MULTIPLE: u64 = 4;
/// 送达速率取多久之内的最大值。
pub const RATE_MEMORY: Duration = Duration::from_secs(10);
/// 往返时间不足这个值时按它算（局域网上往返只有零点几毫秒，窗口不能因此太小）。
const MIN_RTT: Duration = Duration::from_millis(20);
/// 测送达速率的最短时间段。
const MIN_SAMPLE: Duration = Duration::from_millis(50);

/// 安全网窗口的计算（不依赖 quinn，便于单独测试）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SafetyWindow {
    window: u64,
    /// 这一段时间里确认的字节数和起点。
    acked: u64,
    sample_start: Option<Instant>,
    /// 最近一段时间里最大的送达速率（字节每秒）和测到的时刻。
    best: Option<(u64, Instant)>,
    srtt: Duration,
}

impl Default for SafetyWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl SafetyWindow {
    /// 创建，窗口从 [`INITIAL_WINDOW`] 开始。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            window: INITIAL_WINDOW,
            acked: 0,
            sample_start: None,
            best: None,
            srtt: MIN_RTT,
        }
    }

    /// 当前窗口（字节）。
    #[must_use]
    pub const fn window(&self) -> u64 {
        self.window
    }

    /// 收到确认：`bytes` 字节送达，当前平滑往返时间 `srtt`。
    pub fn on_ack(&mut self, now: Instant, bytes: u64, srtt: Duration) {
        self.srtt = srtt.max(MIN_RTT);
        let start = *self.sample_start.get_or_insert(now);
        self.acked = self.acked.saturating_add(bytes);
        let span = now.saturating_duration_since(start);
        if span < MIN_SAMPLE.max(self.srtt) {
            return;
        }
        let rate = u64::try_from(
            u128::from(self.acked).saturating_mul(1_000_000) / span.as_micros().max(1),
        )
        .unwrap_or(u64::MAX);
        self.acked = 0;
        self.sample_start = Some(now);
        let keep = self
            .best
            .filter(|(r, at)| *r >= rate && now.saturating_duration_since(*at) < RATE_MEMORY);
        self.best = keep.or(Some((rate, now)));
        let best = self.best.map_or(0, |(r, _)| r);
        let bdp = u64::try_from(u128::from(best).saturating_mul(self.srtt.as_micros()) / 1_000_000)
            .unwrap_or(u64::MAX);
        self.window = bdp
            .saturating_mul(BDP_MULTIPLE)
            .clamp(MIN_WINDOW, MAX_WINDOW);
    }

    /// QUIC 判定了拥塞事件。只有持续拥塞才收窗口（减半），单个丢包不管。
    pub fn on_congestion(&mut self, persistent: bool) {
        if persistent {
            self.window = (self.window / 2).max(MIN_WINDOW);
            self.best = None;
        }
    }
}

/// quinn 的拥塞控制接口（薄胶水，逻辑在 [`SafetyWindow`]）。
#[derive(Debug, Clone, Copy, Default)]
pub struct SafetyNet {
    inner: SafetyWindow,
}

impl quinn::congestion::Controller for SafetyNet {
    fn on_ack(
        &mut self,
        now: Instant,
        _sent: Instant,
        bytes: u64,
        _app_limited: bool,
        rtt: &quinn_proto::RttEstimator,
    ) {
        self.inner.on_ack(now, bytes, rtt.get());
    }

    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        is_persistent_congestion: bool,
        _lost_bytes: u64,
    ) {
        self.inner.on_congestion(is_persistent_congestion);
    }

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    fn window(&self) -> u64 {
        self.inner.window()
    }

    fn clone_box(&self) -> Box<dyn quinn::congestion::Controller> {
        Box::new(*self)
    }

    fn initial_window(&self) -> u64 {
        INITIAL_WINDOW
    }

    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }
}

/// 建 [`SafetyNet`] 的工厂。
#[derive(Debug, Clone, Copy, Default)]
pub struct SafetyNetConfig;

impl quinn::congestion::ControllerFactory for SafetyNetConfig {
    fn build(
        self: std::sync::Arc<Self>,
        _now: Instant,
        _current_mtu: u16,
    ) -> Box<dyn quinn::congestion::Controller> {
        Box::new(SafetyNet::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    /// 按 `bps` 送达 `secs` 秒，往返 `rtt`；返回最后的时刻。
    fn deliver(
        w: &mut SafetyWindow,
        start: Instant,
        bps: u64,
        rtt: Duration,
        secs: u64,
    ) -> Instant {
        let mut now = start;
        for _ in 0..secs * 100 {
            now += 10 * MS;
            w.on_ack(now, bps / 8 / 100, rtt);
        }
        now
    }

    #[test]
    fn window_follows_the_delivery_rate() {
        let mut w = SafetyWindow::default();
        assert_eq!(w.window(), INITIAL_WINDOW);
        let t0 = Instant::now();
        // 10 Mbps、往返 40 ms：一个往返 50 KB，窗口约 4 倍
        let now = deliver(&mut w, t0, 10_000_000, 40 * MS, 2);
        let bdp = 10_000_000 / 8 * 40 / 1000;
        assert!((bdp * 3..=bdp * 5).contains(&w.window()), "{}", w.window());
        // 慢链路：下限
        let mut slow = SafetyWindow::default();
        deliver(&mut slow, t0, 500_000, 40 * MS, 2);
        assert_eq!(slow.window(), MIN_WINDOW);
        // 局域网上往返很短：按 20 ms 算，窗口不至于太小
        let mut lan = SafetyWindow::default();
        deliver(&mut lan, t0, 100_000_000, Duration::from_micros(300), 2);
        assert!(lan.window() >= 100_000_000 / 8 * 20 / 1000 * 3);
        // 送达变慢：最大值记 10 秒，之后跟着降
        let now = deliver(&mut w, now, 2_000_000, 40 * MS, 5);
        assert!(w.window() > MIN_WINDOW, "还记着之前的速率");
        deliver(&mut w, now, 2_000_000, 40 * MS, 11);
        assert_eq!(w.window(), MIN_WINDOW, "{}", w.window());
    }

    #[test]
    fn only_persistent_congestion_shrinks_the_window() {
        let mut w = SafetyWindow::default();
        deliver(&mut w, Instant::now(), 50_000_000, 40 * MS, 2);
        let before = w.window();
        w.on_congestion(false);
        assert_eq!(w.window(), before, "单个丢包不管");
        w.on_congestion(true);
        assert_eq!(w.window(), before / 2);
        for _ in 0..20 {
            w.on_congestion(true);
        }
        assert_eq!(w.window(), MIN_WINDOW);
        // 太大的速率：上限
        let mut big = SafetyWindow::default();
        deliver(&mut big, Instant::now(), 1_000_000_000, 100 * MS, 2);
        assert_eq!(big.window(), MAX_WINDOW);
    }

    #[test]
    fn quinn_glue_reports_the_window() {
        use quinn::congestion::{Controller, ControllerFactory};
        let c = std::sync::Arc::new(SafetyNetConfig).build(Instant::now(), 1200);
        assert_eq!(c.window(), INITIAL_WINDOW);
        assert_eq!(c.initial_window(), INITIAL_WINDOW);
        let mut net = SafetyNet::default();
        net.on_congestion_event(Instant::now(), Instant::now(), true, 1000);
        net.on_mtu_update(1400);
        assert_eq!(net.window(), INITIAL_WINDOW / 2);
        assert_eq!(net.clone_box().window(), INITIAL_WINDOW / 2);
        assert!(Box::new(net).into_any().downcast::<SafetyNet>().is_ok());
    }
}
