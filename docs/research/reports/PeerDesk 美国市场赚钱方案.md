# PeerDesk 美国市场赚钱方案（截至 2026-11-26）

标记：【已核实】本次打开官方原文；【二手】搜索摘要、第三方页面、或官方页被拦（403）只拿到摘要；【未核实】没找到可靠来源；【推断】我们自己算的或判断的。
"已有调研"指 `docs/research/research_notes/PeerDesk AI Agent 业内调研/` 下的笔记，其中的结论沿用，本次没有重开原文的会注明。
价格都是美元、不含税，注明了月付还是年付折算。

---

## 0. 结论先行（给决策用）

1. **第一个要打的人群：在自己电脑上常驻跑 AI 智能体的美国个人和独立开发者**（Mac mini / Windows / Linux 主机上跑 OpenClaw、Claude Code、Codex 等，要从手机或笔记本远程看一眼、点确认、接管）。理由：痛点有一手证据，获客渠道集中、便宜，产品离"能收钱"最近（已有无人值守、P2P/中继、低延迟画面、文件传输）。付费价位上限不高（$5–15/月），但这是最快验证"有人付钱"的人群。
2. **第二波是自托管和家庭实验室人群**（Home Assistant、RustDesk 自托管用户）。他们是开源的流量来源，用 Nabu Casa 模式（"托管的中继和远程访问"，$6.50/月、$65/年【已核实】）变现。
3. **MSP 和小企业 IT 钱最多、最稳，但要求最高，放到 90 天之后**：ScreenConnect 每个并发技术员约 $47–72/月【二手】，TeamViewer Business $50.90/月起【二手】，PeerDesk 现在没有多租户、批量部署、RMM 集成、SOC 2。90 天内只做"设计伙伴"：找 3–5 家 1–3 人的 IT 小店免费用，换取需求清单。
4. **卖什么：卖连接，不卖模型。** 付费点是常驻在线、托管 MCP 端点、多设备、人工接管画质、审批和审计、团队共享，全部有真实成本或企业价值。模型默认让用户自带 Key（BYOK），首发不卖 AI 积分。
5. **定价（建议）**：Free $0；Plus $7/月（$59/年）；Pro $14/月（$119/年）；Team $15/用户/月（3 席起）；MSP 技术员档 $29/月（90 天后）。对标见第 3 节。
6. **成本能覆盖，前提是限制中继**：按假设（直连成功率 80%、平均码率 2.5 Mbps），Plus 用户典型毛利约 85%，但如果全部走中继并跑满 100 GB，毛利会掉到约 17%。所以必须有中继额度，免费版的中继要卡得很紧，否则 50 个免费用户的中继成本就会吃掉 1 个付费用户的利润（第 3.4 节）。
7. **90 天现实目标：50 个付费用户、MRR $400–700，加 3 家 MSP 设计伙伴。** 这是验证，不是生意规模。第 1–2 周先用 Stripe Payment Link 做"创始会员"预售，不等账户系统做完。
8. **最大的不确定**：美国没有找到 AI 智能体用户"愿意为远程连接付多少"的直接证据（没找到 Reddit 原帖、没有问卷）。只有 Astropad Workbench 定价 $10/月这一个间接的价格信号。预售数据比任何调研都准，所以第一步是预售。

---

## 1. 谁会付钱

### 1.1 排序表

| 排名 | 人群 | 人数（量级） | 付费意愿 | 获客难度 | 现在能不能卖 |
|---|---|---|---|---|---|
| 1 | 常驻 AI 智能体的个人和独立开发者 | 未知。代理指标：OpenClaw GitHub 星标约 39 万【二手】；公网暴露实例 2–3 万【二手】 | 中。Workbench $10/月；但多数人先用免费的 Tailscale 加 VNC | 低到中：社区集中，教程驱动 | 能，90 天内 |
| 2 | 自托管/家庭实验室（Home Assistant、RustDesk 自托管） | Home Assistant 约 200 万装机（2025，二手）；RustDesk 自报 3,000 万下载（厂商自报） | 中。Nabu Casa $65/年、RustDesk Pro $11.88/月 | 低：r/selfhosted、GitHub | 能，但转化低，要靠量 |
| 3 | 小企业 IT、MSP（1–10 个技术员） | 无美国口径数字【未核实】。代理指标：TeamViewer ARR 约 7.37 亿欧元（已有调研，二手） | 高。每技术员每月 $22–72 | 高：要功能、信任、销售 | 90 天后 |
| 4 | 高性能远程（创作者、游戏、远程工作站，Parsec 用户） | 大，数字未核实 | 中。Parsec Warp $8.33–9.99/月，但 Parsec 个人免费 | 中，竞争直接 | 画质和延迟有优势再打 |
| 5 | 家人技术支持（"我是家里的 IT"） | 大 | 低。Quick Assist、Chrome 远程桌面、屏幕共享免费 | 中 | 不主动打 |
| 6 | 企业 | — | 高，但要 SOC 2、SSO、采购 | 很高 | 不打 |

### 1.2 第一人群的证据

- **需求是真的，痛点在"人要能看一眼、点一下"**。Astropad 在官方博客写：智能体常卡在登录界面、权限弹窗、浏览器弹窗上，用户不在就停着；无显示器的 Mac 会掉到模糊的低分辨率；FileVault 重启后要密码，会断掉无人值守访问。【已核实】[Astropad: OpenClaw on Mac mini](https://astropad.com/blog/openclaw-mac-mini/)（本次读原文）。Astropad 自己的说法是盈利、无外部融资、客户超过 10 万，并专门推出给智能体用的 Workbench（已有调研，TechCrunch 2026-04-08）【二手】 [TechCrunch](https://techcrunch.com/2026/04/08/astropads-workbench-reimagines-remote-desktop-for-ai-agents-not-it-support/)。
- **用户现在靠自己拼**：搜索到的教程一致是 Mac 自带屏幕共享、SSH、Tailscale 三条路；Tailscale 最常见；屏幕共享在手机上被指弱，长命令在手机上输入痛苦。【二手，教程和厂商指南，没有找到 Reddit 原帖】 [搜索结果汇总，含 mager.co、DEV 教程、OpenClaw 官方文档 docs.openclaw.ai/platforms/mac/remote](https://docs.openclaw.ai/platforms/mac/remote)。Tailscale 官方博客专门写了 OpenClaw 的远程访问方案（已有调研）【二手】 [Tailscale 2026-03-24](https://tailscale.com/blog/openclaw-tailscale-aperture-serve)。
- **常驻本身是硬约束**：Claude Dispatch 要求桌面 App 保持唤醒并运行【已核实，已有调研】 [Claude Blog](https://claude.com/blog/dispatch-and-computer-use)。
- **安全是第二个痛点**：公网暴露的 OpenClaw 实例 2.1 万（Censys 2026-01-31）到 3 万以上（Bitsight 2026-01-27 至 02-08）【二手，已有调研】 [Bitsight](https://www.bitsight.com/blog/openclaw-ai-security-risks-exposed-instances)。"默认不暴露公网、端到端加密、可审计"可以直接当卖点。
- **Mac mini 售罄**：2026-04 美国官网 $599 基础款售罄【二手，已有调研】 [TechCrunch 2026-04-24](https://techcrunch.com/2026/04/24/mac-mini-price-expensive-ebay-shortage-ai-memory/)。说明常驻主机人群在美国是真的在买硬件，但缺货也有内存供应原因，不能全算成 AI 需求。

### 1.3 这个人群的弱点（要诚实）

- **替代品免费且够用**：Tailscale Personal 免费、最多 6 个用户、不限设备【已核实】 [Tailscale 定价](https://tailscale.com/pricing)；Chrome 远程桌面、Mac 屏幕共享也免费。我们要卖的不是"能连上"，而是"连上之后的体验"（低延迟、手机上能点能打字、无显示器时分辨率正确、重启后能恢复、能接管和审批）。
- **同类产品在 HN 上反响很低**：已有调研记录 HN 上同类帖子只有 1 分【二手】。不能指望靠一次发帖。
- **Windows 和 Linux 是空白**：Workbench 只支持 Apple 设备（Windows、Linux 在计划中）【已核实，已有调研】。PeerDesk 已有三平台，这是对 Astropad 的实际差异。

---

## 2. 卖什么：免费版和付费版的边界

### 2.1 原则

1. **开源版（Apache-2.0）自托管永远免费、功能完整**，包括 P2P、中继服务端、MCP server、CLI。这是信任和获客的基础，也是 RustDesk、n8n Community 的做法（n8n 社区版免费【已核实】 [n8n 定价](https://n8n.io/pricing/)）。
2. **托管云卖"我们替你运营的部分"**：中继、信令、账户、设备管理、常驻在线的可达性、审计留存、团队。Nabu Casa 的模式：Home Assistant 没订阅也完整可用，订阅卖远程访问、备份、语音处理【已核实】 [Nabu Casa Link](https://www.nabucasa.com/link/)、[Home Assistant Cloud 页](https://www.home-assistant.io/cloud/)。
3. **不卖模型**：ChatGPT Work 桌面版对 Free 开放 Computer Use（已有调研，【已核实】[OpenAI](https://openai.com/index/chatgpt-for-your-most-ambitious-work/)），Claude Pro 以上含 Dispatch（【已核实】[Claude Pricing](https://www.claude.com/pricing)），腾讯的远控不耗 Token（二手）。模型是被大厂捆绑的东西，我们卖的是它们不覆盖的：模型中立、Windows/Linux、人工接管画面、审计。

### 2.2 付费功能清单和证据

| 付费功能 | 为什么用户愿意付 | 证据 | PeerDesk 现状 |
|---|---|---|---|
| 托管中继，P2P 失败时自动兜底 | 家里电脑在 CGNAT/企业网后面连不上 | 中继是真实成本；RustDesk、Nabu Casa、Tailscale 都是这样收费 | 已有 |
| 常驻设备数（Free 2 台，Plus 10 台，Pro 25 台） | 多台电脑和 Mac mini | Tailscale、RustDesk（Individual 20 设备）、Jump 都按设备或用户分档 | 已有无人值守，要加账户配额 |
| 托管 MCP 端点（带账户认证的远程 MCP URL，让任意智能体经云到达电脑） | 自托管 MCP 要自己做认证和暴露公网，而公网暴露正是 OpenClaw 的安全事故来源 | Bitsight 暴露数；向日葵把 MCP 做成选配（已有调研，【已核实】[向日葵价格页](https://sunlogin.oray.com/price)）；TeamViewer 做了 MCP（二手） | 计划中，是核心新功能 |
| 审批收件箱：智能体要做高风险操作时，推送到手机让人批准 | 智能体卡在权限弹窗上；人要最后一道关 | Astropad 的卡住场景【已核实】；TeamViewer Tia 的变更要审批并记录【已核实，已有调研】[TeamViewer 2026-09 news](https://www.teamviewer.com/en/insights/teamviewer-september-2026-news/)；Gartner 15% 在考虑完全自主智能体【二手，已有调研】 | 需要手机端和推送，计划中 |
| 审计日志和会话录像（Free 无，Plus 7 天，Pro 30 天，Team 90 天） | 智能体做了什么要能追溯；团队合规 | 向日葵把"AI 智能远控审计"放在最贵的行业版【已核实，已有调研】；Parsec Teams 有 7 天审计日志，Enterprise 才有完整日志【已核实】[Parsec 定价](https://parsec.app/pricing) | 未做，要先做 |
| 高画质上限（码率、多显示器、4K） | 创作者和开发者要清晰 | Jump Desktop：Free 和 Pro 视频最高 20 Mbps、Enterprise 60 Mbps，虚拟显示器 1 对 4【已核实】[Jump 定价](https://jumpdesktop.com/pricing-plans.html) | 编码管线已有，要加限速开关 |
| 无显示器时的虚拟显示器 | Mac mini 无显示器会掉到糊的分辨率 | Astropad 博客【已核实】 | 需确认各平台是否已有 |
| 团队：共享设备组、角色、集中账单 | 小团队共用主机 | Tailscale Standard $8/用户、Bitwarden Teams $4/用户【已核实】 | 未做 |
| 商业使用授权 | 个人档和商业档分开是行业惯例 | Parsec：商业使用必须 Teams 或 Enterprise【已核实】；Tailscale Personal 限非商业【已核实】 | 只是条款，几乎零成本 |

### 2.3 AI 部分怎么收费才不撞大厂

- **按"连接"收，不按"智能"收**：计价单位是设备数、并发会话、中继流量、审计留存天数、MCP 端点数。这些我们有成本，大厂不覆盖跨厂商。
- **BYOK 为默认**。已有调研的成本数据：全 GUI 路线每个任务约 $0.3–0.7，结构化工具路线约 $0.02（推断，来自 routes_reliability_cost 笔记）；如果托管模型，用户每天 20 个 GUI 任务成本 $6–14，订阅价覆盖不了。所以首发不托管模型。
- **以后若卖托管 AI**：只做"工具优先、GUI 兜底"的路由，按任务计量，挂在账户上（符合 CLAUDE.md 计费约定），而且是附加包，不放进基础订阅。
- **兼容而不是对抗**：对外说"PeerDesk 让 Claude、ChatGPT、OpenClaw 任意一个到达你的任意一台电脑"，让大厂的免费功能成为我们的流量，而不是对手。

---

## 3. 定价

### 3.1 对标价格表（访问日期均为 2026-11-26）

| 产品 | 档位和价格 | 口径 | 来源 | 标记 |
|---|---|---|---|---|
| Splashtop | Solo $6/月（$72/年）；Pro $8.25/用户/月（$99/年）；Performance $13/用户/月（$149/年）；SOS 每并发技术员 $259 或 $399/年（$22/月起） | 年付，官方页 | [splashtop.com/pricing](https://www.splashtop.com/pricing) | 【已核实】 |
| Parsec | 个人免费；Warp $8.33/月（年付）或 $9.99（月付）；Teams $30/$35；Enterprise $45（只年付） | 每用户每月，商业使用须 Teams | [parsec.app/pricing](https://parsec.app/pricing) | 【已核实】 |
| Jump Desktop | 免费（Windows、Android、网页）；Mac 和 iOS 一次性购买；Teams Pro $7.99/用户/月；Enterprise $15.99；年付省 20% | 官方页 | [jumpdesktop.com/pricing-plans.html](https://jumpdesktop.com/pricing-plans.html) | 【已核实】（Mac 应用 $34.99 一次性为搜索摘要【二手】） |
| Tailscale | Personal $0（6 用户）；Standard $8/用户/月；Premium $18 | 官方页 | [tailscale.com/pricing](https://tailscale.com/pricing) | 【已核实】 |
| RustDesk Server Pro | Individual $11.88/月（1 用户 20 设备）；Basic $23.88/月（10 用户 100 设备）；只自托管，只年付 | 已有调研读过原文，本次未重开 | [rustdesk.com/pricing](https://rustdesk.com/pricing) | 【已核实（已有调研）】 |
| Astropad Workbench | 免费每天 20 分钟；$10/月或 $50/年；只支持 Apple | TechCrunch 报道；Astropad 博客只确认免费档 20 分钟/天 | [TechCrunch](https://techcrunch.com/2026/04/08/astropads-workbench-reimagines-remote-desktop-for-ai-agents-not-it-support/) | 价格【二手】，免费档【已核实】 |
| TeamViewer | 官网只写年付订阅、只有发起连接的专家要许可证，不列价；报价页 Business $50.90、Premium $112.90、Corporate $229.90 每月（年付）；Remote Access $24.90/月；Black Friday 首年 7 折到 12-04 | 官方页没价，价格来自搜索摘要和 G2 | [TeamViewer 定价](https://www.teamviewer.com/en-us/pricing/) | 价格【二手】 |
| AnyDesk | 官方页 403。多个来源：Solo $28.90、Standard $49.90、Advanced $111.90/月（年付）；另有来源给更低或更高的数，互相冲突 | 年付 | [tsplus 汇总](https://tsplus.net/anydesk-pricing) | 【二手，冲突】 |
| ScreenConnect | 官方页 403。搜索摘要：Standard $47、Premium $57/月（年付，每并发技术员），月付 $62、$72；One $30/月 | 每并发技术员 | [screenconnect.com/pricing](https://screenconnect.com/pricing) | 【二手】 |
| Chrome 远程桌面 | 免费。本次打开的帮助页没有写免费或商业限制，"免费"来自常识 | — | [Google 帮助](https://support.google.com/chrome/answer/1649523) | 【未核实】 |
| Apple 屏幕共享 | macOS 自带，免费。本次没有查 | — | — | 【未核实】（推断：自带功能） |
| Home Assistant Link（原 Cloud） | $6.50/月或 $65/年；31 天免费试用无需卡；2026-12-02 改名 | 官方页 | [nabucasa.com/link](https://www.nabucasa.com/link/) | 【已核实】 |
| Bitwarden | 免费；Premium $19.80/年；Families $47.88/年；Teams $4/用户/月；Enterprise $6 | 年付 | [bitwarden.com/pricing](https://bitwarden.com/pricing/) | 【已核实】 |
| n8n | 云版 Starter €20/月、Pro €50/月、自托管 Business €667/月（年付）；社区版免费；小于 20 人创业公司 Business 半价 | 按执行次数 | [n8n.io/pricing](https://n8n.io/pricing/) | 【已核实】 |

读法：
- **个人档的行业价格带是 $6–10/月**（Splashtop Solo、Nabu Casa、Parsec Warp、Astropad、Jump）。PeerDesk Plus 定在这里不会被嫌贵。
- **团队档的价格带是 $8–30/用户/月**（Tailscale、Jump、Splashtop Pro/Performance、Parsec Teams）。
- **MSP 技术员的价格带是 $22–72/月/并发技术员**（Splashtop SOS、ScreenConnect、TeamViewer、AnyDesk）。
- **免费档都很大方**（Tailscale、Parsec、Jump 的 Windows/Android），没有人靠限制 P2P 连接数赚钱。

### 3.2 PeerDesk 建议档位

| 档位 | 价格 | 包含 | 对标理由 |
|---|---|---|---|
| **Free（托管，非商业）** | $0 | 2 台设备；P2P 不限；中继每月 2 GB；1 路并发；CLI 和本地 MCP 不限（自托管不限）；无审计 | 对标 Tailscale Personal、Parsec 个人。免费档要够让人用起来、不足以替代付费 |
| **Plus（个人）** | **$7/月，$59/年** | 10 台设备；中继每月 100 GB；无人值守；文件传输；手机端；托管 MCP 端点 1 个；审批收件箱；审计 7 天 | 在 Splashtop Solo $6、Nabu Casa $6.50、Parsec Warp $8.33、Astropad $10 之间。年付 $59 比 Astropad $50/年高一点，因为含 Windows、Linux 和 MCP |
| **Pro（专业/小团队主机）** | **$14/月，$119/年** | 25 台设备；中继每月 500 GB；商业使用；多并发；码率上限提高、多显示器；虚拟显示器；MCP 端点 5 个；审计 30 天；优先中继 | 对标 Splashtop Performance $13、Jump Teams Enterprise $15.99；比 Parsec Teams $30 低一半 |
| **Team** | **$15/用户/月，3 席起，年付 $12/用户** | Pro 全部加共享设备组、角色、集中账单、审计 90 天、SSO（后做） | 对标 Tailscale Standard $8 到 Premium $18、Jump Enterprise $15.99 |
| **MSP 技术员（90 天后）** | **$29/并发技术员/月，年付 $24** | 100 个被管设备；批量部署安装包；多租户；品牌化；会话录像；API；被管设备超出 $0.50/台/月 | 比 ScreenConnect Standard $47–62 低约 40%，和 Splashtop SOS $22–33 同档。**功能没做完前不卖**，先做设计伙伴 |
| **自托管 Pro 许可证** | 不做 | — | RustDesk 已占位（$11.88/月，无客服）；我们 90 天内聚焦托管云 |

注意：
- 预售期用"创始会员"：Plus $39/年（首年，永久锁价），限前 200 名。只用来换取最早的付费信号。
- Free 档中继 2 GB 是故意卡紧的（见 3.4 的免费用户成本计算），可以以后放宽，不能以后收紧。

### 3.3 为什么不收更高

- 个人用户在 $10/月以上要和含模型的订阅（Claude Pro $20、ChatGPT Plus 同档）比，而我们不含模型。
- Astropad 是目前唯一明确面向智能体用户的直接对手，$50/年、$10/月就是它的锚点【二手】。

### 3.4 中继成本和毛利

**假设（全部是【推断】，需要用自己的数据替换）：**

| 假设 | 取值 | 说明 |
|---|---|---|
| 平均码率，活跃使用时 | 2.5 Mbps，约 1.1 GB/小时 | 远程桌面大部分时间静止、突然爆发（CLAUDE.md）；这个数要用 e2e 的真实指标替换 |
| 直连成功率 | 80%，中继占 20% | 要用 `scripts/nat-lab.sh` 和线上遥测替换；美国家宽 NAT 情况我们没有数据 |
| 中继单价 A | $0.05/GB | Cloudflare Realtime TURN 官方价，每月前 1,000 GB 免费，只对从边缘发给客户端的流量计费【已核实】[Cloudflare TURN FAQ](https://developers.cloudflare.com/realtime/turn/faq/) |
| 中继单价 B | $0.005–0.01/GB | 自建 VPS 的大流量估计。**没有核实来源**（Hetzner 页面本次没拿到流量价）【未核实】 |
| 支付手续费 | 2.9% + $0.30/笔 | Stripe 常规美国费率，**本次没有核实**【未核实】 |
| 每个付费用户的固定成本 | $0.30/月 | 数据库、信令、日志、客服摊销，拍的数字【推断】 |

**每个付费用户每月的毛利估算（月付口径，A 单价）：**

| 场景 | 收入 | 手续费 | 中继成本 | 固定 | 毛利 | 毛利率 |
|---|---|---|---|---|---|---|
| Plus 典型：每月 25 小时，20% 走中继 → 5.5 GB | $7.00 | $0.50 | $0.28 | $0.30 | $5.92 | 85% |
| Plus 重度：每月 80 小时，20% 中继 → 17.6 GB | $7.00 | $0.50 | $0.88 | $0.30 | $5.32 | 76% |
| Plus 最坏：80 小时，100% 走中继 → 88 GB | $7.00 | $0.50 | $4.40 | $0.30 | $1.80 | 26% |
| Plus 额度顶格：100 GB 全用满 | $7.00 | $0.50 | $5.00 | $0.30 | $1.20 | 17% |
| Pro 重度：200 小时，20% 中继 → 44 GB | $14.00 | $0.71 | $2.20 | $0.30 | $10.79 | 77% |
| Pro 额度顶格：500 GB 全用满 | $14.00 | $0.71 | $25.00 | $0.30 | -$12.01 | 亏损 |

结论：
1. **单价 A 下 Pro 的 500 GB 额度会亏本**。上线前二选一：把 Pro 额度降到 200 GB（顶格 $10，毛利约 $3），或者先自建中继（单价 B，500 GB 只要 $2.5–5）。建议：先用单价 B 的自建中继，Cloudflare TURN 只当兜底和海外节点。
2. **超额处理**：超过额度后降码率到 1 Mbps（不断开，符合"清晰度最后让步"的前提是告诉用户原因），或按 $0.10/GB 加购。必须在工具条里显示"本月中继已用 X/Y GB"。
3. **免费用户的成本要算进去**：假设免费用户平均每月用中继 1 GB（$0.05）加固定 $0.05，每人 $0.10；若免费转付费 2%，则 50 个免费用户养 1 个付费用户 = $5/月的免费成本，约等于 Plus 的全部毛利。所以：免费档中继卡 2 GB、免费档只要有一个设备绑定账户、不提供无人值守之外的重功能，并监控"免费用户每月中继 GB 的分布"。转化率 2% 是【推断】，没有核实的行业数据。
4. **AI 操作本身流量极小**：MCP 和 CLI 走的是文本命令，不是视频流，几乎不增加中继成本。成本集中在"人看屏幕"的视频流。这是"卖连接"的好处：智能体用得越多，我们的成本并不线性涨。
5. **支付手续费对年付更友好**：年付 $59 的手续费约 $2.01（3.4%），月付约 7%。引导年付。

---

## 4. 怎么找到第一批付费用户

### 4.1 渠道（按优先级）

| 渠道 | 做什么 | 证据和注意 |
|---|---|---|
| **OpenClaw/智能体社区**（OpenClaw Discord 和文档、GitHub Discussions、ClawHub 技能） | 写教程"给 Mac mini / Windows 上的 OpenClaw 加手机接管"；做 PeerDesk 的 OpenClaw skill 和 MCP 配置模板 | 官方文档本身就在讲远程访问（Tailscale、LAN、HTTPS 代理）【已核实：搜索摘要列出，docs.openclaw.ai】；Tailscale 已经靠这种教程蹭到这批人（已有调研）。**直接竞品动作也多**：DeskVNC、GlintDesk 等已在 Product Hunt（已有调研，二手），要抢先 |
| **MCP 生态**（官方 Registry、awesome-mcp 列表、Claude/Cursor 的 MCP 目录） | 上架 `peerdesk-mcp`，写"让 Claude 控制你的另一台电脑"案例 | 官方 Registry 2026-09-24 有 35,491 个 server【二手，已有调研】 [DEV](https://dev.to/amareswer/the-mcp-registry-by-the-numbers-38nc)；上架免费，是长尾流量 |
| **Reddit**：r/selfhosted、r/homelab、r/MacMini、r/ClaudeAI、r/sysadmin、r/msp | 发"开源、Rust、能自托管"的实测帖，带延迟和画质数据（CLAUDE.md 要求的指标） | **本次没有找到任何 Reddit 原帖**，无法证明需求；渠道选择是推断【未核实】。自托管版块对自我推广很敏感，要先有价值内容 |
| **Hacker News（Show HN）** | 发 1 次，主题放在"为什么远程桌面的传输层要重写"（QUIC 数据报、静止后马上变清晰），不要放"AI 智能体" | 已有调研：同类"手机控制智能体"帖子在 HN 只有 1 分【二手】。技术文章角度更可能成功，但无保证 |
| **GitHub** | README 第一屏放对比表（对 RustDesk、Tailscale+VNC）；发布签名过的安装包；`good first issue` | 星标是转化漏斗的最上面，不是收入 |
| **Product Hunt** | 放到第 8–10 周，等有预售数据和截图 | 同类已在上面，竞争大 |
| **MSP 社区**（r/msp、MSP 论坛、Slack/Discord 社区） | 90 天后。先在 r/msp 问"你们现在用 ScreenConnect/Splashtop 最痛的是什么"，找设计伙伴 | MSP 按设备收费的调研：Kaseya 全球 MSP 基准调研（2023，1,091 份）28% 的按设备计价 MSP 每台每月收 $50–100【二手，全球口径、不是美国】。**Kaseya 2026 报告本次打不开（PDF 无法解析）**【未核实】 |
| **包管理器** | Homebrew cask、winget、apt 仓库 | 降低安装摩擦，对 Mac mini 人群尤其重要 |

### 4.2 开源到付费的转化路径

1. 访客读 README 或教程 → 下载（Homebrew/winget/官网，已签名）→ 本地用，不要求账户。
2. 想从外面连：**在应用内一键创建免费账户**（Free 档 2 台设备、中继 2 GB）。账户是转化的闸口，必须做在第一次需要"远程"的时候，不是安装时。
3. 触发付费的时刻（要埋点）：第 3 台设备；中继用到 80% 额度；想开无人值守；想要审计或第二个 MCP 端点；想用商业场景。
4. 付费页面写清楚"自托管永远免费，付费买的是我们运营的中继和账户"，写明失败时的原因和解决办法（CLAUDE.md"诚实"）。

### 4.3 美国上架必须的东西

| 项目 | 做法 | 成本和时间 | 来源和标记 |
|---|---|---|---|
| **Apple 开发者账号 + Developer ID 签名 + 公证 + 装订** | 用**组织**账号（显示公司名）。组织需要免费的 D-U-N-S 号，最多 5 个工作日；个人账号不需要。签名身份必须长期稳定，否则每次更新用户都要重新授权屏幕录制和辅助功能 | $99/年；1–2 周 | 已有调研 I-release.md，引用 Apple 官方文档【已核实（已有调研）】 [Apple 会员对比](https://developer.apple.com/support/compare-memberships) |
| **Windows 代码签名** | 首选 Azure Artifact Signing（原 Trusted Signing），约 $9.99/月。**组织必须在美国、加拿大、欧盟、英国等地区；个人只有美国和加拿大**。EV 证书不再绕过 SmartScreen，不要买。首批用户仍会看到 SmartScreen 警告，要攒信誉；从第一个公开版本起就用同一身份签名所有二进制并加时间戳 | $9.99/月；验证 1–3 周 | I-release.md 引用微软 quickstart（2026-11-20 更新）【已核实（已有调研）】 [Microsoft Learn](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart)。**要确认我们的公司主体注册在哪里**：如果在中国大陆，两种都不符合（已有调研结论），要先在美国注册公司（Delaware C-Corp 或 LLC）【推断：这条是决策点，不是技术问题】 |
| **Stripe** | 账户挂在美国公司主体；订阅用 Stripe Billing；**账户和订阅挂在账户 ID 上，不挂设备指纹**（CLAUDE.md） | 手续费见 3.4 | 费率【未核实】；功能不展开 |
| **销售税** | SaaS 在美国是否征税因州而异。Stripe Tax 可以计算、收取、报告；各州有"经济联系"（nexus）门槛，按州各自规定；Stripe 会监控并在达到门槛时提醒，也可以代为注册和申报 | 先开 Stripe Tax 监控，不用一开始就全注册；日常按提醒处理 | 【已核实】[Stripe: Collect tax in the US](https://docs.stripe.com/tax/supported-countries/united-states)。各州具体门槛没有逐个读，别在文档里写死 |
| **隐私政策、服务条款、可接受使用政策** | 明确：不采集屏幕内容；中继只转发加密流量；审计日志存什么、存多久；商业使用条款；禁止用于未授权访问（远程工具常被用于诈骗，必须有滥用举报入口） | 律师审阅，预算 $1–3k【推断，未核实】 | 【推断】；加州隐私法（CCPA）的适用门槛没有核实，要问律师 |
| **SOC 2** | 90 天内不做。触发条件：第一批 Team/MSP 客户的安全问卷要求，或者有单笔 $10k+/年的订单卡在这一条上。先做一份"安全白皮书"（端到端加密、默认不暴露公网、审计）和开源可审计作为替代 | 具体费用和周期本次没有核实【未核实】 | 【推断】 |
| **商标和域名** | peerdesk.ai 已定；注册前查美国商标库有没有冲突的 "PeerDesk"。本次没有查 | — | 【未核实】 |

---

## 5. 90 天计划（按周）

目标：第 90 天有 **50 个付费用户、MRR $400–700**，3 家 MSP 设计伙伴，一份"免费到付费"的真实漏斗数据。

漏斗假设（【推断】，需要被真实数据替换）：90 天内约 2 万访问 → 3,000 次下载 → 900 个账户 → 3–5% 付费 = 27–45 人，加预售的创始会员约 10–20 人。

| 周 | 做什么 | 对应产品功能和现状 | 产出和检查点 |
|---|---|---|---|
| **1** | 注册美国主体（见 4.3 决策点）；开 Stripe；申请 Apple D-U-N-S、Apple Developer、Azure Artifact Signing；建一个落地页和**创始会员预售**（Stripe Payment Link，Plus 首年 $39，限 200 名） | 不需要新代码。落地页放对比表和 e2e 指标 | 预售链接上线。**第 1 周结束就能收第一笔钱**（如果有人买） |
| **2** | 发第一批测试用户邀请（OpenClaw Discord、r/MacMini、r/selfhosted 各一个价值帖：怎么给无显示器 Mac mini 做远程）；手工给预售买家开通 | 手工发"许可码"；先不做账户系统 | 预售买家数 = 第一个需求信号。< 5 就先停下来改定位，不要硬做账户 |
| **3–4** | **账户系统 v1**：邮箱或 GitHub 登录；设备绑定账户；Free 档配额（2 台、2 GB 中继）；Stripe Billing + Customer Portal；订阅状态决定设备数和中继额度 | 账户和订阅是现在缺的，是主线。账户 ID 做主键，**不挂设备指纹** | 能自助升级到 Plus；Stripe Tax 监控打开 |
| **5** | Windows 和 macOS 签名发布流水线（签名、公证、装订、`[pkg]` 标记触发）；Homebrew cask、winget 提交 | 已有调研 I-release.md 的方案 | 干净机器上无警告（macOS）或只有首次 SmartScreen 提示（Windows）；下载页诚实说明 |
| **6** | **中继配额和计量**：每账户每月 GB；工具条显示用量；超额降码率；自建中继节点（美东、美西各一）；每用户中继成本看板 | 需要在传输层加账户级计量；已有中继 | 能回答"每个付费用户的中继成本是多少"，替换 3.4 的假设 |
| **7–8** | **托管 MCP 端点 + 手机端审批**：账户认证的远程 MCP URL；高风险操作推送到手机批准；OpenClaw/Claude/Cursor 的配置模板和教程；上架 MCP Registry | 计划中的 AI/Agent/MCP/CLI；手机端计划中。这是 Plus 的主要卖点，没有它就只是又一个远程桌面 | 3 篇教程；MCP Registry 上架 |
| **9** | **Show HN** 发技术文章；r/selfhosted 和 r/homelab 发布；GitHub README 更新 | e2e 指标要公开（延迟、卡顿、静止后变清晰时间） | 跟踪访问→下载→账户 |
| **10** | **审计日志 v1**（Plus 7 天、Pro 30 天）；会话录像放后面；Pro 档上线（多并发、码率上限、虚拟显示器） | 审计未做；虚拟显示器要查各平台现状 | Pro 开卖 |
| **11** | Product Hunt；对前 20 个付费用户做电话访谈，问"为什么付费"和"差什么就不付" | — | 访谈记录；决定第二批功能 |
| **12** | 在 r/msp 找 3–5 家 1–3 人 IT 小店做**设计伙伴**（免费，换需求清单）；写一页安全说明 | 先只收集需求，不开发 | 一份 MSP 功能缺口清单（批量部署、多租户、RMM 集成、录像） |
| **13** | 复盘：转化率、付费用户数、每用户中继成本、流失；决定是继续深挖个人档还是转 MSP | — | 决策会：继续还是调整 |

### 5.1 要盯的指标

| 指标 | 第 90 天目标 | 为什么 |
|---|---|---|
| 访问 → 下载 | 10–15% | 落地页是否讲清楚（推断） |
| 下载 → 账户 | 25–30% | 应用内账户引导是否顺畅 |
| 账户 → 第二台设备已连接 | 40% | "产品起作用"的信号，比注册更重要 |
| 账户 → 付费 | 3–5%（预售不计入） | 行业数据未核实，是我们的目标 |
| 付费用户数 | 50 | |
| MRR | $400–700 | |
| 月付 vs 年付比例 | 年付 ≥ 50% | 降低手续费和流失 |
| 每付费用户月中继 GB（中位数和 p95） | 中位 < 10 GB，p95 < 100 GB | 验证 3.4 的假设 |
| 每付费用户月中继成本 | < $1（B 单价）| 毛利底线 |
| 免费用户平均月中继 GB | < 1 GB | 免费成本是否可控 |
| 直连成功率（美国用户） | ≥ 80% | 直接决定成本 |
| 首月流失、第二个月流失 | < 10% / < 8% | 目标值是拍的【推断】 |
| 支持工单数 / 付费用户 | 越少越好；记录原因 | 开源产品的运营成本常被低估 |

---

## 6. 风险和不确定

| 风险 / 空白 | 影响 | 状态 |
|---|---|---|
| **没有找到美国 AI 智能体用户付费意愿的直接证据**（没有 Reddit 原帖、没有问卷，只有教程、厂商博客和 Astropad 的产品定价） | 整个第一人群的定位建立在间接证据上 | 【未核实】。第 1–2 周的预售就是为了测这个 |
| **免费替代品够用**：Tailscale + 系统自带屏幕共享/VNC，Chrome 远程桌面 | 转化率可能低于 3% | 【已核实】Tailscale 免费档；转化率【未核实】 |
| **大厂捆绑**：OpenAI Work 桌面版对 Free 开放；Claude Dispatch 对 Pro 以上；Microsoft、Apple 在做原生智能体 | 如果它们补上"远程画面"和"无人值守"，我们的窗口会关 | 【已核实，已有调研】。对策：模型中立、Windows/Linux、自托管、审计 |
| **直接竞品抢跑**：Astropad Workbench（只 Apple），DeskVNC、GlintDesk 等 | 窗口期可能只有几个月 | 【二手】 |
| **Windows 代码签名的主体地区限制**：主体不在美国、加拿大、欧盟、英国等地区就办不下来 | 没有签名 → SmartScreen 红色警告 → 转化崩 | 【已核实（已有调研，微软自己两页也不一致）】，要实测；需要先定公司主体 |
| **SmartScreen 信誉**：即使签名，首批用户仍会看到警告 | 前几周的下载转化偏低 | 【已核实（已有调研）】 |
| **中继成本假设**：直连成功率、平均码率、中继单价 B 都没有真实数据；Stripe 费率没核实 | 毛利估算可能偏乐观 | 【推断】。第 6 周的计量就是为了替换 |
| **安全事故会让整类产品被禁**（OpenClaw 先例，中国已限制；美国企业也可能） | 品牌风险 | 【二手】。对策：默认不暴露公网、每步审计、开源可审计 |
| **滥用**：远程工具常被用于诈骗和未授权访问 | 支付封号、应用商店下架 | 【推断】：条款、举报入口、速率限制要在上线前做 |
| **价格数据质量**：TeamViewer、AnyDesk、ScreenConnect 官方页被拦或不列价，用的是搜索摘要和第三方；AnyDesk 来源之间互相冲突 | MSP 档定价参考有 ±20% 误差 | 【二手】。发布前要用浏览器人工打开核对一遍 |
| **MSP 市场数据缺失**：没有美国口径的 MSP 数量、预算、转换成本；Kaseya 2026 报告本次打不开 | MSP 路线的规模判断无依据 | 【未核实】 |
| **SOC 2、销售税、隐私法的具体门槛和费用**：本次只确认了 Stripe Tax 的机制 | 预算和时间可能偏差 | 【未核实】，需要律师和会计 |
| **商标冲突**：没有查美国商标库 | 品牌可能要改 | 【未核实】 |
| **精力分散**：个人档、MSP、AI 三条线 | 90 天做不完 | 建议：90 天内只做个人档加 MCP，MSP 只收集需求 |

---

## 附：本次实际打开的原文

- 打开并读到内容：Splashtop 定价、Parsec 定价、Tailscale 定价、Jump Desktop 定价、Nabu Casa Link、Home Assistant Cloud 页、Bitwarden 定价、n8n 定价、Cloudflare TURN FAQ、Astropad OpenClaw 博客、Stripe Tax 美国页、Tailscale Series C 博客（只有定性信息，无收入数字）。
- 打不开或没有价格：TeamViewer 官方页（只有政策文字，价格靠搜索摘要）、AnyDesk、ScreenConnect（403）、Kaseya 2026 报告（PDF 无法解析）、Hetzner 流量价（页面没写）、Chrome 远程桌面帮助页（没写免费或商业限制）。
- 沿用已有调研（未重开原文）：RustDesk 定价、签名和公证方案、模型成本、OpenClaw 和 Mac mini 的数据、向日葵、TeamViewer AI、Gartner、Astropad TechCrunch 报道。
