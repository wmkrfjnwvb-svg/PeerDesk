//! PeerDesk 网络层：把核心库的协议跑在真实的 QUIC 连接上。
//!
//! - 视频、输入走 QUIC 不可靠数据报；控制消息和文件走 QUIC 可靠流。
//! - 每台设备有一张自签名证书，证书的 SHA-256 指纹就是设备身份；连接时双向出示证书，
//!   由上层决定是否信任对方的指纹（首次确认后固定）。
//! - 加密套件由 [`peerdesk_core::crypto::Policy`] 决定，映射到 TLS 1.3 的密码套件。
//!
//! 这一层只做 IO 和计时，协议决策都在 `peerdesk-core` 里。

/// 写一行日志。和 `log::info!` 等不同，内容总是先格式化好：日志器没打开时也一样，
/// 这样测试覆盖到的就是真实会写出的那行。日志都是低频的（连接事件、每秒统计），格式化的开销可以忽略。
///
/// 用法：`note!(info, "直连成功 {addr}")`，或者带目标 `note!(info, target: "peerdesk::stats", "…")`。
#[macro_export]
macro_rules! note {
    ($level:ident, target: $target:expr, $($arg:tt)+) => {{
        let line = format!($($arg)+);
        ::log::$level!(target: $target, "{line}");
    }};
    ($level:ident, $($arg:tt)+) => {{
        let line = format!($($arg)+);
        ::log::$level!("{line}");
    }};
}

#[cfg(feature = "media")]
pub mod audio;
#[cfg(feature = "media")]
pub mod bench;
pub mod browse;
pub mod candidates;
pub mod clipsync;
#[cfg(feature = "media")]
pub mod controller;
#[cfg(feature = "media")]
pub mod demo;
pub mod diag;
pub mod error;
pub mod files;
pub mod framelog;
pub mod fs_store;
pub mod gateway;
#[cfg(feature = "media")]
pub mod host;
#[cfg(feature = "media")]
mod host_record;
pub mod ice;
pub mod identity;
pub mod netcheck;
pub mod node;
#[cfg(feature = "portmap")]
pub mod portmap;
pub mod punch;
pub mod recorder;
pub mod rendezvous;
pub mod safety;
pub mod service;
pub mod sidechannel;
pub mod stream;
mod tls;
mod tls_verifier;

pub use error::NetError;
pub use identity::{Fingerprint, Identity};
pub use node::{Link, Node};
