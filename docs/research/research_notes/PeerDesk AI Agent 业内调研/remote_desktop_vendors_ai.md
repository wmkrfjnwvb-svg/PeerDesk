# 远程桌面 / 远程访问厂商的 AI 功能现状（截至 2026-11-26）

> 标记：**[已核实]** 表示本轮打开了厂商原文（官网、文档、新闻稿、GitHub）；**[已打开·媒体]** 表示打开的是媒体稿或软文，不是厂商原文；**[未核实]** 表示只看到搜索摘要、二手来源，或凭记忆。2025 年及更早的信息逐条标出日期。

## 问题 1：各厂商做了哪些 AI 功能（已发布 / 已宣布 / 传闻），解决什么问题，何时发布，怎么收费

### Takeaway
欧美老牌厂商（TeamViewer、GoTo、BeyondTrust、NinjaOne、Kaseya、N-able、ConnectWise）的 AI 全部面向 IT 支持和 MSP，形态有四种：会话总结、会话内排障助手、脚本生成、给管理面开放 MCP。执行动作一律要技术员批准。收费上，TeamViewer 用 AI 积分（credits），其他家大多打包进平台、按端点计价，不单列价格。面向个人用户、"用聊天让远端电脑自己干活"的产品，2026 年先在中国落地：ToDesk AI（约 2026-05）、向日葵 OrayClaw 加 MCP（2026-03 至 06）、华为小艺"跨端找文件"（2026-09）。AnyDesk、Parsec、Chrome 远程桌面、Apple 屏幕共享、Quick Assist 都没找到 AI 功能。

### Cited Findings

#### 总览表

| 厂商 | AI 功能 | 状态与日期 | 收费 |
|---|---|---|---|
| TeamViewer | Session Insights（会话总结）、Tia（会话内排障 agent，原名 CoPilot）、Tia Troubleshooting（批准后执行修复）、脚本生成、Tia Reporting、DEX 自然语言写脚本、MCP | Tia 2025-11-18 上线，MCP 2026-09-24 发布 | AI 积分：按月发放，可加购年包，到期清零 |
| GoTo / LogMeIn Resolve | Resolution Agent（诊断后经一键批准执行修复）、自然语言报表、补丁智能、MCP | Resolution Agent 2026-05-19 公布；MCP 2026-10 开放 beta（二手） | 未公布 |
| BeyondTrust | AI 会话总结（云端）、MCP Gateway 加 AI Query Assistant（只读） | RS 26.1 至 26.3，早期访问 | 需要 Pathfinder，价格未公布 |
| NinjaOne | Katana AI（agent 技术员）加 MCP server | 2026-10-13 | 未公布 |
| Kaseya（Datto RMM） | 3 个 MCP server、17 个技能，Agent Center 计划 2027 年 | 2026-11-17，早期访问 | 未公布 |
| N-able | N-zo 助手加 MCP server | 2026-04-13，公开预览 | 未公布 |
| ConnectWise | zofiQ agentic、RMM Agent、平台 GA；ScreenConnect 自身的 AI 仍在征集意见 | 2026-04 / 2026-07-16 | 未公布 |
| Splashtop | AI 优化编解码器、AI 生成的 CVE 和补丁摘要、AI 威胁情报 | 2026-02 / 2026-04-13（5 月可用） | 按端点计价，价格未公布 |
| AnyDesk | 未找到 | — | — |
| RustDesk | 官方没有；社区提案 #16246（控制端 MCP，67 个工具），维护者回复 "Go ahead" | 2026-09-16 提案 | — |
| ToDesk | ToDesk AI：Computer Use、跨设备调度、定时任务、绑定聊天工具 | 约 2026-05 上线 | 积分制，每日登录送积分（媒体报道） |
| 向日葵（贝锐 Oray） | 全功能 MCP 加内置 OrayClaw，可执行命令、键鼠、截屏、传文件 | MCP 约 2026-03 首发；16.5 版 2026-06-16 | MCP 有免费试用和首年优惠，具体价格未公布 |
| 网易 UU 远程 | 没有官方 AI；有远程终端和 CLI，开发者拿来托管 Claude Code | 2026（二手） | 免费 |
| 阿里无影 | AgentBay：给 agent 用的云端执行环境，工具以 MCP 封装 | 2025-07 WAIC 发布 | 未查到 |
| 华为 | 小艺"跨端找文件"：手机说需求，合盖的鸿蒙电脑检索并回传文件 | 2026-09（HarmonyOS 7） | 未提及 |
| Microsoft | Quick Assist 没有 AI；Windows 365 for Agents 给 agent 提供云 PC | 公开预览，仅限美国 | 按需付费或每台每月固定价 |

#### TeamViewer（AI 投入最大，也是唯一公布采用数据的厂商）
- 时间线（IR 页面）：2025-11-18 发布 Tia（"Intelligent Agent for autonomous IT support"），2025-11-19 给投资人做演示；2026-03-23 Tia Reporting；2026-03-31 AI 会话突破 100 万次；2026-04-27 Tia 能从客户已验证的修复中生成自动化；2026-06-03 Assist AR 接入 Windows AI API；2026-07-07 DEX 支持自然语言写脚本；2026-07-23 与 ServiceNow 合作（DEX 和远程连接接入 ServiceNow AI Platform）；2026-06-10 第三年入选 Gartner DEX 管理工具魔力象限领导者。IR 页面称"more than 645k subscribers" **[已核实]** — [TeamViewer IR: AI @ TeamViewer](https://ir.teamviewer.com/publications/teamviewer-ai)
- Tia 的会话内功能：分析被控设备的性能和系统数据，技术员用自然语言描述问题，Tia 给出排障步骤，可以附截图，也能调用过去的 Session Insights 总结。适用于 Business、Premium、Corporate、Tensor 和 TeamViewer ONE；AI 功能默认开启；目标设备为 Windows 或 macOS；双方都要 15.77 以上版本；按 AI 积分计费。页面最后修改于 2026-06-29，配图说明里还留着旧名"TeamViewer Copilot" **[已核实]** — [TeamViewer KB: Troubleshoot with Tia/Copilot](https://www.teamviewer.com/en-au/global/support/knowledge-base/teamviewer-remote/remote-control/troubleshoot-it-issues-with-teamviewer-copilot/)
- Tia Troubleshooting（2026-09）：从调查到解决全程处理，步骤是分析、找根因、提方案、执行已批准的动作、验证结果。原文："actions that make changes require approval and are logged for visibility and auditing." Compare the Cloud 评价人工批准这道关是"by design rather than a limitation"（由 TeamViewer 转引）。同页客户案例：AkzoNobel 把 DEX 接到员工聊天机器人 Xara，IT 工单减少 40% 以上 **[已核实]** — [TeamViewer September 2026 news](https://www.teamviewer.com/en/insights/teamviewer-september-2026-news/)
- AI 积分计费（知识库页面最后修改 2026-08-26）：
  - Session Insights：每份总结 1 积分。
  - Tia "Contextual assistance"：每个会话 1 积分。
  - Tia "Live Diagnostics / End-to-End Resolution"：按对话长短计费，1–5 条消息 1 积分，6–10 条 2 积分，11–15 条 3 积分，16 条以上 4 积分。
  - 用了 Tia Troubleshooting 的会话，其 Session Insights 不再另外扣积分。
  - 脚本生成：新写一个 5 积分，改写一次 1 积分；仅限 ONE Standard/Advanced，或 Remote/Tensor 加 DEX Essentials。
  - 积分每月 1 日按许可证发放，也可加购年度附加包，附加包与主许可证同期结束，未用完的积分在续费日清零。页面举例用的是 250 积分的附加包，没有公布每积分单价。

  **[已核实]** — [TeamViewer KB: AI credit-based payment model](https://www.teamviewer.com/en-us/global/support/knowledge-base/teamviewer-remote/teamviewer-ai/ai-credit-based-payment-model/)
- 各许可证含多少积分：搜索摘要称 TeamViewer ONE 每个端点每月含 1 积分；Core 产品的 Session Insights 附加包为每月 25、50 或 100 积分。2026-07 的俄文版知识库还写着另一套规则（每个 Tia 对话首条消息 1 积分，此后每 50 条再加 1 积分），说明计费规则半年内改过 **[未核实]** — [TeamViewer ONE 产品说明](https://www.teamviewer.com/th/legal/product-descriptions/teamviewer-one/)、[俄文版 KB](https://www.teamviewer.com/ru/global/support/knowledge-base/teamviewer-remote/licenses/licenses-and-features/ai-credit-based-payment-model/)
- 积分扣除政策：功能"启动或执行时"就扣，不管客户最后有没有使用生成的结果 **[未核实]** — [TeamViewer AI credit consumption policy PDF](https://media.teamviewer.com/is/content/teamviewergmbh/teamviewer/central-image-hub/pdf/en/teamviewer-ai-credit-consumption-policy-en.pdf)
- MCP（2026-09-24 发布）：
  - 能做的事：管理租户、查看设备健康、触发远程动作（"restarting devices or connecting to them"）、取会话数据和日志。
  - 客户端：Microsoft Copilot、Claude 和自定义 agent。
  - 实现："sits on top of the REST APIs"；"does not provide access to every TeamViewer capability"。
  - **会话内的能力不走 MCP**："In-session features are handled by Tia"。

  **[已核实]** — [Introducing TeamViewer MCP integration](https://www.teamviewer.com/en/insights/introducing-teamviewer-model-context-protocol-integration/)
- MCP 功能页列出三种 server：Tensor 托管版、可自托管的 WebAPI MCP（GitHub）、DEX MCP（GitHub）。使用指南覆盖 Claude 和 Microsoft 365 Copilot。没有写价格和许可证等级 **[已核实]** — [TeamViewer ONE MCP feature page](https://www.teamviewer.com/en/platform/one/features/mcp/)
- 2026-04 新闻稿：Tia 以上线以来生成的 100 多万份会话总结为基础，把客户已解决的问题变成可复用的自动化 **[未核实]** — [TeamViewer press 2026: AI-driven scripting](https://www.teamviewer.com/en-us/global/company/press/2026/teamviewer-introduces-ai-driven-scripting/)
- 2025 年的旧信息：TeamViewer CoPilot 当时被定位为"嵌在远程支持会话里的上下文 AI 助手"，产品组合叫 TeamViewer Intelligence **[未核实，2025]** — [TeamViewer press 2025: TeamViewer Intelligence](https://www.teamviewer.com/ko/global/company/press/2025/teamviewer-expands-ai-portfolio-with-teamviewer-intelligence-for-it-support-workflows/)

#### GoTo / LogMeIn Resolve 和 Rescue
- 2026-05-19 的博客：
  - **Resolution Agent**：理解用户请求，追问细节，在设备上跑诊断，技术员一键批准后执行修复（"agentic AI to autonomously identify, diagnose, and remediate issues"）。
  - Dynamic Device Data Insights：用自然语言生成报表和图表。
  - Simplified Patching Intelligence：AI 分析补丁失败原因。
  - Rescue：Q2 起加入来自 Nexthink 的 AI 洞察。
  - 没写 GA 状态和价格。

  **[已核实]** — [GoTo blog 2026-05-19](https://www.goto.com/blog/2026-resolve-rescue-ai-features)
- 2026-10：Resolve 的 MCP server 进入开放 beta，同时扩展了与 Acronis EDR/XDR 的集成 **[未核实]** — [Channel Insider](https://www.channelinsider.com/ai/news-goto-logmein-resolve-ai-msp-tools/)、[IT Brief UK](https://itbrief.co.uk/story/goto-adds-ai-tools-integrations-to-resolve-for-msps)
- 2025-02 的旧信息：GoTo 宣称推出"首个"AI 驱动的洞察和问题解决功能；更早还有 GoPilot 助手 **[未核实，2025 或更早]** — [BusinessWire 2025-02-18](https://www.businesswire.com/news/home/20250218229823/en/GoTo-Introduces-First-to-Market-AI-Powered-Insights-and-Issue-Resolution-for-LogMeIn-Resolve-Mitigating-IT-Workloads-and-Eliminating-Manual-Processes-for-SMBs-MSPs)、[UC Today: GoPilot](https://uctoday.com/goto-launches-gopilot-ai-assistant-for-resolve-platform)

#### BeyondTrust（Remote Support 和 PRA）
- MCP Gateway 加 AI Query Assistant：管理员用自然语言查询用户权限、活动会话、凭据借用和审批历史。原文："All API calls are read-only and execute under the requesting user's existing permissions." 可以用内置的 Pathfinder 聊天机器人，也可以用 Claude Desktop 等任何 MCP 客户端。目前是 Pathfinder 早期访问功能，**只在美国区、RS 26.1** 提供 **[已核实]** — [BeyondTrust docs: MCP Gateway](https://docs.beyondtrust.com/rs/reference/mcp-gateway)
- AI 会话总结：RS 26.2 为 beta；26.2.1 起会话结束时自动生成，内容包括案情、解决步骤、系统信息和标签，只在云端版提供；26.3.1 能写回 ServiceNow 工单。PRA 26.2 还额外生成机器可读字段（会话意图、执行的动作、风险和合规信号）；PRA 26.3.1 要求配 Pathfinder **[未核实]** — [BeyondTrust blog RS 26.2](https://www.beyondtrust.com/blog/entry/remote-support-26-2-ai-session-summaries)、[RS 26.2.1 release notes](https://docs.beyondtrust.com/rs/v1/changelog/remote-support-26-2-1-release-notes)、[PRA 26.2 blog](https://www.beyondtrust.com/blog/entry/privileged-remote-access-26-2)

#### NinjaOne
- 2026-10-13 发布 Katana AI 和 NinjaOne MCP：
  - Katana 是内置的 agent 技术员，基于遥测、历史、告警和活动推测根因，建议修复动作和脚本。
  - MCP 让外部 agent 查询数据并"take action directly in the NinjaOne platform"，权限沿用现有的基于角色的访问控制（RBAC）。
  - 新闻稿没写 GA 状态和价格。NinjaOne 自称约 4 万客户。

  **[已核实]** — [NinjaOne press: Katana AI and MCP](https://www.ninjaone.com/press/katana-ai-and-mcp/)
- 2026-01 发布 Patch Intelligence AI，2026-03 发布 AI 漏洞管理；年度经常性收入（ARR）超过 5 亿美元 **[未核实]** — [BusinessWire 2026-01-06](https://www.businesswire.com/news/home/20260106637601/en/)、[BusinessWire 2026-03-16](https://www.businesswire.com/news/home/20260316385242/en/)、[NinjaOne ARR press](https://www.ninjaone.com/press/ninjaone-surpasses-500-million-arr)

#### Kaseya（Datto RMM、Autotask、IT Glue）
- 2026-11-17：
  - 发布 Autotask、Datto RMM、IT Glue 三个第一方 MCP server，状态是"early access"，同时已放到 GitHub。
  - 17 个预置 AI 技能。
  - 自有 agent 的 Agent Center 计划 **2027 年**推出；Kaseya Insights（自然语言查询）"coming soon"。
  - 原则："AI finds and suggests and people make the calls"。
  - 同稿还提到 Web Remote 提速最多 70%，新的基于视频的远程引擎延迟降低 80% 以上。

  **[已核实]** — [Kaseya press 2026-11-17](https://www.kaseya.com/press-release/kaseya-expands-its-agentic-it-management-platform-with-first-party-mcp-servers-pre-built-skills-and-a-unified-cyber-resilience-portal/)
- Datto RMM 的 MCP 页面写明：AI 提出的动作要技术员审核批准后才通过 Datto RMM 执行；另有 2026-04-29 上线的 AI Activity Log Insights **[未核实]** — [Kaseya: Datto RMM MCP server & skills](https://www.kaseya.com/platform/innovations/datto-rmm-mcp-server-and-skills/)、[Activity Log Insights](https://www.kaseya.com/platform/innovations/dattormm-activity-log-insights/)

#### N-able
- 2026-04-13：MCP server 进入公开预览，同时推出产品内助手 N-zo。覆盖 N-central 和 N-sight，本地部署的 N-central 从 2026.2 版起可用。权限沿用现有认证和角色，通过开发者门户的 API key 接入，支持 Claude、ChatGPT、Copilot **[未核实]** — [N-able press](https://www.n-able.com/press/press-releases/n-able-makes-uem-ai-native-with-mcp-server-launch)、[N-able status blog 2026-04-14](https://status.n-able.com/2026/04/14/introducing-the-public-preview-of-n-zo-n-ables-ai-assistant-and-the-n-able-mcp-server/)

#### ConnectWise（ScreenConnect）
- 2026-04 起 zofiQ 从"给建议"走向"执行动作"；2026-07-16 ConnectWise Platform GA，把 ScreenConnect、RMM、PSA 和 ConnectWise AI Agents 整合在一起；RMM Agent 在工单里做端点调查，修复要技术员批准 **[未核实，官方新闻稿抓取返回 403]** — [ConnectWise zofiQ press](https://www.connectwise.com/company/press/releases/connectwise-advances-ai-leadership-with-agentic-ai-zofiq)、[ConnectWise Platform GA press](https://www.connectwise.com/company/press/releases/the-reactive-support-era-is-over-connectwise-platform-is-now-live-empowering-partners-in-journey-to-predictive-intelligence)、[TBBW 2026-07-16](https://tbbwmag.com/2026/07/16/connectwise-ai-platform/)、[Unite.AI](https://www.unite.ai/?p=497553)
- ScreenConnect 本身还没有成形的 AI 功能，官方社区开帖征集"ScreenConnect AI"在会话中和后台该怎么用 **[未核实]** — [ScreenConnect community: Help Shape the Future of ScreenConnect AI](https://screenconnect.product.connectwise.com/communities/67/topics/4855-help-shape-the-future-of-screenconnect-ai)

#### Splashtop
- 2026-04-13 发布统一 IT 平台，5 月可用。AI 相关功能：AI 生成的 CVE 摘要、AI 补丁智能、"AI-optimized codec"（实时调整会话参数）、来自 CrowdStrike、SentinelOne、Bitdefender 的 AI 威胁情报。按端点计价，没有公布价格。自称"more than 30 million users"。没有会话 AI 助手，也没有 MCP **[已核实]** — [Splashtop press](https://www.splashtop.com/press/unified-platform-for-modern-it-announcement)
- 2026 上半年回顾：AI 编解码器"lower bandwidth usage"，AI Patch Summaries **[已核实]** — [Splashtop H1 2026 recap](https://www.splashtop.com/blog/splashtop-product-recap-h1-2026)
- 2026-02-03 发布 AI 编解码器，面向 4K 和直播制作；2026-03-23 与 SysAid 合作，由 SysAid Copilot 在服务台流程里发起 Splashtop 远程支持 **[未核实]** — [IT Brief AU](https://itbrief.com.au/story/splashtop-unveils-ai-powered-codec-for-4k-remote-work)、[Splashtop × SysAid press](https://www.splashtop.com/press/splashtop-announces-partnership-with-sysaid-to-extend-secure-remote-support)

#### AnyDesk
- 没找到 AnyDesk 官方发布的任何 AI 功能。AnyDesk Assist（Ultimate Cloud 版的支持请求功能，入门指南日期 2026-04-02）也没有提到 AI。官网的 `/ai-info` 页面是写给 AI 助手读的产品说明，不是功能公告 **[未核实；changelog 抓取返回 403]** — [AnyDesk Assist](https://support.anydesk.com/knowledge/anydesk-assist)、[anydesk.com/en/ai-info](https://anydesk.com/en/ai-info)
- 第三方有 Botpress 的 AnyDesk 集成，属于外部搭建，不是 AnyDesk 内置功能 **[未核实]** — [Botpress AnyDesk integration](https://botpress.com/integrations/anydesk)

#### RustDesk 和开源生态
- RustDesk 官方没有 AI 或 MCP 功能。社区提案 #16246（2026-09-16，作者 faithleysath）：
  - 一个可选的、**控制端本地**的 MCP，基于 1.4.9，暴露 67 个工具，覆盖会话、截图、输入、显示器、文件、剪贴板、聊天、录制、终端和 TCP 隧道。
  - 默认关闭，只绑定回环地址，用 Streamable HTTP 加 Bearer token；被控端用原版 RustDesk，不改协议。
  - 人工接管时，排队中的 agent 输入作废，按住的键和鼠标按钮全部释放。
  - 目前只在 macOS ARM64 控制端验证过。
  - 维护者（rustdesk 账号）的回复被标为答案："Go ahead, but we still use Rust 1.75 (we still have some paid users stick to win 7)"，并要求先提一个完整的 PR。
  - 2026-11-12 有用户 Jason733i 表态支持，并指向 PR #16470。

  **[已核实]** — [RustDesk discussion #16246](https://github.com/rustdesk/rustdesk/discussions/16246)
- 第三方的 rustdesk-mcp：一个经第三方 RustDesk API server 控制远端；另一个在 macOS 上驱动本地 RustDesk 客户端，有 21 个工具 **[未核实]** — [glama rustdesk-mcp](https://glama.ai/mcp/servers/f1la53yii6)、[glama rustdesk-mcp macOS](https://glama.ai/mcp/servers/bnxnxhaldb)
- QuickDesk（开源远程桌面，**内置 MCP**）：
  - 许可证：自身代码 MIT，远程组件基于 Chromium Remoting（BSD-3）。
  - 技术栈：Qt 6 加 C++，Rust 写的 MCP 桥，Go 写的信令服务，传输用 WebRTC。
  - MCP 工具："40+ MCP tools"（另一处写的是 20+），包括截图、鼠标、键盘、剪贴板、OCR。被控端还有一个"skill host"，提供系统信息、文件操作和 shell。
  - 601 star。README 的对比表把 RustDesk、ToDesk、TeamViewer 都标为没有 MCP（自述）。

  **[已核实]** — [GitHub barry-ran/QuickDesk](https://github.com/barry-ran/QuickDesk)

#### ToDesk（ToDesk AI）
- 快科技 2026-06-09 的报道：
  - 客户端左侧导航有"智能体、新建会话、定时任务、绑定聊天工具"。
  - 实测场景：按月份归类桌面上的 PDF、搜集新闻生成文档、做竞品调研并输出时间线。
  - 删除和修改前要用户确认（"执行前确认"）。
  - 新用户送 2100 积分，每天登录送 1000 积分。
  - 文中"日均连接次数突破千万级"指的是远程控制，不是 ToDesk AI 的用量。

  **[已打开·媒体]** — [快科技](https://news.mydrivers.com/1/1128/1128230.htm)
- 搜狐 2026-06-25"正式上线一个月"，由此推算约 2026-05 上线：
  - 通过"看屏幕"模拟键鼠，不依赖 API。
  - "一个账号，就能调动你名下所有设备"，可以从手机、平板或另一台电脑用语音或文字下达任务。
  - 执行画面实时回传，用户可以随时接管；支付和输密码交给用户手动完成；用户纠正过的做法会被记住。
  - 以独立应用提供，支持 Windows 和 macOS。

  **[已打开·媒体，偏软文]** — [搜狐](https://www.sohu.com/a/1041452981_458778)
- 据说基于 OpenClaw 深度定制，产品内叫 ToClaw；注册送 5000 积分，测试期免费 **[未核实]** — [ai-bot.cn](https://ai-bot.cn/sites/77198.html)、[aiproducthub](https://aiproducthub.cn/sites/todesk-ai.html)
- 自定义接入第三方模型需要升级套餐 **[未核实]** — [知乎评测](https://zhuanlan.zhihu.com/p/2050689660234602427)
- 权限分为读取、编辑和高风险三级 **[未核实]** — [we0.ai 评测](https://we0.ai/articles/todesk-ai-remote-desktop-agent-review)
- 更早的"AI 百宝箱"是抠图、格式转换、写作等通用 AI 工具，和远控本身无关 **[未核实，日期不明]** — [新浪众测](https://zhongce.sina.com.cn/article/view/191738/)

#### 向日葵（贝锐 Oray）
- 16.5 版（2026-06-16）：
  - "全功能 MCP"：设备管理、命令调用、模拟键鼠、识屏执行、远程传文件；配合向日葵硬件还能做电源管理、断网远控和 BIOS 级控制。
  - **已装向日葵的被控设备不用更新就能直接支持。**
  - MCP 支持 Windows、Mac、Linux。
  - 内置 OrayClaw（"开箱即用的 AI 副手"），执行过程全程可视，可以随时接管。

  **[已核实]** — [向日葵 16.5 发布](https://sunlogin.oray.com/news/53723.html)
- 2026-08-07《什么是向日葵 AI 远控》：
  - 组成：OrayClaw 负责理解任务和编排步骤，MCP 负责执行。
  - "让用户可以用自然语言指令驱动远程电脑自主完成任务"，例子有定时渲染视频、下载文件、远程排查告警。
  - 文件可以上传、下载、浏览。
  - "任何支持 MCP 协议的 AI 模型（如 ChatGPT、Claude、本地 Llama 等）"都能调用。
  - 安全：危险操作可以设成需要人工确认，有审计日志和隐私屏。

  **[已核实]** — [向日葵 AI 远控](https://sunlogin.oray.com/news/56887.html)
- MCP 活动页：
  - 工具：`control_connect()`、`control_command()`（"在无桌面环境下执行命令"）、`control_screenshot()`、`desktop_*()`，另有设备检索、远程关机和唤醒。
  - 支持 stdio 和 HTTP 两种模式。
  - 提供"免费试用"和"首年优惠"，没有写价格；安全方面只有一句"企业级安全保障"。

  **[已核实]** — [向日葵 MCP 活动页](https://activity.sunlogin.oray.com/mcp)
- 首发时间大约在 2026-03-06（CSDN 稿标题为"向日葵 MCP 重磅首发"）；要求客户端 16.2.3.28762 以上 **[未核实]** — [CSDN 2026-03-06](https://www.csdn.net/article/2026-03-06/158740180)、[腾讯云 MCP 广场](https://cloud.tencent.com/developer/mcp/server/11808)

#### 网易 UU 远程
- 没找到官方 AI 助手或 MCP。二手评测说它有内置终端和 CLI、断线后保留的会话、端口映射，开发者用手机通过终端处理家里电脑上 Claude Code 的确认提示，不需要串流画面；没有 Linux 被控端 **[未核实]** — [凤凰网科技](https://tech.ifeng.com/c/8xarEWQkrfx)、[CSDN 实测](https://damodev.csdn.net/6ac07d8f05257b0857147bd7.html)、[80aj 2026-08-03（已打开，作者观点为主）](https://www.80aj.com/2026/08/03/uu-remote-ai-agent/)

#### 阿里云无影（AgentBay 和云电脑）
- AgentBay 在上海世界人工智能大会（WAIC，**2025-07**）上发布，是给 AI agent 开发和运行用的云端平台 **[未核实，2025]** — [AIBase](https://www.aibase.com/zh/news/20097)
- 官方 FAQ 称它"不是简单的虚拟机"，而是 Agent 云基础设施：预置工具以 MCP 封装，支持用户状态持久化，用自研 ASP 协议把画面串流到本地 **[未核实]** — [阿里云 AgentBay FAQ](https://help.aliyun.com/zh/agentbay/support/faq)
- 提供浏览器、桌面、代码、手机四类隔离环境，接入方式有 MCP、多语言 SDK、Web SDK；2026 年阿里云官方教程教用户在无影云电脑或 AgentBay 上部署 OpenClaw 和 Hermes Agent **[未核实]** — [阿里云开发者社区](https://developer.aliyun.com/article/1741491)、[部署 OpenClaw 与 Hermes](https://developer.aliyun.com/article/1740669)、[AgentBay 部署 OpenClaw](https://developer.aliyun.com/article/1711846)
- 产品概述页只有目录，没有价格，更新时间 2025-08-15 **[已核实，但没有实质内容]** — [AgentBay 产品概述](https://help.aliyun.com/zh/agentbay/product-overview/)

#### 华为和联想（PC 端 agent）
- 华为 HarmonyOS 7 的"跨端找文件"：原文："外出时通过手机端描述需求，即使电脑合盖待机，只要设备联网且使用同一账号，小艺就可以跨端检索和回传文件" **[已核实，IT之家 2026-09-07]** — [IT之家](https://www.ithome.com/0/999/340.htm)
- 小艺 Work 正式版上线鸿蒙电脑，执行环境可以是云电脑或本地电脑，离开电脑后能用鸿蒙手机远程下发任务、查看进度；小艺独立 App 11-06 上线，版本 11.7.9.300 **[未核实，年份不确定，可能是 2025]** — [IT之家（未打开）](https://www.ithome.com/0/963/591.htm)
- 联想天禧 AI 被称为跨手机、PC、平板的"超级智能体" **[未核实，2025-12]** — [工商时报](https://www.ctee.com.tw/news/20251227700082-430804)。搜索摘要提到 2026-05 发布的天禧 AI 4.0"让电脑自己干活"，但没能对应到具体 URL，归入 Gaps。

#### Microsoft、Google、Apple、Parsec
- Quick Assist：微软 2026-03 的指南只讲了 6 位验证码连接流程，没有 AI **[未核实]** — [Microsoft: Quick Assist](https://www.microsoft.com/en-us/windows/learning-center/how-to-use-quick-assist-to-get-help-on-windows-11)
- Copilot on Windows 的诊断扫描：能找出电脑问题，但修复留给用户，和 Quick Assist 无关 **[未核实，2026-11-18]** — [Windows Latest](https://www.windowslatest.com/2026/11/18/helpful-ai-doesnt-stop-at-answers-microsofts-copilot-scan-for-windows-11-finds-whats-wrong-with-your-pc-then-leaves-the-fixing-to-you/)
- Windows 365 for Agents：
  - 让"computer-using"的 agent 在受管云 PC 里操作没有 API 的应用。
  - 使用者：Copilot Studio 的 computer use、M365 Copilot Researcher（跑在 Linux 云 PC 上）。
  - agent 用时签出一台云 PC，用完签回；用 Intune 和 Entra 管理，用 Agent 365 治理。
  - 状态：公开预览，仅限美国。计价分按需付费和每台每月固定价两种。

  **[未核实]** — [Microsoft Learn](https://learn.microsoft.com/en-us/windows-365/agents/introduction-windows-365-for-agents)、[定价页](https://www.microsoft.com/en/windows-365/agents/compare-plans-pricing)、[Tech Community blog](https://techcommunity.microsoft.com/blog/windows-itpro-blog/windows-365-for-agents-unlocks-secured-scalable-ai-automation/4468107)
- Chrome 远程桌面、Parsec、Apple 屏幕共享：没找到任何 AI 功能。Google 2026-01 的"auto browse"属于 Gemini in Chrome 浏览器，不是 Chrome 远程桌面 **[未核实]** — [TechCrunch 2026-01-28](https://www.techcrunch.com/2026/01/28/chrome-takes-on-ai-browsers-with-tighter-gemini-integration-agentic-features-for-autonomous-tasks/)

### Inferences
- **欧美厂商的 AI 全部服务于"技术员修别人的电脑"**，价值主张是少写工单、更快解决问题、把一次修复变成以后的自动化。没有一家面向个人用户做"我的电脑替我干活"。TeamViewer 明确说会话内的能力只给 Tia、不对 MCP 开放，即不让外部 agent 直接操作远端桌面。
- **"人工批准加审计日志"已经是行业默认**（TeamViewer、GoTo、Kaseya、NinjaOne、ToDesk、向日葵都这么做）。PeerDesk 不做这一层会显得不专业。
- **收费方式有两种**：TeamViewer 用积分计量，按对话长短收费，当月发放、到期清零；ToDesk 和向日葵用免费积分或试用引流，再靠会员收费。RMM 厂商把 AI 打包进平台、按端点卖。到目前为止没有一家公开 AI 带来的收入。
- **中国厂商离 PeerDesk 的设想最近**：向日葵已经把远控能力做成 MCP（能在无桌面环境下执行命令、传文件），被控端不用升级；ToDesk 已经能一个账号调度多台设备，并且绑定聊天工具。两者都是闭源加账号绑定，模型和隐私不透明。PeerDesk 可以在三点上做出差异：开源、可自托管、本地优先。
- RustDesk 维护者对控制端 MCP 持开放态度（"Go ahead"），所以 RustDesk 生态可能在 2027 年前有官方或半官方的 MCP。PeerDesk 的窗口期有限。

### Gaps
- ToDesk 官网（todesk.com）上的 AI 页面、正式上线日期、会员价格、模型来源都没能打开原文核对，目前只有媒体稿和软文。
- 向日葵 MCP 和 OrayClaw 的价格、要哪一级会员，官方页面没写。
- AnyDesk 的 changelog 和 ConnectWise 的新闻稿抓取都返回 403，没能核对原文。
- NinjaOne、Kaseya、N-able、GoTo、BeyondTrust 的 AI 功能都没有公开价格和等级。
- 网易 UU 远程有没有官方 AI 路线，没找到任何官方信息。
- 华为小艺 Work 的远程下发任务（年份和来源都不确定）和联想天禧 AI 4.0，没核实原文。
- 阿里 AgentBay 的计费页和正式商用日期没查到。
- Parsec、Chrome 远程桌面、Apple 屏幕共享没有 AI，这个结论只是"没搜到"，没逐一翻各家的发布说明确认。

## 问题 2：有没有厂商已经能让用户和远端电脑的 agent 对话来完成任务（找文件、发文件、跑任务）而不用打开远程画面？有没有厂商通过远程连接开放 MCP server 或 agent API？

### Takeaway
有，但都在中国，而且都是 2026 年才出现：华为小艺"跨端找文件"（手机说需求，合盖的电脑检索并回传文件，不用打开画面）、向日葵 OrayClaw 加 MCP（自然语言驱动远端电脑完成任务，`control_command` 在无桌面环境下执行命令）、ToDesk AI（一个账号调度多台设备，手机下达任务，执行画面回传，可以接管）。欧美厂商的 MCP（TeamViewer、NinjaOne、Kaseya、N-able、GoTo、BeyondTrust）都在**管理面**：查数据、发起会话、在批准后执行脚本，不提供"对着某台电脑说话"的能力。开源方面，QuickDesk 内置 MCP，RustDesk 有社区提案。

### Cited Findings
- 华为：手机描述需求，合盖待机的电脑只要联网、登录同一账号，小艺就能跨端检索并回传文件 **[已核实]** — [IT之家 2026-09-07](https://www.ithome.com/0/999/340.htm)
- 华为小艺 Work：离开电脑后用手机远程下发任务、查看进度，执行环境可选云电脑或本地电脑 **[未核实]** — [IT之家（未打开）](https://www.ithome.com/0/963/591.htm)
- 向日葵："用自然语言指令驱动远程电脑自主完成任务"，可以上传、下载、浏览文件，能把结果发回用户，模型不限 **[已核实]** — [向日葵 AI 远控](https://sunlogin.oray.com/news/56887.html)；`control_command()` 用于"在无桌面环境下执行命令"，有 stdio 和 HTTP 两种模式 **[已核实]** — [向日葵 MCP 页](https://activity.sunlogin.oray.com/mcp)；被控端不用更新即可支持 **[已核实]** — [16.5 发布](https://sunlogin.oray.com/news/53723.html)
- ToDesk AI："一个账号，就能调动你名下所有设备"，可以用手机下达任务；执行过程"实时回传"，可以接管（原文没说能否不看画面）**[已打开·媒体]** — [搜狐](https://www.sohu.com/a/1041452981_458778)；导航栏有"绑定聊天工具"和"定时任务" **[已打开·媒体]** — [快科技](https://news.mydrivers.com/1/1128/1128230.htm)
- TeamViewer MCP：可以重启或连接设备、取会话日志，但"In-session features are handled by Tia"，不是所有能力都开放 **[已核实]** — [TeamViewer MCP](https://www.teamviewer.com/en/insights/introducing-teamviewer-model-context-protocol-integration/)
- NinjaOne MCP：外部 agent 可以"take action directly in the NinjaOne platform"，受 RBAC 约束 **[已核实]** — [NinjaOne](https://www.ninjaone.com/press/katana-ai-and-mcp/)
- Kaseya：Autotask、Datto RMM、IT Glue 的 MCP 处于早期访问，已在 GitHub 上 **[已核实]** — [Kaseya](https://www.kaseya.com/press-release/kaseya-expands-its-agentic-it-management-platform-with-first-party-mcp-servers-pre-built-skills-and-a-unified-cyber-resilience-portal/)；Datto RMM 上 AI 提出的动作要技术员批准后才执行 **[未核实]** — [Datto RMM MCP](https://www.kaseya.com/platform/innovations/datto-rmm-mcp-server-and-skills/)
- N-able MCP 公开预览，"query live data and take supported actions" **[未核实]** — [N-able](https://www.n-able.com/press/press-releases/n-able-makes-uem-ai-native-with-mcp-server-launch)
- GoTo Resolve MCP 开放 beta（2026-10）**[未核实]** — [Channel Insider](https://www.channelinsider.com/ai/news-goto-logmein-resolve-ai-msp-tools/)
- BeyondTrust MCP Gateway 只读，在请求用户自己的权限范围内运行，只在美国区提供 **[已核实]** — [BeyondTrust docs](https://docs.beyondtrust.com/rs/reference/mcp-gateway)
- 其他 RMM 和工具的 MCP：Syncro MCP server 已上线；社区的 TacticalRMM MCP 在用户明确开启时可以在受管机器上执行命令；Devolutions RDM 论坛有 MCP 接入帖 **[未核实]** — [Syncro community](https://community.syncromsp.com/t/now-available-syncro-mcp-server/19638)、[trmm-mcp (PyPI)](https://pypi.org/project/trmm-mcp)、[Devolutions forum](https://forum.devolutions.net/topics/51279/)
- 开源：QuickDesk 内置 MCP（截图、键鼠、剪贴板、OCR），被控端的 skill host 提供文件和 shell **[已核实]** — [QuickDesk](https://github.com/barry-ran/QuickDesk)；RustDesk 社区提案 #16246 做控制端本地 MCP，67 个工具，含文件和终端，维护者同意提 PR **[已核实]** — [RustDesk #16246](https://github.com/rustdesk/rustdesk/discussions/16246)
- 给 agent 用的云端电脑：Windows 365 for Agents（公开预览）**[未核实]** — [Microsoft Learn](https://learn.microsoft.com/en-us/windows-365/agents/introduction-windows-365-for-agents)；阿里无影 AgentBay（工具以 MCP 封装）**[未核实]** — [AgentBay FAQ](https://help.aliyun.com/zh/agentbay/support/faq)
- 不走画面的变通做法：网易 UU 远程的终端和 CLI 被开发者用来在手机上处理家里电脑的 Claude Code 提示 **[未核实]** — [CSDN 实测](https://damodev.csdn.net/6ac07d8f05257b0857147bd7.html)

### Inferences
- "对远端电脑说话、不开画面"在中国已经有三个量级不同的玩家：华为（系统级，限鸿蒙）、向日葵（远控厂商，开放 MCP）、ToDesk（远控厂商，自带 agent）。欧美市场上还没有远控厂商做这件事，最接近的是 Windows 365 for Agents（云 PC，不是用户自己的电脑）。
- 技术上有两种路线：一是**系统或命令层**（华为找文件、向日葵 `control_command`、UU 终端），快、省流量，但能做的事有限；二是**GUI 层 Computer Use**（ToDesk、OrayClaw、QuickDesk），什么软件都能操作，但慢、贵、容易出错。PeerDesk 可以两条都做：先走命令和文件 API，必要时退到 GUI。
- 欧美厂商刻意不把会话内能力开放给 MCP（TeamViewer 明说了），BeyondTrust 只开放只读。这说明在企业市场，"外部 agent 直接操作终端"被看作高风险。PeerDesk 如果面向个人用户，可以默认只开本机和本人账号，企业场景另做策略。

### Gaps
- ToDesk AI 能不能完全不打开画面（纯聊天）执行、"绑定聊天工具"支持微信、飞书还是钉钉，都没找到原文。
- 向日葵 OrayClaw 的手机端交互形态（App 内聊天还是 IM 机器人），官方页面没写。
- 有没有厂商开放**被控端本地**的 agent API（而不是云端管理 API），只在向日葵和 QuickDesk 身上看到；其他家没查到。

## 问题 3：用户和分析师怎么评价（评论、论坛、媒体、公布的采用数据）

### Takeaway
公布了采用数据的只有 TeamViewer：用 AI 的客户 12 个月从 6,921 涨到 62,953，Session Insights 累计生成约 400 万份总结，但 Tia 上线 10 个月只有 32,068 次交互。会话总结很受欢迎，agent 执行用得还少。TeamViewer 同期营收下滑、下调全年指引，AI 还没带动收入。媒体和分析师普遍认可"有人工批准的 agent"。MSP 一侧的调查显示：多数在用 AI 自动化，但只有 13% 从 AI 服务里赚到了像样的钱。安全界持续警告 RMM 加 MCP 的攻击面。中文论坛对 ToDesk 和向日葵的抱怨主要是卡顿、断线和收费，对它们 AI 功能的真实讨论还很少。r/msp 的帖子没找到。

### Cited Findings
- TeamViewer AI 客户从 6,921（2025-09）增长到 62,953（2026-09），9.1 倍；Session Insights 累计 4,003,701 份，一年前是 22,381 份；Tia 自 2025-11 上线以来共 32,068 次交互。SVP Schrötel："With Tia, we are now moving from AI that advises to AI that resolves issues." **[已核实]** — [TeamViewer AI adoption grows ninefold](https://www.teamviewer.com/cn/insights/teamviewer-ai-adoption-grows-ninefold-in-twelve-months/)
- TeamViewer 2026-03-31 宣布 AI 会话超过 100 万次，其中 3 月一个月新增 30 多万次 **[未核实]** — [TeamViewer press](https://www.teamviewer.com/hu/global/company/press/2026/teamviewer-surpasses-one-million-ai-sessions-major-adoption-milestone/)
- TeamViewer 2026 年第三季度：营收 1.845 亿欧元，按固定汇率同比 −2.2%；ARR 7.369 亿欧元（−0.6%）；全年营收增长指引下调到 −2% 至 −1%；企业业务在增长，中小企业业务在下滑 **[未核实，二手汇总]** — [Quartr](https://quartr.com/events/teamviewer-ag-tmv-q3-2026_3s1TBnKs)、[itreseller.ch 2026-11-04](https://www.itreseller.ch/Artikel/106525/Teamviewer_senkt_Jahresprognose_nach_Umsatzrueckgang.html)
- 媒体对 Tia 的评价：Compare the Cloud 称人工批准"by design rather than a limitation"；IT Brief 强调授权变更的责任仍在 IT 人员身上（由 TeamViewer 汇编转引）**[已核实，转引]** — [TeamViewer September 2026 news](https://www.teamviewer.com/en/insights/teamviewer-september-2026-news/)
- 分析师：TeamViewer 2026-06-10 第三年入选 Gartner DEX 管理工具魔力象限领导者 **[已核实]** — [TeamViewer IR](https://ir.teamviewer.com/publications/teamviewer-ai)；IDC 的 Phil Hochmuth 为 NinjaOne 背书："Every IT team knows it must automate and accelerate its problem resolution capabilities." **[已核实]** — [NinjaOne press](https://www.ninjaone.com/press/katana-ai-and-mcp/)
- MSP 用户的声音：Range MSP 说"Being able to set what the Katana agent can access and having the ability to approve its actions gives us confidence."（厂商新闻稿里的客户引语）**[已核实]** — [NinjaOne press](https://www.ninjaone.com/press/katana-ai-and-mcp/)
- Kaseya 引用的 MSP 调查：48% 认为 AI 和自动化是客户 2026 年最大的 IT 需求；**只有 13%** 从 AI 和自动化服务中拿到了像样的收入；53% 已经用 AI 自动处理工单、补丁和监控；一半以上只自动化了约四分之一的流程 **[已核实]** — [Kaseya press 2026-11-17](https://www.kaseya.com/press-release/kaseya-expands-its-agentic-it-management-platform-with-first-party-mcp-servers-pre-built-skills-and-a-unified-cyber-resilience-portal/)
- GoTo 的调查：97% 的 IT 负责人想要更多 AI 工具来做性能洞察和排障（厂商自己的调查）**[已核实]** — [GoTo blog](https://www.goto.com/blog/2026-resolve-rescue-ai-features)
- 安全界的担忧：攻破 MSP 的 RMM 就能触达它所有下游客户，而且 RMM 滥用很难和正常使用区分开 **[未核实]** — [Huntress](https://www.huntress.com/blog/remote-monitoring-management-tools-gateway-for-attacks-on-msp-pt-1?setlocale=en)；MCP 的风险包括 agent 自主执行命令、凭据泄露，建议最小权限加持续监控 **[未核实]** — [Red Hat](https://www.redhat.com/en/blog/mcp-security-current-situation)、[CSA: MCP can be RCE](https://cloudsecurityalliance.org/articles/mcp-can-be-rce-for-you-and-me)；Syncro 社区有用户担心 MCP 的 `-Force` 参数会绕过维护窗口 **[未核实]** — [Syncro community](https://community.syncromsp.com/t/now-available-syncro-mcp-server/19638)
- 中文论坛：V2EX 上有帖子抱怨 ToDesk 连几分钟就断开、提示开会员（约 2025-08），也有人说向日葵卡顿、Mac 被控端有 bug（约 2026-07）；有用户转向自建 RustDesk、网易 UU 远程或 Tailscale 加 RDP。这些帖子都不是讨论 AI 功能的 **[未核实，帖号与日期未逐一对应]** — [V2EX t/1150079](https://v2ex.com/t/1150079)、[V2EX t/1228666](https://v2ex.com/t/1228666)
- ToDesk AI 的评测：一篇个人博客给它打 8.6 分，认为它在执行闭环和跨端协同上最强，对比的是腾讯 QClaw（本地隐私优先）和 Kimi（执行偏弱）**[未核实，有利益相关嫌疑]** — [CSDN](https://blog.csdn.net/2302_76305195/article/details/161871479)；另一篇 CSDN 文章称免费版"功能不阉割"，同样是个人观点 **[未核实]** — [CSDN](https://adg.csdn.net/6a3119fe10ee7a33f27de721.html)
- 开源社区：RustDesk #16246 下有用户为车队式设备管理的需求表态支持 **[已核实]** — [RustDesk #16246](https://github.com/rustdesk/rustdesk/discussions/16246)；QuickDesk 有 601 star **[已核实]** — [QuickDesk](https://github.com/barry-ran/QuickDesk)

### Inferences
- TeamViewer 的数据说明，"会话结束自动出总结"这种零门槛、不改变工作习惯的功能最容易普及（约 400 万份）；要人主动去对话或批准的 agent（3.2 万次）普及得慢得多。PeerDesk 的 AI 第一步，最好也是"不用学就能得到好处"的东西。
- AI 没有挽回 TeamViewer 中小企业业务的下滑，说明 AI 目前是留住客户和增加附加收入的工具，还不是增长引擎。另一面是：价格敏感的个人和中小企业用户正在流向免费或开源替代品（V2EX 上的情况与此一致）。
- 中文用户的抱怨集中在"断线、卡顿、逼着充会员"，传输体验和收费方式本身仍是主要的不满来源。PeerDesk 把传输做好仍然是根本，AI 只是加分项。

### Gaps
- 没找到 r/sysadmin 或 r/msp 上关于 Tia、Katana、Datto MCP 的真实讨论帖（搜索只返回厂商页面）。
- ToDesk AI 和向日葵 MCP 在 V2EX、知乎上的真实用户评价没找到，现有的都是软文和教程。
- 除 TeamViewer 外，没有任何厂商公布 AI 采用数或 AI 收入。
- G2、Capterra 上关于 AI 功能的评分没查。

## 问题 4：远程桌面市场规模和增速（标明来源和年份）

### Takeaway
各家分析机构给出的 2025 年全球远程桌面软件市场规模集中在 38 亿到 41 亿美元，年复合增长率在 14% 到 20% 之间。这些数字都来自付费报告的摘要，方法不透明，只能当数量级看。可以拿头部公司的收入交叉验证：TeamViewer 的 ARR 约 7.37 亿欧元，NinjaOne（RMM，含远程）的 ARR 超过 5 亿美元。

### Cited Findings
- Market Glass（经 Research and Markets 和 GII 转售）：2024 年 43 亿美元，2030 年 94 亿美元，年复合增长率 13.9% **[未核实]** — [GII](https://www.giiresearch.com/report/go1739230-remote-desktop-software.html)
- Fortune Business Insights：2025 年 39.2 亿美元，2032 年 119.8 亿美元，年复合增长率 17.3% **[未核实]** — [Fortune Business Insights](https://www.fortunebusinessinsights.com/press-release/global-remote-desktop-software-market-10328)
- TechSci Research：2025 年 37.6 亿美元，2031 年 99.6 亿美元，年复合增长率 17.63% **[未核实]** — [TechSci](https://www.techsciresearch.com/report/remote-desktop-software-market/4548.html)
- Research and Markets：2025 年 41 亿美元，2034 年 140 亿美元，年复合增长率 14.7% **[未核实]** — [Research and Markets](https://www.researchandmarkets.com/reports/6185692/remote-desktop-software-market-outlook-market)
- SNS Insider：2025 年 39.6 亿美元，2035 年 231.8 亿美元，2026–2035 年年复合增长率 19.6%（最激进的一家）**[未核实]** — [SNS Insider](https://www.snsinsider.com/reports/remote-desktop-software-market-10980)
- Prof Research：只给了区间，2025 年 20 亿到 50 亿美元，年复合增长率 10% 到 20%，参考价值低 **[未核实]** — [MarketResearch.com](https://www.marketresearch.com/Prof-Research-v4036/Remote-Desktop-Software-Global-Insights-42775947)
- 公司层面的参照：
  - TeamViewer：2026 年第三季度 ARR 7.369 亿欧元 **[未核实]** — [Quartr](https://quartr.com/events/teamviewer-ag-tmv-q3-2026_3s1TBnKs)；订阅用户超过 64.5 万 **[已核实]** — [TeamViewer IR](https://ir.teamviewer.com/publications/teamviewer-ai)
  - NinjaOne：ARR 超过 5 亿美元 **[未核实]** — [NinjaOne](https://www.ninjaone.com/press/ninjaone-surpasses-500-million-arr)；约 4 万客户 **[已核实]** — [NinjaOne press](https://www.ninjaone.com/press/katana-ai-and-mcp/)
  - Splashtop：自称超过 3000 万用户 **[已核实]** — [Splashtop press](https://www.splashtop.com/press/unified-platform-for-modern-it-announcement)
  - ToDesk：远程控制日均连接次数"突破千万级"（媒体转述）**[已打开·媒体]** — [快科技](https://news.mydrivers.com/1/1128/1128230.htm)

### Inferences
- 如果按 40 亿美元算，TeamViewer 一家（约 8 亿美元）就占约 20%，与"头部集中、长尾分散"的格局一致。分析机构报告的口径（是否包含 RMM、VDI 和远程支持）各不相同，彼此不能直接比较。
- 远程桌面本身只是一个中等规模、两位数增长的市场。AI 的意义更多在于向 RMM、DEX、ITSM 这些更大的市场延伸（TeamViewer、Splashtop 都在往"统一 IT 平台"走），或者开出"个人电脑 agent"这个新市场（ToDesk、向日葵、华为）。

### Gaps
- 中国远程控制市场的规模和各家份额（ToDesk、向日葵、UU）没找到可信数据。
- "AI 远程控制"或"电脑 agent"作为细分市场，没有分析机构的数字。
- 所有市场报告都是付费内容，只看到摘要，方法和口径都无法核实。
