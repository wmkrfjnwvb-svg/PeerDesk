//! 表单的解析和错误的白话说明。

use std::net::{IpAddr, SocketAddr};

use peerdesk_net::identity::parse_hex;
use peerdesk_net::rendezvous::{PathPolicy, Server};
use peerdesk_net::{Fingerprint, NetError};

use super::i18n::Strings;

/// 没写端口时用的端口。
pub const DEFAULT_PORT: u16 = 4433;

/// 控制端要连到哪里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// 直接连这个地址；给了指纹就核对。
    Addr {
        /// 地址。
        addr: SocketAddr,
        /// 要核对的指纹。
        pin: Option<Fingerprint>,
    },
    /// 通过服务器找到这台设备，先试直连，不通走中继。设备指纹同时用来核对。
    Device {
        /// 信令服务器。
        server: Server,
        /// 被控端指纹。
        device: Fingerprint,
        /// 路径策略。
        policy: PathPolicy,
    },
}

/// 端口：1 到 65535。
pub fn parse_port(text: &str, s: &Strings) -> Result<u16, String> {
    match text.trim().parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        Ok(_) | Err(_) => Err(s.err_bad_port.to_owned()),
    }
}

/// `IP:端口`，或者只有 IP（用默认端口）。IPv6 写成 `[::1]:4433` 或者不带端口的 `::1`。
pub fn parse_addr(text: &str, bad: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    text.parse::<SocketAddr>()
        .or_else(|_| {
            text.parse::<IpAddr>()
                .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
        })
        .map_err(|_| bad.to_owned())
}

/// 指纹：64 个十六进制字符，允许中间有空格或冒号。空着表示不核对。
pub fn parse_pin(text: &str, s: &Strings) -> Result<Option<Fingerprint>, String> {
    let hex: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ':')
        .collect();
    if hex.is_empty() {
        return Ok(None);
    }
    parse_hex(&hex.to_ascii_lowercase())
        .map(Some)
        .map_err(|_| s.err_bad_pin.to_owned())
}

/// 连接表单。
#[derive(Debug, Clone, Default)]
pub struct ConnectForm {
    /// 被控端地址。
    pub addr: String,
    /// 要核对的指纹。
    pub pin: String,
    /// 是否通过服务器连接。
    pub via_server: bool,
    /// 服务器地址。
    pub server: String,
    /// 服务器指纹。
    pub server_pin: String,
    /// 被控端指纹。
    pub device: String,
}

impl ConnectForm {
    /// 解析成连接目标。
    pub fn target(&self, s: &Strings) -> Result<Target, String> {
        if self.via_server {
            if self.server.trim().is_empty() || self.device.trim().is_empty() {
                return Err(s.err_need_target.to_owned());
            }
            let server = Server {
                addr: parse_addr(&self.server, s.err_bad_server)?,
                pin: parse_pin(&self.server_pin, s)?,
            };
            let device = parse_pin(&self.device, s)?.ok_or_else(|| s.err_bad_pin.to_owned())?;
            Ok(Target::Device {
                server,
                device,
                policy: PathPolicy::Auto,
            })
        } else {
            if self.addr.trim().is_empty() {
                return Err(s.err_need_target.to_owned());
            }
            Ok(Target::Addr {
                addr: parse_addr(&self.addr, s.err_bad_addr)?,
                pin: parse_pin(&self.pin, s)?,
            })
        }
    }
}

/// 把网络层的错误说成用户看得懂的话，后面附上原始信息便于排查。
#[must_use]
pub fn explain(e: &NetError, s: &Strings) -> String {
    let detail = e.to_string();
    let summary = match e {
        NetError::Io(why) if why.contains("绑定") => s.err_port_in_use,
        NetError::Timeout(_) => s.err_timeout,
        NetError::Connect(why) if why.contains("密码") => s.err_wrong_password,
        NetError::Connect(why) if why.contains("锁定") => s.err_locked,
        NetError::Connect(why) if why.contains("其他设备") => s.err_busy,
        NetError::Connect(why) if why.contains("不在线") => s.err_offline,
        NetError::Connect(_) => s.err_refused,
        NetError::PinMismatch { .. } => s.err_pin_mismatch,
        NetError::Closed(_) => s.err_closed,
        NetError::Io(_) | NetError::Core(_) | NetError::Tls(_) | NetError::Protocol(_) => {
            s.err_other
        }
    };
    format!("{summary}（{detail}）")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::i18n::{EN, ZH};

    #[test]
    fn ports() {
        assert_eq!(parse_port(" 4433 ", &EN), Ok(4433));
        for bad in ["0", "65536", "x", ""] {
            assert_eq!(
                parse_port(bad, &EN),
                Err(EN.err_bad_port.to_owned()),
                "{bad}"
            );
        }
    }

    #[test]
    fn addresses() {
        let bad = "bad";
        assert_eq!(
            parse_addr("10.0.0.2:5000", bad),
            Ok("10.0.0.2:5000".parse().unwrap())
        );
        assert_eq!(
            parse_addr(" 10.0.0.2 ", bad),
            Ok("10.0.0.2:4433".parse().unwrap())
        );
        assert_eq!(parse_addr("[::1]:7", bad), Ok("[::1]:7".parse().unwrap()));
        assert_eq!(
            parse_addr("fe80::1", bad),
            Ok("[fe80::1]:4433".parse().unwrap())
        );
        assert_eq!(parse_addr("example.com:1", bad), Err("bad".to_owned()));
    }

    #[test]
    fn fingerprints() {
        let hex = "AB".repeat(32);
        let spaced: String = hex
            .chars()
            .collect::<Vec<_>>()
            .chunks(4)
            .map(|c| c.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(parse_pin(&spaced, &ZH).unwrap(), Some([0xAB; 32]));
        assert_eq!(
            parse_pin("ab:".repeat(32).as_str(), &ZH).unwrap(),
            Some([0xAB; 32])
        );
        assert_eq!(parse_pin("  ", &ZH), Ok(None));
        assert_eq!(parse_pin("abc", &ZH), Err(ZH.err_bad_pin.to_owned()));
    }

    #[test]
    fn connect_targets() {
        let fp = "cd".repeat(32);
        let mut form = ConnectForm::default();
        assert_eq!(form.target(&EN), Err(EN.err_need_target.to_owned()));
        form.addr = "10.0.0.2".into();
        assert_eq!(
            form.target(&EN),
            Ok(Target::Addr {
                addr: "10.0.0.2:4433".parse().unwrap(),
                pin: None
            })
        );
        form.pin = "nope".into();
        assert!(form.target(&EN).is_err());

        form.via_server = true;
        assert_eq!(form.target(&EN), Err(EN.err_need_target.to_owned()));
        form.server = "10.0.0.9:7000".into();
        form.device = fp.clone();
        assert_eq!(
            form.target(&EN),
            Ok(Target::Device {
                server: Server {
                    addr: "10.0.0.9:7000".parse().unwrap(),
                    pin: None
                },
                device: [0xCD; 32],
                policy: PathPolicy::Auto,
            })
        );
        form.server = "nope".into();
        assert_eq!(form.target(&EN), Err(EN.err_bad_server.to_owned()));
        form.server = "10.0.0.9:7000".into();
        form.server_pin = "zz".into();
        assert!(form.target(&EN).is_err());
        form.server_pin = String::new();
        form.device = " ".into();
        assert!(form.target(&EN).is_err());
        form.device = ":".into();
        assert_eq!(form.target(&EN), Err(EN.err_bad_pin.to_owned()));
    }

    #[test]
    fn errors_in_plain_language() {
        let cases = [
            (
                NetError::Io("绑定 0.0.0.0:1 失败：in use".into()),
                EN.err_port_in_use,
            ),
            (NetError::Io("disk".into()), EN.err_other),
            (NetError::Timeout("握手"), EN.err_timeout),
            (
                NetError::Connect("密码错误，或被控端需要密码".into()),
                EN.err_wrong_password,
            ),
            (
                NetError::Connect("被控端因多次认证失败暂时锁定".into()),
                EN.err_locked,
            ),
            (
                NetError::Connect("被控端正在被其他设备控制".into()),
                EN.err_busy,
            ),
            (NetError::Connect("设备不在线".into()), EN.err_offline),
            (NetError::Connect("refused".into()), EN.err_refused),
            (
                NetError::PinMismatch {
                    expected: "a".into(),
                    actual: "b".into(),
                },
                EN.err_pin_mismatch,
            ),
            (NetError::Closed("x".into()), EN.err_closed),
            (NetError::Protocol("x"), EN.err_other),
            (NetError::Tls("x".into()), EN.err_other),
            (NetError::Core(peerdesk_core::Error::BadUtf8), EN.err_other),
        ];
        for (e, summary) in cases {
            let text = explain(&e, &EN);
            assert!(text.starts_with(summary), "{text}");
            assert!(text.contains(&e.to_string()), "{text}");
        }
    }
}
