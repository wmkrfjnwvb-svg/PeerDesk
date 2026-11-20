//! 本机的画面来源和输入去向：图形界面作为被控端时从这里取。
//!
//! 合并平台层时只改这个文件：
//! - [`local_screen`] 换成 `peerdesk_platform::open_screen(...)`，失败时退回演示桌面；
//! - [`local_input`] 里把 [`Tap`] 包在平台层的输入注入外面。
//!
//! 命令行的 `host` 仍然用演示桌面：它的测试依赖固定的 1280×720 画面。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use peerdesk_core::input::InputEvent;
use peerdesk_net::demo::DemoDesktop;
use peerdesk_net::host::{InputSink, ScreenSource};

/// 演示桌面的尺寸。
const DEMO_SIZE: (u16, u16) = (1280, 720);

/// 本机屏幕的画面来源。
///
/// 现在返回合成的演示桌面。接入真实采集时把函数体换成
/// `peerdesk_platform::open_screen(...)`（失败时退回演示桌面），调用方不用改。
#[must_use]
pub fn local_screen(seed: u64) -> Box<dyn ScreenSource> {
    Box::new(DemoDesktop::new(DEMO_SIZE.0, DEMO_SIZE.1, seed))
}

/// 本机的输入注入。
///
/// 现在只把事件交给 `tap` 记录（界面上显示收到了什么）。接入真实注入时，
/// 返回一个先调用 `tap.record`、再交给平台层注入的包装，调用方不用改。
#[must_use]
pub fn local_input(tap: Arc<Tap>) -> Box<dyn InputSink> {
    Box::new(TapSink(tap))
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

struct TapSink(Arc<Tap>);

impl InputSink for TapSink {
    fn inject(&mut self, event: &InputEvent) {
        self.0.record(event);
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
        assert_eq!(
            tap.last(),
            Some(InputEvent::Key {
                code: 4,
                down: true
            })
        );
    }
}
