//! 对编译出的 `peerdesk-server` 二进制做冒烟测试。
#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_peerdesk-server");

fn run(args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN).args(args).output().unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
    )
}

fn temp_config(name: &str, body: &str) -> String {
    let path = std::env::temp_dir().join(format!("peerdesk-server-{}-{name}", std::process::id()));
    std::fs::write(&path, body).unwrap();
    path.to_string_lossy().into_owned()
}

const GOOD: &str = "node_id = 1\nregion = \"us\"\nhttp_listen = \"127.0.0.1:0\"\n\
                    cluster_listen = \"127.0.0.1:0\"\nadvertise = \"127.0.0.1:1\"\n";

#[test]
fn usage_errors() {
    for args in [
        vec![],
        vec!["a", "b"],
        vec!["a", "--exit-after-ms", "soon"],
        vec!["a", "--nope", "1"],
    ] {
        let (code, text) = run(&args);
        assert_eq!(code, 2, "{args:?}");
        assert!(!text.is_empty());
    }
}

#[test]
fn startup_failures() {
    let (code, text) = run(&["/nonexistent/server.toml"]);
    assert_eq!(code, 1);
    assert!(text.contains("读取"));
    let path = temp_config("bad.toml", "node_id = 0");
    let (code, text) = run(&[&path]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(code, 1);
    assert!(text.contains("配置错误"));
}

#[test]
fn starts_serves_and_exits_cleanly() {
    let path = temp_config("good.toml", GOOD);
    let (code, text) = run(&[&path, "--exit-after-ms", "300"]);
    std::fs::remove_file(&path).unwrap();
    assert_eq!(code, 0, "{text}");
    assert!(text.contains("已启动"));
    assert!(text.contains("http://127.0.0.1:"));
}
