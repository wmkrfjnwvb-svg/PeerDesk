//! 服务器设置：填一次，存在数据目录里。本机用它登记、拿设备码，连别人时也用它按设备码查找。
//!
//! 文件两行：第一行服务器 `地址:端口`，第二行服务器指纹（可以空着）。

use std::path::{Path, PathBuf};

use peerdesk_net::rendezvous::Server;

use super::form::{parse_addr, parse_pin};
use super::i18n::Strings;

/// 文件名。
pub const FILE: &str = "server.txt";

/// 服务器设置。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerSettings {
    /// 服务器 `地址:端口`，空着表示不用服务器。
    pub addr: String,
    /// 服务器指纹，空着表示不核对。
    pub pin: String,
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
        }
    }

    /// 保存到数据目录。
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        std::fs::write(
            path(dir),
            format!("{}\n{}\n", self.addr.trim(), self.pin.trim()),
        )
    }

    /// 是否填了服务器。
    #[must_use]
    pub fn is_set(&self) -> bool {
        !self.addr.trim().is_empty()
    }

    /// 解析成服务器；没填时为 `None`，填错时给出错误说明。
    pub fn server(&self, s: &Strings) -> Result<Option<Server>, String> {
        if !self.is_set() {
            return Ok(None);
        }
        Ok(Some(Server {
            addr: parse_addr(&self.addr, s.err_bad_server)?,
            pin: parse_pin(&self.pin, s)?,
        }))
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::gui::i18n::ZH;

    #[test]
    fn settings_round_trip_and_parse() {
        let dir = std::env::temp_dir().join(format!("peerdesk-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(ServerSettings::load(&dir), ServerSettings::default());
        assert_eq!(ServerSettings::default().server(&ZH), Ok(None));

        let fp = "ab".repeat(32);
        let set = ServerSettings {
            addr: " 203.0.113.5:4433 ".into(),
            pin: fp.clone(),
        };
        set.save(&dir).unwrap();
        let back = ServerSettings::load(&dir);
        assert_eq!(back.addr, "203.0.113.5:4433");
        let server = back.server(&ZH).unwrap().unwrap();
        assert_eq!(server.addr, "203.0.113.5:4433".parse().unwrap());
        assert_eq!(server.pin, Some([0xab; 32]));

        let bad = ServerSettings {
            addr: "nope".into(),
            pin: String::new(),
        };
        assert_eq!(bad.server(&ZH), Err(ZH.err_bad_server.to_owned()));
        let bad_pin = ServerSettings {
            addr: "1.2.3.4".into(),
            pin: "zz".into(),
        };
        assert_eq!(bad_pin.server(&ZH), Err(ZH.err_bad_pin.to_owned()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
