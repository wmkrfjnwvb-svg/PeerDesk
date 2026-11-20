# PeerDesk 传输层调研：实时交互视频的拥塞控制、带宽估计与抗抖动

> 调研日期：2026-11-20。标注说明：**[已核实]** 表示我读过原始源码或论文原文；**[未核实]** 表示只基于摘要、二手资料或记忆，落地前要再确认。

## 0. 结论先行

1. 产品负责人的直觉基本对，但需要说准确。WebRTC 的问题不在"它为通话设计"，而在三个具体假设上，这三个假设在远程桌面里都不成立：
   - 流量连续，基本能把估计"喂饱"；
   - 帧大小大致平稳；
   - 宁可多缓冲几十毫秒也要播放平滑。

   远程桌面是"长时间几乎静止，偶尔突发一个巨帧"，而且对输入延迟极敏感。
2. 学术界和工业界已有更贴近这一场景的方案。最值得借鉴的三个：
   - **Pudica**（NSDI'24，云游戏，已大规模部署）：以帧为探测包，估计"带宽利用率"而不是带宽本身；
   - **Salsify**（NSDI'18）：按网络状态给每帧定字节预算，编码后绝不排队；
   - **ACE**（SIGCOMM'25）：在帧尺度上控制发送突发。
3. quinn 的拥塞控制接口看不到接收时间戳，也不能直接设置发送速率。因此**媒体码率控制必须放在应用层**，跑在 QUIC datagram 之上，quinn 的拥塞控制只作安全网。

---

## 1. WebRTC GCC 深度剖析

**来源**：[Carlucci et al., MMSys'16](https://dl.acm.org/doi/10.1145/2910017.2910605)、[draft-ietf-rmcat-gcc-02](https://datatracker.ietf.org/doc/html/draft-ietf-rmcat-gcc-02)，以及 libwebrtc 当前 main 分支源码 [goog_cc/](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/congestion_controller/goog_cc/)。

**基于时延的估计器（trendline）** [已核实，见 [trendline_estimator.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/congestion_controller/goog_cc/trendline_estimator.cc)]

- 按约 5 ms 的发送突发把包分组，计算组间单向时延梯度 `d = Δarrival − Δsend`，累加后做指数平滑（系数 0.9）。
- 在最近 20 个点（`kDefaultTrendlineWindowSize=20`）上做最小二乘线性回归，斜率即 trend。
- `modified_trend = min(num_deltas, 60) × trend × 4.0`，即阈值增益为 4。

**过载检测器与自适应阈值** [已核实]

- 判定为 overuse 需同时满足：`modified_trend > γ`，持续超过 10 ms（`kOverUsingTimeThreshold`），至少两次采样，且 trend 不在下降。
- 阈值按 `γ += k·(|m| − γ)·Δt` 更新：|m|<γ 时 k_down=0.039，否则 k_up=0.0087；γ 限定在 [6, 600]；|m| 超过 γ+15 时不更新，避免被突发尖峰拉高。
- 自适应阈值的设计目的是与 TCP 竞争时不被饿死：TCP 把队列填满后，γ 被抬高，GCC 就不会持续让步（Carlucci 论文的核心论点）。

**AIMD 码率控制** [已核实，见 [aimd_rate_control.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/remote_bitrate_estimator/aimd_rate_control.cc)]

- 远离收敛点时乘性增长，`α=1.08^min(Δt,1s)`，即约每秒 +8%。
- 接近上次检测到的链路容量时改为加性增长，约每个响应时间（`2×(RTT+100ms)`）增加一个包的大小。
- 过载时降到 `β=0.85 × 实测吞吐`。

**基于丢包的估计 BWE v2** [部分核实，见 [loss_based_bwe_v2.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/congestion_controller/goog_cc/loss_based_bwe_v2.cc)]

- 不再用 v1 的"丢包率 2%/10% 阈值"，改为最大似然模型：在一组候选带宽（实测 ack 速率、时延估计的倍数）上，对一个观测窗口做时间加权，用 Newton 步拟合"固有丢包率 inherent_loss + 超过带宽导致的丢包"。
- 含 hold（初始 300 ms）、instant upper bound、ALR 期间不使用 ack 速率等机制。
- 优点：能区分 Wi-Fi/蜂窝上的随机丢包和拥塞丢包。缺点：参数极多，几乎全部由 field trial 下发，默认值以线上实验为准。

**探测** [已核实，见 [probe_controller.cc](https://webrtc.googlesource.com/src/+/refs/heads/main/modules/congestion_controller/goog_cc/probe_controller.cc)]

- 启动时按 3× 和 6× 初始码率做指数探测；结果超过目标的 70% 时继续 ×2 探测。
- ALR（应用受限区域）期间每 ≥5 s 做一次 2× 探测；ALR 结束 3 s 内允许探测；码率骤降到 66% 以下后 5 s 内做恢复探测。
- 探测簇最短 15 ms，以 padding 或 RTX 填充；接收端用簇内包到达间隔估计容量。

**已知弱点**

- **爬升慢**：+8%/s 意味着从 1 Mbps 到 10 Mbps 约需 30 s，只能靠探测弥补。
- **Wi-Fi 聚合**：A-MPDU 和块确认让包成批到达，时延梯度噪声大，容易误判 overuse，也会导致阈值被抬高 [未核实具体量化数据]。
- **蜂窝网络**：基站深缓冲加调度抖动。Zhuge（SIGCOMM'22）指出，无线 AP 上的拥塞会显著拉长控制环，导致尾延迟恶化 [摘要核实]。
- **竞争 TCP**：自适应阈值能缓解但无法消除问题，基于时延的流在与 Cubic 竞争时通常拿不到公平份额。
- **最关键的一点**：ALR 期间估计会冻结或失真。远程桌面 90% 以上的时间都处于 ALR。

## 2. BBR 与其他实时拥塞控制

**BBR v1/v2/v3**（[draft-ietf-ccwg-bbr-06](https://www.ietf.org/archive/id/draft-ietf-ccwg-bbr-06.html)，即 BBRv3，Experimental）

- 原理：维护 max-filter 交付速率 BtlBw 和 min-filter RTprop，pacing = gain × BtlBw，cwnd = 2·BDP。
- v2/v3 加入了基于丢包和 ECN 的 inflight_hi/lo 上限，ProbeBW 周期更保守。

用于实时媒体时有以下陷阱：

- **应用受限样本**：被标记为 app-limited 的交付速率样本，只有在高于当前估计时才被采纳。结果是：
  - 空闲期间估计不会下降，空闲结束后可能高估；
  - 估计也上不去，因为应用从未填满管道。
- **启动阶段**：[BBR-Copilot（arXiv 2606.03468）](https://arxiv.org/abs/2606.03468) 在直播（含 TikTok Live）中观察到 BBR 难以退出 Startup，造成自损丢包；稳定期发送速率又低于可用带宽。他们的解法是主动发送额外数据来制造准确的带宽样本 [摘要核实]。
- **ProbeBW 的 1.25 增益**：会周期性建立队列，对交互视频直接表现为帧延迟尖峰。
- **业界用法**：一些产品用 QUIC+BBR 承载远程桌面（如题中提到的 Airhand，我未能核实其细节）。这种做法可行，前提是码率由应用层单独控制，BBR 只负责"不让 QUIC 卡住"。

**SCReAM**（[RFC 8298](https://www.rfc-editor.org/rfc/rfc8298)；v2 为 [draft-ietf-ccwg-rfc8298bis-screamv2](https://datatracker.ietf.org/doc/html/draft-ietf-ccwg-rfc8298bis-screamv2-00)）

- 基于窗口：类 LEDBAT 的 qdelay 测量（无需时钟同步）加自时钟。v2 简化了算法，加入 L4S（类 DCTCP/Prague 的 ECN-CE 比例响应），时延部分采用 "pseudo-L4S"。
- 窗口型算法有天然的在途字节上限，这与"控制在途字节"的思路一致。
- 比较研究发现 SCReAM 队列占用最低，但链路利用率偏低（[Zhang & Lei, arXiv 1809.00304](https://arxiv.org/abs/1809.00304)）。

**NADA**（[RFC 8698](https://www.rfc-editor.org/rfc/rfc8698)）

- 把排队时延、丢包惩罚和 ECN 合成一个"聚合拥塞信号"，速率朝该信号的参考值做平滑调整。收敛快，但有 late-comer 不公平问题。

**其他延迟敏感拥塞控制**

| 算法 | 核心机制 | 局限 |
|---|---|---|
| [Sprout](https://www.usenix.org/conference/nsdi13/technical-sessions/presentation/winstein)（NSDI'13） | 用随机过程预测蜂窝链路的未来交付量，保证 95% 概率时延不超 100 ms | 假设独占瓶颈 |
| [Verus](https://dl.acm.org/doi/10.1145/2785956.2787498)（SIGCOMM'15） | 学习"时延—窗口"曲线 | 同上 |
| [Copa](https://www.usenix.org/conference/nsdi18/presentation/arun)（NSDI'18） | 目标速率 = 1/(δ·d_q)，δ 默认 0.5，检测到竞争时切换到 TCP 兼容模式 | — |
| [PCC Vivace](https://www.usenix.org/conference/nsdi18/presentation/dong)（NSDI'18） | 在线效用梯度：吞吐 − 时延梯度惩罚 − 丢包惩罚 | 需要持续用不同速率做微实验，应用受限时无法执行 |
| [Proteus](https://dl.acm.org/doi/10.1145/3387514.3405891)（SIGCOMM'20） | Vivace 的 scavenger 变体 | 同 Vivace |

**L4S/Prague**（[RFC 9330](https://www.rfc-editor.org/rfc/rfc9330)/[9331](https://www.rfc-editor.org/rfc/rfc9331)/[9332](https://www.rfc-editor.org/rfc/rfc9332)）

- 端到端可用时，亚毫秒级 ECN 标记能提供远好于时延梯度的信号。但目前部署有限（部分 DOCSIS 和 5G），只能作为可选增强。

## 3. 编解码与传输协同设计

**Salsify**（[NSDI'18 论文](https://www.usenix.org/conference/nsdi18/presentation/fouladi)）[已核实原文]

- **接收端估计**：把每帧的分片当作 packet train，按 `τ ← 0.1·(T_i − T_{i−1} − grace_i) + 0.9·τ` 平滑到达间隔。grace period 由发送端填入，用于扣除帧间的发送停顿，否则估计会悲观。这正是远程桌面"间歇发送"问题的最小解。
- **发送端预算**：每帧目标大小为 `(d/τ − N_inflight) × MTU`，其中 d=100 ms，N 由"最后发送序号 − 最后确认序号"估计。
- **编码策略**：编码器（函数式 VP8，可保存和恢复状态）每帧并行编出一高一低两个版本，取不超预算的最大者；都超预算就丢帧；连续丢 4 帧后强制发送低质量版本以获得反馈。
- **核心原则**："编码后绝不排队"，网络排队转化为"跳到最新帧"。

**帧级与包级控制**

- 包级拥塞控制（BBR/Cubic）不知道帧边界，会把一个大帧拆开慢慢送出，帧延迟 = 队列 + 帧大小 / 速率。
- 帧级控制把"帧完成时间"作为优化目标，可以决定该帧整体以多快的速率发出（见 Pudica 和 ACE）。

**ACE**（SIGCOMM'25，[ICCRG 幻灯片](https://datatracker.ietf.org/meeting/123/materials/slides-123-iccrg-ace-sending-burstiness-control-for-high-quality-real-time-communication-01)）[仅幻灯片核实]

- 论点：pacing 本身会引入不必要的延迟，线上数据显示它与卡顿相关；带宽低估时 pacing 速率更低，情况更糟。
- 出队侧：自适应调整令牌桶容量来控制突发。
- 入队侧：选择编码复杂度，使编码时间加传输时间最小，从而让帧大小更平滑。
- 启示：pacing 速率不应该等于估计带宽。允许一帧以略高于估计值的速率突发出去，往往帧延迟更低。

## 4. 远程桌面与视频通话的本质差异

我没有找到专门针对"远程桌面空闲期带宽估计"的同行评审论文。最接近的是：

- **Pudica**（云游戏帧流）[已核实原文]；
- **OCC**（[arXiv 2604.22383](https://arxiv.org/abs/2604.22383)，摘要明确处理 traffic bursts、APP-limit 和 encoder lag）；
- **BBR-Copilot**（应用受限）；
- 一篇 [微软专利](https://image-ppubs.uspto.gov/dirsearch-public/print/downloadPdf/8553540)：在窗口型拥塞控制上叠加 pacing，承载 RDP 的突发流量 [未核实正文]。

以下是我把这些结论映射到远程桌面后的判断。

**Pudica 的关键机制**：把整帧当作探测 packet train，测量 BUR = (D − D_min)/L。其中：

- D 是首包发送到末包到达的时间；
- D_min 是 10 s 内的最小单向时延（时钟偏差会被抵消）；
- L 是帧间隔。

具体做法：

- **自适应 pacing 倍率**：`ρ = 1.25 / min(R, 1)`。这样帧刚好以略快于瓶颈的速度发出，制造恰好非零的包级排队，使"可感知期"最长。
- **竞争流探测**：在空闲期内均匀插入 4 个无负载探测包。
- **码率调整**：平滑 BUR ≤ 0.85 时做乘性增长，`ξ = 0.3·((α+1)/2 − R)/R`；高于 0.85 时同步做 AI 与 MD（γ_MD=0.05）；短期 BUR > 1 时立即 MD。
- **效果**：平均和尾部帧延迟分别降低 3.1× 和 4.9×，卡顿率降低 10.3×，已服务数百万玩家。

这比 GCC 更适合远程桌面，因为每一帧都是一次测量，不依赖持续流量。

**空闲期的处理原则**：

- 估计值不衰减，置信度衰减。
- RTT 和 D_min 靠心跳保持新鲜。
- 不在空闲时持续发 padding，否则违背"低流量"目标。

**突发巨帧（切窗口、滚动）的处理**：

- 用巨帧本身做探测（即 Pudica 的思路）。以 ρ×C 发送，它就是一次大样本的速率测量，比 padding 探测更省流量。
- 不要以估计带宽"慢慢 pace"一个 500 KB 的帧。宁可把它变成渐进式更新：先发低质量全帧或脏区域，再用后续帧补足质量。

## 5. 接收端抗抖动

**WebRTC 视频抖动缓冲** [已核实，见 [jitter_estimator.cc](https://raw.githubusercontent.com/webrtc-sdk/webrtc/m125_release/modules/video_coding/timing/jitter_estimator.cc)]

- 用 Kalman 滤波拟合"帧间时延差 = θ·帧大小差 + 噪声"。
- 抖动估计 = θ·(P95 最大帧 − 平均帧) + 2.33·σ_noise（扣除 30 ms 偏置后），再加约 10 ms 操作系统抖动。
- 大帧之后紧跟到达的小帧会被剔除（`kCongestionRejectionFactor=-0.25`）。
- 问题：对远程桌面，"最大帧"往往是偶发巨帧，会把整体缓冲撑大。

**NetEQ 思路**：音频用加速和减速（时间伸缩）消化缓冲。视频对应的手段是：

- 播放时间的小幅提前或推后（±几毫秒对齐 vsync）；
- 积压时直接丢掉中间帧，跳到最新帧。

**云游戏实践**：

- Stadia 实测抖动缓冲 35–58 ms（[arXiv 2009.09786](https://arxiv.org/abs/2009.09786)）。
- 有一篇 ACM 论文指出被动 VSync 排队会额外叠加延迟，提出感知驱动的显示调度，瞬态抖动下缓冲延迟降低 43.5%（[DOI 10.1145/3798065.3798067](https://dl.acm.org/doi/10.1145/3798065.3798067)）[仅摘要]。
- Sony 有多项专利讲服务端与客户端 VSYNC 对齐、动态客户端缓冲 [未核实正文]。

**对远程桌面**：主体内容是静态 UI，"平滑"远不如"最新"重要。只有在检测到视频区域或连续动画时，才引入几毫秒到一帧的缓冲。

## 6. QUIC 相关

**quinn 拥塞控制接口** [已核实源码 [congestion.rs](https://github.com/quinn-rs/quinn/blob/main/quinn-proto/src/congestion.rs)、[pacing.rs](https://github.com/quinn-rs/quinn/blob/main/quinn-proto/src/connection/pacing.rs)]

`Controller` 能看到的信息：

- `on_sent(now, bytes, pn)`；
- `on_ack(now, sent, bytes, app_limited, &RttEstimator)`；
- `on_end_acks(in_flight, app_limited, largest_acked)`；
- `on_congestion_event(now, sent, persistent, is_ecn, lost_bytes)`；
- MTU 更新，以及伪丢包回滚。

它**只能输出 `window()`**。pacer 是 quinn 内部的令牌桶，速率按 `1.25 × cwnd / srtt` 推导，唯一的外部速率上限是 `TransportConfig::max_outgoing_bytes_per_second`（静态配置）。`metrics()` 里的 pacing_rate 只用于上报。

由此得出三个限制：

1. 看不到对端接收时间戳，无法做单向时延梯度。ACK 时间戳扩展 [draft-smith-quic-receive-ts](https://datatracker.ietf.org/doc/html/draft-smith-quic-receive-ts)（据 IETF 124 幻灯片已被 WG 采纳）未见 quinn 实现 [未核实]。
2. 看不到帧边界。
3. 无法直接设置 pacing 速率，只能通过 cwnd/srtt 间接控制。

因此结论是：**媒体速率控制在应用层做**。具体做法：

- 每个 datagram 带上发送时间戳和传输序号；
- 接收端每 1 帧或 20–50 ms 回一个反馈 datagram，内容是每包到达时间，格式类似 [RFC 8888](https://www.rfc-editor.org/rfc/rfc8888) 或 TWCC；
- 应用层 pacer 控制 `send_datagram` 的节奏。

**RoQ**（[draft-ietf-avtcore-rtp-over-quic-14](https://datatracker.ietf.org/doc/draft-ietf-avtcore-rtp-over-quic/)）

- 允许 RTP 走 datagram 或流；建议媒体速率控制与 QUIC 拥塞控制协作，可以利用 QUIC 的 ACK 信息。
- 原型 [mengelbart/rtp-over-quic](https://github.com/mengelbart/rtp-over-quic) 在应用层跑 SCReAM/GCC，QUIC 拥塞控制设为 NewReno 或关闭。IETF 邮件列表上也有人质疑双层拥塞控制的必要性。双层控制的风险是两个控制环叠加，QUIC 的 cwnd 可能截断媒体突发。

**MoQ**（[draft-ietf-moq-transport-22](https://www.ietf.org/archive/id/draft-ietf-moq-transport-22.html)）

- 面向发布/订阅和中继扇出，以 Group/Object 映射到 QUIC 流，靠优先级和丢弃旧 group 来控制延迟。
- 对点对点远程桌面而言过重，但"每个 GOP 或每帧一条流，新流优先，旧流 RESET"的思路值得借鉴。

**datagram 与每帧一条流的取舍**

- datagram：没有队头阻塞；丢包要靠自己的 NACK 或 FEC。
- 每帧一条单向流：可靠、实现简单，过期时 `reset` 即可丢弃；但 QUIC 重传会占用拥塞窗口，而且流级重传时机由 QUIC 决定。
- 我的建议：
  - 视频帧用 datagram 加应用层 NACK（RTT 小时）或 FEC（RTT 大时）；
  - 关键的非视频数据（光标形状、剪贴板、文字层元数据）走可靠流。

## 7. 2022–2026 值得关注的工作

| 工作 | 要点 | 核实程度 |
|---|---|---|
| Zhuge（SIGCOMM'22，[PDF](https://chensun-me.github.io/papers/zhuge-sigcomm22.pdf)） | 在 AP 上缩短控制环，尾延迟降低 17–95% | 需改 AP，摘要核实 |
| Tambur（NSDI'23） | 流式纠删码，解码失败减少 26%，冗余带宽减少 35% | 摘要核实 |
| Grace（NSDI'24） | 神经编解码抗丢包 | 摘要核实 |
| Hairpin（NSDI'24） | 冗余与重传联合优化 | 摘要核实 |
| Pudica（NSDI'24） | 见第 4 节 | 原文核实 |
| ACE（SIGCOMM'25） | 见第 3 节 | 仅幻灯片 |
| OCC（arXiv 2026） | 利用物理层信息 | 摘要核实 |
| BBR-Copilot（arXiv 2026） | 见第 2 节 | 摘要核实 |
| Mowgli 等用 GCC 日志离线学习码率控制的工作 | — | 未核实 |

---

## 8. 推荐设计："PeerDesk-FCC"（帧级拥塞控制）

**分层**

- **QUIC 层**：自定义 quinn `Controller`，作"宽松安全网"。
  - `window = max(2 × C_app × srtt + 一个最大帧, 最小窗口)`，正常情况下不阻塞媒体；
  - 遇到持续拥塞（persistent congestion）时才大幅收缩。
  - 不要用 Cubic，它会和应用层争夺控制权。
- **应用层**：估计器、码率控制、pacer、帧调度全部自己实现。

**估计器**（输出容量 C、置信度、队列时延 q）。以下四个信号融合：

1. **帧 BUR**（取自 Pudica）：每帧计算 R = (D − D_min)/L。对远程桌面，L 应取"本帧可用的发送时长"（例如 min(帧间隔, 33 ms)），不能用空闲间隔。
2. **packet train 到达速率**：只有当一帧有 ≥N 个包（建议 N≥8，待标定）且 ρ>1 时，才用"字节 / 到达跨度"计算一个容量样本。巨帧就是天然的探测。
3. **GCC 式 trendline**：在帧内和帧间做时延梯度，作为早期过载报警。阈值增益和 k_up/k_down 先沿用 WebRTC 默认值，再按 Wi-Fi 标定。
4. **丢包**：区分"突发后丢包"与"随机丢包"，参考 loss-based v2 的固有丢包率思路，可先做简化版。

融合方式：

- C = 近期非受限样本的 max-filter（窗口 2–5 s），再受 BUR 修正：BUR>1 时 C 取实测交付速率 × 0.85~0.9；
- q = 单向时延 − D_min；
- 任一信号报告过载，都立即 MD。

**码率与每帧预算**

- 采用 Pudica 式控制：平滑 BUR ≤ α（建议 0.8–0.85）时乘性增长；接近 α 时做 AI-MD；短期 BUR>1 或 q > q_target（建议 15–25 ms）时 MD 到 0.85× 实测速率。
- 每帧预算（取自 Salsify）：`B_frame = C × (T_deadline − q) − inflight_bytes`。其中 T_deadline 是交互目标，建议 50 ms（局域网 16 ms），需标定。预算直接交给编码器的帧级码率控制。
- **编码后不排队**：发送队列最多容纳一帧。新帧到来时，若旧帧还没开始发送就丢弃旧帧；若已在发送，则让它发完，并对屏幕内容按区域合并成脏矩形。
- 如果编码器支持，对大场景变化先编一个低质量版本发出（相当于 Salsify 的两版本选择，或"一次编码多档 QP"）。

**pacing**（取自 ACE 和 Pudica）

- 帧内速率 = ρ × C，`ρ = clamp(1.25 / min(R̄,1), 1.1, 2.0)`。一帧尽量在 L 内发完。
- 帧与帧之间不额外 pace。
- 令牌桶容量随 q 自适应：q 小时允许更大的突发。

**空闲处理**

- 超过 200 ms 没有媒体时，进入 IDLE 状态：冻结 C，置信度随时间指数衰减（半衰期建议 5–10 s，待标定）。
- 每 250–500 ms 发一次 2 包 keepalive pair，维持 RTT 和 D_min，开销约 1 kbps 量级。
- 恢复发送时，首帧以 `C × 0.8 × 置信度修正` 估算预算，同时以 ρ 突发发出作为探测。

**输入驱动的预探测**：用户有键鼠活动时，突发帧即将到来的概率上升。若置信度低，可发一次短 padding 探测簇（15–20 ms，1.5–2× C），代替 WebRTC 的固定周期 ALR 探测。

**突发处理**

- 巨帧（例如超过 4× 平均帧大小）一律走"渐进式"路径：低质量全帧加后续细化。
- 巨帧的到达跨度就是一次带宽测量，测完立即更新 C。
- 光标独立于视频流，本地渲染，再加上远端位置校正。

**丢包恢复**

- RTT < 1/3 截止时间时，用 NACK 重传；
- RTT 较大或在蜂窝网络上，对巨帧加少量 FEC（Tambur 式按帧冗余）；
- 无法恢复时请求 LTR 或局部 intra refresh，避免全量 I 帧。

**接收端抗抖动**

- 默认"到达即解码，下一个 vsync 呈现，积压时只呈现最新帧"。
- 用 present 反馈（如 DXGI frame statistics、Wayland presentation-time）测量"解码完成到上屏"的延迟。
- 只有检测到连续视频区域时，才启用 WebRTC 式 Kalman 抖动估计。估计时要剔除巨帧（用 P95 帧大小而不是最大帧），目标缓冲上限为一帧。

**需要标定的参数**

| 参数 | 来源 | 初始值 |
|---|---|---|
| α（BUR 目标） | Pudica | 0.85 |
| γ_MI | Pudica | 0.3 |
| γ_MD | Pudica | 0.05 |
| ρ 上下限 | Pudica | [1.1, 2.0] |
| q_target | 本设计 | 15–25 ms |
| T_deadline | 本设计 | 50 ms |
| trendline 窗口 | GCC | 20 |
| 阈值增益 | GCC | 4 |
| k_up / k_down | GCC | 0.0087 / 0.039 |
| 置信度半衰期 | 本设计 | 5–10 s |
| 巨帧判定倍数 | 本设计 | 4× 平均帧 |
| 有效 train 最小包数 | 本设计 | 8 |
| keepalive 周期 | 本设计 | 250–500 ms |

建议用 mahimahi 或 tc-netem 做 trace 回放，覆盖以下场景，并做 A/B 对比：

- Wi-Fi 聚合；
- LTE/5G trace；
- 与 Cubic、BBR 竞争；
- 突发切换场景。

**AI 模型可以插入的位置**

1. 估计器的残差修正：输入特征，输出 C 的乘性修正。
2. 预测下一帧大小和突发概率（结合输入事件和内容类型），用于预探测和预算。
3. 抖动缓冲目标值。

**经典 fallback 必须保证的底线**

1. 模型输出被限制在经典估计的 [0.7, 1.3] 倍内。
2. 过载信号（q > q_max、BUR > 1、突发丢包）触发的 MD 永远优先，模型不能否决。
3. 发送速率 ≤ 1.5 × 最近 1 s 的实测交付速率。
4. 模型超时或输出异常时，立即切换到纯经典路径。
5. 竞争 TCP 时不低于经典算法的公平份额。

**仍然不确定的地方**

- Pudica 是在云游戏这种持续帧流上验证的。在长时间空闲加偶发巨帧的模式下，BUR 的样本稀疏性会怎样，我没有找到数据，需要自己实测。
- ACE 的细节以论文为准。
- quinn 的 datagram 发送缓冲满时的丢弃策略需要阅读源码确认。
