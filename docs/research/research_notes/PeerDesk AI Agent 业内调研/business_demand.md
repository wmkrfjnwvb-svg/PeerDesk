# PeerDesk AI Agent 业内调研：商业模式、定价、市场规模与需求证据（截至 2026-11-26）

说明：
- 标记含义：**【原文】** 本次直接抓取了官方或一手页面核对过；**【二手】** 来自媒体报道或搜索摘要，没有读到一手原文；**【低质量】** 市场研究公司的新闻稿、营销软文或 AI 生成的聚合页，只能当量级参考。
- 所有金额都注明日期。同一指标来源有冲突时，冲突写在条目里。
- 调研对象："PeerDesk: Your Computer. Your AI."：开源（Apache-2.0）远程桌面加托管云。每台电脑变成一个能从手机或电脑上对话的 AI 智能体，同时提供 MCP server 和 CLI。

---

## 问题一：可比产品怎么定价（AI 订阅里的电脑智能体、远程桌面档位和 AI 加价、按量、按席位、积分、开源公司怎么靠托管云赚钱）

### Takeaway
"从手机指挥自己电脑上的 AI"在 2026 年已经成了大模型厂商订阅里的标准功能，价格锚点是 $20 / $100 / $200（OpenAI 另有 $500），模型费用打包在订阅里。ChatGPT Work 桌面版和腾讯 WorkBuddy 对免费用户也开放，腾讯的远程控制部分不消耗 Token。远程桌面厂商目前很少给 AI 单独定价。向日葵把 MCP 做成选配，TeamViewer 把 AI 放进企业平台，ToDesk 用送积分拉新。开源远程桌面的标杆 RustDesk 只卖自托管许可证，不做 SaaS。PeerDesk 如果单独卖"AI 智能体"，会直接撞上免费和捆绑的产品。可以定价的是连接、常驻、多设备和人工接管这一层，模型费用让用户自带 Key 或者按量转嫁。

### Cited Findings

**A. 大模型厂商订阅里的"电脑智能体"**

- **OpenAI 的旧 ChatGPT agent（虚拟浏览器）已下线。** 帮助页写明"ChatGPT agent is no longer available"，让用户改用 ChatGPT Work。下线前的额度是每月 Plus 40 条、Pro 400 条、Business/Enterprise 40 条（灵活计费时每条 30 credits）。只有用户主动发起的请求计数，定时任务每运行一次算一次。【原文】 — [OpenAI Help: ChatGPT agent](https://help.openai.com/en/articles/11752874-chatgpt-agent)
- **ChatGPT Work（2026-07-09 发布）。** 桌面 App 合并了 Codex，能用本地文件和应用，支持 Computer Use（后台点击、输入、移动文件），也能跑定时任务。官方写明桌面 App 里的 Chat、Work、Codex"在所有计划可用，包括 Free"。Web 和手机端先给 Pro、Enterprise、Edu，再给 Plus 和 Business。官方没有公布具体额度，只说"复杂任务会用更多计划内额度"，计量方式和 Codex 相同。可以从手机发起任务、查看进度，但官方没说清手机发起的任务是否在用户自己的电脑上执行。Codex 每周用户超过 500 万。【原文】 — [OpenAI: ChatGPT for your most ambitious work](https://openai.com/index/chatgpt-for-your-most-ambitious-work/)；有分析称桌面版的本地模式任务不会出现在 Web 和手机端【二手】 — [Latent Space](https://latent.space/p/unpacking-chatgpt-work)
- **ChatGPT Pro 三档：$100、$200、$500/月。** Pro 100 和 Pro 500 的 Work/Codex 额度翻倍。9 月 29 日邮件里宣布的 Pro 200 降额已经取消，Pro 200 的额度上调到和高档一致。页面没有提到 agent 额度。【原文】 — [OpenAI Help: About ChatGPT Pro tiers](https://help.openai.com/articles/9793128-about-chatgpt-pro-tiers)；媒体先报道过 Pro 200 降为 Plus 的 10 倍，后被官方撤回【二手】 — [The New Stack](https://thenewstack.io/openai-halves-200-plan/)
- **Claude 的价格（Anthropic 官网定价页）：** Free $0；Pro $20/月（按年付折合 $17）；Max $100/月（Pro 用量的 5 倍，含每月 $100 API 额度）；Ultra $200/月（Pro 用量的 10 倍，含每月 $200 API 额度，已验证账户没有 5 小时限制）。Team 标准席位 $25/月（按年付 $20），高级席位 $125/月（按年付 $100）。Enterprise $20/席位/月，最少 20 席，另按 API 价计费。Pro 及以上都包含"交付和定时任务"（即 Dispatch）。【原文】 — [Claude Pricing](https://www.claude.com/pricing)
- **Claude Dispatch 加 Computer Use（2026-03-23）：** 手机和桌面共用一个会话。从手机派任务后人可以离开，Claude 在电脑上操作屏幕、打开文件、用浏览器。面向 Pro、Max、Ultra，属于研究预览，支持 macOS 和 Windows。**桌面 App 必须保持唤醒并在运行**。先用连接器，没有连接器时才操作屏幕。官方承认"不总是成功、通过屏幕操作比直接集成慢"。【原文】 — [Claude Blog: Dispatch and computer use](https://claude.com/blog/dispatch-and-computer-use)；Cowork 的 Web 和手机端于 2026 年 7 月起对 Max 用户 beta 开放【二手】 — [Help Net Security 2026-07-08](https://www.helpnetsecurity.com/2026/07/08/claude-cowork-phone-mobile-web/)
- **Perplexity Personal Computer（2026-04-16）：** 把一台用户自备的 Mac（官方推荐 Mac mini）变成 7×24 常驻智能体，首发只给 $200/月的 Max 用户。2026-05-07 新版 Mac App 向所有 Mac 用户开放，功能受 Pro/Max/Enterprise 的积分额度限制。【二手】 — [MacRumors 2026-04-16](https://www.macrumors.com/2026/04/16/perplexity-personal-computer-for-mac/)、[Engadget](https://engadget.com/2167301/perplexity-opens-up-its-personal-computer-ai-assistant-to-all-mac-users)；heise 报道 Max 价格为 $167/月（含 10,000 Compute Credits），和其他来源的 $200 冲突，可能是按年付的折算价 — [heise](https://heise.de/-11209925)
- **Perplexity Comet 浏览器** 2025-10-02 起对所有人免费。Comet Plus（新闻内容）$5/月，Pro（$20）和 Max（$200）已包含。【二手】 — [TechCrunch 2025-10-02](https://www.techcrunch.com/2025/10/02/perplexitys-comet-ai-browser-now-free-max-users-get-new-background-assistant/)、[eesel.ai 2026-06](https://www.eesel.ai/blog/perplexity-comet-pricing)
- **Google AI Ultra：** I/O 2026 之后从 $249.99 降到 $199.99，另加 $99.99 档。Project Mariner 只在 $199.99 档提供，属于早期体验且仅限美国。【二手】 — [Digital Trends](https://www.digitaltrends.com/computing/googles-ai-subscriptions-get-a-new-100-tier-a-price-cut-and-new-features-across-all-plans/)、[Google 博客 I/O 2026](https://blog.google/products-and-platforms/products/google-one/google-ai-subscriptions)、[Google One 支持页](https://support.google.com/googleone/answer/16286513)
- **Manus 的积分制：** 第三方整理的价格是 Free（每天 300 积分，每月上限 1,500）、$20/月 4,000 积分、$40/月 8,000 积分、$200/月 40,000 积分，积分不结转，按年付约 83 折。单个任务耗 200 到 9,000 积分不等，余额可以扣成负数。【低质量/二手】 — [Activepieces](https://www.activepieces.com/blog/manus-pricing-2026-plans-credits-what-tasks-cost)、[eesel.ai](https://eesel.ai/blog/manus-ai-pricing)。官方文档只写了 Free/Pro/Team，没有标价；每月重置的计划积分不结转，单独购买的积分包永不过期。【原文】 — [Manus Docs: Plans](https://manus.im/docs/introduction/plans)
- **Kimi Claw（月之暗面基于 OpenClaw 的云端托管版）：** 官方会员页称从 $39/月档起提供。部署的云主机每天固定扣约 0.6% 的会员额度，作为沙箱运行费，不用也扣。【二手，来自官方页面的搜索摘要】 — [Kimi 会员价格](https://www.kimi.com/zh-hans/help/membership/membership-pricing)
- **用户自己跑 OpenClaw 的模型费用实例：** Tailscale 博客作者用 Claude Sonnet 4.6 跑 OpenClaw，Token 费用约 $7/天。【原文，单个案例】 — [Tailscale Blog 2026-03-24](https://tailscale.com/blog/openclaw-tailscale-aperture-serve)

**B. 远程桌面产品的档位和 AI 加价**

- **TeamViewer：** 只卖年付订阅，只有发起连接的专家需要许可证。官网定价页没有直接列出价格，加购按端点、用户、存储报价。Tia（AI 排障助手）和 Session Insights（AI 记录）在页面上有介绍，但没写是否单独收费。【原文】 — [TeamViewer Pricing](https://www.teamviewer.com/en/pricing/)
- **TeamViewer 的 AI 产品线：** 2025-11-18 在 Ignite 发布 Tia（自主 IT 支持智能体，内置于 TeamViewer ONE） — [TeamViewer 新闻稿](https://www.teamviewer.com/en-mea/global/company/press/2025/teamviewer-launches-tia-intelligent-agent-autonomous-it-support/)。2026-03-31 宣布 AI 远程支持会话累计超过 100 万次，仅 3 月就超过 30 万次 — [TeamViewer 新闻稿](https://www.teamviewer.com/hu/global/company/press/2026/teamviewer-surpasses-one-million-ai-sessions-major-adoption-milestone/)。2026-04-27 推出把已解决的会话转成自动化脚本的功能 — [TeamViewer 新闻稿](https://www.teamviewer.com/pl/global/company/press/2026/teamviewer-introduces-ai-driven-scripting/)【以上为原文标题和日期，内容来自搜索摘要】
- **TeamViewer MCP（2026-09-24）：** 基于 REST API 实现租户管理、设备健康查询、重启或连接设备、读取会话日志。**不提供会话内能力**，会话内由 Tia 负责。官方原话是"永远需要 UI"。【二手，厂商页面】 — [TeamViewer: Introducing MCP integration](https://www.teamviewer.com/en/insights/introducing-teamviewer-model-context-protocol-integration/)
- **AnyDesk：** 没有查到 AI 定价。各第三方给出的价格互相冲突：Solo $22.90–37.90/月，Standard $35.90–63.90/月，Advanced $79.90–138.90/月，都按年付，Ultimate 需要询价。设备上限 Solo 100、Standard 500、Advanced 1,000。【低质量/竞品页】 — [Splashtop 对比页](https://www.splashtop.com/anydesk-pricing-comparison)、[G2](https://www.g2.com/products/anydesk/pricing)、[TSplus](https://tsplus.net/anydesk-pricing)
- **Splashtop：** 没有查到 AI 或智能体功能计入 Remote Support 价格。Remote Support 按并发技术员计价，G2 显示起价 $22。另有 Agentic Packr，是 Intune/SCCM 打包工具，100 个端点以内免费。【二手】 — [G2](https://www.g2.com/products/splashtop-inc-splashtop-remote-support/pricing)、[Splashtop Agentic Packr](https://www.splashtop.com/agentic-packr/pricing)
- **RustDesk Server Pro（只卖自托管，不是 SaaS）：** Free；Individual $11.88/月（1 用户、20 设备）；Basic $23.88/月（10 用户、100 设备）；Customized 每加 1 个用户 $1.20、每加 1 台设备 $0.12；Customized V2 每加 1 路并发 $24。只能按年付，不自动续费，没有教育优惠，FAQ 写明暂无客服支持。页面原文："This pricing is for self-hosting solution, it is NOT a SaaS subscription." 没有 AI 功能。【原文】 — [RustDesk Pricing](https://rustdesk.com/pricing)；英文页的加购价（$1、$0.10、$20）和中文、阿拉伯文页（$1.20、$0.12、$24）不一致，以抓取时的英文主页为准 — [搜索摘要](https://rustdesk.com/zh-tw/pricing)
- **向日葵（贝锐，官方价格页）：** 个人版只有年付：瓜子 ¥158、超级 ¥298 起、高性能 ¥468、全球 ¥898、小宇宙 ¥1288。企业版：办公 Pro ¥598/成员/年（5 人起），技术支持 ¥1280/成员/年，青春 ¥2560/通道/年，行业 ¥3380/通道/年。"**向日葵 MCP**（基于 MCP 协议，支持 AI 助手调用向日葵远控能力）"在各个人档（包括免费）都是选配项，可以领试用，页面没写价格。"AI 智能远控审计"标为全新功能，看起来只在行业版里。【原文】 — [向日葵价格页](https://sunlogin.oray.com/price)
- **ToDesk AI（2026-05-27 发布）：** 官方报道和营销文都没有给价格。营销文称每天登录送 1,000 积分，新用户首日送 2,100 积分。【二手/营销】 — [搜狐营销文](https://www.sohu.com/a/1031973605_458778)、[凤凰网 2026-06-01](https://tech.ifeng.com/c/8tbcCD2OFyY)
- **腾讯 WorkBuddy 手机远控（2026-10-28）：** 手机 App 直连同账号的电脑，可以看智能体的每一步，也可以直接点按操作。**远程控制只消耗流量，不消耗 Token**。一台电脑同一时间只允许一部已认证的手机连接。报道只提到 macOS 桌面端。【二手，IT 之家/凤凰网】 — [凤凰网 2026-10-28](https://tech.ifeng.com/c/8xaeluxtnMt)。WorkBuddy 目前以免费接入模型加送积分推广，商业化面向企业，"不设定具体商业化数字目标" — [证券之星 2026-10-30](https://wap.stockstar.com/detail/IG2026103000056442)
- **Astropad Workbench（2026-04，面向"给智能体用的远程桌面"）：** 免费版每天 20 分钟，不限时 $10/月或 $50/年。只支持 Apple 设备（Mac 被控，iPhone/iPad 控制），Windows 和 Linux 在计划中。【原文，TechCrunch】 — [TechCrunch 2026-04-08](https://techcrunch.com/2026/04/08/astropads-workbench-reimagines-remote-desktop-for-ai-agents-not-it-support/)

**C. 开源公司怎么用托管云和许可证赚钱**

- **n8n（fair-code 许可，不是 OSI 开源）：** 云版 Starter 约 €20/月（按年付，每月 2,500 次执行），Pro 约 €50/月（每月 10,000 次执行），按执行次数计量。自托管 Business 约 €667/月（按年付），Enterprise 需询价；Community 版自托管免费且不限执行次数。【二手】 — [n8n 定价页](https://n8n.io/pricing)、[Jetadmin 2026-09-26](https://www.jetadmin.io/blog/n8n-pricing/)
- **n8n 融资：** 2025-10 C 轮估值 $25 亿（Accel、Nvidia） — [Bloomberg](https://www.bloomberg.com/news/articles/2025-10-09/ai-agent-startup-n8n-nets-2-5-billion-valuation-with-backing-from-nvidia)；2026-06 SAP 战略投资后估值 $52 亿 — [Dealroom 2026-06-03](https://dealroom.co/news/132833-n8ns-valuation-doubles-to-5-2b-with-sap-strategic-investment/)。ARR 说法冲突：Sacra 估计 2026-04 为 $1 亿【低质量估算】 — [Sacra](https://sacra.com/c/n8n/)；另一来源称最后披露约 $4,000 万 — [Value Add VC](https://valueaddvc.com/blog/n8n-valuation-2026-5-2b-40m-arr-and-how-it-compares-to-zapier-and-make)
- **Dify：** 2026-03-09 宣布 $3,000 万 Pre-A 轮，用于企业级智能体工作流。估值没有可靠披露。【原文，新闻稿】 — [Business Wire](https://www.businesswire.com/news/home/20260309511426/en)
- **Home Assistant / Nabu Casa：** 云订阅第三方列价 $6.50/月【低质量】 — [subger](https://subger.com/en/service/home-assistant-cloud)。2026-10-02 宣布 Home Assistant Cloud 在 2026-12-02 更名为 Home Assistant Link，功能和价格不变 — [Home Assistant 博客](https://home-assistant.io/blog/2026/10/02/big-tech-ruined-the-cloud-so-were-renaming-ours/)。订阅收入直接资助 Open Home Foundation — [2026.9 发布说明](https://home-assistant.io/blog/2026/09/02/release-20269)。基金会 2025 年度总收入 CHF 884 万（授权合作加捐赠，不单指订阅收入） — [OHF 年报要点](https://www.openhomefoundation.org/media-room/annual-report-factsheet/)。2025 年报道装机 200 万户 — [State of the Open Home 2025](https://home-assistant.io/blog/2025/04/16/state-of-the-open-home-recap/)
- **RustDesk 的模式：** 客户端开源，商业服务端闭源，自托管也收费。【二手】 — [Habr 2025](https://habr.com/ru/articles/968252/)。官网自称客户端下载 3,000 万以上、Docker 下载 1,000 万以上、在线设备 1,000 万以上；营收没有公开。【二手，厂商自报】 — [rustdesk.com](https://rustdesk.com/)

### Inferences
- **价格锚点已经形成：** AI 侧是 $20（入门）、$100（重度）、$200（常驻电脑和全功能）、$500（OpenAI 顶档），都含模型费用。远程桌面侧，个人是 ¥158–1288/年（向日葵）或 $10/月（Workbench）、$11.88/月（RustDesk 自托管）；专业和 IT 是每技术员 $25–140/月（AnyDesk、TeamViewer 一档）。PeerDesk 想定在 $20 以上，就要和含模型的大厂订阅正面比；定在 $5–12/月这一档（"连接加常驻加多设备"），则和 Workbench、RustDesk、向日葵同档，更容易成立。
- **"远程控制不消耗 Token"（WorkBuddy）说明业内把远控当作智能体的配套能力，不是主要收费点。** PeerDesk 的收费重点应放在中继流量、常驻在线、多设备、团队管理和审计、托管 MCP 端点这些有真实成本或企业价值的地方。模型费用建议默认让用户自带 Key（BYOK），只在可选的托管积分上加价。
- **积分制正在成为智能体产品的主流计费方式**（Manus、Kimi Claw、ToDesk AI、ChatGPT Work 按任务复杂度扣额度）。原因是单个任务成本差异很大（Manus 200 到 9,000 积分），按席位定价覆盖不了这种波动。
- **RustDesk 证明了"开源客户端加付费自托管服务端"能赚钱，但它主动放弃了 SaaS。** n8n 和 Nabu Casa 证明"开源核心加托管云订阅"能做到很大规模。PeerDesk 的"Apache-2.0 加托管云"更接近 Nabu Casa 和 n8n 云的路线。

### Gaps
- ChatGPT Work 和 Claude Dispatch 在各档里的具体次数或时长额度没有公开，只有"按复杂度消耗"的说法。
- ToDesk 个人和企业会员价格、ToDesk AI 积分怎么换算成钱：官网价格页抓取返回 404，没有找到。
- 向日葵 MCP 选配和 TeamViewer Tia 的单独价格没有公开。
- 没有抓到 AnyDesk 官网价格，第三方数字差距很大，只能当量级参考。
- Open WebUI、Mattermost 的商业化数据本次没有查到。
- ChatGPT Go 和 Plus 的当前价格本次没有直接核实（Plus 长期是 $20，属于背景常识，这里不作为结论）。

---

## 问题二：市场规模（远程桌面、AI 智能体、RMM/IT 自动化），注明发布方和年份

### Takeaway
远程桌面软件市场 2025 年各家估计集中在 $31–48 亿，全是付费报告公司的新闻稿，口径不透明。可以当真实收入锚点的是 TeamViewer：2026 年三季度 ARR €7.369 亿，同比 −0.6%（固定汇率），全年收入指引下调到 −2% 到 −1%，说明传统远程桌面在存量竞争。AI 智能体市场 2025 年约 $78 亿、2030 年约 $480–530 亿（MarketsandMarkets、BCC，都是新闻稿），增速高但数字很虚。RMM/MSP 赛道最硬的数据是 NinjaOne 估值 $123 亿（2026-06）。中国远控市场没有可靠的规模数字。

### Cited Findings

**远程桌面软件市场（以下全部是市场研究公司的新闻稿或摘要，【低质量】）**
- Fortune Business Insights：2025 年约 $39.2 亿，2026 年 $46.3 亿，2034 年 $193.6 亿，CAGR 19.6% — [FBI](https://www.fortunebusinessinsights.com/remote-desktop-software-market-104278)
- Research and Markets（OG Analysis）：2025 年 $41 亿，2034 年 $140 亿，CAGR 14.7% — [Research and Markets](https://www.researchandmarkets.com/reports/6185692/remote-desktop-software-market-outlook-market)
- SNS Insider：2025 年 $39.6 亿，2035 年 $231.8 亿 — [SNS Insider](https://www.snsinsider.com/reports/remote-desktop-software-market-10980)
- Research Nester：2025 年 $37.4 亿，2035 年 $143.6 亿 — [Research Nester](https://www.researchnester.com/reports/remote-desktop-software-market/5632)
- TechSci：2025 年 $37.6 亿，2031 年 $99.6 亿 — [TechSci](https://www.techsciresearch.com/report/remote-desktop-software-market/4548.html)
- The Business Research Company：2025 年 $31.3 亿，2029 年 $61.3 亿 — [GII 转载](https://www.gii.tw/report/tbrc1672611-remote-desktop-software-global-market-report.html)
- Market Intelo：2025 年 $48 亿，CAGR 12.3% — [Market Intelo](https://marketintelo.com/report/remote-desktop-software-market/amp)

**真实营收锚点（上市公司和私有公司）**
- **TeamViewer 2026 年三季度**（2026-11-03 发布）：收入 €1.845 亿，同比 −2.2%（固定汇率）；ARR €7.369 亿，−0.6%（固定汇率）；调整后 EBITDA 利润率 42.8%。FY2026 收入增长指引从 0% 到 +3% 下调到 −2% 到 −1%，原因是企业大单转化推迟和欧洲的不确定性。企业业务走强，SMB 企稳。【二手，Quartr 的 AI 摘要，没有读到公司原文】 — [Quartr Q3 2026](https://quartr.com/events/teamviewer-ag-tmv-q3-2026_3s1TBnKs)；"三季度卖得更少、下调全年预测"的媒体报道 — [IT Reseller](https://www.itreseller.ch/Artikel/106525/Teamviewer_senkt_Jahresprognose_nach_Umsatzrueckgang.html)
- **TeamViewer 2026 年一季度：** 企业 ARR +8%（固定汇率）；客户累计生成 140 万份以上 AI 会话摘要；TeamViewer ONE 的 ARR 环比翻倍以上。【二手，新闻稿标题加摘要】 — [EQS/Deutsche Börse Q1 2026](https://live.deutsche-boerse.com/nachrichten/EQS-News-TeamViewer-Q1-2026-Revenue-in-line-with-expectations-Enterprise-ARR-up-8-cc-full-year-2026-guidance-reaffirmed-488f3939-8696-4ac4-95c6-cc7069957af6)
- **AnyDesk** 年收入估计 $2,500–5,000 万。【低质量估算】 — [搜索摘要来源 Similarweb 页](https://www.similarweb.com/company/anydesk.com)

**AI 智能体市场（新闻稿，【低质量】）**
- MarketsandMarkets：2025 年 $78.4 亿，2030 年 $526.2 亿，CAGR 46.3%；早一版是 2024 年 $51 亿到 2030 年 $471 亿 — [PR Newswire](https://prnewswire.co.uk/news-releases/ai-agents-market-worth-52-62-billion-by-2030---exclusive-report-by-marketsandmarkets-302435498.html)、[早期版本](https://prnewswire.com/news-releases/ai-agents-market-worth-47-1-billion-by-2030---exclusive-report-by-marketsandmarkets-302246356.html)
- BCC Research：2030 年 $483 亿，CAGR 43.3%（2025–2030） — [BCC](https://www.bccresearch.com/pressroom/ait/ai-agents-market-to-grow-433-annually)
- MarketsandMarkets（企业级 agentic AI）：2025 年 $67.6 亿，2030 年 $460.4 亿 — [MarketsandMarkets](https://www.marketsandmarkets.com/PressReleases/enterprise-agentic-ai.asp)

**AI 头部公司的真实收入（用来衡量"AI 订阅"这块蛋糕）**
- ChatGPT：2026 年 2 月底周活约 9 亿、付费订阅 5,000 万。【二手，TechCrunch】 — [TechCrunch 2026-02-27](https://techcrunch.com/2026/02/27/chatgpt-reaches-900m-weekly-active-users)
- OpenAI 年化收入：2026 年 8 月超过 $400 亿（Bloomberg），9 月底接近 $500 亿（FT），但另有"低于报道"的说法。【二手，口径冲突】 — [BGOV/FT](https://news.bgov.com/artificial-intelligence/openais-annualized-revenue-nears-50-billion-ft-says)、[Semafor](https://semafor.com/article/08/14/2026/openai-revenue-set-to-top-40-billion)

**RMM / MSP / IT 自动化**
- NinjaOne 估值 $123 亿（2026-06，超过 $4 亿的融资，主要是老股转让）。前一轮是 2025-02 的 $50 亿。【原文，新闻稿标题】 — [NinjaOne 新闻稿](https://www.ninjaone.com/press/12-3-billion-valuation/)、[TNW](https://thenextweb.com/news/ninjaone-12-3-billion-valuation-secondary-round)
- NinjaOne 2025 年收入约 $5 亿。【低质量估算，Sacra】 — [Sacra](https://sacra.com/c/ninjaone/)
- RMM 份额（转引 Canalys）：Kaseya 25.9%，ConnectWise 25.4%，NinjaOne 约 9.8%；全球托管服务市场 2026 年预计 $3,800 亿。【二手转引，没有核实原报告】 — [Angel Investors Network](https://angelinvestorsnetwork.com/venture-capital/ninjaone-400-million-series-f-12-billion-valuation-it-management)
- Kaseya 2026 年 4 月推出 agentic IT 管理与安全平台；ConnectWise 推出内嵌 agentic AI 的 ITSM 平台。【二手】 — [Channel Dive](https://channeldive.com/news/ninjaone-kaseya-connectwise-msp-platform-rmm/808905)
- Omdia 有《RMM and PSA Vendor Performance Index 2Q26》，内容没有取到 — [Omdia](https://omdia.tech.informa.com/om147801/rmm-and-psa-vendor-performance-index-2q26)

**中国**
- 没有找到可靠的中国远控市场规模（亿元）。有转载文称"2025 年国内远控月活突破 1.8 亿""CAGR 超过 25%（信通院白皮书）"，但没有原文出处。【低质量】 — [新浪财经转载](https://finance.sina.com.cn/roll/2025-12-24/doc-inhcwtcp4552686.shtml)
- 贝锐（向日葵）自称接入设备 26 亿以上、注册用户 1.2 亿以上、企业客户 150 万以上。【二手，厂商自报】 — [新浪 2026-04-22](https://news.sina.cn/sx/2026-04-22/detail-inhvhmie4930443.d.html)
- 2026 年中国远控行业白皮书称"传统远控红利见顶、进入存量整合期"，并说 ToDesk 领先。【低质量，第三方白皮书或软文】 — [IT 之家](https://www.ithome.com/0/969/850.htm)

### Inferences
- 用 TeamViewer 一家约 €7.4 亿的 ARR 倒推，全球远程桌面和远程支持市场在几十亿美元量级，和新闻稿的 $31–48 亿大致吻合，但**零增长**（TeamViewer 2026 年负增长）。纯远程桌面是存量市场，靠"更便宜或开源"抢份额的空间有限。增量来自 AI：TeamViewer 一季度 ONE 的 ARR 翻倍、AI 会话一个月 30 万次。
- AI 智能体市场的新闻稿数字（2030 年约 $500 亿）不可靠，但 OpenAI 年化 $400–500 亿、ChatGPT 付费 5,000 万订阅这些真实收入说明，"AI 订阅"本身是比远程桌面大 10 倍以上的钱池。PeerDesk 的商业故事应该是"从 AI 订阅的钱池里分到连接层那一份"，而不是"抢 TeamViewer 的份额"。
- RMM/MSP 是付费意愿最明确的 B 端（NinjaOne 估值 $123 亿），而且头部都在加 agentic AI。PeerDesk 进 MSP 需要的权限、审计、多租户成本高，不适合早期作为主战场。

### Gaps
- 没有 Gartner/IDC/Forrester 对远程桌面、远程支持或 RMM 的权威市场规模（都是付费内容）。
- 中国远控市场规模、ToDesk 和向日葵的份额与营收没有可靠来源。
- "电脑智能体（computer-use agent）"作为细分市场没有任何靠谱的规模数据。
- TeamViewer 三季度的 SMB/企业拆分和 AI 相关 ARR 没有读到公司原文。

---

## 问题三：需求证据（"用手机让 AI 操作我的电脑"、"让家里电脑发我个文件"、IT 自动化、MSP；社区帖子、调研、App 数据、MCP 数据）

### Takeaway
需求证据很强，而且在 2026 年已经被验证过。OpenClaw（自己电脑上的智能体，通过聊天软件远程指挥）GitHub 星标约 39 万，超过 React。它在中国引发"养龙虾"热潮，Mac mini 断货，腾讯云一键部署的用户 3 月初就超过 10 万。腾讯 WorkBuddy 2026 年 9 月月活 3,235 万。Codex 每周 500 万用户。MCP SDK 每月下载 9,700 万次（2025-12）。企业侧更谨慎：Gartner 认为超过 40% 的 agentic 项目到 2027 年底会被取消，只有 15% 的 IT 应用负责人在考虑完全自主的智能体。

### Cited Findings

**消费者和开发者侧：自己电脑上的常驻智能体**
- **OpenClaw**（前身 Clawdbot/Moltbot）：在用户电脑上运行的智能体网关，通过 WhatsApp、Telegram、Slack、iMessage、Teams 等聊天软件远程指挥。2026-03-02/03 星标超过 React（约 24.5 万）；2026 年 10 月约 39.2 万。1 月 29 日的发布帖称一周 200 万访客。【二手】 — [Analytics India Mag](https://analyticsindiamag.com/ai-news/openclaw-overtakes-react-in-total-github-stars)、[Trendshift](https://trendshift.io/repositories/19374)、[Star History 2026-02](https://www.star-history.com/blog/clawflare)。星标不等于用户数。
- OpenAI 2026 年 2 月聘请 OpenClaw 作者 Peter Steinberger 负责个人智能体，并承诺 OpenClaw 以基金会形式继续开源。【二手】 — [Decrypt](https://decrypt.co/356730)
- **中国"养龙虾"：** 截至 2026-03-06，腾讯云轻量服务器上的 OpenClaw 用户超过 10 万，线下免费安装活动一天部署 500 多人。【二手，财联社/新京报】 — [财联社](https://www.cls.cn/detail/2309484)、[新京报](https://www.bjnews.com.cn/detail/1773055598129368.html)
- **Mac mini 因为 OpenClaw 断货：** 2026 年 3 月在中国卖断，深圳加价最多 600 元 — [SCMP](https://www.scmp.com/tech/tech-trends/article/3346538/apples-mac-mini-selling-out-across-china-openclaw-fever-rages)、[CnTechPost](https://cntechpost.com/2026/03/13/china-openclaw-frenzy-turns-apple-mac-mini-scarce-commodity)；2026 年 4 月美国官网 $599 基础款售罄。Tim Cook 在 Q2 财报会上说 Mac mini 和 Mac Studio 售罄且可能持续数月，同时也提到内存供应紧张 — [TechCrunch 2026-04-24](https://techcrunch.com/2026/04/24/mac-mini-price-expensive-ebay-shortage-ai-memory/)、[TNW](https://thenextweb.com/news/claude-mac-mini-openclaw-ai-agents-shortage)【二手；缺货原因是需求和内存供应两者叠加】
- **远程访问是 OpenClaw 用户的刚需和痛点：** Censys 统计 2026-01-31 公网暴露的实例超过 2.1 万；Bitsight 在 2026-01-27 到 02-08 观察到超过 3 万。社区普遍推荐用 Tailscale、WireGuard、SSH 隧道、Cloudflare Tunnel 访问家里的智能体。【二手】 — [Bitsight](https://www.bitsight.com/blog/openclaw-ai-security-risks-exposed-instances)、[DigitalOcean](https://www.digitalocean.com/resources/articles/openclaw-security-challenges)；Tailscale 官方博客专门写了 OpenClaw 的安全远程访问方案（Serve 加 Aperture 网关） — [Tailscale 2026-03-24](https://tailscale.com/blog/openclaw-tailscale-aperture-serve)【原文】
- **V2EX 帖子：** 有用户问"在外面用手机指挥家里电脑上的 Codex、Cursor 写代码，用什么方案好"，要求不依赖梯子、不会被封。回复里有 OpenChamber（自带端到端加密打洞）、VibeAround、自制的手机终端 App 等。【原文帖子，内容来自搜索摘要】 — [V2EX t/1230296](https://www.v2ex.com/t/1230296)、[V2EX t/1228751](https://v2ex.com/t/1228751)、[V2EX t/1217542](https://www.v2ex.com/t/1217542)
- **知乎：** 有文章介绍手机发消息，让电脑上的 Codex 借助 computer use 和 Chrome 插件增删改文件、操作浏览器。【二手】 — [知乎](https://www.zhihu.com/tardis/jm/art/2039780868613461142)
- **Product Hunt 和独立开发者大量做"手机控制电脑上的智能体"：** Aerostack、DeskVNC（"给 AI 和人用的 VNC/RDP"，人接管时智能体释放控制）、GlintDesk、通过 MCP 让 AI 远程访问家里电脑的工具（作者注明"目标电脑必须开机在线"）。HN 上同类帖子得分很低（1 分）。【二手，多为发布帖】 — [Product Hunt: DeskVNC](https://www.producthunt.com/p/github-513/deskvnc-remote-for-ai-agents-and-humans)、[Product Hunt: GlintDesk](https://www.producthunt.com/p/glintdesk/glintdesk-hands-on-a-remote-desktop-that-actually-gets-ai)、[HN](https://news.ycombinator.com/submitted?id=leddo)、[Indie Hackers: OpenClaw mobile](https://www.indiehackers.com/post/i-shipped-openclaw-mobile-full-agent-control-from-your-phone-no-more-11pm-laptop-runs-ad705e129b)
- **Astropad（盈利、没有外部融资、客户超过 10 万）专门推出给智能体用的远程桌面 Workbench。** 理由是智能体不需要屏幕，但人需要不时上去看日志、重启卡住的任务、点确认框，现有远程桌面不是为这个设计的，终端和 Telegram 方案也不够。【原文，TechCrunch】 — [TechCrunch 2026-04-08](https://techcrunch.com/2026/04/08/astropads-workbench-reimagines-remote-desktop-for-ai-agents-not-it-support/)

**大平台的使用量**
- **腾讯 WorkBuddy：** QuestMobile 统计 2026 年 9 月办公类智能体月活 3,235.3 万，排第一，季度环比 +647.1%；累计完成 90 亿次任务请求；开放平台合作伙伴超过 5 万、Skills 超过 20 万。【二手，证券之星转述腾讯生态大会】 — [证券之星 2026-10-30](https://wap.stockstar.com/detail/IG2026103000056442)。QuestMobile 统计 2026 年 7 月 PC 客户端月活 658.2 万；有报道称 11 月日活破千万，但只有单一来源 — [36kr](https://eu.36kr.com/zh/p/4075162266735752)【二手】
- **Codex** 每周用户超过 500 万，其中超过 100 万人用它做软件开发以外的工作。【原文】 — [OpenAI 2026-07-09](https://openai.com/index/chatgpt-for-your-most-ambitious-work/)
- **ChatGPT** 周活约 9 亿，付费订阅 5,000 万（2026-02）。【二手】 — [TechCrunch](https://techcrunch.com/2026/02/27/chatgpt-reaches-900m-weekly-active-users)

**MCP 生态**
- 2025-12-09 MCP 捐给 Linux 基金会下的 Agentic AI Foundation（由 Anthropic、Block、OpenAI 共同发起）。当时的数据：Python 和 TypeScript SDK **每月下载超过 9,700 万次**，**活跃公开 server 超过 1 万个**。【原文，官方博客】 — [MCP Blog 2025-12-09](https://blog.modelcontextprotocol.io/posts/2025-12-09-mcp-joins-agentic-ai-foundation/)、[Anthropic](https://anthropic.com/news/donating-the-model-context-protocol-and-establishing-of-the-agentic-ai-foundation)
- 后续数字（第三方）：官方 Registry 在 2026-09-24 有 35,491 个 server、20,581 个发布者；Glama 索引在 2026-10-11 有 97,619 个；有说法称 2026 年 10 月 SDK 每月下载"接近 5 亿"，原文没有核实。【低质量/二手】 — [DEV: MCP in 2026 numbers](https://dev.to/grahamduescn/mcp-in-2026-the-numbers-behind-the-ecosystem-explosion-3j72)、[DEV: registry by the numbers](https://dev.to/amareswer/the-mcp-registry-by-the-numbers-38nc)
- 远控厂商已经开始做 MCP 和 CLI：向日葵 MCP（价格页选配项）【原文】 — [向日葵价格页](https://sunlogin.oray.com/price)；向日葵企业 CLI（2026 年 6 月），文章称 OrayClaw、Cursor、Claude Code、GitHub Copilot 都能通过终端调用远控能力【二手】 — [CSDN](https://adg.csdn.net/6a572195662f9a54cb8f96cb.html)；TeamViewer MCP（2026-09）【二手】 — [TeamViewer](https://www.teamviewer.com/en/insights/introducing-teamviewer-model-context-protocol-integration/)

**企业和 IT 侧调研**
- Gartner（2025-06）：超过 40% 的 agentic AI 项目会在 2027 年底前取消，原因是成本上升、价值不清、风险控制不足；并点名"agent washing"（把聊天机器人包装成智能体）。【原文，新闻稿】 — [Gartner 2025-06-25](https://www.gartner.com/en/newsroom/press-releases/2025-06-25-gartner-predicts-over-40-percent-of-agentic-ai-projects-will-be-canceled-by-end-of-2027)
- Gartner（2025-08）：预测到 2026 年底 40% 的企业应用会集成任务型智能体，2025 年不到 5%。这是预测，不是实测。 — [Gartner 2025-08-26](https://gartner.com/en/newsroom/press-releases/2025-08-26-gartner-predicts-40-percent-of-enterprise-apps-will-feature-task-specific-ai-agents-by-2026-up-from-less-than-5-percent-in-2025)
- Gartner（2025-09，调研 360 位 IT 应用负责人）：只有 15% 在考虑、试点或部署**完全自主**的智能体；75% 在试点或部署某种智能体。 — [Gartner 2025-09-30](https://www.gartner.com/en/newsroom/press-releases/2025-09-30-gartner-survey-finds-just-15-percent-of-it-application-leaders-are-considering-piloting-or-deploying-fully-autonomous-ai-agents)
- McKinsey《State of AI 2025》：23% 的受访组织在规模化使用 agentic AI，39% 在试验；任何单一职能里规模化的不超过 10%。《State of AI 2026》：年收入超过 $10 亿的大企业有 40% 在规模化使用智能体（上年 27%），小企业持平在 22%；报告 AI 对 EBIT 有正贡献的约 37%，基本没变。【二手，McKinsey 页面摘要】 — [McKinsey 2025](https://www.mckinsey.com/capabilities/quantumblack/our-insights/the-state-of-ai-2025)、[McKinsey 2026](https://www.mckinsey.com/capabilities/quantumblack/our-insights/the-state-of-ai)
- IT 支持场景：TeamViewer AI 远程支持会话累计超过 100 万次（2026-03-31），一季度 AI 会话摘要超过 140 万份。 — [TeamViewer 新闻稿](https://www.teamviewer.com/cs/global/company/press/2026/teamviewer-surpasses-one-million-ai-sessions-major-adoption-milestone/)、[EQS Q1 2026](https://live.deutsche-boerse.com/nachrichten/EQS-News-TeamViewer-Q1-2026-Revenue-in-line-with-expectations-Enterprise-ARR-up-8-cc-full-year-2026-guidance-reaffirmed-488f3939-8696-4ac4-95c6-cc7069957af6)

### Inferences
- **"用手机让家里或公司电脑上的 AI 干活"已经从小众变成大众需求，**OpenClaw、WorkBuddy、Mac mini 断货是三个相互独立的信号。需求的核心是三件事：常驻、在本机、能从手机触达。这三件正是远程桌面的老本行（无人值守、穿透 NAT、跨设备身份）。
- **用户的真实痛点是连接和安全，而不是"再来一个智能体"。** OpenClaw 公网暴露的实例有 2–3 万，社区靠 Tailscale、Cloudflare Tunnel 自己拼。V2EX 上的明确要求是"不依赖梯子、不会被封"。Astropad 的判断是"人仍然要看屏幕、点确认"。这些都指向 PeerDesk 的现有能力：P2P 打洞、中继、低延迟画面、人工接管。
- **企业侧要的是可控和审计，而不是完全自主。** Gartner 的 15%、TeamViewer 把会话内能力留给 UI、向日葵把"AI 智能远控审计"放在最贵的行业版，都说明 B 端会为"人在回路、可审计、可接管"付钱。
- **MCP 已经是事实标准，**远控厂商都在做（向日葵、TeamViewer）。PeerDesk 提供 MCP server 是必备项，不是差异点。

### Gaps
- 没有找到"让家里电脑把文件发给我"这类具体场景的问卷或量化数据，只有社区帖子和产品宣传里的场景描述。
- 没有找到 Claude 或 ChatGPT 桌面 App 的下载量或日活（Sensor Tower 类数据没有公开）。
- OpenClaw 的真实活跃用户数没有可靠数据（只有星标和"150 万个智能体"这类说法）。
- Reddit 上的需求帖本次没有直接检索到原帖。

---

## 问题四：谁做过"远程电脑即智能体"，结果如何（创业公司、YC、中国产品；融资、增长、关停）

### Takeaway
2026 年这个方向被大公司集体吃下：Anthropic（Dispatch，并收购 Vercept 后关掉 Vy）、OpenAI（ChatGPT Work 桌面版，聘请 OpenClaw 作者）、Meta（约 $20 亿收购 Manus，3 月推出 My Computer 本地桌面版）、Perplexity（Personal Computer）、腾讯（WorkBuddy 加手机远控）。远控厂商里，ToDesk（5 月，基于 OpenClaw）和向日葵（MCP、CLI、接入 WorkBuddy）在中国先动了，TeamViewer 只做了管理层的 MCP 和会话内的 Tia。独立创业公司（Simular、Cua、Orgo、Astropad Workbench）融资规模很小，被收购或被捆绑的风险高。

### Cited Findings

**大厂和明星公司**
- **Manus → Meta：** 2025-12-29 Meta 宣布收购，WSJ 报道约 $20 亿。Manus 当时称 8 个月 ARR 超过 $1 亿、年化收入 $1.25 亿；交易后停止中国业务和运营。【二手】 — [TechCrunch 2025-12-29](https://techcrunch.com/2025/12/29/meta-just-bought-manus-an-ai-startup-everyone-has-been-talking-about)、[The Register](https://www.theregister.com/2025/12/30/meta_acquires_manus/)
- **Manus My Computer（2026-03-16）：** Manus Desktop App 把智能体从云端沙箱带到用户本机（macOS、Windows），在终端执行命令、读写本地文件、启动和控制应用。只能访问用户授权的目录，每条命令都要用户批准（可以选"总是允许"）。【二手加官方博客】 — [Manus Blog](https://manus.im/en/blog/manus-my-computer-desktop)、[9to5Mac](https://9to5mac.com/2026/03/17/metas-manus-launches-my-computer-to-turn-your-mac-into-an-ai-agent/)。Dealroom 称 Manus 的 Cue 智能体 App 于 2026-09-28 上线，另有一轮 $5 亿、估值 $40 亿的融资在谈。被 Meta 收购后还在融资，这一点存疑。【二手】 — [Dealroom 2026-09-30](https://dealroom.co/charts/personal-ai-agent-startups)
- **Vercept → Anthropic（2026-02-25）：** 收购用于增强 Claude 的 computer use。Vercept 的产品 Vy（能操作远程 MacBook 的云端智能体）在几周内关停，报道称 2026-03-25 停止服务；种子轮至少 $1,600 万。【原文标题加二手】 — [Anthropic 公告](https://anthropic.com/news/acquires-vercept)、[GeekWire](https://www.geekwire.com/2026/anthropic-acquires-vercept-in-early-exit-for-one-of-seattles-standout-ai-startups/)、[TechDogs](https://www.techdogs.com/tech-news/td-newsdesk/anthropic-acquires-computer-use-ai-startup-vercept-as-vy-shuts-down)
- **Anthropic Dispatch 加 Computer Use**（2026-03-23，见问题一）；**OpenAI ChatGPT Work**（2026-07-09，见问题一）；**Perplexity Personal Computer**（2026-04-16，见问题一）。
- **Dealroom（2026-09-30）统计 2026 年 9 个大厂个人智能体上线：** 阿里千问 App（1/15）、三星（2/25）、小米 miclaw（3/6）、Google Gemini Spark（5/26）、Grok Bot（8/11，自带云电脑的常驻智能体）、华为小艺（9/7）、Meta Muse（9/8）、Apple Siri 公测（9/14）、OpenAI Dots（9/29，"自带云电脑的常驻智能体"）。2026 年至今 21 家个人智能体创业公司融资 $18 亿，其中 Instinct（通过 iMessage、短信、电话工作）一家占 $14 亿；不算 Instinct 只有 $4.4 亿。2025 年是 18 家共 $6.2 亿。【二手，Dealroom】 — [Dealroom](https://dealroom.co/charts/personal-ai-agent-startups)

**创业公司（电脑操作和给智能体的桌面）**
- **Simular：** 2025-12-02 宣布 $2,150 万 A 轮（Felicis），累计约 $2,700 万；macOS 智能体先发，Windows 随后；2026 年 9 月 Sai 正式上线。【二手加原文标题】 — [TechCrunch 2025-12-02](https://techcrunch.com/2025/12/02/simular-releases-mac-os-ai-agent-raises-21-5m-from-felicis-with-windows-coming-soon)、[Simular](https://simular.ai/articles/simular-raises-21-5m-to-build-autonomous-computer-agents)
- **Cua（YC，给智能体的虚拟桌面和沙箱）：** 聚合站显示累计约 $50 万，阶段说法冲突。【低质量】 — [StartupHub](https://www.startuphub.ai/startups/cua-ai)、[YC](https://www.ycombinator.com/companies/cua/jobs)
- **Orgo（给智能体的云桌面，退出登录后继续运行）：** 还在运营，融资很少（目录站称 $12.5 万种子）。**Bytebot（YC S21）** 有聚合站标"已收购"，但没有说收购方，存疑。【低质量】 — [Fondo](https://fondo.com/blog/orgo-gives-ai-agents-desktops-that-keep-running-after-logout)、[Altss](https://altss.com/companies/yc/bytebot)
- **Astropad Workbench**（2026-04，$10/月，见上）。

**远程桌面厂商**
- **ToDesk AI（2026-05-27）：** "基于 OpenClaw 深度定制"。一个账号控制多台设备当"桌面员工"，自研 Computer Use，每一步以卡片展示、可以暂停或回滚。可以接入微信、企业微信、飞书，国内版能在 GLM、Qwen、Kimi 之间切换。沿用 ToDesk 的设备授权和全程录屏追溯。场景是手机一句话让异地办公室电脑把报表发给老板。没有公布用户数。【二手，凤凰网】 — [凤凰网 2026-06-01](https://tech.ifeng.com/c/8tbcCD2OFyY)
- **向日葵：** MCP 选配（价格页）【原文】 — [向日葵价格页](https://sunlogin.oray.com/price)；2026 年 6 月推出企业 CLI【二手】 — [CSDN](https://adg.csdn.net/6a572195662f9a54cb8f96cb.html)；2026 年 7 月成为首个接入腾讯 WorkBuddy 的远程控制方案，在连接器里启用即可【二手，IT 之家转载】 — [搜索结果：IT 之家/同花顺 2026-07-17](https://stock.10jqka.com.cn/20260717/c678249388.shtml)
- **TeamViewer：** Tia（2025-11），AI 脚本化（2026-04），MCP（2026-09，只做管理层）。2026 年三季度整体收入仍然下滑（见问题二）。

**中国手机侧智能体（OS 和超级 App 的博弈案例）**
- **豆包手机助手**（2025-12-01，中兴努比亚工程机）：能跨 App 自动操作，随后**微信、支付宝、淘宝和多家银行 App 风控拦截**，微信协议禁止第三方工具操控账号。豆包宣布"规范化调整"，限制刷分、刷激励类场景。【二手】 — [澎湃](https://www.thepaper.cn/newsDetail_forward_32094236)、[香港 01](https://global.hk01.com/数码生活/60301489/豆包ai手机遭微信阿里限制-跨app自动操作引争议-官方宣布调整)

### Inferences
- **"远程电脑即智能体"这条路被验证了，但价值被大厂和模型公司捕获。** 独立做"电脑操作智能体"的公司要么被收购（Vercept、Manus），要么融资很小（Cua、Orgo）。Simular 是少数还在独立发展的。PeerDesk 不应该定位成"又一个电脑智能体"。
- **中国远控厂商的打法是"远控加智能体加接入国内平台"，**ToDesk 自己做智能体，向日葵做 MCP 和 CLI 并接入 WorkBuddy。它们的分发优势（上亿用户）让 PeerDesk 很难在中国 C 端正面竞争。PeerDesk 在中国可能更适合走"开源、可自托管、隐私"这个差异点（贴近 RustDesk 的用户群）。
- **WorkBuddy 在智能体 App 里内置远程桌面（2026-10）是最直接的信号：**智能体厂商最后也要补"人看屏幕、人接管"的能力。这一层是 PeerDesk 的强项，可以做成可嵌入的组件或 MCP，卖给智能体厂商或开发者（B2B2C），而不是只卖给终端用户。

### Gaps
- ToDesk AI、向日葵 AI 功能的用户量和收入没有任何公开数据。
- 2024–2025 年失败或关停的"远程电脑智能体"案例（例如 Adept、Rabbit 等）本次没有系统检索，只核实了 Vercept/Vy 的关停。
- OpenAI 收购 Sky（Mac 端自然语言界面）等交易本次没有核实，所以没有写入。
- Simular、Astropad Workbench 的用户数和收入没有公开。

---

## 问题五：竞争风险（OS 厂商和模型厂商捆绑、分发优势），以及远程桌面厂商独有的东西

### Takeaway
最大的风险是捆绑：OpenAI（桌面 Work 免费可用）、Anthropic（Pro 以上带 Dispatch）、腾讯（WorkBuddy 自带免费的手机远控）、Microsoft（Windows 原生 agent workspace 和 MCP 连接器，预览中）、Apple（macOS 27 的 agentic Siri 和 App Intents）都在把"手机指挥电脑上的 AI"做成免费或订阅内的标准功能，分发优势远远超过 PeerDesk。监管和平台风险也是真的：中国限制银行、国企、政府机关使用 OpenClaw，CNCERT 两次警告，微信封杀豆包的跨 App 操作。远程桌面厂商独有的东西集中在连接层：常驻无人值守的被控端、NAT 穿透和中继、跨设备身份、低延迟画面和人工接管、多设备统一管理、审计录屏。大厂的方案恰恰在这些地方薄弱，例如 Dispatch 要求桌面 App 保持唤醒，WorkBuddy 一台电脑只能连一部手机且只支持 macOS，TeamViewer MCP 不做会话内能力。

### Cited Findings

**OS 厂商**
- **Microsoft：** 2025 年 11 月起向 Windows Insider 灰度推送 Agent Workspace 和 Copilot Actions（默认关闭）。智能体用独立账户运行，在隔离的工作区里操作应用和文件；"agent connectors"就是 MCP server。**没有查到正式发布日期。**【二手，微软支持页摘要】 — [Microsoft Support: Experimental Agentic Features](https://support.microsoft.com/en-us/windows/ai/ai-features/experimental-agentic-features)、[Windows IT Pro Blog Ignite 2025](https://techcommunity.microsoft.com/blog/windows-itpro-blog/evolving-windows-new-copilot-and-ai-experiences-at-ignite-2025/4469466)；Windows Latest 认为 Windows 需要第三方 App 提供独特的智能体体验 — [Windows Latest 2026-01-07](https://www.windowslatest.com/2026/01/07/microsoft-is-encouraging-developers-to-build-next-gen-ai-agents-for-windows-11-as-copilot-alone-isnt-enough)
- **Apple：** WWDC 2026（6 月）上 macOS 27 "Golden Gate" 的 Siri 可以在 Spotlight 里对话，App Intents 驱动的 agentic Siri 能代用户执行操作，支持屏幕感知。MCP 只出现在 Xcode 里，没有进 Siri。**Siri AI 暂不在中国和欧盟提供。**没有查到 Mac 上"操作屏幕"类的 computer use。【二手】 — [MacRumors 2026-06-09](https://macrumors.com/2026/06/09/apple-outlines-major-ai-and-developer-tool-updates)、[Engadget](https://www.engadget.com/2189698/everything-announced-at-apples-wwdc-2026-keynote/)；Dealroom 记录 Apple Siri 智能体于 2026-09-14 公测 — [Dealroom](https://dealroom.co/charts/personal-ai-agent-startups)

**模型厂商和平台的捆绑（详见问题一、四）**
- OpenAI ChatGPT Work 桌面版对 Free 开放 Computer Use【原文】 — [OpenAI](https://openai.com/index/chatgpt-for-your-most-ambitious-work/)；Anthropic Dispatch 在 Pro 以上提供，但要求"桌面 App 必须保持唤醒并在运行"【原文】 — [Claude Blog](https://claude.com/blog/dispatch-and-computer-use)；腾讯 WorkBuddy 手机远控只消耗流量不消耗 Token，一台电脑只能连一部手机，报道只提到 macOS 端【二手】 — [凤凰网](https://tech.ifeng.com/c/8xaeluxtnMt)
- OpenAI Dots、Grok Bot 走的是"智能体自带云电脑"，不依赖用户自己的电脑。【二手】 — [Dealroom](https://dealroom.co/charts/personal-ai-agent-startups)

**监管、平台和安全风险**
- 中国工信部 2026-02-05 发布 OpenClaw 安全提示（默认或弱配置风险高）。2026 年 3 月银行、国企、政府机关收到通知，不得在办公设备安装 OpenClaw（严格程度各报道说法不一）。CNCERT 两次警告"默认安全配置极弱"、存在多个可窃取凭据的严重漏洞。【二手，Bloomberg 等，多为匿名信源】 — [Bloomberg 2026-03-11](https://www.bloomberg.com/news/articles/2026-03-11/china-moves-to-limit-use-of-openclaw-ai-at-banks-government-agencies)、[The Register 2026-03-12](https://www.theregister.com/2026/03/12/china_cert_openclaw_security_warning/)、[Tom's Hardware](https://www.tomshardware.com/tech-industry/artificial-intelligence/china-bans-openclaw-from-government-computers-and-issues-security-guidelines-amid-adoption-frenzy)
- 超级 App 封杀第三方智能体：微信、支付宝、淘宝、银行 App 拦截豆包手机助手的跨 App 操作（2025-12）。 — [澎湃](https://www.thepaper.cn/newsDetail_forward_32094236)
- 安全暴露：OpenClaw 公网暴露实例 2–3 万（2026 年 1–2 月）。 — [Bitsight](https://www.bitsight.com/blog/openclaw-ai-security-risks-exposed-instances)
- Claude 官方提示：被操纵的指令或钓鱼链接可能在电脑上触发难以撤销的操作；有自动的提示注入扫描。【原文和二手】 — [Claude Blog](https://claude.com/blog/dispatch-and-computer-use)、[Help Net Security](https://www.helpnetsecurity.com/2026/07/08/claude-cowork-phone-mobile-web/)
- Gartner 的"agent washing"和 40% 项目取消预测（见问题三）。

**远程桌面厂商独有的东西（佐证）**
- **常驻和无人值守是硬约束：** Dispatch 要求电脑唤醒且 App 在运行；Perplexity 推荐 Mac mini 7×24；WorkBuddy 要求电脑开机且在线；MCP 远程访问工具的作者也注明"目标电脑必须开机在线"。 — [Claude Blog](https://claude.com/blog/dispatch-and-computer-use)、[MacRumors](https://www.macrumors.com/2026/04/16/perplexity-personal-computer-for-mac/)、[凤凰网](https://tech.ifeng.com/c/8xaeluxtnMt)
- **连接和穿透：** OpenClaw 用户靠 Tailscale、Cloudflare Tunnel 自己拼远程访问，并因此大量暴露在公网。V2EX 用户要求"不依赖梯子、不会被封"，推荐的方案也主打"端到端加密打洞"。 — [Tailscale](https://tailscale.com/blog/openclaw-tailscale-aperture-serve)、[V2EX](https://www.v2ex.com/t/1230296)
- **人工接管和低延迟画面：** Astropad 用自家低延迟协议 LIQUID 做 Workbench，理由是"总有需要看 Mac 屏幕的时候"；WorkBuddy 在智能体 App 里补了"像看直播一样"的远程画面；DeskVNC 主打人接管时智能体释放控制。 — [TechCrunch](https://techcrunch.com/2026/04/08/astropads-workbench-reimagines-remote-desktop-for-ai-agents-not-it-support/)、[凤凰网](https://tech.ifeng.com/c/8xaeluxtnMt)、[Product Hunt](https://www.producthunt.com/p/github-513/deskvnc-remote-for-ai-agents-and-humans)
- **审计和合规：** ToDesk AI 沿用"设备授权加全程录屏追溯"；向日葵把"AI 智能远控审计"放进最贵的行业版；TeamViewer 认为会话内"永远需要 UI"。 — [凤凰网](https://tech.ifeng.com/c/8tbcCD2OFyY)、[向日葵价格页](https://sunlogin.oray.com/price)、[TeamViewer](https://www.teamviewer.com/en/insights/introducing-teamviewer-model-context-protocol-integration/)
- **多设备和跨平台：** ToDesk AI 主打"一个账号控制多台设备"；WorkBuddy 远控是一台电脑对一部手机、只有 macOS；Workbench 只支持 Apple 设备。 — [凤凰网](https://tech.ifeng.com/c/8tbcCD2OFyY)、[凤凰网](https://tech.ifeng.com/c/8xaeluxtnMt)、[TechCrunch](https://techcrunch.com/2026/04/08/astropads-workbench-reimagines-remote-desktop-for-ai-agents-not-it-support/)

### Inferences
- **护城河不在"智能体"，在"让任意模型或智能体可靠、安全地触达任意一台常驻电脑，并且人随时能看能接管"。**大厂各自只服务自家模型和自家生态（Claude 只连 Claude，WorkBuddy 只在腾讯生态，Siri 不进中国），所以"模型中立、跨平台（Windows、macOS、Linux）、可自托管、开源可审计"的连接层有存在空间。这也是 Tailscale 在 OpenClaw 场景里被大量使用的原因。
- **定位建议（推断）：** PeerDesk 应该是"AI 时代的远程桌面和连接层"，而不是"又一个 AI 助手"。具体是：(1) 被控端作为常驻服务加 MCP server 加 CLI，让 Claude、ChatGPT、OpenClaw、Codex 等任意智能体通过 PeerDesk 安全触达电脑；(2) 手机端可以对话，也可以随时切到低延迟画面接管；(3) 托管云卖中继、常驻、设备管理、审计和团队功能；(4) 模型默认让用户自带 Key，不和大厂比模型价格。
- **主要风险排序（推断）：** ① 大厂把远控加智能体免费捆绑（腾讯已经做了，OpenAI 和 Anthropic 只差"真正的远程画面"）；② 安全事故（OpenClaw 先例）会让任何"远程控制加 AI"产品被监管或企业一刀切禁用，开源可审计、默认不暴露公网、每一步可审计是必需的防线；③ 中国超级 App 封杀和监管不确定（豆包、OpenClaw 先例）；④ 企业 agentic 项目大量取消（Gartner），B 端节奏会慢。
- **监管反而可能是机会：** 中国限制在办公设备上装 OpenClaw，暴露了对"可控、可审计、可私有部署"的需求。可自托管的 Apache-2.0 方案在信创和政企场景有讲故事的空间，但要先过安全和合规这一关。

### Gaps
- Microsoft Copilot Actions 和 Agent Workspace 的正式发布时间、是否收费，没有查到。
- Apple Siri 智能体在 Mac 上能否跨 App 操作屏幕，没有查到（只看到 App Intents 路线）。
- 没有找到任何关于"用户为纯连接层（不含模型）付费意愿"的直接调研数据。目前的价格锚点只有 Workbench（$10/月）、RustDesk（$11.88/月）、向日葵（¥158–1288/年）。
- OpenAI Dots 和 Grok Bot 的"自带云电脑"具体形态和价格没有核实（只有 Dealroom 一个来源）。
