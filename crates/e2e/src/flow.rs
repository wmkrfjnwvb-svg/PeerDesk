//! 每一对的流量记录（A2-4c，RFC 8868 的指标要用）：按 100 ms 一格记被控端发出的和控制端收到的画面字节、包数，
//! 控制端发出的反馈字节，每个画面包的单程延迟，以及帧末采到的真实容量和排队。
//!
//! 时间都是这一对的本地时间（这一对开始时为 0）。只记数，不影响会话，不进结果的指纹。
//! 200 ms、500 ms 的窗口由 100 ms 的格子合出来（RFC 8868 的发送和接收速率用 200 ms，振荡用 500 ms）。

use peerdesk_core::time::Time;

/// 一格多长（微秒）。
pub const BIN_US: u64 = 100_000;

/// 单程延迟的直方图上限（毫秒）：更大的记在最后一格。
pub const DELAY_HIST_MS: usize = 3_000;

/// 一对的流量记录。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlowLog {
    /// 每格被控端发出的画面数据报字节（含数据报头、重传和探测）。
    pub sent_bytes: Vec<u64>,
    /// 每格被控端发出的包数。
    pub sent_packets: Vec<u64>,
    /// 每格控制端收到的画面数据报字节。
    pub recv_bytes: Vec<u64>,
    /// 每格控制端收到的包数。
    pub recv_packets: Vec<u64>,
    /// 控制端发出的反馈字节（合计）。
    pub feedback_bytes: u64,
    /// 画面包的单程延迟直方图：第 i 格是 [i, i+1) 毫秒，最后一格是不小于 `DELAY_HIST_MS - 1` 毫秒。
    pub delay_hist_ms: Vec<u64>,
    /// 每格帧末采到的正向（画面方向）名义容量（比特每秒，取这一格里最后一次采样；没有采样为 0）。
    pub capacity_bps: Vec<u64>,
    /// 每格帧末采到的这一对画面方向的真实排队（微秒，取这一格里的最大）。
    pub queue_us: Vec<u64>,
    /// 每帧到齐要多久（控制端收到第一个分片到收齐，毫秒）的直方图，格子同 `delay_hist_ms`。
    /// 窄链路上一帧在瓶颈上摊开（4G 的虚假重传、5.1 物理 1 Mbps 的低利用率都是这个机制），B 阶段改帧大小和重传时机时直接比它。
    pub assembly_hist_ms: Vec<u64>,
}

/// 最多记多少格（10 小时）：再往后的不记，免得一个离谱的时刻申请天量内存。
const MAX_BINS: u64 = 360_000;

/// 格号；超出 [`MAX_BINS`] 时为 `usize::MAX`（不记）。
fn bin(t: Time) -> usize {
    let i = t.micros() / BIN_US;
    if i >= MAX_BINS {
        return usize::MAX;
    }
    usize::try_from(i).unwrap_or(usize::MAX)
}

fn add(v: &mut Vec<u64>, i: usize, x: u64) {
    if i == usize::MAX {
        return;
    }
    if v.len() <= i {
        v.resize(i + 1, 0);
    }
    if let Some(slot) = v.get_mut(i) {
        *slot = slot.saturating_add(x);
    }
}

fn set_max(v: &mut Vec<u64>, i: usize, x: u64) {
    if i == usize::MAX {
        return;
    }
    if v.len() <= i {
        v.resize(i + 1, 0);
    }
    if let Some(slot) = v.get_mut(i) {
        *slot = (*slot).max(x);
    }
}

impl FlowLog {
    /// 被控端在 `at` 发出了 `packets` 个包、共 `bytes` 字节。
    pub fn sent(&mut self, at: Time, packets: u64, bytes: u64) {
        let i = bin(at);
        add(&mut self.sent_bytes, i, bytes);
        add(&mut self.sent_packets, i, packets);
    }

    /// 控制端在 `at` 收到一个 `bytes` 字节、单程延迟 `delay_us` 的画面包。
    pub fn received(&mut self, at: Time, bytes: u64, delay_us: u64) {
        let i = bin(at);
        add(&mut self.recv_bytes, i, bytes);
        add(&mut self.recv_packets, i, 1);
        if self.delay_hist_ms.is_empty() {
            self.delay_hist_ms.resize(DELAY_HIST_MS, 0);
        }
        let ms = usize::try_from(delay_us / 1_000)
            .unwrap_or(usize::MAX)
            .min(DELAY_HIST_MS - 1);
        add(&mut self.delay_hist_ms, ms, 1);
    }

    /// 控制端收齐了一帧，从第一个分片到收齐用了 `us` 微秒。
    pub fn assembled(&mut self, us: u64) {
        if self.assembly_hist_ms.is_empty() {
            self.assembly_hist_ms.resize(DELAY_HIST_MS, 0);
        }
        let ms = usize::try_from(us / 1_000)
            .unwrap_or(usize::MAX)
            .min(DELAY_HIST_MS - 1);
        add(&mut self.assembly_hist_ms, ms, 1);
    }

    /// 控制端发出一份 `bytes` 字节的反馈。
    pub fn feedback(&mut self, bytes: u64) {
        self.feedback_bytes = self.feedback_bytes.saturating_add(bytes);
    }

    /// 帧末采样：这时的名义容量和真实排队。
    pub fn sample(&mut self, at: Time, capacity_bps: u64, queue_us: u64) {
        let i = bin(at);
        if i == usize::MAX {
            return;
        }
        if self.capacity_bps.len() <= i {
            self.capacity_bps.resize(i + 1, 0);
        }
        if let Some(slot) = self.capacity_bps.get_mut(i) {
            *slot = capacity_bps;
        }
        set_max(&mut self.queue_us, i, queue_us);
    }

    /// 把 100 ms 的格子按 `k` 格一窗合起来（例如 k = 2 是 200 ms）。
    #[must_use]
    pub fn windows(v: &[u64], k: usize) -> Vec<u64> {
        v.chunks(k.max(1)).map(|c| c.iter().sum()).collect()
    }

    /// 单程延迟的分位（毫秒）：`pct` 是百分数；没有包时为 `None`。
    #[must_use]
    pub fn delay_percentile_ms(&self, pct: u64) -> Option<u64> {
        percentile(&self.delay_hist_ms, pct)
    }

    /// 每帧到齐要多久的分位（毫秒）；没有收齐的帧时为 `None`。
    #[must_use]
    pub fn assembly_percentile_ms(&self, pct: u64) -> Option<u64> {
        percentile(&self.assembly_hist_ms, pct)
    }

    /// 发出、收到的包数合计。
    #[must_use]
    pub fn packets(&self) -> (u64, u64) {
        (
            self.sent_packets.iter().sum(),
            self.recv_packets.iter().sum(),
        )
    }
}

/// 直方图（第 i 格是 [i, i+1) 毫秒）的分位：最小的 i，使得不超过 i 的个数不少于总数 × pct / 100。
fn percentile(hist: &[u64], pct: u64) -> Option<u64> {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return None;
    }
    // 最小的 i，使得不超过 i 毫秒的包数不少于 total × pct / 100
    let want = (total * pct).div_ceil(100).max(1);
    let mut seen = 0u64;
    for (i, n) in hist.iter().enumerate() {
        seen += n;
        if seen >= want {
            return u64::try_from(i).ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn bins_windows_and_percentiles() {
        let mut f = FlowLog::default();
        f.sent(Time(0), 2, 2_400);
        f.sent(Time(150_000), 1, 1_200);
        f.received(Time(30_000), 1_200, 30_000);
        f.received(Time(250_000), 1_200, 50_500);
        f.received(Time(260_000), 1_200, 10_000_000);
        f.feedback(80);
        f.assembled(3_500);
        f.assembled(25_000);
        assert_eq!(f.assembly_percentile_ms(50), Some(3));
        assert_eq!(f.assembly_percentile_ms(100), Some(25));
        assert_eq!(FlowLog::default().assembly_percentile_ms(50), None);
        f.sample(Time(10_000), 5_000_000, 3_000);
        f.sample(Time(90_000), 4_000_000, 1_000);
        assert_eq!(f.sent_bytes, vec![2_400, 1_200]);
        assert_eq!(f.sent_packets, vec![2, 1]);
        assert_eq!(f.recv_bytes, vec![1_200, 0, 2_400]);
        assert_eq!(f.packets(), (3, 3));
        assert_eq!(f.feedback_bytes, 80);
        assert_eq!(f.capacity_bps, vec![4_000_000], "取最后一次");
        assert_eq!(f.queue_us, vec![3_000], "取最大");
        assert_eq!(FlowLog::windows(&f.recv_bytes, 2), vec![1_200, 2_400]);
        assert_eq!(FlowLog::windows(&[1, 2, 3], 0), vec![1, 2, 3]);
        assert_eq!(f.delay_percentile_ms(1), Some(30));
        assert_eq!(f.delay_percentile_ms(50), Some(50));
        assert_eq!(f.delay_percentile_ms(100), Some(2_999), "大的记在最后一格");
        assert_eq!(FlowLog::default().delay_percentile_ms(50), None);
        // 时刻太大（格号溢出）时不记、不出错
        let mut g = FlowLog::default();
        g.sent(Time(u64::MAX), 1, 1);
        g.received(Time(u64::MAX), 1, 1);
        g.sample(Time(u64::MAX), 1, 1);
        assert!(g.sent_bytes.is_empty() && g.recv_bytes.is_empty() && g.capacity_bps.is_empty());
        assert_eq!(g.delay_hist_ms.iter().sum::<u64>(), 1, "延迟照记");
    }
}
