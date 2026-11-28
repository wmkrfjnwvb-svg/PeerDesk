//! 服务器设置，存在数据目录里。本机用服务器登记、拿设备码，连别人时也用它按设备码查找。
//!
//! 默认用官方服务器（[`OFFICIAL_SERVER`]），什么都不用填：新安装打开就有设备码。
//! 自建服务器的用户在设置的“高级”里填自己的地址；清空地址就回到官方服务器。
//!
//! 文件三行：自己的服务器 `地址:端口`（空着表示官方服务器）、服务器指纹（可以空着）、会员令牌（可以空着）。

use std::path::{Path, PathBuf};

use peerdesk_net::rendezvous::{Server, OFFICIAL_SERVER};

use super::form::{parse_addr, parse_pin};
use super::i18n::Strings;

/// 文件名。
pub const FILE: &str = "server.txt";

/// 服务器设置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSettings {
    /// 自己的服务器 `地址:端口`，空着表示用官方服务器。
    pub addr: String,
    /// 服务器指纹，空着表示不核对。
    pub pin: String,
    /// 会员令牌，空着表示免费用户。
    pub account: String,
}

impl ServerSettings {
    /// 读取数据目录里的设置。文件不存在或读不了时为空。
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let text = std::fs::read_to_string(path(dir)).unwrap_or_default();
        let mut lines = text.lines().map(str::trim);
        Self {
            addr: lines.next().unwrap_or_default().to_owned(),
            pin: lines.next().unwrap_or_default().to_owned(),
            account: lines.next().unwrap_or_default().to_owned(),
        }
    }

    /// 保存到数据目录。
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(
            path(dir),
            format!(
                "{}\n{}\n{}\n",
                self.addr.trim(),
                self.pin.trim(),
                self.account.trim()
            ),
        )
    }

    /// 是否填了自己的服务器（没填就用官方服务器）。
    #[must_use]
    pub fn is_custom(&self) -> bool {
        !self.addr.trim().is_empty()
    }

    /// 要用的服务器：填了自己的就用自己的，没填用官方服务器。自己的填错了给出错误说明
    /// （不悄悄换成官方服务器：用户明确要自建，就该知道没生效）。
    pub fn server(&self, s: &Strings) -> Result<Server, String> {
        if !self.is_custom() {
            return Ok(OFFICIAL_SERVER);
        }
        Ok(Server {
            addr: parse_addr(&self.addr, s.err_bad_server)?,
            pin: parse_pin(&self.pin, s)?,
        })
    }

    /// 改回官方服务器：清空自己的地址和指纹（会员令牌留着，它跟着账户，不跟着服务器）。
    pub fn use_official(&mut self) {
        self.addr.clear();
        self.pin.clear();
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::gui::i18n::{EN, ZH};

    #[test]
    fn settings_round_trip_and_parse() {
        let dir = std::env::temp_dir().join(format!("peerdesk-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(ServerSettings::load(&dir), ServerSettings::default());

        let fp = "ab".repeat(32);
        let set = ServerSettings {
            addr: " 203.0.113.5:4433 ".into(),
            pin: fp.clone(),
            account: "pdk1-token".into(),
        };
        set.save(&dir).unwrap();
        let back = ServerSettings::load(&dir);
        assert_eq!(back.addr, "203.0.113.5:4433");
        assert_eq!(back.account, "pdk1-token");
        let server = back.server(&ZH).unwrap();
        assert_eq!(server.addr, "203.0.113.5:4433".parse().unwrap());
        assert_eq!(server.pin, Some([0xab; 32]));

        let bad = ServerSettings {
            addr: "nope".into(),
            ..ServerSettings::default()
        };
        assert_eq!(bad.server(&ZH), Err(ZH.err_bad_server.to_owned()));
        let bad_pin = ServerSettings {
            addr: "1.2.3.4".into(),
            pin: "zz".into(),
            ..ServerSettings::default()
        };
        assert_eq!(bad_pin.server(&ZH), Err(ZH.err_bad_pin.to_owned()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_install_uses_the_official_server() {
        let dir = std::env::temp_dir().join(format!("peerdesk-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // 全新安装：没有设置文件
        let fresh = ServerSettings::load(&dir);
        assert!(!fresh.is_custom());
        assert_eq!(fresh.server(&ZH), Ok(OFFICIAL_SERVER));
        // 只有空白也算没填
        let blank = ServerSettings {
            addr: "  ".into(),
            ..ServerSettings::default()
        };
        assert_eq!(blank.server(&EN), Ok(OFFICIAL_SERVER));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_custom_server_wins_and_clearing_it_returns_to_official() {
        let dir = std::env::temp_dir().join(format!("peerdesk-custom-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mine = ServerSettings {
            addr: "198.51.100.7:5000".into(),
            pin: "cd".repeat(32),
            account: "pdk1-token".into(),
        };
        mine.save(&dir).unwrap();
        let mut loaded = ServerSettings::load(&dir);
        assert!(loaded.is_custom());
        let server = loaded.server(&ZH).unwrap();
        assert_ne!(server, OFFICIAL_SERVER);
        assert_eq!(server.addr, "198.51.100.7:5000".parse().unwrap());
        // 清空：回到官方服务器，存盘后再读也是；会员令牌留着
        loaded.use_official();
        loaded.save(&dir).unwrap();
        let again = ServerSettings::load(&dir);
        assert!(!again.is_custom());
        assert_eq!(again.server(&ZH), Ok(OFFICIAL_SERVER));
        assert_eq!(
            (again.pin.as_str(), again.account.as_str()),
            ("", "pdk1-token")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
