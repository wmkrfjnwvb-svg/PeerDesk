# 行业调研：远程桌面产品的 Agent 接入（Jump Desktop 为主）

调研日期：2026-11-28。范围：Jump Desktop 的 "AI agents (Beta)"，Anthropic computer use、MCP 规范，以及其他远程桌面产品的 Agent 接入。

标记说明：
- 【原文】：来自厂商官网、官方文档、规范原页面。注意：网页由抓取工具转成摘要后读取，除明确加引号的以外是转述，不是逐字复制。
- 【二手】：媒体、博客、第三方列表、下载站。
- 【未核实】：推断，或只有单一来源的说法。
- 页面内容里没有发现夹带"要 AI 做事"的文字。

---

## 0. 结论先行

1. **Jump 的做法本质上是"本机 CLI 当 MCP 服务器"**。Agent 客户端（Claude Code、Codex CLI、OpenCode、Cline、Zed、Continue、Goose）通过 stdio 启动 `jump-cli mcp`，CLI 用 Jump 账号的 viewer token 连云端，再连到装了 Jump Connect 的被控机。能力：列出电脑、在线状态、同时连最多 8 台、截图、鼠标键盘、剪贴板、系统音频录制、显示器/虚拟显示器、隐私模式、请求登录。【原文】
2. **Jump 没有按工具分层的"批准"环节**。文档的安全模型是"Agent 拥有登录用户在被控机上的全部权限"，靠 viewer token、独立账号或 Access Groups 限制能连哪些机器，靠 Mission Control 人工盯着和随时接管。文档明说"批准与审计"没有在这几页里描述。【原文】（"没有"的结论只限于我读到的页面；MCP 页没有列工具清单，所以工具级细节【未核实】。）
3. **Jump 的 MCP 工具清单（名字、参数、返回）公开文档里查不到**。官方 MCP 页只写能力，不写工具名。要拿到清单，只能装 `jump-cli` 后跑 `tools/list`，或用 `--help-ai`。本调研没有装。【未核实】
4. **免费 8 台的条件**：Jump 任何账号（含个人免费版）都能用，不要 Teams 订阅；8 是同时连接数上限。多个虚拟显示器、Sleep Override 要 Teams Enterprise（$15.99/用户/月）。被控端必须是 Jump Connect，必须走 Jump 账号和云（信令/中继）。【原文】
5. **同类产品里，Agent 接入做到"真能操作屏幕"的很少**：Jump（官方、有 Mission Control）、向日葵（MCP + CLI，主要来自二手报道）、QuickDesk（开源 MIT，20+ 工具）。TeamViewer 的 MCP 只管理租户和发起连接，官方明说不含会话内操作。Astropad Workbench 是给人看 Agent 的远程桌面，没有 MCP。Splashtop、AnyDesk、RustDesk 官方没有查到。ToDesk 是自带 Agent（ToDesk AI/ToClaw），不是给第三方 Agent 开接口。
6. **标准层面有现成的做法可以直接抄**：MCP 的 `annotations`（readOnlyHint/destructiveHint/idempotentHint/openWorldHint）只是提示，规范要求客户端对不可信服务器的注解不要信；规范只写"应当有人在环、应当弹确认"，没有强制机制。所以**批准必须由我们的服务端执行，不能靠客户端自觉**。Anthropic computer use 的动作集和坐标约定是事实标准，我们的操作工具应对齐它。
7. **我们最值得做的差异化**（详见第 6 节）：
   - 服务端强制的风险分层批准，批准推送到手机；
   - Windows/Linux/Mac 一致加开源自托管，审计日志可导出、可防篡改；
   - 传输质量：把 Agent 看到的画面和人接管看到的画面都放到我们的低延迟实时层上（静止后马上变清晰，文字可读）。

---

## 1. Jump Desktop 的 Agent 功能

### 1.1 总览

- 首页标注 "Jump CLI and AI agents Beta"：让 Agent 看屏幕、用鼠标键盘；最多 8 台；Mission Control 看实时会话；"Free with any Jump Desktop account"，"No Teams subscription needed"。【原文】
- 支持 Windows、Mac、Linux 的电脑和虚拟机。【原文】
- 搜索引擎（WebSearch）摘要印证了首页内容，另外找到的是无关的 JumpCloud、AWS 等产品。【二手】
- 发布时间：docs 首页的"最近发布"只列到 10.17.14（2026-11-26），没有带日期的 Agent/CLI 条目；`whats-new/` 路径 404。准确的上线日期和版本查不到。【未核实】

### 1.2 Agent 怎么接进来

| 项目 | 内容 | 标记 |
|---|---|---|
| 协议 | MCP，stdio。命令 `jump-cli mcp`，由 Agent 客户端启动 | 【原文】 |
| 登录 | `jump-cli auth viewer login`，打开打印出的网址登录；Agent 代登可加 `--json`，输出 `sign-in-url` / `signed-in` 事件；无头 Linux 有单独流程；服务端从不自己开浏览器 | 【原文】 |
| 自检 | `jump-cli doctor` | 【原文】 |
| 客户端配置 | Claude Code：`claude mcp add jump-cli -- jump-cli mcp`；Codex CLI：`codex mcp add jump-cli -- jump-cli mcp`；OpenCode（`opencode.json`，`type: local`）；Cline/Zed 等 `mcpServers` 风格；Continue（`.continue/mcpServers/jump-cli.yaml`）；Goose（命令行扩展） | 【原文】 |
| 启动参数 | `--idle-suspend-timeout`（秒，默认 120，Agent 空闲就暂停画面更新省带宽，0 关闭）；`--mission-control-addr`（默认只监听本机，随机端口） | 【原文】 |
| 其他命令 | `viewer status [--watch]` 查在线；`connect bootstrap` 一条命令装 Connect 并添加电脑（本地或 SSH）；`tokens list/revoke`；`--help-ai` 给 Agent 看的命令概览；全局 `--json` | 【原文】 |
| 团队管理命令 | `team devices/users/groups/access/history audit|connections/billing` 等，需要 Admin 或 Admin（只读）角色和 API token；说明 Jump 另有 REST API 和 CLI 用于脚本和 Agent 做管理 | 【原文】 |
| 运行位置 | CLI 装在跑 MCP 客户端的那台机器上（Windows/macOS/Linux） | 【原文】 |

### 1.3 工具清单

官方 MCP 页只用文字列能力，没有工具名、参数、返回结构。能力如下：【原文】

- 列出账号可访问的电脑和在线状态；
- 连接（最多同时 8 台）、断开；
- 截图；
- 鼠标：移动、点击、拖动、滚动；键盘：输入文字、按键和组合键；
- 读/写被控机剪贴板；
- 录制被控机系统音频到文件；
- 显示器：选择查看的显示器、创建虚拟显示器、设置分辨率、恢复原布局；
- 隐私模式（被控端支持且管理员允许；官网说明是屏幕黑屏、本地键鼠被锁）；
- 请求登录：由 Mission Control 向人要账号密码，密码不经过 Agent。

截图、剪贴板内容、音频录音都保存成**运行 MCP 客户端那台机器上的文件**，会话结束或 CLI 退出时删除。【原文】

查不到：每个工具的名字、参数（坐标系、缩放）、返回格式、错误码、是否有批量动作。【未核实】

### 1.4 权限和安全模型

| 问题 | 答案 | 标记 |
|---|---|---|
| 谁授权 | Agent 用用户的 Jump 账号登录（viewer token）。被控机上没有单独的"允许 Agent"开关 | 【原文】（"没有"限于所读页面） |
| 权限范围 | "Agent 能做登录用户在被控机上能做的任何事"，包括删文件、改设置 | 【原文】 |
| 限制能连哪些机器 | 用单独的 Jump 账号，或在团队里用 Access Groups（Access Groups 需要 Teams 计划，Pro 有 5 个，Enterprise 不限） | 【原文】 |
| token 类型 | 只用 viewer token：能连接，不能改账号/团队设置；不要给 API token。撤销：`jump-cli auth logout`、`jump-cli tokens revoke <id>` | 【原文】 |
| 批准环节 | 没有看到按操作的批准。人的介入方式是：看 Mission Control，随时点 Take control | 【原文】（文档明确 check-in 页没有描述批准；MCP 页未写） |
| 审计 | Mission Control 的 Activity 列出 Agent 每个动作（本地网页，会话结束后是否保留未写明）。团队有 Activity Logs / Connection Logs 和 `team history audit|connections`，但这是连接/管理层面，不确认是否记录 Agent 的每个动作 | 【原文】/【未核实】 |
| 提示注入 | 文档要求：把被控机屏幕和剪贴板上的文字当不可信，因为可能含针对 Agent 的指令；不要把密码发给 Agent | 【原文】 |
| 数据流向 | 连接端到端加密，Jump 看不到屏幕；但截图、剪贴板、音频会发给 Agent 使用的模型厂商，保留期由模型厂商定 | 【原文】 |
| 密码处理 | 登录凭据在 Mission Control 里由人输入，"never sent to the agent" | 【原文】 |
| Mission Control 访问控制 | 本机网页，链接里带访问密钥，需保密；默认只监听本机 | 【原文】 |
| 清理 | 截图、剪贴板文件在会话结束时删除 | 【原文】 |

### 1.5 Mission Control

【原文】
- 本机 Web 面板，由 MCP 服务端在运行 Agent 客户端的机器上提供，链接由 Agent 给出。
- 实时画面；每个会话页有 Activity（动作列表）；"Sessions waiting on sign-in appear first"（等登录的会话排前面）。
- Take control / Give back control：人接管再还给 Agent。
- 密码输入：人在这里输入。
- 官网首页的示意图显示 8 个实时会话，覆盖 Windows/macOS/Linux。
- Mission Control 标注 Beta。

没有看到：缩略图墙的具体样式、动作回放、按动作暂停/批准、手机推送、跨机器汇总（它是每台跑 Agent 的机器上一个本地面板，不是云端面板）。【未核实】

"Check in"：另一条路是用 Jump Desktop 客户端直接连到 Agent 正在用的同一块（虚拟）显示器看进度，可选 Observe 模式（只看不操作）；手机（iPhone/iPad，Android Beta）也能看；虚拟显示器默认断开即删除，Windows/Mac 上可选 Keep After Disconnect。同一时间只有一个连接能创建虚拟显示器，Agent 会话和人的会话会互相挡住。【原文】

### 1.6 价格和限制

- Agent 接入和 Mission Control：任何 Jump 账号免费，不需要 Teams。【原文】
- 同时最多 8 台。【原文】
- Jump Desktop 个人版：Windows、Android、网页免费；Mac、iPhone/iPad 一次性买断；Mac 14 天试用。Teams Pro $7.99/用户/月，Teams Enterprise $15.99/用户/月，年付省 20%。【原文】
- 多个虚拟显示器（Enterprise 最多 4 个）、Sleep Override（Windows/Mac 主机不休眠）只有 Enterprise 才有。【原文】
- 注意：免费的是 Agent 接入，被控机仍需装 Jump Connect，Mac 端的客户端也要买；"免费"不等于零成本的整条链路。【未核实：整条链路成本是推断】

### 1.7 系统、被控端、云

- CLI：Windows、macOS、Linux（Debian/Ubuntu、RHEL/Fedora）。被控端：Jump Desktop Connect，Windows、macOS、Linux（Linux 是 Beta）。【原文】
- Mac 被控端需要有人在机器前给 Connect 授予"屏幕录制"和"辅助功能"权限，否则 Agent 看不到屏幕、控制不了输入。【原文】
- 必须是 Jump 的被控端：是。需要 Jump 账号和 Jump 的云服务（登录、设备列表、连接）：是（`auth viewer login`、`viewer status`）。没有看到自托管或纯局域网（不走云）的 Agent 模式。docs 里有 "Fluid" 和 "Deployment/Infrastructure" 章节，是否能不依赖云【未核实】。
- 主机不能睡眠，睡眠的主机连不上。【原文】

### 1.8 对 Jump 的评价（我们的判断）

- 强：官方集成、7 个 Agent 客户端配置现成、Mission Control 的"密码只给人输、不给 Agent"这个设计很好、接管体验完整、8 台免费、端到端加密。
- 弱（基于文档，不等于实测）：
  - 无按工具/按风险的批准；人在不在场全靠人盯着面板；
  - 面板是本机网页，不是云端多机汇总，也没看到手机推送；
  - 绑定 Jump 账号和云；Linux 被控端是 Beta；
  - 画面质量、延迟没有任何公开指标；
  - 文档承认"Agent 权限等于用户权限"。

---

## 2. Anthropic computer use 工具定义

来源：platform.claude.com（docs.anthropic.com 301 到这里）。【原文】

- 工具版本：`computer_toolset_20260801`（无需 beta 头，单条声明 `{"type":"computer_toolset_20260801"}`，含 17 个成员工具）；`computer_20251124`（beta 头 `computer-use-2025-11-24`）；`computer_20250124`（beta 头 `computer-use-2025-01-24`）。
- 动作（新工具集的成员）：

| 动作 | 参数 |
|---|---|
| `screenshot` | 无 |
| `zoom` | `region: [x0,y0,x1,y1]`，返回该区域全分辨率图，缩放到常规截图尺寸并保持宽高比 |
| `left_click` / `right_click` / `middle_click` / `double_click` / `triple_click` | `coordinate`（可选 [x,y]，省略则当前光标）、`text`（可选修饰键 shift/ctrl/alt/super，可 `+` 组合） |
| `left_click_drag` | `start_coordinate`、`coordinate`、`text`（可选） |
| `mouse_move` | `coordinate` |
| `left_mouse_down` / `left_mouse_up` | 无 |
| `cursor_position` | 无，返回文本 [x,y] |
| `scroll` | `scroll_direction`（up/down/left/right）、`scroll_amount`、`coordinate`（可选）、`text`（可选） |
| `type` | `text` |
| `key` | `text`（如 "Return"、"ctrl+s"、"alt+Tab"）、`repeat`（1 到 100） |
| `hold_key` | `text`、`duration`（秒，最多 300） |
| `wait` | `duration`（秒，最多 300） |

- 坐标约定：坐标是**截图像素**，原点左上角，zoom 图不改变坐标系。
- 分辨率：工具集模型长边最多 2576 像素、总计约 3.75 兆像素（4784 视觉 token）；更早的模型长边 1568、约 1.15 兆像素；工具集下超大图片直接报校验错误，API 不会替你缩小。推荐显示尺寸：通用桌面 1024x768 或 1280x720，网页 1280x800 / 1366x768，避免高于 1920x1080。Retina 屏要么缩小 2 倍再发，要么坐标除 2。
- 批量：一次回合里多个动作按顺序执行，第一个失败即停，后面的返回 `Not executed: an earlier computer action in this turn failed.`
- 安全建议（原文列表）：用最小权限的专用虚拟机/容器；不要让模型接触账号登录信息等敏感数据；把联网限制在域名白名单；让人确认有现实后果的决定和需要明确同意的任务（接受 cookie、付款、同意条款）。提示注入：有分类器扫描工具返回的截图，但上述防护仍然重要。
- 延迟和可靠性：文档自己承认延迟高、坐标可能出错、滚动不稳；定位是不要求速度的场景。
- 数据：computer use 是客户端工具，截图、操作都存在你自己的环境里。
- 成本：声明工具集约增加 4500 输入 token；关掉 `zoom` 少约 410。

对我们的含义：**操作类工具的参数直接对齐这套（坐标、修饰键、动作名），Agent 提示词里不用另教**；返回的截图要告诉 Agent 缩放比例，并保证不超过上限。

---

## 3. MCP 规范里和风险、确认、annotations 相关的部分

来源：modelcontextprotocol.io，规范 2025-06-18 的 tools 页和 schema 页，以及 draft 的 tools 页。【原文】

- 交互模型：工具是"模型控制"的；协议不规定交互方式。警告块：为安全起见，"SHOULD always be a human in the loop with the ability to deny tool invocations"；应用 SHOULD 提供界面说明暴露了哪些工具、调用时有明显的视觉提示、对操作弹确认。注意都是 SHOULD，不是 MUST。
- 注解（`ToolAnnotations`，schema 页原文）：`title`、`readOnlyHint`（默认 false）、`destructiveHint`（默认 **true**，仅在 readOnlyHint 为 false 时有意义；false 表示只做增量更新）、`idempotentHint`（默认 false）、`openWorldHint`（默认 **true**）。说明：所有属性都是**提示**，不保证如实描述工具行为；客户端不应根据来自不可信服务器的注解做工具使用决定。
- 规范 tools 页：客户端 MUST 把工具注解视为不可信，除非来自可信服务器。
- 安全考虑：服务端 MUST：验证所有输入、实现访问控制、对工具调用限速、清理工具输出。客户端 SHOULD：对敏感操作请求用户确认、调用前给用户看工具入参、验证工具结果、设置超时、**记录工具使用用于审计**。
- 错误：协议错误用 JSON-RPC error；工具执行错误用 `isError: true`，并要给出能让模型自我纠正的信息。
- 结构化输出：`outputSchema` + `structuredContent`，同时在 text 里放序列化 JSON。图片内容用 `type: image`（base64 + mimeType），可带 `annotations.audience/priority`。
- draft 版本的新东西：服务端可以在 `tools/call` 里返回 `input_required`，用 elicitation 向用户要补充信息（`inputRequests` / `inputResponses`）；`tools/list` 的结果**可以按授权不同而不同**（例如只返回调用者被授予的 scope 允许的工具）；工具要保持确定顺序以利于缓存。这是 draft，不是正式版，后续可能变。
- 规范里没有的：没有定义"批准"的协议机制和风险等级；`destructiveHint` 之类只是提示。

对我们的含义：
1. annotations 要写（对客户端有帮助），但**我们的批准逻辑不能依赖它**；风险层由服务端的策略表决定。
2. "按授权返回不同工具"（draft）正好对应"只读 token 看不到操作工具"，可以在正式版中实现。
3. 批准用 draft 的 elicitation 可能能直接在支持它的客户端内弹出，但客户端支持度未知【未核实】，所以必须有带外通道（手机推送）作为主路径。

---

## 4. 其他远程桌面产品的 Agent 接入

| 产品 | 做到哪一步 | 来源和标记 |
|---|---|---|
| **TeamViewer** | 2026-09-24 官方推出 MCP 集成（TeamViewer ONE）。能做：用自然语言管理租户、查设备健康和性能、触发重启或连接设备、看会话数据和日志。**不含会话内操作**，会话内由其 Tia 智能体处理，仍需 TeamViewer 界面。OAuth 登录，令牌权限有限，密码不给 AI；仅付费许可（需要 WebAPI）；可自托管（GitHub 仓库）或用其托管的 MCP 服务。没有看到逐操作确认；Claude 里选 "Always allow" 后可能免确认（页面未明说）。完整工具清单在一份 PDF 里，没读到 | 【原文】（KB 页和博客页）；清单【未核实】 |
| **Astropad Workbench** | 2026-04 发布。远程桌面 + 终端，给人在手机/iPad/Mac 上看和介入 Agent（Claude Code、Codex、Cursor、Xcode 等）。官网和报道均未提 MCP/API；没有 Agent 权限控制，只有隐私幕和 AES-256 端到端加密。无头 Mac mini 的虚拟显示器、智能睡眠管理。免费每天 20 分钟，Unlimited $14.99/月或 $79.99/年（TechCrunch 报道为 $10/月、$50/年，与官网不一致，以官网为准）。被控 macOS 15+，客户端 iOS 26+，不支持 Windows | 官网【原文】；TechCrunch、MacStories、MacRumors 为【二手】 |
| **向日葵（Oray）** | 报道称有 MCP 服务（16.5 版本"全功能 MCP"）和 CLI（npm 包 `@aweray/awesun-cli`），可接 Cursor、Claude Code、Codex；被控端无需改动；用视觉方式操作，轨迹屏幕可见，人可随时介入；接入腾讯 WorkBuddy，6 大功能域（设备管理、远程桌面控制、远程文件管理、远程命令执行、端口转发、桌面自动化）；内置 AI 助手 OrayClaw；有"AI 智能远控审计"（分析会话、标记敏感操作、生成时间线报告）。官网首页只看到 OrayClaw、向日葵 CLI（"AI 远程"）和 AI 审计的名字，没有 MCP 文档原文。发布时间说法不一（3 月 / 6 月） | 官网首页 OrayClaw、CLI、AI 审计字样【原文】；其余【二手】；MCP 工具清单和授权模型【未核实】 |
| **ToDesk** | 自带 Agent：ToDesk AI（原 ToClaw，基于 OpenClaw），多设备调度、@设备下发任务；下载站和报道称有端到端加密、权限管控、操作审计、步骤卡片可暂停/回滚/终止。方向是"自带 Agent"，不是给第三方 Agent 开 MCP。没有读到官网原文 | 【二手】；授权和二次确认机制【未核实】 |
| **Splashtop** | 没有查到官方"让 Agent 操作远程电脑"的产品。能查到：2026-04-13 统一 IT 平台（自主端点管理，AI 用于补丁和漏洞摘要）；2026-03-23 与 SysAid 合作，SysAid 的 Copilot 可在工单里启动 Splashtop 远程会话；Secure Workspace 营销页称可作为 MCP 服务的零信任网关。viaSocket 有第三方的 Splashtop MCP | 搜索结果摘要【二手】；官方 MCP【未核实】（没查到） |
| **AnyDesk** | 没有查到官方 MCP。有第三方（Truto）提供读设备列表和系统状态的 MCP；AnyDesk 的 ai-info 页抓取 403，未读到 | 【二手】；官方【未核实】 |
| **RustDesk**（开源） | 官网首页没有提 Agent 或 MCP。社区项目：一个通过 rustdesk-api 的 Python MCP；一个在 macOS 上驱动本机 RustDesk 客户端的 MCP（据列表称 21 个工具：会话、截图、输入注入、设备簿、诊断，密码按次传入不落盘）。均非官方 | 官网【原文】；社区项目【二手】，活跃度和安全性【未核实】 |
| **QuickDesk**（开源，非上面任何一家） | MIT，基于 Chromium Remoting + WebRTC，自带 MCP 服务（Rust 桥，stdio 或 HTTP/SSE），"20+ 工具"：截图（可缩放）、屏幕尺寸、鼠标点击/拖动/滚动、键盘输入/热键/按下/松开、剪贴板读写、`wait_for_event` / `wait_for_connection_state` / `wait_for_clipboard_change`，另有 MCP resources（设备状态）和 9 个提示模板。无逐操作批准；靠"人随时介入"和 9 位设备号 + 临时访问码。可自托管信令和 TURN。Windows x64、macOS ARM64，Linux 计划中 | GitHub 页面（经抓取摘要）【原文】；成熟度【未核实】 |

小结：
- 官方、完整（截图+键鼠+监控面板+接管）：只有 Jump，向日葵可能也是（待核实）。
- 只管理、不含会话内操作：TeamViewer。
- 给人看 Agent、没有 Agent 接口：Astropad。
- 开源方案：QuickDesk 和 RustDesk 社区项目，都没有逐操作批准和审计。
- **没有查到任何一家做了"按风险分层、需要人批准、批准推手机"的服务端强制机制**（ToDesk 的"可暂停/回滚"是二手，向日葵 AI 审计是事后分析）。这是空白，也是我们的机会。【未核实：只是"没查到"】

---

## 5. 对我们的建议

### 5.1 工具清单草案

命名：小写加下划线，加前缀 `peerdesk_`（规范建议聚合客户端时用前缀防冲突）。所有工具都带 `title`、`annotations`、`outputSchema`。坐标一律用"最近一次截图的像素"，截图返回里带 `scale`、`width`、`height`、`display_id`、`screenshot_id`；操作工具可传 `screenshot_id`，如果画面已变，服务端拒绝并要求重新截图（防止对着旧图乱点）。

| 风险层 | 含义 | 批准策略 |
|---|---|---|
| L0 只读-元数据 | 不接触屏幕内容，不改变任何东西 | 令牌有效即可 |
| L1 只读-屏幕 | 能看到屏幕内容（可能含敏感信息，且内容会发给模型厂商） | 首次对该设备授权一次；会话内有效；通知设备主人 |
| L2 低风险操作 | 改变屏幕状态但可恢复：移动、滚动、悬停、截图区域放大 | 设备已授权"允许操作"即可，记录审计 |
| L3 输入操作 | 点击、输入、按键、快捷键：可能触发有后果的动作 | 默认按"会话"批准；见 5.2 的升级规则 |
| L4 高后果 | 剪贴板写入、文件传输、命令执行类、修改系统/权限、由屏幕内容判断出的"付款/删除/发送/授权弹窗" | 每次都要人批准（手机） |

草案工具：

| 工具 | 参数 | 返回 | 层 | annotations |
|---|---|---|---|---|
| `peerdesk_list_devices` | `filter?`（online/offline/tag）、`cursor?` | 设备数组：`device_id`、名称、系统、在线、最近在线、被控端版本、权限状态（屏幕录制、辅助功能、登录界面是否可用）、是否有人在接管 | L0 | readOnly=true, openWorld=false |
| `peerdesk_get_device_health` | `device_id` | CPU、内存、磁盘、电池、网络（延迟、丢包）、锁屏状态、是否无显示器、运行时长、传输路径（直连/中继） | L0 | readOnly=true, openWorld=false |
| `peerdesk_get_session_status` | `device_id?` | 当前会话、批准状态、剩余有效期、已用额度 | L0 | readOnly=true |
| `peerdesk_screenshot` | `device_id`、`display_id?`、`max_edge?`（默认 1280，上限对齐模型限制） | 图片 + `screenshot_id`、`scale`、尺寸、光标位置、是否锁屏、可见的权限弹窗提示 | L1 | readOnly=true, openWorld=true |
| `peerdesk_zoom` | `device_id`、`region`[x0,y0,x1,y1] | 该区域全分辨率图 | L1 | readOnly=true |
| `peerdesk_mouse_move` | `device_id`、`coordinate`、`screenshot_id?` | 光标新位置 | L2 | readOnly=false, destructive=false, idempotent=true |
| `peerdesk_scroll` | `device_id`、`scroll_direction`、`scroll_amount`、`coordinate?` | ok | L2 | destructive=false |
| `peerdesk_click` | `device_id`、`button`(left/right/middle)、`click_count`(1-3)、`coordinate?`、`modifiers?`、`screenshot_id?` | ok、点击后光标位置 | L3 | destructive=true |
| `peerdesk_drag` | `device_id`、`start_coordinate`、`coordinate`、`modifiers?` | ok | L3 | destructive=true |
| `peerdesk_type_text` | `device_id`、`text`（长度上限） | ok、实际输入字数 | L3 | destructive=true |
| `peerdesk_press_key` | `device_id`、`key`（如 "ctrl+s"）、`repeat?`(1-100) | ok | L3（含系统级快捷键如 alt+F4、cmd+Q、ctrl+alt+del 时升 L4） | destructive=true |
| `peerdesk_wait` | `duration`(秒，上限 30) | ok | L0 | readOnly=true |
| `peerdesk_set_clipboard` | `device_id`、`text` | ok | L4 | destructive=true |
| `peerdesk_get_clipboard` | `device_id` | 文本（可能含密码，标明来源不可信） | L1 | readOnly=true |
| `peerdesk_request_human` | `device_id`、`reason`、`kind`(login/approval/takeover) | 人完成后返回；登录的凭据不返回给 Agent | 控制流 | readOnly=true |

对齐 computer use 的地方：动作名和参数名对应（`left_click` → `click`+`button`，`key` 的 `repeat` 上限 100，`wait` 上限等）。我们加了 `screenshot_id` 防旧图，加了 `device_id`（多设备）。`hold_key`、`left_mouse_down/up`、`triple_click` 先不做；`click_count` 已覆盖双击、三击。

第一阶段（只读）只做 L0：`list_devices`、`get_device_health`、`get_session_status`（以及 `wait`）。截图属于 L1，放到操作阶段之前单独发，不要夹带。

### 5.2 权限模型

1. **三个主体**：设备主人（人）、Agent 令牌、被控端。令牌属于账户（符合"计费挂账户不挂设备指纹"），不是设备。
2. **令牌带 scope**：`devices:read`、`screen:read`、`input:basic`（L2）、`input:full`（L3）、`high_risk`（L4 只能请求批准，不能静默通过）。令牌还限定设备集合（设备组/标签）。这样做到"只读令牌连操作工具都看不到"（对应 draft 规范的"按授权返回不同工具"）。
3. **设备侧开关**：每台设备本机有开关，默认关闭"允许 Agent 看屏幕"和"允许 Agent 操作"，必须在被控端本地（或已登录的主人账号）打开，不能只靠云端令牌。无人值守的 Mac mini 可预先授权，但可设时间窗和自动过期。
4. **批准由服务端强制**：策略表（工具 + 参数 + 屏幕上下文 → 风险层）放在被控端/服务端，不信任客户端传来的 annotations。需要批准时，工具调用在服务端挂起，返回给 Agent 的是"等待批准"状态（或 MCP draft 的 `input_required`），批准结果由主人在手机或面板上给出。超时=拒绝，并告诉 Agent 原因。
5. **批准粒度**：一次性（单次动作）/ 本会话 / 限时（例如 30 分钟），L4 只允许一次性。批准界面展示：哪个 Agent、哪台设备、要做什么、**触发批准时的屏幕截图**。
6. **升级规则**（按结果判断，不只按工具名）：输入或点击前后屏幕出现付款、删除、系统权限、终端 sudo、登录/密码框等 → 升 L4 或强制 `request_human`。规则先做简单可测的（窗口标题/应用名/快捷键黑名单），OCR/视觉判断后做，【我们的判断，未验证】。
7. **密码**：Agent 永远不给密码；`request_human(kind=login)` 让人在手机或面板上输入，值只在被控端内存里使用，不进审计、不进模型。这点学 Jump。
8. **提示注入**：屏幕和剪贴板内容在返回里标明"不可信内容"；在 `description` 里写明；高风险动作不管模型怎么说都要批准。
9. **限速和上限**：每令牌每秒动作数、每会话最大动作数和最长时间、截图频率，MCP 规范要求服务端限速。
10. **撤销**：一键断开所有 Agent 会话（急停），手机上也能点；被控端本地也有急停（托盘/菜单栏图标，随时可见）。
11. **诚实**：没有屏幕录制/辅助功能权限、锁屏、设备睡眠、被人接管时，工具返回明确原因和办法，不静默降级（CLAUDE.md 约定）。

### 5.3 监控面板最小版本

目标：一眼看清"哪台在线、Agent 在干什么、有没有卡在等我"。

1. 设备卡片墙：名称、系统、在线、健康（CPU/内存/磁盘）、传输路径（直连/中继）、最近一张缩略图（L1 授权时才显示，且低频刷新）、当前 Agent 和会话状态。
2. "等我处理"置顶：待批准、等登录、被控机权限缺失（对应 Jump 把等登录会话排前）。
3. 每台设备一个动作时间线：时间、Agent、工具、参数摘要、风险层、结果、批准人；点开能看当时的截图。
4. 按钮：接管（Agent 暂停，人操作，还回时 Agent 收到通知）、暂停 Agent、急停、撤销令牌。
5. 审计日志：追加写，每条带上一条的哈希（防篡改）；可导出（JSON Lines）。记录的是"谁、哪个令牌、哪台设备、什么动作、风险层、批准结果、时间"，**不记录输入的明文文字和密码**（type_text 只记长度和哈希，除非主人选择记录）。
6. 先做云端 Web 面板（多机汇总），不是每台 Agent 机上一个本地面板；自托管版同一份代码。手机端先做 Web + 推送，不急着做原生 app。

不做（第一版）：录像回放、AI 审计报告、团队多角色。

### 5.4 和 Jump 比，我们能做得更好的地方

| 方向 | Jump 现状（据文档） | 我们的做法 |
|---|---|---|
| 批准 | 没有逐操作批准；靠人盯面板 | 服务端强制的风险分层，高风险必须批准；批准带截图推送到手机，超时即拒绝 |
| 审计 | Activity 列表；团队日志层面不清楚是否含 Agent 动作 | 每个动作进审计，哈希链，可导出，自托管可自己存 |
| 面板位置 | 本机网页，每台跑 Agent 的机器一个 | 云端或自托管的多机汇总面板，手机可看 |
| 系统 | 被控 Linux 是 Beta；Mac 端要买 | Windows、Linux、Mac 同等支持，Linux 不当二等公民（需要实测） |
| 开源和自托管 | 闭源，必须 Jump 账号和云 | Apache-2.0，可自托管；Agent 工具和被控端代码可审 |
| 传输和画质 | 无公开指标 | 同一条实时层既给 Agent 截图也给人接管；公开延迟/卡顿/清晰度指标，静止后 0.5 秒内变清晰（文字不糊），对 Agent 的 OCR/识别准确率也有好处【假设，需实测】 |
| 无人值守 | 主机不能睡眠；Sleep Override 要 Enterprise | 无显示器设备管理（虚拟显示器、登录界面、权限弹窗、重启恢复）放进基础功能，不收 Enterprise 价 |
| 成本和计费 | 8 台免费，虚拟显示器多块要 Enterprise | 计费挂账户；免费额度和中继流量成本要先算清（直连成功率决定成本） |

### 5.5 最值得做的差异化 3 条

1. **服务端强制的分层批准 + 手机推送**：市场上没有查到同类做法；Jump 把安全交给"人盯着"，MCP 规范也只写了 SHOULD。这是对"在自己电脑上常驻跑 Agent 的人"最实在的价值：人不用盯着也敢让 Agent 动手。
2. **多设备云端/自托管面板 + 防篡改审计**：单机本地面板解决不了"我有 3 台 Mac mini + 1 台 Windows + 1 台 Linux"。
3. **传输质量和无人值守**：把我们的核心优势（实时层、静止后变清晰、无显示器 Mac mini 的处理）用到 Agent 的截图和接管上，并公开指标。

### 5.6 建议的先后顺序

1. 本机 MCP 只读（L0）：`list_devices`、`get_device_health`、`get_session_status`。同时定下令牌 scope、审计日志格式（这是后面一切的地基，先定）。
2. 截图（L1）+ 设备侧授权开关。
3. 操作工具（L2/L3）+ 服务端策略表 + 手机批准（L4）+ `request_human`。
4. 多设备面板、接管、审计导出。

门槛（建议，写进 e2e）：被批准前，被控机上**零个**输入事件（测试里用真实被控端数输入事件）；批准超时返回明确原因；撤销令牌后 1 秒内操作被拒；审计日志与实际操作条数一致。

---

## 6. 没查到的地方

- Jump 的 MCP 工具名、参数、返回格式（官方 MCP 页只有能力描述）。需要装 `jump-cli` 实测 `tools/list` 或 `--help-ai`。
- Jump Agent 功能的上线日期和版本（whats-new 路径 404，docs 首页无带日期条目）。
- Jump 是否有"不走云"的 Agent 模式；Mission Control 的 Activity 在会话结束后是否保留；团队审计日志是否记录 Agent 的每个动作。
- Jump 博客（jumpdesktop.com 上的博客/发布说明）没有找到专门的 Agent 文章；搜索未返回。
- TeamViewer MCP 的完整工具清单（在 PDF 中，未读）。
- 向日葵的官方 MCP/CLI 文档原文（只读到官网首页的名字和二手报道）；ToDesk 官网原文；Splashtop、AnyDesk 官方是否有 Agent 接入（没查到，不等于没有）。
- RustDesk 官方没有；社区 MCP 项目的活跃度和安全性没核实。
- Anthropic computer use 页面的后 16K 字符之外的部分已读；"earlier tool versions" 表里每个旧版本的参数细节没单独核对。
- 手机批准推送的客户端支持度（MCP elicitation 在各 Agent 客户端里的实现情况）没查。
- 我们的"升级规则"能不能可靠地从屏幕判断高风险，没有数据，需要实验。

---

## 7. 打开过的网址（查看日期 2026-11-28）

Jump Desktop
- https://jumpdesktop.com （首页）
- https://jumpdesktop.com/ai-agents.html
- https://jumpdesktop.com/pricing-plans.html
- https://docs.jumpdesktop.com/viewer/workflows/ai-agents/
- https://docs.jumpdesktop.com/viewer/workflows/check-in/
- https://docs.jumpdesktop.com/cli/mcp/
- https://docs.jumpdesktop.com/cli/reference/
- https://docs.jumpdesktop.com/ （首页，查发布说明）
- https://docs.jumpdesktop.com/whats-new/ （404）

标准和 Anthropic
- https://docs.anthropic.com/en/docs/agents-and-tools/tool-use/computer-use-tool （301 跳转）
- https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool （读了前 100000 字符和 100000 之后部分）
- https://modelcontextprotocol.io/specification/2025-06-18/server/tools
- https://modelcontextprotocol.io/specification/2025-06-18/schema
- https://modelcontextprotocol.io/specification/draft/server/tools

其他产品
- https://www.teamviewer.com/en/global/support/knowledge-base/teamviewer-remote/teamviewer-ai/teamviewer-mcp-interface/
- https://www.teamviewer.com/en/insights/introducing-teamviewer-model-context-protocol-integration/
- https://astropad.com/product/workbench/
- https://techcrunch.com/2026/04/08/astropads-workbench-reimagines-remote-desktop-for-ai-agents-not-it-support/
- https://github.com/acardozzo/quickdesk
- https://sunlogin.oray.com/
- https://rustdesk.com/
- https://anydesk.com/en/ai-info （403，未读到）
- https://www.ithome.com/0/977/985.htm （404）

仅通过搜索结果摘要看到、没有打开全文（所以都按【二手】）：Splashtop 博客和新闻稿（enterprise-agentic-ai-deployments、unified-platform-for-modern-it-announcement、SysAid 合作）、viaSocket 的 Splashtop MCP、Truto 的 AnyDesk MCP、Glama 上的 RustDesk MCP 项目、MacStories / MacRumors / AppleInsider 对 Workbench 的报道、CSDN / B站 / IT之家 / 站长类网站对向日葵和 ToDesk 的报道。
