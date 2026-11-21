//! 远程窗口里的浮动助手：常用快捷键一键发送（按被控端的系统选 Ctrl 还是 Cmd 等）。
//!
//! 这里只有与界面无关的逻辑（预置快捷键、生成按键事件），有单元测试；窗口在 `viewer.rs`。

use peerdesk_core::input::InputEvent;
use peerdesk_core::proto::PeerOs;

use super::i18n::Strings;
use super::keys::{LEFT_ALT, LEFT_CTRL, LEFT_GUI, LEFT_SHIFT};

// 键盘页（0x07）的用途码。
const A: u16 = 0x04;
const C: u16 = 0x06;
const D: u16 = 0x07;
const L: u16 = 0x0F;
const Q: u16 = 0x14;
const S: u16 = 0x16;
const V: u16 = 0x19;
const W: u16 = 0x1A;
const Z: u16 = 0x1D;
const DIGIT_4: u16 = 0x21;
const ESCAPE: u16 = 0x29;
const TAB: u16 = 0x2B;
const SPACE: u16 = 0x2C;
const F4: u16 = 0x3D;
const F11: u16 = 0x44;
const PRINT_SCREEN: u16 = 0x46;
const DELETE: u16 = 0x4C;

/// 预置的快捷键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shortcut {
    /// Ctrl+Alt+Del（Windows 的安全选项界面需要系统服务才能触发，见无人值守）。
    CtrlAltDel,
    /// 开始菜单 / 启动台（Windows 键；macOS 用 Cmd+空格 打开聚焦搜索）。
    Start,
    /// 切换程序。
    SwitchApp,
    /// 关闭窗口。
    CloseWindow,
    /// 任务管理器 / 强制退出。
    TaskManager,
    /// 锁屏。
    Lock,
    /// 显示桌面。
    ShowDesktop,
    /// 截图。
    Screenshot,
    /// 复制。
    Copy,
    /// 粘贴。
    Paste,
    /// 全选。
    SelectAll,
    /// 撤销。
    Undo,
}

impl Shortcut {
    /// 助手里按这个顺序排列。
    pub const ALL: [Shortcut; 12] = [
        Shortcut::CtrlAltDel,
        Shortcut::Start,
        Shortcut::SwitchApp,
        Shortcut::CloseWindow,
        Shortcut::TaskManager,
        Shortcut::Lock,
        Shortcut::ShowDesktop,
        Shortcut::Screenshot,
        Shortcut::Copy,
        Shortcut::Paste,
        Shortcut::SelectAll,
        Shortcut::Undo,
    ];

    /// 按钮上的文字。
    #[must_use]
    pub const fn label(self, s: &Strings) -> &'static str {
        match self {
            Shortcut::CtrlAltDel => "Ctrl+Alt+Del",
            Shortcut::Start => s.sc_start,
            Shortcut::SwitchApp => s.sc_switch_app,
            Shortcut::CloseWindow => s.sc_close_window,
            Shortcut::TaskManager => s.sc_task_manager,
            Shortcut::Lock => s.sc_lock,
            Shortcut::ShowDesktop => s.sc_show_desktop,
            Shortcut::Screenshot => s.sc_screenshot,
            Shortcut::Copy => s.sc_copy,
            Shortcut::Paste => s.sc_paste,
            Shortcut::SelectAll => s.sc_select_all,
            Shortcut::Undo => s.sc_undo,
        }
    }

    /// 在这种系统上要同时按下的键（按顺序按下，倒序抬起）；这种系统上没有对应操作时为 `None`。
    /// 系统未知（对方是老版本）时按 Windows 处理。
    #[must_use]
    pub fn keys(self, os: PeerOs) -> Option<Vec<u16>> {
        let mac = os == PeerOs::Mac;
        let linux = os == PeerOs::Linux;
        // 编辑类快捷键：macOS 用 Cmd，其他用 Ctrl
        let primary = if mac { LEFT_GUI } else { LEFT_CTRL };
        let keys = match self {
            Shortcut::CtrlAltDel if mac => return None,
            Shortcut::CtrlAltDel => vec![LEFT_CTRL, LEFT_ALT, DELETE],
            Shortcut::Start if mac => vec![LEFT_GUI, SPACE],
            Shortcut::Start => vec![LEFT_GUI],
            Shortcut::SwitchApp if mac => vec![LEFT_GUI, TAB],
            Shortcut::SwitchApp => vec![LEFT_ALT, TAB],
            Shortcut::CloseWindow if mac => vec![LEFT_GUI, W],
            Shortcut::CloseWindow => vec![LEFT_ALT, F4],
            Shortcut::TaskManager if mac => vec![LEFT_GUI, LEFT_ALT, ESCAPE],
            Shortcut::TaskManager if linux => return None,
            Shortcut::TaskManager => vec![LEFT_CTRL, LEFT_SHIFT, ESCAPE],
            Shortcut::Lock if mac => vec![LEFT_CTRL, LEFT_GUI, Q],
            Shortcut::Lock => vec![LEFT_GUI, L],
            Shortcut::ShowDesktop if mac => vec![F11],
            Shortcut::ShowDesktop => vec![LEFT_GUI, D],
            Shortcut::Screenshot if mac => vec![LEFT_GUI, LEFT_SHIFT, DIGIT_4],
            Shortcut::Screenshot if linux => vec![PRINT_SCREEN],
            Shortcut::Screenshot => vec![LEFT_GUI, LEFT_SHIFT, S],
            Shortcut::Copy => vec![primary, C],
            Shortcut::Paste => vec![primary, V],
            Shortcut::SelectAll => vec![primary, A],
            Shortcut::Undo => vec![primary, Z],
        };
        Some(keys)
    }

    /// 发给被控端的按键事件：依次按下，再倒序抬起。
    #[must_use]
    pub fn events(self, os: PeerOs) -> Vec<InputEvent> {
        let Some(keys) = self.keys(os) else {
            return Vec::new();
        };
        let down = keys
            .iter()
            .map(|&code| InputEvent::Key { code, down: true });
        let up = keys
            .iter()
            .rev()
            .map(|&code| InputEvent::Key { code, down: false });
        down.chain(up).collect()
    }
}

/// 助手标题栏里显示的被控端系统。
#[must_use]
pub const fn os_label(os: PeerOs, s: &Strings) -> &'static str {
    match os {
        PeerOs::Windows => "Windows",
        PeerOs::Mac => "macOS",
        PeerOs::Linux => "Linux",
        PeerOs::Unknown => s.os_unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::i18n::{EN, ZH};

    fn key(code: u16, down: bool) -> InputEvent {
        InputEvent::Key { code, down }
    }

    #[test]
    fn chords_press_in_order_and_release_in_reverse() {
        assert_eq!(
            Shortcut::CtrlAltDel.events(PeerOs::Windows),
            vec![
                key(LEFT_CTRL, true),
                key(LEFT_ALT, true),
                key(DELETE, true),
                key(DELETE, false),
                key(LEFT_ALT, false),
                key(LEFT_CTRL, false),
            ]
        );
    }

    #[test]
    fn modifiers_follow_the_remote_system() {
        // 编辑类：macOS 用 Cmd，Windows/Linux/未知用 Ctrl
        assert_eq!(Shortcut::Copy.keys(PeerOs::Mac), Some(vec![LEFT_GUI, C]));
        for os in [PeerOs::Windows, PeerOs::Linux, PeerOs::Unknown] {
            assert_eq!(Shortcut::Paste.keys(os), Some(vec![LEFT_CTRL, V]));
        }
        assert_eq!(
            Shortcut::SwitchApp.keys(PeerOs::Mac),
            Some(vec![LEFT_GUI, TAB])
        );
        assert_eq!(
            Shortcut::SwitchApp.keys(PeerOs::Windows),
            Some(vec![LEFT_ALT, TAB])
        );
        assert_eq!(
            Shortcut::Lock.keys(PeerOs::Mac),
            Some(vec![LEFT_CTRL, LEFT_GUI, Q])
        );
        assert_eq!(Shortcut::Lock.keys(PeerOs::Linux), Some(vec![LEFT_GUI, L]));
        assert_eq!(
            Shortcut::Screenshot.keys(PeerOs::Linux),
            Some(vec![PRINT_SCREEN])
        );
        // macOS 没有 Ctrl+Alt+Del；Linux 没有统一的任务管理器快捷键
        assert!(Shortcut::CtrlAltDel.events(PeerOs::Mac).is_empty());
        assert_eq!(Shortcut::TaskManager.keys(PeerOs::Linux), None);
    }

    #[test]
    fn every_shortcut_has_a_label_and_works_somewhere() {
        for s in [&ZH, &EN] {
            for sc in Shortcut::ALL {
                assert!(!sc.label(s).is_empty());
                for os in [PeerOs::Windows, PeerOs::Mac, PeerOs::Linux, PeerOs::Unknown] {
                    if let Some(keys) = sc.keys(os) {
                        assert!(!keys.is_empty());
                        assert_eq!(sc.events(os).len(), keys.len() * 2);
                    }
                }
                assert!(sc.keys(PeerOs::Windows).is_some(), "{sc:?}");
            }
            assert_eq!(os_label(PeerOs::Mac, s), "macOS");
            assert_eq!(os_label(PeerOs::Windows, s), "Windows");
            assert_eq!(os_label(PeerOs::Linux, s), "Linux");
            assert!(!os_label(PeerOs::Unknown, s).is_empty());
        }
    }
}
