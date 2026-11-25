//! 采集线程、最新帧槽位和变化计数。与系统无关，各系统只需实现 [`Grabber`]。
//!
//! * 采集在自己的线程上进行，等画面变化（系统告诉我们变了才读），读到的画面放进**一个**槽位：
//!   不排队，新的直接替换旧的。会话循环随时来取，拿到的总是最新的一帧，取的动作只是交换一个指针；
//! * 槽位每换一次内容，变化计数加一。系统报告了变化、读出来的内容却和上一帧一模一样（例如窗口用同样的内容重画）
//!   时不算变化：计数只在内容真的变了时增加；
//! * 帧率有上限：两次读取之间至少隔 `1 / max_fps`。等待期间发生的变化由系统累积着，到时间一次读出；
//! * 画面缓冲区循环使用：会话换下来的旧画面还给采集线程，下一次读取直接写进去，不用每帧重新分配；
//! * 出错时（例如 Windows 切换到安全桌面、分辨率改变）关掉重新打开，间隔逐渐拉长。
//!   第一帧之前出错则直接报告给打开者，不重试；
//! * 能在显卡上缩放、转颜色的系统（macOS 的 ScreenCaptureKit）按会话要的编码尺寸直接交 NV12 显卡画面
//!   （[`Frame::Gpu`]），CPU 不碰像素；内容比较不了，系统交来新画面就算变了（它只在画面变了时交）。

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use peerdesk_core::frame::Frame;
use peerdesk_core::screen::Screen;
use peerdesk_net::host::ScreenSource;

use crate::{CaptureOptions, PlatformError};

/// 等待第一帧画面的时间上限。
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
/// 采集线程每次最多等这么久的画面变化，然后检查是否该退出。
const POLL: Duration = Duration::from_millis(100);
/// 出错后重新打开的最短、最长间隔。
const RETRY_MIN: Duration = Duration::from_millis(100);
const RETRY_MAX: Duration = Duration::from_secs(2);

/// 各系统的画面读取器。只在采集线程上创建和使用，不需要 `Send`。
pub(crate) trait Grabber {
    /// 等待画面变化，最多 `timeout`。返回 `true` 表示画面（可能）变了，接着会调用 [`Grabber::grab`]。
    /// 第一次调用时应当返回 `true`，让第一帧尽快到手。
    fn wait(&mut self, timeout: Duration) -> Result<bool, PlatformError>;

    /// 读出当前画面。`spare` 是可以复用的旧缓冲区（尺寸可能不对；交显卡画面的系统用不上）。
    fn grab(&mut self, spare: Option<Screen>) -> Result<Frame, PlatformError>;

    /// 改采集尺寸（会话要的编码尺寸，偶数）。能在显卡上缩放的系统照做，之后交的画面就是这个尺寸；
    /// 其他系统忽略（交原尺寸的 CPU 画面，由编码器缩放）。
    fn set_target(&mut self, _size: (u16, u16)) -> Result<(), PlatformError> {
        Ok(())
    }

    /// 原始画面尺寸（像素，例如 Retina 屏 2880×1800）；不知道时为 `None`。
    fn native_size(&self) -> Option<(u16, u16)> {
        None
    }
}

/// 在采集线程上打开读取器；出错重开时会再次调用。
pub(crate) type Opener = Box<dyn FnMut() -> Result<Box<dyn Grabber>, PlatformError> + Send>;

/// 采集的统计数据。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureStats {
    /// 内容有变化、放进槽位的帧数（等于当前的变化计数）。
    pub frames: u64,
    /// 读取次数（包括读出来和上一帧一样的）。
    pub grabs: u64,
    /// 最近一次读取花的时间（从系统读出画面并转换格式，不含等待变化的时间）。
    pub last_grab: Duration,
    /// 所有读取花的时间之和。
    pub total_grab: Duration,
    /// 出错后重新打开的次数。
    pub reopens: u64,
    /// 最近一次错误。
    pub last_error: Option<PlatformError>,
}

impl CaptureStats {
    /// 平均每次读取花的时间。
    #[must_use]
    pub fn mean_grab(&self) -> Duration {
        u32::try_from(self.grabs)
            .ok()
            .filter(|n| *n > 0)
            .map_or(Duration::ZERO, |n| self.total_grab / n)
    }
}

#[derive(Debug, Default)]
struct State {
    /// 最新的一帧。
    latest: Option<Frame>,
    /// 变化计数：`latest` 每换一次加一，第一帧是 1。
    seq: u64,
    /// 会话换下来的旧画面，采集线程拿去复用。
    returned: Option<Frame>,
    /// 会话要的采集尺寸（编码尺寸）。
    target: Option<(u16, u16)>,
    /// 原始画面尺寸。
    native: Option<(u16, u16)>,
    /// 要求采集线程退出。
    stop: bool,
    /// 第一帧之前出的错：打开失败。
    failed: Option<PlatformError>,
    stats: CaptureStats,
    /// 上次写诊断日志以来最慢的一次读取。
    window_max: Duration,
}

#[derive(Debug, Default)]
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stopping(&self) -> bool {
        self.lock().stop
    }

    /// 睡 `d`，要求退出时提前醒来。
    fn sleep(&self, d: Duration) {
        let guard = self.lock();
        let _ = self
            .wake
            .wait_timeout_while(guard, d, |s| !s.stop)
            .unwrap_or_else(PoisonError::into_inner);
    }

    /// 记录一个错误。返回是否应当继续（第一帧之前出错时不继续，交给打开者）。
    fn on_error(&self, e: PlatformError) -> bool {
        let mut st = self.lock();
        if st.seq == 0 {
            st.failed = Some(e);
            drop(st);
            self.wake.notify_all();
            return false;
        }
        st.stats.reopens = st.stats.reopens.saturating_add(1);
        st.stats.last_error = Some(e);
        true
    }

    /// 放进一帧。内容和上一帧一样时不放，计数不变。返回可以复用的缓冲区。
    fn publish(&self, frame: Frame, took: Duration) -> Option<Screen> {
        let previous = {
            let mut st = self.lock();
            st.stats.grabs = st.stats.grabs.saturating_add(1);
            st.stats.last_grab = took;
            st.stats.total_grab = st.stats.total_grab.saturating_add(took);
            st.window_max = st.window_max.max(took);
            st.latest.clone()
        };
        // 比较在锁外进行：只有采集线程会改 `latest`，会话取画面不会被整屏比较挡住。
        // 显卡画面比较不了：系统交来就算变了。
        if previous.as_ref().is_some_and(|p| p.same_content(&frame)) {
            return reusable(frame);
        }
        drop(previous);
        let (old, returned) = {
            let mut st = self.lock();
            let old = st.latest.replace(frame);
            st.seq = st.seq.saturating_add(1);
            st.stats.frames = st.seq;
            (old, st.returned.take())
        };
        self.wake.notify_all();
        // 会话还拿着的画面不能复用；会话还回来的、或者会话根本没取过的旧画面可以。
        [returned, old].into_iter().flatten().find_map(reusable)
    }
}

/// 没人再用的 CPU 画面可以复用它的缓冲区；显卡画面由系统回收。
fn reusable(frame: Frame) -> Option<Screen> {
    match frame {
        Frame::Rgb(a) => Arc::try_unwrap(a).ok(),
        Frame::Gpu(_) => None,
    }
}

/// 第 `failures` 次连续出错后，等多久再重新打开。
fn retry_delay(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(5);
    RETRY_MIN.saturating_mul(1 << shift).min(RETRY_MAX)
}

/// 采集线程的主循环。
fn run(shared: &Shared, mut opener: Opener, interval: Duration) {
    let mut grabber: Option<Box<dyn Grabber>> = None;
    let mut spare: Option<Screen> = None;
    let mut next = Instant::now();
    let mut failures = 0u32;
    // 已经交给这个读取器的采集尺寸（重新打开后要再交一次）
    let mut applied: Option<(u16, u16)> = None;
    while !shared.stopping() {
        let g = match &mut grabber {
            Some(g) => g,
            None => match opener() {
                Ok(g) => {
                    applied = None;
                    shared.lock().native = g.native_size();
                    grabber.insert(g)
                }
                Err(e) => {
                    if !shared.on_error(e) {
                        return;
                    }
                    failures = failures.saturating_add(1);
                    shared.sleep(retry_delay(failures));
                    continue;
                }
            },
        };
        let now = Instant::now();
        if now < next {
            shared.sleep(next - now);
            continue;
        }
        // 会话要的采集尺寸变了：先交给系统（失败按读取出错处理：重新打开、之后再交）
        let wanted = shared.lock().target;
        let retarget = match wanted.filter(|_| wanted != applied) {
            Some(size) => g.set_target(size),
            None => Ok(()),
        };
        applied = wanted;
        let result = retarget.and_then(|()| g.wait(POLL)).and_then(|changed| {
            if !changed {
                return Ok(None);
            }
            let started = Instant::now();
            g.grab(spare.take()).map(|s| Some((s, started)))
        });
        match result {
            Ok(Some((frame, started))) => {
                failures = 0;
                next = started + interval;
                spare = shared.publish(frame, started.elapsed());
            }
            Ok(None) => {}
            Err(e) => {
                grabber = None;
                if !shared.on_error(e) {
                    return;
                }
                failures = failures.saturating_add(1);
                shared.sleep(retry_delay(failures));
            }
        }
    }
}

/// 一个正在采集的显示器。实现 [`ScreenSource`]：`capture()` 取最新的一帧，从不阻塞。
///
/// 丢弃时停止采集线程（最多等一个轮询周期）。
#[derive(Debug)]
pub struct ScreenCapture {
    shared: Arc<Shared>,
    current: Option<Frame>,
    current_seq: u64,
    thread: Option<JoinHandle<()>>,
    /// 上次写诊断日志时的统计（算这一秒的增量）。
    reported: CaptureStats,
}

impl ScreenCapture {
    /// 开始采集，等到第一帧画面到手才返回（最多 [`FIRST_FRAME_TIMEOUT`]）。
    pub fn open(opts: CaptureOptions) -> Result<Self, PlatformError> {
        opts.validate()?;
        Self::start(
            opts.max_fps,
            Box::new(move || crate::sys::open_grabber(&opts)),
            FIRST_FRAME_TIMEOUT,
        )
    }

    pub(crate) fn start(
        max_fps: u32,
        opener: Opener,
        first_frame: Duration,
    ) -> Result<Self, PlatformError> {
        let interval = Duration::from_secs(1) / max_fps.max(1);
        let shared = Arc::new(Shared::default());
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("peerdesk-capture".into())
            .spawn(move || run(&worker, opener, interval))
            .map_err(|e| PlatformError::Os(format!("启动采集线程失败：{e}")))?;
        let mut me = Self {
            shared,
            current: None,
            current_seq: 0,
            thread: Some(thread),
            reported: CaptureStats::default(),
        };
        let failed = {
            let guard = me.shared.lock();
            let (mut st, _) = me
                .shared
                .wake
                .wait_timeout_while(guard, first_frame, |s| s.seq == 0 && s.failed.is_none())
                .unwrap_or_else(PoisonError::into_inner);
            me.current = st.latest.clone();
            me.current_seq = st.seq;
            st.failed.take()
        };
        // 出错或超时时 `me` 在这里被丢弃，采集线程随之停止。
        match (failed, me.current.is_some()) {
            (Some(e), _) => Err(e),
            (None, true) => Ok(me),
            (None, false) => Err(PlatformError::Timeout(format!(
                "{} 秒内没有采集到第一帧画面",
                first_frame.as_secs_f32()
            ))),
        }
    }

    /// 统计数据。
    #[must_use]
    pub fn stats(&self) -> CaptureStats {
        self.shared.lock().stats.clone()
    }

    /// 换上最新的一帧（有新的话）。
    fn refresh(&mut self) {
        let mut st = self.shared.lock();
        let fresh = st.latest.clone().filter(|_| st.seq != self.current_seq);
        if let Some(latest) = fresh {
            let old = self.current.replace(latest);
            self.current_seq = st.seq;
            if st.returned.is_none() {
                st.returned = old;
            }
        }
    }

    /// 最新的一帧读成 RGB 画面。显卡画面会读回内存、在 CPU 上转颜色：只给截图检查、体检和测试用，
    /// 会话用 [`ScreenSource::frame`]。
    pub fn snapshot(&mut self) -> Option<Screen> {
        self.refresh();
        self.current.as_ref().and_then(|f| f.to_screen().ok())
    }
}

impl ScreenSource for ScreenCapture {
    fn frame(&mut self) -> Option<Frame> {
        self.refresh();
        self.current.clone()
    }

    fn change_seq(&self) -> Option<u64> {
        Some(self.current_seq)
    }

    fn native_size(&self) -> Option<(u16, u16)> {
        self.shared.lock().native
    }

    fn set_target_size(&mut self, size: (u16, u16)) {
        self.shared.lock().target = Some(size);
        self.shared.wake.notify_all();
    }

    fn stats_line(&mut self) -> Option<String> {
        let (now, max) = {
            let mut st = self.shared.lock();
            (st.stats.clone(), std::mem::take(&mut st.window_max))
        };
        let grabs = now.grabs.saturating_sub(self.reported.grabs);
        let spent = now.total_grab.saturating_sub(self.reported.total_grab);
        let avg = spent
            .checked_div(u32::try_from(grabs).unwrap_or(u32::MAX))
            .unwrap_or_default();
        let line = format!(
            "采集 新画面 {} 帧，读取 {} 次，读取耗时 {:.1}/{:.1} ms；{}；出错重开 {} 次{}",
            now.frames.saturating_sub(self.reported.frames),
            grabs,
            avg.as_secs_f64() * 1000.0,
            max.as_secs_f64() * 1000.0,
            self.current
                .as_ref()
                .map(|f| {
                    let (w, h) = f.size();
                    let kind = match f {
                        Frame::Gpu(_) => "系统在显卡上缩放、转好的 NV12，CPU 不碰像素",
                        Frame::Rgb(_) => "从系统读出并在 CPU 上转成 RGB",
                    };
                    format!("画面 {w}×{h}（{kind}）")
                })
                .unwrap_or_default(),
            now.reopens.saturating_sub(self.reported.reopens),
            now.last_error
                .as_ref()
                .filter(|_| now.reopens != self.reported.reopens)
                .map_or_else(String::new, |e| format!("（{e}）")),
        );
        self.reported = now;
        Some(line)
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.shared.lock().stop = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peerdesk_core::frame::{Nv12, TestGpuFrame};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// 脚本里的一步。
    #[derive(Debug, Clone)]
    enum Step {
        /// 画面变成这个颜色。
        Color(u32),
        /// 没有变化。
        Idle,
        /// 等待时出错。
        WaitFails,
        /// 读取时出错。
        GrabFails,
    }

    #[derive(Debug, Default)]
    struct Script {
        steps: Mutex<VecDeque<Step>>,
        color: Mutex<u32>,
        reused: AtomicU32,
        opened: AtomicU32,
        /// 交显卡画面（按要的尺寸），不交 CPU 画面。
        gpu: bool,
        /// 要过的采集尺寸。
        targets: Mutex<Vec<(u16, u16)>>,
        /// 改尺寸失败一次。
        target_fails: std::sync::atomic::AtomicBool,
    }

    struct Fake(Arc<Script>);

    impl Grabber for Fake {
        fn wait(&mut self, timeout: Duration) -> Result<bool, PlatformError> {
            let step = self.0.steps.lock().unwrap().pop_front();
            match step {
                Some(Step::Color(c)) => {
                    *self.0.color.lock().unwrap() = c;
                    Ok(true)
                }
                Some(Step::GrabFails) => {
                    *self.0.color.lock().unwrap() = u32::MAX;
                    Ok(true)
                }
                Some(Step::WaitFails) => Err(PlatformError::Lost("测试".into())),
                Some(Step::Idle) => Ok(false),
                None => {
                    std::thread::sleep(timeout.min(Duration::from_millis(5)));
                    Ok(false)
                }
            }
        }

        fn grab(&mut self, spare: Option<Screen>) -> Result<Frame, PlatformError> {
            let color = *self.0.color.lock().unwrap();
            if color == u32::MAX {
                return Err(PlatformError::Os("读取失败".into()));
            }
            if spare.is_some() {
                self.0.reused.fetch_add(1, Ordering::Relaxed);
            }
            if self.0.gpu {
                let (w, h) = self
                    .0
                    .targets
                    .lock()
                    .unwrap()
                    .last()
                    .copied()
                    .unwrap_or((8, 4));
                let (wu, hu) = (usize::from(w), usize::from(h));
                let mut data = vec![u8::try_from(color % 200 + 16).unwrap(); wu * hu];
                data.extend(std::iter::repeat_n(128u8, wu * hu / 2));
                let nv12 = Nv12::new(w, h, data).unwrap();
                return Ok(Frame::Gpu(Arc::new(TestGpuFrame::new(nv12))));
            }
            Ok(Frame::rgb(Screen::new(4, 2, color)))
        }

        fn set_target(&mut self, size: (u16, u16)) -> Result<(), PlatformError> {
            if self.0.target_fails.swap(false, Ordering::Relaxed) {
                return Err(PlatformError::Os("改尺寸失败".into()));
            }
            self.0.targets.lock().unwrap().push(size);
            Ok(())
        }

        fn native_size(&self) -> Option<(u16, u16)> {
            Some((16, 8))
        }
    }

    fn fake(s: &Arc<Script>) -> Box<dyn Grabber> {
        Box::new(Fake(Arc::clone(s)))
    }

    fn script(steps: &[Step]) -> Arc<Script> {
        let s = Arc::new(Script::default());
        s.steps.lock().unwrap().extend(steps.iter().cloned());
        s
    }

    fn opener(s: &Arc<Script>) -> Opener {
        let s = Arc::clone(s);
        Box::new(move || {
            s.opened.fetch_add(1, Ordering::Relaxed);
            Ok(fake(&s))
        })
    }

    fn push(s: &Script, steps: &[Step]) {
        s.steps.lock().unwrap().extend(steps.iter().cloned());
    }

    /// 等脚本走完，再多等一会儿让最后一步处理完。
    fn drain(s: &Script) {
        let started = Instant::now();
        while !s.steps.lock().unwrap().is_empty() {
            assert!(started.elapsed() < Duration::from_secs(5), "脚本没有走完");
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(30));
    }

    fn color(cap: &mut ScreenCapture) -> u32 {
        cap.snapshot().unwrap().pixels()[0]
    }

    #[test]
    fn counts_only_real_changes() {
        let s = script(&[Step::Color(1)]);
        let mut cap = ScreenCapture::start(1000, opener(&s), Duration::from_secs(5)).unwrap();
        assert_eq!(color(&mut cap), 1);
        assert_eq!(cap.change_seq(), Some(1));
        // 什么都没发生、系统报告变化但内容一样：计数不变
        push(&s, &[Step::Idle, Step::Color(1), Step::Idle]);
        drain(&s);
        assert_eq!(color(&mut cap), 1);
        assert_eq!(cap.change_seq(), Some(1));
        // 真的变了
        push(&s, &[Step::Color(2)]);
        drain(&s);
        assert_eq!(color(&mut cap), 2);
        assert_eq!(cap.change_seq(), Some(2));
        // 连着变了几次：只拿到最新的一帧，计数跟着加
        push(&s, &[Step::Color(3), Step::Color(4), Step::Color(5)]);
        drain(&s);
        assert_eq!(color(&mut cap), 5);
        assert_eq!(cap.change_seq(), Some(5));
        // 计数只在取画面时更新：两次取之间的 change_seq 与拿到的画面对应
        push(&s, &[Step::Color(6)]);
        drain(&s);
        assert_eq!(cap.change_seq(), Some(5));
        assert_eq!(color(&mut cap), 6);
        let stats = cap.stats();
        assert_eq!(stats.frames, 6);
        assert_eq!(stats.grabs, 7);
        assert!(stats.mean_grab() <= stats.total_grab);
        assert_eq!(stats.reopens, 0);
        // 诊断日志：这一秒的增量，取完从零算
        let line = cap.stats_line().unwrap();
        assert!(
            line.contains("新画面 6 帧，读取 7 次") && line.contains("出错重开 0 次"),
            "{line}"
        );
        let line = cap.stats_line().unwrap();
        assert!(line.contains("新画面 0 帧，读取 0 次"), "{line}");
        // 旧缓冲区被还回来复用
        assert!(s.reused.load(Ordering::Relaxed) > 0);
    }

    #[test]
    fn reopens_after_errors() {
        let s = script(&[Step::Color(1)]);
        let mut cap = ScreenCapture::start(1000, opener(&s), Duration::from_secs(5)).unwrap();
        push(&s, &[Step::WaitFails, Step::Color(2)]);
        drain(&s);
        assert_eq!(color(&mut cap), 2);
        push(&s, &[Step::GrabFails, Step::Color(3)]);
        drain(&s);
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(color(&mut cap), 3);
        assert_eq!(s.opened.load(Ordering::Relaxed), 3);
        let stats = cap.stats();
        assert_eq!(stats.reopens, 2);
        assert_eq!(stats.last_error, Some(PlatformError::Os("读取失败".into())));
    }

    #[test]
    fn keeps_retrying_when_reopening_fails() {
        let s = script(&[Step::Color(1), Step::WaitFails]);
        let attempts = Arc::new(AtomicU32::new(0));
        let (s2, a2) = (Arc::clone(&s), Arc::clone(&attempts));
        let opener: Opener = Box::new(move || {
            // 第一次成功，接下来两次失败，之后又成功
            match a2.fetch_add(1, Ordering::Relaxed) {
                1 | 2 => Err(PlatformError::Lost("显示器不见了".into())),
                _ => Ok(fake(&s2)),
            }
        });
        let mut cap = ScreenCapture::start(1000, opener, Duration::from_secs(5)).unwrap();
        let started = Instant::now();
        while attempts.load(Ordering::Relaxed) < 4 {
            assert!(started.elapsed() < Duration::from_secs(5), "没有重试");
            std::thread::sleep(Duration::from_millis(10));
        }
        push(&s, &[Step::Color(7)]);
        drain(&s);
        assert_eq!(color(&mut cap), 7);
        assert_eq!(cap.stats().reopens, 3);
    }

    #[test]
    fn errors_before_the_first_frame_are_reported() {
        let failing: Opener = Box::new(|| Err(PlatformError::Unsupported("没有桌面".into())));
        let err = ScreenCapture::start(30, failing, Duration::from_secs(5)).unwrap_err();
        assert_eq!(err, PlatformError::Unsupported("没有桌面".into()));
        let s = script(&[Step::GrabFails]);
        let err = ScreenCapture::start(30, opener(&s), Duration::from_secs(5)).unwrap_err();
        assert_eq!(err, PlatformError::Os("读取失败".into()));
        let s = script(&[Step::WaitFails]);
        let err = ScreenCapture::start(30, opener(&s), Duration::from_secs(5)).unwrap_err();
        assert!(matches!(err, PlatformError::Lost(_)));
    }

    #[test]
    fn times_out_without_a_first_frame() {
        let s = script(&[]);
        let err = ScreenCapture::start(30, opener(&s), Duration::from_millis(50)).unwrap_err();
        assert!(matches!(err, PlatformError::Timeout(_)), "{err}");
        assert!(err.to_string().contains("第一帧"));
    }

    #[test]
    fn frame_rate_is_capped() {
        let s = script(&[Step::Color(0)]);
        let mut cap = ScreenCapture::start(20, opener(&s), Duration::from_secs(5)).unwrap();
        // 画面一直在变：20 fps 的上限下，200 ms 里最多再采集 4 到 5 帧
        push(&s, &(1..200).map(Step::Color).collect::<Vec<_>>());
        std::thread::sleep(Duration::from_millis(200));
        let seq = {
            cap.frame();
            cap.change_seq().unwrap()
        };
        assert!((2..=7).contains(&seq), "{seq}");
    }

    #[test]
    fn dropping_stops_the_thread() {
        let s = script(&[Step::Color(1)]);
        let cap = ScreenCapture::start(1, opener(&s), Duration::from_secs(5)).unwrap();
        // 1 fps：线程正在睡到下一次读取，丢弃时要能马上醒来退出
        let started = Instant::now();
        drop(cap);
        assert!(started.elapsed() < Duration::from_millis(500));
        // 线程退出后 opener 闭包也被释放，只剩这里的一个引用
        assert_eq!(Arc::strong_count(&s), 1);
    }

    /// 显卡画面：会话要的尺寸交给系统（重新打开后再交一次），交来的画面就是这个尺寸；
    /// 内容比较不了，系统交来就算变了；诊断日志写明 CPU 不碰像素。
    #[test]
    fn gpu_frames_follow_the_target_size() {
        let s = Arc::new(Script {
            gpu: true,
            ..Script::default()
        });
        push(&s, &[Step::Color(1)]);
        let mut cap = ScreenCapture::start(1000, opener(&s), Duration::from_secs(5)).unwrap();
        assert_eq!(cap.native_size(), Some((16, 8)));
        assert_eq!(
            cap.frame().unwrap().size(),
            (8, 4),
            "还没要尺寸：系统默认的"
        );
        cap.set_target_size((6, 2));
        push(&s, &[Step::Idle, Step::Color(1)]);
        drain(&s);
        let frame = cap.frame().unwrap();
        assert!(matches!(frame, Frame::Gpu(_)));
        assert_eq!(frame.size(), (6, 2));
        assert_eq!(cap.change_seq(), Some(2), "同样的内容也算变了");
        assert_eq!(*s.targets.lock().unwrap(), vec![(6, 2)]);
        // 读成 RGB 给检查用
        assert_eq!(cap.snapshot().unwrap().width(), 6);
        let line = cap.stats_line().unwrap();
        assert!(line.contains("画面 6×2（系统在显卡上缩放"), "{line}");
        // 改尺寸失败：重新打开，再交一次
        s.target_fails.store(true, Ordering::Relaxed);
        cap.set_target_size((4, 2));
        push(&s, &[Step::Idle, Step::Idle, Step::Color(2)]);
        drain(&s);
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(*s.targets.lock().unwrap(), vec![(6, 2), (4, 2)]);
        assert_eq!(s.opened.load(Ordering::Relaxed), 2);
        assert_eq!(cap.frame().unwrap().size(), (4, 2));
    }

    /// 不能在采集时缩放的系统（X11、Windows 目前）：改尺寸什么都不做，不知道原始尺寸。
    #[test]
    fn grabbers_ignore_the_target_by_default() {
        struct Plain;
        impl Grabber for Plain {
            fn wait(&mut self, _timeout: Duration) -> Result<bool, PlatformError> {
                Ok(true)
            }
            fn grab(&mut self, _spare: Option<Screen>) -> Result<Frame, PlatformError> {
                Ok(Frame::rgb(Screen::new(2, 2, 0)))
            }
        }
        let mut g = Plain;
        assert!(g.wait(Duration::ZERO).unwrap());
        assert_eq!(g.grab(None).unwrap().size(), (2, 2));
        assert!(g.set_target((2, 2)).is_ok());
        assert_eq!(g.native_size(), None);
    }

    #[test]
    fn cpu_frames_say_so_in_the_log() {
        let s = script(&[Step::Color(1)]);
        let mut cap = ScreenCapture::start(1000, opener(&s), Duration::from_secs(5)).unwrap();
        let line = cap.stats_line().unwrap();
        assert!(
            line.contains("画面 4×2（从系统读出并在 CPU 上转成 RGB）"),
            "{line}"
        );
    }

    #[test]
    fn retry_delays_grow_and_are_capped() {
        assert_eq!(retry_delay(0), RETRY_MIN);
        assert_eq!(retry_delay(1), RETRY_MIN);
        assert_eq!(retry_delay(2), RETRY_MIN * 2);
        assert_eq!(retry_delay(100), RETRY_MAX);
        assert_eq!(CaptureStats::default().mean_grab(), Duration::ZERO);
    }
}
