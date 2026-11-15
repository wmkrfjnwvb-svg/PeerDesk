//! 网络层错误。

use std::fmt;

/// 网络层的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// 核心库报告的协议错误。
    Core(peerdesk_core::Error),
    /// 本机 IO 失败（绑定端口、读写身份文件等）。
    Io(String),
    /// TLS 配置或证书问题。
    Tls(String),
    /// 连接建立失败。
    Connect(String),
    /// 连接已关闭。
    Closed(String),
    /// 对端的指纹与预期不符。
    PinMismatch {
        /// 预期的指纹。
        expected: String,
        /// 实际的指纹。
        actual: String,
    },
    /// 对端违反了协议。
    Protocol(&'static str),
    /// 等待超时。
    Timeout(&'static str),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Core(e) => write!(f, "{e}"),
            NetError::Io(why) => write!(f, "IO 错误：{why}"),
            NetError::Tls(why) => write!(f, "TLS 错误：{why}"),
            NetError::Connect(why) => write!(f, "连接失败：{why}"),
            NetError::Closed(why) => write!(f, "连接已关闭：{why}"),
            NetError::PinMismatch { expected, actual } => write!(
                f,
                "对端身份与预期不符：预期指纹 {expected}，实际 {actual}。可能连错了设备，也可能有人在中间冒充"
            ),
            NetError::Protocol(what) => write!(f, "对端违反协议：{what}"),
            NetError::Timeout(what) => write!(f, "超时：{what}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<peerdesk_core::Error> for NetError {
    fn from(e: peerdesk_core::Error) -> Self {
        NetError::Core(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_a_message() {
        let all = [
            NetError::from(peerdesk_core::Error::BadUtf8),
            NetError::Io("x".into()),
            NetError::Tls("x".into()),
            NetError::Connect("x".into()),
            NetError::Closed("x".into()),
            NetError::PinMismatch {
                expected: "aa".into(),
                actual: "bb".into(),
            },
            NetError::Protocol("x"),
            NetError::Timeout("x"),
        ];
        for e in all {
            assert!(!e.to_string().is_empty());
            let boxed: Box<dyn std::error::Error> = Box::new(e);
            assert!(boxed.source().is_none());
        }
    }
}
