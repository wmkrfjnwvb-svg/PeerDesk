//! 不支持的系统：所有操作都返回明确的错误。

use peerdesk_net::host::InputSink;

use crate::capture::Grabber;
use crate::{CaptureOptions, DisplayInfo, PermissionStatus, PlatformError};

fn unsupported() -> PlatformError {
    PlatformError::Unsupported(format!(
        "{} 系统暂不支持屏幕采集和键鼠注入",
        std::env::consts::OS
    ))
}

pub(crate) fn open_grabber(_opts: &CaptureOptions) -> Result<Box<dyn Grabber>, PlatformError> {
    Err(unsupported())
}

pub(crate) fn open_input(_display: usize) -> Result<Box<dyn InputSink>, PlatformError> {
    Err(unsupported())
}

pub(crate) fn displays() -> Result<Vec<DisplayInfo>, PlatformError> {
    Err(unsupported())
}

pub(crate) fn cursor_position() -> Option<(i32, i32)> {
    None
}

pub(crate) fn current_os() -> crate::guide::Os {
    crate::guide::Os::Other
}

pub(crate) fn perform(action: crate::guide::Action) -> Result<(), String> {
    Err(format!("这个系统不需要这一步：{action:?}"))
}

pub(crate) fn permissions() -> PermissionStatus {
    PermissionStatus {
        screen_capture: false,
        input_injection: false,
    }
}
