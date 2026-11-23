//! 编译时记下提交号（`PEERDESK_COMMIT`）：界面、日志、崩溃报告里显示，用户一眼看出装的是哪一版。
fn main() {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short=7", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_owned());
    println!("cargo:rustc-env=PEERDESK_COMMIT={commit}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");
}
