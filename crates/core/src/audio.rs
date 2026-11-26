//! 声音的数据报格式和接收端抖动缓冲（设计见 `docs/research/H-audio.md` 第 7 节）。
//!
//! 每 10 ms 一帧 Opus。数据报里是当前帧，后面可以带前 1–2 帧的副本（RED 式冗余，丢包时补上）。
//!
//! 抖动缓冲按 WebRTC NetEQ 的思路：
//! - 每个包的“相对到达延迟” = 实际到达时刻 − 按采集时刻推算的理想到达时刻，参照最近 2 秒里最快的包；
//! - 目标缓冲延迟取最近相对延迟的高分位（默认 95%），夹在 [`MIN_TARGET_MS`] 到 [`MAX_TARGET_MS`] 之间；
//! - 每 10 ms 取一帧播放：有包就解码（Normal）；缺包就补偿（Expand，Opus 的 PLC）；
//!   缓冲比目标多出一截就丢一帧追上（Accelerate）；比目标少一截就多播一帧补偿、时间线不前进（减速）；
//!   缓冲见底就先不取（等水位回到目标）。
//!
//! 第一版加速、减速都按整帧做；时间伸缩（WSOLA，听不出来的加速/减速）在下一步做。
//!
//! 音画同步打开时（看视频用，见 [`crate::avsync`]）播放时刻不由水位定，而由采集时刻定：
//! [`JitterBuffer::pop_synced`] 按“下一帧离它的播放时刻还有多久”决定正常播、减速还是加速，
//! 这样声音和画面走同一条时间线。关着时走 [`JitterBuffer::pop`]，行为不变。

use std::collections::{BTreeMap, VecDeque};

use crate::wire::{Reader, Writer};
use crate::Error;

/// 一帧的时长（毫秒）。
pub const FRAME_MS: u64 = 10;
/// 目标延迟下限（毫秒）。
pub const MIN_TARGET_MS: u64 = 20;
/// 目标延迟上限（毫秒）：再大就是网络出了问题，应当提示而不是继续堆延迟。
pub const MAX_TARGET_MS: u64 = 200;
/// 起步的目标延迟（毫秒）。
pub const START_TARGET_MS: u64 = 40;
/// 相对延迟的统计窗口（微秒）。
const WINDOW_US: u64 = 2_000_000;
/// 取多高的分位当目标（百分比）。
const QUANTILE_PERCENT: usize = 95;
/// 冗余副本最多几个。
pub const MAX_REDUNDANCY: usize = 2;
/// 音画同步时声音离时间线多远才调整（微秒）：一帧。调整一次挪一帧，所以稳定后偏差不超过一帧
/// （远小于能察觉的音画差：声音早 45 ms、晚 125 ms，ITU-R BT.1359）。
pub const SYNC_BAND_US: i64 = 10_000;
/// 音画同步时离时间线超过这么远（微秒）就不一帧一帧挪，直接等或者跳过去（刚打开同步、播放延迟改了）。
pub const SYNC_JUMP_US: i64 = 60_000;

/// 一个声音数据报。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioPacket {
    /// 当前帧的序号（每 10 ms 加 1）。
    pub seq: u32,
    /// 当前帧的采集时刻（被控端时钟，微秒）。
    pub capture_us: u64,
    /// 当前帧的 Opus 数据。
    pub frame: Vec<u8>,
    /// 前几帧的副本：`redundant[0]` 是 `seq - 1`，`redundant[1]` 是 `seq - 2`。
    pub redundant: Vec<Vec<u8>>,
}

impl AudioPacket {
    /// 编码。
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let n = u8::try_from(self.redundant.len())
            .ok()
            .filter(|n| usize::from(*n) <= MAX_REDUNDANCY)
            .ok_or(Error::TooManyItems {
                len: self.redundant.len(),
                max: MAX_REDUNDANCY,
            })?;
        let mut w = Writer::new();
        w.u32(self.seq).u64(self.capture_us).u8(n);
        for r in &self.redundant {
            let len = u16::try_from(r.len()).map_err(|_| Error::PayloadTooLarge {
                len: r.len(),
                max: usize::from(u16::MAX),
            })?;
            w.u16(len).bytes(r);
        }
        w.bytes(&self.frame);
        Ok(w.into_vec())
    }

    /// 解码。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let seq = r.u32()?;
        let capture_us = r.u64()?;
        let n = usize::from(r.u8()?);
        if n > MAX_REDUNDANCY {
            return Err(Error::TooManyItems {
                len: n,
                max: MAX_REDUNDANCY,
            });
        }
        let mut redundant = Vec::with_capacity(n);
        for _ in 0..n {
            let len = usize::from(r.u16()?);
            redundant.push(r.take(len)?.to_vec());
        }
        Ok(Self {
            seq,
            capture_us,
            frame: r.rest().to_vec(),
            redundant,
        })
    }
}

/// 播放端每 10 ms 该做什么。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Play {
    /// 解码这一帧。
    Normal(Vec<u8>),
    /// 这一帧没到：用补偿生成一帧（Opus PLC）。
    Expand,
    /// 缓冲太多：把这两帧解码后压成一帧（交叉淡化，听不出接缝）。
    Accelerate(Vec<u8>, Vec<u8>),
    /// 缓冲还没攒够（刚开始，或者见底后在等水位回到目标）：播放静音。
    Wait,
}

/// 统计（沿用 W3C WebRTC 统计的名字）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AudioStats {
    /// 正常解码的帧数。
    pub normal: u64,
    /// 补偿出来的帧数（丢包或迟到）。
    pub concealed: u64,
    /// 为了追上目标丢掉的帧数（加速）。
    pub accelerated: u64,
    /// 为了把缓冲攒到目标而多播的补偿帧数（减速）。
    pub decelerated: u64,
    /// 迟到（已经过了播放时刻）而丢弃的包数。
    pub late: u64,
    /// 缓冲见底、播放静音等待的次数。
    pub underruns: u64,
    /// 靠冗余副本补回来的帧数。
    pub recovered: u64,
}

/// 接收端抖动缓冲。
#[derive(Debug, Clone)]
pub struct JitterBuffer {
    frames: BTreeMap<u32, Vec<u8>>,
    /// 下一帧要播放的序号；还没开始时为 `None`。
    next: Option<u32>,
    /// 最近的（到达时刻，相对到达延迟的原始量 = 到达 − 采集）。
    arrivals: VecDeque<(u64, i64)>,
    target_ms: u64,
    /// 见底后等待水位回到目标。
    refilling: bool,
    /// 上一帧是不是减速插入的：最多隔一帧插一次，保证时间线一直在前进。
    decelerated_last: bool,
    /// 最近一个包的（序号，采集时刻）：据此推算任意一帧的采集时刻（每帧 [`FRAME_MS`]）。
    anchor: Option<(u32, u64)>,
    stats: AudioStats,
}

impl Default for JitterBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl JitterBuffer {
    /// 创建。
    #[must_use]
    pub const fn new() -> Self {
        Self {
            frames: BTreeMap::new(),
            next: None,
            arrivals: VecDeque::new(),
            target_ms: START_TARGET_MS,
            refilling: true,
            decelerated_last: false,
            anchor: None,
            stats: AudioStats {
                normal: 0,
                concealed: 0,
                accelerated: 0,
                decelerated: 0,
                late: 0,
                underruns: 0,
                recovered: 0,
            },
        }
    }

    /// 当前目标延迟（毫秒）。
    #[must_use]
    pub const fn target_ms(&self) -> u64 {
        self.target_ms
    }

    /// 统计。
    #[must_use]
    pub const fn stats(&self) -> AudioStats {
        self.stats
    }

    /// 缓冲里从下一帧起已有的连续时长（毫秒）。
    #[must_use]
    pub fn level_ms(&self) -> u64 {
        let Some(next) = self.next else {
            return u64::try_from(self.frames.len()).unwrap_or(u64::MAX) * FRAME_MS;
        };
        let newest = self.frames.keys().next_back().copied().unwrap_or(next);
        u64::from(newest.wrapping_sub(next).wrapping_add(1).min(1000))
            * FRAME_MS
            * u64::from(!self.frames.is_empty())
    }

    /// 收到一个数据报（`now_us` 是本机时刻）。
    pub fn push(&mut self, now_us: u64, packet: AudioPacket) {
        // 相对到达延迟：到达 − 采集。两端时钟不同步，但只用它的“相对”变化。
        let raw = i64::try_from(now_us).unwrap_or(i64::MAX)
            - i64::try_from(packet.capture_us).unwrap_or(i64::MAX);
        self.arrivals.push_back((now_us, raw));
        while self
            .arrivals
            .front()
            .is_some_and(|(t, _)| now_us.saturating_sub(*t) > WINDOW_US)
        {
            self.arrivals.pop_front();
        }
        self.update_target();
        self.anchor = Some((packet.seq, packet.capture_us));

        let mut insert = |seq: u32, data: Vec<u8>, redundant: bool| {
            if let Some(next) = self.next {
                // 已经播过（迟到）的丢掉
                if seq.wrapping_sub(next) > u32::MAX / 2 {
                    if !redundant {
                        self.stats.late = self.stats.late.saturating_add(1);
                    }
                    return;
                }
            }
            if let std::collections::btree_map::Entry::Vacant(slot) = self.frames.entry(seq) {
                if redundant {
                    self.stats.recovered = self.stats.recovered.saturating_add(1);
                }
                slot.insert(data);
            }
        };
        for (i, r) in packet.redundant.into_iter().enumerate() {
            let back = u32::try_from(i).unwrap_or(0).saturating_add(1);
            insert(packet.seq.wrapping_sub(back), r, true);
        }
        insert(packet.seq, packet.frame, false);
    }

    fn update_target(&mut self) {
        // push 之后才调用，窗口里至少有一个包
        let fastest = self.arrivals.iter().map(|(_, d)| *d).min().unwrap_or(0);
        let mut delays: Vec<u64> = self
            .arrivals
            .iter()
            .map(|(_, d)| u64::try_from(d.saturating_sub(fastest)).unwrap_or(0))
            .collect();
        delays.sort_unstable();
        let index = delays
            .len()
            .saturating_sub(1)
            .saturating_mul(QUANTILE_PERCENT)
            / 100;
        let jitter_ms = delays.get(index).copied().unwrap_or(0) / 1000;
        // 目标 = 抖动的高分位 + 一帧余量，夹在上下限之间
        self.target_ms = (jitter_ms + FRAME_MS).clamp(MIN_TARGET_MS, MAX_TARGET_MS);
    }

    /// 播放端每 10 ms 调用一次，取下一帧。
    pub fn pop(&mut self) -> Play {
        if self.refilling {
            // 攒到目标再开始（开头、见底之后）
            if self.level_ms() < self.target_ms {
                return Play::Wait;
            }
            self.refilling = false;
            if self.next.is_none() {
                self.next = self.frames.keys().next().copied();
            }
        }
        let Some(next) = self.next else {
            return Play::Wait;
        };
        // 缓冲比目标多出两帧以上：丢掉最老的一帧追上（加速）
        if self.level_ms() > self.target_ms + 2 * FRAME_MS {
            let second = next.wrapping_add(1);
            if let (Some(a), Some(b)) = (self.frames.get(&next), self.frames.get(&second)) {
                let pair = (a.clone(), b.clone());
                let _ = self.frames.remove(&next);
                let _ = self.frames.remove(&second);
                self.stats.accelerated = self.stats.accelerated.saturating_add(1);
                self.next = Some(second.wrapping_add(1));
                return Play::Accelerate(pair.0, pair.1);
            }
        }
        // 缓冲比目标少一帧以上（目标刚涨上去）：多播一帧补偿、时间线不前进，把缓冲攒起来（减速）
        if !self.decelerated_last
            && self.level_ms() + FRAME_MS < self.target_ms
            && self.frames.contains_key(&next)
        {
            self.decelerated_last = true;
            self.stats.decelerated = self.stats.decelerated.saturating_add(1);
            return Play::Expand;
        }
        self.decelerated_last = false;
        self.next = Some(next.wrapping_add(1));
        if let Some(frame) = self.frames.remove(&next) {
            self.stats.normal = self.stats.normal.saturating_add(1);
            return Play::Normal(frame);
        }
        if self.frames.is_empty() {
            // 见底：后面一帧都没有，补偿一帧后进入等待
            self.stats.underruns = self.stats.underruns.saturating_add(1);
            self.refilling = true;
        }
        self.stats.concealed = self.stats.concealed.saturating_add(1);
        Play::Expand
    }
}

impl JitterBuffer {
    /// 接下来要播的那一帧：正在播时是下一帧，还没开始（或见底后在等）时是缓冲里最早的一帧。
    fn upcoming(&self) -> Option<u32> {
        if self.refilling {
            self.frames.keys().next().copied()
        } else {
            self.next
        }
    }

    /// 接下来要播的那一帧的采集时刻（被控端时钟，微秒），按最近一个包的采集时刻和序号推算。
    /// 缓冲是空的、还没开始时为 `None`。音画同步据此算它的播放时刻（[`crate::avsync::AvSync::lead_us`]）。
    #[must_use]
    pub fn next_capture_us(&self) -> Option<u64> {
        let (seq, capture_us) = self.anchor?;
        let next = self.upcoming()?;
        // 序号差按有符号算：下一帧通常比最近的包早几帧
        let ahead = next.wrapping_sub(seq);
        let frame_us = FRAME_MS * 1000;
        Some(if ahead > u32::MAX / 2 {
            capture_us.saturating_sub(u64::from(seq.wrapping_sub(next)) * frame_us)
        } else {
            capture_us.saturating_add(u64::from(ahead) * frame_us)
        })
    }

    /// 音画同步打开时每 10 ms 调用一次（代替 [`Self::pop`]）。`lead_us` 是接下来要播的那一帧
    /// （[`Self::next_capture_us`]）离它的播放时刻还有多久：正数是还没到，负数是已经过了。
    ///
    /// - 在 ±[`SYNC_BAND_US`] 之内：正常播；
    /// - 早了：多播一帧补偿、时间线不前进（减速）；早得多（超过 [`SYNC_JUMP_US`]）就播静音等；
    /// - 晚了：两帧压成一帧（加速）；晚得多就把过了时刻的帧直接跳过。
    pub fn pop_synced(&mut self, lead_us: i64) -> Play {
        let Some(mut next) = self.upcoming() else {
            return Play::Wait;
        };
        if self.refilling {
            // 开头、见底之后：等最早的一帧到它的播放时刻
            if self.frames.is_empty() || lead_us > SYNC_BAND_US / 2 {
                return Play::Wait;
            }
            self.refilling = false;
            self.next = Some(next);
        }
        let mut lead_us = lead_us;
        if lead_us > SYNC_JUMP_US {
            // 早得多：播静音等时间线追上来（一帧一帧减速要播很久的补偿音）
            self.stats.decelerated = self.stats.decelerated.saturating_add(1);
            return Play::Wait;
        }
        if lead_us < -SYNC_JUMP_US {
            // 晚得多：过了时刻的帧直接跳过
            let frame_us = i64::try_from(FRAME_MS * 1000).unwrap_or(i64::MAX);
            let skip = u32::try_from(lead_us.saturating_neg() / frame_us).unwrap_or(u32::MAX);
            next = next.wrapping_add(skip);
            self.frames
                .retain(|seq, _| seq.wrapping_sub(next) <= u32::MAX / 2);
            self.next = Some(next);
            self.stats.accelerated = self.stats.accelerated.saturating_add(u64::from(skip));
            lead_us = lead_us.saturating_add(frame_us.saturating_mul(i64::from(skip)));
        }
        if lead_us < -SYNC_BAND_US {
            let second = next.wrapping_add(1);
            if let (Some(a), Some(b)) = (self.frames.get(&next), self.frames.get(&second)) {
                let pair = (a.clone(), b.clone());
                let _ = self.frames.remove(&next);
                let _ = self.frames.remove(&second);
                self.stats.accelerated = self.stats.accelerated.saturating_add(1);
                self.decelerated_last = false;
                self.next = Some(second.wrapping_add(1));
                return Play::Accelerate(pair.0, pair.1);
            }
        }
        if lead_us > SYNC_BAND_US && !self.decelerated_last && self.frames.contains_key(&next) {
            self.decelerated_last = true;
            self.stats.decelerated = self.stats.decelerated.saturating_add(1);
            return Play::Expand;
        }
        self.decelerated_last = false;
        self.next = Some(next.wrapping_add(1));
        if let Some(frame) = self.frames.remove(&next) {
            self.stats.normal = self.stats.normal.saturating_add(1);
            return Play::Normal(frame);
        }
        if self.frames.is_empty() {
            self.stats.underruns = self.stats.underruns.saturating_add(1);
            self.refilling = true;
        }
        self.stats.concealed = self.stats.concealed.saturating_add(1);
        Play::Expand
    }
}

/// 冗余开关：丢包率（千分比）达到 [`RED_ON_PERMILLE`] 就带上前一帧的副本；
/// 低于 [`RED_OFF_PERMILLE`] 并持续 [`RED_QUIET_MS`] 才关（防止来回切换）。没有丢包时不带，省流量（中继是成本）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Redundancy {
    on: bool,
    /// 从什么时候开始一直低于关闭门槛（毫秒）。
    quiet_since: Option<u64>,
}

/// 打开冗余的丢包率（千分比，5 = 0.5%）。
pub const RED_ON_PERMILLE: u16 = 5;
/// 可以关闭冗余的丢包率（千分比）。
pub const RED_OFF_PERMILLE: u16 = 2;
/// 低于关闭门槛要持续多久才关（毫秒）。
pub const RED_QUIET_MS: u64 = 10_000;

impl Redundancy {
    /// 当前是否带副本。
    #[must_use]
    pub const fn on(&self) -> bool {
        self.on
    }

    /// 收到接收端报告的丢包率（千分比），`now_ms` 是本机时刻。返回之后是否带副本。
    pub fn report(&mut self, loss_permille: u16, now_ms: u64) -> bool {
        if loss_permille >= RED_ON_PERMILLE {
            self.on = true;
            self.quiet_since = None;
        } else if loss_permille < RED_OFF_PERMILLE {
            let since = *self.quiet_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= RED_QUIET_MS {
                self.on = false;
            }
        } else {
            // 在两个门槛之间：保持现状，重新计时
            self.quiet_since = None;
        }
        self.on
    }
}

/// 两次统计之间的丢包率（千分比）：丢了的帧（不管有没有被副本补回）占应收帧的比例。
#[must_use]
pub fn loss_permille(before: &AudioStats, after: &AudioStats) -> u16 {
    let lost = after.recovered.saturating_sub(before.recovered)
        + after
            .concealed
            .saturating_sub(before.concealed)
            .saturating_sub(after.underruns.saturating_sub(before.underruns));
    let played = after.normal.saturating_sub(before.normal)
        + after.concealed.saturating_sub(before.concealed);
    if played == 0 {
        return 0;
    }
    u16::try_from((lost * 1000 / played).min(1000)).unwrap_or(1000)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn packet(seq: u32, redundancy: usize) -> AudioPacket {
        AudioPacket {
            seq,
            capture_us: u64::from(seq) * FRAME_MS * 1000,
            frame: vec![u8::try_from(seq % 256).unwrap(); 100],
            redundant: (1..=redundancy)
                .filter(|&k| u32::try_from(k).unwrap() <= seq)
                .map(|k| vec![u8::try_from((seq - u32::try_from(k).unwrap()) % 256).unwrap(); 100])
                .collect(),
        }
    }

    #[test]
    fn redundancy_turns_on_fast_and_off_slowly() {
        let mut r = Redundancy::default();
        assert!(!r.on());
        assert!(!r.report(0, 0));
        // 0.5% 丢包：马上打开
        assert!(r.report(5, 100));
        // 回到 0：10 秒后才关
        assert!(r.report(0, 200));
        assert!(r.report(1, 5_000));
        // 中间夹着一次 0.3%（两个门槛之间）：重新计时
        assert!(r.report(3, 6_000));
        assert!(r.report(0, 7_000));
        assert!(r.report(0, 16_000));
        assert!(!r.report(0, 17_000));
    }

    #[test]
    fn loss_rate_from_stats() {
        let before = AudioStats::default();
        let after = AudioStats {
            normal: 95,
            concealed: 5,
            recovered: 5,
            underruns: 1,
            ..AudioStats::default()
        };
        // 丢了 5 + 4 = 9 帧（见底的那一次不算丢包），应收 100 帧 → 9%
        assert_eq!(loss_permille(&before, &after), 90);
        assert_eq!(loss_permille(&after, &after), 0);
    }

    #[test]
    fn packet_round_trip_and_limits() {
        let p = packet(7, 2);
        assert_eq!(AudioPacket::decode(&p.encode().unwrap()).unwrap(), p);
        let too_many = AudioPacket {
            redundant: vec![vec![]; 3],
            ..packet(1, 0)
        };
        assert!(too_many.encode().is_err());
        let mut bad = packet(5, 0).encode().unwrap();
        bad[12] = 9;
        assert!(AudioPacket::decode(&bad).is_err());
        assert!(AudioPacket::decode(&[1, 2]).is_err());
        let huge = AudioPacket {
            redundant: vec![vec![0; 70_000]],
            ..packet(1, 0)
        };
        assert!(huge.encode().is_err());
    }

    /// 按固定网络延迟加抖动送包，每 10 ms 播一帧；返回统计和最终目标。
    fn run(
        jitter_ms: impl Fn(u32) -> u64,
        lose: impl Fn(u32) -> bool,
        redundancy: usize,
    ) -> (AudioStats, u64) {
        let mut jb = JitterBuffer::new();
        let mut pending: Vec<(u64, AudioPacket)> = (0..500u32)
            .filter(|&s| !lose(s))
            .map(|s| {
                (
                    u64::from(s) * 10_000 + 30_000 + jitter_ms(s) * 1000,
                    packet(s, redundancy),
                )
            })
            .collect();
        pending.sort_by_key(|(t, _)| *t);
        let mut it = pending.into_iter().peekable();
        // 只播到最后一个包能到的时刻（之后流结束，见底是正常的）
        for tick in 0..500u64 {
            let now = tick * 10_000;
            while let Some((t, _)) = it.peek() {
                if *t > now {
                    break;
                }
                let (t, p) = it.next().unwrap();
                jb.push(t, p);
            }
            let _ = jb.pop();
        }
        (jb.stats(), jb.target_ms())
    }

    #[test]
    fn steady_network_plays_everything_at_low_delay() {
        let (stats, target) = run(|_| 0, |_| false, 0);
        assert_eq!(target, MIN_TARGET_MS);
        assert!(stats.normal >= 495, "{stats:?}");
        assert_eq!(
            (stats.concealed, stats.late, stats.accelerated),
            (0, 0, 0),
            "{stats:?}"
        );
    }

    #[test]
    fn jitter_raises_the_target_and_avoids_underruns() {
        // 每 7 个包有一个晚到 45 ms
        let (stats, target) = run(|s| if s % 7 == 0 { 45 } else { 0 }, |_| false, 0);
        assert!(target >= 50, "目标 {target} ms");
        // 开头目标还没涨上来时会迟到几包，之后靠减速攒够缓冲，不再迟到
        assert!(stats.late <= 5, "{stats:?}");
        assert!(stats.decelerated >= 2, "{stats:?}");
        assert!(stats.normal >= 450, "{stats:?}");
    }

    #[test]
    fn redundancy_recovers_lost_packets() {
        let lose = |s: u32| s % 10 == 5;
        let (without, _) = run(|_| 0, lose, 0);
        let (with, _) = run(|_| 0, lose, 1);
        assert!(without.concealed >= 45, "{without:?}");
        assert!(with.concealed <= 2, "{with:?}");
        assert!(with.recovered >= 45, "{with:?}");
    }

    /// 音画同步的仿真：被控端时钟比本机晚 `HOST_AHEAD`，网络固定 30 ms 加抖动。每 10 ms 一拍，
    /// `delay_ms(tick)` 是那一拍的播放延迟（`None` 表示同步关着，走水位）。
    /// 返回每个正常播放的帧（拍号，序号，播放时刻 − 采集时刻换到本机时钟后的差，微秒）和抖动缓冲。
    fn run_synced(
        ticks: u64,
        jitter_ms: impl Fn(u32) -> u64,
        arrives: impl Fn(u32) -> bool,
        delay_ms: impl Fn(u64) -> Option<u32>,
    ) -> (Vec<(u64, u32, i64)>, JitterBuffer) {
        const HOST_AHEAD: u64 = 7_000_000;
        let mut jb = JitterBuffer::new();
        let mut sync = crate::avsync::AvSync::new(1).unwrap();
        let count = u32::try_from(ticks).unwrap();
        let mut pending: Vec<(u64, AudioPacket)> = (0..count)
            .filter(|&s| arrives(s))
            .map(|s| {
                let mut p = packet(s, 0);
                p.frame = s.to_be_bytes().to_vec();
                p.capture_us += HOST_AHEAD;
                (u64::from(s) * 10_000 + 30_000 + jitter_ms(s) * 1000, p)
            })
            .collect();
        pending.sort_by_key(|(t, _)| *t);
        let mut it = pending.into_iter().peekable();
        let mut played = Vec::new();
        for tick in 0..ticks {
            let now = tick * 10_000 + 3_000;
            while let Some((t, _)) = it.peek() {
                if *t > now {
                    break;
                }
                let (t, p) = it.next().unwrap();
                sync.observe_sound(t, p.capture_us);
                jb.push(t, p);
            }
            let play = match delay_ms(tick) {
                Some(d) => {
                    sync.set_delay_ms(d);
                    match jb.next_capture_us().and_then(|c| sync.lead_us(now, c)) {
                        Some(lead) => jb.pop_synced(lead),
                        None => jb.pop(),
                    }
                }
                None => jb.pop(),
            };
            let seqs: Vec<u32> = match play {
                Play::Normal(f) => vec![u32::from_be_bytes(f.try_into().unwrap())],
                Play::Accelerate(_, b) => vec![u32::from_be_bytes(b.try_into().unwrap())],
                Play::Expand | Play::Wait => vec![],
            };
            for seq in seqs {
                // 这一帧的采集时刻换到本机时钟：序号 × 10 ms（被控端和本机的时钟差在这里抵消）
                let capture_local = i64::from(seq) * 10_000;
                played.push((tick, seq, i64::try_from(now).unwrap() - capture_local));
            }
        }
        (played, jb)
    }

    #[test]
    fn synced_playout_follows_the_capture_timeline() {
        // 抖动 0–40 ms：水位自适应时延迟跟着抖动走；同步时固定在“最快的包 + 150 ms”= 180 ms
        let jitter = |s: u32| u64::from(s * 7 % 5) * 10;
        let (played, jb) = run_synced(600, jitter, |_| true, |_| Some(150));
        let st = jb.stats();
        assert!(st.normal >= 570, "{st:?}");
        assert_eq!((st.concealed, st.late, st.underruns), (0, 0, 0), "{st:?}");
        for (tick, seq, delay) in &played {
            assert!(
                (delay - 180_000).abs() <= SYNC_BAND_US,
                "第 {tick} 拍播第 {seq} 帧，延迟 {delay} 微秒"
            );
        }
        // 每一帧都按顺序播了一次
        assert!(played.windows(2).all(|w| w[1].1 == w[0].1 + 1));
    }

    #[test]
    fn turning_sync_on_and_changing_the_delay_realigns_the_audio() {
        // 前 2 秒同步关着（水位，约 20–40 ms），然后 150 ms，再降到 60 ms，最后关掉
        let delay = |tick: u64| match tick {
            0..200 => None,
            200..500 => Some(150),
            500..800 => Some(60),
            _ => None,
        };
        let (played, jb) = run_synced(1000, |_| 0, |_| true, delay);
        let at = |tick: u64| played.iter().find(|(t, _, _)| *t >= tick).unwrap().2;
        // 关着：水位决定，远小于 150 ms
        assert!(at(150) < 100_000, "{}", at(150));
        // 打开后 1 秒内对齐到 30 + 150 ms（差得多：播静音等，不是一帧帧减速）
        assert!((at(300) - 180_000).abs() <= SYNC_BAND_US, "{}", at(300));
        // 播放延迟降到 60 ms：跳过过了时刻的帧，对齐到 90 ms
        assert!((at(600) - 90_000).abs() <= SYNC_BAND_US, "{}", at(600));
        // 关掉：回到水位
        assert!(at(950) < 80_000, "{}", at(950));
        let st = jb.stats();
        assert!(st.accelerated >= 9, "{st:?}");
        assert!(st.decelerated >= 1, "{st:?}");
        assert_eq!(st.underruns, 0, "{st:?}");
    }

    #[test]
    fn synced_playout_nudges_by_one_frame_and_survives_gaps() {
        // 播放延迟小幅变化（±25 ms）：一帧一帧减速、加速，不出静音
        let delay = |tick: u64| match tick {
            0..200 => Some(100),
            200..400 => Some(125),
            _ => Some(100),
        };
        let (played, jb) = run_synced(600, |_| 0, |_| true, delay);
        let at = |tick: u64| played.iter().find(|(t, _, _)| *t >= tick).unwrap().2;
        assert!((at(100) - 130_000).abs() <= SYNC_BAND_US, "{}", at(100));
        assert!((at(300) - 155_000).abs() <= SYNC_BAND_US, "{}", at(300));
        assert!((at(500) - 130_000).abs() <= SYNC_BAND_US, "{}", at(500));
        let st = jb.stats();
        assert!((1..=4).contains(&st.decelerated), "{st:?}");
        assert!((1..=4).contains(&st.accelerated), "{st:?}");
        // 断了 1 秒（第 200–299 帧没到）：见底一次，恢复后仍然对齐
        let (played, jb) = run_synced(600, |_| 0, |s| !(200..300).contains(&s), |_| Some(100));
        let st = jb.stats();
        assert_eq!(st.underruns, 1, "{st:?}");
        let (_, seq, delay) = *played.iter().find(|(t, _, _)| *t >= 400).unwrap();
        assert!(
            seq >= 300 && (delay - 130_000).abs() <= SYNC_BAND_US,
            "{seq} {delay}"
        );
        // 单个包丢了：补偿一帧，时间线不乱
        let (played, jb) = run_synced(300, |_| 0, |s| s != 100, |_| Some(100));
        assert_eq!(jb.stats().concealed, 1, "{:?}", jb.stats());
        assert!(played
            .iter()
            .all(|(_, _, d)| (d - 130_000).abs() <= SYNC_BAND_US));
        // 还没收到包：等待
        let mut empty = JitterBuffer::new();
        assert_eq!(empty.next_capture_us(), None);
        assert_eq!(empty.pop_synced(0), Play::Wait);
    }

    #[test]
    fn backlog_is_drained_by_accelerating_and_late_packets_are_dropped() {
        let mut jb = JitterBuffer::new();
        // 一下子来了 30 帧（例如网络卡顿后一起到）
        for s in 0..30 {
            jb.push(1_000, packet(s, 0));
        }
        let mut normal = 0;
        for _ in 0..10 {
            if let Play::Normal(_) = jb.pop() {
                normal += 1;
            }
        }
        assert!(jb.stats().accelerated > 0, "{:?}", jb.stats());
        assert!(jb.level_ms() <= jb.target_ms() + 2 * FRAME_MS);
        assert!(normal > 0);
        // 已经播过的帧迟到了：丢弃并计数
        jb.push(2_000, packet(0, 0));
        assert_eq!(jb.stats().late, 1);
        // 见底：补偿一帧后进入等待
        let mut empty = JitterBuffer::new();
        assert_eq!(empty.pop(), Play::Wait);
        empty.push(0, packet(0, 0));
        empty.push(0, packet(1, 0));
        empty.push(0, packet(2, 0));
        empty.push(0, packet(3, 0));
        for _ in 0..20 {
            let _ = empty.pop();
        }
        // 4 帧都播完（中间可能有减速帧），之后见底一次，然后一直等待
        assert_eq!(empty.stats().normal, 4, "{:?}", empty.stats());
        assert_eq!(empty.stats().underruns, 1, "{:?}", empty.stats());
        assert_eq!(empty.pop(), Play::Wait);
        assert_eq!(JitterBuffer::default().target_ms(), START_TARGET_MS);
    }
}
