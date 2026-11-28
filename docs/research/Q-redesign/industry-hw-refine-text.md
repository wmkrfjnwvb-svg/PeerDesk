# 硬件编码器的补清晰行为，和文字清晰度怎么量

日期：2026-11-28。方法：读官方 API 文档、头文件和源码原文，没有运行任何代码，没有做实验。
接着 `industry-refine.md`（业内补清晰做法）和 `refine-plan.md`（R1 一步补）写，不重复前者已有的 Chromium / TurboVNC / WebRTC / RustDesk 概述，只补硬件后端和文字指标。

标注约定：
- **【原文】**：文档、头文件、源码、论文正文明确写了。
- **【推测】**：我从原文推出来、原文没写的。
- **【二手】**：只读到搜索结果摘要、论坛帖子或别人的转述，没读到原文。
- 源码链接指向默认分支当天内容，没有锁定提交；头文件版本在各处注明。

---

## 0. 结论速览

A（硬件编码器）：
1. **没有一家文档写了“静止宏块跳过规则”。** 查了 VideoToolbox、Media Foundation、NVENC、AMF、oneVPL（QSV）、VA-API 的文档和头文件，都没有 openh264 那种“参考 QP > 26 且差 > 5 才重编”的硬规则。这是“没写”，不是“没有”：硬件内部的 skip 判决是不是随 QP 变，文档不讲，只能实机量。
2. **能逐帧指定 QP 的路径是有的，但条件各不相同**：
   - VA-API：CQP 下每帧 QP 本来就由应用写（`slice_qp_delta`），最干净；
   - VideoToolbox：`kVTEncodeFrameOptionKey_BaseFrameQP`（macOS 12+），但要编码器声明支持，并且一旦用就“每帧都要设”，等于整个会话自己做码控；
   - Media Foundation：`CODECAPI_AVEncVideoEncodeQP` 在“质量”码控模式下直接指定 QP，但文档写的范围是 16–51（微软自带软件编码器），硬件 MFT 要查；
   - NVENC：CQP 下改 constQP 要走 reconfigure，论坛里有人实测会逼出 IDR（不是官方说明）；per-MB 的 QP delta map 是逐帧的，是更可行的路。
3. **按区域控制的接口，硬件端基本都有**：NVENC 的 `qpDeltaMap`（H.264 每宏块，HEVC 每 CTB，AV1 每 SB）、Media Foundation 的 `MFSampleExtension_ROIRectangle`（可选）、VA-API 的 `VAEncMiscParameterBufferROI`、AMF 的 ROI 重要度图、QSV 的 `mfxExtMBQP` / `mfxExtEncoderROI`。**VideoToolbox 的头文件里没找到任何 ROI / QP map。**
4. **和 libvpx active map 最接近的东西**：VA-API 的 `VAEncMacroblockDisableSkipMapBufferType`（每宏块强制非 skip）和 QSV 的 `mfxExtMBDisableSkipMap`，以及 `VAEncMiscParameterBufferDirtyRect`（告诉编码器哪里变了）。这几个是 Intel 系接口，**别的驱动支不支持文档没写**。
5. **业内的硬件路径基本不做“逐帧降 QP”**：Chromium 远程桌面的 H.264 GPU 编码器把输出的量化值固定填 0，所以 top-off 条件永远不成立；RustDesk 对硬件编码器是重复送最后一帧，按后端查表定次数；Sunshine 只有“最低帧率”。没有一家公开做“一步补到目标 QP”。

B（文字清晰度）：
1. **屏幕内容编码的标准评价用整帧 PSNR 的 BD-rate**（JCT-VC 征集文件 + SCC 通用测试条件），**没有专门针对文字区域的客观指标**；主观测试用 A/B 并排、-4 到 +4 打分，对象是整段视频。
2. **学界的文字区域指标是 SSIM 一脉**：SIQAD 数据库（980 张）上，整体相关性 SSIM 明显好于 PSNR 和 MS-SSIM，VIF 最好；专为屏幕内容设计的质量分数（文字用小窗口、按信息量加权）更好。但**同一种失真内的比较（只变 QP），PSNR 也不差**。
3. **OCR 当指标：没找到针对 H.264/HEVC 的 QP 扫描研究。** 只有间接证据：一篇 3DGS 屏幕基准里 PSNR 高 0.08 dB 的方法 OCR 反而低 12.7 个百分点；一篇 2026 年的 TIP 论文研究 VVC 下“字符变成别的字符”的失真。
4. **没有找到“文字在 QP 多少以下肉眼看不出差别”的结论**，更没有具体到编码器和分辨率的。最近的一条是 HEVC 4:4:4 屏幕内容上做了感知量化后，QP 22 全员判“不可察觉”（4 个人，样本很少，不是普通 QP 22）。**所以 QP_text 必须自己量。**

---

## 1. A 组：硬件编码器在“画面没变、降低 QP 再编一帧”时的行为

先说读不到的：**没有任何一家的文档回答“静止画面降 QP 重编，编码器会不会跳过”**。下面逐家列出的是“能控制什么”，跳过行为只能靠实机测。

### 1.1 VideoToolbox（macOS）

读到的原文：Apple SDK 头文件 `VTCompressionProperties.h`（iPhoneOS 17.5 SDK 的镜像，Apple 自己的注释；developer.apple.com 网页是 JS 渲染，WebFetch 读不到正文，所以用头文件）。

- **每帧 QP**【原文】：`kVTEncodeFrameOptionKey_BaseFrameQP`（macOS 12.0+）。
  - “用它强制当前帧的 QP”；用前要查 `kVTCompressionPropertyKey_SupportsBaseFrameQP`，返回 true 才支持；不支持的编码器会报错或忽略。
  - 设了它就**关掉标准码控**：`AverageBitRate`、`DataRateLimits`、`Quality`、`MaxAllowedFrameQP`、`MinAllowedFrameQP` 都被忽略。
  - “如果用，**会话里每一帧都要设**”；QP 是帧级，宏块级 QP 仍可能在帧内被调制；用了就**没有码控导致的丢帧**。
  - 原文劝退：“只在你对画质有具体要求、熟悉帧 QP、并且愿意承担全部码控责任时才用，否则强烈建议依赖编码器的码控。”
- **QP 范围限制**【原文】：`MaxAllowedFrameQP`（macOS 12+）、`MinAllowedFrameQP`（macOS 13+）。注释：并非所有编码器、所有模式都支持，不支持返回 `kVTPropertyNotSupportedErr`；**为了满足上限，编码器可能丢帧**。
- **只改码率**：`AverageBitRate`（软限制，可以超或欠）、`DataRateLimits`（硬上限，时间窗口）、`ConstantBitRate`（macOS 13+，注释说是给老式 CDN 用的，“不适合一般流媒体”）。`kVTCompressionPropertyKey_Quality` 0–1：“有些编码器（如 JPEG）用”，没写 H.264 硬件是否响应。
- **强制关键帧**【原文】：`kVTEncodeFrameOptionKey_ForceKeyFrame`，“编码器未必都能满足”。
- **低延迟码控**【原文】：`kVTVideoEncoderSpecification_EnableLowLatencyRateControl`（macOS 11.3+），强制无限 GOP、无 B 帧、只用 High profile、时间分层。
- **区域控制**：头文件里没找到 ROI、QP map、skip map、dirty rect 之类的键。【没查到】
- **跳过规则**：没有文档。
- **读回实际 QP**：没查到公开的输出属性。【没查到】
- FFmpeg 的 `videotoolboxenc.c` 只用了 `MinAllowedFrameQP` / `MaxAllowedFrameQP`、`ConstantBitRate`、`Quality`、`ForceKeyFrame`，没有用 `BaseFrameQP`。【源码】 [videotoolboxenc.c](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/videotoolboxenc.c)

【推测】：`BaseFrameQP` 的“每帧都要设”意味着要用它就得整个会话改成我们自己码控（我们本来也在做，但 VideoToolbox 的丢帧保护、码率硬上限就都没了）。**是否被 Apple Silicon 的 H.264 硬件编码器支持，只能运行时查 `SupportsBaseFrameQP`，没有读到哪一款支持。**

### 1.2 Media Foundation（Windows，MFT）

读到的原文：Microsoft Learn 的 H.264 编码器页、`CODECAPI_*` 各页、`MFSampleExtension_ROIRectangle` 页。**注意：这些页面描述的是微软自带的软件 H.264 编码器 MFT 加“认证硬件编码器要满足的 HCK 要求”，各家硬件 MFT（NVENC、AMF、QSV 背后的 MFT）实际支持哪些属性要运行时用 `ICodecAPI::IsSupported` / `GetParameterRange` 查。**

- **指定 QP**【原文】：[`CODECAPI_AVEncVideoEncodeQP`](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideoencodeqp)，ULONGLONG，四个 16 位字段（默认、I、P、B）；“为了跨编码器一致，**应假定编码器只看默认 QP，可以忽略 I/P/B 各自的值**”。H.264 编码器页写：范围 16–51，默认 24，“在**质量模式**（`eAVEncCommonRateControlMode_Quality`）下生效”，Windows 8 起 `AVEncCommonQuality` 可以随时改、下一帧生效，内部转成 `AVEncVideoEncodeQP`。
  - 【推测】：微软软件编码器 16 的下限，意味着它补不到我们要的 12。硬件 MFT 的范围看 `GetParameterRange`，没读到。
- **分帧型 QP**【原文】：[`CODECAPI_AVEncVideoEncodeFrameTypeQP`](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideoencodeframetypeqp)，**动态属性，编码中随时可设**；I/P/B 各自 0–51，默认 26；只有想暴露分帧型 QP 的编码器才实现，否则只实现 `EncodeQP`。HCK 里它是“可选但支持了就测”。
- **QP 上下限**：[`CODECAPI_AVEncVideoMinQP`](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder)（0–51，所有码控模式都适用；HCK 可选）；[`CODECAPI_AVEncVideoMaxQP`](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideomaxqp) **是静态属性，只能在编码开始前设**。
- **区域**【原文】：`CODECAPI_AVEncVideoROIEnabled`（非零才生效，Windows 8.1+，HCK 可选）加每个输入样本上的 [`MFSampleExtension_ROIRectangle`](https://learn.microsoft.com/en-us/windows/win32/medfound/mfsampleextension-roirectangle)：`ROI_AREA` 里的 `QPDelta`，H.264 范围 [-25, +25]，**只作用于当前这个输入样本**；区域不要求宏块对齐，编码器自己定边界。
- **码控模式**：CBR、PeakConstrainedVBR（Win 8+）、Quality、UnconstrainedVBR（默认）。`MeanBitRate` 可在编码中改，“码率变了编码器走自适应编码”。
- **读回实际 QP**【原文】：Windows 8 起，**微软编码器**在输出样本上设 `MFSampleExtension_VideoEncodeQP`（还有 `VideoEncodePictureType`）。硬件 MFT 设不设没读到。
- **强制关键帧**：`CODECAPI_AVEncVideoForceKeyFrame`（HCK 必需）。
- **跳过规则**：没有文档。
- FFmpeg 的 `mfenc.c` 只用 `AVEncCommonQuality`、`AVEncVideoEncodeQP`（取 `global_quality`）和码控模式，没有用 ROI。【源码】 [mfenc.c](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/mfenc.c)

### 1.3 NVENC（NVIDIA，直接 SDK 或 MFT 背后）

读到的原文：`nvEncodeAPI.h`（FFmpeg 的 nv-codec-headers，`NVENCAPI_MAJOR_VERSION 13`、`MINOR 1`）、[NVENC 编程指南 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)（读到第 8.17 节，后面约 4 万字符没读）。

- **每帧 QP**：
  - CQP 模式：`NV_ENC_PARAMS_RC_CONSTQP`，用 `rcParams.constQP`（`qpIntra/qpInterP/qpInterB`），指南 3.8.3：“整帧用 `constQP` 编码”。【原文】
  - 改 constQP 要走 `NvEncReconfigureEncoder`。指南 8.4 只写能动态改“码率、帧率、分辨率”，**没写 `rcParams`/`constQP`**；头文件里 `resetEncoder` 的注释：“重置码控状态和内部状态，**只应与 IDR 帧一起用**；`enablePTD` 为 1 时编码器会强制这一帧为 IDR”。【原文】
  - 论坛里有人（2019，[帖子](https://forums.developer.nvidia.com/t/change-constant-qp-value-without-generating-idr/74004)）实测：每帧 reconfigure 改 constQP，每帧都变 IDR；`forceIDR=0` 没用；要 `resetEncoder=1` 才生效。回帖建议改用每帧 QP delta map，发帖人确认可行。【二手，非官方；没有 NVIDIA 官方确认】
  - FFmpeg 的 `nvenc.c` 在 constQP 下不做运行时 reconfigure。【源码，`if (ctx->rc != NV_ENC_PARAMS_RC_CONSTQP && ctx->support_dyn_bitrate)`】
- **区域 / 逐宏块**【原文】：
  - `NV_ENC_PIC_PARAMS::qpDeltaMap`（有符号字节数组，H.264 每宏块、HEVC 每 CTB、AV1 每 SB，光栅顺序），模式由 `NV_ENC_RC_PARAMS::qpMapMode` 定：`NV_ENC_QP_MAP_DELTA` = “在码控选出的 QP 之上加这个偏移”；`NV_ENC_QP_MAP_EMPHASIS` = 当作强调等级（注释写“目前只支持 H.264”）；`NV_ENC_QP_MAP`（直接给 QP）“目前不支持”。**每帧提供，逐帧生效。**
  - 指南 8.9（强调图）：“QP 调整发生在**码控跑完之后**，所以用这个功能有 **VBV/码率违规的可能**”；“**开了 AQ（空间/时间）时不支持**”；用 `NV_ENC_CAPS_SUPPORT_EMPHASIS_LEVEL_MAP` 查支持。
  - `NV_ENC_PIC_FLAG_PREQPDELTAMINQP` / `PREQPDELTAMAXQP`：让 `minQP/maxQP` 作用在“delta map 之前的内部 QP”上。【原文】【推测】：不设这两个标志时，minQP 可能是对加完 delta 后的 QP 起作用，会把我们的负 delta 压住，需要实测。
- **QP 上下限**：`enableMinQP/minQP`、`enableMaxQP/maxQP`（`NV_ENC_RC_PARAMS`）；Sunshine 的 NVENC 路径用 CBR + 超低延迟调优 + 可选 minQP。【源码】 [nvenc_base.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/nvenc/nvenc_base.cpp)
- **读回实际 QP**【原文】：输出里有 `frameAvgQP`（帧平均 QP）。
- **强制关键帧**：`NV_ENC_PIC_FLAG_FORCEIDR` / `FORCEINTRA`。
- **跳过规则**：`NV_ENC_CONFIG_H264`、`NV_ENC_RC_PARAMS` 里没有 skip 相关字段（读到的字段列表里没有）；指南读到的部分没有静态内容 / skip 的说明。**没有文档。**
- **帧内刷新**：`enableIntraRefresh`、`intraRefreshPeriod/Cnt`，指南 8.13。这是容错用，不是补清晰。

### 1.4 VA-API（Linux）

读到的原文：libva 的 [`va/va.h`](https://github.com/intel/libva/blob/master/va/va.h)、`va/va_enc_h264.h` 头文件；FFmpeg 的 [`vaapi_encode.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/vaapi_encode.c) / [`vaapi_encode_h264.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/vaapi_encode_h264.c)。

- **每帧 QP**【原文 + 源码】：`VA_RC_CQP` 是“恒定 QP”。H.264 的 QP 本来就是应用写进 PPS/slice 头的：`VAEncPictureParameterBufferH264.pic_init_qp` 加 `VAEncSliceParameterBufferH264.slice_qp_delta`；FFmpeg 为 I/P/B 分别算 `slice_qp_delta = fixed_qp_x - (pic_init_qp_minus26 + 26)`。**每帧每片都能改，没有“重配置”这一步，所以也没有 NVENC 那种 IDR 问题。**驱动是否支持 CQP 要查 `VAConfigAttribRateControl`（FFmpeg 查不到时的兜底是假设只有 CQP）。
- **区域**【原文】：[`VAEncMiscParameterBufferROI`](https://github.com/intel/libva/blob/master/va/va.h)：每帧发、只作用于当前帧；`VAConfigAttribEncROI` 报告 ROI 个数上限和能力位。**CQP 下 `roi_value` 一定是 delta QP**（int8，“加在帧级 QP 上”）；非 CQP 时看 `roi_rc_qp_delta_support` / `roi_rc_priority_support`。FFmpeg 把 ROI 接进来了（`roi_value_is_qp_delta = 1`，`roi_allowed` 要求 CQP 或 `roi_rc_qp_delta_support`）。
- **跳过相关，这是各家里唯一直接写到 skip 的**【原文】：
  - `VAEncMacroblockDisableSkipMapBufferType`：“**强制指定宏块不走 skip**”，每宏块 1 字节的图；应用提供它就**覆盖** `VAEncMiscParameterEncQuality` 里的 `skipCheckDisable`。
  - `VAEncMiscParameterEncQuality.skipCheckDisable`：“关闭 ME 阶段的 skip 检查，码流变大但质量更好”；这个结构“设一次，后续帧一直有效，直到再更新”。
  - `VAEncMiscParameterBufferDirtyRect`：告诉编码器哪些区域变了，其余区域“假定与上一帧相同，编码器可以做内部优化”；每帧发。`VAConfigAttribEncDirtyRect` 报能力。
  - 【推测】：这些带 FEI、`FTQ` 的接口来自 Intel 的驱动（media-driver）；**Mesa 的 AMD 驱动和其他驱动是否实现，头文件没写，我也没读驱动源码。**
- **码率型**：CBR/VBR/ICQ/MB 级码控等。`VA_RC_ICQ`：“给定初始质量因子，按运动在帧级和宏块级调整 QP”。
- **读回 QP**：没查到统一接口。我们自己写 QP，不需要读回。
- **强制关键帧**：应用自己决定帧类型（IDR）。

### 1.5 顺带读到的两个后端（不在要求里，但会影响 Windows 的选择）

**AMD AMF**（直接 SDK，不走 MFT）【原文】：[AMF 编码文档](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_Encode_API.md)。
- `QP_I`/`QP_P`/`QP_B`（0–51，“只在 CQP 下可用”）属于动态属性；“动态属性的改动在下一次 `Submit()` 之前刷进编码器”；每帧属性可以设在传入的 `AMFSurface` 上（例如强制 IDR）。
- ROI：每个 16×16 宏块的重要度 0–10（`AMF_SURFACE_GRAY32`，系统内存），挂在输入帧上逐帧生效，“在基础码控决定之上再调整码字分配”。`AMF_VIDEO_ENCODER_CAP_ROI` 查支持。
- 统计：`STATISTIC_FRAME_QP`（“图像里第一个宏块的 QP”）、`STATISTIC_AVERAGE_QP`、`STATISTIC_PIX_NUM_SKIP`（skip 编码的像素数）。**这是读到的唯一能读回 skip 像素数的接口**，可直接用来判断“降 QP 后有没有被跳过”。

**Intel QSV（oneVPL）**【原文】：[oneVPL 编码结构](https://intel.github.io/libvpl/latest/API_ref/VPL_structs_encode.html)。
- `mfxEncodeCtrl.QP`：“非零时覆盖当前帧在恒定 QP 模式下的全局 QP”（每帧）。
- `mfxExtCodingOption3.EnableMBQP`：每宏块 QP 控制，“码控必须是 CQP”，只能在初始化时开；`mfxExtMBQP` 给每宏块 QP。
- `mfxExtMBDisableSkipMap`（`MBDisableSkipMap` 初始化时开）：“强制指定宏块非 skip”。
- `mfxExtEncoderROI`：`DeltaQP` 绝对值 -51 到 51，加在宏块 QP 上。

【推测】：Windows 上经 MFT 走 Intel/AMD 硬件时，上面这些高级接口多半拿不到（MFT 只暴露 `ICodecAPI` 里那一小撮），要用就得绕开 MFT 直接接 AMF / oneVPL / NVENC SDK。这条没找到文档证实，只是 MFT 属性表里没有对应项。

### 1.6 问题 A1–A3 的汇总

| 后端 | 有没有 skip 规则 | 能不能强制重编 | 每帧 QP | 区域控制 |
|---|---|---|---|---|
| VideoToolbox | 无文档 | 只有 `ForceKeyFrame`；没有“强制非 skip” | `BaseFrameQP`，需运行时查支持，用了就每帧都设、自己码控；否则只能改码率和 Min/MaxAllowedFrameQP | 头文件里没有 |
| Media Foundation | 无文档 | `ForceKeyFrame`；ROI 负 delta 间接 | `EncodeQP`（质量模式；软件编码器 16–51）、`EncodeFrameTypeQP`（动态，可选）；硬件 MFT 要运行时查 | `ROIRectangle`（可选，delta ±25，逐样本） |
| NVENC | 无文档 | `FORCEIDR`；`qpDeltaMap` 负 delta 间接 | CQP 改 constQP 需 reconfigure，论坛实测会 IDR（二手）；`qpDeltaMap` 逐帧可用 | `qpDeltaMap`（H.264 宏块 / HEVC CTB / AV1 SB），与 AQ 互斥（强调图） |
| VA-API | 有强制非 skip 的接口（Intel 系），无“规则”文档 | `VAEncMacroblockDisableSkipMap`、`skipCheckDisable`、IDR | CQP 下每帧写 `slice_qp_delta`，最干净 | `VAEncMiscParameterBufferROI`（CQP 下是 delta QP），`DirtyRect` |
| AMF（附） | 无文档，但可读 `PIX_NUM_SKIP` | IDR | CQP 下 `QP_*` 动态 | ROI 重要度 0–10 |
| QSV（附） | 无文档 | `MBDisableSkipMap` | `mfxEncodeCtrl.QP`（CQP） | `mfxExtMBQP`、`EncoderROI` |

### 1.7 业内产品在硬件编码下怎么补清晰

只列读到原文的；Chromium 的软件编码路径、RustDesk 的总体机制已在 `industry-refine.md`，不重复。

- **Chromium Remoting 的 H.264 硬件路径不做 top-off**【源码，推断】：`webrtc_video_encoder_gpu.cc` 在输出处把 `encoded_frame->quantizer = 0`（[文件](https://github.com/chromium/chromium/blob/main/remoting/codec/webrtc_video_encoder_gpu.cc)）。而 `webrtc_video_encoder_wrapper.cc` 里 `top_off_active_ = (frame->quantizer > kMinQuantizer)`（`kMinQuantizer = 10`，见 `industry-refine.md`），所以对这条路径 `top_off_active_` 恒为假。该文件里没有出现 top-off 或 QP 控制，只有 `ConstantBitrate`。**没读 `media::VideoEncodeAccelerator` 有没有逐帧 QP 接口。**
- **RustDesk：硬件编码器照样“重复送最后一帧”，次数按后端、编码、质量档查表**【源码】 [static_refresh.rs](https://github.com/rustdesk/rustdesk/blob/master/src/server/video_service/static_refresh.rs)：H.264 NVENC (80, 50, 60, 30)、QSV (80, 70, 130, 110)、AMF (150, 150, 170, 110)、VAAPI (150, 100, 90, 60)，软编兜底 (90, 60, 60, 30)；H.265 才有 VideoToolbox 的项 (90, 90, 50, 90)；注释说这些数是实测调的（对照 1080p 截图每 10 次重复输出抓一次）。
  - 【推测】：不同硬件后端收敛要的帧数差一倍以上，说明它们的内部码控下降速度不同；**它记的是重复帧数，没有记 QP 或 PSNR，所以看不出终点画质。**
  - 它还特意不让补清晰计入“运动”（影响自适应码率），这一点和 `refine-plan.md` 第 3 节“补清晰不算动静”一致。
- **Sunshine / Moonlight**：编码循环有最低帧率（`industry-refine.md` 已写），NVENC 路径是 CBR + 超低延迟调优，配置里有 min QP 开关，**没有静态补清晰逻辑**。【源码】
- **Parsec**【二手，官方支持页面 WebFetch 返回 403，只看到搜索结果摘要】：公开的是一个用户参数 `encoder_min_qp`（“越高越糊；低于 5 多半看不出区别，且会明显增加延迟；会按网络状况自动缩放”）和“若要静止图/文字更清晰，改用 H.265、4:4:4、恒定帧率”的建议。没有找到 Parsec 对静止内容的补清晰机制说明。
- **WebRTC 屏幕共享**：H.264 路径关掉收敛判据（`industry-refine.md` 第 3 节）。硬件路径没有另外读。

### 1.8 对 A 组的整体判断

1. **“跳过规则”需要实机量，不是查文档能得到的。** 方法可以直接沿用 `data-m2b.md` 的做法：同一帧重复送入，QP 阶梯下降，记每帧字节和 PSNR；AMF 还能直接读 `PIX_NUM_SKIP`。
2. **“一步补”在不同后端的实现路径不同**：
   - VA-API、QSV（直接 SDK）：CQP 下逐帧写 QP，一步到位，没有副作用。
   - VideoToolbox：`BaseFrameQP` 可用时同理，但整个会话要自己码控；不可用时只能“重复帧 + 抬码率 / 降 MaxAllowedFrameQP”，靠码控间接下降，等于 R0。
   - Media Foundation：质量模式下改 `EncodeQP`（下一帧生效）；但“质量模式”本身是另一种码控模式，会话里要不要在 CBR 和质量模式之间切，文档没讲能否动态切换。【没查到】
   - NVENC：别靠 reconfigure 改 constQP，用 `qpDeltaMap`（逐帧、区域化）把变粗的宏块压下去，但注意它在码控之后生效，会有码率偏差，且不能和 AQ 同开。
3. **读回实际 QP 的能力不一致**：NVENC 有 `frameAvgQP`，微软编码器有 `MFSampleExtension_VideoEncodeQP`，AMF 有 `STATISTIC_*_QP`，VideoToolbox 没查到，VA-API 要靠自己写的值。现在产品对硬件编码器“按帧大小判断补完没有（`REFINE_DONE_BYTES`）”，这是合理的兜底，但有读回能力的后端应该读 QP。

---

## 2. B 组：文字清晰度怎么量

### 2.1 屏幕内容编码的评价里，文字区域用什么指标

**标准流程（JCT-VC，HEVC SCC）**
- 征集文件【原文】 [CfP-HEVC-coding-screen-content.pdf](https://www.itu.int/en/ITU-T/studygroups/com16/video/Documents/CfP-HEVC-coding-screen-content.pdf)：
  - 客观：报告“文件大小、平均码率、**PSNR**、相对锚点的 **BD-rate**、编解码时间”。没有文字区域指标，也没有 SSIM。
  - 主观：只对 AI 和 LD 配置的 720p、1080p RGB 序列做；锚点 QP 是 22、27、32、37（对应主观码率 R4–R1），**只用较低的三个码率档**（即 QP 27/32/37）；“A/B 并排分屏，观看者打 -4 到 +4 分，0 = 看不出差别”；参照是未压缩原片；可以看三遍再投票；用天真观看者；统计出 MOS、SD、CI。
- SCC 通用测试条件文件（JCTVC-U1015）**没读到原文**（ITU 网站上的会议目录只搜到会议记录）；MERL 版综述 [Xu, Joshi, Cohen, TCSVT 2016](https://www.merl.com/publications/docs/TR2015-126.pdf) 写测试序列分类 TGM（文字和图形加运动）、CC（相机拍的）、M（混合）、A（动画），4:4:4 的 RGB 和 YCbCr 都测，结果表用 **BD-rate**。综述里“visually lossless”只在需求文件的层面出现（MPEG 的需求文件要求支持到视觉无损），没有给 QP。
- **AV1/AV2 一侧**【原文】 [AOM CTC v8（2025-10）](https://aomedia.org/docs/CWG-F384o_AV2_CTC_v8.pdf)：指标列表有 PSNR、SSIM、MS-SSIM、PSNR-HVS-M、CIEDE2000、VMAF、CAMBI；B2 类是“屏幕共享的合成内容”，5 条序列（调试界面、MissionControl、手机屏幕共享等）；**B2 类的 BD-rate 单独报告，不并进总平均**（理由：避免它把总平均拉偏）；B2 用 `--tune-content=screen`。同样**没有文字区域专用指标**。（该文件名是 AV2，但内容里写的是 AOM 通用测试条件；AV1 当年的版本没读到。）

**学界的屏幕内容质量评价**
- SIQAD 数据库【原文】 [Yang 等，QoMEX 2014](https://ece.uwaterloo.ca/~z70wang/publications/QoMEX14b.pdf)：20 张原图、980 张失真图（高斯噪声、高斯模糊、运动模糊、对比度变化、JPEG、JPEG2000、分层压缩 7 种 × 7 档），11 级 ACR，**分别对整图、文字区域、图片区域打分**。结论：文字部分和整图分数的相关性比图片部分高；12 个现成指标在整库上都不够好：Overall PLCC/SROCC，VIF 最高 0.8429/0.8183，SSIM 0.7977/—，PSNR 0.6244/—（表 3，我没抄全 SROCC）。但**单一失真内部都很高**：JPEG 失真下 PSNR 的 PLCC 0.9403，SSIM 0.9231，VIF 0.9245。作者的解释：对比度变化只改文字强度、不改文字形状，而观看者更在乎形状完整。
  - 【推测】：我们的场景是“同一个编码器只变 QP”，接近“单一失真”，所以 PSNR 在排序上不会像跨失真比较那么差；PSNR 的弱点主要在“跨不同失真类型”（比如比较模糊和块效应）。
- 感知评价【原文】 [Wang 等，ICIP 2015](https://ece.uwaterloo.ca/~z70wang/publications/icip15d.pdf)（同一作者群后来发在 IEEE CG&A 2018，[Wang 等](https://ece.uwaterloo.ca/~z70wang/publications/IEEE-CGA16.pdf)）：把屏幕图分成文字块和图片块，**文字块用更小的 SSIM 窗口（11×11、σ 1.5），图片块用 17×17（σ 2.5）**，再按局部信息量加权。依据（引用的心理学结论）：人看文字时用到的视野范围比看图片小。SIQAD 整库上（PLCC/SRCC）：PSNR 0.5788/0.5539、SSIM 0.7445/0.7433、MS-SSIM 0.6047/0.5968、GMSD 0.7277/0.7168、VIF 0.8026/0.7857、他们的 QS 0.8573/0.8456；只看 JPEG 和 JPEG2000 压缩失真：PSNR 0.7271/0.7118、SSIM 0.7586/0.7532、VIF 0.7786/0.7725、QS 0.7987/0.7955。他们还把这个指标放进 HEVC-SCC 的编码器里做感知优化。
  - 两篇的数字对不上（同一个 PSNR，一个 JPEG 子集 0.94，一个 0.73），因为失真子集、评价口径（灰度图 vs 彩色、拟合方式）不同。**不能把这些数当成“PSNR 好不好”的通用结论，只能看相对排序。**
- 视频：SCVD（TIP 2020，16 条原片、800 条失真视频）和 MSU 的 [屏幕内容数据集](https://videoprocessing.github.io/screen-content-dataset)（100 条原片、1600 条失真视频，众包两两比较）。MSU 页面写“多数现有视频质量指标为自然场景设计，没有在屏幕内容上做过基准”，页面本身没给各指标的相关系数（在交互图表里）。【SCVD 只读到搜索摘要，二手】
- **PSNR / SSIM / MS-SSIM / VMAF 在文字上好不好用，小结**：
  - PSNR：全帧 MSE，不看文字边缘的局部结构；只在“同编码器只变 QP”时排序可用；**区域化**（只在文字框里算）后更合理，这是我们可以做的，因为我们渲染文字、知道框在哪。
  - SSIM 一脉：整体相关性明显好于 PSNR 和 MS-SSIM（SIQAD 整库）；MS-SSIM 反而比 SSIM 差（0.60 vs 0.74），原因没读到。
  - VMAF【原文】 [Netflix FAQ](https://github.com/Netflix/vmaf/blob/master/resource/doc/faq.md)：默认模型“预测 1080p 高清电视在客厅里的画质，所有主观数据都是缩放到 1080 后在 **3 倍屏幕高度**（60 像素/度）观看收集的”。**桌面文字是近距离、1:1 像素观看**，和这个前提不一样。【推测】VMAF 的训练内容是否含文字，我没读到训练集的原文，只能说前提不匹配；README 里还提到“图像增强会抬分”的问题（NEG 模式），对“补清晰”不算问题但要记一下。
  - 没有找到“专门针对文字的客观指标”被标准采纳。2026 年 TIP 有一篇 [Yang 等，字符变换失真（CTA）](https://signalprocessingsociety.org/publications-resources/ieee-transactions-image-processing/2026/09/character-transformation-artifact)：在 VVC 下研究“笔画变化使一个字母变成另一个字母”这种失真随 QP 的变化，并提出预测它主观严重度的方法（摘要称预测准确率 95.64%，CTA-IQA 与 MOS 的相关性高于现有方法）。**只读到摘要页，没读到 QP 数值和数据集，【二手】。**

### 2.2 OCR 识别率当文字质量指标

**没有找到“H.264/HEVC 在不同 QP 下的 OCR 识别率”的专门研究。** 找到的间接材料：

- 3DGS-SC 基准【原文摘要】 [arXiv 2609.31756](https://arxiv.org/abs/2609.31756)：10 个程序化生成的场景，有精确的文字框和文字稿，分别测整图保真度、屏幕区域保真度、OCR 可读性、边缘保留。五种方法对比里，LightGaussian 的屏幕 PSNR 只比 Mip-Splatting 高 0.08 dB，**OCR 准确率却低 12.7 个百分点**；10 对方法里有 7 对 PSNR 和 OCR 的排序相反。结论：保真度和可读性要分开评。**领域是 3D 重建，不是视频编码；只读到摘要页**。局限：只有 10 个场景，作者自己说泛化性待验证。
- 一份专利摘要（US 10638168）用 OCR 抽查关键帧、找出“文字仍可读的最小分辨率”【二手，搜索摘要】，是“OCR 当可读性探针”的思路，不是 QP。
- GDELT 的电视新闻 OCR 实验博客：压缩质量设到 10 时 OCR 准确率明显下降，缩到 500×500 没损失、400×400 有损失【二手，搜索摘要，没读原文，内容是 JPEG 帧抽取，不是视频码流】。
- Tesseract 文档【原文】 [ImproveQuality](https://tesseract-ocr.github.io/tessdoc/ImproveQuality.html)：“至少 300 dpi 效果最好”，没给字高像素数，没提压缩伪影。
- 局限（我的判断，【推测】）：
  1. OCR 是二值判断，会在某个 QP 附近突然崩，对“看起来渐渐变糊”不敏感；字体大小、字体族、OCR 引擎版本都会改变结果。
  2. OCR 对“看起来难受但能认出”的情况打满分，对“小字号本来就识别不了”的情况又全判错，**所以要用同一内容在高质量下的 OCR 结果做基线，只看相对退化**。
  3. 引擎的语言模型会“脑补”，用整词语料会高估；用随机字符串（含易混字符）能避免。
  4. 用我们自己渲染的内容有标准答案，这是我们比业内研究有利的地方。

### 2.3 “文字在 QP 多少以下肉眼看不出差别”

**没有找到按编码器和分辨率给出的这类结论。** 最近的几条：

- Prangnell 和 Sanchez，[JND 感知量化，4:4:4 屏幕内容，HEVC SCM-8.0](https://arxiv.org/pdf/1710.09919)【原文】：随机访问配置、QP 22/27/32/37，在 **QP 22** 的所有测试里，4 位观看者全部给 MOS = 5（“不可察觉”，按 ITU-R P.910，观看距离 0.75 米，“选近距离是为了保证所有潜在伪影都能看到”），尽管 PSNR 有明显下降。**要点：这是他们的感知量化器（SC-PAQ）在 QP 22 上的结果，不是普通 QP 22；人数 4；HEVC 不是 H.264；序列是官方 SCC 序列，没有单独拆文字区域。** 同组更早的 [Pixel-PAQ 论文](https://arxiv.org/pdf/1708.06417)搜索摘要写“大多数情况 QP 22 达到视觉无损，个别 QP 27”【二手，没读正文】。
- 同一论文还写了：“4:2:0 的文字内容的可读性比 4:4:4 差很多”（引用了别的文献，没给数）。【原文，转述】
- UT Austin 的 [Mittal 等，H.264 视觉无损压缩](https://live.ece.utexas.edu/publications/2012/mittal_2012_compjnl.pdf)【搜索摘要，二手】是自然视频，给的是按内容预测视觉无损**码率**，不是 QP，也不含文字。
- JCT-VC 征集文件的 QP 22–37 是锚点的取值，主观测试要的是每个码率档下和原片的差异；**没有“多少 QP 以下看不出来”的结论**。
- H.264 和 HEVC 的 QP 量纲：每 +6，量化步长翻倍，两者相同【我的背景知识，没在本次读到原文】；所以 HEVC 上的 QP 数字有参考意义，但去块滤波、变换块大小、RDOQ 不同，**不能直接换算**。

---

## 3. 对我们的建议

### 3.1 每个硬件后端的补清晰声明（给 `BackendTraits`）

先说原则：**文档没有保证的一律声明为“未知，运行时探测或实机量”，不套 openh264 的结论**（和 `refine-plan.md` 第 3 节一致）。

字段建议（命名只是示意）：`per_frame_qp`（Exact / Bias / RateOnly）、`skip_rule`（Scc{ref_floor, delta} / Unknown / Documented-none）、`refine`（补清晰方式）、`qp_readback`、`region`。

| 后端 | 每帧 QP 能不能控制 | 有没有跳过规则 | 补清晰用什么方式（建议） | 读回 QP | 区域 |
|---|---|---|---|---|---|
| openh264（现有） | 能（逐帧 QP 由我们给） | **有**：参考 QP > 26 且差 > 5（源码） | R1 一步补 | 能 | 无（规则天然只补变粗的块） |
| VA-API | **能，精确**：CQP 写 `slice_qp_delta`，无副作用 | 无文档；有强制非 skip 的接口（Intel 系，其他驱动未证实） | R1：一步 CQP 补到 q\*；若量出来降 QP 帧被跳过，用 `skipCheckDisable` / `DisableSkipMap`（先查驱动支持） | 无（用自己写的） | `ROIBuffer`（CQP 下是 delta QP）、`DirtyRect` |
| VideoToolbox | **看支持**：运行时查 `SupportsBaseFrameQP`；真→能，但整个会话要每帧设、自己码控；假→只能改码率和 Min/MaxAllowedFrameQP | 无文档，实机量 | 支持 `BaseFrameQP`：R1；不支持：R0（重复帧 + 抬码率 / `MaxAllowedFrameQP` 压下限，按字节判断补完），必要时 `ForceKeyFrame` 兜底（要单独量代价） | 没查到 | 无 |
| Media Foundation（经 MFT） | **看支持**：运行时 `IsSupported`/`GetParameterRange` 查 `EncodeQP`（质量模式）、`EncodeFrameTypeQP`（动态）；微软软件编码器下限 16 | 无文档，实机量 | `EncodeQP` 可用：R1（质量模式切换是否可动态做，要先验证）；否则 R0；ROI 可用时对变粗区域给负 delta | 微软编码器有 `VideoEncodeQP` 输出属性；硬件 MFT 未知，需探测 | `ROIRectangle`（可选，delta ±25，逐样本） |
| NVENC | **不精确**：CQP 改 constQP 需 reconfigure，论坛实测会 IDR（二手）；CBR 下用 `qpDeltaMap` 对 QP 加偏移，逐帧可用，码控之后生效，不能与 AQ 同开 | 无文档，实机量 | CBR + `qpDeltaMap` 负 delta 只压变粗的宏块（一步补，但终点 QP 由码控 + delta 决定，靠 `frameAvgQP` 验证）；先验证 `PREQPDELTAMINQP` 标志对 minQP 的影响 | 有 `frameAvgQP` | `qpDeltaMap`（H.264 宏块） |
| AMF（附，若直接接 SDK） | 能：CQP 下 `QP_*` 动态属性，下一帧生效 | 无文档，**可读 `PIX_NUM_SKIP`** 直接验证 | R1，用 `PIX_NUM_SKIP` 判断有没有被跳过 | 有 | ROI 图 0–10 |
| QSV（附，若直接接 oneVPL） | 能：CQP 下 `mfxEncodeCtrl.QP` | 无文档，有 `MBDisableSkipMap` | R1 | 无 | `MBQP`、`EncoderROI` |

附带两条工程提醒：
- 声明里“未知”的项，在 L3 实机测里用同一种方法一次性量清：同一帧重复送入，QP 阶梯 30 → 24 → 18 → 12（以及一步 30 → 12），记每帧字节、PSNR、读回 QP / skip 像素数；**和 openh264 的 desc / hold 链用同一套指标**，结论才能对比。
- Windows 上若要用到 ROI 或精确 QP，要评估“绕开 MFT 直接接各家 SDK”的代价（签名、依赖、安装体验）；MFT 路径作为通用兜底，只声明 R0。

### 3.2 QP_text 的量法

目标：找“最高的 QP，使文字相对高质量基线没有可察觉的退化”，作为补清晰下限（`refine-plan.md` 第 2 节第 3 条的 M3）。

**内容**
- 用真实字体渲染的页面（M2 那批），按我们真实用的缩放比例、系统渲染方式（ClearType / macOS 抗锯齿）、颜色（深色和浅色主题）各取，覆盖小字号（比如 9–12 px）到正常字号；**我们自己渲染，所以文字框和标准答案是已知的**，不用做文字检测。
- 色度：H.264 我们是 4:2:0，彩色抗锯齿文字的边缘有 4:2:0 本身造成的损失，降 QP 补不回来。**量两条基线**：（a）原图；（b）原图做 4:2:0 再还原的“理论上限”。QP 的贡献只和 (b) 比，4:2:0 本身的损失单独记一栏。【推测，依据：Prangnell 转述的“4:2:0 文字可读性差”，没读到原始数据】
- 每个编码器、每种分辨率分别量；**编码器之间不共用 QP_text**（去块滤波、RDOQ、读回 QP 的含义都不同，上面没有任何研究给出可换算的结论）。

**指标**（只在文字框内算，按页面取分布）
1. 主指标：**文字区域 SSIM**（Y 通道；窗口按 SIQAD 一脉的做法对文字用小窗口，11×11、σ 1.5）加**文字区域 PSNR**（Y）。依据：SIQAD 整库 SSIM 明显好于 PSNR/MS-SSIM，且单一失真下 PSNR 可用；两个都留，是因为 `refine-plan.md` 里的临时判据已经用这两个。
2. 辅助：**OCR 字符错误率（CER）**，用随机字符串 + 易混字符对（rn/m、l/1/I、O/0、5/6、8/B）渲染成的页面，固定引擎和版本，**只看相对基线的增量**；用来抓“指标正常但字被认成别的字”的情况（对应 CTA 论文的失真类型）。定位是“拦截坏情况”，不当连续指标。
3. 校验：小规模主观 A/B，沿用 JCT-VC 的办法（并排、-4 到 +4，0 = 看不出，对象是原片），观看条件按我们的真实使用（100% 缩放、正常桌面距离），只测候选 QP 和它的上下各一档，每个编码器 × 分辨率一次。**VMAF 只记录，不参与判决**（训练前提是 1080p、3 倍屏高观看）。

**判据（事先写定，不看数据再改；数值是提议，要和 R-1 第 3 条的数据一起核对）**
- 取满足以下**全部**条件的最高 QP：
  1. 文字区域 SSIM 比基线低不超过 0.002，PSNR 低不超过 1.5 dB（沿用 `refine-plan.md` 的临时判据；改判据按“只升不降”的规矩，要有理由）；
  2. 按页面取**最差的 5%**（p5），不是均值，防止少数小字号页面被平均掉；
  3. OCR 的 CER 相对基线增量为 0（易混字符对上不出现新错误），噪声余量先量基线自身的重复差异再定；
  4. 主观 A/B 平均投票的绝对值 < 0.5。
- 另加单调性检查：QP 往下每降一档，指标不能变差（出现反转说明量尺或编码器有问题，先停下查，不调阈值）。
- 结果写成一张表：编码器 × 分辨率 × 缩放 → QP_text，并写明基线、数据集、种子。

**没把握的地方，先记在这里**：OCR 判据 3 的噪声余量、主观测试人数（JCT-VC 用天真观看者，我们人手有限，Prangnell 只用了 4 人，要承认统计力弱）、4:2:0 基线 (b) 的做法是不是业内通行的，这三条没有现成的参考结论。

---

## 4. 没查到或没读到原文的地方

- **各硬件后端静止宏块的 skip 判决**：六家文档都没写。需要实机量。
- **VideoToolbox**：哪些 Mac 的 H.264 硬件编码器支持 `BaseFrameQP`；有没有读回 QP 的输出属性；有没有区域接口（头文件里没找到）。Apple 开发者网页读不到正文，用的是 iPhoneOS 17.5 SDK 头文件镜像（xybp888/iOS-SDKs），版本偏旧于当前 SDK 的可能性存在。
- **Media Foundation**：各厂商硬件 MFT 实际暴露哪些 `ICodecAPI` 属性；质量模式和 CBR 能不能中途切换；硬件 MFT 的 `EncodeQP` 范围。
- **NVENC**：constQP 改动不重置的官方说明（只有论坛二手）；指南后 4 万字符（8.17 节之后）没读；`minQP` 与 `qpDeltaMap` 的先后关系（只有 `PREQPDELTAMINQP` 标志的间接线索）。
- **VA-API**：Mesa 各驱动、Intel media-driver 对 `DisableSkipMap`、`skipCheckDisable`、ROI 的实际支持，没读驱动源码。
- **Chromium `VideoEncodeAccelerator`** 有没有逐帧 QP 接口，没读。
- **Parsec**：官方支持页 403，只有搜索摘要。
- **B 组**：JCTVC-U1015（SCC 通用测试条件）原文没读到；SCVD 论文和 Pixel-PAQ 论文只读到摘要或搜索摘要；SIQAD 的表 3 我只抄了部分列；VMAF 训练集是否含文字没读到原文；**没找到 OCR 识别率 vs H.264/HEVC 的 QP 的专门研究，也没找到具体到编码器和分辨率的“文字多少 QP 以下无差别”结论**。

---

## 5. 出处清单

读到原文（官方文档、头文件、源码、论文正文）：
- Apple VideoToolbox 头文件（`VTCompressionProperties.h`，iPhoneOS 17.5 SDK 镜像）：https://raw.githubusercontent.com/xybp888/iOS-SDKs/master/iPhoneOS17.5.sdk/System/Library/Frameworks/VideoToolbox.framework/Headers/VTCompressionProperties.h
- Microsoft Learn：[H.264 编码器](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder)、[`AVEncVideoEncodeQP`](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideoencodeqp)、[`AVEncVideoEncodeFrameTypeQP`](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideoencodeframetypeqp)、[`AVEncVideoMaxQP`](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideomaxqp)、[`AVEncVideoROIEnabled`](https://learn.microsoft.com/en-us/windows/win32/medfound/codecapi-avencvideoroienabled)、[`MFSampleExtension_ROIRectangle`](https://learn.microsoft.com/en-us/windows/win32/medfound/mfsampleextension-roirectangle)
- NVENC：[`nvEncodeAPI.h`](https://github.com/FFmpeg/nv-codec-headers/blob/master/include/ffnvcodec/nvEncodeAPI.h)（13.1）、[编程指南 13.0](https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html)
- libva：[`va.h`](https://github.com/intel/libva/blob/master/va/va.h)、[`va_enc_h264.h`](https://github.com/intel/libva/blob/master/va/va_enc_h264.h)
- oneVPL：[编码结构](https://intel.github.io/libvpl/latest/API_ref/VPL_structs_encode.html)；AMF：[编码 API 文档](https://github.com/GPUOpen-LibrariesAndSDKs/AMF/blob/master/amf/doc/AMF_Video_Encode_API.md)
- FFmpeg：[`videotoolboxenc.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/videotoolboxenc.c)、[`mfenc.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/mfenc.c)、[`nvenc.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/nvenc.c)、[`vaapi_encode.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/vaapi_encode.c)、[`vaapi_encode_h264.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libavcodec/vaapi_encode_h264.c)
- Chromium：[`webrtc_video_encoder_gpu.cc`](https://github.com/chromium/chromium/blob/main/remoting/codec/webrtc_video_encoder_gpu.cc)、[`webrtc_video_encoder_wrapper.cc`](https://github.com/chromium/chromium/blob/main/remoting/protocol/webrtc_video_encoder_wrapper.cc)；RustDesk：[`static_refresh.rs`](https://github.com/rustdesk/rustdesk/blob/master/src/server/video_service/static_refresh.rs)；Sunshine：[`nvenc_base.cpp`](https://github.com/LizardByte/Sunshine/blob/master/src/nvenc/nvenc_base.cpp)
- 论文和标准文件：[HEVC SCC 征集文件（ITU-T）](https://www.itu.int/en/ITU-T/studygroups/com16/video/Documents/CfP-HEVC-coding-screen-content.pdf)、[Xu/Joshi/Cohen SCC 综述（MERL 版）](https://www.merl.com/publications/docs/TR2015-126.pdf)、[AOM CTC v8](https://aomedia.org/docs/CWG-F384o_AV2_CTC_v8.pdf)、[Yang 等 SIQAD（QoMEX 2014）](https://ece.uwaterloo.ca/~z70wang/publications/QoMEX14b.pdf)、[Wang 等 ICIP 2015](https://ece.uwaterloo.ca/~z70wang/publications/icip15d.pdf)、[Prangnell & Sanchez，arXiv 1710.09919](https://arxiv.org/pdf/1710.09919)、[VMAF FAQ](https://github.com/Netflix/vmaf/blob/master/resource/doc/faq.md)、[Tesseract ImproveQuality](https://tesseract-ocr.github.io/tessdoc/ImproveQuality.html)、[MSU 屏幕内容数据集页](https://videoprocessing.github.io/screen-content-dataset)

只读到摘要或二手：
- [NVIDIA 论坛：改 constQP 不出 IDR](https://forums.developer.nvidia.com/t/change-constant-qp-value-without-generating-idr/74004)（论坛帖，读到正文，但是用户帖不是官方说明）、[NVIDIA 论坛：qpDeltaMap](https://forums.developer.nvidia.com/t/qpdeltamap-in-nvenc-5-0-1-sdk-samples/39121)（只有首帖）
- [Parsec 编码器质量设置](https://support.parsec.app/hc/en-us/articles/32381815770772-Encoder-Video-Quality-Settings)（403，只有搜索摘要）
- [字符变换失真（IEEE TIP 2026，信号处理学会页面）](https://signalprocessingsociety.org/publications-resources/ieee-transactions-image-processing/2026/09/character-transformation-artifact)、[3DGS-SC（arXiv 2609.31756）](https://arxiv.org/abs/2609.31756)（只有摘要页）
- Pixel-PAQ（arXiv 1708.06417）、Mittal 等 H.264 视觉无损、SCVD（TIP 2020）、GDELT OCR 博客、US 10638168：只有搜索结果摘要

## 6. 主会话审查（2026-11-28）

1. **接受**。【原文】【推测】【二手】标得清楚，没查到的单独列了。下面几条是推断或二手，R-4 实机量之前不拿它们下结论：
   - Chromium 的 GPU 路径不做 top-off：从 `quantizer = 0` 和 `top_off_active_` 的条件推出来，没跑过；
   - NVENC 改 constQP 会逼出 IDR：只有论坛帖；
   - Apple Silicon 的 H.264 编码器支不支持 `BaseFrameQP`：只能运行时查 `SupportsBaseFrameQP`。
2. **对 `BackendTraits` 的影响**（refine-plan 第 3 节的“按后端声明”）：
   - 补清晰的声明拆成四项：每帧 QP（精确 / 偏移 / 只能改码率）、跳过规则（openh264 已知，硬件一律“未知”）、读回 QP（有或没有）、区域控制（有或没有）。
   - 控制层按这四项选补清晰方式：
     - 精确 QP 加已知规则，用 R1；
     - 只能改码率，保持现在按帧大小判断收敛的做法；
     - 规则未知时，先按 R1 做，实机量出来再定。
3. **产品现状的风险**：Mac 上用的是 VideoToolbox（任务 #31、#46）。它没有区域接口，每帧 QP 要整个会话自己码控才能用。
   - 补清晰在 Mac 上只能靠码率控制间接压 QP。现在按帧大小判断补完没有（`REFINE_DONE_BYTES`），先保持。
   - R-4 第一个量 VideoToolbox：用 desc 链和 hold 链的办法，加上运行时查 `SupportsBaseFrameQP`。
4. **QP_text 的量法**（第 3.2 节）作为 M3 的基础：
   - 文字区域按小窗口算 SSIM 和 PSNR，取页面最差 5%；OCR 只做辅助拦截。
   - 判据的数和 refine-plan R-1 第 3 条的数据一起核对，不单独拍板。
