//! PeerDesk 服务端入口。逻辑都在库里，这里只接线。

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match peerdesk_server::node::launch(&args, &mut std::io::stdout().lock()) {
        Ok(node) => {
            node.wait();
            ExitCode::SUCCESS
        }
        Err(code) => ExitCode::from(code),
    }
}
