# 高分辨率、高画质远程桌面：业内做法、硬件能力与出处

调研日期 2026-11-27。只整理业内做法，不评价 PeerDesk 代码。
标记约定：**[已核实]** = 本次读到了原文页面；**[二手]** = 搜索摘要或第三方转述，未读到原文；**[未核实]** = 来自我的背景知识或推算，没有出处。
许可证：Sunshine / Moonlight 为 GPL-3，RustDesk 为 AGPL-3，OBS 为 GPL-2+。下文只引它们的思路，不抄代码。
本次没有重复 `industry-codec-layer.md`（编码层架构、SDK 许可、低延迟编码参数）和 `O-modes-industry.md`（模式概念、contentHint、degradationPreference、电影起播缓冲、视频区域检测）的内容。
本次几个站点读不到：Parsec 帮助中心多数页面返回 403，Apple 开发者文档是 JS 渲染，WebFetch 只拿到标题，HP 文档域名 docs.hp.com 不可达。这些位置在文中标出。

---

## 1. 各分辨率要多少码率，各家怎么定

### 要点（带出处）

**GeForce NOW（官方带宽表）[已核实]**
- URL: https://www.nvidia.com/en-us/geforce-now/system-reqs/
- 原话："25 Mbps for FHD resolutions at 60 FPS"；"35 Mbps for UW QHD & QHD resolutions at 120 FPS"；"45 Mbps for 4K resolutions at 120 FPS"。
- 其他平台：智能电视 "40 Mbps for 4K resolutions at 60 FPS"；沉浸式设备 "55 Mbps for 4K resolutions at up to 90 FPS"；1080p 240/360 fps 为 55 Mbps。
- "A GeForce NOW Premium membership is required for features like 5K/4K/UW streaming resolutions"（4K 是付费档的卖点）。
- 注意：这是"最低带宽要求"，不是编码码率。官方页没说 4K 用 HEVC 还是 AV1 [未核实]。这一档靠云端的编码调优和 HEVC/AV1 达成，H.264 同画质需要更高。

**Parsec [二手 + 部分已核实]**
- 帮助页 403，数字来自搜索摘要。
- 带宽上限设置：应用内的带宽上限是 "the maximum bandwidth used by Parsec, split between all connected guests"。高级设置里最大码率默认 10。
- 社区/文档说法："going higher than 50 will definitely have a material impact on your latency" [二手]。
- Parsec 给设计师的推荐：1080p、H.265 开、Prefer 4:4:4 开、Constant FPS 开、带宽 25+ Mbps、60+ fps [二手]。
- 分辨率选项："Keep Host Resolution"、"Use Client Resolution"（默认）、固定值。
- 一个第三方综述写免费版含 4K60 [二手]。
- URL（403，仅作定位）: https://support.parsec.app/hc/en-us/articles/32381776828820-Setting-The-Maximum-Bitrate-Of-The-Stream 、 https://support.parsec.app/hc/en-us/articles/32361396676116-Artists-and-Animators

**Moonlight / Sunshine（GPL-3，只看思路）**
- Moonlight 官方 Setup Guide 没有按分辨率给码率表 [已核实：页面里没有]。URL: https://github.com/moonlight-stream/moonlight-docs/wiki/Setup-Guide
- 客户端 UI 里的默认码率随分辨率/帧率自动算（1080p60 约 20 Mbps、4K60 约 80 Mbps）[未核实，来自记忆]。
- 第三方教程写 4K60 用 80–150 Mbps [二手，非官方]。
- Sunshine 配置文档 [已核实]，URL: https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2configuration.html
  - `max_bitrate`："The maximum bitrate (in Kbps) that Sunshine will encode the stream at."；0 表示用 Moonlight 请求的值。
  - `fec_percentage` 默认 20："Percentage of error correcting packets per data packet in each video frame."，代价是带宽。
  - `minimum_fps_target`："Sunshine tries to save bandwidth when content on screen is static or a low framerate."；0 表示取流 fps 的一半。
  - `qp` 默认 28，仅在设备不支持 CBR 时用。
  - NVENC：`nvenc_twopass` 默认 quarter_res，"Disabling it is not recommended since this can lead to occasional bitrate overshoot and subsequent packet loss."；`nvenc_preset` 默认 P1；`nvenc_spatial_aq` 默认关。

**Steam Remote Play [二手]**
- 没读到 Valve 官方带宽表。
- 老论坛说硬件加速时码率封顶 30 Mbps（2015 帖，过时）。
- 2026 年 Steam beta 允许手动设置 Pyrowave 码率到 1 Gbit/s，Valve 警告会增加延迟。URL: https://mixed-news.com/en/steam-client-beta-pyrowave-1-gbit-manual-bitrate-reset-latency/
- 社区反映 4K 常被"limit res"设置或主机显示器限制在 1080p。

**Microsoft RDP / Azure Virtual Desktop [已核实]**
- URL: https://learn.microsoft.com/en-us/azure/virtual-desktop/graphics-encoding
- 官方没有给"每分辨率多少 Mbps"。
- 默认色度 4:2:0；"On average, approximately 80% of the graphics data for a remote session is text"，所以文字有专用编码器（只在软件编码路径的 mixed-mode）。
- 全屏视频编码（AVC/HEVC）适合图像、3D、视频，"performs worse than mixed-mode encoding when the screen content is largely text based"。
- "HEVC/H.265 allows for 25-50% data compression compared to AVC/H.264, at the same video quality"（微软自己的说法）。
- 4:4:4 要组策略开 "Prioritize H.264/AVC 444 Graphics mode" 且 Image Quality = High（见第 4 节）。

**HP Anyware / PCoIP Ultra [二手，HP 文档页 403/不可达，来自搜索摘要]**
- Session Planning Guide 里 Big Buck Bunny 片头、Q80 默认质量、4K UHD：Auto-Offload 4:2:0 约 25.0 Mbps，4:4:4 约 43.8 Mbps，CPU Offload 约 61.4 Mbps。
- 另一版本表：4K 播放 Q70 约 27、Q80 约 47、Q90 约 68、Q100 约 107 Mbps。
- Teradici 时代估算：单个 3840×2160 显示器高活动区峰值 187–249 Mbps，平均通常不到峰值的 10%。
- "unchanged regions of the image progressively build to a lossless (perfect) quality"；"PCoIP Ultra Auto-Offload offers build-to-lossless color accuracy"。
- 建议留 500 kbps–1 Mbps 突发余量。
- URL: https://anyware.hp.com/web-help/pcoip/session_planning_guide/2023.06/pcoip_ultra/example_benchmark_configuration

**Splashtop [二手，厂商宣传]**
- 宣称 4K 最高 60 fps、支持 4:4:4。2026-02 新闻稿称只有它在测试中稳定 4K60，未给方法和带宽。
- URL: https://www.splashtop.com/press/splashtop-introduces-ai-optimized-codec-to-power-smoother-workflows

**AnyDesk、ToDesk、向日葵、Chrome 远程桌面**
- 没找到官方 4K 码率/带宽文档，**[未核实]**。
- Chrome 远程桌面源码里 AV1 编码器设置 `AV1E_SET_TUNE_CONTENT, AOM_CONTENT_SCREEN` [已核实，Chromium 源码]。URL: https://chromium.googlesource.com/chromium/src.git/+/main/remoting/codec/webrtc_video_encoder_av1.cc

### 推算（我算的，不是出处）[未核实，需用我们自己的素材验证]
- 用 GFN 的 45 Mbps@4K120 反推：约 0.045 bit/像素/帧（HEVC/AV1 且是游戏画面）。
- 文字办公几乎静止，平均码率远低于峰值。HP 的"平均不到峰值 10%"和 Sunshine 的 `minimum_fps_target` 是同一思路：静止时省，爆发时给够。
- 码率要按"峰值爆发能撑多少"定上限，而不是按平均。

### 可以直接借鉴的做法
1. 码率表按"分辨率×帧率×内容类型"三维给，不是只按分辨率。GFN 是分辨率×帧率表。
2. 码率上限和"请求码率"分开（Sunshine 的 `max_bitrate` 与客户端请求）。
3. 静止降帧省带宽（Sunshine `minimum_fps_target`、HP 渐进无损）。
4. 向用户暴露的是"带宽上限"，不是质量数字（Parsec）。Parsec 自己说超过 50 Mbps 明显影响延迟，所以高码率不是免费的。

### 各家差异
- GFN：按档位卖分辨率（4K 要付费）。Parsec：用户自己设上限，默认保守。
- HP：同一画面 4:2:0 与 4:4:4 带宽差约 75%（25→43.8），并有"渐进到无损"。
- RDP：文字单独编码，不是用视频码率去硬压文字。

---

## 2. 4K60 每一环的能力上限（逐环）

### 2.1 采集
- **DXGI Desktop Duplication**：帧由合成器（DWM）驱动，"it will only be able to generate frames at the speed the windows compositor is going, which is usually at most the speed of the monitors refresh rate"（GStreamer 论坛讨论的转述）[二手]。
  - 一个 deepwiki 页面把 DDA 列为全桌面采集里"最低延迟、HDR 最好"的选项 [二手]。URL: https://deepwiki.com/qiin2333/foundation-sunshine/4.1.1-windows-capture-methods
  - Win32CaptureSample 有 issue 称 Desktop Duplication 在 24H2 后变慢 [二手]。URL: https://github.com/robmikh/Win32CaptureSample/issues/82
- **Windows.Graphics.Capture**：Windows 10 1803+，可采窗口；OBS 论坛用户称 Win10 以后两者差不多 [二手]。
  - 没找到 4K60 的公开实测（GPU 占用、延迟）。必须自己在目标 GPU 和 Windows 版本上测 **[未核实]**。
- 采集是 GPU 帧：DDA 给 D3D11 纹理（BGRA）。如果后面编码在同一 GPU，可零拷贝，不经过 CPU **[未核实，一般做法]**。
- **macOS ScreenCaptureKit**：Apple 文档页读不到（JS 渲染）。以下全部 **[未核实]**，需实测：
  - `SCStreamConfiguration` 有 `width/height`、`pixelFormat`（BGRA、420v、420f 等）、`minimumFrameInterval`、`queueDepth`、`captureResolution`、`showsCursor`。
  - 能直接输出 420 像素格式，可省一次 RGB→YUV。这点需要对照文档和实机确认。
  - Retina 屏的像素数是逻辑分辨率的 4 倍，采集尺寸要显式设。

### 2.2 颜色转换（RGB→YUV）
- openh264 只接 YUV 4:2:0 planar（README：输入输出只有 4:2:0）[已核实]。所以走 openh264 必须有一步 RGB→YUV，在 CPU 上做，4K 一帧约 830 万像素，每帧 BGRA 33 MB。
  - 33 MB × 60 = 约 2 GB/s 内存读带宽，只做转换，不算编码。这是推算 **[未核实]**。
- NVENC 能直接吃 BGRA/ARGB 并在驱动内转换，Sunshine/OBS/Parsec 一类做法是在 GPU 上用 shader 做转换 **[未核实，一般做法]**；Sunshine 在 Windows 上确实用 D3D11 shader 做转换（见 codec-layer 文档）。
- **4:4:4 或 AVC444 需要自己拆平面**：RDP AVC444 的拆法见第 4 节，必须在 GPU 或 SIMD 上做。
- 本项目现状相关的教训已在 CLAUDE.md（整数系数、按行遍历），这里不重复。

### 2.3 硬件编码（NVIDIA 已核实，其他厂商部分已核实）
- NVIDIA 官方矩阵 [已核实]，URL: https://developer.nvidia.com/video-encode-decode-gpu-support-matrix
  - Maxwell 二代（5th Gen）：H.264，HEVC 4:2:0 最高 4K。
  - Pascal/Volta（6th）起：H.264+HEVC，4:4:4（H.264 与 HEVC），多数 8K（HEVC）。
  - Turing/Ampere（7th）：同上，无 AV1 编码。
  - Ada（8th）、Blackwell（9th）：H.264、HEVC、AV1 编码，H.264/HEVC 4:4:4，8K（HEVC）。
  - 矩阵备注原话："Codec support may vary by product manufacturer and region."
- NVENC 并发会话：消费级 GeForce 在矩阵里列为 12（Turing 到 Blackwell）；专业卡 "Unrestricted" [已核实，另一页]。URL: https://developer.nvidia.com/video-encode-and-decode-gpu-support-matrix-new
  - 旧资料里的 "3 路/5 路" 限制已被放宽，我们按"12"写，但以实机为准 **[二手/部分核实]**。
- NVENC 编程指南 [已核实，只读到前 100K 字符]：
  - 低延迟调优分 low latency 和 ultra-low latency；"Low latency, with CBR" 推荐给云游戏/串流/视频会议。URL: https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvenc-video-encoder-api-prog-guide/index.html
  - 分帧编码（split frame）只对 HEVC 和 AV1 开，最小帧高 HEVC 2112、AV1 2048。这对 4K（高 2160）有用，H.264 不能用。
  - 指南里没读到 H.264 4K 的帧率数字，**[未核实]**。
- **AMD VCN** [二手，Wikipedia 表格错乱]，URL: https://en.wikipedia.org/wiki/Video_Core_Next
  - 所有 VCN 代都有 H.264 与 HEVC 编解码，所有代 **都不支持 4:4:4**（Sunshine 讨论也写 "No 4:4:4 support in hardware as of the Radeon RX 7900 series"）。
  - AV1 解码从 VCN 3.0（RDNA2）起，AV1 编码从 VCN 4.0（RDNA3，Navi 3x/Phoenix）起。
  - 帧率数字没读到，**[未核实]**。
- **Intel QSV** [二手，Wikipedia]，URL: https://en.wikipedia.org/wiki/Intel_Quick_Sync_Video
  - Ice Lake 起 HEVC 8/10 位 4:2:2、4:4:4 编解码加速。Tiger Lake 起 AV1 解码（10 位 4:2:0）。Arc 独显有 8K 10 位 AV1 编码，AV1 编码为 4:2:0。
  - 与 Sunshine 讨论一致：Intel QSV/VA-API 做 HEVC 4:4:4 编码需要 Ice Lake 及以后。URL: https://github.com/orgs/LizardByte/discussions/220
- **Apple VideoToolbox**：文档读不到。已知：
  - `kVTVideoEncoderSpecification_EnableLowLatencyRateControl` 存在，用于 H.264/HEVC 低延迟 [未核实，页面仅标题]。
  - 4:4:4 编码是否支持 **[未核实]**，没查到。
  - A17 Pro（iPhone 15 Pro）和 M3 起有 AV1 硬件解码（Bitmovin 博客 [二手]，URL: https://bitmovin.com/blog/apple-av1-support/ ）。Apple 芯片没有 AV1 硬件编码（我印象如此 **[未核实]**）。
- **Windows Media Foundation**：本次没有新查。能力取决于底层厂商 MFT，见 codec-layer 文档。

### 2.4 H.264 级别（Wikipedia 表，推算已核实）
URL: https://en.wikipedia.org/wiki/Advanced_Video_Coding （表中 High 档码率是源表 ×1.25）

| 级别 | 最大宏块率 MB/s | 最大帧宏块数 | High 档最大码率 | 典型分辨率 |
|---|---|---|---|---|
| 4.2 | 522,240 | 8,704 | 62.5 Mbps | 1080p 最高 64 fps |
| 5.0 | 589,824 | 22,080 | 168.75 Mbps | 1080p 72 fps |
| 5.1 | 983,040 | 36,864 | 300 Mbps | **4K 31.7 fps**；1080p 120 fps |
| 5.2 | 2,073,600 | 36,864 | 300 Mbps | **4K 66.8 fps**（4096×2160 刚好 60.0） |
| 6.0 | 4,177,920 | 139,264 | 300 Mbps | 4K 128.9 fps；8K 32 fps |

- 推算：3840×2160 = 240×135 = 32,400 宏块/帧；60 fps = 1,944,000 MB/s，超过 5.1 的 983,040，**所以 4K60 的 H.264 必须声明 5.2 级**；5.1 级最多 4K30（31.7 fps）。
- 1440p（2560×1440 = 160×90 = 14,400 宏块）：5.1 级可到 68 fps（983,040/14,400），5.0 级只到 40 fps（589,824/14,400）。1440p60 需要 5.1。
- 1080p60：8,160 宏块 × 60 = 489,600，4.2 级够用（522,240）。
- **openh264**：README 说"Constrained Baseline only"，级别写到 5.2，最大帧 36,864 宏块；头文件里枚举只到 `LEVEL_5_1`（两处不一致，需要实测），"known issue with resolutions above 3840x2160"[已核实]。URL: https://raw.githubusercontent.com/cisco/openh264/master/README.md 、 https://raw.githubusercontent.com/cisco/openh264/master/codec/api/wels/codec_app_def.h
  - 头文件有 `SCREEN_CONTENT_REAL_TIME` 用法类型。枚举里有 `PRO_HIGH`、`PRO_HIGH444`（枚举存在不等于编码器实现，README 说只有 Constrained Baseline）。
  - 解码器"单线程处理所有 slice"，编码器多 slice 才能多线程。4K 软件编码要靠切 slice 并行，会损失压缩效率。
- 软件 4K 实际帧率：openh264、x264 没找到公开的 4K 实测 **[未核实]**。x264 ultrafast/zerolatency 在高端桌面 CPU 上 4K 能跑几十帧是常识，但没出处。必须自己测。

### 2.5 解码
- NVDEC [已核实矩阵]：HEVC 4:4:4（8/10/12 位）只在 Blackwell 一代列出，早期代不支持；AV1 解码在 Ampere/Ada/Blackwell，Turing 无；AV1 只有 4:2:0。注意：和 Sunshine 讨论里"GTX 10 系列起 NVDEC 解 HEVC 4:4:4"的说法冲突（NVIDIA 矩阵与社区不一致，**需要实机验证**）。
- Intel：Ice Lake 起 HEVC 4:4:4 解码（走厂商专用路径）；Tiger Lake 起 AV1 解码 [二手]。
- AMD：无硬件 4:4:4 解码 [二手]。
- Apple：A17 Pro / M3 起 AV1 硬解 [二手]；HEVC 4:4:4 硬解 **[未核实]**；iPad M4 上 Moonlight iOS 的 4:4:4 靠软件解码（社区改版，Mekkermb, 2025-01）[已核实在 Sunshine 讨论里]。
- 浏览器/移动端：不展开，本次没查。

### 2.6 显示
- 帧节奏、垂直同步对延迟的影响：本次没找到一手测量数据，**[未核实]**。一般结论：垂直同步最多多一帧（60 Hz 下 16.7 ms），可变刷新率可以消掉。
- Parsec 的 "Constant FPS" 选项意思是固定帧率（不随内容变），用于设计/视频场景 [二手]。

---

## 3. 4K60 能力表（按编码格式分列的版本见第 11.6 节）

说明：
- "OK" = 硬件和级别都满足，有出处；"未核实" = 没实测。
- 级别列：H.264 4K60 必须 5.2。
- 表里的 4K30/4K60 是**规格能力**，不是我们的实测，不能直接当门槛。

| 环节 / 组合 | 4K60 | 备注和出处 |
|---|---|---|
| H.264 级别 5.1 | 只能 4K30（31.7 fps） | Wikipedia 级别表 |
| H.264 级别 5.2 | 4K60 OK（66.8 fps） | 同上；编码器和解码器都要声明 5.2 |
| openh264 编码（Constrained Baseline） | 级别上限 5.1/5.2 说法不一；3840×2160 以上有已知问题；4K 实际帧率 **未核实** | README、头文件 |
| openh264 解码 | 单线程 slice 处理，4K60 **未核实** | README |
| NVENC（Maxwell 二代起）H.264/HEVC 4K | 规格 OK | NVIDIA 矩阵；4K60 实测 **未核实** |
| NVENC 并发会话 | 消费卡 12 路 | NVIDIA 矩阵新页 |
| NVENC AV1 编码 | Ada/Blackwell 才有 | NVIDIA 矩阵 |
| AMD VCN H.264/HEVC | 规格 OK，实测 **未核实**；AV1 编码 RDNA3+ | Wikipedia（二手） |
| Intel QSV H.264/HEVC | 规格 OK，实测 **未核实**；AV1 编码仅 Arc 独显（及 Meteor Lake 之后，**未核实**） | Wikipedia（二手） |
| Apple VideoToolbox H.264/HEVC 编码 4K60 | **未核实** | 文档读不到 |
| NVDEC HEVC 4:4:4 解码 | 官方矩阵只列 Blackwell | 社区说 GTX 10 起可以，冲突 |
| AMD 硬件 4:4:4 解码 | 做不到 | Sunshine 讨论 |
| Intel Ice Lake+ HEVC 4:4:4 解码 | OK（专用路径） | Sunshine 讨论 |
| Apple 硬件 4:4:4 解码 | **未核实**；iPad M4 实测软件解码 | Sunshine 讨论 |
| AV1 硬件解码 | NVIDIA Ampere+、Intel Tiger Lake+、AMD RDNA2+、Apple A17 Pro/M3+ | 各自出处如上，AMD/Apple 为二手 |
| 4:4:4 编码 | NVIDIA（H.264+HEVC，Pascal 起）、Intel Ice Lake+（HEVC）；AMD 做不到 | NVIDIA 矩阵、Sunshine 讨论 |
| 软件 4:4:4（x264 High 4:4:4） | 4K60 基本做不到 **未核实** | 常识推断 |
| 采集 DXGI DDA | 帧率受合成器限制，≤显示器刷新率 | 二手 |
| 采集 WGC / SCK | 4K60 开销 **未核实** | 无公开数据 |

结论：
- 能做 4K60 的路径：NVENC/QSV/AMF/VideoToolbox H.264 5.2 或 HEVC，且客户端有 4K60 硬解。
- 只能 4K30：软件编码（openh264 类）。H.264 5.1 级设备。
- 做不到：AMD 硬件 4:4:4；openh264 的 High/4:4:4。

---

## 4. 4:4:4 色度和设计模式

### 要点（带出处）
**为什么 4:2:0 会糊**
- 4:2:0 把色度横竖各减半，每 2×2 像素共用一组 U/V。彩色细字（红字蓝底之类）亮度对比小而色度对比大时，边缘颜色被平均，字发糊、有彩边。RDP 文档的默认说明是 4:2:0 "provides a good balance between image quality and network bandwidth"，要更好就升到 4:4:4。URL: https://learn.microsoft.com/en-us/azure/virtual-desktop/graphics-chroma-value-increase-4-4-4 [已核实]
- 具体视觉效果的论文/图示本次没找到，**[未核实]**。

**RDP AVC444：两路 4:2:0 拼成 4:4:4 [已核实]**
- 规范：MS-RDPEGFX。URL: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/8131c1bc-1af8-4907-a05a-f72f4581160f
- 原话："The RFX_AVC444_BITMAP_STREAM structure encapsulates two RFX_AVC420_BITMAP_STREAM structures. These two YUV420p streams MUST be combined to produce a YUV444 frame."
- "The YUV444 frame ... can be packed into two YUV420 frames (a main and auxiliary view)"，主视图是亮度，辅助视图装多出来的色度，按 16×16 宏块排布，辅助视图在宏块内按 8 行交织。
- 解码时：宏块在亮度子帧矩形里，按 4:2:0 转换；在色度子帧矩形里，要用"最近一次亮度子帧"的 Y/U/V 再加当前色度子帧。
- 有个可选的"逆滤波"：若逆滤波使某像素分量变化小于阈值（30），就用未逆滤波的值，防量化伪影。
- 意义：不需要 4:4:4 硬件编解码器，只要普通 4:2:0 H.264 编解码器就能做，代价是码率和算力约 2 倍（两路）。AMD 没有 4:4:4 硬件时可以用这种办法。
- 微软启用方式：组策略 "Prioritize H.264/AVC 444 Graphics mode" + Image Quality = High；"You don't need to use GPU acceleration to change the chroma value, however a 4:4:4 chroma value will use more bandwidth"。

**Parsec [二手]**
- "Prefer 4:4:4 Color" 是客户端开关，且在覆盖层里"(in-stream overlay, paid)"，H.265、10 位色会覆盖解码器选择。
- 没有硬件 4:4:4 解码时可以用 FFmpeg 做软件解码，但"FFmpeg is slower and uses more system resources"，建议换 NVIDIA/Intel GPU。
- 社区说法：Intel 核显、NVIDIA GTX/RTX 可 4:4:4 编解码，AMD 只有 4:2:0（过时、未标日期）。
- URL: https://support.parsec.app/hc/en-us/articles/32361372801172-4-4-4-Decoding-using-FFmpeg （403，来自搜索摘要）

**Sunshine/Moonlight 4:4:4（GPL-3，只看思路）[已核实讨论页]**
- URL: https://github.com/orgs/LizardByte/discussions/220
- 2024-07：Windows 原生 4:4:4 编码"ready for testing"（Sunshine PR #2533，NVIDIA 与 Intel）；"recombined 4:4:4"（由两路 4:2:0 拼）"should be ready soon"；moonlight-qt 的 Vulkan 原生 4:4:4 解码在测试。
- cgutman 说难点：解码 API 缺口、FFmpeg 补丁、SDL 没有 YUV444 纹理。
- 到 2025-01 仍未确认合入正式版。我们不知道到 2026-11 的状态，**[未核实]**。

**Citrix [二手]**：搜索摘要说 Citrix HDX 的 AV1 没有 4:4:4，因为没有硬件编解码支持。URL: https://www.citrix.com/blogs/2023/06/12/maximizing-user-experience-with-advanced-video-codec-support-in-citrix-hdx/

**编码器规格**
- H.264 的 4:4:4 是 High 4:4:4 Predictive 档（profile 244）："adds support for full 4:4:4 chroma sampling and sample depths up to 14 bits ... efficient lossless region coding"。Wikipedia [已核实]。
- HEVC 4:4:4 是 RExt 档；AV1 4:4:4 是 High 档。
- 硬件：NVENC 与 Intel（Ice Lake+）可以；AMD 不行；Apple 编码 **[未核实]**。

**10 位和 HDR**
- Moonlight HDR 要求 [已核实]：主机要有能编 HEVC Main 10 的 NVIDIA/AMD/Intel GPU；客户端 "must support both HEVC Main10 decoding and HDR10 output"；"If HDR is already enabled on your host PC when streaming starts, you may need to toggle HDR off and back on"。
- Sunshine 配置：`hevc_mode` 3 = Main 与 Main10，`av1_mode` 3 = AV1 8/10 位；`dd_hdr_option` 默认 auto（按客户端请求切换 HDR）。
- 10 位对设计很有价值（渐变不起条带），但是 HDR 在 SDR 桌面上做色调映射的做法本次没查，**[未核实]**。

**色彩管理（ICC、色域）**
- 本次没有找到任何远程桌面产品公开说明怎么处理 ICC 和色域，**[未核实]**。
- RDP 文档说的"设计、视频制作"场景只提了 4:4:4 和全屏视频编码，没提 ICC。
- 一般结论：远程桌面传的是被控端显示器经过合成后的像素，不传 ICC 配置；客户端屏幕色域不同会偏色，需要客户端做色彩变换。这是推断。

### 可以直接借鉴的做法
1. 做 4:4:4 的优先级：先 AVC444（两路 4:2:0，任何 4:2:0 硬件都能用）还是原生 4:4:4（需要 NVIDIA/Intel 编码、NVIDIA/Intel 解码）。微软选前者，Sunshine 两条都在做。
2. 4:4:4 做成"客户端开关 + 协商"，客户端不支持就不开，并告诉用户原因（见第 9 节）。Parsec 就是客户端开关。
3. 渐进到无损（HP）：静止后把 4:2:0 补成 4:4:4 的色度，不用一直传高码率。这点和项目的"静止后马上补清晰"一致。
4. 只对变化区域发色度子帧（RDP 的辅助视图有自己的 regionRects）。

### 各家差异
- RDP：软件拆两路，所有 4:2:0 硬件都能解。Parsec/Moonlight：原生 4:4:4，卡在解码硬件（AMD、苹果）。HP：自己的编码器渐进无损。Citrix：AV1 不做 4:4:4。

---

## 5. 游戏模式

本次没有读到各家官方的端到端延迟分解，只有二手数字。

### 要点
- 二手测量（review 站，来源不严谨）[二手]：Parsec 编码约 8 ms、局域网端到端约 11–13 ms（引自 Parsec 自己的 benchmark）；Steam Link 约 14–16 ms；GFN Ultimate 25–75 ms，不同来源差别大。URL: https://tech-insider.org/parsec-vs-moonlight-vs-steam-link-2026/
- 较严谨的老测量：PC Gamer 用 GFN 测 Metro Exodus，96 ms（本地 46 ms），60 fps 上限、带宽 400+ Mbps [二手，Stadia 时代]。URL: https://pcgamer.com/geforce-now-beats-stadia-in-our-input-latency-testing
- 没找到 Parsec、GFN、Steam Link 官方的 click-to-photon 数字。**[未核实]**。
- 降延迟手段，有出处的：
  - NVENC：低延迟调优 + CBR 是官方推荐；`nvenc_latency_over_power` 默认开，"Disabling this is not recommended since this can lead to significantly increased encoding latency"；`nvenc_realtime_hags` 用实时 GPU 调度优先级 [已核实，Sunshine 配置]。
  - 二遍编码：`nvenc_twopass` 默认 quarter_res，关掉会码率超调、丢包 [已核实]。
  - 帧率不超 60 更稳（Parsec 说超过 60 可能在某些设备上不稳 [二手]）。
  - Parsec 称码率高于 50 Mbps 会影响延迟 [二手]。Valve 称 Remote Play 高带宽设置增加延迟 [二手]。
  - 前向纠错：Sunshine 默认 20% [已核实]。
- 光标本地绘制、输入直通、可变刷新率：本次没有查到一手资料，**[未核实]**。

### 可以直接借鉴的做法
- 游戏模式：低延迟 CBR、小码率缓冲、帧率恒定（Parsec 的 Constant FPS）、不做重排缓冲。
- 抖动取舍：游戏宁可丢帧不排队，电影宁可缓冲（O-modes 文档已有，不重复）。

### 各家差异
- Parsec：以低延迟为先，码率保守。Steam：允许用户拉高码率，同时警告延迟。GFN：云端编码，延迟受网络决定。

---

## 6. 电影模式补充

本次范围内只补充了与分辨率/码率相关的部分，起播缓冲和视频区域检测见 O-modes 文档。

- 微软建议视频播放优先用多媒体重定向（把码流发给客户端本地解码），"redirects video playback to the local device ... lowers processing cost on the remote virtual machine regardless of encoding configuration" [已核实]。URL: https://learn.microsoft.com/en-us/azure/virtual-desktop/graphics-encoding
  - 这对我们等于"检测到视频区域就直接转发原码流"这条路，需要被控端能拿到原码流，不通用。
- 24/25/30/60 片源的帧节奏、音画同步的业内指标：本次没有新增一手资料。ITU-R BT.1359 的同步容限（音频超前 45 ms 到滞后 125 ms 的范围）是我记忆里的数字，**[未核实]**。
- 电影码率：HP 的 4K 视频播放基准 25–47 Mbps（Q80 档，见第 1 节）[二手]；GFN 4K 45–55 Mbps。电影档位的目标范围可以参考 25–55 Mbps，用 HEVC/AV1 的话低端更合适。这是我基于两个数字的合并，不是出处。
- Sunshine 的 `minimum_fps_target`：静止或低帧率内容时省带宽 [已核实]；电影 24 fps 按原节奏送，要避免被它当"低帧率内容"降到半帧率，需要自己的逻辑 **[未核实]**。

---

## 7. 分辨率设置的语义

### 要点
**Parsec [二手]**
- 三个选项：Keep Host Resolution、Use Client Resolution（默认）、固定值。"Parsec will try to switch to the closest resolution available if the host display can't run the chosen one."；所有客户端断开后恢复原分辨率。
- 虚拟显示器：驱动级，高级配置可加最多 5 个自定义分辨率（驱动 0.37 起）；虚拟显示器是 Teams/Warp 订阅功能，免费用户只有 "Fallback to Virtual Display"。
- 无显示器主机：有专门页面 "Running Parsec On A Headless Gaming PC Or A Server"。
- URL: https://support.parsec.app/hc/en-us/articles/32361400566804-Changing-the-Resolution-of-the-Video-Stream （403，来自搜索摘要）

**Moonlight/Sunshine（GPL-3）**
- Moonlight 提醒 [已核实]："You may see video scaling artifacts if the stream resolution doesn't match the host display resolution"；HDR 主机要把串流分辨率设成与主机显示器一致。需要 "720p or higher display (or headless display dongle)"。
- Sunshine（Windows）的 `dd_*` 选项 [已核实]：`dd_resolution_option` 默认 auto，"changes to the client's requested resolution"，且要求 Moonlight 里开 "Optimize game settings"；`dd_configuration_option` 有 ensure_active / ensure_primary / ensure_only_display；`dd_mode_remapping` 可以把请求的分辨率和帧率映射到另一个显示模式。
- Apollo（Sunshine 的分支）的 SudoVDA 虚拟显示器：只有 Windows，自动匹配客户端分辨率和帧率 [二手]。Linux 无头方案有的用 headless 输出并在连接时用 prep command 改成客户端的宽高帧率，断开时恢复。

**macOS 无显示器时的虚拟屏**：没查到一手资料，**[未核实]**。

**高 DPI**：Retina 2880×1800 的处理没找到各家的明确说法，**[未核实]**。

**客户端超分辨率**：RTX VSR 是 NVIDIA 在浏览器视频和 OBS 上的功能 [二手，NVIDIA 博客]；没找到任何远程桌面在用 RTX VSR 或 MetalFX 的公开资料，**[未核实]**。URL: https://blogs.nvidia.com/blog/rtx-video-super-resolution 。NVIDIA 还提到 RTX VSR 会增加功耗 [二手]。

### 三种语义对比
| 语义 | 做法 | 代价 |
|---|---|---|
| 改被控端屏幕分辨率 | Parsec 默认、Sunshine `dd_resolution_option` | 会改乱被控端桌面窗口布局，要恢复；多用户或本机有人用时冲突 |
| 编码缩放 | 采集原分辨率，编码前缩小 | 不动被控端，文字被缩放糊；Moonlight 自己说"scaling artifacts" |
| 虚拟显示器 | Parsec VDD、SudoVDA | 不动真实屏幕；要驱动签名和安装；Windows 为主 |

### 可以直接借鉴的做法
1. 给用户三选一：保持主机分辨率 / 匹配客户端 / 固定值（Parsec）。
2. 改分辨率前记下原值，全部客户端断开后恢复（Parsec）。
3. 请求的分辨率做不到时选"最近的可用分辨率"，并告诉用户（见第 9 节）。
4. 无头设备：要么硬件假显示器，要么软件虚拟显示器。Parsec 虚拟显示器做成付费功能，说明它有分发成本（驱动签名）。

---

## 8. 画质怎么量

### 要点
- PSNR/SSIM/VMAF：
  - 搜索 "VMAF 对文字是否有效" 没找到 Netflix 对屏幕内容的官方说法 [已核实：没找到]。
  - AWS MediaConvert 文档把 VMAF 评为对自然内容最可靠，对动画/游戏较差，也没提文字 [二手]。URL: https://docs.aws.amazon.com/mediaconvert/latest/ug/per-frame-metrics-analysis.html
  - 有论文称 VMAF 和 PSNR/SSIM 的相关度分别为 0.83 / 0.88（Inter4k 数据集，1000 个序列，自然视频）[二手]；有论文发现 VMAF 并不比 SSIM、PSNR 好 [二手，Wikipedia 转述]。
  - 我的看法（**[未核实]**）：VMAF 用自然视频训练，对文字边缘和小字不可靠，不能单独当门槛。
- 屏幕内容专用：
  - HEVC SCC 征集提案文件（ITU）有 4:4:4 测试序列表，主观评测用 1920×1080 和 1280×720，RGB 与 YCbCr 都要处理，测试条件有 All Intra 和 Low Delay，低延迟 intra 周期 20/30/60 fps 对应 16/32/64 帧。第一个序列 `sc_flyingGraphics_1920x1080_60`。URL: https://www.itu.int/en/ITU-T/studygroups/com16/video/Documents/CfP-HEVC-coding-screen-content.pdf [二手，序列表被截断]
  - 公共测试条件文档 JCTVC-U1015 本次没找到，**[未核实]**。
  - AV1 屏幕内容工具：Chromium 的 WebCodecs 有 "AV1 screen content coding tools" 的 Intent to Ship。URL: https://groups.google.com/a/chromium.org/g/blink-dev/c/BLAW7YO17jE [二手]。Chrome 远程桌面的 AV1 编码器设了 `AOM_CONTENT_SCREEN`（源码已核实）。
  - Google 称 libaom 3.6.0 屏幕共享改进后，AV1 speed 10 比 VP9 speed 7 压缩好 45% 且快 35% [二手，developer.chrome.com 博客]。URL: https://developer.chrome.com/blog/av1.md.txt
- 各家公开的画质对比：
  - Splashtop 2026-02 新闻稿声称自己稳定 4K60、带宽更低，无方法 [二手，厂商]。
  - Microsoft 的"HEVC 省 25–50%"、HP 的 Q70–Q100 对应带宽 [见第 1 节]。
  - 没找到任何一家公开的文字清晰度对比方法，**[未核实]**。

### 可以借鉴的做法
- 用屏幕内容序列：HEVC SCC 序列（如果能合法获得，**许可证未核实**），或者录自己的真实桌面场景（CLAUDE.md 要求测试数据接近真实）。
- 指标组合：PSNR/SSIM 看每帧；"静止后多久变清晰到无损"看时间；文字可读性可以用 OCR 识别率（我的建议，**[未核实]** 业内有无采用）。

---

## 9. 降级时各家怎么告诉用户

本次没有找到任何一家产品界面提示的截图或原话，页面读不到。已知的片段：
- Parsec：分辨率选了做不到时"switch to the closest resolution available"[二手]，没有看到对用户的提示原话；可以从 Parsec 图标看当前解码器 [二手]。
- Moonlight：只在文档里警告缩放伪影，没有运行时提示 [已核实，文档]。
- Sunshine：有 `dd_mode_remapping` 做映射 [已核实]，是配置而不是提示。
- RDP：微软说验证是否真在用 4:4:4 要"open an Azure support request ... who can verify the chroma value from telemetry"[已核实]，也就是说用户界面里根本看不到，是反面例子。
- Steam：4K 被"limit res"限制，用户要自己找设置 [二手，社区]。
- 总的来说：**[未核实]** 业内普遍不告诉用户，这是我们可以做得更好的地方。

---

## 10. 对我们的提醒（一定会踩的坑）

1. **openh264 只有 Constrained Baseline，且只能 4:2:0**（README）。设计模式的 4:4:4、High 档、按 4K 软件编码都不可能走它。4K 软件路径要么限帧率、要么需要换 x264（GPL，不能用）。软件路径的 4K 能力要先测，不要先承诺。
2. **H.264 4K60 要声明级别 5.2**。5.1 只有 31.7 fps。编码器和解码器两端都要检查级别，别默默掉到 4K30。openh264 头文件枚举只到 5.1，README 又写 5.2，两处不一致，必须实测。
3. **AMD 没有硬件 4:4:4 编码，也没有硬件 4:4:4 解码**。AVC444（两路 4:2:0）是唯一能在 AMD 和 Apple 上用的办法，成本是两路编码。
4. **解码端是 4:4:4 的瓶颈**：NVIDIA 官方矩阵只在 Blackwell 一代列出 HEVC 4:4:4 解码，社区说更早的卡可以，两个说法对不上，要实机验证。Apple 的 4:4:4 硬解没查到，iPad M4 上实测靠软件解码。
5. **4:4:4 要有协商和回退，还要告诉用户**：Parsec 里"Prefer 4:4:4"会覆盖解码器选择，造成"开了 4:4:4 反而卡"的排查成本。
6. **Windows 下 4K60 采集帧率受合成器限制**（DDA），WGC 也要自测；24H2 有变慢的报告。采集 4K60 的开销没有公开数字，必须自己在多块 GPU 上测。
7. **RGB→YUV 在 4K 下每帧 33 MB 的 BGRA**，在 CPU 上做会吃掉核；有 GPU 的路径要用 GPU shader，软件路径要用 SIMD。
8. **码率不是越高越好**：Parsec 说 >50 Mbps 明显影响延迟，Valve 也警告。4K 的码率上限要同时考虑延迟。
9. **用户选了分辨率不等于被控端有这个分辨率**：无显示器的 Mac mini、Windows 主机没有真实显示器时只有 1080p 或更低；需要虚拟显示器或假显示器；Moonlight 文档要求 "720p or higher display (or headless display dongle)"。macOS 虚拟屏本次没查到资料。
10. **改被控端分辨率要能恢复**（Parsec 在所有客户端断开后恢复）；崩溃时也要恢复，这一点业内文档没说，是我们要想的。
11. **NVENC 的 H.264 不能用分帧编码**（只 HEVC/AV1，且帧高要 ≥2112/2048），4K H.264 在单个 NVENC 上的延迟比 HEVC/AV1 高 **[未核实]**。
12. **文字不能用游戏的调优**：RDP 说屏幕数据约 80% 是文字，要专门的文字编码；"全屏视频编码在文字多时比 mixed-mode 差"。
13. **没有任何一家公开做画质量尺**，也没有公开的降级提示。这两块没有现成答案，要自己定。

---

## 11. 编码格式分层：H.264 为主，高分辨率用 HEVC/AV1

### 11.1 H.264 级别：4K/8K 需要哪一级，"做不了 4K"是标准还是硬件的限制

**结论：标准本身能做 4K 和 8K，限制在硬件。** 数字来自 Wikipedia 级别表（[已核实]，URL: https://en.wikipedia.org/wiki/Advanced_Video_Coding），所需级别是我按 MaxFS/MaxMBPS 推算的。

| 目标 | 宏块/帧 | 宏块/秒 | 所需级别（标准） |
|---|---|---|---|
| 4K30（3840×2160） | 32,400 | 972,000 | 5.1（上限 983,040，只剩 1% 余量） |
| 4096×2160 @30 | 34,560 | 1,036,800 | 5.2（5.1 只到 28.5 fps） |
| 4K60 | 32,400 | 1,944,000 | 5.2（上限 2,073,600） |
| 4K120 | 32,400 | 3,888,000 | 6.0（4,177,920） |
| 8K（7680×4320）一帧 | 129,600 | — | 帧大小超过 5.2 的 36,864，必须 6.x（上限 139,264） |
| 8K30 | 129,600 | 3,888,000 | 6.0 |
| 8K60 | 129,600 | 7,776,000 | 6.1（8,355,840） |
- 5.1/5.2 的 High 档最大码率都是 300 Mbps，所以 4K60 级别上码率不是限制。
- 3840 宽超过 5.0 级的最大帧 22,080 宏块，所以 5.0 做不了 4K。

**硬件实际支持到哪一级 [已核实，NVDEC 编程指南]**
- URL: https://docs.nvidia.com/video-technologies/video-codec-sdk/13.0/nvdec-video-decoder-api-prog-guide/index.html
- NVDEC H.264 解码：Maxwell 到 Ada（含 Turing、Ampere、Hopper）上限 **4096×4096，级别 5.1**；只有 Blackwell 和 Jetson Thor 到 **8192×8192、级别 6.2**，并含 High 10 / High 422（不含 MBAFF）。
- NVDEC HEVC：Pascal 起 8192×8192（级别 6.0）。AV1：Ampere/Ada 起 8192×8192（级别 6.0）。
- NVENC H.264 编码：最大 4096×4096（Wikipedia 与 Gyroflow 文档，[二手]）；HEVC 在 Pascal 起 8192×8192，GTX 900 为 4096。URL: https://en.wikipedia.com/wiki/NVENC 、 https://docs.gyroflow.xyz/app/hardware-acceleration/nvidia
  - 因此 NVENC H.264 **不能编 8K**（Blackwell 编码上限没读到，**[未核实]**）。
  - 一个论坛用户报告 3840×3840 H.264 在 RTX 3070 笔记本上编码失败，说明 4096 上限在实际使用中会有别的约束 [二手]，未核实原因。
- 级别标签与吞吐：NVDEC 表写"5.1"，但 4K60 H.264 的宏块率超过 5.1 的标准上限。真机能否解 4K60 H.264 取决于硬件吞吐而不是标签，**[未核实]**，要实测。
- AMD、Intel、Apple 的 H.264 最大宽度和级别：没读到，**[未核实]**。openh264 见第 2.4 节。

### 11.2 HEVC：专利、许可、各平台怎么拿到

**专利池（Wikipedia，[二手]，URL: https://en.wikipedia.org/wiki/High_Efficiency_Video_Coding ）**
- 三个池：MPEG LA（2014，每设备 0.20 美元，前 10 万台免费，年封顶 2500 万美元；"first 100,000 'devices' (which includes software implementations) are royalty-free"）、HEVC Advance（2015，初期每设备最高 2.60 美元，且对内容收 0.5% 收入）、Velos Media（2017）。
- HEVC Advance 2016-11 曾声明：分发到消费级手机和 PC 的软件实现"可以免费分发，不需要专利许可"。这条是老消息。
- 最新 [二手，搜索摘要]：Access Advance 2025-11 的项目概述写"royalties due for HEVC Software Sold separately from a device and for Preloaded HEVC Pay-To-Use Software/Content"；也就是说软件**不是一律免费**，单独销售的软件要付。2025-07 公布 HEVC Advance 价格到 2030 年；2026-01 称 Access Advance 已在 2025-12 收购 Via LA 的 HEVC/VVC 池，更名 VCL Advance。URL: https://accessadvance.com/wp-content/uploads/2025/07/HEVC-Advance-Program-Overview-November-2025-v3.pdf 、 https://www.businesswire.com/news/home/20250721063248/en/Access-Advance-Announces-HEVC-Advance-and-VVC-Advance-Pricing-through-2030
- 对 Apache-2.0 开源加商业版的含义（**我的判断，不是法律意见，需要律师确认**）：
  - 调用系统/显卡驱动自带的 HEVC 编解码器：专利费由设备厂和 OS 厂在出货时付，我们是调用方，一般不单独付。这是行业惯例（Moonlight、Sunshine、Parsec 都这么用），但没有出处 **[未核实]**。
  - 自带软件 HEVC 编码器（如 x265）并打包分发：落入"软件实现"，要付费或要先确认池的最新条款；x265 本身是 GPL。我们的"不引入 GPL"规则下也不能用 x265。
  - Windows：HEVC 解码要装微软商店的 "HEVC Video Extensions"，付费版约 0.99 美元，另有免费的 "from Device Manufacturer" 版给 OEM 预装；Win10 1709 起从系统移除，"due to licensing costs"；Windows 11 22H2 起据 Wikipedia 默认内置 [二手]，但 Windows Latest 等仍把付费版/OEM 版并列 [二手]，状态不一致，**需要在真机上查**。URL: https://windowslatest.com/2025/07/16/can-you-get-hevc-codec-for-free-on-windows-11 、 https://www.windowscentral.com/microsoft-now-charging-hevc-video-extensions
  - Firefox 因专利限制，要用户装微软付费扩展才支持 Windows 上的 HEVC [二手]，说明"装不上"是常见失败点，要有检测与提示。
  - macOS/iOS 系统自带 HEVC 编解码（来自常识，**[未核实]**，本次没读到 Apple 原文）。Linux 用 VA-API（Sunshine/Moonlight 的做法，见 codec-layer 文档）。
- 各家硬件编码解码：见第 2.3、2.5 节和 11.6 表。HEVC 是所有现役 NVIDIA（Maxwell 二代起）、AMD VCN、Intel QSV 的标配。

### 11.3 AV1：许可、硬件、软件

**专利许可原文 [已核实]**，URL: https://aomedia.org/license/patent-license/
- "non-sublicensable, perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable" 许可，覆盖 Licensor 的 "Necessary Claims to make, use, sell, offer for sale, import or distribute any Implementation"（Implementation 包括编码器和解码器）。
- 防御性终止（1.3）：被许可方"initiates or participates in patent litigation"主张某个实现侵犯必要权利要求，则直接授予它的许可立即终止（回应对方诉讼与行使本许可的诉讼除外）。
- 1.2.2：许可直接来自 Licensor，"no rights are received from suppliers, distributors, or otherwise"；1.4 不保证不侵权。
- "免专利费"只覆盖 AOMedia 成员的专利，**不是对所有人的保证**：Sisvel 2020 年起卖 AV1 专利池许可；欧盟委员会 2022-07 立案、2023-05 无处理结案；2026 年有 Dolby 起诉 Snap、Malikie 起诉 TCL 涉及 AV1 专利 [二手，Wikipedia]。URL: https://en.wikipedia.org/wiki/AV1
- SVT-AV1 的 LICENSE.md 是 BSD-3-Clause-Clear，并明说"NO EXPRESS OR IMPLIED LICENSES TO ANY PARTY'S PATENT RIGHTS ARE GRANTED BY THIS LICENSE"（专利许可另在 AOM 专利许可里）[已核实]。URL: https://raw.githubusercontent.com/AOMediaCodec/SVT-AV1/master/LICENSE.md 。libaom 为 BSD-2-Clause [二手]；dav1d、rav1e、rav1d 的许可本次没读到原文，**[未核实]**（我印象里 dav1d 是 BSD-2、rav1e 是 BSD-2 加专利条款，但没出处）。

**硬件支持（Wikipedia 表，[二手]）**

| 厂商 | 解码 | 编码 |
|---|---|---|
| NVIDIA | GeForce RTX 30 | RTX 40 及以后 |
| Intel | Xe（Tiger Lake 核显起） | Xe2 及以后（Battlemage、Lunar Lake）、Arc、Data Center GPU Flex |
| AMD | RDNA2（除 Navi 24） | RDNA3 及以后 |
| Apple | A17 Pro、A18 及以后、M3 及以后 | 无 |
| Qualcomm | 骁龙 8 Gen 2 起；骁龙 X 系列 | 仅骁龙 X Plus/Elite、X2 系列 |
| Google | Tensor 全系 | Tensor G3 起 |
- 注意与前文的小差异：前面从另一页读到 Intel Arc 有 AV1 编码，这里写 Xe2 及以后和 Arc；Meteor Lake 是否有 AV1 编码两处不清，**[未核实]**。
- 硬件 AV1 的 4:4:4：NVIDIA 解码矩阵只列 4:2:0；Citrix 称因无硬件而不做 AV1 4:4:4 [二手]。

**软件速度与屏幕内容**
- AV1 标准的屏幕内容工具在 main profile 里：调色板预测（"palette predictor ... blocks with up to 8 dominant colors"）[二手，Wikipedia]；Agora 称 "AV1 is the first encoder to support screen sharing tools in its main profile" [二手]。帧内块拷贝（intra block copy）在 AV1 里的叫法和启用条件本次没读到，**[未核实]**。
- libaom 3.6.0（Chrome 113）实时模式 speed 6–10 大幅提速，另有给屏幕共享用的 speed 11 [二手]。URL: https://developer.chrome.com/blog/av1 、 https://webrtc.googlesource.com/src/+/17e358096ef31c75bb6bc6586a9b5946e2f52aee
- Chrome 远程桌面源码用 `AOM_CONTENT_SCREEN`（见第 8 节）[已核实]。
- **4K 屏幕内容实时编码在软件下能到几帧：没找到任何公开数字，[未核实]**；SVT-AV1、rav1e 的屏幕内容实时速度也没有。一篇 arXiv 说 libaom 实时模式比非实时模式低效约 33% [二手]。URL: https://arxiv.org/pdf/2009.14165
- 码率节省（Wikipedia 汇总，[二手]，多为自然视频，不是屏幕内容）：Facebook 2018（PSNR）比 x264 High 省 46.2%；Waterloo 2020（4K，主观）比 HEVC 省 9.5%、比 VP9 省 16.4%，AV1 编码比 AVC 慢约 590 倍；Streaming Media 2020 比 x265 veryslow 省 15–20%；Fraunhofer HHI 2016 的结果相反（AV1 比 HEVC 低效 65.7%），差异来自参数。**屏幕内容上比 H.264 省多少：没有可靠公开数字，[未核实]**。

### 11.4 8K
- 编码：NVENC HEVC/AV1 到 8192×8192（Pascal 起 HEVC，Ada 起 AV1）；NVENC H.264 只到 4096 [已核实/二手见 11.1]。NVIDIA 矩阵称 Ada 与 Blackwell "8K (HEVC)"。AMD VCN 3.0 起 8K 10 位（Wikipedia [二手]）；Intel Arc 8K 10 位 AV1 编码 [二手]。Apple 的 8K 编码 **[未核实]**。
- 解码：NVDEC H.264 8K 只有 Blackwell；HEVC/AV1 8192×8192（见 11.1）。其他 **[未核实]**。
- 产品：VMware Horizon Blast 支持 8K（7680×4560）桌面，最多两块 [二手]。URL: https://docs.omnissa.com/HorizonOverviewDeployment/MonitorsandScreenResolution 。Steam Remote Play 新增 8K 流，但报道称游戏很难到 60 fps [二手]。URL: https://www.pcworld.com/article/3612185/you-can-now-stream-steam-games-in-ridonkulous-8k-resolution-but-you-probably-dont-want-to.html 。Splashtop 宣传到 4K/5K，没到 8K [二手]。Parsec、Moonlight、GFN 的 8K：没查到，**[未核实]**。
- 8K60 H.264 需要 6.1 级，几乎没有硬件支持；8K 只能走 HEVC/AV1。

### 11.5 各家怎么选编码格式
- **Sunshine/Moonlight（GPL-3）[二手]**：Sunshine 的 `hevc_mode`、`av1_mode` 默认 0："advertise support ... based on encoder capabilities (recommended)"，也就是主机按能力通告；客户端做最终选择。第三方称 Moonlight 客户端有 Auto、H.264、HEVC 选项，Auto 在 Apple TV 上选 HEVC，Android 优先 HEVC 除非强制 AV1。Flathub 更新日志提到修复"在没有 HEVC 的系统上自动选 AV1"。（**协商出来 + 客户端可强制**，按能力，不是按分辨率。）
- **Parsec [二手]**：高级配置里 H.265 默认开（1），可设 0 回到 H.264；客户端设置里有 "H.265 (HEVC)" 开关；4:4:4、H.265、10 位会覆盖解码器选择。AV1 没查到官方设置。（**用户选 + 默认优先 HEVC**。）
- **GeForce NOW [二手]**：默认"automatically chooses the optimal codec for your session based on your device and network capabilities"，高级用户可在 Settings > Gameplay > Custom > Codec 覆盖；2023 年起引入 AV1（RTX 40 端）；网页端据一份 Mozilla bug 只有 H.264（单个用户报告，**[未核实]**）。URL: https://www.nvidia.com/en-eu/geforce-now/release-highlights/ （**自动 + 可覆盖**）。
- **Chrome 远程桌面**：源码里有 AV1 编码器，使用屏幕内容调优 [已核实]。选择逻辑（VP8/VP9/H.264/AV1）本次没读，**[未核实]**。
- **RDP / AVD [已核实]**：默认 mixed-mode（文字用专用编码器、图像和视频用 AVC）；全屏视频编码用 AVC 或 HEVC，"If you enable both HEVC/H.265 and AVC/H.264 hardware acceleration, but HEVC/H.265 isn't available on the local device, AVC/H.264 is used instead."；HEVC 需要有 GPU 的主机。（**策略配置 + 客户端能力协商回退，不按分辨率自动换**。）
- 按分辨率自动换格式的产品：没找到，**[未核实]**。业内是按"能力协商 + 用户可覆盖"，分辨率只是约束条件（例如 H.264 做不到的分辨率才换）。

### 11.6 按编码格式分列的 4K60 能力表（规格层面，未做实测）
| 环节 | H.264 | HEVC | AV1 |
|---|---|---|---|
| 标准上 4K60 所需 | 5.2 级 | 5.1（HEVC 级别未单独推算，**未核实**） | 5.1/5.0 以上（**未核实**） |
| 标准上 8K | 6.0–6.1 | 6.0 以上 | **未核实** |
| NVENC 编码 | 4096×4096，不能 8K | Pascal 起 8192 | Ada 起 |
| NVDEC 解码 | 4096×4096（5.1），Blackwell 8192 | Pascal 起 8192 | Ampere 起 8192 |
| AMD 编码 | 有（VCN 全系） | 有 | RDNA3 起 |
| AMD 解码 | 有 | 有 | RDNA2 起 |
| Intel 编码 | 有 | 有 | Arc/Xe2 起 |
| Intel 解码 | 有 | 有，Ice Lake 起 4:4:4 | Tiger Lake 起 |
| Apple 解码 | 有（**未核实**） | 有（**未核实**） | A17 Pro / M3 起 |
| Apple 编码 | 有（**未核实**） | 有（**未核实**） | 无 |
| 4:4:4 编码 | NVIDIA、（Intel 未核实） | NVIDIA Pascal 起、Intel Ice Lake 起；AMD 无 | 硬件基本没有 |
| 4:4:4 解码 | NVIDIA 矩阵未列 | Blackwell（官方）；社区称 Pascal 起；Intel Ice Lake 起；AMD 无 | 没有 |
| 软件编码 4K60 | openh264 只有 Constrained Baseline，帧率**未核实** | 软件 HEVC 有专利和 GPL 问题 | 实时屏幕内容帧率**未核实** |
| 许可风险 | 有专利池，OS 与显卡出货方已付 | 池多，软件分发有费用，Windows 解码要装扩展 | 免版税声明，但有 Sisvel 等第三方主张 |

**一句话分层建议（基于上面的事实，不是出处）**：H.264 5.2 作为 4K60 的基础路径（所有硬件都有，没有扩展安装问题）；HEVC 用于 8K、4:4:4、更省码率，优先走系统/驱动自带实现；AV1 用于有硬件的新机型，且我们在软件上没有可用的实时屏幕内容编码速度数据，不要承诺。

## 12. 没查到或没核实的

- 全部 Parsec 官方页（403）：4:4:4、分辨率、码率、虚拟显示器、无头主机原文。需要换工具或手工下载。
- HP Anyware 官方页原文（docs.hp.com 不可达，anyware.hp.com 页 403）：4K 带宽数字只有搜索摘要。
- Apple 文档（ScreenCaptureKit、VideoToolbox）：JS 渲染读不到。ScreenCaptureKit 4K60 开销、VideoToolbox 4K60 编码能力、4:4:4 编解码、低延迟模式细节全部未核实。
- AMD VCN、Intel QSV 各代 H.264/HEVC 4K60 的帧率数字：Wikipedia 表格错乱，没读到。
- NVENC 4K60 H.264 实测帧率与单帧延迟：编程指南只读了前 100K 字符。
- openh264/x264 4K 实际帧率：没有公开数据。
- DXGI DDA / WGC / SCK 的 4K60 开销与延迟：没有公开测量。
- 各家官方端到端延迟分解（Parsec、GFN、Steam Link）：只有二手新闻站数字。
- 光标本地绘制、输入直通、可变刷新率在各家的做法。
- 24/25/30/60 片源帧节奏、音画同步的业内指标（BT.1359 数字未核实）。
- AnyDesk、ToDesk、向日葵、Chrome 远程桌面的 4K 码率和默认值。
- Steam Remote Play 官方码率表和编码器选择（只有 beta 新闻和社区帖）。
- 色彩管理（ICC、广色域、HDR 到 SDR 色调映射）在远程桌面里的处理：没有一手资料。
- Retina 高 DPI 的处理、macOS 无显示器虚拟屏、客户端超分辨率（RTX VSR、MetalFX）在远程桌面里的使用：没有一手资料。
- JCTVC-U1015 公共测试条件、SCC 序列的许可证。
- 屏幕内容画质指标：没找到专门的业内指标，OCR 识别率作为指标是我的建议。
- 各家降级时界面提示的原话和截图。
- Sunshine/Moonlight 4:4:4 到 2026-11 的最终状态。
- （编码格式分层补充）AMD、Intel、Apple 的 H.264 最大宽度与级别；NVENC Blackwell 的 H.264 编码上限；HEVC/AV1 的级别对应 4K60 所需级别的推算。
- （编码格式分层补充）dav1d、rav1e、rav1d 的许可原文；AV1 帧内块拷贝在屏幕内容里的启用条件；SVT-AV1/rav1e 屏幕内容实时速度；4K 软件 AV1 实时屏幕内容能到几帧。
- （编码格式分层补充）AV1 在屏幕内容上比 H.264 省多少码率：只有自然视频的数字。
- （编码格式分层补充）HEVC 许可是否影响 Apache-2.0 开源加商业分发：没有法律意见，Access Advance 软件条款的最新原文没读到，需要律师和 Access Advance 确认。
- （编码格式分层补充）Windows 11 当前版本 HEVC 扩展是否内置：Wikipedia 与新闻站不一致。
- （编码格式分层补充）Parsec、Moonlight、GFN 的 8K 支持；Chrome 远程桌面的编码格式选择逻辑；Parsec 官方 AV1 设置。
