# K. 编解码与传输链路排查：模块互相影响的根因、业内做法和模块边界（2026-11-25）

起因：用户 Intel Mac 被控、Windows 控制，画面糊到没法看、只有两三帧每秒，带宽是够的（往返 20 ms、不丢包）。
连着几版都在修局部问题，效果没有根本改善。这份文档先把整条链路摊开：每个模块做什么、谁影响谁、哪里在打架，
对照业内（WebRTC、Chromium 远程桌面、Sunshine、Parsec、苹果和微软的文档）的做法，定下模块边界，再分步改。

证据来源：
- 用户日志（2026-11-25 01:06–01:09，Mac 被控端 + Windows 控制端）；
- 本机真实二进制在 tbf 限速回环上的会话（`scripts/pace-lab.sh`，1.5 Mbps、单程 20 ms、排队上限 60 ms），
  用 `bcfdac6` 新加的诊断日志；
- 代码（行号以 `bcfdac6` 为准）；
- 三份调研（引用都在文中）。

**没有证据、只是推断的地方都标了“推断”。**

---

## 1. 结论摘要

1. **模块之间确实在互相放大**，单看每个模块都“合理”。用户那次的链条（日志和代码都能对上）：
   颜色转换慢（每帧 130–190 ms）→ 帧率调节降到 5 帧 → 每帧的码率额度变成原来的好几倍、编码器每秒码率上限也允许单帧很大
   → 每次画面变化都是一个大帧突发 → 一秒几十个包丢了要重传（日志：27、52、74）→ 码率估计判拥塞，降到“实际送达 × 0.85”，
   而实际送达因为只有两三帧很小 → 码率掉到 104 kbps → 升码率要求“送达达到目标的六成”，发得少永远达不到 → 一直卡在底线，画面糊。
2. **传输层有三处结构性问题**（本机限速会话的诊断日志直接看到）：
   - 两套拥塞控制叠在一起：我们的码率控制之下，QUIC 库（quinn）还有自己的 BBR，网络一丢包就把窗口压到 11 KB，
     我们的数据报在 QUIC 内部积压（缓冲设了 4 MB，1.5 Mbps 下能积 20 秒）或被挤掉，我们的估计器只看到“排队越来越高”；
   - 接收端的重传请求没有节流：每 20 ms 把所有缺的分片再要一遍，一秒 741 片，其中 506 片是“刚补过、还在路上”的重复请求；
   - 码率估计的两个主要测量（延迟趋势、包串测速）只在“新帧”上测，发送端一停（等关键帧确认、在途满）就完全没信号，只剩排队延迟。
3. **编码器用法和业内有差距**：单帧大小没有上限（业内是一帧大小的编码缓冲）；Mac 上“低延迟码率控制”和“每秒码率上限”
   同时开（Chromium 注明不兼容、会让编码器用不足码率）；两个平台都用 Baseline 档次；没标色彩矩阵；
   给桌面文字的码率区间偏低（业内 1080p60 一般 15–25 Mbps 起，我们常在 1–4 Mbps）。
4. **建议**：先定模块边界和接口契约（第 5 节），再按第 6 节的顺序一步一改，每步用同一组指标（仿真场景、真实二进制限速、
   微软轨迹、用户实机日志）量前后对比。顺序是先治“打架”（QUIC 双重限速、重传风暴、估计器在少发时乱降），再治编码器，
   最后是 Mac 采集直出 NV12 这类性能项。

---

## 2. 现在的链路（谁调谁、谁改谁）

被控端，一帧从采集到发出：

| 步骤 | 模块（文件） | 输入 | 输出 / 改了谁 |
|---|---|---|---|
| 采集 | `platform/capture.rs` + 各系统 | 屏幕 | RGB 画面（Mac 上 CPU 把 BGRA 转成 RGB） |
| 什么时候编 | `net/host.rs` 会话循环 | 画面变化、`pacer.busy`、`in_flight_full`、`governor.due`、`wants_frame` | 开始一帧 |
| 每帧额度 | `core/media.rs` `frame_budget`（736 行） | 码率估计 ÷ **帧率**，再按**排队延迟**收缩 | 交给编码器的“预算”（编码器换算回码率） |
| 编码分辨率 | `core/scale.rs` | 码率估计、关键帧实测送达速度、控制端窗口 | 缩放比例 |
| 编码 | `media/lib.rs` + `vt.rs` / `mf.rs` / `vaapi.rs` / OpenH264 | 画面、码率、是否关键帧 | 码流；CPU 缩放和颜色转换都在这里 |
| 帧率调节 | `core/governor.rs` | **整个编码任务的耗时（含颜色转换）**、帧大小 vs 预算 | 帧率上限（60→…→5） |
| 切片、纠错 | `core/media.rs` `finish_frame` | 码流、纠错比例（按丢包、网络画像） | 数据报 |
| 发送节奏 | `core/pacer.rs` | **默认关**：整帧一次发出 | — |
| QUIC | `net/tls.rs`、`net/node.rs` | 数据报 | **BBR 拥塞窗口 + 4 MB 发送缓冲**决定什么时候真发、挤掉哪些 |

控制端回馈，决定码率：

| 步骤 | 模块 | 输入 | 输出 |
|---|---|---|---|
| 重组 | `core/transport.rs` | 分片 | 收齐的帧、缺哪些分片 |
| 反馈 | `net/controller.rs`（每 20 ms） | 排队延迟、丢包、送达速率、趋势和包串（按帧分组测）、**所有缺的分片** | 反馈数据报 |
| 码率估计 | `core/estimator.rs` | 反馈 | 码率 |
| 重传 | `core/media.rs` `on_feedback_datagram` | 缺的分片 | 补发（同一分片隔“往返 + 排队”才再补） |
| 网络画像 | `core/media.rs` `adapt_profile` | 丢包 | 稳定 / 无线（改纠错比例和估计器参数） |

**同一个信号被好几个模块各自反应**：

- “排队延迟高”：码率估计降码率（`estimator.rs`），每帧额度再按排队收缩一次（`frame_budget`），
  QUIC 的 BBR 也在按自己看到的丢包和延迟收窗口——三层对同一件事各反应一次。
- “编码慢”：帧率调节降帧率，而每帧额度 = 码率 ÷ 帧率，于是单帧变大、突发变大，又反过来造成丢包和排队。
- “丢包”：码率估计（拥塞时降）、网络画像（换成无线、加纠错）、重传、关键帧，四个模块同时动。

---

## 3. 发现的问题（按影响排序）

每条：现象（证据）→ 代码 → 为什么 → 业内做法。

### P1 两套拥塞控制叠在一起，QUIC 内部积压和挤掉我们的数据报

- 现象（本机 1.5 Mbps 限速，被控端同一秒）：`QUIC 应用交出 202 个数据报，实际发出 176（发送缓冲挤掉约 26）；
  网络丢包 45 个，拥塞事件 2 次，拥塞窗口 11 KB`。之后排队延迟从 20 ms 一路涨到 542 ms。
  注意：“挤掉约 26”是“交出 − 实际发出”，里面既有被挤掉的，也有还积压在 QUIC 里没发的（日志措辞要改准）。
- 代码：`net/tls.rs:90–101` QUIC 用 BBR；`tls.rs:28` 数据报发送缓冲 4 MB。
- 为什么：RFC 9221 §5.4 规定 DATAGRAM 帧受 QUIC 拥塞控制约束；quinn 在窗口满时把数据报排在自己的队列里，
  队列满了**静默**丢最旧的（`quinn-proto/src/connection/datagrams.rs`，`send_datagram` 返回 Ok，不报丢弃）。
  所以 BBR 收窗口后，我们的分片在本机排队（发送时间戳是进队时打的，接收端看到的“排队延迟”里有一大段其实是本机积压），
  被挤掉的分片接收端当成网络丢包来要重传，补发的又进同一个队列。
- 业内：RTP-over-QUIC 草案（draft-ietf-avtcore-rtp-over-quic §8.2）说两套完整的拥塞控制叠加“can have unintended effects”，
  建议只用媒体层的估计（GCC、SCReAM、NADA），把媒体按低于容量的速率节奏发出，让 QUIC 的拥塞控制基本不触发。
  BBR 在“突发 + 空闲”的应用受限流量上表现差有专门的草案讨论（draft-huitema-ccwg-bbr-realtime）。
  quinn 的拥塞控制可以换（`TransportConfig::congestion_controller_factory`，`Controller` trait 是公开的）。
- 当初为什么选 BBR：`tls.rs:96` 注释——实测 Cubic/NewReno 把随机丢包当拥塞，8 秒一帧没送到。
  那是在“我们自己不节奏发送、整帧突发”的前提下比的，前提变了要重新比。

### P2 接收端的重传请求没有节流，形成重传风暴

- 现象（本机）：被控端一秒收到重传请求 741 片，补发 112，“刚补过、可能还在路上”没补 506，“这轮额度用完”没补 123；
  控制端一秒发出重传请求 198 片、过期分片 42。
- 代码：`core/transport.rs:397` `missing()` 只看“第一个分片到了多久”，不记每个分片上次要过没有；
  `net/controller.rs:25` 每 20 ms 一次反馈，每次最多 48 片（`media.rs:41`）。
- 为什么：同一个缺片每 20 ms 被要一次，直到补到为止；补发的包还在路上（排队几百毫秒）时会被要十几次。
  发送端靠“往返 + 排队之内不重复补”挡住了大部分，但回传带宽和处理都被占了。
- 业内：WebRTC `modules/video_coding/nack_requester.cc`——第一次请求要等后面更新的包到了（防乱序误判）；
  之后每个包**隔一个往返时间**才再请求；有重试上限；靠纠错或重传恢复的包放进“已恢复”列表不再请求；
  列表太长就清空、改要关键帧。

### P3 码率估计在“发送端自己少发”时按送达速率降，而且升不回来

- 现象（用户 Mac 日志）：每次降码率都正好是“当时实际发出 × 0.85”（发出约 122 kbps → 降到 104 kbps），
  之后一直 100 kbps。本机日志里也能看到“发送端没用满码率”时照样按送达速率降。
- 代码：`core/estimator.rs` 拥塞时 `target = 送达 × 0.85`（约 440 行）；升码率要求“送达 ≥ 目标的六成”（488 行）。
- 为什么：发送端只有两三帧每秒时，“送达速率”是发送端自己少发造成的，不是链路能力。按它降就是把码率降到发送量以下；
  而升码率的门槛又要求发送量够大，形成死锁。
- 业内：WebRTC GoogCC 有“应用受限”（ALR）检测（`goog_cc/alr_detector.cc`）：发送量明显低于估计时，
  基于丢包的估计不使用已确认速率（`loss_based_bwe_v2.cc`，`not_use_acked_rate_in_alr` 默认开）；
  降码率只在延迟梯度判过载时发生（`aimd_rate_control.cc`），不因为“测到的速率低”而降；
  升码率上限是“已确认速率 × 1.5 + 10 kbps”，不设“要用满六成”这种门槛；空闲时由探测（`ProbeController`）主动测容量。

### P4 码率估计的测量依赖新帧，发送端一停就没信号

- 现象：本机日志里“延迟趋势 0.0 ms”“包串测得容量 未知”贯穿整段；仿真里 400 kbps 场景的特征记录同样全是 0。
- 代码：`core/estimator.rs` 100–170 行，分组以帧为单位，一组要等下一帧的第一个包到了才结算；旧帧的包（重传）不算。
- 为什么：等关键帧确认、在途满、只在补发重传时，没有新帧，趋势和包串都停在那里。估计器只剩“排队延迟”一个信号，
  而排队延迟里还混着 P1 的本机积压。
- 业内：WebRTC 在拥塞或暂停时每 500 ms 发一个填充包保持反馈不断（`pacing_controller.cc`）；
  估计器用发送端时间戳和确认来算，不依赖“下一帧来了”；空闲时用探测包测容量。

### P5 单帧大小没有上限，一次画面变化就是一个大突发

- 现象：用户 Mac 日志里大帧（一秒 3 帧共 1300 kbps 左右）紧接着就是几十个重传。
- 代码：`media/vt.rs:698–728` VideoToolbox 的码率上限是“每 **1 秒**不超过平均码率的 1.5 倍”——单帧可以吃掉一整秒的量
  （30 Mbps 时一帧最多约 5.6 MB）；`media/mf.rs:374` Media Foundation 用 CBR 但没设缓冲大小（`AVEncCommonBufferSize`）；
  发送节奏默认关（`core/pacer.rs`），整帧一次发出。
- 业内：Sunshine 的 NVENC 和通用 ffmpeg 路径都把编码缓冲设成**一帧的大小**（`vbvBufferSize = bitrate / framerate`，
  `src/nvenc/nvenc_base.cpp`、`src/video.cpp`），文档说关掉两遍编码会“偶尔超码率、接着丢包”；AMD 用 `max_au_size` 兜底。
  WebRTC 的发送节奏器限制突发（`pacing_controller.cc`）。

### P6 帧率调节把“CPU 慢”和“带宽不够”混在一起，而且影响每帧额度

- 现象：用户 Mac 日志开头 5 秒帧率上限从 60 一路降到 5，原因是一帧要 130–190 ms 转颜色。
- 代码：`core/governor.rs` 用整个编码任务的耗时（含 CPU 缩放和颜色转换）判断 CPU 忙，连续 3 帧就降一档（17 行），
  同一个调节器还按“帧超预算两倍”降帧率；`core/media.rs` 每帧额度 = 码率 ÷ 帧率。
- 为什么：帧率一降，单帧额度按比例变大（5 帧时是 30 帧的 6 倍），把 CPU 问题变成了网络突发问题（P5）。
- 业内：WebRTC 把 CPU 和带宽分成两个资源：CPU 过载检测（`video/adaptation/overuse_frame_detector.cc`）只看编码耗时 vs 采集间隔，
  第一次检查 100 ms 后、之后每 5 秒一次，连续多次才降；画质（QP）是另一个资源；
  由**一个**适配处理器按降级偏好统一决定降帧率还是降分辨率（`video_stream_encoder_resource_manager.cc`）。
  编码器拿到的是“目标码率 + 实测输入帧率”，单帧大小由编码器的缓冲模型约束，不是我们这样直接除。

### P7 每帧额度又按排队延迟收缩一次（第二个对排队的反应）

- 代码：`core/media.rs` `frame_budget`，排队超过 50 ms 目标时额度按比例缩（`QUEUE_TARGET_US`）。
- 为什么：码率估计已经因为排队降了码率，这里再缩一次，等于对同一个信号反应两次；排队里还有 P1 的本机积压。
- 业内：WebRTC 的编码目标只来自带宽估计（减去保护开销），排队由节奏器管。

### P8 丢包恢复靠整帧关键帧，关键帧没确认时所有帧都等着

- 代码：`media/lib.rs` `on_lost` → 下一帧关键帧；`core/media.rs:354` `in_flight_full`：关键帧没确认之前不发依赖它的帧。
- 为什么：丢包 → 大关键帧 → 窄带宽上关键帧本身又容易丢 → 画面停住等它。
- 业内：Sunshine 用参考帧失效（`NvEncInvalidateRefFrames`），不行才要关键帧；NVENC 支持帧内刷新、长期参考帧；
  我们 3.7.8 节验证过 OpenH264 的长期参考帧恢复可用（只在摄像头模式）。

### P9 码率区间对桌面文字偏低

- 现象：用户日志里目标码率大部分时间在 1–4 Mbps，降到 100 kbps 时更不用说。
- 业内：Parsec 给 1080p60 的带宽要求是 25 Mbps 以上；Sunshine 每帧额度 = 码率 ÷ 帧率，4 Mbps、30 帧只有约 16 KB 一帧，
  装不下 1080p 文字的关键帧；WebRTC 屏幕共享有 1.2 Mbps 的最低码率（`kDefaultMinScreenSharebps`）。
  调研给的参考（非厂商官方）：1080p 15–25 Mbps、1440p 25–40 Mbps、4K 40–60 Mbps，静止时花得少。
- 我们的下限是 100 kbps（`net/host.rs` `MIN_BPS`），对文字画面没有意义。

### P10 编码分辨率策略跟码率估计耦合

- 代码：`core/scale.rs`，升回原尺寸的条件用码率估计和关键帧实测送达速度。
- 为什么：码率估计一错（P3），分辨率决策跟着错；分辨率一变又出关键帧（P8）。
- 业内：分辨率由统一的降级处理器按 QP / CPU 信号决定（WebRTC），屏幕共享通常保分辨率、先降帧率。

### P11 硬件编码器设置和业内的差距

| 项 | 我们 | 业内 | 来源 |
|---|---|---|---|
| Mac：低延迟码率控制 + 每秒码率上限 | 两个都开（`vt.rs:603`、`vt.rs:728`） | Chromium 注明两者不兼容、会让编码器用不足码率；Sunshine 低延迟模式下去掉码率上限 | Chromium `vt_video_encode_accelerator_mac.mm`；Sunshine `vt_encoder.mm` |
| 档次 | Baseline（Mac、Windows） | Main / High（CABAC 同画质省约一成码率） | Sunshine 文档；Chromium、OBS |
| 色彩标记 | 没标 | Chromium、Sunshine 都显式标色彩矩阵、原色、传递函数 | 同上 |
| Windows：编码缓冲大小 | 没设 | 设成约一帧到几帧 | 微软 H.264 编码器文档；Sunshine |
| Windows：Intel 时域层数 | 没设 | Chromium：Intel 驱动要显式设成 1 | Chromium `media_foundation_video_encode_accelerator_win.cc` |
| 码率模式 | CBR | CBR 或峰值受限 VBR，加单帧缓冲 | 同上 |

注：选 Baseline 是因为接收端可能用 OpenH264 软件解码，它不支持 CABAC 等 High 档次工具（`mf.rs:7`、`vt.rs:328`）。改档次要先让接收端都能解 High（硬件解码或换软件解码器），改之前要先验证。

### P12 Mac 采集到编码之间在 CPU 上逐像素处理

- 现象：用户 Mac 每帧颜色转换 130–190 ms（我加的 CPU 缩放占大头，本机测 68 ms）。
- 业内：ScreenCaptureKit 用于编码时用 `420v`（WWDC22 10155 示例），由显卡缩放和转换；缓冲在显存（IOSurface）。
- 本地已有改动（未提交，待本文档定方向后再提）：CPU 缩放提速一倍；采集直接交 NV12 的编码通路。

---

## 4. 业内是怎么分模块的（摘要）

**WebRTC（发送端）**：GoogCC 是协调者，下面每个子模块单一职责——ALR 检测（发送量 vs 估计）、已确认速率估计、
延迟梯度 + AIMD（只在过载时降）、基于丢包的估计（区分随机丢包和拥塞丢包，ALR 时不用已确认速率）、探测控制器（空闲时测容量）。
输出“目标码率”和“节奏速率”（目标 × 节奏倍数），交给节奏器和码率分配器。编码器只接收“目标码率 + 实测帧率”。
CPU 过载、画质（QP）是降级的两个独立资源，由一个处理器按降级偏好决定降帧率还是分辨率。NACK 有独立的重试规则。

**Chromium 远程桌面**：采集调度（什么时候采）独立于编码；编码器用 VP9 屏幕内容模式、CBR、只按请求出关键帧、
静止后“循环刷新”补清晰；每个阶段（等采集、采集、等编码、编码）的耗时单独上报。

**Sunshine**：码率固定（用户设）、CBR + 一帧大小的编码缓冲、固定比例纠错、按恒定速率节奏发送、
丢包靠参考帧失效或关键帧恢复，不做码率自适应。

**Parsec**：自研 UDP 协议，宣称拥塞前就检测、视频不设缓冲；细节未公开。

## 5. 我们的模块边界（建议，待确认）

原则（来自上面的调研）：
1. 每个信号只由一个模块消费并做决定；其他模块只读这个决定，不对原始信号再反应一次。
2. 码率估计输出的是“链路能承受多少”，不是“编码器该用多少”；发送端少发时（应用受限）不能用发送量去推链路能力。
3. CPU 和带宽是两个独立的资源，由一个降级处理器统一决定降什么（屏幕内容：保分辨率、先降帧率；文字清晰最后让步）。
4. 只有一个出口控制发送速率和突发（节奏器）；QUIC 层不再自己限速，或者限速远高于我们的估计、只做安全网。
5. 单帧大小由编码器的缓冲模型限定（约一帧），不由“码率 ÷ 帧率”在外面算。
6. 每个模块的决定和输入写日志（已做，`bcfdac6`）。

| 模块 | 唯一职责 | 输入 | 输出 | 不做的事 |
|---|---|---|---|---|
| 采集调度 | 什么时候采、采多大 | 画面变化、目标帧率、编码尺寸 | 画面（尽量直接是 NV12） | 不碰码率 |
| 链路估计 | 链路能承受多少 | 延迟梯度、丢包（区分随机和拥塞）、已确认速率（仅非应用受限时）、探测 | 链路估计 | 不决定编码参数、不看 CPU |
| 码率分配 | 编码器该用多少 | 链路估计、纠错和重传开销、码率上下限 | 编码目标码率、节奏速率 | 不对排队再反应一次 |
| 节奏器 | 按速率发、限制突发、唯一出口 | 节奏速率、待发分片 | 数据报 | 不决定码率 |
| 编码器适配 | 按目标码率出码流，单帧不超缓冲 | 目标码率、实测帧率、关键帧 / 恢复请求 | 码流、耗时、QP | 不做网络判断 |
| CPU 过载检测 | CPU 跟不跟得上 | 编码各步耗时 vs 帧间隔（秒级、连续多次） | “CPU 过载 / 轻松” | 不看带宽 |
| 降级处理器 | 唯一决定降帧率还是分辨率 | CPU 信号、画质信号（QP / 码率不够）、偏好 | 目标帧率、编码尺寸 | 不改码率 |
| 可靠性 | 丢了怎么补 | 接收端按往返节流的缺片请求、丢包率 | 重传、纠错比例、恢复请求（参考帧失效 / 关键帧） | 不改码率 |
| 接收端 | 重组、解码、显示、反馈 | 分片 | 画面、反馈（含发送时间戳、到达时间、缺片） | — |

## 6. 修改计划（每步单独提交、单独量）

每一步都用同一套指标量前后：仿真场景（`crates/e2e`，含 slow-encoder、very-narrow）、真实二进制限速（`scripts/pace-lab.sh`）、
微软真实轨迹、用户实机日志。门槛只升不降。

| 步 | 改什么 | 解决 | 怎么量 |
|---|---|---|---|
| S1 | QUIC 不再自己限速（换成不限速的拥塞控制，或保留但窗口远高于我们的估计）；数据报缓冲缩到一两帧；本机丢的分片不当网络丢包 | P1 | pace-lab 的 QUIC 行：本机积压 / 挤掉 → 0；排队延迟回到真实链路水平 |
| S2 | 接收端重传请求按往返节流、有重试上限、已恢复的不再要 | P2 | 重传请求数接近真实缺片数；回传带宽下降 |
| S3 | 链路估计：应用受限检测，少发时不按送达速率降、不设“用满六成才升” | P3 | slow-encoder 场景；用户 Mac 那种“编码慢、带宽够”不再掉到底线 |
| S4 | 测量不断供：按发送时间戳和到达时间算趋势，不依赖新帧；需要时发探测 / 填充 | P4 | 趋势和容量在等关键帧期间也有值 |
| S5 | 单帧上限：编码器缓冲约一帧（VT 去掉每秒上限、MF 设缓冲大小）；节奏器默认开，按新的估计重新校准 | P5、P7 | 大画面变化后不再成批重传 |
| S6 | CPU 和带宽分开：CPU 过载检测按秒级、看分步耗时；降级处理器统一决定帧率和分辨率；每帧额度不再 ÷ 帧率 | P6、P10 | 编码慢时帧率降但单帧不变大 |
| S7 | 硬件编码器设置对齐业内：档次（先解决接收端 OpenH264 只解 Baseline）、色彩标记、Intel 时域层数；码率区间按分辨率给下限 | P9、P11 | 同码率下 PSNR；实机文字清晰度 |
| S8 | 丢包恢复：参考帧失效 / 长期参考帧，减少整帧关键帧 | P8 | 高丢包微软轨迹的关键帧数和冻结 |
| S9 | Mac 采集直接出 NV12（显卡缩放），去掉 CPU 逐像素转换；之后是 Windows 零拷贝 | P12 | 用户 Mac 每帧转换耗时、帧率 |

S1–S4 是“模块不再打架”，先做；S5–S7 是编码器；S8–S9 是恢复和性能。

## 7. 需要产品负责人决定的

1. 第 5 节的模块边界是否同意（之后的改动都按它来，不再在模块之间互相打补丁）。
2. S1：QUIC 层完全不限速（我们自己负责公平和安全），还是保留一个很宽的安全网。前者干净，后者对同一网络上的别的流量更友好。
3. S7：码率下限按分辨率给多少（文字清晰优先，意味着窄带宽时宁可降帧率也不降到几百 kbps）。
4. S8：是否接受把 OpenH264 换成摄像头模式来换取长期参考帧（3.7.8 节的代价）。

## 引用

- RFC 9221 §5.4：https://www.rfc-editor.org/rfc/rfc9221#section-5.4
- quinn 数据报队列：https://github.com/quinn-rs/quinn/blob/main/quinn-proto/src/connection/datagrams.rs
- quinn 拥塞控制接口：https://github.com/quinn-rs/quinn/blob/main/quinn-proto/src/congestion.rs
- RTP over QUIC §8：https://datatracker.ietf.org/doc/html/draft-ietf-avtcore-rtp-over-quic
- BBR 与实时媒体：https://www.ietf.org/archive/id/draft-huitema-ccwg-bbr-realtime-00.html
- WebRTC GoogCC、节奏器、NACK、适配：https://webrtc.googlesource.com/src/+/refs/heads/main/ （`modules/congestion_controller/goog_cc/`、
  `modules/remote_bitrate_estimator/aimd_rate_control.cc`、`modules/pacing/pacing_controller.cc`、
  `modules/video_coding/nack_requester.cc`、`video/adaptation/`）
- Chromium 远程桌面：https://chromium.googlesource.com/chromium/src/+/refs/heads/main/remoting/
- Chromium VideoToolbox / Media Foundation 编码：`media/gpu/mac/vt_video_encode_accelerator_mac.mm`、
  `media/gpu/windows/media_foundation_video_encode_accelerator_win.cc`
- Sunshine：https://github.com/LizardByte/Sunshine （`src/video.cpp`、`src/stream.cpp`、`src/nvenc/nvenc_base.cpp`、
  `src/platform/macos/vt_encoder.mm`、`docs/configuration.md`）
- Parsec：https://parsec.app/blog/a-networking-protocol-built-for-the-lowest-latency-interactive-game-streaming-1fd5a03a6007
- 苹果 ScreenCaptureKit：WWDC22 10155、10156；https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration
- 微软 H.264 编码器：https://learn.microsoft.com/windows/win32/medfound/h-264-video-encoder
