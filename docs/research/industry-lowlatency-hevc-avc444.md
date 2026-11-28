# 调研：VideoToolbox 低延迟编码、硬件 HEVC、RDP AVC444（2026-11-28，排在最后做）

产品负责人定（11-28）：采集、编解码的优化放到仿真做完、各场景调到最优之后。这里先存调研结果，到时从这里开始。
调研由 Sonnet 子代理读原文完成；【已证实】= 读到了原文，【未证实】= 没读到原文或来自二手来源，用之前要再核。

## 1. VideoToolbox 低延迟编码（macOS）

- `kVTVideoEncoderSpecification_EnableLowLatencyRateControl`：macOS 11.3 起；强制全 P 帧无限 GOP、无 B 帧和前瞻、High 系列 profile、时间分层。【已证实】
  https://developer.apple.com/tutorials/data/documentation/videotoolbox/kvtvideoencoderspecification_enablelowlatencyratecontrol.json
- WWDC21 10158：只说支持 H.264，低延迟模式一律走硬件、一进一出；相比默认模式 720p30 最多少 100 ms，没有绝对延迟数。【已证实】https://developer.apple.com/videos/play/wwdc2021/10158/
- 和 VBR 不兼容（Apple DTS 论坛确认）；CBR 的行为不明确。【已证实】https://developer.apple.com/forums/thread/799459
- HEVC 能不能开低延迟：没有 Apple 原文，FFmpeg 补丁讨论里有人报 -12902。【未证实】要在目标机器上用 `VTCopySupportedPropertyDictionaryForEncoder` 实测。
- `MaxFrameDelayCount`（设 0 或 1 压住缓冲）、`AllowFrameReordering=false`、`RealTime`、`PrioritizeEncodingSpeedOverQuality`（速度优先、可能牺牲质量）：文档链接见各属性页。
- 我们的数（2880×1800 H.264，平均 11 ms、最大 22 ms）好不好，没有一手来源能比；只能同一台 Mac 上逐项开关对比，**同时量文字画质**（速度和画质可能冲突）。

## 2. 硬件 HEVC

- Windows：MF HEVC 编码器 MFT 只有 Main 4:2:0 8-bit，有 `AVLowLatencyMode`、CBR、`VideoEncoderDisplayContentType`；解码 MFT 支持 DXVA 和低延迟模式。【已证实】
  https://learn.microsoft.com/en-us/windows/win32/medfound/h-265---hevc-video-encoder
  “HEVC Video Extensions”商店包是否必需：文档没说，要在干净的 Win10/11 上用 `MFTEnumEx` 实测。【未证实】
- 硬件代际（二手）：Intel Skylake 起 8-bit 编码；AMD Polaris 起；NVIDIA HEVC 4:4:4 编码 Pascal 起（官方）。
- Android：CDD（Android 15）要求手持设备支持 HEVC 解码并开放给第三方应用。【已证实】https://source.android.com/docs/compatibility/15/android-15-cdd
- macOS：哪些 Mac 通过 VideoToolbox 暴露硬件 HEVC 编码，没找到 Apple 的机型表，只能运行时探测。【未证实】
- 码率节省：论文里 HEVC SCC 对屏幕内容相对 H.264 省 60–90%，但那是 SCC 工具带来的；硬件编码器基本没有 SCC，硬件 HEVC 对屏幕内容省多少**要自己量**。
  https://www.microsoft.com/en-us/research/?p=798826
- 专利（不是法律意见）：调用操作系统自带编解码器、Apache-2.0 免费分发是否要许可，Access Advance 的 FAQ 没有直接回答；默认打开前直接问 Access Advance。https://accessadvance.com/faq/

## 3. RDP AVC444 / AVC444v2（彩色文字 4:4:4，用现有 4:2:0 硬件编码器）

- 两路 AVC420：主视图（Y + 降采样 UV）+ 辅助视图（被丢的色度），同一个编码器编、同一个解码器解。【已证实】
  https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/844018a5-d717-4bc9-bddb-8b4d6be5dd3f
- `LC` 字段：0 两路都有、1 只有主视图（色度以后补）、2 只有色度。辅助视图可以延后单独发（推断：静止时补）。【已证实】
- 代价：结构上最多约 2 倍的编解码像素量；微软没给码率对比数字。
- 业内：Parsec 4:4:4 要 Windows 主机加 NVIDIA GTX 1000+ 或 Intel 11 代+（https://parsec.app/warp）；
  NVDEC 不支持 H.264 4:4:4 解码，HEVC 4:4:4 解码 Turing 起（https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new）。

## 4. 到时候的顺序（风险从低到高）

1. 拆开量 Mac 编码那 11 ms（颜色转换、VT 编码、回调等待），逐项开关低延迟设置，同时量画质。
2. AVC444：不要新编解码器、没有专利问题，直接针对彩色文字；先 Windows 主机加 Windows 控制端；辅助视图只在静止时补。
3. 探测 HEVC 4:2:0 硬件能力（写进诊断），自己量省多少码率；授权问清楚再默认打开。
4. HEVC 4:4:4、SCC 放最后，只给高端组合做可选项。
