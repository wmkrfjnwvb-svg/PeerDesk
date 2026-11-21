//! 本机的局域网地址，显示给用户告诉控制端。实现在网络层，被控端登记时也要用。

pub use peerdesk_net::candidates::{lan_addresses, usable};
