//! 表单的解析和错误的白话说明。

use std::net::{IpAddr, SocketAddr};

use peerdesk_core::signal::parse_code;
use peerdesk_net::identity::parse_hex;
use peerdesk_net::rendezvous::{Lookup, PathPolicy, Server};
use peerdesk_net::{Fingerprint, NetError};

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
    /// 解析成连接目标。设备码和指纹要通过 `server` 查找。
    pub fn target(
        &self,
        s: &Strings,
        server: Result<Option<Server>, String>,
    ) -> Result<Target, String> {
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
                server: server?.ok_or_else(|| s.err_need_server.to_owned())?,
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

/// 会话结束时给用户看的一句话：对端退出了就说“连接已断开：对方退出了 PeerDesk”，其他是“会话已结束：原因”。
#[must_use]
pub fn end_text(end: &str, s: &Strings) -> String {
    if end == peerdesk_net::node::END_PEER_QUIT {
        s.peer_quit.to_owned()
    } else {
        format!("{}：{end}", s.session_ended)
    }
}

/// 把网络层的错误说成用户看得懂的话，后面附上原始信息便于排查。
#[must_use]
pub fn explain(e: &NetError, s: &Strings) -> String {
    let detail = e.to_string();
    // 对端点了退出：说人话，不显示 closed by peer 这类原文
    if detail.contains(peerdesk_net::node::QUIT_REASON) {
        return s.peer_quit.to_owned();
    }
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
        let server = Server {
            addr: "10.0.0.9:7000".parse().unwrap(),
            pin: None,
        };
        let mut form = ConnectForm::default();
        assert_eq!(
            form.target(&EN, Ok(None)),
            Err(EN.err_need_target.to_owned())
        );
        form.addr = "10.0.0.2".into();
        assert_eq!(
            form.target(&EN, Ok(None)),
            Ok(Target::Addr {
                addr: "10.0.0.2:4433".parse().unwrap(),
                pin: None
            })
        );
        form.pin = "nope".into();
        assert!(form.target(&EN, Ok(None)).is_err());
        form.pin = String::new();
        form.addr = "nope".into();
        assert_eq!(form.target(&EN, Ok(None)), Err(EN.err_bad_addr.to_owned()));

        // 设备码：需要服务器
        form.addr = " 123 456 789 ".into();
        assert_eq!(form.remembered(), "123456789");
        assert_eq!(
            form.target(&EN, Ok(None)),
            Err(EN.err_need_server.to_owned())
        );
        assert_eq!(form.target(&EN, Err("bad".into())), Err("bad".to_owned()));
        assert_eq!(
            form.target(&EN, Ok(Some(server))),
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
            form.target(&EN, Ok(Some(server))),
            Ok(Target::Device {
                server,
                device: Lookup::Device([0xCD; 32]),
                policy: PathPolicy::Auto,
                account: String::new(),
            })
        );
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
