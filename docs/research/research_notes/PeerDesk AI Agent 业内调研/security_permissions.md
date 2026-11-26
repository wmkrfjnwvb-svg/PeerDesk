# 操作真实电脑的 AI 代理（含 MCP 服务器）：安全、权限与防护（截至 2026-11）

> 调研日期：2026-11-26。用于 PeerDesk 的设计：每台被控电脑变成一个 AI 代理，用户从手机或电脑的聊天界面指挥它；PeerDesk 同时提供 MCP 服务器和命令行。
>
> 可信度标记：
> - 【已核】本次调研打开了原始页面（厂商文档、规范原文、法规原文），亲眼看到了这条内容。
> - 【二手】来自媒体报道、二手汇编或搜索摘要，没有对照原始文件核实。
> - 【未核】凭已有资料写的，附了链接，但本次没有打开核对。
>
> 不同厂商的"攻击成功率"测法不同（攻击者强弱、尝试次数、按次还是按场景算、测的是哪个界面），**数字之间不能直接比较**。下文每个数字都附了测法。

---

## 一、针对电脑操作代理和浏览器代理的提示注入：有记录的攻击、事故、公开的成功率和厂商防护

### Takeaway
从 2024 年 10 月 Claude Computer Use 被一个网页诱导下载并运行木马，到 2026 年 Comet 被一封日历邀请零点击窃取本地文件和 1Password 凭据，"屏幕上或网页里的文字劫持代理"已经反复在真实产品上演示成功。厂商的分层防护（模型训练、分类器、确认、站点限制）已经把成功率从 20%–60% 压到 0.5%–3%，但所有厂商都明说问题没有解决。所以 PeerDesk 不能指望模型本身挡住注入，必须在产品架构上限制损失：隔离、分级权限、确认。

### Cited Findings

**真实攻击和事故（按时间）**
- 2024-10，ZombAIs：研究者 Johann Rehberger 做了一个网页，上面只写着"下载并运行 Support Tool"。Claude Computer Use（测试版）照做，还自己执行 `chmod +x`，被控机器随后连上了攻击者的 Sliver 命令与控制服务器（C2）。Anthropic 的回应是：文档本来就警告过这类风险，建议把 Claude 和敏感数据、敏感操作隔开。【二手】 — [Embrace The Red 原文](https://embracethered.com/blog/posts/2024/claude-computer-use-c2-the-zombais-are-coming)；[Simon Willison 评述](https://simonwillison.net/2024/Oct/25/zombais/)
- 2025-07-25 报告，2025-08-20 公开：Brave 发现 Perplexity Comet 的间接提示注入。Reddit 评论里藏了指令，用户点"总结页面"后，代理读取了用户的邮箱和一次性验证码，再发回 Reddit。Brave 认为根因是 Comet 把用户请求和不可信网页文字混在一起交给模型。8 月 13 日测试时看似已修复，后续测试发现没有修干净，Brave 再次报告。【二手（Brave 原文的摘要）】 — [Brave 博客](https://brave.com/blog/comet-prompt-injection/)；[The Register](https://www.theregister.com/2025/08/20/perplexity_comet_browser_prompt_injection/)
- 2025-10：Brave 第二篇，"看不见的注入"：截图里用人眼几乎看不到的淡色文字藏指令，代理截图后读到并执行。Fellou 浏览器有类似问题；Opera Neon 曾要求 Brave 推迟披露。【二手】 — [Brave 博客](https://brave.com/blog/unseeable-prompt-injections/)；[Simon Willison](https://simonwillison.net/2025/oct/21/unseeable-prompt-injections/)
  - 和 PeerDesk 直接相关：PeerDesk 的代理读的就是屏幕截图。
- 2025（年份据报道）：Microsoft 365 Copilot 的 EchoLeak（CVE-2025-32711）。攻击者发一封邮件，Copilot 在用户不做任何操作的情况下把数据外泄。【二手，本次未核原始公告】 — [CSA 研究笔记（2026-04）](https://labs.cloudsecurityalliance.org/research/csa-research-note-computer-use-agent-safety-blindspots-20260/)
- 2026-03（OECD 事故库收录日期）：Zenity Labs 发现 Comet 的零点击注入。恶意日历邀请就能劫持代理，外泄本地文件和 1Password 凭据。已出补丁，但据称默认设置下仍有部分暴露。【二手】 — [OECD AI 事故库](https://oecd.ai/en/incidents/2026-03-03-3fd7)
- 2025-05-26：Invariant Labs 演示 GitHub MCP 的"有毒代理流程"（详见第二部分）。公开 issue 里的指令让代理把私有仓库内容写进公开 PR。演示用的是 Claude 4 Opus，但问题与具体模型无关。【二手】 — [Invariant Labs](https://invariantlabs.ai/blog/mcp-github-vulnerability)
- 学术测量：
  - CSA 2026-04 的研究笔记称，"忽略弹窗"这类写在系统提示里的防护无效，建议在架构上把"规划"和"执行"分成两个角色。
  - 同一笔记引用的对抗基准（RedTeamCUA，ICLR 2026）测得：Anthropic 当时最新的电脑操作代理在对抗测试里约 50% 被攻破。
  - 【二手】 — [CSA 研究笔记](https://labs.cloudsecurityalliance.org/research/csa-research-note-computer-use-agent-safety-blindspots-20260/)；[RedTeamCUA 论文](https://arxiv.org/abs/2506.02456v2)
- 美国 NIST 下属的 CAISI 联合 Gray Swan、英国 AISI 和多家前沿实验室办了一次红队竞赛：400 多名参与者、超过 25 万次攻击，**每一个**被测前沿模型都至少被攻破一次。【二手（NIST 博客的摘要）】 — [NIST CAISI 博客](https://www.nist.gov/blogs/caisi-research-blog/insights-ai-agent-security-large-scale-red-teaming-competition)

**厂商公开的数字**

*OpenAI Operator 系统卡（2025-01-23）【已核】* — [Operator System Card](https://openai.com/index/operator-system-card)
- 提示注入易感率（31 个场景）：不加防护 62%，只改提示词 47%，最终版本 23%。
- 提示注入监控器：召回率 99%、精确率 90%。测试集是 77 次红队攻击加 13,704 个正常画面，误报 46 个。红队发现新攻击后，一天内召回率从 79% 提到 99%。
- 需要用户确认时，模型主动请求确认的比例（召回率）92%（607 个任务，20 类风险操作）。
- 遇到银行转账、高风险决策等任务主动拒绝的比例 94%。银行交易一律拒绝。
- 不加防护时，100 个提示里模型犯错 13 次；加了确认机制后，这类风险降了约 90%。
- 监视模式（Watch mode）：在敏感网站上，用户离开页面或不活动时自动暂停。没有给出数字。

*ChatGPT agent 和 ChatGPT Atlas*
- ChatGPT agent 在完成购买这类敏感步骤前会暂停等用户确认。在敏感网站上启用"监视模式"：提醒用户这个网站敏感，并要求用户的标签页保持在前台看着代理操作。【二手（OpenAI 页面的摘要）】 — [OpenAI: prompt injections](https://openai.com/index/prompt-injections/)
- 2025-12-22，OpenAI 加固 Atlas：用强化学习训练了一个自动攻击者，专门找能让浏览器代理执行多步恶意操作的注入；找到的新攻击立即成为防御训练目标。原话是 prompt injection "remains an open challenge for agent security, and one we expect to continue working on for years to come"（仍是代理安全的未解难题，预计还要做很多年）。【二手（OpenAI 页面的摘要）】 — [Hardening Atlas](https://openai.com/index/hardening-atlas-against-prompt-injection)
- 2026-08，GPT-5.6：报道称 OpenAI 自己的间接注入测试里平均攻击成功率是 Sol 3.77%、Terra 3.32%、Luna 2.94%；另一报道称按 Gray Swan 式"每个场景尝试 15 次"测，Sol 是 20.0%。两组数字口径不同，互相冲突。【二手，未核系统卡】 — [TechRepublic](https://www.techrepublic.com/de/article/news-gpt-5-6-prompt-injection/)

*Anthropic Claude in Chrome 和 Claude 模型*
- 2025-08-25，研究预览（1,000 名 Max 用户）【已核】 — [Piloting Claude in Chrome](https://claude.com/blog/claude-for-chrome)
  - 测试集：123 个用例、29 种攻击场景。
  - 不加防护时攻击成功率 23.6%，在自主模式下加防护后 11.2%。
  - 4 种浏览器特有攻击组成的"挑战集"：35.7% 降到 0%。
  - 时间线：2025-11-24 向全部 Max 用户开放测试版；2025-12-18 扩展到 Pro、Team、Enterprise。
- 2025-11-24，Anthropic 发布提示注入防御研究【已核】 — [Mitigating prompt injection in browser use](https://www.anthropic.com/research/prompt-injection-defenses)
  - 攻击者：内部的自适应 Best-of-N 攻击者，每个环境尝试 100 次，每组数据约 2,200–3,200 次尝试。
  - 攻击成功率（只有模型 / 旧防护 / 新防护）：

    | 模型 | 只有模型 | 旧防护 | 新防护 |
    |---|---|---|---|
    | Sonnet 4 | 13.8% | 10.7% | 2.0% |
    | Sonnet 4.5 | 13.8% | 10.8% | 1.9% |
    | Haiku 4.5 | 12.0% | 11.1% | 2.4% |
    | Opus 4.5 | 8.0% | 7.6% | 1.4% |

  - 防护手段：
    - 强化学习训练：在模拟网页里放注入，模型拒绝就给奖励。
    - 分类器扫描进入上下文的不可信内容，包括隐藏文字、被篡改的图片、伪造的界面元素。
    - 持续的内部红队，加上外部擂台式挑战。
  - 原话："A 1% attack success rate—while a significant improvement—still represents meaningful risk"（1% 的攻击成功率虽是重大进步，仍代表实际风险）；"No browser agent is immune to prompt injection"（没有哪个浏览器代理能免疫提示注入）。
- 2026-05-28，Opus 4.8 系统卡（据 VentureBeat）【二手】 — [VentureBeat](https://venturebeat.com/security/anthropic-browser-agent-hijacked-31-percent-before-safeguards-engaged)
  - 系统卡把注入结果按工具调用、编程、电脑操作、浏览器四个界面分别列出。
  - 浏览器：不加防护时按单次尝试算 31.5%，加全套防护后 0.5%（129 个训练时没见过的网页环境，每个尝试 10 次）。
  - 编程：7.03% 降到 2.09%。
  - **0.5% 只适用于 Claude in Chrome 和 Claude Cowork 的全套防护；API 默认不带这些防护。**
  - 电脑操作界面的具体数字在原表图片里，文章没转录。
- Opus 5：报道称在 Gray Swan 基准上每个场景尝试 15 次的成功率从 Opus 4.8 的 5.5% 降到 2.0%。【二手，未核】 — [cybersecuritynews](https://cybersecuritynews.com/claude-opus-5-reduces-prompt-injection-attack/)
- Anthropic 电脑操作工具文档：工具会用分类器自动扫描返回的截图，发现疑似注入时引导模型先向用户确认"这条指令是不是你发的"。文档同时强调，有了分类器，隔离等预防措施依然重要。当前最新的工具版本是 `computer_toolset_20260801`。【已核】 — [Computer use tool 文档](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)

*Google Chrome 代理功能（2025-12-08）【二手（Google 博客的摘要）】* — [Google 安全博客](https://blog.google/security/architecting-security-for-agentic/)；[TechCrunch](https://techcrunch.com/2025/12/08/google-details-security-measures-for-chromes-agentic-features/)
- User Alignment Critic：一个独立的 Gemini 模型，逐个审查规划器提出的动作是否符合用户目标。它只看动作的元数据，不看网页原文，所以网页里的注入毒不到它。
- Agent Origin Sets：把网站分成"只读"和"可读写"两组，组外网站的内容不给模型看；规划器要新增网站，必须经过门控批准。
- 其他：模型生成的网址只能是已知的公开网址；敏感网站、登录、购买需要用户确认。
- 首版只实现了"可读写"这一组。

*Microsoft*
- Windows 实验性代理功能页面明确警告跨提示注入（XPIA）：界面元素或文档里的恶意内容可以覆盖代理的指令，导致数据外泄或安装恶意软件。【已核】 — [Experimental agentic features](https://support.microsoft.com/en-us/windows/ai/ai-features/experimental-agentic-features)

### Inferences
- PeerDesk 代理的输入是**整个屏幕**：网页、邮件、聊天窗口、文件名、弹窗都会进入模型上下文。这比浏览器代理的攻击面更大，因为没有"同源"这样的天然边界，Google 的 Origin Sets 不能照搬。可以借鉴的思路是按**应用或窗口**分"只读 / 可操作"两组，组外窗口打码后再给模型看。
- 厂商在全套防护下也只能做到约 0.5%–2%（单次）的成功率。代理每天要看成千上万屏内容，按次算的低概率累积起来就不低。所以必须假设"代理迟早会被劫持一次"，再设计权限和确认，让被劫持的那一次造不成不可逆的损失。
- 外部厂商的数字测的是它们自己的产品形态（例如 Chrome 里的 Claude 带全套分类器），通过 API 调用模型拿不到同样的保护（Opus 4.8 系统卡明说 API 默认不带）。PeerDesk 如果通过 API 接入模型，就得自建分类器和确认层，或者选择带防护的托管方案。
- 按项目"用指标说话"的原则，应该把"注入攻击成功率"做成 PeerDesk 端到端测试的一个指标：在仿真桌面上放入带注入的网页、文档、邮件，量代理被劫持的比例，门槛只升不降。可参考 AgentDojo、RedTeamCUA 这类公开基准的测法。

### Gaps
- 没有找到 PeerDesk 这种"远程桌面加代理"形态的公开测量。现有数据几乎都来自浏览器代理。
- Opus 4.8 和 Opus 5 系统卡里电脑操作界面的具体攻击成功率，本次没有拿到原文。
- OpenAI ChatGPT agent 系统卡里监视模式的触发率、确认率，没有找到公开数字。
- Cowork、Operator 在 2026 年有没有被公开攻破的真实事故，没有找到可靠报道。

---

## 二、MCP 特有风险：工具投毒、换钩子（rug pull）、混淆代理、令牌透传、恶意服务器、暴露在公网的服务器、重要漏洞、授权规范现状

### Takeaway
MCP 的主要风险有三条：
- **工具描述是模型能看到、用户看不到的指令通道**，由此产生工具投毒和换钩子。
- **本地 stdio 服务器等于以用户身份执行任意命令**，2026-04 OX Security 披露的 SDK 层命令注入波及大量下游产品。
- **大量服务器无认证直接暴露在公网**：2026-05 Censys 收录了超过 2.1 万个；同类的本地代理项目 OpenClaw 有 1.5–3 万个实例暴露。

规范方面，最新版 MCP（2026-07-28）对 HTTP 传输规定 OAuth 2.1、受保护资源元数据（RFC 9728）、资源指示（RFC 8707）都是必须项，并明确禁止令牌透传；但"授权"整体仍是可选的，stdio 传输不适用这套授权。

### Cited Findings

**工具投毒、换钩子、有毒流程**
- 2025-04-09，Simon Willison 最早公开提醒：MCP 工具描述里可以藏对抗指令。他建议客户端把初始工具描述展示给用户，描述一变就提醒。【二手】 — [Simon Willison](https://simonwillison.net/2025/Apr/9/mcp-prompt-injection/)
- 2025-04，Invariant Labs 的 WhatsApp 演示：
  - 一个恶意 MCP 服务器在工具描述里藏指令，让代理通过**合法的** whatsapp-mcp 把聊天记录发到攻击者号码。代理不需要直接调用恶意服务器，也不需要用户批准恶意工具。
  - 换钩子（rug pull）：用户批准工具之后，服务器再改工具描述。
  - Invariant 建议用 mcp-scan 之类的工具检查。
  - 【二手】 — [Invariant Labs](https://invariantlabs.ai/blog/whatsapp-mcp-exploited)；[Docker 复盘](https://www.docker.com/blog/mcp-horror-stories-whatsapp-data-exfiltration-issue/)
- 2025-05-26，GitHub MCP 的"有毒代理流程"：
  - 公开仓库的一个 issue 里放指令，代理把私有仓库数据写进一个公开 PR。
  - Invariant 认为这不是 GitHub MCP 服务器代码的漏洞，而是代理系统层面的架构问题。
  - 建议每个会话只允许访问一个仓库，并做动态的、看上下文的访问控制。
  - 据报道没有分配 CVE。2026 年 Noma Security 对 GitHub Agentic Workflows 演示了类似手法。
  - 【二手】 — [Invariant Labs](https://invariantlabs.ai/blog/mcp-github-vulnerability)；[The Hacker News 2026-07](https://thehackernews.com/2026/07/public-github-issue-could-trick-github.html)
- 2025-07，Cursor "MCPoison"（CVE-2025-54136，Check Point 发现）：用户先批准一份无害的 MCP 配置，后续提交把它换成恶意配置，Cursor 不再要求重新批准。v1.3 修复。【二手】 — [CSA 研究笔记 2026-05-04](https://labs.cloudsecurityalliance.org/research/csa-research-note-mcp-security-crisis-20260504/)
- 2025-07，Supabase 加 Cursor：一张客服工单里的 SQL 指令让高权限代理读出集成令牌，并贴到公开的工单线程里。【二手】 — 同上 CSA
- 2025-09，postmark-mcp：**第一个确认在野外出现的恶意 MCP 服务器**。作者先发布 15 个干净版本，v1.0.16 开始把所有邮件悄悄密送给攻击者。Koi Security 估计约 300 个组织受影响。【二手】 — 同上 CSA

**重要漏洞**
- CVE-2025-49596，MCP Inspector（官方调试工具）低于 0.14.1 的版本：Inspector 客户端和代理进程之间没有认证，可以远程执行代码。Oligo 发现。CVSS 分数各处不一致：多数报道写 9.4，Tenable 页面显示 9.8。【二手（分数冲突）】 — [NVD](https://nvd.nist.gov/vuln/detail/CVE-2025-49596)；[Tenable](https://www.tenable.com/cve/CVE-2025-49596)
- CVE-2025-6514，mcp-remote 0.0.5–0.1.15：操作系统命令注入（CWE-78），CVSS 9.6。连上一个恶意远程 MCP 服务器，或被本地中间人攻击，就能在客户端机器上执行任意命令。0.1.16 修复。JFrog 发现，2025-07 公开。【二手】 — [JFrog](https://jfrog.com/blog/2025-6514-critical-mcp-remote-rce-vulnerability/)；[NVD](https://nvd.nist.gov/vuln/detail/CVE-2025-6514)
- CVE-2025-68143、68144、68145：Anthropic 官方的 Git MCP 服务器 mcp-server-git 的三个漏洞，Cyata 研究员 Yarden Porat 披露。【二手，未核 NVD】 — [CSA](https://labs.cloudsecurityalliance.org/research/csa-research-note-mcp-security-crisis/)；[dev.to 汇编](https://dev.to/piiiico/mcp-security-vulnerabilities-in-2026-40-cves-and-counting-4pco)
- 2026-04，OX Security 披露 MCP 官方 SDK 的 stdio 设计缺陷：
  - 官方 SDK（Python、TypeScript、Java、Rust）里存在命令注入根因，波及 LiteLLM（CVE-2026-30623，严重）、Agent Zero（CVE-2026-30624）、Windsurf（CVE-2026-30615，零交互即可利用，截至 2026-05 未修复）、Flowise 等。
  - OX 估计约 20 万个受影响实例，涉及的供应链下载量超过 1.5 亿次。
  - 据 CSA，Anthropic 在 OX 首次联系后 9 天更新了 SECURITY.md，没有改 SDK。有二手来源说 Anthropic "不修"，只有单一来源。
  - 【二手】 — [OX Security](https://www.ox.security/blog/mcp-supply-chain-advisory-rce-vulnerabilities-across-the-ai-ecosystem)；[CSA 研究笔记 2026-05-04](https://labs.cloudsecurityalliance.org/research/csa-research-note-mcp-security-crisis-20260504/)；[The Hacker News](https://thehackernews.com/2026/04/anthropic-mcp-design-vulnerability.html)
  - 注意：同一个 CVE 编号在不同来源里对应的产品有冲突，要以 NVD 为准。
- 其他 2026 年的漏洞【二手，未核 NVD】 — [dev.to 汇编](https://dev.to/piiiico/mcp-security-vulnerabilities-in-2026-40-cves-and-counting-4pco)
  - CVE-2026-23744：MCPJam Inspector 1.4.2 及以下版本监听 0.0.0.0，没有认证。
  - CVE-2026-25536：MCP TypeScript SDK 复用同一个 McpServer 实例时，响应会串到别的客户端。
  - CVE-2026-23523：Dive MCP 客户端可以被特制深链接装上恶意 MCP 配置。

**暴露在公网的服务器（扫描数字和日期）**
- 2025-07：一次全网扫描发现 1,862 个公开暴露、无需认证就会返回工具列表的 MCP 服务器，Dark Reading 报道。CSA 没写扫描方，业内一般归于 Knostic（本次未核）。【二手】 — [CSA](https://labs.cloudsecurityalliance.org/research/csa-research-note-mcp-security-crisis-20260504/)
- Trend Micro（2025 年中）：首次探测发现 492 个既无客户端认证也不加密传输的 MCP 服务器；后续扫描发现 1,467 个暴露的服务器，多数用旧的 SSE 传输。【二手】 — [Trend Micro](https://www.trendmicro.com/vinfo/de/security/news/cybercrime-and-digital-threats/mcp-security-network-exposed-servers-are-backdoors-to-your-private-data)；[Trend Micro 更新](https://www.trendmicro.com/vinfo/kr/security/news/vulnerabilities-and-exploits/update-on-exposed-mcp-servers-the-threat-widens-to-the-cloud)
- Censys：截至 2026-04-28，发现 12,520 个公网可访问的 MCP 服务，分布在 8,758 个 IP 上；到 2026-05-06，数据集里已超过 21,000 个。摘要里没有单列无认证的数量。【二手】 — [Censys](https://censys.com/blog/mcp-servers-on-the-internet/)
- SANS ISC 报告：已经有人在专门扫描 MCP 服务器和 AI 助手的凭据。【二手】 — [SANS ISC 日志](https://isc.sans.edu/diary/33150)
- 同类产品 OpenClaw（原名 Clawdbot、Moltbot，一个本地个人代理，可以从聊天软件指挥），2026-01 至 02：
  - Bitsight 在 13 天内观测到超过 3 万个暴露实例。
  - SecurityScorecard 称 15,200 个实例可被远程执行代码。
  - 部分实例泄露配置、环境变量和历史对话。
  - 有一个漏洞是：经过 Nginx 或 Caddy 反向代理后，所有连接看起来都来自 localhost，认证被完全绕过。
  - 技能市场 ClawHub 出现大量恶意"技能"（ClawHavoc 行动）。Koi 统计 2026-01-27 至 02-02 有 386 个以上；Bitdefender 估计约 900 个，约占全部技能包的 20%；也有报道称总数 1,184 个。一项学术审计在 2,556 个技能里只标出 49 个疑似恶意。各家统计口径差异很大。
  - 【二手】 — [The Register 2026-02-03](https://www.theregister.com/2026/02/03/openclaw_security_problems/)；[The Register 2026-02-05](https://www.theregister.com/2026/02/05/openclaw_skills_marketplace_leaky_security/)；[Tom's Hardware](https://www.tomshardware.com/tech-industry/cyber-security/malicious-moltbot-skill-targets-crypto-users-on-clawhub)；[VentureBeat](https://venturebeat.com/security/mcp-shipped-without-authentication-clawdbot-shows-why-thats-a-problem)

**MCP 授权规范的现状（最新版 2026-07-28）【已核】** — [MCP Authorization](https://modelcontextprotocol.io/specification/latest/basic/authorization)
- 授权整体是**可选**的（OPTIONAL）。如果支持：
  - HTTP 传输应当（SHOULD）遵循这份规范。
  - stdio 传输不应当（SHOULD NOT）使用这套授权，而是从环境变量等本地环境获取凭据。
- 授权服务器必须（MUST）实现 OAuth 2.1（草案 13），对机密客户端和公开客户端都要有相应的安全措施。
- MCP 服务器必须实现受保护资源元数据（RFC 9728），客户端必须用它发现授权服务器。
- 客户端注册：推荐用"客户端 ID 元数据文档"（CIMD，SHOULD）；动态客户端注册（DCR，RFC 7591）在 2026-07-28 版**已标为弃用**，只为兼容保留。
- 必须使用 PKCE。客户端必须按 RFC 9207 校验授权响应里的签发方（`iss`），防止混用攻击。
- 客户端必须使用资源指示（RFC 8707）：授权请求和令牌请求都要带 `resource` 参数，填 MCP 服务器的规范 URI，"不管授权服务器支不支持都要发"。
- 服务器必须校验令牌确实是签发给自己的（受众校验）。"MCP servers MUST NOT accept or transit any other tokens"（不得接受或转发任何别的令牌），也就是禁止令牌透传。令牌不得放在 URL 查询串里。
- 权限范围按需提升：服务器用 `WWW-Authenticate` 里的 `scope` 和 403 `insufficient_scope` 告诉客户端缺什么权限，客户端按最小权限逐步申请。

**MCP 安全最佳实践（官方，2026-07-28 版）【已核】** — [Security Best Practices](https://modelcontextprotocol.io/docs/tutorials/security/security_best_practices)
- 混淆代理（confused deputy）：MCP 代理服务器用一个固定的客户端 ID 去连第三方 API 时，攻击者可以利用第三方授权服务器的"已同意"cookie 跳过用户同意，拿到授权码。对策：
  - **必须**按客户端做同意，并且在转去第三方授权之前完成。
  - 同意页必须显示客户端名称、所申请的权限、回调地址（redirect_uri），并防 CSRF、防点击劫持。
  - redirect_uri 必须精确匹配。
  - OAuth 的 `state` 参数只能在用户同意之后写入，且一次性使用。
- 令牌透传：明确禁止。危害包括绕过限流和审计、无法区分是哪个客户端、被盗令牌可以把服务器当外泄代理。
- SSRF：客户端在 OAuth 发现流程中拉取的网址可能被恶意服务器指向内网或云元数据地址。应只用 HTTPS、屏蔽私有和链路本地网段、逐跳校验重定向、走出口代理，并注意 DNS 重绑定。
- 状态句柄劫持：2026-07-28 版协议改为**无状态**，不再有协议级会话。服务器不得把"持有句柄"当认证，句柄要绑定到已认证用户。
- 本地 MCP 服务器被利用：
  - 支持一键安装本地服务器的客户端**必须**在执行前弹出同意框，完整显示要执行的命令（不截断），注明这是在本机执行代码，并要求用户明确批准。
  - **应当**高亮 `sudo`、`rm -rf`、网络操作等危险模式；在最小权限的沙箱里运行；额外权限由用户显式授予。
  - 本地服务器应当用 stdio，或者用 HTTP 时要求令牌，或者改用 Unix 套接字等访问受限的进程间通信。
- 授权网址校验：客户端**必须**拒绝 `javascript:`、`data:`、`file:` 等协议，**不得**用 shell 打开网址（防命令注入，CVE-2025-6514 就是这一类）。
- 权限范围最小化：初始只给低风险的只读权限，高权限按需提升；不要用 `*`、`all` 这样的通配权限。

**行业控制建议**
- CSA（2026-05-04）建议【已核（CSA 原文）】 — [CSA 研究笔记](https://labs.cloudsecurityalliance.org/research/csa-research-note-mcp-security-crisis-20260504/)
  - 盘点所有 MCP 服务器。
  - stdio 服务器放进没有宿主机凭据的容器或沙箱运行。
  - 锁定版本，工具定义一变就告警；评估 ETDI（arXiv:2506.01333），用密码学方式绑定工具定义。
  - 每个服务器独立进程、独立凭据，跨服务器访问要经过显式策略层。
  - 所有 MCP 连接都要求认证。
- Windows 的"受控 MCP 服务器"：MCP 服务器跑在单独的 Windows 会话里，用单独的代理用户账户。【二手（搜索摘要）】 — [Microsoft Learn: MCP containment](https://learn.microsoft.com/en-nz/windows/ai/mcp/servers/mcp-containment)

### Inferences
- PeerDesk 的 MCP 服务器本质上是"一个能控制整台电脑的工具服务器"。它一旦像 OpenClaw 那样暴露在公网、或认证可被绕过，后果就是整机被控。建议：
  - 本地默认只用 stdio，或者用只有本机用户能访问的 Unix 套接字或命名管道。
  - 远程访问只走 PeerDesk 自己的已认证 QUIC 通道。
  - 不监听 0.0.0.0。
  - 不信任"来自 localhost"这个条件，因为反向代理会把所有外部连接变成 localhost（OpenClaw 就栽在这里）。
- 如果 PeerDesk 提供远程 HTTP 形式的 MCP：
  - 按 2026-07-28 规范实现 RFC 9728、RFC 8707 受众绑定、PKCE、`iss` 校验。
  - 客户端注册优先用 CIMD，不开 DCR（DCR 已弃用，而且是混淆代理攻击的前提之一）。
  - 绝不透传令牌。
- PeerDesk 自己的工具描述应当固定在二进制里并签名，不从网络动态拉取，从根上避免换钩子。如果以后开放第三方工具或技能：
  - 计算工具定义的哈希，安装时展示，变化时重新征求同意。
  - 吸取 OpenClaw 和 ClawHub 的教训：技能就是代码，要签名、审核、沙箱，不能把技能当作可信代码。
- 本地 CLI 和 MCP 服务器的凭据：不放在 URL 里，不写进日志；令牌要有受众限制和过期时间。

### Gaps
- CVE-2025-49596 的 CVSS 分数各处不一致（9.4 还是 9.8），以 NVD 当前值为准，本次没打开 NVD 页面核对。
- OX Security 列出的部分 CVE 编号和产品对应关系，在不同来源里有冲突（例如 CVE-2026-33224）。
- 没有找到 2026 年下半年最新一次全网扫描里"无认证 MCP 服务器"的单独计数。
- Anthropic 对 stdio SDK 问题的正式立场，只有二手说法。

---

## 三、远程控制工具被诈骗利用：现状、厂商的应对，以及加入 AI 代理后风险怎么变

### Takeaway
骗子让受害人装 AnyDesk、TeamViewer、Quick Assist 或开屏幕共享，是全球最主流的诈骗手法之一。FBI 统计 2025 年美国"技术支持诈骗"损失约 21 亿美元，中国金融监管部门也把"屏幕共享"列为新型电信诈骗类型。厂商和平台的应对可以归成四类：
- 打断话术：警告、强制等待。
- 绑定发起方：只有自己主动联系的支持人员才放行。
- 来电加敏感应用的联动检测。
- 第二设备确认。

加入 AI 代理以后，"控制电脑"可以变成在聊天软件里发一句话，不再需要看得见的鼠标操作。豆包手机助手被微信、支付宝等风控拦截，说明国内大平台已经把"AI 模拟用户操作自家应用"当作高风险处理。

### Cited Findings

**规模**
- FBI IC3 2025 年报：技术支持诈骗损失约 21 亿美元，投诉 47,794 起，损失额排第三（前两名是投资诈骗约 86 亿美元、商业邮件诈骗约 30 亿美元）。全年网络犯罪总损失 208.7 亿美元，创纪录。老年人在假冒技术或客服支持上损失约 10 亿美元。【二手；本次看到的年报 PDF 是第三方站点托管的】 — [The Register 2026-04-07](https://www.theregister.com/2026/04/07/cybercrime_losses_reach_alltime_high/)；[SecurityWeek](https://www.securityweek.com/fbi-cybercrime-losses-neared-21-billion-in-2025/amp/)；[AARP](https://www.aarp.org/money/scams-fraud/fbi-ftc-report-2025-losses.html)
- FBI 波士顿分局的警告：技术支持诈骗分子诱导受害人安装远程桌面软件，再让受害人登录网银；建议不要把设备或账户的远程访问权给未经核实的人。【二手】 — [FBI](https://www.fbi.gov/contact-us/field-offices/boston/news/press-releases/fbi-warns-public-to-beware-of-tech-support-scammers-targeting-financial-accounts-using-remote-desktop-software)
- "假退款"变体：骗子让受害人装 AnyDesk、TeamViewer 或 UltraViewer，然后在旁边看着受害人登录网银。【二手（安全厂商博客）】 — 同上 FBI 页面及相关报道

**厂商和平台的应对**
- Microsoft Quick Assist（2024-05-15）：Storm-1811 团伙先用"邮件轰炸"塞满受害人邮箱，再冒充技术支持打电话，通过 Quick Assist 进入电脑，最后投放 Black Basta 勒索软件。Microsoft 的应对：
  - 在 Quick Assist 里加入技术支持诈骗的警告。
  - 建议企业不用就屏蔽或卸载 Quick Assist 这类工具。
  - 建议个人只在自己主动联系了 Microsoft 或本单位 IT 的情况下才允许对方连入。
  - 【二手（Microsoft 博客的摘要）】 — [Microsoft 安全博客](https://microsoft.com/en-us/security/blog/2024/05/15/threat-actors-misusing-quick-assist-in-social-engineering-attacks-leading-to-ransomware)；[Cyber Signals 第 9 期](https://microsoft.com/en-us/security/blog/2025/04/16/cyber-signals-issue-9-ai-powered-deception-emerging-fraud-threats-and-countermeasures)
- Android 来电防护（2025-12-03 扩展到美国）：
  - 用户正在和**未存进通讯录的号码**通话、又在屏幕共享时，一打开参与试点的金融应用，系统就弹出警告，带红色"立即挂断"按钮，以及"等待 30 秒才能继续"的倒计时，用来打断骗子的话术。
  - 英国试点已帮助数千名用户挂断了可能造成重大损失的电话。
  - 通话中不允许关闭 Google Play 保护。
  - 【二手（Google 博客的摘要）】 — [Google 博客](https://blog.google/security/android-expands-pilot-in-call-scam-protection-financial-apps/)；[Android Central](https://www.androidcentral.com/apps-software/android-os/google-expands-in-call-spam-protection-to-the-us)
- ToDesk：第三方评测称它有"二次验证保护"，开启后所有连入本机的连接都要主设备同意；另有设备绑定、IP 黑名单等功能。**没有找到 ToDesk 官方的反诈功能说明。**【二手】 — [新浪众测](https://zhongce.sina.com.cn/article/view/192599/)；[腾讯云开发者社区](https://cloud.tencent.com/developer/article/2524872)
- RustDesk 的反诈文章：只要用户被骗装了客户端、说出了连接码，任何远控工具都可能被利用；架构设计只能降低部分风险，核心还是"不要把电脑控制权交给主动联系你的人"。【二手】 — [RustDesk 博客](https://rustdesk.com/zh-cn/blog/avoid-remote-desktop-scams-zh-cn)
- Splashtop 推出了"反诈保护"功能（未展开核对）。【二手】 — [Splashtop](https://www.splashtop.com/tw/blog/what-is-anti-scam-protection)

**中国情况**
- 国家金融监督管理总局 2024-07 的风险提示把"共享屏幕"列为新型电信网络诈骗。国家反诈中心提醒：共享屏幕时，对方能看到验证码和密码输入画面。
- 2025-07，北京警方提醒"抖音会议""银联会议"是诈骗工具，开启共享屏幕后可以远程操控手机转走存款。
- 2025-12，山东反诈中心发公益短信，提醒冒充客服以"误开通会员"为由诱导开屏幕共享的诈骗。
- 【二手】 — [新华网 2025-12-10](https://www.news.cn/legal/20251210/47903beb12a84147ae9252fecbedb957/c.html)；[UBS 中国 风险提示](https://www.ubs.com/cn/sc/ubs-china/consumer-right-protection/risk-reminder/consumer-risk-tips-money-screen-sharing.html)；[IT之家](https://www.ithome.com/0/868/347.htm)

**AI 代理操作其他应用：国内平台的反应（2025-12）**
- 豆包手机助手通过系统级权限 INJECT_EVENTS 模拟用户点击、输入。用户反映操作微信时被强制下线，微信相关人士回应称可能触发了原有的安全风控。
- 淘宝、支付宝、京东等也以安全为由，限制豆包手机助手操作自家应用。
- 豆包的说法：这个权限需要用户主动授权，敏感操作由用户接管；还发布了《隐私安全白皮书》。
- 12 月 5 日豆包发布说明，将在刷分刷激励、金融类应用、部分游戏等场景限制 AI 操作手机的能力，说明里没有提到微信和电商。
- 【二手】 — [澎湃新闻](https://www.thepaper.cn/newsDetail_forward_32094236)；[界面新闻](https://www.jiemian.com/article/13736294.html)；[虎嗅](https://www.huxiu.com/article/4813501.html)

### Inferences
- **加入代理后，诈骗风险的三个变化**：
  1. 骗子不必再亲手远控。只要诱导受害人把电脑代理"分享"或"绑定"给骗子的聊天账号，或者让受害人自己对代理说"把钱转到某某账户"，代理就会执行。屏幕上没有陌生的鼠标在动，受害人更难察觉。
  2. 提示注入让"能往屏幕上放内容的人"都可能指挥代理：一封钓鱼邮件、一个网页、一条聊天消息，见第一部分。
  3. 手机聊天账号变成了电脑的钥匙：聊天账号或手机被盗，就等于电脑被控。
- 可以借鉴的控制，按来源列：
  - Quick Assist："只有自己发起的支持才放行"。对应到 PeerDesk：代理只接受**本人账户下**的设备指挥；新增指挥设备要在被控电脑本地确认，并设冷静期。
  - Android："来电加敏感应用"联动，30 秒强制等待。对应到 PeerDesk：检测到网银、支付、证券、加密钱包等应用或网站时，代理默认停手，并在被控端本地弹出警示和倒计时。
  - ToDesk："二次验证"。对应到 PeerDesk：高危操作要在第二个已绑定设备上确认。
  - Claude in Chrome 默认屏蔽金融站点、豆包主动限制金融应用。对应到 PeerDesk：金融类应用默认不让代理操作。
- 豆包事件说明，代理去操作微信、支付宝、网银，可能触发这些平台的风控，导致用户账号被下线甚至封禁。这既是体验问题，也是合规和商业关系问题。PeerDesk 应当默认不让代理操作这些高风险应用，并且如实告诉用户原因，符合项目"不静默降级"的原则。
- 远程控制工具常被骗子利用，杀毒软件和反诈系统可能误伤 PeerDesk 本身，签名和信誉会受影响。加入代理后，这类事故的严重程度只会更高，因此需要从第一天起留好反滥用的证据：审计日志、举报入口、连接来源记录。

### Gaps
- 没找到 TeamViewer、AnyDesk 官方反诈功能的一手说明（例如来电检测、新设备冷静期、高风险国家限制）。本次只拿到 Microsoft 和 Google 的做法。
- ToDesk、向日葵官方的反诈说明和数据，没有找到。
- 没有找到"骗子利用 AI 电脑代理行骗"的公开案例，目前只是推演。
- 中国电信诈骗里远控类工具的损失占比，没有找到官方统计。

---

## 四、业内的标准控制和主要厂商公开的安全模型

### Takeaway
业内已经收敛出一套共识组合：
- 隔离：独立的代理账户、独立的会话、虚拟机或容器。
- 最小权限：按文件夹、按站点或应用授权，权限可设时限、可撤销。
- 后果严重的操作前，由人类确认。
- 关键时刻用户接管：输入密码、付款等。
- 动作可区分、可审计，日志防篡改。
- 设计层面守住 Meta 的"代理二选一规则"：不可信输入、敏感数据、对外改变状态或通信，三者在一个会话里最多占两项。

Microsoft 的 Windows Agent Workspace 是目前与 PeerDesk 形态最接近的参考，但它仍在预览阶段。

### Cited Findings

**Microsoft（Windows）**
- 实验性代理功能页面（2025-10-16 首发，11-17 和 12-05 更新）【已核】 — [Experimental agentic features](https://support.microsoft.com/en-us/windows/ai/ai-features/experimental-agentic-features)
  - Agent Workspace 是一个独立的 Windows 会话，比 Windows Sandbox 那样的完整虚拟机更轻。代理在后台操作应用，用户继续用自己的会话。
  - 每个代理有自己的独立账户、独立工作区和独立权限，一个代理的权限不会延伸到另一个。
  - 文件夹访问：代理可以申请读写用户目录下的 6 个已知文件夹（文档、下载、桌面、音乐、图片、视频）。预览版 26100.7344 起，可以按代理设"始终允许 / 每次询问 / 从不允许"。代理账户还能访问所有已认证用户都能访问的公共目录。
  - 用户批准代理对数据的查询和代理的动作；用户可以审阅、批准、监视多步计划；需要时代理必须显式请求用户授权或决定。页面没有描述专门的"接管"控件。
  - 原则：
    - 代理的所有动作可观察、可与用户动作区分；代理要产生日志，Windows 要能用**防篡改审计日志**核验。
    - 最小权限，权限有时限、细粒度。
    - 代理的权限不得超过发起它的用户。
    - 系统里没有任何其他实体能特权访问代理。
  - 默认关闭，只有管理员能开启，开启后对设备上所有用户生效。目前是面向 Windows 预览体验成员的私有预览。
  - 明确警告跨提示注入（XPIA）、幻觉和意外输出。
- Windows 安全手册（2026-11-14 更新）把应用和代理安全分成三块【已核】 — [Application and agent security](https://learn.microsoft.com/en-us/windows/security/application-security/)；[Agent governance（2026-11-04）](https://learn.microsoft.com/en-us/windows/security/application-security/agent-governance)
  - 应用控制。
  - 隔离：其中新增了 Microsoft Execution Containers（MXC），按策略限制代理运行时的文件和网络访问。
  - 代理治理：代理身份、代理治理。企业部署手册的做法是：先观察代理的实际行为，再设计最小权限策略，按部署圈层逐步推开，持续监控，并保证能可靠回滚。
- Windows 365 for Agents：使用专门的 Microsoft Entra 代理用户账户，与人类用户分开；代理不复用、不冒充用户凭据。【二手（搜索摘要）】 — [Windows 365 Agents 安全概述](https://learn.microsoft.com/windows-365/agents/security-overview)

**Anthropic**
- 电脑操作工具的官方建议【已核】 — [Computer use tool 文档](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool)
  1. 用专门的、最小权限的虚拟机或容器。
  2. 不给模型账号登录信息这类敏感数据。
  3. 联网只允许白名单域名。
  4. 可能造成实际后果的决定，以及需要明确同意的事（接受 cookie、完成金融交易、同意服务条款），由人类确认。
  - 另外：在你自己的产品里开启电脑操作功能前，要告知终端用户相关风险并取得同意。
  - 分类器扫描截图，发现疑似注入时引导模型先向用户确认。
- Claude in Chrome【已核】 — [Piloting Claude in Chrome](https://claude.com/blog/claude-for-chrome)
  - 按站点授权，用户可随时授予或撤销。Team 和 Enterprise 管理员可以全组织开关，并设站点白名单和黑名单。
  - 发布、购买、分享个人数据等高风险操作前必须确认；实验性的自主模式仍对部分高度敏感操作保留确认。
  - 默认屏蔽金融服务、成人内容、盗版内容三类网站。

**OpenAI**
- Operator【已核】 — [Operator System Card](https://openai.com/index/operator-system-card)
  - 风险操作前确认（召回率 92%）。
  - 敏感网站上的监视模式：用户不活动或离开页面时自动暂停。
  - 银行交易主动拒绝；限制访问可能用于违法活动的网站。
  - 提示注入监控器（召回率 99%、精确率 90%）。
  - 接管模式：登录、付款等由用户亲自输入。系统卡没有给接管模式的数字。
- ChatGPT agent：敏感步骤前确认；敏感网站上要求标签页保持在前台。【二手】 — [OpenAI: prompt injections](https://openai.com/index/prompt-injections/)

**Google**
- Chrome 代理：独立的对齐审查模型（User Alignment Critic）、按站点分组的读写隔离（Agent Origin Sets）、导航到新网站前的审查、敏感网站、登录和购买前的用户确认。【二手】 — [Google 安全博客](https://blog.google/security/architecting-security-for-agentic/)

**设计原则和框架**
- Meta 的"代理二选一规则"（Agents Rule of Two，2025-10-31）：一个代理会话最多同时具备以下三项中的两项：(A) 处理不可信输入，(B) 访问敏感系统或私人数据，(C) 改变状态或对外通信。三项都需要时，必须有人类在环批准。这条规则受 Chromium 的同名规则和 Simon Willison 的"致命三要素"（lethal trifecta：私密数据、不可信内容、对外通信）启发，并多加了"改变状态"一项。Meta 自己说它是最小权限等原则的补充，不能替代这些原则。【二手（摘要），原文链接附后】 — [Meta AI 博客](https://ai.meta.com/blog/practical-ai-agent-security/)；[Simon Willison 2025-11-02](https://simonwillison.net/2025/Nov/2/new-prompt-injection-papers/)
- CSA（2026-04）：把规划和执行分成两个角色，作为架构层面的控制。【二手】 — [CSA](https://labs.cloudsecurityalliance.org/research/csa-research-note-computer-use-agent-safety-blindspots-20260/)
- Invariant：每个会话只开放一个仓库（一般化就是会话级的资源隔离），加上看上下文的动态访问控制。【二手】 — [Invariant Labs](https://invariantlabs.ai/blog/mcp-github-vulnerability)
- 美国 CISA 和英国 NCSC 等五眼国家机构的联合指南《Careful Adoption of Agentic AI Services》（2026-04-30 或 05-01，来源说法不一）：约 30 页，列出五大类风险（包括权限、设计和配置等）。要求把代理 AI 纳入零信任、纵深防御、最小权限体系，并强调"系统提示是指令，不是控制手段"。【二手，未核原文】 — [CISA 公告](https://content.govdelivery.com/accounts/USDHSCISA/bulletins/41544ff)；[Mayer Brown 2026-06-10](https://www.mayerbrown.com/en/insights/publications/2026/06/multi-agency-guidance-on-securing-agentic-ai-systems)；[CSA 解读](https://labs.cloudsecurityalliance.org/research/csa-research-note-cisa-agentic-ai-guidance)

### Inferences
**PeerDesk 权限设计草案**（综合上面的做法，供设计讨论）：

| 层级 | 代理能做什么 | 确认方式 | 参照 |
|---|---|---|---|
| 0 只看 | 截图和读屏，只描述、回答问题 | 首次授权 | 豆包"用户主动授权"；Windows 文件夹"每次询问" |
| 1 可操作（可撤销） | 点击、输入、打开应用，限白名单里的应用或窗口 | 按应用授权，可设"始终、本次、从不" | Windows 已知文件夹三档；Chrome 按站点授权 |
| 2 有后果 | 发消息、发邮件、提交表单、移动或删除文件、装软件 | 每次在手机上确认，显示**具体动作和目标**的预览 | Operator 和 Claude 的确认；MCP"完整显示命令" |
| 3 默认禁止 | 网银、支付、证券、钱包、密码管理器、系统安全设置 | 默认关闭；开启需在被控端本地操作，并有冷静期 | Claude in Chrome 屏蔽金融站点；Operator 拒绝银行交易；Android 30 秒等待 |

- 中国《智能体实施意见》提出的"仅限用户本人决策 / 需用户授权决策 / 智能体自主决策"三类边界（见第五部分）可以直接对应上表：第 3 层对应"仅限本人"，第 2 层对应"需授权"，第 0、1 层对应"自主"。
- 隔离：
  - 首选给代理一个独立的系统账户和独立会话，参照 Windows Agent Workspace。代理默认不在用户的主会话里操作，从而看不到、碰不到用户已经登录的网银和聊天软件。
  - 用户主动选择"在我的桌面上操作"时，才进入共享会话，这时适用更严格的确认。
  - Linux 和 macOS 没有现成的 Agent Workspace，需要评估用独立用户加虚拟显示，或者用虚拟机。
- 审计：
  - 每个代理动作都记下：时间、发起人、来自哪个设备、模型给出的理由、动作前后的截图哈希、确认人。
  - 日志用哈希链防篡改，参照 Windows 的"防篡改审计日志"。
  - 动作要和用户本人的操作区分开。
  - 可选会话录像：PeerDesk 本来就有编码管线，录像成本很低。
- 人类接管和急停：
  - 被控端本地始终显示"代理正在操作"的醒目标识。
  - 本地任何真实键鼠输入立即暂停代理，类似 Operator 的接管。
  - 手机端和被控端都有一键急停，急停后要重新授权。
- 限速：限制代理每分钟的动作数、每小时的高危动作数。超限要求重新确认，用来防止被劫持后的批量破坏或外泄。
- "二选一规则"的落地：
  - 代理在处理不可信内容（网页、邮件）时，只读、不对外发送。
  - 需要对外发送时，切到不读不可信内容的状态，或者由人确认。
  - 这个约束可以作为传输层"输入优先"之外的另一道硬门槛，写进端到端测试。

### Gaps
- Windows Agent Workspace 目前仍是预览，没有找到正式上线时间和最终的安全模型。MXC 的细节没有展开读。
- 没有找到 Apple（macOS）官方的代理隔离方案。
- CISA 联合指南原文没打开，五类风险的确切名称待核。
- 没有找到"确认疲劳"（用户习惯性点同意）的公开测量数据，以及在多少次确认后效果下降。

---

## 五、标准和法规：OWASP、NIST、CISA、中国规章、欧盟 AI 法案

### Takeaway
- **国际**：OWASP 的两份清单是最常用的威胁分类，即大语言模型应用十大风险（2025 版）和代理应用十大风险（2026 版，2025-12-09 发布）。NIST CAISI 在 2026-01 就"保护 AI 代理系统"公开征求意见；CISA 和五眼伙伴在 2026-04/05 发布了第一份代理 AI 联合指南。
- **中国**：《智能体规范应用与创新发展实施意见》（2026-05-08，网信办、发改委、工信部）提出三类决策边界和"不得超出用户授权范围"；把屏幕数据发给云端模型，主要受《个人信息保护法》约束（敏感个人信息要单独同意，出境要单独同意，要做影响评估）；《生成式人工智能服务管理暂行办法》只管面向境内公众的生成式服务。
- **欧盟**：《AI 法案》的高风险义务已被 Digital Omnibus 推迟到 2027-12 和 2028-08。

### Cited Findings

**OWASP**
- 代理应用十大风险 2026 版（OWASP GenAI 安全项目的代理安全倡议发布，日期 2025-12-09）【发布日期已核；条目名称来自二手】 — [OWASP 页面](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications/)；[promptfoo 整理的条目](https://www.promptfoo.dev/docs/red-team/owasp-agentic-ai/)

  | 编号 | 名称 | 说明 |
  |---|---|---|
  | ASI01 | Agent Goal Hijack | 代理目标被劫持，对应提示注入 |
  | ASI02 | Tool Misuse and Exploitation | 工具被误用 |
  | ASI03 | Identity and Privilege Abuse | 身份和权限被滥用 |
  | ASI04 | Agentic Supply Chain Vulnerabilities | 代理供应链漏洞：工具、插件、外部服务器 |
  | ASI05 | Unexpected Code Execution | 意外执行代码 |
  | ASI06 | Memory and Context Poisoning | 记忆和上下文被投毒 |
  | ASI07 | Insecure Inter-Agent Communication | 代理之间的通信不安全 |
  | ASI08 | Cascading Failures | 连锁失败 |
  | ASI09 | Human Agent Trust Exploitation | 利用人对代理的信任 |
  | ASI10 | Rogue Agents | 失控代理 |

  有资料称 ASI01 在"相关 LLM 十大风险"映射表里对应 LLM01（提示注入）。【二手】 — [HUMAN 博客](https://www.humansecurity.com/learn/blog/owasp-top-10-agentic-applications/)
- 大语言模型应用十大风险 2025 版：LLM01 提示注入、LLM06 过度代理（Excessive Agency）等。【未核：本次没有打开 OWASP 原页】 — [OWASP LLM Top 10](https://genai.owasp.org/llm-top-10/)

**NIST、CAISI**
- 2026-01-08，《联邦公报》刊登 CAISI 的征求意见（RFI）"Securing AI Agent Systems"。文中说自主行动的代理可能遭受劫持、后门攻击等利用；征求关于代理特有威胁、测量方法，以及"限制和监控代理在部署环境中访问范围"的部署措施的意见。意见在 2026-03-09 截止，案卷号 NIST-2025-0035。本次没有找到后续的结论文件。【二手（搜索摘要）】 — [Federal Register](https://www.govinfo.gov/content/pkg/FR-2026-01-08/html/2026-00206.htm)；[NIST 新闻](https://www.nist.gov/news-events/news/2026/01/caisi-issues-request-information-about-securing-ai-agent-systems)
- CAISI 用苏黎世联邦理工学院（ETH Zurich）的开源框架 AgentDojo 评测代理劫持，并开源了改进版 agentdojo-inspect。【二手】 — 同上
- NIST AI RMF（AI 100-1）是通用框架，本次没有专门核对它和代理相关的内容。【未核】 — [NIST AI RMF](https://www.nist.gov/itl/ai-risk-management-framework)

**CISA**
- 见第四部分：《Careful Adoption of Agentic AI Services》（2026-04/05）。【二手】 — [CISA 公告](https://content.govdelivery.com/accounts/USDHSCISA/bulletins/41544ff)

**中国**
- 《智能体规范应用与创新发展实施意见》（2026-05-08，国家网信办、国家发展改革委、工业和信息化部联合印发）【二手（新华网报道和安全内参解读），没有拿到原文全文】 — [新华网](https://www.news.cn/politics/20260508/aa31c404610e4cc59f47fea3ab04929b/c.html)；[安全内参解读](https://www.secrss.com/articles/90111)
  - 定义：智能体是具备自主感知、记忆、决策、交互与执行能力的智能系统。网信办负责人说明，智能体的高自主性和高权限带来隐私泄露、越权操作、行为失控等风险。
  - 要求厘清三类决策边界：**仅限用户本人决策、需用户授权决策、智能体自主决策**；用户对智能体自主决策享有知情权和最终决策权；"智能体执行操作不得超出用户授权范围"。
  - 发展"规则内嵌、行为围栏"等技术；在重要场景探索"行为可验证、可追溯"机制。
  - 分级治理：
    - 敏感领域和重点行业：由网信部门联合行业主管部门确定开放场景，实行备案、检测、问题产品召回。
    - 生活娱乐、日常办公等低风险领域：用合规自测、信息报告、分发平台管理、行业自律等方式治理。
  - 研究建立智能体注册平台，提供数字身份管理、能力声明等服务；防止智能体被用于网络诈骗等违法犯罪。
  - 安全内参文中"高风险操作二次确认""日志留存"等建议是作者本人的观点，**不是文件条款**。
- 《生成式人工智能服务管理暂行办法》（2023-08-15 施行）【已核】 — [网信办原文](https://www.cac.gov.cn/2023-07/13/c_1690898327029107.htm)
  - 第二条：只适用于"向中华人民共和国境内公众提供"生成式人工智能服务。企业等研发、应用这类技术，但没有向境内公众提供服务的，不适用。
  - 第九条：涉及个人信息的，依法承担个人信息处理者责任。
  - 第十一条：对使用者的输入信息和使用记录应依法履行保护义务；不得收集非必要个人信息；不得非法留存能够识别使用者身份的输入信息和使用记录；不得非法向他人提供；要及时处理查阅、复制、更正、删除等请求。
  - 第十五条：建立投诉、举报机制。
  - 第十七条：提供"具有舆论属性或者社会动员能力"的服务的，要做安全评估并履行算法备案。
  - 第二十条：境外向境内提供的服务不合规时，国家网信部门可以采取技术措施处置。
- 《个人信息保护法》【已核（网信办转载的法条原文）】 — [网信办转载](https://www.cac.gov.cn/2021-08/20/c_1631050028355286.htm)
  - 第十三条：合法性基础包括取得同意，以及"为订立、履行个人作为一方当事人的合同所必需"等。
  - 第二十一条：委托处理要约定目的、期限、方式、保护措施等，并监督受托方；受托方不得超出约定处理，未经同意不得转委托，结束后要返还或删除。
  - 第二十三条：向其他处理者提供个人信息，需要个人的单独同意。
  - 第二十八条：敏感个人信息包括生物识别、医疗健康、**金融账户、行踪轨迹**等信息，以及不满十四周岁未成年人的信息；只有在特定目的、充分必要、措施严格时才能处理。
  - 第二十九条：处理敏感个人信息应取得单独同意。
  - 第三十八条：向境外提供，需要通过安全评估、专业机构认证、签订标准合同等条件之一。
  - 第三十九条：向境外提供，需告知境外接收方等信息，并取得单独同意。
  - 第五十五条：处理敏感个人信息、自动化决策、委托处理、向他人提供、向境外提供等情形，要事先做个人信息保护影响评估，记录至少保存三年。
- 《网络安全法》修正：2025-10-28 通过，2026-01-01 施行；新增第二十条，对人工智能安全与发展作专门规定，包括伦理规范、风险监测和安全监管；同时加强与《数据安全法》《个人信息保护法》的衔接。【二手（网信办解读和新华网报道），没有核对条文原文】 — [网信办](https://www.cac.gov.cn/2026-01/02/c_1769093523928606.htm)；[新华网](https://www.news.cn/politics/20251028/b3400522666a4b3f8c844b9fa6c31131/c.html)

**欧盟《AI 法案》**
- Digital Omnibus on AI：
  - 理事会 2026-06-29 最终批准；斯洛文尼亚监管机构称 2026-07-08 由共同立法机构通过，2026-07-27 生效。
  - 附件 III 列出的独立高风险系统，义务推迟到 2027-12-02；嵌入受监管产品的附件 I 系统，推迟到 2028-08-02。
  - 临时协议中，AI 生成内容机器可读标记的期限据报为 2026-12-02，需核对正式文本。
  - 【二手（律所和监管机构的说明）】 — [Orrick 2026-07](https://www.orrick.com/ja-JP/Insights/2026/07/EU-AI-Act-Update-Digital-Omnibus-Finalizes-8-Compliance-Changes)；[AKOS（斯洛文尼亚）](https://www.akos-rs.si/en/umetna-inteligenca/explore/frequently-asked-questions-and-answers/digital-omnibus-on-artificial-intelligence)；[Gibson Dunn](https://www.gibsondunn.com/eu-ai-act-omnibus-agreement)

### Inferences
- **中国合规要点**（推断，不构成法律意见）：
  - 把被控电脑的截图发给云端模型，截图里很可能有**第三方**的个人信息（聊天对象、邮件联系人），还可能有金融账户等**敏感个人信息**。
    - PeerDesk 作为处理者，至少要对本人用户做清楚的告知和同意（第十三条）。
    - 涉及敏感信息时要单独同意（第二十九条），并做影响评估（第五十五条）。
    - 用第三方模型 API 时，要定性为委托处理（第二十一条）还是向他人提供（第二十三条），并相应签约或取得单独同意。
    - 用境外模型（例如 Anthropic、OpenAI）就构成出境，需要满足第三十八、三十九条的条件。
  - 所以产品上要提供：境内模型或本地模型可选；发送前在本地打码（账号、证件号、银行卡号）；"不保存截图、不用于训练"的明确承诺和开关。
  - 《生成式人工智能服务管理暂行办法》只管"面向境内公众"的服务。如果 PeerDesk 托管云向中国公众提供代理服务，很可能落入范围；是否构成"具有舆论属性或者社会动员能力"需要咨询律师。如果只是用户自带模型 API 密钥的工具形态，适用性较弱。这一点待核实。
  - 《智能体实施意见》把"日常办公"列为低风险领域，以合规自测、信息报告、分发平台管理为主；但"不得超出用户授权范围""行为可追溯"这类要求是通用的，和第四部分的权限分级、审计日志一致。
- **欧盟**：通用远程桌面代理本身一般不在附件 III 的高风险清单里。但企业如果拿它做员工监控、招聘评估等，可能落入高风险（推断，未核具体条文）。通用模型义务由模型提供方承担，不由 PeerDesk 承担。
- **OWASP 清单可以直接拿来做 PeerDesk 的威胁建模清单**：
  - ASI01 对应屏幕注入。
  - ASI02、ASI05 对应 shell 和文件操作。
  - ASI03 对应代理账户和令牌。
  - ASI04 对应 MCP 工具和技能。
  - ASI09 对应确认框被骗点同意、诈骗。
  - ASI10 对应急停。

### Gaps
- 《智能体规范应用与创新发展实施意见》原文全文没有拿到，具体条款编号和措辞待核。
- 《人工智能生成合成内容标识办法》（据记忆 2025-09-01 施行）是否适用于代理生成的操作或消息，本次没有调研。
- 个人信息出境认证等配套规定（2026 年施行的部分），本次没有核对。
- OWASP《Agentic AI – Threats and Mitigations》（2025-02）和 LLM Top 10 2025 原文，本次没有打开。
- 欧盟 AI 法案第 50 条透明度义务（告知用户正在与 AI 交互）在 Omnibus 之后的确切日期，没有核对。
- 美国各州的 AI 法规（例如科罗拉多州）对这类产品的影响，没有调研。
