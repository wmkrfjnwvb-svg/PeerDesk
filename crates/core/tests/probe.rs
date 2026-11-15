//! 探测测试：从“探测到的原始事实”到“采取的策略”的整条判定链。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::net::{IpAddr, SocketAddr};

use peerdesk_core::config::ClientConfig;
use peerdesk_core::connect::{classify_nat, plan, Method, NatType, NetProfile};
use peerdesk_core::probe::{classify_tier, CpuFeatures, EncoderBench, Tier};
use peerdesk_core::region::{pick, rank};

fn sa(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

fn profile_from_probe(local: &str, mapped: &[&str], ipv6: bool, upnp: bool) -> NetProfile {
    let local: IpAddr = local.parse().unwrap();
    let mapped: Vec<SocketAddr> = mapped.iter().map(|m| sa(m)).collect();
    NetProfile {
        nat: classify_nat(&[local], &mapped),
        ipv6,
        mapped_udp: upnp,
        mapped_tcp: upnp,
    }
}

#[test]
fn home_host_with_upnp_and_roaming_client_on_cellular() {
    // 被控端在家：锥型 NAT，路由器开了 UPnP。
    let host = profile_from_probe(
        "192.168.1.10",
        &["203.0.113.5:40000", "203.0.113.5:40000"],
        false,
        true,
    );
    // 控制端在蜂窝网络：端口随机的对称 NAT。
    let client = profile_from_probe(
        "10.20.30.40",
        &["198.51.100.9:1111", "198.51.100.9:52000"],
        false,
        false,
    );
    assert_eq!(host.nat, NatType::EndpointIndependent);
    assert_eq!(client.nat, NatType::SymmetricRandom);
    let methods = plan(&client, &host, false);
    assert_eq!(
        methods.first(),
        Some(&Method::DirectUdp),
        "被控端有入口，直接拨"
    );
    assert!(methods.contains(&Method::Birthday));
    assert!(methods.contains(&Method::DirectTcp));
}

#[test]
fn corporate_client_without_udp_reaches_host_over_tcp() {
    let host = profile_from_probe(
        "192.168.1.10",
        &["203.0.113.5:40000", "203.0.113.5:40000"],
        true,
        true,
    );
    let client = profile_from_probe("10.0.0.5", &[], true, false);
    assert_eq!(client.nat, NatType::Blocked);
    assert_eq!(
        plan(&client, &host, false),
        vec![Method::DirectTcp, Method::Relay]
    );
}

#[test]
fn two_carrier_grade_nats_fall_back_to_relay_unless_ipv6() {
    let a = profile_from_probe(
        "100.64.0.1",
        &["198.51.100.1:100", "198.51.100.2:900"],
        false,
        false,
    );
    let b = profile_from_probe(
        "100.64.9.9",
        &["198.51.100.7:5", "198.51.100.7:60000"],
        false,
        false,
    );
    assert_eq!(plan(&a, &b, false), vec![Method::Relay]);
    let (a6, b6) = (
        NetProfile { ipv6: true, ..a },
        NetProfile { ipv6: true, ..b },
    );
    assert_eq!(plan(&a6, &b6, false), vec![Method::Ipv6, Method::Relay]);
}

#[test]
fn hardware_probe_to_tier() {
    let cpu = CpuFeatures::detect();
    let vm = EncoderBench {
        hardware: false,
        median_frame_us: 0,
        reference_control: false,
        frame_size_cap: false,
    };
    let tier = classify_tier(&vm);
    assert_eq!(tier, Tier::C);
    let limits = tier.limits(&cpu);
    assert_eq!(limits.max_fps, 30);
    assert_eq!(
        limits.max_height,
        if cpu.has_wide_simd() { 1080 } else { 720 }
    );
}

#[test]
fn client_config_to_server_choice() {
    let cfg = ClientConfig::from_toml(
        r#"
region = "eu"
region_fallback = ["us-east"]
[[servers]]
addr = "203.0.113.1:4433"
region = "us-east"
[[servers]]
addr = "203.0.113.2:4433"
region = "eu"
[[servers]]
addr = "10.1.2.3:4433"
region = "datacenter"
"#,
    )
    .unwrap();
    let candidates = cfg.candidates();
    let order: Vec<u64> = rank(&cfg.region, &cfg.region_fallback, &candidates)
        .iter()
        .map(|c| c.id.0)
        .collect();
    assert_eq!(order, vec![2, 1, 0], "内网 > 同区域 > 回退区域");

    // 内网服务器不可用时退到同区域
    let mut degraded = candidates;
    degraded[2].healthy = false;
    assert_eq!(
        pick(&cfg.region, &cfg.region_fallback, &degraded)
            .unwrap()
            .id
            .0,
        1
    );
}
