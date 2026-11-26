//! 音画同步和画面的播放缓冲（看视频、电影时用，默认关；调研和设计见 `docs/research/N-av-sync.md`）。
//!
//! 办公时画面“收到就显示”、声音走自己的抖动缓冲，两路各走各的，延迟最低。看视频时要的是另一头：
//! 画面匀、口型对得上，多等一百来毫秒没关系。打开后两路都按同一条时间线播放：
//!
//! > 播放时刻 = 采集时刻（被控端时钟） + 时钟差 + 固定的播放延迟
//!
//! - **采集时刻**：被控端给每个画面帧（[`crate::video::stamp_capture`]）和每个声音包
//!   （[`CaptureClock`]）打上同一个时钟（连接建立以来的微秒）的采集时刻。双方都有
//!   [`crate::proto::Caps::AV_SYNC`] 才打，老版本不受影响。
//! - **时钟差**（[`ClockMap`]）：两端时钟不同步，也不需要同步。取最近一段时间里“到达时刻 − 采集时刻”
//!   的最小值（最快的那个包），和声音抖动缓冲、WebRTC NetEQ 的“相对到达延迟”是同一个办法。
//!   画面和声音用**同一个**时钟差，所以同一时刻采集的画面和声音在同一时刻播放。
//!   有声音时只用声音包算（每个包一样大、间隔一样，最小值稳定）；画面帧有大有小，大帧在链路上要多走
//!   十几毫秒，混在一起算的话，一个小帧进出窗口就会让时钟差跳一下（仿真里实测跳 9.4 ms）。
//!   没有声音时才用画面帧。
//! - **播放延迟**：一个配置值（毫秒，0 表示关）。网络抖动、编码和解码用时都要装得进去；
//!   装不进去的帧（到的时候已经过了播放时刻）马上显示并计数（[`PlayoutStats::late`]），不丢。
//!
//! 这里只有不做 IO 的状态机；定时器和显示在控制端会话里（`peerdesk_net::controller`）。

use std::collections::VecDeque;

/// 播放延迟的上限（毫秒）：再大就不是“看视频多等一下”，而是网络有问题，应当提示。
pub const MAX_DELAY_MS: u32 = 1000;
/// 时钟差的统计窗口：每桶多长（微秒）、留几桶。
const BUCKET_US: u64 = 1_000_000;
const BUCKETS: u64 = 10;
/// 播放缓冲最多压多少帧：再多就不等时间、马上显示（防止时钟差算错时越积越多）。
pub const MAX_HELD: usize = 64;

fn signed(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// 被控端时钟到本机时钟的换算：最近 10 秒里“到达 − 采集”的最小值。
///
/// 最小值对应走得最快的包（没有排队），抖动只会让别的包更晚，不会让最小值变小；窗口让它跟得上
/// 两端时钟的漂移和路径变化。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClockMap {
    /// （桶号，这一桶里的最小差）。
    buckets: VecDeque<(u64, i64)>,
}

impl ClockMap {
    /// 记一个包：`now_us` 是本机到达时刻，`capture_us` 是被控端的采集时刻。
    pub fn observe(&mut self, now_us: u64, capture_us: u64) {
        let offset = signed(now_us).saturating_sub(signed(capture_us));
        let slot = now_us / BUCKET_US;
        match self.buckets.back_mut() {
            Some((s, min)) if *s == slot => *min = (*min).min(offset),
            Some(_) | None => self.buckets.push_back((slot, offset)),
        }
        self.expire(now_us);
    }

    /// 到了 `now_us`：窗口之外的记录作废（这一路停了之后，不让它的老记录一直留着）。
    pub fn expire(&mut self, now_us: u64) {
        let slot = now_us / BUCKET_US;
        while self
            .buckets
            .front()
            .is_some_and(|(s, _)| slot.saturating_sub(*s) >= BUCKETS)
        {
            self.buckets.pop_front();
        }
    }

    /// 当前的时钟差（本机 − 被控端，含最短的传输时间）；还没收到过包时为 `None`。
    #[must_use]
    pub fn offset_us(&self) -> Option<i64> {
        self.buckets.iter().map(|(_, m)| *m).min()
    }
}

/// 音画同步：时钟差加播放延迟，画面和声音共用一个。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvSync {
    delay_us: u64,
    /// 按声音包算的时钟差（有声音时用它）。
    sound: ClockMap,
    /// 按画面帧算的时钟差（没有声音时用）。
    picture: ClockMap,
}

impl AvSync {
    /// 创建。`delay_ms` 是播放延迟；0 表示关（返回 `None`），超过 [`MAX_DELAY_MS`] 按上限算。
    #[must_use]
    pub fn new(delay_ms: u32) -> Option<Self> {
        (delay_ms > 0).then(|| Self {
            delay_us: u64::from(delay_ms.min(MAX_DELAY_MS)) * 1000,
            sound: ClockMap::default(),
            picture: ClockMap::default(),
        })
    }

    /// 播放延迟（毫秒）。
    #[must_use]
    pub const fn delay_ms(&self) -> u64 {
        self.delay_us / 1000
    }

    /// 改播放延迟（时钟差留着）。和 [`Self::new`] 一样按上限夹住；0 按 1 ms 算（要关就把整个对象丢掉）。
    pub fn set_delay_ms(&mut self, delay_ms: u32) {
        self.delay_us = u64::from(delay_ms.clamp(1, MAX_DELAY_MS)) * 1000;
    }

    /// 一个声音包到了（`now_us` 是本机时刻，`capture_us` 是包里的采集时刻）。
    pub fn observe_sound(&mut self, now_us: u64, capture_us: u64) {
        self.sound.observe(now_us, capture_us);
        self.picture.expire(now_us);
    }

    /// 一个画面帧收齐了（`now_us` 是本机时刻，`capture_us` 是帧里的采集时刻）。
    pub fn observe_picture(&mut self, now_us: u64, capture_us: u64) {
        self.picture.observe(now_us, capture_us);
        self.sound.expire(now_us);
    }

    /// 当前的时钟差：有声音时按声音包，没有时按画面帧；什么都没收到时为 `None`。
    #[must_use]
    pub fn offset_us(&self) -> Option<i64> {
        self.sound.offset_us().or_else(|| self.picture.offset_us())
    }

    /// 这个采集时刻的内容该在本机什么时刻播放；还没有时钟差时为 `None`。
    #[must_use]
    pub fn due_us(&self, capture_us: u64) -> Option<u64> {
        let local = signed(capture_us).saturating_add(self.offset_us()?);
        Some(
            u64::try_from(local)
                .unwrap_or(0)
                .saturating_add(self.delay_us),
        )
    }

    /// 离播放时刻还有多久（微秒）：正数是还没到，负数是已经过了。还没有时钟差时为 `None`。
    #[must_use]
    pub fn lead_us(&self, now_us: u64, capture_us: u64) -> Option<i64> {
        Some(signed(self.due_us(capture_us)?).saturating_sub(signed(now_us)))
    }
}

/// 播放缓冲的计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlayoutStats {
    /// 交出去显示的帧数。
    pub shown: u64,
    /// 到的时候已经过了播放时刻的帧数（播放延迟装不下这次抖动）；这些帧马上显示。
    pub late: u64,
    /// 因为积压太多（超过 [`MAX_HELD`]）没等到时间就显示的帧数。
    pub overflow: u64,
}

/// 画面的播放缓冲：解码好的帧按播放时刻排队，到时间才交出去显示。不改变顺序，不丢帧。
#[derive(Debug, Clone)]
pub struct FrameQueue<T> {
    queue: VecDeque<(u64, T)>,
    last_due: u64,
    stats: PlayoutStats,
}

impl<T> Default for FrameQueue<T> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            last_due: 0,
            stats: PlayoutStats::default(),
        }
    }
}

impl<T> FrameQueue<T> {
    /// 放进一帧。`due_us` 是播放时刻（[`AvSync::due_us`]）；`None` 表示不缓冲、马上显示（同步关着，或者这一帧
    /// 没带采集时刻）。
    pub fn push(&mut self, now_us: u64, due_us: Option<u64>, item: T) {
        if due_us.is_some_and(|due| due < now_us) {
            self.stats.late = self.stats.late.saturating_add(1);
        }
        // 不让后面的帧排到前面去（时钟差变小时播放时刻会往前挪）
        let due = due_us.unwrap_or(now_us).max(self.last_due);
        self.last_due = due;
        self.queue.push_back((due, item));
    }

    /// 最早的一帧什么时候到时间；空的时候为 `None`。积压太多时返回 0（马上）。
    #[must_use]
    pub fn next_due(&self) -> Option<u64> {
        let (due, _) = self.queue.front()?;
        Some(if self.queue.len() > MAX_HELD { 0 } else { *due })
    }

    /// 取一帧到了时间的；没有时返回 `None`。一次调用取一帧，调用方循环取到空。
    pub fn pop_due(&mut self, now_us: u64) -> Option<T> {
        let due = self.queue.front().map(|(due, _)| *due)?;
        let overflow = self.queue.len() > MAX_HELD;
        if due > now_us && !overflow {
            return None;
        }
        if due > now_us {
            self.stats.overflow = self.stats.overflow.saturating_add(1);
        }
        self.stats.shown = self.stats.shown.saturating_add(1);
        self.queue.pop_front().map(|(_, item)| item)
    }

    /// 同步关掉了：压着的帧不再等，下一次 [`Self::pop_due`] 起按顺序全部交出。
    pub fn release(&mut self) {
        for (due, _) in &mut self.queue {
            *due = 0;
        }
        self.last_due = 0;
    }

    /// 压着几帧。
    #[must_use]
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// 是不是空的。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// 计数。
    #[must_use]
    pub const fn stats(&self) -> PlayoutStats {
        self.stats
    }
}

/// 被控端给声音帧打采集时刻：和画面帧同一个时钟（连接建立以来的微秒）。
///
/// 声卡每 10 ms 交来一帧，但交来的时刻有抖动（系统一次给几帧），直接用“交来的时刻”会把这个抖动带进
/// 控制端的抖动缓冲。所以时间线按帧号走（第 n 帧 = 起点 + n × 帧长），起点取“交来时刻 − n × 帧长”
/// 在一个窗口里的最小值（交得最及时的那一帧）。声卡时钟和系统时钟有漂移、采集中断过时，窗口最小值会
/// 慢慢偏离起点；偏离超过 [`CAPTURE_STEP_US`] 才挪起点，平时时间线严格等间隔（和不打时刻时一样）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureClock {
    /// 时间线起点（第 0 帧的采集时刻）。
    base: Option<i64>,
    /// 当前窗口：（开始时刻，窗口里的最小起点）。
    window: Option<(u64, i64)>,
}

/// 采集时钟的窗口（微秒）。
const CAPTURE_WINDOW_US: u64 = 5_000_000;
/// 起点偏离窗口最小值多少才挪（微秒）。
pub const CAPTURE_STEP_US: i64 = 5_000;

impl CaptureClock {
    /// 第 `index` 帧（每帧 `frame_us` 微秒）在 `now_us` 交来：返回它的采集时刻（这一帧开头的时刻）。
    pub fn stamp(&mut self, now_us: u64, index: u64, frame_us: u64) -> u64 {
        let elapsed = signed(index.saturating_mul(frame_us));
        // 交来时这一帧已经录完：开头在一帧之前
        let start = signed(now_us)
            .saturating_sub(signed(frame_us))
            .saturating_sub(elapsed);
        let base = *self.base.get_or_insert(start);
        let (since, min) = self
            .window
            .map_or((now_us, start), |(t, m)| (t, m.min(start)));
        self.window = Some((since, min));
        let mut base = base;
        if now_us.saturating_sub(since) >= CAPTURE_WINDOW_US {
            if min.saturating_sub(base).abs() > CAPTURE_STEP_US {
                base = min;
                self.base = Some(min);
            }
            self.window = None;
        }
        u64::try_from(base.saturating_add(elapsed)).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn clock_offset_is_the_fastest_packet_in_the_window() {
        let mut c = ClockMap::default();
        assert_eq!(c.offset_us(), None);
        // 被控端时钟比本机早 5 秒；传输 30 ms，抖动只会更晚
        c.observe(5_030_000, 0);
        c.observe(5_075_000, 10_000);
        c.observe(5_050_000, 20_000);
        assert_eq!(c.offset_us(), Some(5_030_000));
        // 同一桶里来了更快的：最小值变小
        c.observe(5_129_000, 100_000);
        assert_eq!(c.offset_us(), Some(5_029_000));
        // 路径变慢了（+20 ms）：老的最小值出了 10 秒窗口后跟上
        for i in 1..=12u64 {
            c.observe(5_050_000 + i * 1_000_000, i * 1_000_000);
        }
        assert_eq!(c.offset_us(), Some(5_050_000));
        // 被控端时钟比本机晚（差是负的）也行
        let mut neg = ClockMap::default();
        neg.observe(1_000, 9_000_000);
        assert_eq!(neg.offset_us(), Some(-8_999_000));
    }

    #[test]
    fn sync_schedules_audio_and_video_on_one_timeline() {
        assert!(AvSync::new(0).is_none());
        let mut s = AvSync::new(150).unwrap();
        assert_eq!(s.delay_ms(), 150);
        assert_eq!(s.due_us(0), None);
        assert_eq!(s.lead_us(0, 0), None);
        // 只有画面：按画面帧算（这一帧 70 ms 才收齐）
        s.observe_picture(2_070_000, 1_000_000);
        assert_eq!(s.due_us(1_000_000), Some(2_220_000));
        // 声音包 30 ms 到：有声音就按声音包算，不管画面帧是快是慢
        s.observe_sound(2_030_000, 1_000_000);
        s.observe_picture(2_045_000, 1_040_000);
        assert_eq!(s.offset_us(), Some(1_030_000));
        // 两路同一个采集时刻 → 同一个播放时刻
        assert_eq!(s.due_us(1_000_000), Some(2_180_000));
        assert_eq!(s.lead_us(2_070_000, 1_000_000), Some(110_000));
        assert_eq!(s.lead_us(2_200_000, 1_000_000), Some(-20_000));
        s.set_delay_ms(100);
        assert_eq!(s.due_us(1_000_000), Some(2_130_000));
        s.set_delay_ms(0);
        assert_eq!(s.delay_ms(), 1);
        assert_eq!(
            AvSync::new(5_000).unwrap().delay_ms(),
            u64::from(MAX_DELAY_MS)
        );
        // 被控端时钟比本机晚很多、换算出来是负的：按 0 算，不溢出
        let mut odd = AvSync::new(10).unwrap();
        odd.observe_sound(0, 5_000_000);
        assert_eq!(odd.due_us(1_000_000), Some(10_000));
        // 声音停了（关掉了）：10 秒后改按画面帧算
        for i in 0..=12u64 {
            s.observe_picture(3_050_000 + i * 1_000_000, 2_000_000 + i * 1_000_000);
        }
        assert_eq!(s.offset_us(), Some(1_050_000));
        // 反过来：画面停了（静止），声音还在，画面的老记录也作废
        for i in 0..=12u64 {
            s.observe_sound(20_030_000 + i * 1_000_000, 19_000_000 + i * 1_000_000);
        }
        assert_eq!(s.picture.offset_us(), None);
        assert_eq!(s.offset_us(), Some(1_030_000));
    }

    #[test]
    fn frame_queue_holds_frames_until_due_and_keeps_order() {
        let mut q: FrameQueue<u32> = FrameQueue::default();
        assert!(q.is_empty());
        assert_eq!(q.next_due(), None);
        assert_eq!(q.pop_due(100), None);
        q.push(0, Some(150), 1);
        q.push(10, Some(183), 2);
        // 时钟差变小，第 3 帧算出来比第 2 帧早：不插队
        q.push(20, Some(170), 3);
        assert_eq!(q.len(), 3);
        assert_eq!(q.next_due(), Some(150));
        assert_eq!(q.pop_due(149), None);
        assert_eq!(q.pop_due(150), Some(1));
        assert_eq!(q.pop_due(170), None);
        assert_eq!(q.pop_due(183), Some(2));
        assert_eq!(q.pop_due(183), Some(3));
        // 到的时候已经过了播放时刻：马上显示并计数
        q.push(400, Some(390), 4);
        assert_eq!(q.pop_due(400), Some(4));
        // 没有播放时刻（同步关着）：马上显示，不算迟到
        q.push(500, None, 5);
        assert_eq!(q.pop_due(500), Some(5));
        assert_eq!(
            q.stats(),
            PlayoutStats {
                shown: 5,
                late: 1,
                overflow: 0
            }
        );
        // 关掉同步：压着的帧马上按顺序交出
        q.push(600, Some(900), 6);
        q.push(610, Some(930), 7);
        assert_eq!(q.pop_due(620), None);
        q.release();
        assert_eq!(q.next_due(), Some(0));
        assert_eq!((q.pop_due(620), q.pop_due(620)), (Some(6), Some(7)));
        // 之后的帧不受关掉之前的播放时刻影响
        q.push(630, None, 8);
        assert_eq!(q.pop_due(630), Some(8));
    }

    #[test]
    fn frame_queue_does_not_grow_without_bound() {
        let mut q: FrameQueue<usize> = FrameQueue::default();
        for i in 0..=MAX_HELD {
            q.push(0, Some(1_000_000), i);
        }
        // 超过上限：最老的一帧不等时间
        assert_eq!(q.next_due(), Some(0));
        assert_eq!(q.pop_due(0), Some(0));
        assert_eq!(q.stats().overflow, 1);
        assert_eq!(q.pop_due(0), None);
        assert_eq!(q.next_due(), Some(1_000_000));
    }

    #[test]
    fn capture_clock_is_evenly_spaced_and_follows_drift_in_steps() {
        let mut c = CaptureClock::default();
        // 第 0 帧在 1.000 s 交来：这一帧的开头在 0.990 s
        assert_eq!(c.stamp(1_000_000, 0, 10_000), 990_000);
        // 交来的时刻抖动（晚 0–8 ms）：时间线仍然严格每帧 10 ms
        for n in 1..400u64 {
            let late = (n * 7) % 9 * 1000;
            assert_eq!(
                c.stamp(1_000_000 + n * 10_000 + late, n, 10_000),
                990_000 + n * 10_000
            );
        }
        // 声卡比系统时钟慢：每帧实际 10.05 ms，5 秒窗口里偏了 2.5 ms，不到 5 ms 不挪
        let mut slow = CaptureClock::default();
        let mut last = 0;
        let mut steps = 0;
        for n in 0..3000u64 {
            let at = slow.stamp(1_000_000 + n * 10_050, n, 10_000);
            if n > 0 && at != last + 10_000 {
                steps += 1;
                // 挪的时候是往真实时刻靠
                assert!(at > last + 10_000, "第 {n} 帧 {at}，上一帧 {last}");
            }
            last = at;
        }
        // 30 秒漂了 150 ms：分几次跟上，最后和真实采集时刻差不到一个窗口的漂移
        assert!((2..=6).contains(&steps), "挪了 {steps} 次");
        let truth = 1_000_000 + 2999 * 10_050 - 10_000;
        assert!(truth - last < 60_000, "差 {} 微秒", truth - last);
        // 本机时刻比“帧号 × 帧长”还小（不该发生）：按 0 算，不溢出
        assert_eq!(CaptureClock::default().stamp(0, 5, 10_000), 0);
    }
}
