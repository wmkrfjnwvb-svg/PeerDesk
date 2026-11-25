//! 本机的画面来源和输入去向：图形界面作为被控端时从这里取。
//!
//! 用平台层的真实屏幕采集和键鼠注入。打不开（没有显示器、没有权限）就报错：
//! 没有权限就不提供远程，绝不拿演示画面顶替真实屏幕。
//! 测试构建里换成合成桌面和只记录的输入：单元测试不能去动开发者电脑的屏幕和键鼠。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use peerdesk_core::input::InputEvent;
use peerdesk_net::host::{InputSink, ScreenSource};
use peerdesk_platform::{CaptureOptions, PlatformError};

use crate::unattended::SasDetector;

type OpenScreen = fn(CaptureOptions) -> Result<Box<dyn ScreenSource>, PlatformError>;
type OpenInput = fn() -> Result<Box<dyn InputSink>, PlatformError>;

/// 系统的屏幕采集和键鼠注入；测试构建里换成合成桌面和只记录的输入。
const SYSTEM: (OpenScreen, OpenInput) = if cfg!(test) {
    (test_screen, test_input)
} else {
    (
        peerdesk_platform::open_screen,
        peerdesk_platform::open_input,
    )
};

fn test_screen(_: CaptureOptions) -> Result<Box<dyn ScreenSource>, PlatformError> {
    Ok(Box::new(peerdesk_net::demo::DemoDesktop::new(1280, 720, 1)))
}

/// 测试用：数据目录里有这个文件时，这个被控端的屏幕打不开（模拟锁屏）。按数据目录区分，
/// 并行的其他测试不受影响（原来用全局开关，其他测试的被控端也被当成锁屏）。
#[cfg(test)]
pub const TEST_LOCK_FILE: &str = "test-screen-locked";

/// 被控端用的屏幕：`data_dir` 是被控端的数据目录（测试用来模拟锁屏）。
pub fn local_screen_in(data_dir: &std::path::Path) -> Result<Box<dyn ScreenSource>, String> {
    #[cfg(test)]
    if data_dir.join(TEST_LOCK_FILE).exists() {
        return opened_screen(Err(PlatformError::Os("屏幕已锁定（测试）".into())));
    }
    let _ = data_dir;
    local_screen()
}

fn test_input() -> Result<Box<dyn InputSink>, PlatformError> {
    Ok(Box::new(Discard))
}

/// 测试构建里的“系统”输入：什么都不做（外面的 [`TapSink`] 负责记录）。
struct Discard;

impl InputSink for Discard {
    fn inject(&mut self, _: &InputEvent) {}
}

/// 检查本机屏幕和键鼠能不能用（打开后马上关掉）。开启被控之前调用。
/// 屏幕按被控端的数据目录打开（测试用来模拟锁屏）。
pub fn check_local_in(data_dir: &std::path::Path) -> Result<(), String> {
    drop(local_screen_in(data_dir)?);
    drop(local_input(Arc::new(Tap::default()))?);
    Ok(())
}

/// Windows 上锁屏、登录界面、管理员权限提示（安全桌面）时普通程序采集不到屏幕。
const LOCKED_HINT: &str = if cfg!(windows) {
    "。如果这台电脑已锁屏、在登录界面或弹出了管理员权限提示，当前版本还不能远程这些界面\
     ，请先在本机解锁；\
     要远程这些界面，请以管理员身份安装并勾选“无人值守”（或运行 peerdesk service install）"
} else {
    ""
};

/// 本机屏幕的画面来源。打不开时返回给用户看的原因。
pub fn local_screen() -> Result<Box<dyn ScreenSource>, String> {
    opened_screen((SYSTEM.0)(CaptureOptions::default()))
}

fn opened_screen(
    opened: Result<Box<dyn ScreenSource>, PlatformError>,
) -> Result<Box<dyn ScreenSource>, String> {
    opened.map_err(|e| format!("无法采集本机屏幕：{e}{LOCKED_HINT}"))
}

/// 本机的输入注入：先交给 `tap` 记录（界面上显示收到了什么），再注入系统。
/// 打不开时返回给用户看的原因。
pub fn local_input(tap: Arc<Tap>) -> Result<Box<dyn InputSink>, String> {
    opened_input(tap, (SYSTEM.1)())
}

fn opened_input(
    tap: Arc<Tap>,
    opened: Result<Box<dyn InputSink>, PlatformError>,
) -> Result<Box<dyn InputSink>, String> {
    let sink = opened.map_err(|e| format!("无法控制本机键鼠：{e}"))?;
    Ok(Box::new(TapSink(tap, Some(sink), SasDetector::default())))
}

/// 记录收到的输入：数量和最后一个事件。被控端界面用它确认输入确实到了。
#[derive(Debug, Default)]
pub struct Tap {
    count: AtomicU64,
    last: Mutex<Option<InputEvent>>,
}

impl Tap {
    /// 记录一个事件。
    pub fn record(&self, event: &InputEvent) {
        self.count.fetch_add(1, Ordering::Relaxed);
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(event.clone());
    }

    /// 收到的事件数。
    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// 最后一个事件。
    pub fn last(&self) -> Option<InputEvent> {
        self.last
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// 收到 Ctrl+Alt+Del 时调用（无人值守的代理用它请求服务代发；普通注入触发不了安全界面）。
type SasHook = Box<dyn Fn() + Send + Sync>;
static SAS_HOOK: Mutex<Option<SasHook>> = Mutex::new(None);

/// 设置收到 Ctrl+Alt+Del 时做什么。
pub fn set_sas_hook(hook: SasHook) {
    *SAS_HOOK.lock().unwrap_or_else(PoisonError::into_inner) = Some(hook);
}

struct TapSink(Arc<Tap>, Option<Box<dyn InputSink>>, SasDetector);

impl InputSink for TapSink {
    fn inject(&mut self, event: &InputEvent) {
        self.0.record(event);
        if let InputEvent::Key { code, down } = event {
            if self.2.key(*code, *down) {
                if let Some(hook) = SAS_HOOK
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .as_ref()
                {
                    hook();
                }
            }
        }
        if let Some(system) = self.1.as_mut() {
            system.inject(event);
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn opens_or_explains_why_not() {
        check_local_in(std::path::Path::new("")).unwrap();
        let mut screen = local_screen().unwrap();
        assert!(screen.frame().is_some());

        let tap = Arc::new(Tap::default());
        assert!(tap.last().is_none());
        let mut input = local_input(Arc::clone(&tap)).unwrap();
        input.inject(&InputEvent::ReleaseAll);
        input.inject(&InputEvent::Key {
            code: 4,
            down: true,
        });
        assert_eq!(tap.count(), 2);
        assert_eq!(
            tap.last(),
            Some(InputEvent::Key {
                code: 4,
                down: true
            })
        );

        // 打不开：报错，说明是哪一项，不拿演示画面顶替
        let denied = || PlatformError::Permission("屏幕录制".into());
        let err = opened_screen(Err(denied())).err().unwrap();
        assert!(
            err.contains("无法采集本机屏幕") && err.contains("屏幕录制"),
            "{err}"
        );
        let err = opened_input(Arc::clone(&tap), Err(denied())).err().unwrap();
        assert!(err.contains("无法控制本机键鼠"), "{err}");
    }
}

/// 本机的电脑名：握手时告诉控制端，界面上也显示，用户一眼认出是哪台电脑。
/// 取不到时为空（对方就只显示设备码）。
#[must_use]
pub fn device_name() -> String {
    peerdesk_core::proto::clip_name(system_name().trim())
}

/// Windows：计算机名。
#[cfg(windows)]
fn system_name() -> String {
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// macOS：“系统设置 → 通用 → 关于本机”里的名称（例如“张三的 MacBook Pro”），
/// 比主机名（Zhangsans-MacBook-Pro.local）好认。
#[cfg(target_os = "macos")]
fn system_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

/// Linux 等：主机名。
#[cfg(not(any(windows, target_os = "macos")))]
fn system_name() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .unwrap_or_default()
}

#[cfg(test)]
mod name_tests {
    #[test]
    fn this_computer_has_a_short_name() {
        let name = super::device_name();
        assert!(!name.is_empty() && name.len() <= peerdesk_core::proto::MAX_NAME_LEN);
        assert_eq!(name, name.trim());
    }
}
