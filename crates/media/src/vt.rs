//! macOS 硬件 H.264 编解码（系统胶水）：VideoToolbox。
//!
//! - 编码：`VTCompressionSession`，要求硬件编码器（没有就失败，交回 OpenH264）；Baseline、实时、
//!   不重排帧、按预算调平均码率；需要时强制关键帧。输出是 AVCC，转成 Annex B，关键帧前补 SPS、PPS
//!   （接收端是 OpenH264 时也解得了）。每帧编完立即 `CompleteFrames`，输出回调在返回前执行，按帧同步。
//! - 解码：`VTDecompressionSession`，同步解码，直接输出 BGRA（颜色转换在硬件里做）。
//!   SPS、PPS 变了（换分辨率）就重建会话；还没收到关键帧时不出画面、也不算出错。
//!
//! 直接声明 C 接口（CoreFoundation、CoreMedia、CoreVideo、VideoToolbox），只依赖标准库和
//! [`crate::annexb`]。只在 macOS 上编译，覆盖率统计不含这个文件，由 macOS 实机测试验证。

#![allow(unsafe_code, non_upper_case_globals, non_snake_case)]

use std::ffi::c_void;
use std::sync::Mutex;

use crate::annexb;

type CFTypeRef = *const c_void;
type OSStatus = i32;

#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

type CompressionCallback =
    Option<unsafe extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, CFTypeRef)>;
type DecompressionCallback = Option<
    unsafe extern "C" fn(*mut c_void, *mut c_void, OSStatus, u32, CFTypeRef, CMTime, CMTime),
>;

#[repr(C)]
struct DecompressionCallbackRecord {
    callback: DecompressionCallback,
    refcon: *mut c_void,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFTypeDictionaryKeyCallBacks: u8;
    static kCFTypeDictionaryValueCallBacks: u8;
    static kCFBooleanTrue: CFTypeRef;
    static kCFBooleanFalse: CFTypeRef;
    static kCFAllocatorNull: CFTypeRef;
    fn CFDictionaryCreate(
        allocator: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFTypeRef;
    fn CFNumberCreate(allocator: CFTypeRef, number_type: isize, value: *const c_void) -> CFTypeRef;
    fn CFRelease(cf: CFTypeRef);
    fn CFArrayGetCount(array: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFTypeRef, index: isize) -> CFTypeRef;
    fn CFDictionaryContainsKey(dict: CFTypeRef, key: CFTypeRef) -> u8;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    static kCMTimeInvalid: CMTime;
    static kCMSampleAttachmentKey_NotSync: CFTypeRef;
    fn CMTimeMake(value: i64, timescale: i32) -> CMTime;
    fn CMSampleBufferGetDataBuffer(sample: CFTypeRef) -> CFTypeRef;
    fn CMSampleBufferGetFormatDescription(sample: CFTypeRef) -> CFTypeRef;
    fn CMSampleBufferGetSampleAttachmentsArray(sample: CFTypeRef, create: u8) -> CFTypeRef;
    fn CMBlockBufferGetDataLength(buffer: CFTypeRef) -> usize;
    fn CMBlockBufferCopyDataBytes(
        buffer: CFTypeRef,
        offset: usize,
        length: usize,
        dest: *mut c_void,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc: CFTypeRef,
        index: usize,
        ptr: *mut *const u8,
        size: *mut usize,
        count: *mut usize,
        nal_header_len: *mut i32,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        allocator: CFTypeRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_len: i32,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMBlockBufferCreateWithMemoryBlock(
        allocator: CFTypeRef,
        memory: *mut c_void,
        block_length: usize,
        block_allocator: CFTypeRef,
        custom_source: *const c_void,
        offset: usize,
        data_length: usize,
        flags: u32,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn CMSampleBufferCreateReady(
        allocator: CFTypeRef,
        data: CFTypeRef,
        desc: CFTypeRef,
        samples: isize,
        timing_entries: isize,
        timing: *const c_void,
        size_entries: isize,
        sizes: *const usize,
        out: *mut CFTypeRef,
    ) -> OSStatus;
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    static kCVPixelBufferPixelFormatTypeKey: CFTypeRef;
    fn CVPixelBufferCreate(
        allocator: CFTypeRef,
        width: usize,
        height: usize,
        format: u32,
        attributes: CFTypeRef,
        out: *mut CFTypeRef,
    ) -> i32;
    fn CVPixelBufferLockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buffer: CFTypeRef, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddressOfPlane(buffer: CFTypeRef, plane: usize) -> *mut u8;
    fn CVPixelBufferGetBytesPerRowOfPlane(buffer: CFTypeRef, plane: usize) -> usize;
    fn CVPixelBufferGetBaseAddress(buffer: CFTypeRef) -> *mut u8;
    fn CVPixelBufferGetBytesPerRow(buffer: CFTypeRef) -> usize;
    fn CVPixelBufferGetWidth(buffer: CFTypeRef) -> usize;
    fn CVPixelBufferGetHeight(buffer: CFTypeRef) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    static kVTCompressionPropertyKey_RealTime: CFTypeRef;
    static kVTCompressionPropertyKey_ProfileLevel: CFTypeRef;
    static kVTCompressionPropertyKey_AllowFrameReordering: CFTypeRef;
    static kVTCompressionPropertyKey_AverageBitRate: CFTypeRef;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFTypeRef;
    static kVTCompressionPropertyKey_ExpectedFrameRate: CFTypeRef;
    static kVTProfileLevel_H264_Baseline_AutoLevel: CFTypeRef;
    static kVTEncodeFrameOptionKey_ForceKeyFrame: CFTypeRef;
    static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: CFTypeRef;
    static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: CFTypeRef;
    static kVTCompressionPropertyKey_MaxFrameDelayCount: CFTypeRef;
    fn VTCompressionSessionCreate(
        allocator: CFTypeRef,
        width: i32,
        height: i32,
        codec: u32,
        spec: CFTypeRef,
        source_attributes: CFTypeRef,
        data_allocator: CFTypeRef,
        callback: CompressionCallback,
        refcon: *mut c_void,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn VTSessionSetProperty(session: CFTypeRef, key: CFTypeRef, value: CFTypeRef) -> OSStatus;
    fn VTCompressionSessionPrepareToEncodeFrames(session: CFTypeRef) -> OSStatus;
    fn VTCompressionSessionEncodeFrame(
        session: CFTypeRef,
        image: CFTypeRef,
        pts: CMTime,
        duration: CMTime,
        properties: CFTypeRef,
        refcon: *mut c_void,
        info: *mut u32,
    ) -> OSStatus;
    fn VTCompressionSessionCompleteFrames(session: CFTypeRef, until: CMTime) -> OSStatus;
    fn VTCompressionSessionInvalidate(session: CFTypeRef);
    fn VTDecompressionSessionCreate(
        allocator: CFTypeRef,
        desc: CFTypeRef,
        spec: CFTypeRef,
        destination_attributes: CFTypeRef,
        callback: *const DecompressionCallbackRecord,
        out: *mut CFTypeRef,
    ) -> OSStatus;
    fn VTDecompressionSessionDecodeFrame(
        session: CFTypeRef,
        sample: CFTypeRef,
        flags: u32,
        refcon: *mut c_void,
        info: *mut u32,
    ) -> OSStatus;
    fn VTDecompressionSessionInvalidate(session: CFTypeRef);
}

/// `'avc1'`
const CODEC_H264: u32 = u32::from_be_bytes(*b"avc1");
/// `'420v'`：NV12，视频范围（和我们的 RGB→YUV 转换一致）。
const PIXEL_NV12: u32 = u32::from_be_bytes(*b"420v");
/// `'BGRA'`
const PIXEL_BGRA: u32 = u32::from_be_bytes(*b"BGRA");
/// `kCFNumberSInt32Type`
const CF_SINT32: isize = 3;
/// `kCFNumberSInt64Type`
const CF_SINT64: isize = 4;

/// 自己持有的 Core Foundation 对象：丢掉时 `CFRelease`。
struct Owned(CFTypeRef);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 由本结构独占的 +1 引用。
            unsafe { CFRelease(self.0) };
        }
    }
}

fn number_i32(v: i32) -> Owned {
    // SAFETY: 指向局部变量，类型与 kCFNumberSInt32Type 一致。
    Owned(unsafe { CFNumberCreate(std::ptr::null(), CF_SINT32, std::ptr::from_ref(&v).cast()) })
}

fn number_i64(v: i64) -> Owned {
    // SAFETY: 同上（64 位）。
    Owned(unsafe { CFNumberCreate(std::ptr::null(), CF_SINT64, std::ptr::from_ref(&v).cast()) })
}

/// 一个键值对的字典。
fn dict1(key: CFTypeRef, value: CFTypeRef) -> Owned {
    dict(&[(key, value)])
}

/// 几个键值对的字典。
fn dict(pairs: &[(CFTypeRef, CFTypeRef)]) -> Owned {
    let keys: Vec<CFTypeRef> = pairs.iter().map(|(k, _)| *k).collect();
    let values: Vec<CFTypeRef> = pairs.iter().map(|(_, v)| *v).collect();
    let count = isize::try_from(pairs.len()).unwrap_or(0);
    // SAFETY: 数组在调用期间有效；回调表是系统常量，字典会持有键和值。
    Owned(unsafe {
        CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            count,
            std::ptr::addr_of!(kCFTypeDictionaryKeyCallBacks).cast(),
            std::ptr::addr_of!(kCFTypeDictionaryValueCallBacks).cast(),
        )
    })
}

fn check(status: OSStatus, what: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        Err(format!("{what}失败（OSStatus {status}）"))
    }
}

// ───────────────────────────── 编码 ─────────────────────────────

/// 编码回调放结果的地方（编码器自己持有，地址固定）。
/// 一帧的编码结果：Annex B 码流和是否关键帧。
type Encoded = Result<(Vec<u8>, bool), String>;

/// 编码输出回调把结果放进这里，编码线程等它（不调用 CompleteFrames，见 [`VtEncoder::encode`]）。
#[derive(Default)]
struct EncodeSlot {
    /// 按输出顺序排队（编码器可能攒几帧才一起交出）。
    result: Mutex<std::collections::VecDeque<Encoded>>,
    ready: std::sync::Condvar,
}

/// 一个硬件编码会话。
pub struct VtEncoder {
    session: CFTypeRef,
    pixels: CFTypeRef,
    slot: Box<EncodeSlot>,
    width: u32,
    height: u32,
    fps: i32,
    frame: i64,
    bps: u64,
    opts: VtOptions,
}

impl std::fmt::Debug for VtEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtEncoder")
            .field("size", &(self.width, self.height))
            .finish_non_exhaustive()
    }
}

// SAFETY: VideoToolbox 会话和像素缓冲可以在线程之间移动；实际只在编码线程上使用。
unsafe impl Send for VtEncoder {}

/// 编码器的设置（产品用 [`VtOptions::DEFAULT`]，`hw-check` 对比其他几种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VtOptions {
    /// 最大关键帧间隔（帧数）；`None` 不设，由编码器决定。
    pub max_key_interval: Option<i32>,
    /// 每帧编完都强制完成（旧做法；怀疑会让下一帧变成关键帧）。
    pub complete_each_frame: bool,
    /// 要求 Baseline 档次（OpenH264 解得了）；否则由编码器选。
    pub baseline: bool,
    /// 最多攒几帧才交出（`None` 不设）。
    pub max_frame_delay: Option<i32>,
    /// 低延迟码控（编码器规格里打开）。
    pub low_latency: bool,
    /// 只用于体检：不等也不强制，量编码器攒几帧。
    pub pipelined: bool,
}

impl VtOptions {
    /// 产品现在用的设置。
    ///
    /// 低延迟码控和“每收一帧交一帧”只在 Intel Mac 上打开：用户的 Intel Mac 上 hw-check 实测
    /// 普通模式每帧都是关键帧、打开后 1/30 关键帧、10 ms/帧；而 Apple 芯片的 CI 上打开后
    /// Mac 控制 Mac 一帧都出不来（原因还没定位），普通模式一直正常。通用包按架构分别编译，自动区分。
    pub const DEFAULT: Self = Self {
        max_key_interval: Some(7200),
        complete_each_frame: false,
        baseline: true,
        max_frame_delay: if cfg!(target_arch = "x86_64") {
            Some(0)
        } else {
            None
        },
        low_latency: cfg!(target_arch = "x86_64"),
        pipelined: false,
    };

    /// `hw-check` 对比的几种设置：（名称, 设置）。
    pub const VARIANTS: [(&'static str, Self); 5] = [
        ("低延迟码控 + 攒 0 帧 + Baseline（默认）", Self::DEFAULT),
        (
            "不开低延迟码控、攒 0 帧",
            Self {
                low_latency: false,
                ..Self::DEFAULT
            },
        ),
        (
            "低延迟码控、不限 Baseline",
            Self {
                baseline: false,
                ..Self::DEFAULT
            },
        ),
        (
            "流水线：不等也不强制（量编码器攒几帧）",
            Self {
                max_frame_delay: None,
                low_latency: false,
                pipelined: true,
                ..Self::DEFAULT
            },
        ),
        (
            "旧做法：每帧强制交出",
            Self {
                max_frame_delay: None,
                low_latency: false,
                complete_each_frame: true,
                ..Self::DEFAULT
            },
        ),
    ];
}

/// 流水线方式：按 30 帧/秒交帧、不等结果，量编码器最多攒几帧、出的是不是关键帧。
fn pipeline_line(name: &str, enc: &mut VtEncoder, frames: &[Vec<u8>]) -> String {
    let (mut got, mut lag, mut keys, mut errors) = (0usize, 0usize, 0u32, 0u32);
    let count = |out: Vec<Encoded>, keys: &mut u32, errors: &mut u32| {
        for one in &out {
            match one {
                Ok((_, k)) => *keys += u32::from(*k),
                Err(_) => *errors += 1,
            }
        }
        out.len()
    };
    for (i, nv12) in frames.iter().enumerate() {
        if let Err(why) = enc.submit(nv12, 4_000_000, i == 0) {
            return format!("{name}：第 {i} 帧交不进去：{why}");
        }
        std::thread::sleep(std::time::Duration::from_millis(33));
        got += count(enc.take_ready(), &mut keys, &mut errors);
        lag = lag.max((i + 1).saturating_sub(got));
    }
    let before_flush = got;
    let flushed = enc.flush().map(|out| count(out, &mut keys, &mut errors));
    format!(
        "{name}：交了 {} 帧，强制之前出了 {before_flush} 帧，最多攒 {lag} 帧，强制交出 {} 帧，关键帧 {keys}，出错 {errors}",
        frames.len(),
        flushed.map_or_else(|e| format!("失败（{e}）"), |n| n.to_string()),
    )
}

/// 用几种设置分别编 `frames`（NV12，第一帧要求关键帧），每种一行结果：
/// 关键帧数、平均/最长编码耗时、平均每帧字节、软件解码器（`decodes`）能不能解。
pub fn probe(
    width: u32,
    height: u32,
    frames: &[Vec<u8>],
    decodes: &mut dyn FnMut(&[u8]) -> bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    for (name, opts) in VtOptions::VARIANTS {
        let mut enc = match VtEncoder::open_with(width, height, 30, 4_000_000, opts) {
            Ok(enc) => enc,
            Err(why) => {
                lines.push(format!("{name}：打不开：{why}"));
                continue;
            }
        };
        if opts.pipelined {
            lines.push(pipeline_line(name, &mut enc, frames));
            continue;
        }
        let (mut keys, mut bytes, mut total, mut worst, mut empty) =
            (0u32, 0usize, 0u128, 0u128, 0u32);
        let mut decoded_ok = true;
        let mut sizes = Vec::new();
        for (i, nv12) in frames.iter().enumerate() {
            let started = std::time::Instant::now();
            match enc.encode(nv12, 4_000_000, i == 0) {
                Ok((data, key)) => {
                    let took = started.elapsed().as_micros();
                    total += took;
                    worst = worst.max(took);
                    keys += u32::from(key);
                    bytes += data.len();
                    if data.is_empty() {
                        empty += 1;
                    } else {
                        decoded_ok &= decodes(&data);
                    }
                    if i < 12 {
                        sizes.push(format!(
                            "{}{}",
                            data.len() / 1024,
                            if key { "K" } else { "" }
                        ));
                    }
                }
                Err(why) => {
                    lines.push(format!("{name}：第 {i} 帧编码失败：{why}"));
                    break;
                }
            }
        }
        let n = u128::try_from(frames.len().max(1)).unwrap_or(1);
        lines.push(format!(
            "{name}：关键帧 {keys}/{}，编码平均 {} ms、最长 {} ms，平均 {} KB/帧，空帧 {empty}，软件解码 {}，前 12 帧 KB（K=关键帧）{}",
            frames.len(),
            total / n / 1000,
            worst / 1000,
            bytes / frames.len().max(1) / 1024,
            if decoded_ok { "正常" } else { "失败" },
            sizes.join(" "),
        ));
    }
    lines
}

/// 等编码输出回调最多这么久。
const OUTPUT_WAIT: std::time::Duration = std::time::Duration::from_millis(100);

/// 编码输出回调：取出码流（AVCC → Annex B），判断是否关键帧，放进 `refcon` 指向的槽。
unsafe extern "C" fn on_encoded(
    refcon: *mut c_void,
    _frame: *mut c_void,
    status: OSStatus,
    _flags: u32,
    sample: CFTypeRef,
) {
    // SAFETY: refcon 是编码器里 Box<EncodeSlot> 的地址，编码器活着时一直有效。
    let slot = unsafe { &*refcon.cast::<EncodeSlot>() };
    let result = if status != 0 {
        Err(format!("编码失败（OSStatus {status}）"))
    } else if sample.is_null() {
        // 码率控制丢了这一帧：没有画面
        Ok((Vec::new(), false))
    } else {
        // SAFETY: sample 在回调期间有效；只读取其中的数据。
        unsafe { read_sample(sample) }
    };
    if let Ok(mut s) = slot.result.lock() {
        s.push_back(result);
    }
    slot.ready.notify_all();
}

/// 从编码输出里读出 Annex B 码流和关键帧标志。
unsafe fn read_sample(sample: CFTypeRef) -> Result<(Vec<u8>, bool), String> {
    // SAFETY: 调用方保证 sample 有效；以下都是只读的查询。
    unsafe {
        let block = CMSampleBufferGetDataBuffer(sample);
        if block.is_null() {
            return Err("编码输出没有数据".into());
        }
        let len = CMBlockBufferGetDataLength(block);
        let mut avcc = vec![0u8; len];
        check(
            CMBlockBufferCopyDataBytes(block, 0, len, avcc.as_mut_ptr().cast()),
            "读取编码输出",
        )?;
        // 附件里没有 NotSync 就是关键帧
        let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, 0);
        let key = attachments.is_null()
            || CFArrayGetCount(attachments) == 0
            || CFDictionaryContainsKey(
                CFArrayGetValueAtIndex(attachments, 0),
                kCMSampleAttachmentKey_NotSync,
            ) == 0;
        let mut params: Vec<Vec<u8>> = Vec::new();
        if key {
            let desc = CMSampleBufferGetFormatDescription(sample);
            for index in 0..2 {
                let (mut ptr, mut size, mut count, mut nal) =
                    (std::ptr::null(), 0usize, 0usize, 0i32);
                let status = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                    desc, index, &mut ptr, &mut size, &mut count, &mut nal,
                );
                if status == 0 && !ptr.is_null() {
                    params.push(std::slice::from_raw_parts(ptr, size).to_vec());
                }
            }
        }
        let refs: Vec<&[u8]> = params.iter().map(Vec::as_slice).collect();
        let data = annexb::from_avcc(&avcc, &refs).ok_or_else(|| "编码输出格式不对".to_owned())?;
        // 是不是关键帧以码流为准（有 IDR 片），附件只用来决定要不要带上 SPS/PPS
        let idr = annexb::has_idr(&data);
        Ok((data, idr))
    }
}

impl VtEncoder {
    /// 按 `width`×`height`、`fps`、`bps` 打开硬件编码会话。没有硬件编码器时返回原因。
    pub fn open(width: u32, height: u32, fps: u32, bps: u64) -> Result<Self, String> {
        // 低延迟码控要 macOS 11.3+、并且编码器支持；打不开时退回普通模式
        // （Intel Mac 上普通模式每帧都是关键帧，由 H264Encoder 的兜底改用软件编码）
        Self::open_with(width, height, fps, bps, VtOptions::DEFAULT).or_else(|why| {
            log::info!("视频编码：低延迟码控打不开（{why}），用普通模式");
            Self::open_with(
                width,
                height,
                fps,
                bps,
                VtOptions {
                    low_latency: false,
                    ..VtOptions::DEFAULT
                },
            )
        })
    }

    /// 按指定的设置打开（`peerdesk hw-check` 用它对比几种设置）。
    pub fn open_with(
        width: u32,
        height: u32,
        fps: u32,
        bps: u64,
        opts: VtOptions,
    ) -> Result<Self, String> {
        let w = i32::try_from(width).map_err(|_| "画面太大".to_owned())?;
        let h = i32::try_from(height).map_err(|_| "画面太大".to_owned())?;
        let slot: Box<EncodeSlot> = Box::default();
        // SAFETY: 键是系统常量；kCFBooleanTrue 是系统常量。
        let spec = unsafe {
            let mut pairs = vec![(
                kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder,
                kCFBooleanTrue,
            )];
            // 低延迟码控（macOS 11.3+，系统不支持时打开会话会失败，由调用方回退）
            if opts.low_latency {
                pairs.push((
                    kVTVideoEncoderSpecification_EnableLowLatencyRateControl,
                    kCFBooleanTrue,
                ));
            }
            dict(&pairs)
        };
        let mut session: CFTypeRef = std::ptr::null();
        // SAFETY: 输出参数指向局部变量；refcon 指向 Box 里的槽，地址在编码器活着时不变。
        let status = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null(),
                w,
                h,
                CODEC_H264,
                spec.0,
                std::ptr::null(),
                std::ptr::null(),
                Some(on_encoded),
                std::ptr::from_ref::<EncodeSlot>(&slot).cast_mut().cast(),
                &mut session,
            )
        };
        check(
            status,
            "创建硬件编码会话（这台 Mac 可能没有 H.264 硬件编码器）",
        )?;
        let mut enc = Self {
            session,
            pixels: std::ptr::null(),
            slot,
            width,
            height,
            fps: i32::try_from(fps.max(1)).unwrap_or(30),
            frame: 0,
            bps: 0,
            opts,
        };
        // SAFETY: 会话有效；键和值都是系统常量或刚创建的数字。
        unsafe {
            let _ =
                VTSessionSetProperty(session, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
            if opts.baseline {
                check(
                    VTSessionSetProperty(
                        session,
                        kVTCompressionPropertyKey_ProfileLevel,
                        kVTProfileLevel_H264_Baseline_AutoLevel,
                    ),
                    "设置 Baseline 档次",
                )?;
            }
            // 不重排帧（不要 B 帧）：低延迟
            let _ = VTSessionSetProperty(
                session,
                kVTCompressionPropertyKey_AllowFrameReordering,
                kCFBooleanFalse,
            );
            // 最多攒几帧才交出：0 = 每收一帧就交出一帧（Intel Mac 默认要攒，等不到就只能强制完成，
            // 强制完成又让下一帧变成关键帧，hw-check 实测每帧 310 ms、全是关键帧）
            if let Some(frames) = opts.max_frame_delay {
                let delay = number_i32(frames);
                let _ = VTSessionSetProperty(
                    session,
                    kVTCompressionPropertyKey_MaxFrameDelayCount,
                    delay.0,
                );
            }
            if let Some(frames) = opts.max_key_interval {
                let interval = number_i32(frames);
                let _ = VTSessionSetProperty(
                    session,
                    kVTCompressionPropertyKey_MaxKeyFrameInterval,
                    interval.0,
                );
            }
            let rate = number_i32(enc.fps);
            let _ =
                VTSessionSetProperty(session, kVTCompressionPropertyKey_ExpectedFrameRate, rate.0);
            let _ = VTCompressionSessionPrepareToEncodeFrames(session);
        }
        enc.set_bitrate(bps);
        let mut pixels: CFTypeRef = std::ptr::null();
        // SAFETY: 输出参数指向局部变量。
        let status = unsafe {
            CVPixelBufferCreate(
                std::ptr::null(),
                usize::try_from(width).unwrap_or(0),
                usize::try_from(height).unwrap_or(0),
                PIXEL_NV12,
                std::ptr::null(),
                &mut pixels,
            )
        };
        if status != 0 {
            return Err(format!("创建像素缓冲失败（{status}）"));
        }
        enc.pixels = pixels;
        Ok(enc)
    }

    /// 尺寸。
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn set_bitrate(&mut self, bps: u64) {
        if self.bps != 0 && bps.abs_diff(self.bps).saturating_mul(10) <= self.bps {
            return;
        }
        let value = number_i64(i64::try_from(bps).unwrap_or(i64::MAX));
        // SAFETY: 会话有效，值是刚创建的数字。
        if unsafe {
            VTSessionSetProperty(
                self.session,
                kVTCompressionPropertyKey_AverageBitRate,
                value.0,
            )
        } == 0
        {
            self.bps = bps;
        }
    }

    /// 把 NV12 拷进像素缓冲（按缓冲区的行宽）。
    fn fill(&self, nv12: &[u8]) -> Result<(), String> {
        let (w, h) = (
            usize::try_from(self.width).unwrap_or(0),
            usize::try_from(self.height).unwrap_or(0),
        );
        if nv12.len() < w * h * 3 / 2 {
            return Err("画面数据不够".into());
        }
        // SAFETY: 加锁后基地址和行宽有效；每行拷 w 字节，不超过行宽和缓冲区；之后解锁。
        unsafe {
            check(CVPixelBufferLockBaseAddress(self.pixels, 0), "锁像素缓冲")?;
            for (plane, rows, offset) in [(0usize, h, 0usize), (1usize, h / 2, w * h)] {
                let base = CVPixelBufferGetBaseAddressOfPlane(self.pixels, plane);
                let stride = CVPixelBufferGetBytesPerRowOfPlane(self.pixels, plane);
                if base.is_null() || stride < w {
                    let _ = CVPixelBufferUnlockBaseAddress(self.pixels, 0);
                    return Err("像素缓冲格式不对".into());
                }
                for y in 0..rows {
                    let src = nv12
                        .get(offset + y * w..offset + (y + 1) * w)
                        .unwrap_or_default();
                    std::ptr::copy_nonoverlapping(src.as_ptr(), base.add(y * stride), src.len());
                }
            }
            let _ = CVPixelBufferUnlockBaseAddress(self.pixels, 0);
        }
        Ok(())
    }

    /// 交一帧 NV12 给编码器，不等结果（结果由输出回调放进队列）。
    pub fn submit(&mut self, nv12: &[u8], bps: u64, force_key: bool) -> Result<(), String> {
        self.set_bitrate(bps);
        self.fill(nv12)?;
        let props = force_key.then(|| {
            dict1(unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }, unsafe {
                kCFBooleanTrue
            })
        });
        // SAFETY: 会话、像素缓冲有效；属性字典在调用期间有效。
        unsafe {
            let pts = CMTimeMake(self.frame, self.fps);
            let duration = CMTimeMake(1, self.fps);
            let mut info = 0u32;
            check(
                VTCompressionSessionEncodeFrame(
                    self.session,
                    self.pixels,
                    pts,
                    duration,
                    props.as_ref().map_or(std::ptr::null(), |p| p.0),
                    std::ptr::null_mut(),
                    &mut info,
                ),
                "编码",
            )?;
        }
        self.frame = self.frame.saturating_add(1);
        Ok(())
    }

    /// 取走已经交出的结果（不等）。
    pub fn take_ready(&self) -> Vec<Encoded> {
        self.slot
            .result
            .lock()
            .map(|mut q| q.drain(..).collect())
            .unwrap_or_default()
    }

    /// 强制交出所有攒着的帧（之后的第一帧会是关键帧）。
    pub fn flush(&mut self) -> Result<Vec<Encoded>, String> {
        // SAFETY: 会话有效；返回前输出回调已执行。
        check(
            unsafe { VTCompressionSessionCompleteFrames(self.session, kCMTimeInvalid) },
            "完成编码",
        )?;
        Ok(self.take_ready())
    }

    /// 编一帧 NV12。返回 Annex B 码流和是否关键帧。
    ///
    /// 等输出回调，不每帧调用 CompleteFrames：每帧都“完成”会结束当前的参考帧序列，下一帧又是关键帧。
    /// 编码器设成每收一帧就交出一帧（`MaxFrameDelayCount` = 0）；等不到才强制完成
    /// （会多一个关键帧，不会卡住）。一次交出好几帧时按顺序拼在一起（解码器依次解）。
    pub fn encode(
        &mut self,
        nv12: &[u8],
        bps: u64,
        force_key: bool,
    ) -> Result<(Vec<u8>, bool), String> {
        self.submit(nv12, bps, force_key)?;
        let wait = if self.opts.complete_each_frame {
            std::time::Duration::ZERO
        } else {
            OUTPUT_WAIT
        };
        let got = self
            .slot
            .result
            .lock()
            .ok()
            .and_then(|guard| {
                self.slot
                    .ready
                    .wait_timeout_while(guard, wait, |q| q.is_empty())
                    .ok()
                    .map(|(mut q, _)| q.drain(..).collect::<Vec<_>>())
            })
            .unwrap_or_default();
        let got = if got.is_empty() { self.flush()? } else { got };
        let (mut data, mut key) = (Vec::new(), false);
        for one in got {
            let (bytes, k) = one?;
            data.extend_from_slice(&bytes);
            key |= k;
        }
        // 什么都没有：编码器丢了这一帧（码率控制），当作没有画面
        Ok((data, key))
    }
}

impl Drop for VtEncoder {
    fn drop(&mut self) {
        // SAFETY: 会话和像素缓冲由本结构持有，只在这里释放一次。
        unsafe {
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session);
            if !self.pixels.is_null() {
                CFRelease(self.pixels);
            }
        }
    }
}

// ───────────────────────────── 解码 ─────────────────────────────

/// 解出的画面：宽、高、`0x00RRGGBB` 像素。
pub type Picture = (usize, usize, Vec<u32>);
type DecodeSlot = Mutex<Option<Result<Picture, String>>>;

/// 一个硬件解码会话。
pub struct VtDecoder {
    session: CFTypeRef,
    desc: CFTypeRef,
    params: (Vec<u8>, Vec<u8>),
    slot: Box<DecodeSlot>,
}

impl std::fmt::Debug for VtDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtDecoder")
            .field("open", &!self.session.is_null())
            .finish_non_exhaustive()
    }
}

// SAFETY: 同编码器，只在解码线程上使用。
unsafe impl Send for VtDecoder {}

/// 解码输出回调：BGRA 像素缓冲 → `0x00RRGGBB`。
unsafe extern "C" fn on_decoded(
    refcon: *mut c_void,
    _frame: *mut c_void,
    status: OSStatus,
    _flags: u32,
    image: CFTypeRef,
    _pts: CMTime,
    _dur: CMTime,
) {
    // SAFETY: refcon 是解码器里 Box<DecodeSlot> 的地址。
    let slot = unsafe { &*refcon.cast::<DecodeSlot>() };
    let result = if status != 0 || image.is_null() {
        Err(format!("解码失败（OSStatus {status}）"))
    } else {
        // SAFETY: image 在回调期间有效；加锁读、读完解锁；每行只读 4×宽 个字节，不超过行宽。
        unsafe {
            if CVPixelBufferLockBaseAddress(image, 1) == 0 {
                let (w, h) = (CVPixelBufferGetWidth(image), CVPixelBufferGetHeight(image));
                let (base, stride) = (
                    CVPixelBufferGetBaseAddress(image),
                    CVPixelBufferGetBytesPerRow(image),
                );
                let mut pixels = Vec::with_capacity(w * h);
                if !base.is_null() && stride >= w * 4 {
                    for y in 0..h {
                        let row = std::slice::from_raw_parts(base.add(y * stride), w * 4);
                        pixels.extend(row.chunks_exact(4).map(|p| match p {
                            [b, g, r, _] => {
                                (u32::from(*r) << 16) | (u32::from(*g) << 8) | u32::from(*b)
                            }
                            _ => 0,
                        }));
                    }
                }
                let _ = CVPixelBufferUnlockBaseAddress(image, 1);
                if pixels.len() == w * h {
                    Ok((w, h, pixels))
                } else {
                    Err("解出的画面格式不对".into())
                }
            } else {
                Err("锁不住解出的画面".into())
            }
        }
    };
    if let Ok(mut s) = slot.lock() {
        *s = Some(result);
    }
}

impl VtDecoder {
    /// 新解码器；收到第一个关键帧时才建会话。
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: std::ptr::null(),
            desc: std::ptr::null(),
            params: (Vec::new(), Vec::new()),
            slot: Box::new(Mutex::new(None)),
        }
    }

    fn close(&mut self) {
        // SAFETY: 由本结构持有；释放后置空，不会重复释放。
        unsafe {
            if !self.session.is_null() {
                VTDecompressionSessionInvalidate(self.session);
                CFRelease(self.session);
            }
            if !self.desc.is_null() {
                CFRelease(self.desc);
            }
        }
        self.session = std::ptr::null();
        self.desc = std::ptr::null();
    }

    fn open(&mut self, sps: &[u8], pps: &[u8]) -> Result<(), String> {
        self.close();
        let pointers = [sps.as_ptr(), pps.as_ptr()];
        let sizes = [sps.len(), pps.len()];
        let mut desc: CFTypeRef = std::ptr::null();
        // SAFETY: 两个参数集在调用期间有效；输出参数指向成员。
        check(
            unsafe {
                CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    std::ptr::null(),
                    2,
                    pointers.as_ptr(),
                    sizes.as_ptr(),
                    4,
                    &mut desc,
                )
            },
            "建格式描述",
        )?;
        self.desc = desc;
        let format = number_i32(i32::from_be_bytes(PIXEL_BGRA.to_be_bytes()));
        // SAFETY: 键是系统常量。
        let attrs = dict1(unsafe { kCVPixelBufferPixelFormatTypeKey }, format.0);
        let record = DecompressionCallbackRecord {
            callback: Some(on_decoded),
            refcon: std::ptr::from_ref::<DecodeSlot>(&self.slot)
                .cast_mut()
                .cast(),
        };
        let mut session: CFTypeRef = std::ptr::null();
        // SAFETY: 格式描述、属性字典、回调记录在调用期间有效；refcon 指向 Box 里的槽。
        check(
            unsafe {
                VTDecompressionSessionCreate(
                    std::ptr::null(),
                    self.desc,
                    std::ptr::null(),
                    attrs.0,
                    &record,
                    &mut session,
                )
            },
            "创建硬件解码会话",
        )?;
        self.session = session;
        self.params = (sps.to_vec(), pps.to_vec());
        Ok(())
    }

    /// 解一帧 Annex B 码流。还没收到关键帧时返回 `Ok(None)`。
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Picture>, String> {
        let (avcc, sps, pps) = annexb::to_avcc(data);
        if let (Some(sps), Some(pps)) = (sps, pps) {
            if self.session.is_null()
                || (sps.as_slice(), pps.as_slice())
                    != (self.params.0.as_slice(), self.params.1.as_slice())
            {
                self.open(&sps, &pps)?;
            }
        }
        if self.session.is_null() || avcc.is_empty() {
            return Ok(None);
        }
        let mut block: CFTypeRef = std::ptr::null();
        // SAFETY: 块缓冲直接引用 avcc（kCFAllocatorNull：不复制、不释放），同步解码期间 avcc 一直有效。
        check(
            unsafe {
                CMBlockBufferCreateWithMemoryBlock(
                    std::ptr::null(),
                    avcc.as_ptr().cast_mut().cast(),
                    avcc.len(),
                    kCFAllocatorNull,
                    std::ptr::null(),
                    0,
                    avcc.len(),
                    0,
                    &mut block,
                )
            },
            "建数据块",
        )?;
        let block = Owned(block);
        let mut sample: CFTypeRef = std::ptr::null();
        let size = avcc.len();
        // SAFETY: 数据块、格式描述有效；大小数组在调用期间有效。
        check(
            unsafe {
                CMSampleBufferCreateReady(
                    std::ptr::null(),
                    block.0,
                    self.desc,
                    1,
                    0,
                    std::ptr::null(),
                    1,
                    &size,
                    &mut sample,
                )
            },
            "建样本",
        )?;
        let sample = Owned(sample);
        if let Ok(mut s) = self.slot.lock() {
            *s = None;
        }
        let mut info = 0u32;
        // SAFETY: 会话、样本有效；flags = 0 表示同步解码，返回前输出回调已执行。
        check(
            unsafe {
                VTDecompressionSessionDecodeFrame(
                    self.session,
                    sample.0,
                    0,
                    std::ptr::null_mut(),
                    &mut info,
                )
            },
            "解码",
        )?;
        drop(sample);
        match self.slot.lock().ok().and_then(|mut s| s.take()) {
            Some(Ok(picture)) => Ok(Some(picture)),
            Some(Err(e)) => Err(e),
            None => Ok(None),
        }
    }
}

impl Default for VtDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for VtDecoder {
    fn drop(&mut self) {
        self.close();
    }
}
