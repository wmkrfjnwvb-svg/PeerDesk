//! PeerDesk 服务端。
//!
//! 一个二进制承担：集群成员管理（节点之间用 UDP 心跳互联）、区域路由、
//! 内嵌的 React 管理页面与状态接口。信令和中继在 M3 加入，复用这里的集群与路由。

pub mod http;
pub mod json;
pub mod logging;
pub mod node;
pub mod status;

/// 构建时嵌入的管理页面文件：`(路径, 内容)`，按路径排序。
pub mod assets {
    include!(concat!(env!("OUT_DIR"), "/assets.rs"));
}

pub use node::Node;
