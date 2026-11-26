//! 编译时记下提交号（`PEERDESK_COMMIT`）：界面、日志、崩溃报告里显示，用户一眼看出装的是哪一版。
//! Windows 上还把图标编进 exe（资源管理器、任务栏、开始菜单、卸载列表都用它），图标由
//! `scripts/make-icons.py` 生成，见 `packaging/icons/README.md`。
fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_icon()?;
    }
    Ok(())
}

/// 生成只含图标的资源脚本（用绝对路径，不依赖资源编译器的当前目录）并编进 exe。
#[cfg(windows)]
fn windows_icon() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let ico = manifest.join("../../packaging/icons/peerdesk.ico").canonicalize()?;
    println!("cargo:rerun-if-changed={}", ico.display());
    // 资源脚本里的路径用正斜杠，省得转义反斜杠；canonicalize 在 Windows 上会加 \\?\ 前缀，去掉
    let path = ico.display().to_string().replace('\\', "/");
    let path = path.trim_start_matches("//?/");
    let rc = std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("peerdesk.rc");
    std::fs::write(&rc, format!("1 ICON \"{path}\"\n"))?;
    // 找不到资源编译器也算失败：不能悄悄打出没有图标的包
    embed_resource::compile(&rc, embed_resource::NONE).manifest_required()?;
    Ok(())
}
