//! 交叉流量的验收测试（`a2-plan.md` 第 4 节）：定速背景流、一阵一阵的短连接、包一级的 TCP NewReno。
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]

use super::testkit::{check, SEEDS};
use super::*;

type P = Path<u32>;

const PKT: usize = 1250;

fn cap(rate_bps: u64) -> Capacity {
    Capacity::Constant { rate_bps }
}

fn queue(capacity: Capacity, limit: QueueLimit, aqm: Aqm) -> DirConfig {
    DirConfig {
        bottleneck: BottleneckConfig {
            capacity,
            limit,
            aqm,
        },
        delay: Delay::Fixed(20_000),
        ..DirConfig::default()
    }
}

/// 往返 40 ms：两个方向各 20 ms，正向瓶颈 `fwd`，反向不限速。
fn path(seed: u64, fwd: DirConfig) -> P {
    let reverse = DirConfig {
        delay: Delay::Fixed(20_000),
        ..DirConfig::default()
    };
    P::new(
        seed,
        PathConfig {
            forward: fwd,
            reverse,
        },
    )
}

/// 每个种子一个开始时刻的偏移（0 到 `max_us`），让不同种子的 TCP 不同步。
fn offset(seed: u64, max_us: u64) -> Time {
    Time(LayerRng::new(seed, Dir::Forward, "test/offset").below(max_us))
}

fn tcp(start: Time) -> CrossSpec {
    CrossSpec::Tcp {
        cfg: TcpConfig::default(),
        start,
        bytes: None,
    }
}

/// 用带宽 `rate`、`secs` 秒的 goodput（接收端按顺序收到的字节）算利用率：只看 `from_s` 秒之后。
fn goodput_bps(p: &mut P, id: usize, from_s: u64, to_s: u64) -> f64 {
    p.advance_to(Time(from_s * 1_000_000));
    let a = p.cross_stats(id).delivered_bytes;
    p.advance_to(Time(to_s * 1_000_000));
    let b = p.cross_stats(id).delivered_bytes;
    (b - a) as f64 * 8.0 / (to_s - from_s) as f64
}

// ---------------------------------------------------------------- 定速背景流

/// 定速背景流：注入的速率等于设定的速率（误差一个包）；开始、结束时刻对；不排队时在瓶颈里不等。
#[test]
fn a_constant_rate_flow_injects_exactly_its_rate_between_start_and_stop() {
    let mut p = path(
        1,
        queue(cap(10_000_000), QueueLimit::delay(100_000), Aqm::TailDrop),
    );
    // 4 Mbps、1000 字节：每 2 ms 一个，1 s 开始、11 s 结束
    let id = p.add_cross(
        Dir::Forward,
        CrossSpec::Cbr {
            rate_bps: 4_000_000,
            packet_bytes: 1000,
            start: Time(1_000_000),
            stop: Some(Time(11_000_000)),
        },
    );
    p.advance_to(Time(999_999));
    assert_eq!(p.cross_stats(id).sent_packets, 0, "开始之前不发");
    p.advance_to(Time(1_000_000));
    assert_eq!(
        p.cross_stats(id).sent_packets,
        1,
        "恰好在开始的时刻发第一个"
    );
    p.advance_to(Time(20_000_000));
    let st = p.cross_stats(id);
    assert_eq!(st.sent_bytes, 4_000_000 / 8 * 10, "10 秒 × 4 Mbps");
    assert_eq!(
        (st.queue_dropped, st.wait_sum_us),
        (0, 0),
        "没有主流量，不排队"
    );
    assert_eq!(
        p.dir_stats(Dir::Forward).cross_sent,
        0,
        "走源的路径，不走 inject_cross 的计数"
    );
    // 速率 0 不发；一直发的（没有结束时刻）；开始早于现在的按现在算
    let mut q = path(1, DirConfig::default());
    q.advance_to(Time(500));
    let none = q.add_cross(
        Dir::Reverse,
        CrossSpec::Cbr {
            rate_bps: 0,
            packet_bytes: 100,
            start: Time(0),
            stop: None,
        },
    );
    let forever = q.add_cross(
        Dir::Reverse,
        CrossSpec::Cbr {
            rate_bps: 800_000,
            packet_bytes: 100,
            start: Time(0),
            stop: None,
        },
    );
    q.advance_to(Time(1_000_500));
    assert_eq!(q.cross_stats(none).sent_packets, 0);
    assert_eq!(
        q.cross_stats(forever).sent_packets,
        1001,
        "每 1 ms 一个，从 0.5 ms 起"
    );
    assert_eq!(q.cross_stats(99), CrossStats::default());
    assert!(q.cross_sender(forever).is_none() && q.cross_bursts(forever).is_empty());
}

/// 定速背景流占掉的容量等于它的速率：10 Mbps 的瓶颈里，主流量（泊松到达的用户包）offered 5 Mbps + 背景流 4 Mbps（合计不超载）：
/// 主流量一个不丢、吞吐等于 offered，瓶颈忙的时间占 90%（排队时间随之升高：M/D/1 在 ρ=0.9 的方向）。
#[test]
fn a_constant_rate_flow_takes_its_share_of_the_capacity_from_user_traffic() {
    let mut p = path(
        1,
        queue(cap(10_000_000), QueueLimit::delay(500_000), Aqm::TailDrop),
    );
    let cbr = p.add_cross(
        Dir::Forward,
        CrossSpec::Cbr {
            rate_bps: 4_000_000,
            packet_bytes: PKT,
            start: Time(0),
            stop: None,
        },
    );
    // 主流量 5 Mbps：每 2 ms 一个 1250 字节的包，30 秒
    let mut sent_at = Vec::new();
    for i in 0..15_000u32 {
        let t = Time(u64::from(i) * 2_000 + 500);
        p.advance_to(t);
        p.send(0, 1, PKT, i);
        sent_at.push(t);
    }
    let got: Vec<_> = std::iter::from_fn(|| p.pop()).collect();
    assert_eq!(got.len(), 15_000, "不超载，一个都不丢");
    // 没有背景流时每个包只等自己的发送时间 1 ms；有了背景流，偶尔撞上它的包要多等（最多一个包 + 抖动）
    let max_wait = got
        .iter()
        .map(|d| d.at.since(sent_at[d.payload as usize]) - 20_000 - 1_000)
        .max()
        .unwrap();
    assert!(
        max_wait <= 1_000,
        "两路定速错开发，最多多等一个包的发送时间：{max_wait}"
    );
    let c = p.cross_stats(cbr);
    assert_eq!(c.queue_dropped, 0);
    assert!(p.backlog_us(0, 1) < 2_000);
}

/// 过载时先到先服务地分容量：主流量（泊松到达，offered 8 Mbps）和背景流（4 Mbps）合计 12 Mbps 挤 10 Mbps 的瓶颈。
/// 链路一直忙：主流量 + 背景流实际过去的 = 容量。份额不是正好按 offered 的比例：泊松到达的包撞上队满的概率是时间平均的满队概率，
/// 定速的包错开着到，撞上的少一些（实测主流量 63.5%，offered 比例 66.7%）。
#[test]
fn under_overload_the_link_stays_busy_and_the_capacity_is_shared_between_flows() {
    let mut totals = Vec::new();
    let mut shares = Vec::new();
    for seed in SEEDS {
        let mut p = path(
            seed,
            queue(cap(10_000_000), QueueLimit::delay(50_000), Aqm::TailDrop),
        );
        let cbr = p.add_cross(
            Dir::Forward,
            CrossSpec::Cbr {
                rate_bps: 4_000_000,
                packet_bytes: PKT,
                start: Time(0),
                stop: None,
            },
        );
        // 主流量 8 Mbps，泊松到达，60 秒
        let mut rng = LayerRng::new(seed, Dir::Forward, "test/main");
        let (mut t_ns, mut n) = (0u64, 0u32);
        while t_ns < 60_000_000_000 {
            t_ns += rng.exp_ns(1_250_000);
            p.advance_to(Time(t_ns / 1000));
            p.send(0, 1, PKT, n);
            n += 1;
        }
        let main = p.dir_stats(Dir::Forward);
        let main_bytes = (main.sent - main.queue_dropped) as f64 * PKT as f64;
        let c = p.cross_stats(cbr);
        let cbr_bytes = (c.sent_packets - c.queue_dropped) as f64 * PKT as f64;
        totals.push((main_bytes + cbr_bytes) * 8.0 / 60.0 / 10_000_000.0);
        shares.push(main_bytes / (main_bytes + cbr_bytes));
    }
    let total = totals.iter().sum::<f64>() / 12.0;
    let share = shares.iter().sum::<f64>() / 12.0;
    println!("[交叉流量] 8 Mbps 主流量 + 4 Mbps 定速背景流挤 10 Mbps：过去的合计 / 容量 = {total:.4}；主流量占的份额 {share:.3}（offered 的比例 0.667）");
    // 合计略高于 1：瓶颈里还排着最多 50 ms 的包，算进了“收下的”（50 ms / 60 s = 0.083%）
    assert!(
        totals.iter().all(|t| (0.999..=1.002).contains(t)),
        "{totals:?}"
    );
    // 份额不是正好按 offered 的比例：泊松到达的包撞上队满的概率是时间平均的满队概率，定速的包错开着到，撞上的少一些
    assert!((0.55..0.75).contains(&share), "{share}");
}

/// 一条 TCP 和定速背景流共享瓶颈：链路一直忙，TCP 拿走背景流之外的（背景流是不反应的，丢尾也会丢它的包，所以 TCP 拿到的
/// 可以比 容量 − 背景流速率 多一点）：TCP goodput + 背景流实际过去的 = 容量。
#[test]
fn tcp_fills_whatever_a_constant_rate_flow_leaves() {
    let mut totals = Vec::new();
    let mut tcp_rates = Vec::new();
    for seed in SEEDS {
        let mut p = path(
            seed,
            queue(cap(10_000_000), QueueLimit::delay(100_000), Aqm::TailDrop),
        );
        let cbr = p.add_cross(
            Dir::Forward,
            CrossSpec::Cbr {
                rate_bps: 4_000_000,
                packet_bytes: PKT,
                start: Time(0),
                stop: None,
            },
        );
        let t = p.add_cross(Dir::Forward, tcp(offset(seed, 200_000)));
        p.advance_to(Time(30_000_000));
        let c0 = p.cross_stats(cbr);
        let g = goodput_bps(&mut p, t, 30, 90);
        let c1 = p.cross_stats(cbr);
        let served = ((c1.sent_packets - c1.queue_dropped) - (c0.sent_packets - c0.queue_dropped))
            as f64
            * PKT as f64
            * 8.0
            / 60.0;
        totals.push((g + served) / 10_000_000.0);
        tcp_rates.push(g / 6_000_000.0);
    }
    println!(
        "[交叉流量] TCP + 4 Mbps 定速，10 Mbps 瓶颈（30–90 s）：(TCP goodput + 背景流过去的) / 容量 = {:.4}（最小 {:.4}）；TCP / (容量 − 4 Mbps) = {:.3}",
        totals.iter().sum::<f64>() / 12.0,
        totals.iter().copied().fold(9.0, f64::min),
        tcp_rates.iter().sum::<f64>() / 12.0
    );
    assert!(
        totals.iter().all(|t| (0.97..=1.01).contains(t)),
        "{totals:?}"
    );
    assert!(tcp_rates.iter().all(|r| *r > 0.9), "{tcp_rates:?}");
}

// ---------------------------------------------------------------- TCP NewReno

/// 单独一条 TCP 能占满瓶颈：队列上限正好一个带宽时延积（40 ms × 10 Mbps = 50 000 字节）时，利用率 ≥ 90%；
/// 队列在一个带宽时延积以内摆动，会丢包（丢尾），会超时很少。
#[test]
fn one_tcp_flow_fills_the_bottleneck_with_one_bdp_of_buffer() {
    let mut utils = Vec::new();
    let mut waits = Vec::new();
    for seed in SEEDS {
        let mut p = path(
            seed,
            queue(cap(10_000_000), QueueLimit::bytes(50_000), Aqm::TailDrop),
        );
        let t = p.add_cross(Dir::Forward, tcp(offset(seed, 200_000)));
        utils.push(goodput_bps(&mut p, t, 30, 90) / 10_000_000.0);
        waits.push(p.cross_stats(t).mean_wait_us());
        let st = p.cross_stats(t);
        assert!(st.queue_dropped > 0, "丢尾队列要被填满：{st:?}");
    }
    let mean = utils.iter().sum::<f64>() / 12.0;
    println!(
        "[TCP] 单条、队列 1 BDP：利用率 {mean:.3}（最小 {:.3}），平均排队 {:.0} us",
        utils.iter().copied().fold(9.0, f64::min),
        waits.iter().sum::<f64>() / 12.0
    );
    assert!(utils.iter().all(|u| *u >= 0.9), "{utils:?}");
    assert!(utils.iter().all(|u| *u <= 1.0));
}

/// 独立丢包的链路上，吞吐和 Mathis 公式 MSS/RTT·√(3/(2p)) 在 2 倍以内（比值落在 0.5 到 2 之间）。
/// 瓶颈 100 Mbps，窗口远小于带宽时延积，丢的都是链路上随机丢的；往返 40 ms（加一个包的发送时间）。
#[test]
fn tcp_throughput_on_a_lossy_link_is_within_a_factor_two_of_the_mathis_formula() {
    for (p_ppm, name) in [(5_000u32, "0.5%"), (10_000, "1%"), (20_000, "2%")] {
        let p = f64::from(p_ppm) / 1e6;
        let rtt = 0.040 + 8.0 * PKT as f64 / 100e6; // 传播 40 ms + 一个包在 100 Mbps 上的发送时间
        let mathis = PKT as f64 * 8.0 / rtt * (3.0 / (2.0 * p)).sqrt();
        let mut ratios = Vec::new();
        for seed in SEEDS {
            let mut fwd = queue(cap(100_000_000), QueueLimit::delay(100_000), Aqm::TailDrop);
            fwd.loss = Loss::Independent { ppm: p_ppm };
            let mut pa = path(seed, fwd);
            let t = pa.add_cross(Dir::Forward, tcp(Time(0)));
            ratios.push(goodput_bps(&mut pa, t, 20, 320) / mathis);
        }
        let mean = ratios.iter().sum::<f64>() / 12.0;
        println!(
            "[TCP] 丢包 {name}：Mathis 公式 {:.2} Mbps；实测 / 公式 = {mean:.3}（范围 {:.3}–{:.3}）",
            mathis / 1e6,
            ratios.iter().copied().fold(9.0, f64::min),
            ratios.iter().copied().fold(0.0, f64::max)
        );
        assert!(
            ratios.iter().all(|r| (0.5..=2.0).contains(r)),
            "{name}: {ratios:?}"
        );
    }
}

/// 两条同往返时间的 TCP 共享瓶颈：吞吐比在 0.5 到 2 之间；加起来吃满瓶颈。
#[test]
fn two_tcp_flows_with_the_same_rtt_share_the_bottleneck_fairly() {
    let mut ratios = Vec::new();
    let mut totals = Vec::new();
    for seed in SEEDS {
        let mut p = path(
            seed,
            queue(cap(10_000_000), QueueLimit::bytes(100_000), Aqm::TailDrop),
        );
        let a = p.add_cross(Dir::Forward, tcp(Time(0)));
        let b = p.add_cross(Dir::Forward, tcp(offset(seed, 500_000)));
        p.advance_to(Time(30_000_000));
        let (a0, b0) = (
            p.cross_stats(a).delivered_bytes,
            p.cross_stats(b).delivered_bytes,
        );
        p.advance_to(Time(150_000_000));
        let (ga, gb) = (
            (p.cross_stats(a).delivered_bytes - a0) as f64,
            (p.cross_stats(b).delivered_bytes - b0) as f64,
        );
        ratios.push(ga / gb);
        totals.push((ga + gb) * 8.0 / 120.0 / 10_000_000.0);
    }
    println!(
        "[TCP] 两条同 RTT：吞吐比范围 {:.3}–{:.3}；合计利用率 {:.3}",
        ratios.iter().copied().fold(9.0, f64::min),
        ratios.iter().copied().fold(0.0, f64::max),
        totals.iter().sum::<f64>() / 12.0
    );
    assert!(ratios.iter().all(|r| (0.5..=2.0).contains(r)), "{ratios:?}");
    assert!(totals.iter().all(|t| *t > 0.9), "{totals:?}");
}

/// 换成 CoDel 队列后，TCP 在瓶颈里的排队时长明显下降（同一条 TCP、同一个瓶颈，队列从丢尾 200 ms 换成 CoDel），
/// 吞吐代价见数。（取代 A2-1 里那个 Reno 式的发送端。）
/// 利用率只看 60–150 s：没有 SACK 的 NewReno 一个往返只能补一个洞，慢启动在 200 ms 深的队列里一下冲出去丢几百个包，
/// 要几十个往返（每个往返含 200 ms 的排队）才补完，期间接收端按顺序收到的字节几乎是零，补完时一下放出来
/// （实测 0–35 s 的 goodput 约 0.1 Mbps，35 s 那一个 5 秒窗口 66 Mbps）；这是 NewReno 本身的行为，不是仿真的毛病。
#[test]
fn codel_cuts_the_queueing_delay_of_a_tcp_flow() {
    let mut rows = Vec::new();
    for seed in SEEDS {
        let one = |aqm: Aqm, limit: QueueLimit| {
            let mut p = path(seed, queue(cap(10_000_000), limit, aqm));
            let t = p.add_cross(Dir::Forward, tcp(offset(seed, 200_000)));
            let g = goodput_bps(&mut p, t, 60, 150) / 10_000_000.0;
            (p.cross_stats(t).mean_wait_us(), g)
        };
        let (cw, cu) = one(
            Aqm::codel(),
            QueueLimit {
                max_delay_us: None,
                max_bytes: None,
            },
        );
        let (tw, tu) = one(Aqm::TailDrop, QueueLimit::delay(200_000));
        rows.push([cw, cu, tw, tu]);
    }
    let avg = |i: usize| rows.iter().map(|r| r[i]).sum::<f64>() / 12.0;
    println!(
        "[TCP] 队列换成 CoDel：平均排队 {:.0} us → {:.0} us，利用率 {:.3} → {:.3}（丢尾 200 ms / CoDel 目标 5000 us）",
        avg(2), avg(0), avg(3), avg(1)
    );
    assert!(
        rows.iter().all(|r| r[0] * 4.0 < r[2]),
        "CoDel 的排队要明显低于丢尾：{rows:?}"
    );
    // A2-1 那个 Reno 式发送端的测试给出的结论也在这里：CoDel 下的平均排队不超过目标 5 ms
    assert!(avg(0) <= 5_000.0, "{}", avg(0));
    assert!(avg(1) > 0.8, "CoDel 不能把链路饿着：{}", avg(1));
}

/// 确认走反向路径：占反向的容量，会被丢掉（瓶颈丢尾、丢包层）。TCP 挂在反向（数据走反向、确认走正向）也一样对称。
#[test]
fn acks_use_the_reverse_capacity_and_can_be_lost() {
    // 反向只有 32 kbps，每秒只能过 100 个确认包（40 字节）；数据包每个都回确认，确认大量被丢
    let fwd = queue(cap(10_000_000), QueueLimit::delay(100_000), Aqm::TailDrop);
    let rev = DirConfig {
        bottleneck: BottleneckConfig {
            capacity: cap(32_000),
            limit: QueueLimit::delay(50_000),
            aqm: Aqm::TailDrop,
        },
        delay: Delay::Fixed(20_000),
        ..DirConfig::default()
    };
    let mut p = P::new(
        1,
        PathConfig {
            forward: fwd,
            reverse: rev,
        },
    );
    let t = p.add_cross(Dir::Forward, tcp(Time(0)));
    let g = goodput_bps(&mut p, t, 10, 60);
    let st = p.cross_stats(t);
    println!(
        "[TCP] 反向只有 32 kbps：goodput {:.2} Mbps；确认被反向瓶颈丢了 {}，反向排队 {} us",
        g / 1e6,
        st.ack_queue_dropped,
        p.backlog_us(1, 0)
    );
    // 累计确认：确认丢得再多，后面的确认也覆盖前面的，吞吐不受影响（只是发送更成一阵一阵的）
    assert!(
        st.ack_queue_dropped > 1000,
        "反向的容量被确认占满，确认丢尾：{st:?}"
    );
    assert!(p.backlog_us(1, 0) > 0);
    // 反向丢包层丢确认
    let rev = DirConfig {
        loss: Loss::Independent { ppm: 100_000 },
        delay: Delay::Fixed(20_000),
        ..DirConfig::default()
    };
    let mut q = P::new(
        1,
        PathConfig {
            forward: queue(cap(10_000_000), QueueLimit::delay(100_000), Aqm::TailDrop),
            reverse: rev,
        },
    );
    let t = q.add_cross(Dir::Forward, tcp(Time(0)));
    q.advance_to(Time(20_000_000));
    assert!(q.cross_stats(t).ack_lost > 1000);
    // 数据走反向
    let mut r = P::new(
        1,
        PathConfig {
            forward: DirConfig::default(),
            reverse: queue(cap(10_000_000), QueueLimit::delay(100_000), Aqm::TailDrop),
        },
    );
    let t = r.add_cross(Dir::Reverse, tcp(Time(0)));
    assert!(goodput_bps(&mut r, t, 5, 25) > 8e6);
}

/// 有限大小的 TCP 连接发完就停：goodput 字节 = 向上取整到 MSS 的大小；窗口、RTO 可以看。
#[test]
fn a_finite_tcp_transfer_stops_when_done() {
    let mut p = path(
        1,
        queue(cap(10_000_000), QueueLimit::delay(100_000), Aqm::TailDrop),
    );
    let t = p.add_cross(
        Dir::Forward,
        CrossSpec::Tcp {
            cfg: TcpConfig::default(),
            start: Time(100_000),
            bytes: Some(100_000),
        },
    );
    p.advance_to(Time(30_000_000));
    let st = p.cross_stats(t);
    assert_eq!(st.delivered_bytes, 100_000);
    assert_eq!(st.sent_packets, 80);
    assert!(p.cross_sender(t).unwrap().is_done());
    assert!(p.cross_sender(t).unwrap().next_timer().is_none());
}

// ---------------------------------------------------------------- 一阵一阵的短连接

fn bursts(connections: u32, start_on: bool) -> CrossSpec {
    CrossSpec::Bursts {
        cfg: TcpConfig::default(),
        connections,
        min_bytes: 30_000,
        max_bytes: 50_000,
        mean_off_us: 10_000_000,
        start_on,
        start: Time(0),
    }
}

/// RFC 8868 的短连接：空闲时间服从均值 10 s 的指数分布（均值 10 s、变异系数 1），每条连接 30–50 KB 均匀分布
/// （均值 40 000、标准差/均值 = 20000/√12/40000 = 0.1443）；每阵 30 条连接。
#[test]
fn short_connection_bursts_have_the_rfc_8868_off_time_and_size_distributions() {
    check(
        [
            "短连接：空闲时间均值（秒）",
            "短连接：空闲时间变异系数",
            "短连接：文件大小均值（字节）",
            "短连接：文件大小 标准差/均值",
        ],
        [10.0, 1.0, 40_000.0, 20_000.0 / 12f64.sqrt() / 40_000.0],
        |seed| {
            // 瓶颈 1 Gbps：连接很快完成，只看时间和大小的分布
            let mut p = path(
                seed,
                queue(
                    cap(1_000_000_000),
                    QueueLimit::delay(100_000),
                    Aqm::TailDrop,
                ),
            );
            let id = p.add_cross(Dir::Forward, bursts(30, false));
            p.advance_to(Time(4_000_000_000));
            let log = p.cross_bursts(id);
            let offs: Vec<f64> = log.iter().map(|b| b.off_us as f64 / 1e6).collect();
            let sizes: Vec<f64> = log
                .iter()
                .flat_map(|b| b.sizes.iter().map(|s| *s as f64))
                .collect();
            let mean = |v: &[f64]| v.iter().sum::<f64>() / v.len() as f64;
            let sd = |v: &[f64]| {
                (v.iter().map(|x| (x - mean(v)).powi(2)).sum::<f64>() / v.len() as f64).sqrt()
            };
            [
                mean(&offs),
                sd(&offs) / mean(&offs),
                mean(&sizes),
                sd(&sizes) / mean(&sizes),
            ]
        },
    );
}

/// 一阵的结构：30 条连接；每阵在上一阵最后一条连接完成之后空闲 off 才开始（不重叠，间隔正好是记录的空闲时长）；
/// 每条连接都发完（接收端 goodput = 所有连接大小取整到 MSS 之和）；大小在 30–50 KB 之间。
#[test]
fn each_burst_runs_thirty_connections_to_completion_and_bursts_do_not_overlap() {
    let mut p = path(
        3,
        queue(cap(100_000_000), QueueLimit::delay(100_000), Aqm::TailDrop),
    );
    let id = p.add_cross(Dir::Forward, bursts(30, true));
    p.advance_to(Time(200_000_000));
    let log = p.cross_bursts(id).to_vec();
    assert!(log.len() >= 10, "{}", log.len());
    assert_eq!(
        (log[0].off_us, log[0].start),
        (0, Time(0)),
        "start_on：第一阵马上开始"
    );
    let mss = 1250u64;
    let mut want = 0u64;
    for (i, b) in log.iter().enumerate() {
        assert_eq!(b.sizes.len(), 30);
        assert!(b.sizes.iter().all(|s| (30_000..=50_000).contains(s)));
        if let Some(end) = b.end {
            assert!(end > b.start);
            want += b.sizes.iter().map(|s| s.div_ceil(mss) * mss).sum::<u64>();
            if let Some(next) = log.get(i + 1) {
                assert_eq!(
                    next.start,
                    end.plus(next.off_us),
                    "下一阵在最后一条连接完成之后空闲 off 才开始"
                );
            }
        } else {
            assert_eq!(i, log.len() - 1, "只有最后一阵可以还没完成");
        }
    }
    let st = p.cross_stats(id);
    assert!(
        st.delivered_bytes >= want && st.delivered_bytes <= want + 30 * 50_000 + 30 * mss,
        "{} vs {want}",
        st.delivered_bytes
    );
    // 从关起步：第一阵之前有一段指数分布的空闲
    let mut q = path(3, DirConfig::default());
    let id = q.add_cross(Dir::Forward, bursts(2, false));
    q.advance_to(Time(100_000_000));
    assert!(
        q.cross_bursts(id)[0].off_us > 0
            && q.cross_bursts(id)[0].start.micros() == q.cross_bursts(id)[0].off_us
    );
    assert!(q.cross_sender(id).is_some());
    assert!(q.cross_stats(id).tcp.sent > 0);
}

/// 一阵 30 条连接挤进一个窄瓶颈：会丢包、会重传，仍然全部完成，统计里有重传和超时。
#[test]
fn a_burst_of_thirty_connections_on_a_narrow_link_loses_packets_but_finishes() {
    let mut p = path(
        5,
        queue(cap(10_000_000), QueueLimit::bytes(30_000), Aqm::TailDrop),
    );
    let id = p.add_cross(Dir::Forward, bursts(30, true));
    p.advance_to(Time(9_000_000));
    let log = p.cross_bursts(id);
    assert!(
        log[0].end.is_some(),
        "9 秒内 30 × 40 KB = 1.2 MB（10 Mbps 下约 1 s）要完成"
    );
    let st = p.cross_stats(id);
    assert!(st.queue_dropped > 0 && st.tcp.retransmits > 0, "{st:?}");
}
