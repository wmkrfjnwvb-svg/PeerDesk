# 操作电脑的 AI Agent 与系统级 Agent 平台：业内全景（截至 2026-11）

> 调研日期：2026-11-26。标注约定：
> - 【已核实】= 打开了一手来源（官方文档、官方博客、官方仓库、论文/基准官网）原文。
> - 【已读二手】= 打开了原文，但是媒体/第三方报道。
> - 【未核实】= 只看到搜索摘要或聚合站，没有打开原文；需要复核后再引用。
> - 旧条目标明日期。GitHub API 和 github.com 直连在本环境被拦，部分仓库的星数/许可证无法自己核对，已列入 Gaps。

---

## 1. Anthropic：Computer Use API、Claude 桌面端 / Claude Code / Cowork / Dispatch、Claude in Chrome；以及 MCP 本身

### Takeaway
Anthropic 是"能力 + 产品 + 协议"三条线都齐的一家：API 侧 computer use 已从 beta 工具升级为免 beta 头的 17 个成员工具的工具集；产品侧 Cowork + Dispatch 已经实现"手机下任务、自己的 Mac/Windows 电脑执行（含操控屏幕）"，但要求电脑开着、App 开着，Dispatch 已停止接受新用户；协议侧 MCP 已捐给 Linux 基金会下的 AAIF，2026-07-28 版改成无状态协议，适合做远程（经网关/负载均衡）的 MCP 服务。

### Cited Findings

**Computer Use API（开发者接口）**
- 当前版本是 `computer_toolset_20260801`，在 Messages API 的 `tools` 里写 `{"type": "computer_toolset_20260801"}` 即可，"The request needs no beta header"；一次给 Claude 17 个成员工具：`screenshot`、`zoom`、`left_click`、`right_click`、`middle_click`、`double_click`、`triple_click`、`left_click_drag`、`mouse_move`、`left_mouse_down`、`left_mouse_up`、`cursor_position`、`scroll`、`type`、`key`、`hold_key`、`wait`。【已核实】— [Claude Docs: Computer use tool](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)
- 旧版本：`computer_20251124`（beta 头 `computer-use-2025-11-24`，Opus 4.5/4.6/4.7、Sonnet 4.6 只支持这一版）、`computer_20250124`（beta 头 `computer-use-2025-01-24`）。Claude 5.5 系列（Fable 5.2、Mythos 5.2、Opus 5.5、Sonnet 5.5、Haiku 5.5）只接受新工具集，传旧版本返回 400。【已核实】— [同上](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)
- 它是客户端工具：截图和文件在开发者自己的环境里采集和存储，"Claude never connects directly to the environment"，由应用执行动作再把结果回传；参考实现是 Docker 里的 Xvfb 虚拟显示 + Mutter + Tint2。平台：Claude API、Amazon Bedrock、Google Cloud、Microsoft Foundry 等；"Not available in Claude Managed Agents"；ZDR 可用（Covered Models 除外）。【已核实】— [同上](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)
- 官方安全建议四条：专用 VM/容器最小权限；不给登录凭据等敏感数据；网络白名单；关键动作（同意条款、金融交易等）要人确认。Anthropic 会用分类器扫描截图等工具结果中的提示注入，关闭需联系支持；要求产品方先告知终端用户风险并取得同意。【已核实】— [同上](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)

**Cowork / Dispatch（手机远程让自己电脑干活）**
- 时间线：Cowork 2026-01 发布（Axios 2026-01-12 报道）【未核实】— [Axios](https://www.axios.com/2026/01/12/ai-anthropic-claude-jobs)；Dispatch 2026-03-17 研究预览，先 Max 后 Pro【未核实】— [MacStories](https://www.macstories.net/reviews/hands-on-with-claude-dispatch/)、[Impress Watch](https://www.watch.impress.co.jp/docs/news/2094782.html)；Cowork 2026-04-09 GA【未核实】— [The New Stack](https://thenewstack.io/claude-dispatch-versus-openclaw/)；2026-07 Cowork 上 Web 和手机（Max beta），云端会话在笔记本合上后仍继续跑，需要审批时推送通知【已读二手】— [Help Net Security 2026-07-08](https://www.helpnetsecurity.com/2026/07/08/claude-cowork-phone-mobile-web/)。
- 官方博客（2026-03-23）：Dispatch 是"one continuous conversation with Claude from your phone or your desktop"；没有现成工具时 Claude 直接操作屏幕（鼠标、键盘、浏览器）；"Claude will reach for the most precise tool first, starting with connectors"，屏幕操控是兜底且更慢；"Computer use is supported on macOS and Windows"；首次使用新应用前必须征得许可，部分应用默认禁止；扫描模型激活来检测提示注入；需要"desktop app is awake and running"。【已核实】— [Claude Blog: Put Claude to work on your computer](https://claude.com/blog/dispatch-and-computer-use)
- 帮助中心（当前版本）：Dispatch "limited beta on some Pro, Max, and Ultra plans"，**不再接受新用户**；需要最新桌面端（macOS / Windows x64 / Linux）+ 最新手机 App；配对流程：Cowork 左栏点 Dispatch → 打开文件访问和"保持电脑唤醒" → 完成；任务在用户电脑上执行，"your computer needs to be awake and the Claude Desktop app open"；电脑关机时改用跑在 Anthropic 服务器上的云端会话；开发任务走 Claude Code，知识工作走 Cowork；"Computer use isn't available in the Linux beta"；不能开新线程或管理多线程；警告手机指令"can trigger real actions on your computer"。【已核实】— [Claude Help: Assign tasks from anywhere in Cowork](https://support.claude.com/en/articles/13947068-assign-tasks-to-claude-from-anywhere-in-cowork)
- 安全研究者逆向称远程指令除 Claude 账号会话外没有额外认证。【未核实】— [Pluto Security](https://pluto.security/blog/inside-claude-cowork-how-anthropics-autonomous-agent-actually-works/)

**Claude in Chrome**
- 官方已发"generally available"公告；帮助中心称扩展在 Cowork 和 Claude Code 中 GA、在 Chrome 浏览器中仍为 beta；所有付费计划可用；Max/Team 的侧边栏已改为一个 Cowork 会话；2025-12-18 扩展到全部付费计划（此前 2025-11 先给 Max）。【未核实】— [Claude Blog: Claude in Chrome is generally available](https://claude.com/blog/claude-in-chrome-generally-available)、[Claude Help: Claude in Chrome](https://support.claude.com/en/articles/12012173-claude-in-chrome)

**模型与基准（Anthropic 自报）**
- Claude Opus 5.5 于 2026-09-22 发布，官方表格 OSWorld 2.1：Opus 5.5 81.8%（partial）、Fable 5.1 80.7%（partial）、Opus 5 74.0%（partial），GPT-6 Astra / GPT-5.6 Sol 未报告；"partial"未解释。【已核实】— [Anthropic: Claude Opus 5.5](https://www.anthropic.com/news/claude-opus-5-5)

**MCP：规范、治理、远程、鉴权**
- 2025-12-09 Anthropic 把 MCP 捐给 Agentic AI Foundation（Linux 基金会旗下 directed fund，Anthropic、Block、OpenAI 共同发起），治理模式不变。【未核实】— [MCP Blog 2025-12-09](https://blog.modelcontextprotocol.io/posts/2025-12-09-mcp-joins-agentic-ai-foundation/)、[Anthropic 公告](https://www.anthropic.com/news/donating-the-model-context-protocol-and-establishing-of-the-agentic-ai-foundation)
- 当前规范版本 2026-07-28（上一版 2025-11-25）。主要变化：去掉协议级会话和 `Mcp-Session-Id`；去掉 `initialize` 握手，每个请求在 `_meta` 携带协议版本和客户端能力 → **无状态**；新增必须实现的 `server/discover`；用 `subscriptions/listen` 取代 GET 端点和资源订阅；tasks 移出核心成为官方扩展 `io.modelcontextprotocol/tasks`（`tasks/get` 轮询、`tasks/update` 补充输入）；引入 Multi Round-Trip Requests（`input_required`）取代服务器主动发起的 sampling/elicitation 请求；Streamable HTTP 去掉 SSE 断点续传（流断了请求就丢，客户端必须重发）；要求 `Mcp-Method`/`Mcp-Name` 头便于网关路由；结果可缓存（`ttlMs`/`cacheScope`）；OpenTelemetry trace 透传。【已核实】— [MCP Spec 2026-07-28 Changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
- 鉴权：授权服务器应带 `iss`（RFC 9207），客户端必须校验；凭据按签发方绑定；**弃用 Dynamic Client Registration，改用 Client ID Metadata Documents**；Roots、Sampling、Logging、HTTP+SSE 进入弃用，最少保留 12 个月（弃用策略 SEP-2596）。【已核实】— [同上](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
- AAIF 博客（RC 阶段）补充：MCP Apps 扩展（工具关联 `ui://` 资源，宿主在沙箱 iframe 渲染 HTML，经 postMessage 走 JSON-RPC）；Enterprise-Managed Authorization 扩展（IT 通过身份提供方下发服务器访问）；WWW-Authenticate 增量授权；RC 于 2026-05 发布，Python/TS/Go/C# 有 beta SDK。【已核实（基金会官方博客，RC 阶段，终版以 changelog 为准）】— [AAIF Blog](https://aaif.io/blog/mcp-2026-07-28-whats-changing-and-how-to-migrate)

### Inferences
- Anthropic 的产品形态已经就是 PeerDesk 想做的事的一半："手机发消息 → 自己的电脑执行（含 GUI 操控）→ 回传结果"。但它的远程通道只覆盖"Claude 账号绑定的、开着桌面 App 的那台电脑"，没有远程桌面的画面通道，也没有设备管理（多台电脑、无人值守、唤醒）。Dispatch 停止接新用户、不能多线程，说明这条链路本身还不成熟（推断）。
- "连接器优先，屏幕兜底"是一线厂商的共识设计（Anthropic 明说；微软的 agent connectors、Gemini 的 Desktop 连接器同理）。PeerDesk 若做 agent，应同时提供结构化工具（文件、命令、剪贴板、应用启动）和屏幕操控，而不是只做"像素 + 鼠标"。
- MCP 2026-07-28 的无状态化、Streamable HTTP、网关路由头、tasks 扩展，正好适合"每台被控电脑暴露一个远程 MCP 端点，经 PeerDesk 的 P2P/中继到达"的架构；但 SSE 断点续传被去掉，意味着长任务必须用 tasks 扩展而不是长连接（推断）。

### Gaps
- Claude Code 的"Remote Control"（云端会话连接用户电脑上的文件夹）有公开文档页面，但本次没找到可引用的 URL。
- Cowork 在 Mac 上是否跑在本地 VM 里、computer use 截图分辨率/频率、每步延迟，没有找到官方数据。
- Dispatch 停止接新用户的原因和后续计划未查到。

---

## 2. OpenAI、Google、Microsoft、Apple

### Takeaway
四家都已把"操作电脑"做进自家主力产品：OpenAI 在 Codex（已并入 ChatGPT 桌面端）里做了 macOS 后台操控，支持锁屏执行、手机下任务；Google 把 computer use 做成 Gemini 3.5 Flash 的内置工具，并于 2026-11-19 让 Gemini Mac 客户端在隔离后台窗口里操作本地应用；微软把 Windows 定位为"agentic OS"（MCP 注册表 + Agent Workspace + 2026 年的执行容器 MXC + 云端 Windows 365 for Agents）；苹果走 App Intents 结构化路线，公开材料里没有面向系统的通用 computer use，MCP 只确认在 Xcode 里。

### Cited Findings

**OpenAI**
- Operator → ChatGPT agent（2025-07）：把可视浏览器、文本浏览器、终端、API 访问整合进 ChatGPT。【未核实】— [OpenAI: Introducing ChatGPT agent](https://openai.com/index/introducing-chatgpt-agent/)
- ChatGPT Atlas 浏览器（2025-10 发布）于 2026-07-09 宣布退役、2026-08-09 停用，功能并入新的 ChatGPT 桌面端（内置浏览器；可把任务交给 Codex 和 ChatGPT Work；另有在 OpenAI 服务器上跑的云端浏览器给 agent 用），并提供 Chrome 扩展。【未核实】— [TechCrunch 2026-07-09](https://techcrunch.com/2026/07/09/openai-is-shutting-down-atlas-but-its-ai-browser-ambitions-are-still-growing/)、[OpenAI Help](https://help-lb.openai.com/en/articles/20001371-evolving-atlas-into-chatgpt-for-browser-based-agentic-work)
- Codex 桌面端 computer use（2026-04-16）："use all of the apps on your computer by seeing, clicking, and typing with its own cursor"，"Multiple agents can work on your Mac in parallel, without interfering with your own work"；初期仅 macOS，EU/UK 稍后；同批发布 90+ 插件（技能 + 应用集成 + MCP 服务器）、SSH 远程 devbox（alpha）。【已核实】— [OpenAI: Codex for (almost) everything](https://openai.com/index/codex-for-almost-everything/)
- 2026-05-21/22：Codex 可从手机下任务、在 Mac **锁屏且屏幕关闭**时操作应用；需安装 Computer Use 插件并授予屏幕录制和辅助功能权限；每个新应用先征得许可，可设"Always allow"；不能操作终端、Codex 自身、系统管理员提示；EEA/英国/瑞士暂不可用。【已读二手】— [MacRumors 2026-05-22](https://www.macrumors.com/2026/05/22/codex-use-mac-apps-when-locked/)
- Codex App 于 2026-07-09 与 ChatGPT 桌面端合并。【未核实】— [Wikipedia: Codex (AI agent)](https://en.wikipedia.org/wiki/OpenAI_Codex_(AI_agent))

**Google**
- Gemini 2.5 Computer Use 模型（2025-10-07 预览）主要面向浏览器，"not yet optimized for desktop OS-level control"，驱动过 Project Mariner。【未核实】— [Google Blog 2025](https://blog.google/technology/google-deepmind/gemini-computer-use-model/)
- Project Mariner 2026-05-04 停止，技术并入 Gemini Agent、Chrome auto browse 等。【未核实】— [Wikipedia: Project Mariner](https://en.wikipedia.org/wiki/Project_Mariner)
- 2026-06-24：computer use 成为 Gemini 3.5 Flash 内置工具，覆盖"browser, mobile and desktop environments"；经 Gemini API 和 Gemini Enterprise Agent Platform 提供；安全：针对提示注入的对抗训练，两项可选企业护栏（敏感/不可逆动作需用户确认；检测到间接提示注入自动停任务）；文中无具体分数。【已核实】— [Google Blog: computer use in Gemini 3.5 Flash](https://blog.google/innovation-and-ai/models-and-research/gemini-models/introducing-computer-use-gemini-3-5-flash/)
- 2026-11-19：Gemini Mac 客户端上线 computer use，用 `@Computer use` 触发，在"isolated background window with a live preview right in your chat"中执行，不占用户主屏和光标；用户选择允许的应用、一键停止、登录和支付时暂停；Connectors 新增 Blender、Autodesk Fusion 等 Desktop 应用；美国英语区 AI Plus/Pro/Ultra 订阅；文中未提手机远程触发和 Windows。【已读二手】— [9to5Google 2026-11-19](https://9to5google.com/2026/11/19/gemini-mac-computer-use/)

**Microsoft**
- Ignite 2025（2025-11-18）：Windows 原生支持 MCP；应用把能力包装成 MCP 服务器（agent connectors）注册到 Windows on-device registry（ODR），也可注册云端远程 MCP；连接器默认在受控环境内运行、有独立身份和审计，经 Windows 管理的 MCP 代理；必须打包签名；File Explorer 和 System Settings 连接器公开预览；Agent Workspace 是隔离、受策略控制、可审计的空间。【未核实】— [Windows Developer Blog 2025-11-18](https://blogs.windows.com/windowsdeveloper/2025/11/18/ignite-2025-furthering-windows-as-the-premier-platform-for-developers-governed-by-security)
- Copilot Actions 2025-11-17 开始推送给 Windows Insiders，在 Agent Workspace 里处理本地文件。【未核实】— [Windows Insider Blog](https://blogs.windows.com/windows-insider/2025/11/17/copilot-on-windows-copilot-actions-begins-rolling-out-to-windows-insiders/)
- 官方支持页（更新于 2025-12-05）：每个 agent 用**独立账号**、在**独立 Windows 会话**里运行，可与用户并行，比 Windows Sandbox 这类完整 VM 轻；可申请访问 6 个已知文件夹（文档、下载、桌面、音乐、图片、视频），每个 agent 设"始终允许/每次询问/从不"；ODR 中的连接器在 agent workspace 中运行，运行前征得许可；默认关闭，只有管理员能开且对全机用户生效；Agent Workspace 为 Insider 私有预览，无 GA 日期；原则：不可抵赖（agent 动作可与用户动作区分）、机密性、授权、最小权限、防篡改审计日志。【已核实】— [Microsoft Support: Experimental agentic features](https://support.microsoft.com/en-us/windows/ai/ai-features/experimental-agentic-features)
- Manus 是早期合作方：Insider 版文件资源管理器右键"Create website with Manus"。【未核实】— [IBTimes](https://www.ibtimes.co.uk/windows-11-turns-ai-powerhouse-copilot-manus-testing-major-update-reveal-1748402)
- Build 2026（2026-06-02）：Microsoft Execution Containers（MXC，Early Preview），开发者声明 agent 可访问的文件/网络，运行时强制容器边界，进程/会话隔离随后给 Insider，Micro VM、Linux 容器、Windows 365 for Agents 集成在路线图；**Windows 原生 OpenClaw** 用 MXC 在容器里跑节点和网关（开源）；NVIDIA OpenShell 基于 MXC；**Windows 365 for Agents 在 Agent 365 内 GA**，提供给 computer-using agents 跑多步工作流的云 PC；本地模型 Aion 1.0 Instruct（Edge Insider 预览）、Aion 1.0 Plan（14B 工具调用，未来数月）；Windows AI APIs 扩展到 CPU/GPU（公开预览）；Intelligent Terminal 经 ACP 接 agent（实验预览）。【已核实（微软官方新闻稿日文版）】— [Microsoft Source Asia: Build 2026](https://news.microsoft.com/source/asia/features/build-2026-furthering-windows-as-the-trusted-platform-for-development/)
- Copilot Studio：computer use（自动化 Web 和桌面应用）2026-05 GA；Windows 365 for Agents MCP 服务器 2026-06 GA。【未核实】— [Copilot Studio What's new](https://learn.microsoft.com/en-us/microsoft-copilot-studio/whats-new)

**Apple**
- WWDC26 平台概览：App Intents 是"how our platforms understand what your apps can do"，用系统定义的 entity/intent schema，Siri 可自然语言调用；新 View Annotations API 让"这张照片""第二条消息"可被操作；Foundation Models 框架可接入云端模型（"call server models like Claude, Gemini, and more"），任何模型厂商可实现 Language Model 协议，夏季开源；Core AI 框架跑端侧模型；Xcode 用 MCP 接 agent 和工具、新增 Agent Client Protocol，内置 Anthropic/OpenAI/Google 的 agent；agent 能在模拟器里点击、滑动、输入。概览里**没有** macOS 通用 computer use 或系统级 MCP。【已核实】— [Apple WWDC26 Platforms State of the Union](https://developer.apple.com/videos/play/wwdc2026/102/)
- "macOS 27 系统级 MCP、Siri 可调用注册的 MCP 服务器"与"Siri 路径里没有 MCP"两种说法互相矛盾；Siri 可选 Gemini/Claude/ChatGPT 作为扩展的说法也只见于博客。【未核实】— [Pete Goldsmith 博客](https://petegoldsmith.com/2026/06/12/2026-06-12-siri-on-macos-golden-gate/)、[Stork.ai](https://www.stork.ai/blog/wwdc-2026-graded-what-apple-shipped-ios-27)

### Inferences
- 大厂的共同趋势：agent 不再抢用户的鼠标——Codex"own cursor"并行、Gemini"隔离后台窗口"、Windows"独立账号 + 独立会话"、豆包"虚拟桌面"（见第 3 节）。远程桌面产品要支持 agent，最好也让 agent 跑在**独立的虚拟显示/会话**里，用户本人照常用电脑，同时可以通过 PeerDesk 画面实时旁观和接管（推断）。
- 锁屏执行（Codex、豆包 Mac）和"保持唤醒"（Dispatch）说明：agent 要求电脑长期在线、无人值守——这正是远程桌面已有的无人值守服务、开机自启、唤醒的能力，是远控厂商做 agent 的天然优势（推断）。
- 微软把 MCP 注册表、隔离执行、审计做进 OS，第三方 agent（包括 PeerDesk）在 Windows 上未来应走 ODR/MXC 合规路线，否则可能被企业策略拦截（推断，待 GA 后验证）。
- 苹果短期不会给第三方提供 macOS 通用 GUI agent 接口；Mac 上仍只能靠辅助功能 + 屏幕录制权限（Codex 即如此）（推断）。

### Gaps
- Windows Agent Workspace / Copilot Actions 2026 年是否 GA、第三方 agent 能否接入 Agent Workspace，未找到官方 2026 文档。
- OpenAI 合并后的 ChatGPT 桌面端是否保留 Codex 的 computer use、是否支持 Windows，未查到官方说明。
- Gemini computer use 的 OSWorld 等具体分数未在官方博客正文中给出。

---

## 3. 中国厂商：字节、智谱、Manus、Kimi、阿里、腾讯、华为、联想/荣耀

### Takeaway
2026 年国内出现了"龙虾（Claw）"热：腾讯、Kimi、ToDesk、向日葵等大量产品基于或兼容开源 OpenClaw，主打"微信/手机远程让电脑干活"；字节豆包电脑版已能纯视觉操作 Windows/Mac（虚拟桌面）；华为小艺 Work 可从鸿蒙手机/平板远程给电脑下任务并支持 Windows。大部分信息来自自媒体，官方一手材料很少。

### Cited Findings

**字节跳动**
- UI-TARS-desktop 仓库包含 Agent TARS 和 UI-TARS-desktop 两个项目，后者是"native GUI agent for your local computer"，由 UI-TARS 和 Seed-1.5-VL/1.6 驱动；README 最新动态为 Agent TARS CLI v0.3.0（2025-11-05），v0.2.0 为 2025-06、v0.1.0 为 2025-04。【未核实】— [UI-TARS-desktop README](https://raw.githubusercontent.com/bytedance/UI-TARS-desktop/HEAD/README.md)
- 豆包电脑端：2026-08 在"工作任务"模式下通过"豆包虚拟桌面"操作 Windows，不依赖 MCP/API/插件，只靠看懂界面，可实时看、暂停、接管；2026-09-02 上线 Mac"操作电脑"和多 Agent 并行；2026-10 可录制操作保存为技能、Mac 锁屏后继续执行；UI-TARS-2 是豆包手机的底层技术。【未核实】— [新京报](https://m.bjnews.com.cn/detail/1787031438129399.html)、[科技日报](https://www.stdaily.com/web/gdxw/2026-08/18/content_565670.html)、[新浪财经](https://finance.sina.cn/2026-08-21/detail-inipaens1380641.d.html)

**智谱**
- 开源 Open-AutoGLM（手机 agent，模型 AutoGLM-Phone-9B），据评测模型 MIT、代码 Apache-2.0；电脑端能力主要在智谱清言 PC 客户端（"AutoGLM 沉思"）。【未核实】— [腾讯云开发者社区](https://cloud.tencent.com/developer/article/2639570)、[GitHub zai-org/Open-AutoGLM](https://github.com/zai-org/Open-AutoGLM)、[OSCHINA](https://www.oschina.net/news/341982/zhipu-autoglm)

**Manus（已被 Meta 收购）**
- 2026-03-16 发布 Manus Desktop（macOS Apple Silicon、Windows 10/11，不支持 Linux）及核心功能"My Computer"：在终端运行 CLI，读写本地文件、启动和控制本地应用、可用本地 GPU；执行前需用户批准（"Allow Once / Always Allow"）；本地任务需要电脑开着且 App 运行；按积分计费。【未核实】— [CNBC 2026-03-18](https://www.cnbc.com/2026/03/18/metas-manus-launches-desktop-app-to-bring-its-ai-agent-onto-personal-devices.html)、[GIGAZINE](https://gigazine.net/gsc_news/en/20260317-manus-my-computer/)
- 有一则称中国 2026-04-27 阻止 Meta 收购的说法，无其他来源佐证。【未核实】— [Codersera](https://codersera.com/blog/manus-ai-2026-status-meta-block-desktop-app/)

**Kimi（月之暗面）**
- OK Computer 2025-09-26 发布（agent 模式，云端执行）；Kimi Claw 2026-02 公测，"零部署的云端自动化平台"，30 秒启动常驻 agent，类 OpenClaw 云托管。【未核实】— [财联社](https://www.cls.cn/detail/2156286)、[Kimi Agent 帮助页](https://www.kimi.com/zh-cn/help/agent/agent-overview)

**阿里**
- 无影 AgentBay：2025-07 WAIC 发布，面向 agent 的云基础设施，可在 Windows/Linux/Android 云环境执行，接入方式 MCP、多语言 SDK、Web SDK；另有"Agentic Computer"形态的个人云电脑。【未核实】— [阿里云开发者社区](https://developer.aliyun.com/article/1729344)、[阿里云创业页](https://startup.aliyun.com/info/1093249.html)、[DOIT](https://www.doit.com.cn/p/543095.html)
- BenchLM 称 Qwen3.8 Max 以 86.1%（阿里自报）位列 OSWorld-Verified 第一（2026-11-23 快照）。【未核实】— [BenchLM](https://benchlm.ai/benchmarks/osworld-verified)

**腾讯**
- QClaw：个人向，核心是**微信远程操控电脑**，本地部署、扫码即用，内置 Kimi、DeepSeek 等模型，需内测码；WorkBuddy：2026-03-09 上线，企业办公向，兼容 OpenClaw 技能，接 QQ/飞书/钉钉；两者通过 ClawBot 打通微信与电脑。【未核实】— [知乎对比](https://zhuanlan.zhihu.com/p/2037464496638309931)、[CSDN](https://blog.csdn.net/u014354882/article/details/159490629)

**华为**
- HDC 2026（2026-06）小艺定位为鸿蒙"系统级智能体"；笔记本合盖待机时可联动手机与 PC 找文件打包发送（需通电、同一华为账号）。【未核实】— [网易科技](https://www.163.com/tech/article/KV8766IS00099504.html)
- 小艺 Work：2026-09 内测，2026-11-06 随小艺独立 App 上架鸿蒙电脑应用商店发布正式版；支持云电脑和本地电脑两种执行环境；**可从鸿蒙手机、平板远程向电脑下任务、看进度、收结果**；同步支持 Windows；文中未说明是 GUI 操控还是调工具。【已读二手】— [腾讯新闻（雷科技）](https://news.qq.com/rain/a/20261106A0DP1200)

**联想 / 荣耀 / 惠普**
- 联想国内"天禧 AI"（文中提到 4.5 版），海外 Qira（CES 2026 发布，9 月称覆盖 80 多种 PC 配置）；惠普 2026-03 发布企业向 HP IQ；该文未提荣耀。【已读二手】— [腾讯新闻（雷科技）](https://news.qq.com/rain/a/20261106A0DP1200)

### Inferences
- 国内最热的形态不是"电脑上的 agent"，而是"**用手机聊天软件（微信）遥控电脑上的 agent**"——QClaw、ToDesk AI、向日葵 OrayClaw、小艺 Work 都是这个思路。PeerDesk 定位"每台远程电脑变成可从手机/PC 聊天的 agent"与这股潮流正面重合，竞争已经开始（推断）。
- 大量产品基于 MIT 许可的 OpenClaw，门槛低、同质化严重；差异化只能来自：连接可靠性（P2P/穿透/中继）、可视化旁观与接管、安全审计、多设备管理（推断）。

### Gaps
- 字节、腾讯、ToDesk、向日葵都没找到官方公告原文；功能边界（GUI 操控 vs 只调命令/文件）、价格、成功率均缺一手数据。
- 荣耀 YOYO 的 PC agent、联想天禧是否能操作第三方应用 GUI，没查到。
- 阿里"千问直接操作用户电脑"的产品没有找到可靠来源。

---

## 4. 开源项目

### Takeaway
开源侧已经很成熟：OpenClaw（MIT，约 39.5 万星）成为"手机聊天遥控本地 agent"的事实标准并被微软官方移植到 Windows；Browser Use（MIT，约 12.2 万星）主导浏览器 agent；微软 UFO（MIT）有 Windows 原生 UIA + 视觉混合控制和多设备编排；Agent S 系列在 OSWorld 首次报告超过人类基线；还有大量 Windows/VNC/SSH/RustDesk 的 MCP 服务器，甚至出现了内置 MCP 的远程桌面（QuickDesk）。

### Cited Findings
- **OpenClaw**：约 395.2k 星、83.0k fork，MIT；"The AI that really does things. Any OS. Any Platform."；通道包括 Discord、iMessage、Slack、Teams、Telegram、WhatsApp、Google Chat、Signal 等 20+；有 macOS/iOS/Android/Windows/Linux 原生节点 App，节点提供语音、Canvas、摄像头、屏幕、本机动作；一个 Gateway 可做个人助理或多人共享，有远程暴露 runbook 和沙箱指南；页面未明确说明通用 GUI computer use。【已核实】— [GitHub openclaw/openclaw](https://github.com/openclaw/openclaw)
- OpenClaw 由 Peter Steinberger 于 2025-11 以其他名字首发、2026-01 改名；星数 2026-03 约 32.7 万 → 年中 38 万+。【未核实】— [OpenClawHQ](https://openclawhq.io/blog/openclaw-github)、[aiforautomation](https://aiforautomation.io/news/2026-03-21-openclaw-327k-stars-free-ai-assistant-whatsapp-telegram)
- **Browser Use**：122.1k 星、13.7k fork，MIT；"Open-source Python library for AI browser agents, plus Browser Use Cloud"；云浏览器 $0.06/浏览器小时（含反检测、验证码、住宅代理）。【已核实】— [GitHub browser-use/browser-use](https://github.com/browser-use/browser-use)
- **Microsoft UFO / UFO² / UFO³ Galaxy**：10.2k 星，MIT；UFO²（2025-04，LTS）是 Windows Desktop AgentOS，UIA/Win32/WinCOM 原生控制 + 视觉混合检测，GUI 动作与 API 调用混合；UFO³ Galaxy（2025-11，arXiv 2511.11332）多设备编排：任务拆成 DAG，按能力匹配设备，WebSocket 上的 AIP 协议，支持 Windows/Linux/Android，设备 agent 用 MCP 扩展工具，仍在活跃开发；投机多动作"51% fewer LLM calls"。【已核实】— [GitHub microsoft/UFO](https://github.com/microsoft/UFO)
- **Agent S（Simular）**：Agent S3 2025-10-02 报 OSWorld 69.9%，2025-12-15 报 72.60%"first to surpass human performance on OSWorld"；2026-08-28 称 Sai 以更低成本登顶 OSWorld 2.0（73%）；2026-10-20 发布 Agent S4（Qwen3.8-27B 在 OSWorld 2.0 严格通过率 21.3%，高于 GPT-5.5 13.0% 和 Opus 4.8 20.6%）；2026-11-24 v0.3.3 桌面安装包支持"fully private on-device computer use"（本地沙箱 + Ollama）；桌面 App 仅 Apple Silicon、macOS 14+。【已核实（项目自报）】— [Agent-S README](https://raw.githubusercontent.com/simular-ai/Agent-S/main/README.md)
- **Windows-MCP（CursorTouch）**：MCP 服务器连接 LLM 与 Windows，支持任意 LLM；有 remote 模式，经 windowsmcp.io 做云托管 Windows 自动化。【未核实】— [搜索摘要指向 CursorTouch/Windows-MCP](https://github.com/CursorTouch/Windows-MCP)
- **winremote-mcp**：控制任意 Windows 机器，截图（JPEG 压缩、多显示器）、点击、输入、滚动、快捷键，支持 streamable-http 和 stdio。【未核实】— [PyPI winremote-mcp](https://pypi.org/project/winremote-mcp/)
- **mcp-vnc**：让 agent 远程控制任何跑 VNC 服务的 Windows/Linux/macOS，鼠标、键盘、截屏。【未核实】— [mcp-vnc README](https://unpkg.com/@hrrrsn/mcp-vnc@1.0.2/README.md)
- **DeskVNC**：VNC/RDP/SSH 客户端自带 MCP 服务器，被控端不必装额外 agent。【未核实】— [DEV Community](https://dev.to/avinesh_a6c41ecc025971236/deskvnc-1d2b)
- **rustdesk-mcp（社区）**：通过本机 RustDesk 客户端（macOS）控制远程桌面，21 个 MCP 工具；**QuickDesk**：基于 Chromium Remoting 的 Qt 远程桌面，内置 MCP 服务器，其自比表称 RustDesk、ToDesk、TeamViewer 无内置 MCP（约 277 星）。【未核实】— [glama.ai rustdesk-mcp](https://glama.ai/mcp/servers/bnxnxhaldb)、[GitHub QuickDesk](https://github.com/acardozzo/quickdesk)
- **anydesk-agent**：第三方 MCP 服务器，12 个动作控制鼠标键盘截屏，与 AnyDesk 官方无关。【未核实】— [GitHub BryanTheLai/anydesk-agent](https://github.com/BryanTheLai/anydesk-agent)

### Inferences
- 对 PeerDesk 的许可约束：OpenClaw、Browser Use、UFO 都是 MIT，可借用（需在 THIRD_PARTY_NOTICES 注明）；Open Interpreter 的许可证需复核（见 Gaps），若为 AGPL 则不能引入（推断）。
- 已经有人把"远程桌面 + 内置 MCP"做出来（QuickDesk、rustdesk-mcp），但都是小项目，没有成熟的穿透/中继/画质体系；PeerDesk 的传输层优势在这里能转化成 agent 场景的差异化（推断）。
- UFO 的"UIA 结构化 + 视觉"混合控制、Galaxy 的多设备 DAG 编排，可作为 PeerDesk agent 层的参考设计（推断）。

### Gaps
- 本环境 GitHub API 和 github.com 直连被代理拦截，Open Interpreter、OpenAdapt、OS-Copilot、Windows-MCP、Desktop Commander MCP、UI-TARS-desktop 的星数、最近发布日期、许可证没能核实。凭记忆（2025 年中）：Open Interpreter 2024 年起改为 AGPL-3.0、OpenAdapt MIT、Desktop Commander MCP 提供终端/文件/进程控制——均**未核实**，引用前必须到仓库确认。
- 没有找到"手机端 App 控制 PC 的开源 agent"中除 OpenClaw 节点外的成熟项目。

---

## 5. 跨设备远程（手机 A 控制电脑 B）：谁能做、谁已经把远程访问和 agent 结合

### Takeaway
"手机下任务、远端电脑执行"已是主流功能（Claude Dispatch、Codex、OpenClaw、QClaw、小艺 Work），但它们几乎都只覆盖"同一账号、自己开着 App 的电脑"，靠 agent 厂商的云服务中转消息，不提供实时画面；真正把**远程桌面**和 agent 结合的是国内远控厂商（ToDesk AI、向日葵 MCP + OrayClaw）和 TeamViewer（MCP 只管账号/设备，会话内由 Tia 负责）。

### Cited Findings
- **Claude Dispatch**：手机 App 与桌面 App 配对，任务在电脑本地执行（可操控 macOS/Windows 屏幕），电脑须唤醒且 App 开着，现不接新用户。【已核实】— [Claude Help](https://support.claude.com/en/articles/13947068-assign-tasks-to-claude-from-anywhere-in-cowork)
- **Codex**：手机发任务，Mac 锁屏也能执行。【已读二手】— [MacRumors](https://www.macrumors.com/2026/05/22/codex-use-mac-apps-when-locked/)
- **OpenClaw**：通过 WhatsApp/Telegram/iMessage 等聊天软件和自托管 Gateway 遥控本机 agent。【已核实】— [GitHub openclaw](https://github.com/openclaw/openclaw)
- **腾讯 QClaw**：微信遥控电脑。【未核实】— [知乎](https://zhuanlan.zhihu.com/p/2037464496638309931)
- **华为小艺 Work**：鸿蒙手机/平板远程给电脑（含 Windows）下任务。【已读二手】— [腾讯新闻](https://news.qq.com/rain/a/20261106A0DP1200)
- **Manus My Computer**：本地任务要求电脑开机、App 运行（可从其他端发起任务的细节未核实）。【未核实】— [CNBC](https://www.cnbc.com/2026/03/18/metas-manus-launches-desktop-app-to-bring-its-ai-agent-onto-personal-devices.html)
- **ToDesk AI / ToClaw**：2026-05-27 发布的"跨桌面 AI 智能体"，基于 OpenClaw 深度定制；在 AI 界面输入需求即可操控远端电脑，内置桌面整理、数据查询等模板，可随时手动接管；称运算在云端完成；删除文件、改系统设置等敏感操作必须弹窗确认；公测免费；另有付费的"AI 智能审计"（录制远控操作便于追责）。【未核实，均为自媒体】— [凤凰科技](https://tech.ifeng.com/c/8tbcCD2OFyY)、[B 站专栏](https://www.bilibili.com/read/cv47446241)、[知乎横评](https://zhuanlan.zhihu.com/p/2056758468988740951)
- **向日葵（贝锐 Oray）**：向日葵 MCP 把远控能力封装成 MCP（设备检索/状态控制 + 远程会话与桌面自动化），兼容 Claude Code、Cursor、Codex 等；主控端需较新客户端（文称 V16.2.3+），推荐 stdio 模式；被控端需先手动远控一次并保存密码；AI 通过视觉识别操作被控端界面，可实时查看、随时人工介入；16.5+ 推出"向日葵 AI 远控" = OrayClaw（内置任务代理）+ 向日葵 MCP；另有硬件方案实现"关机也能远控"。【未核实】— [IT 之家](https://www.ithome.com/0/977/985.htm)、[中华网科技](https://m.tech.china.com/articles/20260311/202603111822571.html)、[GitCode](https://gitcode.csdn.net/69b776af54b52172bc61b714.html)、[向日葵服务页](https://service.oray.com/question/6011.html)
- **TeamViewer**：2026-09-24 发布 MCP 集成，基于 REST API，可管理租户、查设备健康、触发重启或连接设备、读会话数据和日志，但"no in-session capabilities will be available through the MCP server"；会话内由 Tia（TeamViewer Intelligent Agent）负责。【已核实】— [TeamViewer Insights](https://www.teamviewer.com/en-au/insights/introducing-teamviewer-model-context-protocol-integration/)
- **AnyDesk**：未发现官方 MCP 服务器。【未核实，来自竞品/评测站】— [ultrametric.ai](https://ultrametric.ai/arena/screen-sharing/battle/rustdesk-vs-anydesk)
- **云端执行替代路线**：Windows 365 for Agents（云 PC，GA）【已核实】— [Microsoft Build 2026](https://news.microsoft.com/source/asia/features/build-2026-furthering-windows-as-the-trusted-platform-for-development/)；阿里无影 AgentBay（云电脑 + MCP）【未核实】— [阿里云开发者社区](https://developer.aliyun.com/article/1729344)；Claude/ChatGPT 的云端会话与云浏览器【已核实/未核实见上】。

### Inferences
- 现有方案的共同短板（推断）：
  1. 远端电脑必须"醒着 + App 开着"，没有远程开机/守护/自动重连的完整方案（向日葵有硬件开机是例外）；
  2. 消息走 agent 厂商云端，没有 P2P，也没有实时高质量画面——用户看不到 agent 在远端做什么，最多看截图；
  3. 一个账号只绑一台或少数电脑，缺多设备管理和权限分级；
  4. 安全上，远程指令通常只靠账号会话鉴权（Pluto Security 对 Dispatch 的批评）。
- PeerDesk 的结构性机会：已有的设备码、无人值守、NAT 穿透、低延迟画面 = agent 的"手和眼"所需的通道；在其上加"聊天入口 + agent 执行 + 实时旁观/一键接管 + 审批推送 + 审计录像"，正好补上述短板。最直接的对手是 ToDesk AI 和向日葵 AI 远控（推断）。

### Gaps
- ToDesk AI 的 agent 是在云端看远端画面操作（截图走远控通道）还是在被控端本地执行，报道互相矛盾（"运算都在云端，本地只负责显示"）；需要实测。
- 各家远程 agent 的成功率、每步延迟、流量成本均无公开数据。

---

## 6. 基准：OSWorld / OSWorld-Verified / OSWorld 2.x、WindowsAgentArena、AndroidWorld

### Takeaway
短任务基准已接近或超过人类：OSWorld 人类基线 72.36%，Agent S3 于 2025-12 自报 72.60%，2026 年底聚合站称 OSWorld-Verified 顶分 86%–90%（自报、互相矛盾）；但长流程的 OSWorld 2.0 严格通过率只有约 20%（Opus 4.8 20.6%），说明"能做一步一步的操作"和"可靠完成一小时的真实工作"之间差距仍大。WindowsAgentArena 最高约 60%（人类 74.5%）；AndroidWorld 人类 80%，多家自报已超过。

### Cited Findings
- **OSWorld（2024，arXiv 2404.07972）**：369 个任务（另 43 个 Windows 任务用于分析）；"humans can accomplish over 72.36% of the tasks"；论文发表时最佳模型 12.24%。【已核实】— [OSWorld 官网](http://osworld-v1.xlang.ai/)
- **OSWorld-Verified（2025-07-28）**：修复社区报告的样例、加 AWS 支持（一小时内跑完）；"Verified"结果由维护方在统一设置下评测，自报结果单列；8 个 Google Drive 任务可能初始化失败，可选 369 或 361 题。官网排行表本次加载失败未能读取。【已核实】— [OSWorld 官网](http://osworld-v1.xlang.ai/)
- **OSWorld-Verified 2026 年顶分（聚合站，口径混杂）**：BenchLM 2026-11-23 快照 Qwen3.8 Max 86.1%（阿里自报）、Claude Fable 5 85%、Claude Mythos 5 85%；PresencAI（截至 2026-10-01）称官方榜第一为 Intelligence-Indeed Agent 90.19%；Sophon 称最高 85.0%（Fable 5）；llmlearner 称独立测得最佳为 Muse Spark Open 80.4。【未核实，互相矛盾】— [BenchLM](https://benchlm.ai/benchmarks/osworld-verified)、[PresencAI](https://presenc.ai/research/osworld-computer-use-leaderboard-2026)、[Sophon](https://sophon.at/evals/osworld-verified)、[llmlearner](https://llmlearner.com/rankings/osworld-verified)
- **Agent S3**：OSWorld 72.60%（2025-12-15，自报超过人类）。【已核实（项目自报）】— [Agent-S README](https://raw.githubusercontent.com/simular-ai/Agent-S/main/README.md)
- **OSWorld 2.0（2026，arXiv 2606.29537；官网称 2026-06-26 上线）**：108 个长流程任务，基于真实输入材料和有状态的用户资料，每题附安全审计；人类中位用时约 1.6 小时，69.6% 的任务熟练用户需 1 小时以上；Opus 4.7 每题平均 318 次工具调用（OSWorld 1.0 约 30 次）；500 步下：Claude Opus 4.8（最大思考、批量工具调用）严格 20.6%、部分得分 54.8%；Opus 4.7 18.2%；GPT-5.5 约 14%。【已核实】— [OSWorld 2.0 官网](https://osworld-v2.xlang.ai/)、[OSWorld v1 页面提示](http://osworld-v1.xlang.ai/)
- **口径冲突**：Anthropic 2026-09-22 报 Opus 5.5 在"OSWorld 2.1"上 81.8%（标注 partial，未解释）【已核实】— [Anthropic](https://www.anthropic.com/news/claude-opus-5-5)；Simular 称 Sai 以 73% 登顶 OSWorld 2.0，同时说 Agent S4 严格通过率 21.3%【已核实（自报）】— [Agent-S README](https://raw.githubusercontent.com/simular-ai/Agent-S/main/README.md)。高分（73%、81.8%）应是部分得分口径，严格通过率仍在 20% 左右（推断，口径需逐一核对）。
- **WindowsAgentArena（Microsoft，arXiv 2409.08264）**：基线 agent Navi 约 19%；聚合站列 VLAA-GUI 60.4% / 61.0%（2026-04 论文），人类 74.5%；Agent S3 单跑 50.2%、3 次 rollout 择优 56.6%。【未核实（Agent S3 数字来自其仓库，VLAA-GUI 来自聚合站）】— [Microsoft Research](https://www.microsoft.com/en-us/research/publication/windows-agent-arena-evaluating-multi-modal-os-agents-at-scale/)、[emergentmind](https://www.emergentmind.com/topics/windowsagentarena-waa)
- **AndroidWorld（Google Research）**：人类 80%（三次试验）；原论文最强基线 30.6%；聚合站列 AGI-0 97.4%、Sonnet 4.5 + Sonnet 4 86.2%、AutoGLM-Mobile 80.2%（均为导入的自报数字）；llm-stats 另称 GLM-5V-Turbo 75.7% 领先，口径不同。【未核实】— [benchmarklist](https://benchmarklist.com/benchmarks/androidworld/)、[Stanford AIMS](https://aimslab.stanford.edu/measurement-db/androidworld)、[llm-stats](https://llm-stats.com/benchmarks/androidworld)

### Inferences
- 对产品的含义（推断）：单步/短任务（打开应用、填表、改设置）已经可以交给 agent；一小时级的真实工作流严格成功率约两成，**必须保留人类旁观和随时接管**——远程桌面的实时画面恰好是这个监督界面。
- 基准分数大量为厂商自报、口径（严格/部分得分、步数上限、是否 best-of-N）不统一，PeerDesk 若对外宣传 agent 能力，应自建贴近真实的任务集并公开口径（与 CLAUDE.md"不造假、指标说话"一致）。

### Gaps
- OSWorld-Verified 官方排行表（JS 动态加载）本次没读到，2026 年官方 verified 顶分无法一手确认。
- OSWorld 2.1 与 2.0 的区别、"partial"的定义未查到。
- WindowsAgentArena、AndroidWorld 没有找到 2026 年官方维护的排行榜。

---

## 7. 对 PeerDesk 的含义（"每台远程电脑都变成可从手机/PC 聊天的 agent"）

### Takeaway
市场方向已被验证（Anthropic、OpenAI、Google、华为、腾讯、ToDesk、向日葵都在做"手机遥控电脑上的 agent"），PeerDesk 不是第一个；能赢的点在于远程桌面独有的基础设施——设备在线与唤醒、P2P/中继连接、低延迟实时画面（旁观与接管）、多设备管理、审计——而模型能力本身可买（API）或开源（OpenClaw、UFO、Agent S、UI-TARS）。

### Cited Findings
- 一线厂商都要求"电脑醒着 + App 开着"才能远程执行（Dispatch、Manus），或提供云端会话兜底。【已核实】— [Claude Help](https://support.claude.com/en/articles/13947068-assign-tasks-to-claude-from-anywhere-in-cowork)
- "连接器优先、屏幕兜底"。【已核实】— [Claude Blog](https://claude.com/blog/dispatch-and-computer-use)
- Agent 在独立会话/隔离窗口执行、不抢用户光标：Windows 独立账号 + 独立会话【已核实】— [Microsoft Support](https://support.microsoft.com/en-us/windows/ai/ai-features/experimental-agentic-features)；Gemini 隔离后台窗口 + 聊天内实时预览【已读二手】— [9to5Google](https://9to5google.com/2026/11/19/gemini-mac-computer-use/)；Codex 自己的光标、多 agent 并行【已核实】— [OpenAI](https://openai.com/index/codex-for-almost-everything/)。
- 权限模式趋同：按应用授权（首次询问 / 始终允许 / 从不）、敏感动作人工确认、登录支付暂停、一键停止。【已核实】— [Claude Blog](https://claude.com/blog/dispatch-and-computer-use)、[Microsoft Support](https://support.microsoft.com/en-us/windows/ai/ai-features/experimental-agentic-features)；【已读二手】— [MacRumors](https://www.macrumors.com/2026/05/22/codex-use-mac-apps-when-locked/)
- 计算机操作 API 是客户端工具，执行和截图都在开发者环境里，模型厂商不直连设备。【已核实】— [Claude Docs](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)
- MCP 2026-07-28 无状态 + Streamable HTTP + 网关路由头 + tasks 扩展 + CIMD 鉴权。【已核实】— [MCP Changelog](https://modelcontextprotocol.io/specification/2026-07-28/changelog)
- 远控同行动作：TeamViewer MCP（只管账号/设备，不进会话）【已核实】— [TeamViewer](https://www.teamviewer.com/en-au/insights/introducing-teamviewer-model-context-protocol-integration/)；ToDesk AI、向日葵 MCP/OrayClaw【未核实】— 见第 5 节。

### Inferences（均为推断，供报告撰写参考）
1. **架构建议**：被控端跑 agent 执行器（本地 MCP 服务：屏幕/鼠标键盘 + 文件 + 命令 + 剪贴板 + 应用启动），"大脑"可以是云端 LLM（Claude/Gemini/OpenAI 的 computer use API 都是客户端工具，天然适配）。手机/PC 聊天端通过 PeerDesk 现有的信令 + P2P/中继到达被控端的 MCP 端点；用 MCP tasks 扩展承载长任务，避免依赖长连接（2026-07-28 已去掉 SSE 续传）。
2. **流量与成本**：agent 模式下被控端直连 LLM API，截图不必经过 PeerDesk 中继；PeerDesk 中继只传聊天指令、审批和（用户打开旁观时的）视频流——中继成本比纯远控低。若"大脑"放在 PeerDesk 云端，则每步截图要经过中继，需要按截图大小/步数核算（推断，需实测）。
3. **差异化**：
   - 实时旁观和一键接管：别人给截图或"live preview"，PeerDesk 可给低延迟原画质画面，并在 agent 与人之间无缝切换控制权；
   - 无人值守 + 守护进程 + 唤醒：直接解决"电脑必须醒着、App 必须开着"；
   - 多设备：一个聊天里指挥多台电脑（类似 UFO³ Galaxy 的多设备编排）；
   - 审计：每步截图 + 录像 + 操作日志，企业付费点（ToDesk 已把"AI 智能审计"做成付费功能）。
4. **隔离执行**：Windows 上优先用独立会话/虚拟显示（参考 Agent Workspace、MXC），macOS 上用虚拟显示器 + 辅助功能权限；避免 agent 抢用户鼠标。权限按应用分级、敏感动作推送到手机审批；按 CLAUDE.md"诚实、不静默降级"，没有屏幕录制/辅助功能权限时明确告诉用户而不是假装能做。
5. **开源复用**：OpenClaw（MIT）可作为聊天通道/技能生态参考或直接兼容其技能；UFO（MIT）的 UIA + 视觉混合控制可参考；Open Interpreter 许可证需复核，若为 AGPL 不能用。
6. **能力预期管理**：短任务可用，长流程严格成功率约 20%（OSWorld 2.0），产品文案和计费不应承诺"全自动"，应强调"可随时看、随时接管"。
7. **商业**：Anthropic 把 Dispatch 限于 Pro/Max/Ultra；Manus 按积分计费；ToDesk 审计付费。PeerDesk 可按"agent 设备数 + 托管 LLM 用量 + 审计/企业管控"计费，计费挂账户（与 CLAUDE.md 关键决定一致）。

### Gaps
- 没有任何一家公开远程 agent 的端到端指标（任务成功率、每步延迟、流量），PeerDesk 需要自建量尺。
- 微软 ODR/MXC 对第三方远控软件的签名与打包要求细节（是否需要 MSIX、商店签名）未查到，需在做 Windows agent 前专项调研。
- 中国监管对"AI 远程操控他人电脑"的合规要求（备案、审计留存）未查。
