//! 统一错误类型。每个成员都对应一种可定位的失败原因，不使用笼统的“未知错误”。

use std::fmt;

/// 核心库的错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 数据不够长。
    Truncated { need: usize, have: usize },
    /// 解码完成后还有多余字节。
    TrailingBytes(usize),
    /// 包头魔数不对。
    BadMagic(u8),
    /// 协议版本不支持。
    UnsupportedVersion(u8),
    /// 未知通道号。
    UnknownChannel(u8),
    /// 未知消息类型。
    UnknownMessage(u8),
    /// 未知加密套件编号。
    UnknownSuite(u8),
    /// 未知拒绝原因。
    UnknownReason(u8),
    /// 载荷超过上限。
    PayloadTooLarge { len: usize, max: usize },
    /// 字符串超过上限。
    StringTooLong { len: usize, max: usize },
    /// 字符串不是合法 UTF-8。
    BadUtf8,
    /// 列表元素过多。
    TooManyItems { len: usize, max: usize },
    /// 分片序号或数量非法。
    InvalidFragment { index: u16, count: u16 },
    /// 同一对象的两份数据互相矛盾。
    Inconsistent(&'static str),
    /// 参数非法。
    InvalidArgument(&'static str),
    /// 密钥长度不对。
    KeyLength { need: usize, have: usize },
    /// 加密失败。
    SealFailed,
    /// 解密或认证失败。
    OpenFailed,
    /// 两端没有共同的加密套件。
    NoCommonSuite,
    /// 加密策略为空。
    EmptyPolicy,
    /// 配置里的加密套件名不认识。
    UnknownSuiteName(String),
    /// 当前状态下不该收到这种消息。
    UnexpectedMessage {
        state: &'static str,
        msg: &'static str,
    },
    /// 配置错误。
    Config(String),
    /// 文件路径不安全（绝对路径、上级目录、保留名等）。
    UnsafePath(String),
    /// 读写存储失败。
    Storage(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Truncated { need, have } => {
                write!(f, "数据被截断：需要 {need} 字节，只有 {have} 字节")
            }
            Error::TrailingBytes(n) => write!(f, "解码后剩余 {n} 个多余字节"),
            Error::BadMagic(b) => write!(f, "包头魔数错误：0x{b:02x}"),
            Error::UnsupportedVersion(v) => write!(f, "不支持的协议版本：{v}"),
            Error::UnknownChannel(c) => write!(f, "未知通道：{c}"),
            Error::UnknownMessage(m) => write!(f, "未知消息类型：{m}"),
            Error::UnknownSuite(s) => write!(f, "未知加密套件编号：{s}"),
            Error::UnknownReason(r) => write!(f, "未知拒绝原因：{r}"),
            Error::PayloadTooLarge { len, max } => {
                write!(f, "载荷过大：{len} 字节，上限 {max} 字节")
            }
            Error::StringTooLong { len, max } => {
                write!(f, "字符串过长：{len} 字节，上限 {max} 字节")
            }
            Error::BadUtf8 => write!(f, "字符串不是合法的 UTF-8"),
            Error::TooManyItems { len, max } => write!(f, "元素过多：{len} 个，上限 {max} 个"),
            Error::InvalidFragment { index, count } => {
                write!(f, "分片非法：序号 {index}，总数 {count}")
            }
            Error::Inconsistent(what) => write!(f, "数据前后矛盾：{what}"),
            Error::InvalidArgument(what) => write!(f, "参数非法：{what}"),
            Error::KeyLength { need, have } => {
                write!(f, "密钥长度错误：需要 {need} 字节，实际 {have} 字节")
            }
            Error::SealFailed => write!(f, "加密失败"),
            Error::OpenFailed => write!(f, "解密或认证失败"),
            Error::NoCommonSuite => write!(f, "两端没有共同的加密套件"),
            Error::EmptyPolicy => write!(f, "加密策略不能为空"),
            Error::UnknownSuiteName(name) => write!(f, "未知的加密套件名：{name}"),
            Error::UnexpectedMessage { state, msg } => {
                write!(f, "状态 {state} 下不应收到消息 {msg}")
            }
            Error::Config(why) => write!(f, "配置错误：{why}"),
            Error::UnsafePath(path) => write!(f, "不安全的文件路径：{path}"),
            Error::Storage(why) => write!(f, "存储读写失败：{why}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_a_message() {
        let all = [
            Error::Truncated { need: 4, have: 1 },
            Error::TrailingBytes(3),
            Error::BadMagic(0xff),
            Error::UnsupportedVersion(9),
            Error::UnknownChannel(9),
            Error::UnknownMessage(9),
            Error::UnknownSuite(9),
            Error::UnknownReason(9),
            Error::PayloadTooLarge { len: 2, max: 1 },
            Error::StringTooLong { len: 2, max: 1 },
            Error::BadUtf8,
            Error::TooManyItems { len: 2, max: 1 },
            Error::InvalidFragment { index: 2, count: 1 },
            Error::Inconsistent("x"),
            Error::InvalidArgument("x"),
            Error::KeyLength { need: 32, have: 1 },
            Error::SealFailed,
            Error::OpenFailed,
            Error::NoCommonSuite,
            Error::EmptyPolicy,
            Error::UnknownSuiteName("x".into()),
            Error::UnexpectedMessage {
                state: "Idle",
                msg: "Accept",
            },
            Error::Config("x".into()),
            Error::UnsafePath("x".into()),
            Error::Storage("x".into()),
        ];
        for e in all {
            let text = e.to_string();
            assert!(!text.is_empty(), "{e:?}");
            let boxed: Box<dyn std::error::Error> = Box::new(e);
            assert!(boxed.source().is_none());
        }
    }
}
