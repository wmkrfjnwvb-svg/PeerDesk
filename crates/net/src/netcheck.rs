//! 网络体检：还没有人连接时就把本机的网络情况摸清楚，连接时直接用，也给用户看。
//!
//! 并行做几件事（几秒内完成）：
//! - NAT 类型：问服务器两次“你看到的我是哪个地址”，中间从同一个端口向服务器的探测地址发包，
//!   比较两个端口：一样是容易打洞的锥形 NAT，不一样是难打洞的对称型；
//! - 本机到路由器的延迟（家里 Wi-Fi 好不好）、本机到服务器的延迟（运营商线路好不好）；
//! - 本机的局域网地址和全局 IPv6 地址（有 IPv6 时直连成功率高得多）；
//! - 请路由器为本机映射端口（UPnP、NAT-PMP、PCP）：成功的话外面能直接连进来。

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use peerdesk_core::paths::{Candidate, PathKind};
use peerdesk_core::signal::{observe_packet, Nat, SignalMsg};

use crate::rendezvous::Server;
use crate::stream::{read_signal, write_kind, write_signal, StreamKind};
use crate::{NetError, Node};

/// 两次体检之间等探测包到达服务器的时间。
const PROBE_SETTLE: Duration = Duration::from_millis(200);
/// 路由器延迟测几次、每次最多等多久。
const GATEWAY_TRIES: u32 = 3;
const GATEWAY_TIMEOUT: Duration = Duration::from_millis(500);
/// 请路由器映射端口最多等多久。
pub const MAP_WITHIN: Duration = Duration::from_millis(2500);

/// 体检报告。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetReport {
    /// 服务器看到的本机公网地址。
    pub public: Option<SocketAddr>,
    /// NAT 类型。
    pub nat: Option<Nat>,
    /// 本机到服务器的往返时间。
    pub server_rtt: Option<Duration>,
    /// 连服务器失败的原因。
    pub server_error: Option<String>,
    /// 默认路由器。
    pub gateway: Option<IpAddr>,
    /// 本机到路由器的往返时间。
    pub gateway_rtt: Option<Duration>,
    /// 本机的候选地址（局域网、全局 IPv6）。
    pub locals: Vec<Candidate>,
    /// 路由器映射出来的公网地址和方式（UPnP、NAT-PMP、PCP）。
    pub mapped: Option<(SocketAddr, &'static str)>,
    /// 没有映射成的原因（没有请求时为空）。
    pub mapping_error: Option<String>,
    /// 体检用时。
    pub took: Duration,
}

impl NetReport {
    /// 有没有全局 IPv6 地址。
    #[must_use]
    pub fn ipv6(&self) -> bool {
        self.locals.iter().any(|c| c.kind == PathKind::Ipv6)
    }

    /// 给用户看的几行：（项目，结果）。
    #[must_use]
    pub fn lines(&self) -> Vec<(&'static str, String)> {
        let ms = |d: Duration| format!("{:.1} ms", d.as_secs_f64() * 1000.0);
        let nat = match self.nat {
            Some(Nat::Easy) => "容易打洞（锥形 NAT 或公网）".to_owned(),
            Some(Nat::Hard) => "难打洞（对称型 NAT，换目的地就换端口），会用多端口打洞".to_owned(),
            Some(Nat::Unknown) | None => "未知（服务器没开中继，或者没连上服务器）".to_owned(),
        };
        let server = match (self.server_rtt, &self.server_error) {
            (Some(rtt), _) => ms(rtt),
            (None, Some(e)) => format!("连不上：{e}"),
            (None, None) => "-".to_owned(),
        };
        let gateway = match (self.gateway, self.gateway_rtt) {
            (Some(gw), Some(rtt)) => format!("{gw}，{}", ms(rtt)),
            (Some(gw), None) => format!("{gw}，不回应延迟探测"),
            (None, _) => "找不到默认路由器".to_owned(),
        };
        let v6: Vec<String> = self
            .locals
            .iter()
            .filter(|c| c.kind == PathKind::Ipv6)
            .map(|c| c.addr.ip().to_string())
            .collect();
        let ipv6 = if v6.is_empty() {
            "没有（只能走 IPv4）".to_owned()
        } else {
            format!("有：{}", v6.join("、"))
        };
        let mapping = match (&self.mapped, &self.mapping_error) {
            (Some((addr, how)), _) => format!("{how} 映射成功：{addr}"),
            (None, Some(e)) => format!("没有映射：{e}"),
            (None, None) => "没有请求（开启被控时才请求）".to_owned(),
        };
        vec![
            ("NAT 类型", nat),
            (
                "公网地址",
                self.public
                    .map_or_else(|| "-".to_owned(), |a| a.to_string()),
            ),
            ("本机 → 路由器", gateway),
            ("本机 → 服务器", server),
            ("IPv6", ipv6),
            ("路由器端口映射", mapping),
            ("体检用时", ms(self.took)),
        ]
    }
}

/// 体检应答里的（服务器看到的地址，探测地址，探测看到的地址）。
type Checked = (SocketAddr, Option<SocketAddr>, Option<SocketAddr>);

fn checked(reply: SignalMsg) -> Result<Checked, NetError> {
    if let SignalMsg::Checked {
        observed,
        probe,
        seen,
    } = reply
    {
        return Ok((observed, probe, seen));
    }
    Err(NetError::Protocol("服务器的体检应答不对"))
}

/// 问服务器两次（中间探测一次），得到公网地址、NAT 类型和往返时间。
async fn ask_server(node: &Node, server: &Server) -> Result<(SocketAddr, Nat, Duration), NetError> {
    let link = node.connect(server.addr, server.pin.as_ref()).await?;
    let (mut send, mut recv) = link.open_stream().await?;
    write_kind(&mut send, StreamKind::Signal).await?;
    write_signal(&mut send, &SignalMsg::Check).await?;
    let (_, probe, _) = checked(read_signal(&mut recv).await?)?;
    let nat = match probe {
        Some(probe) => {
            let packet = observe_packet(node.fingerprint());
            for _ in 0..3 {
                node.send_raw(probe, &packet);
            }
            tokio::time::sleep(PROBE_SETTLE).await;
            None
        }
        None => Some(Nat::Unknown),
    };
    write_signal(&mut send, &SignalMsg::Check).await?;
    let (observed, _, seen) = checked(read_signal(&mut recv).await?)?;
    let rtt = link.rtt();
    link.close("done");
    Ok((
        observed,
        nat.unwrap_or_else(|| Nat::classify(observed, seen)),
        rtt,
    ))
}

/// 本机到默认路由器的往返时间（优先 IPv4 路由器）。
async fn gateway() -> (Option<IpAddr>, Option<Duration>) {
    let mut gateways = crate::gateway::default_gateways();
    gateways.sort_by_key(IpAddr::is_ipv6);
    let Some(gw) = gateways.first().copied() else {
        return (None, None);
    };
    (
        Some(gw),
        crate::gateway::gateway_rtt(gw, GATEWAY_TRIES, GATEWAY_TIMEOUT).await,
    )
}

/// 体检。`server` 为 `None` 时只测本机的部分。
pub async fn run(node: &Node, server: Option<&Server>) -> NetReport {
    let started = Instant::now();
    let asking = async {
        match server {
            Some(server) => Some(ask_server(node, server).await),
            None => None,
        }
    };
    let (asked, (gateway, gateway_rtt)) = tokio::join!(asking, gateway());
    let locals = node.local_addr().map_or_else(
        |_| Vec::new(),
        |bound| crate::candidates::local_candidates(bound, &crate::candidates::lan_addresses()),
    );
    let mut report = NetReport {
        gateway,
        gateway_rtt,
        locals,
        ..NetReport::default()
    };
    match asked {
        Some(Ok((public, nat, rtt))) => {
            report.public = Some(public);
            report.nat = Some(nat);
            report.server_rtt = Some(rtt);
        }
        Some(Err(e)) => report.server_error = Some(e.to_string()),
        None => {}
    }
    report.took = started.elapsed();
    report
}

/// 被控端开启时的准备：体检，同时请路由器映射端口（成功的话保持续租）。
/// 返回体检报告和要额外报告给服务器的候选地址（映射出来的公网地址）。
pub async fn prepare_host(node: &Node, server: Option<&Server>) -> (NetReport, Vec<Candidate>) {
    prepare_host_via(node, server, crate::gateway::default_gateways()).await
}

/// 同 [`prepare_host`]，指定路由器（测试时用假路由器）。
pub async fn prepare_host_via(
    node: &Node,
    server: Option<&Server>,
    gateways: Vec<IpAddr>,
) -> (NetReport, Vec<Candidate>) {
    let (mut report, mapped) = tokio::join!(run(node, server), map_port(node, gateways));
    let mut extra = Vec::new();
    match mapped {
        Ok((external, how)) => {
            report.mapped = Some((external, how));
            extra.push(Candidate {
                addr: external,
                kind: PathKind::Mapped,
            });
        }
        Err(e) => report.mapping_error = Some(e),
    }
    (report, extra)
}

/// 请路由器为本机端口映射一个公网端口，成功后在后台按租期续租。
#[cfg(feature = "portmap")]
async fn map_port(
    node: &Node,
    gateways: Vec<IpAddr>,
) -> Result<(SocketAddr, &'static str), String> {
    let local = node.local_addr().map_err(|e| e.to_string())?;
    let mapping = crate::portmap::map_udp(local, &gateways, MAP_WITHIN).await?;
    let found = (mapping.external, mapping.protocol);
    let (tx, rx) = tokio::sync::watch::channel(Some(mapping.clone()));
    tokio::spawn(async move {
        // 接收端留在这里：全部接收端都没了，续租任务就会结束。
        let _keep = rx;
        crate::portmap::keep_mapped(local, gateways, mapping, MAP_WITHIN, tx).await;
    });
    Ok(found)
}

/// 没有编译端口映射功能（服务端）。
#[cfg(not(feature = "portmap"))]
async fn map_port(
    _node: &Node,
    _gateways: Vec<IpAddr>,
) -> Result<(SocketAddr, &'static str), String> {
    Err("这个程序没有端口映射功能".into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn only_checked_replies_are_accepted() {
        assert!(checked(SignalMsg::NotFound).is_err());
        let a: SocketAddr = "1.2.3.4:5".parse().unwrap();
        let reply = SignalMsg::Checked {
            observed: a,
            probe: None,
            seen: Some(a),
        };
        assert_eq!(checked(reply).unwrap(), (a, None, Some(a)));
    }

    #[test]
    fn lines_explain_each_item() {
        let mut r = NetReport::default();
        let text = |r: &NetReport| {
            r.lines()
                .into_iter()
                .map(|(k, v)| format!("{k}:{v}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let empty = text(&r);
        assert!(
            empty.contains("未知") && empty.contains("找不到默认路由器"),
            "{empty}"
        );
        assert!(
            empty.contains("没有（只能走 IPv4）") && empty.contains("没有请求"),
            "{empty}"
        );
        assert!(!r.ipv6());
        r.nat = Some(Nat::Easy);
        r.server_rtt = Some(Duration::from_millis(23));
        r.gateway = Some("192.168.1.1".parse().unwrap());
        r.gateway_rtt = Some(Duration::from_micros(1500));
        r.locals = vec![Candidate {
            addr: "[2408:8000::5]:4433".parse().unwrap(),
            kind: PathKind::Ipv6,
        }];
        r.mapped = Some(("203.0.113.5:4433".parse().unwrap(), "UPnP"));
        let full = text(&r);
        assert!(
            full.contains("容易打洞") && full.contains("23.0 ms"),
            "{full}"
        );
        assert!(
            full.contains("192.168.1.1，1.5 ms") && full.contains("有：2408:8000::5"),
            "{full}"
        );
        assert!(full.contains("UPnP 映射成功：203.0.113.5:4433"), "{full}");
        assert!(r.ipv6());
        r.nat = Some(Nat::Hard);
        r.server_rtt = None;
        r.server_error = Some("超时".into());
        r.gateway_rtt = None;
        r.mapped = None;
        r.mapping_error = Some("路由器没有开 UPnP".into());
        let bad = text(&r);
        assert!(
            bad.contains("难打洞") && bad.contains("连不上：超时"),
            "{bad}"
        );
        assert!(
            bad.contains("不回应延迟探测") && bad.contains("没有映射：路由器没有开 UPnP"),
            "{bad}"
        );
        r.server_error = None;
        assert!(text(&r).contains("本机 → 服务器:-"));
    }
}
