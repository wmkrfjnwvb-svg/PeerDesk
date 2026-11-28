//! 分段路径的验收（`a2-plan.md` 第 8 节“分段路径”；主会话 2026-11-28 定的 7 条加两点）。
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use super::rng::stream_seed;
use super::segments::{Segment, SegmentRoute, Segmented, SegmentedConfig, SEGMENTS};
use super::*;
use crate::sim::{Doze, NetStats, Network, Spikes};
use std::collections::BTreeMap;

type S = Segmented<u32>;

/// 一段“什么毛病都有”的参数：泊松容量、突发丢包、乱序抖动、尖峰、省电。
fn rough() -> DirConfig {
    DirConfig {
        bottleneck: BottleneckConfig {
            capacity: Capacity::Poisson {
                rate_bps: 6_000_000,
                opportunity_bytes: 1500,
            },
            limit: QueueLimit::delay(80_000),
            aqm: Aqm::TailDrop,
        },
        loss: Loss::Burst {
            loss_ppm: 30_000,
            avg_burst_milli: 2_000,
        },
        delay: Delay::Fixed(7_000),
        noise: Noise {
            jitter_us: 4_000,
            fifo_jitter: false,
            spikes: Some(Spikes {
                every_us: 300_000,
                length_us: 20_000,
                max_us: 15_000,
            }),
            doze: Some(Doze {
                idle_us: 40_000,
                wake_us: 10_000,
            }),
            pdv: None,
        },
        ..DirConfig::default()
    }
}

fn only(s: Segment, cfg: PathConfig) -> SegmentedConfig {
    let mut c = SegmentedConfig::default();
    *c.segment_mut(s) = cfg;
    c
}

/// 两个方向交替发 300 个包（每 2 ms 一个，大小不一），每次发之前推进时钟、取走到了的包，最后全部取完。
fn traffic<N: Network<u32>>(n: &mut N) -> (Vec<(u64, usize, usize, u32)>, NetStats) {
    let mut got = Vec::new();
    for i in 0..300u32 {
        let t = Time(u64::from(i) * 2_000);
        n.advance_to(t);
        while n.peek_time().is_some_and(|x| x <= t) {
            if let Some(d) = n.pop() {
                got.push((d.at.micros(), d.from, d.to, d.payload));
            }
        }
        let (from, to) = if i % 3 == 0 { (1, 0) } else { (0, 1) };
        n.send(from, to, 200 + (i as usize * 37) % 1100, i);
    }
    while let Some(d) = n.pop() {
        got.push((d.at.micros(), d.from, d.to, d.payload));
    }
    (got, n.stats())
}

/// 第 1 条：只有一段有参数、其余四段理想时，和单个 Path（同一个种子）逐个送达相同。每一段都试。
#[test]
fn one_rough_segment_is_exactly_that_path() {
    for s in Segment::ALL {
        let cfg = PathConfig {
            forward: rough(),
            reverse: DirConfig {
                delay: Delay::Fixed(3_000),
                ..rough()
            },
        };
        let mut seg: S = Segmented::new(11, only(s, cfg.clone()));
        let name = format!("segment/{}", s.name());
        let mut path: Path<u32> = Path::new(stream_seed(11, Dir::Forward, &name), cfg);
        let (a, sa) = traffic(&mut seg);
        let (b, sb) = traffic(&mut path);
        assert_eq!(a, b, "{s:?}");
        assert_eq!(sa, sb, "{s:?}");
        assert!(sa.lost > 0 && sa.delivered > 200, "{s:?} {sa:?}");
        // 那一段的真值就是那个 Path 的计数，别的段只是转交
        assert_eq!(seg.segment(s).stats(), path.stats());
        for other in Segment::ALL.into_iter().filter(|o| *o != s) {
            let st = seg.segment(other).stats();
            assert_eq!(st.lost + st.queue_dropped + st.outage_dropped, 0);
        }
    }
}

/// 补充 (b)：五段都理想、只有传播延迟时，端到端延迟等于各段传播延迟之和，零丢包。
#[test]
fn ideal_segments_add_up_their_propagation_delays() {
    let mut cfg = SegmentedConfig::default();
    let mut fwd_sum = 0;
    let mut rev_sum = 0;
    for (i, s) in Segment::ALL.into_iter().enumerate() {
        let (f, r) = (1_000 * (i as u64 + 1), 300 * (i as u64 + 2));
        fwd_sum += f;
        rev_sum += r;
        *cfg.segment_mut(s) = PathConfig {
            forward: DirConfig {
                delay: Delay::Fixed(f),
                ..DirConfig::default()
            },
            reverse: DirConfig {
                delay: Delay::Fixed(r),
                ..DirConfig::default()
            },
        };
    }
    let mut n: S = Segmented::new(1, cfg);
    for i in 0..50u32 {
        n.advance_to(Time(u64::from(i) * 500));
        n.send(0, 1, 1200, i);
        n.send(1, 0, 80, 1000 + i);
    }
    let mut count = 0;
    while let Some(d) = n.pop() {
        let sent = u64::from(d.payload % 1000) * 500;
        let want = if d.payload < 1000 { fwd_sum } else { rev_sum };
        assert_eq!(d.at.micros() - sent, want, "{d:?}");
        count += 1;
    }
    assert_eq!(count, 100);
    let st = n.stats();
    assert_eq!(
        (st.sent, st.delivered, st.lost, st.queue_dropped),
        (100, 100, 0, 0)
    );
    assert_eq!(n.in_flight(), 0);
}

/// 第 3 条：探测往返等于对应几段的传播延迟之和（加排队）；从 B 发对称；探测另数。
#[test]
fn probes_turn_around_at_the_right_segment() {
    let mut cfg = SegmentedConfig::default();
    for (i, s) in Segment::ALL.into_iter().enumerate() {
        *cfg.segment_mut(s) = PathConfig {
            forward: DirConfig {
                delay: Delay::Fixed(1_000 << i),
                ..DirConfig::default()
            },
            reverse: DirConfig {
                delay: Delay::Fixed(10_000 << i),
                ..DirConfig::default()
            },
        };
    }
    // 第 k 段：正向 1000·2^k，反向 10000·2^k
    let one_way = |k: usize| (1_000u64 << k) + (10_000u64 << k);
    for depth in 1..=SEGMENTS {
        let mut n: S = Segmented::new(1, cfg.clone());
        n.send_probe(0, depth, 64, 1);
        n.send_probe(1, depth, 64, 2);
        let a = n.pop().unwrap();
        let b = n.pop().unwrap();
        let (a, b) = if a.payload == 1 { (a, b) } else { (b, a) };
        let from_a: u64 = (0..depth).map(one_way).sum();
        let from_b: u64 = (SEGMENTS - depth..SEGMENTS).map(one_way).sum();
        assert_eq!(
            (a.at.micros(), a.from, a.to),
            (from_a, 0, 0),
            "深度 {depth}"
        );
        assert_eq!(
            (b.at.micros(), b.from, b.to),
            (from_b, 1, 1),
            "深度 {depth}"
        );
        assert_eq!(n.probe_stats().sent, 2);
        assert_eq!(n.probe_stats().delivered, 2);
        assert_eq!(n.stats(), NetStats::default(), "探测不算主流量");
    }
    // 不对的端点、深度
    let mut n: S = Segmented::new(1, cfg);
    n.send_probe(2, 1, 64, 0);
    n.send_probe(0, 0, 64, 0);
    n.send_probe(0, SEGMENTS + 1, 64, 0);
    n.send(0, 0, 10, 0);
    assert_eq!(n.stats().unroutable, 4);
    assert!(n.pop().is_none());
}

/// 探测也排队：到第一跳的探测在 A 出口段排着的数据后面。
#[test]
fn probes_wait_in_the_queue_they_cross() {
    let egress = DirConfig {
        bottleneck: BottleneckConfig {
            capacity: Capacity::Constant {
                rate_bps: 10_000_000,
            },
            ..BottleneckConfig::default()
        },
        ..DirConfig::default()
    };
    let mut n: S = Segmented::new(
        1,
        only(
            Segment::AEgress,
            PathConfig {
                forward: egress,
                reverse: DirConfig::default(),
            },
        ),
    );
    // 20 个 1250 字节的包：每个 1 ms，排 20 ms
    for i in 0..20 {
        n.send(0, 1, 1250, i);
    }
    assert_eq!(n.backlog_us(0, 1), 20_000);
    assert_eq!(n.backlog_us(1, 0), 0);
    assert_eq!(n.backlog_us(0, 0), 0);
    n.send_probe(0, 1, 1250, 100); // 到网关：不经过出口段
    n.send_probe(0, 2, 1250, 200); // 到第一跳：排在 20 个包后面
    let mut at = BTreeMap::new();
    while let Some(d) = n.pop() {
        at.insert(d.payload, d.at.micros());
    }
    assert_eq!(at[&100], 0);
    assert_eq!(at[&200], 21_000);
    assert_eq!(at[&19], 20_000);
}

/// 局域网段的乱序如实带进下一段；结果逐字节可复现。
#[test]
fn reordering_in_the_lan_reaches_the_next_bottleneck() {
    let lan = DirConfig {
        noise: Noise {
            jitter_us: 20_000,
            ..Noise::default()
        },
        ..DirConfig::default()
    };
    let egress = DirConfig {
        bottleneck: BottleneckConfig {
            capacity: Capacity::Constant {
                rate_bps: 20_000_000,
            },
            ..BottleneckConfig::default()
        },
        ..DirConfig::default()
    };
    let mut cfg = SegmentedConfig::default();
    cfg.segment_mut(Segment::ALan).forward = lan;
    cfg.segment_mut(Segment::AEgress).forward = egress;
    let run = || {
        let mut n: S = Segmented::new(5, cfg.clone());
        let (got, st) = traffic(&mut n);
        (got, st)
    };
    let (a, sa) = run();
    let (b, sb) = run();
    assert_eq!(a, b);
    assert_eq!(sa, sb);
    let fwd: Vec<u32> = a.iter().filter(|x| x.1 == 0).map(|x| x.3).collect();
    assert!(
        fwd.windows(2).any(|w| w[0] > w[1]),
        "20 ms 的乱序抖动应当让包换了先后"
    );
}

/// 第 4 条的机制：B 出口下行挂长 TCP 时，第 3 段（B 出口）排队上升，别的段不变；主流量照样走完。
#[test]
fn cross_traffic_in_the_middle_segment_queues_only_there() {
    let egress = |rate_bps| DirConfig {
        bottleneck: BottleneckConfig {
            capacity: Capacity::Constant { rate_bps },
            limit: QueueLimit::delay(250_000),
            aqm: Aqm::TailDrop,
        },
        delay: Delay::Fixed(2_000),
        ..DirConfig::default()
    };
    let mut cfg = SegmentedConfig::default();
    *cfg.segment_mut(Segment::BEgress) = PathConfig {
        forward: egress(20_000_000),
        reverse: egress(5_000_000),
    };
    *cfg.segment_mut(Segment::Public) = PathConfig::symmetric(DirConfig {
        delay: Delay::Fixed(20_000),
        ..DirConfig::default()
    });
    let mut n: S = Segmented::new(3, cfg);
    let id = n.add_cross(
        Segment::BEgress,
        Dir::Forward,
        CrossSpec::Tcp {
            cfg: TcpConfig::default(),
            start: Time(500_000),
            bytes: None,
        },
    );
    let mut max_q = [0u64; SEGMENTS];
    let mut got = 0;
    for i in 0..2_000u32 {
        n.advance_to(Time(u64::from(i) * 1_000));
        while n
            .peek_time()
            .is_some_and(|t| t <= Time(u64::from(i) * 1_000))
        {
            got += usize::from(n.pop().is_some());
        }
        n.send(0, 1, 1000, i);
        for (k, s) in Segment::ALL.into_iter().enumerate() {
            max_q[k] = max_q[k].max(n.segment(s).backlog_us(0, 1));
        }
    }
    while n.pop().is_some() {
        got += 1;
    }
    assert!(max_q[3] > 50_000, "TCP 填 B 出口的队列：{max_q:?}");
    assert_eq!([max_q[0], max_q[1], max_q[2], max_q[4]], [0; 4]);
    let cross = n.segment(Segment::BEgress).cross_stats(id);
    assert!(cross.delivered_bytes > 1_000_000, "{cross:?}");
    let st = n.stats();
    assert_eq!(st.delivered, u64::try_from(got).unwrap());
    assert_eq!(st.sent, 2_000);
    assert_eq!(st.delivered + st.lost + st.queue_dropped, 2_000);
}

/// 第 5 条：换路按段指定，同一次换几段只发一次事件，时刻是恢复的时刻；纯中断不发；中断期间的包算 lost。
#[test]
fn one_route_change_across_two_segments_is_one_event() {
    let mut n: S = Segmented::new(1, SegmentedConfig::default());
    let slow = PathConfig::symmetric(DirConfig {
        delay: Delay::Fixed(30_000),
        ..DirConfig::default()
    });
    n.schedule_route_change(SegmentRoute {
        at: Time(100_000),
        down_us: 50_000,
        down: vec![Segment::ALan],
        new: vec![
            (Segment::ALan, slow.clone()),
            (Segment::AEgress, slow.clone()),
        ],
    });
    n.schedule_route_change(SegmentRoute {
        at: Time(300_000),
        down_us: 20_000,
        down: vec![Segment::BLan],
        new: Vec::new(),
    });
    // 同一个恢复时刻再来一次（只换参数），事件不重复
    n.schedule_route_change(SegmentRoute {
        at: Time(150_000),
        down_us: 0,
        down: Vec::new(),
        new: vec![(Segment::Public, slow)],
    });
    assert_eq!(n.next_event_time(), Some(Time(150_000)));
    let mut events = Vec::new();
    for i in 0..400u64 {
        n.advance_to(Time(i * 1_000));
        while let Some(e) = n.pop_event() {
            events.push(e.at());
        }
        n.send(0, 1, 100, 0);
    }
    assert_eq!(events, vec![Time(150_000)]);
    assert_eq!(n.next_event_time(), None);
    // 100–150 ms A 局域网断（50 个包），300–320 ms B 局域网断（20 个包）
    assert_eq!(n.stats().lost, 70);
    // 恢复后 A 局域网、A 出口、公网都换成 30 ms
    for s in [Segment::ALan, Segment::AEgress, Segment::Public] {
        assert_eq!(
            n.segment(s).dir(Dir::Forward).config().delay,
            Delay::Fixed(30_000)
        );
    }
    assert_eq!(
        n.segment(Segment::BLan).dir(Dir::Forward).config().delay,
        Delay::Fixed(0)
    );
}

/// 立即换参数、手动断开、peek 报段间转交的时刻。
#[test]
fn manual_controls_and_transfer_times() {
    let mut cfg = SegmentedConfig::default();
    cfg.segment_mut(Segment::Public).forward.delay = Delay::Fixed(5_000);
    cfg.segment_mut(Segment::BLan).forward.delay = Delay::Fixed(1_000);
    let mut n: S = Segmented::new(1, cfg);
    n.send(0, 1, 100, 7);
    // 不花时间的段在发的时候就转交完了；下一个是公网段的转交（5 ms），推进过去还没有送达
    assert_eq!(n.peek_time(), Some(Time(5_000)));
    n.advance_to(Time(5_000));
    assert_eq!(n.now(), Time(5_000));
    assert_eq!(n.peek_time(), Some(Time(6_000)));
    assert_eq!(n.in_flight(), 1);
    let d = n.pop().unwrap();
    assert_eq!((d.at, d.payload), (Time(6_000), 7));
    n.set_dir(
        Segment::Public,
        Dir::Forward,
        DirConfig {
            delay: Delay::Fixed(9_000),
            ..DirConfig::default()
        },
    );
    n.send(0, 1, 100, 8);
    assert_eq!(n.pop().unwrap().at, Time(6_000 + 10_000));
    n.set_up(Segment::BEgress, false);
    n.send(1, 0, 100, 9);
    assert!(n.pop().is_none());
    assert_eq!(n.stats().lost, 1);
    assert_eq!(
        n.segment_stats()[Segment::BEgress.index()].outage_dropped,
        1
    );
    // 时钟不倒退
    n.advance_to(Time(1));
    assert_eq!(n.now(), Time(16_000));
    assert_eq!(n.main_stats().sent, 3);
}

#[test]
fn segment_names_and_order() {
    let names: Vec<_> = Segment::ALL.iter().map(|s| s.name()).collect();
    assert_eq!(names, ["a-lan", "a-egress", "public", "b-egress", "b-lan"]);
    for (i, s) in Segment::ALL.into_iter().enumerate() {
        assert_eq!(s.index(), i);
        let mut c = SegmentedConfig::default();
        c.segment_mut(s).forward.delay = Delay::Fixed(i as u64 + 1);
        assert_eq!(c.segment(s).forward.delay, Delay::Fixed(i as u64 + 1));
        assert_eq!(c.segments[i].forward.delay, Delay::Fixed(i as u64 + 1));
    }
}
