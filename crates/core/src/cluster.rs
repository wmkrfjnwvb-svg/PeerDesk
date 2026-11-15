//! 服务端集群成员管理（不做 IO）。
//!
//! 每个节点定期向所有已知节点发心跳，心跳里带上自己知道的其他节点，
//! 新节点只要认识任意一个老节点就能发现全体。长时间没有心跳的节点先标为可疑，
//! 再标为死亡；死亡节点不再接新会话，它恢复后一发心跳就自动回到集群。
//! 没有主节点，任何节点崩溃都不影响其余节点。

use std::collections::BTreeMap;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::proto::MAX_REGION_LEN;
use crate::region::{pick, Candidate, NodeId};
use crate::wire::{Reader, Writer};
use crate::{Error, Time};

/// 节点地址字符串的最大长度。
pub const MAX_ADDR_LEN: usize = 64;
/// 一条心跳里最多携带的其他节点数。
pub const MAX_KNOWN: usize = 64;

/// 认证标签的长度。
pub const TAG_LEN: usize = 32;
/// 集群密钥的最小长度（字节）。
pub const MIN_SECRET_LEN: usize = 16;

fn tag(secret: &[u8], stamped: &[u8]) -> Hmac<Sha256> {
    // HMAC 接受任意长度的密钥；万一构造失败退回到空密钥，结果只是验证不过。
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret)
        .unwrap_or_else(|_| <Hmac<Sha256> as hmac::digest::KeyInit>::new(&Default::default()));
    mac.update(stamped);
    mac
}

/// 给一条集群消息加上时间戳和认证标签：`时间戳(8) | 消息 | HMAC-SHA256(32)`。
#[must_use]
pub fn seal(secret: &[u8], stamp_ms: u64, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len().saturating_add(8 + TAG_LEN));
    out.extend_from_slice(&stamp_ms.to_be_bytes());
    out.extend_from_slice(body);
    let mac = tag(secret, &out).finalize().into_bytes();
    out.extend_from_slice(&mac);
    out
}

/// 验证并拆开一条集群消息，返回时间戳和消息体。
///
/// 标签不对、或时间戳与本机时间相差超过 `max_skew_ms` 都会被拒绝。
/// 同一个节点的时间戳是否递增由 [`Cluster::fresh`] 检查，两者合起来防重放。
pub fn open<'a>(
    secret: &[u8],
    packet: &'a [u8],
    now_ms: u64,
    max_skew_ms: u64,
) -> Result<(u64, &'a [u8]), Error> {
    let split = packet
        .len()
        .checked_sub(TAG_LEN)
        .filter(|n| *n >= 8)
        .ok_or(Error::Truncated {
            need: 8 + TAG_LEN,
            have: packet.len(),
        })?;
    let (stamped, mac) = packet.split_at(split);
    tag(secret, stamped)
        .verify_slice(mac)
        .map_err(|_| Error::OpenFailed)?;
    let (stamp, body) = stamped.split_at(8);
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(stamp);
    let stamp = u64::from_be_bytes(bytes);
    if stamp.abs_diff(now_ms) > max_skew_ms {
        return Err(Error::Inconsistent("集群消息的时间戳超出允许范围"));
    }
    Ok((stamp, body))
}

/// 节点对外公布的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    /// 节点标识，集群内唯一。
    pub id: NodeId,
    /// 所在区域。
    pub region: String,
    /// 其他节点用来联系它的地址。
    pub addr: String,
    /// 负载（千分比）。
    pub load_permille: u16,
    /// 当前会话数。
    pub sessions: u32,
}

impl NodeInfo {
    fn encode(&self, w: &mut Writer) -> Result<(), Error> {
        w.u64(self.id.0);
        w.string(&self.region, MAX_REGION_LEN)?;
        w.string(&self.addr, MAX_ADDR_LEN)?;
        w.u16(self.load_permille).u32(self.sessions);
        Ok(())
    }

    fn decode(r: &mut Reader<'_>) -> Result<Self, Error> {
        Ok(Self {
            id: NodeId(r.u64()?),
            region: r.string(MAX_REGION_LEN)?,
            addr: r.string(MAX_ADDR_LEN)?,
            load_permille: r.u16()?,
            sessions: r.u32()?,
        })
    }
}

/// 集群内部消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterMsg {
    /// 心跳。
    Heartbeat {
        /// 发送者。
        from: NodeInfo,
        /// 发送者知道的其他存活节点。
        known: Vec<NodeInfo>,
    },
}

impl ClusterMsg {
    /// 编码。
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer::new();
        match self {
            ClusterMsg::Heartbeat { from, known } => {
                let count = u8::try_from(known.len())
                    .ok()
                    .filter(|c| usize::from(*c) <= MAX_KNOWN)
                    .ok_or(Error::TooManyItems {
                        len: known.len(),
                        max: MAX_KNOWN,
                    })?;
                w.u8(1);
                from.encode(&mut w)?;
                w.u8(count);
                for node in known {
                    node.encode(&mut w)?;
                }
            }
        }
        Ok(w.into_vec())
    }

    /// 解码。
    pub fn decode(buf: &[u8]) -> Result<Self, Error> {
        let mut r = Reader::new(buf);
        let msg = match r.u8()? {
            1 => {
                let from = NodeInfo::decode(&mut r)?;
                let count = usize::from(r.u8()?);
                if count > MAX_KNOWN {
                    return Err(Error::TooManyItems {
                        len: count,
                        max: MAX_KNOWN,
                    });
                }
                let mut known = Vec::with_capacity(count);
                for _ in 0..count {
                    known.push(NodeInfo::decode(&mut r)?);
                }
                ClusterMsg::Heartbeat { from, known }
            }
            other => return Err(Error::UnknownMessage(other)),
        };
        r.finish()?;
        Ok(msg)
    }
}

/// 成员状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeState {
    /// 正常。
    Alive,
    /// 一段时间没有心跳，或者只是听别的节点提起过、尚未直接确认。
    Suspect,
    /// 长时间没有心跳。
    Dead,
}

impl NodeState {
    /// 状态名。
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            NodeState::Alive => "alive",
            NodeState::Suspect => "suspect",
            NodeState::Dead => "dead",
        }
    }
}

/// 一个成员。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// 公布的信息。
    pub info: NodeInfo,
    /// 状态。
    pub state: NodeState,
    /// 最近一次直接收到它心跳的时间。
    pub last_seen: Time,
}

/// 成员变化事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterEvent {
    /// 直接收到了一个新节点的心跳。
    Joined(NodeId),
    /// 从别的节点那里听说了一个新节点。
    Discovered(NodeId),
    /// 节点变为可疑。
    Suspected(NodeId),
    /// 节点被判定死亡。
    Died(NodeId),
    /// 可疑或死亡的节点恢复。
    Recovered(NodeId),
}

/// 时间参数（微秒）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClusterConfig {
    /// 心跳间隔。
    pub heartbeat_us: u64,
    /// 多久没有心跳算可疑。
    pub suspect_after_us: u64,
    /// 多久没有心跳算死亡。
    pub dead_after_us: u64,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            heartbeat_us: 1_000_000,
            suspect_after_us: 3_000_000,
            dead_after_us: 10_000_000,
        }
    }
}

/// 要发出的一条消息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outbound {
    /// 目的地址。
    pub to_addr: String,
    /// 消息。
    pub msg: ClusterMsg,
}

/// 本节点眼中的集群。
#[derive(Debug, Clone)]
pub struct Cluster {
    me: NodeInfo,
    cfg: ClusterConfig,
    seeds: Vec<String>,
    members: BTreeMap<NodeId, Member>,
    next_heartbeat: Time,
    /// 每个节点最近一条已接受消息的时间戳，用于防重放。
    stamps: BTreeMap<NodeId, u64>,
}

impl Cluster {
    /// 创建。`seeds` 是启动时已知的其他节点地址。
    #[must_use]
    pub fn new(me: NodeInfo, cfg: ClusterConfig, seeds: Vec<String>) -> Self {
        let seeds = seeds.into_iter().filter(|s| *s != me.addr).collect();
        Self {
            me,
            cfg,
            seeds,
            members: BTreeMap::new(),
            next_heartbeat: Time::ZERO,
            stamps: BTreeMap::new(),
        }
    }

    /// 本节点信息。
    #[must_use]
    pub const fn me(&self) -> &NodeInfo {
        &self.me
    }

    /// 更新本节点的负载，随下一次心跳公布。
    pub fn set_load(&mut self, load_permille: u16, sessions: u32) {
        self.me.load_permille = load_permille.min(1000);
        self.me.sessions = sessions;
    }

    /// 其他成员，按节点标识排序。
    pub fn members(&self) -> impl Iterator<Item = &Member> {
        self.members.values()
    }

    /// 某个成员的状态。
    #[must_use]
    pub fn state_of(&self, id: NodeId) -> Option<NodeState> {
        self.members.get(&id).map(|m| m.state)
    }

    /// 存活的节点数，包含自己。
    #[must_use]
    pub fn alive_count(&self) -> usize {
        self.members
            .values()
            .filter(|m| m.state == NodeState::Alive)
            .count()
            .saturating_add(1)
    }

    /// 某个节点带着时间戳 `stamp` 的消息是不是新的。是的话记下这个时间戳。
    /// 重放的旧消息（时间戳不大于已见过的）返回 false。
    pub fn fresh(&mut self, from: NodeId, stamp: u64) -> bool {
        match self.stamps.get(&from) {
            Some(last) if stamp <= *last => false,
            Some(_) | None => {
                self.stamps.insert(from, stamp);
                true
            }
        }
    }

    /// 处理收到的消息。
    pub fn on_message(&mut self, now: Time, msg: &ClusterMsg) -> Vec<ClusterEvent> {
        let mut events = Vec::new();
        match msg {
            ClusterMsg::Heartbeat { from, known } => {
                if from.id == self.me.id {
                    // 自己的回声，或者配置里有重复的节点标识：忽略。
                    return events;
                }
                match self.members.get_mut(&from.id) {
                    Some(member) => {
                        match member.state {
                            NodeState::Alive => {}
                            NodeState::Suspect | NodeState::Dead => {
                                events.push(ClusterEvent::Recovered(from.id));
                            }
                        }
                        member.info = from.clone();
                        member.state = NodeState::Alive;
                        member.last_seen = now;
                    }
                    None => {
                        self.members.insert(
                            from.id,
                            Member {
                                info: from.clone(),
                                state: NodeState::Alive,
                                last_seen: now,
                            },
                        );
                        events.push(ClusterEvent::Joined(from.id));
                    }
                }
                for node in known {
                    if node.id == self.me.id || self.members.contains_key(&node.id) {
                        // 已认识的节点只信直接心跳，不让传闻复活死节点。
                        continue;
                    }
                    self.members.insert(
                        node.id,
                        Member {
                            info: node.clone(),
                            state: NodeState::Suspect,
                            last_seen: now,
                        },
                    );
                    events.push(ClusterEvent::Discovered(node.id));
                }
            }
        }
        events
    }

    /// 推进时间：更新成员状态，到点时产生心跳。
    pub fn tick(&mut self, now: Time) -> (Vec<ClusterEvent>, Vec<Outbound>) {
        let mut events = Vec::new();
        for member in self.members.values_mut() {
            let idle = now.since(member.last_seen);
            match member.state {
                NodeState::Alive if idle >= self.cfg.dead_after_us => {
                    member.state = NodeState::Dead;
                    events.push(ClusterEvent::Died(member.info.id));
                }
                NodeState::Alive if idle >= self.cfg.suspect_after_us => {
                    member.state = NodeState::Suspect;
                    events.push(ClusterEvent::Suspected(member.info.id));
                }
                NodeState::Suspect if idle >= self.cfg.dead_after_us => {
                    member.state = NodeState::Dead;
                    events.push(ClusterEvent::Died(member.info.id));
                }
                NodeState::Alive | NodeState::Suspect | NodeState::Dead => {}
            }
        }

        let mut out = Vec::new();
        if now >= self.next_heartbeat {
            self.next_heartbeat = now.plus(self.cfg.heartbeat_us);
            let known: Vec<NodeInfo> = self
                .members
                .values()
                .filter(|m| m.state == NodeState::Alive)
                .map(|m| m.info.clone())
                .take(MAX_KNOWN)
                .collect();
            let msg = ClusterMsg::Heartbeat {
                from: self.me.clone(),
                known,
            };
            // 死亡的成员也继续发：网络分区恢复后双方才能重新发现对方。
            let mut targets: Vec<&str> = self
                .members
                .values()
                .map(|m| m.info.addr.as_str())
                .collect();
            for seed in &self.seeds {
                if !targets.contains(&seed.as_str()) {
                    targets.push(seed);
                }
            }
            out = targets
                .into_iter()
                .map(|to| Outbound {
                    to_addr: to.to_owned(),
                    msg: msg.clone(),
                })
                .collect();
        }
        (events, out)
    }

    /// 可以接新会话的候选节点：自己加上所有存活成员。
    #[must_use]
    pub fn candidates(&self) -> Vec<Candidate> {
        let to_candidate = |info: &NodeInfo, healthy: bool| Candidate {
            id: info.id,
            region: info.region.clone(),
            lan: false,
            rtt_ms: None,
            load_permille: info.load_permille,
            healthy,
        };
        std::iter::once(to_candidate(&self.me, true))
            .chain(
                self.members
                    .values()
                    .map(|m| to_candidate(&m.info, m.state == NodeState::Alive)),
            )
            .collect()
    }

    /// 为某个区域的客户端选一个节点：同区域优先，过载时溢出到回退区域。
    #[must_use]
    pub fn route(&self, client_region: &str, fallback: &[String]) -> Option<NodeInfo> {
        let chosen = pick(client_region, fallback, &self.candidates())?;
        if chosen.id == self.me.id {
            return Some(self.me.clone());
        }
        self.members.get(&chosen.id).map(|m| m.info.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(id: u64, region: &str) -> NodeInfo {
        NodeInfo {
            id: NodeId(id),
            region: region.into(),
            addr: format!("n{id}"),
            load_permille: 0,
            sessions: 0,
        }
    }

    fn beat(from: NodeInfo, known: Vec<NodeInfo>) -> ClusterMsg {
        ClusterMsg::Heartbeat { from, known }
    }

    #[test]
    fn message_roundtrip() {
        let msg = beat(info(1, "us"), vec![info(2, "eu"), info(3, "ap")]);
        let bytes = msg.encode().unwrap();
        assert_eq!(ClusterMsg::decode(&bytes).unwrap(), msg);
    }

    #[test]
    fn message_rejects_garbage() {
        assert_eq!(ClusterMsg::decode(&[9]), Err(Error::UnknownMessage(9)));
        assert!(matches!(
            ClusterMsg::decode(&[]),
            Err(Error::Truncated { .. })
        ));
        let mut bytes = beat(info(1, "us"), vec![]).encode().unwrap();
        bytes.push(0);
        assert_eq!(ClusterMsg::decode(&bytes), Err(Error::TrailingBytes(1)));
        // 声称的节点数超过上限
        let mut bytes = beat(info(1, "us"), vec![]).encode().unwrap();
        *bytes.last_mut().unwrap() = 65;
        assert_eq!(
            ClusterMsg::decode(&bytes),
            Err(Error::TooManyItems { len: 65, max: 64 })
        );
        let too_many = beat(info(1, "us"), (2..=66).map(|i| info(i, "us")).collect());
        assert_eq!(
            too_many.encode(),
            Err(Error::TooManyItems { len: 65, max: 64 })
        );
        let long_addr = NodeInfo {
            addr: "x".repeat(65),
            ..info(1, "us")
        };
        assert!(matches!(
            beat(long_addr.clone(), vec![]).encode(),
            Err(Error::StringTooLong { .. })
        ));
        assert!(matches!(
            beat(info(1, "us"), vec![long_addr]).encode(),
            Err(Error::StringTooLong { .. })
        ));
        let long_region = NodeInfo {
            region: "r".repeat(33),
            ..info(1, "us")
        };
        assert!(matches!(
            beat(long_region, vec![]).encode(),
            Err(Error::StringTooLong { .. })
        ));
    }

    #[test]
    fn join_discover_suspect_die_recover() {
        let cfg = ClusterConfig::default();
        let mut c = Cluster::new(info(1, "us"), cfg, vec!["n2".into(), "n1".into()]);
        assert_eq!(c.me().id, NodeId(1));
        assert_eq!(c.alive_count(), 1);

        // 第一次 tick：只认识种子（自己的地址已被剔除）
        let (events, out) = c.tick(Time(0));
        assert!(events.is_empty());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].to_addr, "n2");
        // 未到下一次心跳时间不重发
        assert!(c.tick(Time(10)).1.is_empty());

        // 节点 2 的心跳带来了节点 3 的消息
        let events = c.on_message(
            Time(100),
            &beat(info(2, "us"), vec![info(3, "eu"), info(1, "us")]),
        );
        assert_eq!(
            events,
            vec![
                ClusterEvent::Joined(NodeId(2)),
                ClusterEvent::Discovered(NodeId(3))
            ]
        );
        assert_eq!(c.state_of(NodeId(2)), Some(NodeState::Alive));
        assert_eq!(c.state_of(NodeId(3)), Some(NodeState::Suspect));
        assert_eq!(c.state_of(NodeId(9)), None);
        assert_eq!(c.alive_count(), 2);
        assert_eq!(c.members().count(), 2);

        // 自己的回声被忽略
        assert!(c
            .on_message(Time(100), &beat(info(1, "us"), vec![]))
            .is_empty());

        // 心跳发给两个成员；种子 n2 已是成员，不重复
        let (_, out) = c.tick(Time(1_000_000));
        let mut to: Vec<_> = out.iter().map(|o| o.to_addr.as_str()).collect();
        to.sort_unstable();
        assert_eq!(to, vec!["n2", "n3"]);
        // 心跳里只公布存活的成员
        match &out[0].msg {
            ClusterMsg::Heartbeat { known, .. } => {
                assert_eq!(known.iter().map(|k| k.id.0).collect::<Vec<_>>(), vec![2]);
            }
        }

        // 节点 3 直接发来心跳：恢复为存活
        let events = c.on_message(Time(1_500_000), &beat(info(3, "eu"), vec![]));
        assert_eq!(events, vec![ClusterEvent::Recovered(NodeId(3))]);

        // 节点 2 沉默：先可疑，后死亡
        let (events, _) = c.tick(Time(100 + cfg.suspect_after_us));
        assert_eq!(events, vec![ClusterEvent::Suspected(NodeId(2))]);
        c.on_message(Time(9_000_000), &beat(info(3, "eu"), vec![info(2, "us")]));
        let (events, out) = c.tick(Time(100 + cfg.dead_after_us));
        assert_eq!(events, vec![ClusterEvent::Died(NodeId(2))]);
        assert_eq!(c.state_of(NodeId(2)), Some(NodeState::Dead));
        // 传闻不能复活死节点，但心跳仍然发给它
        assert!(out.iter().any(|o| o.to_addr == "n2"));
        assert_eq!(c.alive_count(), 2);

        // 节点 2 重新上线
        let events = c.on_message(Time(20_000_000), &beat(info(2, "us"), vec![]));
        assert_eq!(events, vec![ClusterEvent::Recovered(NodeId(2))]);
        // 正常的后续心跳不产生事件
        assert!(c
            .on_message(Time(20_000_001), &beat(info(2, "us"), vec![]))
            .is_empty());
    }

    #[test]
    fn alive_node_can_jump_straight_to_dead() {
        let cfg = ClusterConfig::default();
        let mut c = Cluster::new(info(1, "us"), cfg, vec![]);
        c.on_message(Time(0), &beat(info(2, "us"), vec![]));
        let (events, _) = c.tick(Time(cfg.dead_after_us));
        assert_eq!(events, vec![ClusterEvent::Died(NodeId(2))]);
        // 已死亡的节点不再重复产生事件
        assert!(c.tick(Time(cfg.dead_after_us * 2)).0.is_empty());
    }

    #[test]
    fn routing_prefers_same_region_and_skips_dead_and_overloaded() {
        let cfg = ClusterConfig::default();
        let mut c = Cluster::new(info(1, "us"), cfg, vec![]);
        c.on_message(Time(0), &beat(info(2, "eu"), vec![]));
        c.on_message(Time(0), &beat(info(3, "ap"), vec![]));
        let fallback = vec!["ap".to_owned()];

        assert_eq!(c.route("us", &fallback).unwrap().id, NodeId(1));
        assert_eq!(c.route("eu", &fallback).unwrap().id, NodeId(2));
        // 没有该区域的节点：按回退顺序，这里是 ap
        assert_eq!(c.route("sa", &fallback).unwrap().id, NodeId(3));

        // 自己过载：us 的客户端溢出到回退区域
        c.set_load(2000, 7);
        assert_eq!(c.me().load_permille, 1000);
        assert_eq!(c.me().sessions, 7);
        assert_eq!(c.route("us", &fallback).unwrap().id, NodeId(3));

        // eu 节点死亡后不再被选中
        c.on_message(Time(9_999_999), &beat(info(3, "ap"), vec![]));
        c.tick(Time(cfg.dead_after_us));
        assert_eq!(c.route("eu", &fallback).unwrap().id, NodeId(3));
        assert_eq!(c.candidates().iter().filter(|x| x.healthy).count(), 2);
    }

    #[test]
    fn sealed_messages_verify_and_resist_tampering_and_replay() {
        let secret = b"0123456789abcdef";
        let body = beat(info(2, "us"), vec![]).encode().unwrap();
        let packet = seal(secret, 1_000_000, &body);
        assert_eq!(packet.len(), body.len() + 8 + TAG_LEN);
        let (stamp, opened) = open(secret, &packet, 1_000_500, 5000).unwrap();
        assert_eq!((stamp, opened), (1_000_000, body.as_slice()));

        // 密钥不对
        assert_eq!(
            open(b"another-secret-0", &packet, 1_000_500, 5000),
            Err(Error::OpenFailed)
        );
        // 任何一个比特被改
        for i in [0, 8, packet.len() - 1] {
            let mut bad = packet.clone();
            bad[i] ^= 1;
            assert_eq!(
                open(secret, &bad, 1_000_500, 5000),
                Err(Error::OpenFailed),
                "{i}"
            );
        }
        // 太旧或来自未来
        assert!(matches!(
            open(secret, &packet, 1_010_000, 5000),
            Err(Error::Inconsistent(_))
        ));
        assert!(matches!(
            open(secret, &packet, 990_000, 5000),
            Err(Error::Inconsistent(_))
        ));
        assert!(
            open(secret, &packet, 1_005_000, 5000).is_ok(),
            "刚好在边界上"
        );
        // 太短
        for len in [0, 8, 39] {
            assert!(matches!(
                open(secret, &packet[..len], 0, 0),
                Err(Error::Truncated { .. })
            ));
        }
        // 空消息体也能封装
        let empty = seal(secret, 7, &[]);
        assert_eq!(open(secret, &empty, 7, 0).unwrap(), (7, &[][..]));

        // 重放：同一个节点的时间戳必须递增
        let mut c = Cluster::new(info(1, "us"), ClusterConfig::default(), vec![]);
        assert!(c.fresh(NodeId(2), 100));
        assert!(!c.fresh(NodeId(2), 100));
        assert!(!c.fresh(NodeId(2), 99));
        assert!(c.fresh(NodeId(2), 101));
        assert!(c.fresh(NodeId(3), 50), "不同节点各算各的");
    }

    #[test]
    fn state_names() {
        assert_eq!(NodeState::Alive.name(), "alive");
        assert_eq!(NodeState::Suspect.name(), "suspect");
        assert_eq!(NodeState::Dead.name(), "dead");
    }
}
