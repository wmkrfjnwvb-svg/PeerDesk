//! 节点状态快照及其 JSON 表示。

use std::fmt::Write as _;

use peerdesk_core::cluster::{Cluster, NodeInfo};
use peerdesk_core::crypto::Policy;
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

/// 集群状态的 JSON：本节点、其他成员、加密策略。
#[must_use]
pub fn status_json(cluster: &Cluster, policy: &Policy, now: Time, cluster_auth: bool) -> String {
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
    let _ = write!(
        out,
        "],\"plaintext_allowed\":{},\"cluster_auth\":{cluster_auth}}}",
        policy.allows_plaintext()
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
        );
        assert!(json.starts_with("{\"version\":\""));
        assert!(json.contains("\"uptime_ms\":1500"));
        assert!(json.contains("\"alive\":1"));
        assert!(json.contains("\"members\":[]"));
        assert!(json.contains("\"region\":\"us\\\"east\""));
        assert!(json.contains("\"crypto\":[\"aes-256-gcm\",\"chacha20-poly1305\",\"aes-128-gcm\"]"));
        assert!(json.ends_with("\"plaintext_allowed\":false,\"cluster_auth\":true}"));

        for id in [2, 3] {
            cluster.on_message(
                Time::from_millis(1000),
                &ClusterMsg::Heartbeat {
                    from: info(id),
                    known: vec![],
                },
            );
        }
        let json = status_json(&cluster, &Policy::none(), Time::from_millis(1500), false);
        assert!(json.contains("\"alive\":3"));
        assert!(json.contains("\"id\":2,"));
        assert!(json.contains("},{\"id\":3,"));
        assert!(json.contains("\"state\":\"alive\",\"idle_ms\":500"));
        assert!(json.contains("\"crypto\":[\"none\"]"));
        assert!(json.ends_with("\"plaintext_allowed\":true,\"cluster_auth\":false}"));
        assert_eq!(json.matches('{').count(), json.matches('}').count());
    }
}
