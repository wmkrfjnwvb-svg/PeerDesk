//! 关闭退到托盘的状态逻辑（纯逻辑，不碰系统）。系统托盘本身在 `tray.rs`。
//!
//! 产品规则（产品负责人 2026-11-27）：关闭窗口不退出，退到托盘（菜单栏）继续运行，被控端照常可被连接；
//! 只有用户从托盘菜单点“退出”才真正退出（退出码 0）。被杀、崩溃（退出码非 0）由进程守护拉起，
//! 见 `crate::autostart` 和 `crate::guard`。
//!
//! 做法和 Tailscale、RustDesk 一致：关闭只是隐藏；第一次关闭时提示一次“还在后台运行”。
//! 和常规不同的一点：提示用系统通知而不是窗口里的提示，因为窗口马上就隐藏了，窗口里的提示没人看得到。
//!
//! 失败要诚实：系统没有托盘（例如没装 AppIndicator 扩展的 GNOME）时，托盘图标就是不存在的，
//! 这时关闭窗口只能是退出，否则程序隐藏了、用户却找不到也关不掉。此时不静默降级：启动时告诉用户。

use std::path::Path;

use super::i18n::Strings;

/// 记录“已经提示过一次”的标记文件（在数据目录里）。
pub const HINT_FILE: &str = "tray-hint-shown";

/// 托盘菜单发给主窗口的命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// 打开（显示）主窗口。左键点托盘图标、第二次启动程序也是这个命令。
    Open,
    /// 断开当前连着的控制端，被控继续等待下一个。
    Disconnect,
    /// 退出程序。
    Quit,
}

/// 托盘图标的样子：点灰色表示没人连，青绿色表示有人连着（见 `packaging/icons/README.md`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    /// 没人连。
    Idle,
    /// 有人连着。
    Active,
}

/// 按有没有人连着选图标。
#[must_use]
pub const fn icon_kind(connected: bool) -> IconKind {
    if connected {
        IconKind::Active
    } else {
        IconKind::Idle
    }
}

/// 鼠标悬停时的提示文字。
#[must_use]
pub const fn tooltip(connected: bool, s: &Strings) -> &'static str {
    if connected {
        s.tray_tip_active
    } else {
        s.tray_tip_idle
    }
}

/// 托盘菜单的一项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuEntry {
    /// 点了发出的命令。
    pub command: TrayCommand,
    /// 显示的文字。
    pub label: &'static str,
}

/// 菜单：打开主窗口；有人连着时多一项“断开连接”；退出。
#[must_use]
pub fn menu(connected: bool, s: &Strings) -> Vec<MenuEntry> {
    let mut items = vec![MenuEntry {
        command: TrayCommand::Open,
        label: s.tray_open,
    }];
    if connected {
        items.push(MenuEntry {
            command: TrayCommand::Disconnect,
            label: s.tray_disconnect,
        });
    }
    items.push(MenuEntry {
        command: TrayCommand::Quit,
        label: s.tray_quit,
    });
    items
}

/// 用户点了窗口的关闭按钮，该怎么办。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// 隐藏窗口，程序继续运行。
    Hide,
    /// 这次先不隐藏，窗口留着（第一次关闭时提示发不出去：用户必须看到提示才能知道程序还在）。
    Stay,
    /// 真的关闭，程序退出。
    Quit,
}

/// 窗口要做的事（处理托盘命令的结果）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowAction {
    /// 显示并带到最前。
    Show,
    /// 断开控制端。
    Kick,
    /// 关闭窗口并退出程序。
    Exit,
    /// 什么都不做（例如没人连着时收到“断开连接”）。
    Nothing,
}

/// 窗口生命周期：关闭、退出、首次提示。
#[derive(Debug)]
pub struct Lifecycle {
    tray: bool,
    quitting: bool,
    hint_shown: bool,
}

impl Lifecycle {
    /// `tray`：托盘图标建好了；`hint_shown`：以前已经提示过。
    #[must_use]
    pub const fn new(tray: bool, hint_shown: bool) -> Self {
        Self {
            tray,
            quitting: false,
            hint_shown,
        }
    }

    /// 有托盘（关闭 = 隐藏）。
    #[must_use]
    pub const fn has_tray(&self) -> bool {
        self.tray
    }

    /// 已经决定退出了。
    #[must_use]
    pub const fn quitting(&self) -> bool {
        self.quitting
    }

    /// 窗口收到关闭请求。第一次需要提示时调用 `hint`（返回提示有没有发出去）。
    ///
    /// 提示不管发没发出去都只尝试一次：发不出去时这次窗口留着，下一次关闭才隐藏，
    /// 而不是每次关闭都卡在“发不出提示”上。
    pub fn on_close_requested(&mut self, hint: impl FnOnce() -> bool) -> CloseAction {
        if self.quitting || !self.tray {
            self.quitting = true;
            return CloseAction::Quit;
        }
        if self.hint_shown {
            return CloseAction::Hide;
        }
        self.hint_shown = true;
        if hint() {
            CloseAction::Hide
        } else {
            CloseAction::Stay
        }
    }

    /// 第一次关闭的提示是否还没发过（给界面决定要不要写标记文件）。
    #[must_use]
    pub const fn hint_pending(&self) -> bool {
        self.tray && !self.hint_shown
    }

    /// 收到托盘命令。`connected` 是现在有没有人连着。
    pub fn on_command(&mut self, command: TrayCommand, connected: bool) -> WindowAction {
        match command {
            TrayCommand::Open => WindowAction::Show,
            TrayCommand::Disconnect if connected => WindowAction::Kick,
            TrayCommand::Disconnect => WindowAction::Nothing,
            TrayCommand::Quit => {
                self.quitting = true;
                WindowAction::Exit
            }
        }
    }
}

impl TrayCommand {
    /// 菜单项的标识（托盘库回调里认命令用）。
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Disconnect => "disconnect",
            Self::Quit => "quit",
        }
    }

    /// 由菜单项标识找回命令。
    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        [Self::Open, Self::Disconnect, Self::Quit]
            .into_iter()
            .find(|c| c.id() == id)
    }
}

const IDLE_32: &[u8] = include_bytes!("../../../../packaging/icons/tray-idle-32.png");
const ACTIVE_32: &[u8] = include_bytes!("../../../../packaging/icons/tray-active-32.png");

/// 托盘图标的 PNG（`packaging/icons` 里的 32 像素版，高分屏下系统自己缩小）。
#[must_use]
pub const fn icon_png(kind: IconKind) -> &'static [u8] {
    match kind {
        IconKind::Idle => IDLE_32,
        IconKind::Active => ACTIVE_32,
    }
}

/// macOS 菜单栏的模板图（纯黑加透明，系统按深浅色着色）：只用在没人连的时候，
/// 有人连着时模板图画不出青绿色，改用带颜色的 `tray-active`。
pub const MAC_TEMPLATE_PNG: &[u8] =
    include_bytes!("../../../../packaging/icons/tray-template@2x.png");

/// 解码 PNG，得到 RGBA 像素和宽高。
pub fn decode_rgba(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader.output_buffer_size().ok_or("图标尺寸异常")?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| p.iter().copied().chain(std::iter::once(255)))
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|p| {
                let g = p.first().copied().unwrap_or(0);
                [g, g, g, p.get(1).copied().unwrap_or(255)]
            })
            .collect(),
        png::ColorType::Indexed => return Err("不支持索引色图标".to_owned()),
    };
    Ok((rgba, info.width, info.height))
}

/// RGBA 转成 StatusNotifierItem 要的 ARGB（每个像素 A、R、G、B 四个字节）。
#[must_use]
pub fn rgba_to_argb(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|p| {
            let at = |i: usize| p.get(i).copied().unwrap_or(0);
            [at(3), at(0), at(1), at(2)]
        })
        .collect()
}

/// 以前是不是已经提示过。
#[must_use]
pub fn hint_seen(dir: &Path) -> bool {
    dir.join(HINT_FILE).exists()
}

/// 记下已经提示过。
pub fn mark_hint_seen(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join(HINT_FILE), b"")
}

/// 系统通知要运行的命令（程序名和参数）。
pub type Command = Vec<String>;

/// 发一条系统通知的命令：Linux 用 `notify-send`，macOS 用 `osascript`，Windows 用 PowerShell 的气泡提示。
/// 不新增依赖，也不碰任何通知服务的私有接口。`title`、`body` 不能含引号（调用的都是我们自己的固定文字）。
#[must_use]
pub fn notify_command(os: NotifyOs, title: &str, body: &str) -> Command {
    let strs = |parts: &[&str]| parts.iter().map(|p| (*p).to_owned()).collect();
    match os {
        NotifyOs::Linux => strs(&[
            "notify-send",
            "-a",
            "PeerDesk",
            "-i",
            "peerdesk",
            title,
            body,
        ]),
        NotifyOs::MacOs => strs(&[
            "osascript",
            "-e",
            &format!("display notification \"{body}\" with title \"{title}\""),
        ]),
        NotifyOs::Windows => strs(&[
            "powershell",
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &format!(
                "Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
                 $n = New-Object System.Windows.Forms.NotifyIcon; \
                 $n.Icon = [System.Drawing.SystemIcons]::Information; $n.Visible = $true; \
                 $n.ShowBalloonTip(8000, '{title}', '{body}', 'Info'); \
                 Start-Sleep -Seconds 9; $n.Dispose()"
            ),
        ]),
    }
}

/// 发通知的系统。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyOs {
    /// Linux。
    Linux,
    /// macOS。
    MacOs,
    /// Windows。
    Windows,
}

impl NotifyOs {
    /// 当前系统。
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(windows) {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::gui::i18n::{EN, ZH};

    #[test]
    fn close_hides_when_there_is_a_tray_and_hints_only_once() {
        let mut life = Lifecycle::new(true, false);
        assert!(life.has_tray() && life.hint_pending());
        let mut hinted = 0;
        let first = life.on_close_requested(|| {
            hinted += 1;
            true
        });
        assert_eq!(first, CloseAction::Hide);
        assert!(!life.hint_pending());
        let second = life.on_close_requested(|| {
            hinted += 1;
            true
        });
        assert_eq!(second, CloseAction::Hide);
        assert_eq!(hinted, 1, "提示只发一次");
        assert!(!life.quitting());
    }

    #[test]
    fn failed_hint_keeps_the_window_once_then_hides() {
        let mut life = Lifecycle::new(true, false);
        assert_eq!(life.on_close_requested(|| false), CloseAction::Stay);
        assert_eq!(life.on_close_requested(|| false), CloseAction::Hide);
    }

    #[test]
    fn already_hinted_never_hints_again() {
        let mut life = Lifecycle::new(true, true);
        assert!(!life.hint_pending());
        let action = life.on_close_requested(|| panic!("不该再提示"));
        assert_eq!(action, CloseAction::Hide);
    }

    #[test]
    fn without_a_tray_closing_quits() {
        let mut life = Lifecycle::new(false, false);
        assert!(!life.has_tray() && !life.hint_pending());
        assert_eq!(life.on_close_requested(|| true), CloseAction::Quit);
        assert!(life.quitting());
    }

    #[test]
    fn quit_from_the_tray_really_quits() {
        let mut life = Lifecycle::new(true, true);
        assert_eq!(
            life.on_command(TrayCommand::Quit, false),
            WindowAction::Exit
        );
        assert!(life.quitting());
        // 退出时窗口收到的关闭请求不能再被当成“隐藏”
        assert_eq!(life.on_close_requested(|| true), CloseAction::Quit);
    }

    #[test]
    fn open_and_disconnect_commands() {
        let mut life = Lifecycle::new(true, true);
        assert_eq!(
            life.on_command(TrayCommand::Open, false),
            WindowAction::Show
        );
        assert_eq!(
            life.on_command(TrayCommand::Disconnect, true),
            WindowAction::Kick
        );
        assert_eq!(
            life.on_command(TrayCommand::Disconnect, false),
            WindowAction::Nothing,
            "没人连着时断开什么都不做"
        );
        assert!(!life.quitting());
    }

    #[test]
    fn icon_and_menu_follow_the_connection() {
        assert_eq!(icon_kind(false), IconKind::Idle);
        assert_eq!(icon_kind(true), IconKind::Active);
        assert_eq!(tooltip(false, &ZH), ZH.tray_tip_idle);
        assert_eq!(tooltip(true, &EN), EN.tray_tip_active);
        let idle: Vec<_> = menu(false, &ZH).iter().map(|m| m.command).collect();
        assert_eq!(idle, [TrayCommand::Open, TrayCommand::Quit]);
        let busy = menu(true, &EN);
        let cmds: Vec<_> = busy.iter().map(|m| m.command).collect();
        assert_eq!(
            cmds,
            [
                TrayCommand::Open,
                TrayCommand::Disconnect,
                TrayCommand::Quit
            ]
        );
        assert_eq!(busy.last().unwrap().label, EN.tray_quit);
    }

    #[test]
    fn tray_icons_decode_and_the_dot_color_means_connection() {
        let (idle, w, h) = decode_rgba(icon_png(IconKind::Idle)).unwrap();
        assert_eq!((w, h, idle.len()), (32, 32, 32 * 32 * 4));
        let (active, w2, _) = decode_rgba(icon_png(IconKind::Active)).unwrap();
        assert_eq!(w2, 32);
        assert_ne!(idle, active, "有人连着和没人连的图标不同");
        // 点的颜色：有人连着是青绿（绿、蓝明显高于红），没人连是灰（三个分量接近）
        let teal = |px: &[u8]| {
            px.chunks_exact(4)
                .filter(|p| {
                    p[3] > 100 && p[1] > p[0].saturating_add(60) && p[2] > p[0].saturating_add(60)
                })
                .count()
        };
        assert!(teal(&active) > 0, "有人连着的图标里有青绿点");
        assert_eq!(teal(&idle), 0, "没人连的图标里没有青绿");
        let (tpl, w3, h3) = decode_rgba(MAC_TEMPLATE_PNG).unwrap();
        assert_eq!((w3, h3, tpl.len()), (36, 36, 36 * 36 * 4));
        assert!(decode_rgba(b"not a png").is_err());
    }

    #[test]
    fn pixel_formats_convert() {
        assert_eq!(
            rgba_to_argb(&[1, 2, 3, 4, 5, 6, 7, 8]),
            [4, 1, 2, 3, 8, 5, 6, 7]
        );
    }

    #[test]
    fn command_ids_round_trip() {
        for c in [
            TrayCommand::Open,
            TrayCommand::Disconnect,
            TrayCommand::Quit,
        ] {
            assert_eq!(TrayCommand::from_id(c.id()), Some(c));
        }
        assert_eq!(TrayCommand::from_id("nope"), None);
    }

    #[test]
    fn hint_marker_round_trip() {
        let dir = std::env::temp_dir().join(format!("peerdesk-tray-hint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!hint_seen(&dir));
        mark_hint_seen(&dir).unwrap();
        assert!(hint_seen(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notification_commands_for_each_system() {
        let linux = notify_command(NotifyOs::Linux, "标题", "正文");
        assert_eq!(linux.first().map(String::as_str), Some("notify-send"));
        assert_eq!(linux.last().map(String::as_str), Some("正文"));
        let mac = notify_command(NotifyOs::MacOs, "T", "B");
        assert_eq!(mac.first().map(String::as_str), Some("osascript"));
        assert!(mac
            .last()
            .unwrap()
            .contains("display notification \"B\" with title \"T\""));
        let win = notify_command(NotifyOs::Windows, "T", "B");
        assert_eq!(win.first().map(String::as_str), Some("powershell"));
        assert!(win
            .last()
            .unwrap()
            .contains("ShowBalloonTip(8000, 'T', 'B'"));
        let _ = NotifyOs::current();
        // 固定文字里不能有会破坏脚本的引号
        for s in [&ZH, &EN] {
            for text in [s.tray_hint, s.app_title] {
                assert!(!text.contains(['"', '\'', '\\']), "{text}");
            }
        }
    }
}
