//! 账户下的设备表（不做 IO）。服务器为准：有哪些设备、叫什么名字、什么时候加的、什么时候移除的。
//!
//! - **设备属于账户**：主键是“账户号 + 设备的证书指纹”。一个指纹同一时刻只能在一个账户里。
//!   计费只看账户号（[`crate::billing::AccountId`]），这张表不参与计费判断。
//! - **入表之后身份就是指纹**：设备用入表令牌（[`crate::billing::issue_enroll`]）加入一次，之后服务器按连接的
//!   证书指纹认出它属于哪个账户，不再需要令牌。
//! - **编号**：每台设备在账户里有一个稳定的编号（从 1 起，不复用），界面和 MCP 用它指设备。
//! - **修订号**：账户里这张表每改一次（入表、改名、移除）加 1。客户端记住自己见过的修订号，下次只取之后的改动。
//! - **移除留墓碑**：移除的设备留一条带修订号的记录，[`TOMBSTONE_S`] 之后清掉；否则只取增量的客户端不知道它没了。
//!   墓碑清掉以后，修订号比它还老的客户端会被要求重新取全量（见 [`Changes::full`]）。
//! - **落盘**：每次改动产生一条 [`Record`]，一行文本，存储实现把它追加进日志；启动时逐行 [`DeviceTable::apply`] 读回，
//!   再 [`DeviceTable::prune`]。日志只追加、不重写（重写要另存“编号发到哪了、清掉的墓碑到哪个修订号”，以后日志
//!   长了再做）。
//!
//! 设计和分步计划见 `docs/research/N-devices.md`。

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::billing::{account_hex, parse_account, AccountId};
use crate::signal::DeviceId;

/// 设备名最多这么多个字。
pub const MAX_NAME: usize = 64;
/// 一个账户最多这么多台没移除的设备（防止表被灌爆；订阅档位的设备数上限以后在这之下另算）。
pub const MAX_DEVICES: usize = 2000;
/// 墓碑留多久（秒）：30 天。
pub const TOMBSTONE_S: u64 = 30 * 86_400;

/// 设备的角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 能被控（常驻主机）。
    Host,
    /// 只做控制端。
    Controller,
}

impl Role {
    /// 线上和日志里的写法。
    #[must_use]
    pub const fn id(self) -> u8 {
        match self {
            Role::Host => 1,
            Role::Controller => 2,
        }
    }

    /// 从线上的写法读；不认识的值当成只做控制端（最保守：不会被当成可以连的主机）。
    #[must_use]
    pub const fn from_id(id: u8) -> Self {
        if id == 1 {
            Role::Host
        } else {
            Role::Controller
        }
    }
}

/// 入表时设备报的信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// 证书指纹（由连接本身证明，不是设备自己说的）。
    pub fingerprint: DeviceId,
    /// 电脑名。
    pub name: String,
    /// 系统（[`crate::proto::PeerOs`] 的数值）。
    pub os: u8,
    /// 角色。
    pub role: Role,
}

/// 表里的一台设备。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// 账户内的编号。
    pub row: u32,
    /// 证书指纹。
    pub fingerprint: DeviceId,
    /// 名字。
    pub name: String,
    /// 系统。
    pub os: u8,
    /// 角色。
    pub role: Role,
    /// 入表时间（Unix 秒）。
    pub added_s: u64,
    /// 上次在线的时间（Unix 秒，下线时记；从没下线过为 0）。
    pub last_seen_s: u64,
    /// 最近一次改动时账户的修订号。
    pub changed_rev: u64,
    /// 已移除：移除的时间（Unix 秒）。
    pub removed_s: Option<u64>,
}

/// 服务器拒绝的原因。每种原因在客户端有一句“原因加解决办法”。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// 入表令牌不对。
    BadToken,
    /// 入表令牌过期了。
    ExpiredToken,
    /// 这台设备已经在别的账户里。
    OtherAccount,
    /// 账户里的设备数到上限了。
    Full,
    /// 名字不合适（空、太长、有控制字符）。
    BadName,
    /// 这台设备不在任何账户里（或者已经被移除）。
    NotMember,
    /// 没有这个编号的设备。
    NoSuchDevice,
    /// 服务器没有开账户设备表。
    Unsupported,
}

impl Refusal {
    /// 全部原因。
    pub const ALL: [Refusal; 8] = [
        Refusal::BadToken,
        Refusal::ExpiredToken,
        Refusal::OtherAccount,
        Refusal::Full,
        Refusal::BadName,
        Refusal::NotMember,
        Refusal::NoSuchDevice,
        Refusal::Unsupported,
    ];

    /// 线上的写法。
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Refusal::BadToken => 1,
            Refusal::ExpiredToken => 2,
            Refusal::OtherAccount => 3,
            Refusal::Full => 4,
            Refusal::BadName => 5,
            Refusal::NotMember => 6,
            Refusal::NoSuchDevice => 7,
            Refusal::Unsupported => 8,
        }
    }

    /// 从线上的写法读；不认识的（新版本服务器加的原因）当成“服务器不支持”。
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        Self::ALL
            .into_iter()
            .find(|r| r.code() == code)
            .unwrap_or(Refusal::Unsupported)
    }

    /// 给用户看的话：原因和解决办法。
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Refusal::BadToken => {
                "入表令牌不对（不是这台服务器签发的，或者抄错了）。请到账户里重新生成一枚再试。"
            }
            Refusal::ExpiredToken => "入表令牌过期了。请到账户里重新生成一枚再试。",
            Refusal::OtherAccount => {
                "这台设备已经在另一个账户里。先在那个账户里移除它（或在这台设备上退出账户），再加入。"
            }
            Refusal::Full => "账户里的设备数到上限了。先移除不用的设备，再加入。",
            Refusal::BadName => "设备名不合适：不能为空，最多 64 个字，不能有换行这类控制字符。",
            Refusal::NotMember => {
                "这台设备不在账户里（可能已经被移除）。用入表令牌重新加入账户后再试。"
            }
            Refusal::NoSuchDevice => "账户里没有这个编号的设备（可能刚被移除）。刷新设备列表后再试。",
            Refusal::Unsupported => "这台服务器没有开账户设备表（版本旧，或者没有配置）。升级服务器后再试。",
        }
    }
}

/// 入表的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enrolled {
    /// 账户号。
    pub account: AccountId,
    /// 这台设备在账户里的编号。
    pub row: u32,
    /// 账户现在的修订号。
    pub rev: u64,
}

/// 一次查询的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    /// 账户现在的修订号。
    pub rev: u64,
    /// `true`：`rows` 是全部没移除的设备，客户端把手上的表整个换掉；`false`：`rows` 是 `since` 之后改动过的
    /// （含移除的），客户端按编号合并。
    pub full: bool,
    /// 设备，按编号排序。
    pub rows: Vec<Row>,
}

/// 一条落盘的改动（一行文本，制表符分隔）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Record {
    /// 入表。
    Enroll {
        /// 账户。
        account: AccountId,
        /// 新的一行（`changed_rev` 就是这次改动后的修订号）。
        row: Row,
    },
    /// 改名。
    Rename {
        /// 账户。
        account: AccountId,
        /// 编号。
        row: u32,
        /// 改动后的修订号。
        rev: u64,
        /// 新名字。
        name: String,
    },
    /// 移除。
    Remove {
        /// 账户。
        account: AccountId,
        /// 编号。
        row: u32,
        /// 改动后的修订号。
        rev: u64,
        /// 移除的时间。
        at_s: u64,
    },
    /// 下线了，记上次在线时间（不占修订号）。
    Seen {
        /// 账户。
        account: AccountId,
        /// 编号。
        row: u32,
        /// 时间。
        at_s: u64,
    },
}

fn hex32(id: &DeviceId) -> String {
    id.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(text: &str) -> Option<DeviceId> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

/// 名字合不合适：不空、不超过 [`MAX_NAME`] 个字、没有控制字符（日志一行一条、制表符分隔）。
#[must_use]
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_NAME
        && !name.chars().any(char::is_control)
}

impl Record {
    /// 写成一行（不含换行）。
    #[must_use]
    pub fn to_line(&self) -> String {
        match self {
            Record::Enroll { account, row } => format!(
                "E\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                account_hex(account),
                row.row,
                row.changed_rev,
                hex32(&row.fingerprint),
                row.os,
                row.role.id(),
                row.added_s,
                row.name
            ),
            Record::Rename {
                account,
                row,
                rev,
                name,
            } => format!("N\t{}\t{row}\t{rev}\t{name}", account_hex(account)),
            Record::Remove {
                account,
                row,
                rev,
                at_s,
            } => format!("X\t{}\t{row}\t{rev}\t{at_s}", account_hex(account)),
            Record::Seen { account, row, at_s } => {
                format!("S\t{}\t{row}\t{at_s}", account_hex(account))
            }
        }
    }

    /// 从一行读；看不懂的行（写了一半、以后的版本加的种类）返回 `None`，读日志时跳过。
    #[must_use]
    pub fn from_line(line: &str) -> Option<Self> {
        let mut f = line.split('\t');
        let kind = f.next()?;
        let account = parse_account(f.next()?)?;
        let row: u32 = f.next()?.parse().ok()?;
        let record = match kind {
            "E" => {
                let rev = f.next()?.parse().ok()?;
                let fingerprint = unhex32(f.next()?)?;
                let os = f.next()?.parse().ok()?;
                let role = Role::from_id(f.next()?.parse().ok()?);
                let added_s = f.next()?.parse().ok()?;
                let name = f.next()?.to_owned();
                Record::Enroll {
                    account,
                    row: Row {
                        row,
                        fingerprint,
                        name,
                        os,
                        role,
                        added_s,
                        last_seen_s: 0,
                        changed_rev: rev,
                        removed_s: None,
                    },
                }
            }
            "N" => Record::Rename {
                account,
                row,
                rev: f.next()?.parse().ok()?,
                name: f.next()?.to_owned(),
            },
            "X" => Record::Remove {
                account,
                row,
                rev: f.next()?.parse().ok()?,
                at_s: f.next()?.parse().ok()?,
            },
            "S" => Record::Seen {
                account,
                row,
                at_s: f.next()?.parse().ok()?,
            },
            _ => return None,
        };
        // 后面多出字段的行也不认：宁可跳过，不读错
        f.next().is_none().then_some(record)
    }
}

/// 一个账户的设备。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Book {
    rev: u64,
    next_row: u32,
    rows: BTreeMap<u32, Row>,
    /// 已经清掉的墓碑里最大的修订号：见过的修订号不比它大的客户端得重新取全量。
    forgotten_rev: u64,
}

impl Book {
    fn live(&self) -> usize {
        self.rows.values().filter(|r| r.removed_s.is_none()).count()
    }
}

/// 全部账户的设备表。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceTable {
    accounts: BTreeMap<AccountId, Book>,
    /// 指纹 → 它在哪个账户、编号多少（只含没移除的）。
    owner: BTreeMap<DeviceId, (AccountId, u32)>,
}

impl DeviceTable {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 这个指纹在哪个账户、编号多少（没入表或已移除为 `None`）。
    #[must_use]
    pub fn member(&self, device: &DeviceId) -> Option<(AccountId, u32)> {
        self.owner.get(device).copied()
    }

    /// 账户现在的修订号（没有这个账户为 0）。
    #[must_use]
    pub fn rev(&self, account: &AccountId) -> u64 {
        self.accounts.get(account).map_or(0, |b| b.rev)
    }

    /// 读回一条落盘的改动（启动时用；也用于把改动应用到内存）。对不上的改动（账户里没有这个编号）跳过。
    pub fn apply(&mut self, record: &Record) {
        match record {
            Record::Enroll { account, row } => {
                let book = self.accounts.entry(*account).or_default();
                book.rev = book.rev.max(row.changed_rev);
                book.next_row = book.next_row.max(row.row);
                self.owner.insert(row.fingerprint, (*account, row.row));
                book.rows.insert(row.row, row.clone());
            }
            Record::Rename {
                account,
                row,
                rev,
                name,
            } => {
                let Some(book) = self.accounts.get_mut(account) else {
                    return;
                };
                if let Some(r) = book.rows.get_mut(row) {
                    r.name.clone_from(name);
                    r.changed_rev = *rev;
                    book.rev = book.rev.max(*rev);
                }
            }
            Record::Remove {
                account,
                row,
                rev,
                at_s,
            } => {
                let Some(book) = self.accounts.get_mut(account) else {
                    return;
                };
                if let Some(r) = book.rows.get_mut(row) {
                    r.removed_s = Some(*at_s);
                    r.changed_rev = *rev;
                    book.rev = book.rev.max(*rev);
                    // 只在这个指纹还指着这一行时才摘掉（它可能已经重新入表成了新的一行）
                    if self.owner.get(&r.fingerprint) == Some(&(*account, *row)) {
                        self.owner.remove(&r.fingerprint);
                    }
                }
            }
            Record::Seen { account, row, at_s } => {
                if let Some(r) = self
                    .accounts
                    .get_mut(account)
                    .and_then(|b| b.rows.get_mut(row))
                {
                    r.last_seen_s = r.last_seen_s.max(*at_s);
                }
            }
        }
    }

    /// 入表。已经在这个账户里的设备再入一次不算改动（返回原来的编号，没有要落盘的记录）。
    pub fn enroll(
        &mut self,
        account: AccountId,
        info: &DeviceInfo,
        now_s: u64,
    ) -> Result<(Enrolled, Option<Record>), Refusal> {
        if !valid_name(&info.name) {
            return Err(Refusal::BadName);
        }
        match self.member(&info.fingerprint) {
            Some((other, _)) if other != account => return Err(Refusal::OtherAccount),
            Some((_, row)) => {
                let rev = self.rev(&account);
                return Ok((Enrolled { account, row, rev }, None));
            }
            None => {}
        }
        let book = self.accounts.entry(account).or_default();
        if book.live() >= MAX_DEVICES {
            return Err(Refusal::Full);
        }
        let row = Row {
            row: book.next_row.saturating_add(1),
            fingerprint: info.fingerprint,
            name: info.name.clone(),
            os: info.os,
            role: info.role,
            added_s: now_s,
            last_seen_s: 0,
            changed_rev: book.rev.saturating_add(1),
            removed_s: None,
        };
        let enrolled = Enrolled {
            account,
            row: row.row,
            rev: row.changed_rev,
        };
        let record = Record::Enroll { account, row };
        self.apply(&record);
        Ok((enrolled, Some(record)))
    }

    /// 账户里 `since` 之后的改动；`since` 为 0、比账户的修订号还新（客户端换过服务器）、或者老到墓碑已经清掉时
    /// 给全量。
    #[must_use]
    pub fn list(&self, account: &AccountId, since: u64) -> Changes {
        let Some(book) = self.accounts.get(account) else {
            return Changes {
                rev: 0,
                full: true,
                rows: Vec::new(),
            };
        };
        let full = since == 0 || since > book.rev || since < book.forgotten_rev;
        let rows = book
            .rows
            .values()
            .filter(|r| {
                if full {
                    r.removed_s.is_none()
                } else {
                    r.changed_rev > since
                }
            })
            .cloned()
            .collect();
        Changes {
            rev: book.rev,
            full,
            rows,
        }
    }

    /// 账户里没移除的那一行。
    fn live_row(&mut self, account: &AccountId, row: u32) -> Result<(&mut Book, u32), Refusal> {
        let book = self
            .accounts
            .get_mut(account)
            .ok_or(Refusal::NoSuchDevice)?;
        if book.rows.get(&row).is_some_and(|r| r.removed_s.is_none()) {
            Ok((book, row))
        } else {
            Err(Refusal::NoSuchDevice)
        }
    }

    /// 改名。返回改动后的修订号和要落盘的记录。
    pub fn rename(
        &mut self,
        account: AccountId,
        row: u32,
        name: &str,
    ) -> Result<(u64, Record), Refusal> {
        if !valid_name(name) {
            return Err(Refusal::BadName);
        }
        let (book, row) = self.live_row(&account, row)?;
        let rev = book.rev.saturating_add(1);
        let record = Record::Rename {
            account,
            row,
            rev,
            name: name.to_owned(),
        };
        self.apply(&record);
        Ok((rev, record))
    }

    /// 移除（退出账户）。之后这个指纹不再是成员；它可以用入表令牌重新加入，得到新的编号。
    pub fn remove(
        &mut self,
        account: AccountId,
        row: u32,
        now_s: u64,
    ) -> Result<(u64, Record), Refusal> {
        let (book, row) = self.live_row(&account, row)?;
        let rev = book.rev.saturating_add(1);
        let record = Record::Remove {
            account,
            row,
            rev,
            at_s: now_s,
        };
        self.apply(&record);
        Ok((rev, record))
    }

    /// 设备下线了：记上次在线时间。不是成员时为 `None`。
    pub fn seen(&mut self, device: &DeviceId, now_s: u64) -> Option<Record> {
        let (account, row) = self.member(device)?;
        let record = Record::Seen {
            account,
            row,
            at_s: now_s,
        };
        self.apply(&record);
        Some(record)
    }

    /// 清掉过了 [`TOMBSTONE_S`] 的墓碑（启动时和定期调用）。返回清了几条。
    pub fn prune(&mut self, now_s: u64) -> usize {
        let mut gone = 0usize;
        for book in self.accounts.values_mut() {
            let old: Vec<u32> = book
                .rows
                .values()
                .filter(|r| {
                    r.removed_s
                        .is_some_and(|at| now_s.saturating_sub(at) >= TOMBSTONE_S)
                })
                .map(|r| r.row)
                .collect();
            for row in old {
                if let Some(r) = book.rows.remove(&row) {
                    book.forgotten_rev = book.forgotten_rev.max(r.changed_rev);
                    gone = gone.saturating_add(1);
                }
            }
        }
        gone
    }

    /// 各账户没移除的设备数。
    #[must_use]
    pub fn counts(&self) -> BTreeMap<AccountId, usize> {
        self.accounts.iter().map(|(a, b)| (*a, b.live())).collect()
    }
}

/// 账户设备表的存储。阶段 1 的实现是“追加日志加内存”（服务器里的文件实现，和这里测试用的内存实现）；
/// 以后换数据库只换实现。时间由调用方给（Unix 秒）。
pub trait DeviceStore: Send + Sync + std::fmt::Debug {
    /// 这个指纹在哪个账户、编号多少。
    fn member(&self, device: &DeviceId) -> Option<(AccountId, u32)>;
    /// 入表。
    fn enroll(
        &self,
        account: AccountId,
        info: &DeviceInfo,
        now_s: u64,
    ) -> Result<Enrolled, Refusal>;
    /// 账户里 `since` 之后的改动。
    fn list(&self, account: &AccountId, since: u64) -> Changes;
    /// 改名，返回改动后的修订号。
    fn rename(&self, account: AccountId, row: u32, name: &str) -> Result<u64, Refusal>;
    /// 移除，返回改动后的修订号。
    fn remove(&self, account: AccountId, row: u32, now_s: u64) -> Result<u64, Refusal>;
    /// 设备下线了。
    fn seen(&self, device: &DeviceId, now_s: u64);
}

/// 只在内存里的实现（测试、不配数据目录的单机服务器用；重启就没了）。
#[derive(Debug, Default)]
pub struct MemoryStore {
    table: Mutex<DeviceTable>,
}

impl MemoryStore {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, DeviceTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl DeviceStore for MemoryStore {
    fn member(&self, device: &DeviceId) -> Option<(AccountId, u32)> {
        self.lock().member(device)
    }

    fn enroll(
        &self,
        account: AccountId,
        info: &DeviceInfo,
        now_s: u64,
    ) -> Result<Enrolled, Refusal> {
        self.lock().enroll(account, info, now_s).map(|(e, _)| e)
    }

    fn list(&self, account: &AccountId, since: u64) -> Changes {
        self.lock().list(account, since)
    }

    fn rename(&self, account: AccountId, row: u32, name: &str) -> Result<u64, Refusal> {
        self.lock().rename(account, row, name).map(|(rev, _)| rev)
    }

    fn remove(&self, account: AccountId, row: u32, now_s: u64) -> Result<u64, Refusal> {
        self.lock().remove(account, row, now_s).map(|(rev, _)| rev)
    }

    fn seen(&self, device: &DeviceId, now_s: u64) {
        self.lock().seen(device, now_s);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    const A: AccountId = [1; 8];
    const B: AccountId = [2; 8];

    fn info(fp: u8, name: &str) -> DeviceInfo {
        DeviceInfo {
            fingerprint: [fp; 32],
            name: name.to_owned(),
            os: 2,
            role: Role::Host,
        }
    }

    /// 入三台设备，返回表和落盘的记录。
    fn three() -> (DeviceTable, Vec<Record>) {
        let mut t = DeviceTable::new();
        let mut log = Vec::new();
        for (fp, name) in [(1, "mac-mini"), (2, "办公室 PC"), (3, "linux-box")] {
            let (e, record) = t.enroll(A, &info(fp, name), 100).unwrap();
            assert_eq!((e.account, e.row, e.rev), (A, u32::from(fp), u64::from(fp)));
            log.push(record.unwrap());
        }
        (t, log)
    }

    #[test]
    fn enrolling_assigns_stable_numbers_and_bumps_the_revision() {
        let (mut t, _) = three();
        assert_eq!(t.rev(&A), 3);
        assert_eq!(t.rev(&B), 0);
        assert_eq!(t.member(&[2; 32]), Some((A, 2)));
        assert_eq!(t.member(&[9; 32]), None);
        // 已经在这个账户里：再入一次不算改动
        let (again, record) = t.enroll(A, &info(2, "换了名字也不算"), 200).unwrap();
        assert_eq!((again.row, again.rev, record), (2, 3, None));
        assert_eq!(t.list(&A, 0).rows[1].name, "办公室 PC");
        // 已经在别的账户里：拒绝
        assert_eq!(t.enroll(B, &info(2, "x"), 200), Err(Refusal::OtherAccount));
        // 名字不合适：拒绝
        for bad in ["", "   ", "a\tb", "a\nb", &"字".repeat(MAX_NAME + 1)] {
            assert_eq!(
                t.enroll(A, &info(9, bad), 200),
                Err(Refusal::BadName),
                "{bad:?}"
            );
        }
        assert!(valid_name(&"字".repeat(MAX_NAME)));
        // 另一个账户有自己的编号和修订号
        let (e, _) = t.enroll(B, &info(7, "别人的"), 200).unwrap();
        assert_eq!((e.row, e.rev), (1, 1));
        assert_eq!(t.counts(), BTreeMap::from([(A, 3), (B, 1)]));
    }

    #[test]
    fn listing_gives_everything_or_only_what_changed() {
        let (mut t, _) = three();
        let all = t.list(&A, 0);
        assert!(all.full);
        assert_eq!(all.rev, 3);
        assert_eq!(
            all.rows.iter().map(|r| r.row).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        // 没改动：增量是空的
        let none = t.list(&A, 3);
        assert_eq!((none.full, none.rev, none.rows.len()), (false, 3, 0));
        // 改名、移除之后：增量只有改动过的两行（含墓碑）
        let (rev, _) = t.rename(A, 1, "书房的 Mac").unwrap();
        assert_eq!(rev, 4);
        let (rev, _) = t.remove(A, 3, 500).unwrap();
        assert_eq!(rev, 5);
        let delta = t.list(&A, 3);
        assert!(!delta.full);
        assert_eq!(delta.rev, 5);
        assert_eq!(delta.rows.len(), 2);
        assert_eq!(
            (delta.rows[0].row, delta.rows[0].name.as_str()),
            (1, "书房的 Mac")
        );
        assert_eq!((delta.rows[1].row, delta.rows[1].removed_s), (3, Some(500)));
        // 只落后一步：只有移除那一行
        assert_eq!(t.list(&A, 4).rows.len(), 1);
        // 全量不含移除的
        let all = t.list(&A, 0);
        assert_eq!(all.rows.iter().map(|r| r.row).collect::<Vec<_>>(), [1, 2]);
        // 客户端的修订号比服务器还新（换过服务器）：给全量
        assert!(t.list(&A, 99).full);
        // 没有这个账户：空的全量
        let empty = t.list(&B, 7);
        assert_eq!((empty.full, empty.rev, empty.rows.len()), (true, 0, 0));
    }

    #[test]
    fn rename_and_remove_need_a_live_device() {
        let (mut t, _) = three();
        assert_eq!(t.rename(A, 9, "x"), Err(Refusal::NoSuchDevice));
        assert_eq!(t.rename(B, 1, "x"), Err(Refusal::NoSuchDevice));
        assert_eq!(t.rename(A, 1, ""), Err(Refusal::BadName));
        assert_eq!(t.remove(A, 9, 1), Err(Refusal::NoSuchDevice));
        t.remove(A, 2, 500).unwrap();
        // 移除之后：不再是成员，不能再改名、再移除
        assert_eq!(t.member(&[2; 32]), None);
        assert_eq!(t.rename(A, 2, "x"), Err(Refusal::NoSuchDevice));
        assert_eq!(t.remove(A, 2, 600), Err(Refusal::NoSuchDevice));
        assert_eq!(t.seen(&[2; 32], 700), None);
        // 可以重新入表（也可以入别的账户）：新的编号，老的墓碑还在
        let (e, _) = t.enroll(A, &info(2, "回来了"), 800).unwrap();
        assert_eq!((e.row, e.rev), (4, 5));
        assert_eq!(t.member(&[2; 32]), Some((A, 4)));
        let delta = t.list(&A, 3);
        assert_eq!(
            delta
                .rows
                .iter()
                .map(|r| (r.row, r.removed_s.is_some()))
                .collect::<Vec<_>>(),
            [(2, true), (4, false)]
        );
        // 下线：记上次在线时间，不占修订号；时间不倒退
        assert!(t.seen(&[1; 32], 900).is_some());
        t.seen(&[1; 32], 850);
        assert_eq!(t.rev(&A), 5);
        assert_eq!(t.list(&A, 0).rows[0].last_seen_s, 900);
    }

    #[test]
    fn an_account_cannot_grow_without_bound() {
        let mut t = DeviceTable::new();
        for i in 0..MAX_DEVICES {
            let mut d = info(0, "d");
            d.fingerprint[..8].copy_from_slice(&(i as u64).to_be_bytes());
            t.enroll(A, &d, 1).unwrap();
        }
        assert_eq!(t.enroll(A, &info(0xff, "多出来的"), 1), Err(Refusal::Full));
        // 移除一台就又能加
        t.remove(A, 1, 2).unwrap();
        assert!(t.enroll(A, &info(0xff, "多出来的"), 3).is_ok());
    }

    #[test]
    fn the_log_replays_to_the_same_table() {
        let (mut t, mut log) = three();
        log.push(t.rename(A, 1, "书房的 Mac").unwrap().1);
        log.push(t.remove(A, 3, 500).unwrap().1);
        log.push(t.seen(&[2; 32], 600).unwrap());
        log.push(t.enroll(A, &info(3, "又回来了"), 700).unwrap().1.unwrap());
        log.push(t.enroll(B, &info(8, "别人的"), 800).unwrap().1.unwrap());
        let text: Vec<String> = log.iter().map(Record::to_line).collect();
        assert_eq!(
            text[0],
            format!(
                "E\t0101010101010101\t1\t1\t{}\t2\t1\t100\tmac-mini",
                "01".repeat(32)
            )
        );
        assert_eq!(text[3], "N\t0101010101010101\t1\t4\t书房的 Mac");
        assert_eq!(text[4], "X\t0101010101010101\t3\t5\t500");
        assert_eq!(text[5], "S\t0101010101010101\t2\t600");
        // 逐行读回：同样的表
        let mut back = DeviceTable::new();
        for line in &text {
            back.apply(&Record::from_line(line).unwrap());
        }
        assert_eq!(back, t);
        // 看不懂的行跳过：写了一半的、种类不认识的、字段不对的、多出字段的
        for bad in [
            "",
            "E",
            "E\t0101010101010101\t1\t1",
            "Z\t0101010101010101\t1\t1",
            "N\txx\t1\t4\tname",
            "N\t0101010101010101\tone\t4\tname",
            "X\t0101010101010101\t3\t5\t500\textra",
            "E\t0101010101010101\t1\t1\tzz\t2\t1\t100\tname",
        ] {
            assert_eq!(Record::from_line(bad), None, "{bad:?}");
        }
        // 对不上的改动（账户或编号不存在）不改表
        let before = t.clone();
        for line in [
            "N\t0909090909090909\t1\t4\tname",
            "N\t0101010101010101\t99\t9\tname",
            "X\t0909090909090909\t1\t4\t5",
            "X\t0101010101010101\t99\t9\t5",
            "S\t0909090909090909\t1\t5",
            "S\t0101010101010101\t99\t5",
        ] {
            t.apply(&Record::from_line(line).unwrap());
        }
        assert_eq!(t, before);
    }

    #[test]
    fn old_tombstones_are_pruned_and_stale_clients_get_everything() {
        let (mut t, _) = three();
        t.remove(A, 3, 1_000).unwrap();
        assert_eq!(t.prune(1_000 + TOMBSTONE_S - 1), 0);
        assert!(!t.list(&A, 3).full, "墓碑还在：给增量");
        assert_eq!(t.prune(1_000 + TOMBSTONE_S), 1);
        // 修订号比清掉的墓碑还老的客户端：给全量（否则它不知道 3 号没了）
        let stale = t.list(&A, 3);
        assert!(stale.full);
        assert_eq!(stale.rows.iter().map(|r| r.row).collect::<Vec<_>>(), [1, 2]);
        // 已经见过那次移除的客户端：照常给增量
        assert!(!t.list(&A, 4).full);
        // 编号不复用
        let (e, _) = t.enroll(A, &info(9, "新的"), 2_000_000).unwrap();
        assert_eq!(e.row, 4);
    }

    #[test]
    fn refusals_have_codes_and_actionable_text() {
        for r in Refusal::ALL {
            assert_eq!(Refusal::from_code(r.code()), r);
            assert!(r.text().chars().count() > 10);
        }
        assert_eq!(Refusal::from_code(0), Refusal::Unsupported);
        assert_eq!(Refusal::from_code(200), Refusal::Unsupported);
        assert_eq!(Role::from_id(Role::Host.id()), Role::Host);
        assert_eq!(Role::from_id(Role::Controller.id()), Role::Controller);
        assert_eq!(Role::from_id(77), Role::Controller);
    }

    #[test]
    fn the_memory_store_serves_the_table() {
        let s = MemoryStore::new();
        let e = s.enroll(A, &info(1, "mac-mini"), 10).unwrap();
        assert_eq!((e.row, e.rev), (1, 1));
        assert_eq!(s.enroll(B, &info(1, "x"), 10), Err(Refusal::OtherAccount));
        assert_eq!(s.member(&[1; 32]), Some((A, 1)));
        assert_eq!(s.rename(A, 1, "书房").unwrap(), 2);
        s.seen(&[1; 32], 50);
        let all = s.list(&A, 0);
        assert_eq!(
            (all.rows[0].name.as_str(), all.rows[0].last_seen_s),
            ("书房", 50)
        );
        assert_eq!(s.remove(A, 1, 60).unwrap(), 3);
        assert_eq!(s.member(&[1; 32]), None);
        assert_eq!(s.rename(A, 1, "x"), Err(Refusal::NoSuchDevice));
    }
}
