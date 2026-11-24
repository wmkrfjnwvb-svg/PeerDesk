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
//! - 编码器声明支持 D3D11 时，给它一个建在**它所在显卡**上的 D3D11 设备（按 `MFT_ENUM_ADAPTER_LUID`
//!   找显卡；双显卡笔记本上默认显卡不一定是编码器那块）。AMD 的编码器没有设备打不开；
//!   NVIDIA、Intel 有设备时走显存更省。给了设备打不开时再不给设备试一次。
//!
//! 只在 Windows 上编译，覆盖率统计不含这个文件（Linux 上不编译），由 Windows 实机测试验证。

#![allow(unsafe_code)]

use std::mem::ManuallyDrop;
use std::sync::Once;
use std::time::{Duration, Instant};

use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
use windows::Win32::Media::MediaFoundation::{
    eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_Base, eAVScenarioInfo_DisplayRemoting,
    CODECAPI_AVDecVideoAcceleration_H264, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVDefaultBPictureCount,
    CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode,
    CODECAPI_AVScenarioInfo, ICodecAPI, IMFActivate, IMFMediaEventGenerator, IMFMediaType,
    IMFSample, IMFTransform, METransformDrainComplete, METransformHaveOutput, METransformNeedInput,
    MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
    MFSampleExtension_CleanPoint, MFStartup, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_NV12,
    MFVideoInterlace_Progressive, MFSTARTUP_NOSOCKET, MFT_CATEGORY_VIDEO_ENCODER,
    MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_OUTPUT_DATA_BUFFER,
    MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES,
    MFT_REGISTER_TYPE_INFO, MF_EVENT_FLAG_NO_WAIT, MF_E_NOTACCEPTING, MF_E_NO_EVENTS_AVAILABLE,
    MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MF_TRANSFORM_ASYNC_UNLOCK, MF_VERSION,
};
use windows::Win32::Media::MediaFoundation::{
    CLSID_MSH264DecoderMFT, IMF2DBuffer, IMFDXGIDeviceManager, MFCreateDXGIDeviceManager,
    MFT_ENUM_ADAPTER_LUID, MFT_MESSAGE_SET_D3D_MANAGER, MF_E_TRANSFORM_NEED_MORE_INPUT,
    MF_MT_DEFAULT_STRIDE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_SA_D3D11_AWARE,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::System::Variant::VARIANT;

/// 第一帧等编码器要输入最多这么久（硬件要初始化）。
const FIRST_WAIT: Duration = Duration::from_secs(3);
/// 编码器这么久没有任何进展（不要输入、不交帧）才算卡住。
const STALL: Duration = Duration::from_secs(2);
/// 交帧之后等编好的帧最多这么久（等不到下次再取，不算出错）。
const OUTPUT_WAIT: Duration = Duration::from_millis(20);
/// 排空最多等这么久。
const DRAIN_WAIT: Duration = Duration::from_millis(500);

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

/// 编码器所在显卡（按 LUID 找；找不到时用默认显卡）上的 D3D11 设备和 DXGI 设备管理器。
fn d3d_manager(activate: &IMFActivate) -> Result<(ID3D11Device, IMFDXGIDeviceManager), String> {
    // SAFETY: 读属性。
    let adapter = unsafe { activate.GetUINT64(&MFT_ENUM_ADAPTER_LUID) }
        .ok()
        .and_then(adapter_by_luid);
    d3d_device(adapter)
}

/// 在显卡 `adapter`（`None` 为默认显卡）上建 D3D11 设备和 DXGI 设备管理器。
fn d3d_device(
    adapter: Option<IDXGIAdapter>,
) -> Result<(ID3D11Device, IMFDXGIDeviceManager), String> {
    // SAFETY: 建设备、建管理器，输出参数都指向局部变量。
    unsafe {
        let driver = if adapter.is_some() {
            D3D_DRIVER_TYPE_UNKNOWN
        } else {
            D3D_DRIVER_TYPE_HARDWARE
        };
        let mut device: Option<ID3D11Device> = None;
        D3D11CreateDevice(
            adapter.as_ref(),
            driver,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_VIDEO_SUPPORT | D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
        .map_err(|e| err("建 D3D11 设备", &e))?;
        let device = device.ok_or("D3D11CreateDevice 没有返回设备")?;
        // 编码器在自己的线程上用这个设备：打开多线程保护
        if let Ok(mt) = device.cast::<ID3D10Multithread>() {
            let _ = mt.SetMultithreadProtected(true);
        }
        let mut token = 0u32;
        let mut manager: Option<IMFDXGIDeviceManager> = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)
            .map_err(|e| err("建 DXGI 设备管理器", &e))?;
        let manager = manager.ok_or("MFCreateDXGIDeviceManager 没有返回管理器")?;
        manager
            .ResetDevice(&device, token)
            .map_err(|e| err("设备交给管理器", &e))?;
        Ok((device, manager))
    }
}

/// LUID（低 32 位 `LowPart`，高 32 位 `HighPart`）对应的显卡。
fn adapter_by_luid(luid: u64) -> Option<IDXGIAdapter> {
    // SAFETY: 枚举显卡，输出都是返回值。
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut i = 0u32;
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            if let Ok(desc) = adapter.GetDesc1() {
                let high = u64::from(u32::from_ne_bytes(desc.AdapterLuid.HighPart.to_ne_bytes()));
                if (high << 32) | u64::from(desc.AdapterLuid.LowPart) == luid {
                    return adapter.cast().ok();
                }
            }
            i += 1;
        }
        None
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
    /// 编码器交出的帧（按顺序；硬件编码器可能攒几帧才交出，交出时可能一次好几帧）。
    ready: std::collections::VecDeque<(Vec<u8>, bool)>,
    /// 最近一次有进展（编码器要输入或交出帧）的时刻：长时间没有进展才算编码器卡住。
    progress_at: Instant,
    bps: u64,
    provides_samples: bool,
    output_size: u32,
    /// 给编码器的 D3D11 设备和管理器（没给时为 `None`）。编码器用着，要活得比它长。
    _d3d: Option<(ID3D11Device, IMFDXGIDeviceManager)>,
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
            // 先给 D3D11 设备试，不行再不给设备试（同一个激活对象要先关掉才能重新激活）
            for with_d3d in [true, false] {
                match Self::open_one(&activate, width, height, fps, bps, with_d3d)
                    .and_then(Self::self_test)
                {
                    Ok(enc) => return Ok(enc),
                    Err(why) => reasons.push(why),
                }
                // SAFETY: 释放这次激活出来的对象，下一次重新激活。
                let _ = unsafe { activate.ShutdownObject() };
            }
        }
        if reasons.is_empty() {
            Err("系统里没有硬件 H.264 编码器（显卡驱动不支持或没装）".into())
        } else {
            Err(format!("硬件编码器都打不开：{}", reasons.join("；")))
        }
    }

    /// 编一帧黑画面试试：有的编码器打开时不报错，第一帧才失败（例如给了设备却不收内存里的画面）。
    /// 试的这一帧不发出去；之后第一帧照样是关键帧（尺寸变了要重开时调用方会强制关键帧）。
    fn self_test(mut self) -> Result<Self, String> {
        let (w, h) = (
            usize::try_from(self.width).unwrap_or(0),
            usize::try_from(self.height).unwrap_or(0),
        );
        let mut black = vec![16u8; w * h];
        black.resize(w * h + w * h / 2, 128);
        let bps = self.bps;
        let name = self.name.clone();
        let got = self.encode(&black, bps, true).and_then(|got| match got {
            Some(_) => Ok(()),
            // 攒着：让它交出来，确认真的编得出（刚打开时这样做不影响之后）
            None => self
                .drain()
                .and_then(|()| self.poll()?.map(drop).ok_or_else(|| "交不出帧".to_owned())),
        });
        got.map_err(|e| format!("{name}：试编一帧失败：{e}"))?;
        self.ready.clear();
        Ok(self)
    }

    fn open_one(
        activate: &IMFActivate,
        width: u32,
        height: u32,
        fps: u32,
        bps: u64,
        with_d3d: bool,
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
            let mut d3d_aware = false;
            if let Ok(attrs) = transform.GetAttributes() {
                // 硬件 MFT 是异步的，要先“解锁”才能用；低延迟。
                let _ = attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1);
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
                d3d_aware = attrs.GetUINT32(&MF_SA_D3D11_AWARE).is_ok_and(|v| v != 0);
            }
            let d3d = if with_d3d && d3d_aware {
                let made = d3d_manager(activate)?;
                transform
                    .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, made.1.as_raw() as usize)
                    .map_err(|e| err(&format!("{name}：交给 D3D11 设备"), &e))?;
                Some(made)
            } else if with_d3d {
                return Err(format!("{name}：不支持 D3D11，直接用内存"));
            } else {
                None
            };
            let name = if d3d.is_some() {
                format!("{name}（D3D11）")
            } else {
                name
            };
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
                // 场景：远程桌面（Sunshine 用 FFmpeg 的 MF 编码器时也这么设）
                let _ = c.SetValue(
                    &CODECAPI_AVScenarioInfo,
                    &VARIANT::from(u32::try_from(eAVScenarioInfo_DisplayRemoting.0).unwrap_or(1)),
                );
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
                activate: activate.clone(),
                transform,
                events,
                codec,
                name,
                fps: fps.max(1),
                width,
                height,
                frame: 0,
                need_input: 0,
                ready: std::collections::VecDeque::new(),
                progress_at: Instant::now(),
                bps,
                provides_samples: provides,
                output_size: info.cbSize.max(width.saturating_mul(height)),
                _d3d: d3d,
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
    /// 收下编码器发来的所有事件（不等）：要输入就记下，有输出就取出来排队。返回是否排空了（DrainComplete）。
    fn pump(&mut self) -> Result<bool, String> {
        let mut drained = false;
        loop {
            // SAFETY: 不等待地取事件；没有事件时返回 MF_E_NO_EVENTS_AVAILABLE。
            let kind = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                // SAFETY: 事件对象有效。
                Ok(event) => unsafe { event.GetType() }.map_err(|e| err("事件类型", &e))?,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(drained),
                Err(e) => return Err(err("取事件", &e)),
            };
            let kind = i32::try_from(kind).unwrap_or(-1);
            if kind == METransformNeedInput.0 {
                self.need_input = self.need_input.saturating_add(1);
                self.progress_at = Instant::now();
            } else if kind == METransformHaveOutput.0 {
                let frame = self.output()?;
                self.ready.push_back(frame);
                self.progress_at = Instant::now();
            } else if kind == METransformDrainComplete.0 {
                drained = true;
            }
        }
    }

    /// 等事件，直到 `done` 为真或者过了 `limit`；编码器很久没有任何进展时报错（卡住了）。
    fn pump_until(&mut self, limit: Duration, done: impl Fn(&Self) -> bool) -> Result<(), String> {
        let started = Instant::now();
        loop {
            let _ = self.pump()?;
            if done(self) || started.elapsed() >= limit {
                break;
            }
            if self.progress_at.elapsed() > STALL {
                return Err(format!(
                    "{} 没有响应（{} 秒没有任何进展）",
                    self.name,
                    STALL.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    /// 交一帧 NV12，取回一个编好的帧（码流和是否关键帧）。**异步**（和 FFmpeg、Chromium 一样）：
    /// 编码器要输入时才交；编好的帧随时取，可能是之前交的；还没有就返回 `None`，之后用 [`Self::poll`]。
    /// 不再一帧一帧等结果：硬件编码器常常攒一两帧，同步等必然超时、然后整个改用软件编码。
    pub fn encode(
        &mut self,
        nv12: &[u8],
        bps: u64,
        force_key: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, String> {
        self.set_bitrate(bps);
        if force_key {
            if let Some(c) = &self.codec {
                // SAFETY: 接口有效，VARIANT 是本地值。
                let _ =
                    unsafe { c.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32)) };
            }
        }
        // 等编码器要输入（第一帧硬件要初始化，给长一点）
        let limit = if self.frame == 0 { FIRST_WAIT } else { STALL };
        self.pump_until(limit, |e| e.need_input > 0)?;
        if self.need_input == 0 {
            return Err(format!("{} 一直不要输入", self.name));
        }
        let sample = self.input_sample(nv12)?;
        // SAFETY: 样本有效。
        match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
            Ok(()) => self.need_input = self.need_input.saturating_sub(1),
            // 编码器还没准备好收：这一帧不编（下一帧再说），不算出错
            Err(e) if e.code() == MF_E_NOTACCEPTING => {}
            Err(e) => return Err(err("送入画面", &e)),
        }
        // 编好的帧一般马上就到：等一小会儿（低延迟），等不到下次再取
        self.pump_until(OUTPUT_WAIT, |e| !e.ready.is_empty())?;
        Ok(self.ready.pop_front())
    }

    /// 取走一个编好的帧（不等）。
    pub fn poll(&mut self) -> Result<Option<(Vec<u8>, bool)>, String> {
        let _ = self.pump()?;
        Ok(self.ready.pop_front())
    }

    /// 让编码器交出攒着的帧（画面停下后用；和 FFmpeg 结束时一样发 DRAIN，等 DrainComplete）。
    pub fn drain(&mut self) -> Result<(), String> {
        // SAFETY: 对象有效。
        unsafe { self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
            .map_err(|e| err("排空", &e))?;
        let started = Instant::now();
        while started.elapsed() < DRAIN_WAIT {
            if self.pump()? {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(())
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

/// Windows H.264 解码：系统自带的 Microsoft H.264 解码器 MFT。给了 D3D11 设备时用显卡解码（DXVA），
/// 解出的画面由 Media Foundation 拷回内存；不支持时在 CPU 上解（名称里写明）。输出 NV12，转成 RGB。
pub struct MfDecoder {
    transform: IMFTransform,
    name: String,
    provides_samples: bool,
    output_size: u32,
    /// 解码器输出的尺寸（高度按 16 对齐，例如 1080 → 1088）。
    coded: (usize, usize),
    /// 实际画面的尺寸（裁掉对齐的部分）。
    shown: (usize, usize),
    /// 每行字节数（没有 2D 缓冲区接口时用）。
    stride: usize,
    frame: i64,
    _d3d: Option<(ID3D11Device, IMFDXGIDeviceManager)>,
}

impl std::fmt::Debug for MfDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MfDecoder")
            .field("name", &self.name)
            .field("shown", &self.shown)
            .finish_non_exhaustive()
    }
}

// SAFETY: COM 对象按多线程套间创建，可以在线程之间移动；实际只在解码线程上使用。
unsafe impl Send for MfDecoder {}

/// 从小端字节里读一个 i32（`MFVideoArea` 的字段）。
fn le_i32(bytes: &[u8], at: usize) -> Option<i32> {
    let b: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
    Some(i32::from_le_bytes(b))
}

impl MfDecoder {
    /// 打开系统的 H.264 解码器。
    pub fn open() -> Result<Self, String> {
        // SAFETY: 同 MfEncoder::open；以下是对刚创建的 COM 对象的方法调用，参数是本地值。
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            startup()?;
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)
                    .map_err(|e| err("打开系统 H.264 解码器", &e))?;
            let mut aware = false;
            if let Ok(attrs) = transform.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
                aware = attrs.GetUINT32(&MF_SA_D3D11_AWARE).is_ok_and(|v| v != 0);
            }
            // 微软文档：低延迟解码（不攒帧，解出就交）；明确打开硬件加速
            if let Ok(c) = transform.cast::<ICodecAPI>() {
                let _ = c.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true));
                let _ = c.SetValue(&CODECAPI_AVDecVideoAcceleration_H264, &VARIANT::from(1u32));
            }
            // 有 D3D11 设备才是显卡解码；给不了就在 CPU 上解（照样能用，名称里写明）
            let d3d = if aware {
                match d3d_device(None).and_then(|made| {
                    transform
                        .ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, made.1.as_raw() as usize)
                        .map_err(|e| err("交给 D3D11 设备", &e))
                        .map(|()| made)
                }) {
                    Ok(made) => Some(made),
                    Err(why) => {
                        log::info!("H.264 解码不用显卡：{why}");
                        None
                    }
                }
            } else {
                None
            };
            let name = if d3d.is_some() {
                "Media Foundation（显卡 DXVA）"
            } else {
                "Media Foundation（CPU）"
            };
            let input = MFCreateMediaType().map_err(|e| err("输入格式", &e))?;
            input
                .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
                .map_err(|e| err("输入格式", &e))?;
            input
                .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
                .map_err(|e| err("输入格式", &e))?;
            transform
                .SetInputType(0, &input, 0)
                .map_err(|e| err("设置输入格式（H.264）", &e))?;
            let mut dec = Self {
                transform,
                name: name.to_owned(),
                provides_samples: false,
                output_size: 0,
                coded: (0, 0),
                shown: (0, 0),
                stride: 0,
                frame: 0,
                _d3d: d3d,
            };
            dec.select_output()?;
            dec.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
                .map_err(|e| err("开始", &e))?;
            dec.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
                .map_err(|e| err("开始", &e))?;
            Ok(dec)
        }
    }

    /// 解码器名称。
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 选 NV12 输出，记下尺寸、每行字节、实际画面区域。开始时和画面尺寸变了（STREAM_CHANGE）时调用。
    fn select_output(&mut self) -> Result<(), String> {
        // SAFETY: 对解码器和它给出的格式对象的方法调用，输出参数指向局部变量。
        unsafe {
            let mut i = 0u32;
            loop {
                let t = self
                    .transform
                    .GetOutputAvailableType(0, i)
                    .map_err(|e| err("解码器没有 NV12 输出", &e))?;
                i += 1;
                if t.GetGUID(&MF_MT_SUBTYPE).ok() != Some(MFVideoFormat_NV12) {
                    continue;
                }
                self.transform
                    .SetOutputType(0, &t, 0)
                    .map_err(|e| err("设置输出格式（NV12）", &e))?;
                let packed = t.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                let w = usize::try_from(packed >> 32).unwrap_or(0);
                let h = usize::try_from(packed & 0xffff_ffff).unwrap_or(0);
                self.coded = (w, h);
                self.stride = t
                    .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                    .ok()
                    .and_then(|s| usize::try_from(s).ok())
                    .filter(|s| *s >= w)
                    .unwrap_or(w);
                // 实际画面区域（H.264 的裁剪，例如 1088 行里只有 1080 行是画面）
                let mut area = [0u8; 16];
                let mut len = 0u32;
                let shown = t
                    .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut area, Some(&mut len))
                    .ok()
                    .and_then(|()| Some((le_i32(&area, 8)?, le_i32(&area, 12)?)))
                    .and_then(|(cw, ch)| {
                        Some((usize::try_from(cw).ok()?, usize::try_from(ch).ok()?))
                    })
                    .filter(|(cw, ch)| *cw > 0 && *ch > 0 && *cw <= w && *ch <= h);
                self.shown = shown.unwrap_or((w, h));
                let info = self
                    .transform
                    .GetOutputStreamInfo(0)
                    .map_err(|e| err("输出信息", &e))?;
                self.provides_samples = info.dwFlags
                    & u32::try_from(
                        MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0
                            | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0,
                    )
                    .unwrap_or(0)
                    != 0;
                let nv12 = u32::try_from(self.stride * h * 3 / 2).unwrap_or(u32::MAX);
                self.output_size = info.cbSize.max(nv12);
                return Ok(());
            }
        }
    }

    /// 解一帧（Annex B）。返回（宽, 高, `0x00RRGGBB`）；还没有画面时为 `None`。
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<(usize, usize, Vec<u32>)>, String> {
        let len = u32::try_from(data.len()).map_err(|_| "数据太大".to_owned())?;
        // SAFETY: 缓冲区按长度创建；Lock 的指针在 Unlock 之前有效，拷贝长度不超过缓冲区。
        let sample = unsafe {
            let buffer = MFCreateMemoryBuffer(len).map_err(|e| err("建缓冲区", &e))?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            buffer
                .Lock(&mut ptr, None, None)
                .map_err(|e| err("锁缓冲区", &e))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock().map_err(|e| err("解锁缓冲区", &e))?;
            buffer
                .SetCurrentLength(len)
                .map_err(|e| err("缓冲区长度", &e))?;
            let sample = MFCreateSample().map_err(|e| err("建样本", &e))?;
            sample.AddBuffer(&buffer).map_err(|e| err("样本", &e))?;
            let _ = sample.SetSampleTime(self.frame.saturating_mul(166_666));
            self.frame = self.frame.saturating_add(1);
            sample
        };
        // SAFETY: 样本有效。
        unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(|e| err("送入码流", &e))?;
        let mut latest = None;
        for _ in 0..8 {
            match self.output()? {
                Step::Frame(f) => latest = Some(f),
                Step::Changed => self.select_output()?,
                Step::NeedInput => break,
            }
        }
        Ok(latest)
    }

    fn output(&mut self) -> Result<Step, String> {
        let sample = if self.provides_samples {
            None
        } else {
            // SAFETY: 按解码器要求的大小建输出缓冲区。
            unsafe {
                let buffer =
                    MFCreateMemoryBuffer(self.output_size).map_err(|e| err("输出缓冲区", &e))?;
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
        // SAFETY: 输出数组只有一项；调用后取走其中的样本和事件集合释放。
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
                let sample = sample.ok_or_else(|| "解码器没有给出样本".to_owned())?;
                self.read(&sample).map(Step::Frame)
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(Step::NeedInput),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => Ok(Step::Changed),
            Err(e) => Err(err("取解码结果", &e)),
        }
    }

    /// 把一帧 NV12 读出来转成 RGB。显卡上的画面由 2D 缓冲区接口拷回内存。
    fn read(&self, sample: &IMFSample) -> Result<(usize, usize, Vec<u32>), String> {
        let (w, h) = self.shown;
        let rows = self.coded.1;
        let mut rgb = Vec::new();
        // SAFETY: Lock/Lock2D 返回的指针在对应的 Unlock 之前有效，读取长度按缓冲区实际长度截断。
        unsafe {
            let buffer = sample
                .ConvertToContiguousBuffer()
                .map_err(|e| err("取画面", &e))?;
            if let Ok(two_d) = buffer.cast::<IMF2DBuffer>() {
                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut pitch = 0i32;
                two_d
                    .Lock2D(&mut ptr, &mut pitch)
                    .map_err(|e| err("读画面", &e))?;
                let pitch = usize::try_from(pitch).unwrap_or(0);
                let total = pitch * rows * 3 / 2;
                let bytes = std::slice::from_raw_parts(ptr, total);
                let (y, uv) = bytes.split_at((pitch * rows).min(total));
                crate::color::nv12_to_rgb(y, uv, pitch, w, h, &mut rgb);
                let _ = two_d.Unlock2D();
            } else {
                let mut ptr: *mut u8 = std::ptr::null_mut();
                let mut len = 0u32;
                buffer
                    .Lock(&mut ptr, None, Some(&mut len))
                    .map_err(|e| err("读画面", &e))?;
                let bytes = std::slice::from_raw_parts(ptr, usize::try_from(len).unwrap_or(0));
                let split = (self.stride * rows).min(bytes.len());
                let (y, uv) = bytes.split_at(split);
                crate::color::nv12_to_rgb(y, uv, self.stride, w, h, &mut rgb);
                let _ = buffer.Unlock();
            }
        }
        Ok((w, h, rgb))
    }
}

/// 取一次输出的结果。
enum Step {
    Frame((usize, usize, Vec<u32>)),
    Changed,
    NeedInput,
}
