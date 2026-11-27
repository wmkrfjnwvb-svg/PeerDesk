//! 账户设备表存盘：`数据目录/devices/devices.log`，一行一条改动，只追加（和用量日志同样的做法）。
//!
//! - 启动时逐行读回（[`peerdesk_core::devices::DeviceTable::apply`]），看不懂的行跳过（写了一半的最后一行、
//!   以后的版本加的记录种类），然后清掉过期的墓碑。
//! - 每次改动先改内存里的表，再把这条改动追加进日志。写盘失败时改动仍然生效（设备已经得到了答复），
//!   记一条错误日志说明“重启后这条改动会丢”——不静默。
//! - 以后换数据库：实现同一个 [`DeviceStore`] 接口，别处不用动（`docs/research/N-devices.md` 3.3）。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use peerdesk_core::billing::AccountId;
use peerdesk_core::devices::{
    Changes, DeviceInfo, DeviceStore, DeviceTable, Enrolled, Record, Refusal,
};
use peerdesk_core::signal::DeviceId;

/// 日志文件名。
pub const FILE: &str = "devices.log";

/// 存在文件里的账户设备表。
#[derive(Debug)]
pub struct FileDeviceStore {
    table: Mutex<DeviceTable>,
    path: PathBuf,
}

impl FileDeviceStore {
    /// 打开 `dir` 里的日志（没有就是空表），读回，清掉过期的墓碑。返回表和跳过了几行。
    pub fn open(dir: &Path, now_s: u64) -> (Self, usize) {
        let path = dir.join(FILE);
        let mut table = DeviceTable::new();
        let mut skipped = 0usize;
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                for line in text.lines().filter(|l| !l.trim().is_empty()) {
                    match Record::from_line(line) {
                        Some(record) => table.apply(&record),
                        None => skipped = skipped.saturating_add(1),
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                peerdesk_net::note!(error, "读取账户设备表 {} 失败：{e}", path.display());
            }
        }
        table.prune(now_s);
        (
            Self {
                table: Mutex::new(table),
                path,
            },
            skipped,
        )
    }

    fn lock(&self) -> MutexGuard<'_, DeviceTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 各账户没移除的设备数（管理接口用）。
    #[must_use]
    pub fn counts(&self) -> std::collections::BTreeMap<AccountId, usize> {
        self.lock().counts()
    }

    /// 把一条改动追加进日志。在持有表的锁时调用，保证日志里的顺序和内存里改动的顺序一致。
    fn append(&self, record: &Record) {
        let written = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)
            })
            .and_then(|mut file| writeln!(file, "{}", record.to_line()));
        if let Err(e) = written {
            peerdesk_net::note!(
                error,
                "账户设备表写盘失败（{}）：{e}。这条改动现在生效，但服务器重启后会丢；请检查磁盘空间和目录权限",
                self.path.display()
            );
        }
    }
}

impl DeviceStore for FileDeviceStore {
    fn member(&self, device: &DeviceId) -> Option<(AccountId, u32)> {
        self.lock().member(device)
    }

    fn enroll(
        &self,
        account: AccountId,
        info: &DeviceInfo,
        now_s: u64,
    ) -> Result<Enrolled, Refusal> {
        let mut table = self.lock();
        let (enrolled, record) = table.enroll(account, info, now_s)?;
        if let Some(record) = record {
            self.append(&record);
        }
        Ok(enrolled)
    }

    fn list(&self, account: &AccountId, since: u64) -> Changes {
        self.lock().list(account, since)
    }

    fn rename(&self, account: AccountId, row: u32, name: &str) -> Result<u64, Refusal> {
        let mut table = self.lock();
        let (rev, record) = table.rename(account, row, name)?;
        self.append(&record);
        Ok(rev)
    }

    fn remove(&self, account: AccountId, row: u32, now_s: u64) -> Result<u64, Refusal> {
        let mut table = self.lock();
        let (rev, record) = table.remove(account, row, now_s)?;
        self.append(&record);
        Ok(rev)
    }

    fn seen(&self, device: &DeviceId, now_s: u64) {
        let mut table = self.lock();
        if let Some(record) = table.seen(device, now_s) {
            self.append(&record);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;
    use peerdesk_core::devices::{Role, TOMBSTONE_S};

    const A: AccountId = [7; 8];

    fn info(fp: u8, name: &str) -> DeviceInfo {
        DeviceInfo {
            fingerprint: [fp; 32],
            name: name.to_owned(),
            os: 3,
            role: Role::Host,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("peerdesk-devstore-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_table_survives_a_restart() {
        let dir = temp("restart").join("devices");
        let (store, skipped) = FileDeviceStore::open(&dir, 100);
        assert_eq!(skipped, 0);
        assert_eq!(store.enroll(A, &info(1, "mac-mini"), 100).unwrap().row, 1);
        assert_eq!(store.enroll(A, &info(2, "办公室 PC"), 100).unwrap().row, 2);
        // 重复入表不写盘
        assert_eq!(store.enroll(A, &info(2, "x"), 100).unwrap().row, 2);
        assert_eq!(store.rename(A, 1, "书房的 Mac").unwrap(), 3);
        assert_eq!(store.remove(A, 2, 200).unwrap(), 4);
        store.seen(&[1; 32], 300);
        store.seen(&[9; 32], 300);
        assert_eq!(store.rename(A, 9, "x"), Err(Refusal::NoSuchDevice));
        assert_eq!(store.remove(A, 9, 1), Err(Refusal::NoSuchDevice));
        assert_eq!(store.enroll(A, &info(3, ""), 1), Err(Refusal::BadName));
        let before = store.list(&A, 0);
        assert_eq!(store.counts().get(&A), Some(&1));
        let lines = std::fs::read_to_string(dir.join(FILE)).unwrap();
        assert_eq!(
            lines.lines().count(),
            5,
            "两次入表、改名、移除、上次在线：\n{lines}"
        );
        drop(store);

        // 重启：同样的表；被移除的不是成员；修订号接着走
        let (again, skipped) = FileDeviceStore::open(&dir, 400);
        assert_eq!(skipped, 0);
        assert_eq!(again.list(&A, 0), before);
        assert_eq!(again.list(&A, 0).rows[0].last_seen_s, 300);
        assert_eq!(again.member(&[1; 32]), Some((A, 1)));
        assert_eq!(again.member(&[2; 32]), None);
        assert_eq!(
            again.list(&A, 3).rows.len(),
            1,
            "墓碑还在，增量里有移除的那一行"
        );
        assert_eq!(again.enroll(A, &info(2, "回来了"), 400).unwrap().row, 3);
        drop(again);

        // 日志里混进坏行（写了一半的最后一行）：跳过并数出来，其余照常
        let path = dir.join(FILE);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("garbage line\n\nE\t0707070707070707\t9\t9");
        std::fs::write(&path, text).unwrap();
        let (third, skipped) = FileDeviceStore::open(&dir, 500);
        assert_eq!(skipped, 2);
        assert_eq!(third.list(&A, 0).rows.len(), 2);
        // 过了 30 天：启动时清掉墓碑
        drop(third);
        let (late, _) = FileDeviceStore::open(&dir, 200 + TOMBSTONE_S);
        assert!(late.list(&A, 3).full, "墓碑清了，老客户端重新取全量");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[test]
    fn a_disk_that_cannot_be_written_does_not_lose_the_answer() {
        // 日志的位置被一个文件占着（建不了目录）：改动照样生效，只是不落盘（并记错误日志）
        let base = temp("readonly");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("blocker"), "x").unwrap();
        let (store, _) = FileDeviceStore::open(&base.join("blocker").join("devices"), 1);
        assert_eq!(store.enroll(A, &info(1, "mac"), 1).unwrap().row, 1);
        assert_eq!(store.member(&[1; 32]), Some((A, 1)));
        // 读不了的日志（是个目录）：当成空表启动
        let dir = base.join("as-dir");
        std::fs::create_dir_all(dir.join(FILE)).unwrap();
        let (empty, skipped) = FileDeviceStore::open(&dir, 1);
        assert_eq!((empty.list(&A, 0).rows.len(), skipped), (0, 0));
        let _ = std::fs::remove_dir_all(base);
    }
}
