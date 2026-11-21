//! 节点状态快照及其 JSON 表示。

use std::fmt::Write as _;

use peerdesk_core::cluster::{Cluster, NodeInfo, NodeState};
use peerdesk_core::crypto::Policy;
use peerdesk_core::relay::Usage;
use peerdesk_core::Time;

use crate::json;

fn node_fields(info: &NodeInfo) -> String {
    format!(
        "\"id\":{},\"region\":{},\"addr\":{},\"load_permille\":{},\"sessions\":{}",
        info.id.0,
        json::string(&info.region),
        json::string(&info.addr),
        info.load_permille,
        info.sessions
    )
}

/// 单个节点的 JSON 对象。
#[must_use]
pub fn node_json(info: &NodeInfo) -> String {
    format!("{{{}}}", node_fields(info))
}

/// 管理中心的集群总览：自己和所有成员，带角色、状态、信令和中继地址、负载。
#[must_use]
pub fn cluster_json(cluster: &Cluster, now_ms: u64) -> String {
    let row = |info: &NodeInfo, state: &str, me: bool| {
        format!(
            "{{\"id\":{},\"region\":{},\"role\":\"{}\",\"state\":\"{state}\",\"addr\":{},\"signal\":{},\"relay\":{},\"load_permille\":{},\"sessions\":{},\"self\":{me}}}",
            info.id.0,
            json::string(&info.region),
            info.role.name(),
            json::string(&info.addr),
            json::string(&info.signal),
            json::string(&info.relay),
            info.load_permille,
            info.sessions,
        )
    };
    let mut rows = vec![row(cluster.me(), "alive", true)];
    rows.extend(cluster.members_with_state().map(|(info, state)| {
        let state = match state {
            NodeState::Alive => "alive",
            NodeState::Suspect => "suspect",
            NodeState::Dead => "dead",
        };
        row(info, state, false)
    }));
    format!(
        "{{\"self\":{},\"generated_ms\":{now_ms},\"nodes\":[{}]}}",
        cluster.me().id.0,
        rows.join(",")
    )
}

/// 信令和中继的运行数据。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rendezvous {
    /// 在线设备数；`None` 表示没有开启信令。
    pub online: Option<usize>,
    /// 中继启动以来的转发总量、当前会话数、丢弃的包数；`None` 表示没有开启中继。
    pub relay: Option<(Usage, usize, u64)>,
}

/// 集群状态的 JSON：本节点、其他成员、加密策略、信令与中继。
#[must_use]
pub fn status_json(
    cluster: &Cluster,
    policy: &Policy,
    now: Time,
    cluster_auth: bool,
    rendezvous: &Rendezvous,
) -> String {
    let mut out = String::new();
    // 向 String 写入不会失败。
    let _ = write!(
        out,
        "{{\"version\":{},\"uptime_ms\":{},\"alive\":{},\"node\":{},\"members\":[",
        json::string(peerdesk_core::VERSION),
        now.micros() / 1000,
        cluster.alive_count(),
        node_json(cluster.me())
    );
    for (i, member) in cluster.members().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let _ = write!(
            out,
            "{{{},\"state\":{},\"idle_ms\":{}}}",
            node_fields(&member.info),
            json::string(member.state.name()),
            now.since(member.last_seen) / 1000
        );
    }
    out.push_str("],\"crypto\":[");
    for (i, suite) in policy.suites().iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json::string(suite.name()));
    }
    let (usage, sessions, dropped) = rendezvous.relay.unwrap_or_default();
    let _ = write!(
        out,
        "],\"plaintext_allowed\":{},\"cluster_auth\":{cluster_auth},\
         \"signal\":{{\"enabled\":{},\"online\":{}}},\
         \"relay\":{{\"enabled\":{},\"sessions\":{sessions},\"bytes\":{},\"packets\":{},\"dropped\":{dropped}}}}}",
        policy.allows_plaintext(),
        rendezvous.online.is_some(),
        rendezvous.online.unwrap_or(0),
        rendezvous.relay.is_some(),
        usage.bytes,
        usage.packets
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerdesk_core::cluster::{ClusterConfig, ClusterMsg};
    use peerdesk_core::region::NodeId;

    fn info(id: u64) -> NodeInfo {
        NodeInfo {
            id: NodeId(id),
            region: "us\"east".into(),
            addr: format!("10.0.0.{id}:7946"),
            load_permille: 5,
            sessions: 2,
            signal: String::new(),
            fingerprint: [0; 32],
            role: peerdesk_core::cluster::Role::Free,
            relay: String::new(),
        }
    }

    #[test]
    fn status_json_shape() {
        let mut cluster = Cluster::new(info(1), ClusterConfig::default(), vec![]);
        let json = status_json(
            &cluster,
            &Policy::recommended(),
            Time::from_millis(1500),
            true,
            &Rendezvous::default(),
        );
        assert!(json.starts_with("{\"version\":\""));
        assert!(json.contains("\"uptime_ms\":1500"));
        assert!(json.contains("\"alive\":1"));
        assert!(json.contains("\"members\":[]"));
        assert!(json.contains("\"region\":\"us\\\"east\""));
        assert!(json.contains("\"crypto\":[\"aes-256-gcm\",\"chacha20-poly1305\",\"aes-128-gcm\"]"));
        assert!(json.contains("\"plaintext_allowed\":false,\"cluster_auth\":true,"));
        assert!(json.ends_with(
            "\"signal\":{\"enabled\":false,\"online\":0},\"relay\":{\"enabled\":false,\
             \"sessions\":0,\"bytes\":0,\"packets\":0,\"dropped\":0}}"
        ));

        for id in [2, 3] {
            cluster.on_message(
                Time::from_millis(1000),
                &ClusterMsg::Heartbeat {
                    from: info(id),
                    known: vec![],
                },
            );
        }
        let rendezvous = Rendezvous {
            online: Some(4),
            relay: Some((
                Usage {
                    bytes: 900,
                    packets: 7,
                },
                2,
                3,
            )),
        };
        let json = status_json(
            &cluster,
            &Policy::none(),
            Time::from_millis(1500),
            false,
            &rendezvous,
        );
        assert!(json.contains("\"alive\":3"));
        assert!(json.contains("\"id\":2,"));
        assert!(json.contains("},{\"id\":3,"));
        assert!(json.contains("\"state\":\"alive\",\"idle_ms\":500"));
        assert!(json.contains("\"crypto\":[\"none\"]"));
        assert!(json.contains("\"plaintext_allowed\":true,\"cluster_auth\":false,"));
        assert!(json.ends_with(
            "\"signal\":{\"enabled\":true,\"online\":4},\"relay\":{\"enabled\":true,\
             \"sessions\":2,\"bytes\":900,\"packets\":7,\"dropped\":3}}"
        ));
        assert_eq!(json.matches('{').count(), json.matches('}').count());

        // 管理中心的集群列表：节点 2 心跳停了 5 秒变“可疑”，再过一阵变“下线”
        let _ = cluster.tick(Time::from_millis(6000));
        let listed = cluster_json(&cluster, 6000);
        assert!(listed.contains("\"suspect\""), "{listed}");
        let _ = cluster.tick(Time::from_millis(20_000));
        let listed = cluster_json(&cluster, 20_000);
        assert!(listed.contains("\"dead\""), "{listed}");
    }
}
