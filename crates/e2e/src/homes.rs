//! 分段路径的固定场景（`docs/research/Q-redesign/a2-plan.md` 第 8 节“分段路径”）：两个用有线宽带、家里用 Wi-Fi 的家庭，
//! 加“B 家有人看视频”“A 家有人上传”两种家里的干扰。
//!
//! 参数都是**占位**，按 `industry-network-types.md` 取一个有代表性的点；A3 按网络类型抽样之后由抽样替换：
//! - 局域网（Wi-Fi）：50 Mbps、单程 1.5 ms、不乱序的 3 ms 抖动（Wi-Fi 在链路层按序重传）、0.5% 独立丢包；
//! - 家庭出口（美国有线）：下行 100 Mbps、上行 10 Mbps（M-Lab 多数用户 P75 是 50–100 下、4–12 上，第 1 节）；
//!   单程 5 ms（FCC 第 13 期 cable 空闲 RTT 技术平均 19 ms，路由器到最近的测试服务器，第 3 节）；丢尾，
//!   上行队列 200 ms、下行 100 ms（FCC cable 满载比空闲多出的排队：上行 +65 到 +230 ms，下行 47–379 ms 的满载 RTT，第 3、4 节）；
//! - 公网：单程 20 ms（两个家庭在美国两地）。
//!
//! 交叉流量的往返只有它挂的那一段自己的传播延迟（`Segmented::add_cross` 的限制），比真实的下载、上传往返短；
//! A3 给交叉流加单独的往返之前，它们比真实的更“凶”。

use peerdesk_core::path::segments::{Segment, SegmentedConfig};
use peerdesk_core::path::{
    Aqm, BottleneckConfig, Capacity, CrossSpec, Delay, Dir, DirConfig, Loss, Noise, PathConfig,
    QueueLimit, TcpConfig,
};
use peerdesk_core::time::Time;

use crate::SegmentedPlan;

/// Wi-Fi 局域网一个方向。
#[must_use]
pub fn wifi() -> DirConfig {
    DirConfig {
        bottleneck: BottleneckConfig {
            capacity: Capacity::Constant {
                rate_bps: 50_000_000,
            },
            limit: QueueLimit::delay(50_000),
            aqm: Aqm::TailDrop,
        },
        loss: Loss::Independent { ppm: 5_000 },
        delay: Delay::Fixed(1_500),
        noise: Noise {
            jitter_us: 3_000,
            fifo_jitter: true,
            ..Noise::default()
        },
        ..DirConfig::default()
    }
}

/// 有线宽带出口一个方向：`rate_bps` 的丢尾瓶颈，排队上限 `queue_us`，单程 5 ms。
#[must_use]
pub fn cable(rate_bps: u64, queue_us: u64) -> DirConfig {
    DirConfig {
        bottleneck: BottleneckConfig {
            capacity: Capacity::Constant { rate_bps },
            limit: QueueLimit::delay(queue_us),
            aqm: Aqm::TailDrop,
        },
        delay: Delay::Fixed(5_000),
        ..DirConfig::default()
    }
}

/// 有线上行（10 Mbps，200 ms 丢尾）。
#[must_use]
pub fn cable_up() -> DirConfig {
    cable(10_000_000, 200_000)
}

/// 有线下行（100 Mbps，100 ms 丢尾）。
#[must_use]
pub fn cable_down() -> DirConfig {
    cable(100_000_000, 100_000)
}

/// 两个家庭：A、B 都是 Wi-Fi 加有线宽带，中间公网单程 20 ms。
/// A 出口段的正向是 A 的上行，B 出口段的正向是 B 的下行（画面从 A 走向 B）。
#[must_use]
pub fn two_homes() -> SegmentedConfig {
    SegmentedConfig {
        segments: [
            PathConfig::symmetric(wifi()),
            PathConfig {
                forward: cable_up(),
                reverse: cable_down(),
            },
            PathConfig::symmetric(DirConfig {
                delay: Delay::Fixed(20_000),
                ..DirConfig::default()
            }),
            PathConfig {
                forward: cable_down(),
                reverse: cable_up(),
            },
            PathConfig::symmetric(wifi()),
        ],
    }
}

/// 两个家庭，家里没有别的流量。
#[must_use]
pub fn quiet() -> SegmentedPlan {
    SegmentedPlan {
        start: two_homes(),
        ..SegmentedPlan::default()
    }
}

/// B 家有人看视频：从 `start` 起，B 出口下行上一阵一阵的 TCP 下载（每一阵 1 条连接取 2–3 MB，空闲时长指数分布、
/// 均值 4 秒，大约是自适应码率的视频按块取流的样子）。码率和节奏是占位，A3 按真实视频网站取。
pub fn b_watching_video(plan: &mut SegmentedPlan, start: Time) {
    plan.cross.push((
        Segment::BEgress,
        Dir::Forward,
        CrossSpec::Bursts {
            cfg: TcpConfig::default(),
            connections: 1,
            min_bytes: 2_000_000,
            max_bytes: 3_000_000,
            mean_off_us: 4_000_000,
            start_on: true,
            start,
        },
    ));
}

/// A 家有人上传：从 `start` 起，A 出口上行挂一条发不完的 TCP。`aqm` 为真时 A 的上行开 CoDel（对照：
/// Comcast 开上行 AQM 后满载延迟 15–30 ms，没开平均约 250 ms，`industry-network-types.md` 第 4 节；他们用的是 PIE，这里是 CoDel）。
pub fn a_uploading(plan: &mut SegmentedPlan, start: Time, aqm: bool) {
    if aqm {
        plan.start
            .segment_mut(Segment::AEgress)
            .forward
            .bottleneck
            .aqm = Aqm::codel();
    }
    plan.cross.push((
        Segment::AEgress,
        Dir::Forward,
        CrossSpec::Tcp {
            cfg: TcpConfig::default(),
            start,
            bytes: None,
        },
    ));
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use crate::{run, Action, CodecKind, NetworkPlan, Phase, Scenario};

    #[test]
    fn the_builders_put_traffic_where_the_names_say() {
        let mut plan = quiet();
        assert!(plan.cross.is_empty());
        b_watching_video(&mut plan, Time(1));
        a_uploading(&mut plan, Time(2), true);
        assert_eq!(plan.cross[0].0, Segment::BEgress);
        assert_eq!(plan.cross[1].0, Segment::AEgress);
        assert!(plan.cross.iter().all(|(_, d, _)| *d == Dir::Forward));
        assert_eq!(
            plan.start.segment(Segment::AEgress).forward.bottleneck.aqm,
            Aqm::codel()
        );
        let mut tail = quiet();
        a_uploading(&mut tail, Time(2), false);
        assert_eq!(
            tail.start.segment(Segment::AEgress).forward.bottleneck.aqm,
            Aqm::TailDrop
        );
        let homes = two_homes();
        assert_eq!(homes.segment(Segment::AEgress).forward, cable_up());
        assert_eq!(homes.segment(Segment::BEgress).forward, cable_down());
        assert_eq!(homes.segment(Segment::BEgress).reverse, cable_up());
    }

    fn session(plan: SegmentedPlan) -> crate::Outcome {
        let p = |action, frames| Phase { action, frames };
        let mut s = Scenario::fixed(
            "homes",
            vec![
                p(Action::Typing, 45),
                p(Action::Scrolling, 45),
                p(Action::Idle, 30),
            ],
        );
        s.codec = CodecKind::Tile;
        s.network = NetworkPlan::Segmented(Box::new(plan));
        run(&s).unwrap()
    }

    /// 方向：B 家看视频时 B 出口（第 3 段）排队比安静时高，A 出口不受影响；A 家上传时 A 出口（第 1 段）排队比安静时高得多，
    /// 开了 CoDel 低得多，B 出口不受影响。安静时我们自己的画面也会在 10 Mbps 的上行排一点队（大画面时几十毫秒），
    /// 所以都和安静的同一个种子比，不用绝对数。
    #[test]
    fn home_traffic_queues_in_its_own_segment() {
        let base = session(quiet());
        assert_eq!(base.segments.len(), 5);
        assert!(base.route_changes.is_empty() && base.cross.is_empty());
        let q = |o: &crate::Outcome, i: usize| o.segments[i].mean_queue_ms;
        let max = |o: &crate::Outcome, i: usize| o.segments[i].max_queue_ms;
        // 安静时公网、两端局域网、B 出口（100 Mbps）几乎不排队
        for i in [0, 2, 3, 4] {
            assert!(max(&base, i) < 10.0, "{i}：{:?}", base.segments);
        }

        let mut video = quiet();
        b_watching_video(&mut video, Time(500_000));
        let v = session(video);
        assert!(max(&v, 3) > max(&base, 3) + 30.0, "{:?}", v.segments);
        assert!(v.cross[0].delivered_bytes > 0);

        let mut up = quiet();
        a_uploading(&mut up, Time(500_000), false);
        let u = session(up);
        let mut up_aqm = quiet();
        a_uploading(&mut up_aqm, Time(500_000), true);
        let c = session(up_aqm);
        assert!(
            q(&u, 1) > q(&base, 1) + 20.0,
            "{:?} 对 {:?}",
            u.segments[1],
            base.segments[1]
        );
        assert!(
            q(&c, 1) < q(&u, 1) / 2.0,
            "CoDel {:?} 丢尾 {:?}",
            c.segments[1],
            u.segments[1]
        );
        assert!(max(&u, 3) < 10.0, "{:?}", u.segments);
    }
}
