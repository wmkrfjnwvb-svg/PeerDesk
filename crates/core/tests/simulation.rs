//! 仿真测试：在各种劣化网络上跑真实代码，检查整体行为和可重放性。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use peerdesk_core::cluster::{
    Cluster, ClusterConfig, ClusterEvent, ClusterMsg, NodeInfo, NodeState,
};
use peerdesk_core::region::NodeId;
use peerdesk_core::scenario::{run, Params};
use peerdesk_core::sim::{Link, Net};
use peerdesk_core::Time;

#[test]
fn same_seed_gives_identical_report() {
    let params = Params {
        seed: 99,
        link: Link::wifi(),
        ..Params::default()
    };
    let a = run(&params).unwrap();
    let b = run(&params).unwrap();
    assert_eq!(a, b);
    let c = run(&Params {
        seed: 100,
        ..params
    })
    .unwrap();
    assert_ne!(a.trace_dump, c.trace_dump);
}

#[test]
fn lossy_wifi_drops_some_frames_but_never_delivers_garbage() {
    let report = run(&Params {
        seed: 3,
        link: Link {
            loss_permille: 50,
            ..Link::wifi()
        },
        frames: 300,
        ..Params::default()
    })
    .unwrap();
    assert!(report.established);
    assert_eq!(report.frames_sent, 300);
    // 没有重传和纠错时，5% 丢包、每帧 6 个分片，约四分之一的帧不完整。
    assert!(report.frames_delivered < 300);
    assert!(report.frames_delivered > 150, "{}", report.frames_delivered);
    assert_eq!(report.bad_datagrams, 0, "丢包不会造成解码或认证失败");
    let expired = report.trace_dump.matches("frame_expired").count();
    assert!(expired > 0);
}

#[test]
fn intercontinental_latency_is_dominated_by_propagation() {
    let link = Link::intercontinental();
    let report = run(&Params {
        seed: 5,
        link,
        frames: 60,
        ..Params::default()
    })
    .unwrap();
    assert!(report.established);
    assert!(report.latency_p50_us >= link.delay_us);
    assert!(report.latency_p50_us < link.delay_us + 30_000);
    // 握手至少一个来回
    assert!(report.handshake_us >= 2 * link.delay_us);
}

#[test]
fn narrow_link_overflows_queue_instead_of_growing_delay_without_bound() {
    // 每秒要发 60 × 6000 字节 ≈ 2.9 Mbps，链路只有 1 Mbps。
    let report = run(&Params {
        seed: 6,
        link: Link {
            rate_bps: Some(1_000_000),
            max_queue_us: 100_000,
            ..Link::lan()
        },
        frames: 120,
        ..Params::default()
    })
    .unwrap();
    assert!(report.net.queue_dropped > 0);
    assert!(report.frames_delivered < report.frames_sent);
    // 这正是 M2 要解决的问题：发送端需要按带宽估计给每帧定预算。
}

fn info(id: u64, region: &str) -> NodeInfo {
    NodeInfo {
        id: NodeId(id),
        region: region.into(),
        addr: format!("n{id}"),
        load_permille: 0,
        sessions: 0,
    }
}

/// 三个服务端节点通过仿真网络互发心跳。返回 (集群, 网络)。
struct ClusterSim {
    nodes: Vec<Cluster>,
    running: Vec<bool>,
    net: Net<Vec<u8>>,
    events: Vec<(usize, ClusterEvent)>,
}

impl ClusterSim {
    fn new(seed: u64) -> Self {
        let cfg = ClusterConfig::default();
        // 节点 1、2 只认识节点 0；靠心跳里的传闻互相发现。
        let nodes = vec![
            Cluster::new(info(0, "us"), cfg, vec![]),
            Cluster::new(info(1, "us"), cfg, vec!["n0".into()]),
            Cluster::new(info(2, "eu"), cfg, vec!["n0".into()]),
        ];
        let mut net = Net::new(seed);
        for a in 0..3 {
            for b in (a + 1)..3 {
                net.set_link(
                    a,
                    b,
                    Link {
                        loss_permille: 100,
                        ..Link::wifi()
                    },
                );
            }
        }
        Self {
            nodes,
            running: vec![true; 3],
            net,
            events: Vec::new(),
        }
    }

    fn run_until(&mut self, end: Time) {
        let mut next_tick = self.net.now();
        while next_tick <= end {
            self.net.advance_to(next_tick);
            for i in 0..self.nodes.len() {
                if !self.running[i] {
                    continue;
                }
                let (events, out) = self.nodes[i].tick(next_tick);
                self.events.extend(events.into_iter().map(|e| (i, e)));
                for o in out {
                    let to: usize = o.to_addr.trim_start_matches('n').parse().unwrap();
                    let bytes = o.msg.encode().unwrap();
                    self.net.send(i, to, bytes.len(), bytes);
                }
            }
            next_tick = next_tick.plus(100_000);
            while self.net.peek_time().is_some_and(|t| t < next_tick) {
                let d = self.net.pop().unwrap();
                if !self.running[d.to] {
                    continue;
                }
                let msg = ClusterMsg::decode(&d.payload).unwrap();
                let events = self.nodes[d.to].on_message(d.at, &msg);
                self.events.extend(events.into_iter().map(|e| (d.to, e)));
            }
        }
    }
}

#[test]
fn cluster_converges_survives_a_crash_and_heals() {
    let mut sim = ClusterSim::new(21);
    sim.run_until(Time::from_millis(5_000));
    for (i, node) in sim.nodes.iter().enumerate() {
        assert_eq!(node.alive_count(), 3, "节点 {i} 应看到全部 3 个节点");
    }
    // 节点 1 和 2 互不在对方的种子里，是通过节点 0 的心跳发现彼此的
    assert!(
        sim.events
            .contains(&(1, ClusterEvent::Discovered(NodeId(2))))
            || sim
                .events
                .contains(&(2, ClusterEvent::Discovered(NodeId(1))))
    );

    // 同区域优先
    assert_eq!(sim.nodes[0].route("eu", &[]).unwrap().id, NodeId(2));

    // 节点 2（唯一的 eu 节点）崩溃
    sim.running[2] = false;
    sim.run_until(Time::from_millis(20_000));
    for i in [0, 1] {
        assert_eq!(sim.nodes[i].state_of(NodeId(2)), Some(NodeState::Dead));
        assert_eq!(sim.nodes[i].alive_count(), 2);
        // 其余节点继续服务：eu 的客户端被导向存活的节点
        let chosen = sim.nodes[i].route("eu", &["us".into()]).unwrap();
        assert_ne!(chosen.id, NodeId(2));
    }
    assert!(sim.events.contains(&(0, ClusterEvent::Died(NodeId(2)))));

    // 节点 2 重启（全新状态，只知道种子）
    sim.nodes[2] = Cluster::new(info(2, "eu"), ClusterConfig::default(), vec!["n0".into()]);
    sim.running[2] = true;
    sim.run_until(Time::from_millis(30_000));
    for (i, node) in sim.nodes.iter().enumerate() {
        assert_eq!(node.alive_count(), 3, "节点 {i} 应重新看到 3 个节点");
    }
    assert!(sim
        .events
        .contains(&(0, ClusterEvent::Recovered(NodeId(2)))));
    assert_eq!(sim.nodes[1].route("eu", &[]).unwrap().id, NodeId(2));
}

#[test]
fn cluster_partition_heals_from_both_sides() {
    let mut sim = ClusterSim::new(22);
    sim.run_until(Time::from_millis(5_000));
    // 把节点 2 与另外两个节点隔开，时间长到双方都判定对方死亡
    sim.net.set_up(0, 2, false);
    sim.net.set_up(1, 2, false);
    sim.run_until(Time::from_millis(25_000));
    assert_eq!(sim.nodes[0].state_of(NodeId(2)), Some(NodeState::Dead));
    assert_eq!(sim.nodes[2].state_of(NodeId(0)), Some(NodeState::Dead));
    assert_eq!(sim.nodes[2].alive_count(), 1);
    // 被隔开的节点自己仍然可以服务
    assert_eq!(sim.nodes[2].route("eu", &[]).unwrap().id, NodeId(2));

    sim.net.set_up(0, 2, true);
    sim.net.set_up(1, 2, true);
    sim.run_until(Time::from_millis(35_000));
    for node in &sim.nodes {
        assert_eq!(node.alive_count(), 3);
    }
}

use peerdesk_core::crypto::{Policy, Suite};

/// 途中被翻转比特的数据报：加密套件必须全部拦下，绝不交付内容错误的帧。
#[test]
fn corruption_is_always_caught_when_encrypted() {
    for suite in [Suite::Aes128Gcm, Suite::Aes256Gcm, Suite::ChaCha20Poly1305] {
        let policy = Policy::custom(&[suite]).unwrap();
        for seed in 0..40 {
            let report = run(&Params {
                seed,
                corrupt_permille: 200,
                frames: 40,
                controller_policy: policy.clone(),
                host_policy: policy.clone(),
                ..Params::default()
            })
            .unwrap();
            assert_eq!(report.frames_corrupted, 0, "{suite} seed={seed}");
            if report.established {
                assert!(report.bad_datagrams > 0, "{suite} seed={seed}");
            }
        }
    }
}

/// 不加密时没有完整性保护：损坏会被当作正常数据交付。这是“不加密”选项的真实代价。
#[test]
fn corruption_slips_through_without_encryption() {
    let corrupted: u32 = (0..40)
        .map(|seed| {
            run(&Params {
                seed,
                corrupt_permille: 200,
                frames: 40,
                controller_policy: Policy::none(),
                host_policy: Policy::none(),
                ..Params::default()
            })
            .unwrap()
            .frames_corrupted
        })
        .sum();
    assert!(corrupted > 0);
}

/// 握手阶段的大量随机损坏：无论损坏成什么样，仿真都正常结束，不 panic、不返回错误。
#[test]
fn handshake_survives_heavy_corruption() {
    let strict = Policy::custom(&[Suite::Aes256Gcm]).unwrap();
    let mut established = 0;
    let mut failed = 0;
    for seed in 0..3000 {
        let report = run(&Params {
            seed,
            corrupt_permille: 500,
            frames: 2,
            controller_policy: strict.clone(),
            host_policy: Policy::recommended(),
            ..Params::default()
        })
        .unwrap();
        assert_eq!(report.frames_corrupted, 0);
        if report.established {
            established += 1;
        } else {
            failed += 1;
        }
    }
    assert!(established > 2000, "{established}");
    assert!(failed > 0, "应当出现因损坏而失败的握手");
}
