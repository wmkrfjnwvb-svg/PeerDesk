//! 诊断记录（会话记录，见 `peerdesk_net::recorder`）的开关、目录和“访问”的串联。
//!
//! - 开关：默认开；数据目录里有 `records-off` 这个空文件就是关（和“允许别人连接”的开关同一个做法）。
//!   关掉之后不写任何文件，也不删已有的。
//! - 目录：数据目录下的 `records/`，被控端和控制端都写；本机保留最近 20 份或 200 MB。
//! - 访问：控制端从用户点连接到最终断开是一次“访问”（`peerdesk_net::viewer::Visit`）。
//!
//! 这里只放逻辑，界面接线（开关控件）在 `gui/app.rs`。

use std::io;
use std::path::Path;

use peerdesk_net::recorder::{RecordSetup, Retention};

/// 关闭记录的标记文件名（在数据目录里）。
pub const OFF_FILE: &str = "records-off";
/// 记录目录名（在数据目录里）。
pub const DIR: &str = "records";

/// 记录是不是开着。
#[must_use]
pub fn enabled(data_dir: &Path) -> bool {
    !data_dir.join(OFF_FILE).exists()
}

/// 打开或关闭记录，立即生效（下一次会话起）。
pub fn set_enabled(data_dir: &Path, on: bool) -> io::Result<()> {
    let marker = data_dir.join(OFF_FILE);
    if on {
        match std::fs::remove_file(&marker) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            Ok(()) | Err(_) => Ok(()),
        }
    } else {
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(&marker, b"")
    }
}

/// 被控端会话的记录设置；开关关着时为 `None`。
#[must_use]
pub fn setup(data_dir: &Path) -> Option<RecordSetup> {
    enabled(data_dir).then(|| RecordSetup {
        dir: data_dir.join(DIR),
        retention: Retention::default(),
        program_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        commit: Some(env!("PEERDESK_COMMIT").to_owned()),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn temp(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("peerdesk-diagrec-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_switch_defaults_to_on_and_is_remembered() {
        let dir = temp("switch");
        assert!(enabled(&dir));
        let s = setup(&dir).unwrap();
        assert_eq!(s.dir, dir.join(DIR));
        assert!(s.commit.is_some() && s.program_version.is_some());
        set_enabled(&dir, false).unwrap();
        assert!(!enabled(&dir) && setup(&dir).is_none());
        set_enabled(&dir, false).unwrap();
        set_enabled(&dir, true).unwrap();
        assert!(enabled(&dir));
        // 已经是开着：再开一次不是错误
        set_enabled(&dir, true).unwrap();
        // 数据目录还不存在时关掉也行（先建目录）
        let fresh = dir.join("not-yet");
        set_enabled(&fresh, false).unwrap();
        assert!(!enabled(&fresh));
        // 数据目录是个文件：关不了，报错不装作成功
        let file = dir.join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(set_enabled(&file.join("sub"), false).is_err());
        // 标记文件是个目录：开不了（删不掉），报错
        std::fs::create_dir_all(dir.join(OFF_FILE).join("x")).unwrap();
        assert!(set_enabled(&dir, true).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
