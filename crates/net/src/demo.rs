//! 演示和测试用的画面来源与去向：真实的屏幕采集和窗口显示接入之前，用它们跑通整条链路。

use std::sync::{Arc, Mutex, PoisonError};

use peerdesk_core::frame::{Frame, Picture};
use peerdesk_core::screen::{Screen, SyntheticDesktop, Workload};

use crate::controller::ScreenSink;
use crate::host::ScreenSource;

/// 合成桌面作为画面来源：按脚本依次做几段操作，脚本做完后保持静止。
#[derive(Debug)]
pub struct DemoDesktop {
    desktop: SyntheticDesktop,
    script: Vec<(Workload, u32)>,
    step: u32,
    limit: Option<u32>,
    mirror: Arc<Mutex<Screen>>,
}

impl DemoDesktop {
    /// 创建。默认脚本：打字、拖窗口、滚动、播放视频各一小段，然后静止。
    #[must_use]
    pub fn new(width: u16, height: u16, seed: u64) -> Self {
        let desktop = SyntheticDesktop::new(width, height, seed);
        let mirror = Arc::new(Mutex::new(desktop.screen().clone()));
        Self {
            desktop,
            script: vec![
                (Workload::Typing, 60),
                (Workload::Dragging, 60),
                (Workload::Scrolling, 10),
                (Workload::Video, 30),
            ],
            step: 0,
            limit: None,
            mirror,
        }
    }

    /// 换一个脚本：`(操作, 帧数)` 的序列。
    #[must_use]
    pub fn with_script(mut self, script: Vec<(Workload, u32)>) -> Self {
        self.script = script;
        self
    }

    /// 取了 `frames` 次画面之后结束（会话随之结束）。
    #[must_use]
    pub const fn with_limit(mut self, frames: u32) -> Self {
        self.limit = Some(frames);
        self
    }

    /// 当前画面的共享副本，测试用它核对观看端看到的是否一致。
    #[must_use]
    pub fn mirror(&self) -> Arc<Mutex<Screen>> {
        Arc::clone(&self.mirror)
    }

    fn workload(&self) -> Workload {
        let mut at = self.step;
        for (workload, frames) in &self.script {
            if at < *frames {
                return *workload;
            }
            at -= *frames;
        }
        Workload::Idle
    }
}

impl DemoDesktop {
    /// 走一步，取当前画面；取满 `limit` 次之后为 `None`。
    pub fn capture(&mut self) -> Option<&Screen> {
        if self.limit.is_some_and(|limit| self.step >= limit) {
            return None;
        }
        self.desktop.step(self.workload());
        self.step = self.step.saturating_add(1);
        self.mirror
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone_from(self.desktop.screen());
        Some(self.desktop.screen())
    }
}

impl ScreenSource for DemoDesktop {
    fn frame(&mut self) -> Option<Frame> {
        self.capture().map(|s| Frame::rgb(s.clone()))
    }
}

/// 丢弃画面的去向，只数更新了多少次。
#[derive(Debug, Clone, Default)]
pub struct CountSink {
    count: Arc<Mutex<u64>>,
}

impl CountSink {
    /// 创建。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 画面更新的次数。
    #[must_use]
    pub fn count(&self) -> u64 {
        *self.count.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl ScreenSink for CountSink {
    fn present(&mut self, _picture: &Picture) {
        let mut count = self.count.lock().unwrap_or_else(PoisonError::into_inner);
        *count = count.saturating_add(1);
    }
}

/// 把画面保存成 BMP 文件的字节（24 位、不压缩），任何看图软件都能打开。
#[must_use]
pub fn to_bmp(screen: &Screen) -> Vec<u8> {
    let (w, h) = (usize::from(screen.width()), usize::from(screen.height()));
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut out = Vec::with_capacity(size);
    let u32le = |v: usize| u32::try_from(v).unwrap_or(u32::MAX).to_le_bytes();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&u32le(size));
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&u32le(54));
    out.extend_from_slice(&u32le(40));
    out.extend_from_slice(&u32le(w));
    out.extend_from_slice(&u32le(h));
    out.extend_from_slice(&[1, 0, 24, 0]);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&u32le(row * h));
    out.extend_from_slice(&[0; 16]);
    // BMP 的行是从下往上存的，像素是蓝绿红的顺序。
    for y in (0..screen.height()).rev() {
        let start = out.len();
        for pixel in screen.row(0, y, screen.width()) {
            let [_, r, g, b] = pixel.to_be_bytes();
            out.extend_from_slice(&[b, g, r]);
        }
        out.resize(start + row, 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_runs_then_goes_idle_and_limit_ends_the_source() {
        let mut demo = DemoDesktop::new(160, 120, 1)
            .with_script(vec![(Workload::Typing, 2), (Workload::Dragging, 1)])
            .with_limit(6);
        let mirror = demo.mirror();
        let mut frames = Vec::new();
        while let Some(screen) = demo.capture() {
            assert_eq!(&*mirror.lock().unwrap(), screen);
            frames.push(screen.clone());
        }
        assert_eq!(frames.len(), 6);
        assert_ne!(frames[0], frames[1], "打字");
        assert_ne!(frames[1], frames[2], "拖窗口");
        assert_eq!(frames[3], frames[4], "脚本结束后静止");
        assert!(demo.capture().is_none());
        // 默认脚本：不限帧数
        let mut demo = DemoDesktop::new(160, 120, 1);
        assert!((0..200).all(|_| demo.capture().is_some()));
    }

    #[test]
    fn count_sink_counts() {
        let sink = CountSink::new();
        let mut writer = sink.clone();
        let one = Picture::Rgb(Arc::new(Screen::new(1, 1, 0)));
        writer.present(&one);
        writer.present(&one);
        assert_eq!(sink.count(), 2);
    }

    #[test]
    fn bmp_layout() {
        let mut screen = Screen::new(3, 2, 0x112233);
        screen.fill(0, 1, 1, 1, 0xaabbcc);
        let bmp = to_bmp(&screen);
        // 每行 3 像素 × 3 字节 = 9，补齐到 12
        assert_eq!(bmp.len(), 54 + 12 * 2);
        assert_eq!(&bmp[..2], b"BM");
        assert_eq!(u32::from_le_bytes(bmp[2..6].try_into().unwrap()), 78);
        assert_eq!(u32::from_le_bytes(bmp[18..22].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(bmp[22..26].try_into().unwrap()), 2);
        // 文件里的第一行是画面的最下面一行，颜色顺序是蓝绿红
        assert_eq!(&bmp[54..57], &[0xcc, 0xbb, 0xaa]);
        assert_eq!(&bmp[57..60], &[0x33, 0x22, 0x11]);
        assert_eq!(&bmp[63..66], &[0, 0, 0], "行尾补零");
        assert_eq!(to_bmp(&Screen::new(0, 0, 0)).len(), 54);
    }
}
