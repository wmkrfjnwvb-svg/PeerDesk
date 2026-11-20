# PeerDesk E2E 仿真与真实运行测试调研

> 说明：本次只做了 3 次网页检索（MMSys'24 数据集、VMAF/XPSNR/FLIP、Lumos5G/爱尔兰 5G），其余内容来自我的既有知识，标注 **[未验证]** 的务必在落地前复核。

## 1. 网络仿真工具与保真度

| 工具 | 能力 | CI 可用性 | 保真度要点 |
|---|---|---|---|
| Linux tc/netem | delay、jitter、分布、loss（含 Gilbert-Elliott 的 `loss gemodel`）、duplicate、corrupt、reorder、rate | ubuntu runner 有 sudo，可 `ip netns` + veth，`modprobe sch_netem` 通常可用 [未验证：偶发内核模块缺失] | 见下方 caveat |
| tc tbf / htb | 令牌桶/分级限速，带队列（burst、limit/latency）；常作为 netem 之后的瓶颈 | 同上 | 比 netem 内置 `rate` 更接近真实瓶颈队列；队列长度决定 bufferbloat |
| Mahimahi | 用户态 shell（mm-link、mm-delay、mm-loss），trace 驱动链路；每行一个毫秒时间戳 = 一个 1500B 包的发送机会 | Linux only，需要 TUN/netns 与 sudo/setuid；可在 ubuntu runner 上编译运行 [未验证] | 对时变带宽最直观，支持 droptail/codel/pie 队列；非实时内核路径，CPU 负载高时会漂移 |
| ns-3 | 全协议栈离散事件仿真，确定性 | 纯 CPU，所有 OS 可跑 | 需要把 PeerDesk 的 sans-IO 核心接入 ns-3（可行但工程量大）；与自带模拟器功能重叠，优先级低 |
| Pantheon (Stanford) | 拥塞控制对比平台，含 Mahimahi 校准仿真器（"calibrated emulators"，用真实路径校准） | Python2 时代代码，维护弱 [未验证] | 主要价值是其 trace/校准方法与基线 CC（BBR、Cubic、Copa、Sprout、Vegas 等） |
| Windows: clumsy（基于 WinDivert） | 延迟、丢包、乱序、限速 | 需管理员；runner 上可行性未验证 | 用户态抓包重注入，精度低；不建议放进 CI 关键路径 |
| macOS: dnctl + pfctl（dummynet）/ Network Link Conditioner | 带宽、延迟、丢包 | sudo 可用；GitHub macOS runner 上 pf 能否改写回环 [未验证] | NLC 为 GUI/预设，不便自动化；dnctl 可脚本化 |

**netem 注意事项（来自既有知识，建议复核）**
- `netem rate` 只做简单串行化延迟模拟，不模拟真实队列；与 `delay`/`jitter` 同时使用时顺序与包间隔交互复杂。更稳的做法：根 qdisc 用 `tbf`/`htb` 做瓶颈，子 qdisc 挂 `netem` 做 delay/loss，或反过来用 ifb 分别处理上下行。
- jitter 默认会导致乱序（后发包延迟更小）。需要不乱序时在 netem 上限制或用 `reorder`/分布显式控制；真实链路通常保序，所以 jitter 要么配合 `tc ... delay 40ms 10ms distribution normal` 并接受乱序，要么用 trace 驱动。
- netem 默认 limit=1000 包，可能成为隐藏的缓冲区；必须显式设置。
- 只作用于出向（egress）；入向需 ifb。loopback 上 netem 作用于 `lo` 会同时影响双向流量，建议 netns+veth 对。
- 定时精度受内核 HZ/hrtimer 限制，CI 虚拟机上抖动在亚毫秒到毫秒级。

**回放带宽 trace 到 netem**：用脚本按时间窗（如每 100ms 或 1s）循环执行 `tc qdisc change dev veth0 root tbf rate X ...` 或 `netem rate X`；每次 change 会有微小扰动。更精确的选项：Mahimahi（原生 trace）、或自写 eBPF/用户态代理。**对"确定性"目标，推荐在进程内模拟器里回放 trace，netem/Mahimahi 仅用于真实二进制的"现实性校验"。**

## 2. 公共网络 trace 数据集

| 数据集 | 格式 | 许可 | 转换思路 |
|---|---|---|---|
| Mahimahi 自带 traces（Verizon/ATT/TMobile LTE 等，[repo](https://github.com/ravinet/mahimahi)） | 每行一个整数毫秒时间戳，代表一次 1500B 发送机会 | 仓库为 GPL-3.0 [未验证 trace 文件单独许可] | 按 100ms 窗口计数 ×1500×8 得 rate；delay/loss 需另行设定 |
| Norway HSDPA（Riiser 等，3G 公交/渡轮/火车） | 文本：时间戳、位置、字节数、间隔 ms 等 [未验证列定义] | 学术公开，许可条款 [未验证] | 字节数/间隔 → kbps 序列，1s 粒度，适合循环/缩放 |
| [Lumos5G](https://networking.umn.edu/lumos5g) | CSV（吞吐、位置、信号等） | CC BY 4.0（见[检索结果](https://ieee-dataport.org/node/3598)） | 取 throughput 列为 rate 时序；mmWave 的剧烈波动适合压测 |
| Irish 5G（Raca 等，[arXiv 参考](https://arxiv.org/pdf/2301.03722)中有描述） | 83 条 trace、共约 3142 分钟，含 RSRP/RSRQ/CQI 及上下行吞吐，1s 粒度 | 许可未能确认 [未验证] | 吞吐列→rate；信号质量低时可启发式叠加丢包 |
| Wi-Fi traces | 无统一权威集；CRAWDAD 有若干（许可各异）[未验证] | 逐集核实 | 通常需自合成：用 Gilbert-Elliott + 带宽阶跃模拟干扰、漫游 |
| FCC Measuring Broadband America | CSV，含下行/上行吞吐、延迟、丢包（unit 级聚合） | 美国政府公开数据，可自由使用 [未验证具体条款] | 固网：用 latency/loss 分布抽样得稳态参数，适合"家宽"基线 |
| Starlink 测量（如 LEO-NET 等学术数据集） | 不统一，常为 CSV 的 RTT/吞吐/丢包；15 秒切换周期特征（已知现象，[未验证具体数据集]） | 逐集核实 | 重点复现：每 15s 的 RTT 跳变与短时丢包突发 |
| Microsoft MMSys'24 带宽估计数据集 | 来自 Teams 的 18859 条测试床通话 + 9405 条仿真通话；含带宽估计、接收音频质量等逐步字段；仿真集含真实瓶颈容量与丢包（[来源](https://www.microsoft.com/en-us/research/academic-program/bandwidth-estimation-challenge/data/)） | 许可未能确认，须查 [RL4BandwidthEstimationChallenge 仓库](https://github.com/microsoft/RL4BandwidthEstimationChallenge) | 仿真集的 ground-truth 容量/丢包可直接作为 rate/loss 时序；测试床数据主要用于对照估计器行为 |
| Pantheon 校准仿真器 | 参数化（rate、delay、队列、loss）描述真实路径 | [未验证] | 直接映射为静态参数的 tbf+netem 配置，作为"稳态路径"套件 |

**统一 schedule 格式建议**（内部 JSON/CSV）：`t_ms, rate_kbps, one_way_delay_ms, jitter_ms, loss_pct, loss_model`。
- 进程内模拟器：每个 tick 查表；trace 用最近邻/线性插值；loss 用带种子的 Gilbert-Elliott。
- netem：脚本按 100ms~1s 窗口生成 `tc qdisc change` 序列；或把 Mahimahi 时间戳格式作为中间表示，同时可喂给 mm-link 与自己的模拟器，**一份 trace 两端共用**，减少转换偏差。
- 仓库中不要直接提交大 trace，提交转换脚本 + 少量（<1MB）代表性片段，附带许可说明。

## 3. 质量指标（屏幕内容 + 游戏/视频）

- **PSNR**：对屏幕内容的锐利边缘敏感、便宜、确定性强；适合"时间到锐利（time-to-sharp）"与无损/近无损场景判定，如 PSNR≥40 dB 视为收敛。
- **SSIM/MS-SSIM**：ffmpeg 内置（`ssim`、`msssim` 在 libvmaf 中）；对文字模糊敏感度优于 PSNR。
- **VMAF**：libvmaf 可用，但训练域是自然视频；屏幕录制文字属于其弱域，且 v0 仅看亮度（[Fora Soft 指南](https://www.forasoft.com/learn/video-quality/articles-vqm/what-a-metric-can-and-cannot-tell-you)，厂商文章，倾向性意见）。可作辅助趋势指标，不作为通过/失败的唯一依据。
- **XPSNR**：ffmpeg 有 `xpsnr` 滤镜，感知加权 PSNR，计算便宜；对屏幕内容的专门验证我未找到文献 [未验证]。
- **FLIP**（NVIDIA，渲染差异）：有 Rust/C++ 实现，适合"同一帧对比参考"的差异热图，便于生成可视化 artifact；屏幕内容表现 [未验证]。
- **SCC 专用指标**（如 SIQAD/CSIQ-SCI 研究中的 SIQM、ESIM 等）：学术性强，实现不成熟 [未验证]，不建议进 CI。
- **文字可读性**：对关键区域（终端、代码、网页）用 Tesseract 或 `ocrs`（Rust）做 OCR，比较与参考帧的字符错误率（CER）；阈值如 CER≤2%。注意 OCR 版本固定以保持确定性。
- **卡顿/抖动**：基于帧时间戳统计——freeze（>2×期望帧间隔或 >200ms 无更新）计数与总时长、帧间隔 p95/p99、帧率稳定性（标准差/均值）、judder（帧间隔与显示节拍偏差）。ffmpeg 的 `freezedetect` 可辅助。
- **ITU-T G.1072**（云游戏 QoE 模型，输入为时延、丢包、码率、分辨率、帧率等）、**P.1203/P.1204**（自适应流媒体质量；P.1204 为比特流/像素模式）：面向流媒体/游戏，不是为远程桌面设计 [未验证适用性]；可作为长期"综合分"的参考，但 CI 阶段不必实现。

**推荐 CI 小集合（初始阈值，需按基线校准）**
1. 稳态收敛后 PSNR（Y）≥ 38 dB 且 SSIM ≥ 0.98（静止桌面）。
2. 关键文字区域 OCR CER ≤ 2%。
3. time-to-sharp（内容变化到 PSNR 达标）p95 ≤ 基线×1.2。
4. freeze 次数 = 0（良好网络），弱网下总冻结时长 ≤ 阈值。
5. 帧间隔 p99 ≤ 3× 目标间隔。
6. VMAF 作为信息项，仅报告不阻断，相对回归（下降>3 分）告警。
所有阈值以"相对基线回归"为主、绝对下限为辅，避免因 CI 噪声误报。

## 4. 延迟测量

- **嵌入时间戳/条码**：发送端抓屏前，在一个固定小块叠加编码帧序号/时间的条码或 QR（高对比、大模块，经 codec 有损后仍可解）；查看端解码出序号，与发送时间对照。同机测试用单调时钟；跨机需 PTP/NTP 或用环回法（同一台机器同时跑发送与查看，网络用 netns 隔开）规避时钟偏差。
- **glass-to-glass**：相机拍显示器法（外部硬件）；CI 中用软件等价：发送端合成帧→viewer 的最终渲染缓冲，近似 g2g（不含显示器扫描延迟）。
- **输入到显示（input-to-display）**：脚本注入输入事件（如 xdotool 在 Xvfb 中移动窗口/改变像素块颜色），记录注入时刻，viewer 侧检测像素变化的第一帧时刻。
- **业界做法**（均 [未验证具体细节]）：NVIDIA LDAT 用光电传感器测鼠标点击到屏幕亮度变化；Moonlight/Sunshine 在统计叠加中显示解码/网络延迟，不含 g2g；Parsec 在 overlay 显示网络/编解码延迟；OBS 无原生端到端测量，常见做法是屏幕时钟拍摄对比。
- **CI 建议**：把"帧序号条码"作为测试专用的 capture 源叠加（不进入产品路径），并且记录每个阶段（capture/encode/send/recv/decode/present）打点，便于定位回归。

## 5. 合成但真实的屏幕内容

- **JCT-VC SCC 测试序列**：HEVC SCC 通用测试条件（JCTVC-U1015 等）包含 `sc_*` 序列（如 console、desktop、web_browsing、map、programming、slide_show、robot 等）[未验证列表]；许可通常为标准化测试用途，再分发限制需逐个核实，**不建议放进仓库**，可在本地/私有缓存下载使用。
- **Xvfb 脚本化负载**（全部可确定性）：固定分辨率与 DPI、固定字体包；`xdotool` 打字、滚动、拖动窗口；用本地静态 HTML 页面（离线、固定内容）+ 无头浏览器滚动；`ffplay`/`mpv` 播放固定种子生成的测试视频；终端中 `cat` 固定文本。禁用光标闪烁、动画和时钟。
- **纯合成生成器**：Rust 内生成确定性帧（文本渲染用固定字体与种子、滚动偏移、窗口矩形移动、渐变/噪声块、扫描线），既用于进程内仿真（无需 X）也用于真实运行；覆盖"静止-突变-滚动-视频区域"几类典型场景。
- **CI 确定性**：固定种子；固定时钟（模拟器内虚拟时间）；锁定 ffmpeg/libvmaf/tesseract 版本（容器或 nix）；真实运行里不对绝对值做精确断言，只做区间/相对断言。

## 6. 推荐 E2E 架构

**三层金字塔**
1. **L1：进程内确定性仿真（每次 PR，秒级~分钟级）**：sans-IO 核心 + 虚拟时钟 + 自带网络模拟器回放统一 schedule；使用真实编解码器（软件编码，注意不同平台/版本输出可能不同，需要锁定版本或仅在 Linux 上跑做硬断言）。合成内容生成器提供帧。输出逐帧指标，可完全复现（同种子同结果）。
2. **L2：真实二进制 + 单机 netns/veth + tc（每次 PR 或 nightly，几分钟）**：Linux runner 上建 `ip netns`，veth 对，瓶颈 tbf + netem；用 Xvfb 跑 host 端脚本化工作负载；viewer 端无头录制到帧序列。Windows/macOS runner 仅做冒烟（无网络整形或用回环），指标不设硬门槛。
3. **L3：跨机真实运行（nightly/周更，自托管或两台 runner）**：需要公网/私网互通和时钟对齐；GitHub 托管 runner 间的跨机连通与稳定性 [未验证]；可选用自托管机器或云上一对 VM。

**断言项**
- 延迟：glass-to-glass / 输入到显示 p50/p95/p99；
- 字节：总发送字节、平均码率、与链路容量的利用率、重传开销；
- time-to-sharp：内容突变后质量收敛时间；
- freeze 次数/时长、帧间隔 p99、帧率稳定性；
- 质量：PSNR/SSIM/OCR CER，VMAF 为信息项；
- 恢复：模拟 5s 中断/带宽骤降后的恢复时间。
断言形态：对 L1 用精确/紧区间；L2 用区间 + 相对基线；L3 仅趋势与告警。

**可视化 artifact**：失败/所有运行产出：参考与接收帧并排 PNG（ffmpeg `hstack`）、差异热图（FLIP 或绝对差）、时间线图（带宽 trace、码率、延迟、freeze 标记）、10~20s 并排 MP4/WebM；用 `actions/upload-artifact` 上传，保留 14~30 天。

**速度与确定性**：L1 用虚拟时间可快于实时；每场景 5~15s 内容；场景矩阵用 trace 子集 + 并行 job；缓存 ffmpeg/模型；L2 限制在 Linux 上，每 job < 10 分钟。

**回归追踪**：每次 main 提交把指标 JSON 推送到 `gh-pages` 或使用 `github-action-benchmark`（支持自定义工具，自动对比并评论）；长期用 Bencher 或自建 SQLite/CSV 存储 + 简单看板 [未验证具体集成细节]。PR 内对比基线并设阈值（如关键指标恶化>5% 失败）。记录 runner 类型、内核、ffmpeg 版本以便解释噪声。

## 自适应切换卡顿的测量

**背景**：项目负责人反馈 ToDesk 在自动切换画质时会卡住数秒。这类"适配期卡顿"必须作为一等指标，独立于稳态质量。

**场景（均为统一 schedule，可同时喂进程内模拟器与 netem/tbf）**
- S1 阶梯下降再上升：20 Mbps(10s) → 3 Mbps(15s) → 20 Mbps(15s)。
- S2 突发深跌：稳态 20 Mbps，t=10s 起 2s 内降 90%（约 2 Mbps），之后恢复。
- S3（建议补充）：同上但叠加 RTT 突增 +100ms 或 2% 丢包，检验丢包型与排队型拥塞的不同反应。
- 每个场景在三类内容上跑：静止桌面+文字、滚动网页、视频区域；固定种子，各重复 N 次（L1 一次即可，L2 取 5 次中位数）。

**viewer 侧测量方法**
1. 在 viewer 的"呈现"点（解码后、提交给渲染器时）为每个呈现的新帧记录单调时钟时间戳；内容里叠加帧序号条码，用于确认"新内容帧"而非重复帧。
2. 只对"内容在变化"的时段统计帧间隔（静止桌面本来就可能不发帧，要用测试源强制每 1/30s 变化一个计数块，或在 host 端知道真实内容变更时刻，以"内容变更→viewer 呈现"的延迟衡量）。
3. 指标：
   - 最大帧显示间隔 max_gap；
   - freeze 次数与累计时长，按 gap>100ms 与 gap>200ms 两档统计（可再记 >500ms、>1s）；
   - time-to-recover：带宽事件时刻（降/升）到帧率与延迟恢复稳定（帧间隔 p95 回到基线×1.5 以内且端到端延迟回到基线×1.5 以内并持续 1s）的时间；
   - 质量凹陷：事件窗口内 PSNR/SSIM 的最低值与"低于稳态阈值的持续时长"，以及升档后回到稳态质量的 time-to-sharp；
   - 降档期间的端到端延迟峰值（排队型卡顿常表现为延迟飙升而非丢帧）；
   - 额外：编码器切换/IDR 次数与大小，用于定位卡顿原因（大 IDR 突发导致排队是常见原因，属一般经验 [未验证本产品]）。
4. 实现：Rust 里对时间戳序列做滑窗统计即可，无需 ffmpeg；录制的视频仅用于 artifact。

**建议通过阈值（初值，需用基线校准，且以稳态良好网络基线做相对约束）**
| 场景 | max_gap | >200ms 冻结 | >100ms 冻结 | time-to-recover | 质量凹陷 |
|---|---|---|---|---|---|
| 良好网络对照 | ≤ 66ms | 0 | 0 | - | - |
| S1 降档 20→3 | ≤ 300ms | 0 | ≤ 3 次且累计 ≤ 500ms | ≤ 2s | PSNR 不低于稳态 -8 dB，且低质持续 ≤ 3s |
| S1 升档 3→20 | ≤ 150ms | 0 | ≤ 1 | 质量 time-to-sharp ≤ 5s | 升档不得引发卡顿 |
| S2 突发 90% 跌 2s | ≤ 500ms（带宽仅 10%，允许短暂） | ≤ 1 次 | ≤ 4 次 | 恢复 ≤ 3s | 允许明显降质，但不得黑屏/停帧 |
| 任何场景 | 不得出现 >1s 冻结（硬门槛，直接对应 ToDesk 数秒卡顿的投诉） |
这些数值是我的经验性提议，没有公开标准可依 [未验证]；先在当前实现上测基线，再以"回归>20% 或超过硬门槛则失败"落地。

**判读提示**：若 max_gap 大但延迟峰值也大，说明码率降档滞后、在瓶颈队列里积压（应更快下调或限制在途数据）；若 gap 大而延迟不变，说明编码器重启/IDR/重配置造成的停顿（应无缝切换码率而非重建编码器）。

## 7. 主要风险与待验证清单
1. GitHub 托管 Linux runner 上 netns/netem/ifb 模块可用性与时间精度；
2. 软件编码器跨平台输出一致性（影响 L1 的"精确"断言）；
3. 各数据集再分发许可（Norway HSDPA、Irish 5G、MMSys'24、Starlink）；
4. XPSNR/FLIP/OCR 对屏幕内容的相关性，需用少量人工评分标定；
5. 跨机时钟同步方案。

## 来源
- [MMSys'24 带宽估计数据页](https://www.microsoft.com/en-us/research/academic-program/bandwidth-estimation-challenge/data/)、[论文 arXiv](https://arxiv.org/pdf/2403.06324)
- [Lumos5G](https://networking.umn.edu/lumos5g)、[IEEE DataPort](https://ieee-dataport.org/node/3598)（CC BY 4.0）
- [Irish 5G 描述（arXiv 2301.03722）](https://arxiv.org/pdf/2301.03722)
- [VMAF 盲区讨论（Fora Soft）](https://www.forasoft.com/learn/video-quality/articles-vqm/what-a-metric-can-and-cannot-tell-you)
- [Mahimahi](https://github.com/ravinet/mahimahi)
