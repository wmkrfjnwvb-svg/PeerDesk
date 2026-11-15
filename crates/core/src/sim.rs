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

/// 单向链路的特性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Link {
    /// 固定传播延迟（微秒）。
    pub delay_us: u64,
    /// 附加的随机抖动上限（微秒），均匀分布。
    pub jitter_us: u64,
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
        let at = depart.plus(link.delay_us).plus(jitter);
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
