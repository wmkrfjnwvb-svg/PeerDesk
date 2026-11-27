# 电脑操作 AI Agent 的技术路线：可靠性、延迟、成本（截至 2026-11-26）

说明：
- 每条数字后标注来源和日期。"已读原文"表示本次直接读了该页面/论文正文；"搜索摘要"表示只看到搜索结果摘要或二手转述，没有读到原文，属于未核实。
- 价格均为美元、标准档（非批处理、非加速），读取日期 2026-11-26，除非另注。
- "推断"部分是根据引用数据做的计算或判断，不是来源原话。

## 一、可靠性：纯截图 GUI agent、API/工具 agent、混合（无障碍树/工具 + 截图）的成功率对比和典型失败

### Takeaway
2026 年前沿模型在 OSWorld-Verified 上纯 GUI 已到 78%–86%（厂商自报，超过人类基线 72.36%），但在更长、更难的任务（OSWorld2 子集）上 Claude 5 代只有 39%–52%、GPT-6 Astra 75%，开放权重模型 2%–29%。多篇对照研究一致表明：混合（能用 API/MCP/shell 时用，否则退回 GUI）成功率最高、步数最少；纯 API/MCP 的瓶颈是覆盖不全，纯 GUI 的瓶颈是长任务和超时。

### Cited Findings
**总榜（OSWorld-Verified，各方说法不一致）**
- 第三方汇总榜 Steel.dev：Qwen3.8-Max 86.1%（2026-08-03）、Claude Mythos 5 85.0%、Claude Fable 5 85.0%、Claude Opus 4.8 83.4%（2026-05-28）、小米 MiMo-V2.6-Pro 82.0%（开放权重，2026-09-22）、Claude Sonnet 5 81.2%、MiMo-V2.6-Flash 80.8%（开放权重）、Claude Sonnet 4.6 78.5%、GPT-5.4 75.0%（2026-03-05）、Claude Opus 4.6 72.7%、Kimi K2.5 63.3%（开放权重）、GLM-5V-Turbo 62.3%、Claude Haiku 4.5 50.7%、OpenAI Operator 32.6%（2025-01）、Qwen3-VL-235B-A22B 31.6%；页面自己警告各行的框架、步数上限、工具权限不同，不可直接比较；人类基线 72.36% — [Steel.dev OSWorld Leaderboard](https://leaderboard.steel.dev/leaderboards/osworld/)（已读原文，汇总自厂商公告）
- Yutori 榜单称 claude-fable-5 86.0%、Yutori Navigator n2 85.3%、claude-opus-5 83.4%；Presenc.ai 称官方榜第一是 Intelligence-Indeed Agent 90.19% — [Yutori](https://yutori.com/leaderboards/osworld-verified)、[Presenc.ai](https://presenc.ai/research/osworld-computer-use-leaderboard-2026)（搜索摘要，未核实；与 Steel 榜互相矛盾）
- 官方 OSWorld 站点的 Verified 榜数据由前端脚本加载，本次没读到表格；站点注明 OSWorld 2.0 已发布并推荐使用；原论文人类成功率 72.36% — [OSWorld](http://osworld-v1.xlang.ai/)（已读原文）
- Mage-VL-Cowork 模型卡里的对照表（OSWorld-Verified，361 题，不含 Google Drive 题）：GPT-5.5 78.7、Claude Opus 4.7 78.0、GPT-5.4 75.0、Kimi-K2.6 73.1、人类 72.4、ScaleCUA-Qwen3.5-9B 68.7、Qwen3.6-27B 63.9、EvoCUA-32B 56.7 — [Mage-VL-Cowork 模型卡](https://huggingface.co/microsoft/Mage-VL-Cowork)（已读原文，2026）

**更难的长任务：差距仍然很大**
- cua-speedrun（CMU 等，2026-09-30）OSWorld2 52 题子集：Claude Opus 5（low）52.1%、Claude Sonnet 5（low）39.1%、GPT-6 Astra（high）75.0%、Kimi K3（max）28.5%、GLM-5V Turbo 1.5%；OSWorld 50 题子集上同类模型 75.8%–91.6%。注意该论文报的是"带部分得分的平均验证分"，不是严格成功率 — [cua-speedrun, arXiv 2609.40284](https://arxiv.org/html/2609.40284)（已读原文）
- BenchLeader 称 Claude Opus 5 以 31.4% 领先 OSWorld-Verified 2.0 — [BenchLeader](https://www.benchleader.com/benchmarks/osworld_2)（搜索摘要，未核实）

**GUI vs API vs 混合的对照实验**
- MCPWorld（201 题、10 个应用，Claude 3.7 Sonnet 的 Computer Use 框架）：任务成功率 GUI-Only 70.65%、MCP-Only 53.23%、Hybrid 75.12%；关键步骤完成率 68.82% / 59.78% / 69.63% — [MCPWorld, arXiv 2506.07672](https://arxiv.org/pdf/2506.07672)（已读原文，2025-06）
- MCPWorld 按 GUI 步数分档：0–5 步 GUI 90.41% / MCP 63.01% / 混合 90.41%；5–10 步 72.29% / 51.81% / 74.70%；10 步以上 35.56% / 40.00% / 51.11%。任务变复杂时 GUI-Only 成功率下降 54.85%，MCP-Only 和混合只下降 23.01% 和 39.30%；5–10 步档加了 MCP 反而略降，作者推测是 MCP 工具描述过长干扰 — 同上
- UFO2（微软，Windows）：同样 27 个 OSWorld 办公任务，GUI-only 16.3% → GUI+API 22.4%（GPT-4o）、16.3% → 24.5%（o1）；平均完成步数 GPT-4o 13.8→12.9，o1 16.0→6.6（省 58.5%）。WindowsAgentArena 总成功率 UFO2(o1) 30.5%、OpenAI Operator 20.8%；UFO2+Operator 26.0% — [UFO2, arXiv 2504.14603](https://arxiv.org/html/2504.14603v2)（已读原文，2025-04）
- UFO2 控件检测对照：只用 UIA（Windows 无障碍接口）在 WAA 上 25.3%（o1），只用视觉解析 OmniParser-v2 20.8%，两者融合 27.9%；OSWorld-W 上 24.5% / 14.3% / 28.6% — 同上
- UFO2 多动作推测执行：OSWorld-W 平均步数最多减少 51.5%，成功率不降反升（GPT-4o 22.4%→24.5%） — 同上
- OSWorld-MCP（阿里，ICLR 2026）：加入 MCP 工具后 o3 在 15 步上限下 8.3% → 20.4%（GitHub README 写 17.6%，版本不一）；Claude 4 Sonnet 在 50 步下 40.1% → 43.3%；需要 4 轮工具调用的任务，Claude 4 Sonnet 纯 GUI 0%、加 MCP 16.7%；最高"工具调用率"只有 33%–36%，即使有工具，模型也常常不用；工具调用率和准确率强正相关 — [OSWorld-MCP, arXiv 2510.24563](https://arxiv.org/html/2510.24563v2)（搜索摘要，数字在不同版本间有出入）
- 微软 "API Agents vs. GUI Agents: Divergence and Convergence"：GUI agent 需要多步模仿用户操作，更慢、更易错；在 UFO 里做了 GUI+API 混合原型，办公任务成功率提高、平均步数减少 — [arXiv 2503.11069](https://arxiv.org/abs/2503.11069)（搜索摘要）
- AXIS（API 优先的 agent，Word 上实验）：完成时间减少 65%–70%，认知负担减少 38%–53%，准确率达人类的 97%–98% — [AXIS, arXiv 2409.17140](https://arxiv.org/abs/2409.17140)（已读摘要）；搜索摘要另称 50 个任务上 UI agent 平均 59.5 秒、成功 52.0%，AXIS 29.9 秒、84.0%（未核实，未读到正文）
- 小模型也走混合：Mage-VL-Cowork（4B）的动作空间是 `computer_use`（GUI）+ `bash`（shell）两个工具 — [Mage-VL-Cowork](https://huggingface.co/microsoft/Mage-VL-Cowork)（已读原文）
- 微软 MagenticLite（2026-05-21）：浏览器任务交给 Fara1.5 走"截图→动作"循环，文件和代码任务由编排模型 MagenticBrain（14B）直接调工具（bash、edit、search、open） — [MagenticLite 博客](https://microsoft.com/en-us/research/blog/magenticlite-magenticbrain-fara1-5-an-agentic-experience-optimized-for-small-models)（已读原文）
- WindowsAgentArena 原始结果：最佳配置（Set-of-Marks + 系统无障碍树 + 像素检测）19.5%，人类 74.5%；154 题；本次没找到 2026 年的 WAA 新纪录 — [WAA, arXiv 2409.08264](https://arxiv.org/abs/2409.08264)（搜索摘要）

**典型失败方式**
- MCPWorld 失败原因占比：GUI-Only = 推理能力不足 72.88%、超时 18.64%、光标定位不准 5.08%、找不到控件 1.69%；MCP-Only = MCP 覆盖不足 48.42%、推理不足 47.37%、API 不兼容 3.16%；Hybrid = 推理不足 85.41%、MCP 覆盖不足 4.17% — [MCPWorld](https://arxiv.org/pdf/2506.07672)（已读原文）
- UFO2-base（GPT-4o）在 WAA 上超过 62% 的失败是控件检测失败；OSWorld-W 上以计划错误为主 — [UFO2](https://arxiv.org/html/2504.14603v2)（已读原文）
- Anthropic 官方文档列出的问题：模型有时不检查结果就当作已成功；下拉框、滚动条难用鼠标操作（建议用快捷键）；截图缩放失真、长宽比变形、4K 源图缩小导致点偏；屏幕内容中的指令会被执行（提示注入）；单次请求超过 20 张图后每张图的尺寸限制变严 — [Claude computer use 文档](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)（已读原文）
- OSWorld-Human：最好的 agent 也比人类必需步数多走 1.4–2.7 倍（v1），后续版本写 2.7–4.3 倍 — [OSWorld-Human, arXiv 2506.16042](https://arxiv.org/html/2506.16042v1)（已读原文 v1；后续版本数字为搜索摘要）

### Inferences
- 对"整理桌面文件、发合同"这类日常任务，结构化工具路线可靠性取决于工具覆盖面；GUI 是兜底。业内三组独立数据（MCPWorld、UFO2、OSWorld-MCP）方向一致：混合 > 纯 GUI > 纯 API（覆盖不全时）。
- 纯 GUI 在短任务（≤5 步）上已经很高（MCPWorld 90%），掉点主要在长任务和超时；这正好是"手机上发一句话让电脑做事"最常见的长尾。
- 工具多不等于会用：OSWorld-MCP 工具调用率只有三成多，MCPWorld 发现工具描述过长会拉低中等任务成功率。PeerDesk 的工具集应少而精，描述短。
- 无障碍树（UIA/AX）和视觉融合优于任何一种单独使用（UFO2），PeerDesk 在 Windows 上应同时取 UIA 控件信息和截图，而不是二选一。

### Gaps
- 没读到官方 OSWorld-Verified 榜的实时表格（前端加载），排名第一是谁各方说法矛盾（86.1% / 86.0% / 90.19%），均为厂商自报或第三方汇总。
- 没找到 2026 年 WindowsAgentArena 的新成绩，也没找到 macOS 专门基准的数据；Windows 上前沿模型纯 GUI 的真实成功率不明。
- 没有找到同一前沿模型（2026 年款）在"同一批真实日常任务"上 GUI vs 工具 vs 混合的直接对照；现有对照多是 2025 年模型。

## 二、延迟：GUI agent 每步/每任务秒数、与工具调用的对比、截图分辨率的影响

### Takeaway
2026 年前沿模型配合快速环境，OSWorld 短任务每题约 1.2–2.5 分钟、每轮"截图→模型→动作"约 6–7 秒；长任务（OSWorld2）要 15–30 分钟以上。时间主要花在模型推理上（约 95%），截图和执行动作本身可以压到几十毫秒。

### Cited Findings
- cua-speedrun（2026-09-30，1920×1080 截图）OSWorld 50 题均值：Claude Opus 5（low）85.9 秒/题、14.04 轮；Claude Opus 5（high）136.0 秒、19.92 轮；Claude Sonnet 5（high）140.8 秒、20.24 轮；GPT-6 Astra（low）约 90 秒、平均 5.9–8.2 轮；GPT-5.6 Luna（low，直接 API）74 秒；Kimi K3（max）443.7 秒；GLM-5V Turbo（关思考）752.4 秒 — [cua-speedrun](https://arxiv.org/html/2609.40284)（已读原文）
- 同上，OSWorld2（长任务）：Claude Opus 5（low）1,385.7 秒/题、146.5 轮；Claude Sonnet 5（low）1,993.6 秒；GPT-6 Astra（high）914 秒；Kimi K3（max）4,825.1 秒 — 同上
- 同上，环境开销：标准 OSWorld 点击动作中位延迟 2,515 ms、输入 100 字符 3,522 ms；作者的 Fast I/O 环境点击 27.6 ms、输入 15 ms；"命令到截图"中位 38.5 ms（点击）。用 Fast I/O 后环境时间从每题 12.68 秒降到 0.34 秒，但总时长反而 89.5 → 99.0 秒（作者称"更快的环境 I/O 可能让整体变慢"）；Fast I/O 下 agent（模型）时间约占 95% — 同上
- 同上，思考强度：对部分模型，提高推理强度反而更快完成（Gemini 3 Flash Preview low 492 秒/33.6% vs medium 268 秒/57.6%，low 调用模型 56 次 vs medium 32.5 次）；强制每次只输出一个动作，GPT-6 Astra（xhigh）从 127 秒增到 183 秒 — 同上
- OSWorld-Human（2025-06，Agent S2 + GPT-4.1 规划 + UI-TARS-7B 定位）：计划和反思占任务时间 75%–94%，定位 3.1%–6.1%，截图 1.0%–3.6%，执行动作 0.9%–2.3%；一个 50 步任务超过 40 分钟；一个调行距任务 agent 用 12 分钟，人不到 30 秒；越往后每步越慢，后期一步可达前期 3 倍（因为提示里累积了全部历史） — [OSWorld-Human](https://arxiv.org/html/2506.16042v1)（已读原文）
- 同上：生成无障碍树每步需 3–26 秒（视应用而定），并增加数千 token；加上无障碍树后部分应用步数反而暴涨（LibreOffice Calc 14→50 步、Writer 21→50 步）；Set-of-Marks 标注几乎无额外开销 — 同上
- 小模型：Mage-VL-Cowork（4B）演示中 Calc 任务 6 步 39 秒，Qwen3.6-27B 同任务 39 步 9 分 56 秒；GIMP 任务 15 步 1 分 58 秒 vs 29 步 7 分 2 秒（硬件未说明）；每题平均输出 5,499 token — [Mage-VL-Cowork](https://huggingface.co/microsoft/Mage-VL-Cowork)（已读原文）
- UFO2 推测式多动作（一次调用输出多个动作）在 OSWorld-W 上平均步数最多减少 51.5% — [UFO2](https://arxiv.org/html/2504.14603v2)（已读原文）
- Anthropic 官方：批量动作可省去往返；建议桌面任务用 1024×768 或 1280×720，网页用 1280×800 或 1366×768，避免超过 1920×1080；超限的截图 tool_result 会被直接拒绝（不自动缩放）；把文字说明放在截图前面可提高点击准确率 — [Claude computer use 文档](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)（已读原文）
- Anthropic 官方：图片越大 token 越多、延迟越高，建议上传前先缩放 — [Claude vision 文档](https://platform.claude.com/docs/en/build-with-claude/vision.md)（已读原文）
- AXIS（API 优先）比 UI 操作省 65%–70% 完成时间 — [AXIS](https://arxiv.org/abs/2409.17140)（已读摘要）

### Inferences
- 由 cua-speedrun 表格折算（推断）：Claude Opus 5 low 每轮约 6.1 秒（85.9/14.04）、Sonnet 5 high 约 7.0 秒（140.8/20.24）、Kimi K3 max 约 38 秒、GLM-5V 约 45 秒。即前沿云模型每步 6–7 秒量级，开放权重大模型慢 5–7 倍。
- 一个结构化工具调用（如"按日期搜文件"）只需 1–3 轮模型调用、无截图，按每轮数秒计，总时长应在 5–20 秒；同一任务走 GUI（打开资源管理器、筛选、拖拽附件）按 10–20 轮计要 1–2 分钟以上。这是推断，没有找到同一任务两条路线的实测对比。
- PeerDesk 的采集和输入注入已是毫秒级，环境不是瓶颈；优化重点是减少轮数（批量动作、混合工具）和每轮 token（截图降到 720p 左右、只留最近几张）。
- 手机端聊天体验：GUI 路线下用户要等分钟级，必须显示进度（当前步骤、截图缩略），并允许中途接管——PeerDesk 的远程画面本身就是现成的"看 agent 干活"窗口。

### Gaps
- 没找到截图分辨率对每步延迟和成功率影响的系统实验（cua-speedrun 固定 1920×1080）。
- 没找到 Claude/GPT/Gemini 在带一张截图时的首 token 延迟（TTFT）官方或第三方实测。
- 没找到 Windows Search / Spotlight 查询延迟的公开测量。

## 三、成本：每张截图 token、单步/单任务费用、2026-11 官方价格、缓存效果

### Takeaway
1080p 截图在 Claude 高分辨率档约 2,691 token、GPT-5.6 约 2,448 token、Gemini 默认 1,120 token、Qwen3-VL 约 2,000 token；按 2026-11 价格，一张截图从约 $0.0002（Haiku 5.5 / GPT-6 Luna）到约 $0.02（Fable 5.2）。实测 OSWorld 短任务每题约 $0.01–$0.71，长任务每题约 $2–$15；结构化工具路线做同类简单任务应便宜 1–2 个数量级。

### Cited Findings
**截图 token 计算（官方文档）**
- Claude：每 28×28 像素一个视觉 token，`⌈宽/28⌉×⌈高/28⌉`；Claude 4.7 及以后为高分辨率档（长边 2,576 px、上限 4,784 token），其他为标准档（长边 1,568 px、上限 1,568 token）。1920×1080：标准档缩到 1456×819 = 1,560 token，高分辨率档不缩 = 2,691 token；3840×2160 高分辨率档缩到 2576×1449 = 4,784 token。官方举例：Opus 5.5 下 1000×1000 图约 $5.18/千张，4K 图约 $19.14/千张 — [Claude vision 文档](https://platform.claude.com/docs/en/build-with-claude/vision.md)（已读原文，2026-11-26）
- Claude computer use：长任务里每张截图约 1,000–1,800 输入 token（官方粗估）；`computer_toolset_20260801` 默认成员定义加约 4,500 输入 token（Sonnet 5 约 4,590，其他约 4,520），关掉 zoom 少约 410；旧版 `computer_20251124` 系统提示 466–499 token + 工具定义约 735 token；bash 工具另加 325 token（4.7 及以后），text editor 974 token；工具调用系统提示 Opus 5.5 / Sonnet 5.5 / Haiku 5.5 为 286 token — [Claude 价格页](https://platform.claude.com/docs/en/about-claude/pricing.md)、[computer use 文档](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)（已读原文）
- OpenAI：GPT-5.x 及以后按 32×32 patch 计，`⌈宽/32⌉×⌈高/32⌉×倍率`，gpt-6-astra 和 gpt-5.6-sol/terra/luna 倍率 1.2，high 档上限 2,500 patch；官方例子 1920×1080 在 gpt-5.6-sol high = 2,040 patch × 1.2 = 2,448 token；旧 gpt-4o 同图 1,105 token — [OpenAI images & vision 文档](https://developers.openai.com/api/docs/guides/images-vision)（已读原文）
- Gemini 3 / 4：每张图按 media_resolution 固定计费：low 280、medium 560、high 1,120（默认）、ultra_high 2,240；文档称 ultra_high "computer use 等特定场景需要" — [Gemini media resolution 文档](https://aistudio.google.com/docs/media-resolution)（已读原文）
- Qwen3-VL（及 Qwen3.5–3.8）：每 32×32 像素一个 token，图片 token = `h×w/1024 + 2`；默认 max_pixels 2,621,440（约 2,560 token），高分辨率模式上限 16,384 token；Qwen2.5-VL 为 28×28 — [阿里云百炼 vision 文档](https://www.alibabacloud.com/help/en/model-studio/vision)（已读原文）；第三方计算 1920×1080 = 2,025 token、4K = 8,100 token — [Jacar, 2026-09-04](https://jacar.es/?p=5454)（已读原文）

**2026-11 官方 API 价格（每百万 token，输入 / 输出 / 缓存读取）**
- Anthropic：Fable 5.2 $8 / $40 / $0.20；Opus 5.5 $4 / $20 / $0.20；Opus 5 $5 / $25 / $0.50；Sonnet 5.5 $2 / $10 / $0.10；Sonnet 5 $2 / $10 / $0.20；Haiku 5.5 $0.10 / $0.50 / $0.01（提示 ≤10 万 token；超过则 $0.50 / $2.50）；Haiku 4.5 $1 / $5。5 分钟缓存写入 1.25 倍、1 小时 2 倍；批处理五折；加速模式 Opus 5.5 $8 / $40；新分词器（4.7 及以后）同样文本约多 30% token — [Claude 价格页](https://platform.claude.com/docs/en/about-claude/pricing.md)（已读原文）
- OpenAI（≤27.2 万输入 token）：gpt-6.2-astra $8 / $40（缓存 $0.40）；gpt-6-astra $10 / $50；gpt-6.2-sol、gpt-6.1-sol $2 / $10（缓存 $0.10）；gpt-6.1-terra $1 / $5；gpt-6-luna $0.10 / $0.50（缓存 $0.01）；gpt-5.6-sol $4 / $20；gpt-5.6-luna $0.20 / $1.20；gpt-5.4-mini $0.75 / $4.50 — [OpenAI API 价格](https://developers.openai.com/api/docs/pricing)（已读原文）
- Google：Gemini 4 Carbon / Argon $2 / $10（2026-12-01 起 $4 / $20）；Gemini 4 Neon $3 / $15；Gemini 4 Hydrogen $1.50 / $7.50（只有隐式缓存）；Gemini 3.8 Flash $0.75 / $3.75（2027-01-01 起 $1.50 / $7.50）；Gemini 3.1 Pro Preview $2 / $12（>20 万 token $4 / $18）；Gemini 3.5 Flash-Lite $0.30 / $2.50；本页未见单独的 computer use 模型价格 — [Gemini API 价格](https://aistudio.google.com/docs/pricing)（已读原文，页面后段未读完）
- 阿里 Qwen3-VL-Plus（国际部署）：最低档输入约 $0.143、输出约 $1.434/百万 token，长上下文档更贵；官方另称 Qwen3.5-Plus 多模态"显著超过" Qwen3-VL — [阿里云 qwen3-vl-plus](https://www.alibabacloud.com/help/en/model-studio/qwen3-vl-plus)（搜索摘要，表格有乱码，未核实）
- DeepSeek、智谱官方价格本次未查到（见 Gaps）。

**实测每任务费用**
- cua-speedrun OSWorld 50 题（1920×1080）：Claude Opus 5 high $0.661/题（91.6 分）、low $0.390（87.6）；Claude Sonnet 5 high $0.309（89.6）、low $0.294（75.8）；GPT-6 Astra low $0.71（90.8）；Gemini 3.8 Flash low $0.11（91.6）；GPT-5.6 Luna low 约 $0.01（61.8）；Kimi K3 low $0.174（73.6）；GLM-5V Turbo $0.22（57.8） — [cua-speedrun](https://arxiv.org/html/2609.40284)（已读原文；成本不含环境托管和验证调用）
- 同上 OSWorld2：Claude Opus 5 low $10.0/题、Sonnet 5 low $8.86、GPT-6 Astra high $7.39、Gemini 3.8 Flash medium $2.91；MyPCBench（GPT-6 Astra via Codex）$4.33–$4.81/题；CUA-World $8.42–$15.06/题 — 同上
- 同上：Gemini 3.8 Flash high 比 low 多 5.6 倍 token、慢 2.7 倍；在 OSWorld 上 low 和 medium 同分（91.6%），medium 时间翻倍、成本约翻倍 — 同上

**缓存**
- Claude 缓存读取：Opus 5.5 和 Sonnet 5.5 为基础价 0.05 倍，Fable 5.1/5.2 为 0.025 倍，其他 0.1 倍；5 分钟写入读 1 次即回本 — [Claude 价格页](https://platform.claude.com/docs/en/about-claude/pricing.md)（已读原文）
- Anthropic 建议按批次而不是每轮裁剪旧截图，因为改动前缀会让缓存失效 — [computer use 文档](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)（已读原文）

### Inferences
- 单张截图费用（推断，按上面官方公式和价格）：1920×1080 → Opus 5.5 约 $0.0108、Sonnet 5.5 约 $0.0054、Haiku 5.5 约 $0.00027、Fable 5.2 约 $0.0215；GPT-5.6-sol 约 $0.0098；Gemini 3.8 Flash（ultra_high 2,240 token）约 $0.0017。缩到 1280×720 时 Claude 为 46×26 = 1,196 token，费用降到 44%。
- Claude 高分辨率档下 Mac Retina 原生截图（2880×1800）超过长边 2,576 和 4,784 token 上限，必须在客户端缩放，否则 tool_result 被拒；PeerDesk 已有的缩放编码管线可复用。
- 结构化工具路线的简单文件任务（推断）：3 轮调用、每轮约 3k 输入 + 200 输出，Sonnet 5.5 约 $0.024（不计缓存），Haiku 5.5 约 $0.0012；同类任务走 GUI 按 cua-speedrun 实测约 $0.3–$0.7（Claude）。差 1–2 个数量级。
- 托管模式成本模型（推断）：若一个付费用户每天 20 个简单任务全走工具路线，Sonnet 级约 $0.5/天、$15/月；若全走 GUI 约 $6–$14/天，订阅价覆盖不了。托管云必须"工具优先、GUI 兜底"，并按任务计量。
- 低成本 GUI 选项：Gemini 3.8 Flash 在 OSWorld 子集上 $0.11/题、91.6 分，是本次看到的性价比最高的云端 GUI 模型（单篇论文、单次运行、2027 年起涨价一倍）。

### Gaps
- DeepSeek、智谱（GLM-5V）、月之暗面官方价格页本次未读取；Qwen 价格表格有乱码。
- 没有 Claude 5.5 代（Opus 5.5 / Sonnet 5.5 / Haiku 5.5）在 OSWorld 上的每题实测费用和成功率，只有 Claude 5 代数据。
- OpenAI 价格页没有单独列出 computer-use 模型价格；gpt-6.x 的 sol/luna 是否用同样 1.2 倍图片倍率，文档表格未列（只列了 gpt-6-astra 和 gpt-5.6）。

## 四、本地模型：能在普通电脑上跑的视觉/agent 小模型、硬件需求、速度、与前沿的差距、端侧运行时

### Takeaway
2026 年本地小模型进步很大：微软 Mage-VL-Cowork（4B，Apache-2.0）在 OSWorld-Verified 上 66.0%，ScaleCUA-Qwen3.5-9B 68.7%，接近人类基线、落后前沿约 12–20 个点。但在普通笔记本上，8B 级视觉模型处理一张 1080p 截图的预填充就要十几秒，每步延迟比云端前沿模型慢约 1.3–2.7 倍（视截图尺寸）；长任务和开放权重模型都不在性能/速度/成本的帕累托前沿上。

### Cited Findings
**模型和成功率**
- Mage-VL-Cowork（微软，2026）：4B（Qwen3-4B-Instruct-2507 + Mage-ViT），Apache-2.0；OSWorld-Verified 66.0%（两次 65.33% / 66.75%，50 步上限，只保留最近 3 张截图）；输入 1920×1080 截图、坐标归一化到 0–999；动作 = computer_use + bash；bf16 权重 9.48 GB；Hub 上有 12 个量化版本；模型卡称"推进了端侧智能的前沿"，但没写显存/NPU 需求 — [Mage-VL-Cowork](https://huggingface.co/microsoft/Mage-VL-Cowork)（已读原文）
- 同卡对照：ScaleCUA-Qwen3.5-9B 68.7、Muse Glimmer-30B 65.9、Qwen3.6-27B 63.9、EvoCUA-32B 56.7、Fara1.5-4B 13.9（浏览器专用模型做桌面任务） — 同上
- Fara-7B（微软，2025-11-24，MIT，基于 Qwen2.5-VL-7B）：只看截图、预测坐标；WebVoyager 73.5、Online-Mind2Web 34.1；对照 UI-TARS-1.5-7B 66.4 / 31.3、OpenAI computer-use-preview 70.9 / 42.9、GPT-5 SoM agent 90.6 / 57.7；未报 OSWorld；官方测试硬件为 A6000/A100/H100 + vLLM bf16 — [Fara-7B 模型卡](https://huggingface.co/microsoft/Fara-7B)（已读原文）
- Fara1.5（2026-07-22，4B / 9B / 27B，基于 Qwen 3.5，MIT，浏览器专用）：Online-Mind2Web 约 57.3% / 63.4% / 72.3%（README）；微软博客称 27B 超过 90%，两处数字矛盾 — [MagenticLite 博客](https://microsoft.com/en-us/research/blog/magenticlite-magenticbrain-fara1-5-an-agentic-experience-optimized-for-small-models)（已读原文）、[Fara README](https://raw.githubusercontent.com/microsoft/fara/HEAD/README.md)（搜索摘要）
- OpenCUA-72B OSWorld-Verified 45.0%；OpenCUA-7B 分数本次未读到 — [OpenCUA-7B](https://huggingface.co/xlangai/OpenCUA-7B)（搜索摘要）
- 开放权重大模型：小米 MiMo-V2.6-Pro 82.0%、MiMo-V2.6-Flash 80.8%（2026-09，规模未核实，不一定能在普通电脑上跑） — [Steel.dev](https://leaderboard.steel.dev/leaderboards/osworld/)（已读原文，汇总）
- cua-speedrun：评测的开放权重模型（Kimi K3、MiniMax M3、GLM-5V Turbo）在任何基准上都不在"性能–时间"或"性能–成本"帕累托前沿；Kimi K3（max）追平 GPT-6 Astra（high）要多花 4.4 倍时间 — [cua-speedrun](https://arxiv.org/html/2609.40284)（已读原文）

**硬件和速度**
- Qwen3-VL-8B：Q4_K_M 权重 4.68 GiB + 视觉编码器/投影 1.08 GiB（F16）；KV 缓存 144 KiB/token；8K 上下文合计约 6.89 GiB（含 CUDA 开销约 7.5 GiB），32K 约 10.26 GiB；一张 1080p 截图 2,025 token ≈ 285 MiB KV 缓存，四张 4K 截图约 4.4 GiB；Qwen3-VL-4B 下载 3.07 GiB — [Jacar, 2026-09-04](https://jacar.es/?p=5454)（已读原文）
- 同上：作者按 RTX 5060 Ti 16 GB 上 Llama 2 7B 的实测（预填充 4,195 tok/s、生成 93 tok/s）推算 Qwen3-VL-8B 生成约 71 tok/s、一张 1080p 截图预填充约 0.5 秒——这是外推，不是实测 — 同上
- Apple M1 Pro 16 GB、Qwen3-VL-8B 4-bit（oMLX，macOS 26.4，2026-03-09）：1K 上下文预填充 157.8 tok/s、生成 31.6 tok/s、首 token 6.5 秒、峰值内存 6.2 GB；8K 上下文预填充 145.2、生成 22.6 tok/s、7.2 GB — [oMLX 社区测量](https://omlx.ai/benchmarks/q6m7miny)（已读原文，社区数据）
- Apple M4 32 GB 同模型 8K 上下文：预填充约 183 tok/s、生成约 9 tok/s、首 token 约 45 秒 — [oMLX 社区测量](https://omlx.ai/benchmarks/il1sgnsw)（搜索摘要，未核实）
- NVIDIA Jetson AGX Thor 上 TensorRT-Edge-LLM 跑 Qwen3-VL-8B 单用户解码约 108–112 tok/s — [TensorRT-Edge-LLM 基准](https://nvidia.github.io/TensorRT-Edge-LLM/latest/user_guide/performance/performance-benchmarks.html)（搜索摘要）

**端侧运行时和系统模型**
- llama.cpp：视觉模型的投影器是单独文件，用 `--mmproj` 加载；编码器和投影器在预填充阶段每张图跑一次 — [Jacar](https://jacar.es/?p=5454)（已读原文）
- Windows ML 在 Ignite 2025 宣布正式可用（GA） — [Thurrott](https://www.thurrott.com/dev/329764/ignite-2025-mcp-comes-to-windows-developers-in-preview)、[Windows 开发者博客](https://blogs.windows.com/windowsdeveloper/2025/11/18/ignite-2025-furthering-windows-as-the-premier-platform-for-developers-governed-by-security)（搜索摘要，未读原文）
- Apple Foundation Models 框架：WWDC26 起端侧模型支持图片输入（任意尺寸，越大越费 token、越慢）、工具调用增强；上下文开发者论坛说 4,096 token（输入输出共享），WWDC26 示例打印 8,192，互相矛盾；参数量第三方称约 3B，Apple 未官方确认；系统升级到 26.4 时模型会变，需重新测试提示 — [Apple 开发者论坛](https://developer.apple.com/forums/thread/833642)、[WWDC26 session 241](https://developer.apple.com/videos/play/wwdc2026/241/)、[DEV 社区](https://dev.to/owen_fox/apples-third-generation-foundation-models-a-developers-read-on-wwdc-2026-2ej1)（搜索摘要，未核实）
- Open Interpreter：本地模式可接 Ollama / LM Studio / llamafile / Jan 的视觉模型；本地模式默认上下文 3,000、max_tokens 1,000，多步 OS 模式需手动调大；OS 模式（`--os`）仍被评为实验性、脆弱 — [Open Interpreter 本地文档](https://docs.openinterpreter.com/guides/running-locally)、[DEV 评测 2026](https://dev.to/jovan_chan_9500711396d4e6/open-interpreter-review-2026-code-interpreter-offline-4g80)（搜索摘要）

### Inferences
- 笔记本本地 GUI agent 每步延迟估算（推断）：M1 Pro 上 Qwen3-VL-8B 预填充 2,025 token 的 1080p 截图约 13 秒（2025/158），再加约 100 token 输出约 3 秒，即每步约 16 秒；截图缩到 720p（约 920 token）可降到约 9 秒。比云端前沿模型每步 6–7 秒慢约 1.3–2.7 倍，成功率还低 12–20 个点。独立显卡（16 GB 级）理论上可到每步 1–3 秒，但缺实测。
- 4B 级模型（Mage-VL-Cowork bf16 9.48 GB，4-bit 量化估计 3 GB 左右，未核实）是"普通电脑本地跑"的现实下限；8 GB 内存的机器很难同时跑模型和正常办公。
- 本地模型适合做：隐私敏感的短任务、离线兜底、以及"工具路线 + 本地小模型选工具"（不需要看图时，文本小模型足够且快得多）。GUI 长任务仍应交给云端。
- PeerDesk 是一个 Rust 单二进制，内嵌推理（如 llama.cpp 绑定）会显著增大体积和平台适配面；更务实的是"可选连接本机 Ollama/LM Studio/Foundry Local 等 OpenAI 兼容端点"。

### Gaps
- 没找到 Mage-VL-Cowork / Fara1.5 / UI-TARS 在普通消费级 PC（核显、NPU、8–16 GB 内存）上的实测每步延迟。
- 没找到 NPU（Copilot+ PC 40 TOPS 级、Apple Neural Engine）跑视觉 agent 模型的实测数据，也没核实 Windows ML / Foundry Local 对这些模型的支持情况。
- UI-TARS-1.5-7B、Qwen3-VL-4B/8B 在 OSWorld-Verified 上的 2026 年可比分数本次未取到。
- Apple 端侧模型的参数量、上下文长度官方数字未核实。

## 五、自带 Key（BYOK）、托管、本地：同类产品怎么组织，各自取舍

### Takeaway
主流产品普遍是"托管订阅为主 + 有限 BYOK"：BYOK 往往只限几家大厂、只在付费档开放、部分功能仍走官方服务器或官方模型，隐私承诺不一定覆盖 BYOK 请求。新变化是 Anthropic、OpenAI 的订阅开始附带 API 额度，降低了用户自带 Key 的门槛。

### Cited Findings
- Raycast：BYOK 为 Pro 专属；支持 Anthropic、Google、OpenAI、OpenRouter，每家一个 Key；前三家只能用 Raycast AI 已上架的模型，且请求经 Raycast 服务器中转（统一接口、回退、提示管理）；OpenRouter 的请求直连 OpenRouter；部分在 Raycast 服务器上跑的工具（如图片生成）仍用 Raycast 模型并扣 AI 额度 — [Raycast BYOK 手册](https://manual.raycast.com/ai/bring-your-own-key)（已读原文）
- Cursor：BYOK 只能配一个 base URL、不能调模型参数（论坛用户描述）；Tab 补全始终用 Cursor 自家模型；Cursor 的零数据保留政策不适用于自带 Key 的请求；是否需要 Pro 档各方说法矛盾 — [Cursor 论坛](https://forum.cursor.com/t/when-will-cursor-fully-support-byok/164375)、[byoklist](https://byoklist.com/guides/cursor-byok)（搜索摘要，未核实）；有博客称 OpenAI 约 2026-11-12 起切断 Cursor 访问其模型，单一来源，未核实 — [digitalapplied](https://www.digitalapplied.com/blog/openai-ends-cursor-model-access-november-cutoff)
- Warp：BYOK 只支持 Anthropic、OpenAI、Google；自定义 OpenAI 兼容端点只用于本地 agent，云端 agent 仍扣 Warp 额度；BYOK 是否需 Pro 档有矛盾说法（维护者 2026-09 称免费档可用自定义端点） — [Warp issue #12251](https://github.com/warpdotdev/warp/issues/12251)、[Warp issue #9288](https://github.com/warpdotdev/warp/issues/9288)（搜索摘要）
- Open Interpreter：开源，直接用用户自己的 Key 或本地模型（Ollama 等），无托管中转 — [Open Interpreter 文档](https://docs.openinterpreter.com/language-models/local-models/ollama)（搜索摘要）
- Anthropic 订阅（claude.com/pricing，2026-11-26 读取）：Pro $20/月（年付 $17）；Max $100/月，含每月 $100 API 额度；Ultra $200/月，含每月 $200 API 额度；Team 高级席位 $100–125/席/月，团队共享最多 $500/月 API 额度 — [Claude 价格](https://claude.com/pricing)（已读原文）
- OpenAI：ChatGPT Business 计划含"每月最多 $500 API 额度"；标准席位 $20–25/月，高级席位 $100–125/月 — [OpenAI 价格页](https://openai.com/api/pricing/)（已读原文）
- 微软 MagenticLite：开源应用，模型（MagenticBrain、Fara1.5）放在 Microsoft Foundry 上提供；称"数据留在用户机器上"，浏览器和代码执行跑在基于 QEMU 的 Quicksand 沙箱里；登录、付款、不可撤销提交等关键点暂停等用户确认 — [MagenticLite 博客](https://microsoft.com/en-us/research/blog/magenticlite-magenticbrain-fara1-5-an-agentic-experience-optimized-for-small-models)（已读原文）
- Claude Managed Agents（托管 agent）按 token + 会话运行时 $0.08/小时计费；computer use 工具集目前不在 Managed Agents 中提供 — [Claude 价格页](https://platform.claude.com/docs/en/about-claude/pricing.md)、[computer use 文档](https://platform.claude.com/docs/en/agents-and-tools/tool-use/computer-use-tool.md)（已读原文）

### Inferences
- 三种模式的取舍（推断）：
  - **BYOK**：我们零模型成本、用户隐私承诺清楚（直连厂商）；缺点是新手门槛高、无法统一体验和计费、不同厂商工具调用格式不同要做适配。订阅附带 API 额度（Claude Max/Ultra、ChatGPT Business）让"用自己订阅的额度"变得现实。
  - **托管**：体验最好、可做模型路由（简单任务用便宜模型、难的用前沿）、可做缓存和计量；但 GUI 任务成本高（见第三节），必须按用量收费并在账户上计量（与 CLAUDE.md"计费挂账户"一致），且我们要承担数据处理责任。
  - **本地**：零边际成本、隐私最好；但慢、弱、吃内存，适合短任务和离线兜底。
- 建议的组织方式（推断）：开源版默认 BYOK + 可选本地 OpenAI 兼容端点（直连，不经我们服务器）；托管云作为付费功能，提供路由和统一计量。避免学 Raycast 那样 BYOK 也经自家服务器中转——对开源产品，这会削弱"Key 不出本机"的信任卖点。
- 与 PeerDesk 现有"不静默降级"原则一致：BYOK 额度不足、本地模型能力不够时要明确告诉用户原因和办法，而不是悄悄切到托管模型。

### Gaps
- 没有读到 Cursor、Warp 官方文档原文，计划限制有矛盾说法。
- 没有查到 ChatGPT agent 模式、Claude 桌面端 computer use 的具体用量限制和定价结构。
- 没有找到 BYOK 与托管两种模式在付费转化率、留存上的公开数据。

## 六、实用文件/搜索任务（如"找今天桌面上的 PDF，把合同发出去"）：最便宜可靠的路径

### Takeaway
这类任务应走"文件索引 / 系统搜索 API + 少量结构化工具"，GUI 只用于最后发送一步（若没有对应 API）。证据：混合与 API 优先在成功率、步数、时间上都更好；文件/代码类操作业内（MagenticLite）也是直接调工具，而不是看屏幕。

### Cited Findings
- MagenticLite：浏览器任务走 Fara1.5 截图循环，文件和代码任务由编排模型直接调 bash / edit / search / open 工具 — [MagenticLite 博客](https://microsoft.com/en-us/research/blog/magenticlite-magenticbrain-fara1-5-an-agentic-experience-optimized-for-small-models)（已读原文，2026-05-21）
- MCPWorld：0–5 步的短任务 GUI 与混合都是 90.41%，但 MCP-Only 只有 63.01%（主要因 MCP 覆盖不足）；10 步以上混合 51.11% 明显高于 GUI 35.56% — [MCPWorld](https://arxiv.org/pdf/2506.07672)（已读原文）
- UFO2：GUI+API 让 o1 平均步数从 16.0 降到 6.6，成功率 16.3% → 24.5% — [UFO2](https://arxiv.org/html/2504.14603v2)（已读原文）
- OSWorld-Human：每步生成无障碍树需 3–26 秒并加数千 token，部分应用步数暴涨 — [OSWorld-Human](https://arxiv.org/html/2506.16042v1)（已读原文）
- Windows Search：应用可通过 OLE DB 用扩展的 SQL-92/99 语法查询 Windows Search 索引（Vista 及以后、所有 Windows 10 版本支持），官方提供 WSSQL / WSOleDB 示例 — [Microsoft Learn: Windows Search SQL](https://learn.microsoft.com/en-us/windows/win32/search/-search-sql-windowssearch-entry)（已读原文）
- macOS Spotlight：`NSMetadataQuery` 用于查询 Spotlight 元数据索引（本次页面内容未加载出来，具体属性如 `kMDItemContentType`、`kMDItemFSContentChangeDate` 和"新文件可能未及时入索引"等细节来自通用知识，未核实） — [Apple NSMetadataQuery](https://developer.apple.com/documentation/foundation/nsmetadataquery)
- Windows 11 智能体平台：设备上 MCP 注册表（On-Device Registry）公开预览，应用可把功能做成 MCP 服务器注册进去，agent 通过代理访问，代理负责认证、审计、授权；"agent workspace"是一个单独的、受限的空间，开启后会在设备上创建单独的 agent 账户，在后台使用授权的应用和文件；各来源对是私有预览还是公开预览说法不一 — [Windows 开发者博客](https://blogs.windows.com/windowsdeveloper/2025/11/18/ignite-2025-furthering-windows-as-the-premier-platform-for-developers-governed-by-security)、[Microsoft 支持：实验性智能体功能](https://support.microsoft.com/en-us/windows/ai/ai-features/experimental-agentic-features)（搜索摘要）
- Anthropic 价格：bash 工具定义 325 token、工具调用系统提示 286 token（Opus/Sonnet/Haiku 5.5）；相比之下 computer use 工具集定义约 4,500 token，每张 1080p 截图 2,691 token — [Claude 价格页](https://platform.claude.com/docs/en/about-claude/pricing.md)、[vision 文档](https://platform.claude.com/docs/en/build-with-claude/vision.md)（已读原文）

### Inferences
- 示例任务"找今天桌面上的 PDF，把合同发出去"的建议路径（推断）：
  1. `search_files(位置=桌面, 类型=pdf, 修改时间=今天)`：Windows 用 Windows Search 索引（SystemIndex），macOS 用 Spotlight，Linux 或索引未覆盖时退回目录遍历（桌面文件少，遍历毫秒级）。
  2. 模型从文件名判断哪个是合同；拿不准时调 `read_pdf_text(前 1–2 页)` 让模型看文字，而不是截图。
  3. "发出去"：如果是发给手机端的用户本人，直接用 PeerDesk 已有的文件传输通道；发邮件/IM 若有 API 或 MCP 就用，否则才用 GUI（打开邮件客户端、附件）。
  - 成本：工具路线约 3 轮调用，Sonnet 5.5 约 $0.02、Haiku 5.5 约 $0.001；全 GUI 按 cua-speedrun 实测量级约 $0.3–$0.7、1–2 分钟。时间与成本都差 1–2 个数量级。
- 本地小模型在工具路线上反而够用：只需选工具和读文本，不用看图，4B 级文本模型即可；这让"离线也能用"的承诺在文件类任务上可兑现。
- 结构化工具应做成 MCP 服务器（PeerDesk 本来就要提供 MCP server 和 CLI），这样同一套工具既给自家 agent 用，也给 Claude Desktop、Windows MCP 注册表等外部 agent 用；Windows 侧可注册进设备 MCP 注册表获得系统级审计。
- 安全：文件发送属于"不可撤销"动作，应像 MagenticLite 一样在关键点暂停等用户在手机端确认；GUI 路线还有屏幕内容提示注入风险（Anthropic 官方明确指出），工具路线风险面更小。

### Gaps
- 没找到 Windows Search / Spotlight 查询延迟、索引覆盖率（默认索引位置是否含桌面、新文件入索引延迟）的公开测量；Spotlight API 细节未从官方页面核实。
- 没找到"文件查找 + 发送"这类任务在 GUI 与工具两条路线上的同任务实测对比，上面的成本和时间对比是推断。
- Windows 设备 MCP 注册表和 agent workspace 的当前（2026-11）正式状态、第三方应用能否注册，未从官方原文核实。
