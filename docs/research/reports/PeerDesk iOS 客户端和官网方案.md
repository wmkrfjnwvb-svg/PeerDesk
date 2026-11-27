# PeerDesk iOS 客户端和官网方案（截至 2026-11-27）

标记：【已核实】本次打开了一手原文（App Store 审核指南、苹果开发者文档、厂商官网和 App Store 页、GitHub、Cloudflare 文档）；【二手】搜索摘要、第三方报道、缓存页；【推断】我们自己的判断；【未核实】没查到或页面打不开。
承接：`PeerDesk 美国市场赚钱方案.md`（定价、90 天计划）、`PeerDesk 核心用户功能设计.md`、`PeerDesk AI Agent 业内调研.md`、`devices` 分支的 `N-devices.md`（账户设备表 S0–S5）、`I-release.md`（签名发布）。已有结论不重复。
没写代码、没编译、没提交。苹果开发者文档页是网页脚本渲染的，WebFetch 读不到正文，本次改用苹果文档站的 JSON 数据接口取原文（`developer.apple.com/tutorials/data/documentation/...`），取到的段落在下面按原意引用；取不到的标【未核实】。

---

## 0. 结论先行（一页）

**推荐路线**

1. **iOS：SwiftUI 原生界面 + Rust 核心（手写 C 接口为主，控制面可用 uniffi）**。视频先走 `AVSampleBufferDisplayLayer`（系统硬解加显示），触控、软键盘、快捷键栏、外接键鼠、iPad 全部用原生控件。不选 egui（eframe 官方只列 Linux/Mac/Windows/Android，没有 iOS），Flutter 只在“确定要做 Android”时才值得。
2. **官网：Astro 静态站 + Starlight 文档，放 Cloudflare Workers 静态资源（新项目官方推荐，静态请求免费不限量）；安装包放 GitHub Releases（单文件 < 2 GiB，不限流量）+ 官网域名做稳定跳转**，R2 零出口流量费，留作以后。英文为默认、中文放 `/zh/`。现有 `web/`（React + Vite 管理控制台）继续独立，放 `app.` 子域名，不合并。
3. **订阅：iOS 上必须同时提供 App 内购（IAP）。** 订阅在官网买、App 里直接用，指南 3.1.3(b) 只允许“这些内容在 App 里也能用内购买到”的情况。账户挂在 PeerDesk 账户上，IAP 只是另一个付款渠道（符合 CLAUDE.md “计费挂账户不挂设备”）。

**v1 范围（最小但值得付费）**：登录 → 账户设备列表（在线、状态）→ 一键连接（P2P 优先，中继兜底）→ 硬解画面 → 直接点击/触控板两种触控模式 + 手势 → 软键盘加快捷键栏 + 外接键盘 → 文本剪贴板 → 网络差时的原因提示 → 推送（设备离线/需要处理；有审批功能后加“Agent 等你批准”）→ IAP 订阅 + 恢复购买 → 账户删除 → Face ID 锁。文件、多显示器切换、终端视图、Live Activity 放 v1.1 以后。

**关键审核风险（按严重度）**

| # | 风险 | 依据 | 对策 |
|---|---|---|---|
| 1 | 官网买、App 里用，但 App 内没有 IAP → 被拒 | 3.1.1、3.1.3(b) 原文（第 2 节） | 做 IAP；美国区可以另加官网购买链接，但规则正在诉讼中，不能当主路径 |
| 2 | 审核员把我们当 4.2.7 的“局域网才能连”的远程桌面 | 4.2.7(a) 原文；但该条只管“镜像特定软件或服务、不是通用镜像”的 App | 审核备注写清楚是通用远程桌面客户端；准备 Demo 主机 |
| 3 | 审核账号和后端：需要一台一直在线的 Demo 主机 | 2.1(a) 原文要求“带登录就给 demo 账号并打开后端” | 提前搭一台 Mac mini 加一台 Windows 做 Demo 主机，账号写进备注 |
| 4 | 推送内容里带敏感信息；把推送当必需功能 | 4.5.4 原文 | 通知正文只写“某设备上有一项待处理”，细节打开 App 再取；没推送也能用 |
| 5 | 登录方式用了 GitHub/Google 又没有 Apple 登录 | 4.8 原文 | 邮箱/通行密钥 + Sign in with Apple，GitHub 作为附加 |
| 6 | IPv6-only 网络下不能用 | 2.5.5 原文 | 全程域名 + 双栈，用 IPv6-only 网络测（第 3.5 节） |
| 7 | 下载并执行代码（OpenH264 运行时下载那套做法在 iOS 上不行） | 2.5.2 原文 | iOS 只用系统 VideoToolbox，不带 OpenH264 |
| 8 | 出口合规（加密）和隐私清单 | 隐私清单自 2024-05-01 起必填（已核实）；出口合规要求【未核实】 | 第 9 节问题 |

**时间粗估（推断，1 名熟悉 Rust 的工程师 + 一台 Mac，Apple 账号和主体由产品负责人准备）**：

| 阶段 | 内容 | 周数 |
|---|---|---|
| P0 | iOS 目标编译通过、真机显示一路真实画面（TestFlight 内部） | 2 |
| P1 | 核心接口 + 触控/键盘/剪贴板 + 设备列表 + 网络切换 | 4–5 |
| P2 | 账户登录、IAP、推送、审核准备、真机测量 | 3–4 |
| 外部 TestFlight（首个外部构建要过 Beta 审核） | | 第 9–11 周 |
| 上架（含一轮被拒的缓冲） | | 第 12–14 周 |
| 官网 v1 | 1 人，和 iOS 并行；卡点是法律页和素材 | 3–4（耗时），可在第 8 周前上线 |

服务端依赖（账户设备表、在线推送、APNs）和 `N-devices.md` 的 S1–S2 同一条骨干，3.5–4.5 周，要先或同时做。90 天计划第 7–8 周写了“手机端审批”，按上表只能做到 TestFlight 测试版，公开上架在第 12–14 周。

**最大的不确定**：①Jump Desktop 已经有“AI Agent 用的远程桌面”落地页，并写明 iPhone/iPad/Android 的手机查看在 beta；Astropad Workbench 2026-10 加了终端。这一档不是空白（第 1.3 节）。②审核结果本身，上面 1、2 两条要靠真实提交才知道。

---

## 1. 同行 iOS 版做了什么

### 1.1 功能对照表

取证方式：App Store 页面原文（WebFetch，页面是缓存快照，日期只有“月日”没有年，下面写“Nov 10”这样的是页面原样）。没写的格子 = 页面没提，不等于没有。

| 产品 | 价格模式 | 触控模式 / 手势 | 键盘 / 快捷键 | 外接键鼠 | 多显示器 | 剪贴板 / 文件 | 画质 | 设备列表 | 推送 | iPad | 大小 / 系统 |
|---|---|---|---|---|---|---|---|---|---|---|---|
| **Jump Desktop**（RDP/VNC/Fluid） | **$14.99 一次性买断，页面写“not subscriptions”**；Teams 订阅 $7.99/$15.99 每用户每月，官网写“iPhone and iPad sold separately”（Mac 版含在 Teams Pro 里，iOS 不含） | “触摸手势实现完整鼠标：左中右键、拖拽、滚动、精确指针移动” | 蓝牙键盘、快捷键、功能键、方向键、宏 | 支持实体鼠标（有限制） | “外接显示器当真显示器用”；RDP 多显示器 | 文本剪贴板；RDP 文件夹共享、远程打印 | Fluid 最高 120 fps、最高 10-bit 色 | “新的电脑列表”（10.17.5）、iCloud 同步连接、多个同时连接、预览 | 页面没提 | iPad Pro 分屏 + Apple Pencil | 57.2 MB；iOS 16+ |
| **Screens 5**（Edovia） | 通用 App（iPhone/iPad/Mac 一个 App，不再分开买），免费下载 + 内购：月付/年付/买断。**价格各页互相矛盾**：2023 年评测 $2.99 月、$24.99 年、$74 买断；某搜索摘要 $3.99/$24.99/$139.99 或 $119；帮助页现版 $4.99 月、$29.99 年、$249.99 买断 | 本次没读到 | 工具条含修饰键、功能键、剪贴板；iPhone 竖放工具条在底部、横放在左侧 | 未读到 | 未读到 | 剪贴板（工具条） | 未读到 | 靠 **Screens Connect**（免费服务，Mac/Windows 装 helper、登录后设备自动出现），Linux 官网建议用 Tailscale | 未读到 | 是 | iOS 17+（搜索摘要） |
| **RustDesk** | **免费，无内购**；App 描述写明“只能控制，不能被控制或共享屏幕”；服务端 Pro 另收费（$11.88/月起，见赚钱方案） | 触控手势和鼠标/键盘控制；iPad 接妙控键盘/外接鼠标 | 实体键盘、终端的悬浮控制条 | iPad 外接鼠标 | 未读到 | 文件传输（版本说明）；地址簿（自托管 Pro） | H.264/H.265/AV1；SOCKS5/HTTP 代理 | 地址簿 | 未读到 | 是 | 73.2 MB；iOS 15+；1.5.3；评分 3.9（104 个评分） |
| **Microsoft Windows App**（原 Remote Desktop） | 免费 | **两种模式：直接点击（Direct touch，点哪是哪，无指针）和鼠标指针模式（有指针，手指拖动指针）**【二手：Microsoft Learn 搜索摘要，原页 iOS 段没取到】 | 屏幕键盘、iPadOS 26 悬浮键盘、扩展键 | 蓝牙鼠标 | 外接显示器 | 剪贴板重定向、文件夹重定向 | 未读到 | 未读到 | 未读到 | 要求 iPadOS 26 | 208.7 MB |
| **Parsec** | **没有官方 iOS 版**：官网首页和下载页没列 iOS；帮助中心安装指南只列网页版、Android、Linux、macOS、Windows【二手：搜索摘要，帮助中心页 403】；社区有开源的 OpenParsec（要自己下 Parsec SDK，靠 AltStore 侧载，非官方） | — | — | — | — | — | — | — | — | 第三方页说不支持 iPad | — |
| **AnyDesk** | 免费（个人）；商业另买 | 页面只有零星提到（评论里有“触摸板式”控制） | 硬件键盘布局、特殊键盘、妙控键盘 | 蓝牙鼠标修复 | 外接显示器（5.5.1） | 文件管理器、拖放传文件、批量操作 | 未读到 | 未读到 | 页面没提 | 是，iPadOS 15.6+ | 50.3 MB；8.3.0 |
| **TeamViewer Remote Control** | 免费 + 内购；被控端装 QuickSupport | 未读到 | 未读到 | 未读到 | 未读到 | 文件传输、聊天 | 未读到 | 未读到 | 未读到 | 是 | 要求 iOS 17.7 或 15.4（各区页面不一致）【二手】；评论普遍抱怨被误判为商业用途 |
| **Chrome 远程桌面** | 免费；2015-01 上架 | 未读到 | 未读到 | 未读到 | 未读到 | 未读到 | 未读到 | 登录 Google 账号后点在线电脑 | 未读到 | 是 | 年龄分级 17+【二手】；App Store 当前页没打开 |
| **Astropad Workbench**（直接对手，只 Apple） | **免费每天 20 分钟；无限用量订阅**。价格三处不一致：4 月报道 $10/月、$50/年；2026-10-27 的 9to5Mac 报道 $14.99/月、$79.99/年；官网首页写 $99.99/年 | Apple Pencil 光标控制、键盘快捷键、语音输入 | 快捷键 | 未读到 | 合并多个 Mac 显示器（Unified Display） | 终端会话断线后继续运行；Mac 到 Mac 拖放传文件 | 实时流，端到端 AES-256 | “设备选择器”（TechCrunch，已有调研） | **页面没提** | 是 | 50.9 MB；iOS 26+；1.4.1 |

来源（均 2026-11-27 打开）：
- Jump：[App Store](https://apps.apple.com/us/app/jump-desktop-rdp-vnc-fluid/id364876095)、[定价](https://jumpdesktop.com/pricing-plans.html)
- Screens：[帮助页定价](https://help.edovia.com/en/screens-5/faq/s5-pricing)、[MacStories 评测](https://www.macstories.net/reviews/screens-5-an-updated-design-improved-user-experience-and-new-business-model/)、[Screens Connect](https://www.screensconnect.com/)
- RustDesk：[App Store](https://apps.apple.com/us/app/rustdesk-remote-desktop/id1581225015)
- Windows App：[App Store](https://apps.apple.com/us/app/windows-app-remote-desktop/id714464092)；触控模式描述来自 [Microsoft Learn 输入页](https://learn.microsoft.com/en-us/windows-app/input-keyboard-mouse-touch-pen)的搜索摘要
- Parsec：[parsec.app](https://parsec.app/)、[帮助中心](https://support.parsec.app/hc/en-us/categories/32361192574228-Parsec-App)（403，靠搜索摘要）、[OpenParsec](https://github.com/hugeBlack/OpenParsec)
- AnyDesk：[App Store](https://apps.apple.com/us/app/anydesk-remote-desktop/id1176131273)
- TeamViewer：[App Store](https://apps.apple.com/us/app/-/id692035811)（只拿到搜索摘要）
- CRD：[App Store](https://apps.apple.com/ai/app/id944025852?l=en-gb)（只拿到搜索摘要）、[2015 年发布报道](https://9to5google.com/2015/01/12/google-releases-chrome-remote-desktop-for-ios-to-access-your-computer-from-your-iphoneipad/)
- Workbench：[App Store](https://apps.apple.com/app/id6758788573)、[9to5Mac 1.4](https://9to5mac.com/2026/10/27/astropad-workbench-1-4-adds-terminal-access-iphone-duo-support-and-more/)、[官网](https://astropad.com/workbench/)

### 1.2 从表里读出来的

1. **价格模式没有统一答案**：Jump 买断（iOS 单独买）；Screens 通用 App 加订阅或买断，一次购买 iPhone/iPad/Mac 通用；Workbench 免费额度加订阅；RustDesk、微软、AnyDesk、CRD 免费。订阅类的都是“免费下载 + 内购”，和指南 3.1.1 一致。
2. **共用订阅**：Screens 一个通用 App，一次购买各设备通用（MacStories 2023）；Jump iOS 和 Mac 分开买（Jump 官网）。我们的账户订阅天然跨设备，而指南 3.1.2(a) 本来就要求订阅“在用户所有可用该 App 的设备上都能用”（原文第 2 节）。
3. **没有一家的 App Store 页面写推送通知**（Jump、Workbench、RustDesk、AnyDesk、Windows App 都没提）。页面没提不等于没做，要装 App 才知道【未核实】；如果属实，“Agent 等你批准、主机掉线”的推送就是差异化。
4. **Parsec 没有官方 iOS 版**（二手证据），不用对标它的 iOS。
5. **画质、帧率是 Jump 的主卖点**（Fluid、120 fps、10-bit），但没写延迟数字；我们的 e2e 指标（冻结、静止后变清晰时间）可以做对标，前提是真机量出来（第 6 节）。

### 1.3 官网和产品上的对手新动作（影响定位）

- **Jump Desktop 已有 AI Agent 落地页**（[ai-agents.html](https://jumpdesktop.com/ai-agents.html)，2026-11-27 读）：标题“Remote Desktop for AI Agents”；`jump-cli mcp` 起 MCP 服务；Agent 最多同时连 8 台电脑；Mission Control 看每个 Agent 会话和动作日志，可接管，输密码不经过 Agent；**“Mobile viewing from iPhone, iPad, or Android is in beta”**；Mission Control 免费，Sleep Override 是 Enterprise。
- **Astropad Workbench 1.4（2026-10-27）加了远程终端**，终端会话断线后继续运行，iPhone 上可用语音输入；主打监控和重启 Agent。
- 含义【推断】：“手机上看一眼、接管”这件事两家已经在做。我们的差异：Windows/Linux 同样一流、传输层指标、开源可自托管、审批推送（待查证对手没有）、账户计费透明。终端视图（比视频便宜得多、对 CLI 类 Agent 更合适）值得放进 v1.1 评估。

---

## 2. App Store 审核指南原文和对我们的影响

来源：[App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)（2026-11-27 读，页面共约 114 千字符，本次读到 5.2.5 之前；5.6 开发者行为准则没读到【未核实】）。下面“原文”都是英文原句。

### 2.1 4.2.7 远程桌面客户端

> If your remote desktop app acts as a mirror of specific software or services rather than a generic mirror of the host device, it must comply with the following:
> (a) The app must only connect to a user-owned host device that is a personal computer or dedicated game console owned by the user, and both the host device and client must be connected on a local and LAN-based network.
> (b) Any software or services appearing in the client are fully executed on the host device, rendered on the screen of the host device, and may not use APIs or platform features beyond what is required to stream the Remote Desktop.
> (c) All account creation and management must be initiated from the host device.
> (d) The UI appearing on the client does not resemble an iOS or App Store view, does not provide a store-like interface, or include the ability to browse, select, or purchase software not already owned or licensed by the user. …
> (e) Thin clients for cloud-based apps are not appropriate for the App Store.

对我们的影响【推断，需要提交时验证】：
- **整条 4.2.7 的前提是“镜像特定软件或服务，而不是通用镜像”**。PeerDesk 是通用的整机桌面远程，按字面不在这一条之内。同行里 Jump、RustDesk、微软、AnyDesk、CRD 都能经互联网连接并在 App Store 上架（见第 1 节），也佐证了“通用远程桌面”没有“只能局域网”的限制。
- 但审核员有可能把“给 Agent 用”的功能看成“特定软件的镜像”。所以：描述写成“远程控制你自己的电脑”，不要写成“Claude/OpenClaw 专用客户端”；审核备注主动说明这是通用远程桌面。
- 即便按 4.2.7 对照，我们也要守住这些边界：只连用户自己的电脑（账户内的设备）；软件都在主机上执行和渲染；App 里不做商店式界面。**“审批收件箱”**是原生界面，不是主机屏幕的镜像：它只展示“来自你自己设备的待处理事项”，要在备注里解释。
- 4.2.7(c)“账户创建和管理要从主机发起”只对“特定软件镜像”适用；我们的账户创建走官网或桌面端，iOS 里先只做登录，不做注册（同时省掉 5.1.1(v) 的账户删除要求？见 2.4，不能偷懒）。

### 2.2 3.1 支付：IAP、订阅、其他购买方式

原文要点：

- 3.1.1：“If you want to unlock features or functionality within your app, (by way of example: subscriptions, … access to premium content, or unlocking a full version), you must use in-app purchase. Apps may not use their own mechanisms to unlock content or functionality, such as license keys, … QR codes, …”
- 3.1.1(a)：“Developers may apply for entitlements to provide a link in their app to a permitted destination in order to purchase digital content or services. These entitlements are not required for developers to include buttons, external links, or other calls to action in their United States storefront apps.”；其他区：“In all other storefronts, except for the United States storefront, where this prohibition does not apply, apps and their metadata may not include buttons, external links, or other calls to action that direct customers to purchasing mechanisms other than in-app purchase.”
- 3.1.2(a)：自动续期订阅必须“provide ongoing value”，周期至少 7 天，“**Subscriptions must work on all of the user's devices where the app is available**”；允许“software as a service (SAAS)”和“cloud support”。
- 3.1.3(b) 多平台服务：“Apps that operate across multiple platforms may allow users to access content, subscriptions, or features they have acquired in your app on other platforms or your web site, including consumable items in multi-platform games, **provided those items are also available as in-app purchases within the app**.”
- 3.1.3(a) “Reader”应用只限杂志、报纸、书、音频、音乐、视频，不含我们；3.1.3(c) 企业服务：“If your app is only sold directly by you to organizations or groups for their employees or students … Consumer, single user, or family sales must use in-app purchase.”；3.1.3(f) 免费独立 App 只限 VoIP、云存储、邮件、网站托管，不含远程桌面。

对我们的影响：

1. **订阅在官网买、App 里能不能用：能，但前提是同一档位在 App 内也能用内购买到（3.1.3(b)）。** 所以 v1 要做 StoreKit 订阅，不能只“登录就用”。
2. **App 内要不要提供内购：要。** Plus/Pro 各一个自动续期订阅（可用同一订阅组，满足 3.1.2(b) 的升降级要求）。免费档（2 台设备）不需要内购，直接登录可用。
3. **账户为主，IAP 是渠道**：购买时把我们的账户标识作为 `appAccountToken` 传给 StoreKit，服务端收苹果的服务器通知后给账户加订阅状态，桌面端和 iOS 都读账户状态（符合 3.1.2(a)“所有设备可用”）。`appAccountToken` 的文档页正文没取到，用法是常规做法【未核实原文措辞】。
4. **美国区外链**：现行指南文本允许美国区 App 放外部购买链接且不需要专门授权。但这件事在诉讼中：2026-04-28 第九巡回法院撤销了暂缓令（[MacRumors](https://www.macrumors.com/2026/04/29/epic-games-wins-reversal-app-store-fee-battle/)），佣金如何收回到地区法院；有报道称最高法院已受理（[TheNextWeb](https://thenextweb.com/news/supreme-court-apple-epic-contempt-app-store-commission)，TechSpot 称 2026-06-30 受理、2027-06 前裁决）【二手，来源之间日期不一致】。**不能把外链当主路径，可以当美国区的附加路径**；最新状态要在提交前重查。
5. **成本**：苹果小企业计划佣金 15%（[小企业计划页](https://developer.apple.com/app-store/small-business-program/)：上年收入不超过 100 万美元的开发者，或新开发者；超过后当年后续按标准费率）。按赚钱方案的 Plus 典型场景 $7/月、中继 $0.28、固定 $0.30 估算【推断】：
   - 官网 Stripe（2.9%+$0.30，【未核实】）：毛利约 $5.92（85%）
   - iOS 内购 15%：$7 − 1.05 − 0.28 − 0.30 = $5.37（约 77%）
   - iOS 内购 30%：$7 − 2.10 − 0.28 − 0.30 = $4.32（约 62%）
   iOS 内购价是否比官网高，要产品负责人定（问题 3）。
6. **创始会员预售（Stripe 买的）**：iOS 里要能识别，但购买渠道是官网；同时因为 3.1.3(b)，同档位必须有 IAP。创始会员的价格不用在 iOS 内购里出现。

### 2.3 2.5.x 后台、公共 API、IPv6、自包含

- 2.5.4：“Multitasking apps may only use background services for their intended purposes: VoIP, audio playback, location, task completion, local notifications, etc.” → 不能为了“保持连接”滥用 VoIP、音频、定位。后台保持连接不能靠这些（第 4 节）。
- 2.5.1：只能用公共 API。
- 2.5.2：“Apps should be self-contained in their bundles, and may not … download, install, or execute code which introduces or changes features or functionality of the app …” → **iOS 版不能走“运行时下载 Cisco OpenH264 二进制”的方案**（`crates/media/Cargo.toml` 的注释写了桌面发布版要改成运行时加载）；iOS 只用系统 VideoToolbox。
- 2.5.5：“Apps must be fully functional on IPv6-only networks.”
- 2.5.14：录屏、录音类要明确同意和提示。我们是接收端，不录制用户的 iPhone；不涉及，但会话录像以后若做在主机侧。
- 2.4.2：省电，不要后台乱跑。

### 2.4 其他会碰到的条款

- 2.1(a)：“include demo account info (and turn on your back-end service!) if your app includes a login. If you are unable to provide a demo account due to legal or security obligations, you may include a built-in demo mode in lieu of a demo account with prior approval by Apple.” → 需要一台一直在线的演示主机和一个演示账号；演示模式不能是假画面（CLAUDE.md 不造假），宁可真有一台 Demo 主机。
- 4.5.4：“Push Notifications must not be required for the app to function, and should not be used to send sensitive personal or confidential information. Push Notifications should not be used for promotions or direct marketing purposes unless customers have explicitly opted in …”
- 4.8：用第三方或社交登录设置主账户时，必须同时提供另一个满足条件（只收姓名邮箱、可隐藏邮箱、不为广告收集互动）的登录方式；只用公司自己账户体系则不需要。→ 邮箱 + 通行密钥自己的体系不触发；加 GitHub/Google 登录就要加 Sign in with Apple。
- 5.1.1(v)：“If your app supports account creation, you must also offer account deletion within the app.” 和“如果没有重要的账户功能，就不要强制登录”。→ iOS 里只做登录而不做注册，条文字面不触发删除；但稳妥起见做“删除账户”（服务端调现有账户删除，以后正式账户系统也需要）。我们的核心功能是账户设备列表，需要登录是合理的；“连接码直连”不登录也能用，写进 v1 以满足“不登录也能用”的倾向。
- 5.1.1(i)：隐私政策链接要同时放在 App Store Connect 元数据和 App 内；说明收集什么、保留多久、如何删除。
- 5.1.2(i)：把个人数据发给第三方（含第三方 AI）要明确同意。我们 BYOK、不托管模型，v1 不涉及。
- 4.2：“Your app should include features, content, and UI that elevate it beyond a repackaged website.” → 原生体验有助于通过。
- 苹果开发者新闻（[upcoming-requirements](https://developer.apple.com/news/upcoming-requirements/)）：自 2026-04-28 起上传要用 Xcode 26 / iOS 26 SDK；自 2026-09-09 起最低部署目标至少 iOS 13；隐私清单（必填理由 API）自 2024-05-01 起。**Rust 标准库和 tokio 用到的“文件时间戳”“系统启动时间”类 API 需要在 `PrivacyInfo.xcprivacy` 里声明理由**【推断，要在第一次上传时看苹果的邮件反馈】。
- 出口合规（加密）：App 里有自己的 TLS/QUIC 加密。是否要填 `ITSAppUsesNonExemptEncryption` 或年度自分类报告【未核实】，写进问题 9。

---

## 3. 技术路线

### 3.1 现状（读了仓库，dev 工作树）

- 工作区 7 个 crate：`core`（纯逻辑，无 IO）、`net`（quinn 0.11 + rustls ring，约 1.16 万行）、`media`（OpenH264 + 各平台硬件编解码）、`platform`（采集、输入、剪贴板，各系统胶水）、`client`（eframe 0.32 glow 界面 + 命令行）、`server`、`e2e`。
- `media/src/vt.rs`：macOS 的 VideoToolbox 编解码，“直接声明 C 接口，只依赖标准库和 `annexb`”，只在 macOS 编译，**解码输出 BGRA**（硬件做颜色转换）。`media/src/gl.rs`：用 glow 把 NV12 画到屏幕（和桌面界面共用 OpenGL）。
- `net/src/controller.rs`：控制端会话的钩子接口（`ScreenSink`、`LinkHook`、`QualityHook` 等），是 iOS 外壳最自然的接缝。
- 网络层已按双栈写（`candidates.rs` 处理 IPv6，`PREFER_IPV6` 默认优先）。

### 3.2 三条路线

| | A. SwiftUI + Rust 核心 | B. Flutter + Rust（flutter_rust_bridge） | C. egui 直接上 iOS |
|---|---|---|---|
| 成熟度 | uniffi：Mozilla 在 Firefox 移动和桌面里大量使用，README 写“ready for production use”，但仍是 1.0 以前，“高级功能升级可能破坏”；支持 Swift/Kotlin/Python/Ruby（[uniffi-rs](https://github.com/mozilla/uniffi-rs)）。手写 C 接口 + cbindgen 是最常规做法 | flutter_rust_bridge 约 5.5k 星，支持 Android/iOS/Windows/Linux/macOS/Web（[GitHub](https://github.com/fzyzcjy/flutter_rust_bridge)）。**RustDesk 就是这么做的**：iOS 版是 Flutter，App Store 上是 73.2 MB，只能控制别人（RustDesk 页） | eframe 官方 README：“can be compiled both to run natively (for Linux, Mac, Windows, and Android)”，**没写 iOS**（[README](https://raw.githubusercontent.com/emilk/egui/master/crates/eframe/README.md)）。egui-winit 里有 iOS 分支代码和安全区模块（0.34.x，docs.rs），eframe 的 iOS 路径只是零星兼容；没找到官方 iOS 示例或活跃 issue 跟踪【未核实】。我们现在是 eframe 0.32，glow（OpenGL ES）后端 |
| 包体 | 小：Rust 核心 + 系统 UI。同类实测：Workbench 50.9 MB、AnyDesk 50.3 MB、Jump 57.2 MB（SwiftUI/原生为主）。我们的核心大小要量【推断，估计个位数 MB 到十几 MB】 | 多一份 Flutter 引擎，RustDesk 73.2 MB 是参照 | 比 A 大，且要自带字体（`egui-phosphor` 等） |
| 审核风险 | 低：原生控件，最不会被说“不像 App”（4.2） | 低到中：Flutter App 常见，但视频走外部纹理，自定义输入要自己管 | 中：自绘界面，辅助功能、输入法、系统键盘、安全区、Dynamic Island 都要自己处理（egui-winit 注释说明最外框包括灵动岛区域，“要由 eframe 使用者保证内容不被遮挡”） |
| 手感 | 最好：原生手势识别器、`UIKeyCommand`、指针交互、外接显示器、Stage Manager 都是一等公民 | 好，但键盘快捷键、系统指针、输入法要靠插件 | 差：触控、IME、软键盘要自己接 |
| 开发和维护 | 需要 Swift/SwiftUI 技能；两套界面（桌面 egui、iOS SwiftUI）。核心共用最多 | 需要 Dart/Flutter 技能；三套 UI（egui、Flutter、将来 Android 可复用 Flutter） | 复用桌面界面，但要重写成触控交互；每次升级 eframe/winit 都可能掉坑 |
| 对 CLAUDE.md 的影响 | 新增 FFI crate 需要 `unsafe`，和“`unsafe` 只在平台层和媒体层系统接口文件”冲突，要明确放宽范围（问题 7）；Swift 代码不计覆盖率，FFI 胶水按 `scripts/coverage.sh` 的规则排除并写理由 | 同左，且多 Dart 层的测试体系 | 不需要新 FFI，但图形界面在 iOS 上无法 e2e 自动测 |

**推荐 A。** 理由：
1. 我们的卖点是“手机上能点、能打字、一眼看清”，这一档是触控、键盘、画面，三样都要原生手感；
2. 传输层、编解码、码率控制是 Rust 核心，A 的 FFI 最薄，和“自己做核心”一致；
3. 审核风险最低，包体最小；
4. Android 不在近期计划里（赚钱方案和核心用户都没提），为 Android 预付 Flutter 的代价不划算。将来做 Android 可用 Jetpack Compose + 同一个 uniffi 接口（uniffi 本来就支持 Kotlin），核心是同一套。
5. 不选 C 的主因：官方没有 iOS 支持，且 OpenGL ES 后端的苹果支持状况本次没核实（常识是自 iOS 12 起弃用，【未核实】）；我们的 `gl.rs` 走 glow，换 Metal 反正要重写。

**手写 C 接口还是 uniffi**：建议“控制面用 uniffi（设备列表、账户、设置、会话事件，类型多、字符串多）、数据面用手写 C 接口（视频访问单元回调、输入事件，零拷贝、无异步）”。但这样两套并存会增加构建复杂度。**更保守的 v1 做法**：全部手写 C 接口，一个小而稳定的表面（约 20 个函数：初始化、登录令牌、订阅设备表、连接、断开、发输入事件、读状态、设置回调），Swift 侧再包一层。uniffi 留到接口稳定后评估。要不要先试 uniffi 的异步（本次 README 没提 Swift 异步支持，【未核实】）放在 P0 里做小实验。

### 3.3 视频解码和渲染怎么接

苹果的两条路（`AVSampleBufferDisplayLayer`：“displays compressed or uncompressed video frames”；`VTDecompressionSession`：“supports the decompression of a sequence of video frames”，均来自苹果文档摘要）：

| 方案 | 做法 | 优点 | 缺点 |
|---|---|---|---|
| **V1：Rust 只做收包、重组、前向纠错、NACK、码率控制，把整帧 H.264 访问单元交给 Swift；Swift 转成 AVCC 的 `CMSampleBuffer`，标 DisplayImmediately，送 `AVSampleBufferDisplayLayer`** | 系统解码加显示一步到位，硬件解码、零拷贝，代码最少 | 帧调度细节不在我们手里；要实测延迟 | 这是 P0 的默认做法 |
| V2：Swift 或 Rust 里用 `VTDecompressionSession` 解码出 `CVPixelBuffer`，用 `CVMetalTextureCache` 包成 Metal 纹理，自己画 | 可以叠光标、做解码后缩放、精确控制显示时刻，指标可量（解码时间、显示时刻） | 工作量大；Metal 管线要自己写 | 如果 V1 的延迟或画质达不到门槛再上 |

要点：
- `vt.rs` 的解码输出是 BGRA，在 iOS 上多一次颜色转换和内存拷贝，不适合直接复用；**可复用的是 `annexb.rs`（Annex B 与 AVCC 互转、SPS/PPS 提取）**。如果要用 Rust 持有解码会话，iOS 版要另写 `vt_ios` 路径，目标是 NV12 的 `CVPixelBuffer` 直通 Metal。
- 不要带 OpenH264：iOS 机器都有硬解（常识，具体到每个机型的 H.264 硬解能力【未核实】），而 2.5.2 禁止运行时下载可执行代码，自己从源码编译 OpenH264 又会遇到专利许可问题（桌面发布版用 Cisco 官方二进制正是为了这个）。
- 光标：主机端采集时是否带光标要看现有协议；iOS 直接点击模式不需要远端光标，触控板模式需要把光标叠在画面上（V1 的 `AVSampleBufferDisplayLayer` 上方盖一层原生视图即可）。
- CLAUDE.md 要求指标说话：**P0 就要在真机上量“收到帧到显示”的耗时和卡顿**，和桌面 Mac 的同一条链路对比。

### 3.4 QUIC（quinn）在 iOS 上

- **平台支持**：Rust 的 `aarch64-apple-ios`、`aarch64-apple-ios-sim`、`x86_64-apple-ios` 都是“Tier 2 without Host Tools”：保证能编译，不保证自动测试都跑（[平台支持页](https://doc.rust-lang.org/rustc/platform-support.html)）。
- **quinn 自己没有 iOS CI**：quinn 仓库里 #1778“Add CI for mobile platforms (iOS & Android)”仍是 open；iOS 相关历史 issue 有 #1625“Ignoring error setting IP_DONTFRAG on iOS”（已关）和 #1993“Faster UDP/IO on Apple platforms”（已合并）（[issue 列表](https://github.com/quinn-rs/quinn/issues?q=ios)）。含义：能用，但没人替我们守，**iOS 的构建和测试要进我们自己的 CI**。
- **网络切换（Wi-Fi ↔ 蜂窝）**：quinn 的 `Endpoint::rebind_abstract` 文档写明“Allows the endpoint's address to be updated live, affecting all active connections”，“Incoming connections and connections to servers unreachable from the new address will be lost”，出错时保留旧套接字（[docs.rs](https://docs.rs/quinn/latest/quinn/struct.Endpoint.html)）。所以切换网络时要在 Rust 里：`NWPathMonitor`（苹果的网络变化监视器）通知 → 我们的 ICE/打洞层重新收集候选 → 重新绑定套接字 → 必要时重连并立刻请求关键帧。我们自己的 `ice.rs`/`punch.rs` 需要一个“网络已变”的入口，现在是否有要看代码【未核实】。
- **后台**：App 一进后台就挂起（第 4 节），套接字不保活，回前台要快速重连（目标：从点击图标到出画面的时间，写进 iOS 的门槛指标）。
- **IPv6-only（2.5.5）**：苹果的网络选型技术说明 TN3151：UDP 用 Network framework 最好，“BSD Sockets”在“跨平台代码、已有基于 BSD Sockets 的库”时可以接受；并写明“Write code that supports both IPv4 and IPv6. Test that code on an IPv6-only network.”（[TN3151](https://developer.apple.com/documentation/technotes/tn3151-choosing-the-right-networking-api)，JSON 接口读的原文）。我们用 quinn 的 UDP 套接字属于“跨平台库”，可行。要点：连信令服务器用域名而不是 IPv4 字面量；ICE 候选里遇到纯 IPv4 的主机，在 NAT64 网络里要靠系统解析合成地址，直接发 IPv4 字面量会失败【推断】；在 macOS 上用“创建 NAT64 网络”（共享网络里的 IPv6-only 选项）测。
- **本地网络隐私**：局域网直连要触发 iOS 的本地网络弹窗（TN3179：“The first time a program accesses the local network, the system displays an alert asking the user to approve that access”，iOS 14 引入，MDM 无法预先配置）。要在 `Info.plist` 写用途说明，并且被拒后给出原因和解决办法（CLAUDE.md“诚实”）；没有同意就只能走打洞/中继。

### 3.5 推荐落地结构

```
crates/ios/        新增：C 接口（unsafe 集中在这里），只依赖 core、net（media 的 iOS 子集）
  include/peerdesk.h   cbindgen 生成
ios/               Xcode 工程（SwiftUI）
  PeerDeskKit        Swift 包：封装 C 接口、会话状态机、StoreKit
  App                界面
  Extensions/        通知服务扩展（解密/补全文本）、以后的 Widget
scripts/ios-build.sh   cargo build 三个目标 + 打成 xcframework
```

`net` 和 `core` 里没有桌面专属代码的部分应能直接复用；`platform`（采集、注入）不进 iOS（苹果也不允许 iOS 被远程控制）；`media` 里 iOS 只留 `annexb`、`color` 之类纯计算。

---

## 4. 推送（APNs）和后台限制

来源：苹果文档 [Sending notification requests to APNs](https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns)、[Pushing background updates to your app](https://developer.apple.com/documentation/usernotifications/pushing-background-updates-to-your-app)、[Extending your app's background execution time](https://developer.apple.com/documentation/uikit/extending-your-app-s-background-execution-time)、[Declaring your actionable notification types](https://developer.apple.com/documentation/usernotifications/declaring-your-actionable-notification-types)（均用文档站 JSON 读了正文）。

### 4.1 能依赖什么、不能依赖什么

| 事实（原文意思） | 对我们 |
|---|---|
| 后台通知（静默推送）是低优先级，“the system doesn't guarantee their delivery”，会节流，“don't try to send more than two or three per hour”；App 被强制退出时系统丢掉暂存的后台通知；醒来后只有 30 秒 | **不能用静默推送保持连接或同步状态**。设备列表的在线状态要在 App 前台时靠长连接订阅（`N-devices.md` 的 S2），后台靠可见推送唤起用户 |
| APNs 是尽力而为服务，可能乱序；离线时最多存 30 天，且**每个 App 只存一条**（通常是最新一条） | “Agent 要你批准”这种有时效的推送要设 `apns-expiration` 为很短（批准窗口多久就多久），到期不补发；多条合并成一条“有 N 项待处理” |
| `apns-priority`：10 立即发，5 按省电考虑发；通知体最大 4 KB | 批准类用 10 和 `alert` 类型；4 KB 足够放“设备名 + 事项编号”，详情打开 App 再取 |
| 可操作通知：在通知上放按钮，点按钮后 App 在后台处理（不用打开 App） | 做“批准/拒绝”按钮是可行的。**高风险动作（删文件、花钱、执行命令）不要只靠锁屏按钮**：设成需要解锁（`authenticationRequired`）或点击后进 App 里看详情再批准【推断，属于安全设计，和 AI Agent 调研的“风险分级”对齐】 |
| App 进后台时只有 5 秒，之后挂起；可以用后台任务申请额外时间，不及时结束会被系统终止 | 回到后台的会话一律视为暂停；回前台立即重连、请求关键帧 |
| 4.5.4：推送不能是必需功能，不应带敏感信息，不得营销（除非明确同意） | 通知正文不带命令内容、路径、密钥片段；“批准”事项的细节放在 App 内、需要登录后拉取；单独提供开关 |
| 2.5.4：后台服务只能用于其本来用途 | 不为“保持后台连接”申请 VoIP、音频、定位 |

### 4.2 服务端要做什么（`peerdesk-server` 目前没有任何推送能力）

1. 设备表里新增 iOS 推送令牌（每账户多个），登录/换机/卸载（APNs 反馈）时更新。
2. 事件源：设备离线/上线（S2）、设备自报“需要处理”（S3：权限失效、卡在 FileVault 预启动、无人值守没开）、Agent 审批请求（核心用户功能设计的 P2：托管 MCP + 审批收件箱）。
3. **自托管服务器怎么办**【推断，重要】：APNs 的认证密钥属于我们的开发者团队，自建服务器的管理员拿不到，也不应该拿到。所以自托管用户要推送，只能把“推送请求”交给 PeerDesk 云上的推送转发（只带令牌和一句固定文案，不带内容）。这一点要写进隐私政策和自托管文档；不用转发的自托管用户在 iOS 上只有前台状态，没有推送。
4. APNs 请求是 HTTP/2 + 令牌认证（JWT）。服务端“只发两个二进制、静态链接 musl”的约束下，是否把 HTTP/2 客户端放进 `peerdesk-server`，还是做一个单独的云端推送服务（不属于开源二进制），见问题 6。

### 4.3 iOS 后台保持连接

做不到，也不要硬做：App 在后台约 5 秒后挂起（文档原文）。可行的产品形态：
- 前台：长连接，在线状态实时。
- 后台：靠可见推送叫醒用户；点击后直接进入对应设备的会话（需要 `UNUserNotificationCenter` 的点击回调带上设备编号）。
- 会话中切到别的 App：保持“最后一帧”，回来时秒连；超过一定时间（比如 30 秒）自动断开，省中继流量（成本视角，和赚钱方案第 3.4 节一致）。

---

## 5. v1 建议功能清单

按“能付费的用户愿意用”的最小范围，核心用户是“Mac mini/Windows/Linux 常驻跑 Agent、想从手机看一眼、点确认、接管”。

### v1（目标：TestFlight 外部测试 → App Store）

| 类 | 功能 | 说明 / 依赖 |
|---|---|---|
| 账户 | 登录（邮箱或通行密钥 + Sign in with Apple；GitHub 作可选）；退出；账户删除；订阅状态显示；恢复购买 | 依赖账户系统（赚钱方案第 3–4 周）；账户号不变，计费挂账户 |
| 订阅 | StoreKit 2：Plus、Pro 两个自动续期订阅；`appAccountToken`；服务器通知 | 第 2.2 节 |
| 设备 | 账户设备列表：名称、系统图标、在线/离线/最后在线、有人在控制、无人值守状态；订阅式刷新 | 依赖 `N-devices.md` 的 S1、S2、S3 |
| 连接 | 点设备一键连；连接码直连（不登录也能用）；P2P 优先、中继兜底；失败给原因和办法 | 复用现有信令和打洞 |
| 画面 | 硬解 H.264；横竖屏；捏合缩放和平移；全屏/安全区；离开 App 后回来快速恢复 | 第 3.3 节 |
| 触控 | 两种模式：**直接点击**（点哪是哪）、**触控板模式**（手指移动光标）；手势：单指点 = 左键，双指点 = 右键，长按拖动 = 拖拽，双指滚动，捏合缩放，三指手势留给系统 | 对标微软的两种模式（第 1 节）；手势表要做成可预览、可改的设置 |
| 键盘 | 软键盘 + **快捷键栏**（Esc、Tab、Ctrl、Alt、Win/Cmd、方向键、F 键、常用组合 Ctrl+C/V、Ctrl+Alt+Del 之类）；外接键盘（含 Cmd/Ctrl 映射、全部快捷键转发）；iPad 外接鼠标/触控板指针 | Agent 用户要在手机上输命令，输入体验决定留存 |
| 剪贴板 | 文本剪贴板双向 | 沿用现有 `clipsync` |
| 状态 | 工具条：延迟、帧率、码率、丢包；网络差时显示原因（本机到路由器还是公网）和建议，不弹窗 | CLAUDE.md 约定；沿用 `QualityHook` |
| 通知 | 设备离线/回来、设备需要处理（权限失效等）；有审批功能后加“Agent 等你批准”（批准/拒绝按钮，高风险进 App 内批准） | 第 4 节；推送要先有服务端 |
| 安全 | Face ID/Touch ID 锁 App；退出后台自动断开；连接密码存钥匙串 | 远程控制类 App 的基本要求【推断】 |
| 其他 | 英文界面（默认）+ 简体中文；诊断日志导出；崩溃日志上传（沿用 `crash-upload`，要在隐私说明里写清楚） | |
| 合规 | 隐私清单、隐私政策链接、本地网络用途说明、IPv6-only 测试、演示主机和账号 | 第 2、3 节 |

### v1.1（上架后 4–6 周）

- 多显示器切换（主机端枚举显示器已有的话）、Unified 缩略。
- 文件：用共享菜单把照片/文件发到主机，从主机下载（沿用 `files.rs` 协议）。
- 终端视图（文本流，不走视频，对 CLI 类 Agent 更省流量也更清晰）——对手 Workbench 1.4 已有，需要协议侧新增（核心用户功能设计里的“本机 MCP”思路之外的新通道），**要产品负责人先决定**（问题 5）。
- 通知里的“批准/拒绝”按钮、Live Activity/锁屏组件显示“N 台在线、M 项待处理”（ActivityKit 的能力和限制本次没核实【未核实】）。
- iPad 专门布局、Stage Manager、外接显示器输出。
- Siri 快捷指令/App Intents：“唤醒我的 Mac mini”“连到 XX”。
- 语音输入（Jump、Workbench 都有，用系统听写即可）。

### 以后

- 音频（需要先确认 2.5.4 下的后台策略）、Apple Pencil 的压感、Apple Watch、visionOS、Android。
- 会话缩略墙（几十台）、批量操作（核心用户功能设计的 P3）。
- 在 App 里和托管 MCP 端点的审批、审计日志查看。

**不做**：被远程控制（苹果不允许，已有结论）、在 App 里做商店式界面（4.2.7(d)）、托管模型（赚钱方案已决定）、后台常驻连接。

---

## 6. 没有 Mac 的 Linux 开发环境里怎么做

结论：**代码在 Linux 写，构建、签名、上传都在 macOS 机器上。** 能在 Linux 上做的只有“纯逻辑”的单元测试。

### 6.1 能在 Linux 上做什么

- `core`、`net`（不含 iOS 专用）、协议、推送服务端、账户服务：全部和现在一样在 Linux 跑，覆盖率门槛不变。
- iOS 专用的 Rust 代码（`crates/ios`）里，不碰系统接口的逻辑（状态机、接口数据结构）写成纯 Rust、用假 sink 在 Linux 单元测试。
- **交叉编译到 iOS 目标需要苹果 SDK**（C 头文件和链接器，`ring` 里有 C/汇编代码、`cc` 也会找 Apple 工具链）。在 Linux 上做不到，要在 macOS 上编【推断，没有在本环境试过】。

### 6.2 CI 方案

| 步骤 | 做法 | 来源/状态 |
|---|---|---|
| 编译 | GitHub Actions 的 macOS 机器：装 Rust iOS 目标（Tier 2，见 3.4）→ `cargo build --target aarch64-apple-ios` 和 `aarch64-apple-ios-sim` → 打成 xcframework → `xcodebuild` | 常规做法【未核实命令】 |
| 费用 | 页面把 macOS 小型机列为每分钟 $0.062（3 核或 4 核，M1/Intel）、12 核 $0.077、M2 Pro 5 核 $0.102；**页面没写公开仓库是否免费，也写了“Larger runners are not free for public repositories”**。本仓库是否公开以及标准 macOS 机在公开仓库是否免费【未核实】 | [GitHub 计费](https://docs.github.com/en/billing/reference/actions-minute-multipliers) |
| 替代 | **Xcode Cloud**：开发者计划含每月 25 计算小时；之后 100 小时 $49.99、250 小时 $99.99（[Xcode Cloud](https://developer.apple.com/xcode-cloud/)）。自定义构建脚本支持情况页面没写【未核实】 | |
| 签名和上传 | 用 App Store Connect API 密钥（由产品负责人在 Connect 里生成，放 GitHub Secrets）；`fastlane`/`xcrun altool`/`xcodebuild -exportArchive` 上传 TestFlight | 常规做法【未核实命令】 |
| 标记 | 按 CLAUDE.md，新增提交标记 `[ios]`：只在 macOS 机上编译 iOS 并跑模拟器测试；`[ios][pkg]`：出 TestFlight 构建。仍遵守“先本地后 CI、攒着推” | 建议 |

### 6.3 模拟器测试

- 模拟器用 `aarch64-apple-ios-sim`（Apple Silicon 的 macOS 机）。可以测：协议、连接、信令、界面流程（XCUITest）、IPv6-only（macOS 的共享网络里能建 NAT64 网络，【未核实具体步骤】）。
- 模拟器里 VideoToolbox 硬解和真机不一样，**性能、延迟、画质不能用模拟器下结论**【推断，要在 P0 实测确认】。
- **CLAUDE.md 要求“不造假”**：e2e 里给模拟器连的主机要用真实屏幕内容（沿用 `e2e/content.rs` 的真实录屏素材，不是合成画面）。在同一台 macOS 机上起 `peerdesk-server` 和主机端进程，iOS 模拟器 App 连回环。断言：首帧时间、连续解码帧数、无冻结。
- 真机指标（解码耗时、显示延迟、掉帧、发热、耗电）只能靠真机，产品负责人的 iPhone 加 TestFlight；要在 App 里做一个“诊断”页，把这些指标导出成日志，和桌面端同一份指标口径。

### 6.4 TestFlight

（[TestFlight 概览](https://developer.apple.com/help/app-store-connect/test-a-beta-version/testflight-overview/)，已核实）：
- 内部测试员最多 100 人（App Store Connect 里有权限的用户）；外部测试员最多 10,000 人，可用邮件或公开链接邀请。
- 构建 90 天过期。
- 外部测试要过 Beta App 审核，首个构建会被送审，后续构建不一定要完整审核。
- 流程：产品负责人建 App 记录和 Bundle ID → 生成 API 密钥 → CI 上传构建 → 内部测试员先试（不用审核）→ 提交外部测试审核 → 公开链接发给预售买家和 OpenClaw 社区的测试者。
- 这和赚钱方案第 2 周的预售、第 7–8 周的手机审批教程对得上：**TestFlight 公开链接可以作为“手机端”的测试版交付物**。

### 6.5 需要产品负责人（或你有没有 Mac）确认

- 开发时是否有一台 Mac（核心用户本来就用 Mac mini；一台跑 Xcode 的 Mac 能大幅缩短真机调试的循环：不然每次改动要走 CI 加 TestFlight，一次循环至少十几分钟到几十分钟【推断】）。
- Apple 账号用组织账号，包含 D-U-N-S（赚钱方案 4.3 节已写）；Bundle ID、推送密钥（.p8）、App Store Connect API 密钥由他生成。

---

## 7. 官网

### 7.1 同类官网结构（2026-11-27 读）

| 站 | 首页卖点怎么写 | 价格页 | 下载页 | 文档 | AI Agent 落地页 | 备注 |
|---|---|---|---|---|---|---|
| [Tailscale](https://tailscale.com/) | “Secure Connectivity for AI, IoT & Multi-Cloud”；产品卡片（Business VPN、PAM、Securing AI…）、60,000 家企业社会证明、IT/安全/DevOps/工程四个标签页、开发者推文 | 有（导航里） | 导航里的 Download | Docs、Blog、Changelog | **有**：“Secure Access to AI”“Secure AI Agent Connectivity”“Securing AI（Aperture）” | 大站，不要照抄结构；学它的“按角色讲同一产品” |
| [Parsec](https://parsec.app/) | “Connect to your … from anywhere”；“Remote desktop reimagined”；4K 60fps、近零延迟；画质对比滑块；SOC 2 Type 2 | 套餐卡片（个人免费、Warp、Teams、Enterprise），14 天试用 | Download | Support、Blog；Teams API 文档 | 无 | 页脚有“Do Not Sell or Share My Personal Information”“Your Privacy Choices”（美国隐私合规入口） |
| [RustDesk](https://rustdesk.com/) | “The Fast Open-Source Remote Access and Support Software”；换掉 TeamViewer/AnyDesk 的自托管叙事；Docker 三步安装；1,000 多位自托管用户的场景占比；30M+ 下载、113K+ 星 | 有 | Download 页**没列 iOS**，只有 Windows/macOS/Linux/Android（页面如此，但 iOS App 确实在 App Store 上） | /docs、Blog | 无 | **11 种语言的语言选择器** |
| [Jump Desktop](https://jumpdesktop.com/) | “Remote Desktop That Feels Local”；六大能力；用例卡片（远程工作、创作、编程、游戏、远程支持、**AI Agents**）；性能区（帧率、硬编、虚拟显示器、外设、色彩） | 有 | 首页有 Windows/macOS/iOS/Android(Beta)/Linux/Web 下载按钮 | 文档、Cloud API、Teams 安全白皮书、Trust Center | **有**（首页一整节 + ai-agents.html，见 1.3） | 页脚有服务状态、社区论坛 |
| [Screens Connect](https://www.screensconnect.com/) | 极简：三步（创建账户、装 helper、登录 Screens） | 免费服务，不列价 | 装 Mac/Windows helper | 帮助 | 无 | 没有营销感，是“账户 + 下载”页 |
| [Astropad Workbench](https://astropad.com/workbench/) | “Full remote Mac access. Free for 20 minutes per day.”；“no subscription or credit card”“setup under a minute”；4.9 App Store 评分 | 首页一行：升级无限用量 $99.99/年 | 首屏两个按钮：Download for Mac / Download for iOS | Learn 栏有“headless Mac mini 指南”等博客 | 产品本身就是为 Agent 做的 | 首屏就是下载按钮和免费额度 |

读法：
1. **所有同类都把“下载”放在导航最显眼的位置**；Workbench 首屏直接放两个下载按钮，我们学这一点。
2. **价格页里“免费档能干什么”是第一屏**（Parsec 免费个人、Workbench 免费 20 分钟、Tailscale 免费），和赚钱方案的 Free 档一致。
3. **AI Agent 落地页：Jump 和 Tailscale 都有**；Workbench 是整站主题。我们不是第一个；要用“真实指标 + 开源 + 三平台”去写，不是“AI 也能用”。
4. **安全信息是独立一块**（Parsec 的 SOC 2、Jump 的 Trust Center）。我们没有 SOC 2，用“端到端加密 + 开源可审计 + 中继看不到内容”代替（赚钱方案已定）。
5. **RustDesk 做多语言**，美国市场英文为主，中文放次要。

### 7.2 技术栈和托管建议

**推荐：Astro（静态输出）+ Starlight（文档）+ Cloudflare Workers 静态资源。**

| 项 | 建议 | 理由和来源 |
|---|---|---|
| 站点框架 | **Astro**，营销页用 `.astro`，文章用 MDX | 内容为主、少脚本，适合 SEO；内建多语言路由：`i18n` 配置里列 `locales` 和 `defaultLocale`，默认语言无前缀、其他语言带 `/语言/` 前缀，`fallback` 可用默认语言兜底缺页（[Astro i18n](https://docs.astro.build/en/guides/internationalization/)） |
| 文档 | **Starlight**（Astro 官方文档主题）放在 `/docs/` | 内建多语言：`defaultLocale` + `locales`，未翻译页显示默认语言并带提示，界面字符串自带“Chinese”，搜索是 Pagefind（[Starlight i18n](https://starlight.astro.build/guides/i18n/)） |
| 备选 | Next.js 静态导出 | 能做，但对纯内容站偏重；和现有 `web/` 一样是 React 的好处是共用组件，我们的营销页交互很少，不需要 |
| 托管 | **Cloudflare Workers 静态资源**（`assets.directory` 指向构建产物，不需要 Worker 脚本） | Cloudflare 文档：“If you are starting a new project, use Workers instead of Pages. Pages continues to work, but new features and optimizations are focused on Workers.”；静态资源请求“free and unlimited”，存储无额外费用；免费计划单次部署最多 20,000 个文件、单文件 25 MiB（[Workers 最佳实践](https://developers.cloudflare.com/workers/best-practices/workers-best-practices/)、[计费和限制](https://developers.cloudflare.com/workers/static-assets/billing-and-limitations/)、[限制变更](https://developers.cloudflare.com/changelog/post/2025-09-02-increased-static-asset-limits/)）。我们的域名、中继、TURN 以后可能都在 Cloudflare，一处管理 |
| 备选托管 | Vercel、GitHub Pages | 本次没有取这两家的当前限制和价格【未核实】；GitHub Pages 最省事但放不了重定向规则和自定义头、没有预览环境（常识【未核实】） |
| 安装包分发 | **GitHub Releases 存文件**（“Each file included in a release must be under 2 GiB”，“There is no limit on the total size of a release, nor bandwidth usage”，[GitHub 文档](https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases)），**官网 `/download` 用稳定的跳转链接指向最新版**；自动更新器用同一份文件 | 零成本，和 `[pkg]` 构建流水线一致 |
| 将来迁到 R2 | 存储 $0.015/GB·月，免费 10 GB·月；“There are no charges for egress bandwidth for any storage class”；读操作 $0.36/百万次，免费 1,000 万次（[R2 定价](https://developers.cloudflare.com/r2/pricing/)） | 触发条件：想要下载计数、国内外下载速度（GitHub 在部分地区慢，本次没测）、发布后立刻回滚、增量更新包。每个版本约 100 MB × 3 平台，月 5,000 次下载的流量约 1.5 TB，R2 仍是零流量费【推断】 |
| 域名 | 营销站 `peerdesk.ai` 和 `www`；管理控制台 `app.peerdesk.ai`；文档 `peerdesk.ai/docs`；状态页 `status.` | 赚钱方案写“peerdesk.ai 已定”，商标未核实 |
| 多语言 | **英文为默认（无前缀），简体中文放 `/zh/`**；所有页面双语对应，缺页回退英文；`<link rel="alternate" hreflang>` 要每页列出所有语言版本（含自己）、用完整 URL、互相指向、加 `x-default`（[Google 说明](https://developers.google.com/search/docs/specialty/international/localized-versions)，已核实） | 美国市场优先，英文是主站；中文不是美国用户的需求，是产品负责人和中文开源社区的需求，成本低就做，但**不要因为翻译拖慢英文上线** |
| 与 `web/` 的关系 | `web/` 是**已登录的管理控制台**（React 18 + Vite 5，页面 Admin、Cluster、Usage，没有路由库和 SEO 需求）。**不合并**：营销站要预渲染和 SEO，控制台是需要登录的单页应用，发布节奏和安全面都不同。营销站只放链接：登录/账户/账单入口指向 `app.` | 账户、订阅、设备管理、Stripe 客户门户页面放在 `app.` 一侧（赚钱方案第 3–4 周的账户系统）；设计令牌（颜色、字体）可以抽成一份共用 CSS 变量文件，避免两边长得不一样 |
| SEO | 每页独立标题和描述；`sitemap.xml`（Astro 有官方集成；与 i18n 的 hreflang 联动是否开箱即用，本次页面没写【未核实】）；`robots.txt`；结构化数据（SoftwareApplication）；Open Graph 图；性能（Lighthouse 目标另定） | 内容 SEO 靠文档和教程：“无显示器 Mac mini 远程”“给 OpenClaw/Claude Code 加手机接管”（Astropad 页脚就有 headless Mac mini 指南；渠道与赚钱方案第 4.1 节一致）。关键词搜索量本次没查【未核实】 |
| 统计和隐私 | 先用无 Cookie 的统计（具体产品待选），避免 Cookie 横幅；漏斗指标（访问→下载→账户）要埋点（赚钱方案 5.1） | 如果上 Cookie 或广告追踪，需要美国各州隐私入口（Parsec 页脚的做法），适用门槛要问律师【未核实】 |

### 7.3 v1 页面清单和每页要点

| 页面 | 路径 | 要点 |
|---|---|---|
| 首页 | `/` | 首屏：一句话 + 两个下载按钮（Mac、Windows，另有 iPhone、Linux）+ 一行免费额度（学 Workbench）；真实演示视频或截图（不用合成画面）；三个卖点：画面（静止后秒清晰、卡顿指标）、连接（P2P 优先、中继兜底、端到端加密）、给 Agent 用的人（设备健康、手机接管）；对比表（对 Tailscale+VNC、RustDesk、Jump、Workbench）；e2e 指标；开源和自托管；定价摘要；FAQ |
| 下载 | `/download` | 识别系统，高亮对应平台；macOS 签名公证、Windows 签名说明，诚实写首批用户可能看到 SmartScreen 提示（赚钱方案已写）；Homebrew cask、winget、apt 命令；校验值；iOS 的 App Store 徽章；TestFlight 公开链接（上架前）；旧版本和更新日志入口 |
| 价格 | `/pricing` | Free / Plus / Pro / Team 四档（以产品负责人最终定价为准）；每档“包含什么”和“限制”（设备数、中继流量、审计天数）；“自托管永远免费”；常见问题（订阅在网页买和 iPhone 内购买的区别、取消、退款、中继用完怎么办）；iOS 内购价如果与官网不同要写清楚 |
| AI Agent 落地页 | `/agents` | 对 Mac mini/Windows/Linux 常驻 Agent 的人讲“看一眼、点确认、接管”；三个问题：卡在权限弹窗、重启后失联、无显示器糊分辨率；一键配置 Claude Code/Codex/OpenClaw 的 MCP 示例（等本机 MCP 做完再放，不写没有的功能）；安全：默认不暴露公网、每步可审计、BYOK 不卖模型；与 Jump、Workbench 的差异（三平台、开源、指标） |
| 文档 | `/docs/…` | 快速开始（五分钟连上第二台电脑）；无显示器 Mac mini 指南；Agent/MCP 配置；自托管服务器；iOS 使用；故障排查（原因 + 解决办法，沿用 CLAUDE.md 的“诚实”）；FAQ |
| 安全 | `/security` | 加密方式、中继看不到什么、日志存什么存多久、漏洞披露、开源可审计；SOC 2 现状如实写“尚未”（赚钱方案已定） |
| 更新日志 | `/changelog` | 自动从发布说明生成 |
| 博客 | `/blog` | v1 先放 2–3 篇：为什么重写远程桌面的传输层（Show HN 用）、无显示器 Mac mini 怎么远程、实测指标；RSS |
| 关于/联系/支持 | `/about`、`/contact`、`/support` | 支持邮箱（App Store 审核要求有支持网址，【推断：Connect 里的字段，页面未核实原文】）、滥用举报入口（赚钱方案已列） |
| 法律 | `/terms`、`/privacy`、`/acceptable-use`、`/subprocessors`（可晚一点） | **隐私政策必须有**（5.1.1(i)）；条款含商用/非商用边界（Parsec、Tailscale 都分）；可接受使用政策含“不得用于未授权访问”；以后加数据处理协议（Team 档用） |
| 状态页 | `status.` 子域 | Jump 有服务状态页；可晚一点，用第三方托管 |
| 404、品牌素材 | | 简单 |
| 应用关联文件 | `/.well-known/apple-app-site-association` | 通用链接（点官网链接直接开 App）才需要，v1 可不做【推断】 |

### 7.4 需要产品负责人提供的素材

1. **文案方向**：一句话定位（给常驻 Agent 的人，还是通用远程桌面？）、语气、对竞品的比较口径（能写“比 X 好”的必须有数据）。
2. **价格**：Free/Plus/Pro/Team 最终数字和限额；创始会员文案；iOS 内购价是否同官网（问题 3）。
3. **截图和视频**：真实界面截图（桌面、iPhone 真机）、一两段真实的无显示器 Mac mini 接管演示视频。不能用合成画面（CLAUDE.md）；App Store 要求的截图尺寸规格本次没核实【未核实】。
4. **法律页面**：隐私政策、服务条款、可接受使用政策——**需要律师审**（赚钱方案预算 $1–3k，【推断】）；公司主体、注册地址、联系邮箱；CCPA 等是否适用。
5. **品牌**：Logo、色板、字体、应用图标（1024×1024 的 App Store 图标）、域名和商标检查（赚钱方案 4.3 节“未核实”）。
6. **账号和平台**：Apple Developer 组织账号、Bundle ID、App Store Connect 的 API 密钥、推送密钥（.p8）、Cloudflare 账号（或授权）、Stripe。
7. **社会证明**：现在没有客户证言，不要编。可以放 GitHub 星标、e2e 指标、测试者的真实反馈。

---

## 8. 路线对 90 天计划的影响（建议）

| 赚钱方案的周 | iOS / 官网对应动作 |
|---|---|
| 1 | 官网最小版（首页、预售购买链接、隐私政策草稿）；Apple 组织账号和 D-U-N-S 同时开始（最长 5 个工作日，赚钱方案已写） |
| 2–4 | iOS P0（真机出画面）；官网价格页和下载页骨架；账户系统 v1 |
| 5 | 桌面签名发布；官网下载页接真实包；iOS P1 |
| 6 | 中继配额计量；iOS 工具条显示用量 |
| 7–8 | iOS TestFlight 外部测试（公开链接）；推送和 Agent 审批入口；官网 `/agents` 页和文档 |
| 9 | Show HN：官网首页、博客、文档、指标都要在此前准备好 |
| 10–13 | App Store 公开上架（含被拒一轮缓冲）；iOS 订阅正式开卖；官网中文版 |

---

## 9. 需要产品负责人决定的问题

每条附建议：

1. **iOS 上订阅怎么办**：同意“iOS 同档位提供内购，账户订阅跨设备通用”吗？（审核硬要求，见 2.2）建议同意。
2. **要不要在美国区 iOS App 里放“官网购买”链接**：现行指南文本允许，但诉讼未结，规则可能变。建议 v1 不放，只做 IAP；以后看规则。
3. **iOS 内购价**：和官网同价（接受 15% 佣金下毛利约 77%），还是 iOS 加价约 15–30%（和官网价对冲）？苹果对“引导用户去更便宜的渠道”的限制见 3.1.3 开头；价格不同本身不违规，但 App 内文案不能暗示去官网买（非美国区）。建议先同价，看数据。
4. **小企业计划**：是否马上申请 15% 佣金（上年收入 ≤100 万美元）？建议申请。
5. **终端视图（文本流）要不要进 v1.1**：对手 Workbench 1.4 已有，对 CLI Agent 用户更合适，但要加新协议通道。建议评估，先不进 v1。
6. **推送服务放在哪**：放进 `peerdesk-server`（要加 HTTP/2 客户端，影响“静态链接、只发两个二进制”），还是托管云单独的推送转发服务（开源版不含，自托管用户的推送要走我们的转发）？建议后者，隐私政策里写清楚。
7. **新增 `crates/ios` 需要 `unsafe`**：放宽 CLAUDE.md 的“`unsafe` 只允许在平台层和媒体层系统接口文件”的范围，允许 FFI 层，且覆盖率排除写理由？建议同意，范围只限这一个 crate。
8. **有没有 Mac 做开发**：没有就完全靠 CI + TestFlight，迭代慢很多。建议买或借一台 Mac mini（也能当 Demo 主机和 e2e 机）。
9. **出口合规和隐私**：自带 TLS/QUIC 的 App 在美国提交时怎么填加密声明？要不要年度报告？建议问律师/会计，上架前定。
10. **Demo 主机和审核账号**：谁维护？建议专用的 Mac mini + Windows 各一台，一直在线，账号写进审核备注。
11. **Android 近期做不做**：决定了 Flutter 是否值得（第 3.2 节）。建议近期不做，v1 后看数据。
12. **官网先英文还是双语同时**：建议英文先上，中文在 TestFlight 公开前补齐。
13. **文案定位**：官网首页以“常驻 Agent 的人”还是“通用远程桌面”？影响 4.2.7 的措辞（第 2.1 节）和对 Jump 的差异化。
14. **App 里的自定义服务器地址**：v1 是否允许填自建服务器（对自托管人群有吸引力，推送转发和测试面会变大）？建议放在“高级设置”，v1 可延后。

---

## 10. 未核实清单

1. 4.2.7 对“通用远程桌面”不适用，这是按条文字面的读法【推断】，没有苹果的明确说明；最终以首次提交审核结果为准。
2. 美国区外部购买链接的当前规则、佣金、最高法院进展：只有 2026-04 的报道和搜索摘要，TechSpot 和 TechCrunch 日期互相不一致；提交前必须重查。
3. `appAccountToken`、StoreKit 服务器通知的原文措辞没取到（页面脚本渲染）；5.6 开发者行为准则没读到；指南后半段（5.2.5 之后）没读。
4. Stripe 费率（2.9% + $0.30）【赚钱方案已标未核实】；自建中继单价没有来源。
5. 同行 App 的触控模式、多显示器、剪贴板细节：Jump 只读到 App Store 页描述；Screens、TeamViewer、CRD、AnyDesk 的触控模式和快捷键栏没读到原文；Windows App 的 iOS 手势表只拿到搜索摘要；Parsec 无 iOS 版是二手证据（帮助中心 403）。
6. “没有同行 App Store 页面提推送通知”：页面没提不等于 App 里没有，要实机装一遍；Jump 的 iOS 手机查看 beta 的具体能力也没实测。
7. Screens、Workbench 的价格各页互相矛盾，Workbench 官网 $99.99/年与 9to5Mac 的 $79.99/年；以购买页为准。
8. `AVSampleBufferDisplayLayer` 路径的实际延迟、卡顿、发热：没有数据，P0 要量。iOS 各机型 H.264 硬解的能力范围未核实。
9. 在 Linux 上是否完全不能交叉编译 iOS 目标（`ring` 等）、`cc` 找不到苹果工具链：没试。
10. GitHub Actions 标准 macOS 机在公开仓库是否免费、仓库是否公开：没查到。Xcode Cloud 是否支持自定义脚本：页面没写。
11. 苹果 Live Activity（ActivityKit）、通知服务扩展的限制和审核要求：没读。
12. App Store 截图尺寸、支持网址字段、年龄分级问卷的具体要求：没读。
13. 加密出口合规（`ITSAppUsesNonExemptEncryption`、BIS 自分类报告）：没查。
14. 隐私清单里 Rust 标准库和 tokio 触发哪些“必填理由 API”：要等第一次构建上传后看苹果邮件，或自己用工具扫。
15. 我们现有的 ICE/打洞层是否有“网络变化后重新收集候选并重绑套接字”的入口：没读完代码。
16. 官网：Vercel、GitHub Pages 的当前限制与价格；Astro 的 sitemap 集成和 i18n/hreflang 的联动；关键词搜索量；Cookie/隐私入口的法律门槛。
17. OpenGL ES 在 iOS 上被弃用的说法（常识）没有取原文。
18. 时间估算全部是【推断】，没有按本仓库的实际进度验证；要等 P0 结束后重估。

---

## 附：本次实际打开的一手来源

- 苹果：[App Review Guidelines](https://developer.apple.com/app-store/review/guidelines/)、[Upcoming Requirements](https://developer.apple.com/news/upcoming-requirements/)、[Small Business Program](https://developer.apple.com/app-store/small-business-program/)、[TestFlight 概览](https://developer.apple.com/help/app-store-connect/test-a-beta-version/testflight-overview/)、[Xcode Cloud](https://developer.apple.com/xcode-cloud/)；文档站 JSON 读到正文的：APNs 请求、后台通知、后台执行时间、可操作通知、TN3151 网络 API 选型、TN3179 本地网络隐私。
- 同行：Jump（首页、定价页、ai-agents 页、App Store 页）、Screens（帮助页、MacStories、Screens Connect）、RustDesk（App Store、官网）、Windows App（App Store）、AnyDesk（App Store）、Astropad（Workbench 页、App Store、9to5Mac）、Tailscale 首页、Parsec 首页。
- 技术：uniffi-rs、flutter_rust_bridge、egui eframe README、quinn issue 列表与 `Endpoint` 文档、Rust 平台支持页、GitHub 计费页、GitHub Releases 文档。
- 官网：Cloudflare Workers 最佳实践、静态资源计费和限制、R2 定价、Astro i18n、Starlight i18n、Google 多语言说明。
- 仓库：`Cargo.toml`、`crates/media/src/vt.rs`、`crates/net/Cargo.toml`、`crates/client/Cargo.toml`、`web/package.json`、`N-devices.md`（devices 分支）。
