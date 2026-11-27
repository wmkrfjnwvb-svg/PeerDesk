# 网络仿真可用的公开数据集：补充调研（按许可证分三张表）

调研日期：2026-11-27。只读页面和许可证，没有下载任何数据本体。
已有内容见 `industry-network-sim.md` 第 3 节（微软 MMSys 2024、Lumos5G、Starlink LEO Access Link Traces、Mahimahi 自带轨迹、Pantheon、Puffer、挪威 3G、比利时 4G），本文不重复。

## 0. 先给结论

- 许可证明确、可商用、可再分发、而且内容对我们有用的，只有 20 个左右，其中真正"每包"粒度的只有 Starlink 单向时延、NeSt-VR 的包级抓包、M-Lab 的 NDT 数据，其余多是每秒或更粗。
- 找不到任何一个同时给出"每包时延 + 容量真值"的公开数据集，微软 MMSys 2024 仿真部分仍是唯一带容量真值的。本次新找到的、有真值的只有 NeSt-VR（人为设置的带宽上限、丢包、抖动，见 §1 第 7 项）。
- 屏幕共享、远程桌面的流量记录：只找到 Polito 的 RTC 抓包（含 screen sharing 类型，但页脚是 All rights reserved，归入"许可不明"）和 MIT VNAT（含 RDP，无许可声明）。没有一个许可明确的。
- 云游戏：Polito 的 Stadia/GeForce NOW/PS Now 抓包样本是 CC BY，但要 IEEE DataPort 订阅才能下。
- 重要的"不能用"：Ookla 开放数据（CC BY-NC-SA）、RIPE Atlas（商用要预先许可）、MAWI（只限研究）、Cloudflare Radar（据搜索摘要为 CC BY-NC）。
- 方法说明：页面内容由抓取工具摘要后返回，下面标"原话"的是工具从页面抽出的原文，Zenodo 的 License 字段、IEEE DataPort 的许可句、GitHub 的 README 都是页面上的字样。没读到原文的我都标了"未核实"。

字段说明：
- 粒度：每包 / 每秒 / 每 10 秒 / 小时汇总等。
- 容量真值：有 = 数据里带链路容量的真值（人为设置或专用工具量得）；无 = 只有 TCP 吞吐、送达速率这类下界或观测值。

---

## 1. 表一：许可证允许商用和再分发的（可用）

### 1.1 总览

| # | 数据集 | 网络类型 | 粒度 | 许可证 | 容量真值 | 获取难度 |
|---|---|---|---|---|---|---|
| 1 | M-Lab（NDT、TCP INFO、Packet Header、Traceroute） | 全球家庭宽带、移动、企业，含卫星 | 每次测速一条；TCP INFO 为连接内快照；Packet Header 为每包 | CC0 1.0 | 无（TCP 观测，BBR 估计 BW） | BigQuery 或 GCS，免费 |
| 2 | Raca 等 爱尔兰 4G LTE（Zenodo 1219679） | 4G，静止、步行、汽车、电车、火车 | 每秒 | CC BY 4.0 | 无（吞吐，含信道指标） | 直接下载，2.3 MB |
| 3 | 5Gophers v1.0（UMN，IEEE DataPort） | 商用 5G（毫米波和中频），三个城市 | 吞吐和 RTT 的 CSV，间隔页面未写 | CC BY 4.0 | 无 | IEEE 免费账号 |
| 4 | Starlink 单向时延（Zenodo 16275284，卡尔斯塔德大学） | Starlink 下行、上行 | 每包 | CC BY 4.0 | 无（有每包单向时延和序号） | 直接下载，5.9 GB |
| 5 | Starlink 时延和下行吞吐（Zenodo 10020034） | Starlink | 页面未写，README 里 | CC BY 4.0 | 无 | 直接下载，1.2 GB |
| 6 | WetLinks（Osnabrück，GitHub sys-uos/WetLinks） | Starlink，含天气 | 吞吐每秒（15 秒一次测试）；RTT 约 3 分钟一轮 | CC BY-SA 4.0 | 无 | git clone |
| 7 | NeSt-VR（Zenodo 14832268） | Wi-Fi 6，VR 串流（低延迟视频，近似远程桌面负载） | 包级抓包（tshark CSV）+ 会话日志 | CC BY 4.0 | 有（人为限速、丢包、抖动、重复包设置已知） | 直接下载，110.9 MB |
| 8 | MONROE-SOMETIME（Zenodo 1300512） | 欧洲 4G，可用带宽测量 | 日志和包抓取 | CC BY-SA 4.0 | 有（Pathload、Yaz 估计的可用带宽，不是严格真值） | 直接下载，654.5 MB |
| 9 | MONROE 挪威火车覆盖（Zenodo 47707） | 2G/3G/4G 覆盖，沿火车线路 | 按网格块 | CC0 1.0 | 无（只有接入制式，不是性能） | 直接下载，34 MB |
| 10 | MONROE 网页体验（Zenodo 2574157） | 11 个商用移动网，4 国 | 每次页面访问 | CC BY 4.0 | 无 | 直接下载，332.7 MB |
| 11 | MONROE 云服务（Zenodo 1136576） | 移动网到云服务的 DNS、TCP/TLS、UDP ping | 每个样本每 4–24 小时一次 | CC BY 4.0 | 无 | 14.2 GB 数据库文件 |
| 12 | JackRabbit 无线 ISP 时延（Zenodo 13388093，IMC 2024） | 固定无线宽带 ISP | 每分钟 JSON；子网统计每 10 秒；RTT 直方图 4 ms 分箱 | CC BY 4.0 | 无 | 11.7 GB |
| 13 | Wi-Fi 车到路侧（Zenodo 6884095） | Wi-Fi 802.11n/ac/ad，移动车辆 | 每秒（含重传数、位置） | CC BY 4.0 | 无 | 22.8 MB |
| 14 | 瑞典 5G 交互测试（Zenodo 13987386，CSCN 2024） | 5G，交互场景 QoS | 页面未写，README 里 | CC BY 4.0 | 无 | 8 MB |
| 15 | Lumos 自适应流（GitHub GreenLv/Lumos） | 真实移动网，视频会话 | 每个视频块 | 页面标 MIT（LICENSE 全文未读到） | 无（块级下行带宽、送达时间） | git clone |
| 16 | FCC Measuring Broadband America 原始数据 | 美国固定宽带，家庭路由器上的 Whitebox | UDP 时延/丢包按小时汇总；VoIP 抖动；吞吐 10 秒一次 | 无许可文件；FCC 技术附录称 "open data ... freely and publicly available" | 无 | data.fcc.gov 直接下载，每月 1.6–2.0 GB |
| 17 | Polito 云游戏抓包样本（Stadia、GeForce NOW、PS Now） | 云游戏流量（含不同网络条件） | pcap 每包 | CC BY（IEEE DataPort License 字段） | 无 | 要 IEEE DataPort 订阅，5.25 GB |
| 18 | 高铁带宽（北京交大，IEEE DataPort） | 中国高铁线路移动网 | 每 10 秒 | CC BY（License 字段） | 无 | 要订阅，数据量页面未写 |
| 19 | CRAWDAD isti/rural/packet_loss（IEEE DataPort） | 室外 802.11b | 每帧（收到或丢失，信号强度） | CC BY（License 字段） | 无 | 要 IEEE 账号，数据 2005–2006 年，较旧 |
| 20 | Nokia 5G-IANA 路测（Zenodo 12664724） | 5G 路测的上下行和 RTT | KML 文件，页面未写粒度 | CC BY 4.0 | 无 | 522 KB，很小 |

### 1.2 逐个说明

#### 1. M-Lab（Measurement Lab）
- 内容：全球众测。NDT（TCP 吞吐和时延，2009 至今）、TCP INFO（内核 TCP 统计，2019 至今）、Packet Header（TCP 流的包头 pcap，2019 至今）、Traceroute（2013 至今）、Reverse Traceroute（2024 至今）、Wehe（流量区别对待，2018 至今）。
- 规模：每月数千万次测速量级（规模数字页面未写，未核实）。
- 许可证原话："available to the public without restriction under a No Rights Reserved Creative Commons Zero Waiver"，另一句 "released under CC0 1.0 Universal with no restrictions on use"。网站自己的原创内容是 CC BY 4.0，和数据分开。
- 来源：https://www.measurementlab.net/data/
- 商用和再分发：可以（CC0）。
- 获取：加工过的数据用 BigQuery 查询；原始数据在 Google Cloud Storage。免费，但 BigQuery 查询费用要自己的账号付。
- 字段（ndt7 文档，https://www.measurementlab.net/tests/ndt/ndt7/）：`TCPInfo.RTT`、`RTTVar`、`MinRTT`，`BytesAcked`、`BytesRetrans`、`Retrans`、`Lost`、`DeliveryRate`，`BBRInfo.BW`、`BBRInfo.MinRTT`；汇总字段 `LossRate`（"Loss rate from the lifetime of the connection"）。
- 对我们的用处：各国各 ISP 的家庭宽带、移动网的 RTT 和丢包分布，给"网络类型参数的初值"和分布；Packet Header 理论上可以重建每包到达序列（要读文档和抽样确认，未核实）。
- 局限：是 TCP 测速，一次约 10 秒，不是连续轨迹；没有容量真值，`BBRInfo.BW` 是估计；ndt7 的 TCPInfo 快照间隔页面没写（页面只说吞吐是开始和结束两个快照的平均），未核实。

#### 2. Raca 等爱尔兰 4G LTE 数据集（MMSys 2018）
- 内容：两家爱尔兰运营商，客户端侧蜂窝指标（信道、上下文、小区）加上下行吞吐；静止、步行、汽车、电车、火车五种移动方式。用 G-NetTrack Pro 采集。
- 规模：135 条轨迹，平均约 15 分钟，吞吐 0–173 Mbit/s，每秒一个样本。另有 ns-3 仿真数据。有缺失值（多是服务小区 GPS）。
- 许可证原话（Zenodo License 字段）："Creative Commons Attribution 4.0 International"。注意 UCC 自己的页面没有许可声明，只要求引用，两处不矛盾但要以 Zenodo 为准。
- 来源：https://zenodo.org/records/1219679 ；UCC 页面 https://www.ucc.ie/en/misl/research/datasets/ivid_4g_lte_dataset/
- 商用和再分发：可，需署名。
- 获取：直接下载 LTE_Dataset.zip（2.3 MB）。
- 用处：4G 移动场景的每秒吞吐轨迹，含"吞吐掉到 0"的断流；是少数覆盖电车、火车、公交的公开轨迹。
- 容量真值：无，吞吐是下载大文件得到的，是下界。

#### 3. 5Gophers v1.0（UMN）
- 内容：三家运营商（两家毫米波、一家中频）、三个城市的商用 5G 测量；文件夹：吞吐、RTT、用户移动；毫米波专项（视距、非视距、朝向、距离、网页加载、CDN 对比）。
- 规模：5Gophers-v1.0.zip 253.82 KB（页面所写，是否为全部数据未知；论文称研究总数据量超过 15 TB）。
- 采集年份：2019 年前后（5G 刚商用；页面没给日期，只给了上传日 2021-02-11）。
- 许可证原话："5Gophers 1.0 dataset is licensed under the Creative Commons Attribution 4.0 International License."
- 来源：https://ieee-dataport.org/open-access/5gophers-v10
- 商用和再分发：可，需署名。下载要登录（免费 IEEE 账号）。
- 用处：5G 毫米波的遮挡和突降；有 RTT 文件，比纯吞吐信息多。
- 容量真值：无。

#### 4. Starlink 单向时延（LEONET 2025）
- 内容：卡尔斯塔德（瑞典）一个视野开阔的用户终端，2025 年 4 月，用同一块硬件时间戳网卡的两个口测，收发共用一个时钟，所以单向时延可信。
- 规模：5.9 GB，上下行各 1,437 个文件；每个文件三列：序号 `seq_nr`、发送时间戳（纳秒）、单向时延（纳秒）。
- 许可证原话（Zenodo License 字段）："Creative Commons Attribution 4.0 International"
- 来源：https://zenodo.org/records/16275284
- 商用和再分发：可，需署名。
- 特别注意：页面说"缺的序号不一定是丢包，偶尔是硬件时间戳读取失败"，用来算丢包要先处理。
- 用处：每包级别的 LEO 时延，包括 15 秒重配置附近的跳变；是目前找到的最细的卫星数据，与已知的 IEEE DataPort Starlink 数据互补，而且这个不用订阅。
- 容量真值：无。

#### 5. Starlink 时延和下行吞吐（Zenodo 10020034）
- 内容：2023-09-13 至 09-17，延迟和下行吞吐，脚本名 irtt-iperf3.py（页面）。
- 规模：1.2 GB（data-20230913-20230917.tar.zst）。
- 许可证原话："Creative Commons Attribution 4.0 International"
- 来源：https://zenodo.org/records/10020034
- 商用和再分发：可。粒度和字段在 README.txt，本次没读到，未核实。
- 容量真值：无。

#### 6. WetLinks（TMA 2024）
- 内容：两个欧洲测点（Osnabrück、Enschede），2023-10 至 2024-03，吞吐、RTT、traceroute、Starlink 终端元数据，配气象站数据。
- 粒度：`iperf_cleaned_seconds` 是每秒吞吐（只留满 15 秒的运行）；其他间隔页面没写，论文称 3 分钟一轮测量。
- 许可证原话（README）："This work is licensed under a Creative Commons Attribution-ShareAlike 4.0 International License"
- 来源：https://github.com/sys-uos/WetLinks
- 商用和再分发：可商用；是 ShareAlike，我们再分发或基于它做衍生数据集时，衍生部分要用同样的许可证。仅用来做内部测试输入、不再分发数据本身，不触发。
- 容量真值：无。
- 补充：论文还说天气（降雨）对 Starlink 的影响，可用来做"链路突降"场景。

#### 7. NeSt-VR（Zenodo 14832268）
- 内容：用 ALVR v20.6.0 的分支在 Wi-Fi 6 上串流 VR，数据是 Section VI（恒定 100 Mbps，人为设置带宽上限、丢包 0.5/1/2%、重复包、抖动 0–6/10/20 ms）和 Section VII（自适应码率；单用户移动；两用户并发；重叠 AP 在 40/80 MHz）。
- 格式：ALVR 会话 JSON（管线延迟、帧时序、RTT、丢包、吞吐、抖动、码率决策）；tshark 包级 CSV（相对时间、IP、包长、负载，前 22 字节是 ALVR 的帧和包元数据）。
- 规模：110.9 MB；发布 2025-02-07。
- 许可证原话："Creative Commons Attribution 4.0 International"
- 来源：https://zenodo.org/records/14832268
- 商用和再分发：可，需署名。
- 用处：这是最接近我们负载的公开数据：低延迟、帧驱动的实时视频串流，在 Wi-Fi 上，带已知的人为损伤，所以有"真值"（设置值）。可以拿来验证我们的量尺在"丢包 + 抖动 + 限速"下的响应是否与真实系统一致。
- 局限：VR 是 90 fps 高码率，不是静止后爆发的桌面；损伤是人为设置的，不是真实 Wi-Fi 干扰。

#### 8. MONROE-SOMETIME（Zenodo 1300512）
- 内容：三次 4G 测量活动，Yaz 和 Pathload 测可用带宽、D-ITG 测可达吞吐，含日志和包抓取，加处理后的文件。2018 发布。
- 规模：654.5 MB（DATASET_MONROE_SOMETIME.tar.gz）。
- 许可证原话："Creative Commons Attribution Share Alike 4.0 International"
- 来源：https://zenodo.org/records/1300512
- 商用和再分发：可，注意 SA。
- 用处：有"可用带宽"估计，比纯 TCP 吞吐更接近容量；但估计工具本身有误差，不是严格真值。粒度页面没写，未核实。

#### 9–11. MONROE 其他三个 Zenodo 数据集
- 47707（CC0）：沿挪威火车线的 2G/3G/4G/无服务覆盖，按网格块；只有接入制式，没有吞吐、时延，用来做"覆盖空洞和制式切换的统计"。
- 2574157（CC BY 4.0）：约 200 万次网页会话，11 个商用移动网，4 国；论文称切换（handover）的页面加载惩罚最明显。可以提取"切换时的时延增量"。
- 1136576（CC BY 4.0）：2017 年 4 周的 DNS、TCP/TLS、UDP traceroute 和 ping，14.2 GB。用处很弱，列出来是为了完整。
- 来源：https://zenodo.org/records/47707 、https://zenodo.org/record/2574157 、https://zenodo.org/records/1136576

#### 12. JackRabbit 无线 ISP（IMC 2024）
- 内容：一家固定无线 ISP 内部的被动时延测量，2023-10-30 至 12-01；原始数据每分钟一个压缩 JSON；子网统计每 10 秒；RTT 直方图 4 ms 分箱。
- 规模：11.7 GB。
- 许可证原话："Creative Commons Attribution 4.0 International"
- 来源：https://zenodo.org/records/13388093
- 用处：宽带接入网里，真实用户流量的时延分布和随时间的变化；是被动测量，没有丢包字段的话要读文档（未核实）。

#### 13. Wi-Fi 车到路侧（Zenodo 6884095）
- 内容：移动车辆与固定 AP 之间 802.11n/ac/ad 三种 Wi-Fi 的性能，2019-08 两天采集，每秒一行，含 GPS、Wi-Fi 层重传。
- 规模：22.8 MB；轨迹编号 302–405。
- 许可证原话："Creative Commons Attribution 4.0 International"
- 来源：https://zenodo.org/records/6884095
- 用处：Wi-Fi 链路质量随移动的变化（含重传），可当"Wi-Fi 越走越差"的场景；不是家庭 Wi-Fi。

#### 14. 瑞典 5G 交互测试（CSCN 2024）
- 内容：5G 的"超越 eMBB"QoS/QoE，交互类测试，2023 年；一个 8 MB 的 zip 加 README。
- 许可证原话："Creative Commons Attribution 4.0 International"
- 来源：https://zenodo.org/records/13987386
- 粒度和字段页面没写（README 据搜索摘要含 ping 和 RTT 字段，未读 README 本身，未核实）。

#### 15. Lumos 自适应流数据集（GreenLv/Lumos，INFOCOM 2022 / TMC 2023）
- 内容：590 个视频会话（392 个 ABR、198 个恒定码率），每个约 5–5.5 分钟，每行一个视频块：下行带宽、连接类型、信号强度、码率、块大小、应用吞吐、送达时间。2019-12 至 2021-05 采集。
- 许可证：页面显示标签 "MIT license"，指向 LICENSE 文件，文件全文没读到，所以只能标"页面标注为 MIT"。数据是否也适用 MIT，未核实。
- 来源：https://github.com/GreenLv/Lumos
- 注意：这个 Lumos 和 UMN 的 Lumos5G 不是同一个。
- 容量真值：无，粒度是视频块，不是每包。

#### 16. FCC Measuring Broadband America 原始数据
- 内容（SamKnows 文档整理）：`curr_udplatency.csv`（UDP 时延和丢包）、`curr_udpjitter.csv`（VoIP 抖动，含上下行抖动、时延）、`curr_ping.csv`、`curr_dlping.csv`/`curr_ulping.csv`（负载下的时延，可看缓冲膨胀）、`curr_httpget*.csv`（下载，含 TCP 重传）、`curr_lct_ul.csv`（轻量容量测试）、Netflix 和 Hulu QoE 等。
- 粒度（FCC 2023 技术附录原文）：UDP 时延"Approximately two thousand packets are sent within a one-hour period"，按小时汇总一条；VoIP 测试 64 kbps、160 字节包；下载和上传测试"we report only one interval of 10 seconds"。所以是汇总，不是每包。
- 规模：2023 年 1–7 月原始数据每月一个 tar.gz，约 1.6–2.0 GB，共 12.5 GB；每年一个目录（2012–2023），另有 2011 年的目录。
- 许可：没有 LICENSE 或使用条款文件（目录页没有）。FCC 2023 技术附录 4.3 节原话："The data collected in this program are made available as open data for review and use by the public. Raw and processed data sets, mobile testing software, and the methodologies used to process and analyze data are freely and publicly available."
- 来源：https://data.fcc.gov/download/measuring-broadband-america/2023/ ；技术附录 https://data.fcc.gov/download/measuring-broadband-america/2023/Technical-Appendix-fixed-2023.pdf ；字段 https://samknows.zendesk.com/hc/en-gb/articles/360003076317-Guide-25-Using-Raw-Data
- 商用和再分发：按"美国政府项目、官方称 open data、自由公开"可判断为可用，但数据由承包商 SamKnows 采集，官方没有写明版权归属和许可证名称，所以这一条的置信度比 CC0 低。FCC 自己页面声明原始数据"未经审计，不适合用于比较 ISP"（搜索摘要转述，FCC 网页本机 403，未读原文），我们只用来做统计分布，不用来评价 ISP，不冲突。
- 备注：SamKnows 文档页脚有 "© SamKnows Ltd 2017 / All Rights Reserved"，这是文档的版权，不是数据的。
- 用处：美国家庭宽带的 RTT、丢包、抖动、负载下时延的真实分布（按小时），可用来定"家庭宽带"类型参数。
- 容量真值：无。

#### 17. Polito 云游戏抓包样本
- 内容：Stadia、GeForce NOW、PS Now 的游戏会话 pcap 样本，是论文数据集的子集（论文共 225 条）。
- 规模：5.25 GB（Perna_Gaming_captures.zip），2021-01 上传。
- 许可证：页面 License 字段 "Creative Commons Attribution"（链接到 CC BY 4.0），没有整句。
- 来源：https://ieee-dataport.org/documents/network-analysis-cloud-gaming-stadia-geforce-now-and-psnow
- 限制：页面写 "This dataset requires an IEEE DataPort Subscription to access"，下载要付费订阅。
- 用处：真实云游戏的包大小、帧间隔、码率适配对网络条件的响应，最接近"实时视频、输入反向小包"的负载。

#### 18. 高铁带宽（北京交通大学，IEEE DataPort）
- 内容：中国高铁线路上移动网带宽，每条记录有时间戳、服务器地址、TCP/UDP、带宽（MB/s），每 10 秒一个点。
- 许可证：License 字段 "Creative Commons Attribution"（CC BY 4.0）；创建日期 2026-10-29；需要订阅；页面只列了样例文件，完整大小没写。
- 来源：https://ieee-dataport.org/documents/bandwidth-dataset-along-high-speed-railway-lines
- 用处：高速移动。粒度 10 秒太粗，只能当宏观起伏参考。

#### 19. CRAWDAD isti/rural/packet_loss
- 内容：室外 802.11b 点对点，每帧一行（收到或丢失、信号强度、序号），1/2/5.5/11 Mb/s，约 20 万帧一次；2005-03 至 2006-04。
- 许可证：License 字段 "Creative Commons Attribution"，要求注明来源；需 IEEE 账号。
- 来源：https://ieee-dataport.org/open-access/crawdad-istirural
- 价值：每帧丢包序列可以直接当"突发丢包"模型的真实样本，但是 2005 年的 11b，和今天的 Wi-Fi 差得远，低优先级。
- 说明：CRAWDAD 其余数据集许可各不相同，见表三。

#### 20. Nokia 5G-IANA 路测
- 三个 KML 文件，2023-01-29 一次路测的上下行吞吐和 RTT；CC BY 4.0；522 KB；来源 https://zenodo.org/records/12664724 。体量太小，用处有限，列出备查。

---

## 2. 表二：不能用的（附原因）

| # | 数据集 | 许可证原话 | 不能用的原因 | 来源 |
|---|---|---|---|---|
| 1 | Ookla Open Data（Speedtest，全球固定和移动网络性能，zoom 16 瓦片） | "CC BY-NC-SA 4.0"（AWS 开放数据注册表的 License 字段；GitHub 页面写 "Creative Commons Attribution-NonCommercial-ShareAlike 4.0 International"） | 非商用，且 SA。另外是按 610 米瓦片平均，没有时间序列，本来也用处不大 | https://registry.opendata.aws/speedtest-global-performance/ ；https://github.com/teamookla/ookla-open-data |
| 2 | RIPE Atlas 测量结果 | 服务条款 4.5："Any commercial use of the RIPE Atlas Data is subject to prior permission by the RIPE NCC." | 商用需要事先书面许可；数据按条款公开但不是开放许可证 | https://www.ripe.net/about-us/legal/ripe-atlas-service-terms-and-conditions/ |
| 3 | MAWI（WIDE 骨干网抓包） | "You may use WIDE traffic data for only research purposes."；"Actions that trespass upon users' privacy are prohibited." | 只限研究；没有再分发条款 | https://mawi.wide.ad.jp/mawi/ |
| 4 | Starlink Measurements 01/2022–06/2022（Zenodo 15303863，26.1 GB，ping、traceroute、HTTP、TLS、卫星数量、探针断连事件） | Zenodo License 字段："GNU General Public License v3.0 or later" | 项目规定不引入 GPL/AGPL 代码；GPL 用在数据上，"衍生"的边界不清，风险高，不碰 | https://zenodo.org/records/15303863 |
| 5 | uccmisl/5Gdataset（Raca 5G，GitHub） | 仓库 LICENSE 是 "GNU GENERAL PUBLIC LICENSE Version 3, 29 June 2007"，页面标 GPL-3.0 | 同上，GPL-3.0。同一份数据在 Zenodo 3751194 上 License 字段写 "No further description."，两处不一致，见表三 | https://github.com/uccmisl/5Gdataset |
| 6 | Cloudflare Radar 下载和 API | 搜索摘要转述：通过 API 或直接下载的数据按 CC BY-NC 4.0（另一个镜像页写 CC BY 4.0，两处冲突） | 非商用；冲突没有核实原文，先按不能用处理 | https://radar.cloudflare.com/about（本次没读到原文，未核实） |
| 7 | CAIDA 多数数据集（含被动 Internet 包头抓包） | 未读到条款原文（页面重定向到新目录，没取到） | 据一般了解，CAIDA 的被动数据要申请，并限学术使用；本次未核实，所以放这里是按"要签协议"保守处理 | https://catalog.caida.org/ （未读到条款，未核实） |
| 8 | 微软 MMSys 2024（已知的，复述） | 只有 "AS IS" 免责声明 | 不在本次范围，归类见 `industry-network-sim.md` 3.1 和 3.11：需向微软确认商用 | — |

---

## 3. 表三：许可不明的（页面没有写，或来源互相冲突）

| # | 数据集 | 内容 | 许可情况 | 对我们的价值 | 来源 |
|---|---|---|---|---|---|
| 1 | Polito RTC 应用对比抓包（Meet、Teams、Zoom 等 13 个应用） | 334 个 pcap，类型含音频、视频、屏幕共享，带设备和系统；2020 | 页面无数据许可；页脚 "© 2026 SmartData@PoliTO – All rights reserved" | 高：这是找到的唯一明确含"屏幕共享"流量类型的公开抓包，可联系作者问授权 | https://smartdata.polito.it/a-comparative-study-of-rtc-applications-the-dataset |
| 2 | Purdue 视频会议数据集（Webex、Meet、Teams、Zoom） | 约 2.6 GB；场景：恒定带宽、变化带宽、TCP 背景流、UDP 背景流；有排队日志和 QoE；2026 PAM | 页面没有许可、条款或使用声明（只有 NSF 资助致谢） | 高：直接是"变化带宽下视频会议行为"，可联系作者 | https://www.cs.purdue.edu/homes/fahmy/datasets/VCAPurdue |
| 3 | Polito/MONROE 移动网开放数据集 | 6 国 27 家运营商 120 个节点，2018–2019，Tstat 的 TCP/UDP 流日志加 RSRP/RSRQ/RSSI，约 46 GB | 页面只写 "The dataset is open to the community to reproduce our results or perform further analysis." | 中：流级而非包级，规模大 | https://smartdata.polito.it/mobile-networks-open-dataset/ |
| 4 | Raca 4G 在 UCC 自己页面 | 同表一第 2 项 | UCC 页面无许可，只要求引用；Zenodo 上是 CC BY 4.0。建议引用 Zenodo 的版本 | — | https://www.ucc.ie/en/misl/research/datasets/ivid_4g_lte_dataset/ |
| 5 | Raca 5G 在 Zenodo 3751194 | 5G 轨迹（静止、汽车；视频和下载），80.7 MB | Zenodo License 字段 "No further description."；GitHub 上是 GPL-3.0 | 中：5G 轨迹有价值，需要作者澄清授权 | https://zenodo.org/record/3751194 |
| 6 | Oboe 带宽轨迹（USC-NSL/Oboe） | 500 个真实视频会话，每块起止时间和平均带宽 | 仓库页面没有许可；README 说 "being made available to the research community"（研究社区） | 低：块级 | https://www.github.com/USC-NSL/Oboe |
| 7 | MIT Lincoln Lab VNAT | 33,711 条连接、272 小时、36.1 GB，10 种应用，含 RDP、SSH、Netflix、Zoiper | 页面没有许可条款 | 中：含 RDP，但是在隔离虚拟子网里生成的，不是真实网络路径 | https://www.ll.mit.edu/r-d/datasets/vpnnonvpn-network-application-traffic-dataset-vnat |
| 8 | Cloudflare AIM 数据（经 M-Lab 发布） | 网络质量测试 | M-Lab 公告页只有网站自己的 CC BY 4.0 页脚，数据没写；是否 CC0 或 NC 未知 | 中 | https://measurementlab.net/blog/cloudflare-aimscoredata-announcement |
| 9 | OpenNetLab 轨迹（2021 MMSys 挑战赛） | gym 仓库测试目录里有轨迹，据挑战赛讨论 | 代码 BSD-3-Clause；README 说明编译后的二进制含 ns-3（GPL）；轨迹本身没有许可，仓库页面也没看到轨迹目录 | 低 | https://github.com/OpenNetLab/gym |
| 10 | Intel Open Wi-Fi RTT 数据集（IEEE DataPort） | 约 3 万次办公室 Wi-Fi FTM 测距 RTT | 搜索摘要称文件带 BSD-3-Clause 标识；页面没读到，未核实 | 低：FTM 测距，不是 ping 时延 | https://ieee-dataport.org （DOI 10.21227/h5c2-5439，未读） |
| 11 | BWiFi "Best Wi-Fi Zone"（IEEE DataPort） | 家和办公室 Wi-Fi，平均 ping 时延、RSSI、吞吐 | 搜索结果里 License 字段为空；未读页面 | 中 | https://ieee-dataport.org/node/14627 （未读） |
| 12 | CRAWDAD 其他数据集 | 各种 Wi-Fi 和移动轨迹 | CRAWDAD 页面只写 "All CRAWDAD datasets will be Open Access after approval."；已迁到 IEEE DataPort，每个数据集 License 字段不同，要逐个看 | 中 | https://crawdad.org/ |
| 13 | LEOScope 测试床数据 | 5555 个 Starlink 测量客户端（页面所述） | LEOScope 代码开源；数据是否公开、许可，网站没写 | 中 | https://leoscope.surrey.ac.uk |
| 14 | Starlink 机上 Wi-Fi（arXiv 2508.09839）、Northwestern 机上 Wi-Fi 研究 | 机上卫星网络测量 | 前者数据链接是 "[To be added later]"；后者数据发布情况没读到 | 中：覆盖 GEO/LEO 机上网络 | https://arxiv.org/abs/2508.09839 |
| 15 | Hassan 等 SIGCOMM 2021/2022 美国驾车 5G/4G 路测 | 多运营商驾车，含切换 | 没找到仓库和许可 | 中 | 未核实 |
| 16 | 微软 Teams 约 100 万条通话网络轨迹（arXiv 2510.12265） | 带 QoE 的 Teams 通话轨迹 | 摘要没有发布声明 | 高（如果公开） | https://arxiv.org/abs/2510.12265 |

---

## 4. 推荐优先接入的 5 个

排序依据：许可证干净、数据格式简单、不需要订阅、直接补足已知集合的空白。

1. **M-Lab NDT + TCP INFO（CC0）**
   - 理由：唯一明确 CC0、全球规模、覆盖家庭宽带、移动、卫星用户。可以用 BigQuery 抽取各国各 ISP 的 RTT、最小 RTT、丢包、重传的分布，给"网络类型"的参数设初值，并且能回答"某类网络上 P95 丢包是多少"。
   - 提醒：是 TCP 测速，不是连续轨迹；先抽样看字段再决定能提取什么。

2. **Starlink 单向时延（Zenodo 16275284，CC BY，5.9 GB）**
   - 理由：每包单向时延和序号，2025 年新数据，不需要订阅，补足已知的 IEEE DataPort 轨迹；时间戳是硬件打的，时延可信。可直接做每包回放。
   - 先决：处理时间戳读取失败造成的序号缺口。

3. **Raca 4G LTE（Zenodo 1219679，CC BY，2.3 MB）加 5Gophers v1.0（CC BY）**
   - 理由：4G 的 135 条每秒轨迹，覆盖电车、火车、汽车、步行，体积极小、立刻能用；5Gophers 补 5G 毫米波的遮挡突降和 RTT。两个都是 CC BY。
   - 提醒：吞吐是下界，不是容量，使用时要把"没用满"的段标出来，避免把送达速率当链路能力（正好是我们 CLAUDE.md 里的反例）。

4. **NeSt-VR（Zenodo 14832268，CC BY，110.9 MB）**
   - 理由：低延迟帧驱动串流、Wi-Fi 6、包级抓包，并且损伤（限速、丢包、抖动）是已知设置，有可对照的"真值"；是我们用来验证"量尺对不对"的最好的公开对照。
   - 提醒：VR 码率远高于桌面，只拿"损伤响应"，不拿负载。

5. **FCC MBA 原始数据（2023，每月 1.6–2.0 GB）**
   - 理由：美国家庭宽带的 UDP 时延、丢包、VoIP 抖动、负载下时延，真实用户的小时级分布，给"家庭宽带"类型定参数；对应我们先做美国市场的判断。
   - 提醒：官方称 open data 但没有明确的许可证名称，上线前建议发邮件向 FCC 确认；只做统计分布使用。
   - 候补：WetLinks（CC BY-SA，卫星 + 天气）和 MONROE-SOMETIME（CC BY-SA，有可用带宽估计），SA 的义务需要法务确认后再用。

另外建议立刻发邮件问授权（价值高但许可不明）：Polito RTC 抓包（含屏幕共享）、Purdue 视频会议数据集（变化带宽下的 Webex/Meet/Teams/Zoom）。

---

## 5. 没查到或没核实的

- FCC 网页（fcc.gov）本机访问 403：raw-data-releases 页面、各期 raw data 页、移动数据页都没读到；FCC 原始数据是否被 FCC 自己声明过"公共领域"没有确认。能确认的只有技术附录里的 open data 那句话和 data.fcc.gov 的目录。
- FCC 移动端 App 数据（`curr_udplatency.tar`，约 255 MB，据搜索摘要）：内容和粒度没读到。
- FCC `curr_udpjitter.csv` 的 US 数据实际样本：只读到了 SamKnows 的字段表，没读到真实文件。
- M-Lab：每月数据规模、ndt7 TCPInfo 快照间隔、Packet Header 的采样比例，没确认。
- CAIDA：条款页重定向未取到，所有 CAIDA 结论未核实。
- 国内公开数据集（清华、中科院等）：搜索没有找到明确的公开蜂窝或 Wi-Fi 轨迹；只找到北京交大的高铁带宽（IEEE DataPort，要订阅）。国内数据集覆盖基本是空白。
- Wi-Fi 的家庭和办公室每包时延抖动数据：只找到车载、户外 11b、VR 串流这些，没有找到"家庭 Wi-Fi 干扰下的每包时延抖动"的公开数据集；这部分可能要自己采集。
- 企业网、手机切网络或断网的记录：只有 MONROE 网页体验（切换惩罚）和 IMC 2024 切换论文（Zenodo 14222500，但页面上只有论文 PDF，不是数据，所以没算数据集）。Wi-Fi 与蜂窝切换的专门数据没找到。
- 跨洋路径：M-Lab Traceroute 和 NDT 有，RIPE Atlas 因许可被排除；其他（例如 WonderNetwork 之类的商业延迟矩阵）没有查。
- 屏幕共享、远程桌面的流量记录：除了表三里的 Polito 和 VNAT，没找到别的；TeamViewer 检测论文是否发布了数据没确认。
- 云游戏：Loria 的 cloud-gaming-traces 站点（两次访问都是 503）没读到；2025 年 13 个游戏 500 多个会话的数据集据说会发布，没找到发布位置。
- 微软 Teams 约 100 万条轨迹（arXiv 2510.12265）：未见发布声明。
- Pensieve 的轨迹和代码许可：搜索没有返回相关结果，未核实。
- Lumos（GreenLv）的 MIT：只看到页面标签，未读 LICENSE 全文。
- 这次没有下载任何数据，所有"粒度""规模"都来自页面描述；实际文件的时间戳间隔、缺失情况，接入前要各取一个样本确认。
