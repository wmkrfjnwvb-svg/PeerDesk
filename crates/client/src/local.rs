//! 本机的画面来源和输入去向：图形界面作为被控端时从这里取。
//!
//! 优先用平台层的真实屏幕采集和键鼠注入；打不开（没有显示器、没有权限）时退回演示桌面，
//! 输入只记录不注入。测试构建里系统接口一律当作打不开：单元测试不能去动开发者电脑的键鼠。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use peerdesk_core::input::InputEvent;
use peerdesk_net::demo::DemoDesktop;
use peerdesk_net::host::{InputSink, ScreenSource};
use peerdesk_platform::{CaptureOptions, PlatformError};

/// 演示桌面的尺寸。
const DEMO_SIZE: (u16, u16) = (1280, 720);

#[cfg(not(test))]
fn system_screen() -> Result<Box<dyn ScreenSource>, PlatformError> {
    peerdesk_platform::open_screen(CaptureOptions::default())
}

#[cfg(test)]
fn system_screen() -> Result<Box<dyn ScreenSource>, PlatformError> {
    Err(PlatformError::InvalidOptions(format!(
        "测试里不打开屏幕：{:?}",
        CaptureOptions::default()
    )))
}

#[cfg(not(test))]
fn system_input() -> Result<Box<dyn InputSink>, PlatformError> {
    peerdesk_platform::open_input()
}

#[cfg(test)]
fn system_input() -> Result<Box<dyn InputSink>, PlatformError> {
    Err(PlatformError::InvalidOptions("测试里不注入键鼠".into()))
}

/// 本机屏幕的画面来源：真实屏幕，打不开时用演示桌面。
#[must_use]
pub fn local_screen(seed: u64) -> Box<dyn ScreenSource> {
    screen_or_demo(system_screen(), seed)
}

fn screen_or_demo(
    opened: Result<Box<dyn ScreenSource>, PlatformError>,
    seed: u64,
) -> Box<dyn ScreenSource> {
    opened.unwrap_or_else(|_| Box::new(DemoDesktop::new(DEMO_SIZE.0, DEMO_SIZE.1, seed)))
}

/// 本机的输入注入：先交给 `tap` 记录（界面上显示收到了什么），再注入系统；
/// 系统注入打不开时只记录。
#[must_use]
pub fn local_input(tap: Arc<Tap>) -> Box<dyn InputSink> {
    Box::new(TapSink(tap, system_input().ok()))
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
    use super::*;

    #[test]
    fn screen_and_input_work() {
        let mut screen = local_screen(3);
        let first = screen.capture().unwrap();
        assert_eq!((first.width(), first.height()), DEMO_SIZE);
        assert!(screen.change_seq().is_none());

        let tap = Arc::new(Tap::default());
        assert!(tap.last().is_none());
        let mut input = local_input(Arc::clone(&tap));
        input.inject(&InputEvent::ReleaseAll);
        input.inject(&InputEvent::Key {
            code: 4,
            down: true,
        });
        assert_eq!(tap.count(), 2);
        // 系统注入打开了的话，事件也交给它
        let inner = Arc::new(Tap::default());
        let mut both = TapSink(
            Arc::clone(&tap),
            Some(Box::new(TapSink(Arc::clone(&inner), None))),
        );
        both.inject(&InputEvent::ReleaseAll);
        assert_eq!((tap.count(), inner.count()), (3, 1));
        // 真实屏幕打开了的话用它
        let mut real = screen_or_demo(Ok(Box::new(DemoDesktop::new(320, 200, 1))), 9);
        assert_ne!(
            real.capture().unwrap().width(),
            DEMO_SIZE.0,
            "用的是打开的那个来源"
        );
        assert_eq!(tap.last(), Some(InputEvent::ReleaseAll));
    }
}
