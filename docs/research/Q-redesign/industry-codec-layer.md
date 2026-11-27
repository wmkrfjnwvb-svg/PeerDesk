# 远程桌面编解码层：业内做法与出处

调研日期 2026-11-27。只整理业内做法，不评价我们的代码。
所有引文来自 2026-11-27 当天抓取的 master/main 分支文件，行为可能随版本变化。
标注"（未核实）"的是没读到原文、或只有二手摘要的内容。

## 0. 许可证速查（先看这个，决定哪些只能"读"不能"抄"）

| 项目 | 许可证 | 出处 | 对我们（Apache-2.0）|
|---|---|---|---|
| Sunshine | GPL-3.0 | https://raw.githubusercontent.com/LizardByte/Sunshine/master/LICENSE 首行 "GNU GENERAL PUBLIC LICENSE Version 3" | 只能参考思路，不能抄代码 |
| Moonlight-qt / moonlight-ios | GPL-3.0 | https://raw.githubusercontent.com/moonlight-stream/moonlight-qt/master/LICENSE ；moonlight-ios/LICENSE.txt 同为 GPLv3 | 同上 |
| RustDesk | AGPL-3.0 | https://raw.githubusercontent.com/rustdesk/rustdesk/master/LICENCE 首行 "GNU AFFERO GENERAL PUBLIC LICENSE Version 3" | 同上，且 hwcodec 子库的许可证没读到（见第 2 节）|
| OBS Studio | GPL-2.0+ | obs-ffmpeg-nvenc.c 文件头 "GNU General Public License, either version 2 ... or (at your option) any later version" | 同上 |
| Chromium / WebRTC | BSD-3 风格（未逐文件核实）| 文件头未读；常识 | 可读，可参考；如需拷贝代码要核实具体文件头 |
| FFmpeg | LGPL-2.1+（默认），开 --enable-gpl 变 GPL | https://ffmpeg.org/legal.html | 见第 2 节 |

## 1. 各家编码层架构

### 1.1 Sunshine（GPL-3，C++）

要点：
- 直接对接厂商 SDK + FFmpeg 两条路并存。编码器清单按顺序登记：Windows 是 `nvenc`、`quicksync`、`amdvce`、`mediafoundation`；Linux 是 `vulkan`、`vaapi`；macOS 是 `videotoolbox`；最后是 `software`。
  出处：https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/video.cpp ，`encoders` 向量。
- NVENC 是自己直接调 NVENC API（src/nvenc/），不经过 FFmpeg；QSV、AMF、VA-API、VideoToolbox 经 FFmpeg 的 `av_hwdevice`/`avcodec`。
  依据：video.cpp 里有 `nvenc_encode_session_t`、`avcodec_encode_session_t`、`vt_encode_session_t` 三种会话类；display_vram.cpp 的 `d3d_nvenc_encode_device_t::init_device()` 调 `create_nvenc_d3d11_native`。
- **选择器 = 逐个试，不是查表**。`probe_encoders()` 按顺序对每个编码器调 `validate_encoder()`，第一个通过的胜出；软件编码器带 `ALWAYS_REPROBE` 标记（"encoder of last resort and we want to aggressively probe for a better one"），意思是用软件兜底时每次都重新探测硬件。
  出处：video.cpp 第 426 行（枚举注释）和第 3662 行起 `probe_encoders`。
- **探测 = 真编一帧**。`validate_config()` 建会话、喂一帧 dummy 图、请求 IDR、等第一个包，然后检查：
  - 第一个包必须是 IDR，否则报错 "First packet type is not an IDR frame" 并判失败；
  - H.264/HEVC 还用 `cbs::validate_sps` 检查 SPS 里的 VUI 参数（是否带 bitstream_restriction），结果作为能力位 `VUI_PARAMS`。
  探测用两套配置：`config_max_ref_frames {1920,1080,60,6000,...}`（限定参考帧数）和 `config_autoselect`（不限），结果分别记为 `REF_FRAMES_RESTRICT` 和 `PASSED`。
  出处：video.cpp `validate_config`（约第 3321 行）与 `validate_encoder`（约第 3416 行）。
- **GPU 厂商过滤**：`is_codec_supported()` 按适配器 VendorId 只接受匹配的编码器：AMD(0x1002) 只收 `*_amf`，Intel(0x8086) 只收 `*_qsv`，NVIDIA(0x10de) 只收 `*_nvenc`，Qualcomm 只收 `*_mf`；AV1 要求 AMF ≥1.4.30。
  出处：https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/platform/windows/display_vram.cpp
- **零拷贝**：捕获纹理用 `D3D11_RESOURCE_MISC_SHARED_NTHANDLE` 建共享句柄，在编码器那边的 D3D11 设备上 `OpenSharedResource1` 打开，带 keyed mutex；颜色转换是一次 render pass（`convert_Y_or_YUV_vs` 等 HLSL 着色器），RGB→NV12/P010 直接画进 NVENC 的输入纹理（`nvenc_d3d->get_input_texture()`）。QSV 走 FFmpeg 派生的 D3D11 hwframes。全程不读回内存。
  出处：display_vram.cpp，`d3d_base_encode_device::convert()`、`initialize_image_context()`。
- **运行中不改码率**：video.cpp 里 grep `set_bitrate|reconfigure|change_bitrate` 无命中；码率在建会话时写死（`ctx->bit_rate`、`ctx->rc_max_rate`、`rc_buffer_size = bitrate / framerate`）。客户端改码率要重开串流。nvenc_base.cpp 里同样没有 `NvEncReconfigureEncoder` 调用。（我们读的是文件，未实际运行验证）
- **出错回退**：
  - 建 FFmpeg 会话失败时允许 1 次重试，换一组 `fallback_options`（例：QSV 先 `low_power=1`，失败改 `low_power=0`；video.cpp 约第 2291、2496、2543 行）。
  - 没有可用硬件时落到 `software`（x264/x265/SVT-AV1）。
  - 编码器不支持参考帧失效时：`invalidate_ref_frames()` 日志 "Encoder doesn't support reference frame invalidation" 然后改请求 IDR。
  - 没有活动显示器时拒绝探测："No display devices are active at the moment! Cannot probe the encoders."

各编码器的低延迟参数（均出自 video.cpp 的 `encoders` 定义）：

| 编码器 | 选项 | 标志位 |
|---|---|---|
| nvenc（直接 SDK）| 见 nvenc_base.cpp，下文 3 节 | `REF_FRAMES_INVALIDATION`（仅 Windows NVENC）|
| quicksync | `low_delay_brc=1`、`low_power=1`（失败回退 `low_power=0`）| `CBR_WITH_VBR | RELAXED_COMPLIANCE | NO_RC_BUF_LIMIT` |
| amdvce | `latency=1`、`frame_skipping=0`、`header_insertion_mode=idr`；H.264 回退 `usage=2(LOW_LATENCY)` | - |
| mediafoundation | `rate_control=cbr`、`scenario=display_remoting` | `FIXED_GOP_SIZE`（注释：不支持按需 IDR）|
| vaapi | `async_depth=1`、`idr_interval=INT_MAX`、`blbrc` | `LIMITED_GOP_SIZE | NO_RC_BUF_LIMIT` |
| videotoolbox | 选项为空 | 仅 `PARALLEL_ENCODING` |

注意 `CBR_WITH_VBR`：QSV 要把 `bit_rate` 减 1 让它和 `rc_max_rate` 不相等，才能"在 QSV 里实际跑成 CBR 行为"（video.cpp 第 2507 行 `ctx->bit_rate--`）。RustDesk 也有同样处理（见 1.3）。这是"码率控制不听话"的一个具体案例。

### 1.2 Moonlight（GPL-3，仅解码端，见第 5 节）

### 1.3 RustDesk（AGPL-3，Rust + C++ FFmpeg 封装）

- hwcodec 是 RustDesk 自己维护的 crate（rustdesk-org/hwcodec），底层是 FFmpeg。
  出处：https://github.com/rustdesk-org/hwcodec 页面："Windows VRAM encoding and decoding use FFmpeg only, and the SDK code is kept just for driver availability checks."
  README 的支持表：Windows Intel→QSV、NVIDIA→NVENC、AMD→AMF，各有 RAM 和 VRAM 两种路径；Linux 用 VAAPI（NVIDIA 另有 NVENC/NVDEC）；macOS 只有 H.265 RAM 路径；Android 编码 RAM 路径。
- 两条路径：
  - RAM 路径（`ffmpeg_ram`）：帧从内存喂给 FFmpeg 编码器（`AV_PIX_FMT_NV12`/`YUV420P`），Windows 用 `AV_HWDEVICE_TYPE_D3D11VA`、Linux 用 `VAAPI` 上传。
  - VRAM 路径（`vram`，Windows）：直接吃 D3D11 纹理；`hw_ffmpeg_vram_encode.cpp` 里 `av_hwdevice_ctx_alloc` 后对 QSV 做 `av_hwdevice_ctx_create_derived`。
  - 该仓库声明避开 CUDA 上下文路径："the project avoids CUDA-context-based paths because of a reported system freeze and black-screen issue. NVIDIA encoding feeds NV12 frames through D3D11, and NVIDIA decoding uses D3D11VA instead of CUDA."（hwcodec README 页面）
- **能力探测在子进程里做**，结果缓存，并带 GPU 签名：`check_available_hwcodec()` 在 libs/scrap/src/common/hwcodec.rs，建一个 1280x720、1000kbps、CBR 的测试编码上下文逐个试；加载缓存时比较 `get_gpu_signature()`，不一致就丢缓存重测（日志 "gpu signature changed"）。子进程里调用 `setup_parent_death_signal()`（注释 "called by child process"）。
  出处：https://raw.githubusercontent.com/rustdesk/rustdesk/master/libs/scrap/src/common/hwcodec.rs 第 496、614、683 行附近。
  理由推测（未见作者说明）：驱动在探测时崩溃不会带走主进程。
- **参数**：GOP 默认 `i32::MAX`（`DEFAULT_GOP`，hwcodec.rs 第 33 行）；速率控制默认 CBR（`rate_control()` 只有 Android mediacodec 返回 VBR）；FFmpeg 上下文 `c->flags |= AV_CODEC_FLAG_LOW_DELAY`，`slices=1`；每个编码器各自设 "无延迟"选项：nvenc `delay=0`、amf `query_timeout=1000`、qsv/vaapi `async_depth=1`、videotoolbox `realtime=1` 和 `prio_speed=1`。
  出处：https://raw.githubusercontent.com/rustdesk-org/hwcodec/master/cpp/common/util.cpp 第 21–100 行。
- 质量预设：nvenc `preset p1`/`p4`，amf `quality` 取 `speed/balanced/quality`，qsv `veryfast/medium/veryslow`（util.cpp `set_quality`）。
- **运行中改码率**：`set_bitrate()` 只改 `c->bit_rate = kbs*1000`，QSV 额外同步 `rc_max_rate`，不重建编码器（util.cpp `change_bit_rate`）。是否真被各厂商编码器采纳，**未核实**（FFmpeg 的 nvenc 包装是否在 bit_rate 变化时调 reconfigure，我没读 libavcodec/nvenc.c）。
  `set_quality` 经 `check_bitrate_range` 把码率夹在编码器上下限内。
- **回退**：探测不到硬件就返回软件路径（libvpx/openh264，此处未展开）；Windows 上每个 GPU 厂商一个 `Driver` 枚举 `NV/AMF/MFX/FFMPEG`。

### 1.4 Chromium remoting（Chrome 远程桌面）和 WebRTC（BSD 系）

- **接口形态**（WebRTC `VideoEncoder`，https://webrtc.googlesource.com/src/+/refs/heads/main/api/video_codecs/video_encoder.h）：
  - `Encode(frame, frame_types)`：`frame_types` 里可带 key frame 请求；
  - `SetRates(RateControlParameters)`：含 `target_bitrate`、`bitrate`、`framerate_fps`、`bandwidth_allocation`，注释写 "These settings are instantaneous (i.e. not moving averages) and should apply from now until the next call to SetRates()"；
  - `OnPacketLossRateUpdate(float)`、`OnRttUpdate(int64_t)`；
  - `EncodedImageCallback::OnFrameDropped`：编码器丢帧时回报，注释 "usually either because of rate control buffer overflow or because the rate allocation of a given spatial index is set to zero"；
  - `EncoderInfo` 里有 `has_trusted_rate_controller`、`is_hardware_accelerated`、`supports_native_handle`、`requested_resolution_alignment`、`preferred_pixel_formats`。
  - `GetEncoderInfo()` 注释说 "may change during runtime. For instance if a hardware encoder fails, it may fall back to doing software encoding"。
- **`has_trusted_rate_controller` 的注释是本调研最直接相关的一条**："If this method returns false (default behavior), the media opt frame dropper will drop input frames if it suspect encoder misbehavior. Misbehavior is common, especially in hardware codecs."（video_encoder.h 第 205–219 行）。即 WebRTC 在编码器之外再有一层丢帧器兜底，默认不信任硬件码控。
- **WebRTC 软件 H.264（OpenH264 封装）**：`iUsageType` 屏幕共享用 `SCREEN_CONTENT_REAL_TIME`，否则 `CAMERA_VIDEO_REAL_TIME`；`iRCMode = RC_BITRATE_MODE`；`bEnableFrameSkip = frame_dropping_on`；`uiIntraPeriod = key_frame_interval`；`iMaxBitrate = UNSPECIFIED_BIT_RATE`（注释链接 crbug 11543）。
  出处：https://webrtc.googlesource.com/src/+/refs/heads/main/modules/video_coding/codecs/h264/h264_encoder_impl.cc 第 750–771 行。
- **Chromium 远程桌面的硬件编码器** `WebrtcVideoEncoderGpu`：用 `media::VideoEncodeAccelerator`（系统层抽象），固定 `H264PROFILE_MAIN`，`ContentType::kDisplay`，`ConstantBitrate`；码率变化走 `RequestEncodingParametersChange(Bitrate::ConstantBitrate(bps), fps, ...)`；关键帧走 `Encode(video_frame, params.key_frame)`。
  出处：https://chromium.googlesource.com/chromium/src/+/refs/heads/main/remoting/codec/webrtc_video_encoder_gpu.cc 第 56–58、268–278、368–372 行。
  - 同目录有 `video_encoder_active_map.cc`（只编码有变化的区域，VP8/VP9 软件路径用）和 `encoder_bitrate_filter.cc`。
  - **`kH264MinimumTargetBitrateKbpsPerMegapixel = 1800`**：硬件 H.264 在码率太低时质量崩，所以给带宽估计加一个按分辨率算的下限。`EncoderBitrateFilter` 另有 `kEncoderBitrateChangePercentage = 33`（目标码率变化不到 33% 就不下发）和 `kBandwidthWeightFactor = 0.95`（指数平均）。
- **Chromium 的 Windows MF 编码器（`media_foundation_video_encode_accelerator_win.cc`）里的厂商适配，这是最有价值的第一手"踩坑清单"**：
  https://chromium.googlesource.com/chromium/src/+/refs/heads/main/media/gpu/windows/media_foundation_video_encode_accelerator_win.cc
  - 用 `ICodecAPI` 设：`AVEncCommonRateControlMode`（CBR / PeakConstrainedVBR / Quality 之一）、`AVEncCommonMeanBitRate`、`AVEncCommonMaxBitRate`、`AVEncMPVGOPSize`、`AVLowLatencyMode`、`AVEncMPVDefaultBPictureCount`（高通强制 0，注释 crbug 343748806）、强制关键帧用 `AVEncVideoForceKeyFrame`。
  - **软件码控（SW BRC）**：`SetSWRateControl()` 在 CBR 且时间层 ≤3 时启用（`kMediaFoundationUseSoftwareRateCtrl` 默认开，media_switches.cc）。做法是 Chromium 自己算每帧 QP，用 `CODECAPI_AVEncVideoEncodeQP` 逐帧下发（第 2043 行），必要时自己丢帧（"Frame dropped by software rate control"）。理由是 MFT 自带码控不可靠。对 H.264 有厂商例外：`disable_h264_accelerator_sw_brc` 工作区开关下，AMD 和 ARM 的 H.264 HMFT 禁用 SW BRC（crbug 417752242、390581539）；高通 HEVC 和高通 AV1 的 HMFT 不适用 SW BRC（crbug 390581539、343757696）。
  - **屏幕内容提示**：对 Intel 且 `kDisplay` 时设 `CODECAPI_AVScenarioInfo = eAVScenarioInfo_DisplayRemoting`。注释原文："For AV1 screen content encoding, configure scenario to enable AV1 SCC tools(palette mode, intra block copy, etc.) This will also turn off CDEF on I-frame, and enable long term reference for screen contents."；"TODO: Set scenario info if we confirm it works on other vendors"（第 1870–1890 行）。即目前只有 Intel 被验证过。
  - 高通要设 `eAVScenarioInfo_CameraRecord`，因为 DisplayRemoting 会引起丢帧和时间戳问题（crbug 343757695）。
  - NVIDIA：给 MFT 喂一张刚创建的纹理后必须 `device_context->Flush()`，否则 MFT "may not see the data"（第 2279–2284 行）。
  - 输入纹理 `BindFlags = D3D11_BIND_VIDEO_ENCODER`，用 `MFCreateDXGISurfaceBuffer` 包成 MF 缓冲，设备要和 MFT 的 LUID 匹配（`IsMatchingDevice`）；设备管理器设置失败时回退到非 D3D11 编码（"fallback to non-D3D11 encoding"）。
  - Intel 双时间层时要把 `AVEncVideoMaxNumRefFrame` 设 1。
  - 分辨率变化时 "Some software rate controllers require reset on resolution change"。
- **MF H.264 编码器官方文档**（https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder）：
  - 平台自带 MFT 默认是无约束 VBR，CBR 和约束 VBR 要 Windows 8+；
  - `CODECAPI_AVEncCommonMeanBitRate`："In Windows 8, you can set the average bit rate during encoding. If the bit rate changes, the encoder uses adaptive encoding."；
  - 默认 `AVEncMPVDefaultBPictureCount = 0`、`AVLowLatencyMode` 默认走切片并行（"The encoder defaults to slice encoding, to minimize latency"）；
  - 认证硬件编码器必须支持：RateControlMode、Quality、QualityVsSpeed、MeanBitRate、MaxBitRate、BufferSize、GOPSize、EncodeQP、ForceKeyFrame；
  - LTR（`AVEncVideoLTRBufferControl`/`MarkLTRFrame`/`UseLTRFrame`）、MinQP/MaxQP、`SliceControlMode`、`MaxNumRefFrame` 是可选，HCK 里"如果支持则测试"。所以不能假设厂商 MFT 都有 LTR。
  - `AVEncVideoLTRBufferControl` 是静态属性（"This is a static API"）。

### 1.5 Parsec（闭源，只有公开说法）

- "Parsec is designed from that point onward is to never let the raw frame touch system memory"；"Parsec performs all color conversion on the GPU via pixel shaders"；"We've made the decision at Parsec to only support hardware enabled video encoding and decoding."（https://parsec.app/blog/description-of-parsec-technology-b2738dcc3842）
- 当前技术页："We support the h.264 codec and low latency desktop capture, with a zero-copy GPU pipeline to the encoder."；"We use hardware decoding whenever possible on every platform we support."；"ultra-responsive dynamic bitrate adjustment"（https://parsec.app/technology）。该页只写 H.264，另一个搜索摘要说 H.265 已支持，我没在原文看到，（未核实）。
- 旧博文给出编码延迟中位数 NVENC 5.8 ms、AMD VCE 15.06 ms（https://parsec.app/blog/nvidia-nvenc-outperforms-amd-vce-on-h-264-encoding-latency-in-parsec-co-op-sessions-713b9e1e048a ，来自搜索摘要，未读原文；数据是 VCE 时代的，已过时）。
- 支持文档摘要：主机需要有硬件视频编码器的 GPU，且显示器要直接接在该 GPU 上（搜索摘要，未读原文，未核实）。

### 1.6 OBS（GPL-2+，直播/录屏，不是低延迟桌面，但编码器接入最全）

- 编码器是分开的插件：`plugins/obs-nvenc`（直接 NVENC，含 `nvenc-d3d11.c`、`nvenc-cuda.c`、`nvenc-opengl.c` 三种输入路径）、`plugins/obs-qsv11`（QSV）、`plugins/obs-ffmpeg/texture-amf.cpp`（AMF，直接用 AMF SDK）、`plugins/obs-ffmpeg/obs-ffmpeg-nvenc.c`（FFmpeg 的 NVENC 回退）。
- NVENC D3D11 路径：`NvEncRegisterResource` 注册 `NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX`，格式 NV12/P010；输入纹理由 `OpenSharedResource` 从 句柄打开（obs_nvenc_d3d11.c 第 118–188 行）。
- **运行中改码率**（obs_nvenc.c 第 95–125 行）：先查 `NV_ENC_CAPS_SUPPORT_DYN_BITRATE_CHANGE`，支持才调 `nvEncReconfigureEncoder`，且参数是 `resetEncoder = 1; forceIDR = 1;`，也就是 OBS 每次改码率都会出一个 IDR。注释写 "Only support reconfiguration of CBR bitrate"。
- **AMF 改码率**（texture-amf.cpp 第 1386–1411 行）：改 `TARGET_BITRATE`、`PEAK_BITRATE`、`VBV_BUFFER_SIZE` 属性，注释 "Changing the bitrate (or any other dynamic property) is updated with the next SubmitInput() call. For CBR mode, flushing the pipeline is not needed"。CBR 同时开 `FILLER_DATA_ENABLE`；延迟约束 VBR 的 VBV = `(bitrate/framerate)*1.1`。
- OBS 默认（FFmpeg NVENC 包装）：`rate_control=CBR`、`preset p5`、`tune hq`、`multipass qres`、`bf=2`、`psycho_aq=true`；创建失败只重试"关 psycho_aq"，没有跨编码器回退。这是直播参数，不是低延迟参数。
- OBS 官方说明（https://obsproject.com/kb/hardware-encoding）：NVENC 从 Maxwell 起，Turing（GTX 1650 rev2 起）质量最好；QSV 推荐 Haswell 及以后，早期 QSV 质量差；多 GPU 时不要用核显 QSV 而要用独显编码器；Intel Mac 的 VideoToolbox 因没有 CBR 不支持直播。（"lacks the constant bitrate most streaming services require"）

### 1.7 FFmpeg（LGPL-2.1+ 主体）

出处：https://ffmpeg.org/ffmpeg-codecs.html
- `h264_mf`（Media Foundation 包装）：选项 `rate_control`（default/cbr/pc_vbr/u_vbr/quality/ld_vbr/g_vbr/gld_vbr）、`scenario`（含 `display_remoting`、`display_remoting_with_feature_map`、`video_conference`、`live_streaming` 等）、`quality`(0-100)、`hw_encoding`（默认 0，要显式开）。文档说输入 "nv12 is the safer choice, especially among HW encoders"，硬件加速要 D3D11。
- `h264_nvenc`：`forced-idr`（强制关键帧时是否用 IDR，-1 交给编码器）、`zerolatency`（无重排延迟）、`intra-refresh`、`single-slice-intra-refresh`、`dpb_size`、`nonref_p`、`max_slice_size`、`delay`、`tune`、`rc`。
- `h264_qsv`：`async_depth`（"the higher the value the higher the latency"）、`low_power`（"to reduce power consumption and GPU usage"）、`scenario`（含 `displayremoting`、`remotegaming`、`gamestreaming`）、`int_ref_*` 一类帧内刷新选项（选项名未逐个核对；文档原文 "Distance between the beginnings of the intra-refresh cycles in frames"）。
- `h264_vaapi`：`low_power`、`async_depth`；rate control 由 `-b:v`/`-maxrate`/`-bufsize` 推；
- `h264_videotoolbox`：RustDesk 用到的 `realtime`、`prio_speed`；Sunshine 的 videotoolbox 选项为空，由自己的 `vt_encode_session_t` 直接调系统 API，不走 FFmpeg。
- `h264_amf`：Sunshine 用 `usage=lowlatency`（Sunshine H.264 回退）、`frame_skipping=0`、`header_insertion_mode=idr`（见 1.1 表）。

## 2. 厂商 SDK 许可证和打包

| 组件 | 许可证（已读到的原文）| 能否进 Apache-2.0 项目 | 打包方式 |
|---|---|---|---|
| NVENC 头文件（`nvEncodeAPI.h` 的 FFmpeg 副本 `nv-codec-headers`）| 文件头为 MIT 风格："Permission is hereby granted, free of charge ... to deal in the Software without restriction"，并写 "This copyright notice applies to this header file only"。https://raw.githubusercontent.com/FFmpeg/nv-codec-headers/master/include/ffnvcodec/nvEncodeAPI.h | 头文件本身可以。NVIDIA Video Codec SDK 整包的许可协议（含示例代码）**未读到**（developer.nvidia.com 的协议页 404），不要把 SDK 整包带进仓库，只带 MIT 头文件副本 | 运行时库 `nvEncodeAPI64.dll` / `libnvidia-encode.so` 随显卡驱动安装；`dynlink_loader.h` 用 `LoadLibrary`/`dlopen` 动态加载（第 56、72、77 行）。头文件 README 标注对应 SDK 13.1.15，最低驱动 610.0 |
| AMD AMF | MIT。https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/AMF/master/LICENSE.txt 写 "MIT license Copyright (c) 2016 Advanced Micro Devices"；另有声明 "AMD does not provide a license or sublicense to any Intellectual Property Rights relating to any standards, including ... AVC/H.264; HEVC/H.265 ... you will pay any royalties due" | 可以（MIT）；H.264 专利费由使用方自负 | `AMFFactory.cpp` 用 `LoadLibrary`/`amf_get_proc_address` 加载驱动里的 `AMF_DLL_NAME`（即 amfrt64.dll），再调 `AMFInit`；运行时来自显卡驱动，不用随包带 |
| Intel oneVPL (libvpl) | MIT。https://github.com/intel/libvpl 页面 "This project is licensed under the MIT License."；仓库 LICENSE 首行 "MIT License Copyright (c) 2020 Intel Corporation" | 可以 | 包里带 Dispatcher（静态或动态），它在运行时去找 GPU 运行时（oneVPL GPU runtime 或旧 Media SDK）；两个运行时在驱动里。同页说：Media SDK 面向旧显卡（Broadwell 起），VPL GPU Runtime 面向 Iris Xe 及以后；TGL/DG1 两者都有时 Dispatcher 优先 VPL（除非要求 D3D9）|
| libva（VA-API 头文件）| MIT 风格（va.h 文件头 "Permission is hereby granted, free of charge ... to deal in the Software without restriction"）| 可以 | Linux 运行时 `libva.so` 来自发行版，动态加载或链接均可 |
| Apple VideoToolbox / ScreenCaptureKit | 系统框架 | 系统 API，随 OS | - |
| Windows Media Foundation | 系统 API | 同上 | - |
| FFmpeg | 默认 LGPL-2.1+；https://ffmpeg.org/legal.html 的 LGPL 清单："Use dynamic linking (on windows, this means linking to dlls) for linking with FFmpeg libraries." 和 "Compile FFmpeg without "--enable-gpl" and without "--enable-nonfree"."；"If those parts get used the GPL applies to all of FFmpeg."；"Make sure your program is not using any GPL libraries (notably libx264)." | Apache-2.0 项目可以**动态链接**一份 LGPL 构建的 FFmpeg（不开 gpl/nonfree，不含 libx264/libx265）。要保留用户替换库的能力、附 LGPL 全文、声明使用。**iOS 上 LGPL 动态链接是否可行：未核实**（App Store 分发时静态链接和重新链接要求的冲突，需要法务意见）。该页没有提 nvenc/amf/libvpl 是否触发 nonfree，**这几项未核实**，需读 FFmpeg configure 或 --enable-nonfree 的说明 | 自己编译并随安装包带 DLL/dylib，不依赖系统 FFmpeg |
| OpenH264（Cisco）| 源码和 Cisco 编的二进制都是 BSD-2。https://www.openh264.org/faq.html：二进制 "is licensed under Cisco's AVC/H.264 Patent Portfolio License from MPEG LA, at no cost to you, provided the requirements and conditions listed ... are met"；"products or projects that utilize it must download it at the time the product or project is installed on the user's computer or device"；"a team can choose to use the source code, in which case the team is responsible for paying all applicable license fees"；"Cisco is providing no such guarantee [for other patent holders]"；二进制许可全文要在应用的"license/about"处原样展示 | 代码许可没问题（BSD）。**专利**：要 Cisco 出专利费，必须在安装时从 Cisco 下载其官方二进制，不能自己编译后随包分发，也不能把二进制静态进主程序。FAQ 同时说 iOS 不允许安装后再下模块，所以 iOS 上没有这条路，要走 VideoToolbox | 在安装/首次运行时下载 Cisco 二进制，`dlopen`/`LoadLibrary` 加载 |

补充：
- Sunshine 在 Windows 上 NVENC 自己直调、AMF/QSV 经 FFmpeg（见 1.1），这种结构可以参考，但源码不能抄（GPL-3）。
- RustDesk 的 hwcodec 子库（rustdesk-org/hwcodec）：LICENSE 路径 404，Cargo.toml 无 license 字段，**许可证未核实**，不要引入。
- H.264 专利：除 OpenH264 的 Cisco 二进制路径外，使用系统硬件编码（NVENC/AMF/QSV/VideoToolbox/MF）时专利费通常由芯片/OS 厂商缴纳。这是常识，没找到一手原文（未核实）。AMF 的 LICENSE 里 "you will pay any royalties" 的措辞是反面证据，需要法务判断。

## 3. 低延迟远程桌面编码参数的共识

下表每一行都有至少两家一手证据支撑。

| 项 | 做法 | 证据 |
|---|---|---|
| B 帧 | 全部关闭 | Sunshine `ctx->max_b_frames = 0`、NVENC `frameIntervalP = 1`；MF 文档默认 B 帧数 0；VideoToolbox 低延迟码控"No frame reordering (B frame) or looking ahead"；Chromium Qualcomm 路径强制 B 帧 0 |
| 帧重排/lookahead | 关闭，`zerolatency` | Sunshine NVENC `zerolatency=1` 并关 lookahead；FFmpeg `zerolatency` 选项文档 "no reordering delay" |
| 码率控制 | CBR 为主 | Sunshine NVENC `NV_ENC_PARAMS_RC_CBR`；RustDesk `RC_CBR`；Chromium remoting `ConstantBitrate`；OBS 默认 CBR；MF 平台编码器默认 VBR（微软文档），所以必须显式设 |
| VBV/缓冲 | 约一帧的缓冲 | Sunshine NVENC `vbvBufferSize = bitrate / framerate`；Sunshine FFmpeg `rc_buffer_size = bitrate / framerate`（软件 HEVC 或多切片用 1.5 倍，注释 "libx264 can severely degrade quality if the buffer is too small"）；OBS AMF 延迟约束 VBR 用 `(bitrate/framerate)*1.1`；QSV/VAAPI 标 `NO_RC_BUF_LIMIT`，即不设 |
| GOP | 无限 GOP，只在需要时出 IDR | Sunshine NVENC `gopLength = idrPeriod = NVENC_INFINITE_GOPLENGTH`；VAAPI `idr_interval=INT_MAX`；RustDesk `DEFAULT_GOP = i32::MAX`；VideoToolbox 低延迟码控 "Infinite GOP (all P frames following the beginning IDR)" |
| 参数集重复 | 每个关键帧带 SPS/PPS | Sunshine `repeatSPSPPS = 1`（AV1 `repeatSeqHdr`）；AMF `header_insertion_mode=idr` |
| 帧丢弃 | 编码器内部丢帧关闭，丢不丢由传输层决定 | Sunshine AMF `frame_skipping=0`；WebRTC OpenH264 `bEnableFrameSkip` 由上层 `frame_dropping_on` 决定 |
| 切片 | 默认 1 个，按客户端要求才多 | Sunshine 硬件编码 `slices = config.slicesPerFrame`（由客户端协商）；RustDesk `slices=1`；MF 文档说默认用切片并行换低延迟 |
| 异步深度 | 1 | RustDesk 对 QSV/VAAPI 设 `async_depth=1`；Sunshine QSV/VAAPI 同；FFmpeg 文档说深度越高延迟越高；NVENC 官方建议 Windows 上异步模式（"NVIDIA recommends using the asynchronous mode"），DX 应用 `enableEncodeAsync=1, doNotWait=0` |
| 预设/调优 | NVENC tuning=ULTRA_LOW_LATENCY，预设按档位 P1–P7 | Sunshine `tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY`；RustDesk 用 p1/p4 |
| 空间 AQ/二遍编码 | 默认关，由用户选 | Sunshine `enableAQ = config.adaptive_quantization`，multiPass 可选 |
| 关键帧策略 | 按需 IDR 为主，不定期关键帧 | Sunshine `idr_events` + `request_idr_frame()`；Moonlight iOS 在显示层失败时重建并 `return DR_NEED_IDR` / `LiRequestIdrFrame()` |
| 丢包恢复 | 分级：参考帧失效 > IDR；也有帧内刷新 | Sunshine 只有 Windows NVENC 声明 `REF_FRAMES_INVALIDATION`（其余编码器收到失效请求就改 IDR）；NVENC `NvEncInvalidateRefFrames`，按 `inputTimeStamp` 标识帧，没有可用参考时当前帧编成 intra；Sunshine 对失效范围 ≥ `ref_frames_in_dpb` 时直接 IDR |
| 帧内刷新 | NVENC/QSV/VA-API 都支持，Sunshine 只在客户端要求且硬件支持时打开 | Sunshine `enableIntraRefresh=1`、`intraRefreshPeriod=300`、`intraRefreshCnt=299`、开 recovery point SEI；NVENC 指南 8.13 "when using infinite GOP length, no intra frames are transmitted and intra refresh may be a useful mechanism for recovery"；QSV `IntRefType` 有垂直/水平/切片三种（mfxstructures.h 第 1600–1633 行）；VA-API `VAConfigAttribEncIntraRefresh` 有 `ROLLING_COLUMN/ROW/ADAPTIVE/CYCLIC`（va.h 第 1326–1343 行）；Moonlight 协议里有 `CAPABILITY_INTRA_REFRESH`（moonlight-common-c Limelight.h 第 285 行）|
| 长期参考帧（LTR） | 厂商都有接口，但主流开源项目没人用于远程桌面（未核实"没人用"，只是我读的几份里没看到）| NVENC：`enableLTR`/`ltrMarkFrame`/`ltrUseFrameBitmap`，"The current SDK does not support LTR when the encoding session is configured with B frames"；AMF：`MAX_LTR_FRAMES`、`MARK_CURRENT_WITH_LTR_INDEX`、`FORCE_LTR_REFERENCE_BITFIELD`，"LTR and B-frames ... cannot be used simultaneously"，关键帧会清空所有 LTR 槽，引用不存在的 LTR "will generate an Intra only frame"；MF：`AVEncVideoLTRBufferControl`（静态，微软文档 H.264 编码器页列为"可选，支持则测试"）；Chromium 注释说 Intel 的 DisplayRemoting 场景会"enable long term reference for screen contents" |
| 屏幕内容工具 | H.264 没有专门工具；靠场景提示 + AV1 SCC | MF `eAVScenarioInfo_DisplayRemoting`（Sunshine、Chromium 都用；FFmpeg `h264_mf scenario=display_remoting`）；QSV `MFX_SCENARIO_DISPLAY_REMOTING`、`MFX_CONTENT_NON_VIDEO_SCREEN`（mfxstructures.h 第 1795、1809 行）；OpenH264 `SCREEN_CONTENT_REAL_TIME` |
| 码率运行中变更 | 见第 4 节 | |
| 单帧大小上限 | 仅 QSV 有 | `mfxExtCodingOption2::MaxFrameSize`："The encoder tries to keep frame size below specified limit but minor overshoots are possible"，"valid during initialization and runtime"，建议 I 帧 5–10 倍目标帧大小、P/B 帧 2–4 倍；VA-API 有 `VAConfigAttribMaxFrameSize`（va.h 第 925 行）；NVENC 头文件里 grep `frameSizeCap|maxFrameSize` 无命中（我只 grep 了头文件）|

Sunshine 对解码端的反向约束（编码端要配合）：Moonlight 在没有"参考帧失效"能力时改写 H.264 SPS：`num_ref_frames=1`、`vui.max_dec_frame_buffering=1`，如果没有 `bitstream_restriction_flag` 就补上并设 `num_reorder_frames=0`。
注释："Fixup the SPS to what OS X needs to use hardware acceleration. This is also critical for decoding latency on the Pi 2."；"NVENC doesn't seem to add bitstream restrictions anymore (591.59), so we need to add them ourselves"。
出处：https://raw.githubusercontent.com/moonlight-stream/moonlight-qt/master/app/streaming/video/ffmpeg.cpp 第 1783–1812 行。说明 SPS 里的重排/DPB 字段直接决定解码端的缓冲深度，自己控制两端时应该在编码端就写对。

## 4. 帧率/码率交给编码器的接口

### 4.1 "传输层指挥编码器"的接口形态（各家对照）

| 动作 | WebRTC `VideoEncoder` | Chromium `VideoEncodeAccelerator` | NVENC | AMF | QSV | MF | VideoToolbox | VA-API |
|---|---|---|---|---|---|---|---|---|
| 改目标码率 | `SetRates()`（瞬时值）| `RequestEncodingParametersChange(Bitrate, fps)` | `NvEncReconfigureEncoder`（需 `NV_ENC_CAPS_SUPPORT_DYN_BITRATE_CHANGE`）| 改动态属性 `TARGET_BITRATE` 等，下一次 `SubmitInput` 生效 | `MFXVideoENCODE_Reset`（未读原文，未核实）；FFmpeg 路径 RustDesk 直接改 `bit_rate`/`rc_max_rate` | `CODECAPI_AVEncCommonMeanBitRate` 运行中可设（Win8+），"If the bit rate changes, the encoder uses adaptive encoding" | `kVTCompressionPropertyKey_AverageBitRate`/`DataRateLimits`，可在会话中 `VTSessionSetProperty`（未核实可动态改的清单）| 提交 `VAEncMiscParameterTypeRateControl` 缓冲 |
| 改帧率 | `SetRates` 里的 `framerate_fps` | 同上 | `NvEncReconfigureEncoder` 可改 | 动态属性 `FRAMERATE`（未核实）| 同上 | `MF_MT_FRAME_RATE`，改需要重设类型（未核实）| `ExpectedFrameRate`（未核实）| - |
| 改分辨率 | 重建 | 重建 | `Reconfigure` 可改，但需要建会话时设 `maxEncodeWidth/Height`，且一般要 IDR | 重建 | 重建/Reset | 重建（"Some software rate controllers require reset on resolution change"）| 重建 | 重建 |
| 要关键帧 | `frame_types` | `Encode(frame, force_keyframe)` | `NV_ENC_PIC_FLAG_FORCEIDR` / `FORCEINTRA` | `FORCE_PICTURE_TYPE`（未读）| `MFX_FRAMETYPE_xIDR`（未读）| `CODECAPI_AVEncVideoForceKeyFrame` | `kVTEncodeFrameOptionKey_ForceKeyFrame`，官方文档："it may not be possible for the encoder to accommodate all requests" | 提交 IDR 帧类型 |
| 参考帧失效 | 无（OnLossNotification 已移除，video_encoder.h 注释 "Deprecated: Loss notification (goog-lntf) support is removed; never called by WebRTC"）| 无 | `NvEncInvalidateRefFrames` | LTR 机制间接实现 | 未核实 | LTR 可选 | 无（Sunshine 的 `vt_encode_session_t::invalidate_ref_frames` 直接 `force_idr`）| 无 |
| 编码器回报丢帧 | `OnFrameDropped` | 无 | 无专用回调；Sunshine 靠 dummy 帧/计数 | AMF 有 `frame_skipping` 开关 | `MFX_WRN_DEVICE_BUSY`（未读）| 输出为空时自己判断 | 回调里 `infoFlags` 有 `kVTEncodeInfo_FrameDropped`（未读，未核实）| - |

Apple 文档（本次读到的）：
- `EnableLowLatencyRateControl`（编码器规格，创建会话时指定）："Low latency rate control enforces: Infinite GOP (all P frames following the beginning IDR). No frame reordering (B frame) or looking ahead. Only High profiles. The encoder sets the levels automatically. Temporal Layer structure."
  https://developer.apple.com/documentation/videotoolbox/kvtvideoencoderspecification_enablelowlatencyratecontrol
- `AverageBitRate`：“not a hard limit; the bit rate may peak above this”；"bit rate settings only have an effect when timing information is provided for source frames"。
- `DataRateLimits`：以"字节 + 秒"成对的硬限制，"requires that the total size of compressed data for any contiguous segment of that duration (in decode time) not exceed the data size"。
- `MaxFrameDelayCount`：编码器可以持有的最多帧数，用来限制延迟。
- `RealTime`、`AllowFrameReordering`、`ConstantBitRate`、`PrioritizeEncodingSpeedOverQuality`、`MaxAllowedFrameQP` 都存在（文档摘要已读，ConstantBitRate 的系统版本要求没有读到，未核实）。

### 4.2 编码器做不到时怎么回报：各家做法

1. **WebRTC**：`OnFrameDropped(rtp_timestamp, spatial_id, is_end_of_temporal_unit)` 回调告诉上层"这帧被丢了"；`EncodedImageCallback::Result::drop_next_frame` 让上层通知编码器下一帧丢掉；`has_trusted_rate_controller=false`（默认）时，WebRTC 自己用 media optimization 丢帧器兜底（video_encoder.h 注释）。
2. **Chromium MF 编码器**：硬件码控不稳就在编码器外面套一层软件码控，自己逐帧给 QP、自己决定丢帧（第 2005–2025 行）；对已知有问题的厂商关闭（AMD/ARM H.264）。
3. **Chromium remoting**：`EncoderBitrateFilter` 在编码器前面给带宽估计做平滑和下限，目标变化小于 33% 就不下发，减少编码器被频繁改参数。
4. **Sunshine**：没有运行中改码率，因此没有"回报"一说；编码器忙时 dummy 帧维持心跳（video.cpp 第 2751–2800 行注释 "the blank dummy frame just to let Moonlight know that we're alive"）；图像池满时 sleep 重试（第 1910 行）。
5. **OBS**：能改码率的 NVENC 路径每次 `forceIDR=1` 付出一个关键帧的代价，AMF 的 CBR 路径不需要 flush（texture-amf.cpp 注释说 flush 会引起"unaligned IDRs in an encoder group"）。

可核对的事实：QSV 要靠 `bit_rate--` 与 `rc_max_rate` 不等来"伪装 CBR"（Sunshine `CBR_WITH_VBR`，RustDesk `c->bit_rate--; // cbr with vbr`），运行中改码率时 RustDesk 也要同步改 `rc_max_rate`（util.cpp `change_bit_rate`）。

## 5. 解码和显示端

### 5.1 Moonlight（GPL-3；参考它的结构和回退顺序）

https://raw.githubusercontent.com/moonlight-stream/moonlight-qt/master/app/streaming/video/ffmpeg.cpp
- 解码用 FFmpeg，硬件加速渲染器按平台分：Windows `D3D11VARenderer`（优先）> `DXVA2Renderer`；macOS `VTRendererFactory`（VideoToolbox，Metal 显示 `vt_metal.mm`，`CVMetalTextureCacheCreateTextureFromImage` 直接把 `CVPixelBuffer` 变 Metal 纹理）；Linux `VAAPIRenderer`、`CUDARenderer`、`DRMRenderer`、`PlVkRenderer`（libplacebo Vulkan）；最终回退 `SdlRenderer`（软件上传）。
- 选择逻辑：`tryInitializeRenderer()` 按顺序试；第 998–1001 行注释说 DXVA2 在 hwaccel 列表里排在 D3D11VA 前面，所以第一轮只试 D3D11VA，保证优先 D3D11VA。每个解码器先"测试解码一帧"（`TestMode::TestFrame`，第 700 行），成功后 `avcodec_flush_buffers` 再正式开始。
- 解码上下文：`AV_CODEC_FLAG_LOW_DELAY`；硬件解码 `thread_count = 1`，软件解码用 `FF_THREAD_SLICE` 多线程（最多 `MAX_SLICES`）。
- D3D11 显示：`DXGI_SWAP_EFFECT_FLIP_DISCARD` 交换链；有 `DXGI_FEATURE_PRESENT_ALLOW_TEARING` 就用 `Present(0, DXGI_PRESENT_ALLOW_TEARING)`；没有就在全屏独占时回退到 DXVA2；解码输出的纹理 `BindFlags |= D3D11_BIND_SHADER_RESOURCE`，直接当着色器资源采样，不拷回内存。
- VA-API 显示：优先 EGL 零拷贝，部分驱动（注释提到 "can hang in vaSyncSurface() during buffer import"）改走慢路径；`vaPutSurface()` 假设有限范围色彩，缺少 `VAEntrypointVideoProc` 的驱动会崩。
- 解码器能力位：`CAPABILITY_REFERENCE_FRAME_INVALIDATION_AVC/HEVC/AV1`、`CAPABILITY_DIRECT_SUBMIT`、`CAPABILITY_INTRA_REFRESH`、`CAPABILITY_SLICES_PER_FRAME(x)`（moonlight-common-c 的 Limelight.h 第 250–290 行）。也就是说"解码端能做什么"通过能力位在握手时告诉编码端，编码端据此选恢复方式。Limelight.h 第 254 行注释："If using this feature, the bitstream may not be patched (changing num_ref_frames or max_dec_frame_buffering)"，即参考帧失效和 SPS 改写互斥。

### 5.2 Moonlight iOS（GPL-3）

https://raw.githubusercontent.com/moonlight-stream/moonlight-ios/master/Limelight/Stream/VideoDecoderRenderer.m
- 不用 FFmpeg：直接 `CMVideoFormatDescriptionCreateFromH264ParameterSets`（或 HEVC 版）建格式描述，`CMSampleBufferCreateReady` 包码流，`[displayLayer enqueueSampleBuffer:]`，由 `AVSampleBufferDisplayLayer` 内部调用 VideoToolbox 解码并显示（第 44、447、485、585–598 行）。
- 显示节奏用 `CADisplayLink`，`preferredFrameRateRange` 设为流帧率（第 104–111 行）。
- 错误处理：每帧入队前检查 `displayLayer.status == AVQueuedSampleBufferRenderingStatusFailed`，失败就在主线程重建显示层，并返回 `DR_NEED_IDR` 请求 IDR（第 519–530 行）。
- HDR 元数据变化时也要重建格式描述并 `LiRequestIdrFrame()`（第 680 行）。
- 采样时间戳是 `CMSampleTimingInfo {kCMTimeInvalid, presentationTimeUs, kCMTimeInvalid}`。是否设 `kCMSampleAttachmentKey_DisplayImmediately`：本文件的 grep 无命中，未核实。
- Apple 对 `AVSampleBufferDisplayLayer` 的一句话描述："An object that displays compressed or uncompressed video frames."（https://developer.apple.com/documentation/avfoundation/avsamplebufferdisplaylayer）。想用 Metal 自己画，路径是 `VTDecompressionSession` → `CVPixelBuffer` → `CVMetalTextureCache`，Moonlight macOS 的 `vt_metal.mm` 就是这样做的。

### 5.3 Parsec

- "We use hardware decoding whenever possible on every platform we support."（https://parsec.app/technology）
- 另一博文说 "Parsec by default enables V-sync, but drops accumulated frames."（显示端处理，丢掉积压帧）。其余（D3D11/Metal 细节）没有公开原文，未核实。

### 5.4 RustDesk

- hwcodec README 表：Windows 全部三家解码走 D3D11（`D3D11VA`），Linux 走 VAAPI（NVIDIA 额外有 NVDEC），macOS 解码 H.265 走 RAM 路径；iOS 在 `HwCodecConfig` 里直接返回空（hwcodec.rs 中 `#[cfg(target_os = "ios")] HwCodecConfig::default()`），即 iOS 没有 hwcodec。UI 显示是 Flutter 纹理或软件，**未读，未核实**。
- 解码端也有"没找到参考帧"的标志：`util_decode::has_flag_could_not_find_ref_with_poc()`（util.cpp 第 321–325 行），解码器报告缺参考帧时上层可以据此请求关键帧。这是业内常见写法：把 FFmpeg 日志里的 "Could not find ref with POC" 转成事件。

## 6. AV1 现状

已读到的一手信息：

| 项 | 内容 | 出处 |
|---|---|---|
| NVIDIA | AV1 编码（YUV 4:2:0）只有 Ada（RTX 40 系，8th Gen）和 Blackwell（RTX 50 系）；Ampere/Turing 及更早 "NO"；矩阵页只列 4:2:0，没有 AV1 4:4:4 | https://developer.nvidia.com/video-encode-decode-gpu-support-matrix |
| NVENC AV1 功能 | NVENC 指南/搜索摘要：AV1 支持 B 帧、7 个参考帧、基于 tile 的渐进帧内刷新；头文件有 `repeatSeqHdr`、`enableBitstreamPadding`；是否有屏幕内容工具开关：**指南前 10 万字符未提到，未核实** | NVENC 编程指南 13.0；Sunshine nvenc_base.cpp |
| AMD | AMF AV1 API 有 `SCREEN_CONTENT_TOOLS`、`PALETTE_MODE`、`FORCE_INTEGER_MV`（"If true, allow enabling screen content tools ...; if false, all screen content tools are disabled"），usage 有 `TRANSCODING/LOW_LATENCY/ULTRA_LOW_LATENCY/WEBCAM/HIGH_QUALITY/LOW_LATENCY_HIGH_QUALITY`。支持 AV1 编码的具体 GPU 世代**本次没读到原文**（一般认为 RDNA3 起，未核实）| https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/AMF/master/amf/doc/AMF_Video_Encode_AV1_API.md 第 619、1505–1560 行 |
| Intel | Chromium 注释指出 Intel 的 MF 编码器在 DisplayRemoting 场景下会开 AV1 SCC 工具（palette、intra block copy），并关掉 I 帧上的 CDEF、开 LTR。Intel Arc 支持 AV1 编码的官方清单页被重定向，**未读到**（未核实）| Chromium media_foundation_video_encode_accelerator_win.cc 第 1870–1890 行 |
| Windows API | D3D12 视频编码在 Windows 11 24H2（WDDM 3.2）起扩展到 AV1（搜索摘要，未读原文）| https://learn.microsoft.com/windows-hardware/drivers/display/video-encoding-d3d12-av1 |
| Chromium | WebCodecs 的 `forceScreenContentTools` 在桌面 M121 发布，示例 `av1: { forceScreenContentTools: true }`；这是软件编码器（libaom/SVT/rav1e 等）的开关，没看到硬件相关说明 | blink-dev 意向帖 https://groups.google.com/a/chromium.org/g/blink-dev/c/BLAW7YO17jE/m/JTlF3HirBgAJ |
| Sunshine | 已有 `av1_nvenc`、`av1_qsv`、`av1_amf`、`av1_vaapi`；软件 `svtav1-params` = `keyint=-1:pred-struct=1:force-key-frames=1:mbr=0`；没有 VideoToolbox AV1；`av1_mode` 默认按编码器能力自动声明，文档警告 "AV1 is more CPU-intensive to encode" | video.cpp 第 840–1421 行；docs/configuration.md `av1_mode` |
| Moonlight | 有 AV1 参考帧失效能力位 `CAPABILITY_REFERENCE_FRAME_INVALIDATION_AV1` | Limelight.h 第 281 行 |
| Apple | VideoToolbox 的 AV1 **编码**状态未核实（只知道 Sunshine/Moonlight 没有用）；AV1 硬件解码需要 M3/A17 Pro 之后（常识，未读原文，未核实）| - |

结论层面的事实：目前实时屏幕内容 AV1 硬件编码有明确一手证据的只有 NVIDIA Ada/Blackwell（4:2:0）、AMD AMF（有 SCC 开关）和 Intel（Chromium 里已启用）。苹果没有。各家是否真的使用屏幕内容工具，需要实机验证：开关存在不等于硬件实现了对应工具（AMF 文档说"allow enabling"，不保证所有 GPU 都有）。

## 7. 各节"可以直接借鉴的做法"汇总

1. **探测 = 真编一帧并检查输出**（Sunshine `validate_config`）：检查首包是 IDR、SPS 带 bitstream_restriction（VUI）；用 1080p60 6Mbps 与限制参考帧数两套配置各试一次；记能力位而不是布尔值。RustDesk 补充：探测放进子进程、结果缓存，按 GPU 签名失效。
2. **按 GPU 厂商 ID 选编码器**，不要在 Intel 核显上试 NVENC 之类（Sunshine `is_codec_supported`）。多 GPU 时用显示器所连的那块（OBS 与 Parsec 都强调；Parsec 支持页说法未读原文）。
3. **零拷贝链路**：DXGI 捕获纹理 → 共享 NT 句柄 → 编码设备打开 → 着色器 RGB→NV12 直接写入编码输入纹理（Sunshine 的 render pass；OBS `NvEncRegisterResource`）。Chromium MFT 路径要注意 NVIDIA MFT 需要 `Flush()`、纹理要带 `D3D11_BIND_VIDEO_ENCODER`、设备 LUID 必须一致。
4. **能力位握手**：解码端把"能做参考帧失效/帧内刷新/切片数"告诉编码端（Moonlight 的 `CAPABILITY_*`），编码端据此选择恢复手段；做不到就明确降级到 IDR 并写日志（Sunshine 日志 "Encoder doesn't support reference frame invalidation"）。
5. **SPS 约束写对**：`num_ref_frames=1`、`max_dec_frame_buffering=1`、`num_reorder_frames=0`，让硬件解码器不多缓冲（Moonlight 的 SPS 改写）。
6. **码控不可信时在上面套一层**：Chromium 的软件码控（逐帧 QP + 丢帧）、WebRTC 的 `has_trusted_rate_controller=false` 丢帧器、Chromium remoting 的下限和 33% 去抖。
7. **码率变化按能力做**：NVENC 先查 `DYN_BITRATE_CHANGE` 再 `Reconfigure`（OBS）；AMF 直接改属性且 CBR 不 flush；MF 用 `CODECAPI_AVEncCommonMeanBitRate`；FFmpeg 路径 QSV 要同时改 `rc_max_rate`。
8. **屏幕内容提示**：MF 用 `eAVScenarioInfo_DisplayRemoting`（Intel 验证过，Chromium 注释说其它厂商没确认）；QSV 用 `MFX_SCENARIO_DISPLAY_REMOTING`；OpenH264 用 `SCREEN_CONTENT_REAL_TIME`。
9. **厂商黑名单是写在代码里的**：Chromium MF 编码器里 AMD/ARM H.264 禁用 SW BRC、Qualcomm 强制 CameraRecord + B 帧 0；Sunshine MF 编码器标 `FIXED_GOP_SIZE`（不支持按需 IDR）；RustDesk 避开 NVIDIA 的 CUDA 路径。都带 bug 编号或注释，可以逐条核对我们遇到的 Intel QSV 卡、关键帧被丢。

## 8. 各家差异一览

| 项 | Sunshine | RustDesk | Chromium remoting | OBS | Parsec |
|---|---|---|---|---|---|
| 编码器接入 | NVENC 直调 SDK；其余走 FFmpeg | 全走 FFmpeg（hwcodec）| 系统抽象 `VideoEncodeAccelerator`（Windows 下是 MF）| 各家 SDK 直调（NVENC、AMF、QSV）+ FFmpeg 回退 | 闭源，公开说法是硬件编码 + GPU 零拷贝 |
| 选择方式 | 顺序探测，首个通过者 | 子进程探测，缓存，GPU 签名 | 厂商能力表 + 工作区开关 | 用户选择 | 未公开 |
| 运行中改码率 | 无 | 改 `bit_rate`，QSV 同步 `rc_max_rate` | `RequestEncodingParametersChange` | NVENC `Reconfigure`+`forceIDR`；AMF 改属性 | "动态码率"，实现未公开 |
| 恢复手段 | NVENC 参考帧失效，其他 IDR | IDR | 关键帧 | - | 未公开 |
| 许可证 | GPL-3 | AGPL-3 | BSD 系 | GPL-2+ | 闭源 |

## 9. 没查到或没核实的

1. NVIDIA Video Codec SDK 许可协议原文（页面 404）：只能确认 `nvEncodeAPI.h` 副本是 MIT 风格，不能确认整包 SDK 的再分发条款。
2. FFmpeg 开 `--enable-nvenc`/`--enable-amf`/`--enable-libvpl` 是否触发 nonfree：legal.html 没写，需读 configure 脚本和 FFmpeg 文档。
3. LGPL 动态链接 FFmpeg 在 iOS 上是否可行（App Store 的静态链接限制）：未查，建议问法务。
4. RustDesk hwcodec 子库的许可证：LICENSE 路径 404，Cargo.toml 没有 license 字段。
5. FFmpeg 的 nvenc/qsv/amf 包装在运行时改 `AVCodecContext.bit_rate` 是否被采纳：没读 libavcodec 源码。RustDesk 的做法是"直接改"，效果未验证。
6. QSV、AMF、VideoToolbox、VA-API 各自"运行中改码率/帧率"的具体 API 细节，第 4.1 节标未核实的格子：只读到 AMF 文档里"动态属性在下一次 Submit 前刷新"、MF 文档、VideoToolbox 属性摘要。
7. Parsec 的具体码控、关键帧、丢包恢复实现：没有公开原文。H.265 支持和"显示器必须接在该 GPU"的说法来自搜索摘要，没读原文。
8. 各家实际默认 GOP、码率上限与分辨率的对应表（RustDesk 的 `base_bitrate`、Sunshine 的客户端默认值）：没有逐一列出。
9. AMD 哪一代 GPU 起有 AV1 编码、Intel Arc/Core Ultra 的 AV1 编码清单：官方页没读到。
10. 屏幕内容 AV1 工具在各家硬件上的实际效果（palette、IBC 是否真的由硬件实现，对文字清晰度的收益）：只有 API 开关和 Chromium 注释，没有实测数据。
11. Apple VideoToolbox 的 AV1 编码支持和 `ConstantBitRate` 的系统版本：未读原文。
12. Chromium 各文件的许可证头（BSD-3）：没逐个读。
13. iOS 端 `AVSampleBufferDisplayLayer` 和 Metal 自绘在延迟上的实测对比：没有找到一手数据。
14. LTR 在 Windows MFT 上的真实可用度：微软文档只说"可选，若支持则测试"；没有厂商清单。
15. 各家在多显卡/混合显卡（笔电核显+独显）场景下的编码器选择和纹理跨设备拷贝：Sunshine 的共享句柄路径读到了，但具体跨 GPU 时的拷贝代价和失败回退没有读。
