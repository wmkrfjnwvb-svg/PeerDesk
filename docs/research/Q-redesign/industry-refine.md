# 业内“静止后补清晰”调研

日期：2026-11-27。方法：直接读源码和官方文档原文（GitHub 原始文件、微软协议文档），没有运行任何代码，没有做实验。
所有源码链接指向各仓库默认分支（chromium `main`、openh264 `master` 等）当天读到的内容，没有锁定提交；行号只是当天的位置，后续会漂移，函数名更稳。
“原文”指代码和注释；“推断”指我从原文推出来、原文没有直接写的东西，已单独标出。

没找到原文的项目放在最后一节“没找到原文”。

---

## 1. Chrome 远程桌面（Chromium `remoting/`）

### 做法（读到的原文）

Chromium 不是“补一帧高质量”，而是**把最后一帧反复重新编码，直到编码器自己的量化参数降到下限**。

文件：[`remoting/protocol/webrtc_video_encoder_wrapper.cc`](https://github.com/chromium/chromium/blob/main/remoting/protocol/webrtc_video_encoder_wrapper.cc)

- 常量（约第 49–73 行）：
  - `kMaxQuantizer = 50`、`kMinQuantizer = 10`。注释：最小量化值“选在再往下的帧只会白花带宽、画质没有明显提升”的位置；调低 max 会提升画质但增加延迟，调高 max 缩短延迟但“top-off 时间更长”。
  - `kBigFrameThresholdPixels = 300000`，`kEstimatedBytesPerMegapixel = 100000`（每百万像素约 100 KB，注释说“只是粗略估计”）。
  - `kKeepAliveInterval = 2 s`：没有更新、没有 top-off、没有关键帧请求时，最多每 2 秒发一帧保活。
- 触发和结束（`OnFrameEncoded`，约第 502–510 行）：每编完一帧，`top_off_active_ = (frame->quantizer > kMinQuantizer)`。也就是说，**只要上一帧实际用的量化值还高于 10，就继续 top-off**；到 10 或以下停止。编码器没有输出数据时也停止。
- 怎么继续（`UpdateTopOffExtrapolationTimer` / `ExtrapolateFrame`，约第 627–660 行）：top-off 期间起一个定时器，间隔 `max(帧间隔 × 1.1, 帧间隔 + 2 ms)`，到点就把**最后一次抓到的帧**（`last_capturer_fed_frame_`）再送去编码。间隔比帧率略长，是为了“被真实抓屏的新帧抢占”；如果编码器正忙（`encode_pending_` 或有 `pending_frame_`）就这一拍不补。新画面来了就让路。
- 每帧参数（`EncodeDesktopFrame`，约第 593–612 行）：`vpx_min_quantizer = 大帧 ? 50 : 10`，`vpx_max_quantizer = 50`，`clear_active_map = !top_off_active_`。
- 大帧降画质（`ShouldDropQualityForLargeFrame`，约第 543–567 行）：**只对 VP8**。更新面积比历史最大值多出 30 万像素以上，且“预估帧大小 = 面积 × 100000 / 1e6 字节；按当前码率发送耗时 > 当前帧间隔”，才把 min 量化值也抬到 50（相当于这一帧强制粗编），之后靠 top-off 补回来。

文件：[`remoting/codec/webrtc_video_encoder_vpx.cc`](https://github.com/chromium/chromium/blob/main/remoting/codec/webrtc_video_encoder_vpx.cc)

- 编码器设置（约第 55–100 行）：`rc_end_usage = VPX_CBR`，`rc_undershoot_pct = 100`，`rc_overshoot_pct = 15`，`rc_dropframe_thresh = 0`，关键帧间隔设成 10000（基本不出周期关键帧）。
- 区域限定（`Encode`，约第 246–262 行）：用 libvpx 的 active map 限定编码范围。关键帧时把整屏填满，注释写明原因：“让后续的 top-off 帧（`clear_active_map = false`）能给关键帧 top-off”。非关键帧：`clear_active_map` 为真时先清空，再把本帧的更新区域并进去。top-off 期间不清空，所以 **active map 累积的是“自上次收敛后改过的所有区域”**，top-off 只在这些区域里做。VP9 在所有块都活跃时绕过 active map。
- VP9 另加（约第 163–164 行）：`VP9E_SET_AQ_MODE = 3`，注释原文：“Set cyclic refresh (aka "top-off") for lossy encoding.” 即 VP9 把循环刷新叫作 top-off。
- 屏幕内容：VP9 设 `VP9E_CONTENT_SCREEN`。

### 对我们有用的事实

- 一帧补多少没有固定值：由 libvpx 的 CBR 码率控制每帧自己往下走，Chromium 只给上下限（10–50）和一个“继续还是停”的判据（上一帧实际量化值 > 10）。
- 补的是“改过的区域”（active map 累积），不是整屏。
- 新画面优先，重编码的定时器故意比帧率慢 10%。
- 补清晰的停止判据用的是**编码器报告的实际量化值**，不是固定帧数。

---

## 2. TurboVNC / TigerVNC 的自动无损刷新

### TurboVNC（官方用户手册 7.4 节）

文档：[User’s Guide for TurboVNC 3.3.2，7.4 Automatic Lossless Refresh](https://cdn.jsdelivr.net/gh/TurboVNC/turbovnc@master/doc/index.html)

原文要点：
- `-alr timeout` 开启。“如果距离上一次发给某个 viewer 的帧缓冲更新已超过 timeout 秒，服务器就给该 viewer 发一份数学上无损的、‘符合 ALR 条件’且受过有损压缩影响的屏幕区域副本。”
- 可用 `-alrqual` / `-alrsamp` 改成用 JPEG 发刷新（即“有损但更好”，不是无损）。
- 设计用途：低带宽下用低质量 JPEG 做 3D 旋转，停下后发无损图。
- 默认只有 `X[Shm]PutImage()` 画出来的区域，以及源区域受过有损影响的 CopyRect 目标区域符合条件；设 `TVNC_ALRALL=1` 才全屏。原因（原文）：避免“闪烁光标困境”——光标闪烁比 ALR 超时更频繁，会让“距上次更新的空闲时间”永远攒不够。
- 官方说明：一直重复渲染同一图像的程序会让 ALR 失效，除非开启帧间比较（interframe comparison）。

更新日志（[ChangeLog.md](https://github.com/TurboVNC/turbovnc/blob/main/ChangeLog.md)）里读到的几条：
- 较早版本（ChangeLog 1.x 段）加入 ALR：“在不活动期间发一份数学上无损的屏幕副本”。
- 区域如果先用 JPEG 发、后来又用无损子编码（如 color index）重发，就不再发 ALR。
- 开帧间比较时，ALR 忽略“内容被完全消除的冗余更新”。
- 在流控扩展下，ALR 超时会被临时抬到大于测得的网络往返时间，避免高延迟网络上性能下降。

### TigerVNC（源码）

文件：[`common/rfb/EncodeManager.cxx`](https://github.com/TigerVNC/tigervnc/blob/master/common/rfb/EncodeManager.cxx)、[`common/rfb/VNCSConnectionST.cxx`](https://github.com/TigerVNC/tigervnc/blob/master/common/rfb/VNCSConnectionST.cxx)

- 用 `lossyRegion` 记录“当前是有损画面的区域”，`pendingRefreshRegion` 记录“该刷新的区域”。
- 触发：`RecentChangeTimeout = 50`（毫秒，第 65 行）。每次发更新后启动 50 ms 定时器；`handleTimeout` 里把“有损区域减去最近 50 ms 内又变过的区域”加入待刷新区域；如果还有没进待刷新的有损区域，定时器重复。**最近变过的区域不刷，等它静止。**
- 只补哪些：`getLosslessRefresh`（约第 551–602 行）从待刷新区域里**随机**取矩形（注释：不要反复损坏再恢复同一个矩形），累加到面积上限，最后一个矩形按“最窄的轴”截断以免产生细条。
- 限速（`VNCSConnectionST::writeLosslessRefresh`，约第 1069–1140 行）：
  - 有真实更新要发（`msToNextUpdate() == 0`）就不发刷新；
  - 上限 `maxUpdateSize = bandwidth × nextUpdate / 1000`，带宽取拥塞控制估计值，**硬编码封顶 5,000,000 字节/秒**（原文注释：“Hard coded value for maximum CPU throughput”）；
  - 压缩比保守按 2:1、4 字节/像素换算成像素数；
  - 刷新前后各发一次 RTT ping，用来测量。
- 刷新用的编码器必须是无损的（`losslessQuality`），否则不能用（约第 453–485 行）。

### 对我们有用的事实

- VNC 系的做法是**两种画质：有损和无损，没有中间档**，静止后一次补成无损；分批靠“区域 + 带宽预算 + 随机顺序”，而不是逐步降量化。
- 触发看“该区域是否静止（TigerVNC 50 ms；TurboVNC 秒级，参数 timeout）”，而不是整屏。

---

## 3. WebRTC（libwebrtc）

文件（webrtc-mirror 镜像，`main`）：
[`video/quality_convergence_monitor.cc`](https://github.com/webrtc-mirror/webrtc/blob/main/video/quality_convergence_monitor.cc)、
[`video/quality_convergence_controller.cc`](https://github.com/webrtc-mirror/webrtc/blob/main/video/quality_convergence_controller.cc)、
[`video/frame_cadence_adapter.cc`](https://github.com/webrtc-mirror/webrtc/blob/main/video/frame_cadence_adapter.cc)、
[`video/frame_cadence_adapter.h`](https://github.com/webrtc-mirror/webrtc/blob/main/video/frame_cadence_adapter.h)

### 做法

**谁来补**：零赫兹（zero-hertz）屏幕共享模式下，`ZeroHertzAdapterMode` 在没有新抓屏帧时**重复送入同一帧**（`ProcessRepeatedFrameOnDelayedCadence`，把 update rect 设为空、`is_repeat_frame = true`）。这些重复帧让编码器继续在同一输入上收敛。

**什么时候停快、什么时候放慢**：
- 没收敛时，按帧间隔重复（`RepeatDuration(false) = FrameDuration()`）。
- 全部空间层“收敛”后，改成**空闲重复**，周期 `kZeroHertzIdleRepeatRatePeriod = 1000 ms`（注释：折中于 RTP 接收端 3 秒关键帧超时、其他后端限制和最坏 RTT）。
- 另有 `kOnDiscardedFrameRefreshFramePeriod = 3`：等 3 个帧周期没有新帧就开始请求刷新帧。
- 一旦有新帧，重新当作“未收敛”（`ResetQualityConvergenceInfo`）；发关键帧后也重置，原因注释：关键帧要很多字节。

**怎么判断收敛（`QualityConvergenceMonitor::AddSample`，原文算法注释）**：
0. 静态阈值：QP 不高于 `static_qp_threshold` 就算收敛。
1. 不是刷新帧（输入与上一帧相同）则清空窗口，返回未收敛。
2. 已经收敛则保持。
3. 把 QP 加入窗口（最近 6 个 + 更早 6 个）。
4. 窗口不够长则未收敛。
5. 动态判据：`avg(past) <= dynamic_qp_threshold` 且 `avg(past) <= avg(recent)`，即“QP 已经降到够低，并且不再继续下降”。
- 动态阈值 = 静态阈值 + `alpha`(0.06) × 编码器最大 QP（VP8 127、VP9 255、AV1 255）。
- 静态阈值默认：VP8 15、VP9 32、AV1 60，也取编码器上报的 `min_qp`。VP8 屏幕共享上报 `kScreenshareMinQp = 15`（`libvpx_vp8_encoder.cc`）；VP9 屏幕共享的 `kMinQP = 32`（`libvpx_vp9_encoder.cc`）。

**VP8/VP9 屏幕共享的其他设置（原文）**：
- VP8：`VP8E_SET_STATIC_THRESHOLD` 屏幕共享为 100（普通为 1），注释“让更多屏幕内容被判为静止”；`VP8E_SET_SCREEN_CONTENT_MODE = 2`（更激进的码控，目标码率大幅超出时丢帧）。
- VP9：屏幕共享下有“稳态”帧率限制器：连续 5 帧（`kFramesBeforeSteadyState`）更新区域为空、QP ≤ 32 且帧大小小于阈值后，降到 `kMinFps = 5`。

**H.264（`modules/video_coding/codecs/h264/h264_encoder_impl.cc`）**：
- 屏幕共享时设 `iUsageType = SCREEN_CONTENT_REAL_TIME`；`iRCMode = RC_BITRATE_MODE`。
- `quality_convergence_controller.cc` 对 H.264 **返回 -1，等于关闭静态阈值**，且动态检测只对 VP8/VP9/AV1 开启（`quality_convergence_monitor.cc` 的 switch 里 H264 不设置 `dynamic_config.enabled`）。
- 推断（读代码得出，没有看到实测）：H.264 路径下 WebRTC 永远判定“未收敛”，重复帧按帧间隔一直发下去，不会进入 1 秒空闲重复。`h264_encoder_impl.cc` 里没有找到任何“补清晰”的专门逻辑；QP 门限 24/37 只是质量缩放器（scaling_settings）用的。

### 对我们有用的事实

- WebRTC 和 Chromium 一样：**靠重复送入同一帧让编码器自己收敛**，只是 WebRTC 多了“收敛判据”和“收敛后降到 1 Hz”。
- 收敛判据是 **QP 的统计趋势**（够低 + 不再下降），不是固定帧数。
- H.264 在 WebRTC 里没有这套判据，是空白。

---

## 4. 其他开源项目

### RustDesk（有，且最接近我们的场景）

文件：[`src/server/video_service/static_refresh.rs`](https://github.com/rustdesk/rustdesk/blob/master/src/server/video_service/static_refresh.rs)，调用点 [`src/server/video_service.rs`](https://github.com/rustdesk/rustdesk/blob/master/src/server/video_service.rs)（`StaticRefresh`、`repeat_encode_counter`）

- 做法：抓屏空闲（`WouldBlock`）时，**把缓存的最后一帧 YUV 重新送去编码**，直到达到上限；有新帧立即清零重来（`on_frame`）。
- 参数（原文）：
  - 首次重复间隔 `INITIAL_REPEAT_INTERVAL = 100 ms`，之后按 `max(帧间隔, 1/30 s)`；VP8/VP9 每次都等 ≥100 ms。
  - 重复帧数上限 `max_repeat_frames(codec, quality, backend)`：**按编码器和质量档的实测表**。例如 VP8：(低 20, 中低 20, 中高 20, 最佳 30)；VP9：(40, 20, 20, 20)；H.264 软编（`_`）：(90, 60, 60, 30)；H.264 NVENC：(80, 50, 60, 30)；AMF：(150, 150, 170, 110)；AV1 返回 0（不做）。
  - 表上方注释：这些值来自实测，“Original 值来自 AI 跑的测试，Active 值是对照 1080p 截图每 10 个重复输出抓一次后调整的”。
  - 停止条件：达到上限、连续 100 次没有输出（`MAX_REPEAT_NO_OUTPUTS`）、失败 3 次（非 VP8/VP9/AV1）。
  - 注释：“静态细化不能算作运动，以免影响自适应码率。”
- 没读到：重复帧用的量化值怎么变（它依赖各编码器自己的码控）。

### Sunshine

文件：[`src/video.cpp`](https://github.com/LizardByte/Sunshine/blob/master/src/video.cpp)，约第 2741–2783 行。
- 编码循环有最低帧率：`minimum_fps_target`（配置 > 0 取配置，否则 `framerate / 2`），`images->pop(max_frametime)` 等不到新图就用上一图继续编。注释原文：“Encode at a minimum FPS to avoid image quality issues with static content”。
- 没找到更多“补清晰”逻辑。

### Apache Guacamole（guacamole-server）

文件：[`src/libguac/display-worker.c`](https://github.com/apache/guacamole-server/blob/main/src/libguac/display-worker.c)（`LFR_guac_display_layer_should_use_jpeg`）、[`src/libguac/display-plan.h`](https://github.com/apache/guacamole-server/blob/main/src/libguac/display-plan.h)
- 不是“先糊后补”，而是**按区域历史更新帧率选格式**：帧率 ≥ `GUAC_DISPLAY_JPEG_FRAMERATE`（3 fps）、面积 > `GUAC_DISPLAY_JPEG_MIN_BITMAP_SIZE`（4096）、且 PNG 压缩效果不更好，才用 JPEG；否则用无损 PNG（或 WebP）。图层被设为 lossless 时一律不用 JPEG。
- 没找到“静止后把有损区域重发成无损”的代码（在 `display-*.c` 里搜 lossless 只找到上面这些）。

### FreeRDP / RDP 渐进式编码（RemoteFX Progressive）

源码：[`libfreerdp/codec/progressive.c`](https://github.com/FreeRDP/FreeRDP/blob/master/libfreerdp/codec/progressive.c)；
协议：[MS-RDPEGFX 2.2.4.2.1.5.5 RFX_PROGRESSIVE_TILE_UPGRADE](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/ed6495ce-0849-4718-96f4-84596cc57969)、[4.1.2.1.2 Encode Frame #1 at 50%](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/acb4388c-d0fe-40f1-9dd7-e7307a6452b2)、[3.2.8.1 RemoteFX Progressive Codec Compression](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegfx/f8c3c134-bf99-45eb-be7e-d22c8178255f)

读到的原文：
- 第一遍用 `RFX_PROGRESSIVE_TILE_FIRST`，之后改善质量的各遍用 `RFX_PROGRESSIVE_TILE_UPGRADE`（blockType 0xCCC7）。
- 解码端有持久的渐进状态；收到某块的首个渐进片段（或包含整块全部数据）时先清空，之后每遍的熵解码结果**加到当前帧上**（`progressive.c` 里 `progressive_rfx_upgrade_coefficient` 把升级值加到已有系数）。
- 升级遍里发的是 **DRS**：协议文档 4.1.2.1.2 节原文：“量化后的数据被反量化再从输入中减掉，得到剩余要发的值（DRS），再按 50% 渐进质量量化。”即**每一遍对“输入 − 上一遍的重建”再量化**。
- 两个质量档的位位置之差决定本遍发多少位幅度；上一遍已经发过非零值的系数，本遍直接用原始位（raw bits）发，其余用 SRL（简化游程）编码。示例里 LL 带这一遍不发新位。
- 质量索引 0xFF 表示全渐进表（质量 100%、系数全零）。
- 解码器按 tile（`xIdx`/`yIdx`）处理，每个 tile 带自己的 `quality`。
- 与旧 RemoteFX 比：去掉了帧间差分，加了子带差分，渐进编码做进熵编码器里。

**为什么这样分层：协议文档里我没有读到原因说明。** 第 4 节只写了机制。FreeRDP 里也没有解释设计原因的注释。

### xrdp

没找到原文（没有去读它的源码）。

---

## 5. openh264 源码：静止宏块什么时候被跳过

以下读的是 [cisco/openh264](https://github.com/cisco/openh264) `master`，路径前缀 `codec/encoder/core/src/`。我们实际用的 openh264 版本（crate 绑定的版本）没有核对，行为可能略有出入。

### 5.1 屏幕内容模式的开关（`encoder_ext.cpp` 约第 278–297 行）

屏幕内容模式下参数校验会强制：自适应量化关（AdaptiveQuant）、背景检测关（BackgroundDetection，日志原文“is not supported yet for screen content, auto turned off”）、场景变化检测开。`encoder.cpp` 第 206 行：只有“屏幕内容 && 场景变化检测开 && 复杂度模式 < HIGH_COMPLEXITY”才启用 SCD（Scene Change Detection）跳过判定。

### 5.2 屏幕内容的静止跳过（`svc_mode_decision.cpp`）

`WelsMdInterMb`（`svc_base_layer_md.cpp` 第 1858 行）顺序：背景跳过 → **SCD 静止/滚动跳过** → 普通 P-skip 判定。屏幕内容下第一个是空操作，第二个生效。

- 静止判定（`JudgeStaticSkip`，约第 323 行）：某个 8×8 块是否 `COLLOCATED_STATIC` 来自预处理阶段的场景检测，比较的是**原始输入帧和参考原始帧**（`pRefOri`），不是和重建帧比；整个 16×16 宏块的 4 个 8×8 块都静止，且 Cb、Cr 的 SAD 都为 0，才算。
- 然后（`MdInterSCDPskipProcess`，约第 468–500 行）：
  ```
  bQpSimilarFlag = (kiRefMbQp - kiCurMbQp <= DELTA_QP_SCD_THD || kiRefMbQp <= 26);
  ```
  其中 `DELTA_QP_SCD_THD = 5`（`svc_mode_decision.h` 第 54 行）。
- `SvcMdSCDMbEnc`（约第 392 行）：**只有“QP 相近”且“预测跳过 MV 等于当前 MV(0,0)”才真的当 P_Skip 处理**（不写残差）。否则这个宏块被标成 `MB_TYPE_16x16`、MV 取 0，并调用 `WelsInterMbEncode` + `WelsPMbChromaEncode` **写残差**（即重新用当前 QP 编码）。
- `kiRefMbQp` 是什么（`WelsMdUpdateBGDInfo`，第 267–281 行，非背景检测模式下也走同一函数，见 `encoder.cpp` 第 148 行）：如果该宏块有残差（`uiCbp != 0`）、或在 I 帧、或不是零运动，就记为当前 QP；**否则继承参考帧里记录的值**。即“该宏块上一次真正写过残差时的 QP”。

由此直接读出的行为（原文逻辑，不是实测）：
- 静止宏块 QP 降幅 **不超过 5**，且参考 QP ≤ 26 或差 ≤ 5 时，一律跳过、不更新画质。参考 QP 一旦 ≤ 26，**不管当前 QP 降多少，静止宏块都被视为“QP 相近”而跳过**。
- 只有“参考 QP > 26 且 参考 QP − 当前 QP > 5”才会触发重新写残差。写完后如果残差非零（`uiCbp != 0`），参考 QP 更新为当前 QP；如果量化后全零（`uiCbp == 0`），参考 QP 继承旧值，**下一帧会继续满足触发条件，再重编一次**（推断：这时每帧都付出一个非跳过宏块的头部字节但画质没变）。
- 这套 QP 相近判据**只在屏幕内容 SCD 路径里**；这条路径里没有按“残差是否落在死区”判断。

### 5.3 普通 P-skip 判定（`svc_base_layer_md.cpp`）

- `WelsMdInterJudgePskip`（约第 1906 行）：只有当邻居（左、上、左上、右上）至少有一个是 skip（`bTrySkip`）、参考宏块类型是 skip 等条件满足时才尝试。
- `WelsMdPSkipEnc`（约第 1423 行）：
  - 先对预测 MV 做运动补偿，算 SAD（亮度 + 两个色度，对**重建参考帧**）。`SAD == 0`，或 SAD < 邻居预测 SAD（`iSadPredSkip`），或比参考帧上这个宏块的 skip SAD 还小，就直接判 skip。
  - 否则对残差做 DCT，调用 `WelsTryPYskip`（`svc_encode_mb.cpp` 第 323 行）：亮度每个 4×4 块量化后，**任何系数绝对值 > 1 就不能 skip**；只有 ±1 的系数要累计“单系数代价”（`iSingleCtrMb`，注释出处 JVT-O079），**累计 ≥ 6 就不能 skip**。色度（`WelsTryPUVskip`）同理，阈值 ≥ 7，并先检查 2×2 DC。
  - 全部通过才 skip。
- 随后 `WelsMdInterDoubleCheckPskip`（约第 1937 行）：16×16 宏块若 `uiCbp == 0` 且参考索引 0 且预测 MV 与当前 MV 相同，改回 `MB_TYPE_SKIP`。这个函数只在 `WelsMdInterSecondaryModesEnc` 里调用（第 2017 行）；我 grep 到的 `SvcMdSCDMbEnc` 里没有调用。（没有读到更后面的写码阶段是否另有转换，这一点没查到。）

由此直接读出的行为：**普通路径里，宏块能不能 skip 取决于残差在当前 QP 下量化后是否“几乎全零”**——这就是“落在死区里就跳过、留着旧画质”的机制，有原文。

### 5.4 回到我们的两个推测

| 推测 | 原文支持程度 |
|---|---|
| 宏块没被跳过，按零残差的 P16×16 编了 | **有部分原文支持**：`SvcMdSCDMbEnc` 的非跳过分支把宏块设成 `MB_TYPE_16x16`、MV 为 0，并写残差；该分支里没有调用 `WelsMdInterDoubleCheckPskip`，所以量化后 `uiCbp == 0` 的宏块可能仍是 16×16，而不是 skip。没读到的是：我们的实测里 QP ≤ 17 那一档是不是走了这条分支。 |
| 逐步降时每步残差落在新步长死区里，宏块被跳过、留旧画质 | **有原文支持**两条机制：① 普通路径的 `WelsTryPYskip`（系数 > 1 不能跳；±1 的代价 ≥ 6 不能跳）；② 屏幕内容路径的 `bQpSimilarFlag`：QP 每步降 1，参考 QP 不变（宏块被跳过时继承旧值），只有累计降幅 > 5 才重编，且参考 QP ≤ 26 以后永远判“相近”。这两条都能让逐步降（每步小）的静止宏块被跳过。 |
| QP ≤ 17 才花字节、≥ 18 只要 14 字节 | **原文没有出现 17/18 这个分界**。能确定的是：判据里有 26、5 两个常数，没有 17/18。一个可检验的猜想（推断，原文没有）：QP ≥ 18 时静止帧上“原图 − 重建”的残差量化后全在死区，QP ≤ 17 时有些系数 > 1 或 ±1 代价累计 ≥ 6，宏块被写出残差。需要实验验证。 |

可用的验证手段（原文里有）：`svc_encode_slice.cpp` 约第 180–200 行有按宏块类型（PSkip / Inter16x16 / Intra…）的计数。可以在我们的编码路径上统计每帧各类型宏块数、以及 `uiCbp != 0` 的数量，就能区分“零残差 P16×16”和“带残差”。我没有确认这个计数在发布版是否打日志。

---

## 6. 对 PeerDesk 的含义

### 6.1 几种做法在原文里的位置

| 做法 | 谁这么做 | 原文里的特点 |
|---|---|---|
| **一步到底**（两档：有损 → 无损/最高质量） | TurboVNC ALR、TigerVNC（无损刷新）、Guacamole（直接用 PNG） | 静止到点后一次补；靠“按区域、按带宽预算、随机顺序”分批；刷新时有真实更新就让路；必须有无损/高质量编码器。 |
| **按区域分批** | TigerVNC（随机矩形 + 带宽预算）、Chromium（active map 累积改过的区域）、TurboVNC（只刷“符合条件”的区域） | 补的是“动过的区域”，不是整屏；最近变过的区域先不补。 |
| **逐帧重复让编码器自己收敛** | Chromium（top-off）、WebRTC（zero-hertz 重复帧）、RustDesk（static_refresh） | 把同一帧反复喂给编码器，靠码控每帧降量化；停止判据用编码器报告的量化值或实测帧数表。 |
| **渐进分层（成熟的位平面/残差细化）** | RDP RemoteFX Progressive | 每一遍编码“输入 − 上一遍重建”的残差，解码端累加；分 tile；编码器本身是为多遍细化设计的（不依赖重复喂同一帧）。 |

### 6.2 各做法的利弊

下面“原文”指上文已读到；“推断”是我的判断，没有原文直接支持。

**一步到底**
- 利：原文里的无损刷新就是这样；实现简单。与我们的实测同向：一步 12 比逐步降到 12 高 7 dB，字节相当。
- 弊（原文）：TurboVNC 和 TigerVNC 的“无损”是另一种编码器（Zlib/无损），不是同一个 H.264 码流的延续；我们要在 H.264 里做，对应的是“把 QP 一步降到目标”。
- 弊（推断）：一步到底的单帧字节数很大（我们的实测一步 616 KB），会占满链路、挤占新画面和输入；需要分区域限额，TigerVNC 的“带宽 × 距离下次更新的时间”的预算可以借鉴。

**按区域分批**
- 利（原文）：TigerVNC 和 Chromium 都这样；动过的区域才补，静止的大面积背景不用再花字节；新画面来时可以只打断正在补的那部分。
- 利（原文）：openh264 的屏幕内容路径本来就是逐宏块决策，区域分批可以在输入侧做（只把要补的区域“变”给编码器，其余仍是静止宏块，被跳过）。这是**推断**：原文只说明了跳过规则，没说区域分批能否这样利用。
- 弊（推断）：需要自己维护“哪些区域是有损的”的状态（TigerVNC 的 `lossyRegion`/`pendingRefreshRegion`），状态机要有出口和超时。

**逐帧重复（Chromium / WebRTC / RustDesk 路线）**
- 原文能说明的是：这类做法**依赖编码器的码控每帧往下降量化，且收敛需要的帧数很多**。RustDesk 的实测表给出 20 到 560 个重复帧，且按编码器和质量档分别标定，说明没有普适的数字。
- 对 openh264 的含义（推断 + 5.2 的原文）：openh264 在屏幕内容 SCD 路径里，静止宏块在参考 QP ≤ 26 后就不再因 QP 变化而重编（`bQpSimilarFlag`），所以“喂同一帧等它自己变清楚”在这个编码器上**可能不起作用或很慢**。这条需要用我们自己的编码路径实测确认。
- 我们现在的逐步降，在原文里最接近的就是这一类。

**渐进分层（RDP Progressive 路线）**
- 利（原文）：每遍编码的是“输入 − 上一遍重建”的残差，**不会因“同一帧重复喂入”而被跳过**；没有“QP 相近就跳”的问题。
- 弊（原文/推断）：它是独立的小波+位平面编码器，不是 H.264；我们要在 H.264 里做同样的事，需要自己控制每个宏块的残差量（比如强制 `uiCbp != 0` 或不走 skip），这是 openh264 公开接口里没有的，**原文里没有看到相关参数**（推断：要么改 openh264，要么换编码器的控制接口）。
- 为什么这样分层：**没找到原文**。

### 6.3 能直接落地的、有原文依据的几条

1. **停止判据用编码器实际报告的量化值/统计趋势，不用固定帧数**：Chromium（上一帧 QP > 10 就继续）、WebRTC（QP 窗口均值够低且不再下降）。RustDesk 用固定帧数表，但注释承认这是实测标定的、随编码器变。
2. **只补动过的区域**：Chromium 的 active map 累积、TigerVNC 的 lossyRegion；最近 50 ms 内变过的区域暂不补（TigerVNC）。
3. **新画面一律让路**：Chromium（定时器比帧间隔慢 10%，编码器忙就跳过）、TigerVNC（`msToNextUpdate() == 0` 就不发刷新；刷新大小受下次更新前可用字节限制）、RustDesk（`on_frame` 清零）。
4. **补清晰的流量不能计入“运动”**：RustDesk 注释：静态细化不能算运动，否则会影响自适应码率。我们的码率控制如果把补清晰的字节当作拥塞或活动信号，会出问题。
5. **收敛后降到低频保活**：WebRTC 收敛后 1 Hz，Chromium 保活 2 s，Sunshine 最低帧率为 `framerate / 2`。

### 6.4 原文没有给出答案的

- 一步到底 vs 逐步降在 H.264 上差 7 dB 的原因：原文里没有直接说明。openh264 的读法给出两个可能的机制（5.4 表），需要实验分开：
  - 实验 A：在 5.2 的 SCD 路径和 5.3 的普通路径上分别统计各类型宏块数和 `uiCbp`。
  - 实验 B：改变“参考 QP”的起点（例如降 QP 前先插入一次真实更新，或让宏块变成非静止）看逐步降是否还差。
- QP ≤ 17 / ≥ 18 的分界：原文没有；见 5.4。
- libvpx 码控每个重复帧具体降多少量化值：没读。

---

## 7. 没找到原文

- xrdp：没有读它的源码。
- Moonlight：Sunshine 只是服务端，Moonlight 客户端没读。
- libvpx 内部码控每帧如何下降量化值：没读，Chromium/WebRTC 的行为只能说“靠 libvpx 码控”。
- RDP Progressive “为什么这样分层”：协议文档（读到的 3 节）只写机制。
- WebRTC 里 H.264 屏幕共享是否有别的补清晰逻辑：在 `h264_encoder_impl.cc`、`video_stream_encoder.cc`、`frame_cadence_adapter.cc` 里没找到。
- openh264 各版本：只读了 `master`，没和我们使用的版本对照。
- TurboVNC 对 ALR “timeout” 的默认值：官方手册里只写了参数，没读到默认数字。

## 8. 主会话核对（2026-11-27）

1. **版本**：产品依赖 `openh264-sys2 0.9.10`，随包带上游源码。我在这份源码里核对了第 5.2 节的判据，和上游 master 一致：
   - `upstream/codec/encoder/core/src/svc_mode_decision.cpp` 第 480 行：
     `bQpSimilarFlag = (kiRefMbQp - kiCurMbQp <= DELTA_QP_SCD_THD || kiRefMbQp <= 26)`；
   - `inc/svc_mode_decision.h` 第 54 行：`DELTA_QP_SCD_THD 5`。
2. **这条判据能解释 B2 的逐步降那条链**（`data-m2b.md` 第 1 节，desc 链，12 个种子的中位）：
   - QP 30 → 24（第 1–6 帧）：每一帧和参考 QP 30 的差都 ≤ 5，静止宏块全部判成“QP 相近”而跳过。6 帧合计 50 KB，PSNR 只涨 0.4 dB。
   - QP 23（第 7 帧）：差 7 > 5，参考 30 > 26，整屏重新写残差，这一帧 128 KB，PSNR 从 36.05 升到 39.53 dB。
   - 之后参考 QP 变成 23（≤ 26），静止宏块照规则一律跳过，PSNR 每步只涨 0.1–0.3 dB。
   - 后面每步还要 22–65 KB，没有完全解释。推测是残差量化后全零（`uiCbp == 0`）的宏块继承旧的参考 QP，一直满足触发条件，每帧都被重编（第 5.2 节最后一条）。这一点没有验证。
   - 一步跳到底（hold 链第 1 帧）：参考 30 > 26，差 ≥ 10，整屏一次重编，所以字节差不多，PSNR 却高 7 dB。
3. **对产品的含义（任务“补清晰策略”）**：
   - 在 openh264 屏幕内容模式下，静止画面要想补清晰，必须从参考 QP > 26 一步降 6 以上。参考 QP 一旦 ≤ 26，静止宏块就补不上去了。
   - 产品现在逐步往下补，正好和这条规则对着干。
   - 这是后端的特有行为，应该写进编解码层的后端声明（`BackendTraits`），由控制层按声明选补清晰的方式，不写死在 QP 控制里。
   - 硬件编码器（VideoToolbox、Media Foundation、VA-API）要分别量，不能拿 openh264 的结论去套。
