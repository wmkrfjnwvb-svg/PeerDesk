# 实时视频网络仿真：业内做法、数据集与出处

> 2026-11-28 补充：G.1050、BLADE、Starlink、FCC 几处结论已在 `industry-network-types.md` 第 9 节更正，以那里为准。

整理日期 2026-11-27。只收业内做法和原文出处，不评价 PeerDesk 现有代码。
标记约定：【已核实】= 本次直接读到原文或源码；【未核实】= 只有二手转述或凭记忆。
引文均为原文英文，不翻译，避免走样。

本次在本机落盘的东西（都在 scratchpad 下）：
- `/tmp/claude-0/-home-user-peerdesk/a4149fae-328b-5bde-8281-bf0aeca8fdf1/scratchpad/pdfs/`：Pantheon、Mahimahi、Salsify、Puffer、SC-PAQ 的论文 PDF 和转出的文本，以及 Mahimahi 的 `linkshell.cc`（共约 6 MB）。
- `/tmp/claude-0/-home-user-peerdesk/a4149fae-328b-5bde-8281-bf0aeca8fdf1/scratchpad/msdata/`：微软 MMSys 2024 仿真数据集 chunk_2.zip 的末尾 3 MB（读目录用）、其中一个样本 `sample_emul.json`（6.7 MB，解压后），以及 `one.bin`（该样本的压缩原文，0.6 MB）。总共不到 15 MB。没有下载任何整包数据集。

---

## 0. 先给结论（给设计量尺的人）

1. 业内的"仿真可信"不是靠模型细致，而是靠"拿真实链路和仿真各跑一遍，比结果"。Pantheon 只用 5 个参数的仿真器，让各算法的吞吐和延迟与真实路径平均差 17%；并且明确说"模型越细越容易过拟合"。
2. 业内测实时视频拥塞控制的标准做法有三层：
   - 底层：IETF RFC 8867/8868/8869 的测试矩阵（固定模板，参数写死）。
   - 中层：WebRTC 自己的 `goog_cc_network_control_unittest.cc` 场景单测（场景少、断言宽，如 450±100 kbps）。
   - 上层：Pantheon / Salsify / Puffer 这类"真实轨迹回放 + 多次重复 + 置信区间"。
3. 轨迹回放（Mahimahi 式）只模拟"链路容量随时间变化"；缓冲区深度、AQM、丢包、抖动、乱序要另外叠加。业内公开轨迹大多只有吞吐，没有同步的丢包和排队真值。微软仿真数据集是少数同时带"真实容量 + 丢包率真值"的。
4. 统计方面：Puffer 论文量化过，现实网络上要在统计上分出 ±10–17%（95% 置信区间宽度）的差异，需要约 1.75 年的流数据；这是"量尺要先分得出好坏"的业内旁证。

---

## 1. 业内网络仿真工具和方法

### 1.1 Mahimahi（MIT，USENIX ATC 2015）

要点：
- 一组可嵌套的 shell：`mm-delay`（固定单向时延）、`mm-loss`（独立同分布丢包）、`mm-onoff`（按平均开/关时间断网）、`mm-intermittent`、`mm-link`（轨迹驱动链路）、`mm-meter`（实时画图）、`mm-webrecord/replay`。【已核实】
  出处：https://raw.githubusercontent.com/ravinet/mahimahi/master/src/frontend/Makefile.am
- `mm-loss` 用法只有 `uplink|downlink RATE`，丢包率 0–1，是独立同分布，没有突发。`mm-onoff` 用法 `uplink|downlink MEAN-ON-TIME MEAN-OFF-TIME`。【已核实】
  出处：https://raw.githubusercontent.com/ravinet/mahimahi/master/src/frontend/lossshell.cc 、 `.../onoffshell.cc`
- 链路轨迹格式：每行一个毫秒时间戳，表示"这一毫秒可以放行一个 1500 字节的包"；同一毫秒可重复多行表示更高速率；一行 `1` 的文件就是 12 Mbps 固定链路；到末尾自动循环。
  引文："Each line in the trace is a packet-delivery opportunity: the time at which an MTU-sized packet will be delivered in the emulation." "Delivery opportunities are wasted if bytes are unavailable at the instant of the opportunity."【已核实】
  出处：论文 https://files.usenix.org/s3fs-public/cdn/atc15-paper-netravali.pdf §3.4；trace 目录说明 https://raw.githubusercontent.com/ravinet/mahimahi/master/traces/README
- 包级队列和时延：LinkShell 里每个方向一个包队列，进来的包直接入队，按轨迹出队；"Per-packet queuing delay is computed as the time each packet remains in LinkShell's uplink or downlink queues."【已核实】
- 队列策略可选：`--uplink-queue=QUEUE_TYPE`，QUEUE_TYPE = `infinite | droptail | drophead | codel | pie`，参数用 `--uplink-queue-args="packets=...,bytes=..."` 这类形式（参数名细节未逐个核实）。默认是 infinite。【已核实：类型列表和默认值；参数名【未核实】】
  出处：https://raw.githubusercontent.com/ravinet/mahimahi/master/src/frontend/linkshell.cc
- 与 netem / dummynet 的区别（论文原话）："Unlike dummynet and netem, LinkShell can emulate variable-rate cellular links, in addition to static link rates, because it runs over packet-delivery traces."【已核实】
- 精度：论文报告的是网页加载场景，用 ReplayShell + 延迟/链路 shell 重放 vs 真实互联网，中位误差 12.4%（对比对照工具 36.7%）。这是网页，不是实时视频，不能直接当实时视频的精度。【已核实：数字出自 §5 文本；上下文细节只读到片段】
- 许可：Mahimahi 代码是 GPLv3（`COPYING` 为 GPL v3）。PeerDesk 是 Apache-2.0，不能引入其代码；借用的只能是"轨迹格式"这个想法（格式本身是一行一个毫秒数，很简单）。【已核实】
  出处：https://raw.githubusercontent.com/ravinet/mahimahi/master/COPYING
- 自带的轨迹（Verizon LTE、AT&T LTE、T-Mobile UMTS 等）来自 Sprout 论文的 Saturator 工具录制，描述为"time-varying capacity of U.S. cellular networks as experienced by a mobile user"。轨迹本身的单独许可证未写明。【已核实：描述；许可证【未核实】】

可借鉴：
- 用"每毫秒放行机会"表示容量，而不是"每 100 ms 的速率值"：粒度到毫秒，且天然带出"放行机会作废"（队列空时机会浪费）这个现象，能同时表示稀疏小包和突发。
- 容量（轨迹）、固定时延、丢包、断网、队列策略是互相独立、可嵌套的几个层，这样每层可以单独校准。
- 内建 CoDel/PIE/DropHead 作为队列的可选项，对比"深缓冲 vs AQM"。

### 1.2 Pantheon（Stanford，USENIX ATC 2018）

要点：
- 目标：一个共享的拥塞控制测试场，有"校准过的仿真器"和"连续测试真实互联网"两部分。
  引文："calibrated network emulators that reproduce real Internet path behavior for reproducible experiments"（摘要）【已核实】
  出处：https://www.usenix.org/conference/atc18/presentation/yan-francis ，PDF https://www.usenix.org/system/files/conference/atc18/atc18-yan-francis.pdf
- 5 个参数的仿真模型（§5.1 原文）：
  1. bottleneck link rate；2. constant propagation delay；3. DropTail threshold for the sender's queue；4. stochastic loss rate (per-packet, i.i.d.)；5. "a bit that selects whether the link runs isochronously (all interarrival times equal), or with packet deliveries governed by a memoryless Poisson point process, characteristic of the observed behavior of some LTE networks"。【已核实】
- 核心判断（原文）："We found that a five-parameter network model is sufficient to produce emulators that approximate a diverse variety of real paths, matching the throughput and delay of a range of algorithms to within 17% on average." 以及反对"越细越好"："more highly-parameterized network models create a risk of overfitting—but may be justified if lower-parameter models cannot achieve sufficient accuracy."【已核实】
- 校准方法：给定某条真实路径上若干种算法的结果，用非线性（贝叶斯）优化搜 <速率, 传播时延, 队列大小, 丢包率>，目标函数是"仿真 vs 真实 的平均吞吐和平均 95 分位时延的百分比差，在一组参考算法上取平均"。每次评估：所有算法跑 30 秒脚本场景 3 遍并行，约 30 秒墙钟；一条路径约 2 小时、30 台 4 核机器。【已核实】
- 判据（"figure of merit"）：仿真器能不能在整个系统里（包括端点算法）代替真实路径而不改变整体性能；评估要覆盖多种算法，"each new algorithm serves as a novel probe that could put the network into an edge case"。【已核实】
- 留一验证：用 n-1 个算法校准的仿真器，预测没见过的算法，误差约 20%。【已核实，脚注 5】
- 每个参数的边际贡献（图 5，中国有线路径）：只有速率 211.8 → 加时延 189.7 → 加缓冲大小 32.3 → 加随机丢包 17.7；哥伦比亚蜂窝路径需要"恒定 → Poisson 放行"才从 23.7 降到 13.0。可见缓冲大小对复现影响最大，蜂窝需要放行抖动。【已核实】
- 局限（原文）："Our current emulators replicate throughput and delay metrics only within 17% accuracy on average. An open question is whether we can improve emulator fidelity—especially on cellular paths"。有线路径仿真比含无线的更准（图 3 说明）。【已核实】
- 数据：每条结果提供性能 JSON、原始日志 tar.gz、每路径对比图。站点已停止维护，最新结果 2020-04-17。许可证网页未写明。【已核实】
  出处：https://pantheon.stanford.edu/
- 路径：美国、墨西哥、巴西、哥伦比亚、印度、中国的蜂窝和有线，加伦敦、爱荷华、东京、悉尼。【已核实】
- 合成训练仿真：Indigo 用 24 个合成仿真器（5,10,20,50,100,200 Mbps 等组合）训练，对比用校准仿真器训练，后者更好。【已核实：片段】

可借鉴：
- "仿真能不能代替真实"的度量就是：同一批算法在真实 vs 仿真里的吞吐/延迟差，而不是仿真自己看起来多真。
- 先用最少参数，每加一个参数都要证明误差降了多少（图 5 的做法）。
- 蜂窝用 Poisson 放行而不是恒定放行，是低成本模拟"放行抖动"的办法。

### 1.3 WebRTC 内置网络仿真（Google）

位置：`api/test/simulated_network.h`、`api/test/network_emulation_manager.h`、`test/network/`、`test/scenario/`。

要点（均已核实，读的是 main 分支源码）：
- `BuiltInNetworkBehaviorConfig` 字段：`queue_length_packets`（队列长度，包数）、`queue_delay_ms`（"Delay in addition to capacity induced delay"）、`delay_standard_deviation_ms`（额外时延标准差）、`link_capacity`、`loss_percent`（"Random packet loss, range 0 to 100"）、`allow_reordering`、`avg_burst_loss_length`（"The average length of a burst of lost packets"，默认 -1 即不突发）、`packet_overhead`、`forward_ecn`。
  出处：https://webrtc.googlesource.com/src/+/refs/heads/main/api/test/simulated_network.h
- 实现（`test/network/simulated_network.cc`）：
  - 突发丢包是两态（Gilbert-Elliott 式）：`prob_loss_bursting = 1 - 1/avg_burst_loss_length`，`prob_start_bursting = prob_loss/(1-prob_loss)/avg_burst_loss_length`；`avg_burst_loss_length == -1` 时退化为独立丢包。
  - 抖动：额外时延 = 高斯(均值 queue_delay_ms，标准差 delay_standard_deviation_ms)，截到 0；`allow_reordering=false` 时调整使包不乱序，为 true 时按到达时间稳定排序。
  - 容量：按字节串行化，传输时间 = bits/capacity（向上取整到微秒）；容量 0 表示包永不到达；暂停传输建模为额外时延。
  - 队列：默认 `LeakyBucketNetworkQueue`，长度以包数计，满了就丢。没有按字节或按时延限长，也没有 AQM。
  - ECN 只有透传，没有标记 CE。
  出处：https://webrtc.googlesource.com/src/+/refs/heads/main/test/network/simulated_network.cc
- `NetworkEmulationManager`：建节点（`CreateEmulatedNode`，节点由 delay/capacity/loss/队列长度配置）、单向路由（"This route is single direction only."）、`CreateTcpRoute`（"This can be used to emulate HTTP cross traffic."）、`StartCrossTraffic`、统计（`kDebug` 模式收每包统计）。
  出处：https://webrtc.googlesource.com/src/+/refs/heads/main/api/test/network_emulation_manager.h
- 交叉流量生成器（`test/network/cross_traffic.h` 和 `api/test/network_emulation/cross_traffic.h`）：
  - `RandomWalkCrossTraffic`："Generated traffic rises and falls randomly."，字段 `random_seed, peak_rate, min_packet_size, min_packet_interval, update_interval, variance, bias`；
  - `PulsedPeaksCrossTraffic`："Generated traffic has form of periodic peaks alternating with periods of silence."，字段 `peak_rate, min_packet_size, min_packet_interval, send_duration, hold_duration`；
  - `FakeTcpCrossTraffic`（简化 TCP，字段 `packet_size, send_limit, process_interval, packet_timeout`）。
  出处：https://webrtc.googlesource.com/src/+/refs/heads/main/api/test/network_emulation/cross_traffic.h
- GoogCC 单测（`goog_cc_network_control_unittest.cc`）的场景和断言风格，见 §5.3。
- `test/scenario/` 有 29 个文件（scenario、video_stream、stats_collection、performance_stats、probing_test 等），是"场景 + 统计"框架；文件名列表【已核实】，各文件职责只是按名字推断【未核实】。
  出处：https://webrtc.googlesource.com/src/+/refs/heads/main/test/scenario/

可借鉴：
- 突发丢包用"平均突发长度 + 总丢包率"两个参数反推两态转移概率，比直接调转移概率更直观，且参数有下限检查（突发长度必须大到能达到给定丢包率）。
- 抖动用"单独的高斯时延 + 是否允许乱序"，把抖动和乱序拆成两个开关。
- 交叉流量分三类：随机游走（背景波动）、脉冲（周期性突发）、假 TCP（会随链路自适应）。
- 缺的东西：AQM、按字节队列、容量随时间变化的轨迹（容量变化要靠测试代码在运行中改 config）、Wi-Fi 聚合。【已核实：源码里没有这些；"靠测试代码运行中改 config"是从 GoogCC 单测的写法推断【未核实】】

### 1.4 OpenNetLab / AlphaRTC / MMSys 2021 挑战赛

要点：
- AlphaRTC 是 Google WebRTC 的分支，用 PyInfer / ONNXInfer 两个可定制接口替换 GCC；仓库 BSD-3-Clause 许可。【已核实】
  出处：https://github.com/OpenNetLab/AlphaRTC
- OpenNetLab `gym`：用 NS-3 + WebRTC 建的训练/评测环境，Python 接口 `gym.reset(trace_path, report_interval_ms=60, duration_time_ms=3000)`，`step(bandwidth_bps)`。
  出处：https://github.com/OpenNetLab/gym 、源码 https://raw.githubusercontent.com/OpenNetLab/gym/master/alphartc_gym/gym.py 【已核实】
- 轨迹格式是 JSON 分段常量，每段 `{duration(ms), capacity(kbps), loss(0.0~0.1), rtt(ms), jitter(当前版本不支持)}`，区分 uplink/downlink。引文（源码注释）：`"capacity": 300, # link bandwidth (kbps)`, `"loss": 0.1, # loss rate (0.0~0.1)`, `"rtt": 85`, `"jitter": 0, # not supported in current version`。【已核实】
- 2021 年挑战赛的具体轨迹集大小、来源、许可：没有查到一手页面。二手信息（社区帖子）说组织方提供有限数量的轨迹，后续补充。【未核实】
- 论文：APNet 2022 的 OpenNetLab 论文 https://fyy.cs.illinois.edu/files/opennetlab-apnet22.pdf（仅在搜索结果中见到，未读）【未核实】

可借鉴：
- 分段常量 JSON（容量、丢包、RTT、持续时间）是最简单的、跨工具的"场景描述"，容易人手写和生成。
- 汇报间隔 60 ms 与微软 2024 数据集的短期监控间隔 60 ms 一致。

### 1.5 微软 MMSys 2024 带宽估计挑战赛的仿真部分（详见 §3.1）

只在此处点出：它的"仿真"是"两台机器之间用网络仿真软件，随机变化瓶颈链路的容量和丢包率"，仿真软件没有点名，没有给容量范围、时延、队列大小、变化间隔。【已核实】

### 1.6 Salsify（NSDI 2018）的评测方法

要点：
- 测试台：用 Mahimahi 的 AT&T LTE、T-Mobile UMTS、Verizon LTE 轨迹，另加合成的"间歇链路"（12 Mbps，平均每 5 秒（指数分布）出一次故障，故障期间全部丢包，平均 0.2 秒后恢复）和一条仿真 Wi-Fi 恒速链路；队列用 DropTail，阈值 64 包等多档对比，其余测试 256 包缓冲，蜂窝轨迹往返时延 40 ms。
  引文："The link's capacity is 12 Mbps with no drops until a "failure" arrives, which happens on average after five seconds (exponentially distributed). During failure, all packets are dropped until a "restore" event arrives, on average after 0.2 seconds"。【已核实】
- 每次实验 10 分钟、60 fps 1280×720 的真实视频（三人的一段对话），逐帧加条形码，用硬件时钟给发送和接收帧打时间戳，度量 SSIM 和端到端时延的 95 分位；先做空载回环校准测试台自身时延。【已核实】
- 把 AT&T 和 T-Mobile 轨迹留作"没用来调参的"测试集。【已核实：原文说 held out】
  出处：https://www.usenix.org/system/files/conference/nsdi18/nsdi18-fouladi.pdf §4–5

可借鉴：
- 间歇链路（指数分布的开关）是一个只有两个参数的"断网/切换"模型，可用来覆盖"瞬断"。
- 把一部分轨迹留出不参与调参。
- 用硬件/外部时钟度量端到端，而不是用被测系统自己的计时（对我们是"仿真内部指标是否可信"的参考）。

### 1.7 Puffer（Stanford，NSDI 2020）

要点：
- 真实部署：2019 年起向真实用户直播电视，随机对照实验比较各种 ABR 算法。论文：14.2 年视频、56,000 用户。
- 对仿真的立场（原文）："Accurately simulating or emulating the diversity of Internet paths is challenging"；"algorithms trained in emulated environments"在真实网络上表现不及仿真里。【已核实：片段；精确引文在摘要与引言】
- 对比它们自己搭的仿真：本地 Puffer 服务器 + 无头 Chrome 放在 Mahimahi shell 里，每个 shell 加 40 ms 端到端时延，下行容量按 FCC 宽带轨迹限制，上行统一 12 Mbps，12 个客户端并发自动重复。【已核实】
  出处：https://arxiv.org/pdf/1906.01113
- 数据：每天一组 CSV（`video_sent`、`video_acked`、`client_buffer`、`video_size`、`ssim`），含 `cwnd, in_flight, min_rtt, rtt, delivery_rate, size, buffer, cum_rebuf` 等；一天数 GB；2019-01 至今每日更新；已匿名；许可证在数据页没写。【已核实】
  出处：https://puffer.stanford.edu/data-description/
- 统计：用 bootstrap 模拟按真实流的经验分布重抽样，给出置信区间；时长加权的标准误；结论是"1.75 年数据下，卡顿比例的 95% 置信区间宽度约为均值的 ±10%–±17%"。【已核实】

### 1.8 Linux tc / netem / fq_codel

- netem 能力（`tc-netem(8)`）：【已核实】
  - 延迟 + 抖动 + 相关系数；分布：uniform、normal、pareto、paretonormal。
  - 丢包三种：随机（可带相关）、`state`（"Use a 4-state Markov chain to describe packet loss"）、`gemodel`（"Use a Gilbert-Elliot (burst loss) model"，参数是进入坏状态概率、退出坏状态概率、坏/好状态的丢包概率）。
  - 乱序（`reorder`，按百分比或固定间隔）、重复、损坏；`ecn`（用标记代替丢弃，需要丢包模型）；`rate`（按包大小算延迟模拟定速链路，可加每包/每信元开销）；`limit`（队列包数上限）；`seed`（让随机丢包可复现）。
  - `slot`（"allows emulating slotted networks"，包被攒到时隙里统一放行，可设最小/最大时隙时间、每时隙最多包数/字节数）：手册说它近似 DOCSIS、WiFi、LTE 这类成批放行的 MAC。这是最接近"Wi-Fi 聚合造成的批量放行"的现成原语。
  出处：https://man7.org/linux/man-pages/man8/tc-netem.8.html
- fq_codel 默认值：target 5 ms、interval 100 ms、limit 10240 包、flows 1024、quantum 1514、ecn 默认开。【已核实】
  出处：https://man7.org/linux/man-pages/man8/tc-fq_codel.8.html
- 代价：netem 在真实内核里跑，吞吐高了会受内核定时器粒度影响；不能直接回放变速率轨迹（要靠脚本周期性改 `rate`）。这两条是常识性说法，本次没有一手来源【未核实】。

### 1.9 ns-3

- Wi-Fi 模型：支持 802.11a/b/g/n/ac/ax/be，含 802.11e QoS、A-MPDU 和 A-MSDU 聚合（可两级叠加）、Block Ack、多种速率自适应（Aarf、Arf、Cara、Onoe、Rraa、ConstantRate、Minstrel、Minstrel-HT）；默认 YansWifiPhy 是"按包、无频率选择性传播、无干扰"，要建干扰要用 SpectrumWifiPhy；错误率模型只有 AWGN；不支持波束成形；处理时延不建模。
  出处：https://www.nsnam.org/docs/models/html/wifi-design.html 【已核实】
- RFC 8869 建议的 LTE 场景就是用 ns-3 LTE 仿真（见 §5.2）。
- 代价：ns-3 是离散事件仿真，Wi-Fi/LTE 场景要建 21 小区、几十用户，单次几分钟到几十分钟，不适合 CI 内联；适合离线"生成轨迹"再回放（OpenNetLab 的 gym 就是把 ns-3 包起来用）。运行时间数字没有一手来源【未核实】。

### 1.10 各家对比

| 能力 | Mahimahi | netem | WebRTC SimulatedNetwork | Pantheon(基于 Mahimahi) | ns-3 | OpenNetLab gym |
|---|---|---|---|---|---|---|
| 容量随时间变化 | 轨迹（毫秒放行） | 要脚本改 rate | 测试代码改 config | 常量或 Poisson 放行 | 可建模 | JSON 分段常量 |
| 队列深度/策略 | infinite/droptail/drophead/codel/pie | limit（包数）；可叠 fq_codel | 仅包数队列 | DropTail 包数 | 任意 | 未核实 |
| 随机丢包 | mm-loss（独立同分布） | 随机/4态/gemodel | 独立或突发（平均突发长度） | 独立同分布 | 任意 | 单个 loss 值 |
| 抖动/乱序 | 无（固定时延） | 有，含分布 | 高斯抖动 + 可乱序 | 无 | 任意 | 抖动未支持 |
| 断网 | mm-onoff / intermittent | 100% 丢包 | 容量 0 | 无 | 可建 | capacity 置 0（推断） |
| 交叉流量 | 需另起流 | 需另起流 | 内建三类生成器 | 多流实验 | 任意 | 无 |
| Wi-Fi 聚合/空口竞争 | 无 | slot 近似 | 无 | 无 | A-MPDU、竞争都有 | 无 |
| 确定性/可复现 | 轨迹确定；内核调度有噪声 | seed 可复现丢包 | 种子+虚拟时间，确定 | 校准后可复现 | 种子确定 | 种子 |

---

## 2. 网络类型怎么建模

没有找到"一份公开的、被业内公认的网络类型分类体系"。能找到的是下面四类"事实上的清单"：

1. IETF RMCAT：RFC 8867/8868/8869，按"有线瓶颈 + 变速/竞争流"和"蜂窝/Wi-Fi 仿真"组织（§5）。
2. ITU-T G.1050：IP 网络模型，按场景给出时延、抖动、丢包的统计分布，2016 版改为离散事件仿真，加入链路故障、路由抖动、乱序；"snapshot of actual network data provided by anonymous IP service providers ... in the 2008 timeframe"，且明确"不代表任何具体网络"。
  出处：https://www.itu.int/ITU-T/recommendations/rec.aspx?rec=11074（以及搜索结果摘要页）；全文未读，只读到摘要【部分核实：摘要；参数表【未核实】】
3. 工具预设：WebPageTest 的连接档位、Chrome DevTools 节流预设、Apple Network Link Conditioner 预设（见下表）。
4. 学术数据集按"交通方式"分类（挪威 3G、比利时 4G），Pantheon 按"有线/蜂窝 + 城市"分类。

### 2.1 工具预设里的数值（可当"类型参数的初值"，注意它们都是为网页加载设计的，不含抖动和突发丢包）

WebPageTest `connectivity.ini.sample`【已核实】，出处 https://raw.githubusercontent.com/WPO-Foundation/webpagetest/master/www/settings/connectivity.ini.sample ：

| 档位 | 下行 | 上行 | 延迟(RTT) | 丢包 |
|---|---|---|---|---|
| FIOS | 20 Mbps | 5 Mbps | 4 ms | 0 |
| Cable | 5 Mbps | 1 Mbps | 28 ms | 0 |
| DSL | 1.5 Mbps | 384 kbps | 50 ms | 0 |
| 4G | 9 Mbps | 9 Mbps | 170 ms | 未设 |
| LTE | 12 Mbps | 12 Mbps | 70 ms | 0 |
| 3G | 1.6 Mbps | 768 kbps | 300 ms | 0 |
| 3GFast | 1.6 Mbps | 768 kbps | 150 ms | 0 |
| 3GSlow | 400 kbps | 400 kbps | 400 ms | 0 |
| Edge | 240 kbps | 200 kbps | 840 ms | 0 |
| 2G | 280 kbps | 256 kbps | 800 ms | 0 |

Chrome DevTools（2026 年 main 分支，注释原文）【已核实】，出处 https://raw.githubusercontent.com/ChromeDevTools/devtools-frontend/main/front_end/core/sdk/NetworkManager.ts ：
- Slow 3G：~500 kbps 下/上，400 ms RTT；Slow 4G：~1.6 Mbps 下、~0.75 Mbps 上，150 ms RTT；Fast 4G：9 Mbps 下、1.5 Mbps 上，60 ms RTT。原文注："Note that for the download, upload and RTT values we multiply them by adjustment factors to make DevTools' emulation more accurate."

Apple Network Link Conditioner 预设名：100% Loss、3G、DSL、EDGE、High Latency DNS、LTE、Very Bad Network、WiFi、WiFi 802.11ac。每个方向可设带宽、丢包率、延迟。具体数值没有从苹果官方文档读到，只有第三方转述。【预设名已由多个二手来源一致；数值【未核实】】
出处：https://nshipster.com/network-link-conditioner/

### 2.2 RFC 8868 给的"基础网络参数"（实时视频测试的通用取值）【已核实】

出处：https://www.rfc-editor.org/rfc/rfc8868.html
- 单向时延：0–1 ms、50 ms、150 ms、300 ms。
- 端到端丢包：0%、1%、5%、10%、20%；"Independent random losses must be used in the evaluation"是底线，相关性丢包建议用 Gilbert-Elliott。
- 丢尾队列长度：约 70 ms（QoS 感知）、300–500 ms（常规）、1000–2000 ms（缓冲膨胀）；AQM 鼓励但可选。
- 抖动：NR-BPDV 模型"must be used"，截断高斯，标准差 5 ms，N_STD=3，不允许同一流内乱序。
- RFC 8867 默认值：瓶颈 1 Mbps，单向时延 50 ms，最大端到端抖动 30 ms，队列 300 ms 丢尾，路径丢包 0%。

### 2.3 分类型的建模手段与参数（见 §7 汇总表）

要点摘录：
- 家庭宽带 + 其他设备下载：业内用"交叉流量"建模。RFC 8868 的 TCP 流量模型：长连接（无限数据）、短连接（30 条连接各取 30–50 KB，空闲时间指数分布均值 10 s）、背景 UDP（恒速、1500 字节包）；WebRTC 的 `FakeTcpCrossTraffic`、`PulsedPeaksCrossTraffic`、`RandomWalkCrossTraffic` 是对应实现。RFC 8867 §5.6 要求"实时流对抗一条长 TCP 流，队列 300–1000 ms"。
- Wi-Fi：RFC 8869 §3 用 ns-3，802.11n MCS 11（52 Mbps 原始速率，应用层约 20 Mbps），1–20 条流竞争，背景 CBR 开/关（0–60 s 开、61–120 s 关或反之），背景 TCP 在 40–80 s；对数距离路径损耗；承认应升级到 802.11ac/ax。
- 蜂窝：RFC 8869 §2 用 ns-3 LTE，3GPP case 1（21 小区、半径 166.67 m、10 MHz）和 case 3（半径 577.33 m，信号差），用户移动 3 km/h 和 30 km/h，背景 web/FTP TCP；明确"没有专门的切换测试用例，只指出切换会引起丢包、延迟或抖动突发，默认尽力承载通常排队而不是丢包"。
  出处：https://www.rfc-editor.org/rfc/rfc8869.html
- 卫星（Starlink）：见 §3 数据集；关键机理是全球同步的 15 秒重配置周期（见 §3.9）。
- 跨洋高延迟：RFC 8868 的 150 ms/300 ms 单向时延档；RFC 8867 §5.5 的 RTT 公平性测试用 10/25/50/100/150 ms 五档；Pantheon 的真实路径（东京–悉尼、伦敦–爱荷华）。
- 企业网、酒店和咖啡馆网络：没有找到业内公开的专门模型或数据集。常见做法是"共享 Wi-Fi + 交叉流量 + 策略限速（policer）"的组合。微软挑战赛论文提到仿真含 "burst loss, traffic policing, and bandwidth fluctuations"（见 §3.1），policer 是其中一类。【酒店/咖啡馆/企业专项：未查到】

---

## 3. 公开网络轨迹数据集

### 3.1 微软 MMSys 2024 带宽估计挑战赛（Khairy 等）

出处：
- 仓库：https://github.com/microsoft/RL4BandwidthEstimationChallenge
- 论文：https://arxiv.org/abs/2403.06324 （HTML https://arxiv.org/html/2403.06324v2）
- 数据页：https://www.microsoft.com/en-us/research/academic-program/bandwidth-estimation-challenge/data/

内容（均已核实）：
- 两部分：测试床（真实互联网）和仿真（emulated）。2023-10-20 发布。
- 测试床：18,859 通 P2P 的 Teams 音视频电话，节点分布在多国多洲，经不同 ISP 的有线或无线接入；6 种行为策略（含 Kalman 滤波估计器、WebRTC、若干 ML 策略）。没有带宽真值。
- 仿真：9,405 通测试电话，两台机器间用网络仿真软件（未点名）模拟"burst loss, traffic policing, and bandwidth fluctuations"；瓶颈链路的容量和丢包率"are randomly varied throughout the duration of the test call"。有真值：`true_capacity`、`true_loss_rate`。真值不允许当模型输入（"Any model which has ground truth information in the inputs will be disqualified"）。
- 评测：第一阶段仿真评测 160 通 22 分钟 P2P 电话，16 种网络场景，五组：固定低带宽、固定高带宽、变化带宽、突发丢包、变化突发丢包；决赛在测试床上跑 600 通 3 分钟电话，12 天（2024-02-14 至 02-26）。
- 观测向量：150 维 = 15 个特征 × （5 个 60 ms 短期监控间隔 + 5 个 600 ms 长期监控间隔）。15 个特征：接收速率、收包数、收字节、排队时延、时延、最小时延、时延比、时延均值与最小值差、包间隔、抖动、丢包率、平均丢包数、视频包比例、音频包比例、探测包比例。
- 标签：客观音频/视频质量（0–5，由与 ITU-T P.808 / P.910 主观分高度相关的 ML 模型预测），带宽估计 `b_n`。

文件结构（本次实测，1 个样本，已核实）：
- 下载脚本只列了 Azure blob：`https://dnschallengepublic.blob.core.windows.net/bwechallenge-2023/`，仿真 `emulated_dataset/emulated_dataset_chunk_{0,1,2}.zip`，测试床 `testbed_dataset/testbed_dataset_chunk_{0..4}.zip`。脚本里的下载命令默认是注释掉的，只发 HEAD。
- 包大小（HEAD 请求的 Content-Length）：仿真 3.54 GB + 3.41 GB + 1.14 GB，约 8.1 GB；测试床 7.52 + 7.47 + 7.57 + 7.54 + 5.39 GB，约 35.5 GB。
- 我读了 chunk_2 的 zip 目录：1,405 个文件，名字形如 `09177.json`，每个压缩后约 0.8 MB，解压约 6–8 MB。
- 一个样本 JSON（`sample_emul.json`，已落盘）的键：`policy_id`（如 "v0"）、`observations`（1946 × 150）、`bandwidth_predictions`、`video_quality`（有大量 NaN，1946 中 1735 个非 NaN）、`audio_quality`、`true_capacity`、`true_loss_rate`。这个样本是固定低带宽场景：`true_capacity` 全程 161000（即 161 kbps），`true_loss_rate` 全程 0。因此一个样本不能代表全部 16 种场景，别的场景的变化间隔、范围仍未知。
- 每步的时间长度：数据页和论文都没写；由样本 1946 步和"22 分钟"不能直接推出（含 NaN、可能丢弃部分）【未核实】。调用方说"每 100 ms"，请以实际文件的时间戳为准【未核实】。
- 许可证：数据没有单独许可证，只有 "MICROSOFT PROVIDES THE DATASETS ON AN "AS IS" BASIS. ..."；仓库代码 MIT，文档 CC BY 4.0；论文要求使用者引用 Khairy 等。是否允许商用：未写明，需向微软确认。【已核实：文本；商用结论【未核实】】
  出处：https://raw.githubusercontent.com/microsoft/RL4BandwidthEstimationChallenge/main/README.md "Dataset licenses" 一节
- 对"仿真 = 带真值的链路轨迹"的意义：这是目前查到的唯一同时公开"链路容量真值 + 丢包率真值 + 应用侧逐包观测"的实时通话数据；但真值只有容量和丢包，没有时延/队列/抖动真值，且数据来自音视频通话负载，不是屏幕共享。

### 3.2 MMSys 2021 OpenNetLab 挑战赛

- 数据：轨迹格式见 §1.4（JSON 分段常量）；规模、来源、许可没有查到一手页面。【未核实】
- 环境：NS-3 + AlphaRTC 的 gym。【已核实】

### 3.3 FCC Measuring Broadband America（MBA）

- FCC 自 2023-08-01 起不再与 SamKnows 合作做固定宽带数据采集，改为内部继续。【搜索摘要，出处 https://www.fcc.gov/page/measuring-broadband-america-open-methodology，页面本身被 403 挡住，未读原文】【部分核实】
- 原始数据按季度/月度发布，页面在 https://www.fcc.gov/oet/mba/raw-data-releases 和各期 "raw data" 页（本机访问均 403，未读到文件清单和许可）。FCC 页面声明原始数据"未经审计，不适合用于比较 ISP"（搜索摘要转述）。【未核实】
- 测量内容（来自技术附录目录）：UDP 时延和丢包、ICMP 时延、下载/上传吞吐等。移动端 App 的 UDP 时延定义："average round-trip time in milliseconds of up to 60 UDP data packets that are acknowledged as received within 2 seconds or are recorded as lost"。【转述】
  出处：https://data.fcc.gov/download/measuring-broadband-america/2023/Technical-Appendix-fixed-2023.pdf
- 业内用法：Pensieve 和 Puffer 把 FCC 宽带吞吐记录处理成固定间隔的吞吐轨迹再用 Mahimahi 回放（Puffer 论文"limited the downlink capacity over time to match the capacity recorded in a set of FCC broadband network traces"）。【已核实：Puffer 一句；Pensieve 的具体处理未能读到】
- 粒度：FCC 的吞吐测试是周期性主动测试，不是连续轨迹；用在轨迹里需要拼接/插值。这个判断来自 Pensieve 一类工作的常识，本次没读到处理细节。【未核实】
- 大小、商用：美国政府数据，一般可自由使用；本次未读到 FCC 对原始数据的授权条款。【未核实】

### 3.4 挪威 3G/HSDPA（Riiser 等，MMSys 2013）

- 内容：Telenor 3G/HSDPA 网络上 TCP 流媒体会话的日志；2010-09-13 至 2011-04-21；交通方式：公交、地铁、有轨电车、轮渡、汽车、火车；页面列出 11 条路线日志（1 公交、1 地铁、3 有轨电车、1 轮渡、3 汽车、2 火车）。
- 格式：6 列空白分隔：Unix 时间戳、毫秒时间戳、纬度、经度、距上次测量收到字节数、距上次测量的毫秒数；带宽 = 字节 / 毫秒；示例日志约 1 秒一行。
- 许可：页面未写明，只要求引用。
  出处：https://skulddata.cs.umass.edu/traces/mmsys/2013/pathbandwidth/ 【已核实】

### 3.5 比利时 4G/LTE（van der Hooft，根特大学）

- 内容：2015-12-16 至 2016-02-04，根特及周边路线；Android 手机下载大文件，从 100 Mb/s 以太网服务器拉；约 1 秒一行；40 条日志，约 5 小时，每条 166–758 秒；交通方式：步行、自行车、公交、有轨电车、火车、汽车。
- 格式：6 列（纪元毫秒、实验开始后毫秒、GPS 纬度、经度、收字节、间隔毫秒）。
- 许可：页面未写明，要求引用 IEEE Communications Letters 2016 论文。
  出处：https://users.ugent.be/~jvdrhoof/dataset-4g/ 【已核实】

### 3.6 Lumos5G（明尼苏达大学）

- 内容：明尼阿波利斯市中心 1300 米环线；毫米波 5G；每秒 iPerf 3.7 TCP 下行吞吐；步行和驾车；19 列：run_num, seq_num, abstractSignalStr, latitude, longitude, movingSpeed, compassDirection, nrStatus, lte_rssi, lte_rsrp, lte_rsrq, lte_rssnr, nr_ssRsrp, nr_ssRsrq, nr_ssSinr, Throughput, mobility_mode, trajectory_direction, tower_id。
- 大小：Lumos5G-v1.0.zip 1.53 MB（CSV）。
- 许可：CC BY 4.0（页面原话："licensed under the Creative Commons Attribution 4.0 International License"），可商用但需署名。下载需要 IEEE DataPort 免费账号。
  出处：https://ieee-dataport.org/node/3598 【已核实】
- 用途限制：只覆盖一个环线，吞吐是 TCP 吞吐不是链路容量；有 5G/LTE 切换状态列 `nrStatus`，可用来取"切换和掉到 LTE"的片段。

### 3.7 Puffer 数据

见 §1.7。每天 CSV；字段含 `cwnd, rtt, min_rtt, delivery_rate`；一天数 GB；匿名；数据页未写许可证。没有链路容量真值，`delivery_rate` 是 TCP 层观测。【已核实】

### 3.8 Pantheon 数据

见 §1.2。JSON + 原始日志 tar.gz，每条路径多次运行；站点已停止维护；许可未写明；Pantheon 代码许可证未核实。数据里有各算法在真实路径上的吞吐/时延/丢包，没有每 100 ms 的容量轨迹。【已核实：内容；许可【未核实】】

### 3.9 Starlink

- WetLinks（2024）：两个欧洲测点、六个月、约 140,000 次测量，含上下行吞吐、RTT、丢包、traceroute，配气象站数据；论文说 "release our datasets and all accompanying tooling as open data"，但摘要页没给出下载地址和许可。
  出处：https://arxiv.org/abs/2402.16448 【已核实：内容；下载与许可【未核实】】
- LEO Access Link Traces（IEEE DataPort）：2026-05-05 至 2026-06-30，每包主动测量（UDP 记录含收发时间戳、64 位序号、负载大小、校正后的单向时延），gzip CSV 每个运行一个；TCP 运行含 pcap、iperf3、socket 状态；整包 dataset_release.zip 91.69 GB；CC BY 4.0；下载需要 IEEE DataPort 订阅/登录。
  出处：https://ieee-dataport.org/documents/leo-access-link-traces-measurements-commercial-starlink-terminal 【已核实】（体积大，不下载）
- 15 秒重配置：Mohan 等 "A Multifaceted Look at Starlink Performance"（WWW 2024）用 19.2M 条 M-Lab 众测数据（34 国）加两国的专用终端，分析"globally synchronized 15-second reconfiguration intervals"；发现在只有一颗卫星可见时，RTT 和吞吐仍在这些间隔边界处变化，因此不是单纯的卫星切换。
  出处：https://arxiv.org/abs/2310.09242v2 （搜索摘要转述，PDF 全文未读）【摘要级核实】
- 另有 LENS（维多利亚大学，13 个终端、7 个 PoP、3 大洲，侧重时延）【搜索摘要，未核实许可】、LEOScope 测试床（arXiv 2310.11835，未读）。

### 3.10 其他

- Sprout / Saturator 轨迹（Mahimahi 自带，美国蜂窝）：见 §1.1。
- 微软论文后续（arXiv 2510.12265）提到约 1100 万条 Teams 真实通话网络轨迹的更大训练集，是否公开未核实。【只见搜索摘要；是否公开未核实】
- 5Gophers 更大的 5G 数据集（UMN 页面提到）【未读】。
- GitHub uccmisl/5Gdataset（爱尔兰 5G 吞吐轨迹，搜索结果见到）【未读，未核实】。

### 3.11 能否商用，汇总

| 数据集 | 许可证 | 是否可商用 |
|---|---|---|
| Lumos5G | CC BY 4.0 | 可，需署名 |
| LEO Access Link Traces（Starlink，IEEE DataPort） | CC BY 4.0 | 可，需署名；体积 91.69 GB |
| 微软 MMSys 2024 | 仅 "AS IS" 免责声明，无明确授权条款 | 未写明，先问微软 |
| 挪威 3G、比利时 4G | 页面只要求引用 | 未写明 |
| Puffer | 页面未写 | 未写明 |
| Pantheon | 页面未写 | 未写明 |
| FCC MBA | 美国政府数据，原始数据页未读到 | 未核实 |
| Mahimahi 自带轨迹 | 仓库代码 GPLv3，轨迹未单独声明 | 未核实 |
| WetLinks | 论文说 open data，许可未读到 | 未核实 |

---

## 4. 流量类型（应用负载）怎么建模

### 4.1 业内对"视频应用负载"的标准模型（会议视频，不是屏幕）

RFC 8593 "Video Traffic Models for RTP Congestion Control Evaluations"（读了正文）【已核实】，出处 https://www.rfc-editor.org/rfc/rfc8593.html ：
- 统计模型：编码器对速率变化的响应有滞后 τ_v；瞬态突发参数 K_d（突发帧数）、K_B（突发帧大小），引文 "the occasional burst of large frames is followed by smaller-than-average encoded frames"；稳态时帧间隔和帧大小围绕 t0=1/FPS、B0=R_v/8/FPS 做零均值拉普拉斯扰动。
- 轨迹模型：用真实高分辨率测试序列产生的编码帧大小序列回放；序列 2–4 分钟才"sufficiently avoids the periodic pattern"；固定 30 fps、720p；在不同码率的帧大小向量间插值；SkipFrames 典型值 20（避免重复 I 帧）。
- 混合：稳态用轨迹，瞬态（速率变化超过 10%）用统计模型。
- 测试视频：RFC 8868 推荐最低 Foreman（CIF）和 FourPeople（720p）。
这些是会议人脸视频，对屏幕共享的静止/突发特征不适用，是业内最接近的"负载模型"。

### 4.2 屏幕内容 / 远程桌面负载的公开资料

没有找到"业内公认的远程桌面负载标准模型"。能找到的：

1. HEVC 屏幕内容编码通用测试条件（JCT-VC，JCTVC-U1015 为有损 CTC）：测试序列是 4:4:4 屏幕内容，一篇使用该 CTC 的论文表 1 列出的序列名【已核实】：Basketball Screen、CAD Waveform、Console、Desktop、Flying Graphics、Kimono（10-bit，非屏幕内容）、Mission Control 3、PCB Layout、PPT DOC XLS。
  - 出处：https://arxiv.org/pdf/1710.09919 ；CTC 原文 JCTVC-U1015 本次没读到，搜索只命中征集提案文件 https://www.itu.int/en/ITU-T/studygroups/com16/video/Documents/CfP-HEVC-coding-screen-content.pdf （要求 RGB 与 YCbCr 两种版本；有损和数学无损两种条件）。【CTC 原文【未核实】】
  - 这些序列是为编码效率设计的短片段，不带用户交互时间线（没有"静止多久、突然爆发"的节奏），也不含真实的 UI 交互序列。
2. Screen Content Video Dataset（videoprocessing.github.io）：100 条参考视频 + 1,600 条失真视频；1920×1080、30 fps、7–10 秒；录自视频会议应用（Google Meet、Microsoft Teams 等）；"Access ... is provided by request"；许可证未写。
  出处：https://videoprocessing.github.io/screen-content-dataset 【已核实】
3. SCVD（TIP 2020，16 条参考屏幕内容视频，800 条失真，10 秒，1080p）和 SIQAD（20 幅参考图、980 幅失真图）：质量评价库，搜索结果转述。【未读原文，未核实】
4. 远程桌面协议的流量研究（PLoS ONE 2019）【已核实】，出处 https://pmc.ncbi.nlm.nih.gov/articles/PMC6320061 ：
   - 比较 RDP、VNC、ICA、TeamViewer、PCoIP（Amazon WorkSpaces）。三种用户画像：办公（LibreOffice 文字编辑）、网页浏览（三个富内容网站）、视频（144p–1080p）。
   - 办公："For 99% of the time, the upstream link rate usage was less than 100 Kb/s." 大范围屏幕变化时下行接近 900 Kb/s。
   - 网页（PCoIP）持续速率超过 6 Mb/s，峰值约 8–9 Mb/s；视频（720p）链路使用达 10 Mb/s。"sustained rates up to 10 Mb/s are common"，远高于亚马逊推荐的办公 300–1000 Kb/s。
   - 突发性（Hurst 参数 H）：PCoIP 办公 0.750、视频 0.760、网页 0.846；其他协议办公 0.553（ICA）–0.693（TeamViewer）、网页 0.781（ICA）–0.946（TeamViewer）。90 用户叠加后 H 只略降。
   - 包大小：PCoIP 网页和视频约 70% 的包到最大 1156 字节；办公包更小更杂；UDP 协议（PCoIP、TeamViewer）不到路径 MTU。
   - 数据可得性："All relevant data are within the paper."，即没有公开原始轨迹。
5. 其他：RDP 加密流量活动识别（Lapczyk & Skillicorn，arXiv 2008.02685，30 秒 RDP 轨迹，五种活动识别精度 >97%）、硕士论文（卢布尔雅那）说看视频时"constant bitrate of 30 Mbit/s"，均来自搜索摘要【未读原文，未核实】。
6. Chromium Remote Desktop（remoting）的测试：没有查到公开的负载模型或标准屏幕数据集；本次也没有读 Chromium 源码里的 remoting 测试。【未查到】

### 4.3 可借鉴的做法

- 把负载分成"画面类别"的状态机，每个类别有自己的帧大小分布：静止、局部更新（打字、光标）、滚动、整屏切换（窗口切换/打开应用）、视频区域（网页动画、播放）。PLoS ONE 论文的三种画像（办公、网页、视频）和 99% 时间 <100 Kb/s 的结论可当比例依据。
- 用真实屏幕录制取"帧大小时间序列"作轨迹回放，而不是合成像素：RFC 8593 的轨迹模型就是这个思路（取 2–4 分钟避免周期性）。
- 速率变化时采用 RFC 8593 的"瞬态用统计模型、稳态用轨迹"的混合法，用来模拟编码器对码率目标变化的反应滞后。

---

## 5. 怎么验证仿真可信，以及业内实时视频拥塞控制的评测指标和测试矩阵

### 5.1 验证仿真可信（Pantheon 之外）

- Pantheon：真实 vs 仿真 的平均吞吐和 95 分位时延百分比差；留一算法验证（约 20%）；每个参数的边际贡献表（见 §1.2）。
- Mahimahi：重放 vs 真实网页加载时间的相对误差中位数（12.4%）；以及对照工具。
- Salsify：先空载回环校准测试台自身，再减去固有延迟；留出部分轨迹不参与调参。
- Puffer：用真实部署结果对照 Pensieve 在仿真训练里的"好成绩"，指出仿真里的优势没带到真实网络（原文立场见 §1.7）。
- 共同的方法论：
  1. 同一批算法（越多越好）在真实和仿真里各跑；
  2. 比较的是端到端用户相关指标（吞吐、时延分位、卡顿），而不是仿真内部量；
  3. 报告误差的分布，不只是平均；
  4. 保留没参与校准的算法或路径做检验。

### 5.2 IETF RMCAT 测试矩阵（RFC 8867 / 8868 / 8869）

出处均已核实：https://www.rfc-editor.org/rfc/rfc8867.html 、 .../rfc8868.html 、 .../rfc8869.html 。草案 draft-ietf-rmcat-eval-test 已成为 RFC 8867（2021-01，Informational）。

RFC 8867 基础用例（参数原文）：
- 5.1 单流变速：100 s，容量比例 1.0（0 s）→ 2.5（40 s）→ 0.6（60 s）→ 1.0（80 s），基准 1 Mbps，单向时延 50 ms 或 100 ms；若用 UDP 背景流制造变化，物理容量保持 4 Mbps。
- 5.2 多流变速：125 s，两条流，基准 2 Mbps，比例 2.0（0）→1.0（25）→1.75（50）→0.5（75）→1.0（100）。
- 5.3 反向链路拥塞（双向媒体）：100 s，前向 2.0→1.0→0.5→2.0，反向 2.0→0.8→2.0。
- 5.4 同算法竞争：120 s，三条流在 0/20/40 s 先后加入，3.5 Mbps。
- 5.5 RTT 公平：300 s，五条流单向时延 10/25/50/100/150 ms，4 Mbps。
- 5.6 对抗长 TCP：120 s，2 Mbps，队列 300–1000 ms。
- 5.7 对抗短 TCP：300 s，十个短 TCP 流（2 个开始为 ON、8 个 OFF），2 Mbps。
- 5.8 暂停和恢复：媒体流在 40 s 暂停、60 s 恢复。
- 6.x 可选：优先级、ECN、多瓶颈。

RFC 8868 度量（原文列表）：发送速率/接收速率/goodput（200 ms 间隔）、包延迟（直方图或 CDF）、丢包和被抖动缓冲丢弃的包、重传/FEC 后丢包、公平性（同优先级同 RTT 的流吞吐比应在 0.333–3）、收敛时间（启动后和容量变化后到稳定速率）、速率振荡（在高低水位间来回的次数，例子 500 Kbps 和 2 Mbps、500 ms 窗口）、带宽利用率（发送速率/瞬时瓶颈容量）。要求每个实验记录每个收发包（RTP 日志格式：时间戳、负载类型、SSRC、序号、RTP 时间戳、标记位、负载大小）。
- 它的"没写"：RFC 8868 本身 "defines network parameters and traffic models but no quality metrics"（即没有 SSIM/MOS 这类画质指标）；无线/蜂窝模型也不在其中，转引 RFC 8869。

RFC 8869（无线）：见 §2.3。蜂窝仿真长 91 s、评估 30–60 s；Wi-Fi 测试 120 s。度量：小区平均吞吐、应用码率和 goodput、丢包率、端到端帧延迟（采集到显示）、传输时延、码率变化稳定性。

### 5.3 WebRTC GoogCC 的场景单测（goog_cc_network_control_unittest.cc）

已核实，出处 https://webrtc.googlesource.com/src/+/refs/heads/main/modules/congestion_controller/goog_cc/goog_cc_network_control_unittest.cc 。风格：每个场景是一条短时的单一网络设定 + 一个宽松的数值断言，没有统计学门槛。代表性条目：
- `UpdatesTargetRateBasedOnLinkCapacity`：500 kbps/100 ms → 25 s 后 450±100 kbps；800 kbps → 20 s 后 750±150；100 kbps/200 ms → 50 s 后 90±25。
- `LossBasedControlDoesModestBackoffToHighLoss`：2000 kbps、10% 丢包、200 ms → 120 s 后目标 >100 kbps。
- `NoBandwidthTogglingInLossControlTrial`：2000 kbps、20% 丢包；在 500 ms 窗口里 >100 kbps 的下降不超过 2 次。
- `IsFairToTCP`：1000 kbps、50 ms、TCP 交叉流 → 10 s 后 <750 kbps。
- `AverageBitrateAfterCrossInducedLoss`：1000 kbps、100 ms、25 包队列；4 次 2 s 的 TCP 突发，每次间隔 20 s 恢复。
- `CongestionWindowPushbackOnNetworkDelay`：1000 kbps、100 ms；10 s 暂停后目标 <40 kbps。
- `NoRttBackoffCollapseWhenVideoStops`：2000 kbps、100 ms；视频暂停 4 s 后 >1000 kbps。
- `NoCrashOnVeryLateFeedback`：反馈延迟 300 s 不崩溃。
- 路由变化：`OnNetworkRouteChanged` 等一组（重置、探测、"safe reset"试验）。
这些把"断言"写成区间（±100、±150、<750），说明业内承认单次仿真有数值噪声。

### 5.4 业内常见的"实时视频"画质/体验指标

- Salsify：SSIM（dB）和端到端帧时延 95 分位，逐帧用条形码对齐；帧没显示的算作 "arrival time = 无穷"（原文："or weren't displayed by the receiver, we assign an arrival..." 片段）。
- 微软挑战赛：客观音频/视频质量分（0–5，P.808/P.910 相关）的时间平均，作为奖励。
- Puffer：SSIM、卡顿比例（stall ratio）、画质变化。
- RFC 8868：见上，包括收敛时间、振荡、利用率、公平性。

---

## 6. 规模和统计

要点（均有出处）：

| 工作 | 轨迹/场景数 | 重复和时长 | 统计方法 |
|---|---|---|---|
| Pantheon 校准 | 6 条路径做仿真器 | 每次评估 30 秒场景 × 3 次、所有算法并行，一条路径约 2 小时（30 台 4 核机器） | 平均吞吐/95 分位时延的百分比差 |
| Pantheon 真实测试 | 数十个节点对 | 论文图例 "10 trials"、"5-runs-3-flows"（站点示例） | 画均值点和误差椭圆（图 2/3） |
| Salsify | 3 条蜂窝轨迹 + 间歇链路 + Wi-Fi | 每条 10 分钟、每系统一次（我没读到多次重复的说明） | 均值点 + 误差（图 6）；未做假设检验【细节未核实】 |
| 微软挑战赛 | 仿真评测 16 个场景 × 约 10 通 = 160 通 22 分钟电话；决赛测试床 600 通 3 分钟电话，12 天 | — | 平均奖励 |
| RMCAT | 每个用例 100–300 s | 没规定重复次数 | 要求报告 min/max/mean/std，画 CDF |
| WebRTC GoogCC 单测 | 数十个单测 | 单次、虚拟时间，几秒到几分钟 | 宽区间断言 |
| Puffer | 真实流，14.2 年视频、56,000 用户 | — | bootstrap 置信区间，时长加权标准误 |

- Puffer 的量化结论（原文）："with 1.75 years of data for each scheme, the width of the 95% confidence interval on a scheme's stall ratio is between ±10% and ±17% of the mean value."，并说这与某些学术工作的"总收益"一样大。结论：真实环境里，卡顿类的重尾指标需要极大样本才能分出 10–15% 的差异。
- 仿真里同一道理：重尾指标（冻结次数、最大冻结时长）用均值比较会被少数极端运行主导；业内（Puffer）用 bootstrap 重抽样给区间。"均值加两倍标准误"是正态近似，对重尾可能偏乐观。【后一句是我的判断，不是出处里的话；列出供参考，不当作事实】
- 在 CI 里控制时间的业内做法（本次没找到专门针对"实时视频 CC 的 CI"的公开文献）：
  - WebRTC 用虚拟时间（`NetworkEmulationManagerConfig` 里有时间模式，旧接口的 "time mode" 参数被弃用，见 `create_network_emulation_manager.h`），所以几十个场景能在一次单测里跑完；
  - Pantheon 把"校准"（慢、2 小时）和"跑仿真评测"（快）分开，校准一次后重复使用；
  - 其余（并行分片、按改动范围选择性运行）是通用 CI 实践，没有实时视频领域的一手来源。【后两条未核实】

---

## 7. 网络类型分类建议（可直接用于设计量尺）

下表"关键参数"栏的数值，来源已标注；没来源的标"待测"。参数写成区间是为了覆盖随机抽样；业内没有公认的"每类标准参数"，所以这是建议，不是标准。

| 类型 | 关键参数（含取值参考） | 建模手段 | 可用数据来源 |
|---|---|---|---|
| 有线局域网 | RTT <1–5 ms，容量 100 Mbps–1 Gbps，几乎无丢包，队列深度大但很少填满 | 固定容量 + 小固定时延；主要用于验证"无瓶颈时不误降" | RFC 8868 的 0–1 ms 时延档；WebPageTest "FIOS"（20/5 Mbps，4 ms） |
| 家庭宽带（独占） | 下行 5–100+ Mbps、上行 1–20 Mbps，RTT 5–50 ms，丢尾队列常为数百 ms 到 2 s（缓冲膨胀） | 固定容量 + 深队列 + 可选 AQM（CoDel/fq_codel 对比） | WebPageTest Cable(5/1 Mbps,28 ms)、DSL(1.5/0.384,50 ms)、FIOS；RFC 8868 队列 300–500 ms 常规、1000–2000 ms 膨胀；FCC MBA（拿不到文件，见 §3.3） |
| 家庭宽带 + 他人下载造成拥堵 | 交叉流量使可用容量在数秒内跌到 10–50%；长 TCP 流会填满队列使时延涨数百 ms | 交叉流量：长 TCP、短 TCP、背景 UDP、脉冲（周期峰值）、随机游走 | RFC 8867 §5.6/5.7；RFC 8868 的 TCP 模型（30 连接、30–50 KB、空闲指数均值 10 s）；WebRTC `PulsedPeaks/RandomWalk/FakeTcp` 生成器；GoogCC `IsFairToTCP` |
| Wi-Fi 5 GHz 信号好 | 空口容量几十到几百 Mbps，时延尖峰（数十 ms）、批量放行；多个设备竞争 | 容量轨迹 + 时延尖峰 + 批量放行（netem `slot`），再加背景流 | RFC 8869 §3（802.11n MCS11 应用层约 20 Mbps；N=1–20 流；CBR 开/关背景）；ns-3 Wi-Fi；BLADE 论文称"packet delivery droughts"（约 200 ms 无包放行，与 86.19% 卡顿相关，搜索摘要转述，原文未读） |
| Wi-Fi 2.4 GHz / 信号差 | 速率下降到数 Mbps，重传造成时延和丢包上升，邻居干扰 | 容量降低 + 随机丢包 + 时延尖峰；ns-3 SpectrumWifiPhy 才有干扰 | RFC 8869 §3.3（异构速率 2–54 Mbps，"802.11b 性能异常"）；Pantheon 含无线路径但没有 Wi-Fi 专项 |
| Wi-Fi 省电 / 接收端批量 | 信标周期 100 ms，省电模式下缓存的包在信标时才发，可增加至约 100 ms 时延 | 批量放行 + 固定周期抖动（netem `slot`） | 只有专利转述（美国专利"Systems and methods for providing real-time audio and data"系列，搜索摘要）；无同行评议模型【未核实】 |
| 4G/LTE | 容量 1–50 Mbps 剧烈变化（可低于 100 kbps 到 >20 Mbps，Salsify 轨迹范围），RTT 40–100 ms，突发放行，缓冲深，切换时有秒级中断 | 轨迹回放（Mahimahi 式毫秒放行）+ 深队列；Poisson 放行近似 | Mahimahi 自带 Verizon/AT&T LTE；比利时 4G 40 条日志；Salsify 的范围描述；RFC 8869 §2（ns-3 LTE）；WebPageTest LTE(12 Mbps,70 ms) |
| 3G | 速率 0.1–2 Mbps，RTT 150–400 ms | 同上 | 挪威 HSDPA 11 条；Mahimahi T-Mobile UMTS；WebPageTest 3G 档 |
| 5G | 低频类似 LTE；毫米波容量极高但遮挡时骤降，并会回落到 LTE | 轨迹回放 + 掉线/回落事件 | Lumos5G（每秒吞吐 + `nrStatus`，CC BY 4.0，仅一个环线） |
| 切换、隧道（移动场景） | 数百 ms 到数秒的中断或丢包突发，之后重新建链 | `mm-intermittent` 式指数开关，或轨迹里置零；中断后容量重新爬升 | Salsify 的间歇链路（均值 5 s 故障间隔、0.2 s 故障时长）；挪威 HSDPA 地铁/轮渡/火车；RFC 8869 对切换的论述（"可能引起丢包、延迟或抖动突发"，没有专门用例） |
| 卫星 LEO（Starlink） | RTT 约 25–60 ms 量级（待核实），每 15 s 全局同步重配置，在边界处时延和吞吐跳变 | 周期 15 s 的容量/时延阶跃 + 随机丢包 | Mohan 等（WWW 2024）；WetLinks；LEO Access Link Traces（CC BY 4.0，91.69 GB）；LENS |
| 跨洋 / 高延迟 | 单向 100–300 ms，容量可很高，丢包可能偏高 | 固定大传播时延 + 常规队列 | RFC 8868（150、300 ms 档）；RFC 8867 §5.5（至 150 ms）；Pantheon 东京–悉尼、伦敦–爱荷华 |
| 企业网 | 可能有策略限速（policer）、深度包检查，丢包呈现整形特征，出口共享 | token bucket 限速 + 突发丢包 | 微软仿真数据集含 "traffic policing" 场景（论文原文）；无公开企业网轨迹【未查到】 |
| 酒店 / 咖啡馆 | 共享 Wi-Fi + 出口拥挤，用户数多、时变强 | Wi-Fi 竞争 + 交叉流量 + 出口限速 | 没有公开数据集或模型【未查到】；只能用 Wi-Fi 竞争（RFC 8869 N=16/20 流）加限速组合 |
| 纯随机丢包链路 | 1%、5%、10%、20% 独立丢包，不伴随时延上升 | 独立丢包（RFC 8868 底线）+ 突发丢包（Gilbert-Elliott，平均突发长度） | RFC 8868 丢包档；WebRTC `loss_percent` + `avg_burst_loss_length`；微软仿真"burst loss"和"fluctuating burst loss"场景 |
| 间歇断网 | 秒级以下的全丢，之后恢复 | 两态指数开关 | Salsify intermittent；Mahimahi `mm-onoff` / `mm-intermittent` |

补充说明：
- 建议的分层结构（来自 Mahimahi 和 Pantheon 的可组合设计）：①容量层（轨迹，每类一组）；②时延/抖动/批量放行层；③队列层（深度 + 策略）；④丢包层（独立 + 突发）；⑤中断层；⑥交叉流量层；⑦应用负载层（屏幕内容状态机）。各层独立校准、独立开关，才能做"消融式"验证（Pantheon 图 5 的做法）。
- 随机抽样组合：RFC 8868 的取值（时延 4 档、丢包 5 档、队列 3 档）加容量轨迹，笛卡尔积很大，业内（RMCAT、WebRTC 单测）都不跑全积，而是挑定义好的代表用例；全积式随机抽样是 Pantheon 合成仿真（24 个）和 OpenNetLab 训练的做法。

---

## 8. 各节"可以直接借鉴的做法"汇总

1. 链路容量用"每毫秒放行机会"的轨迹格式（Mahimahi 的格式）：简单、粒度细、能表示稀疏小包和突发；自己实现，不引入 GPLv3 代码。
2. 场景描述用 OpenNetLab 的分段常量 JSON（duration、capacity、loss、rtt、jitter）：便于生成、便于人读；或在其上加 queue、burst_len 等字段。
3. 突发丢包用"总丢包率 + 平均突发长度"（WebRTC `avg_burst_loss_length`）；抖动与乱序分成两个开关。
4. 校准用 Pantheon 的方法：同一批算法在真实/仿真各跑，比较平均吞吐与 95 分位时延的差；先 5 个参数，逐个加、证明误差降。
5. 测试矩阵：RFC 8867 的 8 个基础用例作为底线（容量跳变、竞争流、RTT 公平、长/短 TCP、暂停恢复）；RFC 8868 的指标（收敛时间、振荡、利用率、延迟分布）；Salsify 的间歇链路作"断网"用例。
6. 交叉流量：三类（长 TCP、短 TCP、脉冲、随机游走），用 WebRTC 生成器的字段作参数定义。
7. 留出一部分轨迹不参与调参（Salsify 做法）。
8. 负载：用真实屏幕录制的帧大小序列做轨迹回放，瞬态用 RFC 8593 的统计模型（响应滞后 τ_v、突发 K_d/K_B）。
9. 统计：bootstrap 置信区间 + 时长加权；承认重尾（Puffer）。

---

## 9. 各家差异（一句话）

- Mahimahi：只做"毫秒级放行机会轨迹 + 固定时延 + 独立丢包"，队列策略可换，没有抖动/乱序，GPLv3。
- Pantheon：不追求细节，追求"与真实对得上"，5 个参数 + 自动校准。
- WebRTC SimulatedNetwork：单链路包级队列 + 高斯抖动 + 两态突发丢包 + 交叉流量生成器；不含 AQM 和轨迹回放，但确定、可嵌入单测。
- netem：真内核、功能杂（含 slot 近似批量放行和 gemodel），变速率要外部驱动。
- ns-3：物理/MAC 层细（Wi-Fi 聚合、LTE 小区），慢，适合离线生成轨迹。
- OpenNetLab gym：NS-3 + WebRTC，分段 JSON 轨迹，60 ms 报告间隔，抖动未支持。
- 微软 2024 数据集：真实通话观测 + 仿真里带容量/丢包真值；容量范围和变化模式未公开说明，仿真软件未点名。
- Salsify：评测重点在外部硬件时钟、逐帧条形码、真实视频；网络只用 Mahimahi 现成轨迹 + 间歇链路。
- Puffer：不靠仿真，靠大规模真实随机对照 + bootstrap；明确说仿真难以覆盖真实多样性。

---

## 10. 没查到或没核实的

1. 微软 2024 数据集：
   - 仿真软件是什么；16 个场景各自的容量范围、变化间隔、丢包取值、时延、队列大小；每一步的时间长度（样本里 1946 步，我只看了一个固定 161 kbps 的样本）；
   - 数据集的商用授权（只有 "AS IS" 免责声明）；
   - 测试床数据有无带宽真值（从论文和仓库说明看没有，但我没解压测试床样本验证）。
2. MMSys 2021 OpenNetLab 挑战赛的官方轨迹集：规模、来源、许可。AlphaRTC 的 README 里没有网络仿真描述；`gym` 只读到了源码注释里的 JSON 格式。
3. Mahimahi `--uplink-queue-args` 的具体参数名（packets/bytes/target/interval 等）没有逐个核实；Mahimahi 自带轨迹的单独许可证没找到。
4. FCC MBA 的原始数据页（fcc.gov）本机访问 403，没读到：文件清单、字段、授权条款、粒度；FCC 不再与 SamKnows 合作的说法来自搜索摘要。Pensieve/Puffer 如何把 FCC 数据加工成轨迹，只读到 Puffer 一句，Pensieve 的 `traces/` 说明没读到。
5. ITU-T G.1050 的参数表（各场景的时延、抖动、丢包分布）没读到，只看了摘要页。
6. Apple Network Link Conditioner 各预设的官方数值没读到。
7. Starlink：15 秒重配置只读到摘要级；"RTT 约 25–60 ms"这类数值我没有一手来源；WetLinks 的下载地址和许可没读到；LENS 和 LEOScope 未读。IEEE DataPort 上的 Starlink 数据集日期为 2026-05 至 06，只读了页面描述，没验证内容。
8. Wi-Fi 聚合/省电/空口竞争的同行评议模型：BLADE（arXiv 2603.16119）只读了摘要，"packet delivery droughts"约 200 ms 和 86.19% 的数字来自搜索摘要转述，不是我读原文得到的；省电 100 ms 来自专利摘要，不是论文。没有查到可直接用的"Wi-Fi 时延尖峰参数库"。
9. 企业网、酒店、咖啡馆网络：没有找到公开的数据集、模型或分类。
10. 屏幕内容负载：没有找到"远程桌面负载的公开数据集或标准模型"；JCTVC-U1015 原文没读；Chromium Remote Desktop 的测试和帧大小统计没查（本次未读 Chromium 源码）；SCVD/SIQAD 只有搜索摘要；Screen Content Video Dataset 需向作者申请，许可证未写。
11. "在 CI 里控制时间"的业内做法：没有找到实时视频拥塞控制领域的专门文献；上文的相关条目只有 WebRTC 虚拟时间一条有一手来源。
12. Pantheon 代码和数据的许可证；Puffer 数据的许可证；挪威/比利时数据的许可证：页面均未写明，没有进一步查。
13. 重尾指标是否应该用均值加两倍标准误：没有一手来源，上文第 6 节的那一句是我的判断，已标出。
14. Salsify 论文里每个场景实际重复了几次：只读到每个实验 10 分钟，没有读到多次重复的说明。
15. Mahimahi 论文里 12.4% 误差的完整实验设定（只读到片段）。
