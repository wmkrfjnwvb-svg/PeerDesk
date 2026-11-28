# 网络类型参数调研：A3 按网络类型抽参数的先验（2026-11-28）

目的：A3 步要“每种网络类型给出各层参数的分布”。层的定义见 `a2-plan.md` 2.1 节（中断和换路、限速器、瓶颈=容量+队列+交叉流量、丢包、传播延迟、延迟噪声、接收端、反向路径）。
本文只补 `industry-network-sim.md` 第 10 节“没查到或没核实的”里的 1、4、5、7、8、9 条，不重复已有内容。只调研，没改代码。

标注：
- 【原文】= 本次下载并读了论文 PDF、官方报告、数据说明或许可证原文；每条给链接和页码（PDF 页，不是印刷页码）或表号。
- 【二手】= 只读到搜索摘要、第三方转述、或工具对网页的摘要。
- 【推测】= 我自己的推断。
- 没读到原文的地方直接写“只读到摘要”。

本次落盘的原文在 `/tmp/claude-0/-home-user-peerdesk/a4149fae-328b-5bde-8281-bf0aeca8fdf1/scratchpad/n/`（PDF 和转出的文本，约 120 MB，没有下载任何整包数据集）。

---

## 0. 先给结论

1. **Starlink 的 15 秒周期，相位是有一手来源的**：Tanveer 等的预印本 arXiv 2307.00402 第 2 页原文写“at the 12th, 27th, 42nd, and 57th second past every minute”，而且“observed from all our measured locations for all periods of time”。WWW 2024 那篇（Mohan 等）只说“globally synchronized”，**没有写是每分钟的第几秒**。两篇不矛盾：Mohan 证实了两个国家的终端边界对齐，Tanveer 给了对齐在哪几秒。
   边界处的延迟尖峰：Casparsen 等（arXiv 2601.08439，2026-01，丹麦奥尔堡，500 Hz 单程探测）测到周期开头约 140 ms、结尾约 75 ms 受影响，开头平均比周期均值高 74 ms。
2. **Wi-Fi 没有“尖峰频率、时长、幅度”的现成分布**。同行评议里最接近的是：
   - 腾讯 BLADE（NSDI 2026）：云游戏 5 GHz Wi-Fi 上，帧延迟 >200 ms 的比例 0.08%（周边 2 个 AP）到 1.34%（≥8 个 AP）；
   - 清华校园网（MobiSys 2016）：分钟均值的中位数 3 ms，P90 约 20 ms，P99 >250 ms。
   “约 200 ms 的送达空窗”要改写：200 ms 是 AP 记录统计的**窗口长度**，“空窗”指某个 200 ms 窗口里一个包都没送出去；不是“有一段 200 ms 的固定停顿”（见 3.2 节）。
3. **美国有线接入的数是最扎实的**：FCC 第 13 期报告（2022-09、10 月数据）给了按技术的空闲延迟和满载延迟，数据明确“可无限制使用”。满载（尤其上行）是各层里最大的一项：DSL 上行满载均值 181–720 ms，有线 88–252 ms，光纤 24–41 ms；Comcast 开 AQM 后上行满载 15–30 ms，没开的平均约 250 ms、最坏超过 1 秒。
4. **蜂窝和 5G 的数是论文给的，但都是 2021–2022 年的 Verizon/T-Mobile**；切换：NSA 5G 平均 167 ms，4G 平均 76 ms。毫米波每 0.13 km 一次切换。数据集许可：Lumos5G 和 5Gophers 是 CC BY 4.0（二手），SIGCOMM'21/'22 的发布仓库没找到许可文件。
5. **G.1050 不能直接拿来当 A3 的先验**：2016 版只有 DSL 和 GPON 两种接入，CATV 和无线写着“for further study”，数据是 2010 年前后的，**明确不含链路故障、路由抖动**（PDF 第 10 页）。`industry-network-sim.md` 第 2 节第 2 条说“2016 版加入链路故障、路由抖动、乱序”，**与原文相反，需要更正**（见第 10 节）。
6. **企业网、酒店、咖啡馆：没有现代的公开测量。**只有 2009 年 Seattle 13 个热点（Starbucks 等）的 802.11b/g 测量，和清华校园网（学术企业网）。
7. **数据集许可是个雷区**：WetLinks 和 LENS 的数据是 CC BY-SA 4.0（ShareAlike），Ookla 开放数据是 CC BY-NC-SA 4.0（**不可商用**）。只有 FCC MBA（无限制）、M-Lab（CC0）、LEO Access Link Traces（CC BY 4.0，二手）、Lumos5G（CC BY 4.0）可以放心引用。我们不往 Apache-2.0 仓库里放原始数据，只放自己拟合出来的参数，并且最好让法务看一眼。

---

## 1. Starlink

### 1.1 15 秒重新分配：周期、相位、边界行为

| 项 | 内容 | 来源 |
|---|---|---|
| 周期 | 15 s，全球同步（不是每颗卫星或每个终端各自的） | 【原文】Mohan 等 WWW 2024，https://arxiv.org/pdf/2310.09242 ，第 7 页 §6.2“Global Scheduling”：两个国家（爱丁堡、慕尼黑）终端的 RTT 在 15 s 间隔内稳定、间隔之间变化，“time-wise alignment ... indicates that Starlink operates on a globally coordinated schedule, rather than on a per-Dishy or per-satellite basis” |
| **相位** | **每分钟的第 12、27、42、57 秒**；四个终端（西欧、美国东北、中西部、西北）、整个测量期都如此；相邻 15 s 窗口的延迟分布经 Mann-Whitney U 检验两两不同（p<.05） | 【原文】Tanveer 等，arXiv 2307.00402，https://arxiv.org/pdf/2307.00402 ，第 2 页 §3。原文没写时区；测量端和服务器用 NTP 对时。预印本的模板页眉还是“SIGCOMM'18”，**发表场合我没核实** |
| 相位说明 | WWW 2024 论文没有给出“第几秒”。它的脚注 4（第 7 页）说，连接窗口结束时间常常落在边界后 1 s，是因为 gRPC 数据只有每秒粒度、时间戳取自发请求的一端，是测量假象 | 【原文】 |
| 是不是卫星切换 | 不是。遮住天空只留一颗可见卫星时，RTT 和吞吐仍在边界处变化；连接窗口的结束时间与边界强相关，开始时间不相关（推测为调度器每 15 s 重新分配卫星或频率） | 【原文】Mohan 等第 7 页 §6.2“Disproving Satellite Handoff Hypothesis”“Scheduling Updates” |
| 延迟形状 | 每个 15 s 窗口内延迟相对稳定，常出现相差几 ms 的“平行条带”（雷达帧分配的痕迹） | 【原文】Tanveer 等第 3 页 |
| 边界尖峰 | 周期开头约 140 ms、结尾约 75 ms 是受影响区间；开头平均比周期均值高 74 ms，之后快速回落，结尾有二次抬升；去掉这两段后的周期内部可用于预测，99 分位预测误差 <50 ms | 【原文】Casparsen 等，arXiv 2601.08439，https://arxiv.org/pdf/2601.08439 ，第 8 页 Fig. 3 和正文；摘要。奥尔堡的一个终端，500 Hz 单程探测，“多个月”的轨迹 |
| 边界上的吞吐 | 上行和下行吞吐都在边界处下降，窗口内较稳 | 【原文】Mohan 等第 7 页 Fig. 16b 的描述 |
| 边界上的丢包 | WWW 2024 引用他人（[25, 51, 73]）说边界处有“RTT spikes and packet losses of several orders”，**没有自己的丢包数**；WetLinks 的丢包是 250 包/0.1 s 间隔的探测，没有对齐到边界 | 【原文】Mohan 等第 5 页；WetLinks 第 5 页。**边界处丢包率的一手分布没有找到** |
| 应用层影响 | Zoom：上行有 FEC 包 146±99 kbps（地面网 2±2 kbps）；Luna 云游戏：偶尔帧率掉到 <20 FPS，和边界重合；单程延迟上行 52±14 ms（地面 27±7），下行 35±11（地面 32±7） | 【原文】Mohan 等第 5–6 页 §5 |
| 长尾 | LEOScope（欧洲 5 个终端）：中位数 30–50 ms，P99 >2.4× 中位数，最大 11–16× 中位数；有持续 ≥15 s 的 >70 ms（2× 中位数）尖峰，作者归因为“次优切换”，持续时间是 15 s 的整数倍 | 【原文】Tiwari 等 arXiv 2310.11835，https://arxiv.org/pdf/2310.11835 ，第 4–5 页 §3 |

对 A2 的“延迟阶跃”层的含义：
- 周期 15 s 有一手来源。相位 12/27/42/57 是**绝对时钟**（每分钟的秒），不是相对会话起点；仿真里相位应作为参数，默认可以固定为“t ≡ 12 (mod 15) s”（UTC），也可以每个种子随机抽（会话起点相对 UTC 是均匀的）。
- 阶跃不只是一次延迟跳变：边界前后有 ~140 ms / ~75 ms 的尖峰区间（幅度均值 +74 ms，来自欧洲一个终端），中间几 ms 的条带，再加窗口之间的水平变化。【推测】建议在 A2 的层里分三部分：窗口水平（每窗口一个新抽的基础延迟和容量）、边界尖峰、窗口内条带噪声。
- 美国上的边界数据：本次没有读到。Tanveer 的 4 个终端里有 3 个在美国，但文中只用来证明周期。

### 1.2 RTT、抖动、丢包、带宽的分布

| 量 | 数值 | 来源 |
|---|---|---|
| 往返延迟（美国） | 美国整体稳定，Seattle 的 P75 远低于 50 ms；“Seattle ... 75th percentile well below 50 ms”，并确认 Seattle 不是特例，全美基本一致（Appendix Fig. 21a） | 【原文】Mohan 等第 4 页 §4。数据是 M-Lab 众测（TCP minRTT） |
| 往返延迟（欧洲，专用终端，到自己的服务器） | WetLinks：均值 61.52 ms（σ=7.81，奥斯纳布吕克）、63.53 ms（σ=6.87，恩斯赫德）；约 72% 的测量在 1σ 内；离群到 200 ms；中位数 62±3 ms，与时间段无关。bent-pipe 段均值 31.08 ms（σ=11.18）和 34.56 ms（σ=12.0） | 【原文】Laniewski 等，arXiv 2402.16448，https://arxiv.org/pdf/2402.16448 ，第 5–6 页 §V-B/C（含 Fig. 7） |
| bent-pipe 延迟 | 53° 壳层全球约 40 ms（36–48 ms） | 【原文】Mohan 等第 6 页 §6.1，RIPE Atlas |
| 欧洲长尾 | 欧洲分位数更长，常超过 100 ms；南美 P75 >100 ms，尾部 200 ms | 【原文】Mohan 等第 4 页 |
| 满载延迟 | 下载时 RTT 比空闲高 ≈2–4×，到 400–500 ms；上传时第 60 分位 ≤100 ms（下载时 ≈200 ms）；作者怀疑有 AQM | 【原文】Mohan 等第 4 页 Fig. 8（用最大 RTT 减最小 RTT，M-Lab 测速） |
| 丢包 | WetLinks 的专用探测：28.4%/33.4%（两个点）的测量里有丢包；均值 1.0%（σ 2.8%/2.4%）；最大 87.2%/84.0%；约 65% 的丢包事件只丢 1 个包（0.4%）；>80% 的测量丢包率 ≤1%，约 3% ≥5%；中位 PLR 0.4%；两个点的丢包事件不相关；“短尖峰”可视为突发丢包。M-Lab 测速的 TCP 丢包率 P75 在 4–8%（与吞吐负相关） | 【原文】WetLinks 第 5 页 Table II、Fig. 5–6；Mohan 等第 4 页。两者定义不同（探测 vs TCP 重传），不能直接对比 |
| 下行带宽 | M-Lab：多数用户 P75 为 50–100 Mbps 下、4–12 Mbps 上；WetLinks（欧洲，UDP，专用）：下行中位 ≈210–250 Mbps（深夜）、傍晚 19–20 时最低 ≈180–205 Mbps；下行范围 0 到 >400 Mbps（0 为短暂罕见的断线）；上行中位 ≈13–17 Mbps，范围 0 到 >50 Mbps；下行日内最低约比最高低 20%；下雨时下行中位 181.52 比无雨 218.12 低约 17% | 【原文】Mohan 等第 4 页 §4“Goodput”；WetLinks 第 4–8 页 Fig. 4、8、10、11，Table III。WetLinks 的中位数是用 UDP 测的，作者说比以前 TCP 的研究高，原因“highly likely”是丢包敏感的拥塞控制 |
| 断线 | WetLinks 称下行有“short and rare Starlink outages”；Mohan 等的 Zoom 测试里“we did not experience second-long outages”。**断线的频率和时长分布没有找到** | 【原文】 |

### 1.3 数据集：下载和许可

| 数据集 | 内容 | 下载 | 许可 | 商用 | 放进开源仓库 |
|---|---|---|---|---|---|
| **WetLinks**（Osnabrück 大学等，TMA 2024） | 两个欧洲点、约 6 个月、14 万次测量，吞吐/RTT/丢包/traceroute/天气 | GitHub `sys-uos/WetLinks`；README 列出 Raw_Data、Preprocessed_Data、Plotting_Scripts；气象局数据要从 `DWD_KNMI_Downloadlinks.txt` 另下 | 【原文】README：**CC BY-SA 4.0**（https://raw.githubusercontent.com/sys-uos/WetLinks/main/README.md） | 可，需署名，**衍生数据要同样用 SA 许可共享** | 不要把原始数据放进 Apache-2.0 仓库（SA 会传染到包含它的合集）。拟合出的参数表（几个数字）是否算“改编”要法务判断，**我没有法律意见** |
| **Mohan 等 WWW 2024 数据集** | >300 GB（M-Lab、Zoom、Luna、RIPE Atlas、控制终端） | 数据在 mediaTUM（DOI 10.14459/2024mp1734703），用 rsync，密码要在 mediaTUM 页面上取；代码仓库 `nitindermohan/multifaceted-starlink-performance` | **许可没读到**：mediaTUM 页面被 Anubis 反爬挡住（返回 Access Denied）；代码仓库 main 分支没有 LICENSE 文件（404）。数据 DOI 页是 doi.org/10.14459/2024mp1734703 | 未知 | 不放。先写邮件问作者 |
| **LENS**（维多利亚大学，MMSys 2024 数据集赛道） | 13 个 Starlink 终端、7 个 PoP 的延迟轨迹（最近两年约 20 TB 原始数据，二手） | GitHub `clarkzjw/LENS`（README 里列出每月快照，存 Zenodo；2025-11 起新数据在 lens-starlink.jinwei.me，经 Cloudflare CDN，下载会收“有限的遥测数据”）；Zenodo 旧快照 10.5281/zenodo.10020034（2023-09-13~17，1.17 GB 的 `.tar.zst`） | 【原文】仓库代码 GPL-3.0；README 称 Zenodo 数据文件为 **CC BY-SA 4.0**；Zenodo 记录 API 里该条（v20230917）的 license 字段写 **cc-by-4.0**。**两处不一致**，要以下载的那一份的 Zenodo 页面为准 | CC BY 可；CC BY-SA 可但有 SA | 同上：不放原始数据；GPL 代码不能引入（CLAUDE.md） |
| **LEOScope 测试床代码**（T3P 论文） | 代码，不是数据 | 论文说已发布，链接在文献 [42]，**我没点开** | 未读 | 未读 | 不引入 |
| **LEO Access Link Traces**（IEEE DataPort，2026-05~06） | 每包主动测量，91.69 GB | 要 IEEE DataPort 登录 | 【二手】`industry-network-sim.md` 3.9 节：CC BY 4.0 | 可，需署名 | 不放，体积也太大 |
| **Casparsen 等 2026** | 500 Hz 单程延迟 | 论文里**没有写数据获取方式**（全文没有 availability/repository 字样，只引了 irtt 工具） | 无 | 无 | 无 |

**【勘误】** `industry-network-sim.md` 3.9 节写“WetLinks 下载地址和许可没读到”：现已读到，是 GitHub + CC BY-SA 4.0。`industry-network-sim.md` 3.11 表里 WetLinks 一行“未核实”可以改。

---

## 2. 家用 Wi-Fi：延迟尖峰、聚合、省电、空口竞争

### 2.1 找到了什么、没找到什么

- **没找到**同行评议的“家用 Wi-Fi 延迟尖峰的频率、时长、幅度三联分布”。已有的是下面三类：
  1. 校园/企业规模的延迟分布（清华，每分钟 6 次 ping 的均值，**不是每包**）；
  2. 云游戏平台里的帧级停顿（腾讯，33.6 亿帧，**帧延迟 >200 ms**）；
  3. 机理和对策论文里引用的零散数字（Law）。
- 2.4 GHz 对 5 GHz：只有清华一条直接对比。
- Wi-Fi 5/6/7：有 Wi-Fi 6 的真机实验（Liu & Choi，SIGMETRICS 2023），Wi-Fi 7 只有基于频谱数据集的仿真，**没有商用 Wi-Fi 7 的实测论文**。

### 2.2 BLADE（NSDI 2026）：核对“约 200 ms 的送达空窗”

原文：Guo 等，arXiv 2603.16119，https://arxiv.org/pdf/2603.16119 ，NSDI 2026 收录。读了摘要、引言、§3.1、表 1、表 2、图 8 的文字。

| 说法 | 原文怎么写 | 判断 |
|---|---|---|
| “约 200 ms 的送达空窗” | 第 2 页：“packet-delivery drought: a 200 ms interval during which an access point fails to deliver a single packet to a user”。第 5 页：AP 每 200 ms 报告一次“成功发送的包数”，**200 ms 是统计窗口**。表 1：对“帧端到端延迟 >200 ms 且 服务器到路由器 RTT <50 ms”的帧，看每个 200 ms 窗口里路由器成功发出的包数 | **要改写**：不是“一段 200 ms 的固定停顿”，而是“某个 200 ms 窗口里 AP 一个包都没送出”。下限是 200 ms 窗口的粒度，真实空窗可以更长（作者说帧总延迟可超过 1000 ms，第 5 页 Fig. 5） |
| 86.19% | 表 1（第 5 页）：在这类停顿帧里，**86.19%** 的帧至少有一个 200 ms 窗口送出 0 个包；窗口内送出 1/2/3/4 个包的各占 0.29/0.39/0.36/0.29%；5、[6,10)、[10,20)、[20,50)、>50 个包的各占 0.78/2.55/2.86/2.46/3.82% | 数对，但意思是“**停顿帧中**有 86.19% 对应空窗”，不是“86.19% 的卡顿由 Wi-Fi 造成”（后者没有这么写） |
| 与“竞争”的关系 | 图 8（第 6 页）：窗口内送出 0 包的概率随同频其他发射机的空口占用率上升；占用率 >80% 时是 <20% 时的 **74.5 倍**。我从文字里能对上的端点是 0.02% 和 1.49%（1.49/0.02=74.5）；中间三档的数从文本提取里顺序对不上，不引 | 【原文】 |
| 周边 AP 数和停顿率 | 表 2（第 6 页，8 周的在线云游戏，Wi-Fi 会话）：周边 AP 数 2 / 4 / 6 / ≥8 → 会话数 52349 / 25624 / 14414 / 7976，停顿率 0.08% / 0.17% / 0.42% / 1.34% | 【原文】“停顿”= 帧端到端延迟 >200 ms（脚注 3） |
| 物理层发送不慢 | 一旦拿到发送机会，PPDU 传输很快：92.7% 在 3.5 ms 内完成，最大 7.5 ms（图 7）；PHY TX 延迟 P99.99 <5 ms | 【原文】第 2、5 页 |
| 原因 | 竞争间隔（等信道的时间）在 P99.99 超过 200 ms；归因于 CSMA/CA 的指数退避造成的短期不公平（碰撞后窗口翻倍，别的设备反复抢占，退避计数器被不断冻结） | 【原文】第 2 页 |
| 频段和代际 | 只说“5 GHz Wi-Fi”；Fig. 4 对比 2022-12 与 2024-12 的 5 GHz 会话停顿率分位数，“相似”，作者据此认为硬件升级后竞争造成的尾部停顿仍是主因。**没有 2.4 GHz、没有分 Wi-Fi 5/6/7 的数** | 【原文】第 5 页 |
| 局限 | 来源是腾讯云游戏平台（中国用户），不是美国家庭；停顿的定义是 200 ms；数据和代码没看到公开；作者的方案是改 CW 的算法，仿真里的 5× 是他们自己的结果 | 【原文】/【推测】 |

### 2.3 其他同行评议和一手数据

| 量 | 数值 | 来源和页码 |
|---|---|---|
| 校园规模的延迟分布 | 47000+ 台设备；**每分钟 6 次 ping2 的均值**当作一条记录：中位数约 3 ms，P90 约 20 ms，P99 >250 ms；约 20% 的记录落在“slow（11–20 ms）+ EX slow（>20 ms）”；75% 的设备一周内至少有一次“EX slow 持续超过 1 分钟”的情形 | 【原文】Sui 等 MobiSys 2016，https://www.microsoft.com/en-us/research/wp-content/uploads/2017/01/mobisys_2016b.pdf ，第 4–5 页 §3.1、Table 3。ping2 先发一个包唤醒设备再发第二个，避开设备省电造成的假延迟（第 3 页）。**分钟均值会抹掉秒级尖峰，所以每包的尾巴更重** |
| 2.4 GHz vs 5 GHz | 清华校园：5 GHz 平均延迟比 2.4 GHz 低 35%；原因之一是 5 GHz 上只有一半设备，信道占用率更低（Fig. 5）；占用率 >47.5% 时才出现“EX slow”类，建议占用率 <50% | 【原文】同上第 5、9 页。**这是校园网，不是家庭**；2.4 GHz 的邻居干扰在家里可能更重（推测） |
| 逐包尾巴（二手汇总） | Law 的表 1 汇总了前人测量：“extreme per-packet delays over 300 ms observed at 95ile”；“Wi-Fi last-hop RTT can be higher than 100 ms at 80ile”；“99.9ile Wi-Fi RTT goes above 150 ms even in the 5G band”；帧级：“0.2% of frames suffer high latency more than 100 ms”；“frame delivery latency at 99.9ile exceeds 200 ms” | 【原文，但是引用】Shen & Meng，Law，NSDI 2026，https://www.usenix.org/system/files/nsdi26-shen-yibin.pdf ，第 4 页 Table 1。每一行对应哪篇原论文（ABC、QAir、Augur、Hairpin）、环境是什么，**文本提取里对不上，没有逐个追原文** |
| 空载 AP 的尾巴 | Law 的校园两周实验（TP-Link TL-WDR4310 + OpenWrt）：端到端延迟 >70 ms 时 Wi-Fi 最后一跳占 >95%；这类方法“introduce hundreds of milliseconds of latency at the tail, even when there are no competing flows” | 【原文】Law 第 3–4 页 §2.1；这是自己的 AP 实验 |
| 重传 | ath9k 驱动对同一个包最多重传 31 次再放弃，“can take up to hundreds of milliseconds”，并堵住后面的包 | 【原文】Law 第 2 页 |
| 聚合 | 帧聚合（A-MPDU）把竞争开销摊到多个包上（BLADE 第 4 页 §2），Law 的发送路径里“driver performs frame aggregation and header packing”。**没有找到聚合造成的批量放行或延迟的测量数**（间隔、批大小、延迟增量） | 【原文】机理描述；数值没有 |
| 省电（手机端） | CoNEXT 2016：手机 Wi-Fi 的 PSM 超时 Tip 随机型在 40 ms（Nexus 4）到约 205 ms（Nexus 5）；实际监听间隔为 0，即 1 个信标周期 = **102.4 ms**，所以 PSM 可使 RTT 膨胀 >100 ms；SDIO 总线休眠的空闲门限 50 ms，膨胀均值可达 14 ms（发包间隔 1 s 时） | 【原文】Li 等 CoNEXT 2016，https://lakmalrupasinghe.github.io/papers/CoNEXT16_WiFiMeasure.pdf ，第 4–5、7 页，Table 4。机型是 2013–2015 年的 Android；**笔记本和台式机的省电行为这篇没有测**。【推测】我们的两端多半是电脑，但笔记本用电池、屏幕静止时上行几乎无包，空闲后第一个包（鼠标动）正好撞上唤醒，值得 M1 单独记录 |
| Wi-Fi 6 | Liu & Choi（SIGMETRICS 2023）：10 个客户端的真机测试；下行有 Wi-Fi 5 外部竞争时，CSMA/CA 的平均延迟 48.5 ms（25 Mbps 的 Wi-Fi 6 流量 + 64 Mbps 竞争）→ 170.5 ms（112 Mbps 竞争）；OFDMA 下最大 35.58 ms，“5×”缓解；上行无外部竞争时，CSMA/CA 延迟从 2 个客户端的 1.8 ms 增到 8 个的 22.4 ms，OFDMA 增长慢得多 | 【原文】https://liux4189.github.io/files/sigmetric23_wifi6_cameraready.pdf ，第 12 页 §5.2.1–5.2.2。**这是平均延迟，不是尾巴**；多客户端场景，单台电脑远程桌面的家庭里 OFDMA 的收益小（【推测】） |
| Wi-Fi 6 的另一面 | SCTE/CableLabs 测试（二手）：OFDMA 开启时上行满载最大延迟 161→51 ms，下行半数客户端是 Wi-Fi 5 时最大 98 ms（OFDMA 关 166 ms，纯 Wi-Fi 5 445 ms）；SmallNetBuilder 评测说用 4 部手机没能稳定复现收益；一项机器人控制测试说开 OFDMA 反而延迟和抖动更大 | 【二手】只读到搜索摘要（SCTE 幻灯片下载返回的是网页，不是 PDF）。不用作数值 |
| Wi-Fi 7 MLO | Carrascosa 等：用 WACA 公开频谱占用数据集（拥挤体育场）驱动的**仿真**：两条链路占用对称时 MLO-STR 的 P95 延迟比单链路好一个数量级；不对称时最差比单链路差 **112%** | 【原文】arXiv 2305.02052，https://arxiv.org/pdf/2305.02052 ，第 2 页摘要和贡献段。**不是真机，不是家庭**；没有商用 Wi-Fi 7 AP 的尾延迟实测论文 |

### 2.4 对 A3 的含义

- Wi-Fi 的“尖峰层”现在只能用这几个锚：
  - **分钟均值**：中位 3 ms、P90 20 ms、P99 250 ms（清华）；
  - **帧级尾巴**：>200 ms 的帧占 0.08%（安静）到 1.34%（≥8 个 AP）（腾讯）；
  - **空窗的上限形状**：竞争占用率 >80% 时 200 ms 窗口里送出 0 包的概率 ≈1.5%，<20% 时 ≈0.02%（腾讯）。
- 仿真里的尖峰层要同时有“有多密”和“有多长”两个量；上面只有前者（帧级）。**长度**（空窗持续多久）、**幅度**、**尖峰之间的间隔**，没有一手分布，只能用我们自己的会话记录校准（第 12 节 M1 项 2、3）。
- 2.4 GHz 的差异只能写“比 5 GHz 差，约 35%（校园）”，没有尖峰频率数据。

---

## 3. 美国有线接入（FCC Measuring Broadband America）

### 3.1 报告原文

- 第 13 期固定宽带报告（2022-09、10 月数据，2024-08-09 发布）：https://data.fcc.gov/download/measuring-broadband-america/2023/2023-Fixed-Measuring-Broadband-America-Report.pdf 【原文】，下面的页码是 PDF 页。
- 技术附录：https://data.fcc.gov/download/measuring-broadband-america/2023/Technical-Appendix-fixed-2023.pdf 【原文】。
- 图表数据 xlsx（我下了 Chart06/07/08/16）：同目录 `ChartNN-fixed-2023.xlsx`。
- 目录里还有按月的原始数据 `data-raw-2023-*.tar.gz`（每个 1.6–2.0 GB）和 `validated-data-sept2022...`（1.4 GB）；我没下。
- **覆盖范围**：报告里按技术分三类：DSL、cable、fiber（图表）。第 13 期**没有卫星和固定无线（fixed wireless）**；卫星（GEO）在较早的报告里有：2017 年报告第 16 页写“median latencies of satellite-based broadband services ... 594 ms to 624 ms”，对比地面 11–43 ms（https://data.fcc.gov/download/measuring-broadband-america/2017/2017-Fixed-Measuring-Broadband-America-Report.pdf ）。这和 Starlink 无关。**固定无线（FWA，T-Mobile/Verizon 家庭宽带）MBA 固定报告里没有**。
- **测量方法**（第 13 期第 17 页）：路由器（SamKnows 白盒）到“时间上最近”的测试服务器的 UDP 往返；空闲延迟：每小时 2000 个包（<0.56 个/秒）；满载延迟：在 10 秒的下行/上行速度测试期间每秒 10 个包；3 秒没回的算丢包。测量的是“路由器到服务器”，**不含家里的 Wi-Fi**。
- **满载延迟是第一次报告，ISP 做了匿名处理**（第 18 页脚注 18），并且说明“some ISPs have since introduced AQM ... leading to improved results compared to the measurements from 2022”。

### 3.2 按技术的延迟（美国，2022 年秋，晚高峰 19–23 点周一至周五）

数据来自 Chart 6 / Chart 7 / Chart 16 的 xlsx 和报告第 18、19、39 页；Chart 7 图名写“weighted median”，正文第 19 页写“weighted mean”，**我没法判断是哪个**，下面统一说“按 ISP 加权的值”。

| 技术 | 空闲 RTT（各 ISP 的中位） | 按速度档的加权中位（第 39 页 Chart 16） | 下行满载 RTT（各 ISP 加权值） | 上行满载 RTT（各 ISP 加权值） |
|---|---|---|---|---|
| 光纤（3 家） | 6.66–13.96 ms（技术平均 9.93） | 8.41–14 ms | 27.2 / 31.2 / 88.6 ms | 23.9 / 27.1 / 41.3 ms |
| 有线 cable（5 家） | 11.9–23.87 ms（技术平均 19.09） | 12.67–22.13 ms | 47.4 / 57.8 / 101.6 / 196.2 / 378.7 ms | 88.4 / 155.8 / 226.8 / 230.2 / 251.8 ms |
| DSL（4 家） | 22.8–34.21 ms（技术平均 31.09） | 19.61–61.18 ms（低速档高） | 69.5 / 102.4 / 103.2 / 113.3 ms | 181.3 / 651.2 / 679.2 / 720.0 ms |

- 报告原文：“Measured idle latencies for DSL (between 23 ms to 34 ms) were slightly higher than those for cable (12 ms to 24 ms), and idle latencies were lowest for fiber ISPs (7 ms to 14 ms)”（第 18 页）。
- 周末和非高峰时段的空闲值几乎一样（Chart 6：DSL 30.4–30.8，cable 17.9–18.0，fiber 9.65–9.76）。
- **满载延迟与空闲的差，就是队列**：DSL 上行 +150 到 +690 ms，cable 上行 +65 到 +230 ms，fiber 上行 +10 到 +30 ms。
- MBA 测的是一分钟内的速度测试期间，**不是**家里有人同时在看视频的分布。

### 3.3 丢包

- 峰时（Chart 8，第 19–20 页）：把用户分成“<0.4%”“0.4%–1%”“>1%”三档。≥1% 丢包的用户占比：DSL 0–4%，cable 1–4%，fiber 0–3%；“Within a given technology class, packet loss also varies among ISPs”。大部分用户 <0.4%（Chart 8 按档的数在 `Chart08-fixed-2023.xlsx`，例如 CenturyLink DSL 的 1.5 Mbps 档 75% 的用户 <0.4%、12.5% >1%）。
- FCC 自己说“packet loss of a few tenths of a percent ... is unlikely to significantly affect the perceived quality of most Internet applications”（PDF 第 10 页）。
- MBA 的丢包是**每小时 2000 个包的平均**，看不出突发结构。

### 3.4 数据的许可

- 【原文】第 13 期报告第 7 页：“all methodologies used in the program are fully documented, and all collected data is published for public use without any restrictions”。技术附录第 40 页：“Raw and processed data sets ... are freely and publicly available”。
- 所以：**可以商用；放进开源仓库也没有许可问题**（但原始数据 GB 级，不该放仓库，只放加工出的参数）。
- 注意：FCC 自 2023-08-01 起不再与 SamKnows 合作做固定宽带采集（二手，`industry-network-sim.md` 3.3 节），第 13 期报告仍然是 SamKnows 的数据。

### 3.5 M-Lab NDT、Ookla Open Data 的许可

| 数据集 | 许可 | 可商用 | 能否放进开源仓库 | 来源 |
|---|---|---|---|---|
| M-Lab（NDT 等） | CC0 1.0（“No Rights Reserved”，“available to the public without restriction”；要求引用格式） | 可 | 可以，但数据量大，不放原始 | 【原文页面，由网页摘要工具读出】https://www.measurementlab.net/data/ 。字段（下载/上传速度、RTT、丢包）也是该页说的；NDT7 BigQuery 表的字段文档页返回 404，**MinRTT 等字段名没有核实** |
| Ookla Open Data | **CC BY-NC-SA 4.0**（NC = 非商用，SA = 相同方式共享） | **不可** | 不可 | 【原文】https://raw.githubusercontent.com/teamookla/ookla-open-data/master/README.md “## License”。内容是 z=16 瓦片（约 610 m）的平均值：`avg_d_kbps`、`avg_u_kbps`、`avg_lat_ms`、`avg_lat_down_ms`、`avg_lat_up_ms`（满载延迟，只有 Parquet，稀疏）、`tests`、`devices`；2019Q1 到 2026Q1；平均值不是分布 |

结论：Ookla 的数据**不能**作为我们商用产品参数的来源，哪怕只拟合几个数也有风险；它也只有平均值，对我们的用处很小。不用。

---

## 4. 满载排队（bufferbloat）

### 4.1 能找到的大规模、一手的数据只有四类

| 来源 | 内容 | 数值 | 来源和页码 |
|---|---|---|---|
| FCC MBA 第 13 期 | 美国各 ISP，下行/上行满载延迟 | 见 3.2 节：DSL 上行 181–720 ms，cable 上行 88–252 ms，fiber 上行 24–41 ms；下行 DSL 70–113、cable 47–379、fiber 27–89 | 【原文】第 18–19 页、`Chart07-fixed-2023.xlsx` |
| Comcast 的 AQM 报告（厂商/运营商报告，非同行评议） | 2020-10-01 到 12-31，随机抽样的 RDK-B 有线调制解调器，两种同型号网关（一个开上行 DOCSIS-PIE，一个没开）共 26000+ 次测试，上行满载延迟 | 有 AQM：绝大多数 15–30 ms，分布紧凑在 15–45 ms，**77%** 的结果在 15–30 ms；没有 AQM：平均约 **250 ms**，分布从 15 ms 到几百 ms，只有 1% 的结果在 15–30 ms，最大延迟分布里有的超过 **1 秒** | 【原文】Flickinger 等 2021，https://arxiv.org/pdf/2107.13968 ，第 1、13、15 页（PDF 页码）。Fig. 5/6 的标题在文本提取里与正文说的对不上（疑似图题写反），**以正文为准** |
| Sundaresan 等 SIGCOMM 2011 | SamKnows 4200+ 户（美国）、BISmark 16 户；满载延迟/基线延迟的比值，上行饱和时比下行饱和时高得多；调制解调器的缓冲：2Wire 最坏 800 ms、Motorola 约 1600 ms、Westell **>10 秒**；Comcast 用户“less than one second”但受 PowerBoost 整形影响（延迟从 200 到 700 ms，随放行速率下降 7→2 Mbps 而升） | | 【原文】https://conferences.sigcomm.org/sigcomm/2011/papers/sigcomm/p134.pdf ，第 9–10 页（Fig. 13、14、15）。**这是 2011 年的数据，只能当“最坏情况”的形状参考，不是当前分布** |
| Starlink 的 M-Lab | 见 1.2 节：下载时 RTT 膨胀 2–4×（到 400–500 ms），上传时 P60 ≤100 ms | | 【原文】Mohan 等第 4 页 |

### 4.2 没找到的

- **DSLReports** 的测速已关闭，没有找到它的聚合统计；**Waveform** 没有公开的聚合统计（只能导出单次 CSV）；Waveform 的评级阈值（添加延迟 A+ <5 ms，A <30，B <60，C <200，D <400，F ≥400）来自第三方网站 dnsrobot.net，【二手】。
- **“家里有人在看视频、传文件时”的分布**：MBA 和 Comcast 的数据都是**测速仪器自己加的负载**（10 秒的饱和 TCP 流），不是“家里有人看视频”的自然负载。自然负载下的上行/下行排队延迟分布，**没有找到公开测量**。【推测】看视频主要占下行，且视频流是自适应的，通常不会把队列填满；传文件、云备份、上传视频是上行的主要来源。
- 以 FCC 和 Comcast 的数据为准，上行满载延迟是**有 AQM / 没有 AQM 两个峰**，不是一个单峰：AQM 的有无是主因（第 18 页脚注 18 也说了 2023 年 ISP 已陆续加 AQM）。

### 4.3 对 A3 的含义

- 队列层应该写成“队列深度（按时间计）”的混合分布，且**上行和下行分开**：上行深（没有 AQM 时 150–700 ms 量级），下行浅一些。
- AQM 开关要做成一个独立的层参数，混合比例没有数据（Comcast 是同型号对照，不是占比）；这是第 12 节 M1 要补的。

---

## 5. 美国 4G、5G

### 5.1 5G（低频、中频、毫米波）

来源：Narayanan 等 SIGCOMM 2021 “A Variegated Look at 5G in the Wild”（https://par.nsf.gov/servlets/purl/10291906 ）、Hassan 等 SIGCOMM 2022 “Vivisecting Mobility Management in 5G Cellular Networks”（https://par.nsf.gov/servlets/purl/10355260 ），都是明尼苏达大学；数据来自美国三大运营商（Verizon、T-Mobile 为主）。

| 量 | 数值 | 来源和页码 |
|---|---|---|
| 毫米波最低 RTT | ≈6 ms（离终端 ≈3 km 的 Speedtest 服务器，终端静止、视距约 50 m，Verizon，明尼阿波利斯）；比 2019 年的早期部署（5Gophers）低约 50%。RTT 随终端到服务器的距离增大，320 km 时翻倍 | 【原文】SIGCOMM'21 第 3–4 页 §3.2。**报告的是同一设置重复测试的第 95 分位的“峰值性能”**（第 3 页），**不是用户实际体验的分布**，作者自己强调了 |
| 低频 5G 对毫米波 | 低频比毫米波多 ≈6–8 ms；低频和毫米波都比 LTE 好 6–15 ms | 【原文】SIGCOMM'21 第 4 页 |
| 下行吞吐 | 毫米波多连接 >3 Gbps（8CC 载波聚合，静止、视距） | 【原文】SIGCOMM'21 第 4 页 |
| SA 与 NSA | T-Mobile 低频 SA 与 NSA 的 RTT 没有显著差别；10 km 的驾车路线上的切换次数：SA-n71 13 次、NSA+LTE 110、LTE 30、SA+LTE 38、全部频段 64；NSA 里约 90 次是 4G↔5G 的垂直切换 | 【原文】SIGCOMM'21 第 5 页 Fig. 9 |
| 小区直径（NSA 实测） | 低频 1.4 km、中频 0.73 km、毫米波 0.15 km | 【原文】SIGCOMM'22 第 2 页 |
| 切换频率（高速公路） | 5G 平均每 0.4 km 一次（4G 每 0.6 km）；毫米波每 0.13 km，中频每 0.35 km，低频每 0.4 km，SA 每 0.9 km；130 km/h 驾驶一小时平均 553 次 5G 切换 | 【原文】SIGCOMM'22 第 2 页 |
| **切换时长** | NSA 5G 平均 **167 ms**（比 4G 的 **76 ms** 多 119%）；数据面中断时间比 4G 长 1.4×；SA 的时间波动大。准备阶段占 NSA 切换时长的 41% | 【原文】SIGCOMM'22 第 2、6 页 |
| 切换时的 RTT | NSA 双连接模式：中位 RTT 变化 1–4%（4G 链路继续传）；仅 5G 模式：中位 RTT 增加 37–58% | 【原文】SIGCOMM'22 第 5 页 Fig. 7（TCP BBR 驾车测试） |
| 切换对吞吐 | 毫米波切换时吞吐常常掉 ≈2 Gbps；低频切换时降幅小。体积视频应用延迟在毫米波切换时增加 107%（这是应用层延迟，不是网络 RTT） | 【原文】SIGCOMM'22 第 5 页 |

**【推测】对 A3 的限制**：以上都是移动（驾车）或静止的手机/测试机，不是“放在窗口边当家庭宽带用的 5G”（FWA）；**我们的核心用户几乎不会在驾车时远程桌面，所以切换层主要用于“蜂窝作被控端或控制端，人在走动/换基站”的场景**，频率比驾车低很多。

### 5.2 4G（LTE）

来源：Huang 等 SIGCOMM 2013 “An In-depth Study of LTE”（https://web.eecs.umich.edu/~zmao/Papers/lte_sigcomm13.pdf ），美国某大都市某运营商的 22 个基站、2012-10-12 起 240 小时的被动抓包。**数据是 2012 年的，已经很旧。**

| 量 | 数值 | 页码 |
|---|---|---|
| 握手 RTT（TCP SYN） | 表 1：该网络 50% 70 ms，95% 467 ms（TCP 握手 RTT，在抓包点测得，不是端到端的完整 RTT）。同一表里还有与前人 3GTest/4GTest/SpeedTest 的比较，个别格子的行列对应从文本提取里不清楚，**只引这个网络的 70/467** | 第 6 页 Table 1 |
| 空闲到连接的提升延迟（RRC promotion） | 25%/50%/75% 分位：**319/435/558 ms**；尾时间 10 s（超过后回到空闲） | 第 4–5 页（尾时间在第 4 页，提升延迟在第 5 页） |
| 重传率 | 下行 TCP 重传率中位数 0.06%，38.1% 的流为 0；链路层重传和缓冲把传输层丢包藏起来了 | 第 6 页 |
| 缓冲 | 下行在途字节可 >1200 KB；大流（>1 MB）里约 10% 的在途字节 >200 KB；RTT 随在途字节增长 | 第 6–7 页 |

**对我们的含义**：
- “空闲后的第一个包要多等 300–550 ms”是蜂窝上最该进“延迟噪声/唤醒”层的一项。远程桌面在屏幕静止时上行包很稀，进入空闲后鼠标一动就撞上；这个数是 2012 年 LTE 的，5G 的值我没找到。
- 4G 在美国的现代 RTT 和带宽分布，**没有读到一手的新论文**；`industry-network-sim.md` 里的比利时 4G、Mahimahi 的 Verizon/AT&T LTE 轨迹仍是主要的容量变化来源。

### 5.3 数据集的许可

| 数据集 | 许可 | 可商用 | 放仓库 | 来源 |
|---|---|---|---|---|
| Lumos5G（明尼苏达，毫米波） | CC BY 4.0 | 可，需署名 | 1.53 MB 的 CSV，可以放（但要署名；也只有一个环线） | 【原文】https://ieee-dataport.org/node/3598 （见 `industry-network-sim.md` 3.6 节） |
| 5Gophers v1.0（明尼苏达，美国多运营商） | CC BY 4.0，下载要 IEEE DataPort 登录或联系 fivegophers@umn.edu | 可，需署名 | 未确认下载方式 | 【二手】搜索摘要 |
| SIGCOMM'21 5G 的 artifact（`SIGCOMM21-5G/artifact`）和 SIGCOMM'22 的 artifact | **仓库里没有找到 LICENSE 文件**（main/master 都 404）；README 说数据集小则放仓库、大则放共享 Google Drive；5G Tracker 工具只对学术和非营利免费 | 未知 | 不放 | 【原文】README；LICENSE 404 |
| 比利时 4G、挪威 3G | 页面只要求引用 | 未写明 | 不放 | 见 `industry-network-sim.md` 3.4、3.5 节 |

---

## 6. ITU-T G.1050

原文：G.1050（07/2016），https://www.itu.int/rec/dologin_pub.asp?lang=e&id=T-REC-G.1050-201607-I!!PDF-E&type=items （114 页，6.7 MB，ITU 网页标注“Freely available items”）【原文】。该建议书网页另有 Annex H 的仿真输出（Zip，780 MB，同样标注 freely available），**我没下**。页码为 PDF 页。

### 6.1 它是什么、不是什么

- **范围**：只含 **GPON 和 DSL** 两种接入；“Characteristics of other access technologies such as cable television (CATV) and wireless are for further study”（PDF 第 10 页的局限条款）。所以**没有 cable、没有 Wi-Fi、没有蜂窝、没有卫星**。
- **数据来源和年份**：“snapshot of actual network data provided by anonymous IP service providers and IP network equipment manufacturers in the **2010** timeframe”（第 7 页）。
- **不含异常事件**：“Abnormal events such as link failures and route flaps (and the packet reordering that such events can cause) are **not included** in this Recommendation”（PDF 第 10 页）。
- **模型方式**：离散事件仿真（核心到局域网的拓扑），干扰流来自真实抓包；UDP 包记录每包延迟或丢弃，TCP 流（iPerf）记录每 200 ms 的可用带宽、延迟、丢包；输出文件放在 Annex H。
- **版权**：“All rights reserved. No part of this publication may be reproduced, by any means whatsoever, without the prior written permission of ITU.”（前言页）。能读、能下载，**不能把表整张抄进仓库**；引用单个数值和出处是常规做法，但要不要法务看一下我没有意见。

### 6.2 参数表（Table 6，第 16 页印刷页码，PDF 第 24 页）

| 量 | 单位 | A：well-managed | B：partially-managed | C：unmanaged（互联网） |
|---|---|---|---|---|
| 单程延迟 | ms | 20–100（区域内）；90–300（洲际） | 20–100（区域内）；90–400（洲际） | 20–500 |
| 抖动（峰峰值） | ms | 0–50 | 0–150 | 0–500 |
| 连续丢包（持续时间） | ms | 只有随机丢包 | 40–200 | 40–10000 |
| 连续丢包的发生率 | 次/秒 | 只有随机丢包 | <1e-3 | <1e-1 |
| 随机丢包 | % | 0–0.05 | 0–2 | 0–20 |
| 乱序 | % | 0–0.001 | 0–0.01 | 0–0.1 |

测试用例（第 7–9 表，PDF 第 26–27 页）的参数：每种用例给出交换机数（3–15 台）、核心链路总延迟（10–250 ms）、DSL/GPON 的接入速率和残余误码率。例如“u1…u7”（Unmanaged，印刷页码 18，PDF 第 26 页）：核心延迟 10/25/50/75/100/200/250 ms；GPON 下行 35/35/25/15/5/15/5 Mbps、上行 35/15/25/5/2/5/2 Mbps；DSL 下行 24/18/12/6/3/6/3 Mbps、上行 3/1.5/1.5/1/1/1/1 Mbps。“接入速率和损伤程度代表 2010 年的典型值”（印刷页码 17，PDF 第 25 页）。**表头在文本提取里与用例的对应有错位，我只引 Table 6 的范围和用例里的数值，没有逐格核对每个用例属于哪张表。**

### 6.3 对 A3 的用法

- **能用的**：Table 6 的范围可以作为“互联网 C 档”上界的参照（单程延迟 20–500 ms、抖动 0–500 ms、随机丢包 0–20%）；它写的是**范围（min to max）**，**没有分布**，因此不能直接当“分布先验”。
- **不能用的**：它不含 Wi-Fi、cable、蜂窝、卫星；不含链路故障和路由抖动；数据是 2010 年的。我们的家用网络类型它基本覆盖不到。
- 因此 G.1050 在 A3 里只作为**交叉检验**：我们抽出来的“unmanaged”网络，其延迟、抖动、丢包不应远超 C 档范围；如果超过，要解释为什么（例如 Starlink 边界尖峰或 Wi-Fi 空窗）。

---

## 7. 企业网、酒店、咖啡馆

**结论：没有现代的公开测量。**搜索了公共 Wi-Fi 热点测量（IMC/PAM/SIGCOMM/MobiSys）和企业/大型事件 Wi-Fi 测量，找到的是：

| 来源 | 内容 | 与我们的相关性 |
|---|---|---|
| Wifi-Reports（Seattle，MobiSys 2009） | 13 个热点（Starbucks、Tully's、咖啡馆、旅馆附近），1 周，802.11b/g；每个 AP 测 8–13 次。TCP 吞吐中位数从 <100 kbps 到 >5 Mbps（starbucks 1 和 oasis 超过 5 Mbps）；“response time”（取 google.com 首页，含 DNS）从 <100 ms 到几秒；13 个里 8 个有零星的连接失败；DHCP 失败；**1 个 AP（bookstore 的官方 AP）封掉了所有非特权 TCP 端口和除了 DNS、NTP 之外的所有 UDP 端口**，另有 3 个 AP 封了非 DNS 的 53 端口；全部封 NetBIOS | 【原文】https://cseweb.ucsd.edu/~dlmccoy/papers/mobisys09-wifireports.pdf ，第 2–5 页 §2。唯一读到的“咖啡馆/旅馆”测量，**但是 2009 年、802.11b/g、没有分位数**。它说明：**UDP 被封是真的会发生**（13 个里 1 个）——我们的 QUIC 数据报要有“UDP 被封就回落”的路径，这不是 A3 的参数而是 L2 的测试 |
| 清华校园网（MobiSys 2016） | 学术企业 WLAN，47000+ 设备；见 2.3 节 | 最接近“企业网”的大规模测量；**不是企业办公网**，也没有策略限速/DPI 的信息 |
| Bosch 等 “How is your event Wi-Fi doing?” | 大型活动的密集 Wi-Fi 外场试验（802.11n/ac） | 【二手】只读到搜索摘要，没读原文 |
| Intel IT Wi-Fi 6 白皮书 | 称延迟增加倍数：Wi-Fi 6 为 3.5×，Wi-Fi 5 为 14×（厂商文档） | 【二手】只读到搜索摘要；厂商产出，不用 |
| 公共热点 Captive Portal 隐私研究 | 蒙特利尔 67 个热点（含咖啡馆、旅馆、机场），只做隐私分析，没有性能数据 | 【二手】与性能无关 |

- 企业网的“限速器/整形”：`industry-network-sim.md` 里引的微软挑战赛仿真含 “traffic policing”，那是仿真场景，不是对企业网的测量。
- **酒店和咖啡馆的共享出口限速、客户隔离、UDP/QUIC 封锁的比例，没有公开数据。**需要我们自己的会话记录（第 12 节）。

---

## 8. 数据集许可汇总（本次涉及的）

| 数据集 | 许可证 | 可商用 | 放进开源仓库 | 证据 |
|---|---|---|---|---|
| FCC MBA | 报告原文：“published for public use without any restrictions” | 可 | 可（只放加工后的参数） | 【原文】第 13 期报告第 7 页 |
| M-Lab（NDT 等） | CC0 1.0 | 可 | 可 | 【原文页面，摘要工具读出】 |
| WetLinks | CC BY-SA 4.0 | 可，需署名，SA | 原始数据不放 | 【原文】README |
| LENS | 代码 GPL-3.0；数据 README 写 CC BY-SA 4.0，Zenodo 记录字段写 CC BY 4.0（不一致） | 视实际下载的那一份 | 不放；GPL 代码不引入 | 【原文】README、Zenodo API |
| Mohan 等 WWW'24 数据 | 没读到（mediaTUM 页面被挡） | 未知 | 不放 | – |
| LEO Access Link Traces | CC BY 4.0 | 可，需署名 | 不放（91.69 GB） | 【二手】 |
| Lumos5G | CC BY 4.0 | 可，需署名 | 可，署名 | 【原文】DataPort 页（旧文档） |
| 5Gophers | CC BY 4.0 | 可，需署名 | 未确认下载方式 | 【二手】 |
| SIGCOMM'21/'22 的 artifact | 没有 LICENSE 文件 | 未知 | 不放 | LICENSE 404 |
| Ookla Open Data | **CC BY-NC-SA 4.0** | **不可** | 不可 | 【原文】README |
| ITU-T G.1050（含 Annex H 仿真输出） | © ITU，未经许可不得复制 | 未知 | 不整表抄；引用单个数值并注明出处 | 【原文】前言页 |
| 论文里的图表 | 论文版权 | – | 只引数值，不贴图 | – |

---

## 9. 对已有文档的更正

1. `industry-network-sim.md` 第 2 节第 2 条：“G.1050 ... 2016 版改为离散事件仿真，加入链路故障、路由抖动、乱序；...2008 timeframe”。
   - 离散事件仿真：对（2011 版起；2016 版 §1 和摘要）。
   - **“加入链路故障、路由抖动”：与 2016 版原文相反**，PDF 第 10 页写明“not included”；乱序是 Table 6 里的一个参数（0–0.1%），不是“事件”。
   - **年份**：2016 版写的是 2010 年前后（2011 版的摘要页没有这句）；我没找到“2008”的出处。
   - 它**只含 GPON 和 DSL 两种接入**。
2. `industry-network-sim.md` 第 7 节 Wi-Fi 一行：“BLADE 论文称 ‘packet delivery droughts’（约 200 ms 无包放行，与 86.19% 卡顿相关）”。改为：**200 ms 是 AP 的统计窗口；86.19% 是“帧延迟 >200 ms 且有线 RTT <50 ms 的帧”里至少有一个 200 ms 窗口送出 0 个包的比例**（第 2.2 节）。
3. `industry-network-sim.md` 第 3.9 节：WetLinks 的下载和许可现已读到（CC BY-SA 4.0，GitHub）；“15 秒重配置只读到摘要”现已读原文；相位 12/27/42/57 来自 Tanveer 等，不是 Mohan 等。
4. `a2-plan.md` 3 节“延迟阶跃”一行：“Starlink 模式按周期 15 秒、相位可设（具体相位要读到一手来源再定默认值）”——现在可以定：默认 t ≡ 12 (mod 15) s（UTC 每分钟的 12/27/42/57 秒，来源 Tanveer 等第 2 页），并建议同时加“边界尖峰”部分（开头约 140 ms、结尾约 75 ms）。
5. `industry-network-sim.md` 第 3.3 节：FCC 原始数据的许可已读到（“without any restrictions”），文件清单也有了。

---

## 10. 建议：网络类型 × 层参数的初值表（A3 的先验）

说明：
- 数值能直接当先验的，写“起点值（范围）”和来源编号；没有数据的写“**没有数据，用我们自己的日志校准**”。
- 来源编号：
  - [F] FCC 第 13 期报告和 Chart xlsx（3 节）；
  - [C] Comcast AQM 报告（4.1 节）；
  - [M] Mohan 等 WWW 2024；
  - [T] Tanveer 等 arXiv 2307.00402；
  - [K] Casparsen 等 arXiv 2601.08439；
  - [W] WetLinks；
  - [L3] LEOScope/T3P；
  - [S] Sui 等清华 MobiSys 2016；
  - [B] BLADE；
  - [Lw] Law；
  - [LC] Liu & Choi；
  - [H] Hassan 等 SIGCOMM'22；
  - [N] Narayanan 等 SIGCOMM'21；
  - [Hu] Huang 等 SIGCOMM'13；
  - [G] ITU-T G.1050；
  - [WR] Wifi-Reports；
  - [Sundaresan] Sundaresan 等 SIGCOMM 2011；
  - [CoNEXT16] Li 等 CoNEXT 2016。
- 我们的两端各有一段接入，P2P 的单程延迟 = 两端接入 + 中间的核心网；下表的“基础单程延迟”只是**一端接入的**，取 RTT 的一半（【推测】MBA 测的是路由器到最近服务器的 RTT，已含部分核心网）。
- Wi-Fi 是接入网**之上**再叠一层（最后一跳），表里 Wi-Fi 行单独列，使用时与有线行叠加。
- “满载排队”是**加在基础延迟上的最大排队延迟**（=满载 RTT − 空闲 RTT），按方向写（下行=往被控端的方向看，视频流向）。

### 10.1 初值表

| 网络类型 | 容量（下/上，Mbps） | 基础单程延迟（接入一端） | 满载排队（下行 / 上行） | 丢包 | 延迟噪声 | 周期阶跃/中断/换路 | 限速器、反向路径 |
|---|---|---|---|---|---|---|---|
| **光纤** | 没有读到按档位的上行；广告档位 75–500 下（[F] Chart 16）。上行：没有数据，用日志 | 3–7 ms（空闲 RTT 7–14，[F]） | 下行 +20 / +80；上行 +10 / +30（[F] Chart 7） | 峰时 ≥1% 的用户占 0–3%，大多数 <0.4%（[F]）；突发结构没有数据 | 没有数据 | 没有数据 | 限速器：没有数据；反向路径：上行容量较小，见左 |
| **有线 cable，无 AQM** | 广告档位 50–800 下（[F] Chart 16）；上行没有数据 | 6–12 ms（RTT 12–24，[F]） | 下行 +30 到 +350；**上行 +65 到 +230，均值 ≈ +230（[C] 无 AQM 平均约 250 ms，尾部 >1 s）** | 峰时 ≥1% 的用户占 1–4%（[F]） | 没有数据 | 没有数据 | 没有数据；Sundaresan 2011 提到 PowerBoost 式整形会让延迟随放行速率变化（[Sundaresan] 第 10 页，2011 年） |
| **有线 cable，有 AQM（DOCSIS-PIE）** | 同上 | 同上 | 上行总延迟 15–30 ms（77% 在这段），即**排队 ≈10–25 ms**（[C]） | 同上 | 同上 | 同上 | 同上 |
| **DSL** | 广告档位 1.5–100 下（[F]）；上行：没有数据 | 10–17 ms（RTT 23–34；低速档到 61，[F]） | 下行 +45 到 +90；**上行 +150 到 +690（[F] Chart 7）** | 峰时 ≥1% 的用户占 0–4%（[F]） | 没有数据 | 没有数据 | 没有数据 |
| **固定无线（FWA）** | **没有数据**（MBA 固定报告不含） | 没有数据 | 没有数据 | 没有数据 | 没有数据 | 没有数据 | 用我们自己的日志校准 |
| **Wi-Fi 5 GHz，安静** | 没有数据（取决于接入网和 PHY 速率） | +1–2 ms（分钟均值中位 3 ms，[S]） | Wi-Fi 自己的排队：没有数据 | 链路层重传隐藏了多数丢包，传输层丢包没有数据 | 尖峰密度：**帧延迟 >200 ms 的帧占 0.08%**（周边 2 个 AP，[B] 表 2）；分钟均值 P90 ≈20 ms、P99 >250 ms（[S]）；**尖峰时长、幅度分布没有数据** | 没有数据 | – |
| **Wi-Fi 5 GHz，拥挤（≥8 个周边 AP）** | 同上 | 同上 | 同上 | 同上 | **帧延迟 >200 ms 的帧占 1.34%**（[B] 表 2）；200 ms 窗口送出 0 个包的概率：占用率 >80% 时 ≈1.5%，<20% 时 ≈0.02%（[B] 图 8） | 同上 | – |
| **Wi-Fi 2.4 GHz** | 没有数据 | 比 5 GHz 平均高约 35%（[S]，校园，推测在家里更差） | 没有数据 | 没有数据 | 没有尖峰分布；用 5 GHz 的参数乘一个系数，系数用我们自己的日志校准 | 没有数据 | – |
| **Wi-Fi 省电（手机/笔记本空闲后）** | – | 空闲后第一个包 +0–102.4 ms（1 个信标周期，[CoNEXT16]；设备相关，2013–2015 年的 Android） | – | – | 作为“省电唤醒”层：触发条件 = 空闲 ≥40–205 ms（设备相关），延迟 ≈ U(0,102.4) ms | – | 电脑上的真实值：没有数据，用日志 |
| **4G LTE** | 比利时 4G 的轨迹（见 `industry-network-sim.md` 3.5）和 Mahimahi 的美国 LTE 轨迹 | RTT 中位 70 ms → 单程 ≈35 ms；P95 467 ms（[Hu]，2012，一个美国运营商） | 深队列：下行在途字节 >200 KB 的占 10%，RTT 随之增长；没有当前数据 | 下行 TCP 重传中位 0.06%（[Hu]） | **空闲提升：319/435/558 ms（25/50/75 分位，[Hu]），尾时间 10 s** | 切换：4G 平均 76 ms 的数据面中断（[H]）；每 0.6 km 一次（驾车） | – |
| **5G 低频/中频（NSA 为主）** | 没有当前的美国分布，用 Mahimahi/Lumos5G 以外的日志校准 | 比 LTE 低 6–15 ms RTT（[N]，静止、峰值设置） | 没有数据 | 没有数据 | 空闲提升：没有 5G 的数 | **切换：NSA 平均 167 ms，SA 波动大；驾车每 0.35–0.4 km 一次，仅 5G 模式时 RTT 中位 +37–58%**（[H]） | – |
| **5G 毫米波** | 下行可 >3 Gbps，遮挡时骤降并回落 LTE（Lumos5G 轨迹；切换时吞吐掉 ≈2 Gbps，[H]） | 最低 RTT ≈6 ms（静止、近服务器、P95，[N]） | 没有数据 | 没有数据 | – | **切换：每 0.13 km 一次**（驾车，[H]）；中断时长按 NSA 平均 167 ms | – |
| **Starlink** | 美国 M-Lab P75：50–100 下 / 4–12 上（[M]）；欧洲专用终端：下行 200 上下、上行 13–17 中位，日内低谷约 -20%，下雨约 -17%（[W]） | RTT：美国 P75 <50 ms（[M]），欧洲到服务器 62±3（[W]）；bent-pipe ≈40 ms，单程 ≈20–30 ms | 下载时 RTT +200 到 +400（[M]）；上行 P60 ≤+100（[M]） | **均值 1.0%，28–33% 的测量含丢包，≈65% 的丢包事件为单包；>80% 的测量 ≤1%，≈3% ≥5%**（[W]） | 周期内条带几 ms（[T]）；P99 >2.4× 中位数，最大 11–16×（[L3]） | **周期 15 s，相位 12/27/42/57 秒（UTC）；边界开头约 140 ms +74 ms 平均尖峰，结尾约 75 ms**（[T][K]）；窗口之间水平变化 | – |
| **企业网** | **没有数据** | **没有数据**（清华校园 WLAN 分钟均值中位 3 ms 作 Wi-Fi 段参考） | 没有数据 | 没有数据 | 没有数据 | 没有数据 | 限速器：没有数据；UDP 封锁：**没有数据**（2009 年一个 AP，[WR]） |
| **酒店/咖啡馆** | 2009 年 Seattle 13 个热点：TCP 吞吐中位 <0.1 到 >5 Mbps（802.11b/g，[WR]）；现代值没有数据 | 取页面首页响应 <100 ms 到几秒（[WR]） | 没有数据 | 没有数据 | 没有数据 | 没有数据 | **没有数据**，只有“UDP 封锁在 13 个里出现 1 次”（[WR]） |
| **互联网“C 档”参考上界** | – | 20–500 ms（G.1050 表 6） | – | 随机丢包 0–20%；连续丢包 40–10000 ms，<0.1 次/秒 | 抖动 0–500 ms 峰峰值；乱序 0–0.1% | **不含链路故障和路由抖动**（G.1050 PDF 第 10 页） | 仅用于检查我们抽出的参数是否过分 |

使用时的几条规矩（【推测】）：
- 初值只给**先验的中心和范围**，不当分布。需要分布的格子（尖峰时长、省电触发、突发丢包长度、AQM 比例、限速器）统一标成“没有数据”，A3 先用宽先验，再按我们自己的记录收窄。
- 美国有线的满载排队是目前最硬的一格（来自 FCC），仿真里的有线家庭网络优先把它做对。
- Starlink 的三部分（窗口水平、边界尖峰、窗口内条带）参数有出处，但**样本在欧洲**；美国终端的边界数据没读到。
- 5G/4G 的“空闲后首包”只有 2012 年 4G 的数。

---

## 11. 我们自己的会话记录（M1）最该采哪些量

目的：补上面表里“没有数据”的格子。下面只列新增的量，现有 M1 记录（`m1-recorder.md`）里已有的不重复。均为**统计量，不记地址、不记内容**。

| 优先级 | 要采的量 | 补哪一格 | 怎么采 |
|---|---|---|---|
| 1 | **网络标签**：本端接入类型（有线 / Wi-Fi + 频段 2.4/5/6 GHz + 标准 11n/ac/ax/be + RSSI/噪声 + 协商速率 + 信道 + 周边 AP 数 / 蜂窝 + 制式 LTE/NR + 频段类别 / 卫星），及对端的同类标签 | 全表：把每次会话归类到网络类型，才有“每类的分布” | 系统 API（Windows WLAN API、macOS CoreWLAN、Linux nl80211）；放在 M1 文件头的“设备”字段旁（现在写着“有线还是 Wi-Fi”，要细化）；蜂窝/星链用户自己选也行，【推测】星链可以由出口 ASN 或 PoP 域名推断，但 M1 不记地址 |
| 2 | **送达空窗**：以 100 ms 和 200 ms 为窗口，“发送端发了包但接收端一个包都没收到”的窗口数、连续窗口数；同时记同一时段的反馈往返和单程最小值（排除有线/对端问题） | Wi-Fi 尖峰的**时长和间隔**（现有数据只有帧级比例，没有时长分布）；BLADE 的 drought 的口径 | 接收端按窗口计数，每秒一条 |
| 3 | **空闲后首包延迟**：距上一个包的空闲时长 ≥50/100/200/500/1000/5000 ms 后，第一个到达的包的单程/往返延迟，对比之后 10 个包的中位 | 省电唤醒（Wi-Fi 约 0–102.4 ms，蜂窝 RRC 约 300–550 ms）、电脑上的真实值 | 接收端记；分网络标签统计 |
| 4 | **每个延迟样本的 UTC 秒数 mod 15**（或把每秒的最小/最大延迟连同 UTC 秒记下来） | Starlink 的相位（验证 12/27/42/57）和美国边界尖峰的形状 | 文件头已有会话开始的墙上时间（Unix 微秒）和单调时钟；只要保证墙上时间 NTP 已对时；离线分析里按 (t mod 15) 聚合。**M1 现有设计已经够用，只要把墙上时间的“是否已对时”记进去** |
| 5 | **丢包结构**：每次连续丢包的长度分布（1、2、3–5、>5 个包）；丢包前后 200 ms 的延迟走势（用来区分“拥塞丢包”和“随机丢包”）；丢包与 15 s 边界的相对位置（星链） | 丢包层（独立 vs 突发，平均突发长度）；Starlink 边界丢包（没有一手数据） | 接收端根据序号统计 |
| 6 | **自己加的负载 vs 外来负载下的排队**：发送码率接近送达速率时，往返延迟减最小往返的分布，分上行和下行；同时记发送速率和送达速率 | 满载排队（上行 vs 下行，AQM 有无） | 现有“估计器状态”里应该已有；要加的是“当时是不是我们自己填满的”这个标记 |
| 7 | **路径变化事件**：Wi-Fi 漫游（BSSID 变）、蜂窝基站/制式变（LTE↔NR）、本机 IP/路由变；事件前后的丢包窗口长度、恢复到稳定延迟的时间 | 切换层（Wi-Fi 漫游没有任何公开数据；蜂窝只有驾车数据） | 平台 API 事件 + 现有的路径记录 |
| 8 | **UDP/QUIC 是否被封、回落到中继的次数和原因**；中继路径的容量上限（稳定速率）和“一阵突发后丢包”的形态（令牌桶特征） | 限速器；企业、酒店、咖啡馆（完全没有数据） | 打洞/连接阶段的结果；速率和丢包三元组（发送速率、送达速率、丢包）每秒一条 |
| 9 | **到达间隔直方图**（微秒级分辨率，按 0.1 ms 分桶）：批量放行的特征（聚合、接收端忙） | 批量放行（聚合），接收端层 | 接收端每秒一个直方图，64 桶 |
| 10 | **逐秒送达速率的上界**（只在发送端用满码率时记） | 容量层的实际取值；固定无线 FWA、5G 的“没有数据” | 现有的送达速率记录里加“是否被我们限制”的标志 |

按“补空白的价值 / 实现成本”排序，1、2、3、5 最值得先做：1 让数据能分类；2、3、5 直接补 Wi-Fi 尖峰、省电唤醒、丢包结构这三个最大的空白。

---

## 12. 没查到或没读到的

1. **Starlink**：
   - 美国终端在 15 s 边界处的丢包率分布（只有他人转述）；
   - Mohan 等 WWW'24 数据的许可（mediaTUM 页面被反爬挡住）；
   - LEOScope 代码的许可（论文只给了引用，没点开）；
   - LENS 数据许可的两处不一致，需看下载的那一份；
   - 断线的频率和时长分布。
2. **Wi-Fi**：
   - 尖峰的时长和间隔分布，没有；
   - 2.4 GHz 的尖峰数据，没有，只有清华“5 GHz 比 2.4 GHz 平均低 35%”；
   - 聚合的批量放行和延迟，没有数值；
   - Wi-Fi 6 的尾延迟只有平均值（Liu & Choi）；SCTE 的数只读到摘要；
   - Wi-Fi 7 没有商用设备实测；
   - Law 表 1 里前人测量各行对应的原论文和环境没有追；
   - BLADE 图 8 中间三档的数。
3. **有线**：固定无线（FWA）没有；自然负载（家里有人看视频、传文件）下的排队延迟分布没有；Waveform、DSLReports 的聚合统计没有；Chart 7 是“加权中位”还是“加权均值”没法判断；有线/光纤/DSL 的上行广告速率没有读（只读了下行的 Chart 16）；M-Lab NDT7 的字段名没有核实。
4. **蜂窝**：4G 的当前美国分布（只有 2012 年的一个运营商）；5G 的空闲到连接延迟（RRC 提升）；FWA；Hassan 等和 Narayanan 等的数据许可；5Gophers 的下载方式。
5. **G.1050**：只读了 2016 版正文，没有下载 Annex H 的 780 MB 仿真输出，也没有逐格核对 Table 7–9 属于哪张表（文本提取里表头错位）。
6. **企业网、酒店、咖啡馆**：没有现代的公开测量；2009 年的 Wifi-Reports 是唯一读到的，Bosch 等和 Intel 白皮书只读到摘要。
7. **UDP/QUIC 被封的比例**：没有查（只有 Wifi-Reports 里 13 个里 1 个的数）；这影响中继成本，但不属于本次的七项。

---

## 11. 主会话审查（2026-11-28）

1. **接受**。第 9 节的更正已经改进 `a2-plan.md`（Starlink 的相位），`industry-network-sim.md` 开头加了指向这里的说明。G.1050 只引用了范围和少量数值并注明页码，没有整表照抄。
2. **对 A3 的三点要求**（从这份数据来）：
   - **上行满载延迟按“有 AQM / 没有 AQM”两个峰抽**，不当成单峰。cable 没有 AQM 时满载 RTT 88–252 ms（FCC 第 13 期），开了 AQM 只有 15–30 ms（Comcast）。家里有人在传文件时，我们的反馈和重传请求走的就是这条上行。
   - **Starlink 的相位按会话随机抽**（见 a2-plan 第 3 节），边界尖峰作为延迟噪声的一项。
   - **Wi-Fi 的尖峰没有现成的分布**，现在只有锚点：周边 AP 2 到 8 个以上，超过 200 ms 的帧从 0.08% 升到 1.34%；校园网分钟均值的 P99 超过 250 ms。A3 用这些锚点约束合成的尖峰过程，参数以后用我们自己的会话记录校准，用户同意后才上传，匿名化。
3. **合规**：
   - Ookla Open Data 不可商用，不用；
   - WetLinks（CC BY-SA 4.0）要放进仓库的话，派生的轨迹也得用 CC BY-SA，先只在本地用；
   - FCC、M-Lab 可以用；
   - 这几条要并进 `industry-network-sim.md` 第 3.11 节的许可汇总，在 A3 动手时做。
4. **M1 记录仪该采的量**（第 10 节末尾的清单）并进 R5–R9 的设计。最要紧的三项：
   - Wi-Fi 还是有线，以及频段；
   - 空闲 RTT 和满载 RTT 的差（判断 AQM）；
   - 送达空窗（连续多久一个包都没到）的直方图。
