//! 包一级的 TCP NewReno：发送端和接收端的状态机，不依赖路径模型，时间由调用方传入。
//!
//! 依据：RFC 5681（慢启动、拥塞避免、快速重传、快速恢复）、RFC 6582（NewReno：部分确认）、
//! RFC 6298（RTO 的计算）。不追求和任何内核逐字节一样，只要“会把队列填满、丢包后退让”两个行为对
//! （`a2-plan.md` 第 4 节）。简化和取舍，每一条都影响结果，所以写在这里：
//! - 序号以“包”为单位（一个包 = 一个 MSS），窗口用字节记（`cwnd = n × MSS`），公式照 RFC 5681 的字节写法；
//! - **接收端每个包都回确认，不用延迟确认**（RFC 1122 允许每两个包确认一次）。后果：慢启动时窗口增长比常见内核快
//!   （每个包 +1 MSS，而不是每两个包 +1 MSS），拥塞避免时每个往返仍然只涨 1 个 MSS，吞吐公式不受影响；
//! - 没有 SACK（RFC 2018）：一个窗口里丢多个包时，NewReno 每个往返只能补一个，这正是 NewReno 的特点；
//! - 没有限制性重传（RFC 3042）、没有窗口缩放和接收窗口（接收端窗口无限大，只受拥塞窗口限制）、没有 ECN；
//! - 超时之后回到 `snd_una` 重发（go-back-N），随后的确认会跳过接收端已经收到的包；
//! - RTO 的最小值可设（RFC 6298 第 2.4 条建议 1 s，测试里可以设小）；
//! - 只测一个包的往返时间，重传的包不取样（Karn 算法）。

use std::collections::BTreeSet;

use crate::Time;

/// TCP 参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpConfig {
    /// 数据包大小（字节），也是 MSS。
    pub mss_bytes: usize,
    /// 确认包大小（字节）。
    pub ack_bytes: usize,
    /// 初始窗口（包数）。RFC 6928 允许到 10；RFC 5681 是 2 到 4。
    pub init_cwnd_segments: u64,
    /// RTO 下限（微秒）。RFC 6298 建议 1 s。
    pub min_rto_us: u64,
    /// RTO 上限（微秒）。RFC 6298 要求至少 60 s。
    pub max_rto_us: u64,
    /// 没有往返时间样本之前的 RTO（微秒），RFC 6298 是 1 s。
    pub initial_rto_us: u64,
    /// 时钟粒度 G（微秒），RFC 6298 的 `RTO = SRTT + max(G, 4 × RTTVAR)`。
    pub granularity_us: u64,
}

impl Default for TcpConfig {
    fn default() -> Self {
        Self {
            mss_bytes: 1250,
            ack_bytes: 40,
            init_cwnd_segments: 10,
            min_rto_us: 1_000_000,
            max_rto_us: 60_000_000,
            initial_rto_us: 1_000_000,
            granularity_us: 1_000,
        }
    }
}

/// 发送端的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TcpStats {
    /// 发出的数据包（含重传）。
    pub sent: u64,
    /// 其中的重传。
    pub retransmits: u64,
    /// 快速重传的次数（进入快速恢复）。
    pub fast_retransmits: u64,
    /// 超时的次数。
    pub timeouts: u64,
}

/// 发送端。
#[derive(Debug, Clone)]
pub struct TcpSender {
    cfg: TcpConfig,
    /// 总共要发的包数；`None` 是发不完（长连接）。
    total: Option<u64>,
    /// 最早没确认的包（累计确认号）。
    una: u64,
    /// 下一个要发的新包。
    nxt: u64,
    /// 超时前发出过的最大序号 + 1：小于它的包都是重传。
    high_water: u64,
    cwnd: u64,
    ssthresh: u64,
    dupacks: u32,
    /// 最近一次丢包时发出过的最大序号（RFC 6582 的 `recover`）。
    recover: Option<u64>,
    in_recovery: bool,
    srtt: Option<u64>,
    rttvar: u64,
    rto: u64,
    /// 正在计时的包（序号，发出的时刻）。
    timed: Option<(u64, Time)>,
    rto_at: Option<Time>,
    stats: TcpStats,
}

impl TcpSender {
    /// 新发送端；`total_segments` 为 `None` 表示一直发下去。
    #[must_use]
    pub fn new(cfg: TcpConfig, total_segments: Option<u64>) -> Self {
        let mss = cfg.mss_bytes as u64;
        Self {
            cfg,
            total: total_segments,
            una: 0,
            nxt: 0,
            high_water: 0,
            cwnd: cfg.init_cwnd_segments.max(1) * mss,
            ssthresh: u64::MAX,
            dupacks: 0,
            recover: None,
            in_recovery: false,
            srtt: None,
            rttvar: 0,
            rto: cfg.initial_rto_us,
            timed: None,
            rto_at: None,
            stats: TcpStats::default(),
        }
    }

    fn mss(&self) -> u64 {
        self.cfg.mss_bytes as u64
    }

    /// 数据包大小（字节）。
    #[must_use]
    pub const fn mss_bytes(&self) -> u64 {
        self.cfg.mss_bytes as u64
    }

    /// 计数。
    #[must_use]
    pub const fn stats(&self) -> TcpStats {
        self.stats
    }

    /// 拥塞窗口（字节）。
    #[must_use]
    pub const fn cwnd(&self) -> u64 {
        self.cwnd
    }

    /// 慢启动门限（字节）。
    #[must_use]
    pub const fn ssthresh(&self) -> u64 {
        self.ssthresh
    }

    /// 平滑往返时间（微秒）。
    #[must_use]
    pub const fn srtt(&self) -> Option<u64> {
        self.srtt
    }

    /// 现在的 RTO（微秒）。
    #[must_use]
    pub const fn rto(&self) -> u64 {
        self.rto
    }

    /// 是否在快速恢复里。
    #[must_use]
    pub const fn in_recovery(&self) -> bool {
        self.in_recovery
    }

    /// 发完并且全部确认了。
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.total.is_some_and(|t| self.una >= t)
    }

    /// 下一个超时的时刻（没有在途数据时为 `None`）。
    #[must_use]
    pub const fn next_timer(&self) -> Option<Time> {
        self.rto_at
    }

    /// 开始发送：把初始窗口里的包放进 `out`。
    pub fn start(&mut self, now: Time, out: &mut Vec<u64>) {
        self.fill(now, out);
    }

    /// 窗口允许就发新包。
    fn fill(&mut self, now: Time, out: &mut Vec<u64>) {
        let mss = self.mss();
        while (self.nxt - self.una) * mss < self.cwnd && self.total.is_none_or(|t| self.nxt < t) {
            let seq = self.nxt;
            self.nxt += 1;
            self.emit(now, seq, out);
        }
        if self.rto_at.is_none() && self.nxt > self.una {
            self.rto_at = Some(now.plus(self.rto));
        }
    }

    /// 发一个包：登记计数；小于 `high_water` 的是重传，不计时（Karn）。
    fn emit(&mut self, now: Time, seq: u64, out: &mut Vec<u64>) {
        self.stats.sent += 1;
        if seq < self.high_water {
            self.stats.retransmits += 1;
        } else {
            self.high_water = seq + 1;
            if self.timed.is_none() {
                self.timed = Some((seq, now));
            }
        }
        out.push(seq);
    }

    /// 重传 `seq`（快速重传、部分确认）。
    fn retransmit(&mut self, seq: u64, out: &mut Vec<u64>) {
        self.stats.sent += 1;
        self.stats.retransmits += 1;
        if self.timed.is_some_and(|(s, _)| s <= seq) {
            self.timed = None;
        }
        out.push(seq);
    }

    /// RFC 6298 第 2 节：用一个往返时间样本更新 SRTT、RTTVAR、RTO。
    fn sample_rtt(&mut self, r: u64) {
        match self.srtt {
            None => {
                self.srtt = Some(r);
                self.rttvar = r / 2;
            }
            Some(srtt) => {
                self.rttvar = (3 * self.rttvar + srtt.abs_diff(r)) / 4;
                self.srtt = Some((7 * srtt + r) / 8);
            }
        }
        let srtt = self.srtt.unwrap_or(r);
        let rto = srtt + self.cfg.granularity_us.max(4 * self.rttvar);
        self.rto = rto.clamp(self.cfg.min_rto_us, self.cfg.max_rto_us);
    }

    /// 收到累计确认 `ack`（接收端下一个想要的包的序号）。
    pub fn on_ack(&mut self, now: Time, ack: u64, out: &mut Vec<u64>) {
        let mss = self.mss();
        if ack > self.una {
            let acked = ack - self.una;
            self.una = ack;
            self.nxt = self.nxt.max(ack);
            self.dupacks = 0;
            if let Some((s, t0)) = self.timed {
                if s < ack {
                    self.sample_rtt(now.since(t0));
                    self.timed = None;
                }
            }
            if self.in_recovery {
                if self.recover.is_some_and(|r| ack > r) {
                    // 完整确认（RFC 6582）：退出快速恢复，窗口收到 ssthresh 以内
                    let flight = (self.nxt - self.una) * mss;
                    self.cwnd = self.ssthresh.min(flight.max(mss) + mss);
                    self.in_recovery = false;
                } else {
                    // 部分确认：重传下一个没确认的包；窗口减去新确认的数据，新确认至少一个包再加回一个 MSS
                    self.retransmit(self.una, out);
                    self.cwnd = self.cwnd.saturating_sub(acked * mss).max(mss) + mss;
                }
            } else if self.cwnd < self.ssthresh {
                self.cwnd += (acked * mss).min(mss); // 慢启动：每个确认涨 min(N, MSS)
            } else {
                self.cwnd += (mss * mss / self.cwnd).max(1); // 拥塞避免：每个往返约涨一个 MSS
            }
            self.rto_at = (self.una < self.nxt).then(|| now.plus(self.rto));
        } else if self.nxt > self.una {
            self.dupacks += 1;
            if self.in_recovery {
                self.cwnd += mss; // 窗口膨胀：每个重复确认说明有一个包离开了网络
            } else if self.dupacks == 3 && self.recover.is_none_or(|r| self.una > r) {
                // 快速重传
                let flight = (self.nxt - self.una) * mss;
                self.ssthresh = (flight / 2).max(2 * mss);
                self.recover = Some(self.nxt - 1);
                self.in_recovery = true;
                self.cwnd = self.ssthresh + 3 * mss;
                self.stats.fast_retransmits += 1;
                self.retransmit(self.una, out);
                self.rto_at = Some(now.plus(self.rto));
            }
        }
        self.fill(now, out);
    }

    /// 超时（`next_timer` 到了）：RFC 5681 第 3.1 条。
    pub fn on_timeout(&mut self, now: Time, out: &mut Vec<u64>) {
        if self.una >= self.nxt {
            self.rto_at = None;
            return;
        }
        let mss = self.mss();
        let flight = (self.nxt - self.una) * mss;
        self.ssthresh = (flight / 2).max(2 * mss);
        self.cwnd = mss;
        self.in_recovery = false;
        self.dupacks = 0;
        self.recover = Some(self.nxt - 1);
        self.timed = None;
        self.stats.timeouts += 1;
        self.rto = (self.rto * 2).min(self.cfg.max_rto_us);
        // 回到最早没确认的包重发
        self.nxt = self.una;
        self.rto_at = Some(now.plus(self.rto));
        self.fill(now, out);
    }
}

/// 接收端：累计确认，乱序的包先存着。
#[derive(Debug, Clone, Default)]
pub struct TcpReceiver {
    next: u64,
    ahead: BTreeSet<u64>,
}

impl TcpReceiver {
    /// 新接收端。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 已经按顺序收齐的包数。
    #[must_use]
    pub const fn delivered(&self) -> u64 {
        self.next
    }

    /// 收到第 `seq` 个包，返回要回的确认号（下一个想要的包）。每个包都回，重复的、乱序的也回（重复确认）。
    pub fn on_segment(&mut self, seq: u64) -> u64 {
        if seq == self.next {
            self.next += 1;
            while self.ahead.remove(&self.next) {
                self.next += 1;
            }
        } else if seq > self.next {
            self.ahead.insert(seq);
        }
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    enum Ev {
        Data(u64),
        Ack(u64),
    }

    /// 一个只有固定单程延迟、不限带宽的网络；`lose(seq, 第几次发这个包)` 决定丢不丢数据包。
    /// 返回（发送端，接收端，完成时刻）。
    fn run(
        cfg: TcpConfig,
        total: u64,
        one_way_us: u64,
        mut lose: impl FnMut(u64, u32) -> bool,
    ) -> (TcpSender, TcpReceiver, Time) {
        let mut snd = TcpSender::new(cfg, Some(total));
        let mut rcv = TcpReceiver::new();
        let mut events: BTreeMap<(Time, u64), Ev> = BTreeMap::new();
        let mut id = 0u64;
        let mut tx_count: BTreeMap<u64, u32> = BTreeMap::new();
        let mut now = Time::ZERO;
        let mut out = Vec::new();
        snd.start(now, &mut out);
        loop {
            for seq in out.drain(..) {
                let n = tx_count.entry(seq).or_insert(0);
                *n += 1;
                if !lose(seq, *n) {
                    events.insert((now.plus(one_way_us), id), Ev::Data(seq));
                    id += 1;
                }
            }
            if snd.is_done() {
                return (snd, rcv, now);
            }
            let next_ev = events.keys().next().copied();
            let timer = snd.next_timer();
            let take_event = next_ev.is_some_and(|(t, _)| timer.is_none_or(|x| t <= x));
            match (next_ev, timer) {
                (Some(_), _) if take_event => {
                    let ((t, _), ev) = events.pop_first().unwrap();
                    now = t;
                    match ev {
                        Ev::Data(seq) => {
                            let ack = rcv.on_segment(seq);
                            events.insert((now.plus(one_way_us), id), Ev::Ack(ack));
                            id += 1;
                        }
                        Ev::Ack(a) => snd.on_ack(now, a, &mut out),
                    }
                }
                (_, tm) => {
                    now = tm.expect("卡住了：没有事件也没有定时器");
                    snd.on_timeout(now, &mut out);
                }
            }
            assert!(now.micros() < 3_600_000_000, "跑了一个小时还没完");
        }
    }

    fn cfg() -> TcpConfig {
        TcpConfig {
            min_rto_us: 200_000,
            initial_rto_us: 200_000,
            ..TcpConfig::default()
        }
    }

    #[test]
    fn a_lossless_transfer_completes_in_slow_start() {
        let (snd, rcv, done) = run(cfg(), 200, 10_000, |_, _| false);
        assert_eq!(rcv.delivered(), 200);
        let st = snd.stats();
        assert_eq!(
            (st.retransmits, st.fast_retransmits, st.timeouts),
            (0, 0, 0)
        );
        assert_eq!(st.sent, 200);
        // 往返 20 ms：srtt 就是 20 ms；RTO = max(下限 200 ms, srtt + max(G, 4 rttvar))
        assert_eq!(snd.srtt(), Some(20_000));
        assert_eq!(snd.rto(), 200_000);
        // 慢启动：窗口每个确认涨一个 MSS，ssthresh 还是无穷
        assert!(snd.cwnd() > 100 * 1250 && snd.ssthresh() == u64::MAX);
        assert!(!snd.in_recovery());
        // 初始窗口 10 个包，每个往返翻倍：200 个包 5 个往返左右
        assert!(done.micros() < 8 * 20_000, "{done:?}");
        assert!(snd.next_timer().is_none());
    }

    /// 单个丢包：三个重复确认触发快速重传，进快速恢复，完整确认后退出；没有超时；窗口减半。
    #[test]
    fn one_loss_is_repaired_by_fast_retransmit_without_a_timeout() {
        let (snd, rcv, _) = run(cfg(), 300, 10_000, |seq, n| seq == 40 && n == 1);
        assert_eq!(rcv.delivered(), 300);
        let st = snd.stats();
        assert_eq!(
            (st.fast_retransmits, st.timeouts, st.retransmits),
            (1, 0, 1)
        );
        assert!(snd.ssthresh() < u64::MAX);
        assert!(!snd.in_recovery());
    }

    /// 一个窗口里丢两个包：NewReno 的部分确认，仍然只进一次快速恢复，每个丢的包重传一次，没有超时。
    #[test]
    fn two_losses_in_one_window_are_repaired_by_partial_acks() {
        let (snd, rcv, _) = run(cfg(), 300, 10_000, |seq, n| {
            (seq == 40 || seq == 43) && n == 1
        });
        assert_eq!(rcv.delivered(), 300);
        let st = snd.stats();
        assert_eq!(
            (st.fast_retransmits, st.timeouts, st.retransmits),
            (1, 0, 2)
        );
    }

    /// 尾部丢包没有后续的包产生重复确认：只能等超时，超时后窗口回到一个 MSS。
    #[test]
    fn a_tail_loss_needs_a_timeout() {
        let (snd, rcv, done) = run(cfg(), 100, 10_000, |seq, n| seq == 99 && n == 1);
        assert_eq!(rcv.delivered(), 100);
        let st = snd.stats();
        assert_eq!((st.timeouts, st.fast_retransmits), (1, 0));
        assert!(
            done.micros() >= 200_000,
            "要等 RTO（下限 200 ms）：{done:?}"
        );
    }

    /// 连续超时：RTO 翻倍（指数退避），丢包停止之后第一个有样本的确认把 RTO 拉回正常。
    #[test]
    fn repeated_timeouts_back_off_exponentially() {
        let mut s = TcpSender::new(cfg(), Some(10));
        let mut out = Vec::new();
        s.start(Time(0), &mut out);
        assert_eq!(out.len(), 10);
        let mut t = Time(0);
        for k in 0..3u32 {
            t = s.next_timer().unwrap();
            s.on_timeout(t, &mut out);
            assert_eq!(s.rto(), 200_000 << (k + 1));
            assert_eq!(s.cwnd(), 1250, "超时后窗口回到一个 MSS");
        }
        assert_eq!(s.stats().timeouts, 3);
        // 超时之后重发的包是重传，不取样（Karn）：确认回来也不更新 SRTT
        s.on_ack(t.plus(10_000), 1, &mut out);
        assert_eq!(s.srtt(), None);
        // 上限
        let small = TcpConfig {
            max_rto_us: 300_000,
            min_rto_us: 200_000,
            ..TcpConfig::default()
        };
        let mut s = TcpSender::new(small, None);
        s.start(Time(0), &mut out);
        s.on_timeout(s.next_timer().unwrap(), &mut out);
        s.on_timeout(s.next_timer().unwrap(), &mut out);
        assert_eq!(s.rto(), 300_000);
        // 没有在途数据时超时什么也不做
        let mut idle = TcpSender::new(cfg(), Some(0));
        idle.on_timeout(Time(5), &mut out);
        assert!(idle.next_timer().is_none() && idle.stats().timeouts == 0);
    }

    /// RFC 6298 第 2 节：第一个样本 SRTT = R、RTTVAR = R/2；之后 RTTVAR = (3 RTTVAR + |SRTT − R|)/4、SRTT = (7 SRTT + R)/8；
    /// RTO = SRTT + max(G, 4 RTTVAR)，夹在下限和上限之间。
    #[test]
    fn rtt_estimation_follows_rfc_6298() {
        let c = TcpConfig {
            min_rto_us: 1_000,
            ..TcpConfig::default()
        };
        let mut s = TcpSender::new(c, None);
        s.sample_rtt(100_000);
        assert_eq!(
            (s.srtt(), s.rttvar, s.rto()),
            (Some(100_000), 50_000, 300_000)
        );
        s.sample_rtt(60_000);
        // rttvar = (150000 + 40000)/4 = 47500；srtt = (700000 + 60000)/8 = 95000；rto = 95000 + 190000
        assert_eq!(
            (s.srtt(), s.rttvar, s.rto()),
            (Some(95_000), 47_500, 285_000)
        );
        // 样本稳定时 4×RTTVAR 比 G 小，RTO 贴着 SRTT + G
        let mut s = TcpSender::new(c, None);
        for _ in 0..200 {
            s.sample_rtt(10_000);
        }
        assert!(s.rto() <= 10_000 + 1_000 + 10, "{}", s.rto());
    }

    #[test]
    fn the_receiver_acks_every_segment_cumulatively() {
        let mut r = TcpReceiver::new();
        assert_eq!(r.on_segment(0), 1);
        assert_eq!(r.on_segment(2), 1, "缺 1：重复确认");
        assert_eq!(r.on_segment(3), 1);
        assert_eq!(r.on_segment(0), 1, "重复的旧包");
        assert_eq!(r.on_segment(1), 4, "补上缺口，一下确认到 4");
        assert_eq!(r.delivered(), 4);
    }

    #[test]
    fn dupacks_without_data_in_flight_are_ignored_and_the_fast_retransmit_guard_holds() {
        let mut s = TcpSender::new(cfg(), Some(5));
        let mut out = Vec::new();
        s.start(Time(0), &mut out);
        s.on_ack(Time(1), 5, &mut out);
        assert!(s.is_done());
        s.on_ack(Time(2), 5, &mut out); // 没有在途数据的重复确认
        assert_eq!(s.stats().fast_retransmits, 0);
        // 超时之后 recover 限制：una 没越过 recover 之前三个重复确认不触发快速重传
        let mut s = TcpSender::new(cfg(), None);
        s.start(Time(0), &mut out);
        s.on_timeout(s.next_timer().unwrap(), &mut out);
        for _ in 0..3 {
            s.on_ack(Time(300_000), 0, &mut out);
        }
        assert_eq!(s.stats().fast_retransmits, 0);
    }
}
