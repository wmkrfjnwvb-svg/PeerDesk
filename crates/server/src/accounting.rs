//! 中继记账和集群的中继安排。
//!
//! - 记账：中继每 10 秒交来一批增量，追加写进 `数据目录/usage/usage-YYYYMM.log`（一行一笔，
//!   崩溃最多丢最近 10 秒；写了一半的最后一行在重启读回时跳过），同时累加进内存账本。
//!   启动时读回本月和上个月的文件。
//! - 集群：每个节点通过心跳报告自己本月各账户的中继字节数，会员额度按全集群合计检查。
//! - 安排中继（[`ClusterDirectory`]）：会员（令牌有效、本月额度没用完）优先付费节点，
//!   付费节点都满了用免费节点；免费用户只用免费节点。

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use peerdesk_core::billing::{
    charge_cents, decode_totals, encode_totals, month_of, AccountId, Entry, Ledger, Totals,
    PUBLIC_ACCOUNT,
};
use peerdesk_core::cluster::{Cluster, Role};
use peerdesk_core::relay::Record;
use peerdesk_net::service::{member, sign_ticket, Directory, RelayPlan};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 现在的 Unix 毫秒。
pub fn wall_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// 上一个月（例如 202601 → 202512）。
#[must_use]
pub const fn previous_month(month: u32) -> u32 {
    if month % 100 == 1 {
        (month / 100 - 1) * 100 + 12
    } else {
        month - 1
    }
}

/// 账户号写成十六进制。
#[must_use]
pub fn hex(id: &AccountId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

/// 本节点的账本和账本文件。
#[derive(Debug)]
pub struct Accounting {
    node: u64,
    dir: Option<PathBuf>,
    ledger: Mutex<Ledger>,
}

/// 管理接口的一行：账户、本节点的用量、全集群的字节数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    /// 账户。
    pub account: AccountId,
    /// 本节点的用量。
    pub totals: Totals,
    /// 全集群的字节数。
    pub cluster_bytes: u64,
}

impl Accounting {
    /// 节点 `node` 的账本，文件放在 `dir`（`None` 表示只在内存里）。读回本月和上个月的记录。
    #[must_use]
    pub fn open(node: u64, dir: Option<PathBuf>, now_ms: u64) -> Self {
        let mut ledger = Ledger::new(node);
        if let Some(dir) = &dir {
            let month = month_of(now_ms);
            for m in [previous_month(month), month] {
                let text = std::fs::read_to_string(file(dir, m)).unwrap_or_default();
                for entry in text.lines().filter_map(Entry::from_line) {
                    ledger.add(&entry);
                }
            }
        }
        Self {
            node,
            dir,
            ledger: Mutex::new(ledger),
        }
    }

    /// 记一批中继交来的增量。写文件失败时仍然记进内存，并返回错误说明（由调用方写日志）。
    pub fn record(&self, records: &[Record], now_ms: u64) -> Result<(), String> {
        if records.is_empty() {
            return Ok(());
        }
        let entries: Vec<Entry> = records
            .iter()
            .map(|r| Entry {
                unix_ms: now_ms,
                node: self.node,
                account: r.account,
                session: r.session,
                bytes: r.usage.bytes,
                packets: r.usage.packets,
                last: r.last,
            })
            .collect();
        {
            let mut ledger = lock(&self.ledger);
            for e in &entries {
                ledger.add(e);
            }
        }
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let mut text = String::new();
        for e in &entries {
            text.push_str(&e.to_line());
            text.push('\n');
        }
        let path = file(dir, month_of(now_ms));
        std::fs::create_dir_all(dir)
            .and_then(|()| OpenOptions::new().create(true).append(true).open(&path))
            .and_then(|mut f| f.write_all(text.as_bytes()))
            .map_err(|e| format!("写账本 {} 失败：{e}", path.display()))
    }

    /// 发给其他节点的本月汇总。
    #[must_use]
    pub fn payload(&self, now_ms: u64) -> Vec<u8> {
        let month = month_of(now_ms);
        encode_totals(month, &lock(&self.ledger).own_month(month))
    }

    /// 收到另一个节点的汇总。解析不了的忽略。
    pub fn merge(&self, node: u64, payload: &[u8]) {
        if let Ok((month, totals)) = decode_totals(payload) {
            lock(&self.ledger).merge(node, month, totals);
        }
    }

    /// 账户本月还能不能用会员通道（额度 0 表示不限）。
    #[must_use]
    pub fn allows(&self, account: &peerdesk_core::billing::Account, now_ms: u64) -> bool {
        lock(&self.ledger).allows(month_of(now_ms), account)
    }

    /// 某月本节点的所有账户（管理接口）。
    #[must_use]
    pub fn month(&self, month: u32) -> Vec<Line> {
        let ledger = lock(&self.ledger);
        ledger
            .month(month)
            .into_iter()
            .map(|(account, totals)| Line {
                account,
                totals,
                cluster_bytes: ledger.cluster_bytes(month, &account),
            })
            .collect()
    }

    /// 管理接口的用量 JSON（`price` 是每 GB 的价格，分）。
    #[must_use]
    pub fn usage_json(&self, month: u32, price: u64, free: &str) -> String {
        let accounts: Vec<String> = self
            .month(month)
            .iter()
            .map(|l| {
                format!(
                    "{{\"account\":\"{}\",\"bytes\":{},\"sessions\":{},\"cents\":{},\"cluster_bytes\":{}}}",
                    hex(&l.account),
                    l.totals.bytes,
                    l.totals.sessions,
                    charge_cents(l.totals.bytes, price),
                    l.cluster_bytes
                )
            })
            .collect();
        format!(
            "{{\"month\":{month},\"node\":{},\"price_cents_per_gb\":{price},\"accounts\":[{}],\"free\":{free}}}",
            self.node,
            accounts.join(",")
        )
    }
}

fn file(dir: &Path, month: u32) -> PathBuf {
    dir.join(format!("usage-{month}.log"))
}

/// 集群的中继安排。
#[derive(Debug)]
pub struct ClusterDirectory {
    /// 成员表。
    pub cluster: Arc<Mutex<Cluster>>,
    /// 账本。
    pub accounting: Arc<Accounting>,
    /// 计费密钥（会员令牌、中继票据）。
    pub key: Vec<u8>,
    /// 付费节点负载超过这个值就算满。
    pub paid_limit: u16,
}

impl Directory for ClusterDirectory {
    fn plan_relay(&self, caller: &[u8; 32], account: &str) -> RelayPlan {
        let now = wall_ms();
        let (found, why) = member(Some(&self.key), account);
        let over = found.filter(|a| !self.accounting.allows(a, now));
        let member = found.filter(|_| over.is_none());
        let pick = lock(&self.cluster).pick_relay(member.is_some(), self.paid_limit);
        let Some((node, vip)) = pick else {
            let note = if member.is_some() {
                "集群里没有可用的中继节点"
            } else {
                "免费服务器拥挤或不可用，请稍后重试；升级会员可走 VIP 通道"
            };
            return RelayPlan {
                relay: None,
                note: note.into(),
            };
        };
        let Ok(addr) = node.relay.parse::<SocketAddr>() else {
            return RelayPlan {
                relay: None,
                note: format!("节点 {} 的中继地址不对：{}", node.id.0, node.relay),
            };
        };
        let id = member.map_or(PUBLIC_ACCOUNT, |m| m.id);
        let (session, ticket) = sign_ticket(&self.key, caller, id, vip);
        let channel = match (member.is_some(), vip) {
            (true, true) => format!("会员通道（付费节点 {}）", node.id.0),
            (true, false) => format!("付费节点已满，改走免费通道（节点 {}）", node.id.0),
            (false, _) => format!("免费通道（节点 {}）", node.id.0),
        };
        let note = match (why, over) {
            (Some(why), _) => format!("{channel}；{why}"),
            (None, Some(_)) => format!("{channel}；本月会员中继额度已用完，按免费用户处理"),
            (None, None) => channel,
        };
        RelayPlan {
            relay: Some((addr, session, ticket)),
            note,
        }
    }

    fn nodes(&self) -> Vec<(SocketAddr, [u8; 32], bool)> {
        lock(&self.cluster)
            .alive()
            .filter_map(|n| {
                let addr = n.signal.parse::<SocketAddr>().ok()?;
                (n.fingerprint != [0; 32]).then_some((addr, n.fingerprint, n.role == Role::Paid))
            })
            .collect()
    }
}

/// 各账户的汇总（测试和管理接口用）。
#[must_use]
pub fn by_account(lines: &[Line]) -> BTreeMap<AccountId, u64> {
    lines.iter().map(|l| (l.account, l.totals.bytes)).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use peerdesk_core::billing::{issue, verify};
    use peerdesk_core::cluster::{ClusterConfig, ClusterMsg, NodeInfo};
    use peerdesk_core::region::NodeId;
    use peerdesk_core::relay::{Ticket, Usage};
    use peerdesk_core::Time;

    const NOW: u64 = 1_795_249_805_123; // 2026-11
    const KEY: &[u8] = b"billing-secret-for-tests";

    fn record(account: AccountId, bytes: u64, last: bool) -> Record {
        Record {
            session: [1; 16],
            account,
            usage: Usage { bytes, packets: 1 },
            last,
        }
    }

    #[test]
    fn months_roll_over() {
        assert_eq!(previous_month(202_611), 202_610);
        assert_eq!(previous_month(202_601), 202_512);
    }

    /// 记账写进文件，重启后读回；写了一半的最后一行跳过；别的节点的汇总按月合计。
    #[test]
    fn ledger_survives_a_restart() {
        let dir = std::env::temp_dir().join(format!("peerdesk-acct-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Accounting::open(1, Some(dir.clone()), NOW);
        a.record(&[], NOW).unwrap();
        a.record(&[record([7; 8], 1_000, false)], NOW).unwrap();
        a.record(
            &[record([7; 8], 500, true), record(PUBLIC_ACCOUNT, 10, true)],
            NOW,
        )
        .unwrap();
        // 崩溃时写了一半的一行
        let path = file(&dir, 202_611);
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"1795249805123 1 0707").unwrap();
        drop(f);
        let b = Accounting::open(1, Some(dir.clone()), NOW);
        let lines = b.month(202_611);
        assert_eq!(by_account(&lines)[&[7; 8]], 1_500);
        assert_eq!(
            lines
                .iter()
                .find(|l| l.account == [7; 8])
                .unwrap()
                .totals
                .sessions,
            1
        );
        // 别的节点报告
        let other = Accounting::open(2, None, NOW);
        other.record(&[record([7; 8], 2_000, false)], NOW).unwrap();
        b.merge(2, &other.payload(NOW));
        b.merge(3, b"junk");
        let line = b
            .month(202_611)
            .into_iter()
            .find(|l| l.account == [7; 8])
            .unwrap();
        assert_eq!(line.cluster_bytes, 3_500);
        let json = b.usage_json(202_611, 80, "{}");
        assert!(
            json.contains("\"account\":\"0707070707070707\",\"bytes\":1500"),
            "{json}"
        );
        assert!(
            json.contains("\"cents\":1,\"cluster_bytes\":3500"),
            "{json}"
        );
        // 写不进去（目录被一个文件占着）：内存照记，报告原因
        let blocked = Accounting::open(1, Some(path.clone()), NOW);
        assert!(blocked.record(&[record([1; 8], 1, true)], NOW).is_err());
        assert_eq!(by_account(&blocked.month(202_611))[&[1; 8]], 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn node(id: u64, role: Role, relay: &str) -> NodeInfo {
        NodeInfo {
            id: NodeId(id),
            region: "cn".into(),
            addr: format!("n{id}"),
            load_permille: 0,
            sessions: 0,
            signal: format!("10.0.0.{id}:4433"),
            fingerprint: [u8::try_from(id).unwrap(); 32],
            role,
            relay: relay.into(),
        }
    }

    fn directory(me: NodeInfo, others: &[NodeInfo]) -> ClusterDirectory {
        let mut cluster = Cluster::new(me, ClusterConfig::default(), vec![]);
        for o in others {
            let beat = ClusterMsg::Heartbeat {
                from: o.clone(),
                known: vec![],
            };
            let _ = cluster.on_message(Time(0), &beat);
        }
        ClusterDirectory {
            cluster: Arc::new(Mutex::new(cluster)),
            accounting: Arc::new(Accounting::open(1, None, NOW)),
            key: KEY.to_vec(),
            paid_limit: 900,
        }
    }

    #[test]
    fn members_get_paid_relays_and_free_users_free_ones() {
        let d = directory(
            node(1, Role::Free, "10.0.0.1:3478"),
            &[
                node(2, Role::Paid, "10.0.0.2:3478"),
                node(3, Role::Paid, "bad"),
            ],
        );
        let token = issue(KEY, [5; 8], 1);
        let vip = d.plan_relay(&[9; 32], &token);
        let (addr, session, ticket) = vip.relay.unwrap();
        assert_eq!(addr, "10.0.0.2:3478".parse().unwrap(), "{}", vip.note);
        assert!(vip.note.contains("会员通道"));
        let t = Ticket::open(KEY, &ticket, NOW).unwrap();
        assert_eq!(
            (t.session, t.account, t.vip, t.subject),
            (session, [5; 8], true, [9; 8])
        );
        let free = d.plan_relay(&[9; 32], "");
        assert_eq!(free.relay.unwrap().0, "10.0.0.1:3478".parse().unwrap());
        assert!(free.note.contains("免费通道"));
        let bad = d.plan_relay(&[9; 32], "pdk1-nonsense");
        assert!(bad.note.contains("会员令牌无效"), "{}", bad.note);
        // 用满额度（1 GB）：按免费用户
        d.accounting
            .record(&[record([5; 8], 1_000_000_000, false)], wall_ms())
            .unwrap();
        let over = d.plan_relay(&[9; 32], &token);
        assert!(over.note.contains("额度已用完"), "{}", over.note);
        assert_eq!(over.relay.unwrap().0, "10.0.0.1:3478".parse().unwrap());
        // 节点列表：有信令地址和指纹的存活节点
        let nodes = d.nodes();
        assert_eq!(nodes.len(), 3);
        assert!(nodes.iter().any(|(a, _, paid)| a.port() == 4433 && *paid));
        assert!(verify(KEY, &token).is_ok());
    }

    #[test]
    fn missing_or_broken_relays_are_explained() {
        let none = directory(node(1, Role::Paid, ""), &[]);
        assert!(none
            .plan_relay(&[1; 32], "")
            .note
            .contains("免费服务器拥挤"));
        let token = issue(KEY, [5; 8], 0);
        assert!(none
            .plan_relay(&[1; 32], &token)
            .note
            .contains("没有可用的中继节点"));
        let broken = directory(node(1, Role::Free, "not-an-addr"), &[]);
        assert!(broken
            .plan_relay(&[1; 32], "")
            .note
            .contains("中继地址不对"));
        let full = directory(
            node(1, Role::Free, "10.0.0.1:3478"),
            &[NodeInfo {
                load_permille: 950,
                ..node(2, Role::Paid, "10.0.0.2:3478")
            }],
        );
        assert!(full
            .plan_relay(&[1; 32], &token)
            .note
            .contains("付费节点已满"));
    }
}
