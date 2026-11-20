//! 平台层的错误。

use std::fmt;

/// 平台层的错误。说明文字是中文，可以直接给用户看。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// 这个系统或桌面环境不支持（例如 Wayland）。
    Unsupported(String),
    /// 缺少系统权限（macOS 的“屏幕录制”“辅助功能”），说明里写着去哪里授权。
    Permission(String),
    /// 没有这个显示器。
    NoDisplay {
        /// 要求的序号。
        index: usize,
        /// 实际有几个显示器。
        count: usize,
    },
    /// 参数不合法。
    InvalidOptions(String),
    /// 系统调用失败。
    Os(String),
    /// 采集中断（例如 Windows 切换到安全桌面、分辨率改变），重新打开即可恢复。
    Lost(String),
    /// 等待超时。
    Timeout(String),
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(why) => write!(f, "不支持：{why}"),
            Self::Permission(why) => write!(f, "缺少权限：{why}"),
            Self::NoDisplay { index, count } => {
                write!(f, "没有第 {index} 个显示器（共 {count} 个，从 0 开始编号）")
            }
            Self::InvalidOptions(why) => write!(f, "参数错误：{why}"),
            Self::Os(why) => write!(f, "系统调用失败：{why}"),
            Self::Lost(why) => write!(f, "采集中断：{why}"),
            Self::Timeout(why) => write!(f, "超时：{why}"),
        }
    }
}

impl std::error::Error for PlatformError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_readable() {
        let cases = [
            (
                PlatformError::Unsupported("Wayland".into()),
                "不支持：Wayland",
            ),
            (PlatformError::Permission("x".into()), "缺少权限：x"),
            (
                PlatformError::NoDisplay { index: 3, count: 1 },
                "没有第 3 个显示器（共 1 个，从 0 开始编号）",
            ),
            (PlatformError::InvalidOptions("x".into()), "参数错误：x"),
            (PlatformError::Os("x".into()), "系统调用失败：x"),
            (PlatformError::Lost("x".into()), "采集中断：x"),
            (PlatformError::Timeout("x".into()), "超时：x"),
        ];
        for (err, text) in cases {
            assert_eq!(err.to_string(), text);
            let boxed: Box<dyn std::error::Error> = Box::new(err);
            assert_eq!(boxed.to_string(), text);
        }
    }
}
