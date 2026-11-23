//! Windows 硬件 H.264 编码（系统胶水）：Media Foundation 的硬件编码器 MFT。
//!
//! 显卡厂商的编码器（NVIDIA NVENC、Intel Quick Sync、AMD AMF）都以“硬件 MFT”的形式注册在系统里，
//! 这里只枚举硬件的那一类（`MFT_ENUM_FLAG_HARDWARE`），没有就不用，交回 OpenH264 软件编码。
//!
//! 要点：
//! - 输出 **Baseline** 档次：接收端用 OpenH264 解码，它不支持 High 档次的 CABAC 等工具；
//! - 低延迟模式、不用 B 帧、关键帧只在需要时（丢包、换尺寸）由我们强制；
//! - 码率每帧按传输层给的预算调（`CODECAPI_AVEncCommonMeanBitRate`）；
//! - 硬件 MFT 是异步的：靠事件（要输入 / 有输出）推进。这里一帧一帧同步地等，每次等待都有时限，
//!   超时算失败，由调用方改用软件编码（不卡住会话）。
//! - 所有调用都在编码线程上（第一次编码时才打开），COM 按多线程套间初始化。
//!
//! 只在 Windows 上编译，覆盖率统计不含这个文件（Linux 上不编译），由 Windows 实机测试验证。

#![allow(unsafe_code)]

use std::mem::ManuallyDrop;
use std::sync::Once;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_Base, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
    ICodecAPI, IMFActivate, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform,
    METransformHaveOutput, METransformNeedInput, MFCreateMediaType, MFCreateMemoryBuffer,
    MFCreateSample, MFMediaType_Video, MFSampleExtension_CleanPoint, MFStartup, MFTEnumEx,
    MFVideoFormat_H264, MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFSTARTUP_NOSOCKET,
    MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
};
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARIANT;

/// 等一个事件（要输入 / 有输出）最多这么久。第一帧硬件要初始化，给长一点。
const FIRST_WAIT: Duration = Duration::from_secs(3);
const WAIT: Duration = Duration::from_millis(500);

fn err(what: &str, e: &windows::core::Error) -> String {
    format!("{what}：{e}")
}

/// 进程内只初始化一次 Media Foundation。
fn startup() -> Result<(), String> {
    static ONCE: Once = Once::new();
    static mut RESULT: Option<String> = None;
    ONCE.call_once(|| {
        // SAFETY: 参数都是常量；只在第一次调用时执行。
        if let Err(e) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) } {
            // SAFETY: 只在 call_once 里写一次，之后只读。
            unsafe { RESULT = Some(err("MFStartup", &e)) };
        }
    });
    // SAFETY: call_once 之后不再写。
    match unsafe { (*std::ptr::addr_of!(RESULT)).clone() } {
        Some(why) => Err(why),
        None => Ok(()),
    }
}

/// 一个打开的硬件编码器。
pub struct MfEncoder {
    activate: IMFActivate,
    transform: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: Option<ICodecAPI>,
    name: String,
    fps: u32,
    width: u32,
    height: u32,
    frame: i64,
    /// 编码器要了几次输入还没给。
    need_input: u32,
    bps: u64,
    provides_samples: bool,
    output_size: u32,
}

impl std::fmt::Debug for MfEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfEncoder")
            .field("name", &self.name)
            .field("size", &(self.width, self.height))
            .finish_non_exhaustive()
    }
}

// SAFETY: COM 对象按多线程套间创建，可以在线程之间移动；实际只在编码线程上使用。
unsafe impl Send for MfEncoder {}

fn media_type(
    subtype: &windows::core::GUID,
    width: u32,
    height: u32,
    fps: u32,
) -> Result<IMFMediaType, String> {
    // SAFETY: 创建和设置媒体类型的属性，参数都是本地值。
    unsafe {
        let t = MFCreateMediaType().map_err(|e| err("MFCreateMediaType", &e))?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| err("类型", &e))?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)
            .map_err(|e| err("子类型", &e))?;
        t.SetUINT64(
            &MF_MT_FRAME_SIZE,
            (u64::from(width) << 32) | u64::from(height),
        )
        .map_err(|e| err("尺寸", &e))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, (u64::from(fps) << 32) | 1)
            .map_err(|e| err("帧率", &e))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)
            .map_err(|e| err("像素比例", &e))?;
        t.SetUINT32(
            &MF_MT_INTERLACE_MODE,
            u32::try_from(MFVideoInterlace_Progressive.0).unwrap_or(2),
        )
        .map_err(|e| err("扫描方式", &e))?;
        Ok(t)
    }
}

impl MfEncoder {
    /// 找一个硬件 H.264 编码器并按 `width`×`height`、`fps`、`bps` 打开。没有硬件编码器时返回原因。
    pub fn open(width: u32, height: u32, fps: u32, bps: u64) -> Result<Self, String> {
        // SAFETY: 按多线程套间初始化 COM；已经初始化过（同一种或另一种）时返回的错误可以忽略。
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        startup()?;
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        // SAFETY: 输出参数指向本地变量；返回的数组用 CoTaskMemFree 释放，元素逐个取出释放。
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut list,
                &mut count,
            )
        }
        .map_err(|e| err("枚举硬件编码器", &e))?;
        let mut found: Vec<IMFActivate> = Vec::new();
        for i in 0..usize::try_from(count).unwrap_or(0) {
            // SAFETY: i < count，数组由 MFTEnumEx 分配；read 取走所有权，之后由 found 或这里释放。
            if let Some(a) = unsafe { std::ptr::read(list.add(i)) } {
                found.push(a);
            }
        }
        if !list.is_null() {
            // SAFETY: 数组本身由 MFTEnumEx 用 CoTaskMemAlloc 分配，元素已经取走。
            unsafe { CoTaskMemFree(Some(list.cast())) };
        }
        let mut reasons = Vec::new();
        for activate in found {
            match Self::open_one(activate, width, height, fps, bps) {
                Ok(enc) => return Ok(enc),
                Err(why) => reasons.push(why),
            }
        }
        if reasons.is_empty() {
            Err("系统里没有硬件 H.264 编码器（显卡驱动不支持或没装）".into())
        } else {
            Err(format!("硬件编码器都打不开：{}", reasons.join("；")))
        }
    }

    fn open_one(
        activate: IMFActivate,
        width: u32,
        height: u32,
        fps: u32,
        bps: u64,
    ) -> Result<Self, String> {
        // SAFETY: 以下都是对刚创建的 COM 对象的方法调用，参数是本地值。
        unsafe {
            let name = {
                let mut buf = [0u16; 256];
                let mut len = 0u32;
                match activate.GetString(
                    &windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute,
                    &mut buf,
                    Some(&mut len),
                ) {
                    Ok(()) => String::from_utf16_lossy(
                        buf.get(..usize::try_from(len).unwrap_or(0))
                            .unwrap_or_default(),
                    ),
                    Err(_) => "硬件编码器".to_owned(),
                }
            };
            let transform: IMFTransform = activate.ActivateObject().map_err(|e| err(&name, &e))?;
            if let Ok(attrs) = transform.GetAttributes() {
                // 硬件 MFT 是异步的，要先“解锁”才能用；低延迟。
                let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }
            let events: IMFMediaEventGenerator =
                transform.cast().map_err(|e| err("事件接口", &e))?;
            let codec: Option<ICodecAPI> = transform.cast().ok();
            if let Some(c) = &codec {
                let _ = c.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                let _ = c.SetValue(
                    &CODECAPI_AVEncCommonRateControlMode,
                    &VARIANT::from(u32::try_from(eAVEncCommonRateControlMode_CBR.0).unwrap_or(0)),
                );
                let _ = c.SetValue(&CODECAPI_AVEncMPVDefaultBPictureCount, &VARIANT::from(0u32));
                // 不按固定间隔出关键帧：需要时（丢包、换尺寸）我们强制
                let _ = c.SetValue(&CODECAPI_AVEncMPVGOPSize, &VARIANT::from(u32::MAX));
            }
            let out = media_type(&MFVideoFormat_H264, width, height, fps)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, u32::try_from(bps).unwrap_or(u32::MAX))
                .map_err(|e| err("码率", &e))?;
            out.SetUINT32(
                &MF_MT_MPEG2_PROFILE,
                u32::try_from(eAVEncH264VProfile_Base.0).unwrap_or(66),
            )
            .map_err(|e| err("档次", &e))?;
            transform
                .SetOutputType(0, &out, 0)
                .map_err(|e| err("设置输出格式（H.264 Baseline）", &e))?;
            let input = media_type(&MFVideoFormat_NV12, width, height, fps)?;
            transform
                .SetInputType(0, &input, 0)
                .map_err(|e| err("设置输入格式（NV12）", &e))?;
            let info = transform
                .GetOutputStreamInfo(0)
                .map_err(|e| err("输出信息", &e))?;
            let provides = info.dwFlags
                & u32::try_from(
                    MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0,
                )
                .unwrap_or(0)
                != 0;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| err("开始", &e))?;
            transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| err("开始", &e))?;
            Ok(Self {
                activate,
                transform,
                events,
                codec,
                name,
                fps: fps.max(1),
                width,
                height,
                frame: 0,
                need_input: 0,
                bps,
                provides_samples: provides,
                output_size: info.cbSize.max(width.saturating_mul(height)),
            })
        }
    }

    /// 编码器名称（日志用）。
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 尺寸。
    #[must_use]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// 取一个事件；没有时等，最多 `limit`。返回事件类型。
    fn next_event(&self, limit: Duration) -> Result<i32, String> {
        let started = Instant::now();
        loop {
            // SAFETY: 不等待地取事件；没有事件时返回 MF_E_NO_EVENTS_AVAILABLE。
            match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(event) => {
                    // SAFETY: 事件对象有效。
                    let kind = unsafe { event.GetType() }.map_err(|e| err("事件类型", &e))?;
                    return Ok(i32::try_from(kind).unwrap_or(-1));
                }
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => {
                    if started.elapsed() > limit {
                        return Err(format!(
                            "{} 没有响应（等了 {} ms）",
                            self.name,
                            limit.as_millis()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(e) => return Err(err("取事件", &e)),
            }
        }
    }

    fn set_bitrate(&mut self, bps: u64) {
        if bps.abs_diff(self.bps).saturating_mul(10) <= self.bps {
            return;
        }
        if let Some(c) = &self.codec {
            // SAFETY: 接口有效，VARIANT 是本地值。
            if unsafe {
                c.SetValue(
                    &CODECAPI_AVEncCommonMeanBitRate,
                    &VARIANT::from(u32::try_from(bps).unwrap_or(u32::MAX)),
                )
            }
            .is_ok()
            {
                self.bps = bps;
            }
        }
    }

    fn input_sample(&mut self, nv12: &[u8]) -> Result<IMFSample, String> {
        let len = u32::try_from(nv12.len()).map_err(|_| "画面太大".to_owned())?;
        // SAFETY: 缓冲区按长度创建；Lock 返回的指针在 Unlock 之前有效，拷贝长度不超过缓冲区。
        unsafe {
            let buffer = MFCreateMemoryBuffer(len).map_err(|e| err("建缓冲区", &e))?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            buffer
                .Lock(&mut ptr, None, None)
                .map_err(|e| err("锁缓冲区", &e))?;
            std::ptr::copy_nonoverlapping(nv12.as_ptr(), ptr, nv12.len());
            buffer.Unlock().map_err(|e| err("解锁缓冲区", &e))?;
            buffer
                .SetCurrentLength(len)
                .map_err(|e| err("缓冲区长度", &e))?;
            let sample = MFCreateSample().map_err(|e| err("建样本", &e))?;
            sample.AddBuffer(&buffer).map_err(|e| err("样本", &e))?;
            let duration = 10_000_000 / i64::from(self.fps);
            sample
                .SetSampleTime(self.frame.saturating_mul(duration))
                .map_err(|e| err("时间戳", &e))?;
            sample
                .SetSampleDuration(duration)
                .map_err(|e| err("时长", &e))?;
            self.frame = self.frame.saturating_add(1);
            Ok(sample)
        }
    }

    /// 取一帧输出：H.264 码流（Annex B）和是否关键帧。
    fn output(&mut self) -> Result<(Vec<u8>, bool), String> {
        for _ in 0..3 {
            let sample = if self.provides_samples {
                None
            } else {
                // SAFETY: 按编码器要求的大小建输出缓冲区。
                unsafe {
                    let buffer = MFCreateMemoryBuffer(self.output_size)
                        .map_err(|e| err("输出缓冲区", &e))?;
                    let s = MFCreateSample().map_err(|e| err("输出样本", &e))?;
                    s.AddBuffer(&buffer).map_err(|e| err("输出样本", &e))?;
                    Some(s)
                }
            };
            let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            // SAFETY: 输出数组只有一项，按约定填好；调用后取走其中的样本和事件集合释放。
            let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };
            let (sample, events) = match buffers.first_mut() {
                // SAFETY: 每个 ManuallyDrop 只取一次。
                Some(b) => unsafe {
                    (
                        ManuallyDrop::take(&mut b.pSample),
                        ManuallyDrop::take(&mut b.pEvents),
                    )
                },
                None => (None, None),
            };
            drop(events);
            match result {
                Ok(()) => {
                    let sample = sample.ok_or_else(|| "编码器没有给出样本".to_owned())?;
                    // SAFETY: 样本有效；Lock 的指针和长度在 Unlock 之前有效。
                    unsafe {
                        let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
                        let buffer = sample
                            .ConvertToContiguousBuffer()
                            .map_err(|e| err("输出缓冲区", &e))?;
                        let mut ptr: *mut u8 = std::ptr::null_mut();
                        let mut len = 0u32;
                        buffer
                            .Lock(&mut ptr, None, Some(&mut len))
                            .map_err(|e| err("锁输出", &e))?;
                        let data =
                            std::slice::from_raw_parts(ptr, usize::try_from(len).unwrap_or(0))
                                .to_vec();
                        buffer.Unlock().map_err(|e| err("解锁输出", &e))?;
                        return Ok((data, key));
                    }
                }
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    // 编码器要重新协商输出格式（有些硬件编码器在第一帧时这样做）：用它给的第一种。
                    // SAFETY: 接口有效。
                    unsafe {
                        let t = self
                            .transform
                            .GetOutputAvailableType(0, 0)
                            .map_err(|e| err("可用输出格式", &e))?;
                        self.transform
                            .SetOutputType(0, &t, 0)
                            .map_err(|e| err("重设输出格式", &e))?;
                    }
                }
                Err(e) => return Err(err("取编码结果", &e)),
            }
        }
        Err("编码器反复要求重设输出格式".into())
    }

    /// 编一帧 NV12。`force_key` 时编关键帧。
    pub fn encode(
        &mut self,
        nv12: &[u8],
        bps: u64,
        force_key: bool,
    ) -> Result<(Vec<u8>, bool), String> {
        self.set_bitrate(bps);
        if force_key {
            if let Some(c) = &self.codec {
                // SAFETY: 接口有效，VARIANT 是本地值。
                let _ =
                    unsafe { c.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32)) };
            }
        }
        let limit = if self.frame == 0 { FIRST_WAIT } else { WAIT };
        // 等编码器要输入
        while self.need_input == 0 {
            let kind = self.next_event(limit)?;
            if kind == METransformNeedInput.0 {
                self.need_input = self.need_input.saturating_add(1);
            } else if kind == METransformHaveOutput.0 {
                // 上一帧多出来的输出（不该有）：丢掉
                let _ = self.output()?;
            }
        }
        let sample = self.input_sample(nv12)?;
        // SAFETY: 样本有效。
        unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(|e| err("送入画面", &e))?;
        self.need_input = self.need_input.saturating_sub(1);
        // 等这一帧的输出
        loop {
            let kind = self.next_event(limit)?;
            if kind == METransformHaveOutput.0 {
                return self.output();
            }
            if kind == METransformNeedInput.0 {
                self.need_input = self.need_input.saturating_add(1);
            }
        }
    }
}

impl Drop for MfEncoder {
    fn drop(&mut self) {
        // SAFETY: 对象有效；结束流、关掉激活对象（释放显卡上的编码会话）。
        unsafe {
            let _ = self
                .transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.activate.ShutdownObject();
        }
    }
}
