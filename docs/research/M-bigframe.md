# M：窄带下大关键帧（整屏 IDR）怎么控——业界做法调研

日期：2026-11-25。只读调研，未改代码。源码均取自各仓库主干当天版本，函数名为准，行号会漂移。
标记：**已核实** = 读了源码或官方文档原文；**未核实** = 凭记忆或间接证据。

## 问题

窄链路（340 kbps–1 Mbps）上 720p 整屏关键帧约 120 KB（openh264 码率模式，QP 夹在 [12,42]），要发 1–5 秒，期间编码被阻塞，用户看到数秒冻结。
Chromium Remoting 的 `ShouldDropQualityForLargeFrame`（`remoting/protocol/webrtc_video_encoder_wrapper.cc`，`kMaxQuantizer=50`/`kMinQuantizer=10`，大帧时该帧 min=max=50，再靠 top-off 补清晰）已知，本文不重复。

## 1. libwebrtc：关键帧大小怎么限

### 1.1 VP8（libvpx）——有显式的关键帧上限
- `modules/video_coding/codecs/vp8/libvpx_vp8_encoder.cc`，`LibvpxVp8Encoder::MaxIntraTarget(optimalBuffersize)`：
  `targetPct = rc_buf_optimal_sz(600) * 0.5 * maxFramerate / 10`，下限 `minIntraTh = 300`（单位是"每帧平均带宽的百分比"，即关键帧最多约 3 倍平均帧大小）。结果存入 `rc_max_intra_target_`，在 `InitEncode`/`InitAndSetControlSettings` 里用 `VP8E_SET_MAX_INTRA_BITRATE_PCT` 设给 libvpx。
- 同文件 `Encode()`：屏幕共享、单时间层、**每次强制关键帧时**，把该上限临时改成 `forceKeyFrameIntraTh = 100`（`VP8E_SET_MAX_INTRA_BITRATE_PCT`）。即屏幕共享的关键帧被压到约 1 倍平均帧大小，靠 libvpx 把 QP 推高实现。
- 码控配置（`InitEncode`）：CBR；`rc_min_quantizer = 12`（屏幕共享，普通视频为 2）；`rc_max_quantizer = qp_max_`（默认来自 `inst->qpMax`，可被 field trial `vp8_qp_max` 覆盖）；`rc_undershoot_pct=100`、`rc_overshoot_pct=15`；`rc_buf_initial/optimal/sz = 500/600/1000 ms`；`rc_dropframe_thresh` 由 `FrameDropThreshold()` 给。另有 `kScreenshareMinQp = 15`，写入 `EncoderInfo.scaling_settings.min_qp`（只影响分辨率缩放判断，不是编码器限制）。
- 结论（已核实）：WebRTC 的做法是 **把关键帧限制成"几倍平均帧"，让码控用高 QP 去达成**；这和 Chromium Remoting 的"单帧 QP 拉满 + top-off"思路同源，只是 libvpx 内部帮做了。

### 1.2 H.264（openh264 封装）——基本没做限制
- `modules/video_coding/codecs/h264/h264_encoder_impl.cc`，`H264EncoderImpl::CreateEncoderParams`：
  - `iUsageType = SCREEN_CONTENT_REAL_TIME`（屏幕共享）/ `CAMERA_VIDEO_REAL_TIME`
  - `iRCMode = RC_BITRATE_MODE`；`iMaxBitrate = UNSPECIFIED_BIT_RATE`（注释引 crbug.com/webrtc/11543：WebRTC 的 max bitrate 与 openh264 的 iMaxBitrate 含义不同）
  - `bEnableFrameSkip = frame_dropping_on`；`uiIntraPeriod = key_frame_interval`
  - `uiMaxNalSize = 0`；切片：`SingleNalUnit` 模式用 `SM_SIZELIMITED_SLICE`（按 MTU 切），`NonInterleaved` 用 `SM_FIXEDSLCNUM_SLICE`（1 片）
  - **没有设置 `iMinQp`/`iMaxQp`**，走 openh264 默认（见 4.1：屏幕内容 [26,35]；普通 [12,42]，我们看到的 [12,42] 就是后者）。
- 没有任何关键帧大小上限（H.264 路径无 MaxIntraTarget 等价物）。H.264 在 WebRTC 里主要靠硬件编码和 `FrameDropper` 兜底。

### 1.3 VideoStreamEncoder 层
- `video/video_stream_encoder.cc`：
  - `FrameDropper`（`modules/video_coding/utility/frame_dropper.cc`）：漏桶，`Fill(frame_size, delta_frame)` 记录每帧大小，`Leak(fps)` 按码率排空，`DropFrame()` 决定丢**后续输入帧**。它对关键帧有 `key_frame_ratio_` 估计，并把"大于 3 倍平均 delta 帧的帧"（`kLargeDeltaFactor = 3`）摊到多帧里累计，以避免关键帧后连续丢帧。只在编码器没有 `has_trusted_rate_controller` 且未被 field trial `WebRTC-FrameDropper` 关闭时启用。
  - `DropDueToSize()`：仅在**开头**按"像素数 × 码率"判断，码率太低就丢初始帧（"Dropping frame. Too large for target bitrate."），不处理中途大帧。
- 没找到 `kMaxFrameSize` 之类的常量（**已核实：不存在于这几个文件**）。

**对我们**：WebRTC 对 H.264 + openh264 没有现成"大关键帧"办法；VP8 的 `MaxIntraTarget` 思路（关键帧大小=N×平均帧）可以直接借鉴为目标，但 openh264 没有对应参数，只能自己控 QP（见方案 A）。

## 2. Sunshine / Moonlight / Parsec

### 2.1 Sunshine（LizardByte/Sunshine，`src/nvenc/nvenc_base.cpp`、`src/video.cpp`、`src/rtsp.cpp`、`src/stream.cpp`）
- **NVENC 路径** `nvenc_base::configure_rate_control`：CBR；`gopLength = NVENC_INFINITE_GOPLENGTH`（只有首帧和请求时才出 IDR）；`zeroReorderDelay=1`；`lowDelayKeyFrameScale=1`；`tuningInfo = NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY`；
  **`vbvBufferSize = bitrate / framerate`（恰好一帧）**，可再加 `vbv_percentage_increase`。即 VBV ≈ 1 帧（方案 C 的原型）。已核实。
- **FFmpeg 路径**（AMF/QSV/VA-API/软编）`video.cpp`：`rc_max_rate = rc_min_rate = bit_rate`；`rc_buffer_size = bitrate / framerate`（一帧）；软编启用多切片或 HEVC 时放大到 1.5 帧（`bitrate / ((framerate*10)/15)`），注释写明"libx264 在缓冲太小时画质会严重劣化"。也有 `NO_RC_BUF_LIMIT` 标志位给个别编码器关掉。已核实。
- **帧内刷新**：`nvenc_base.cpp` 中只有当客户端要求（`client_config.enableIntraRefresh == 1`）且编码器支持 `NV_ENC_CAPS_SUPPORT_INTRA_REFRESH` 时才开：`enableIntraRefresh=1`、`intraRefreshPeriod=300`、`intraRefreshCnt=299`、`outputRecoveryPointSEI=1`、有能力时 `singleSliceIntraRefresh=1`。默认 `x-ss-video[0].intraRefresh=0`（`rtsp.cpp`）。Moonlight 侧由 moonlight-common-c `SdpGenerator.c` 在渲染器声明 `CAPABILITY_INTRA_REFRESH` 时发 `intraRefresh=1`。**哪些 Moonlight 客户端实际声明了该能力：未核实。**
- **丢包恢复**：优先 **参考帧失效（RFI）**，不行才 IDR。
  - 客户端：moonlight-common-c `VideoDepacketizer.c`/`RtpVideoQueue.c`，能力位 `CAPABILITY_REFERENCE_FRAME_INVALIDATION_{AVC,HEVC,AV1}`；丢帧时发 RFI 请求，甚至"投机 RFI"（`SPECULATIVE_RFI_COOLDOWN_PERIOD_US` = 5 分钟冷却）；无法 RFI 时 `LiRequestIdrFrame()`。
  - 服务端：`stream.cpp` 控制消息 `IDX_INVALIDATE_REF_FRAMES`（0x0301）→ `video.cpp` → `nvenc_base::invalidate_ref_frames(first,last)`：对区间每帧调 `nvEncInvalidateRefFrames`；区间 ≥ `ref_frames_in_dpb` 或失败则返回 false，由上层出 IDR。FFmpeg 路径的默认实现直接 `request_idr_frame()`（"Encoder doesn't support reference frame invalidation"），只有 NVENC 标了 `REF_FRAMES_INVALIDATION`。
- QP：Sunshine 的 NVENC 路径只设 `minQP`（`min_qp_h264` 等，保证画质下限），**不设 maxQP**——它是局域网串流，不面向窄带。

### 2.2 Parsec / GameStream
GameStream 是 Moonlight 协议的来源（IDR + RFI + 可选 intra refresh）。Parsec 自研协议，公开文档只有高层描述（动态码率、BUD 协议），**没有关键帧大小处理的公开资料：未核实**。

**对我们**：业界游戏串流的主流是"无限 GOP + 一帧 VBV + 丢包用 RFI/帧内刷新代替 IDR"，目标是**永远不出现巨型帧**。但它们面向 ≥ 10 Mbps 链路；我们的 340 kbps 下一帧 VBV 只有约 1.4 KB@30fps，整屏首帧在任何设置下都画不清，这是和游戏串流的本质区别（需要首帧"慢但清晰"，或先发低清再 top-off）。

## 3. FFmpeg / x264 / NVENC / QSV 的限帧大小旋钮

| 编码器 | 旋钮 | 状态 |
|---|---|---|
| x264 | `--vbv-maxrate` + `--vbv-bufsize`（kbit，≈ 一帧即严格低延迟）；`--intra-refresh`（"Use Periodic Intra Refresh instead of IDR frames"）；`--nal-hrd cbr`/`--filler`（硬 CBR）；`--crf-max`（CRF+VBV 时限 RF） | 已核实（x264.c 帮助文本）。x264 的 `qpmax` 之类用 `--qpmax` |
| NVENC | `rcParams.vbvBufferSize`（bit，需 `NV_ENC_CAPS_SUPPORT_CUSTOM_VBV_BUF_SIZE`）；`lowDelayKeyFrameScale`（"单帧 VBV + CBR 时 I 帧与 P 帧比特比，低延迟 tuning 默认 2，超低延迟默认 1"）；`enableMaxQP`+`maxQP`；`enableIntraRefresh`/`intraRefreshPeriod`/`intraRefreshCnt`（H.264/HEVC/AV1 配置里都有）；`NV_ENC_PIC_PARAMS_H264.forceIntraRefreshWithFrameCnt`（逐帧触发一次刷新）；`NvEncInvalidateRefFrames`（`NV_ENC_CAPS_SUPPORT_REF_PIC_INVALIDATION`）；`NvEncReconfigureEncoder`（动态改码控，不带 `resetEncoder`/`forceIDR` 就不重置） | 已核实（nv-codec-headers `nvEncodeAPI.h`）。**NVENC 没有"最大帧字节数"字段**（未在头文件找到 maxFrameSize）；能做的是 VBV + maxQP + `qpDeltaMap`（逐宏块 QP 偏移，需 `qpMapMode`） |
| QSV（oneVPL/MFX，FFmpeg `libavcodec/qsvenc.c`） | `mfxExtCodingOption2.MaxFrameSize`（FFmpeg 选项 `max_frame_size`）；`mfxExtCodingOption3.MaxFrameSizeI/MaxFrameSizeP`（I/P 分开）；`IntRefType`/`IntRefCycleSize`/`IntRefQPDelta`（帧内刷新，选项 `int_ref_type`）；`MinQPI/MaxQPI/MinQPP/MaxQPP`；`LowDelayBRC`；`MaxKbps`/`BufferSizeInKB`。`qsvenc.c` 里 `UPDATE_PARAM(q->old_max_frame_size, ...)`、`old_qmax` 说明这些可在运行时通过 `MFXVideoENCODE_Reset` 更新 | 已核实。Reset 是否强制 IDR：未核实 |
| VA-API | 见 4.4 | 已核实 |

## 4. 各平台编码器 API 需要的旋钮

### 4.1 openh264（我们当前用的软编）
- `codec_app_def.h`：`SEncParamExt` 有 `iMinQp/iMaxQp`、`bEnableFrameSkip`、`iMaxBitrate`、`uiMaxNalSize`、`uiIntraPeriod`、LTR（`bEnableLongTermReference`、`iLTRRefNum`、`iLtrMarkPeriod`）。`ENCODER_OPTION` 里有 `ENCODER_OPTION_SVC_ENCODE_PARAM_EXT`、`ENCODER_OPTION_BITRATE`、`ENCODER_OPTION_MAX_BITRATE`、`ENCODER_OPTION_RC_FRAME_SKIP`、`ENCODER_OPTION_LTR`、`ENCODER_LTR_RECOVERY_REQUEST`、`ENCODER_OPTION_DELIVERY_STATUS` 等。**没有 QP 范围的单独 option，也没有任何 intra refresh 字段/选项**（对 `codec_app_def.h`、`encoder_ext.cpp`、`welsEncoderExt.cpp` 全文搜 intra refresh 无结果；已核实：**openh264 不支持帧内刷新**）。
- **运行时改 QP 范围：不行（已核实）。**
  - `codec/encoder/plus/src/welsEncoderExt.cpp`，`CWelsH264SVCEncoder::SetOption` 的 `ENCODER_OPTION_SVC_ENCODE_PARAM_EXT` 分支：`ParamTranscode` → `WelsEncoderParamAdjust`。
  - `codec/encoder/core/src/encoder_ext.cpp`，`WelsEncoderParamAdjust`：`bNeedReset` 的条件列表（分辨率、层数、LTR、线程数、背景检测、自适应量化、SPS/PPS 策略、profile/level、切片、帧率比例等）**不含 iMinQp/iMaxQp**；`!bNeedReset` 分支只逐字段拷贝 fMaxFrameRate、uiIntraPeriod、iNumRefFrame、iRCMode、iTargetBitrate、每层 iSpatialBitrate/iMaxSpatialBitrate、iDLayerQp 等，**没有 `pOldParam->iMaxQp = pNewParam->iMaxQp`**。
  - QP 范围只在 `codec/encoder/core/src/ratectl.cpp` 的 `RcInitSequenceParameter`（`pWelsSvcRc->iMinQp = pSvcParam->iMinQp` 等）里读入码控。所以只有走"重建编码器"（`WelsUninitEncoderExt` + `WelsInitEncoderExt`，需要触发 reset）才会生效，而重建就是新序列，下一帧必为 IDR。**即无法"不触发 IDR 地改 QP 范围"。**
  - 另外 `ParamValidation`：iMinQp/iMaxQp ≤ 0 时，屏幕内容用 `MIN_SCREEN_QP=26 / MAX_SCREEN_QP=35`，其他用 `GOM_MIN_QP_MODE=12 / MAX_LOW_BR_QP=42`（`rc.h`），再夹到 [12,51]。注意 **iMinQp 下限被夹到 12**。
- 可行的绕法（未实测，**未核实**）：
  1. 初始化时就把 `iMaxQp` 设到 51（上限放开），平时靠码率/`ENCODER_OPTION_BITRATE`（可运行时改，不重建，已核实）控制；大帧时用**临时压低 `ENCODER_OPTION_BITRATE`** 逼码控抬 QP（`SetOption(ENCODER_OPTION_BITRATE)` 在 `welsEncoderExt.cpp` 里直接改 `pSvcParam->iTargetBitrate` 并重算，不 reset）。代价：码控响应有延迟，QP 是渐进抬高，首个 IDR 仍可能偏大。
  2. 把 `iUsageType` 保持为 `SCREEN_CONTENT_REAL_TIME`（运行中不允许改，`WelsEncoderParamAdjust` 返回 `ENC_RETURN_UNSUPPORTED_PARA`）。
  3. 想要"每帧指定 QP"在 openh264 里没有 API（没有 per-frame QP 输入），做不到 Chromium 那种逐帧 min=max=QP。
- 帧内刷新替代：openh264 没有，但有 LTR 恢复（`ENCODER_LTR_RECOVERY_REQUEST`）和 `ForceIntraFrame(bIDR)`（只支持 IDR）。LTR 在 `bEnableLongTermReference` 变更时要重建（见上 bNeedReset 列表）。

### 4.2 VideoToolbox（macOS）
- `kVTCompressionPropertyKey_MaxAllowedFrameQP`：macOS **12.0+**；`MinAllowedFrameQP`：macOS **13.0+**（Apple 文档页 JSON，已核实）。`kVTCompressionPropertyKey_DataRateLimits`：macOS 10.8+，"任意连续时间窗口内压缩数据不超过 N 字节"，可设 1–2 组（已核实）。
- FFmpeg `libavcodec/videotoolboxenc.c` 用动态符号加载（`GET_SYM(...MaxAllowedFrameQP...)`）并把 `avctx->qmin/qmax` 映射到这两个键；DataRateLimits 用 `[bytes_per_second, 1]`。WebRTC `sdk/objc/components/video_codec/RTCVideoEncoderH264.mm` 也设 DataRateLimits，限制 = 平均码率 × 1.5（`kLimitToAverageBitRateFactor`）。
- 这些属性能否在会话中途改（`VTSessionSetProperty`）：WebRTC 对 AverageBitRate/DataRateLimits 是运行中改的（已核实）；MaxAllowedFrameQP 中途改是否生效、是否强制关键帧：**未核实**。VT 没有 intra refresh 属性（**未核实**，未在 FFmpeg/WebRTC 里见到使用）。逐帧控制只有 `kVTEncodeFrameOptionKey_ForceKeyFrame`，没有逐帧 QP。
- 要把 DataRateLimits 设成"一帧"量级（如 1 秒窗口太宽）：窗口时长可设小数秒，但太小 VT 行为未核实。

### 4.3 Media Foundation（Windows）
- `CODECAPI_AVEncVideoMaxQP`（Win 8.1+）：文档明确写 **"static property，只能在编码会话开始前设置"**（已核实）。`CODECAPI_AVEncVideoMinQP`（Win 8+，范围 0–51，所有码控模式有效）：文档未写 static/dynamic，**未核实**。
- `CODECAPI_AVEncCommonBufferSize`：read/write，仅 CBR/VBR 有效；`AVEncCommonMeanBitRate`/`MaxBitRate` 在 MS H.264 编码器上可运行时改（文档列为必需属性，是否动态：**未核实**）。`AVEncVideoForceKeyFrame`：只作用于下一帧，用完自动复位。`AVLowLatencyMode`：无重排。
- `AVEncVideoEncodeQP`（逐帧 QP）仅适用质量码控模式，范围 16–51。`AVEncVideoEncodeFrameTypeQP`、LTR（`AVEncVideoLTRBufferControl` 静态、`MarkLTRFrame`/`UseLTRFrame`）可用于恢复（文档列为可选，HCK 测试）。
- MS 软编没有 intra refresh 属性（**未核实**）；硬件 MFT（NVENC/QSV/AMF 的 MFT 封装）按厂商实现，能力用 `ICodecAPI::IsSupported` 查。
- 结论：Windows 上"运行时抬 MaxQP"走 MF 不可靠（MaxQP 是静态的），要么初始就设宽（如 51）后靠码率控制，要么直接走 NVENC/QSV/AMF 原生 API。

### 4.4 VA-API（Linux，libva `va/va.h`）
- 码控：`VAEncMiscParameterRateControl` 含 `min_qp`（"到达下限时输出码率可能低于目标"）、`max_qp`（"到达上限时输出码率可能超过目标"），0 表示忽略；misc buffer 可随帧提交，因此理论上可逐帧改（驱动是否都支持：**未核实**）。
- 最大帧大小：`VAConfigAttribMaxFrameSize`，位 `max_frame_size==1` 才支持；用 `VAEncMiscParameterTypeMaxFrameSize`/`VAEncMiscParameterBufferMaxFrameSize{max_frame_size (bit)}` 设，"编码器会尽量保证帧不超过，超了会在 `VACodedBufferSegment.status` 里置 `VA_CODED_BUF_STATUS_FRAME_SIZE_OVERFLOW`"；`multiple_pass==1` 时可用 `VAEncMiscParameterTypeMultiPassFrameSize` 多遍重编码。已核实（头文件注释）。这是目前所有平台里**唯一一个标准化的"逐帧最大字节"接口**（与 QSV `MaxFrameSize` 同类）。
- 帧内刷新：`VAConfigAttribEncIntraRefresh`（`VA_ENC_INTRA_REFRESH_ROLLING_COLUMN/ROW`、`ADAPTIVE`、`CYCLIC` 等）+ `VAEncMiscParameterRIR`（`enable_rir_column/row`、`intra_insertion_location`、`intra_cycle_size`、`qp_delta_for_inserted_intra`）。已核实头文件；驱动支持度（Intel iHD 与 Mesa radeonsi 各异）：未核实。

## 5. RustDesk（`libs/scrap/src/common/{vpxcodec,hwcodec,codec}.rs`、`src/server/video_service.rs`）
- **VP9（libvpx）**：`calc_q_values(quality ratio)`：`ratio*100` 夹到 200，`t = b/200`，`q_min = 36→0`、`q_max = 56→37` 线性插值（所以 "Low/speed" 档 `BR_SPEED=0.5` → q 约 [27,51]；balanced `0.67`；best `1.5`）。CBR，`rc_undershoot_pct=95`，`rc_dropframe_thresh=25`。`set_quality()` 运行时直接改 `rc_min/max_quantizer` 和 `rc_target_bitrate`（libvpx 的 `vpx_codec_enc_config_set` 可不换序列）。
- **关键帧**：`keyframe_interval` 为 None 时 `kf_mode = VPX_KF_DISABLED`（注释 "reduce bandwidth a lot"），只有首帧和请求时出关键帧；只有录制才设 240（`video_service.rs`）。硬编 `DEFAULT_GOP = i32::MAX`。码率自适应：`VideoQoS` + `check_qos()` 改 `set_quality`，**没有针对大关键帧的专门处理**（没发现 max frame size/VBV 一帧之类，已核实 `vpxcodec.rs`/`hwcodec.rs`）。
- 新连接/切分辨率 = `bail!("SWITCH")` 重建编码器，必然出整屏关键帧；窄带下同样会卡，靠 VP9 的 q_max 56 压体积。
- 对我们的意义：RustDesk 在窄带靠"VP9 + 高 q_max（最高 56/63）+ 不定期关键帧"，没有秘诀；其 q_max 高于 H.264 的 51 上限，属于编码格式红利。

## 6. 汇总：三个方案在各编码器上的可行性

| | A 逐帧抬 QP + top-off（Chromium 式） | B 帧内刷新 | C VBV≈1 帧 + 高 max QP |
|---|---|---|---|
| openh264 | **逐帧 QP 无 API；运行时改 QP 范围无效（改了要重建→IDR）**。只能"初始 maxQP=51 + 临时压 `ENCODER_OPTION_BITRATE`"间接实现（未核实） | **不支持** | 无 VBV 大小参数；只有 `iMaxBitrate` + 码率；可用但粗糙 |
| x264（对照/软编备选，GPL 不能进产品） | `--qpmax`，需重配 | `--intra-refresh` | `--vbv-maxrate/--vbv-bufsize` |
| NVENC | `NvEncReconfigureEncoder` 改 maxQP（不带 reset）；`qpDeltaMap` 逐宏块偏移 | `enableIntraRefresh` + `forceIntraRefreshWithFrameCnt`（逐帧触发） | `vbvBufferSize`（Sunshine 在用）+ `lowDelayKeyFrameScale` |
| QSV | `MinQP*/MaxQP*` 经 Reset 更新 | `IntRefType` | `MaxFrameSize/MaxFrameSizeI` + `BufferSizeInKB` |
| VA-API | misc RC buffer 逐帧提交 `max_qp` | `VAEncMiscParameterRIR`（驱动相关） | `VAEncMiscParameterBufferMaxFrameSize`（最直接） |
| VideoToolbox | `MaxAllowedFrameQP`（macOS 12+）中途改：未核实 | 未发现 | `DataRateLimits`（10.8+） |
| Media Foundation | `MaxQP` 静态，不可运行时改 | 无 | `AVEncCommonBufferSize`，read/write |

## 建议

1. **短期（跨平台、可落地）= 方案 A 的变体，在传输层/编码器外做**：openh264 无逐帧 QP，所以不依赖它改 QP。改为：
   - 大帧**预测**放在编码前（用 Chromium 的 `ShouldDropQualityForLargeFrame` 同款判据：更新面积 + 经验字节/像素 + 当前可用码率 × 帧间隔）；
   - 命中时**不发整屏高清 IDR**：先对该帧降分辨率或把码率目标临时压低（`ENCODER_OPTION_BITRATE`，不触发 IDR，已核实）出一个小的"粗糙帧"，随后静止 top-off（提高码率/补发清晰帧）。分辨率切换要走"先接后断"。
   - 把 openh264 初始化的 `iMaxQp` 放到 51（现在 42），`iMinQp` 保持 12（被夹到 ≥12）。
   实测指标：首帧体积、冻结时长、静止后变清晰时间，进 e2e。
2. **中期 = 方案 C（VBV≈1 帧）** 用于硬件编码器：NVENC `vbvBufferSize`、QSV `MaxFrameSize`、VA-API `MaxFrameSize`、VT `DataRateLimits`，这些都是原生支持、不用重建。游戏串流（Sunshine）已验证 NVENC 路径稳定；但窄带下一帧 VBV 太小，要给**首帧/整屏切换**放宽（像 Sunshine 放大 1.5 倍那样，或只对 IDR 放大 `lowDelayKeyFrameScale`）。
3. **方案 B（帧内刷新）作为丢包恢复手段**，不是窄带首帧方案：它能避免"丢包→整屏 IDR"，Sunshine 优先用 RFI，次选 IDR。openh264 不支持，硬件侧 NVENC/QSV/VA-API 支持，适合配合硬编落地；丢包恢复优先级建议 RFI/LTR > 帧内刷新 > IDR（与 CLAUDE.md "高丢包先加纠错" 的原则一致）。
4. 不建议在 Windows 上走 Media Foundation 去运行时抬 MaxQP（静态属性）；窄带场景用原生 SDK 路径。
5. 待实测（均为未核实）：openh264 压低 `ENCODER_OPTION_BITRATE` 后第一帧 IDR 的实际体积变化；VT `MaxAllowedFrameQP` 中途修改；QSV `MFXVideoENCODE_Reset` 是否强制 IDR；VA-API 各驱动的 MaxFrameSize/RIR 支持。

## 实测补充（2026-11-25，T6 落地之后）

**VideoToolbox 的 DataRateLimits 管不住关键帧。** CI 的 Apple 芯片虚拟机上，300 kbps、720p 文字画面，会话的第一个关键帧
94786 字节（100 ms 的量是 3750 字节，超出 25 倍）。设置属性的返回值原来被忽略，不知道是系统拒绝了还是接受了不管；现在记下结果码
（不为 0 时日志写一次），测试 `vt_keyframe_under_rate_limits` 把“产品的两组 / 只有 100 ms / 只有 1 秒（WebRTC）”× 低延迟码控开关
各量一遍（第一帧和第 30 帧的关键帧），下一次 Mac CI 出数字。

CI 实测（2026-11-25，b7acd9b）：三种设法系统都接受（结果码 0），关键帧大小完全一样（低延迟码控 94786 字节；不开低延迟
85522 / 112058 字节），VideoToolbox 根本不拿 DataRateLimits 约束关键帧。另一个发现：低延迟码控在 300 kbps 时，第 30 帧强制要的
关键帧没有出来（31 帧共 97 KB，码控把那一帧丢了），20 Mbps 时两个都出来了——窄带上 Mac 被控端丢包后要的关键帧可能被丢，
恢复可能卡住，要专门查（#46：低码率时用 `MinAllowedFrameQP` 限关键帧，或者窄带改用我们自己控 QP 的软件编码）。
第二轮 CI 实测（d76d63f，300 kbps、90 帧、第 0/30/60 帧要关键帧）：

| VideoToolbox 设置 | 丢帧 | 关键帧（帧号, 大小） | 3 秒总量 |
|---|---|---|---|
| 低延迟码控 | 73/90 | (0, 95 KB) (32, 95 KB) (60, 95 KB) | 291 KB（目标的 2.6 倍） |
| 低延迟 + MinAllowedFrameQP 48 | 78/90 | 48 KB ×3 | 147 KB |
| 普通码控 | 0 | 86 / 112 / 57 KB | 319 KB |
| 普通 + MinAllowedFrameQP 48 | 0 | 48 KB ×3 | 168 KB |

强制关键帧没有真丢，只是被码控推迟（第 30、31 帧丢了，第 32 帧出关键帧）；原来的测量只编到第 30 帧，看起来像丢了。
真正的问题是窄带上 VideoToolbox 不合适：码控丢掉八成的帧（Mac 上就是卡顿），关键帧最小也要 48 KB（300 kbps 上 1.3 秒），
而软件编码自己控 QP，同样的画面能装进 100 ms。

做法（#46）：窄带改用软件编码。按当前码率估计硬件关键帧要传多久（按实测的每百万像素字节数，没测到时按 VT 的
100 KB/MP），超过 500 ms 就在本来要出关键帧时（丢包恢复、尺寸变了）换成软件；低于 250 ms、画面停下补完清晰时换回
硬件（出关键帧）。720p 大约 1.5 Mbps 以下用软件、3 Mbps 以上换回硬件。Chrome 远程桌面对屏幕内容也是用软件编码
（VP8/VP9）。只在硬件编码器开着时才往软件换（没有硬件的机器不判断）。

**大帧规则只在“码率估计新鲜”时放宽。** e2e `drop-while-idle`：链路在画面静止时从 20 降到 2 Mbps，发送端没有任何数据能察觉，
切窗口时按 17.5 Mbps 的 100 ms 额度编了 222 KB，2 Mbps 上等了 1226 ms（T6 之前同一场景 183 KB、990 ms，只是刚好没越线）。
照 TCP 的拥塞窗口验证（RFC 2861，Linux `tcp_slow_start_after_idle`：闲置后不信闲置前的窗口），码率估计超过 2 秒没被测量证实
（包串速率，或送达速率到了估计的一半）就算过期，过期时关键帧和静止后第一帧最多给一帧的预算，先发粗的再补清晰：
222 KB → 159 KB，1226 → 859 ms。根本解决要靠静止时的低成本探测（每秒几个背靠背的包测瓶颈），列在 #26。

**画面在变时 QP 能降多低，要看“这块区域突然整个换掉”装不装得进时限。** 蜂窝网络上拖动窗口（变化面积约 19%，
增量帧靠运动补偿很小）时，按“上一帧大小 × 比例”把 QP 一路降到 12；马上切换窗口，同一块区域换成新内容，按 QP 12
编了 203 KB，最长等待 546 ms，停下后 1/5 没在 0.5 秒内清晰。试过的三种改法（都有实测）：
- 每帧都按变化面积估计大小（Chromium `ShouldDropQualityForLargeFrame` 的思路）：这个场景好了，但窄带上把运动补偿的
  增量帧估得太大（视频、拖动之后停下的画面 PSNR 34.5 → 18.6 dB，ms-05729 冻结 7 → 20 次），不行。
- 面积突然变大 2 倍以上才按面积估计：拖动时面积本来就不小，切换时没到 2 倍，没抓住。
- 小面积变化一律不低于 QP 22：滚动一个小区域（面积 5%）也被限住，2 Mbps 上停下后 569 ms 才清晰，不行。

最后的规则：只在按上一帧推算要降到 22（关键帧下限）以下时，检查“变化的这块区域整个换成新内容”按这个 QP 多大，
装不进大帧时限就把 QP 提到刚好装得进（最多提到 22）。窄带上 QP 本来就在 22 以上，完全不受影响；宽带上小块变化
照样能到 12。office-cellular 最长等待 546 → 210 ms、5 次停下都按时清晰。另外加了兜底：整屏变化（面积 ≥ 一半）编出来
超过时限 2 倍时，按实际大小推 QP 重编成关键帧（照 libvpx 屏幕内容实时模式 `vp9_encodedframe_overshoot`；openh264 编完
参考帧就更新了，只能重编成关键帧）。openh264 自己检测到场景切换出的关键帧用的也是我们给的 QP，同样处理。

**单条真实轨迹的结果是混沌的。** 过期阈值先用 1 秒时，微软轨迹 ms-05729 上让 21.6 秒的一帧从 12819 B 变成 12043 B（差 6%），
之后随机丢包落在不同的帧上，23.3 秒碰上一次补不回来的丢包、要了关键帧，冻结 7 → 10 次。判断改动好坏要看全部轨迹的总体，
单条轨迹的门槛只能防大的倒退。阈值定 2 秒：办公操作之间 1 秒的停顿很常见，不该每次都先粗后清。
最终版本在 8 条微软轨迹上的结果见提交说明。
