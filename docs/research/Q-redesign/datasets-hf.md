# Hugging Face 上的网络仿真数据集调研

调研日期：2026-11-27。只读卡片、文件列表、许可，没有下载数据本体（只用 HTTP Range 读了几个文件的开头几百字节看列名）。
已有内容见 `datasets-more.md`、`industry-network-sim.md` 第 3 节，本文不重复。

## 0. 先给结论

- **Hugging Face 上几乎没有真正的网络测量数据集。** 能用的真实测量数据，只有 3 个值得接入，而且都不是"每包时延加容量真值"这一档。
  我们已知的好数据（M-Lab、Starlink 单向时延、Raca 4G、NeSt-VR、Lumos5G、微软 MMSys）一个都不在 HF 上。
- 没有找到：Mahimahi、Pantheon、Puffer、挪威 3G、比利时 4G、Ookla、M-Lab、WebRTC 通话轨迹、云游戏、屏幕共享、远程桌面的任何一个 HF 副本。
- HF 上带 network、latency、5G、LTE 字样的数据集，绝大多数是：入侵检测流量（CIC、TON_IoT 之类）、LLM 问答、机器人遥操作数据（"latency"、"jitter" 是名字里的词）、合成数据。
- 容量真值：没有一个真实测量数据集带容量真值。有"真值"的都是合成的（见表二的合成部分）。
- 许可的坑：HF 卡片上写的许可常常是搬运者随手填的。本次发现 3 处需要特别注意：
  1. `GreatLove/CellularTrafficPrediction` 卡片写 MIT，实际是意大利电信的米兰数据（原始来源许可我没核实到原文，按 ODbL 的传闻也不是 MIT），归"许可不明"。
  2. `subinak/Open_RAN...` 卡片写 MIT，原始 Mendeley 数据是 CC BY 4.0（我读到了 Licence 字段）。两者不矛盾（CC BY 比 MIT 多一条署名义务），但要按 CC BY 署名。
  3. `THULab/africa_telecom_mobility_trace...` 卡片写 other，其来源卡片写 gpl，而且是合成数据，直接不能用。

## 1. 方法和覆盖

- HF 公开接口可用，没有被拦：`https://huggingface.co/api/datasets?search=...&limit=100&full=true`、`/api/datasets/<id>`、`/api/datasets/<id>/tree/main`、`/raw/main/README.md` 都正常。
- **关键限制**：`search=` 只匹配数据集编号（名字），不匹配卡片正文和标签。所以"名字里没有关键词"的数据集搜不到。
  `filter=` 能按标签筛（我用了 `modality:timeseries`、`task_categories:time-series-forecasting`、`tabular-classification`、`tabular-regression`，每个取前 1000 个，按下载量和更新时间各一遍），但仍然没有网络专用标签。
- 试了约 290 个关键词（英文，以及"网络、带宽、延迟、丢包、流量、卫星、移动网络、无线、视频会议、云游戏"10 个中文词，中文词全部 0 条命中），其中 195 个有命中，共扫到约 7300 个数据集编号，按名字筛出约 900 个相关候选，再人工挑出约 60 个读了卡片或文件列表。
- 英文关键词覆盖了任务要求的全部：network trace、bandwidth、throughput、latency、RTT、jitter、packet loss、4G、LTE、5G、cellular、wifi、starlink、satellite、congestion control、bandwidth estimation（0 条）、WebRTC、video conferencing、video streaming、QoE、cloud gaming（0 条）、network telemetry、pcap、mobile network、traffic、mahimahi（0 条）、pantheon、puffer、M-Lab、NDT，另加 CESNET、srsRAN、O-RAN、mmWave、CSI、iperf、netem、speedtest、FCC、broadband、ISP、QUIC、HLS、DASH 等。
- 网页搜索（`site:huggingface.co/datasets`）试了 2 次，一条 HF 页面都没返回，只返回 IEEE DataPort、arXiv，没有用。
- 看了卡片或文件列表的数据集：约 60 个。下面表里列出的是其中有意义的 21 个；其余（机器人、LLM、入侵检测、电信客服问答等）直接排除，不逐个列。

字段说明：粒度、规模、容量真值的含义同 `datasets-more.md`。

---

## 2. 表一：能用的（许可允许商用和再分发，内容是真实测量）

| # | HF 编号 | 内容 | 原始来源 | 粒度 | 规模 | HF 许可 / 原始许可 | 容量真值 | 对我们的用处 |
|---|---|---|---|---|---|---|---|---|
| 1 | `subinak/Open_RAN_Performance_Measurement_Dataset_with_Traffic_and_Mobility_Labels` | 真实 O-RAN 基站（OpenIreland 测试床）上，每个 UE 的 MAC/PHY 指标，毫秒级。列有 CQI、MCS、下行/上行比特率（`mac_dl_brate`、`mac_ul_brate`）、SINR、RSSI、成功/失败块数（`mac_dl_ok`、`mac_dl_nok`）、时间戳、移动方式（car、train、pedestrian、bus、static）、流量类别（YouTube、网页等）。另有攻击标签（portscan 等），不用 | Mendeley Data "RAN Performance measurements for security threats"（DOI 10.17632/t2rzh9y4mp.1，2024-07）；论文 Xavier 等，Computer Networks 2024；msData 论文 arXiv 2603.16497（HF 版本即其发布位置） | 毫秒（行内有时间戳；论文称 millisecond-resolution） | `5G_millisecond.csv` 794 MB（分号分隔） | MIT / Mendeley 页面 Licence 字段 "CC BY 4.0"（我读到了） | 无（有每 UE 的实际比特率和 MCS，是无线层观测，不是端到端容量） | 5G 蜂窝的无线层衰落、MCS 变化、分组块错误率，随移动方式变化。可以推出"无线层突降"的时间结构，比每秒吞吐细得多。注意：是测试床上的 srsRAN 基站，不是商用运营商网络；要按 CC BY 署名 |
| 2 | `ezharjan/LocalNetDynamics`（还有一份副本 `Ezharjan/...`，同作者） | 一个美国家庭网络，一台有线 Windows 桌面，每 30 秒一个样本，连续 19 天（2026-09-20 至 10-09），共 54,617 行：到 8.8.8.8 和网关的 ICMP 时延、丢包、抖动，TCP 握手时延，DNS 缓存和递归解析时延，每小时一次测速 | 作者自己采集（卡片写 MIT，代码和数据同一许可，仓库里有 LICENSE 文件 1075 字节） | 30 秒 | parquet 加 csv，约几十 MB（文件列表里 dataset 目录未展开） | MIT / 同（作者即原始来源） | 无 | 真实家庭宽带的时延、丢包、抖动时间序列，能看到"两个状态"（9 月 20 到 29 日有 23.2% 的样本有丢包，9 月 30 日起降到 5.3%）。30 秒太粗，不能回放，只能当"家庭宽带丢包和时延分布、状态切换"的参考。注意：有线，不是 Wi-Fi |
| 3 | `Lystea/CESNET-QUICEXT25-PARQUET`（CESNET-QUICEXT-25 的转换版） | 捷克学术骨干网 CESNET3 上 2024-06 至 2025-05 的 QUIC 流记录，1:100 抽样，共 1.94 亿条；每条带前若干个包的"包大小、方向、包间隔"序列（PPI）、包大小和包间隔直方图、QUIC 版本 | Zenodo 17249078（CESNET） | 每流一条（流内含包间隔序列） | 360 个 parquet，17.1 GB（Zenodo 原版 28.2 GB） | CC BY 4.0 / Zenodo 页面 License 字段 "Creative Commons Attribution 4.0 International"（我读到了） | 无 | 真实 QUIC 业务的包大小、包间隔、方向分布，可以校准我们的"应用负载"模型（QUIC 数据报下的包长和节奏）。不是时延或丢包测量。我们的传输是 QUIC，这是少有的真实 QUIC 包序列统计。注意：只有每流前几十个包的序列，不是长会话 |
| 4 | `Lystea/CESNET-TIMESERIES24-RAW`（Zenodo 原档的字节级镜像） | CESNET 骨干网 40 周、27.5 万个 IP、548 个机构子网的聚合时间序列：流数、包数、字节数、目的 IP/端口数、UDP/TCP 比、流时长、TTL | Zenodo 13382427（CESNET-TimeSeries24） | 10 分钟、1 小时、1 天 | 41.5 GB（`ip_addresses_full.tar.gz` 40 GB；`ip_addresses_sample.tar.gz` 0.17 GB 可先试） | CC BY 4.0 / Zenodo License 字段 "Creative Commons Attribution 4.0 International"（我读到了） | 无 | 只有流量规模的聚合，没有时延、丢包。用处很弱：只能看"某个 IP 的流量日夜节律"。列出来是为了说清楚它不是网络质量数据 |
| 5 | `geocables/internet-latency-and-routing-events` | 全球海缆登陆区的每日 RTT 汇总（中位数、P90、最小值）和路由事件（绕路、恢复）。第三个文件是在海缆图上建模的断缆影响，不是测量 | GeoCables 公司（自家探针网络，卡片注明"署名 GeoCables 并链接"） | 每日汇总 | `geocables-latency-daily.csv` 63 MB，路由事件 1.5 MB | CC BY 4.0 / 同（作者即来源） | 无 | 跨洋路径 RTT 的日变化和绕路事件，能当"跨洋 RTT 基线"。粒度是天，不能回放。每天更新，内容会变 |

> 没有一个"表一"数据集带容量真值，也没有一个是每包粒度的时延或丢包。想要每包数据，仍要用 `datasets-more.md` 里的 Starlink 单向时延（Zenodo 16275284）和 NeSt-VR（Zenodo 14832268）。

### 2.1 许可可用，但内容价值很低，只备查

| HF 编号 | 内容 | 粒度 | 规模 | 许可 | 说明 |
|---|---|---|---|---|---|
| `jacksonbin/internet-speed-by-country-and-isp` | internetspeedtest.net（LibreSpeed 网页测速）的月度汇总：按国家、按 ISP 的下载、上传、ping 的均值和 25/50/75 分位 | 月 | 每月一个 JSON，约 10 到 20 KB | CC BY 4.0（作者即来源） | 自选样本，不是代表性样本（卡片自己说明）；每月汇总，只能给各国各 ISP 的粗分布 |
| `netop/gotsf-ds` | 5G 基站波束级时间序列：DLPRB、下行吞吐量（DLThpVol、DLThpTime）等，分 train、test | 未读到（按周切分） | 未统计 | MIT（来源未写明，未核实） | 基站侧资源占用，不是用户侧体验；来源未核实，不建议用 |
| `AliMaatouk/TelecomTS` | 5G 测试床 KPI 时间序列（PHY、MAC、网络层共 18 个通道，32,000 个样本，每个 128 步），含"真实干扰（jamming）"和 10 种合成异常 | 每样本 128 步（采样率写在每个样本的 `sampling_rate_hz`，未读到具体值） | 文件在 `normal`、`anomalous` 两个目录，未统计 | MIT / 论文称自测，来源 arXiv 2510.06063（ICML 2026） | 有"拥塞状态、移动方式、应用"标签；一部分异常是合成注入的，使用时要排除异常部分；主要是 LLM 训练用的问答。对我们价值低 |
| `Zenodo-ML/SUMO-Mobile-Wireless-Networks-Synthetic-Traces` | 见表二，合成 | | | | |

---

## 3. 表二：不能用的（含合成数据）

| HF 编号 | 为什么不能用 | HF 许可 / 原始许可 | 备注 |
|---|---|---|---|
| `Charles59/lens-network-traffic`、`lens-network-traffic-generation` | 非商用 | CC BY-NC 4.0 | 名字里的 "lens" 不是 LENS Starlink 数据集；内容我没细读，因为许可已经排除 |
| `Brosnan/WIFI_RSSI_Indoor_Positioning_Dataset` | 非商用；内容是 Wi-Fi 室内定位 RSSI，不是网络性能 | CC BY-NC-SA 4.0 | |
| `yysd123/indoor_mmwave` | 非商用；内容是毫米波雷达感知 | CC BY-NC 4.0 | |
| `bailuyucha/xrf55-wifi-har-derived` | 非商用；Wi-Fi 感知，不是网络性能 | CC BY-NC 4.0 | |
| `kaust-generative-ai/telco-gaia`、`...-groundtruth` | 非商用；电信知识问答 | CC BY-NC 4.0 | LLM 文本，不要 |
| `Askedrin/NeversNet5G_Vehicular_5G_NR_Dataset` | **合成**（SUMO 加 Simu5G/OMNeT++ 联合仿真，卡片写明）；同时是 CC BY-SA | CC BY-SA 4.0 / Zenodo 21225461 | 1.1 GB。仿真输出，对我们的"量尺要接近真实"没用；只在需要"车联网 5G 仿真的参照"时才看 |
| `Zenodo-ML/SUMO-Mobile-Wireless-Networks-Synthetic-Traces` | **合成** | CC BY 4.0 / Zenodo 5513705 同 | 需求是"真实测量"，不要；许可本身没问题 |
| `dougdotcon/douvras-network-edge-telemetry` | **合成**（卡片写 synthetic，60 条，不含真实流量） | CC BY 4.0 | 只有 60 条 |
| `AriaAICompany/net-flow-lab` | **合成**（卡片写 synthetic，数据中心流量演示） | CC BY 4.0 | |
| `THULab/africa_telecom_mobility_trace_datasets_nigeria`、`electricsheepafrica/africa-synth-*` 系列 | **合成**（名字里就有 synth）；来源卡片写 gpl | other / gpl | 一整个机构批量生成的"非洲电信"合成数据，全部排除 |
| `csc-architecture/csc-wireless-latency-synthetic-100k` | **合成** | other | |
| `learningjia/hilum-speedtest` | 名字是"speedtest"，实际是**合成**的基因数据，用来测 HF 的读取速度，不是网络测速 | CC0 | 避免误选 |
| `infinite-dataset-hub/5GNetworkOptimization` 等 `infinite-dataset-hub/*` | **合成**（LLM 生成的表格） | MIT | |
| `Semiloore-io/5GNetworkOptimization` | 疑似同类 LLM 生成的表格（未逐行核实） | MIT | |
| `Shashkovich/Telecommunication_SMS_time_series` | GPL-3.0；内容是短信量，不是网络性能 | GPL-3.0 | 项目不引入 GPL |
| `aadankan/NADW-network-attacks-dataset` 等 | GPL-3.0；入侵检测 | GPL-3.0 | |
| `CSRL-NITK/5G-NSD`、`alberttchalla/CrossPlane-5G`（及 `Zenodo-ML/CrossPlane-5G-...`）、`cha0ss/cyborg-cage4-real-pcap-ids`、`mtasfi/ton-iot-with-pcap-payload`、`rdpahalavan/network-packet-flow-header-payload`、`yoyoyafeng/MACCDC-PCAP`、`gates04/network-intrusion-dataset`、`TURHAN1923/network_anomaly_csv_dataset` 等入侵检测类 | 题目要求排除：入侵检测流量，没有时延和带宽的时间序列 | 多为 CC BY 4.0 或 MIT | 许可本身没问题。其中 CrossPlane-5G 有 5G 核心网和 O-RAN 的双平面遥测（5.1 GB csv，0.9 GB parquet），但目的是攻击检测 |
| `Lightcap/cloudflare-domain-traffic-analytics` 等 | 内容是 Cloudflare 站点的访问统计，不是链路测量 | CC BY 4.0 | |
| `kimhammar/5G_measurements` | 内容是"分布式 5G 测试床的 CU/DU 的 CPU、内存、功耗、路由配置，加每 DU 的下行比特率、块错误率、端到端抖动"；没有 README；2026 年一次实验的 2 个 CSV（共约 52 MB） | CC BY-SA 4.0（卡片只有许可，无说明） | 内容是核网编排实验，不是用户侧网络测量；SA 限制；来源不明。不推荐 |
| `Ridikuius/XL-MIMO-Ground-Dataset`、`yoshitomo-matsubara/mu-mimo` | MIMO 信道矩阵，物理层研究，不是网络性能 | 前者无许可，后者 cdla-permissive-2.0 | 对网络层仿真无用 |

> 把"合成"单独标出来：
> `Askedrin/NeversNet5G...`、`Zenodo-ML/SUMO-...`、`dougdotcon/douvras...`、`AriaAICompany/net-flow-lab`、`THULab/africa_*`、`electricsheepafrica/africa-synth-*`、`csc-architecture/csc-wireless-latency-synthetic-100k`、`infinite-dataset-hub/*`、`learningjia/hilum-speedtest`。这些都不能当"接近真实"的测试数据。

---

## 4. 表三：许可不明（卡片没写、或和来源对不上）

| HF 编号 | 内容 | 许可情况 | 对我们的价值 |
|---|---|---|---|
| `GreatLove/CellularTrafficPrediction` | 目录名 `sms-call-internet-mi`，加 `milano-grid.geojson`，共 66 个文件 9.84 GB；看起来是意大利电信（Telecom Italia）米兰的短信、通话、互联网活动量，10 分钟粒度（这是我对这个公开数据集的了解，卡片 README 是空的，**未核实**） | 卡片写 MIT，但 README 为空，没写来源。原始数据（Telecom Italia Big Data Challenge）我没有读到许可原文（想读 Nature Scientific Data 论文页面被重定向拦了），印象里是 Harvard Dataverse 上的 ODbL，**未核实**。转传者填的 MIT 不能当真 | 低：是"区域流量活动量"，不是链路质量，对我们没有用 |
| `Sept77/LIVE-QoE` | 直播的 QoS 日志字段说明（TLP、RTO 触发次数、最小/平滑/最大 RTT、cwnd 受限时长、拥塞算法、下载速度等 28 个字段）加 `QoE.csv`（每 5 分钟的首帧时延、缓冲空、卡顿时长，76 KB）；2025-01 | 卡片无许可、无来源、无作者说明 | 中：字段设计（QUIC 的 QoS 日志）很有参考价值，但内容只有一个 76 KB 的 QoE 表。需问作者 |
| `WaloupGarou/nz-isp-dns-routing-latency` | 新西兰各 ISP 到 DNS 的 `latency_ms`、`jitter_ms`、`packet_loss_pct`，每 5 分钟一行（`2026-01-01T00:00:00Z` 起），336 KB | 卡片写 MIT，README 只有一行；没说怎么采集。**时间戳严格 5 分钟、丢包百分比是 0.1 这种整齐值，像是合成的**（只看了前 4 行，未验证） | 低，疑似合成 |
| `thiassi/berkshire-fttp-latency-data` | 英国东伯克郡宽带的时延、DNS 响应时间、路由抖动，聚合数据加脚本 | 卡片许可字段为空，仓库有 42 字节的 LICENSE 文件（内容我没读） | 低：本地小范围、聚合 |
| `Henokbfg/5g_testbed_nwdaf_dataset` | 5G 测试床 NWDAF 数据，论文 arXiv 2606.11877 | 无许可、README 只有一行 | 低（核网侧） |
| `dvu/network-metrics-dataset` | 一组 json（约 380 KB），名字是 network metrics | 无 README、无许可 | 低，内容未读 |
| `hlarcher/cloud-provider-bandwidth` | 云厂商带宽表，8 KB 的 csv | 无 README、无许可 | 低，估计是实例规格表，未读 |
| `iqbalha/CALM_Availability_and_Latency_Data` | 论文 "Characterizing the Availability and Latency in AWS Network From the Perspective of Tenants"（IEEE/ACM ToN）的数据，`aws_dataset` 目录 | 卡片写 MIT，来源是论文作者（工具在 GitHub `iqbal-h/CALM`）；未读文件内容和大小 | 中：AWS 内部区域间时延和可用性，可作"云中继节点之间的时延"参考；需要看一下文件再定 |
| `Jasonic121/fcc_s15`、`Liening2025/FCC-DATA` 等 FCC 开头的 | 前者是 FCC 法规（47 CFR 15）的 HTML，后者 19 MB csv（未读，名字像 FCC 的数据，但无说明） | 无 | 不是 Measuring Broadband America；不要 |
| `Lystea/CESNET-TLS-YEAR22-PARQUET` | 骨干网 5 亿条 TLS 流（39.5 GB） | CC BY 4.0（原始 Zenodo 10608607，我没有读原文） | 比 QUICEXT25 弱（TLS over TCP），不列入表一 |

---

## 5. 推荐优先接入的

HF 上真正值得的东西很少。按"对量尺有没有用"排序：

1. **`subinak/Open_RAN_Performance_Measurement_Dataset_with_Traffic_and_Mobility_Labels`（5G 无线层毫秒级）**
   - 理由：这是我在 HF 上找到的唯一一个真实、毫秒级、带移动方式标签的蜂窝数据，比 Raca 4G、5Gophers 的"每秒吞吐"细三个数量级；有 CQI、MCS、块错误率，可以从无线层的角度生成"突降、抖动"的真实时间结构。
   - 先决：794 MB，先用 Range 请求取一段，确认时间戳单位（`timestamp` 看起来是个 1.66e14 的大数，单位待确认）；按 CC BY 署名 Mendeley 原始数据和论文；把攻击类别（portscan 等）的行去掉，只保留正常流量。
   - 局限：测试床的 srsRAN 小基站，不是商用运营商网络；比特率是 MAC 层观测，不是容量。
2. **`Lystea/CESNET-QUICEXT25-PARQUET`（真实 QUIC 包序列）**
   - 理由：我们传输用 QUIC 数据报，这是公开的真实 QUIC 流的包大小、方向、包间隔统计，可以校准"应用负载模型"。CC BY 4.0 已核实。
   - 先决：17 GB 太大，只取一两天的 parquet（几 MB 到几十 MB 一个）；注意 1:100 抽样；只有每流前若干个包。
   - 局限：只有包序列，没有时延和丢包。
3. **`ezharjan/LocalNetDynamics`（真实家庭网络，30 秒）**
   - 理由：卡片很规范，作者自采、许可清楚（MIT），有丢包、抖动、时延和"状态切换"的统计，可以拿来给"家庭宽带"类型的丢包状态机做参数。
   - 局限：有线，30 秒太粗，一个家庭，一个 19 天窗口；不能回放。
4. **`geocables/internet-latency-and-routing-events`**：跨洋 RTT 基线和绕路事件，CC BY 4.0。只用作"跨洋 RTT 范围"的参考，一天一个点。优先级最低。

> 结论建议：HF 不是网络轨迹的好来源。数据仍应来自 `datasets-more.md` 里的 Zenodo、IEEE DataPort、GitHub、M-Lab。HF 上有价值的只是上面 3 个补充项。

---

## 6. 没查到或没核实的

- **搜索接口只匹配数据集名字**，不匹配卡片正文。所以名字里没有关键词的真实网络数据集（例如作者用了缩写或论文名），我搜不到。要补，可以：
  1. 按作者或组织去查，已知的网络研究组（MIT、CMU、UCSD、Stanford、Aalto、Karlstad、UCC、UMN 等）在 HF 上有没有账号；
  2. 用 HF 的 `filter=` 加一批标签再翻页（我只试了 5 个标签，每个前 1000 个）。
- 没有读的候选（名字像但没读卡片）：`Amdeous/NetworkTraffic`、`Razvan27/network_paper`、`huzhzh/Network_datasets`、`Anonymous-Bear/dapt-2020-pcaps`、`RubensYamamoto/PCAPs`、`WeaveLtda/PCAPs`、`cbb02/pcap`、`youssefMakled/RTSP_Traffic_Dataset`（RTSP 视频流量，名字像，没读卡片，值得一看）、`yyy999/Unicauca-dataset-April-June-2019-Network-flows`（流量分类，非时延）、`isabik/Linking_cellular_topology_to_compression_behavior`、`Romz16/Mobility_Network_Data-Analysis`、`WIFI6GHZDATASET/WiFi_6GHz_Dataset`（卡片读了：6 GHz Wi-Fi 的 CSI 感知，不是网络性能）、`fancz2002/league-of-legends-decoded-replay-packets`（游戏回放，不是网络测量）、`Jalalk1/Birmingham_AL_680K_Synthetic_Traffic_DataSet`（合成车流）。
- 屏幕共享、远程桌面、云游戏、WebRTC 通话的真实网络记录：在 HF 上一个也没找到。
- 微软 Teams 通话轨迹、Mahimahi 轨迹、Pantheon、Puffer：HF 上没有镜像（名字搜索都是 0 条或无关）。
- 部分数据集的规模和字段：`ezharjan/LocalNetDynamics` 的 `dataset` 目录大小没统计；`AliMaatouk/TelecomTS` 的 `normal`、`anomalous` 目录大小没统计；`CESNET-QUICEXT25` 和 `Open RAN` 的实际时间戳间隔没验证。
- `GreatLove/CellularTrafficPrediction` 的原始来源和许可（见表三）。
- 网页搜索对 HF 无效（2 次，0 条 HF 页面），所以没有"网页搜索补漏"。
- 没有下载任何数据；只对 5 个文件读了开头几百字节看列名：Open RAN 的 csv、`kimhammar/5G_measurements`、`WaloupGarou/nz-isp...`、`Sept77/LIVE-QoE/QoE.csv`、`Gssmc/Dataset_wired_wireless`（后者是耳机商品价格，与网络无关）。
