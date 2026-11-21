# PeerDesk 远程音频调研：NetEQ 抖动缓冲、Opus 配置、采集与播放、Rust 生态

> 调研日期：2026-11-21。标注说明：**[已核实]** 表示我读过原始源码、规范或官方文档原文；**[未核实]** 表示只基于摘要、二手资料或记忆，落地前要再确认。
> 本文只做调研，不改代码。仓库里目前没有音频相关调研（`grep -i audio docs/research/*.md` 无结果）。

## 0. 结论先行

1. **抖动缓冲值得学 NetEQ 的"思想"，不值得移植整个 NetEQ。** 核心思想有三条，全部可以用几百行 Rust 重写：
   - 目标延迟 = 相对到达延迟（relative arrival delay）直方图的高分位数（遗忘式），不是平均抖动；
   - 播放缓冲水位对比目标延迟，决定"正常 / 加速 / 减速（预扩展）/ 丢包补偿"；
   - 时钟漂移和延迟收敛都靠时间伸缩（WSOLA 类）吸收，而不是丢包或插静音。
2. **音频要比视频更"硬"地保证连续**：视频卡 100 ms 用户几乎无感，音频断 20 ms 就是爆音。所以音频的延迟预算应该比视频更保守（目标延迟 30–40 ms 起步），代价是 P2P 直连时多出的这几十毫秒，换来的是稳定。
3. **Opus 默认：48 kHz 立体声、10 ms 帧、约 96–128 kbps 受限 VBR、复杂度 10、关 DTX。** 立体声音乐在 CELT 模式下不会产生 LBRR（带内 FEC），所以丢包保护不靠 Opus 自带 FEC。
4. **丢包保护用"RED 式重复"（把前 1 帧、必要时前 2 帧放进下一个数据报）+ 抖动缓冲兜底**，按丢包率自适应开关；在 RTT 小于目标延迟时叠加 NACK。Moonlight 那种 4+2 Reed-Solomon 在 5 ms 帧下才合算，10 ms 帧下块等待延迟翻倍。
5. **音画同步：音频不等视频，也不被视频拖延。** 远程桌面是交互场景，延迟比唇音同步重要；只在音频超前视频超过约 40 ms 时，给音频加一点上限很小的延迟（见 §3）。
6. **采集：** Windows 用 WASAPI loopback（进程级 loopback 要 Win10 build 20348+/Win11 [见 §4 的版本分歧]）；macOS 用 ScreenCaptureKit（13+），14.2+ 可选 Core Audio process tap，更老的系统直接告诉用户需要 BlackHole 类驱动（不静默降级）；Linux 用 PipeWire（兼容 PulseAudio）sink monitor。
7. **许可证无障碍**：`opus`（MIT/Apache-2.0，自带 libopus BSD-3）、`cpal`（Apache-2.0）、`wasapi`（MIT）、`screencapturekit`（MIT/Apache-2.0）、`rubato`（MIT/Apache-2.0）、`pipewire`（MIT）、`libpulse-binding`（MIT/Apache-2.0）全部可用。**要注意的两个点**：`libpulse-binding` 底层链接 LGPL 的 libpulse（动态链接，需评估）；`opus-rs` 之类纯 Rust 实现作者自述是 LLM 生成，不能用于生产主路径。
8. **指标可以自己算，不需要 POLQA**：连续性指标（隐藏率、扩展率、欠载次数）直接从抖动缓冲里统计；端到端口到耳延迟用"同步啁啾 + 互相关"测；质量用 ViSQOL（Apache-2.0）做回归，不做 CI 硬门槛。

---

## 1. WebRTC NetEQ 深度剖析

**来源**：libwebrtc main 分支 [modules/audio_coding/neteq/](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/)，下文逐文件标注。

### 1.1 整体结构

NetEQ 把"网络侧"和"声卡侧"完全解耦：

- **网络线程**每收到一个包就 `InsertPacket`：放进按时间戳排序的 `PacketBuffer`，同时更新 `DelayManager`（估计目标延迟）。
- **声卡线程**每 10 ms 调一次 `GetAudio`：`DecisionLogic` 看缓冲水位和目标延迟，决定这 10 ms 输出怎么产生，然后 `Decode` / `Expand` / `Accelerate` / `PreemptiveExpand` / `Merge` / `ComfortNoise` 之一。
- 输出永远是恰好 10 ms 的 PCM，声卡不会饿。

### 1.2 目标延迟估计（DelayManager）

主要文件：[delay_manager.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/delay_manager.cc)、[underrun_optimizer.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/underrun_optimizer.cc)、[reorder_optimizer.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/reorder_optimizer.cc)、[packet_arrival_history.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/packet_arrival_history.cc)。 [已核实]

**相对到达延迟（relative arrival delay）**
- 不需要发送端和接收端时钟同步。对每个包算"到达时间 − RTP 时间戳"（统一换成 ms），在最近 2 秒窗口（`kPacketHistorySizeMs=2000`）里取这个偏移的**最小值**对应的包做参照，当前包相对它的延迟 = `(到达 − 参照到达) − (RTP − 参照RTP)`，下限 0。
- 直观理解：窗口内"最快的那个包"代表网络基础延迟，其他包比它晚多少就是排队抖动。
- 乱序包照常入历史并参与最大/最小值跟踪；`Insert` 返回"是否乱序"。

**欠载优化器（UnderrunOptimizer）**
- 100 个桶，每桶 20 ms，覆盖 0–2000 ms（`kDelayBuckets=100`，`kBucketSizeMs=20`），相对延迟超出范围的样本不记录。
- 直方图带遗忘因子（`forget_factor`、`start_forget_weight`，启动时更新权重大，让估计快速收敛）。
- 可选 `resample_interval_ms`：每个间隔只把窗口内**最大**相对延迟记一次，抑制突发期间单个间隔里大量样本把分布拉偏。
- 目标延迟 = 分位桶上沿：`optimal_delay_ms = (1 + bucket_index) * 20`，`bucket_index` 是 `histogram.Quantile(q)`。
- 分位数 q、遗忘因子等参数来自 field trial `WebRTC-Audio-NetEqDelayManagerConfig`，默认值我没在源码里直接读到：记忆中 q≈0.95 **[未核实]**，落地时读 `delay_manager.h` / field trial 默认值确认。

**乱序优化器（ReorderOptimizer，可选）**
- 另一个 100×20 ms 直方图，专门记录"乱序包"的延迟；对每个候选桶 i 算代价
  `延迟代价 = max(0, i×20 − 基础延迟)`，`丢失代价 = 100 × ms_per_loss_percent × 丢失概率`，取总代价最小的桶，直到丢失概率归零。
- 含义：把"多等 1% 丢包要多少毫秒"做成可调参数，是对"延迟 vs 欠载"最直接的显式折中。

**DelayManager 合成**（`Update`）：
1. 非乱序包的到达延迟喂给欠载优化器，目标 = 它的输出，没有输出时用 `kStartDelayMs = 80`（启动默认 80 ms）；
2. 若启用乱序优化器，目标取两者最大值。

**历史演变** [部分核实，见 commit 记录]：
- 早期（2020 前）：单位是"包数"，用包间到达间隔（IAT）直方图加峰值检测器；
- 2020–2021：Jakob Ivarsson 的 [Refactor NetEq delay manager logic](https://webrtc.googlesource.com/src/+/2a7c57c34f323ee1977f6e7809ee23bfcf9a7459)：去掉对序号的依赖，目标延迟单位改成 ms，加速/预扩展阈值移进 DecisionLogic（bug 10333）；
- 2021-09：拆出 `RelativeArrivalDelayTracker` 与 `DelayOptimizer`，为加乱序优化器做准备（bug 10178）；
- 之后：改用 `PacketArrivalHistory` 取代 `RelativeArrivalDelayTracker`（等价，位精确性略变，bug 13322）；
- [Make relative arrival delay mode default](https://webrtc.googlesource.com/src/+/507f43465b3306f3ba1c3ff639e7a46ab33a8904)。
- 精确日期和每次改动的数据，commit 页面只看到片段，**[未核实]**。

### 1.3 缓冲水位与决策逻辑（DecisionLogic）

来源 [decision_logic.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/decision_logic.cc) [已核实]。

**关键常量**：`kPostponeDecodingLevel=50`（缓冲不足目标的 50% 时推迟解码）、`kTargetLevelWindowMs=100`、`kDelayAdjustmentGranularityMs=20`、`kPacketHistorySizeMs=2000`、`kCngTimeoutMs=1000`。

**"期望的下一个包就在缓冲里"时**（`ExpectedPacketAvailable`）：
- 允许时间伸缩且上一次不是 Expand 时：低限 = 目标延迟；高限 = 目标 + 窗口内最大延迟 + 20 ms；
- 当前播放延迟 ≥ 4×高限 → **FastAccelerate**；≥ 高限 → **Accelerate**；< 低限 → **PreemptiveExpand**；否则 **Normal**。

**"下一个包还没来、后面的包到了"时**（`FuturePacketAvailable`）：
- 上一次是 Expand → **Merge**（把新解码的数据和扩展出来的尾巴平滑拼接）；
- 如果上次是编解码器自带 PLC / CNG → **Normal**；
- 否则 → **Expand**（等包，同时生成补偿音）。

**没有包**：Expand（RFC 3389 舒适噪声继续则 CNG；内部 CNG 超过 1 s 切回 Expand）。

**推迟解码**：缓冲 < 目标的 50%，且不在 DTX 期间等，就先不解码、继续补偿，攒到够水位，避免刚解码一个包马上又欠载。

**缓冲水位滤波**：水位 = 包缓冲 + 同步缓冲里的样本，对其做平滑，时间伸缩会把增减的样本数记入滤波，避免"刚加速完，水位下降又触发减速"的振荡。

### 1.4 时间伸缩（WSOLA 类）

来源 [time_stretch.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/time_stretch.cc)、[accelerate.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/accelerate.cc) [已核实]。

1. 取最近一段信号（多声道只取一个参考声道），降采样到 4 kHz，算自相关，找基音周期 `peak_index`（映射回原采样率）。
2. 取两段 15 ms：`vec1` 比 `vec2` 早一个基音周期，计算两段能量与互相关，归一化相关系数（Q14）。
3. "是否有语音活动"的简单判断：能量对比背景噪声（没估计时用固定阈值 75000）；无活动语音时随意伸缩即可（`kSuccessLowEnergy`）。
4. **Accelerate**：前 15 ms 原样复制，把 `peak_index` 长度的一段与输出末尾**交叉淡化**后继续拷贝，即**删掉一个基音周期**；快速模式相关阈值降到 0.5（8192/16384），并可以删掉多个周期（凑进 15 ms 内）。输入不足约 30 ms 时不伸缩。
5. **PreemptiveExpand**：对称操作，**重复**一个基音周期。
6. 相关阈值不够（信号不周期，例如噪声/打击乐）则 `kNoStretch`，等待下一轮机会，实在没有再靠 Expand/丢弃。

**与我们的相关性**：NetEQ 为语音设计，基音周期是 4 kHz 域下的近似。桌面音频是音乐/游戏/视频声音，相关性弱、周期不明显。WSOLA 本身（任意信号，找最相似的重叠位置做交叉淡化）依然适用，但"一次改变量"要保守，音乐的质量更依赖用小幅度、长时间的连续伸缩（见 §4.5 漂移）。

### 1.5 丢包补偿（Expand）

来源 [expand.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/audio_coding/neteq/expand.cc) [已核实]。

- 对最近 256 ms 历史做 LPC 分析和基音估计（4 kHz 域，滞后 10–63）；
- 有声段重复最近的基音周期（三个滞后之间交叉混合），无声段用 AR 滤波的随机噪声，背景噪声另外叠加；
- **静音因子**从 1.0 起逐样本衰减，连续扩展次数越多衰减越快；扩展持续时间上限 `fs*2` 即 2 秒；
- 有声/无声混合系数由相关系数的三次拟合给出，过渡用 8/16/32 ms 交叉淡化。
- **注意**：对 Opus，NetEQ 不一定用 Expand，决策里有 `CodecPlc` 模式，即用 Opus 自带 PLC；Expand 主要给 G.711 等没有 PLC 的编解码器 **[已核实：决策逻辑里出现 CodecPlc 分支；"Opus 走 CodecPlc"这一条是我从模式名推断，未逐行核对]**。

### 1.6 延迟与欠载的折中，典型数值

- 折中被分成两层：①分位数（q 越高越保守、延迟越大）和遗忘因子决定"长期"目标；②Accelerate/PreemptiveExpand 在水位偏离目标时"短期"拉回；Expand/Merge 是欠载后的善后。
- 默认启动目标 80 ms [已核实]；稳定网络上目标收敛到 20–60 ms 量级，抖动大的无线网络上 100–200 ms，具体数字我没有公开的统计出处 **[未核实]**，可以用 `chrome://webrtc-internals` 的 `jitterBufferTargetDelay / jitterBufferEmittedCount` 自己测。
- 度量定义见 [W3C webrtc-stats](https://www.w3.org/TR/webrtc-stats/)：`concealedSamples`、`concealmentEvents`、`insertedSamplesForDeceleration`、`removedSamplesForAcceleration`、`jitterBufferDelay`、`jitterBufferTargetDelay` [已核实]，我们的指标直接沿用这些名字（§6）。

### 1.6.1 对比：Moonlight 没有自适应抖动缓冲

[AudioStream.c](https://raw.githubusercontent.com/moonlight-stream/moonlight-common-c/master/src/AudioStream.c) [已核实]：队列上限 30 个包，溢出整体清空；启动时丢弃约 500 ms 的音频（`packetsToDrop = 500 / AudioPacketDuration`）；丢包靠 `decodeAndPlaySample(NULL, 0)` 触发 libopus PLC；**没有自适应抖动缓冲**。这是它在差网络下音频会"断一下再追上"的原因 **[推断，未实测]**。

---

## 2. Opus 低延迟桌面音频配置

### 2.1 帧长与算法延迟

- Opus 默认 20 ms 帧，"对大多数应用是好选择"；更小帧延迟低但包头开销大 [[Xiph 推荐设置](https://wiki.xiph.org/Opus_Recommended_Settings)，本次抓取 503，内容来自搜索摘要，**[未核实]**]。
- libopus 支持 2.5/5/10/20/40/60 ms。**10 ms** 是桌面音频的折中：端到端编码延迟 ≈ 帧长 + 前瞻（CELT 约 2.5 ms 重叠，AUDIO 应用模式约 6.5 ms 总前瞻 **[未核实]**）。
- `OPUS_APPLICATION_RESTRICTED_LOWDELAY`（Sunshine 的选择，[audio.cpp](https://github.com/LizardByte/Sunshine/blob/master/src/audio.cpp) [已核实]）去掉 SILK 并取最小前瞻，但**音乐质量/低码率表现略差，也不会有 LBRR**。
- 开销算账：96 kbps、10 ms 帧 ≈ 120 B 负载；QUIC 短包头 + 数据报帧 + 我们的音频头约 40 B，开销约 33%（约 +32 kbps）。可以接受，并且码率控制要把头开销算进去。

### 2.2 码率

- RFC 7587 给的 20 ms 帧"甜点"：窄带语音 8–12 kbps，宽带语音 16–20，全带语音 28–40，**全带单声道音乐 48–64 kbps** [[RFC 7587](https://www.rfc-editor.org/rfc/rfc7587)，来自搜索摘要 **[未核实]**]。立体声音乐按 2× 估计：96–128 kbps，我们的建议默认 **96 kbps（受限 VBR），"高质量"档 160 kbps**。
- Sunshine：立体声 96 kbps 标准 / 512 kbps 高质量；5.1 声道 256 / 1536 kbps；7.1 声道 450 / 2048 kbps；固定码率（`OPUS_SET_VBR(0)`）；包缓冲 1400 B [已核实]。
- 桌面音频多为"系统混音"，静音时段很多（视频会议、文档编辑）：DTX 在这类场景下能省流量。但 RFC 7587 建议"除非网络容量严重受限，使用连续传输"，DTX 质量略低 [摘要，**[未核实]**]。**建议默认关 DTX**，因为：①音频码率相对视频本来就小；②关 DTX 时 NetEQ/我们的抖动缓冲不用处理 CNG 与静默段的时间戳跳变。静音时用 Opus 编码出的静音帧即可（几字节）。

### 2.3 带内 FEC（LBRR）：什么时候有用

- 需要同时设置 `OPUS_SET_INBAND_FEC(1)` 和 `OPUS_SET_PACKET_LOSS_PERC(x)` 编码器才会生成 LBRR [[Opus 邮件列表](https://lists.xiph.org/pipermail/opus/2019-July/004377.html)，摘要]。
- LBRR 只在 **SILK 层**存在，一次只覆盖**前一帧**，约占常规包码率的 2/3；解码端要等下一个包才能用它恢复缺失的包（多一帧延迟）。CELT-only 模式的包没有 FEC [摘要，**[未核实]**，以 libopus 源码 `opus_encoder.c` 为准]。
- [Mozilla 的 FEC 实验](https://blog.mozilla.org/webrtc/audio-fec-experiments/) [已核实]：默认 40 kbps 立体声下**不产生 LBRR**，立体声要提高码率才能启动；丢包率约 1% 才触发；19% 丢包下差异显著，1.8% 丢包差异"微妙"。
- **结论**：我们是高码率立体声音乐（CELT 为主）。LBRR 基本无用；可以在"语音模式"（会议类场景、单声道 32 kbps）下启用。**不依赖 LBRR**。

### 2.4 复杂度

- 默认 10（最高）。桌面编码用 10 没问题（现代 CPU 上单线程占用 < 1%）。主机 CPU 吃紧（游戏占满）时可降到 5–8 **[CPU 数据未核实]**。

### 2.5 RED（RFC 2198）

- [RED 的 WebRTC Hacks 文章](https://webrtchacks.com/red-improving-audio-quality-with-redundancy/) [已核实]：
  - 丢包 20% 时，隐藏率：无 RED 18%，RED 距离 1 为 4%，距离 2 为 0.7%；
  - 丢包 60% 时：无 RED 60%，距离 1 为 32%，距离 2 为 18%（距离 2 "几乎完美"）；
  - 带宽约 ×2–3（30 kbps → 60/90 kbps，另加约 10 kbps 头）；Opus FEC 最多约 10 kbps，上限 25%；
  - Chrome 实现为每包带最多前 2 帧的冗余副本。
- Chrome 默认状态：我在搜索结果里看到"M96 默认启用 opus+red"的讨论串 [[discuss-webrtc](https://groups.google.com/g/discuss-webrtc/c/5761etCrSuA)]，但没读到正文，来源之间对默认值说法不一：**[未核实]**。Google Meet 的具体理由没找到。
- 关键点：RED 对 CELT 同样有效（只是重复编码后的字节），不依赖 SILK。代价是码率成倍增加，所以必须**按丢包自适应**。

### 2.6 libopus 1.5：DRED 与 Deep PLC

- libopus 1.5（2024-03）"通过 Deep Redundancy (DRED) 大幅提高丢包鲁棒性，通过 Deep PLC 改善丢包补偿" [[发布说明](https://opus-codec.org/release/stable/2024/03/04/libopus-1_5.html)，已核实]。
- DRED [[draft-valin-opus-dred-05](https://www.ietf.org/archive/id/draft-valin-opus-dred-05.html)，已核实]：只对**语音**，冗余信息是声学特征 + 声码器重建，约占常规码率的 1/50，可覆盖"1 秒或更多"；波形不一致。
- 对桌面音乐无用，对"语音通话式场景"有用。构建开关、模型大小、CPU 开销本次没查到 **[未核实]**，**v1 不做**，留作语音模式的后续选项。

### 2.7 Parsec / Moonlight / Sunshine 怎么做

| 方案 | 编码 | 帧/包 | 丢包保护 | 抖动缓冲 |
|---|---|---|---|---|
| Sunshine（主机）| Opus，`RESTRICTED_LOWDELAY`，固定码率，立体声 96 kbps | 包时长由客户端配置（`packetDuration`），常见 5 ms **[未核实]**，包上限 1400 B | RS FEC，见下 | 无（客户端实现） |
| Moonlight（客户端）| libopus 解码 | `samplesPerFrame = 48 × packetDuration` | 每 4 个数据包配 2 个 Reed-Solomon 校验包（`RTPA_DATA_SHARDS=4`，`RTPA_FEC_SHARDS=2`）；乱序等待 10 ms（`RTPQ_OOS_WAIT_TIME_MS`）；包缺失用 libopus PLC | 无自适应，队列上限 30，启动丢 500 ms |
| Parsec | 自研 UDP 协议，音频编解码器：没找到官方说明 **[未核实]** | — | — | 官方博客只讲视频管线 [已核实，博客不涉及音频] |

- 4+2 Reed-Solomon 的代价是固定 50% 额外码率，块等待 ≈ 4 帧；5 ms 帧时 20 ms，10 ms 帧时 40 ms，不合算。
- Wolf 项目的协议文档确认：音频走 RTP + AES-CBC 加密，FEC 包在 RTP 头后多 12 字节 FEC 头 [[Wolf rtp-opus](https://games-on-whales.github.io/wolf/stable/protocols/rtp-opus.html)，已核实]。

---

## 3. 音画同步（唇音同步）

### 3.1 原则

- 视频会议里 WebRTC 用 RTCP Sender Report 把音视频映射到同一个发送端时钟，接收端让快的那一路等慢的一路。代价是：**视频的缓冲会把音频一起拖慢**，在 [discuss-webrtc 讨论](https://groups.google.com/g/discuss-webrtc/c/ZvAHvkHsb0E) 里就有人报告 Chromium 的 A/V 同步导致意外延迟 [标题已核实，正文未读]。
- 远程桌面里：**输入响应延迟比唇音同步重要**（用户盯着自己的鼠标，不是别人的嘴）。"硬同步"的代价是让两路都更慢，不值得。
- ITU-R BT.1359 的可察觉阈值：音频超前约 45 ms、音频滞后约 125 ms 才被察觉 **[未核实，记忆值，落地前读原文]**。**音频超前比滞后更容易被察觉**。

### 3.2 其他产品

- Moonlight：音频独立队列，到了就播，没有与视频的时间戳对齐 [AudioStream.c，已核实只有队列和 PLC，无同步逻辑]。
- Parsec、GeForce NOW：没找到公开资料 **[未核实]**，不要在对外材料里写它们怎么做。

### 3.3 PeerDesk 建议："音频主、视频不等、只对音频做小幅补偿"

1. 采集端给每个音频包和每个视频帧打同一个主机单调时钟的采集时间（微秒），控制端用已有的会话时钟对齐（传输层已有 RTT/时钟偏移估计则复用）。
2. 控制端持续统计 `av_skew = 视频显示时刻对应的采集时间 − 音频此刻播放的采集时间`（EWMA 平滑）。
3. 默认不处理。只有当**音频比视频早 > 40 ms**（音频超前）时，抖动缓冲的目标延迟额外加 `min(skew−20 ms, 40 ms)`，由 NetEQ 式的 PreemptiveExpand 慢慢吸收；视频不动。音频滞后视频时**什么也不做**（音频已经是最快路径，再加速只会增加欠载）。
4. 这个补偿开关做成配置，e2e 里测它对口到耳延迟的代价（§6）。
5. 对比说明：Parsec/ToDesk 一档的产品在此无公开资料，我们不对标这一项，只对标自己的数据。

---

## 4. 采集与播放

### 4.1 Windows

- **WASAPI loopback**：对渲染端点用 `AUDCLNT_STREAMFLAGS_LOOPBACK` 初始化，只支持共享模式；Win10 1703 之前事件驱动 loopback 收不到事件，要用渲染流事件触发（现在已无需）；采集到的是系统混音，格式是设备的混音格式 [[Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording)，已核实]。
- 已知行为：**没有任何程序在播放时 loopback 不产生包**（时间线出现空洞），需要自己补静音或依据时间戳处理 **[未核实，来自经验；Learn 页面没写，要实测]**。这对采集时间戳和 NetEQ 式的时间戳连续性有影响：采集线程应按墙钟节拍补零。
- **进程 loopback**（只采某个进程树或排除某个进程树）：用 `ActivateAudioInterfaceAsync` 加 `AUDIOCLIENT_ACTIVATION_PARAMS`；Microsoft 的 [ApplicationLoopback 示例](https://github.com/microsoft/windows-classic-samples/tree/main/Samples/ApplicationLoopback) 写明"需要 Windows 10 build 20348 或更高"，提供 `includetree` 和 `excludetree` [已核实]。产品需求里常说"Win10 2004+"，与示例页冲突：**[未核实]**，以实测为准。
- 产品价值：`excludetree` 可以**排除我们自己的播放进程**（本机同时是被控端和控制端时防回声），也能做"只共享某个应用的声音"。
- 设备变化：默认渲染设备切换、拔耳机时，loopback 流会失效（`AUDCLNT_E_DEVICE_INVALIDATED`），需要监听 `IMMNotificationClient` 重建流 **[未核实]**。

### 4.2 macOS

- **ScreenCaptureKit（macOS 13+）**：`SCStreamConfiguration.capturesAudio`、`sampleRate`、`channelCount`、`excludesCurrentProcessAudio` [[Apple 文档](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration.md)，摘要]；Rust 绑定 README 标注"系统音频 + 麦克风采集（macOS 13.0+ / 15.0+）" [已核实]。
- 好处：音视频来自同一个 `SCStream`，**时间戳天然同源**，对音画同步有利；并且需要的权限就是屏幕录制权限，我们本来就要。
- **Core Audio process tap（macOS 14.2+）**：`CATapDescription` + `AudioHardwareCreateProcessTap`，要包进一个私有 aggregate device 才能读；可以选择"采集同时静音原输出"；权限是系统音频录制权限（`NSAudioCaptureUsageDescription`），没有单独 entitlement [[Apple](https://developer.apple.com/documentation/coreaudio/audiohardwaretap.md)、[Recall.ai 博客](https://www.recall.ai/blog/core-audio-taps)，摘要，**[未核实]**]。开发者论坛有"tap 返回全零缓冲"和多立体声对时电平衰减的报告 [标题已核实]。
- 老系统（< 13）：只能靠虚拟驱动（如 BlackHole，需要用户安装，且是 GPL-3.0 **[未核实]**，不能随我们分发）。**按"诚实"原则**：不提供静默降级，界面里写明"此系统不支持采集声音，需要 macOS 13 及以上；或自行安装虚拟声卡"。
- 建议路线：13+ 默认 ScreenCaptureKit；14.2+ 且需要"排除/只含某个进程"时再用 process tap。

### 4.3 Linux

- **PipeWire**：创建 `Stream/Input/Audio` 流并设置 `stream.capture.sink=true`，即采集默认 sink 的 monitor；`target.object` 可指定 sink [[pipewire-props(7)](https://man.archlinux.org/man/pipewire-props.7.en)、官方 [audio-capture 示例](https://docs.pipewire.org/audio-capture_8c-example.html)，已核实示例里有这两个属性的注释；第三方 crate 使用同样做法]。有 issue 报告 `pw_stream_new` 忽略 `PW_KEY_TARGET_OBJECT` 取了麦克风 [[pipewire#2787](https://gitlab.freedesktop.org/pipewire/pipewire/-/issues/2787)，摘要]，必须实测。
- **PulseAudio**（含 PipeWire 的 pulse 兼容层）：监视源 `<sink名>.monitor`；`@DEFAULT_MONITOR@`；用 `libpulse-binding` 或 cpal 的 PulseAudio/ALSA 后端 **[cpal 是否支持采集监视源未核实]**。
- **CI 的实测路径**：`pactl load-module module-null-sink sink_name=pd_test`，播放真实样本到这个 sink，再采它的 monitor：整条真实采集路径都跑到，没有造假（符合 CLAUDE.md）。

### 4.4 控制端播放（cpal）

- cpal 0.18.2，Apache-2.0；CoreAudio、WASAPI、ALSA、JACK、PipeWire 等后端；最近的更新日志有"CoreAudio 设备采样率变化时等待、设备断开修复"、"PipeWire 设备被拔时报 `DeviceNotAvailable`"这类设备变化处理 [[CHANGELOG](https://raw.githubusercontent.com/RustAudio/cpal/master/CHANGELOG.md)，已核实]。
- 设备变化处理：监听流错误回调，`DeviceNotAvailable`/`StreamInvalidated` 后在后台线程重建输出流，**抖动缓冲和解码器不重置**，只换声卡侧；换设备后采样率可能变化，重采样器重建，缓冲目标延迟保持不变。
- 声卡回调缓冲：用最小的稳定缓冲（Windows 共享模式通常 10 ms 周期、macOS 可 5 ms 级 **[未核实]**），这是口到耳延迟里不可压缩的一块，需要在 e2e/实机里量。
- cpal 回调是**实时线程**：回调里只做"从无锁环形缓冲读 PCM"，解码、抖动缓冲、时间伸缩跑在单独线程，提前产出 20–40 ms 的 PCM 放入环缓冲（这是额外延迟，要计入）。更好的方式：回调里直接调抖动缓冲的 `get_audio(10 ms)`，抖动缓冲内部无锁、无分配。**推荐后者**（零额外缓冲）。

### 4.5 采样率转换与时钟漂移

- 采集 48 kHz（Opus 的原生率）是首选；设备混音格式不是 48 kHz 时，采集端先重采样到 48 kHz（`rubato`），因为 Opus 只支持 8/12/16/24/48 kHz。
- 控制端声卡可能不是 48 kHz（例如蓝牙耳机 44.1 kHz 或 16 kHz）：播放端再用 `rubato` 转到设备采样率。
- **时钟漂移**：主机采集声卡时钟和控制端播放声卡时钟的差典型是几十 ppm 量级（50 ppm ≈ 每 20 秒差 1 ms）**[量级来自常识，未核实]**。后果：长时间播放缓冲缓慢增长（延迟变大）或缓慢减少（最终欠载）。
- **NetEQ 怎么吸收**：不单独估计漂移，漂移表现为"相对到达延迟缓慢单向变化"，水位偏离目标后 DecisionLogic 触发 Accelerate / PreemptiveExpand，一次删/加一个基音周期，长期平均速率就被拉到和声卡一致。精度粗但不需要先验知识。
- **更好的做法（我们）**：两级。
  1. 持续的微调重采样：`rubato` 的异步重采样器支持运行中调整比率，用水位误差做一个很慢的 PI 控制（比率偏离 ≤ ±200 ppm），连续、无可闻伪影；
  2. 水位偏差超出范围（突发抖动之后）才触发 WSOLA 的 Accelerate/PreemptiveExpand。
  `rubato` 5.x 的具体 API（`set_resample_ratio_relative`）我没读 **[未核实，实现前查文档]**。

---

## 5. Rust 生态与许可证

通过 crates.io API 查到的版本/许可证/更新日期（2026-11-21）[已核实]：

| crate | 版本 | 许可证 | 备注 |
|---|---|---|---|
| [`opus`](https://github.com/SpaceManiac/opus-rs) | 0.4.0（2026-08） | MIT/Apache-2.0 | 安全封装，**依赖 `opusic-sys`**（不再是 `audiopus_sys`）；下载量 430 万 |
| `opusic-sys` | 0.7.7（2026-11） | BSD-3-Clause | 目标 libopus 1.6.1；默认 `bundled`，用 cmake 编译；也可 `OPUS_LIB_DIR` 链系统库；"许可证要求同 C 源码"，改动在 `opus.patch`；需要 `cmake` |
| `opusic-c` | 1.6.1 | BSD-3-Clause | 同作者的高层绑定 |
| `audiopus` / `audiopus_sys` | 0.2.0 / 0.2.2（2021-04） | ISC | **2021 年后无更新**，不建议新用 |
| `magnum` | 1.0.1（2024-06） | MIT | 下载量很小（8 千），**[维护情况未核实]** |
| `libopus_sys` | 0.4.0 | MIT | 另一个 libopus 绑定，**[未核实内容]** |
| `opus-rs`（restsend） | 0.1.42（2026-11） | BSD-3-Clause | **纯 Rust**，`no_std`，`forbid(unsafe_code)`，对照 libopus 位精确测试；README 顶部明确标注 **"LLM GENERATED"** |
| `ropus` | 0.12.18 | BSD-3-Clause | Rust 移植，需要 C 编译器与外部资源下载（含约 135 MB DNN 权重），用于对照测试；不适合直接用 |
| `opus-decoder` | 0.1.1 | MIT/Apache-2.0 | 仅解码；**[内容未核实]**，仓库 README 404 |
| [`cpal`](https://github.com/RustAudio/cpal) | 0.18.2 | Apache-2.0 | 播放（和采集）跨平台后端 |
| [`wasapi`](https://github.com/HEnquist/wasapi-rs) | 0.27.0（2026-11） | MIT | 共享/独占、事件/轮询、**含 loopback 示例**；README 没提进程 loopback，**[未核实是否封装 `ActivateAudioInterfaceAsync`]**，不行就用 `windows` crate 自写（平台层允许 unsafe） |
| [`screencapturekit`](https://github.com/doom-fish/screencapturekit-rs) | 11.1.2（2026-11） | MIT OR Apache-2.0 | 音视频同一 `SCStream`；系统音频 13.0+，麦克风 15.0+ |
| `libpulse-binding` | 2.30.1（2025-04） | MIT OR Apache-2.0 | 绑定本身宽松，但**链接的是 PulseAudio 客户端库 libpulse（LGPL-2.1+）**，动态链接下分发一般可行，要在 `THIRD_PARTY_NOTICES.md` 写明 **[LGPL 合规细节未核实]** |
| [`pipewire`](https://gitlab.freedesktop.org/pipewire/pipewire-rs) | 0.10.1（2026-08） | MIT | 链 libpipewire（MIT），需要开发头文件 |
| [`rubato`](https://github.com/HEnquist/rubato) | 5.1.0（2026-11） | MIT OR Apache-2.0 | 同步/异步重采样，纯 Rust；配套 `audioadapter` 5.0.0 |
| `coreaudio-rs` | 0.14.2 | MIT/Apache-2.0 | 需要直接调 Core Audio process tap 时用，但 `AudioHardwareCreateProcessTap` 封装是否有 **[未核实]** |

**构建**：

- Linux：`opus` 通过 `opusic-sys` 自带 libopus（cmake + C 编译器）；`pipewire`/`libpulse` 需要系统开发包；
- Windows：MSVC + cmake（GitHub Actions 的 Windows runner 都有）；
- macOS：Xcode 命令行工具 + cmake。
- **全部 libopus 是 BSD-3-Clause**，Apache-2.0 兼容（保留版权和声明，写进 `THIRD_PARTY_NOTICES.md`）。
- 不要选 GPL/AGPL：BlackHole（见上）和 Linux 上一些 JACK 工具链要注意；本文推荐的 crate 都不在其中。
- 纯 Rust Opus 解码器：目前找到的都是新项目（`opus-rs` 自述 LLM 生成，`opus-decoder` 0.1.x），**不建议当主路径**；可以作为"没有 C 编译器的构建"或 WASM 控制端（浏览器端控制端）的备选，另外测试里用它与 libopus 位精确对照（那条链路还是要用真 libopus）。

**对 CLAUDE.md 的"unsafe 只允许在平台层和媒体层"的影响**：libopus 绑定 `opus` crate 是安全封装，不需要我们写 unsafe；WASAPI/ScreenCaptureKit/PipeWire 采集放在 `crates/platform`，`opus` 编解码放 `crates/media`。

---

## 6. 指标与 e2e 测试

### 6.1 连续性指标（直接从抖动缓冲统计，沿用 W3C 名字）

| 指标 | 定义 | 来源 |
|---|---|---|
| 隐藏率 `concealment_ratio` | `concealedSamples / totalSamplesDuration`（丢包或迟到被补偿的样本占比） | [webrtc-stats](https://www.w3.org/TR/webrtc-stats/) [已核实] |
| 隐藏事件数 `concealment_events` | 每次"补偿样本紧跟非补偿样本"计一次 | 同上 |
| 加速/减速量 | `removedSamplesForAcceleration`、`insertedSamplesForDeceleration`，比例 = 样本数 / 总样本数 | 同上 |
| 平均缓冲延迟 / 目标延迟 | `jitterBufferDelay/EmittedCount`，`jitterBufferTargetDelay/EmittedCount` | 同上 |
| 欠载次数 | 声卡回调里抖动缓冲无法提供足够样本、被迫补零的次数（**我们独有，硬门槛 0**） | — |
| 扩展率 Expand rate | Expand + 编解码 PLC 输出样本占比 | NetEQ 统计里有类似量 **[未逐项核实]** |

### 6.2 口到耳延迟（mouth-to-ear）

- 在被控端采集到的音频里**注入同步啁啾（chirp）/导频**（测试构建里往 null sink 播的内容中带，不是造假：真实音乐里嵌入标记），在控制端的播放输出（同样用 null sink 的 monitor 再采）里用互相关找到它，两者时间差 = 口到耳延迟，包含采集缓冲、编码、网络、抖动缓冲、播放缓冲。
- 在确定性仿真里（E-e2e 文档里的仿真时钟）可以直接用虚拟时钟记录"采集时刻"和"送入声卡回调时刻"，不需要真的发声；实机/集成测试再用 null sink 做真实验证。

### 6.3 质量

- **ViSQOL**：[google/visqol](https://github.com/google/visqol)，**Apache-2.0**，Bazel 构建（Windows 实验性），音频模式要求 48 kHz，输入多声道会混成单声道；语音模式 16 kHz；有 Python 绑定，没有 Rust 绑定 [已核实]。适合做**回归趋势**（同一段素材，不同网络条件下 MOS-LQO 的变化），不适合做硬门槛（模型噪声）。
- **POLQA**（ITU-T P.863）：商用授权（OPTICOM/专利池）**[未核实授权细节]**，不进开源仓库，也不进 CI。
- **PESQ**（P.862）：ITU 提供代码但有专利和使用限制 **[未核实]**，且是窄带/宽带语音模型，不适合音乐。不使用。
- **便宜的替代**：对齐后算 SNR/频谱失真（只用于"无网络损伤时编码链路本身"的回归，如码率、前瞻变更）。

### 6.4 建议的 CI 门槛（proposed，首版用实测值定死，只升不降）

| 场景（仿真） | 门槛 |
|---|---|
| 理想链路（RTT 20 ms，无丢包无抖动）| 口到耳延迟 p95 ≤ 90 ms；欠载 0；隐藏率 = 0；ViSQOL ≥ 4.0（趋势，不硬卡）|
| 抖动（均匀 ±15 ms） | 欠载 0；隐藏率 ≤ 0.5%；目标延迟 ≤ 抖动 p95 + 20 ms |
| 丢包 1% 随机 | 隐藏率 ≤ 1%（RED 开启后 ≤ 0.2%）；欠载 0 |
| 丢包 5% 突发 | 隐藏率 ≤ 5%；最长连续隐藏 ≤ 40 ms |
| 时钟漂移 ±200 ppm，10 分钟 | 缓冲水位偏离目标 ≤ ±20 ms；无欠载；加速/减速量之和 ≤ 0.5% |
| 视频爆发（4K 关键帧挤占链路）| 音频口到耳延迟 p99 不超过理想链路 + 30 ms（验证"音频优先于视频"）|
| 输入优先 | 加入音频后输入往返延迟 p99 回归 ≤ 1 ms（音频不能拖慢输入）|

---

## 7. PeerDesk 推荐设计

### 7.1 管线

```
主机：采集(WASAPI/SCK/PipeWire, 设备格式) → 重采样 48k(rubato) → 10 ms 分帧
      → Opus 编码(48k 立体声, 10 ms, 96 kbps 受限VBR, 复杂度10, DTX 关)
      → 音频包(序号 + 采集时间 + 冗余) → QUIC 数据报(音频优先级)
控制端：QUIC 数据报 → 去冗余/NACK → 抖动缓冲(NetEQ 式) → Opus 解码/PLC
      → 慢速重采样(漂移) → cpal 回调(10 ms) → 声卡
```

### 7.2 数据报封装（接我们已有的 QUIC 数据报实时层）

- **音频头**（约 12–16 B）：`seq:u16`（每 10 ms 帧加 1，按帧计数）、`capture_ts:u32`（48 kHz 采样计数，同 RTP 时间戳语义，用于相对到达延迟）、`host_time:u32`（微秒低位，用于音画同步）、`flags`（有无冗余、冗余距离、静音帧）、`redundancy_len`。
- **负载**：当前帧 Opus 字节，后跟 0–2 个前帧的 Opus 字节副本（RED 式，距离 1/2，各带长度）。
- 一个数据报 ≤ 约 200 B，远小于 MTU，不分片。
- **优先级**：输入 > 音频 > 视频。音频数据报**不进视频每帧预算的发送队列**，直接走高优先级通道、绕过视频整形；音频码率（含冗余）事先从视频预算里扣除（视频预算 = 估计带宽 × 利用率 − 音频码率），避免音频被视频突发的队列延迟拖住。输入与音频谁先：10 ms 的音频数据报对输入影响可以忽略（同在一个 pacer tick 内，几百字节）。
- **加密**：沿用 QUIC 的 TLS，不再单独加密（Moonlight 要单独做 AES 是因为它走 RTP/UDP）。
- **统计回传**：接收端每 100–250 ms 把丢包率、迟到率、当前目标延迟、水位回传，用于冗余自适应和（可选）码率调整。

### 7.3 FEC/RED 选择

- **默认**：开 RED 距离 1，仅当近 2 秒丢包率 > 0.5% 或出现突发（连续丢 ≥ 2）；丢包率 > 5% 时升到距离 2；丢包回到 < 0.2% 并持续 10 秒后关闭（滞回，防止来回切换）。
- 额外码率：距离 1 约 +96 kbps、距离 2 约 +192 kbps（再加包头），**仍远小于视频**；中继流量按成本计，所以只在需要时开，P2P 直连时可以更激进地保持。
- **NACK**：当 RTT < 目标延迟 − 10 ms 时，对未被 RED 覆盖的缺失帧发一次 NACK，主机只重发一次（超过目标延迟就没用了）。远程到海外的 100+ ms RTT 下 NACK 无意义，直接靠 RED。
- 不用 Reed-Solomon（延迟和固定开销，见 §2.7），不用 LBRR（立体声音乐 CELT 不产生，见 §2.3）。
- 语音模式（后续）：单声道 32 kbps + SILK/LBRR；libopus 1.5 DRED 可选。

### 7.4 抖动缓冲：在 Rust 里移植 NetEQ 的核心思想

放在 `crates/media`（纯逻辑、无 unsafe、无系统调用）。**不移植**：舒适噪声（CNG/DTX，我们关了 DTX）、DTMF、多编解码器管理、位精确定点实现。

**模块**：

1. `ArrivalHistory`：2 秒窗口，存（RTP 时间戳展开、到达时间），维护单调队列求最小偏移（参照包）和最大偏移；输出相对到达延迟；乱序标记。（对应 `packet_arrival_history.cc`）
2. `DelayEstimator`：
   - 直方图：10 ms 桶 × 100（覆盖 1 秒；NetEQ 是 20 ms 桶，我们帧是 10 ms，桶更细，才能把默认目标降到 30 ms 量级）；
   - 带遗忘因子的更新；每 500 ms 间隔取最大相对延迟再入直方图（NetEQ 的 `resample_interval` 思路）；
   - 目标 = q 分位桶上沿 + 安全余量；q 默认 0.95（参考 NetEQ，**[其默认值未核实]**，我们用 e2e 的抖动场景自己标定，不盲信）；
   - **乱序/丢包代价项**（`ms_per_loss_percent`）：把"多等 1% 丢包愿意花多少毫秒"做成参数，默认值用 e2e 标定；
   - 夹紧：最小 20 ms，最大 200 ms，启动 40 ms（NetEQ 是 80 ms，我们 P2P 更低，启动时用 `start_forget_weight` 让估计很快收敛）。
3. `DecisionLogic`：水位（包缓冲 + 已解码待播样本）平滑后与目标比：
   - ≥ 高限（目标 + 窗口最大延迟余量 + 20 ms）→ Accelerate；≥ 4× 高限 → FastAccelerate；
   - < 低限（目标）→ PreemptiveExpand；
   - 包缺失 → Expand（用 Opus PLC，`decode(NULL)`）；之后第一个到达的包 → Merge（交叉淡化 5 ms）；
   - 水位 < 目标 50% 时推迟解码（`kPostponeDecodingLevel` 的做法）。
   - 水位滤波考虑已伸缩样本，防振荡。
4. `TimeStretch`（WSOLA 类）：在 48 kHz 降采样到 8 kHz 域找最佳重叠位置；每次最多删/加 ≤ 1 个周期（上限 ~10 ms）；对高相关性内容才做；相关性差时多个机会后降级为"按现有位置做短交叉淡化"；**立体声共用一个偏移**，保持声像。因为是音乐内容，淡化窗要比语音更长（10–15 ms）**[参数需 e2e/听感标定]**。
5. `DriftController`：慢速 PI，输出 `rubato` 重采样比率微调（±200 ppm 内）。
6. `Stats`：§6.1 全部指标，每秒出一份，进 e2e 的 JSON 报告。

**默认目标延迟**：起步 40 ms，P2P 稳定网络收敛到 20–30 ms；跨公网/中继网络 60–100 ms；硬上限 200 ms（再大就是网络出问题，应当提示用户，而不是继续堆延迟）。给用户一个"低延迟/平稳"开关：低延迟 = q 0.9、下限 20 ms；平稳 = q 0.99、下限 50 ms。

**对比**：NetEQ 在好网络上的目标延迟量级和我们相近，但我们的帧更短、起步更低，**欠载风险更高**；所以欠载 0 是硬门槛，并且"过载时降码率优先"的原则同样适用于音频：丢包上升先加冗余，再降码率到 64 kbps，最后才允许自己拉大目标延迟。

### 7.5 其他

- 控制端静音/窗口失焦时：停止解码但保留抖动缓冲的时间线；主机端根据"没有任何控制端在听"停掉采集，省 CPU 和流量（必须要有"控制端在听"的状态通知）。
- 本机自控（被控端和控制端同机）：Windows 用进程 loopback 的 `excludetree` 排除自己；macOS 用 `excludesCurrentProcessAudio`；Linux 创建独立 sink。否则会回声。
- 权限失败要明确提示（CLAUDE.md "不静默降级"）：macOS 没有屏幕录制/系统音频权限 → 界面写出原因和去系统设置的路径；Windows 没有输出设备 → 写"此机没有声卡/默认输出设备"；Linux 没有 PipeWire/PulseAudio → 写出需要安装的组件。

### 7.6 实施步骤与工作量估计

（1 人天 = 一个有经验的 Rust 工程师一天；含测试，不含评审；估计为经验值，**[未核实]**）

| 步骤 | 内容 | 估计 |
|---|---|---|
| A1 | `media::audio`：Opus 编解码封装（`opus` crate）、10 ms 分帧、重采样（rubato）、基础单测与覆盖率 | 2–3 天 |
| A2 | 音频包格式、QUIC 数据报接入、高优先级通道、预算扣除、回传统计；e2e 仿真里接入音频流 | 3–4 天 |
| A3 | Linux 采集（PipeWire）+ CI 的 null-sink 真实采集测试；控制端 cpal 播放；最简缓冲（固定延迟）先把整条链路跑通（闭环优先） | 3–4 天 |
| A4 | `ArrivalHistory` + `DelayEstimator` + `DecisionLogic`（不含时间伸缩，用丢帧/补零先顶）；指标 §6.1；e2e 抖动/丢包场景与门槛 | 4–5 天 |
| A5 | RED 冗余 + 自适应开关 + NACK 联动；丢包场景门槛 | 2–3 天 |
| A6 | `TimeStretch`（WSOLA）+ `DriftController`；漂移/突发场景；听感评审与参数标定（ViSQOL 趋势） | 5–7 天 |
| A7 | 口到耳测量（啁啾 + 互相关）与门槛；音画同步补偿 §3.3 | 3 天 |
| A8 | Windows 采集（WASAPI loopback，含设备切换）+ 实机测试；进程 loopback | 4–5 天 |
| A9 | macOS 采集（ScreenCaptureKit，和视频同流）+ 权限提示；可选 process tap | 4–5 天 |
| A10 | 设备变化处理、静音省流、自环排除、`THIRD_PARTY_NOTICES.md` 更新（libopus BSD-3、LGPL libpulse） | 2–3 天 |
| 合计 | | 约 32–42 人天 |

按"闭环优先"，A1–A3 完成就是可用的测试版；其后每一步都带指标和门槛进 e2e。可以派子代理做的：A10 的文档和通知文件、A8/A9 的平台胶水初稿（我审查）、ViSQOL 脚本；核心 A4/A6 自己做。

---

## 8. 未解决/需要补的数据

1. NetEQ 默认分位数、遗忘因子、`ms_per_loss_percent` 的实际默认值：读 `delay_manager.h` 与 field trial 配置，或在 `chrome://webrtc-internals` 对一段真实通话抓目标延迟曲线。
2. Opus：CELT-only 立体声是否确实不产生 LBRR（读 `opus_encoder.c`）；10 ms 帧下 `AUDIO` vs `RESTRICTED_LOWDELAY` 的真实前瞻和音质对比（我们自己测）。
3. 进程 loopback 的真实最低版本（Win10 2004 还是 build 20348）、WASAPI loopback 静音时的包行为：在 Windows CI/实机上实测。
4. `wasapi` crate 是否封装了进程 loopback；`coreaudio-rs`/`screencapturekit` 是否封装 process tap。
5. Parsec、GeForce NOW 的音频编码、抖动缓冲与音画同步：没有公开资料，**不要写进对标表**，除非用抓包实测。
6. libopus 1.5 DRED/Deep PLC 的构建开关、模型体积、CPU 开销：语音模式阶段再查。
7. BT.1359 的音画同步阈值原文；BlackHole 的许可证。
8. 时钟漂移在真实声卡（特别是蓝牙耳机、USB 声卡）上的实际分布：上线后遥测（只收聚合统计，不收内容）。
9. Windows 蓝牙耳机的播放延迟（经常 150–250 ms）是口到耳延迟里我们控制不了的一块：需要在对标时写明，指标分"到声卡缓冲"和"到耳"两档。
