//! 确定性网络仿真。
//!
//! 时间是虚拟的，随机性来自固定种子：同一个种子每次运行结果完全相同，
//! 所以仿真里发现的问题都能原样重放。链路模型包含延迟、抖动、随机丢包、
//! 带宽限制与队列溢出、链路中断。

use std::collections::BTreeMap;

use crate::Time;

/// 小型确定性随机数发生器（SplitMix64）。只用于仿真，不用于密码学。
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    /// 由种子创建。
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// 下一个 64 位随机数。
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// `[0, n)` 内的随机数；`n` 为 0 时返回 0。
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next_u64() % n
        }
    }

    /// 以千分之 `permille` 的概率返回 true。
    pub fn chance(&mut self, permille: u16) -> bool {
        self.below(1000) < u64::from(permille)
    }
}

/// Wi-Fi 式的延迟尖峰：信道忙、省电唤醒、后台扫描时，一段时间里发出的包一起晚到（不乱序）。
/// 2026-11-26 用户日志（Wi-Fi、约 30 Mbps）：大部分时候排队读数 0–2 ms，零星小包时常碰上 12–25 ms，延迟趋势正常。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spikes {
    /// 平均多久来一次（微秒）；两次之间的间隔在 0 到 2 倍之间均匀随机。
    pub every_us: u64,
    /// 一次持续多久（微秒）。
    pub length_us: u64,
    /// 这段时间里发出的包最多晚到多久（微秒）；每次在一半到全部之间随机。
    pub max_us: u64,
}

/// Wi-Fi 省电：链路空闲一阵之后，接收端网卡睡着了，路由器把包攒着、等它醒来再送，空闲之后的第一个包要多等一会儿，
/// 跟在它后面的包排在它后面（不乱序）；连续发包时网卡醒着，不多等。
/// 2026-11-26 用户日志（Wi-Fi、约 30 Mbps）：画面静止时零星小包的排队读数几乎每次都是 12–25 ms，连续发包时 0–2 ms。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Doze {
    /// 空闲多久（微秒）之后睡着。
    pub idle_us: u64,
    /// 醒来要多久（微秒）：每次在一半到全部之间随机。
    pub wake_us: u64,
}

/// 接收端偶尔忙一阵：这段时间到的包等它忙完一起读走（逐个读，时间戳相差约 1 微秒）。
/// 2026-11-26 用户日志：偶尔一帧几十个包几乎同一时刻读到，包串速率算成 Tbps；大部分时候排队读数 0–2 ms。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Busy {
    /// 平均多久忙一次（微秒）；两次之间的间隔在 0 到 2 倍之间均匀随机。
    pub every_us: u64,
    /// 一次忙多久（微秒）。
    pub length_us: u64,
}

/// 单向链路的特性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// 固定传播延迟（微秒）。
    pub delay_us: u64,
    /// 附加的随机抖动上限（微秒），均匀分布。
    pub jitter_us: u64,
    /// 抖动不乱序（Wi-Fi 式）：晚到的包挡住它后面的包，一阵一阵地晚到。默认关（每个包各抖各的，会乱序）。
    /// 真实 Wi-Fi 的抖动来自信道争用和聚合，基本不乱序；乱序的抖动会被接收端当成丢包（2026-11-26：
    /// 仿真 20 ms 乱序抖动下“丢包”读数 27–555‰，真实日志 0‰）。
    pub fifo_jitter: bool,
    /// Wi-Fi 式的延迟尖峰（见 [`Spikes`]），不乱序。默认没有。
    pub spikes: Option<Spikes>,
    /// Wi-Fi 省电（见 [`Doze`]），不乱序。默认没有。
    pub doze: Option<Doze>,
    /// 接收端偶尔忙一阵（见 [`Busy`]），不乱序。默认没有。
    pub busy: Option<Busy>,
    /// 接收端一批一批地收（微秒，0 表示随到随收）：到达时间向上取到这个间隔的整数倍。模拟 Wi-Fi 聚合、
    /// 网卡中断合并、程序忙时一次读走一批（2026-11-26 用户日志：一帧几十个包几乎同一时刻读到，包串测出 Tbps）。
    pub batch_us: u64,
    /// 随机丢包率（千分比）。
    pub loss_permille: u16,
    /// 带宽上限（比特每秒）；`None` 表示不限。
    pub rate_bps: Option<u64>,
    /// 排队时间超过这个值的包被丢弃（微秒）。
    pub max_queue_us: u64,
    /// 链路是否连通。
    pub up: bool,
}

impl Link {
    /// 无延迟、无丢包、不限速。
    #[must_use]
    pub const fn ideal() -> Self {
        Self {
            delay_us: 0,
            jitter_us: 0,
            fifo_jitter: false,
            spikes: None,
            batch_us: 0,
            doze: None,
            busy: None,
            loss_permille: 0,
            rate_bps: None,
            max_queue_us: 200_000,
            up: true,
        }
    }

    /// 有线局域网。
    #[must_use]
    pub const fn lan() -> Self {
        Self {
            delay_us: 300,
            jitter_us: 100,
            ..Self::ideal()
        }
    }

    /// 家用 Wi-Fi：有抖动和少量随机丢包。
    #[must_use]
    pub const fn wifi() -> Self {
        Self {
            delay_us: 3_000,
            jitter_us: 8_000,
            loss_permille: 10,
            rate_bps: Some(50_000_000),
            ..Self::ideal()
        }
    }

    /// 蜂窝网络：延迟较大、带宽有限。
    #[must_use]
    pub const fn cellular() -> Self {
        Self {
            delay_us: 40_000,
            jitter_us: 20_000,
            loss_permille: 20,
            rate_bps: Some(8_000_000),
            ..Self::ideal()
        }
    }

    /// 跨洲链路：延迟大。
    #[must_use]
    pub const fn intercontinental() -> Self {
        Self {
            delay_us: 90_000,
            jitter_us: 5_000,
            loss_permille: 5,
            rate_bps: Some(20_000_000),
            ..Self::ideal()
        }
    }
}

/// 一次送达。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery<T> {
    /// 到达时间。
    pub at: Time,
    /// 发送节点。
    pub from: usize,
    /// 接收节点。
    pub to: usize,
    /// 内容。
    pub payload: T,
}

/// 仿真网络的计数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetStats {
    /// 进入链路的包数。
    pub sent: u64,
    /// 已送达的包数。
    pub delivered: u64,
    /// 随机丢包或链路中断丢掉的包数。
    pub lost: u64,
    /// 因排队过长被丢掉的包数。
    pub queue_dropped: u64,
    /// 没有对应链路的包数。
    pub unroutable: u64,
}

/// 仿真网络。节点用下标标识。
#[derive(Debug, Clone)]
pub struct Net<T> {
    now: Time,
    rng: Rng,
    links: BTreeMap<(usize, usize), Link>,
    busy_until: BTreeMap<(usize, usize), Time>,
    /// 每个方向最后一个包的到达时间（不乱序的抖动用）。
    last_arrival: BTreeMap<(usize, usize), Time>,
    /// 每个方向的延迟尖峰：(下一次开始, 这一次结束, 这一次多晚)。
    spike: BTreeMap<(usize, usize), (Time, Time, u64)>,
    /// 每个方向上一个包离开瓶颈的时间（省电用）。
    last_depart: BTreeMap<(usize, usize), Time>,
    /// 每个方向当前这一批：(批的时刻, 已经有几个包)。
    batch: BTreeMap<(usize, usize), (Time, u64)>,
    /// 每个方向接收端忙的时段：(下一次开始, 这一次开始, 这一次结束)。
    busy: BTreeMap<(usize, usize), (Time, Time, Time)>,
    queue: BTreeMap<(Time, u64), Delivery<T>>,
    next_id: u64,
    stats: NetStats,
}

impl<T> Net<T> {
    /// 由种子创建。
    #[must_use]
    pub const fn new(seed: u64) -> Self {
        Self {
            now: Time::ZERO,
            rng: Rng::new(seed),
            links: BTreeMap::new(),
            busy_until: BTreeMap::new(),
            last_arrival: BTreeMap::new(),
            spike: BTreeMap::new(),
            last_depart: BTreeMap::new(),
            batch: BTreeMap::new(),
            busy: BTreeMap::new(),
            queue: BTreeMap::new(),
            next_id: 0,
            stats: NetStats {
                sent: 0,
                delivered: 0,
                lost: 0,
                queue_dropped: 0,
                unroutable: 0,
            },
        }
    }

    /// 当前虚拟时间。
    #[must_use]
    pub const fn now(&self) -> Time {
        self.now
    }

    /// 计数。
    #[must_use]
    pub const fn stats(&self) -> NetStats {
        self.stats
    }

    /// 仿真自带的随机数发生器，供场景复用同一个种子。
    pub fn rng(&mut self) -> &mut Rng {
        &mut self.rng
    }

    /// 在 `a` 与 `b` 之间建立双向链路，两个方向特性相同。
    pub fn set_link(&mut self, a: usize, b: usize, link: Link) {
        self.links.insert((a, b), link);
        self.links.insert((b, a), link);
    }

    /// 只设置 `from → to` 一个方向。
    pub fn set_one_way(&mut self, from: usize, to: usize, link: Link) {
        self.links.insert((from, to), link);
    }

    /// 接通或切断 `a` 与 `b` 之间的双向链路。链路不存在时不做任何事。
    pub fn set_up(&mut self, a: usize, b: usize, up: bool) {
        for key in [(a, b), (b, a)] {
            if let Some(link) = self.links.get_mut(&key) {
                link.up = up;
            }
        }
    }

    /// 发送一个 `size_bytes` 字节的包。发送方无法得知它是否会丢。
    pub fn send(&mut self, from: usize, to: usize, size_bytes: usize, payload: T) {
        let Some(link) = self.links.get(&(from, to)).copied() else {
            self.stats.unroutable = self.stats.unroutable.saturating_add(1);
            return;
        };
        self.stats.sent = self.stats.sent.saturating_add(1);
        if !link.up || self.rng.chance(link.loss_permille) {
            self.stats.lost = self.stats.lost.saturating_add(1);
            return;
        }

        let depart = match link.rate_bps {
            Some(rate) if rate > 0 => {
                let busy = self
                    .busy_until
                    .get(&(from, to))
                    .copied()
                    .unwrap_or(self.now);
                let start = busy.max(self.now);
                if start.since(self.now) > link.max_queue_us {
                    self.stats.queue_dropped = self.stats.queue_dropped.saturating_add(1);
                    return;
                }
                let bits = u64::try_from(size_bytes)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(8);
                let tx_us = bits.saturating_mul(1_000_000) / rate;
                let done = start.plus(tx_us);
                self.busy_until.insert((from, to), done);
                done
            }
            Some(_) | None => self.now,
        };

        let jitter = self.rng.below(link.jitter_us.saturating_add(1));
        let mut at = depart.plus(link.delay_us).plus(jitter);
        if let Some(spikes) = link.spikes {
            at = at.plus(self.spike_delay(from, to, depart, spikes));
        }
        if let Some(doze) = link.doze {
            let asleep = self
                .last_depart
                .get(&(from, to))
                .is_none_or(|t| depart.since(*t) >= doze.idle_us);
            if asleep {
                at = at.plus(doze.wake_us / 2 + self.rng.below(doze.wake_us / 2 + 1));
            }
            self.last_depart.insert((from, to), depart);
        }
        if link.batch_us > 0 {
            // 同一批里的包被程序逐个读走，时间戳相差约 1 微秒（真实程序里一帧几十个包的首尾间隔不到 1 微秒，
            // 包串速率算成 Tbps；时间戳完全相同的话间隔是 0，反而被当成测不了）
            let boundary = Time(
                at.micros()
                    .div_ceil(link.batch_us)
                    .saturating_mul(link.batch_us),
            );
            let index = match self.batch.get(&(from, to)) {
                Some((b, n)) if *b == boundary => n.saturating_add(1),
                Some(_) | None => 0,
            };
            self.batch.insert((from, to), (boundary, index));
            at = boundary.plus(index);
        }
        if let Some(busy) = link.busy {
            at = self.busy_read(from, to, at, busy);
        }
        if link.fifo_jitter || link.spikes.is_some() || link.doze.is_some() || link.busy.is_some() {
            if let Some(last) = self.last_arrival.get(&(from, to)) {
                at = at.max(*last);
            }
            self.last_arrival.insert((from, to), at);
        }
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.queue.insert(
            (at, id),
            Delivery {
                at,
                from,
                to,
                payload,
            },
        );
    }

    /// `at` 时刻到的包什么时候被读走：接收端在忙就等它忙完，同一批逐个读（各差 1 微秒）。
    fn busy_read(&mut self, from: usize, to: usize, at: Time, busy: Busy) -> Time {
        let (mut next, mut start, mut end) =
            self.busy
                .get(&(from, to))
                .copied()
                .unwrap_or((Time::ZERO, Time::ZERO, Time::ZERO));
        if next == Time::ZERO && end == Time::ZERO {
            next = at.plus(self.rng.below(busy.every_us.saturating_mul(2)));
        }
        while next <= at {
            start = next;
            end = next.plus(busy.length_us);
            next = next.plus(self.rng.below(busy.every_us.saturating_mul(2)).max(1));
        }
        self.busy.insert((from, to), (next, start, end));
        if at < start || at >= end {
            return at;
        }
        let index = match self.batch.get(&(from, to)) {
            Some((b, n)) if *b == end => n.saturating_add(1),
            Some(_) | None => 0,
        };
        self.batch.insert((from, to), (end, index));
        end.plus(index)
    }

    /// `depart` 时刻离开瓶颈的包碰上的尖峰延迟（不在尖峰里为 0）。
    fn spike_delay(&mut self, from: usize, to: usize, depart: Time, spikes: Spikes) -> u64 {
        let (mut next, mut end, mut extra) =
            self.spike
                .get(&(from, to))
                .copied()
                .unwrap_or((Time::ZERO, Time::ZERO, 0));
        if next == Time::ZERO && end == Time::ZERO {
            next = depart.plus(self.rng.below(spikes.every_us.saturating_mul(2)));
        }
        while next <= depart {
            end = next.plus(spikes.length_us);
            extra = spikes.max_us / 2 + self.rng.below(spikes.max_us / 2 + 1);
            next = next.plus(self.rng.below(spikes.every_us.saturating_mul(2)).max(1));
        }
        self.spike.insert((from, to), (next, end, extra));
        if depart < end {
            extra
        } else {
            0
        }
    }

    /// 下一次送达的时间；队列为空时返回 `None`。
    #[must_use]
    pub fn peek_time(&self) -> Option<Time> {
        self.queue.keys().next().map(|(at, _)| *at)
    }

    /// 取出下一次送达，并把时钟推进到它的到达时间。
    pub fn pop(&mut self) -> Option<Delivery<T>> {
        let (_, delivery) = self.queue.pop_first()?;
        self.now = self.now.max(delivery.at);
        self.stats.delivered = self.stats.delivered.saturating_add(1);
        Some(delivery)
    }

    /// 把时钟推进到 `t`；时钟不会倒退。
    pub fn advance_to(&mut self, t: Time) {
        self.now = self.now.max(t);
    }

    /// 还在途中的包数。
    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.queue.len()
    }

    /// `from → to` 方向瓶颈里现在真实排着多久的数据（微秒）：现在发一个包，要等这么久才轮到它。
    /// 不限速或没有这条链路时为 0。这是仿真才知道的真实值，量尺用它核对发送端的判断
    /// （例如“真实队列很高时有没有被判成应用受限”）。
    #[must_use]
    pub fn backlog_us(&self, from: usize, to: usize) -> u64 {
        self.busy_until
            .get(&(from, to))
            .map_or(0, |busy| busy.since(self.now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic_and_bounded() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        let xs: Vec<u64> = (0..8).map(|_| a.next_u64()).collect();
        let ys: Vec<u64> = (0..8).map(|_| b.next_u64()).collect();
        assert_eq!(xs, ys);
        assert_ne!(Rng::new(43).next_u64(), xs[0]);
        assert_eq!(a.below(0), 0);
        assert!((0..1000).all(|_| a.below(7) < 7));
        assert!((0..100).all(|_| !a.chance(0)));
        assert!((0..100).all(|_| a.chance(1000)));
    }

    #[test]
    fn delivers_in_time_order_with_fixed_delay() {
        let mut net: Net<u32> = Net::new(1);
        net.set_link(
            0,
            1,
            Link {
                delay_us: 1000,
                ..Link::ideal()
            },
        );
        net.send(0, 1, 100, 1);
        net.advance_to(Time(500));
        net.send(1, 0, 100, 2);
        assert_eq!(net.in_flight(), 2);
        assert_eq!(net.peek_time(), Some(Time(1000)));
        let first = net.pop().unwrap();
        assert_eq!(
            (first.payload, first.from, first.to, first.at),
            (1, 0, 1, Time(1000))
        );
        let second = net.pop().unwrap();
        assert_eq!((second.payload, second.at), (2, Time(1500)));
        assert_eq!(net.now(), Time(1500));
        assert!(net.pop().is_none());
        assert_eq!(net.peek_time(), None);
        let s = net.stats();
        assert_eq!((s.sent, s.delivered, s.lost), (2, 2, 0));
    }

    #[test]
    fn clock_never_goes_backwards() {
        let mut net: Net<()> = Net::new(1);
        net.advance_to(Time(100));
        net.advance_to(Time(50));
        assert_eq!(net.now(), Time(100));
    }

    #[test]
    fn unroutable_down_and_lossy_links() {
        let mut net: Net<u8> = Net::new(7);
        net.send(0, 1, 10, 0);
        assert_eq!(net.stats().unroutable, 1);

        net.set_one_way(0, 1, Link::ideal());
        net.send(1, 0, 10, 0); // 反方向没有链路
        assert_eq!(net.stats().unroutable, 2);

        net.set_link(0, 1, Link::ideal());
        net.set_up(0, 1, false);
        net.set_up(5, 6, false); // 不存在的链路：无事发生
        net.send(0, 1, 10, 0);
        assert_eq!(net.stats().lost, 1);
        net.set_up(0, 1, true);

        net.set_link(
            0,
            1,
            Link {
                loss_permille: 500,
                ..Link::ideal()
            },
        );
        for _ in 0..1000 {
            net.send(0, 1, 10, 0);
        }
        let lost = net.stats().lost - 1;
        assert!((400..600).contains(&lost), "lost={lost}");
        assert_eq!(net.rng().below(1), 0);
    }

    #[test]
    fn rate_limit_serialises_and_overflows() {
        let mut net: Net<u32> = Net::new(3);
        // 8000 bps：1000 字节的包要发 1 秒；排队上限 2.5 秒。
        net.set_link(
            0,
            1,
            Link {
                rate_bps: Some(8000),
                max_queue_us: 2_500_000,
                ..Link::ideal()
            },
        );
        for i in 0..4 {
            net.send(0, 1, 1000, i);
        }
        // 前三个分别在 1s、2s、3s 发完；第四个要等 3s 才能开始，超过上限被丢。
        assert_eq!(net.stats().queue_dropped, 1);
        let times: Vec<Time> = std::iter::from_fn(|| net.pop()).map(|d| d.at).collect();
        assert_eq!(
            times,
            vec![Time(1_000_000), Time(2_000_000), Time(3_000_000)]
        );

        // 速率为 0 视为不限速。
        net.set_link(
            0,
            1,
            Link {
                rate_bps: Some(0),
                ..Link::ideal()
            },
        );
        net.send(0, 1, 1000, 9);
        assert_eq!(net.pop().unwrap().at, net.now());
    }

    #[test]
    fn backlog_is_the_true_wait_at_the_bottleneck() {
        let mut net: Net<u32> = Net::new(3);
        assert_eq!(net.backlog_us(0, 1), 0, "没有这条链路");
        net.set_link(0, 1, Link::ideal());
        net.send(0, 1, 1000, 0);
        assert_eq!(net.backlog_us(0, 1), 0, "不限速不排队");
        // 80 kbps：1000 字节的包要发 100 ms
        net.set_link(
            0,
            1,
            Link {
                rate_bps: Some(80_000),
                ..Link::ideal()
            },
        );
        net.send(0, 1, 1000, 1);
        net.send(0, 1, 1000, 2);
        assert_eq!(net.backlog_us(0, 1), 200_000);
        assert_eq!(net.backlog_us(1, 0), 0, "另一个方向没发");
        net.advance_to(Time(150_000));
        assert_eq!(net.backlog_us(0, 1), 50_000, "随时间排掉");
        net.advance_to(Time(500_000));
        assert_eq!(net.backlog_us(0, 1), 0);
    }

    #[test]
    fn jitter_stays_within_bounds_and_same_seed_replays() {
        let run = |seed| {
            let mut net: Net<u32> = Net::new(seed);
            net.set_link(0, 1, Link::wifi());
            for i in 0..200 {
                net.send(0, 1, 1200, i);
            }
            std::iter::from_fn(|| net.pop())
                .map(|d| (d.at, d.payload))
                .collect::<Vec<_>>()
        };
        let a = run(11);
        assert_eq!(a, run(11));
        assert_ne!(a, run(12));
        let wifi = Link::wifi();
        assert!(a.iter().all(|(at, _)| at.micros() >= wifi.delay_us));
    }

    /// 不乱序的抖动：包按发出的顺序到，晚到的包挡住后面的；乱序的抖动会打乱顺序。
    #[test]
    fn fifo_jitter_keeps_the_order() {
        let order = |fifo| {
            let mut net: Net<u32> = Net::new(5);
            net.set_link(
                0,
                1,
                Link {
                    jitter_us: 20_000,
                    fifo_jitter: fifo,
                    rate_bps: Some(30_000_000),
                    ..Link::lan()
                },
            );
            for i in 0..200 {
                net.send(0, 1, 1200, i);
            }
            std::iter::from_fn(|| net.pop())
                .map(|d| d.payload)
                .collect::<Vec<_>>()
        };
        let sorted: Vec<u32> = (0..200).collect();
        assert_eq!(order(true), sorted);
        assert_ne!(order(false), sorted);
    }

    /// 省电：空闲之后的第一个包多等一会儿，紧跟着的包不多等（排在它后面）。
    #[test]
    fn a_dozing_receiver_delays_the_first_packet_after_idle() {
        let mut net: Net<u32> = Net::new(4);
        net.set_link(
            0,
            1,
            Link {
                doze: Some(Doze {
                    idle_us: 50_000,
                    wake_us: 24_000,
                }),
                ..Link::lan()
            },
        );
        // 一个包，空闲 200 ms，再连着三个包（各隔 1 ms）
        net.send(0, 1, 1200, 0);
        let first = net.pop().unwrap();
        net.advance_to(Time(200_000));
        for i in 1..4 {
            net.send(0, 1, 1200, i);
            net.advance_to(Time(200_000 + u64::from(i) * 1_000));
        }
        let rest: Vec<_> = std::iter::from_fn(|| net.pop()).collect();
        let lan = Link::lan().delay_us + Link::lan().jitter_us;
        assert!(first.at.micros() >= 12_000);
        assert!(rest[0].at.since(Time(200_000)) >= 12_000);
        // 后面两个包跟在醒来的那个后面到，自己不再多等一次
        assert!(rest[2].at.since(rest[0].at) <= 2_000 + lan);
        assert!(rest.windows(2).all(|w| w[0].payload < w[1].payload));
    }

    /// 接收端偶尔忙：忙的时候到的包在忙完时一起读走（各差 1 微秒），其余随到随收；顺序不变。
    #[test]
    fn a_busy_receiver_reads_a_burst_at_once_now_and_then() {
        let mut net: Net<u32> = Net::new(6);
        net.set_link(
            0,
            1,
            Link {
                busy: Some(Busy {
                    every_us: 300_000,
                    length_us: 20_000,
                }),
                rate_bps: Some(30_000_000),
                ..Link::lan()
            },
        );
        // 3 秒里每 33 ms 一帧、每帧 30 个包（30 Mbps 下约 10 ms 发完）
        for f in 0..90u32 {
            net.advance_to(Time(u64::from(f) * 33_000));
            for p in 0..30 {
                net.send(0, 1, 1200, f * 100 + p);
            }
        }
        let got: Vec<_> = std::iter::from_fn(|| net.pop()).collect();
        assert!(got.windows(2).all(|w| w[0].payload < w[1].payload));
        // 有的帧整帧在 30 微秒内读完（忙完一起读），大多数帧照常按链路速度到
        let span = |f: u32| {
            let at: Vec<u64> = got
                .iter()
                .filter(|d| d.payload / 100 == f)
                .map(|d| d.at.micros())
                .collect();
            at.last().unwrap() - at.first().unwrap()
        };
        let squeezed = (0..90).filter(|f| span(*f) < 100).count();
        assert!((1..=30).contains(&squeezed), "{squeezed}");
    }

    /// 一批一批地收：到达时间都是批间隔的整数倍，顺序不变。
    #[test]
    fn batched_arrivals_land_on_the_batch_boundaries() {
        let mut net: Net<u32> = Net::new(3);
        net.set_link(
            0,
            1,
            Link {
                batch_us: 8_000,
                rate_bps: Some(30_000_000),
                ..Link::lan()
            },
        );
        for i in 0..50 {
            net.send(0, 1, 1200, i);
        }
        let got: Vec<_> = std::iter::from_fn(|| net.pop()).collect();
        // 同一批里逐个读：比批的时刻晚几微秒
        assert!(got.iter().all(|d| d.at.micros() % 8_000 < 50));
        assert!(got.windows(2).all(|w| w[0].payload < w[1].payload));
        // 50 个包按 30 Mbps 要 16 ms：落在 3 个批次里
        let mut batches: Vec<u64> = got.iter().map(|d| d.at.micros() / 8_000).collect();
        batches.dedup();
        assert_eq!(batches.len(), 3);
    }

    /// 延迟尖峰：一阵包一起晚到，其余时候不晚；不乱序。
    #[test]
    fn spikes_delay_bursts_and_keep_the_order() {
        let mut net: Net<u32> = Net::new(9);
        let spikes = Spikes {
            every_us: 500_000,
            length_us: 40_000,
            max_us: 25_000,
        };
        net.set_link(
            0,
            1,
            Link {
                spikes: Some(spikes),
                ..Link::lan()
            },
        );
        // 10 秒里每 5 ms 发一个包（先全部发完再收，时钟不被收包推着走）
        for i in 0..2000u32 {
            net.advance_to(Time(u64::from(i) * 5_000));
            net.send(0, 1, 1200, i);
        }
        let mut order = Vec::new();
        let mut late = 0;
        while let Some(d) = net.pop() {
            let sent = u64::from(d.payload) * 5_000;
            if d.at.micros() - sent > 12_000 {
                late += 1;
            }
            order.push(d.payload);
        }
        let sorted: Vec<u32> = (0..2000).collect();
        assert_eq!(order, sorted);
        // 平均 0.5 秒一次、一次 40 ms：大约 8% 的包碰上，每次至少晚 12.5 ms
        assert!((80..=260).contains(&late), "{late}");
        assert_eq!(net.stats().delivered, 2000);
    }

    #[test]
    fn presets_are_distinct() {
        let presets = [
            Link::ideal(),
            Link::lan(),
            Link::wifi(),
            Link::cellular(),
            Link::intercontinental(),
        ];
        for (i, a) in presets.iter().enumerate() {
            for b in presets.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }
}
