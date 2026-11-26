//! 设备在线状态：最近连接和设备列表里显示，按设备码连的设备不在线时不让连。
//!
//! 每 [`REFRESH`] 问一次服务器（[`peerdesk_net::rendezvous::presence`]），一次把列表里的设备都问了；列表里出现新设备时
//! 提前问（但两次之间至少隔 [`MIN_GAP`]，服务器出问题时不狂问）。
//!
//! 诚实：服务器连不上、是老版本、限流时状态是“未知”，带着原因显示；未知不当成不在线，照样能连。
//! 按直连地址（IP:端口）记下的设备，服务器说不在线也不挡：局域网里可能照样连得上。

use std::collections::BTreeSet;
use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use peerdesk_core::crypto::Policy;
use peerdesk_core::signal::{parse_code, DeviceId, MAX_PRESENCE};
use peerdesk_net::identity::Fingerprint;
use peerdesk_net::rendezvous::{presence, Server};
use peerdesk_net::{Identity, Node};

use super::session::Repaint;
use crate::remote::runtime;

/// 多久问一次。
pub const REFRESH: Duration = Duration::from_secs(30);
/// 两次查询之间至少隔这么久（列表里来了新设备时提前问，也不能比这更勤）。
pub const MIN_GAP: Duration = Duration::from_secs(5);

/// 查询键：按设备码，或者按设备指纹。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    /// 按设备码连的设备。
    Code(u32),
    /// 按直连地址记下、知道指纹的设备。
    Device(DeviceId),
}

impl Key {
    /// 最近连接里记下的地址（设备码或“IP:端口”）和指纹对应的查询键：按设备码连的按码查，直连地址知道指纹的按指纹查，
    /// 都没有时查不了（`None`，界面不显示状态）。
    #[must_use]
    pub fn of(addr: &str, pin: Option<&Fingerprint>) -> Option<Self> {
        parse_code(addr)
            .map(Key::Code)
            .or_else(|| pin.map(|p| Key::Device(*p)))
    }
}

/// 一台设备的状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// 在线。
    Online,
    /// 服务器明确说不在线。
    Offline,
    /// 不知道，带着原因（还没查、服务器连不上、服务器是老版本……）。
    Unknown(String),
}

/// 一次查询的结果：和问的设备一一对应，或者失败的原因。
pub type Answer = Result<Vec<bool>, String>;

/// 列表里各设备的在线状态。界面每帧调 [`Board::tick`]。
#[derive(Debug, Default)]
pub struct Board {
    known: BTreeMap<Key, Presence>,
    asking: Option<(Vec<Key>, Receiver<Answer>)>,
    last_ask: Option<Instant>,
}

impl Board {
    /// 这台设备的状态。
    #[must_use]
    pub fn status(&self, key: &Key) -> Presence {
        self.known
            .get(key)
            .cloned()
            .unwrap_or_else(|| Presence::Unknown("还没查".to_owned()))
    }

    /// 不让连的原因（`None` 表示可以连）：只有按设备码连、服务器明确说不在线时才挡，这种连接本来就要经过服务器找设备，
    /// 挡住只是省得用户干等。直连地址、状态未知时不挡。
    #[must_use]
    pub fn blocks(&self, key: &Key) -> Option<&'static str> {
        match (key, self.known.get(key)) {
            (Key::Code(_), Some(Presence::Offline)) => Some("对方不在线：等对方打开 PeerDesk 后再连"),
            _ => None,
        }
    }

    /// 每帧调用：先收上一次查询的结果；到了时间、或者列表里有还没查过的设备，就用 `ask` 发起一次查询
    /// （`ask` 收到设备码和指纹两组，返回取结果的通道）。返回这次有没有发起查询。
    pub fn tick(
        &mut self,
        now: Instant,
        keys: &[Key],
        ask: impl FnOnce(Vec<u32>, Vec<DeviceId>) -> Receiver<Answer>,
    ) -> bool {
        if let Some((asked, rx)) = &self.asking {
            let answer = match rx.try_recv() {
                Ok(answer) => answer,
                Err(TryRecvError::Empty) => return false,
                Err(TryRecvError::Disconnected) => Err("查询中断".to_owned()),
            };
            match answer {
                Ok(online) => {
                    for (key, on) in asked.iter().zip(online) {
                        let state = if on { Presence::Online } else { Presence::Offline };
                        self.known.insert(*key, state);
                    }
                }
                Err(reason) => {
                    for key in asked {
                        self.known.insert(*key, Presence::Unknown(reason.clone()));
                    }
                }
            }
            self.asking = None;
        }
        let wanted: Vec<Key> = keys
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(MAX_PRESENCE)
            .collect();
        if wanted.is_empty() {
            return false;
        }
        let since = self.last_ask.map(|t| now.duration_since(t));
        let due = since.is_none_or(|d| d >= REFRESH);
        let fresh = wanted.iter().any(|k| !self.known.contains_key(k));
        if !due && !(fresh && since.is_some_and(|d| d >= MIN_GAP)) {
            return false;
        }
        // 服务器的回答先设备码、后指纹：按这个顺序记下问了哪些
        let (codes, devices): (Vec<Key>, Vec<Key>) =
            wanted.into_iter().partition(|k| matches!(k, Key::Code(_)));
        let code_list = codes
            .iter()
            .filter_map(|k| match k {
                Key::Code(c) => Some(*c),
                Key::Device(_) => None,
            })
            .collect();
        let device_list = devices
            .iter()
            .filter_map(|k| match k {
                Key::Device(d) => Some(*d),
                Key::Code(_) => None,
            })
            .collect();
        let asked = codes.into_iter().chain(devices).collect();
        self.last_ask = Some(now);
        self.asking = Some((asked, ask(code_list, device_list)));
        true
    }
}

/// 在新线程上问服务器（界面线程不等），问完请求重绘。结果从返回的通道里取。
#[must_use]
pub fn spawn_check(
    servers: Vec<Server>,
    codes: Vec<u32>,
    devices: Vec<DeviceId>,
    data_dir: PathBuf,
    repaint: Repaint,
) -> Receiver<Answer> {
    let (tx, rx) = mpsc::channel();
    // 线程起不来时 tx 跟着闭包丢掉，界面收到“查询中断”
    let _ = std::thread::Builder::new()
        .name("peerdesk-presence".to_owned())
        .spawn(move || {
            let answer = check_now(&servers, &codes, &devices, &data_dir);
            let _ = tx.send(answer);
            repaint();
        });
    rx
}

/// 问一次（阻塞）。服务器的回答和问的设备一一对应（先设备码、后指纹）。
pub fn check_now(
    servers: &[Server],
    codes: &[u32],
    devices: &[DeviceId],
    data_dir: &Path,
) -> Answer {
    let rt = runtime().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let identity = Identity::load_or_create(data_dir)?;
        let any6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0);
        let any4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0);
        let node = Node::bind_first(&[any6, any4], &identity, &Policy::recommended())?;
        let answer = presence(&node, servers, codes, devices).await;
        node.close();
        answer
    })
    .map_err(|e| format!("查不到在线状态：{e}"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn answer(a: Answer) -> impl FnOnce(Vec<u32>, Vec<DeviceId>) -> Receiver<Answer> {
        move |_, _| {
            let (tx, rx) = mpsc::channel();
            tx.send(a).unwrap();
            rx
        }
    }

    #[test]
    fn keys_come_from_codes_or_pins() {
        assert_eq!(Key::of("123 456 789", None), Some(Key::Code(123_456_789)));
        assert_eq!(Key::of("123456789", Some(&[1; 32])), Some(Key::Code(123_456_789)));
        assert_eq!(Key::of("10.0.0.2:4433", Some(&[1; 32])), Some(Key::Device([1; 32])));
        assert_eq!(Key::of("10.0.0.2:4433", None), None);
    }

    #[test]
    fn board_asks_on_schedule_and_only_blocks_codes_known_offline() {
        let mut b = Board::default();
        let t0 = Instant::now();
        let code = Key::Code(123_456_789);
        let lan = Key::Device([2; 32]);
        assert!(!b.tick(t0, &[], answer(Ok(vec![]))), "没有设备不问");
        assert_eq!(b.status(&code), Presence::Unknown("还没查".to_owned()));
        assert_eq!(b.blocks(&code), None, "没查过不挡");
        // 问：设备码在前、指纹在后（重复的只问一次）
        let mut seen = None;
        assert!(b.tick(t0, &[lan, code, code], |c, d| {
            seen = Some((c, d));
            let (tx, rx) = mpsc::channel();
            tx.send(Ok(vec![false, false])).unwrap();
            rx
        }));
        assert_eq!(seen, Some((vec![123_456_789], vec![[2; 32]])));
        // 下一帧收结果；没到时间不再问
        assert!(!b.tick(t0 + Duration::from_secs(1), &[code, lan], answer(Ok(vec![]))));
        assert_eq!(b.status(&code), Presence::Offline);
        assert_eq!(b.blocks(&code), Some("对方不在线：等对方打开 PeerDesk 后再连"));
        assert_eq!(b.status(&lan), Presence::Offline);
        assert_eq!(b.blocks(&lan), None, "直连地址：局域网里可能连得上，不挡");
        // 来了新设备：至少隔 5 秒才提前问
        let new = Key::Code(987_654_321);
        assert!(!b.tick(t0 + Duration::from_secs(2), &[code, new], answer(Ok(vec![]))));
        assert!(b.tick(t0 + MIN_GAP, &[code, new], answer(Ok(vec![true, true]))));
        assert!(!b.tick(t0 + MIN_GAP, &[code, new], answer(Ok(vec![]))));
        assert_eq!(b.status(&code), Presence::Online);
        assert_eq!(b.blocks(&code), None);
        // 到时间了：服务器出问题，状态未知（带原因），不挡
        let later = t0 + MIN_GAP + REFRESH;
        assert!(b.tick(later, &[code], answer(Err("服务器连不上".to_owned()))));
        assert!(!b.tick(later, &[code], answer(Ok(vec![]))));
        assert_eq!(b.status(&code), Presence::Unknown("服务器连不上".to_owned()));
        assert_eq!(b.blocks(&code), None);
        // 查询线程没回答就没了：查询中断
        let lost = later + REFRESH;
        assert!(b.tick(lost, &[code], |_, _| mpsc::channel().1));
        assert!(!b.tick(lost, &[code], answer(Ok(vec![]))));
        assert_eq!(b.status(&code), Presence::Unknown("查询中断".to_owned()));
        // 还在查：不重复问
        let (_keep, pending) = mpsc::channel::<Answer>();
        let mut c = Board::default();
        assert!(c.tick(lost, &[code], move |_, _| pending));
        assert!(!c.tick(lost + REFRESH, &[code], answer(Ok(vec![]))));
    }

    #[test]
    fn a_failed_check_says_why() {
        let dir = std::env::temp_dir().join(format!("peerdesk-presence-{}", std::process::id()));
        let answer = check_now(&[], &[123_456_789], &[], &dir);
        assert_eq!(answer, Err("查不到在线状态：连接失败：没有配置服务器".to_owned()));
        let rx = spawn_check(Vec::new(), vec![1], Vec::new(), dir, std::sync::Arc::new(|| {}));
        assert!(rx.recv().unwrap().is_err());
    }
}
