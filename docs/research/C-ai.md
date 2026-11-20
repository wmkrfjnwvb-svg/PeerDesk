# C. 用 AI 做实时视频的带宽估计、拥塞控制与抗丢包：现状与 PeerDesk 落地路线

> 调研日期：2026-11-20。标记说明：**[生产]** 表示论文或厂商报告了线上部署或 A/B 结果；**[实验室]** 表示只有仿真、模拟或小规模真实网络结果；**[未核实]** 表示我只看到二手来源，或凭记忆写出、本轮没有打开原文核对。

## 0. 结论先行

1. **能在生产中站住的学习型带宽估计，几乎都走"离线学习 + 启发式兜底"这条路**，没有一个是纯在线 RL。Microsoft Teams 先后发布了 Merlin（模仿学习）、Ivy（离线元学习，在多个 BWE 之间做选择）和 2025 年的人在回路离线 RL，最后这项的 A/B 结果是"主观差评通话比例降低 11.41%"。阿里淘宝直播部署了 Concerto、OnRL、Loki，其中 Loki 把规则模型与学习模型融合，用来压制长尾。
2. **纯 RL 拥塞控制（Aurora、Orca、Sage 等）在泛化上仍有公认缺陷**。2025 年的独立复现研究显示，这些方案在带宽与时延同时变化时表现不如人工设计的算法。PeerDesk 不应让模型直接控制发包节奏（pacing）或拥塞窗口（cwnd）。
3. **"糊一下但不冻结"这个目标，用今天的硬件编码器已经能达成大部分**：帧内刷新（intra-refresh）、长期参考帧（LTR）和参考帧选择（RPS）、时域 SVC，再加上由预测驱动的 FEC 和快速降码率。神经编解码器（Grace、Reparo）效果最好，但需要 GPU 级算力，并且不兼容硬件 H.264/HEVC/AV1，只适合放在远期选项里。
4. **对 PeerDesk 的建议**：v1 先用经典的 GCC 风格估计器，配合硬性护栏和完整遥测；第一个模型选"小型 GRU/MLP，用离线 RL 或行为克隆训练，只输出带宽上限的修正量"，并以影子模式（shadow mode）上线。

---

## 1. 学习型拥塞控制与带宽估计

### 1.1 经典学术工作（多为通用 TCP 拥塞控制，**[实验室]**）

| 系统 | 方法 | 输入特征 | 规模与成本 | 泛化与安全 |
|---|---|---|---|---|
| Aurora / PCC-RL（ICML'19）[未核实细节] | 在线 RL（PPO），在每个监控间隔（MI）调整发送速率 | 时延梯度、时延比、发送比，取最近 k 个 MI 的历史 | 两层小型 MLP（约 32/16 个单元，凭记忆），推理开销可忽略 | 只在仿真中训练，对未见过的网络泛化差 |
| Indigo（Pantheon，ATC'18）[未核实] | 对"预知链路的 oracle"做模仿学习（DAgger），LSTM | 排队时延、发送与接收速率、cwnd | 单层小型 LSTM | 在 Pantheon 的真实路径上表现不稳定 |
| Orca（SIGCOMM'20） | 底层跑 Cubic，DRL 每隔一段时间改写 cwnd | 吞吐、时延、丢包、cwnd | 小型 MLP，决策粒度在毫秒到数十毫秒 | 用"经典算法打底、模型只做粗调"换来安全性 |
| Sage（SIGCOMM'23，[论文](https://cs538.github.io/readings/yen23.pdf)、[代码](https://github.com/Soheil-ab/sage)） | 离线 RL，从十余种已有 CC 策略的日志里学 | 约数十维内核 TCP 统计量 | 有内核实现，GRU 加 MLP | 只用离线数据，不做在线探索 |
| MOCC（多目标 CC）[未核实会议] | 把偏好向量作为输入的 RL，可在线迁移到新目标 | 与 Aurora 类似 | 小型 MLP | 用迁移学习缓解对单一目标的过拟合 |

独立评测：[Learning-Based vs Human-Derived CC（arXiv 2510.25105）](https://arxiv.org/abs/2510.25105) 在大规模实验中比较了 Orca、Sage、Astraea 与 Cubic、BBRv3，结论有三点：把公平性写进奖励函数能起作用，但到了未见场景就不再成立；在带宽和时延动态变化时，学习型方案整体表现落后。对远程桌面来说，"突变"正是主场景，所以这是一条关键的负面证据。

### 1.2 面向实时音视频（RTC）的工作

- **Concerto（MobiCom'19，"Learning to Coordinate Video Codec with Transport Protocol for Mobile Video Telephony"，Zhou 等，DOI 10.1145/3300061.3345430）**：用模仿学习协调编码器码率和传输层，解决两者"各调各的"的问题。据[二手资料](https://www.besthub.dev/articles/machine-learning-based-bandwidth-prediction-and-adaptive-streaming-for-taobao-live-concerto-onrl-and-loki-7e4c30b2ca62)，已在淘宝直播部署 **[生产，二手]**。
- **OnRL（MobiCom'20）**：每个用户端都放一个在线 RL agent，在淘宝直播上做了一个月的评测，覆盖 151 名用户、543 小时，卡顿率降低 14.22%，画质持平。论文设计了"混合学习"，在探索导致 QoE 恶化时切回规则算法。Mowgli 论文把它描述为"检测到灾难性 QoE 下降就临时降级到 GCC"（[Mowgli](https://arxiv.org/abs/2410.03339) 转述，OnRL 本身的切换逻辑我**[未核实]**）。**[生产（小规模）]**
- **Loki（MobiCom'21）**：把学习模型和规则模型在特征层面融合，专门针对长尾。据报告在商用系统中评测了 1.01 亿个会话；二手资料给出的数字是卡顿率降低 13.98%–27.27%（**[生产，数字为二手]**）。后续的 Loki+ 指出 Loki 的输出会在动作空间两端来回振荡。
- **Palette（[arXiv 2304.03505](https://arxiv.org/abs/2304.03505)）**：跨层 RL，直接输出下一帧的压缩因子（如 QP），真实网络中卡顿率降低 3.1%–46.3% **[实验室加真实网络]**。
- **Mowgli（[arXiv 2410.03339](https://arxiv.org/abs/2410.03339)）**：**只用线上已有 GCC 的遥测日志**学习码率控制，配合保守的反事实推理，比 GCC 码率高 15%–39%，冻结减少 60%–100% **[仿真加真实网络]**。它最适合 PeerDesk 照搬，因为训练期间不必让任何用户承担探索风险。
- **Merlin（[arXiv 2309.13481](https://arxiv.org/abs/2309.13481)，微软）**：对 Teams 生产中的 UKF（无迹卡尔曼滤波）估计器做行为克隆。洲际真实通话中，与原启发式没有统计显著差异；在 75 通电话上微调后，QoE 最多提升 7.8%，所需样本比在线 RL 少 80% **[真实通话，未全量]**。
- **Ivy（[arXiv 2409.19867](https://arxiv.org/abs/2409.19867)，微软）**：离线元学习，按当前网络状态在多个 BWE 算法之间挑选，在 Teams 中实现，QoE 提升 5.9%–11.2% **[Teams 实验]**。
- **人在回路 BWE（[arXiv 2510.12265](https://arxiv.org/abs/2510.12265)，2025）**：先用主观评分训练客观 QoE 奖励模型，再从约 100 万条 Teams 真实通话轨迹中用分布式离线 RL 训练估计器。真实 A/B 中"主观差评通话比例"降低 11.41%，论文称已部署到 Teams 媒体栈 **[生产]**。这是目前公开证据最强的一项。
- **NVC 感知 CC（[arXiv 2411.06742](https://arxiv.org/abs/2411.06742)）**：利用神经编解码器的抗丢包能力，在 RL 奖励中容忍丢包，训练时间缩短 41%，卡顿减少 20%–77% **[仅仿真]**。
- **Vidaptive（[arXiv 2309.16869](https://arxiv.org/abs/2309.16869)）与 Pudica（[NSDI'24](https://www.usenix.org/conference/nsdi24/presentation/wang-shibo)）**：两者都不是学习型，但与远程桌面关系很近。Vidaptive 用 padding 让视频流像持续满载的流一样被时延型 CC 控制，比 GCC 码率高约 1.5 倍，P95 帧延迟降低 2.2 秒。Pudica 来自腾讯等团队，目标是排队时延接近零，已部署在大规模云游戏平台，覆盖超过 5.7 万次游戏会话；幻灯片给出的平均帧延迟是 23.2 ms，Copa 为 39.8 ms **[生产，作者自报]**。说明：**在云游戏和远程桌面上，经典的"近零排队"设计已经是很强的基线**。

至于字节跳动、Zoom、Agora 的学习型 BWE，本轮**没有找到一手论文或官方技术说明**，只能标记为 **[未核实]**。

### 1.3 ACM MMSys 2024 离线 RL 带宽估计挑战赛（微软）

一手来源：[挑战赛论文 arXiv 2403.06324](https://arxiv.org/abs/2403.06324)、[结果页](https://www.microsoft.com/en-us/research/academic-program/bandwidth-estimation-challenge/results/)、[GitHub](https://github.com/microsoft/RL4BandwidthEstimationChallenge)。

- **数据集**：一份是 18,859 通真实 Teams P2P 测试床通话，节点分布在多个国家，有线和无线都有，使用 6 种行为策略（v0–v5，包括卡尔曼滤波类、WebRTC 和若干 ML 策略）。另一份是 9,405 通网络仿真通话，带有容量和丢包率的真值。每条轨迹由四元组构成：观测、动作（bps）、音频 MOS 奖励、视频 MOS 奖励。奖励来自无参考和有参考的 MOS 预测模型，与主观 MOS 的皮尔逊相关系数（PCC）大于 0.95。
- **观测（150 维）**：15 个特征，各取 5 个 60 ms 短窗和 5 个 600 ms 长窗。特征包括接收速率、包数、字节数、排队时延（当前减去历史最小）、时延、最小时延、时延比、窗内平均减最小、包到达间隔、抖动、丢包率、平均连续丢包数，以及音频、视频、探测包的占比。**这是一份可以直接照抄的接收端特征清单。**
- **基线**：用 IQL 训练，结构是归一化层加两层 128 单元 tanh，输出动作的均值和方差，再按对数映射到 10 kbps–8 Mbps，以 ONNX 格式发布。参数约 3.5 万个，CPU 单次推理在微秒级（参数量是我按结构推算的）。
- **结果**：21 支队伍报名，7 支提交。前 4 名在测试床上各跑 600 通 3 分钟通话，历时 12 天。冠军 **Schaferct** 是 GRU 加残差的 actor-critic，用 IQL 在约 10% 的测试床数据上训练；亚军 Fast and furious 用两阶段非对称 actor-critic，critic 可以看到真值。第 3 名 **Paramecium 是纯规则算法**。论文的原话是：冠军"与数据集中最好的行为策略表现相当"，也就是**没有超过最好的生产启发式**。另一项观察是：接收速率与视频 MOS 强相关；MSE 低不等于 QoE 高。

---

## 2. 带宽与突降预测

- **Sprout（NSDI'13）**[未核实细节]：把蜂窝链路建模为泊松过程，速率按布朗运动漂移，用贝叶斯滤波得到速率分布，以 95% 分位做保守预测。每 20 ms 一个 tick，向前预测约 8 个 tick（约 160 ms），据此决定"未来 100 ms 内可安全发送多少字节"。启示：**预测分布的下分位数比预测均值更有用**。
- **Lumos5G（IMC'20，[PDF](https://feng-qian.github.io/paper/5g_imc20.pdf)）**：针对毫米波 5G，以 1 秒为粒度，用 GDBT 和 Seq2Seq（LSTM）预测下一秒乃至未来 k 秒（例如 30 秒）的吞吐。特征包括位置、移动速度和方向、LTE/5G 的 RSRP/RSRQ/RSSI、水平与垂直切换。误差比已有模型低 1.37–4.84 倍。论文自己也指出：短期（下一秒）预测最适合做实时决策。
- **对远程桌面而言，可用的预测范围**：拥塞控制需要的是 50–500 ms 级的突降预警，这正是 RTT 级反馈来不及的地方；码率阶梯和 FEC 档位要看 1–5 秒；10 秒以上的预测只对"预取"类业务有意义，对交互流用处很小。蜂窝和 Wi-Fi 的突降很多由切换、漫游、遮挡触发，**OS 或无线电事件（漫游开始、RSSI 骤降、MCS 下降、蜂窝切换）提供的是先验信号，比包级统计更早**。但这只是逻辑推断，我没有找到量化"提前多少毫秒"的一手结论 **[未核实]**。
- **可获取的 OS 信号**：Windows 有 `WlanQueryInterface`（RSSI、链路速率）；macOS 有 CoreWLAN（`rssiValue`、`transmitRate`）；Linux 用 nl80211 读站点信息；Android 有 `WifiInfo.getRssi/getLinkSpeed`、`TelephonyManager`、`SignalStrength`（RSRP）和 `ConnectivityManager.NetworkCallback`；iOS 公开 API 很少，只有 `NWPathMonitor` 能看接口类型和 `isExpensive`/`isConstrained`。这些 API 都只提供低频采样（约 1 Hz），应当作为模型的慢变上下文，而不是主信号。

---

## 3. 优雅降级的抗丢包："先糊、立刻恢复"

| 技术 | 原理 | 额外开销 | 恢复延迟 | 硬件编码器支持 | 成熟度 |
|---|---|---|---|---|---|
| 帧内刷新（列或行逐步刷新） | 把 I 帧拆散到 N 帧里 | 码率平滑，总量约 +5–15%（经验值）[未核实] | N 帧（例如 30 帧约 0.5 s），画面逐条变清 | NVENC、Intel QSV、AMD AMF、VideoToolbox 的 H.264/HEVC 普遍支持；AV1 支持程度因厂商而异 | 高（云游戏标配） |
| LTR 与参考帧选择（RPS） | 收到丢包反馈后，以最后一个确认收到的 LTR 为参考编下一帧 | 只在恢复时多花一帧 P 帧的码率 | 1 个 RTT，加一帧 | NVENC（H.264/HEVC 的 LTR）、部分 QSV/AMF 支持；Apple 平台支持有限 [未核实] | 高（WebRTC 和 Moonlight/Sunshine 一类实现在用） |
| 时域 SVC（L1T2/L1T3） | 丢弃增强层帧不影响基础层 | 编码效率损失约 5–10% [未核实] | 0（直接降帧率） | NVENC 支持 H.264 时域层；AV1 硬件编码器支持程度不一 | 高 |
| 空域 SVC | 基础层分辨率低，增强层补细节 | 额外 10–30% | 0（直接降分辨率，"变糊"） | 硬件支持很少 | 中 |
| 预测驱动的 FEC | 根据预测的丢包率和突发长度动态调整冗余 | 冗余比例 | 0 | 与编码器无关 | 高。Tambur（NSDI'23，流式码）[未核实细节] |
| 神经错误隐藏 | 解码端用光流或生成模型补出丢失的宏块 | 需要 GPU/NPU | 0 | 可与硬件解码共存（后处理） | 低 |
| **Grace（NSDI'24，[arXiv 2305.12333](https://arxiv.org/abs/2305.12333)）** | 在模拟丢包下联合训练神经编码器和解码器，收到任意子集的包都能解出一帧 | 编解码都需要 GPU | 0 | **不兼容**，替代整个编解码器 | 研究阶段：不可解码帧减少 95%，卡顿减少 90%，MOS +38% [实验室加众包主观评测] |
| **Reparo（[arXiv 2305.14135](https://arxiv.org/abs/2305.14135)，[MLSys'26](https://mlsys.org/virtual/2026/oral/3839)）** | 先把帧编成 token，接收端用生成模型补全丢失的 token | 生成模型，GPU | 0 | 不兼容 | 研究阶段，主要面向人脸/会议画面，对桌面文字内容未验证 |
| 接收端超分辨率（NAS OSDI'18、NEMO MobiCom'20、LiveNAS SIGCOMM'20） | 低码率传输，接收端用 DNN 恢复细节。NEMO 只在 3–11% 的锚点帧上跑 SR，其余帧复用结果 | 移动端可实时 [NEMO 项目页](https://ina.kaist.ac.kr/projects/nemo/) | 0 | 与硬件解码兼容（后处理），但 NEMO 需要改动解码器 | 中；内容感知模型需要按内容训练 |

**对远程桌面的判断**：桌面内容以文字和 UI 为主，大面积静止，局部突变。神经超分和生成式补全容易把文字"脑补"错，这种错误比模糊更糟糕。所以首选组合是：**快速降码率（降 QP 或降分辨率）+ 时域分层 + LTR/RPS 恢复 + 帧内刷新兜底 + 基于预测的 FEC**。神经增强只作为可选后处理，并且必须在文字区域关闭。

---

## 4. 双端估计与融合

- **接收端知道的**：每个包的单向时延梯度、到达间隔与抖动、丢包的模式（随机还是突发）、接收速率、ECN 标记（若有），以及本机 Wi-Fi/蜂窝信号。这些都是带宽和排队信息的"一手观测"，MMSys 2024 的 150 维特征全部来自接收端。
- **发送端知道的**：发送速率、pacing 队列长度、编码器实际输出码率和 QP、QUIC 层的 RTT（smoothed、min、variance）、ACK 时间戳、丢包检测与重传，以及本机上行链路的信号。
- **融合方式**：(1) 遵循 GCC 和 RFC 8888 的思路，接收端每 50–100 ms 回传紧凑的反馈，可以是原始到达时间戳，也可以是接收端算好的估计值，由发送端统一决策。(2) 双方各自产出"本端链路健康度和事件"，例如"本机 Wi-Fi 正在漫游""RSSI 在 1 秒内下降 15 dB"，以低频事件的方式发给对端，作为模型的上下文特征。(3) 决策只在发送端做，最终码率取 min(发送端估计, 接收端建议, 护栏上限)。
- **推断对端（例如控制端用 Wi-Fi）的网络**：不需要对端上报也能做一部分。Wi-Fi 的特征包括：上行 ACK 和反馈包的到达呈"聚合突发"，因为 A-MPDU 和 TXOP 会成批发出；时延抖动呈双峰；丢包后常跟一段时延尖峰，这是 MAC 层重传的表现。蜂窝的特征是：到达间隔呈约 1 ms 或 TTI 量级的调度颗粒，速率呈台阶式跳变。也可以直接让对端上报接口类型和 RSSI（低成本，推荐）。被动推断的准确率我没有找到一手量化数据 **[未核实]**。远程桌面还有一个特点：控制端到被控端的上行只有输入事件和反馈，流量很小，所以很难从中测出对端的下行容量。**主要的视频方向必须依赖接收端（控制端）回传。**

---

## 5. 工程部署

- **Rust 推理选型**：[tract](https://github.com/sonos/tract) 是纯 Rust，支持 ONNX 和 NNEF，体积小（约 1–3 MB 量级 [未核实]），适合小型 MLP/GRU。[ort](https://github.com/pykeio/ort) 绑定 ONNX Runtime，功能全，但动态库有十几 MB。[candle](https://github.com/huggingface/candle) 适合在 Rust 里直接写网络结构，但 ONNX 导入并不完整。**建议用 tract，或者干脆手写前向计算**：两层 128 单元的 MLP 只需约 3.5 万次乘加，200 行 Rust 就能写完，没有依赖，便于审计。
- **体积与 CPU 预算**：模型应小于 500 KB，否则推理库会让二进制体积显著变大。决策频率 10–20 Hz（与 MMSys 的 60 ms 监控间隔对齐），单次推理控制在 50 µs 以内；在现代 CPU 上 MLP 只要个位数微秒，GRU（隐层 64）在 20 µs 以内。这些都是估算，**需要实测**。
- **训练数据来源**：(a) 产品自身的遥测轨迹，按 Mowgli 和 Merlin 的做法记录"观测、动作、结果"。(b) 公开轨迹：Pantheon、MMSys 2024 数据集（有许可证，需检查条款）、Lumos5G、FCC/Norway 3G/HSDPA 宽带轨迹 [未核实许可证]。(c) 仿真：Mahimahi、ns-3、OpenNetLab/AlphaRTC（MMSys 2021 挑战赛使用的平台）[未核实]，以及 Linux 的 tc/netem。注意 MMSys 2024 论文明确写道，第一届挑战赛的经验是"仿真里训练的 RL 存在 sim-to-real 差距"。
- **评测**：按 Pantheon 的思路，在固定的轨迹集上同时测几组指标：码率、P95/P99 帧延迟、冻结次数与总时长（例如超过 200 ms 无新帧）、恢复时间（突降后多少毫秒回到可用画质）、和 TCP/BBR 竞争流共存时的公平性。必须包含的场景有：阶跃下降（例如 20 Mbps 降到 2 Mbps）、Wi-Fi 漫游、突发丢包、策略限速器（policer），以及长 RTT。
- **上线方式**：(1) 影子模式：模型只算不用，同时记录"模型建议值与实际值的差"和反事实指标。(2) 在小比例用户上做 A/B，按网络类型分层。(3) 硬性护栏：输出夹在 [min, 经典估计 × k] 区间内；排队时延或丢包超过阈值时立即交给经典路径；检测到模型输出 NaN 或超时，就永久回退到经典算法，直到本次会话结束；保留远程开关（kill switch）。(4) 模型版本与特征版本一起签名。这些都是 OnRL、Loki、Mowgli、Teams 共同采用的模式。

---

## 6. 给 PeerDesk 的分阶段方案（带主观判断）

### v1：经典方法，但按"可插拔模型"来设计
- **估计器**：接收端用 GCC 风格的时延梯度滤波器（trendline 或 Kalman）加丢包控制器；发送端结合 QUIC 的 RTT 和 ACK 速率，参考 Pudica 和 Vidaptive 的思路，以排队接近零为目标。
- **接口**：定义 `trait RateController { fn on_feedback(&mut self, obs: &Observation) -> RateDecision }`，其中 `RateDecision { target_bps, max_bps, fec_ratio, keyframe_req, layer_drop }`。`Observation` 直接采用 MMSys 的 15 个特征，分 60 ms 和 600 ms 两个窗口，再加上发送端的 QUIC 统计、编码器 QP 与实际码率，以及双端的链路事件（接口类型、RSSI、漫游）。
- **遥测**：每 60 ms 记录一行"观测向量、决策、事后结果（冻结、帧延迟、实际接收速率）"，压缩后由用户自愿上报。默认只在本地保存环形缓冲区，并且绝不包含画面内容或 IP 地址。这份日志就是将来离线 RL 和行为克隆的训练集。
- **抗丢包**：使用硬件编码器的 LTR/RPS 加帧内刷新，加时域分层（能用时），加自适应 FEC。突降时的动作是：第一个反馈周期内立即把码率降到估计值的 50–70%，降分辨率而不降帧率，用 LTR 恢复，不发全量 I 帧（全量 I 帧在带宽骤降时会造成数百毫秒的冻结）。
- **轨迹工具**：内置 netem 和 Mahimahi 风格的轨迹回放测试，作为回归测试集。

### v2：第一个模型
- **模型族**：小型 GRU（隐层 32–64）或两层 MLP，加 150 维特征。先**对 v1 估计器做行为克隆**，也就是 Merlin 的路线，验证管线能否复现基线；然后在 v1 遥测上**用 IQL 做离线 RL 微调**，奖励取"码率效用 − 冻结惩罚 − 延迟惩罚"，这对应 Mowgli、MMSys 冠军和 Teams 2025 的做法。
- **选它的理由**：有生产证据，训练时不需要线上探索，推理在微秒级，用纯 Rust 可以实现。
- **输出形式**：模型不直接给码率，而是给经典估计的**乘性修正**（例如 0.5–1.5 倍）加一个"突降概率"，后者用来提前提高 FEC 比例、提前切到更低的时域层。模型失效时，系统等同于 v1。
- 暂时不做：在线 RL（OnRL 式探索）、端到端 Transformer、神经编解码器。

### v3：远期
- 用无线电和 OS 事件做 100–500 ms 的突降预测（分类器）；按用户或网络做个性化微调（Merlin 微调、Ivy 式选择器）；评估把接收端神经增强用于非文字区域。

### 经典层必须始终保证的事
1. 码率上限永远不超过经典估计 × k，下限保证音频和光标/输入通道可用。
2. 排队时延超过阈值（例如 100 ms）或连续丢包时，经典算法立即接管，不经过模型。
3. 恢复路径（LTR/RPS、关键帧请求、帧内刷新）完全由确定性逻辑控制，不受模型影响。
4. 与竞争流公平共存，不饿死同一链路上的 TCP 流（参考 2025 年那项研究：学到的公平性到了新场景就不成立）。
5. 模型超时、NaN、版本不匹配时自动回退；保留远程开关。
6. 所有遥测默认只在本地，上报需要用户自愿同意，且不包含画面内容。

---

### 主要来源
- MMSys 2024 挑战赛：<https://arxiv.org/abs/2403.06324>，<https://github.com/microsoft/RL4BandwidthEstimationChallenge>，<https://www.microsoft.com/en-us/research/academic-program/bandwidth-estimation-challenge/results/>
- Merlin <https://arxiv.org/abs/2309.13481>；Ivy <https://arxiv.org/abs/2409.19867>；人在回路 BWE <https://arxiv.org/abs/2510.12265>
- Mowgli <https://arxiv.org/abs/2410.03339>；Palette <https://arxiv.org/abs/2304.03505>；NVC 感知 CC <https://arxiv.org/abs/2411.06742>；Vidaptive <https://arxiv.org/abs/2309.16869>
- Sage <https://github.com/Soheil-ab/sage>；学习型 CC 实测 <https://arxiv.org/abs/2510.25105>；Pudica <https://www.usenix.org/conference/nsdi24/presentation/wang-shibo>
- Grace <https://arxiv.org/abs/2305.12333>；Reparo <https://arxiv.org/abs/2305.14135>；NEMO <https://ina.kaist.ac.kr/projects/nemo/>；Lumos5G <https://feng-qian.github.io/paper/5g_imc20.pdf>
- 淘宝 Concerto/OnRL/Loki（二手）：<https://www.besthub.dev/articles/machine-learning-based-bandwidth-prediction-and-adaptive-streaming-for-taobao-live-concerto-onrl-and-loki-7e4c30b2ca62>，OnRL（阿里云中文）<https://developer.aliyun.com/article/792344>
