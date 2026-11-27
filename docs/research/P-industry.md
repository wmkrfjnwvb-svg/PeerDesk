# 业内资料：帧率稳定、QSV MFT、GPU 读回、软编帧率、局域网带宽探测

调研日期：2026-11-27。只整理业内做法和出处，不评价我们的代码。

**说明（请先看）**
- 源码和文档是用抓取工具读的，工具会先用小模型摘要页面。文中引号里的内容是摘要里给出的原文片段，
  数值和函数名我认为可信，但不是逐字比对过。要当作决策依据的，请打开 URL 再核一遍。
- WebRTC 的源码 URL 用的是 `chromium.googlesource.com/external/webrtc/+/refs/heads/main/...`。
  该镜像可能和 `webrtc.googlesource.com/src` 的主线有细微差别。
- 标"未核实"的是搜索结果里的二手信息或没读到原文的。
- Parsec 的帮助中心（support.parsec.app）抓取返回 403，那一部分只有搜索摘要，标了未核实。

---

## 主题 1：帧率怎么保持稳定

### 要点

1. **Chrome Remote Desktop（CRD）：固定间隔的采集调度，用"上一帧采集完成"做背压。**
   - `WebrtcFrameSchedulerConstantRate` 类注释："captures frames at a fixed rate"，使用 `SetMaxFramerateFps()` 给的最大帧率。
   - 采集间隔 = `Hertz(max_framerate_fps) - post_task_adjustment_`，下限 1 ms；帧率上限 1000；帧率 0 表示不采集。
   - 背压只有一个：`frame_pending_`。`CaptureNextFrame()` 置 true，`OnFrameCaptured()` 置 false 并调 `ScheduleNextFrame()`；
     有帧在途时不调度，在途超过 1 秒只打错误日志。
   - `GetPostTaskAdjustment()`：处理器数 < 16 返回 0 ms；>= 16 返回 2 ms。注释："We've observed the encoding rate in the client as being a couple of frames lower than the target."
   - `BoostCaptureRate(capture_interval, duration)`：在 `boost_window_` 内用更短的采集间隔，下限 1 ms，
     注释："the capture pipeline starts acting weird when we try to capture at sub-millisecond intervals"。
   - 这个文件里没有编码忙、带宽、关键帧、静止屏幕的处理（这些在别的文件里，我没读到，见"没查到"）。
   - 出处：
     - https://raw.githubusercontent.com/chromium/chromium/main/remoting/protocol/webrtc_frame_scheduler_constant_rate.cc
     - https://raw.githubusercontent.com/chromium/chromium/main/remoting/protocol/webrtc_frame_scheduler_constant_rate.h
   - `WebrtcVideoStream::SetTargetFramerate()` 通过 RTP sender 设置 `encoding.max_framerate`；`SetMaxFramerateFps()` 调 `capturer_->SetMaxFrameRate()`。
     出处：https://raw.githubusercontent.com/chromium/chromium/main/remoting/protocol/webrtc_video_stream.cc

2. **WebRTC：帧率由"降级偏好（degradation preference）"决定，降 2/3、升 3/2，没有固定计时器。**
   - 三种偏好：`MAINTAIN_FRAMERATE`（降分辨率）、`MAINTAIN_RESOLUTION`（降帧率）、`BALANCED`（二者都可）。
     出处：https://webrtc.googlesource.com/src/+show/main/video/g3doc/adaptation.md （文档摘要："Quality can be adapted along two dimensions, frame rate and resolution"）
   - `VideoStreamAdapter`：帧率步长 "For frame rate, the steps we take are 2/3 (down) and 3/2 (up)."；`kMinFrameRateFps = 2`；
     分辨率步长 3/5（降）、5/3（升）。
     出处：https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/call/adaptation/video_stream_adapter.cc
   - `MAINTAIN_RESOLUTION` 下 `GetAdaptationUp` 只升帧率；`BALANCED` 先升帧率，再升分辨率。
   - 屏幕共享：`DegradationPreferenceManager` 对屏幕共享且偏好为 BALANCED 的情况，实际按 MAINTAIN_RESOLUTION 处理（"switches to maintaining resolution"）。
     出处：https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/video/video_stream_encoder.cc
     （搜索摘要也提到旧版本同样如此；摘要来自 webrtc.googlesource.com/src 的历史版本，未核实具体提交。）

3. **WebRTC 编码太慢时：先靠"编码占用率"检测降级；编码队列满则直接丢帧。**
   - `OveruseFrameDetector`（`EncodeUsageResource` 的底层）：占用率 = 滤波后的编码耗时 / 帧间隔 × 100。
     默认高阈值 85%，低阈值 `(85-1)/2 = 42%`；`min_frame_samples = 120`，`min_process_count = 3`，
     `high_threshold_consecutive_count = 2`，`frame_timeout_interval_ms = 1500`。
     首次检查在开始后 100 ms，之后每 5000 ms 检查一次（`kCheckForOveruseIntervalMs`）。
     出处：https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/video/adaptation/overuse_frame_detector.cc
     （头文件另有私有成员 `last_rampup_time_ms_`、`in_quick_rampup_`、`current_rampup_delay_ms_`，说明升档有回退延迟，
     但具体数值我没读到，未核实。https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/video/adaptation/overuse_frame_detector.h ）
   - `VideoStreamEncoder::OnFrame`：编码器前已有一帧在等时，新帧被丢弃，原因 `kEncoderQueue`；
     `DropDueToSize()`：帧大小超出目标码率时丢帧。
     出处：https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/video/video_stream_encoder.cc （该文件只读了前 100000 字符）
   - `ResourceAdaptationProcessor`：多个资源（CPU、QP、码率）同时限制时，升档要求"所有最受限资源都发出 underuse"；
     文件本身没有计时或冷却逻辑。
     出处：https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/call/adaptation/resource_adaptation_processor.cc
   - 升档的闸门：`GetAdaptationUp` 里 "Don't adapt if we're awaiting a previous adaptation to have an effect."，
     并要求所有 `VideoAdaptationConstraint` 同意；`BalancedConstraint` 在 BALANCED 下检查目标码率是否够新帧率/分辨率。
     出处：同上 video_stream_adapter.cc；https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/video/adaptation/balanced_constraint.cc

4. **Sunshine：捕获线程不限速，编码侧设"最低帧率"，静止屏幕靠超时重编。**
   - 编码循环 `images->pop(max_frametime)`，`max_frametime = 1000 / minimum_fps_target`；注释："Encode at a minimum FPS to avoid image quality issues with static content."
   - `minimum_fps_target` 默认 0，表示取客户端帧率的一半；配置文档："This setting controls the lowest effective framerate a stream can reach."；
     另一处："Sunshine tries to save bandwidth when content on screen is static or a low framerate."
   - 捕获线程用 critical 优先级；捕获图像池 12 张（`capture_buffer_size = 12`），空闲图像 3 秒后回收。
   - 编码单帧失败（"Could not encode video packet"）只关该会话；显示切换才触发 reinit。
   - 出处：
     - https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/video.cpp
     - https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2configuration.html
   - DDA 路径：`dup.next_frame(frame_info, timeout, ...)`，超时返回 `capture_e::timeout`，由调用方决定怎么处理；
     光标变化但桌面没变时，复用已存的中间纹理合成光标。
     出处：https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/platform/windows/display_vram.cpp

5. **RustDesk：服务端决定帧率，延迟驱动，带明确的滞回和恢复规则（最完整的公开规则之一）。**
   - 常量：`FPS=30, MIN_FPS=1, MAX_FPS=120, INIT_FPS=15, MIN_AUTO_FPS=5`；观看端只上报上限（`user_custom_fps`），
     `adjust_fps()` 取所有用户中最小的 `delay.fps`，再受 `highest_fps()` 限制。
   - 延迟分档（`user_network_delay`）：平均延迟 < 50 ms 每次 +1（连续好样本可 +5）；< 100 ms 仅在变好时 +1；< 150 ms 保持；>= 150 ms 按比例下调。
   - 滞回：坏样本（超出 150 ms）需要 3 次确认才降，除非超出 >= 1000 ms 或上次恢复失败；
     普通降幅约 1/5，严重降幅 1/2。
   - 恢复：每个好应答 `current + max(current/10, 1)`；两个好应答后回到拥塞前帧率 `fps_before_congestion`；
     新连接启动 1 秒内固定 `INIT_FPS`，之后 `accelerate_startup` 在超出 < 50 ms 时翻倍直到上限。
   - 超时刹车：应答超过 2000 ms 视为停滞，每多一秒帧率减半（最多 1/32），下限 `MIN_AUTO_FPS`。
   - 画质比例 `adjust_ratio` 每 3 秒一次（`ADJUST_RATIO_INTERVAL = 3`），降需要连续 2 次坏应答，升需要画面有动态且没有人超阈值。
   - 出处：https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/server/video_qos.rs
   - 采集循环：`check_qos` 每轮取 `video_qos.spf()`，一帧耗时不足 `spf` 就 sleep 补足；
     `WouldBlock`（屏幕没变）时，若编码器不是 latency_free，会把缓存的 YUV 重编，最多 `repeat_encode_max = 10` 次，再走 `static_refresh`（该模块源码没读到）。
     DXGI 访问丢失：10 秒窗口内最多重启 3 次（`DXGI_RECOVERY_LIMIT/WINDOW`），之后回退 GDI；
     硬编码连续失败 3 次（Android 9 次）就 `encoder.disable()` 并切换（报 "SWITCH"）。
   - 出处：https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/server/video_service.rs

6. **Parsec / Moonlight：帧率主要由用户/客户端设定，不是自适应；Parsec 优先级写明"延迟 > 帧率 > 画质"。**
   - Parsec 博客："we prioritize latency, frame rates, and then video quality, in that order."
     出处：https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007
   - Parsec 默认开 V-sync，"but drops accumulated frames if the frame rate on the server is slightly higher"。
     出处：https://parsec.app/blog/description-of-parsec-technology-b2738dcc3842
   - Parsec 帮助中心里 FPS 设置的描述、"高于 60 可能不稳定"：来自搜索摘要，原页面 403，未核实。
     https://support.parsec.app/hc/en-us/articles/360001562772-All-Advanced-Configuration-Options
   - Moonlight：`m_StreamConfig.fps = m_Preferences->fps`，客户端设定；显示刷新率选能被 fps 整除的。
     出处：https://raw.githubusercontent.com/moonlight-stream/moonlight-qt/master/app/streaming/session.cpp

### 可以直接借鉴的做法

- 帧率调整的"信号"分两层：慢信号（编码占用率 = 编码耗时/帧间隔，窗口平均 + 连续几次才触发，WebRTC 默认 85% 降、42% 升）
  和快信号（队列满/应答超时，RustDesk 的停滞刹车、WebRTC 的 `kEncoderQueue` 丢帧）。
- 降快升慢：WebRTC 降一步 ×2/3，升一步 ×3/2 且要等上一次调整见效；RustDesk 降需 3 次确认，升每次 +10%，两次好应答回到拥塞前值。
- 编码太慢时主流做法是"丢新帧而不是排队"（WebRTC 编码前只留 1 帧；CRD 靠 `frame_pending_` 不采集；RustDesk 靠 `frame_controller` 等应答）。
- 静止屏幕：Sunshine 用"最低帧率"超时强制重编（默认客户端帧率一半），RustDesk 重编缓存的 YUV 并有次数上限，
  WebRTC 的 DesktopCapturer 在无变化时重新导出上一帧（`dxgi_output_duplicator.cc` "re-exported"）。
- 屏幕共享下 WebRTC 倾向保持分辨率、牺牲帧率。

### 各家差异

| 项目 | 谁决定帧率 | 降/升规则 | 编码慢时 | 静止时 |
|---|---|---|---|---|
| CRD | 客户端经 `max_framerate` 下发，调度器按固定间隔采集 | 无自适应（本文件） | 上一帧未采完就不采 | 未读到 |
| WebRTC | 编码器/适配器按偏好 | 2/3、3/2，带"等上次见效"和约束 | 队列满丢帧，占用率超 85% 降 | 屏幕共享倾向保分辨率 |
| Sunshine | 客户端设定，服务端设下限 | 无自适应 | 单帧失败仅结束会话；12 张图像池 | 下限帧率超时重编 |
| RustDesk | 服务端 QoS，观看端只给上限 | 延迟分档 + 3 次确认 + 渐进恢复 | 延迟升高自然降 | 重编缓存 YUV（≤10 次） |
| Parsec | 用户设定 + 丢积压帧（未核实细节） | 未公开 | 未公开 | 未公开 |

---

## 主题 2：Windows 上 Intel QSV 硬件编码 MFT 卡住或出错

### 要点

1. **异步 MFT 的规则（微软文档）。**
   - 必须先设 `MF_TRANSFORM_ASYNC_UNLOCK = TRUE`，否则方法返回 `MF_E_TRANSFORM_ASYNC_LOCKED`。
   - 只有收到 `METransformNeedInput` 才能 `ProcessInput`，否则返回 `MF_E_NOTACCEPTING`；只有收到 `METransformHaveOutput` 才能 `ProcessOutput`，否则返回 `E_UNEXPECTED`；
     异步 MFT 不应返回 `MF_E_TRANSFORM_NEED_MORE_INPUT`。
   - `MFT_MESSAGE_NOTIFY_START_OF_STREAM` 之前 MFT 不得发 NeedInput；Flush 或 Drain 之后，在再次收到 START_OF_STREAM 之前不再发 NeedInput。
   - 出处：https://learn.microsoft.com/en-us/windows/win32/medfound/asynchronous-mfts
   - 含义：卡住时先看是不是漏发了 START_OF_STREAM、Flush/Drain 之后没重发，或者 NeedInput 计数记错。

2. **Intel 论坛上的"卡住"现象（二手，未核实根因）。**
   - 现象 A：硬件 MFT 发了 `METransformHaveOutput`，但 `ProcessOutput` 给出的 sample 为 NULL，程序卡在该状态；
     现象 B：`ProcessInput/ProcessOutput` 都返回 S_OK，但状态是 `MFT_OUTPUT_DATA_BUFFER_NO_SAMPLE`；
     同一份代码换成 NVIDIA 硬件 MFT 没问题。Intel 回复称该状态只是"没有 sample"，不是 MFT 特有错误。
   - 出处（搜索摘要，未读原帖全文）：
     - https://community.intel.com/t5/Media-Intel-Video-Processing/Intel-Quick-Sync-Video-H-264-Encoder-MFT-MFT-OUTPUT-DATA-BUFFER/m-p/1097488
     - https://community.intel.com/t5/Media-Intel-Video-Processing/Intel-Quick-Sync-Video-H-264-Encoder-program-blocked-and/m-p/1064057
   - Intel Media SDK 已停止维护，搜索结果认为驱动层修复不太可能（未核实）。

3. **Chromium `MediaFoundationVideoEncodeAccelerator`：专门为 Intel 和其他厂商写了一批绕法。**
   - 连续关键帧：`Encode()` 里 "Avoid corruption triggered by consecutive key frames on Intel drivers." 在 VP9 连续关键帧请求之间插一个丢弃帧。
   - 层数：`SetEncoderModes()` "Intel drivers want the layer count to be set explicitly for H.264/HEVC"，即使是 1 层也要显式设。
   - 分辨率变化：`UpdateFrameSize()` 在 Intel 和 Qualcomm 上先发 `MFT_MESSAGE_COMMAND_FLUSH` 再发结束流消息；注释说不这样，分辨率变化后第一帧 MFT 可能失败。
   - 低延迟：`CODECAPI_AVLowLatencyMode` 仅当 `IsModifiable()` 返回 `S_OK` 才设。
   - 异步回调：`MFAsyncCallbackProxy::Invoke()` 在"某个随机 OS 线程"被调用，所以投递回自己的事件线程处理。
   - D3D：给 MFT 设 `MFT_MESSAGE_SET_D3D_MANAGER` 只在 LUID 一致时做，失败就 "fallback to non-D3D11 encoding"；
     共享设备上调用 `SetMultithreadProtected(TRUE)`；keyed mutex 超时（`WAIT_TIMEOUT/WAIT_ABANDONED`）按丢帧处理（"Frame dropped because of keyed mutex timeout"）。
   - 错误处理：`MEError` 映射到 `kEncoderHardwareDriverError`（"Media Foundation encountered a critical failure."）；
     异步事件失败报 `kSystemAPICallError`；`MF_E_TRANSFORM_STREAM_CHANGE` 时重新协商输出类型。
     NVIDIA 另有 workaround：纹理初始化后调用 `device_context->Flush()` 否则 MFT 可能看不到数据。
   - 本文件前 100000 字符里没有设备丢失的恢复逻辑，`OnSharedImageResourceAvailable` 里甚至用 `CHECK(SUCCEEDED(hr))`；
     摘要说"未发现 kPlatformFailureError、重建编码器或软件回退"，后 36000 字符我读了关于错误的部分，仍然没有。
     所以 Chromium 的 MFT 编码器是出错即上报，由上层（`MediaFoundationVideoEncodeAccelerator` 的使用方，如 WebRTC 的 `RTCVideoEncoder`）决定回退软件。该结论是推断，未核实上层行为。
   - 出处：https://raw.githubusercontent.com/chromium/chromium/main/media/gpu/windows/media_foundation_video_encode_accelerator_win.cc

4. **FFmpeg `h264_mf`（`mfenc.c`）：硬件编码强制用异步模式，事件循环没有超时，只有 EAGAIN。**
   - `mf_unlock_async()` 只对硬件视频编码器执行，注释："For hw encoding we unfortunately need to use async mode, otherwise"（后半句摘要被截断）。
   - `mf_wait_events()` 循环 `GetEvent()` 等 need-input/have-output/drain/marker，没有显式超时。
   - 只有 `mf_init_encoder()` 为获取 extradata 做了有限轮询（倍增休眠，累计 70 ms）；注释提到"some versions of the QSV H264 encoder"在编码前不提供 extradata。
   - 低延迟：只有设置了 `AV_CODEC_FLAG_LOW_DELAY` 才设 `CODECAPI_AVLowLatencyMode`；2025-08 有补丁新增 `low_latency` 选项，
     提交信息说它"cannot be achieved by selecting any of the low latency 'scenario' encoder presets alone"。
   - 枚举：`ff_instantiate_mf` 用 `MFT_ENUM_FLAG_SORTANDFILTER`，硬件加 `MFT_ENUM_FLAG_HARDWARE`，否则 `MFT_ENUM_FLAG_SYNCMFT`。
   - D3D11：`initialize_dxgi_manager()` 创建 DXGI device manager、`ResetDevice`、发 `MFT_MESSAGE_SET_D3D_MANAGER`；D3D11 输入只接受 NV12。
   - 出处：
     - https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/mfenc.c
     - https://raw.githubusercontent.com/FFmpeg/FFmpeg/master/libavcodec/mf_utils.c
     - https://ffmpeg.org/pipermail/ffmpeg-devel/2025-August/347343.html

5. **`MF_LOW_LATENCY` 与 `CODECAPI_AVLowLatencyMode` 是同一个 GUID。**
   - 微软："The GUID value of this attribute is identical to the CODECAPI_AVLowLatencyMode property"；
     "For encoders, the encoder might support low latency through the ICodecAPI interface."
   - 也就是说对编码器应该通过 `ICodecAPI::SetValue(CODECAPI_AVLowLatencyMode, TRUE)` 设，不是只给 `GetAttributes()` 的 MFT 属性仓。
   - 微软 H.264 编码器页（软件 MFT）说 "The encoder defaults to slice encoding, to minimize latency. To enable multi-frame encoding, set the CODECAPI_AVLowLatencyMode property to VARIANT_FALSE."（来自搜索摘要，未读原页）
   - 一位开发者报告在 Quick Sync MFT 上设了 `MF_LOW_LATENCY` 和 ICodecAPI 属性"没有效果"（搜索摘要，未核实；他自己也说测量可能有偏差）。
   - 出处：
     - https://learn.microsoft.com/en-us/windows/win32/medfound/mf-low-latency
     - https://learn.microsoft.com/windows/win32/medfound/codecapi-avlowlatencymode
     - https://learn.microsoft.com/en-ie/Windows/Win32/medfound/h-264-video-encoder

6. **Sunshine 对 QSV：走 FFmpeg `h264_qsv` + D3D11VA 设备共享，要求多线程保护；失败有一次"回退选项重试"。**
   - `display_vram.cpp` 的 `prepare_to_derive_context` 里，QSV 要求 `SetMultithreadProtected(TRUE)`（`ID3D11Multithread`）。
   - 捕获和编码用 keyed mutex，`AcquireSync(0, INFINITE)` 无限等待。
   - `avcodec_open2` 失败且存在 fallback 选项时 "Retrying with fallback configuration options for ["；
     H.264 的 QSV 注释："Some old/low-end Intel GPUs don't support low power encoding"。
   - 运行期编码单帧失败不重建编码器，只结束该会话；编码器选择在启动时 `probe_encoders()` / `validate_encoder()`，失败就从列表删掉试下一个；软件编码被称为 "encoder of last resort"。
   - 配置项：`qsv_preset`（默认 medium）、`qsv_coder`（auto/cabac/cavlc）、`qsv_slow_hevc`。
   - 出处：
     - https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/video.cpp
     - https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/platform/windows/display_vram.cpp
     - https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2configuration.html

7. **QSV 设备失败的通行处理是"关闭并重新初始化"（来自 FFmpeg trac 的讨论，二手）。**
   - 搜索摘要称：FFmpeg 的 QSV H.264 编码器返回 -17（对应 Intel `MFX_ERR_DEVICE_FAILED`）后一直停在设备忙；
     VLC 遇到该错误调 `MFXVideoENCODE_Reset()`；QSVEnc 在 `MFX_ERR_DEVICE_{FAILED,LOST}` 时 `Close()` 再 `Init()`；
     帖子称 Intel 文档也建议重新初始化会话。
   - 出处（trac 页面抓取被反爬拦截，只有搜索摘要，未核实原文）：https://trac.ffmpeg.org/ticket/5708
   - 这是 libmfx（不是 MFT）路径的错误码；对 MFT 路径是否同样适用未核实。

8. **OBS：QSV 问题多是驱动/多 GPU 选择导致，未读到 OBS 对 MFT 的重建逻辑。**
   - Intel 支持文章：OBS 用 QuickSync H.264 在 UHD 730 上启动失败，归因于 "driver-level incompatibility"，建议更新驱动并让 OBS 运行在核显上。
     https://www.intel.com/content/www/us/en/support/articles/000101294.html
   - OBS 论坛：新版 QSV 编码器只有 OBS 跑在核显上才出现在列表里；新 Arc 驱动 32.0.101.6557 起 QSV 选项消失，回退旧驱动可绕过
     （搜索摘要）：https://community.intel.com/t5/Intel-ARC-Graphics/QSV-HW-encoding-not-working-in-OBS-with-driver-version-32-0-101/m-p/1668671
   - OBS 的 `win-mf` 插件源码和 `obs-qsv11` 的出错重建逻辑：没读到（见"没查到"）。

### 可以直接借鉴的做法

- 事件状态机严格按微软文档：NeedInput 计数、START_OF_STREAM、Flush/Drain 后重发 START_OF_STREAM；每次 ProcessInput/Output 都绑定对应事件。
- 对 Intel 的已知绕法（Chromium 里有注释）：显式设置层数、Flush 之后再改分辨率、避免连续关键帧、`CODECAPI_AVLowLatencyMode` 用 `ICodecAPI` 设。
- 共享 D3D11 设备：`ID3D11Multithread::SetMultithreadProtected(TRUE)`（Chromium 和 Sunshine 都做）；
  device manager 用 `MFT_MESSAGE_SET_D3D_MANAGER`，失败就退到非 D3D 的内存输入（Chromium）。
- 出错后的恢复，业内常见两种：
  - 上报错误由上层回退软件编码（Chromium、RustDesk：硬编连续失败 3 次禁用并切换）；
  - 关闭并重新初始化（QSVEnc、VLC，libmfx 路径）。
  - 没查到哪个项目在 MFT 卡住（没有事件）时有看门狗超时；这是公开资料的空白，不是证明没有。
- FFmpeg 的 `mf_wait_events` 也没有超时，所以"没有事件就永远等"在 FFmpeg 里也是现状。

### 各家差异

| 项目 | 路径 | 出错时 |
|---|---|---|
| Chromium | 异步 MFT + D3D11 共享图像 | 报 `kEncoderHardwareDriverError` 等，由上层处理 |
| FFmpeg h264_mf | 异步 MFT，无超时事件循环 | 返回 `AVERROR_EXTERNAL` / EAGAIN |
| FFmpeg h264_qsv / Sunshine | libmfx(oneVPL) + D3D11VA | 打开失败走 fallback 选项；运行期失败结束会话 |
| RustDesk | hwcodec（含 MFT/QSV 等） | 连续失败 3 次禁用硬件并切换 |
| OBS | QSV 走 obs-qsv11（非 MFT） | 论坛里多为驱动/GPU 选择问题（未读源码） |

---

## 主题 3：从显卡把画面读回内存（GPU readback）

### 要点

1. **微软文档：copy 是异步的，Map 太早会卡住整个管线；建议至少等两帧。**
   - "If the Map method is called after the data has actually been copied, no performance loss occurs. On the other hand, if the Map method is called before the data has been copied, then a pipeline stall will occur."
   - 对 `CopyResource` 之后立刻 `Map` 的后果："not only does the Copy method call need to execute, but all other buffered commands in the command buffer must execute as well"。
   - "How long should the application wait? At least two frames"：第 N 帧发出 copy，第 N+1 帧 GPU 执行，第 N+2 帧结果可读。
   - 出处：https://learn.microsoft.com/en-us/windows/uwp/graphics-concepts/copying-and-accessing-resource-data

2. **`D3D11_MAP_FLAG_DO_NOT_WAIT`：GPU 没完成时立刻返回 `DXGI_ERROR_WAS_STILL_DRAWING`。**
   - 文档："Specifies that Map should return DXGI_ERROR_WAS_STILL_DRAWING when the GPU blocks the CPU from accessing a resource."
   - `Map` 在 CPU 读权限下设备被移除时返回 `DXGI_ERROR_DEVICE_REMOVED`。
   - 出处：
     - https://learn.microsoft.com/en-us/windows/win32/api/d3d11/ne-d3d11-d3d11_map_flag
     - https://learn.microsoft.com/en-us/windows/win32/api/d3d11/nf-d3d11-id3d11devicecontext-map

3. **OBS：N 个槽位的环形 staging，每帧"拷到当前槽、映射上一槽"。**
   - `output_frame`：`prev_texture = cur_texture == 0 ? NUM_TEXTURES - 1 : cur_texture - 1`；`download_frame` 对上一槽 `gs_stagesurface_map`；
     `textures_copied[prev_texture]` 为 false 就不映射；`stage_output_texture` 开头先 `unmap_last_surface`。
   - `NUM_TEXTURES` 的值没在该文件里（未核实，我印象中是 6，但没读到）。
   - 源码里没有解释这个一帧偏移的注释，"为了避免映射刚发出的拷贝"是我的推断。
   - 出处：https://raw.githubusercontent.com/obsproject/obs-studio/master/libobs/obs-video.c

4. **Sunshine 的 RAM 路径（`display_ddup_ram_t`）：单个 staging 纹理，同步 Map，不用 DO_NOT_WAIT。**
   - staging 纹理首帧创建，`D3D11_USAGE_STAGING` + `D3D11_CPU_ACCESS_READ`；
     有更新才 `CopyResource`；`device_ctx->Map(texture.get(), 0, D3D11_MAP_READ, 0, &img_info)`，flags 为 0；之后拷贝并做旋转，再合成光标。
   - Sunshine 的主路径是 VRAM（`display_vram.cpp`），帧不回 CPU，直接喂硬件编码器；RAM 路径用于软件编码。
   - 出处：
     - https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/platform/windows/display_ram.cpp
     - https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/platform/windows/display_vram.cpp

5. **WebRTC `modules/desktop_capture/win`：单 staging，同步映射，`AcquireNextFrame` 超时设为 0。**
   - `DxgiTextureStaging::InitializeStage()`：`D3D11_USAGE_STAGING` + `D3D11_CPU_ACCESS_READ`，描述不一致时重建；`CopyFromTexture()` 用 `CopyResource`；
     映射用 `surface_->Map(rect(), DXGI_MAP_READ)`；不用 `DO_NOT_WAIT`，也不用 `MapDesktopSurface`。
   - `DxgiOutputDuplicator`：`AcquireNextFrame()` 超时为 0，注释说调度由上层控制器负责；超时不释放帧；
     没有新帧但有上一帧时重新导出上一帧；用脏区域只拷贝变化像素（`CopyPixelsFrom`）。
   - 出处：
     - https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/modules/desktop_capture/win/dxgi_texture_staging.cc
     - https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/modules/desktop_capture/win/dxgi_output_duplicator.cc
   - 我没读到 `DxgiDuplicatorController`（负责多输出调度与重试），见"没查到"。

6. **桌面复制直接走 CPU：`MapDesktopSurface`，仅当桌面图像已在系统内存时可用。**
   - 文档："You can successfully call MapDesktopSurface if the DesktopImageInSystemMemory member of the DXGI_OUTDUPL_DESC structure is set to TRUE." 否则返回 `DXGI_ERROR_UNSUPPORTED`，
     "the application must first transfer the image to a staging surface and then lock the image by calling the IDXGISurface::Map method."
   - 未映射前不能再次调用，需要 `UnMapDesktopSurface`；`DXGI_ERROR_ACCESS_LOST` 时必须释放并重建 `IDXGIOutputDuplication`。
   - 出处：https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_2/nf-dxgi1_2-idxgioutputduplication-mapdesktopsurface
   - 帧格式固定为 `DXGI_FORMAT_B8G8R8A8_UNORM`，并附带脏矩形/移动矩形；
     出处：https://learn.microsoft.com/en-us/windows/win32/direct3ddxgi/desktop-dup-api
   - 在常见独显机器上 `DesktopImageInSystemMemory` 是否多为 FALSE：没有查到公开统计，未核实。

7. **耗时量级：没有查到可引用的公开毫秒数据。**
   - 搜索没有找到 1080p/1440p staging 读回的公开基准。唯一相关的是微软 DevBlogs 的说法：Graphics Capture 比 Desktop Duplication 开销低得多（尤其 4K 60 Hz 以上），没有给毫秒。
     https://devblogs.microsoft.com/oldnewthing/20211223-00/?p=106050 （搜索摘要，未读原文）
   - 自己算的数据量（推算，不是出处）：1080p BGRA = 1920×1080×4 ≈ 8.3 MB；1440p ≈ 14.7 MB；
     若走 PCIe 3.0 x16（约 12 GB/s 实际）理论拷贝 < 1.5 ms，核显共享内存则不走 PCIe。实际毫秒数要在目标硬件上量。

### 可以直接借鉴的做法

- 多个 staging 轮换：N=2 到 3 个槽位，每帧 `CopyResource` 到槽 i，映射槽 i-1（OBS）；微软文档说"至少两帧"，OBS 实际用一帧偏移（见要点 3）。
- 想完全不阻塞：`Map(..., D3D11_MAP_FLAG_DO_NOT_WAIT)`，收到 `DXGI_ERROR_WAS_STILL_DRAWING` 就推迟到下一轮重试；
  没找到哪个开源项目这样用（Sunshine、WebRTC 都是同步 `Map`），这是微软文档支持但业内少见的做法。
- 只在有更新时才拷贝（Sunshine 看 `frame_update_flag`；WebRTC 用脏矩形）。
- `ACCESS_LOST` 要释放并重建 duplication；RustDesk 在 10 秒内最多重建 3 次，之后回退 GDI。

### 各家差异

| 项目 | staging 个数 | Map 方式 | 延迟 |
|---|---|---|---|
| OBS | NUM_TEXTURES 个环形 | 同步映射上一槽 | 一帧偏移 |
| Sunshine RAM | 1 个 | 同步 `D3D11_MAP_READ`，flags=0 | 当帧 |
| WebRTC | 1 个 | `IDXGISurface::Map(DXGI_MAP_READ)` | 当帧 |
| 微软文档建议 | 多个 | 晚两帧 | +2 帧 |

---

## 主题 4：软件编码能到多少帧

### 要点

公开数据很少，**下面没有一条是"1080p/1440p 屏幕内容、OpenH264 或 x264 ultrafast 的权威基准"**。

1. **x264 的预设与调优定义（源码帮助文本）。**
   - `--preset ultrafast`：`--no-8x8dct --aq-mode 0 --b-adapt 0 --bframes 0 --no-cabac --no-deblock --no-mbtree --me dia --no-mixed-refs --partitions none --rc-lookahead 0 --ref 1 --scenecut 0 --subme 0 --trellis 0 --no-weightb --weightp 0`
   - `--tune zerolatency`：`--bframes 0 --force-cfr --no-mbtree --sync-lookahead 0 --sliced-threads --rc-lookahead 0`
   - `--sliced-threads`："Low-latency but lower-efficiency threading"
   - 出处：https://raw.githubusercontent.com/mirror/x264/master/x264.c （mirror 仓库，可能不是最新）

2. **切片线程会明显降低吞吐，但不加编码延迟（旧数据，未核实）。**
   - 2010 年邮件列表：720p veryfast + zerolatency，保留 sliced-threads 约 43 fps，去掉约 81 fps。
     https://mailman.videolan.org/pipermail/x264-devel/2010-April/007121.html （搜索摘要）
   - 2020 年论坛报告：720p baseline，zerolatency 下每帧最长 12 ms，关掉 sliced-threads 后约 2 ms（CPU 占用约 40% → 20%）。来源页面我没拿到 URL（搜索摘要没给），未核实。
   - 这两条是旧硬件、非屏幕内容，只能说明"切片线程有明显代价"，不能当 1080p 数据用。

3. **x264 ultrafast 1080p30 吞吐（非 zerolatency、非屏幕内容、离线整片）。**
   - x264-benchmark 项目：i7-8700K 全核，10 秒 1080p30 片段，ultrafast 约 4.78 秒完成（约 2 倍实时，即约 63 fps）。
     https://github.com/cmoore1776/x264-benchmark （搜索摘要）
   - HandBrake 文档给过 1080p30 x264 各预设的速度（如 "Very Fast" 66.4 FPS），但不含 OpenH264，且是转码场景。
     https://handbrake.fr/docs/en/1.8.0/technical/performance.html （搜索摘要，未读原文）

4. **OpenH264 的速度：只有一条 2018 年的邮件列表数据，且条件不明。**
   - 报告称单线程 OpenH264 比 x264 快 4 倍；1080p "Jockey" 600 帧，OpenH264 约 13.6 秒（约 23 ms/帧），FFmpeg 那一侧约 54.7 秒。
     x264 维护者回复"用更快的预设"，说明对方很可能用的默认 medium 预设，对比不公平。
   - 出处：https://ffmpeg.org/pipermail/libav-user/2018-May/011131.html 、https://mailman.videolan.org/pipermail/x264-devel/2018-May/012414.html （搜索摘要，未读全文，未核实）
   - OpenH264 README 没有基准数字，只说 "Multiple threads automatically used for multiple slices"（编码器）。
     https://raw.githubusercontent.com/cisco/openh264/master/README.md

5. **OpenH264 的多线程只靠"多切片"：`iMultipleThreadIdc` + 切片模式。**
   - `codec_app_def.h`：`SM_FIXEDSLCNUM_SLICE`（"enabled dynamic slicing for multi-thread"）、`SM_RASTER_SLICE`、`SM_SIZELIMITED_SLICE`；
     `SSliceArgument.uiSliceNum` 为 0 时默认取 CPU 核数；`iMultipleThreadIdc` 为 0 表示自动。
   - 相关开关：`SCREEN_CONTENT_REAL_TIME`（屏幕内容用途）、`bEnableFrameSkip`（超 VBV 时丢帧）、`iComplexityMode`（`LOW_COMPLEXITY` 是 "the lowest compleixty,the fastest speed"）、
     `bEnableBackgroundDetection`、`bEnableAdaptiveQuant`、`bEnableSceneChangeDetect`。
   - 出处：https://raw.githubusercontent.com/cisco/openh264/master/codec/api/wels/codec_app_def.h

6. **WebRTC 里的 OpenH264 封装：线程数固定为 1，屏幕共享用 `SCREEN_CONTENT_REAL_TIME`。**
   - `NumberOfThreads()` 恒返回 1，按分辨率和核数分档的逻辑被注释掉了（这是 webrtc-sdk 的 fork，非 Google 官方树，需要确认官方树是否一致，未核实）。
   - SingleNalUnit 用 `SM_SIZELIMITED_SLICE`，NonInterleaved 用 `SM_FIXEDSLCNUM_SLICE`；`iUsageType = SCREEN_CONTENT_REAL_TIME`；`bEnableFrameSkip = frame_dropping_on`。
   - 出处：https://raw.githubusercontent.com/webrtc-sdk/webrtc/main/modules/video_coding/codecs/h264/h264_encoder_impl.cc

7. **Sunshine 的软件编码默认值与线程设置。**
   - `sw_preset` 默认 `superfast`，`sw_tune` 默认 `zerolatency`，`min_threads` 默认 2；
     文档："Increasing the value slightly reduces encoding efficiency, but the tradeoff is usually worth it"。
   - 代码：`ctx->slices = std::max(config.slicesPerFrame, config::video.min_threads); ctx->thread_count = ctx->slices;`（线程数等于切片数）。
   - 出处：
     - https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2configuration.html
     - https://raw.githubusercontent.com/LizardByte/Sunshine/master/src/video.cpp （offset 50000 以后的部分）

### 可以直接借鉴的做法

- 低延迟软编：线程数 = 切片数（Sunshine），切片数取客户端给的值和 `min_threads` 的较大者；
  x264 的 zerolatency 自带 sliced-threads，代价是效率下降（公开旧数据显示吞吐可能接近减半）。
- OpenH264 屏幕内容：`SCREEN_CONTENT_REAL_TIME` + `SM_FIXEDSLCNUM_SLICE` + `iMultipleThreadIdc` 自动或显式设线程；
  `iComplexityMode` 可降到 LOW_COMPLEXITY 换速度。
- 数据要自己量：固定分辨率、预设、线程、屏幕内容样本（不要用自然视频），分别量 1 线程和 N 切片 N 线程。

### 各家差异

| 项目 | 编码器 | 预设/复杂度 | 线程 |
|---|---|---|---|
| Sunshine | x264（FFmpeg） | superfast + zerolatency | 线程 = 切片 = max(客户端值, min_threads=2) |
| WebRTC (fork) | OpenH264 | SCREEN_CONTENT_REAL_TIME | `NumberOfThreads` 恒为 1 |
| x264 默认 zerolatency | x264 | — | sliced-threads |

---

## 主题 5：高带宽、低延迟局域网上的带宽探测

### 要点

1. **WebRTC `ProbeController`：指数探测 3×、6× 起始码率，探测上限默认 5 Mbps（未设置最大码率时）。**
   - 常量：`kMinProbePacketsSent = 5`，`kMinProbeDurationMs = 15`，`kMaxWaitingTimeForProbingResultMs = 1000`，
     `kDefaultMaxProbingBitrateBps = 5000000`，`kBitrateDropThreshold = 0.66`，`kBitrateDropTimeoutMs = 5000`，
     `kProbeFractionAfterDrop = 0.85`，`kAlrEndedTimeoutMs = 3000`，`kMinTimeBetweenAlrProbesMs = 5000`，`kProbeUncertainty = 0.05`。
   - 字段试验默认：`first_exponential_probe_scale = 3.0`，`second_exponential_probe_scale = 6.0`，`further_exponential_probe_scale (step_size) = 2`，
     `further_probe_threshold = 0.7`，`alr_probing_interval = 5 s`，`alr_probe_scale = 2`，`alloc_p1 = 1`，`alloc_p2 = 2`。
   - 分配探测上限："max_probe_bitrate = std::min(max_probe_bitrate, max_total_allocated_bitrate_ * 2);"
   - 超过 1 秒没拿到探测结果，状态进入 `kProbingComplete`。
   - 摘要里没找到 `repeated_initial_probing`。
   - 出处：
     - https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/modules/congestion_controller/goog_cc/probe_controller.cc
     - https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/modules/congestion_controller/goog_cc/probe_controller.h
   - 注意：我没读到最新版 `ProbeControllerConfig` 里是否还有 `min_probe_packets_sent` 等新字段，摘要说头文件不含它们。

2. **WebRTC 探测结果直接成为估计值，除非检测到过载。**
   - `DelayBasedBwe::MaybeUpdateEstimate`：无过载且有 `probe_bitrate` 时 `rate_control_.SetEstimate(*probe_bitrate, at_time)`；过载时忽略探测值。
   - `kStreamTimeOut = 2 s`：流空闲超过 2 秒会重置到达间隔和延迟检测器。
   - 过载且没有已确认码率时估计减半，注释 "by 50% every 200 ms"。
   - 出处：https://chromium.googlesource.com/external/webrtc/+/refs/heads/main/modules/congestion_controller/goog_cc/delay_based_bwe.cc
   - 我想核实的"AIMD 增长不超过已确认速率 1.5 倍 + 10 kbps"那段：抓取 `aimd_rate_control.cc` 的请求没有成功（该次工具调用返回的是其他内容，不能引用），**未核实**。

3. **ALR（应用受限区）探测：画面静止时发包少，估计会停在低位，所以周期性探测。**
   - `alr_probing_interval = 5 s`，`alr_probe_scale = 2`（当前估计的 2 倍），`kMinTimeBetweenAlrProbesMs = 5000`，
     ALR 结束后 3 秒内（`kAlrEndedTimeoutMs`）仍允许因码率下跌触发探测。
   - 出处：同 probe_controller.cc（常量值）；这些名字和值来自摘要，而"ALR 为什么存在"的解释是我的理解，不是源码原句。
   - 远程桌面大部分时间在 ALR，所以这条机制和我们场景直接相关；WebRTC 对屏幕共享的 ALR 处理有无特殊点（如 `alr_experiments`）：没查到。

4. **BBR（IETF 草案 draft-ietf-ccwg-bbr-02）：STARTUP 增益 2.77，连续 3 轮增长不足 25% 退出；PROBE_BW 每 2–3 秒随机探测一次。**
   - "BBRStartupPacingGain (2.77)"（4·ln2），cwnd 增益 2；退出条件：3 个非应用受限的轮次中交付速率增长 < 25%。
   - PROBE_BW 四个阶段 DOWN / CRUISE / REFILL / UP，增益 0.90 / 1.0 / 1.0 / 1.25；探测间隔 "uniformly randomly distributed between 2 and 3 secs"。
   - 带宽滤波器 `BBR.MaxBwFilter` 覆盖 "up to 2 ProbeBW cycles"。
   - ProbeRTT：至少每 5 秒一次，持续 200 ms，期间把在途数据降到约 BDP 的一半。
   - 关键点：只有"非应用受限"的采样才能抬高带宽估计，应用受限的样本不能当作链路上限用。
   - 出处：https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-02.txt （只读了前 100000 / 170059 字符）

5. **Moonlight/Sunshine：局域网码率由客户端设定，一次性写入，没有看到服务端拥塞控制。**
   - Moonlight 默认码率表（"We're just using the values that the Shield used"）：720p 5、1080p 10、1440p 20、4K 40 Mbps（对 30 fps 基准；帧率因子 `fps<=60 ? fps : sqrt(fps/60)*60` 再除以 30，所以 1080p60 = 20 Mbps）；4:4:4 乘 2。
     出处：https://raw.githubusercontent.com/moonlight-stream/moonlight-qt/master/app/settings/streamingpreferences.cpp
   - `m_StreamConfig.bitrate = m_Preferences->bitrateKbps`，没有看到远程/局域网的码率钳制；`unlockBitrate` 复选框提示 "It should only be used when streaming over an Ethernet LAN connection."；
     UI 帮助："Lower the bitrate on slower connections. Raise the bitrate to increase image quality."
     出处：https://raw.githubusercontent.com/moonlight-stream/moonlight-qt/master/app/streaming/session.cpp ； https://raw.githubusercontent.com/moonlight-stream/moonlight-qt/master/app/gui/SettingsView.qml
   - `moonlight-common-c`：`bitrate` "includes error correction data, so the actual encoder bitrate will be about 20% lower when using the standard 20% FEC configuration"；
     `streamingRemotely` 自动判断（RFC 1918 地址视为局域网），远程只是把包大小限到 1024；
     `connectionStatusUpdate` 回调只是提示用户 "Consider displaying an overlay for the user to notify them why their stream is not performing as expected."
     出处：https://raw.githubusercontent.com/moonlight-stream/moonlight-common-c/master/src/Limelight.h
   - Sunshine：`max_bitrate` 默认 0，"0 means Sunshine uses the bitrate Moonlight requests"；非 0 为上限（Kbps）。
     出处：https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2configuration.html
   - Sunshine 官方故障排除只提到：主机带宽远大于链路最慢段时会大量丢包，建议用 `tc` 做流量整形，暗示主机不会自己降。
     https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2troubleshooting.html （搜索摘要，未读原页）
   - 社区有人要求自适应码率（Apollo issue #399，未核实内容），也暗示现状是固定。

6. **Parsec：公开说法是动态码率 + 用户可设带宽上限。**
   - 博客："a highly tuned and custom congestion control algorithm"；"the best move with a video stream is to tweak the encoder's bitrate"；
     目标是 "saturate that 12 Mb/s connection right at the edge without introducing loss or latency"；
     并承认 "it tends to be too conservative for people with higher latencies."
     出处：https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007
   - "With network latencies below 20 ms, and bandwidth above 10 Mb/s, Parsec offers a near-native experience."
     出处：https://parsec.app/blog/description-of-parsec-technology-b2738dcc3842
   - 带宽上限设置（"The maximum bandwidth used by Parsec, split between all connected guests"）和"上限随拥塞调整"的说法：只有搜索摘要，原页 403，未核实。

7. **千兆局域网上估计常见问题（公开资料很少，以下仅为已读材料能支持的部分）。**
   - 估计受应用限制：BBR 草案要求应用受限样本不能抬估计；WebRTC 用 ALR 探测来补；`kDefaultMaxProbingBitrateBps = 5 Mbps` 说明默认探测天花板很低，高带宽场景必须显式配置最大码率。
   - 编码器的输出不会超过它的码率设置：没用满码率时，"送达速率"不等于"链路能力"（这和 CLAUDE.md 里已有的教训一致，这里只是说明业内资料里 BBR 的"应用受限"概念对应它）。
   - 没查到"千兆局域网上带宽估计偏低/偏高"的专门报告或论文，见"没查到"。

### 可以直接借鉴的做法

- 起始探测：先 3× 再 6× 起始码率，指数增长；每个探测簇至少 5 个包、15 ms，等结果最多 1 秒（WebRTC）。
- 画面静止（ALR）时：周期性（5 秒）探测到当前估计的 2 倍；码率下跌 > 34% 且 5 秒内，按下跌前的 85% 探测恢复。
- 探测结果在没有过载信号时直接采用；有过载就丢弃探测结果（WebRTC）。
- 只有"链路被填满"的样本能证明带宽上限（BBR 的应用受限规则）。
- 局域网：Moonlight/Sunshine 的做法是让用户设上限，服务端按请求码率发；Parsec 是动态码率加用户上限。
- 码率里要预留 FEC：Moonlight 的 `bitrate` 含 20% FEC，编码器实际码率约低 20%。

### 各家差异

| 项目 | 码率谁定 | 探测 | 局域网 |
|---|---|---|---|
| WebRTC | GCC（延迟 + 丢包）+ ProbeController | 指数 3×/6×，ALR 2× 每 5 秒 | 默认探测上限 5 Mbps，需配置最大码率 |
| BBR | 链路最大交付速率滤波 | STARTUP 增益 2.77，PROBE_BW 每 2–3 秒 | — |
| Moonlight/Sunshine | 客户端设定，`max_bitrate` 上限 | 无（未看到） | 用户设定，局域网可解锁更高 |
| Parsec | 动态 + 用户上限（未核实细节） | 未公开 | 未公开 |

---

## 没查到或没核实的

**主题 1**
- CRD 的帧率谁在上游设定（客户端请求到 `SetMaxFramerateFps` 的链路）、静止屏幕时 CRD 是否仍按固定间隔采集并发空帧：我只读了调度器和 `webrtc_video_stream.cc` 的开头部分，没读到。
  建议继续看 `remoting/host/desktop_capturer_wrapper`、`remoting/protocol/webrtc_frame_scheduler_simple.cc`（旧实现）。
- WebRTC `BalancedDegradationSettings`（`rtc_base/experiments/balanced_degradation_settings.cc`）里按分辨率的帧率档位和码率阈值：没读到原文，搜索摘要中的 10/15 fps 和旧版 `MinFps/MaxFps` 来自历史版本，**未核实**。
- WebRTC `OveruseFrameDetector` 的升档回退（`current_rampup_delay_ms_`、quick rampup）具体数值：没读到。
- WebRTC `VideoStreamEncoder` 文件只读了前 100000 字符，屏幕共享的 frame dropper / `kMaxFrameRate` 相关部分可能在后面。
- Parsec 帮助中心全部 403：FPS 设置、带宽上限、Stream Overlay 指标含义都没拿到原文。Parsec 的帧率适配规则没有公开资料。
- Sunshine 里 `frame_threshold`、`minimum_fps_target` 在 `video.cpp` 中的实际使用位置没看到（只通过配置文档和 `encode_run` 一段确认）。

**主题 2**
- OBS `win-mf` 插件、`obs-qsv11` 的源码和出错重建逻辑：没读到。
- Chromium 在编码器报错后是否回退软件（`RTCVideoEncoder` / `VideoEncodeAccelerator` 使用方）：没读。
- Intel MFT 具体的卡死原因和修复驱动版本：只有 Intel 论坛旧帖的现象描述（未读全文），没有官方根因。
- FFmpeg trac #5708 页面被反爬拦截，`MFX_ERR_DEVICE_FAILED` 的说法只来自搜索摘要。
- 没找到任何项目对"异步 MFT 超时没事件"设看门狗的公开实现。
- `ID3D11Multithread`、`ResetDevice` 在 Intel MFT 上的专门要求：微软 hardware MFT 指南页返回 404，没读到。
- Sunshine 的 QSV 编码器选项定义（`async_depth`、`low_delay_brc`、`forced_idr` 等）在 `video.cpp` 里的具体值：读的两段都没出现，未核实。

**主题 3**
- 1080p、1440p 读回的公开毫秒数据：没找到，只给了数据量的推算。
- `DxgiDuplicatorController`（Chromium/WebRTC）的重试与多显示器调度：没读到。
- OBS `NUM_TEXTURES` 的值：没读到定义。
- 实际使用 `DO_NOT_WAIT` 的开源屏幕捕获项目：没找到。
- `DesktopImageInSystemMemory` 在实际机器上的取值分布：没找到。

**主题 4**
- 1080p/1440p 屏幕内容下 OpenH264、x264 ultrafast/zerolatency 每帧耗时的权威公开基准：没找到。
- OpenH264 多切片线程的具体加速比：没找到。
- arXiv 2009.14165（AV1 实时模式性能）里有 OpenH264 列，但表格摘要不清，没引用。
- 2020 年"12 ms → 2 ms 关闭 sliced-threads"那条的原始 URL 没拿到。

**主题 5**
- `aimd_rate_control.cc` 的"已确认速率 1.5 倍"类上限：没成功读到，不能引用。
- 最新 `ProbeController` 是否仍保留上面这些默认值：用的是镜像 main 分支的摘要。
- Moonlight `autoAdjustBitrate` 设置的确切语义（是仅在改分辨率/帧率时自动换默认码率，还是运行期调整）：只看到它被读写，未看到使用处，未核实。
- GFE/Sunshine 是否有任何运行期码率调整：没有读到服务端实现，"没有"是根据文档和 issue 的推断，未核实。
- 千兆局域网上带宽估计的专门问题（偏低、探测过冲、应用受限）：没有找到专门文献。
- BBR 草案只读了前 100000 字符。
