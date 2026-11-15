//! PeerDesk 核心库。
//!
//! 这里的所有代码都不做 IO、不读系统时钟、不用全局随机数：时间和随机性由调用方传入。
//! 因此同一份逻辑既能跑在真实网络上，也能在 [`sim`] 里按固定种子确定性地重放。
//!
//! 模块一览：
//! - [`wire`] / [`proto`]：线上格式的编解码。
//! - [`crypto`]：可配置的加密套件、策略与协商。
//! - [`session`] / [`auth`]：会话握手状态机、密码授权与失败限制。
//! - [`transport`]：视频帧的分片与按截止时间重组。
//! - [`connect`] / [`probe`]：网络画像与直连方法规划、硬件档位。
//! - [`region`] / [`cluster`]：区域优先的节点选择、服务端集群成员管理。
//! - [`input`]：输入事件与冗余传输。
//! - [`file`]：文件传输协议与断点续传。
//! - [`trace`]：业务链路追踪。
//! - [`sim`] / [`scenario`]：确定性网络仿真与端到端场景。
//! - [`config`]：客户端与服务端配置。

pub mod adaptive;
pub mod auth;
pub mod cluster;
pub mod codec;
pub mod config;
pub mod connect;
pub mod crypto;
pub mod desktop_sim;
pub mod error;
pub mod file;
pub mod input;
pub mod probe;
pub mod proto;
pub mod region;
pub mod scenario;
pub mod screen;
pub mod session;
pub mod sim;
pub mod time;
pub mod trace;
pub mod transport;
pub mod wire;

pub use error::Error;
pub use time::Time;

/// 版本号，来自 Cargo 包版本。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
