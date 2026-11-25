//! Windows 采集的显卡通路（系统胶水）：桌面复制的 BGRA 画面在显卡里拷一份，用显卡的视频处理器
//! （`ID3D11VideoProcessor`）转成 NV12、缩到编码尺寸，交出显卡画面；CPU 不碰像素。
//!
//! 依据（`docs/research/K-pipeline.md` P15）：微软 Learn 的 Video Processor / `VideoProcessorBlt` 文档；
//! FFmpeg `libavfilter/vf_scale_d3d11.c`（同样的接口）；Sunshine `src/platform/windows/display_vram.cpp`
//! （桌面复制的纹理先 `CopyResource` 再转换，因为它只在 `ReleaseFrame` 之前有效）。
//!
//! 颜色：输入按 sRGB 全范围，输出 BT.601 有限范围（`DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601`），
//! 和全链路一致（macOS 采集、解码端、界面着色器都按 BT.601）。默认值不可靠（FFmpeg 不设，结果看驱动），
//! 这里显式设。
//!
//! 软件编码（或编码器在另一块显卡上）时 [`WinFrame`] 读回一次 NV12（只拷贝）。
//! 只在 Windows 上编译，覆盖率统计不含这个文件，由 Windows 实机测试验证。

#![allow(unsafe_code)]

use std::mem::ManuallyDrop;

use peerdesk_core::frame::{GpuFrame, GpuHandle, Nv12};
use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext1, ID3D11VideoDevice,
    ID3D11VideoProcessor, ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_TEX2D_VPIV,
    D3D11_TEX2D_VPOV, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE, D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT, D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC, D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_OPTIMAL_SPEED, D3D11_VPIV_DIMENSION_TEXTURE2D,
    D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709, DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
    DXGI_FORMAT, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL, DXGI_SAMPLE_DESC,
};

/// 显卡上的格式转换和缩放：桌面尺寸 `src` 的 BGRA → 编码尺寸 `dst` 的 NV12。
pub(crate) struct GpuConverter {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    video: ID3D11VideoDevice,
    video_ctx: ID3D11VideoContext1,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// 桌面画面的拷贝（桌面复制给的纹理只在 ReleaseFrame 之前有效）。
    copy: ID3D11Texture2D,
    src: (u32, u32),
    dst: (u16, u16),
    luid: u64,
}

impl GpuConverter {
    /// 在采集用的设备上建转换器。显卡不支持时返回原因，调用方改在 CPU 上转（并写日志）。
    pub(crate) fn new(
        device: &ID3D11Device,
        ctx: &ID3D11DeviceContext,
        luid: u64,
        src: (u32, u32),
        dst: (u16, u16),
    ) -> Result<Self, String> {
        let video: ID3D11VideoDevice = device
            .cast()
            .map_err(|e| format!("设备不支持视频处理（{e}）"))?;
        let video_ctx: ID3D11VideoContext1 = ctx
            .cast()
            .map_err(|e| format!("设备上下文不支持视频处理（{e}）"))?;
        // 采集线程转换、编码线程读回和编码：同一个设备在两个线程上用，打开多线程保护
        if let Ok(mt) = device.cast::<ID3D10Multithread>() {
            // SAFETY: 只是打开设备的多线程保护。
            let _ = unsafe { mt.SetMultithreadProtected(true) };
        }
        let rate = DXGI_RATIONAL {
            Numerator: 60,
            Denominator: 1,
        };
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: src.0,
            InputHeight: src.1,
            OutputFrameRate: rate,
            OutputWidth: u32::from(dst.0),
            OutputHeight: u32::from(dst.1),
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        // SAFETY: 描述是完整填写的局部变量；返回的对象由本结构持有。
        let enumerator = unsafe { video.CreateVideoProcessorEnumerator(&desc) }
            .map_err(|e| format!("建视频处理器枚举（{e}）"))?;
        for (format, need, what) in [
            (
                DXGI_FORMAT_B8G8R8A8_UNORM,
                D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_INPUT.0,
                "输入 BGRA",
            ),
            (
                DXGI_FORMAT_NV12,
                D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0,
                "输出 NV12",
            ),
        ] {
            // SAFETY: 只是查询。
            let support = unsafe { enumerator.CheckVideoProcessorFormat(format) }
                .map_err(|e| format!("查询视频处理格式（{e}）"))?;
            let need = u32::try_from(need).unwrap_or(0);
            if support & need == 0 {
                return Err(format!("显卡的视频处理器不支持{what}"));
            }
        }
        // SAFETY: 枚举是上面建的。
        let processor = unsafe { video.CreateVideoProcessor(&enumerator, 0) }
            .map_err(|e| format!("建视频处理器（{e}）"))?;
        let src_rect = rect(src.0, src.1);
        let dst_rect = rect(u32::from(dst.0), u32::from(dst.1));
        // SAFETY: 处理器是上面建的；矩形是局部变量。
        unsafe {
            video_ctx.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_ctx.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            video_ctx.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
            );
            video_ctx.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src_rect));
            video_ctx.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst_rect));
            video_ctx.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst_rect));
        }
        let copy = texture(
            device,
            src,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0,
        )?;
        Ok(Self {
            device: device.clone(),
            ctx: ctx.clone(),
            video,
            video_ctx,
            enumerator,
            processor,
            copy,
            src,
            dst,
            luid,
        })
    }

    /// 桌面尺寸和编码尺寸。
    pub(crate) const fn sizes(&self) -> ((u32, u32), (u16, u16)) {
        (self.src, self.dst)
    }

    /// 拷下桌面复制交来的画面（在 ReleaseFrame 之前调用）。
    pub(crate) fn copy_desktop(&self, desktop: &ID3D11Texture2D) {
        // SAFETY: 两个纹理尺寸、格式相同（调用方按桌面尺寸建的转换器），都在同一个设备上。
        unsafe { self.ctx.CopyResource(&self.copy, desktop) };
    }

    /// 把拷下的画面转成编码尺寸的 NV12（新的一张纹理：上一张可能还在编码器里）。
    pub(crate) fn convert(&self) -> Result<WinFrame, String> {
        let nv12 = texture(
            &self.device,
            (u32::from(self.dst.0), u32::from(self.dst.1)),
            DXGI_FORMAT_NV12,
            D3D11_BIND_RENDER_TARGET.0,
        )?;
        let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV {
                    MipSlice: 0,
                    ArraySlice: 0,
                },
            },
        };
        let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut input: Option<ID3D11VideoProcessorInputView> = None;
        let mut output: Option<ID3D11VideoProcessorOutputView> = None;
        // SAFETY: 纹理、枚举都在同一个设备上；描述和输出参数是局部变量。
        unsafe {
            self.video
                .CreateVideoProcessorInputView(
                    &self.copy,
                    &self.enumerator,
                    &in_desc,
                    Some(&mut input),
                )
                .map_err(|e| format!("建视频处理输入（{e}）"))?;
            self.video
                .CreateVideoProcessorOutputView(
                    &nv12,
                    &self.enumerator,
                    &out_desc,
                    Some(&mut output),
                )
                .map_err(|e| format!("建视频处理输出（{e}）"))?;
        }
        let output = output.ok_or("没有建出视频处理输出")?;
        let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: ManuallyDrop::new(input),
            ..D3D11_VIDEO_PROCESSOR_STREAM::default()
        };
        // SAFETY: 处理器、输出视图、输入视图都有效；结构里多持有的输入视图引用在下面释放。
        let blt = unsafe {
            self.video_ctx.VideoProcessorBlt(
                &self.processor,
                &output,
                0,
                std::slice::from_ref(&stream),
            )
        };
        // SAFETY: 上面放进去的引用只释放这一次。
        unsafe { ManuallyDrop::drop(&mut stream.pInputSurface) };
        blt.map_err(|e| format!("显卡转换颜色和缩放（{e}）"))?;
        Ok(WinFrame {
            texture: nv12,
            device: self.device.clone(),
            ctx: self.ctx.clone(),
            luid: self.luid,
            size: self.dst,
        })
    }
}

fn rect(w: u32, h: u32) -> RECT {
    RECT {
        left: 0,
        top: 0,
        right: i32::try_from(w).unwrap_or(i32::MAX),
        bottom: i32::try_from(h).unwrap_or(i32::MAX),
    }
}

/// 在显卡上建一张纹理。
fn texture(
    device: &ID3D11Device,
    (w, h): (u32, u32),
    format: DXGI_FORMAT,
    bind: i32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: u32::try_from(bind).unwrap_or(0),
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    // SAFETY: 描述完整填写，输出参数指向局部变量。
    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }
        .map_err(|e| format!("建纹理 {w}×{h}（{e}）"))?;
    tex.ok_or_else(|| "CreateTexture2D 没有返回纹理".to_owned())
}

/// 显卡里的一帧 NV12（编码尺寸）。硬件编码器在同一块显卡上时直接用；否则读回一次。
pub(crate) struct WinFrame {
    texture: ID3D11Texture2D,
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    luid: u64,
    size: (u16, u16),
}

impl std::fmt::Debug for WinFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinFrame")
            .field("size", &self.size)
            .field("luid", &self.luid)
            .finish_non_exhaustive()
    }
}

// SAFETY: D3D11 设备是线程安全的；立即上下文在两个线程（采集、编码）上用，设备打开了多线程保护
// （GpuConverter::new）；纹理建好后只读。
unsafe impl Send for WinFrame {}
// SAFETY: 同上。
unsafe impl Sync for WinFrame {}

impl GpuFrame for WinFrame {
    fn size(&self) -> (u16, u16) {
        self.size
    }

    fn handle(&self) -> GpuHandle {
        GpuHandle::D3d11Texture {
            texture: self.texture.as_raw().cast_const(),
            adapter_luid: self.luid,
        }
    }

    fn read_nv12(&self) -> Result<Nv12, peerdesk_core::Error> {
        let fail = |_| peerdesk_core::Error::Inconsistent("显卡画面读不回来");
        let (w, h) = (u32::from(self.size.0), u32::from(self.size.1));
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_NV12,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: u32::try_from(D3D11_CPU_ACCESS_READ.0).unwrap_or(0),
            MiscFlags: 0,
        };
        let mut staging: Option<ID3D11Texture2D> = None;
        // SAFETY: 描述完整填写，输出参数指向局部变量。
        unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut staging)) }.map_err(fail)?;
        let staging = staging.ok_or(peerdesk_core::Error::Inconsistent("没有建出读回用的纹理"))?;
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: 两个纹理同尺寸、同格式、同设备；staging 允许 CPU 读，输出参数指向局部变量。
        unsafe {
            self.ctx.CopyResource(&staging, &self.texture);
            self.ctx
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
                .map_err(fail)?;
        }
        let pitch = usize::try_from(mapped.RowPitch).unwrap_or(0);
        let rows = usize::from(self.size.1);
        let result = if mapped.pData.is_null() {
            Err(peerdesk_core::Error::Inconsistent("显卡画面读不回来"))
        } else {
            // SAFETY: NV12 映射出来是 亮度 行宽×高 字节、紧接着色度 行宽×高/2 字节，Unmap 之前有效。
            let bytes = unsafe {
                std::slice::from_raw_parts(mapped.pData.cast::<u8>(), pitch * rows * 3 / 2)
            };
            let (y, uv) = bytes.split_at(pitch * rows);
            Nv12::from_planes(self.size.0, self.size.1, y, pitch, uv, pitch)
        };
        // SAFETY: 与上面成功的 Map 配对。
        unsafe { self.ctx.Unmap(&staging, 0) };
        result
    }
}
