//! PeerDesk 平台层：真实的屏幕采集与键鼠注入。
//!
//! | 系统 | 采集 | 变化检测 | 注入 |
//! |---|---|---|---|
//! | Linux（X11） | MIT-SHM，不支持时 `GetImage` | DAMAGE | XTEST |
//! | Windows | DXGI 桌面复制，还没有画面时 GDI | `LastPresentTime` / `AccumulatedFrames` | `SendInput` |
//! | macOS | ScreenCaptureKit（BGRA） | 帧状态 Complete / Idle | `CGEvent` |
//!
//! 采集在自己的线程上进行，最新的一帧放进一个槽位（不是队列，旧的直接被替换），
//! [`ScreenSource::capture`] 只取槽位里最新的那一帧，从不阻塞会话循环。
//! 只有画面内容真的变了才更新槽位并把变化计数加一（见 [`capture`]）。
//!
//! 与系统无关的逻辑（键位表、像素转换、坐标换算、槽位和变化计数）放在独立的模块里并有单元测试；
//! 系统胶水在 `x11.rs`、`windows.rs`、`macos.rs`，只有这三个模块允许 `unsafe`。
//!
//! Wayland 不在支持范围内：会返回明确的错误（见 [`PlatformError::Unsupported`]）。

pub mod audio;
mod audio_sys;
pub mod capture;
pub use audio_sys::{system_capture, system_playback};
pub mod clipboard;
mod clipboard_sys;
pub use clipboard_sys::{open_clipboard, OnNote, SystemClip};
mod error;
pub mod guide;
pub mod keymap;
pub mod pixel;
pub mod pointer;
pub mod session;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod unsupported;
#[cfg(windows)]
mod windows;
/// Windows 系统服务用到的会话和进程操作（无人值守）。
#[cfg(windows)]
pub mod winsession;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "macos")]
use macos as sys;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use unsupported as sys;
#[cfg(windows)]
use windows as sys;
#[cfg(target_os = "linux")]
use x11 as sys;

pub use capture::{CaptureStats, ScreenCapture};
pub use error::PlatformError;
use peerdesk_net::host::{InputSink, ScreenSource};

/// 帧率上限的允许范围。
pub const MAX_FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=240;

/// 屏幕采集的参数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureOptions {
    /// 采集第几个显示器，顺序与 [`displays`] 一致（0 是主显示器）。
    pub display: usize,
    /// 每秒最多采集多少帧（1 到 240）。画面不变时不采集，实际帧率可能低得多。
    pub max_fps: u32,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            display: 0,
            max_fps: 60,
        }
    }
}

impl CaptureOptions {
    /// 检查参数是否合法。
    pub fn validate(&self) -> Result<(), PlatformError> {
        if MAX_FPS_RANGE.contains(&self.max_fps) {
            Ok(())
        } else {
            Err(PlatformError::InvalidOptions(format!(
                "帧率上限 {} 超出范围（{} 到 {}）",
                self.max_fps,
                MAX_FPS_RANGE.start(),
                MAX_FPS_RANGE.end()
            )))
        }
    }
}

/// 一个显示器。
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    /// 序号，与 [`CaptureOptions::display`] 对应。
    pub index: usize,
    /// 系统给的名字（可能为空）。
    pub name: String,
    /// 左上角在桌面坐标系里的位置。Windows、Linux 是物理像素，macOS 是点（Retina 屏 1 点 = 2 像素）。
    pub x: i32,
    /// 见 [`DisplayInfo::x`]。
    pub y: i32,
    /// 采集到的画面宽度（物理像素）。
    pub width: u32,
    /// 采集到的画面高度（物理像素）。
    pub height: u32,
    /// 每个逻辑单位（点、DIP）对应多少物理像素，例如 Retina 屏和 Windows 200% 缩放都是 2。
    pub scale: f64,
    /// 是否是主显示器。
    pub primary: bool,
}

impl DisplayInfo {
    /// 桌面坐标 `(x, y)` 是否落在这个显示器上（只对像素坐标的系统准确，macOS 的坐标是点）。
    #[must_use]
    pub fn contains(&self, x: i32, y: i32) -> bool {
        let (dx, dy) = (
            i64::from(x) - i64::from(self.x),
            i64::from(y) - i64::from(self.y),
        );
        (0..i64::from(self.width)).contains(&dx) && (0..i64::from(self.height)).contains(&dy)
    }
}

/// 平台需要的系统权限是否已经授予。只有 macOS 需要用户授权，其他系统总是 `true`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionStatus {
    /// 能否采集屏幕（macOS 的“屏幕与系统录音”）。
    pub screen_capture: bool,
    /// 能否注入键盘和鼠标（macOS 的“辅助功能”）。
    pub input_injection: bool,
}

/// 缺少“屏幕录制”权限时给用户的说明。
pub const SCREEN_PERMISSION_HINT: &str = "没有“屏幕录制”权限，无法采集画面。\
请打开“系统设置 → 隐私与安全性 → 屏幕与系统录音”（较早的系统叫“屏幕录制”），\
允许运行 peerdesk 的程序（例如“终端”），然后退出并重新打开这个程序。";

/// 缺少“辅助功能”权限时给用户的说明。
pub const INPUT_PERMISSION_HINT: &str = "没有“辅助功能”权限，无法控制键盘和鼠标。\
请打开“系统设置 → 隐私与安全性 → 辅助功能”，允许运行 peerdesk 的程序（例如“终端”），\
然后退出并重新打开这个程序。";

impl PermissionStatus {
    /// 全部已授予。
    #[must_use]
    pub const fn all_granted(&self) -> bool {
        self.screen_capture && self.input_injection
    }

    /// 缺少的权限各自的说明（中文，告诉用户去哪里授权）。
    #[must_use]
    pub fn hints(&self) -> Vec<&'static str> {
        let mut hints = Vec::new();
        if !self.screen_capture {
            hints.push(SCREEN_PERMISSION_HINT);
        }
        if !self.input_injection {
            hints.push(INPUT_PERMISSION_HINT);
        }
        hints
    }
}

/// 开始采集一个显示器。等到第一帧画面到手才返回（最多几秒），之后 `capture()` 总有画面可取。
pub fn open_screen(opts: CaptureOptions) -> Result<Box<dyn ScreenSource>, PlatformError> {
    Ok(Box::new(ScreenCapture::open(opts)?))
}

/// 打开键鼠注入，绝对坐标对应主显示器（序号 0）。
pub fn open_input() -> Result<Box<dyn InputSink>, PlatformError> {
    open_input_on(0)
}

/// 打开键鼠注入，绝对坐标对应第 `display` 个显示器（应当与采集的显示器一致）。
pub fn open_input_on(display: usize) -> Result<Box<dyn InputSink>, PlatformError> {
    sys::open_input(display)
}

/// 列出所有显示器：主显示器排第一，其余按位置从左到右。列不出来时为空。
#[must_use]
pub fn displays() -> Vec<DisplayInfo> {
    sys::displays().unwrap_or_default()
}

/// 检查系统权限。macOS 上查询“屏幕录制”和“辅助功能”两项授权，其他系统总是全部授予。
#[must_use]
pub fn check_permissions() -> PermissionStatus {
    sys::permissions()
}

/// 现在运行的系统（macOS 带版本号，Linux 区分 X11 和 Wayland），给权限向导用。
#[must_use]
pub fn current_os() -> guide::Os {
    sys::current_os()
}

/// 执行权限向导里一个按钮的动作（打开系统设置、弹出授权对话框、重新打开程序）。
/// 失败时返回给用户看的原因。
pub fn perform(action: guide::Action) -> Result<(), String> {
    sys::perform(action)
}

/// 鼠标光标现在的位置（桌面坐标，单位同 [`DisplayInfo::x`]）。拿不到时为 `None`。
#[must_use]
pub fn cursor_position() -> Option<(i32, i32)> {
    sys::cursor_position()
}

/// 第 `index` 个显示器；不存在时给出有几个显示器的错误。
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn pick_display(
    list: Vec<DisplayInfo>,
    index: usize,
) -> Result<DisplayInfo, PlatformError> {
    let count = list.len();
    list.into_iter()
        .nth(index)
        .ok_or(PlatformError::NoDisplay { index, count })
}

/// 按主显示器优先、再从左到右、从上到下排好，并重新编号。没有标出主显示器时把第一个当作主显示器。
#[cfg_attr(windows, allow(dead_code))]
pub(crate) fn sort_displays(mut list: Vec<DisplayInfo>) -> Vec<DisplayInfo> {
    list.sort_by_key(|d| (!d.primary, d.x, d.y));
    let has_primary = list.iter().any(|d| d.primary);
    for (i, d) in list.iter_mut().enumerate() {
        d.index = i;
        if i == 0 && !has_primary {
            d.primary = true;
        }
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Linux 上不需要授权：向导里的动作都不适用，系统类型按会话判断。
    #[cfg(target_os = "linux")]
    #[test]
    fn guide_actions_on_linux() {
        assert!(matches!(
            current_os(),
            guide::Os::LinuxX11 | guide::Os::LinuxWayland
        ));
        assert!(perform(guide::Action::Relaunch).is_err());
    }

    fn display(x: i32, primary: bool) -> DisplayInfo {
        DisplayInfo {
            index: 9,
            name: String::new(),
            x,
            y: 0,
            width: 100,
            height: 100,
            scale: 1.0,
            primary,
        }
    }

    #[test]
    fn options_are_validated() {
        assert_eq!(CaptureOptions::default().validate(), Ok(()));
        for fps in [0, 241] {
            let opts = CaptureOptions {
                display: 0,
                max_fps: fps,
            };
            let err = opts.validate().unwrap_err();
            assert!(err.to_string().contains("帧率"), "{err}");
        }
    }

    #[test]
    fn permission_hints_name_the_settings_page() {
        let all = PermissionStatus {
            screen_capture: true,
            input_injection: true,
        };
        assert!(all.all_granted());
        assert!(all.hints().is_empty());
        let none = PermissionStatus {
            screen_capture: false,
            input_injection: false,
        };
        assert!(!none.all_granted());
        let hints = none.hints();
        assert_eq!(hints.len(), 2);
        assert!(hints[0].contains("屏幕与系统录音"));
        assert!(hints[1].contains("辅助功能"));
    }

    #[test]
    fn displays_are_sorted_primary_first() {
        let list = sort_displays(vec![display(1920, false), display(-1280, false)]);
        assert_eq!(list[0].x, -1280);
        assert!(list[0].contains(-1280, 99) && !list[0].contains(-1281, 0));
        assert!(!list[0].contains(-1180, 0) && !list[0].contains(-1200, 100));
        assert!(list[0].primary, "没有主显示器时第一个算主显示器");
        assert_eq!((list[0].index, list[1].index), (0, 1));
        let list = sort_displays(vec![display(-1280, false), display(0, true)]);
        assert_eq!(list[0].x, 0);
        assert!(!list[1].primary);
        assert_eq!(pick_display(list.clone(), 1).unwrap().x, -1280);
        assert_eq!(
            pick_display(list, 5),
            Err(PlatformError::NoDisplay { index: 5, count: 2 })
        );
    }

    #[test]
    fn system_queries_do_not_fail() {
        // 有没有显示器取决于运行环境：这里只要求调用本身不出错、结果自洽。
        let list = displays();
        let sane = |(i, d): (usize, &DisplayInfo)| d.index == i && d.width > 0 && d.scale > 0.0;
        assert!(list.iter().enumerate().all(sane), "{list:?}");
        // 光标总在某个显示器上（列不出显示器时也拿不到光标位置）
        let on_screen = cursor_position().map(|(x, y)| list.iter().any(|d| d.contains(x, y)));
        assert_ne!(on_screen, Some(false));
        let status = check_permissions();
        assert_eq!(status.all_granted(), status.hints().is_empty());
        // 不存在的显示器：采集和注入都报错，而不是悄悄用别的显示器。
        let opts = CaptureOptions {
            display: 1000,
            max_fps: 30,
        };
        assert!(open_screen(opts).is_err());
        assert!(open_input_on(1000).is_err());
        // 只打开不注入：有没有图形界面都不应当出问题。
        drop(open_input());
        let bad = CaptureOptions {
            display: 0,
            max_fps: 0,
        };
        assert!(matches!(
            open_screen(bad),
            Err(PlatformError::InvalidOptions(_))
        ));
    }
}
