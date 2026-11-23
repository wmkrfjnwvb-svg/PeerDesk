//! 最近连接过的被控端，存在数据目录里。不存密码。
//!
//! 文件每行一条，最近的在前。新格式用制表符分隔：`地址 指纹 系统 电脑名`；
//! 老格式 `地址 [指纹]`（空格分隔）照样能读。老版本读新文件只取前两项，也不出错。

use std::path::{Path, PathBuf};

use peerdesk_core::proto::PeerOs;

/// 最多记住这么多条。
pub const MAX: usize = 8;
/// 文件名。
pub const FILE: &str = "recent_hosts.txt";

impl Recent {
    /// 卡片上显示的标题：设备码按“三位一组”显示，地址原样显示；以及是不是设备码。
    #[must_use]
    pub fn title(&self) -> (String, bool) {
        match peerdesk_core::signal::parse_code(&self.addr) {
            Some(code) if !self.addr.contains(':') => {
                (peerdesk_core::signal::format_code(code), true)
            }
            Some(_) | None => (self.addr.clone(), false),
        }
    }

    /// 列表里的两行：有电脑名时电脑名是标题、设备码在下面；没有时设备码是标题。
    #[must_use]
    pub fn heading(&self, code_label: &str, ip_label: &str) -> (String, String) {
        let (title, is_code) = self.title();
        let kind = if is_code { code_label } else { ip_label };
        if self.name.is_empty() {
            (title, kind.to_owned())
        } else {
            (self.name.clone(), format!("{kind} {title}"))
        }
    }
}

/// 系统在文件里的写法。
#[must_use]
pub const fn os_key(os: PeerOs) -> &'static str {
    match os {
        PeerOs::Windows => "windows",
        PeerOs::Mac => "macos",
        PeerOs::Linux => "linux",
        PeerOs::Unknown => "-",
    }
}

/// [`os_key`] 的反向。
#[must_use]
pub fn os_from_key(key: &str) -> PeerOs {
    match key {
        "windows" => PeerOs::Windows,
        "macos" => PeerOs::Mac,
        "linux" => PeerOs::Linux,
        _ => PeerOs::Unknown,
    }
}

/// 写进文件前去掉会破坏格式的字符（制表符、换行）。
fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// 一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recent {
    /// `地址:端口`。
    pub addr: String,
    /// 核对过的指纹；没有时为空。
    pub pin: String,
    /// 对方的系统（老版本对方或老记录为未知）。
    pub os: PeerOs,
    /// 对方的电脑名；不知道时为空。
    pub name: String,
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
            let parts: Vec<&str> = if line.contains('\t') {
                line.split('\t').map(str::trim).collect()
            } else {
                line.split_whitespace().collect()
            };
            let field = |i: usize| parts.get(i).copied().unwrap_or_default();
            let addr = field(0);
            if addr.is_empty() || addr.starts_with('#') || items.iter().any(|r| r.addr == addr) {
                continue;
            }
            items.push(Recent {
                addr: addr.to_owned(),
                pin: if field(1) == "-" {
                    String::new()
                } else {
                    field(1).to_owned()
                },
                os: os_from_key(field(2)),
                name: field(3).to_owned(),
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
    /// `os` 和 `name` 不知道时（对方是老版本）保留这条记录原来的值。
    pub fn remember(
        &mut self,
        addr: &str,
        pin: &str,
        os: PeerOs,
        name: &str,
    ) -> std::io::Result<()> {
        let old = self.items.iter().position(|r| r.addr == addr.trim());
        let old = old.map(|i| self.items.remove(i));
        let (old_os, old_name) = old.map_or((PeerOs::Unknown, String::new()), |r| (r.os, r.name));
        self.items.insert(
            0,
            Recent {
                addr: addr.trim().to_owned(),
                pin: pin.trim().to_owned(),
                os: if os == PeerOs::Unknown { old_os } else { os },
                name: if name.trim().is_empty() {
                    old_name
                } else {
                    clean(name)
                },
            },
        );
        self.items.truncate(MAX);
        self.save()
    }

    /// 删掉一条记录。
    pub fn forget(&mut self, addr: &str) -> std::io::Result<()> {
        self.items.retain(|r| r.addr != addr);
        self.save()
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text: String = self
            .items
            .iter()
            .map(|r| {
                let pin = if r.pin.is_empty() { "-" } else { &r.pin };
                format!("{}\t{pin}\t{}\t{}\n", r.addr, os_key(r.os), r.name)
            })
            .collect();
        std::fs::write(&self.path, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_and_forgetting() {
        let code = Recent {
            addr: "339166837".into(),
            pin: String::new(),
            os: PeerOs::Unknown,
            name: String::new(),
        };
        assert_eq!(code.title(), ("339 166 837".into(), true));
        let ip = Recent {
            addr: "192.168.1.20:4433".into(),
            pin: String::new(),
            os: PeerOs::Unknown,
            name: String::new(),
        };
        assert_eq!(ip.title(), ("192.168.1.20:4433".into(), false));
        let dir = temp_dir("forget");
        let mut store = RecentStore::load(&dir);
        store
            .remember("339166837", "", PeerOs::Unknown, "")
            .unwrap();
        store
            .remember("10.0.0.1:4433", "", PeerOs::Unknown, "")
            .unwrap();
        store.forget("339166837").unwrap();
        assert_eq!(RecentStore::load(&dir).items().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        store
            .remember("10.0.0.1:4433", "", PeerOs::Unknown, "")
            .unwrap();
        store
            .remember("10.0.0.2:4433", "abcd", PeerOs::Unknown, "")
            .unwrap();
        store
            .remember("10.0.0.1:4433", "ef01", PeerOs::Unknown, "")
            .unwrap();
        let again = RecentStore::load(&dir);
        assert_eq!(
            again.items(),
            &[
                Recent {
                    addr: "10.0.0.1:4433".into(),
                    pin: "ef01".into(),
                    os: PeerOs::Unknown,
                    name: String::new(),
                },
                Recent {
                    addr: "10.0.0.2:4433".into(),
                    pin: "abcd".into(),
                    os: PeerOs::Unknown,
                    name: String::new(),
                },
            ]
        );
        for i in 0..20 {
            store
                .remember(&format!("10.0.1.{i}:1"), "", PeerOs::Unknown, "")
                .unwrap();
        }
        assert_eq!(store.items().len(), MAX);
        assert_eq!(store.items()[0].addr, "10.0.1.19:1");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn keeps_name_and_os_and_reads_old_files() {
        let dir = temp_dir("named");
        let mut store = RecentStore::load(&dir);
        store
            .remember("339166837", "ab", PeerOs::Mac, "张三的\tMacBook\n")
            .unwrap();
        // 对方是老版本（没有名字和系统）：保留原来的
        store
            .remember("339166837", "ab", PeerOs::Unknown, "")
            .unwrap();
        let again = RecentStore::load(&dir);
        let item = &again.items()[0];
        assert_eq!(
            (item.os, item.name.as_str()),
            (PeerOs::Mac, "张三的 MacBook")
        );
        assert_eq!(
            item.heading("设备码", "IP"),
            ("张三的 MacBook".into(), "设备码 339 166 837".into())
        );
        store.remember("10.0.0.1:1", "", PeerOs::Linux, "").unwrap();
        let again = RecentStore::load(&dir);
        assert_eq!(again.items()[0].pin, "");
        assert_eq!(
            again.items()[0].heading("设备码", "IP"),
            ("10.0.0.1:1".into(), "IP".into())
        );
        for os in [PeerOs::Windows, PeerOs::Mac, PeerOs::Linux, PeerOs::Unknown] {
            assert_eq!(os_from_key(os_key(os)), os);
        }
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
        assert!(store
            .remember("1.1.1.1:1", "", PeerOs::Unknown, "")
            .is_err());
        assert_eq!(store.items().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
