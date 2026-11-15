//! PeerDesk 客户端入口。逻辑都在库里，这里只接线。

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    ExitCode::from(peerdesk::run(&args, &mut out))
}
