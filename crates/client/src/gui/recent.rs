//! 最近连接过的被控端，存在数据目录里。不存密码。
//!
//! 文件每行一条：`地址 [指纹]`，最近的在前。

use std::path::{Path, PathBuf};

/// 最多记住这么多条。
pub const MAX: usize = 8;
/// 文件名。
pub const FILE: &str = "recent_hosts.txt";

/// 一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recent {
    /// `地址:端口`。
    pub addr: String,
    /// 核对过的指纹；没有时为空。
    pub pin: String,
}

/// 最近连接的列表。
#[derive(Debug)]
pub struct RecentStore {
    path: PathBuf,
    items: Vec<Recent>,
}

impl RecentStore {
    /// 读取数据目录里的列表。文件不存在或读不了时从空列表开始；看不懂的行跳过。
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(FILE);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut items: Vec<Recent> = Vec::new();
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (Some(addr), pin) = (parts.next(), parts.next()) else {
                continue;
            };
            if addr.starts_with('#') || items.iter().any(|r| r.addr == addr) {
                continue;
            }
            items.push(Recent {
                addr: addr.to_owned(),
                pin: pin.unwrap_or_default().to_owned(),
            });
        }
        items.truncate(MAX);
        Self { path, items }
    }

    /// 列表，最近的在前。
    #[must_use]
    pub fn items(&self) -> &[Recent] {
        &self.items
    }

    /// 记下一次成功的连接并保存。
    pub fn remember(&mut self, addr: &str, pin: &str) -> std::io::Result<()> {
        self.items.retain(|r| r.addr != addr);
        self.items.insert(
            0,
            Recent {
                addr: addr.trim().to_owned(),
                pin: pin.trim().to_owned(),
            },
        );
        self.items.truncate(MAX);
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text: String = self
            .items
            .iter()
            .map(|r| format!("{} {}\n", r.addr, r.pin))
            .collect();
        std::fs::write(&self.path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("peerdesk-recent-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn remembers_most_recent_first_without_duplicates() {
        let dir = temp_dir("mru");
        let mut store = RecentStore::load(&dir);
        assert!(store.items().is_empty());
        store.remember("10.0.0.1:4433", "").unwrap();
        store.remember("10.0.0.2:4433", "abcd").unwrap();
        store.remember("10.0.0.1:4433", "ef01").unwrap();
        let again = RecentStore::load(&dir);
        assert_eq!(
            again.items(),
            &[
                Recent {
                    addr: "10.0.0.1:4433".into(),
                    pin: "ef01".into()
                },
                Recent {
                    addr: "10.0.0.2:4433".into(),
                    pin: "abcd".into()
                },
            ]
        );
        for i in 0..20 {
            store.remember(&format!("10.0.1.{i}:1"), "").unwrap();
        }
        assert_eq!(store.items().len(), MAX);
        assert_eq!(store.items()[0].addr, "10.0.1.19:1");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn skips_garbage_and_survives_unwritable_paths() {
        let dir = temp_dir("garbage");
        std::fs::create_dir_all(&dir).unwrap();
        let mut text = String::from("\n# comment\n1.2.3.4:5 pin\n1.2.3.4:5 other\n   \n");
        for i in 0..20 {
            text.push_str(&format!("9.9.9.{i}:1\n"));
        }
        std::fs::write(dir.join(FILE), text).unwrap();
        let store = RecentStore::load(&dir);
        assert_eq!(store.items().len(), MAX);
        assert_eq!(store.items()[0].pin, "pin");
        assert_eq!(store.items()[1].addr, "9.9.9.0:1");

        // 数据目录是一个文件：保存失败，但列表本身照常更新。
        let file = dir.join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let mut store = RecentStore::load(&file);
        assert!(store.remember("1.1.1.1:1", "").is_err());
        assert_eq!(store.items().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
