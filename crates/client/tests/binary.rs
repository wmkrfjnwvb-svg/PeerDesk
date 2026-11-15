//! 对编译出的 `peerdesk` 二进制做冒烟测试。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_peerdesk");

#[test]
fn version_and_exit_codes() {
    let out = Command::new(BIN).arg("version").output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout)
        .unwrap()
        .starts_with("peerdesk "));

    let out = Command::new(BIN).output().unwrap();
    assert_eq!(out.status.code(), Some(2));

    let out = Command::new(BIN)
        .args(["sim", "--frames", "10"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8(out.stdout).unwrap().contains("送达 10"));
}
