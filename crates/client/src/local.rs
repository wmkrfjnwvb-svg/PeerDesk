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

fn test_input() -> Result<Box<dyn InputSink>, PlatformError> {
    Ok(Box::new(TapSink(Arc::new(Tap::default()), None)))
}

/// 检查本机屏幕和键鼠能不能用（打开后马上关掉）。开启被控之前调用。
pub fn check_local() -> Result<(), String> {
    drop(local_screen()?);
    drop(local_input(Arc::new(Tap::default()))?);
    Ok(())
}

/// 本机屏幕的画面来源。打不开时返回给用户看的原因。
pub fn local_screen() -> Result<Box<dyn ScreenSource>, String> {
    opened_screen((SYSTEM.0)(CaptureOptions::default()))
}

fn opened_screen(
    opened: Result<Box<dyn ScreenSource>, PlatformError>,
) -> Result<Box<dyn ScreenSource>, String> {
    opened.map_err(|e| format!("无法采集本机屏幕：{e}"))
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
    Ok(Box::new(TapSink(tap, Some(sink))))
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

struct TapSink(Arc<Tap>, Option<Box<dyn InputSink>>);

impl InputSink for TapSink {
    fn inject(&mut self, event: &InputEvent) {
        self.0.record(event);
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
        check_local().unwrap();
        let mut screen = local_screen().unwrap();
        assert!(screen.capture().is_some());

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
