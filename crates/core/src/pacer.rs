//! 发送节奏（pacing）：大帧的分片按估计带宽的 [`PACE_FACTOR_PERMILLE`]（千分比）倍匀速发出，而不是一次全部发出。
//!
//! 为什么：瓶颈的缓冲区很小（400 kbps、排队 200 ms 时只有约 10 KB），一个 96 KB 的关键帧整帧突发，
//! 八成以上在瓶颈被丢；QUIC 不重传数据报，靠我们自己的重传又受码率限制，结果画面一直出不来
//! （真实二进制在 400 kbps 上实测 10 秒 0 帧）。设计见 `docs/research/README.md` 第 3.3 节。
//!
//! 为了不破坏带宽估计的“包串测速”（一帧的包背靠背发出、按到达间隔测瓶颈），
//! 允许一小段突发：[`BURST_US`] 的量，至少 [`MIN_BURST_PACKETS`] 个包。
//! 正常带宽上一帧在预算之内，几乎都在突发额度里直接发出，行为和原来一样。
//! 重传的分片插到队首：接收端正在等它们。
//!
//! **状态：默认关**（`PEERDESK_PACE=1` 打开，打开时按估计带宽的 2.5 倍发，和 WebRTC 的 pacing factor 一样）。
//! 2026-11-24 校准，两边的证据不一致，所以不默认打开：
//! - 真实二进制、tbf 限速的回环（`scripts/pace-lab.sh`，没有随机丢包和抖动）：2.5 倍时 400 kbps 最长停顿
//!   3.8 → 2.0 秒，5 Mbps 441 → 225 ms，1.5 Mbps（缓冲 60 ms）700 → 439 ms。
//! - 仿真 10 个场景全过，瓶颈排队丢包几乎都降到 0。
//! - 微软真实网络轨迹（带宽变化、随机丢包；冻结次数 / 最长等待 ms / 首帧 ms）：
//!   | 轨迹 | 关 | 2.5 倍 | 4 倍 |
//!   |---|---|---|---|
//!   | ms-09401 | 4 / 1519 / 526 | 7 / 1784 / 461 | 4 / 1310 / 471 |
//!   | ms-02560 | 19 / 322 / 66 | 18 / 409 / 186 | 23 / 339 / 110 |
//!   | ms-05729 | 13 / 1531 / 558 | 15 / 2020 / 493 | 15 / 1366 / 515 |
//!   每种倍数都有一项变差（超过回归门槛）。
//! - 下一步：仿真的瓶颈模型按 tbf（字节桶）校准，再在两边一起选默认值；或只对超过一帧预算的大帧（关键帧）限速。

/// 发送节奏是否打开：默认关，`PEERDESK_PACE=1` 打开（见模块说明里的取舍）。
#[must_use]
pub fn enabled() -> bool {
    enabled_from(std::env::var_os("PEERDESK_PACE").as_deref())
}

/// 按环境变量的值判断：只有 `1` 表示开。
#[must_use]
pub fn enabled_from(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|v| v == "1")
}

/// 交给 [`Pacer`] 的速率：打开时是估计带宽，关闭时不限速。
#[must_use]
pub fn rate_for(enabled: bool, rate_bps: u64) -> u64 {
    if enabled {
        rate_bps
    } else {
        u64::MAX
    }
}

use std::collections::VecDeque;
use std::sync::OnceLock;

use crate::media::Outgoing;
use crate::time::Time;

/// 发送速率是估计带宽的多少倍（千分比）。2.5 倍（WebRTC 同值）：估计偏低时很快探上去，大帧又不会整帧突发挤爆瓶颈缓冲区。
pub const PACE_FACTOR_PERMILLE: u64 = 2500;
/// 实际用的倍数：默认 [`PACE_FACTOR_PERMILLE`]；校准时（`scripts/pace-lab.sh`）用 `PEERDESK_PACE_FACTOR`
/// （千分比，1000–5000）改。
#[must_use]
pub fn factor_permille() -> u64 {
    static FACTOR: OnceLock<u64> = OnceLock::new();
    *FACTOR.get_or_init(|| factor_from(std::env::var_os("PEERDESK_PACE_FACTOR").as_deref()))
}

/// 从环境变量的值得到倍数；没有或不合理时用默认值。
#[must_use]
pub fn factor_from(value: Option<&std::ffi::OsStr>) -> u64 {
    value
        .and_then(|v| v.to_str()?.trim().parse::<u64>().ok())
        .filter(|f| (1000..=5000).contains(f))
        .unwrap_or(PACE_FACTOR_PERMILLE)
}

/// 允许的突发：这么长时间的发送量。
pub const BURST_US: u64 = 10_000;
/// 突发至少允许这么多个包（包串测速需要几个背靠背的包）。
pub const MIN_BURST_PACKETS: u64 = 4;
/// 一个包按这么大估计突发额度的下限。
const PACKET_BYTES: u64 = 1200;

/// 视频分片的发送队列加令牌桶。
#[derive(Debug, Clone)]
pub struct Pacer {
    queue: VecDeque<Outgoing>,
    /// 当前可发的字节数（可以为负：一个包超出额度时先发出，欠的下次补）。
    tokens: i64,
    last: Time,
}

impl Pacer {
    /// 空队列，额度满（第一次取用时按突发上限截断）。
    #[must_use]
    pub const fn new(now: Time) -> Self {
        Self {
            queue: VecDeque::new(),
            tokens: i64::MAX,
            last: now,
        }
    }

    /// 一帧的分片排到队尾。
    pub fn push(&mut self, out: Vec<Outgoing>) {
        self.queue.extend(out);
    }

    /// 重传的分片插到队首（按原顺序）。
    pub fn push_front(&mut self, out: Vec<Outgoing>) {
        for o in out.into_iter().rev() {
            self.queue.push_front(o);
        }
    }

    /// 队列空了。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// 队列里的东西按 `rate_bps` 发完要超过 `interval_us`：这时不该再编新的一帧（发送队列最多放约一帧），
    /// 变化攒着，等发得差不多再编最新的画面。只剩一点尾巴时照常编，免得停下后补清晰被推迟。
    #[must_use]
    pub fn busy(&self, rate_bps: u64, interval_us: u64) -> bool {
        let bits = u64::try_from(self.queued_bytes())
            .unwrap_or(u64::MAX)
            .saturating_mul(8);
        let drain_us = bits.saturating_mul(1_000_000) / Self::pace_bps(rate_bps).max(1);
        drain_us > interval_us
    }

    /// 排队的字节数。
    #[must_use]
    pub fn queued_bytes(&self) -> usize {
        self.queue.iter().map(|o| o.body.len()).sum()
    }

    fn pace_bps(rate_bps: u64) -> u64 {
        rate_bps.saturating_mul(factor_permille()) / 1000
    }

    fn burst_bytes(rate_bps: u64) -> i64 {
        let by_time = Self::pace_bps(rate_bps).saturating_mul(BURST_US) / 8_000_000;
        i64::try_from(by_time.max(MIN_BURST_PACKETS * PACKET_BYTES)).unwrap_or(i64::MAX)
    }

    fn refill(&mut self, now: Time, rate_bps: u64) {
        let cap = Self::burst_bytes(rate_bps);
        let earned = Self::pace_bps(rate_bps).saturating_mul(now.since(self.last)) / 8_000_000;
        self.tokens = self
            .tokens
            .saturating_add(i64::try_from(earned).unwrap_or(i64::MAX))
            .min(cap);
        self.last = now;
    }

    /// 取出现在可以发的分片。
    pub fn pop_ready(&mut self, now: Time, rate_bps: u64) -> Vec<Outgoing> {
        self.refill(now, rate_bps);
        let mut out = Vec::new();
        while self.tokens > 0 {
            let Some(o) = self.queue.pop_front() else {
                break;
            };
            self.tokens = self
                .tokens
                .saturating_sub(i64::try_from(o.body.len()).unwrap_or(i64::MAX));
            out.push(o);
        }
        out
    }

    /// 下一个分片什么时候能发；队列空时为 `None`。
    #[must_use]
    pub fn next_due(&self, now: Time, rate_bps: u64) -> Option<Time> {
        if self.queue.is_empty() {
            return None;
        }
        if self.tokens > 0 {
            return Some(now);
        }
        let deficit =
            u64::try_from(self.tokens.saturating_neg().saturating_add(1)).unwrap_or(u64::MAX);
        let bps = Self::pace_bps(rate_bps).max(1);
        let wait = deficit.saturating_mul(8_000_000).div_ceil(bps);
        Some(self.last.plus(wait.max(1)))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn factor_from_environment() {
        use std::ffi::OsStr;
        assert_eq!(factor_from(None), PACE_FACTOR_PERMILLE);
        assert_eq!(factor_from(Some(OsStr::new("2500"))), 2500);
        assert_eq!(factor_from(Some(OsStr::new("9000"))), PACE_FACTOR_PERMILLE);
        assert_eq!(factor_from(Some(OsStr::new("快"))), PACE_FACTOR_PERMILLE);
        assert!(factor_permille() >= 1000);
        assert!(!enabled_from(None));
        assert!(enabled_from(Some(OsStr::new("1"))));
        assert!(!enabled_from(Some(OsStr::new("0"))));
    }
    use super::*;

    fn pkt(n: usize) -> Vec<Outgoing> {
        (0..n)
            .map(|i| Outgoing {
                flags: u8::try_from(i % 200).unwrap(),
                body: vec![0; 1000],
            })
            .collect()
    }

    #[test]
    fn small_frames_go_out_at_once() {
        // 20 Mbps：突发额度 31 KB，一帧 10 个包立刻全部发出
        let mut p = Pacer::new(Time(0));
        p.push(pkt(10));
        assert_eq!(p.pop_ready(Time(0), 20_000_000).len(), 10);
        assert!(p.is_empty() && p.next_due(Time(0), 20_000_000).is_none());
    }

    #[test]
    fn large_frames_are_spread_at_the_pace_rate() {
        // 400 kbps：发送速率 1 Mbps（2.5 倍），突发额度 4 个包；96 个 1000 字节的包约 0.77 秒发完
        let rate = 400_000;
        // 起步额度也只是一个突发的量，不会一下全发
        let mut p = Pacer::new(Time(0));
        p.push(pkt(96));
        let mut now = Time(0);
        let mut sent = p.pop_ready(now, rate).len();
        assert!(sent <= 5, "突发 {sent}");
        while let Some(due) = p.next_due(now, rate) {
            assert!(due > now || p.next_due(now, rate) == Some(now));
            now = due;
            sent += p.pop_ready(now, rate).len();
        }
        assert_eq!(sent, 96);
        let secs = now.micros() as f64 / 1e6;
        assert!((0.7..0.85).contains(&secs), "用了 {secs} 秒");
        assert_eq!(p.queued_bytes(), 0);
    }

    #[test]
    fn disabled_means_unlimited() {
        let mut p = Pacer::new(Time(0));
        p.push(pkt(500));
        assert_eq!(p.pop_ready(Time(0), rate_for(false, 400_000)).len(), 500);
        assert_eq!(rate_for(true, 400_000), 400_000);
        let _ = enabled();
    }

    #[test]
    fn retransmissions_jump_the_queue() {
        let mut p = Pacer::new(Time(0));
        let _ = p.pop_ready(Time(0), 1_000_000);
        p.push(pkt(3));
        let mut again = pkt(1);
        again[0].flags = 250;
        p.push_front(again);
        assert_eq!(p.queued_bytes(), 4000);
        // 有额度时下一个包马上可以发
        let mut fresh = Pacer::new(Time(0));
        fresh.push(pkt(1));
        let _ = fresh.pop_ready(Time(0), 1); // 1 bps：发一个包就欠很多
        fresh.push(pkt(1));
        fresh.tokens = 1;
        assert_eq!(fresh.next_due(Time(5), 1), Some(Time(5)));
        // 1 Mbps（发送 2.5 Mbps）发 4000 字节要 12.8 ms
        assert!(p.busy(1_000_000, 10_000) && !p.busy(1_000_000, 15_000));
        let now = Time(1_000_000);
        let out = p.pop_ready(now, 1_000_000);
        assert_eq!(out.first().map(|o| o.flags), Some(250));
    }
}
