//! macOS 的系统胶水：显示器枚举、系统权限、ScreenCaptureKit 采集、`CGEvent` 注入。
//!
//! * 采集屏幕需要“屏幕与系统录音”权限，注入键鼠需要“辅助功能”权限，都只能由本机用户在
//!   “系统设置 → 隐私与安全性”里授予。没有权限时返回 [`PlatformError::Permission`]，说明去哪里授权；
//! * ScreenCaptureKit 只在画面有变化时交付 `Complete` 状态的样本，静止时是 `Idle`（没有画面），
//!   直接忽略：静止的桌面不产生帧。系统按 `minimumFrameInterval` 限制帧率，输出 BGRA，不画光标；
//! * 坐标：macOS 的全局坐标以“点”为单位（Retina 屏 1 点 = 2 像素），原点是主显示器左上角，y 向下。
//!
//! 部分代码改编自 Airhand（MIT 许可，见仓库根目录的 THIRD_PARTY_NOTICES.md）：
//! `client/src-tauri/src/host/sck.rs`（流的回调对象、异步调用的等待、帧状态）、
//! `client/src-tauri/src/host/macos.rs`（权限、显示器枚举）、
//! `client/src-tauri/src/host/capture/sck.rs`（采集配置、最新样本的交接）、
//! `client/src-tauri/src/host/input/macos.rs`（键鼠事件的构造、拖动、点击次数、滚动）。

#![allow(unsafe_code)]

use std::collections::HashSet;
use std::ffi::c_void;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, Once, PoisonError};
use std::time::{Duration, Instant};

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayIsMain,
    CGDisplayMirrorsDisplay, CGDisplayMode, CGError, CGEvent, CGEventField, CGEventFlags,
    CGEventSource, CGEventSourceStateID, CGEventTapLocation, CGEventType, CGGetActiveDisplayList,
    CGMouseButton, CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess, CGScrollEventUnit,
};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::{
    kCVPixelFormatType_32BGRA, CVPixelBuffer, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType,
    CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
    CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSString,
};
use objc2_screen_capture_kit::{
    SCContentFilter, SCDisplay, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamDelegate, SCStreamOutput, SCStreamOutputType,
};
use peerdesk_core::input::InputEvent;
use peerdesk_core::screen::Screen;
use peerdesk_net::host::InputSink;

use crate::capture::Grabber;
use crate::keymap::{mac_event_flags, mac_key_flags, to_mac};
use crate::pixel::bgra_to_screen;
use crate::pointer::{round_to_i32, to_points, ClickTracker, WheelAccumulator};
use crate::{sort_displays, CaptureOptions, DisplayInfo, PermissionStatus};
use crate::{PlatformError, PlatformError as E, INPUT_PERMISSION_HINT, SCREEN_PERMISSION_HINT};

/// ScreenCaptureKit 的异步调用最多等这么久。
const SCK_TIMEOUT: Duration = Duration::from_secs(5);
/// 系统最多同时拿着几帧（会话还拿着的帧系统不能复用）。
const QUEUE_DEPTH: isize = 3;
/// 注入时用的显示器位置多久刷新一次。
const GEOMETRY_REFRESH: Duration = Duration::from_secs(2);
/// 滚动：每 6 个增量发 5 个像素，滚轮一格（120）= 100 像素 ≈ 3 行。
const WHEEL_UNITS_PER_STEP: i32 = 6;
const WHEEL_PIXELS_PER_STEP: i32 = 5;
const PIXELS_PER_LINE: f64 = 100.0 / 3.0;
/// 一个键盘事件最多带 20 个 UTF-16 字符（系统的限制）。
const TEXT_CHUNK: usize = 20;
/// 键码：Caps Lock、Return、Tab。
const KVK_CAPS_LOCK: u16 = 0x39;
const KVK_RETURN: u16 = 0x24;
const KVK_TAB: u16 = 0x30;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> u8;
    static kAXTrustedCheckOptionPrompt: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> *const c_void;
    fn CFRelease(object: *const c_void);
}

/// 系统设置里“屏幕录制”和“辅助功能”页面的地址（macOS 10.15 到 26 都认）。
const SCREEN_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture";
const INPUT_SETTINGS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";

pub(crate) fn current_os() -> crate::guide::Os {
    // 读不到版本号时按较新的系统处理。
    let (major, minor) = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|v| crate::guide::parse_mac_version(&v))
        .unwrap_or((15, 0));
    crate::guide::Os::MacOs { major, minor }
}

fn open_url(url: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(url)
        .status()
        .map_err(|e| format!("打不开系统设置：{e}"))
        .and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err(format!("打不开系统设置（open 退出码 {s}）"))
            }
        })
}

/// 请系统弹出“辅助功能”授权对话框；同时把本程序加进设置里的列表。
fn prompt_accessibility() {
    // SAFETY: 系统导出的常量和 CoreFoundation 的标准用法：用一对键值建一个字典，
    // 传给查询函数，然后释放。键值数组在调用期间有效，字典由我们创建、由我们释放。
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
            std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
        );
        if !options.is_null() {
            let _ = AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
        }
    }
}

/// 一秒后重新打开本程序（程序包时用 `open -n`），然后退出当前进程。
fn relaunch() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("找不到程序位置：{e}"))?;
    let target = exe
        .ancestors()
        .find(|p| p.extension().is_some_and(|e| e == "app"))
        .map_or_else(|| exe.clone(), Path::to_path_buf);
    std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg("sleep 1; open -n \"$0\"")
        .arg(&target)
        .spawn()
        .map_err(|e| format!("无法重新打开：{e}"))?;
    std::process::exit(0)
}

pub(crate) fn perform(action: crate::guide::Action) -> Result<(), String> {
    use crate::guide::Action;
    match action {
        Action::OpenScreenSettings => open_url(SCREEN_SETTINGS_URL),
        Action::OpenInputSettings => open_url(INPUT_SETTINGS_URL),
        Action::RequestScreen => {
            let _ = CGRequestScreenCaptureAccess();
            // 以前拒绝过时系统不再弹窗：直接打开设置页面。
            open_url(SCREEN_SETTINGS_URL)
        }
        Action::RequestInput => {
            prompt_accessibility();
            Ok(())
        }
        Action::Relaunch => relaunch(),
    }
}

fn screen_allowed() -> bool {
    CGPreflightScreenCaptureAccess()
}

fn input_allowed() -> bool {
    // SAFETY: 无参数的查询函数。
    unsafe { AXIsProcessTrusted() != 0 }
}

/// 检查“屏幕录制”权限；没有时请求一次（系统弹出对话框，或者把本程序加进设置里的列表）。
fn require_screen_permission() -> Result<(), PlatformError> {
    static REQUEST: Once = Once::new();
    if screen_allowed() {
        return Ok(());
    }
    REQUEST.call_once(|| {
        let _ = CGRequestScreenCaptureAccess();
    });
    if screen_allowed() {
        Ok(())
    } else {
        Err(E::Permission(SCREEN_PERMISSION_HINT.into()))
    }
}

pub(crate) fn permissions() -> PermissionStatus {
    PermissionStatus {
        screen_capture: screen_allowed(),
        input_injection: input_allowed(),
    }
}

// ───────────────────────────── 显示器 ─────────────────────────────

/// 一个显示器：系统编号和在全局坐标系里的范围（点）。
#[derive(Debug, Clone, PartialEq)]
struct Display {
    id: CGDirectDisplayID,
    info: DisplayInfo,
    /// 左上角和大小（点）。
    origin: (f64, f64),
    size: (f64, f64),
}

fn list_displays() -> Vec<Display> {
    let mut ids = [0 as CGDirectDisplayID; 32];
    let mut n = 0u32;
    let max = u32::try_from(ids.len()).unwrap_or(0);
    // SAFETY: `ids` 有 `max` 个元素的空间，`n` 是有效、可写的 u32。
    let err = unsafe { CGGetActiveDisplayList(max, ids.as_mut_ptr(), &mut n) };
    if err != CGError::Success {
        return Vec::new();
    }
    let count = usize::try_from(n).unwrap_or(0).min(ids.len());
    let mut list: Vec<Display> = ids
        .iter()
        .take(count)
        // 镜像显示器只算一个
        .filter(|&&id| CGDisplayMirrorsDisplay(id) == 0)
        .filter_map(|&id| {
            let b = CGDisplayBounds(id);
            let mode = CGDisplayCopyDisplayMode(id);
            let pixels = mode.as_deref().map(|m| {
                (
                    CGDisplayMode::pixel_width(Some(m)),
                    CGDisplayMode::pixel_height(Some(m)),
                )
            });
            let (pw, ph) = match pixels {
                Some((w, h)) if w > 0 && h > 0 => (w, h),
                Some(_) | None => (
                    usize::try_from(round_to_i32(b.size.width)).ok()?,
                    usize::try_from(round_to_i32(b.size.height)).ok()?,
                ),
            };
            let (width, height) = (u32::try_from(pw).ok()?, u32::try_from(ph).ok()?);
            if width == 0 || height == 0 || b.size.width <= 0.0 {
                return None;
            }
            Some(Display {
                id,
                info: DisplayInfo {
                    index: 0,
                    name: format!("display-{id}"),
                    x: round_to_i32(b.origin.x),
                    y: round_to_i32(b.origin.y),
                    width,
                    height,
                    scale: f64::from(width) / b.size.width,
                    primary: CGDisplayIsMain(id),
                },
                origin: (b.origin.x, b.origin.y),
                size: (b.size.width, b.size.height),
            })
        })
        .collect();
    let infos = sort_displays(list.iter().map(|d| d.info.clone()).collect());
    list.sort_by_key(|d| (!d.info.primary, d.info.x, d.info.y));
    for (d, info) in list.iter_mut().zip(infos) {
        d.info = info;
    }
    list
}

fn display(index: usize) -> Result<Display, PlatformError> {
    let list = list_displays();
    let count = list.len();
    list.into_iter()
        .nth(index)
        .ok_or(E::NoDisplay { index, count })
}

pub(crate) fn displays() -> Result<Vec<DisplayInfo>, PlatformError> {
    Ok(list_displays().into_iter().map(|d| d.info).collect())
}

pub(crate) fn cursor_position() -> Option<(i32, i32)> {
    let event = CGEvent::new(None)?;
    let p = CGEvent::location(Some(&event));
    Some((round_to_i32(p.x), round_to_i32(p.y)))
}

// ───────────────────────────── ScreenCaptureKit ─────────────────────────────

/// 错误的说明：错误域和错误码。
fn error_code(e: &NSError) -> String {
    format!(
        "{} {}（{}）",
        e.domain(),
        e.code(),
        e.localizedDescription()
    )
}

/// 已经 retain 过的样本，交给采集线程（CMSampleBuffer 的引用计数是线程安全的）。
struct Sample(CFRetained<CMSampleBuffer>);

// SAFETY: CMSampleBuffer 是不可变的 CoreFoundation 对象，引用计数线程安全；系统交出来之后不再修改它。
unsafe impl Send for Sample {}

/// 把不是 `Send` 的对象交给另一个线程（只在交接时用，之后只有一个线程访问）。
struct Handoff<T>(T);

// SAFETY: 只用于把 SCShareableContent 从回调队列交给等待它的线程，交接后只有接收方访问。
unsafe impl<T> Send for Handoff<T> {}

/// 采集流交给采集线程的东西：最新的一帧（旧的直接被替换），或者流被系统停止的原因。
#[derive(Default)]
struct Inbox {
    state: Mutex<(Option<Sample>, Option<String>)>,
    wake: Condvar,
}

type OnSample = Box<dyn Fn(&CMSampleBuffer, SCStreamOutputType) + Send + Sync>;
type OnStop = Box<dyn Fn(String) + Send + Sync>;

struct Handlers {
    on_sample: OnSample,
    on_stop: OnStop,
}

define_class!(
    // SAFETY: NSObject 没有特殊的子类化要求；本类没有实现 Drop，实例变量随对象正常释放。
    #[unsafe(super(NSObject))]
    #[name = "PeerDeskStreamOutput"]
    #[ivars = Handlers]
    struct StreamOutput;

    unsafe impl NSObjectProtocol for StreamOutput {}

    unsafe impl SCStreamOutput for StreamOutput {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample: &CMSampleBuffer,
            kind: SCStreamOutputType,
        ) {
            (self.ivars().on_sample)(sample, kind);
        }
    }

    unsafe impl SCStreamDelegate for StreamOutput {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_did_stop(&self, _stream: &SCStream, error: &NSError) {
            (self.ivars().on_stop)(error_code(error));
        }
    }
);

impl StreamOutput {
    fn new(handlers: Handlers) -> Retained<Self> {
        let this = Self::alloc().set_ivars(handlers);
        // SAFETY: 调用父类 NSObject 的 init，返回初始化好的对象。
        unsafe { msg_send![super(this), init] }
    }
}

/// ScreenCaptureKit 里与某个显示器编号对应的对象。
fn shareable_display(id: CGDirectDisplayID) -> Result<Retained<SCDisplay>, PlatformError> {
    let (tx, rx) = mpsc::channel::<Result<Handoff<Retained<SCShareableContent>>, String>>();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // SAFETY: 系统传来的指针要么为空，要么指向有效对象；retain 一份交给等待的线程。
            let result = match unsafe { Retained::retain(content) } {
                Some(c) => Ok(Handoff(c)),
                // SAFETY: 同上，`error` 为空或指向有效的 NSError。
                None => {
                    Err(unsafe { error.as_ref() }.map_or_else(|| "未知错误".into(), error_code))
                }
            };
            let _ = tx.send(result);
        },
    );
    // SAFETY: 完成回调是一个有效的 block，系统在任意队列上调用它一次。
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            false, true, &block,
        );
    }
    let content = rx
        .recv_timeout(SCK_TIMEOUT)
        .map_err(|_| E::Timeout("ScreenCaptureKit 没有及时响应".into()))?
        .map_err(|e| E::Os(format!("列不出可以采集的屏幕（SCShareableContent：{e}）")))?
        .0;
    // SAFETY: `content` 是有效的 SCShareableContent。
    let list = unsafe { content.displays() };
    (0..list.count())
        .map(|i| list.objectAtIndex(i))
        // SAFETY: `d` 是有效的 SCDisplay。
        .find(|d| unsafe { d.displayID() } == id)
        .ok_or_else(|| E::Os(format!("ScreenCaptureKit 不认识显示器 {id}")))
}

/// 调用一个带完成回调的异步方法并等待结果。
fn wait_completion(
    call: impl FnOnce(&block2::DynBlock<dyn Fn(*mut NSError)>),
) -> Result<(), String> {
    let (tx, rx) = mpsc::channel::<Option<String>>();
    let block = RcBlock::new(move |e: *mut NSError| {
        // SAFETY: `e` 为空或指向有效的 NSError。
        let _ = tx.send(unsafe { e.as_ref() }.map(error_code));
    });
    call(&block);
    match rx.recv_timeout(SCK_TIMEOUT) {
        Ok(None) => Ok(()),
        Ok(Some(e)) => Err(e),
        Err(_) => Err("超时".into()),
    }
}

/// 画面样本的状态：只有 `Complete` 的样本带着新画面，`Idle` 表示画面没有变化。
fn frame_status(sample: &CMSampleBuffer) -> Option<isize> {
    // SAFETY: 读取样本的附加信息，不创建新的。
    let array = unsafe { sample.sample_attachments_array(false) }?;
    // SAFETY: CFArray 与 NSArray、CFDictionary 与 NSDictionary 是“免费桥接”的同一种对象，
    // 这里的数组元素是以字符串为键的字典；引用借自 `array`，在它被释放之前用完。
    let array: &NSArray<NSDictionary<NSString, NSNumber>> =
        unsafe { &*CFRetained::as_ptr(&array).as_ptr().cast() };
    if array.count() == 0 {
        return None;
    }
    let info = array.objectAtIndex(0);
    // SAFETY: 系统定义的常量字符串。
    let key: &NSString = unsafe { objc2_screen_capture_kit::SCStreamFrameInfoStatus };
    info.objectForKey(key).map(|n| n.integerValue())
}

struct SckGrabber {
    stream: Retained<SCStream>,
    // 流只弱引用回调对象，需要自己持有；队列同理。
    _output: Retained<StreamOutput>,
    _queue: DispatchRetained<DispatchQueue>,
    inbox: Arc<Inbox>,
    pending: Option<Sample>,
}

pub(crate) fn open_grabber(opts: &CaptureOptions) -> Result<Box<dyn Grabber>, PlatformError> {
    require_screen_permission()?;
    let d = display(opts.display)?;
    let target = shareable_display(d.id)?;
    let fps = i32::try_from(opts.max_fps.max(1)).unwrap_or(60);
    // SAFETY: 新建并填写一个采集配置，所有参数都在系统允许的范围内。
    let config = unsafe {
        let c = SCStreamConfiguration::new();
        c.setWidth(usize::try_from(d.info.width).unwrap_or(0));
        c.setHeight(usize::try_from(d.info.height).unwrap_or(0));
        c.setPixelFormat(kCVPixelFormatType_32BGRA);
        c.setShowsCursor(false);
        c.setMinimumFrameInterval(CMTime::new(1, fps));
        c.setQueueDepth(QUEUE_DEPTH);
        c
    };
    let inbox = Arc::new(Inbox::default());
    let (rx, stop) = (Arc::clone(&inbox), Arc::clone(&inbox));
    let output = StreamOutput::new(Handlers {
        on_sample: Box::new(move |sample, kind| {
            if kind != SCStreamOutputType::Screen
                || frame_status(sample) != Some(SCFrameStatus::Complete.0)
            {
                return;
            }
            // SAFETY: `sample` 在回调期间有效，retain 一份留到采集线程处理完。
            let retained = unsafe { CFRetained::retain(NonNull::from(sample)) };
            let mut st = rx.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.0 = Some(Sample(retained));
            drop(st);
            rx.wake.notify_all();
        }),
        on_stop: Box::new(move |why| {
            let mut st = stop.state.lock().unwrap_or_else(PoisonError::into_inner);
            st.1 = Some(why);
            drop(st);
            stop.wake.notify_all();
        }),
    });
    let queue = DispatchQueue::new("com.peerdesk.capture", None);
    // SAFETY: 用整个显示器（不排除任何窗口）、上面的配置和回调对象创建采集流。
    let stream = unsafe {
        let filter = SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &target,
            &NSArray::new(),
        );
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            Some(ProtocolObject::from_ref(&*output)),
        )
    };
    // SAFETY: 回调对象和队列都由 SckGrabber 持有，生命周期不短于流。
    unsafe {
        stream.addStreamOutput_type_sampleHandlerQueue_error(
            ProtocolObject::from_ref(&*output),
            SCStreamOutputType::Screen,
            Some(&queue),
        )
    }
    .map_err(|e| E::Os(format!("SCStream addStreamOutput 失败：{}", error_code(&e))))?;
    // SAFETY: 完成回调是有效的 block。
    wait_completion(|done| unsafe { stream.startCaptureWithCompletionHandler(Some(done)) })
        .map_err(|e| E::Os(format!("无法开始 ScreenCaptureKit 采集：{e}")))?;
    Ok(Box::new(SckGrabber {
        stream,
        _output: output,
        _queue: queue,
        inbox,
        pending: None,
    }))
}

impl Drop for SckGrabber {
    fn drop(&mut self) {
        let stream = &self.stream;
        // SAFETY: 完成回调是有效的 block。停止失败也没有别的办法，只能放弃。
        let _ =
            wait_completion(|done| unsafe { stream.stopCaptureWithCompletionHandler(Some(done)) });
    }
}

impl Grabber for SckGrabber {
    fn wait(&mut self, timeout: Duration) -> Result<bool, PlatformError> {
        if self.pending.is_some() {
            return Ok(true);
        }
        let guard = self
            .inbox
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (mut st, _) = self
            .inbox
            .wake
            .wait_timeout_while(guard, timeout, |s| s.0.is_none() && s.1.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(why) = st.1.take() {
            return Err(E::Lost(format!("系统停止了屏幕采集：{why}")));
        }
        self.pending = st.0.take();
        Ok(self.pending.is_some())
    }

    fn grab(&mut self, spare: Option<Screen>) -> Result<Screen, PlatformError> {
        let sample = self
            .pending
            .take()
            .ok_or_else(|| E::Os("没有可读的画面".into()))?;
        // SAFETY: `sample` 是有效的画面样本。
        let image =
            unsafe { sample.0.image_buffer() }.ok_or_else(|| E::Os("画面样本里没有图像".into()))?;
        let pb: &CVPixelBuffer = &image;
        if CVPixelBufferGetPixelFormatType(pb) != kCVPixelFormatType_32BGRA {
            return Err(E::Os("ScreenCaptureKit 交来的像素格式不是 BGRA".into()));
        }
        let ro = CVPixelBufferLockFlags::ReadOnly;
        // SAFETY: 锁住像素缓冲区以便读取，下面一定解锁。
        if unsafe { CVPixelBufferLockBaseAddress(pb, ro) } != 0 {
            return Err(E::Os("无法锁定画面缓冲区".into()));
        }
        let (w, h) = (CVPixelBufferGetWidth(pb), CVPixelBufferGetHeight(pb));
        let stride = CVPixelBufferGetBytesPerRow(pb);
        let base = CVPixelBufferGetBaseAddress(pb).cast::<u8>();
        let result = if base.is_null() {
            Err(E::Os("画面缓冲区没有数据".into()))
        } else {
            // SAFETY: 锁定期间 `base` 指向 每行字节数 × 高度 字节的可读内存。
            let data = unsafe { std::slice::from_raw_parts(base, stride.saturating_mul(h)) };
            bgra_to_screen(spare, w, h, data, stride)
        };
        // SAFETY: 与上面成功的加锁配对。
        unsafe { CVPixelBufferUnlockBaseAddress(pb, ro) };
        result
    }
}

// ───────────────────────────── 注入 ─────────────────────────────

/// `CGEventSource` 只在持有注入器的线程上使用。
struct Source(CFRetained<CGEventSource>);

// SAFETY: 事件源只被持有它的注入器使用，注入器同一时间只在一个线程上（`&mut self`）。
unsafe impl Send for Source {}

struct MacInput {
    source: Option<Source>,
    index: usize,
    display: Display,
    refreshed: Instant,
    /// 按下的键（macOS 键码）。
    keys: HashSet<u16>,
    buttons: HashSet<u8>,
    caps_lock: bool,
    pos: Option<CGPoint>,
    clicks: ClickTracker,
    wheel: (WheelAccumulator, WheelAccumulator),
}

pub(crate) fn open_input(index: usize) -> Result<Box<dyn InputSink>, PlatformError> {
    let display = display(index)?;
    if !input_allowed() {
        return Err(E::Permission(INPUT_PERMISSION_HINT.into()));
    }
    Ok(Box::new(MacInput {
        source: None,
        index,
        display,
        refreshed: Instant::now(),
        keys: HashSet::new(),
        buttons: HashSet::new(),
        caps_lock: false,
        pos: None,
        clicks: ClickTracker::default(),
        wheel: (
            WheelAccumulator::new(WHEEL_UNITS_PER_STEP),
            WheelAccumulator::new(WHEEL_UNITS_PER_STEP),
        ),
    }))
}

fn cg_button(button: u8) -> CGMouseButton {
    match button {
        0 => CGMouseButton::Left,
        1 => CGMouseButton::Right,
        2 => CGMouseButton::Center,
        n => CGMouseButton(u32::from(n)),
    }
}

impl MacInput {
    fn source(&mut self) -> Option<CFRetained<CGEventSource>> {
        if self.source.is_none() {
            let s = CGEventSource::new(CGEventSourceStateID::CombinedSessionState)?;
            // 默认注入之后会屏蔽本地键鼠 0.25 秒；远程控制时本地用户也可能在操作，不屏蔽
            CGEventSource::set_local_events_suppression_interval(Some(&s), 0.0);
            CGEventSource::set_pixels_per_line(Some(&s), PIXELS_PER_LINE);
            self.source = Some(Source(s));
        }
        self.source.as_ref().map(|s| s.0.clone())
    }

    fn flags(&self) -> u64 {
        mac_event_flags(self.keys.iter().copied(), self.caps_lock)
    }

    fn post(event: &CGEvent, flags: u64) {
        CGEvent::set_flags(Some(event), CGEventFlags(flags));
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(event));
    }

    fn cursor(&self) -> CGPoint {
        self.pos.unwrap_or_else(|| {
            CGEvent::new(None).map_or(CGPoint::new(0.0, 0.0), |e| CGEvent::location(Some(&e)))
        })
    }

    fn global_point(&mut self, x: u16, y: u16) -> CGPoint {
        if self.refreshed.elapsed() > GEOMETRY_REFRESH {
            self.refreshed = Instant::now();
            if let Ok(d) = display(self.index) {
                self.display = d;
            }
        }
        let d = &self.display;
        let pixel = 1.0 / d.info.scale.max(1.0);
        CGPoint::new(
            to_points(x, d.origin.0, d.size.0, pixel),
            to_points(y, d.origin.1, d.size.1, pixel),
        )
    }

    /// 发一个移动事件（按住键时是拖动），带上移动量。
    fn post_move(&mut self, p: CGPoint, dx: i64, dy: i64) {
        let (kind, button) = if self.buttons.contains(&0) {
            (CGEventType::LeftMouseDragged, CGMouseButton::Left)
        } else if self.buttons.contains(&1) {
            (CGEventType::RightMouseDragged, CGMouseButton::Right)
        } else if let Some(b) = self.buttons.iter().next() {
            (CGEventType::OtherMouseDragged, cg_button(*b))
        } else {
            (CGEventType::MouseMoved, CGMouseButton::Left)
        };
        let flags = self.flags();
        let src = self.source();
        let Some(ev) = CGEvent::new_mouse_event(src.as_deref(), kind, p, button) else {
            return;
        };
        CGEvent::set_integer_value_field(Some(&ev), CGEventField::MouseEventDeltaX, dx);
        CGEvent::set_integer_value_field(Some(&ev), CGEventField::MouseEventDeltaY, dy);
        Self::post(&ev, flags);
    }

    fn mouse_button(&mut self, button: u8, down: bool) {
        let p = self.cursor();
        let count = if down {
            self.clicks.press(button, (p.x, p.y), Instant::now())
        } else {
            self.clicks.release(button)
        };
        let kind = match (button, down) {
            (0, true) => CGEventType::LeftMouseDown,
            (0, false) => CGEventType::LeftMouseUp,
            (1, true) => CGEventType::RightMouseDown,
            (1, false) => CGEventType::RightMouseUp,
            (_, true) => CGEventType::OtherMouseDown,
            (_, false) => CGEventType::OtherMouseUp,
        };
        let flags = self.flags();
        let src = self.source();
        let cg = cg_button(button);
        let Some(ev) = CGEvent::new_mouse_event(src.as_deref(), kind, p, cg) else {
            return;
        };
        CGEvent::set_integer_value_field(Some(&ev), CGEventField::MouseEventClickState, count);
        CGEvent::set_integer_value_field(
            Some(&ev),
            CGEventField::MouseEventButtonNumber,
            i64::from(cg.0),
        );
        Self::post(&ev, flags);
    }

    fn scroll(&mut self, dx: i16, dy: i16) {
        let py = self.wheel.1.push(dy) * WHEEL_PIXELS_PER_STEP;
        let px = self.wheel.0.push(dx) * WHEEL_PIXELS_PER_STEP;
        if px == 0 && py == 0 {
            return;
        }
        let flags = self.flags();
        let src = self.source();
        // wheel1 为正是向上，wheel2 为正是向左（我们的 dx 为正是向右）
        let Some(ev) = CGEvent::new_scroll_wheel_event2(
            src.as_deref(),
            CGScrollEventUnit::Pixel,
            2,
            py,
            -px,
            0,
        ) else {
            return;
        };
        Self::post(&ev, flags);
    }

    fn key(&mut self, kc: u16, down: bool, repeat: bool) {
        let flags = self.flags() | mac_key_flags(kc);
        let src = self.source();
        let Some(ev) = CGEvent::new_keyboard_event(src.as_deref(), kc, down) else {
            return;
        };
        if repeat {
            CGEvent::set_integer_value_field(Some(&ev), CGEventField::KeyboardEventAutorepeat, 1);
        }
        Self::post(&ev, flags);
    }

    fn tap(&mut self, kc: u16) {
        let src = self.source();
        for down in [true, false] {
            if let Some(ev) = CGEvent::new_keyboard_event(src.as_deref(), kc, down) {
                Self::post(&ev, 0);
            }
        }
    }

    fn unicode(&mut self, units: &[u16]) {
        if units.is_empty() {
            return;
        }
        let src = self.source();
        let len = units.len().try_into().unwrap_or(0);
        for down in [true, false] {
            if let Some(ev) = CGEvent::new_keyboard_event(src.as_deref(), 0, down) {
                // SAFETY: `units` 是有效的 UTF-16 数组，长度与 `len` 一致，调用期间不变。
                unsafe { CGEvent::keyboard_set_unicode_string(Some(&ev), len, units.as_ptr()) };
                // 不带修饰键：按住的 Command 之类不会把文字变成快捷键
                Self::post(&ev, 0);
            }
        }
    }

    /// 输入文字：换行、Tab 按真实的键，其余字符直接以 Unicode 字符串发送（不受键盘布局和输入法影响）。
    fn type_text(&mut self, text: &str) {
        let mut chunk: Vec<u16> = Vec::with_capacity(TEXT_CHUNK + 1);
        for ch in text.chars() {
            let special = match ch {
                '\r' | '\n' => Some(KVK_RETURN),
                '\t' => Some(KVK_TAB),
                c if c.is_control() => continue,
                _ => None,
            };
            if let Some(kc) = special {
                self.unicode(&chunk);
                chunk.clear();
                self.tap(kc);
                continue;
            }
            let mut buf = [0u16; 2];
            let units = ch.encode_utf16(&mut buf);
            if chunk.len() + units.len() > TEXT_CHUNK {
                self.unicode(&chunk);
                chunk.clear();
            }
            chunk.extend_from_slice(units);
        }
        self.unicode(&chunk);
    }

    fn release_all(&mut self) {
        // 先抬起普通键，再抬起修饰键
        let mut keys: Vec<u16> = self.keys.iter().copied().collect();
        keys.sort_by_key(|&k| crate::keymap::mac_modifier_flags(k).is_some());
        for kc in keys {
            self.keys.remove(&kc);
            self.key(kc, false, false);
        }
        for b in std::mem::take(&mut self.buttons) {
            self.mouse_button(b, false);
        }
        self.wheel.0.reset();
        self.wheel.1.reset();
    }
}

impl InputSink for MacInput {
    fn inject(&mut self, event: &InputEvent) {
        match event {
            InputEvent::MouseMove { x, y } => {
                let p = self.global_point(*x, *y);
                let prev = self.cursor();
                self.pos = Some(p);
                let (dx, dy) = (round_to_i32(p.x - prev.x), round_to_i32(p.y - prev.y));
                self.post_move(p, i64::from(dx), i64::from(dy));
            }
            InputEvent::MouseRelative { dx, dy } => {
                // 之后的移动、点击都从光标真正所在的位置算
                self.pos = None;
                let cur = self.cursor();
                let p = CGPoint::new(cur.x + f64::from(*dx), cur.y + f64::from(*dy));
                self.post_move(p, i64::from(*dx), i64::from(*dy));
            }
            InputEvent::MouseButton { button, down } => {
                if *button > 4 {
                    return;
                }
                if *down {
                    self.buttons.insert(*button);
                } else if !self.buttons.remove(button) {
                    // 没按下过的键不发送抬起，避免干扰本地用户
                    return;
                }
                self.mouse_button(*button, *down);
            }
            InputEvent::Wheel { dx, dy } => self.scroll(*dx, *dy),
            InputEvent::Key { code, down } => {
                let Some(kc) = to_mac(*code) else {
                    return;
                };
                let repeat = if *down {
                    !self.keys.insert(kc)
                } else if self.keys.remove(&kc) {
                    false
                } else {
                    return;
                };
                if kc == KVK_CAPS_LOCK && *down && !repeat {
                    self.caps_lock = !self.caps_lock;
                }
                self.key(kc, *down, repeat);
            }
            InputEvent::Text(text) => self.type_text(text),
            InputEvent::ReleaseAll => self.release_all(),
        }
    }
}

// ---------------------------------------------------------------- 系统声音（ScreenCaptureKit，macOS 13 起）

/// CoreAudio 的 AudioBuffer（只读其中的数据指针和长度）。
#[repr(C)]
struct AudioBufferRaw {
    channels: u32,
    bytes: u32,
    data: *mut c_void,
}

/// 最多两个缓冲（双声道分开存放时每个声道一个）。
#[repr(C)]
struct AudioBufferList2 {
    count: u32,
    buffers: [AudioBufferRaw; 2],
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sbuf: *const c_void,
        size_needed: *mut usize,
        list: *mut AudioBufferList2,
        list_size: usize,
        struct_alloc: *const c_void,
        block_alloc: *const c_void,
        flags: u32,
        block_out: *mut *const c_void,
    ) -> i32;
}

/// 从声音样本里取出交错的浮点采样（双声道）。格式不是预期的浮点时返回 `None`。
fn audio_samples(sample: &CMSampleBuffer) -> Option<Vec<f32>> {
    let mut list = AudioBufferList2 {
        count: 0,
        buffers: [
            AudioBufferRaw {
                channels: 0,
                bytes: 0,
                data: std::ptr::null_mut(),
            },
            AudioBufferRaw {
                channels: 0,
                bytes: 0,
                data: std::ptr::null_mut(),
            },
        ],
    };
    let mut block: *const c_void = std::ptr::null();
    // SAFETY: 传入有效的样本和足够大的 AudioBufferList（两个缓冲）；系统写入列表并返回 retain 过的块缓冲，
    // 用完后 CFRelease。标志 1 = 要求 16 字节对齐。
    let status = unsafe {
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
            std::ptr::from_ref(sample).cast(),
            std::ptr::null_mut(),
            &mut list,
            size_of::<AudioBufferList2>(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            &mut block,
        )
    };
    if status != 0 || block.is_null() {
        return None;
    }
    let slice = |b: &AudioBufferRaw| -> &[f32] {
        if b.data.is_null() {
            return &[];
        }
        // SAFETY: 系统给出的缓冲在块缓冲释放前有效；ScreenCaptureKit 的声音是 32 位浮点，长度按字节数换算。
        unsafe {
            std::slice::from_raw_parts(
                b.data.cast::<f32>(),
                usize::try_from(b.bytes).unwrap_or(0) / 4,
            )
        }
    };
    let out = match list.count {
        // 两个声道分开存放：交错
        2 => {
            let (l, r) = (slice(&list.buffers[0]), slice(&list.buffers[1]));
            l.iter().zip(r).flat_map(|(a, b)| [*a, *b]).collect()
        }
        // 一个缓冲：已经交错（双声道）或单声道
        1 if list.buffers[0].channels >= 2 => slice(&list.buffers[0]).to_vec(),
        1 => slice(&list.buffers[0])
            .iter()
            .flat_map(|v| [*v, *v])
            .collect(),
        _ => Vec::new(),
    };
    // SAFETY: 释放上面拿到的块缓冲（retain 过的）。
    unsafe { CFRelease(block) };
    Some(out)
}

/// 系统声音采集：持有它期间一直在采，丢弃时停止。
pub(crate) struct AudioGrabber {
    stream: Retained<SCStream>,
    _output: Retained<StreamOutput>,
    _queue: DispatchRetained<DispatchQueue>,
}

impl Drop for AudioGrabber {
    fn drop(&mut self) {
        let stream = &self.stream;
        // SAFETY: 完成回调是有效的 block。
        let _ =
            wait_completion(|done| unsafe { stream.stopCaptureWithCompletionHandler(Some(done)) });
    }
}

/// 开始采集本机正在播放的声音（不含本程序自己的声音，避免回声）。`on_samples` 收到交错的
/// 双声道 48 kHz 浮点采样。需要 macOS 13 和“屏幕录制”权限。
pub(crate) fn open_audio(
    on_samples: Box<dyn Fn(&[f32]) + Send + Sync>,
) -> Result<AudioGrabber, String> {
    if !objc2::available!(macos = 13.0) {
        return Err("远程声音需要 macOS 13 或更新的系统".into());
    }
    require_screen_permission().map_err(|e| e.to_string())?;
    let d = display(0).map_err(|e| e.to_string())?;
    let target = shareable_display(d.id).map_err(|e| e.to_string())?;
    // SAFETY: 新建并填写一个采集配置。画面只要 2×2、每秒 1 帧（不用，只是 ScreenCaptureKit 要求有画面）。
    let config = unsafe {
        let c = SCStreamConfiguration::new();
        c.setWidth(2);
        c.setHeight(2);
        c.setMinimumFrameInterval(CMTime::new(1, 1));
        c.setCapturesAudio(true);
        c.setSampleRate(48_000);
        c.setChannelCount(2);
        c.setExcludesCurrentProcessAudio(true);
        c
    };
    let output = StreamOutput::new(Handlers {
        on_sample: Box::new(move |sample, kind| {
            if kind != SCStreamOutputType::Audio {
                return;
            }
            if let Some(samples) = audio_samples(sample) {
                on_samples(&samples);
            }
        }),
        on_stop: Box::new(|why| peerdesk_net::note!(warn, "声音：系统停止了声音采集：{why}")),
    });
    let queue = DispatchQueue::new("com.peerdesk.audio", None);
    // SAFETY: 用整个显示器、上面的配置和回调对象创建采集流。
    let stream = unsafe {
        let filter = SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &target,
            &NSArray::new(),
        );
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            Some(ProtocolObject::from_ref(&*output)),
        )
    };
    // SAFETY: 回调对象和队列都由 AudioGrabber 持有，生命周期不短于流。
    unsafe {
        stream.addStreamOutput_type_sampleHandlerQueue_error(
            ProtocolObject::from_ref(&*output),
            SCStreamOutputType::Audio,
            Some(&queue),
        )
    }
    .map_err(|e| format!("SCStream addStreamOutput 失败：{}", error_code(&e)))?;
    // SAFETY: 完成回调是有效的 block。
    wait_completion(|done| unsafe { stream.startCaptureWithCompletionHandler(Some(done)) })
        .map_err(|e| format!("无法开始声音采集：{e}"))?;
    Ok(AudioGrabber {
        stream,
        _output: output,
        _queue: queue,
    })
}
