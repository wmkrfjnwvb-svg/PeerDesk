//! PeerDesk 网络层：把核心库的协议跑在真实的 QUIC 连接上。
//!
//! - 视频、输入走 QUIC 不可靠数据报；控制消息和文件走 QUIC 可靠流。
//! - 每台设备有一张自签名证书，证书的 SHA-256 指纹就是设备身份；连接时双向出示证书，
//!   由上层决定是否信任对方的指纹（首次确认后固定）。
//! - 加密套件由 [`peerdesk_core::crypto::Policy`] 决定，映射到 TLS 1.3 的密码套件。
//!
//! 这一层只做 IO 和计时，协议决策都在 `peerdesk-core` 里。

pub mod bench;
pub mod controller;
pub mod demo;
pub mod error;
pub mod files;
pub mod fs_store;
pub mod host;
pub mod identity;
pub mod node;
pub mod rendezvous;
pub mod service;
pub mod stream;
mod tls;
mod tls_verifier;

pub use error::NetError;
pub use identity::{Fingerprint, Identity};
pub use node::{Link, Node};
