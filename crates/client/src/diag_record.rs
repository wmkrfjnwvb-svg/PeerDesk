//! 诊断记录（会话记录，见 `peerdesk_net::recorder`）的开关、目录和“访问”的串联。
//!
//! - 开关：默认开；数据目录里有 `records-off` 这个空文件就是关（和“允许别人连接”的开关同一个做法）。
//!   关掉之后不写任何文件，也不删已有的。
//! - 目录：数据目录下的 `records/`，被控端和控制端都写；本机保留最近 20 份或 200 MB。
//! - 访问：控制端从用户点连接到最终断开是一次“访问”，访问号不变；中间升级直连、断线重连会换新连接和新会话号，
//!   新的那份记录的文件头写上一份的会话号和原因，两份能接上。
//!
//! 这里只放逻辑，界面接线（开关控件）在 `gui/app.rs`。

use std::io;
use std::path::Path;

use peerdesk_core::record::{Previous, PreviousReason};
use peerdesk_net::recorder::{new_visit_id, RecordSetup, Retention, ViewRecordSetup};

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

/// 控制端的一次访问：访问号，以及上一份记录的会话号和“为什么有下一份”。
#[derive(Debug)]
pub struct Visit {
    id: Option<[u8; 16]>,
    last_session: Option<[u8; 16]>,
    pending: Option<PreviousReason>,
}

impl Visit {
    /// 开始一次访问。生成不出访问号（系统随机数不可用）时写日志，这次访问不记录。
    #[must_use]
    pub fn new() -> Self {
        let id = new_visit_id()
            .inspect_err(|e| log::warn!("生成访问号失败，这次访问不记录会话：{e}"))
            .ok();
        Self {
            id,
            last_session: None,
            pending: None,
        }
    }

    /// 一次新的会话（新连接握手完成）开始：交出这份记录的设置，并记下它的会话号给下一份用。
    /// 开关关着、或没有访问号时为 `None`。
    pub fn begin(
        &mut self,
        session_id: Option<[u8; 16]>,
        data_dir: &Path,
    ) -> Option<ViewRecordSetup> {
        let previous = self
            .last_session
            .zip(self.pending.take())
            .map(|(session_id, reason)| Previous { session_id, reason });
        self.last_session = session_id;
        let setup = setup(data_dir)?;
        Some(ViewRecordSetup {
            setup,
            visit_id: self.id?,
            previous,
        })
    }

    /// 走中继时打通了直连，要在直连上重新开始。
    pub const fn note_upgrade(&mut self) {
        self.pending = Some(PreviousReason::Upgraded);
    }

    /// 网络断了，要重连。
    pub const fn note_lost(&mut self) {
        self.pending = Some(PreviousReason::Reconnected);
    }
}

impl Default for Visit {
    fn default() -> Self {
        Self::new()
    }
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

    #[test]
    fn a_visit_chains_its_sessions() {
        let dir = temp("visit");
        let mut visit = Visit::new();
        // 第一份：没有上一份
        let first = visit.begin(Some([1; 16]), &dir).unwrap();
        assert_eq!(first.previous, None);
        assert_ne!(first.visit_id, [0; 16]);
        // 升级直连：第二份接着第一份
        visit.note_upgrade();
        let second = visit.begin(Some([2; 16]), &dir).unwrap();
        assert_eq!(second.visit_id, first.visit_id, "同一次访问");
        assert_eq!(
            second.previous,
            Some(Previous {
                session_id: [1; 16],
                reason: PreviousReason::Upgraded
            })
        );
        // 断线重连：第三份接着第二份；原因只用一次
        visit.note_lost();
        let third = visit.begin(Some([3; 16]), &dir).unwrap();
        assert_eq!(third.previous.unwrap().session_id, [2; 16]);
        assert_eq!(third.previous.unwrap().reason, PreviousReason::Reconnected);
        let again = visit.begin(Some([4; 16]), &dir).unwrap();
        assert_eq!(again.previous, None);
        // 会话号导不出来：下一份没有上一份可接
        visit.begin(None, &dir).unwrap();
        visit.note_lost();
        assert_eq!(visit.begin(Some([5; 16]), &dir).unwrap().previous, None);
        // 开关关着：不给设置，但串联的状态照常走
        set_enabled(&dir, false).unwrap();
        assert!(visit.begin(Some([6; 16]), &dir).is_none());
        // 没有访问号：不记录
        let mut broken = Visit::new();
        broken.id = None;
        set_enabled(&dir, true).unwrap();
        assert!(broken.begin(Some([7; 16]), &dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
