//! 连接表单和服务器设置的解析。连接失败、会话结束怎么说见 `reasons.rs`。

use std::net::{IpAddr, SocketAddr};

use peerdesk_core::signal::parse_code;
use peerdesk_net::identity::parse_hex;
use peerdesk_net::rendezvous::{Lookup, PathPolicy, Server};
use peerdesk_net::Fingerprint;

use super::i18n::Strings;

/// 没写端口时用的端口。
pub const DEFAULT_PORT: u16 = 4433;

/// 控制端要连到哪里（和手机端共用，见 `peerdesk_net::viewer`）。
pub use peerdesk_net::viewer::ViewTarget as Target;

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
    /// 对方的设备码，或者 `地址:端口`，或者设备指纹。
    pub addr: String,
    /// 要核对的指纹（直连地址时可选）。
    pub pin: String,
}

impl ConnectForm {
    /// 解析成连接目标。设备码和指纹要通过 `server` 查找（没填自己的服务器时是官方服务器；
    /// 自己的服务器填错了是错误说明，只有按设备码连接时才报，直接连 IP 不受影响）。
    pub fn target(&self, s: &Strings, server: Result<Server, String>) -> Result<Target, String> {
        let text = self.addr.trim();
        if text.is_empty() {
            return Err(s.err_need_target.to_owned());
        }
        let lookup = parse_code(text).map(Lookup::Code).or_else(|| {
            let hex: String = text
                .chars()
                .filter(|c| !c.is_whitespace() && *c != ':')
                .collect();
            (hex.len() == 64)
                .then(|| parse_hex(&hex.to_ascii_lowercase()).ok())
                .flatten()
                .map(Lookup::Device)
        });
        match lookup {
            Some(device) => Ok(Target::Device {
                server: server?,
                device,
                policy: PathPolicy::Auto,
                account: String::new(),
            }),
            None => Ok(Target::Addr {
                addr: parse_addr(text, s.err_bad_addr)?,
                pin: parse_pin(&self.pin, s)?,
            }),
        }
    }

    /// 记进“最近连接”的文字：设备码去掉空格，其他原样。
    #[must_use]
    pub fn remembered(&self) -> String {
        let text = self.addr.trim();
        parse_code(text).map_or_else(|| text.to_owned(), |c| c.to_string())
    }
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
        let server = Server {
            addr: "10.0.0.9:7000".parse().unwrap(),
            pin: None,
        };
        let mut form = ConnectForm::default();
        assert_eq!(
            form.target(&EN, Ok(server)),
            Err(EN.err_need_target.to_owned())
        );
        form.addr = "10.0.0.2".into();
        // 直接连 IP：自己的服务器填错了也不影响
        assert_eq!(
            form.target(&EN, Err("bad".into())),
            Ok(Target::Addr {
                addr: "10.0.0.2:4433".parse().unwrap(),
                pin: None
            })
        );
        form.pin = "nope".into();
        assert!(form.target(&EN, Ok(server)).is_err());
        form.pin = String::new();
        form.addr = "nope".into();
        assert_eq!(
            form.target(&EN, Ok(server)),
            Err(EN.err_bad_addr.to_owned())
        );

        // 设备码：通过服务器找；自己的服务器填错了就说出来
        form.addr = " 123 456 789 ".into();
        assert_eq!(form.remembered(), "123456789");
        assert_eq!(form.target(&EN, Err("bad".into())), Err("bad".to_owned()));
        assert_eq!(
            form.target(&EN, Ok(server)),
            Ok(Target::Device {
                server,
                device: Lookup::Code(123_456_789),
                policy: PathPolicy::Auto,
                account: String::new(),
            })
        );
        // 指纹也可以
        form.addr = fp.to_uppercase();
        assert_eq!(form.remembered(), fp.to_uppercase());
        assert_eq!(
            form.target(&EN, Ok(server)),
            Ok(Target::Device {
                server,
                device: Lookup::Device([0xCD; 32]),
                policy: PathPolicy::Auto,
                account: String::new(),
            })
        );
    }
}
