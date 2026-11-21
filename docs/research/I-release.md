# I. 硬件编解码（Rust 实现）与发布（签名、自动更新）调研

调研日期：2026-11-21。约定：**[官方]** 厂商或平台官方文档；**[源码]** 出自仓库源码或 README；**[二手]** 论坛、博客、社区问答；**[未核实]** 找不到一手来源或只是推断，不要当结论用。能力和编解码器的总体对比见 [B-industry.md](B-industry.md)，这里不重复，只讲在 Rust 里怎么落地。

## 0. 结论先行

**第一部分：硬件编解码**

1. **不用 FFmpeg 做主路径。** FFmpeg 本体是 LGPL 2.1+，一旦编进 `--enable-gpl`（比如 libx264）整个 FFmpeg 就变 GPL；LGPL 版本静态链接进 Apache-2.0 二进制，要么得让用户能重新链接，要么得动态链接。官方清单直接建议动态链接（[FFmpeg legal](https://ffmpeg.org/legal.html)）。我们只需要 3 个编码器，直接调平台 API 更干净，也能拿到 LTR、参考帧失效、动态码率这些 FFmpeg 封装里不一定暴露的控制 **[未核实]**。
2. **Windows 分两步走：先 Media Foundation（MF）硬件 MFT，再补 NVENC 直连。** MF 一套代码覆盖 NVIDIA / AMD / Intel 三家，带 D3D11 纹理输入；`windows` crate 是 Apache-2.0 / MIT。NVENC 直连用来拿 MF 做不到的控制（参考帧失效、帧内刷新、逐帧码率），头文件是 MIT。AMF（MIT）和 oneVPL（MIT）放到后面，等真有 MF 覆盖不了的机器再做。
3. **macOS 用 VideoToolbox，开低延迟码率控制。** `objc2-video-toolbox` 许可证是 Zlib OR Apache-2.0 OR MIT。低延迟模式只支持 H.264，且一定走硬件编码 [官方 WWDC21]。
4. **Linux 优先级最低**：VA-API（`cros-libva` / `cros-codecs`，BSD-3-Clause）加 NVENC，V4L2 不做。
5. **RustDesk 的 hwcodec 和 Sunshine 只读不抄。** RustDesk 是 AGPL-3.0；Sunshine 是 GPL。许可证宽松又有硬件 H.264 的现成 Rust crate **没找到**，要自己写，参考 Airhand（MIT，B 报告 §1.12 已有它的 LTR 经验）。
6. **抽象层的核心是“能力探测 + 冒烟测试 + 自动回退 OpenH264”**。Airhand 的教训照搬：驱动声称支持 LTR 但实际不遵守，只能靠建编码器时跑冒烟测试发现。

**第二部分：发布**

7. **Windows 签名首选 Azure Artifact Signing（原 Trusted Signing），约 9.99 美元/月**，但**有国家限制**：公共信任证书，组织可用地区包括美国、加拿大、欧盟、英国、澳大利亚、新西兰、日本、韩国、新加坡、瑞士、挪威、以色列；个人只有美国和加拿大 [官方 quickstart，2026-11-20 更新]。**如果主体在中国大陆，目前两种都不符合**，要么在上述地区注册公司，要么用传统 OV 证书。
8. **SmartScreen：EV 已无优势**。微软官方说 EV、OV、Artifact Signing 三者表现一样，都要攒信誉，首批用户会看到警告 [官方]。不要为了躲警告花 EV 的钱。
9. **macOS：Apple Developer Program（99 美元/年）加 Developer ID 签名、公证、装订。** 屏幕录制和辅助功能都走 TCC 授权，不需要 entitlement；**签名身份必须长期稳定，否则每次更新用户都要重新授权**。
10. **自动更新推荐 Velopack（MIT，官方 Rust 库，带增量包和渠道）用于有界面的客户端；无人值守的 Windows 服务要自己写一层更新器**（SYSTEM 服务下载、校验、换文件、回滚）。无论用哪个，都要额外加一层 ed25519（minisign）签名校验，Velopack 文档里没找到包签名校验的说明 **[未核实]**。

---

# 第一部分　硬件视频编解码（Rust）

## 1. 许可证与分发约束

| 组件 | 许可证 | 能否进 Apache-2.0 产品 | 备注 |
|---|---|---|---|
| `windows` crate（微软） | Apache-2.0 与 MIT 双许可（仓库页标注，[windows-rs](https://github.com/microsoft/windows-rs)） | 可以 | MF、D3D11、DXGI 都在其中，靠 feature 开关 |
| NVENC 头文件 `nvEncodeAPI.h`（FFmpeg 镜像 [nv-codec-headers](https://github.com/FFmpeg/nv-codec-headers)） | MIT 样式，文件头明文写 “This copyright notice applies to this header file only” [源码] | 可以 | 运行时由驱动里的 `nvEncodeAPI64.dll` 提供，**我们不分发 NVIDIA 的任何二进制**。镜像对应 Video Codec SDK 13.1.15，要求驱动 610+ |
| AMD AMF | MIT，另外 LICENSE 开头声明“AMD 不授予任何编解码标准（H.264、HEVC 等）的知识产权许可，相关专利费由使用者自己承担” [源码，[LICENSE.txt](https://raw.githubusercontent.com/GPUOpen-LibrariesAndSDKs/AMF/master/LICENSE.txt)] | 可以 | 运行时来自 AMD 驱动，Linux 上 25.20 起 AMF 运行时单独发布 [源码] |
| Intel oneVPL（libvpl） | MIT（[libvpl](https://github.com/intel/libvpl)） | 可以 | 只有分发器，必须装 Intel GPU 运行时（Iris Xe 及以后用 VPL 运行时，更老的用 Media SDK）。Rocket Lake 及更新只支持 VPL 运行时 [源码] |
| VideoToolbox（系统框架） | Apple SDK | 可以 | 系统自带，随 macOS 发布 |
| `objc2-video-toolbox` | Zlib OR Apache-2.0 OR MIT（0.3.0 的 Cargo.toml） | 可以 | 只核实了 0.3.0 的清单，用之前再看一次当前版本 |
| `cros-libva`、`cros-codecs`（ChromeOS） | BSD-3-Clause（[cros-codecs](https://github.com/chromeos/cros-codecs)） | 可以 | 都是 ChromeOS 为 crosvm 写的，Linux 通用发行版适配靠 fork 的 `libva` crate（许可证 **[未核实]**） |
| FFmpeg | LGPL 2.1+；带 GPL 部件（x264 等）则整体 GPL；`--enable-nonfree` 则不可再分发 | **有条件** | 见下 |
| RustDesk（含 hwcodec） | AGPL-3.0（RustDesk 主仓库）；hwcodec 单独仓库上找不到许可证文件，镜像页写“未指定”（[21pages/hwcodec](https://github.com/21pages/hwcodec)） | **不能抄** | hwcodec 是 RustDesk 的 vendored 依赖，按 AGPL 对待 |
| Sunshine | GPL-3.0 **[未核实具体版本]** | **不能抄** | B 报告已引用其行为，只读 |
| Airhand | MIT（[sqlpxc/airhand](https://github.com/sqlpxc/airhand)） | 可以抄，需在 `THIRD_PARTY_NOTICES.md` 注明 | 仓库首页只列出 `airhand-proto` / `airhand-link` / `airhand-mobile` 三个 crate，MF 编码代码在哪个目录要进仓库翻（`client/` 或 `server/`），**[未核实]** |

### 1.1 用 FFmpeg 会带来什么约束

依据 [FFmpeg legal](https://ffmpeg.org/legal.html) 的合规清单：

- 编译时**不能**带 `--enable-gpl` 和 `--enable-nonfree`，也不能链 libx264（官方点名）。否则 GPL 会传染到整个 FFmpeg，我们的二进制就不能是 Apache-2.0。
- 清单明确建议**动态链接**（Windows 上是 DLL）。LGPL 的要求是用户必须能替换库；静态链接要另外提供目标文件让用户重新链接。**[猜测]** 这和我们“只发两个二进制”的规则冲突：要么多带一批 DLL / dylib，要么承担静态链接的合规成本。
- 随产品要附 LGPL 许可证文本、FFmpeg 版本和源码获取方式、我们自己的补丁（如果有）。
- 专利：FFmpeg 官方说它无法判断是否用到专利算法，商用产品可能被专利方索要费用。**硬件路径的专利成本由谁承担，本次没找到权威结论 [未核实]**；AMF 的许可证明确说自己不授权，只能说明“调驱动不等于免专利费”，需要法务确认。这条对用 OpenH264 自己编译的二进制同样适用。
- 结论：FFmpeg 能帮我们省时间的地方只有“一个接口覆盖所有硬件”，但这正是我们要自己控制的地方（逐帧码率、LTR、失效参考帧）。**决定：不引入 FFmpeg。**

## 2. Windows

### 2.1 路线对比

| 路线 | 覆盖 | 逐帧控制 | 零拷贝输入 | 风险 |
|---|---|---|---|---|
| **MF 硬件 MFT**（`windows` crate） | NVIDIA / AMD / Intel 都由厂商驱动提供 MFT，一套代码 | 靠 `ICodecAPI` 属性，能力因驱动而异 | 支持 D3D11 纹理（给 MFT 设 `MFT_MESSAGE_SET_D3D_MANAGER`） | LTR、帧内刷新、动态码率是否生效必须逐机验证 |
| **NVENC 直连** | 仅 NVIDIA | 最全：`NvEncReconfigureEncoder` 改码率、`NvEncInvalidateRefFrames`、帧内刷新、`ULTRA_LOW_LATENCY` 调优 | 支持 D3D11 纹理注册 | 头文件 MIT，运行时依赖驱动；Rust 包装 crate 的现状见 §5 |
| **AMF 直连** | 仅 AMD | LTR、帧内刷新、按 tile 输出，AV1 SDK 文档齐全（[AMF 文档](https://github.com/GPUOpen-LibrariesAndSDKs/AMF)） | 支持 D3D11 | 需自己写 C ABI 绑定；先不做 |
| **oneVPL 直连** | 仅 Intel | B 报告说改分辨率必出 IDR | 支持 D3D11 | 需带分发器；先不做 |

MF 的一个争议点要实测：Airhand 说 MF 下 NVIDIA 和 Intel 的 H.264 LTR 可靠（B 报告 §1.12）；但另一份第三方基准写“NVIDIA 当前驱动没有注册 H.264 的 MF 硬件编码 MFT” [二手，[mediaway-encoder 基准](https://docs.rs/crate/mediaway-encoder/0.1.1/source/docs/windows/benchmarks.md)，**[未核实]**，两者矛盾]。**做法：探测时用 `MFTEnumEx` + 硬件标志枚举，枚举不到就自动走 NVENC 直连或软件，不靠假设。**

### 2.2 MF 编码器的低延迟设置（以微软官方的 H.264 编码器页为准）

依据 [H.264 Video Encoder](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder) [官方]：

- 输入子类型：NV12、I420、IYUV、YUY2、YV12；**必须先设输出类型再设输入类型**。
- 码率控制：`CODECAPI_AVEncCommonRateControlMode` 取 CBR、PeakConstrainedVBR、Quality、UnconstrainedVBR（默认）；CBR 和受限 VBR 需要 Windows 8 及以上。
- 缓冲：`CODECAPI_AVEncCommonBufferSize`（CBR 的缓冲字节数）、`CODECAPI_AVEncCommonMaxBitRate`。
- **动态改码率**：Windows 8 起 `CODECAPI_AVEncCommonMeanBitRate` 可以在编码中修改，“码率变化时编码器使用自适应编码”。这是软件 MFT 的文档；硬件 MFT 是否也不出 IDR，文档没写，**必须实测（统计改码率前后 IDR 数量）[未核实]**。
- **B 帧**：`CODECAPI_AVEncMPVDefaultBPictureCount` 默认 0，Baseline 一定为 0。必须在 `SetOutputType` 之前设置。
- **GOP**：`CODECAPI_AVEncMPVGOPSize`，0 表示编码器自选；我们要设成很大或让传输层按需强制关键帧（`CODECAPI_AVEncVideoForceKeyFrame`，微软认证必选项）。
- **低延迟**：`CODECAPI_AVLowLatencyMode`。软件 MFT 默认走切片并行（低延迟），设成 `VARIANT_FALSE` 才是多帧并行。硬件 MFT 对这个属性的响应属于认证里的可选项。
- **LTR（长期参考帧）**：`AVEncVideoLTRBufferControl`、`AVEncVideoMarkLTRFrame`、`AVEncVideoUseLTRFrame` 都是 Windows 8.1 的**可选**属性，认证时“支持才测”。所以驱动可以不支持、也可以声称支持但实际有 bug（Airhand 的结论正是如此）。
- **帧内刷新**：微软文档的属性列表里**没有**对应属性 **[未核实]**，MF 路径可能做不到，只能靠 LTR 恢复或关键帧；NVENC 直连有。
- 其他可选属性：`AVEncVideoMaxNumRefFrame`、`AVEncVideoTemporalLayerCount`（动态）、`AVEncVideoMinQP/MaxQP`、`AVEncSliceControlMode`、`AVEncVideoROIEnabled`。

### 2.3 零拷贝输入：DXGI 桌面复制 → GPU 上转 NV12 → 编码

链路：`IDXGIOutputDuplication::AcquireNextFrame` 得到 BGRA 的 `ID3D11Texture2D` → `ID3D11VideoContext::VideoProcessorBlt` 在 GPU 上转成 NV12 纹理（同时可缩放）→ 作为输入样本交给 MFT（经 `IMFDXGIDeviceManager`）或注册给 NVENC。Airhand 实测走的就是这条链（DXGI → D3D11 VideoProcessor → MF 硬件 MFT，1080p 编码约 5 ms，RTX 4080 笔记本，B 报告 §1.12）。

- 本次没能取到 VideoProcessor 的微软文档正文（Learn 的 D3D11 Video APIs 页面只有一句话）**[未核实]**。实现时回读 `ID3D11VideoContext` 的 API 页，重点确认：输入 BGRA 到输出 NV12 的色彩空间设置（BT.709、限制范围，要和解码端一致）；输出纹理需要的 BindFlags（`D3D11_BIND_RENDER_TARGET` 之类）；多线程下对设备的串行化。
- **每次采集到的纹理必须马上拷贝或转换**，因为 `ReleaseFrame` 之后纹理归 DXGI；转换后的 NV12 纹理要做小池子轮转，避免编码器还在读时被覆盖。
- 桌面复制的限制：不能抓 UAC 安全桌面和登录界面，除非进程在 SYSTEM 且附着到相应桌面；这与无人值守服务的架构有关，不属于编码范围，但会影响“探测”和“设备丢失”的处理：切换桌面时 `AcquireNextFrame` 会返回 `DXGI_ERROR_ACCESS_LOST`，编码器要能不重建而继续。**[未核实]**，实现时按错误码测。

### 2.4 控制端：解码与显示

- MF 自带 H.264 解码器（`CLSID_CMSH264DecoderMFT`，[官方页](https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-decoder)）：
  - 支持 DXVA 2（不支持 DXVA 1），`MF_SA_D3D_AWARE` 标示可接 DXVA；`CODECAPI_AVLowLatencyMode` 可用于低延迟解码。
  - 色度**只有 4:2:0**；最高 Level 5.1、**最大 4096×2304**；DXVA 的“保证分辨率”只有 1920×1088，更高分辨率“硬件支持就用 DXVA，否则软件解”。也就是说 **4K 以上用 MF 的 H.264 解码器会有静默回退软解的风险**，要在探测时实测解码耗时。
- 显示：解码输出 NV12 的 D3D11 纹理。最稳的第一版：把 NV12 读回内存再上传给 wgpu/egui（1080p 的 NV12 约 3 MB，读回成本小）；零拷贝要把 D3D11 纹理通过共享句柄交给 wgpu 的 DX12 或 Vulkan 后端，这块互操作的难度和稳定性我们没验证 **[未核实]**，列为后期优化。
- HEVC 解码器是独立 MFT，且不在所有 Windows 版本上自带 **[未核实]**，放到做 HEVC 的时候再查。

### 2.5 逐帧码率控制与我们现有 trait 的关系

我们的 `VideoEncoder::encode(frame_id, &Screen, changed, budget)` 是**每帧给字节预算**。硬件编码器的接口是**目标码率 + VBV 缓冲**，没有“这一帧不超过 N 字节”的直接参数（NVENC 是否有逐帧大小上限 **[未核实]**）。映射办法见 §6。

## 3. macOS

- **创建会话**：`VTCompressionSessionCreate`，在 encoderSpecification 里设 `kVTVideoEncoderSpecification_EnableLowLatencyRateControl = true` 开低延迟模式。依据 [WWDC21 10158](https://developer.apple.com/wwdc21/10158)：低延迟模式**始终使用硬件加速编码器**，**支持的编码格式只有 H.264**（2021 年的说法，之后是否放开 HEVC **[未核实]**）。FFmpeg 的 `videotoolboxenc` 补丁也是用同一个键 [二手]。有开发者报告设了这个键后 `VTCompressionSessionCreate` 返回 12902 [二手，[论坛帖](https://developer.apple.com/forums/thread/799459)，未见结论]，所以必须有“失败后去掉该键重试”的回退路径。
- **实时属性**（[Apple 直播编码示例](https://developer.apple.com/documentation/videotoolbox/encoding-video-for-live-streaming.md)，该示例标注适用 macOS 26 起，旧系统支持范围 **[未核实]**）：
  - `kVTCompressionPropertyKey_RealTime = true`；`ExpectedFrameRate` 只是码率控制的提示，实际按送入的帧率编。
  - 码率三种：`AverageBitRate`（软限制，可能超调或欠调）；`AverageBitRate` 加 `DataRateLimits`（给定时间窗内的硬上限，编码器不会超过）；`ConstantBitRate`（官方说“面向老式内容分发网络，不适合一般串流”，会用填充补齐）。还有 `VariableBitRate` 加 `VBVMaxBitRate`。
  - `MaxKeyFrameInterval`、`MaxKeyFrameIntervalDuration` 控制关键帧间隔；`SuggestedLookAheadFrameCount` 要设成 0 或不设以免增加延迟 **[猜测]**。
  - 示例里有预设字典 `kVTCompressionPreset_VideoConferencing`，可以拿来当初始设置，再覆盖。
- **动态改码率**：对运行中的会话再次 `VTSessionSetProperty`（`AverageBitRate` / `DataRateLimits`）。是否会引发 IDR **[未核实]**，实测。
- **关键帧**：每次 `VTCompressionSessionEncodeFrame` 的 frameProperties 里带 `kVTEncodeFrameOptionKey_ForceKeyFrame`。
- **LTR**：Apple 文档没找到。开发者论坛里有人用 `kVTCompressionPropertyKey_EnableLTR` 并在每次编码调用里强制刷新 LTR [二手，[论坛帖](https://developer.apple.com/forums/thread/703292)，发帖人自己说没找到文档]。**按“没有可靠的 LTR”设计，失丢包时用关键帧或帧内刷新（`MaxKeyFrameInterval` 之外强制 IDR）。**
- **零拷贝输入**：ScreenCaptureKit 的帧是 IOSurface 支持的 `CMSampleBuffer`/`CVPixelBuffer`，可直接作为 `VTCompressionSessionEncodeFrame` 的 `imageBuffer`；IOSurface 转 CVPixelBuffer 用 `CVPixelBufferCreateWithIOSurface`（[Apple 论坛答复](https://developer.apple.com/forums/thread/714421)）。在 `SCStreamConfiguration` 里直接要 NV12 像素格式可以省掉转换 **[未核实]**。社区 crate `screencapturekit`（含 IOSurface/Metal 支持）和 `videotoolbox`（README 声称可对接 screencapturekit 零拷贝）都存在，**许可证和成熟度没核实**，且“零拷贝”只是 README 说法，要用 Instruments 实测 [二手，[docs.rs/videotoolbox](https://docs.rs/videotoolbox)]。
- **Rust 绑定**：`objc2-video-toolbox` 是系统绑定（Zlib / Apache-2.0 / MIT），需要自己包一层安全接口，最小集合：Create、SetProperty、EncodeFrame、回调、Invalidate。回调在 VT 内部线程，要用 channel 送回编码循环。
- **解码**：`VTDecompressionSession`，输入从 NAL 单元重建 `CMVideoFormatDescription`（需要 SPS/PPS），输出 `CVPixelBuffer`（NV12，IOSurface）；渲染可直接用 Metal 纹理缓存，零拷贝。低延迟解码需要设 `kVTDecompressionPropertyKey_RealTime` 并保证码流无 B 帧、`num_reorder_frames=0` **[未核实]**。
- **公证相关**：VideoToolbox 不需要特别的 entitlement。

## 4. Linux（低优先级）

- **VA-API**：`cros-codecs`（BSD-3-Clause）提供 VA-API 的 H.264、H.265、VP8、VP9、AV1 解码，H.264、VP9、AV1 编码，以及有状态 V4L2 编码器；V4L2 解码还是“计划中” [源码，[cros-codecs](https://github.com/chromeos/cros-codecs)]。它是为 ChromeOS 的 crosvm 开发，通用发行版上的稳定性和维护节奏 **[未核实]**（描述自己是只读镜像，共 990 次提交）。底层 `cros-libva` 是 BSD-3-Clause，需要系统 libva ≥ 1.20 [二手]，另有 fork 的 `libva` crate 支持 dlopen，适合不想在构建时依赖 libva 开发包的情况（许可证 **[未核实]**）。
- **NVENC**：同 Windows，头文件是同一个 MIT 头，库是 `libnvidia-encode.so`，运行时 `dlopen`。
- **AMF（Linux）**：AMD 驱动 25.20 起运行时单独发布，仅部分发行版 [源码]。
- **采集**：Wayland 下是 PipeWire + xdg-desktop-portal，得到的是 DMA-BUF，理论上可直接交给 VA-API；这是单独的大工程，不在本报告范围，**[未核实]**。
- 建议：先只做 OpenH264 软件路径加 VA-API，NVENC 用同一套 Windows 代码。

## 5. 现有项目和 crate

| 项目 / crate | 许可证 | 做什么 | 我们怎么用 |
|---|---|---|---|
| RustDesk + hwcodec | AGPL-3.0 / 未指定 | 基于 FFmpeg 的硬件编解码；Windows 的显存路径只用 FFmpeg，驱动 SDK 代码只用来检测可用性；NV12 帧转给 D3D11 以避开 CUDA 上下文问题；macOS 只有内存（RAM）路径的 H.265；最低驱动版本 Intel 27.20.100.8935、AMD 23.1.2、NVIDIA 471.41 [源码，[hwcodec README](https://github.com/21pages/hwcodec)] | **只读设计思路，不抄代码** |
| Sunshine | GPL | NVENC（含 RFI、split encode）、AMF、QSV、VAAPI、VideoToolbox | 只读，行为已写在 B 报告 §1.2 |
| Airhand | MIT | DXGI → VideoProcessor → MF；LTR 冒烟测试 | 最值得读和借用；借用要在 `THIRD_PARTY_NOTICES.md` 写明 |
| `nvidia-video-codec-sdk` 0.4.0 | 0.3.x 在 lib.rs 上是 MIT（0.4.0 **[未核实]**） | NVENC 的 bindgen 绑定加安全包装 | **构建脚本找不到 SDK 库就 panic**，CI 上不友好；我们更适合自己用 MIT 头文件加 `libloading` 手写最小绑定 |
| `shiguredo_nvcodec` | **[未核实]** | 运行时动态加载 CUDA 驱动 API，构建时不依赖 CUDA 工具包 | 可以参考 dynamic loading 的做法 |
| `win-native-media` 0.1.1 | **[未核实]** | WGC 采集 + D3D11 VideoProcessor 转 NV12 + MF H.264 | 结构可参考；docs.rs 上构建失败，成熟度未知 |
| `hwcodec` 0.1.0（crates.io 同名，非 RustDesk 那个） | MIT OR Apache-2.0 | 搜索结果说侧重 AV1/VP9 **[未核实]** | 不用 |
| `cros-codecs` / `cros-libva` | BSD-3 | 见 §4 | Linux 阶段再评估 |
| `objc2-video-toolbox` | Zlib / Apache-2.0 / MIT | VT 系统绑定 | macOS 直接用 |

**一句话结论：没有现成的、许可证友好的、覆盖 D3D11 零拷贝加低延迟的 Rust H.264 硬件编码 crate，要自己写平台胶水。** 自己写的好处是 `unsafe` 全部落在我们规定的“平台层/媒体层系统接口文件”里，每处带 `// SAFETY:`，也符合 lint 规则。

## 6. 推荐抽象：硬件编码器接在 `VideoEncoder` 后面

现有 trait（`crates/core/src/video.rs`）是：`encode(frame_id, &Screen, changed, budget) -> Option<EncodedFrame>`、`on_feedback(floor, mask)`、`on_lost(frame_id)`、`wants_refine()`。其中 `on_feedback` / `on_lost` 本来就是为 LTR 和参考帧失效准备的，可以保留。要改的有三点：

### 6.1 输入：不能只收 CPU 里的 `&Screen`

```text
enum FrameInput<'a> {
    Cpu(&'a Screen),                    // 现有路径，OpenH264 用
    #[cfg(windows)] D3d11(NativeTexture),   // DXGI 复制得到的 BGRA 纹理
    #[cfg(target_os = "macos")] IoSurface(NativeSurface),
}
```

硬件编码器收到 `Cpu` 时自己上传（慢路径，也是 GPU 失败后的回退），收到 GPU 句柄时走零拷贝。采集端按“当前编码器声明的输入类型”决定产出什么，避免无谓的读回。

### 6.2 能力声明和探测

```text
struct EncoderCaps {
    vendor, device_name, driver_version,
    codecs: [H264, Hevc, ...], max_width, max_height,
    gpu_input: bool,
    dynamic_bitrate_without_idr: bool,   // 实测得出，不靠属性是否存在
    ltr_slots: u8,                       // 实测，0 表示不可用
    ref_invalidation: bool, intra_refresh: bool,
    zero_b_frames: bool,                 // 从 SPS 验证
    measured_encode_ms_p50 / p95: f32,   // 冒烟测试量出来
}
```

探测分三层，结果按 `(GPU 型号, 驱动版本, 我们的版本)` 缓存到磁盘，换驱动自动重测：

1. **枚举**：MF 用 `MFTEnumEx`（硬件标志），NVENC 用 `NvEncOpenEncodeSessionEx` 查询 GUID 和能力，VT 用 `VTCopyVideoEncoderList` 或直接建会话。
2. **冒烟测试**：用一小段**真实屏幕内容**（不是纯色，CLAUDE.md 的“不造假”）编 60 帧。检查：码流里是否出现 B 帧（看 SPS 和 slice 类型）；实际码率是否在目标的 ±30% 以内；**中途改码率后 IDR 数量是否为 0**；LTR 恢复后的码流能否被我们的解码器正确解出（Airhand 的教训：驱动说遵守了，实际没遵守）；单帧耗时 p95。
3. **隔离**：探测放在**子进程**里跑，防驱动崩溃或卡死带走主进程 **[猜测]**，超时（如 5 秒）算失败。

### 6.3 预算怎么映射到码率

我们的传输层每帧给的是**字节预算**，硬件接口是**码率 + 缓冲**。映射：

- `目标码率 = 预算 × 帧率`，与上次设置相比变化超过 5% 才下发（减少驱动抖动），只走“不触发 IDR”的路径。
- **缓冲（VBV）设成约一两帧的预算**，让编码器自己压住突发。MF 用 `AVEncCommonBufferSize`，NVENC 用 `vbvBufferSize`，VT 用 `DataRateLimits`。
- 硬件编码器会超调，所以做**闭环校正**：记录“实际字节 / 预算”的滑动比值，超调超过 1.5 倍连续 N 次，就把下一帧的目标压低，或者由传输层直接丢这一帧、等下一帧。具体阈值要用 E 报告的仿真调。
- 画质降级顺序（画质 → 帧率 → 分辨率）仍由传输层决定，编码器只执行。改分辨率仍然要“先建后切”，硬件编码器通常必出 IDR（B 报告 §3）。
- 静止补清晰度（`wants_refine`）：硬件编码器没有“只补残差”的模式，用降低 QP 加强制 P 帧的办法，效果和 Airhand 的 8 次补帧是否等价 **[未核实]**，先在 OpenH264 路径上保留此功能，硬件路径等测完再说。

### 6.4 回退与“不静默降级”

- 触发回退的条件：探测失败；运行时连续 N 帧编码超时；`DXGI_ERROR_DEVICE_REMOVED` / `DEVICE_RESET`；编码器返回错误；输出码流校验失败（比如发现 B 帧）。
- 回退动作：换成 OpenH264，立刻出一个 IDR，**在日志和界面里写明原因**（“NVENC 在驱动 xxx 上改码率会出关键帧，已改用软件编码”），并把本次回退记入遥测，用于统计各 GPU/驱动的失败率。这符合 CLAUDE.md 的“失败要给出原因和解决办法，不静默降级”。
- **码流兼容性**是硬性约束，不论哪个编码器出：H.264 High 或 Constrained Baseline，Annex B 格式，每个 IDR 前重复 SPS/PPS，无 B 帧，SPS 的 VUI 里 `num_reorder_frames = 0`、`max_dec_frame_buffering` 取最小，这样控制端硬件解码器才会立刻输出而不攒帧 **[未核实 VUI 对各家解码器的实际影响]**。这样两端的编码器和解码器可以任意搭配。

### 6.5 测试和覆盖率

- 策略（探测结果的判定、码率映射、闭环校正、回退状态机）全写成**纯 Rust、与平台无关**，用假后端做单元测试，吃进 99% 覆盖率。只有调用系统接口的胶水文件进 `scripts/coverage.sh` 的排除列表，理由写在脚本注释里。
- GitHub 托管的 Windows runner 没有 GPU，但有微软软件 H.264 编码器（`Mfh264enc.dll`），可以用来跑 MF 的代码路径（会话建立、属性设置、D3D 管理器），验证不到硬件特性 **[猜测]**。硬件特性要在开发者机器和自托管 GPU runner 上跑，所以 CLAUDE.md 里“先本地后 CI”的原则在这里更重要。macOS 托管 runner 上 VideoToolbox 是否有硬件编码 **[未核实]**。
- e2e 新增指标（只升不降）：硬件编码耗时 p50/p95、码率命中误差、改码率时 IDR 数、回退次数、硬件与 OpenH264 在同一内容上的 PSNR/SSIM 对比。

## 7. 第一部分的建议与步骤

| 步骤 | 内容 | 工作量（人日，主要由我做核心，子代理做调研和边角） |
|---|---|---|
| 1 | 抽象层：`FrameInput`、`EncoderCaps`、探测与缓存、码率映射、回退状态机；假后端加单元测试 | 4–5 |
| 2 | Windows MF 编码：枚举硬件 MFT、D3D11 输入、VideoProcessor 转 NV12、低延迟属性、动态码率、LTR 冒烟测试、探测接入 | 8–12 |
| 3 | Windows MF 解码（D3D11/DXVA）加显示（先读回再上传） | 4–6 |
| 4 | macOS VT 编码（低延迟、IOSurface 输入、动态码率） | 5–7 |
| 5 | macOS VT 解码，Metal 显示 | 3–5 |
| 6 | NVENC 直连（头文件加 `libloading`，RFI、帧内刷新、`NvEncReconfigureEncoder`） | 8–10 |
| 7 | e2e 指标、真实屏幕内容的冒烟素材 | 3 |
| 8 | AMF / oneVPL 直连（按线上回退统计再决定） | 各 6–8 |
| 9 | Linux VA-API | 8–10 |

顺序：1 → 2 → 3 → 4 → 5 → 6 → 7，先得到“Windows 与 macOS 都能硬件编解码，失败回退 OpenH264”的闭环，再做 NVENC 的精细控制。步骤 1 完成前不要写任何平台代码。

---

# 第二部分　自动更新与代码签名（海外发布）

## 8. Windows 代码签名

### 8.1 选项对比（依据 [微软 Code signing options，2026-11-10 更新](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/code-signing-options)）

| 选项 | 价格 | 可用地区 | SmartScreen | 适合 |
|---|---|---|---|---|
| Azure Artifact Signing（原 Trusted Signing） | 约 9.99 美元/月 | 见 §8.2 | 要攒信誉，初期有警告 | 微软推荐的非商店分发方案 |
| OV 证书（DigiCert、Sectigo 等） | 150–300 美元/年 | 全球 | 同上 | 用不了 Artifact Signing 的情形 |
| EV 证书 | 400 美元以上/年 | 全球 | **2024 年起和 OV 一样，不再直接绕过** | 官方说“不再建议为 SmartScreen 购买” |
| Microsoft Store（MSIX） | 免费，商店重签 | 全球 | 无警告 | 见 §8.4 |
| SignPath Foundation（开源免费） | 免费 | 全球 | 同 OV | 见 §8.5 |

### 8.2 Artifact Signing 的资格（**最关键，需产品负责人对号入座**）

- 公共信任证书：**组织**可用地区为美国、加拿大、欧盟、英国、澳大利亚、新西兰、日本、韩国、新加坡、瑞士、挪威、以色列；**个人**只有美国和加拿大 [官方 [quickstart](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart)，2026-11-20 更新]。私有信任证书不受地区限制，但私有信任不能用于公开分发（系统默认不信任）。
- 同一天另一页（Code signing options）写的组织地区只有“美国、加拿大、欧盟、英国”，比 quickstart 少。**以 quickstart 为准并在 Azure 门户实测**；微软自己的两页不一致，说明范围仍在调整。
- 社区问答里有法国个人、爱尔兰个体户申请被拒的反馈，澳大利亚也曾有“不支持”的帖子 [二手，[Q&A](https://learn.microsoft.com/en-us/answers/questions/5915697/can-an-irish-sole-trader-(non-incorporated)-qualif)]，只能说明规则收紧过，不能当结论。
- 个人验证：Azure 计费账户类型必须是“个人”，法定姓名和账单地址必须和政府身份证件一致，用 AU10TIX 加微软 Authenticator 做证件核验。
- 组织验证：要有法定实体名称、官网、**该实体自有域名下的邮箱**、商业登记号和地址，审核 1–30 个工作日，可能要补材料（12 个月内的文件、域名注册凭证等）。预览期曾要求公司成立满 3 年 **[未核实，现行规则以门户为准]**。
- **价格**：Basic 约 9.99 美元/月，含每月 5000 次签名；Premium 每月含 10 万次、可建 10 个证书配置，价格页没渲染出数字（我记得是 99.99 美元/月，**[未核实]**）；超量按次计费 [官方定价页]。我们只签安装包和主程序，Basic 绰绰有余。
- **如果主体在中国大陆**：个人和公司都不在名单里，**不能用**。出路：（a）在新加坡、美国、英国、欧盟等名单内地区注册一家公司，同时也满足后面 Apple 组织账号和收款的需要；（b）买 OV 证书。香港不在名单里（quickstart 未列）。

### 8.3 SmartScreen 信誉

- 微软官方：Artifact Signing **不提供即时信任**，新文件在积累足够下载记录前会有警告；EV 自 2024 年起同样如此（微软信任根计划从 2024 年 8 月起移除 EV 代码签名标识）[官方]。
- 信誉**按文件哈希累计**，二进制变了要重新攒；同一个发布者身份连续发布能让后续版本继承部分信任 [官方 [SmartScreen reputation](https://learn.microsoft.com/windows/apps/package-and-deploy/smartscreen-reputation)]。
- 2026 年 4 月前后有多条社区反馈：换了新的中间 CA 后，之前能过的 Trusted Signing 签名文件集体出现“无法识别的应用”警告 [二手，[Q&A](https://learn.microsoft.com/en-us/answers/questions/5855708/trusted-signing-regression-in-smartscreen-reputati)]，不是微软确认的问题。
- **应对**：首批用户会看到警告，这是所有新发布者的共同遭遇。做法：从第一个公开版本起就用同一个身份签名；**主程序、DLL 和安装包全部签名并加时间戳**；每个版本尽量少改文件；官网下载页写明“首次运行会有 SmartScreen 提示，点‘更多信息’再运行”，诚实告知；可通过 Microsoft Defender 的文件提交渠道申诉 **[未核实具体入口]**。

### 8.4 Microsoft Store

MSIX 上架免费且由商店重签、无警告 [官方]。但远程桌面的被控端需要 SYSTEM 服务、抓登录界面、注入安全桌面输入，MSIX 对“需要 SYSTEM 账户的 Windows 服务”的支持在社区问答里被说成不支持 [二手]，**[未核实]**。**可行的方案是控制端走商店，被控端走独立安装包**，等有精力再考虑，不在首发范围。

### 8.5 SignPath Foundation（开源免费签名）

条件：所有组件必须是 OSI 认可的开源许可证、**不能商业双许可**、不能含专有代码、项目在维护、已发布 [[SignPath 条款](https://signpath.org/terms)，页面标注为草案]。**证书的主体是 “SignPath Foundation”，不是我们**，要求项目发布代码签名政策、成员开启多因素认证。我们的客户端是 Apache-2.0，托管云另算，**客户端本身可能符合**；但对一个要收费的产品，发布者显示别人的名字不利于信任。**用途：Azure 资格办下来之前的过渡**。

### 8.6 在 GitHub Actions 里签名

依据 [azure/artifact-signing-action](https://github.com/Azure/artifact-signing-action)：

- 只能在 **Windows runner** 上跑（windows-2022 / 2025，不支持 Arm）。用 OIDC：job 设 `permissions: id-token: write`，先 `azure/login@v3`（client-id、tenant-id、subscription-id 放 secrets），再 `azure/artifact-signing-action@v2`。
- 身份需要 “Artifact Signing Certificate Profile Signer” 角色，否则签名失败。
- 输入：`endpoint`（和账号所在区域一致，如美东是 `https://eus.codesigning.azure.net/`）、`signing-account-name`、`certificate-profile-name`、`files-folder` 加 `files-folder-filter`、`file-digest: SHA256`、`timestamp-rfc3161: http://timestamp.acs.microsoft.com`、`timestamp-digest: SHA256`。
- 顺序：先签 exe 和 dll → 打安装包 → 再签安装包。Velopack 也直接支持：`--azureTrustedSignFile` 传元数据 JSON，Linux 和 macOS 上签名要 Velopack 1.3 及以上 [官方 [Velopack signing](https://docs.velopack.io/packaging/signing)]。
- Artifact Signing 的证书是短期证书、自动轮换，所以**时间戳必须加** **[未核实]**。
- CI 成本：签名步骤只在带 `[pkg]` 的提交上跑，和 CLAUDE.md 的标记约定一致。

## 9. macOS：Developer ID、公证、装订

### 9.1 账号

- Apple Developer Program 99 美元/年；Developer ID 和公证是付费会员的功能 [官方 [Apple 会员对比](https://developer.apple.com/support/compare-memberships)]。
- 个人注册**不需要 D-U-N-S**；组织注册需要免费的 D-U-N-S 编号（申请后最多 5 个工作日）[官方 [注册说明](https://developer.apple.com/programs/enroll/)]。个人账号签名时显示个人法定姓名；组织账号显示公司名，做收费产品应该用组织。
- 哪些地区无法注册 Apple 说“因制裁等限制某些地区可能不支持”；**大陆主体的具体要求本次没查到 [未核实]**，到 Apple 的中国区注册页确认。

### 9.2 流程（在 `macos-latest` runner 上）

下面是 Apple 的工具链惯例，具体参数实现时回读 `xcrun notarytool --help`（Apple 的“Customizing the notarization workflow”页面这次只拿到标题，**命令行参数未逐条对照官方 [未核实]**）：

1. 把 Developer ID Application 证书（.p12，base64 放 secrets）导入**临时钥匙串**：`security create-keychain`、`security import`、`security set-key-partition-list`。
2. 签名：`codesign --force --options runtime --timestamp --entitlements ent.plist --sign "Developer ID Application: 公司名 (TEAMID)"`，先签内部的二进制和动态库，最后签 .app。**必须启用 hardened runtime 和安全时间戳**，否则公证被拒。
3. 打 .zip（用 `ditto -c -k --keepParent`）或 .dmg，`xcrun notarytool submit … --key AuthKey_XXXX.p8 --key-id … --issuer … --wait`。API 密钥来自 App Store Connect（Users and Access → Integrations）；是否必须是团队级密钥 **[未核实]**。
4. `xcrun stapler staple` 把公证票据钉在 .app / .dmg / .pkg 上（.zip 不能装订）；`stapler validate` 和 `spctl -a -vvv` 验证。
5. 一个裸命令行二进制不能装订，需要放进 .app 包（带 Info.plist 和 bundle id）再装进 dmg。
6. Velopack 对应参数：`--signAppIdentity`、`--signInstallIdentity`、`--notaryProfile`（需要 `notarytool store-credentials` 保存的配置）、`--keychain`、`--signEntitlements` [官方]。文档只写了 Apple ID 加应用专用密码的方式，没写 API 密钥；用 API 密钥时可以自己先 `notarytool store-credentials` 再引用 **[未核实]**。

### 9.3 权限（entitlement 与 TCC）

- **屏幕录制**：Developer ID 分发的非沙盒应用**不需要任何 entitlement**，由 TCC 弹窗（“屏幕与系统音频录制”）授权。苹果工程师在论坛说私有的 `com.apple.private.*` 不对第三方开放；`com.apple.security.screen-capture` 在 Mac App Store 的校验里不被识别 [二手，[论坛](https://developer.apple.com/forums/thread/683860)、[论坛](https://developer.apple.com/forums/thread/778616)]。ScreenCaptureKit 文档要求先向用户申请屏幕录制权限。
- **辅助功能**（注入键鼠，`CGEventPost`）：同样是 TCC 授权，无 entitlement；被控端还可能需要“输入监控”。**我们没有拿到 Apple 一手文档逐条对照 [未核实]**，实机验证。
- **hardened runtime 例外**：纯 Rust 二进制无 JIT，**不需要任何例外**。如果将来带第三方动态库（比如 FFmpeg）且不是同一个 Team ID 签的，需要 `com.apple.security.cs.disable-library-validation`——这又是不用 FFmpeg 的一条理由，或者把动态库用同一证书重签。
- **签名身份必须稳定**：TCC 授权按代码签名的指定要求绑定。论坛反复出现“更换签名身份后屏幕录制权限丢失”[二手，[论坛](https://developer.apple.com/forums/thread/819406)]。含义：**自动更新不能换证书、不能换 bundle id**；开发期用 ad-hoc 签名时权限会随每次构建丢失，要用固定的开发证书测。
- macOS 15 以后系统会定期提示重新确认屏幕录制 **[未核实]**；有开发者报告 macOS 27 在“隐私与安全”里新增了独立的“远程桌面”条目，与“屏幕与系统音频录制”分开授权 [二手，单个帖子，[论坛](https://developer.apple.com/forums/thread/841530)]，**要在测试版出来后实测**，对远程桌面产品影响大。
- 被控端无人值守要用 LaunchDaemon 加用户会话里的 LaunchAgent，特权辅助要用 SMAppService，不在本报告范围 **[未核实]**。

## 10. 自动更新方案

### 10.1 候选

| 方案 | 许可证 | Rust 支持 | 增量 | 渠道 | 签名校验 | 平台 | 评价 |
|---|---|---|---|---|---|---|---|
| **Velopack** | MIT [源码] | 官方 Rust crate，语言表标 “Ready”；启动时先调 `VelopackApp::build().run()`，更新用 `UpdateManager`，`HttpSource` 指向更新地址，`check_for_updates` → `download_updates` → `apply_updates_and_restart` [官方 [Rust 入门](https://docs.velopack.io/getting-started/rust)] | 有增量包（“只下载变化部分”）[源码]；具体算法和效果 **[未核实]** | 有：`--channel`，生成 `releases.{channel}.json`，默认渠道名是 `win`、`osx`、`linux` [官方 [channels](https://docs.velopack.io/packaging/channels)] | **文档没写包签名校验 [未核实]**，要翻源码 | Windows、macOS、Linux | 首选 |
| **cargo-packager + cargo-packager-updater** | Apache-2.0 或 MIT [源码] | 纯 Rust，不依赖 Tauri（依赖列表里是 reqwest、semver、minisign-verify、tar）[官方 [docs.rs](https://docs.rs/cargo-packager-updater/latest/cargo_packager_updater/)] | **无** | 没有内置，用不同 endpoint URL 实现 | **minisign 签名**，`Config` 里放公钥，更新 JSON 里带 `.sig` 内容 | 安装包格式：app、appimage、nsis、wix；Windows 安装模式有 Passive（默认）、BasicUi、Quiet（Quiet 不能自己申请管理员权限） | 备选，签名校验现成 |
| `tauri-plugin-updater` | Apache-2.0 / MIT **[未核实]** | 需要 Tauri 运行时，能否脱离 Tauri 单独用 **[未核实]**，不查 | 无 | | minisign | | **不选**，cargo-packager-updater 本来就是它的独立版 |
| **Sparkle 2** | MIT **[未核实]** | 无 Rust 绑定，是 Obj-C 框架，嵌入 egui 程序要自己接 | 有（`generate_appcast` 生成 `.delta`）| 文档页未提 | **EdDSA（ed25519）**，可要求 appcast 本身也签名（`SURequireSignedFeed`）[官方 [Sparkle 文档](https://sparkle-project.org/documentation/)] | 仅 macOS | **不选**：只覆盖一个平台，要接 Obj-C，和 Windows 方案分裂 |
| **WinSparkle** | MIT [源码，[winsparkle](https://github.com/vslavik/winsparkle)] | C API，无官方 Rust 绑定 | 页面没提 | | EdDSA、appcast | 仅 Windows | **不选**，同上 |
| `self_update` crate | MIT **[未核实]** | 纯 Rust，从 GitHub Releases 下载替换 | 无 | | 默认不验签 **[未核实]** | | 不选，不能处理安装器和服务 |

### 10.2 Velopack 的要点和问题

- **构建**：需要 `.NET SDK 8` 装 `vpk`（`dotnet tool update -g vpk`），再 `vpk pack`；只是**构建时依赖**，产物里没有 .NET。要在 CI 里装，对 Linux runner 够用（Linux 和 macOS 上签 Windows 包需 1.3+）。
- **安装位置**：默认 `Setup.exe` 装到 `%LocalAppData%\{packId}`，不需要管理员权限；可用 `--installto` 改位置；也可以 `--msi --instLocation PerMachine` 装到 Program Files（要提权，写 HKLM）[官方 [Windows](https://docs.velopack.io/packaging/operating-systems/windows)]。
- **更新机制**：整个 `current` 目录被替换；换不动时，更新器会试图杀掉目录里的进程，还不行就问用户杀还是放弃；**如果占用进程是管理员身份，用户会看到错误，旧版本继续启动** [官方]。这对“以 SYSTEM 身份跑的服务”是个直接问题，见 §10.4。
- **托管**：“任何能放静态文件的地方，如云存储、GitHub Releases”[官方]；`vpk upload` 可直接传。
- **增量和渠道**：增量页没读到（文档站点的 deltas 路径本次没取到），渠道已核实。**分阶段放量（先 5% 用户）静态文件做不到**，要把 `releases.{channel}.json` 改成我们自己的动态接口，按安装 ID 的哈希决定给不给新版 **[猜测]**。
- **签名校验缺口**：Velopack 文档没写“应用更新前验证包的数字签名”；如果更新源被劫持，只靠 HTTPS 和文件哈希 **[未核实]**。**我们自己加一层**：发布时对每个更新包用 ed25519（minisign）离线签名，公钥写死在客户端；下载完、应用前用 `minisign-verify`（cargo-packager-updater 就用它）校验。Velopack 能不能在下载和应用之间插入这一步，需要 1 天的验证性实验 **[未核实]**。
- **macOS**：.app 包加签名加公证，参数见 §9.2。

### 10.3 更新源：GitHub Releases 还是自建 CDN

- **GitHub Releases**：免费、开源项目天然适配，Velopack 直接支持；缺点是国内访问慢甚至被阻断，且没有放量控制、下载统计有限。海外首发可以先用。
- **自建（对象存储 + CDN）**：可做动态清单、放量、统计、回滚（把清单指回旧版本）。Cloudflare R2 的特点是出站流量免费 **[未核实当前价格]**，适合安装包体积大、用户多的情形。
- **决定**：开源构建产物发在 GitHub Releases（也满足 Apache-2.0 项目的透明度）；客户端的更新清单和包走自己的域名，后端同时挂 R2 和 GitHub 作为回退。这个域名同时是 Azure 组织验证要求的“实体自有域名”。

### 10.4 无人值守（Windows 服务）怎么安全更新

问题：被控端以 SYSTEM 服务运行，二进制被服务进程占用；Velopack 默认的“每用户安装、杀占用进程”的模型（§10.2）不适合；更新期间远程会话会断，且更新失败会让机器永久失联（用户不在机器前），代价是普通桌面应用的几十倍。参考业界三种模式 [二手，[codenote](https://codenote.net/en/posts/windows-desktop-app-per-machine-silent-auto-update-patterns/)]：Chrome 的 Omaha（SYSTEM 服务代为更新，实现成本高）、Slack 的 MSIX（交给系统）、VS Code 系统安装版（交给 IT 重新分发，不自动更新）。MSIX 不适合服务（§8.4），我们走第一种的**精简版**：

1. **安装位置**：被控端用 **per-machine**（Program Files）安装，由 MSI 或 NSIS 做；界面客户端可以继续 Velopack 每用户安装。**两个安装通道分开**，不要让 Velopack 去动服务的文件。
2. **谁来更新**：服务自己（SYSTEM，有网络和权限）定时查清单，下载到 `%ProgramData%\PeerDesk\staging`，校验：minisign 签名（公钥内置）、Authenticode（`WinVerifyTrust`，并检查发布者与当前一致）、**版本号单调递增（防回滚攻击）**。
3. **安全时机**：没有活跃远程会话时更新；有会话时推迟，除非是紧急安全更新并提示对端。更新前通知会话对端“即将重启”，或在会话结束时更新。
4. **换文件**：服务把 `peerdesk-updater.exe`（有签名的小程序）复制到临时目录执行，它停止服务和会话辅助进程，用**目录重命名**切换（`current` → `previous`，`staging` → `current`），启动服务。或者用 MSI 静默升级（`msiexec /i … /qn`），由 Windows Installer 处理文件占用和失败回滚；cargo-packager-updater 的 Quiet 模式不能自己申请管理员权限，但从 SYSTEM 服务里调用本来就有权限 **[推断，未试]**。
5. **回滚**：新服务启动后 30 秒内要向更新器报“健康”（能连上信令服务、能抓屏）；超时就换回 `previous` 并启动旧版，同时上报失败。SCM 的恢复策略设成“失败自动重启”。保留前一个版本。
6. **版本偏差**：控制端和被控端会差几个版本，协议要有最小兼容版本号；更新清单里带 `min_supported`，低于它的强制更新，否则可以拖延。
7. **macOS 同理**：LaunchDaemon 自己下载、验签、替换 .app，由 launchd 重启；并且**签名身份不能变**（§9.3）。

这一块没有现成库，是一个独立任务，工作量见 §11。

## 11. 第二部分的建议与步骤

**推荐**：
- 代码签名：Windows 用 Azure Artifact Signing（有资格的话），不够资格就买 OV；macOS 用 Developer ID 加公证。**第一步是确认主体和地区**（§8.2），这决定后面所有账号怎么开。
- 自动更新：界面客户端用 Velopack；无人值守服务自写更新器；所有更新包额外加 minisign 签名；清单走自有域名。

| 步骤 | 内容 | 工作量（人日） |
|---|---|---|
| 1 | 确认法律主体和国家，开 Azure、Apple、域名和邮箱；提交 Azure 身份验证（等 1–30 个工作日，**先做**，它是关键路径） | 1（加等待） |
| 2 | CI：Windows 签名作业（OIDC、artifact-signing-action），只在 `[pkg]` 提交上跑；未签名构建在测试期先放行 | 1–2 |
| 3 | CI：macOS 签名、公证、装订（临时钥匙串、`notarytool`、stapler）、打 .app 和 dmg | 2–3 |
| 4 | Velopack 接入验证：Windows 与 macOS 打包、`HttpSource` 指向自有清单、渠道（stable/beta）、增量包实测、验证能否插入 minisign 校验（1 天实验） | 3–4 |
| 5 | 更新清单后端（放量、回滚、统计，可放在 `peerdesk-server` 或 Worker） | 2–3 |
| 6 | Windows 服务更新器：下载、验签、安全时机、换目录、健康检查、回滚，以及对应的 e2e 测试（模拟更新失败必须自动回滚） | 8–12 |
| 7 | macOS LaunchDaemon 更新（沿用步骤 6 的设计） | 3–5 |
| 8 | 文档：SmartScreen 与 Gatekeeper 提示说明、TCC 授权引导 | 1 |

e2e 新增门槛：更新成功率、更新耗时、**更新失败后的自动回滚（硬门槛）**、更新后 TCC 权限不丢（macOS，需在真机上测）。

## 12. 产品负责人需要买或注册的东西

| 项目 | 费用 | 说明 |
|---|---|---|
| 法律主体（如主体在大陆，需要境外公司） | **[未核实]**，几百到几千美元量级，取决于地区 | 新加坡、美国、英国、欧盟等都在 Azure 的组织名单内；同时满足 Apple 组织账号、收款和托管云的合规需要。先咨询会计师或律师，不要先买证书 |
| Apple Developer Program | 99 美元/年 | 组织账号要先申请 D-U-N-S（免费，最多 5 个工作日）；Developer ID 和公证包含在内 |
| Azure 订阅加 Artifact Signing | 约 9.99 美元/月（Basic）加 Azure 本身的信用卡 | 要有 Entra 租户、订阅；个人身份验证要用证件和手机；组织验证要域名邮箱和商业登记 |
| 备选：OV 证书 | 150–300 美元/年 | 需硬件令牌或 CA 的云 HSM 才能在 CI 里用，具体产品和价格 **[未核实]** |
| 域名和域名邮箱 | 约十几美元/年 | Azure 组织验证要求邮箱域名属于该实体；官网也是验证材料之一 |
| 更新分发（R2 / CDN） | 按用量 **[未核实]** | GitHub Releases 作回退 |
| minisign 更新签名密钥对 | 免费 | 私钥离线保管并备份两份；丢失意味着已安装的客户端无法再验证新包，需要走“密钥轮换”流程，要提前设计 |
| 测试硬件 | 视采购 | NVIDIA、AMD、Intel 各一块带 GPU 的 Windows 机器，一台 Apple Silicon Mac；或按小时租带 GPU 的云机器（价格 **[未核实]**）。硬件编码的冒烟测试没有这些机器就跑不出结论 |
| NVIDIA / AMD / Intel 开发者账号 | 不需要 | 头文件是公开的 MIT；NVENC 不需要单独授权；AMF、oneVPL 也都是 MIT 公开仓库 |
| 法务确认 | **[未核实]** | H.264/HEVC 硬件编码的专利费归属，OpenH264 自行编译二进制的专利费，EV/OV 之外的发布者名称显示 |

## 13. 没查到、需要后续核实的事项

1. Azure 的组织验证现在是否还有“成立满 3 年”之类的要求，以及 Premium 的价格。
2. 大陆主体能否注册 Apple 开发者账号（组织）、需要哪些材料。
3. MF 硬件 MFT 在 NVIDIA、AMD、Intel 上：动态改码率是否出 IDR、LTR 是否真遵守、是否支持帧内刷新、NVIDIA 是否注册 H.264 MFT（上面两份资料矛盾）。要在真机上跑 §6.2 的冒烟测试。
4. VideoToolbox 低延迟模式是否支持 HEVC、动态改码率是否出 IDR、LTR 是否可用，以及 macOS 托管 runner 上有无硬件编码。
5. Velopack：更新包是否做签名校验、能否在下载和应用之间插入校验、增量包算法与实测收益、更新器在 per-machine 和 SYSTEM 场景下的行为。
6. D3D11 的 VideoProcessor 与 NVENC/MF 之间共享纹理的细节（BindFlags、色彩空间设置、多线程串行化），D3D11 解码纹理到 wgpu 的零拷贝互操作。
7. macOS 15 及之后屏幕录制的定期重新确认，以及 macOS 27 新增的“远程桌面”TCC 项对我们的影响。
8. `screencapturekit`、`videotoolbox`、`shiguredo_nvcodec`、`win-native-media` 等社区 crate 的许可证和维护状态（决定是否参考）。
9. H.264/HEVC 硬件编码的专利费归属。
