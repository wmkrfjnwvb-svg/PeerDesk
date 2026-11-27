# PeerDesk 核心用户功能设计：设备管理、MCP、无显示器主机（截至 2026-11-26）

标记：【已核实】本次打开了官方或一手原文；【二手】搜索摘要、厂商博客、第三方汇总；【未核实】没找到可靠来源；【推断】我们自己的判断。
承接：`PeerDesk 美国市场赚钱方案.md`（定价、渠道）、`PeerDesk AI Agent 业内调研.md`（安全分级）、`docs/research/G-unattended.md`（三平台无人值守）。已有结论不重复，只写新增和需要修正的地方。

**取证上的限制（诚实）**：Reddit 在本环境完全打不开（WebFetch 被拦），claudeissues.com 也被网络策略挡了，所以"痛点按频率排序"不是统计，而是按"独立来源出现的次数和严重度"排的，原话来自 GitHub issue、Apple/MacRumors 论坛、HN、厂商博客里转述的用户问题。r/macmini、r/LocalLLaMA 的原帖没读到，要产品负责人或下一轮用能上 Reddit 的环境补。

---

## 0. 结论先行

1. **做什么**：把三块做成同一个故事——"这台常驻主机现在是不是健康的，不健康时我在手机上一步修好"。设备列表不是地址簿，是**主机健康面板**：在线、能不能被远程（权限齐不齐）、Agent 是否卡在弹窗、系统是否会在下次重启后失联。
2. **为什么**：用户最痛的不是"连不上"，而是**三件事反复发生**（见第 1 节）：①重启或断电后主机卡在 FileVault 预启动界面，人不在就彻底失联；②Agent 卡在系统权限弹窗/批准框，无人值守时永远等；③更新后屏幕录制/辅助功能授权失效，又要有人到机器前点。Tailscale+VNC 解决不了这三件，Astropad Workbench（只 Apple）在抢这块。
3. **先后顺序（第一阶段小而能闭环）**：
   - **P1（约 6–8 周）**：设备健康面板（账户设备列表 + 在线/最后在线/权限缺失/需更新）+ 无显示器 Mac 的"体检与修复向导"（检测 FileVault、自动登录、断电自启、睡眠、权限，逐项说原因和一步修法）+ `peerdesk mcp` 只读 + 受控输入两档，本机 stdio，远程走已认证通道。**不做**：虚拟显示器、托管 MCP 端点、批量操作。
   - **P2（再 6–8 周）**：托管 MCP 端点（OAuth）、手机审批收件箱、macOS 虚拟显示器、批量部署（一行安装 + 预置令牌）、标签与"只看异常"。
   - **P3**：几十台缩略墙、局域网自动发现、批量升级/重启、审计留存。
4. **规模补充（新要求：一个人管几十到几百台，局域网很多）**：原设计是"一个人 3–10 台"，**当前代码撑不住几百台**——在线状态查询每次最多 64 台（`MAX_PRESENCE = 64`，`presence` 分支 `signal.rs`），设备簿是本机文本文件 `devices.txt`，"同一网络"标签页写着"这个版本还不能发现同网络的电脑"（`ui-redesign` 分支 `i18n.rs`）。几百台要换成服务端权威的账户设备表 + 订阅式状态推送 + 标签/过滤 + 批量部署。第 7 节专门写。
5. **按什么计费**：**按账户（人）分档，设备数只作为宽松的软上限，只数"被控主机"，控制端不数**；规模用户用"每多 25 台主机 $X"的阶梯加购并封顶。不要走 TeamViewer Remote Access 那种"$24.90 含 3 台"的路（每台约 $8.3）。理由：主流对手里 Tailscale、Jump Desktop、Parsec 按人收费、设备不限或不数；真正的成本是中继流量不是设备数。对已有方案要改：Free 从 2 台升到 3 台主机，Plus 10→15，Pro 25→50，另加 Fleet 加购（第 8 节）。
6. **最需要先验证的一件事**：**ScreenCaptureKit 在 Mac mini 没接显示器时到底给不给画面、给多大**。已有 `G-unattended.md` 写的是"不接显示器 GPU 通常不创建桌面"（未核实），Astropad 博客说"Apple Silicon 自动创建 1920×1080 1x 显示器"（厂商说法）。两边矛盾，必须拿一台真 Mac mini 拔掉 HDMI 实测，再决定虚拟显示器的优先级。

---

## 1. 用户痛点（带原话和来源）

排序依据：独立来源数 × 一旦发生的严重度。

| # | 痛点 | 严重度 | 证据（原话/要点）和来源 |
|---|---|---|---|
| 1 | **重启或断电后失联：FileVault 预启动界面不联网** | 失联，只能人到场 | Astropad：开着 FileVault 时 Mac"sits at the FileVault prompt, preventing access to a network, screen sharing and SSH"；"after a reboot, FileVault requires a password at the pre-boot screen before the network even comes up"【二手，厂商博客】[FileVault 与无显示器 Mac mini](https://astropad.com/blog/filevault-headless-mac-mini/)、[无显示器设置指南](https://astropad.com/blog/headless-mac-mini-setup-guide/)。Apple 论坛有人写断电后"it will not boot up without me needing a password"，另有 Apple 工程师回复"Also useful if you set up ahead of time to make sure you have uptime even in the event of a kernel panic or power failure" [Apple 开发者论坛 831442](https://developer.apple.com/forums/thread/831442)【已核实】。Apple 论坛串：[Screen Sharing / SSH after power fail reboot](https://discussions.apple.com/thread/253682124)（只拿到搜索摘要，【二手】） |
| 2 | **Agent 卡在权限/批准弹窗，没人在机器前** | Agent 永久挂起 | Claude Code issue #80806（2026-07-24，Claude Code 2.1.218）：「This is the single blocker to fully phone-managed computer use. A headless/always-on machine driven via Remote Control can never complete the app-approval step without the user keeping terminal access around solely for this one dialog.」【已核实】[issue 80806](https://github.com/anthropics/claude-code/issues/80806)。issue #47796（2026-04-14）：「Running Claude Code as a 24/7 autonomous agent (via tmux + Telegram plugin) on a dedicated Mac mini. Computer-use is needed for native app automation, but the interactive prompt makes it unusable in headless/unattended mode.」「Currently forced to use Playwright MCP for web tasks only」【已核实】[issue 47796](https://github.com/anthropics/claude-code/issues/47796)。HN 评论（gdonelli，2026-04-10）：「once your machine's running headless 24/7, you lose visibility.」【已核实，经 HN Algolia API 取得】[HN 47712974](https://news.ycombinator.com/item?id=47712974)。市场信号：Port22、agy-remote、Leash、Nudge Remote 等"把权限提示转发到手机"的小产品已出现，Product Hunt 评论者提到"agents stalling on permission prompts"【二手，搜索摘要】 |
| 3 | **更新后授权失效，又要手动重授** | 每次更新都要人到场 | issue #41101（2026-03-30）：「Every Claude Code update will break Computer Use MCP permissions. The CLI binary is version-pinned … macOS Accessibility and Screen Recording permissions are tied to the specific binary. Update Claude Code, permissions are gone, back to the manual grant dance.」【已核实】[issue 41101](https://github.com/anthropics/claude-code/issues/41101)。OpenClaw 文档把"stale-grant"列为独立状态，并写"Unsigned or ad-hoc signed builds do not persist permissions"【已核实】[OpenClaw macOS 权限](https://docs.openclaw.ai/platforms/mac/permissions)。**这是我们的机会也是我们的责任**：PeerDesk 自己必须用稳定签名身份，否则自己也会制造这个痛点（赚钱方案 4.3 已写） |
| 4 | **macOS 的重启后/定期屏幕录制提示让远控 App"没法用"** | 重启后画面被挡 | Apple 开发者论坛（2024-08，Sequoia 15 beta）发帖者："useless, a specially for headless computers like my Mac mini"，Apple DTS 回复："ScreenCaptureKit is not available in the pre-login context … file a bug"（后来 14.4 起修复，见 3.4）【已核实】[论坛 756908](https://developer.apple.com/forums/thread/756908)。Sequoia 定期提示：beta 阶段从每周改成每月"Allow For One Month"，并取消了重启后重复提示【已核实，仅 beta 报道】[MacRumors](https://www.macrumors.com/2024/08/15/macos-sequoia-screen-recording-app-permissions/)。**macOS 15 正式版和 26 现在对常驻远控 App 的真实频率没有一手来源，必须实机量** |
| 5 | **无显示器时分辨率低、画面糊** | 体验差但可用 | MacRumors 用户："Without a monitor … the Mini would have a strange resolution … low resolution and pretty unusable"，要用 HDMI 假负载【二手，搜索摘要】[MacRumors 讨论](https://forums.macrumors.com/threads/headless-mini-boot.2337173)（页面本次 403）。Astropad："By default Apple Silicon machines create a low-resolution 1920×1080 1x display"，Retina 客户端上"will appear fuzzy"【二手，厂商】 |
| 6 | **暴露面和自己拼 Tailscale+VNC 的麻烦** | 安全事故、手机上难用 | Astropad："Port-forwarding Screen Sharing (5900) is a bad idea. Bots constantly scan"；"You can't interact with an AI agent's browser window over SSH"；系统 Screen Sharing"doesn't work from iPhone or iPad without third-party apps"【二手，厂商】。HN（2026-10）有人说"需要开着 Screen Sharing 才能点掉 macOS 权限弹窗"（转述，【二手】）[HN 49963457](https://news.ycombinator.com/item?id=49963457)。公网暴露的 OpenClaw 实例数见赚钱方案 1.2 |
| 7 | **睡眠：机器睡了就连不上** | 间歇失联 | Astropad："An always-on Mac mini can't be asleep when you try to reach it"，要开"Prevent automatic sleeping when the display is off / Wake for network access / Start up automatically after a power failure"【二手，厂商】。Apple 工程师也先问"can you assert that the machine will not be sleeping or hibernated?"【已核实】 |
| 8 | **多会话互相抢 computer-use 锁；一次只能一个会话控制电脑** | Agent 排队 | Claude Code 文档："Only one session at a time can use your computer … A second session's computer use fails with an error naming the session that holds the lock"【已核实】[Claude Code computer use](https://code.claude.com/docs/en/computer-use) |

对 PeerDesk 的含义：1、2、3 是"人不在场就修不好"的问题，正好是远程桌面该解决的；4、5、7 是一次性配置+持续体检；6 是安全卖点。

---

## 2. 同行对比表

价格口径：美元，年付折月，除非注明。免费档设备上限里，"主机"指被控端。

| 产品 | 设备列表显示什么 | 分组/标签 | 在线状态与上次在线 | 计费单位与档位 | 免费档设备上限 | 来源/标记 |
|---|---|---|---|---|---|---|
| **Tailscale** | Machines 页：设备、状态、可按"Last seen/连接状态"过滤；需审批的设备带"Needs approval"徽标置顶 | ACL 标签（tag）、用户；标签设备默认关闭密钥过期（普通设备默认 180 天过期） | 有连接状态和 last seen（列布局本次没读到） | **按用户（席位）**：Personal $0（≤6 用户）、Standard $8/用户、Premium $18/用户；**设备不限，所有档** | 不限设备（6 用户内） | [定价](https://tailscale.com/pricing)【已核实】；[设备审批](https://tailscale.com/kb/1099/device-list)【已核实】；[密钥过期](https://tailscale.com/kb/1028/key-expiry)【已核实（搜索摘要）】 |
| **Parsec** | 个人：电脑列表；Teams：管理面板，团队管理的电脑可分配给个人或组 | Teams 有组、共享团队电脑 | 未读到 | **按用户**：Free；Warp $8.33（月付 $9.99）；Teams $30（月付 $35）；Enterprise $45 | 页面未写数量上限 | [定价](https://parsec.app/pricing)【已核实】。商业使用须 Teams |
| **RustDesk Pro（自托管）** | 控制台设备、用户、地址簿；策略可下发到设备/用户/设备组 | 设备组；**一台设备只能在一个组**；策略优先级 设备>用户>设备组>默认 | 有（未逐项读） | **按用户+设备双维度**：Individual $11.88（1 用户 20 设备）、Basic $23.88（10 用户 100 设备）、Customized $23.88 起，**每多 1 设备 $0.12、每多 1 用户 $1.20**；只年付 | Free 档未列上限 | [定价](https://rustdesk.com/pricing)【已核实】；[策略](https://rustdesk.com/docs/de/self-host/rustdesk-server-pro/strategy/)【二手，非英文页摘要】 |
| **Jump Desktop** | Jump Desktop Connect 装在每台被控机（Win/Mac/Linux） | Access Groups：Teams Pro 5 个，Enterprise 不限 | 未读到 | **按用户**：个人免费（Win/Android/网页）；Mac/iOS 买断（Mac $34.99 为二手）；Teams Pro $7.99、Enterprise $15.99 | **电脑数不限，所有档**；并发连接不限 | [定价](https://jumpdesktop.com/pricing-plans.html)【已核实】。另：官方页写"AI agents can control up to 8 computers at once through Jump CLI"，Free/Pro 1 个虚拟显示器、Enterprise 4 个、视频带宽 20/60 Mbps |
| **Screens 5 / Screens Connect**（Edovia） | 本次未读到 | 未读到 | 未读到 | Screens 5：$3.99/月、$24.99/年、买断 $139.99（另一页 $119，冲突）；Screens Connect App Store 页 $159.99（含义不明） | 未核实 | [Edovia 帮助](https://help.edovia.com/en/screens-5/faq/s5-pricing)【二手，来源冲突】 |
| **Chrome 远程桌面** | 打开 remotedesktop.google.com/access 选"Access"，列表细节官方页没写 | 无 | 官方页未写 | 免费（"免费"本次官方页没写，【未核实】） | 官方页未写上限 | [Google 帮助](https://support.google.com/chrome/answer/1649523)【已核实：设置 4 步、必须设 PIN、分享中每 30 分钟确认】 |
| **AnyDesk** | 地址簿，本次未读到 | 标签/组（未读到） | 未读到 | Solo $28.90（1 用户、100 设备）、Standard $49.90（20 用户 500 设备）、Advanced $111.90（1000 设备）；额外并发 $44.90；各来源价格互相冲突 | 个人免费，限制未核实 | [TSplus 汇总](https://tsplus.net/en_gb/anydesk-pricing/)【二手，冲突】 |
| **TeamViewer** | 未读到 | 未读到 | 未读到 | **Remote Access $24.90/月只含 3 台被管设备，加到 6 台 $49.80**；Business $50.90（200 台）；Premium $112.90（300 台）；Corporate $229.90（500 台） | 免费限个人非商业，会话时长受限，常误判商业使用被断开（G2 评论） | [RealVNC 对比博客](https://www.realvnc.com/en/blog/teamviewer-pricing/)、[G2](https://www.g2.com/products/teamviewer/pricing)【二手】 |
| **Splashtop** | 未读到 | 未读到 | 未读到 | Solo $6（**2 台**，1 用户）、Pro $8.25/用户（10 台）、Performance $13/用户（10 台） | 无免费档，只有试用 | [定价](https://www.splashtop.com/pricing)【已核实】；电脑数来自 [功能矩阵](https://splashtop.com/api/assets/21lBvAeHqXKduMeNqRk48a/Splashtop-Remote-Access-Feature-Matrix.pdf)【二手，多页不一致】 |
| **RealVNC Connect** | — | — | — | 按设备档 3 / 25 / 150 台后转定制（各地区页不一致） | 未核实 | [RealVNC](https://www.realvnc.com/da/discover/p/pricing/)【二手】 |
| **Astropad Workbench**（直接对手） | "device chooser"在多台 Mac 间切换（TechCrunch） | 未读到 | 未读到 | **免费每天 20 分钟；价格在涨**：4 月 TechCrunch $10/月或 $50/年；Astropad 博客写 $14.99/月、$79.99/年；产品页写 $99.99/年（三处不一致，以购买页为准）。只支持 Mac/iOS/iPadOS | 每天 20 分钟 | [TechCrunch 2026-04-08](https://techcrunch.com/2026/04/08/astropads-workbench-reimagines-remote-desktop-for-ai-agents-not-it-support/)、[对比页](https://astropad.com/blog/workbench-vs-jump-desktop/)、[产品页](https://astropad.com/workbench/)【已核实三处原文，三处冲突】 |
| **Apple Remote Desktop** | 多机"观察"窗口：缩略图、电脑名、IP、登录用户头像、状态图标，可显示 CPU/磁盘/内存 | 列表可存搜索条件 | 状态图标 | 一次性买断（价格本次没查） | — | [Apple 手册](https://support.apple.com/guide/remote-desktop/apd94e9ae2a/mac)【已核实】 |

**读法**：
- **按"人"计费、设备不限或几乎不限**：Tailscale、Jump、Parsec（页面没写设备数）。这是对"多台常驻主机"用户最友好的做法。
- **按"设备"计费**：TeamViewer Remote Access（$8.3/台）、Splashtop Solo（$3/台，2 台）、RustDesk（$0.12/台 + 用户费）、AnyDesk（实际是用户费，设备上限 100–1000）。
- **没有一家公开"静止主机"和"活跃会话"区分计费**。我们的成本主要来自中继流量，所以设备数只该当防滥用上限。
- 设备列表的细节（徽标、"需审批"置顶、last seen 过滤）只有 Tailscale 读到一部分；其他家的列表字段本次没读到，**不要据此宣称"同行都显示 X"**。

---

## 3. 无显示器设备（先做 Mac，Windows/Linux 简列）

### 3.1 用户流程（"体检与修复向导"，不是文档）

1. 在主机上首次启动 PeerDesk，检测到"当前无物理显示器"或"用户勾选了无人值守"，进入**主机体检**，一屏列出 7 项，每项三态（正常 / 需要你做一步 / 做不到+原因）：
   1) 屏幕录制与辅助功能授权；2) 登录后自动运行（LaunchAgent 已装）；3) 开机前就可连（LaunchDaemon + 登录窗口代理）；4) FileVault 与断电自启冲突；5) 睡眠/唤醒；6) 无显示器分辨率；7) 远程登录（SSH）是否开着（用于 macOS 26 的预启动解锁）。
2. 每个"需要你做"的项，只给**一个按钮**：能直接打开对应系统设置页的就打开，并显示截图指引；需要管理员密码的走系统授权对话框。
3. 体检结果上报到账户，设备列表里这台主机显示"健康 / 3 项需处理"。以后每次启动和每次系统更新后重新体检（权限失效最常发生在这个时刻）。
4. 在手机上看到"这台主机更新后屏幕录制失效"，点进去看到原因和**需要有人在机器前点的一步**，并明说"远程做不到，这是 macOS 的限制"（符合"不静默降级"）。

### 3.2 界面上显示什么

- 设备卡片：在线点、名称、系统、**健康徽标**（正常 / N 项需处理 / 重启后将失联）、上次在线、PeerDesk 版本（需更新）。
- "重启后将失联"是一个**预测性告警**：FileVault 开着 + 没有开 macOS 26 的 SSH 解锁 + 没有备用远程路径 → 提示"如果现在重启，这台机器会停在解锁界面，直到有人到场"。这是对用户最有价值的一句话。
- 重启前动作：提供"安全重启"按钮，内部用 `fdesetup authrestart`（见 3.4 条目 b）。

### 3.3 各平台接口和限制：能做 / 只能引导

先给总表，细节下面分条。

| 问题 | 我们用公开 API 能做到 | 只能引导用户手动 | 做不到 |
|---|---|---|---|
| 无显示器分辨率（macOS） | 用 ScreenCaptureKit 按需缩放采集；检测"当前无物理显示器" | 推荐 HDMI 假负载（几美元）或 BetterDisplay 之类 | **创建虚拟显示器没有公开 API**，只有私有接口（见 a） |
| FileVault 预启动 | 检测状态；`fdesetup authrestart` 做计划内重启 | 开远程登录，使 macOS 26 可在预启动用 SSH 解锁；或关 FileVault | 断电后无人值守自动过 FileVault 登录界面：做不到（无公开方法） |
| 断电自启 | 读取并提示设置 | 系统设置 → 能源（路径随 macOS 版本变） | 无法在不要管理员授权的情况下代改 |
| 屏幕录制/辅助功能 | 检测（`CGPreflightScreenCaptureAccess` 等，代码已用）、指引、检测失效 | 授权必须在机器前点（或 MDM 的 PPPC 配置） | 无法静默授权或续期 |
| 登录窗口前就能远程 | LaunchDaemon + LaunchAgent（Aqua 与 LoginWindow）拆分，macOS 14.4+ | — | 14.4 之前只能走旧接口；FileVault 预启动阶段以前 |
| 睡眠/App Nap | 用 IOKit 电源断言保持唤醒；提示"唤醒以供网络访问" | 系统能源设置 | — |

**a. 虚拟显示器（macOS）**
- Apple 官方：没有公开 API 创建桌面显示器。Apple DTS（2026-08）原话："No, not a public one. There's an SPI thats relatively widely known and which several apps have built upon, but we've never provided public API for this." [Apple 论坛 842949](https://developer.apple.com/forums/thread/842949)【已核实】。
- 第三方做法：基于未公开的 `CGVirtualDisplay` 类（Go 封装文档说"used by every third-party virtual-display app on macOS"，且不能上 Mac App Store）[go-macos/virtualdisplay](https://pkg.go.dev/github.com/go-macos/virtualdisplay)【二手】；BetterDummy/BetterDisplay 都走这条。公开替代是 DriverKit 扩展，要 Apple 授予的 entitlement（二手）。
- **决策建议**：我们要分发 Developer ID 签名的直装包（不上 App Store），用私有接口技术上可行，但有"系统更新后可能失效"的风险。做法：**P1 不做**，体检里只检测并推荐 HDMI 假负载和 BetterDisplay；**P2 做**，但用私有接口要写进文档，并保留"假负载"兜底，且在体检里检测创建失败并说原因。不确定公证是否接受：未核实。
- Windows：用 IddCx 间接显示驱动，社区有 MIT 的 Virtual-Display-Driver；驱动更新时 Windows 重新枚举，无物理屏时可能黑屏（项目自己警告）【二手】[VirtualDrivers](https://github.com/VirtualDrivers/Virtual-Display-Driver)。`G-unattended.md` 1.6 已有方案，这里不重复。

**b. FileVault（和已有文档的一处冲突，需要改）**
- `G-unattended.md` 2.4 写"FileVault（预启动）：做不到"。**现在不完全对**：Apple 平台安全指南写明，"On a Mac with Apple silicon with macOS 26 or later"，开了远程登录且有网络，重启后可以用 SSH 解锁 FileVault【已核实】[Apple Platform Security](https://support.apple.com/guide/security/sec8447f5049/web)。Jeff Geerling（2025-09）实测：用以太网可以，Wi-Fi 在 26.5 之前不行，26.5 起也能【已核实】[博客](https://www.jeffgeerling.com/blog/2025/you-can-finally-manage-macs-filevault-remotely-tahoe/)。需要管理员账户密码。限制：机器睡着或没网时无效（Apple 工程师原话）。
- 所以我们**不能代用户做解锁**（需要管理员密码，我们不该存），但可以：①检测 macOS 26+ 和远程登录是否开启；②引导开启；③提供一个"解锁引导"页：教用户从另一台机器 `ssh user@host` 输入密码，并把命令按主机地址生成好复制。**不要存管理员密码来替用户解锁**（安全上不能接受，要产品负责人确认）。
- `fdesetup authrestart`：只对"FileVault 开着"有效，一次性跳过预启动解锁；Apple man page 警告"FileVault protections are reduced during authenticated restarts"（把解锁密钥临时放在内存/SMC）【二手，搜索摘要】。适合计划内重启（装更新后重启），不适合断电。
- 自动登录与 FileVault：多个指南说开着 FileVault 时"自动登录"不可用（HomeTechOps、Astropad 的表述），Apple 官方文档没读到这条【二手/未核实】。
- 厂商建议"关 FileVault"（Astropad），代价是磁盘无加密，里面可能有 API Key 和 SSH 密钥。**我们的立场**：不替用户做这个决定，体检里给两条路并说明代价：A 关 FileVault（重启自动恢复，磁盘不加密）；B 开 FileVault + 开远程登录（macOS 26+，需要一次 SSH 解锁）。

**c. 断电后自动开机**
- Apple 官方（2026，macOS 26.5+，Mac mini 2024 及以后、Mac Studio 2025+、iMac 2024+）：系统设置 → 能源 → "Start up when power is connected"设为 Always；官方页没提 FileVault 和登录【已核实】[Apple 125517](https://support.apple.com/125517)。旧机型/旧系统走旧的"Start up automatically after a power failure"，用户反馈不一致（有人有效有人无效）【二手】。
- 我们用公开方式读取状态不确定：`pmset -g` 里的 `autorestart` 项本次没有一手来源验证，**未核实**，实现前要实机试。引导用户断电演练一次（拔电 30 秒再插）并把结果记录到体检里。

**d. 屏幕录制与辅助功能权限（现状）**
- Claude Code 文档：首次要授权 Accessibility 和 Screen Recording，授权后"macOS may require you to restart Claude Code"【已核实】。
- 授权绑定二进制身份：路径、bundle ID、签名都要稳定（OpenClaw 文档，【已核实】）。这对我们意味着：**签名身份和安装路径永不变**（`/Applications/PeerDesk.app`），更新时原地替换，且被控端核心不能每个版本换二进制路径。
- 把"授权失效"做成状态：沿用 OpenClaw 的状态分类思路：`未决定 / 已拒绝 / 需重启 / 授权过期 / 被禁用`，各给一句原因和一步修法（OpenClaw 做法见上；我们的实现要自己写）。
- macOS 15 起的"每月提示"和 26 的行为：**没有一手的正式版说明**。设计上把它当"可能随时出现的 TCC 提示"，用 CGPreflight 检测，失效后在手机上推送"这台主机需要有人点一下 Allow"。
- 无法做到：远程替用户点系统授权对话框（辅助功能接口点系统弹窗受 TCC 保护，通常不允许；这点未核实，是安全设计上就应该不能）。能做的：**如果 Screen Sharing/PeerDesk 自己已经有屏幕录制权限且 Agent 有人接管，则人可在手机上点这个弹窗**，这正是 Nudge Remote 之类产品卖的点。

**e. 登录前就能远程**
- Apple DTS（Quinn）答复：守护进程（LaunchDaemon）管网络连接，**GUI 工作（含 ScreenCaptureKit 采集和事件注入）放进 LaunchAgent**，`LimitLoadToSession` 设为 `Aqua` 和 `LoginWindow` 两个，二者用 XPC 通信；macOS 14.4 起 ScreenCaptureKit 在预登录环境可用（之前有 bug，用 `CGDisplayStream`）【已核实】[Apple 论坛 814152](https://developer.apple.com/forums/thread/814152)。Apple 还明说"Daemons should not be messing around with GUI stuff, and that includes ScreenCaptureKit"。
- 这与已有 `G-unattended.md` 2.1 的方向应一致；需要对照一下，且要把"最低 macOS 14.4"写成硬门槛并说清楚（低于 14.4 提示"登录窗口不可远程，原因是系统 bug，修法：升级系统"）。
- 重要边界：这只在 FileVault **已解锁、停在登录窗口**时有效（预启动解锁界面不在此列）。

**f. 睡眠 / App Nap / 节能**
- 保持唤醒用电源断言，关"在显示器关闭时自动睡眠"，开"网络唤醒"（设置项名称取自 Astropad，【二手】；`pmset` 具体参数没核实）。
- 常驻进程避免 App Nap：用 `ProcessInfo.beginActivity`，具体选项实现时查官方文档（本次未读，【未核实】）。
- 注意：睡着时 macOS 26 的 SSH 解锁也不可用（Apple 工程师）。

**Windows 常驻主机的坑（简列）**：
- 无显示器：Session 0 隔离，服务不能直接采集，要由 SYSTEM 服务在控制台会话里拉起代理；无物理屏要加 IddCx 虚拟显示器（`G-unattended.md` 已写）。
- UAC 安全桌面：只有 SYSTEM 才能操作；通用厂商文档说"the on-demand remote control session is paused because the on-demand target is not allowed to capture the secure desktop"【二手，BigFix/微软 Q&A】[微软 Q&A](https://learn.microsoft.com/en-us/answers/a/12737194)。做法：服务检测桌面切换，在 Winlogon 桌面上拉起 worker（社区说法，【二手】），仓库 `winsession.rs` 已有对应胶水。
- 锁屏/登录界面：同样靠服务，不是用户态程序。开机自启靠服务（`peerdesk service install` 已有）。

**Linux 常驻主机的坑（简列）**：
- X11：Xvfb/Xdummy 做无头显示（`G-unattended.md` 3.4）。
- Wayland：屏幕捕获要走 xdg-desktop-portal，**需要人批准**，所以无人值守抓登录界面很难（Pinggy 文章的总结，【二手】）。GNOME 有"headless 远程登录"模式：系统守护进程让 GDM 创建无头登录会话，只走 RDP，用 `grdctl --system` 配置；有已知问题：从 Windows mstsc 连会 NLA 失败黑屏（Launchpad bug）【二手】。结论：Linux Wayland 无人值守**能做到的是"GNOME 无头 RDP/或我们自己的 compositor 后端"，通用方案没有**，先只承诺 X11 和 GNOME headless，其他诚实写"暂不支持，原因是 Wayland 要人批准"。

### 3.4 失败时怎么提示（示例，沿用"原因 + 修法"）

| 情况 | 提示 |
|---|---|
| 屏幕录制授权失效 | "这台主机上的屏幕录制授权失效了（常见于系统或 PeerDesk 更新后）。需要有人在机器前打开 系统设置 → 隐私与安全性 → 屏幕与系统音频录制，勾选 PeerDesk。远程做不到：这是 macOS 的限制。" |
| FileVault 开着、未开远程登录 | "如果这台主机重启，它会停在解锁界面，直到有人在机器前输入密码。修法 A：现在开启远程登录（需要 macOS 26 以上），之后重启可以 SSH 解锁；修法 B：关闭 FileVault（磁盘将不加密）。" |
| 无显示器且分辨率低 | "没检测到显示器，当前画面是 1920×1080，文字会偏糊。修法：插一个 HDMI 假负载（约几美元），或安装虚拟显示器工具。" |
| macOS 低于 14.4 | "此系统版本在登录窗口无法被远程控制（系统限制）。升级到 macOS 14.4 以上。" |
| 睡眠 | "这台主机昨晚睡眠了 3 小时，期间无法连接。已为你打开'防止自动睡眠'。" |

---

## 4. MCP：让 Agent 安全地连到你的电脑

### 4.1 规范现状（以官方原文为准）

- 当前规范版本：**2026-07-28**（官方页面链接可见）[传输](https://modelcontextprotocol.io/specification/latest/basic/transports)【已核实】。标准传输两种：**stdio** 和 **Streamable HTTP**（每条消息是对单一 MCP 端点的 HTTP POST，回复是 JSON 或请求范围的 SSE 流）。该版本的要点：**无协议级会话**（"MCP is stateless and has no protocol-level sessions"），每个请求都带协议版本和客户端能力，服务器不主动发 JSON-RPC 请求。需要跨调用状态时，服务器发"句柄"，作为普通工具参数传回。
- 授权：整体仍是**可选**；stdio 不应该走这套，应从环境取凭据；HTTP 做授权就必须 OAuth 2.1 + PKCE，服务器**必须**实现受保护资源元数据（RFC 9728），客户端**必须**带资源指示 `resource`（RFC 8707），校验签发方 `iss`（RFC 9207）；客户端注册推荐 **Client ID Metadata Documents（CIMD）**，动态客户端注册（DCR）已**弃用**；服务器"MUST NOT accept or transit any other tokens"（禁止令牌透传）；支持 `insufficient_scope` 加 `WWW-Authenticate` 的逐级提权（step-up）【已核实】[授权规范](https://modelcontextprotocol.io/specification/latest/basic/authorization)。
- 工具规范：应始终有人在环能拒绝调用；工具注解（annotations）"MUST be considered untrusted unless they come from trusted servers"；服务器必须校验输入、做访问控制、限速、清洗输出；客户端应记录工具调用用于审计【已核实】[Tools](https://modelcontextprotocol.io/specification/latest/server/tools)。（注解里的 `readOnlyHint` 等具体字段名，本次没读到，**未核实**，以规范原文为准。）
- 安全最佳实践：本地服务器威胁（一键安装必须完整显示命令、高亮 `sudo`/`rm -rf`；HTTP 本地服务要要求令牌或用 Unix 域套接字）、SSRF（客户端要拦内网/云元数据地址）、范围最小化（初始只给低风险只读范围，用 `WWW-Authenticate scope=` 逐步提权，日志记录提权事件）【已核实】[安全最佳实践](https://modelcontextprotocol.io/docs/tutorials/security/security_best_practices)。

### 4.2 三个客户端怎么配（决定我们要提供的"一键配置"）

| 客户端 | 远程 HTTP 服务器 | 带 OAuth | 本地 stdio | 工具审批 | 来源 |
|---|---|---|---|---|---|
| **Claude Code** | `claude mcp add --transport http <名> <url>`；可加 `--header "Authorization: Bearer …"` | `/mcp` 里完成浏览器登录，或 `claude mcp login <名>`（`--no-browser` 可用于 SSH 没显示器）；某些服务器要固定回调端口 `--callback-port` | `claude mcp add <名> -- <命令>`；作用域 local/project/user | 工具层面见 Claude Code 权限设置（本次未读）；管理员有 `managedMcpServers` | [Claude Code MCP 文档](https://code.claude.com/docs/en/mcp)【已核实（前 10 万字符）】 |
| **Codex** | `~/.codex/config.toml` 里 `[mcp_servers.名]` 的 `url`、`bearer_token_env_var`、`http_headers` | `codex mcp login <名>`（`--no-browser`、`--scopes`、`--oauth-client-registration cimd|dcr|auto`） | `command`/`args`/`env` | `default_tools_approval_mode`：`auto`/`prompt`/`writes`/`approve`；`enabled_tools`/`disabled_tools`；**`writes` 只对没标只读的工具询问** | [Codex MCP（OpenAI）](https://learn.chatgpt.com/docs/extend/mcp?surface=cli)【已核实】 |
| **OpenClaw** | `mcp.servers` 里写 `url`、`transport: "streamable-http"`、`headers`；默认 `sse` | `auth: "oauth"` + `openclaw mcp login <名>`；`oauth.identity: "per-requester"` 可每个发消息的人用自己的账户（需要 `gateway.publicOrigin`） | `command`/`args`/`env`；会拒绝 `NODE_OPTIONS`、`LD_*` | `openclaw mcp doctor --probe`、`status --verbose`；OpenClaw 自己也能作为 MCP 服务器 `openclaw mcp serve` | [OpenClaw MCP](https://docs.openclaw.ai/cli/mcp/transports)【已核实】 |

**设计含义**：
- 三个客户端都支持"远程 HTTP + OAuth"，也都支持"静态 Bearer"。我们的托管端点必须兼容**CIMD**（优先）和 DCR（旧客户端兜底），且 `resource` 参数和 `iss` 校验要对。
- Codex 的 `writes` 模式依赖工具"标注只读"，所以我们要**准确标注**每个工具是否只读；同时知道客户端必须把注解当不可信，所以**不能只靠注解，服务端自己也要强制**。
- 一键配置：设备详情页提供"复制给 Claude Code / Codex / OpenClaw"三段现成命令或配置，包含 `--no-browser` 变体（因为这类用户常常在 SSH 里配置）。

### 4.3 现有"控制电脑"的 MCP 服务器与安全做法（调研）

| 项目 | 能力 | 安全相关做法 | 来源 / 标记 |
|---|---|---|---|
| Claude Code 内置 `computer-use` | 点击、输入、截图（仅 macOS CLI，Pro/Max/Ultra，不支持 `-p` 非交互） | **每个会话按应用审批**；终端、Finder、系统设置有额外警告；按应用类别限制（浏览器只看、终端只点）；终端窗口排除在截图外；Esc 全局中止；**一次只允许一个会话（锁）**；Dispatch 之外无远程审批界面 | [文档](https://code.claude.com/docs/en/computer-use)【已核实】 |
| 向日葵 MCP、TeamViewer MCP、QuickDesk | 见《AI Agent 业内调研》第 49 行附近 | TeamViewer MCP 只开放管理面 | 已有调研，不重复 |
| winremote-mcp（Windows）、native-devtools-mcp、AutoControl、desktop-mcp、OpenDesk 等开源 | 截图、点击、输入 | 可选 Bearer 令牌；desktop-mcp 输入类工具默认关闭、要配置打开；AutoControl 可写 JSONL 审计；老版 winremote 把"按工具权限/审计日志/IP 白名单"标为"Not yet" | 搜索摘要【二手】，**没打开仓库核对**，不要引用为事实 |
| 没有读到的 | "只读模式 + 每次确认 + 审计"三者齐全的现成实现 | — | 搜索摘要：未找到齐全者【未核实】 |

**结论**：现成的开源"控制电脑 MCP"几乎都是"本机进程、令牌或无认证"，缺少"按工具分级授权、手机审批、不可篡改审计"。这就是我们的位置（呼应《AI Agent 业内调研》的 0/1/2 三层分级表）。

### 4.4 PeerDesk MCP 应暴露的工具（按风险分层）

沿用业内调研的三层（0 只读、1 可撤销、2 有后果），再加一层"管理面"。**每个工具都有 `risk` 标注 + 服务端强制，不依赖客户端遵守**。

| 层 | 工具（建议名） | 作用 | 默认 | 确认方式 |
|---|---|---|---|---|
| **管理面（只读）** | `list_devices`、`get_device_status`（在线、健康、权限状态、版本） | Agent 知道有哪些机器、哪台能用 | 开 | 无 |
| **0 只读** | `screenshot`（可指定窗口）、`read_clipboard`（默认关）、`list_files`、`read_file_text`（限目录白名单）、`system_info` | 看 | 开 | 首次授权该设备+范围 |
| **1 可撤销** | `open_app`、`click`、`type_text`、`key`、`scroll`（限已批准应用）、`send_file_to_owner` | 动手但可撤回 | **关**，用户在设备上按应用打开 | 按应用"始终/本次/从不" |
| **2 有后果** | `run_command`、`write_file`/`delete_file`、`install`、`transfer_to_third_party` | 改系统 | **关**，且用户的手机批准每一次（显示完整命令） | **每次手机确认** |
| **会话** | `request_human_takeover`（Agent 求助：弹到用户手机，用户进来接管）、`release` | 解决痛点 2：Agent 卡住时不死等 | 开 | 推送给用户 |

设计要点：
- **按设备、按范围的 OAuth scope**：如 `devices:read`、`screen:read`、`input:apps:<app>`、`exec:ask`。用 MCP 的 `scope` 挑战做**逐级提权**：Agent 一开始只拿 `devices:read screen:read`，第一次要点击时由我们返回 403 `insufficient_scope`，客户端走 step-up，用户在手机上批准。符合规范的"最小范围、逐步提权"。
- **工具列表按授权变化**：规范允许 `tools/list` 随授权返回不同集合（"MAY vary by the authorization presented"），所以没有 `exec` 范围的令牌看不到 `run_command`，从源头减少误用。
- **令牌绑定到账户+设备**，`resource` 指向具体设备或设备组端点，**不接受透传的令牌**（规范明令）。
- **只读模式一键切换**：设备详情里有总开关"只读"，对所有 Agent 令牌立即生效（服务端强制）。
- **审计**：每次调用记录谁（哪个客户端/令牌）、哪台设备、什么工具、参数摘要、结果、是否经人批准、时间；写入账户审计（Plus 7 天、Pro 30 天，与赚钱方案一致）；日志本身用户可导出。**审计不可被 Agent 删**（只追加，存服务端）。
- **提示注入防护（承接业内调研）**：截图里的指令不可信；`run_command` 之类永远人确认；工具结果统一标注"来自屏幕/文件的内容"。
- **单会话锁的取舍**：Claude Code 一次只允许一个会话控制一台电脑。我们允许多个 Agent 读，**同一时刻只允许一个"写"会话**，其他排队并提示"被谁占用"，避免两个 Agent 打架。

### 4.5 怎么授权、怎么让用户放心

授权流程（第一次，目标 ≤ 4 步）：
1. 设备详情页点"给 Agent 用"，选客户端（Claude Code / Codex / OpenClaw / 其他）。
2. 我们显示一条命令（或配置片段），用户粘贴到终端。
3. 客户端弹浏览器（或用 `--no-browser` 给出链接）→ PeerDesk 登录页显示：**哪个客户端、要哪些范围、对哪台设备、回调地址**（规范要求显示 CIMD 主机名/回调主机名，防钓鱼），用户点同意。
4. 手机收到"Claude Code 已连接到 mac-mini，范围：只读"的通知，可一键撤销。

让用户放心的 6 件事（都要在界面上看得见）：
- **连接中的 Agent 列表**：谁、什么范围、最近一次动作；每个都有"撤销"。
- **被控机上有可见指示**（菜单栏/托盘图标变色 + 通知"Claude Code 正在使用这台电脑"），Esc 或按钮立即中止（对标 Claude Code 的做法）。
- **默认只读**，写操作要用户主动开。
- **活动时间线**：像审计，但面向普通人，用人话写"10:32 看了屏幕；10:33 运行命令 `npm test`（你批准）"。
- **审批只在手机上做**，命令完整显示，高危词高亮（规范要求的"完整显示命令"）。
- **开源可审计**，MCP 工具表和权限表写进文档。

不放在 P1 的：托管端点的 OAuth（要账户系统和授权服务器）。P1 先做本机 `peerdesk mcp`（stdio）+ 远程走已认证 QUIC 通道的 CLI，规范明确 stdio 不走 OAuth，这样不被授权卡住。

---

## 5. 设备管理（单人 3–10 台的核心设计，规模见第 7 节）

### 5.1 现状（读了代码）

- `presence` 分支 `presence.rs`：按设备码或指纹，每 30 秒（`REFRESH`）查一次服务器，状态三态：在线/不在线/未知（带原因）；只在设备码连接且服务器明说不在线时才挡；**一次最多 64 台**（`MAX_PRESENCE`）。
- `ui-redesign` 分支（本地没有该分支，读的是 `origin/ui-redesign`）`book.rs`：设备簿，设备有稳定 id、电脑名、别名、收藏、分组（树形，最多 4 层）、上次连接时间、修改时间；设计上已经留了账户同步（墓碑、后改的赢）；本地文本文件。
- `home.rs`：四个标签页：最近、收藏、分组、同一网络；"这台电脑"的状态徽标；**"同一网络"在这个版本显示"不能发现同网络的电脑"**。
- 已有无人值守（`unattended.rs`、`winsession.rs`）。

缺的：账户设备表（服务端权威）、健康/权限/Agent 状态、标签、版本、批量、局域网发现。

### 5.2 用户流程

**加第一台设备（目标：2 步，对标 Chrome 远程桌面的 4 步）**
1. 被控机安装 PeerDesk，登录账户（或扫码/输入一次性码）。
2. 在手机/笔记本的列表里它自动出现，默认设备名 = 电脑名；首次连接时设"无人值守"并设 PIN。

对标（只有 Chrome 读到了原文）：Chrome 远程桌面设置 4 步，必须设 PIN；每次分享中还要"每 30 分钟确认"【已核实】[Google](https://support.google.com/chrome/answer/1649523)；Astropad 只写"under a minute"【二手】。本次没有读到其他同行的步数，**不要写成"同行最好做法是 N 步"**。

**加第 N 台**：同账户登录即可出现；不用输设备码。

### 5.3 界面：设备卡片显示什么

一行就够，从左到右：状态点 | 名称 | 系统图标 | 一行状态句 | 主操作。

状态句优先级（从最紧急开始，只显示最紧急的一条，其余在详情里）：
1. **离线 X 分钟/小时/天**（用相对时间，鼠标悬停显示绝对时间）。若离线前有"预测失联"告警，显示"重启后未恢复，可能停在 FileVault 解锁界面"。
2. **缺权限**：`屏幕录制已失效`（带"怎么修"）。
3. **Agent 在等你**：`Claude Code 等待批准：运行 npm test`（带批准按钮）。
4. **需要更新**：`PeerDesk 需更新（影响授权保持）`。
5. 在线空闲 / 在线，有人正在控制 / Agent 正在使用。

状态的"原因 + 修法"来自第 3.4 节的表，统一用同一组字符串，列表、详情、通知三处一致。

### 5.4 权限显示（"多台设备一眼看清权限"）

每台设备详情里一个固定块"这台电脑允许谁做什么"：
- 本机系统权限：屏幕录制 / 辅助功能 / 文件访问 / 完整磁盘（按需），每项绿/黄/红 + 一键去修。
- 连接来源：哪些账户设备可以连、哪些 Agent 令牌有效（范围、最近一次使用）。
- 无人值守：开/关、是否需要 PIN、是否允许在锁屏/登录界面连。

### 5.5 失败时提示

沿用 CLAUDE.md"失败要说原因和解决办法，不静默降级"：不显示"未知"，显示"服务器连不上（原因），仍可局域网直连"。这一点 `presence.rs` 已经这样做了（未知带原因），列表要保留。

---

## 6. 丝滑体验的细节小结

| 环节 | 同行做法（读到的） | 我们的目标 |
|---|---|---|
| 第一次安装到能远程 | Chrome 远程桌面 4 步，要 PIN | 安装 → 登录 → 授权两项系统权限 → 完成，系统权限在向导里一次引导完，**不让用户去找设置页** |
| 加设备 | Tailscale 靠登录或预授权密钥（auth key）；RustDesk Pro 有自定义客户端生成器把服务器地址内置 | 同账户登录即出现；批量见第 7 节 |
| 权限缺失怎么提示 | OpenClaw：结构化错误 `PERMISSION_MISSING` + 控制界面"Grant 卡片"，状态 5 种；"Granting permission does not automatically repeat the command"（授权后不自动重试） | 同样结构化；授权后**自动重试**被卡住的一步（OpenClaw 要用户再确认，是可以改进的点；改之前要确认重试安全，只对只读重试） |
| 出问题时 | Tailscale 对过期设备可临时延长 30 分钟（二手） | 手机上"一步修"，修不了就写清楚需要谁在机器前做什么 |

---

## 7. 规模化设备管理（一个人管几十到几百台，局域网很多）

这一节是新增需求，证据比前面更薄，先说清：**读到一手原文的少，很多同行细节是搜索摘要。**

### 7.1 要解决的问题

200 台设备时用户的行为变成：**"只想看哪些不对"**，不是浏览列表。所以：
- 默认视图是"**需要关注**"（离线超过阈值、权限失效、需更新、Agent 卡住、重启后失联风险），正常的折叠成一行汇总"187 台正常"。
- 查找：按名称/标签/系统/所在位置/状态过滤；保存过滤为"视图"；键盘快速搜索（Ctrl/Cmd+K）。
- 分组：**允许一台设备属于多个标签**，不是单一树。同行对比：RustDesk **一台设备只能在一个组**（社区部署项目和文档摘要，【二手】），这是已知局限，我们用"标签 + 位置层级"避免。Tailscale 的做法是 ACL 标签（tag），标签设备默认关密钥过期【已核实】。
- 当前代码的树形分组（最多 4 层，一台设备只在一个组里）适合"机房→机柜→机位"，但不适合"哪些是 Agent 主机"这类横切属性，要加标签。

### 7.2 批量操作

| 操作 | 难点 | 设计 |
|---|---|---|
| 批量升级 | 升级会重启服务，**授权要保持**（回到痛点 3）；Windows 服务升级要处理文件占用 | 分批滚动（金丝雀 1 台 → 10% → 全部），每批后跑体检，失败自动停并列出原因；升级期间保证二进制路径和签名身份不变（macOS） |
| 批量授权（给一批设备开无人值守、给 Agent 令牌批量授权） | 系统权限**不能远程授予**（TCC 在机器前点，MDM 的 PPPC 可预授权） | 对账户级权限（谁可连、Agent 范围）批量设置；对系统权限提供"MDM 配置描述文件下载"（PPPC + 登录项 + 系统扩展），让用户用 Jamf/Kandji 等一次下发。MDM 能否静默授予屏幕录制：**我们没查到一手资料，未核实**（已知 PPPC 对屏幕录制只能"允许用户授予"，不能直接授予的说法是常识，但没核实，实现前必须查 Apple 的 PPPC 文档） |
| 批量重启 | FileVault 开着重启后失联 | 重启前自动检查"重启后能否恢复"，不能就拒绝并说明；能就用 `authrestart`，逐台、限速 |
| 批量改名/打标签/移组 | 无 | 多选后的通用操作 |

每个批量操作必须：①先显示将影响哪些设备和哪些会被跳过及原因；②逐台结果表（成功/失败/原因/怎么修）；③可中途停止；④写审计。

### 7.3 同行在这个规模下的做法（读到的）

- **Tailscale**：规模用设备标签 + ACL；**auth key 预授权**加入，可自动打标签；可通过 OAuth 客户端用 API 批量创建 key；有 Jamf 部署文档（只做分发，不涉及 key）【二手，搜索摘要】[auth keys](https://tailscale.com/kb/1085/auth-keys)、[Jamf](https://tailscale.com/kb/1328/mdm-jamf)。
- **RustDesk Pro**：策略（strategy）可下发到设备/用户/设备组；REST API 批量枚举、入组、删除设备；安装参数 `--device_group_name`（官方文档摘要）；官方建议"先小范围试，再大规模下发"；大批量部署推荐**自定义客户端生成器**把服务器设置内置到安装包里【二手，非英文页摘要】[策略文档](https://rustdesk.com/docs/de/self-host/rustdesk-server-pro/strategy/)、[部署](https://rustdesk.com/docs/zh-cn/self-host/client-deployment/)。
- **AnyDesk**：官方命令行文档说明 CLI 可配合部署工具对多台设备安装和配置；许可证和密码通过标准输入传给 `--register-licence`、`--set-password`；也可在 my.anydesk 生成自定义 MSI【二手，搜索摘要】[AnyDesk CLI](https://support.anydesk.com/Automatic_Deployment)。
- **TeamViewer**：MSI 批量部署、按配置 ID 分配的做法本次**没搜到**，未核实。
- **Parsec Teams**：管理面板、团队管理的电脑、组、SSO、API；SCIM 在 Enterprise【已核实】。
- **Apple Remote Desktop**：多机观察窗口，每台缩小，分页，可选显示 CPU/磁盘/内存状态【已核实】。
- **MDM 类工具（Jamf 等）**：本次只验证到 Tailscale 的 Jamf 集成是"分发 + 配置描述文件"，**没有读到 Jamf/Kandji 对屏幕录制的 PPPC 一手资料**，所以关于 MDM 能做什么都标未核实。

### 7.4 批量部署和加设备（不能一台台点）

目标：**对一批机器，管理员做一次，设备自己出现并带标签**。

| 方式 | 做法 | 谁适合 |
|---|---|---|
| **一行命令安装**（Linux/macOS/Windows PowerShell） | `curl … \| sh -s -- --token <预置令牌> --tag lab-a` 之类，装好后自动注册到账户、带标签 | 几十台，有 SSH 的人 |
| **预置令牌（enrollment token）** | 账户里生成"部署令牌"：有范围（只能加设备、不能读屏幕）、有有效期、有数量上限、可撤销、自动打标签；类似 Tailscale 的 auth key（可预授权、可带标签）和 RustDesk 的"内置服务器设置的客户端"。令牌不等于设备身份，注册后换成设备自己的密钥 | 所有人 |
| **安装包带配置**（MSI/pkg 参数或配置文件） | 安装时参数 `/token=… /tag=…`；macOS 用 `.mobileconfig` 配置描述文件下发令牌和权限 | 有 MDM/Intune/GPO 的机房 |
| **无人值守安装** | 安装服务、开机自启、不弹界面；macOS 需要 PPPC 预授权才能真正无人值守，否则每台要有人点 | 机房 |
| **克隆镜像坑** | 同一镜像克隆出的机器**设备指纹/密钥会重复**（计费挂账户不挂设备指纹的原则正好帮上忙） | 机房，必须处理 |

**必须解决的关键风险**：
- **系统权限无法远程授予**，所以"几百台 Mac 一次性装好并全能录屏"只有在有 MDM 的前提下才可能；没有 MDM 的人，只能每台人到场点一次。要诚实地告诉用户，并在批量部署页上明示"macOS 需要 MDM 预授权，否则每台需要人到场点两个开关"。
- 令牌泄漏：令牌只能"加设备"，且加进来的设备在"待批准"队列里默认不可被 Agent 使用，批准后才生效（对标 Tailscale 的"Needs approval"设备审批【已核实】）。

### 7.5 局域网：同网段发现、不经服务器直连、断网可用

读到的同行信息很少，诚实列出：
- **RustDesk**：官方文档提到 UDP 广播发现同网段对端；用户反馈"Discovery"标签列出其他局域网客户端，但跨网段看不到【二手，搜索摘要】。有 RustDesk 的 fork（SubnetDesk）用 **mDNS** 做发现，宣称不需要公共 rendezvous/中继/云账户【二手，README 摘要】[SubnetDesk](https://raw.githubusercontent.com/zibo-chen/SubnetDesk/master/README.md)。
- **AnyDesk、TeamViewer、Parsec、Jump 的局域网发现**：本次**没读到**，未核实。（Apple Remote Desktop 的 Bonjour 扫描确有，但本次没拿到原文，也标未核实。）
- **我们现状**：`ui-redesign` 的"同一网络"标签页是占位："这个版本还不能发现同网络的电脑"。设置页显示本机局域网地址（`lan_addresses`）。

设计：
- **发现**：mDNS/DNS-SD 服务类型（`_peerdesk._udp`）发布设备名、设备 id 的短指纹、端口、版本；同账户的设备**用签名过的广播**（含设备公钥指纹），客户端用已知的账户设备表验证，防止恶意机器冒充。非本账户的设备显示为"未配对，需配对"。mDNS 被交换机/VLAN 隔离时给出原因（"你的网络可能隔离了多播，请直接输入地址"），并保留 UDP 广播和手动输入 IP 作兜底（符合"不静默降级"）。
- **直连**：同网段优先 LAN 直连（候选地址收集已有 `candidates`），连接建立不依赖服务器；已有"按直连地址（IP:端口）记下的设备，服务器说不在线也不挡"的逻辑（`presence.rs` 开头注释）。
- **断网可用**：**LAN 直连需要本地能验证设备身份**，所以设备簿要缓存账户内各设备的公钥和权限策略，服务器不可达时用缓存验证；撤销设备的黑名单会有延迟，要写明（离线期间已被撤销的设备仍可能连入，直到下次联网同步，这是个安全取舍，要产品负责人确认）。
- **性能与安全**：mDNS 在大机房（几百台）会广播风暴；用 TTL、抑制重复、随机延时；每个主机只响应已配对或同账户的查询，不暴露设备名给陌生人（设备名可能泄露信息）。

### 7.6 同时开多个会话：缩略墙

- **同行**：Apple Remote Desktop 的多机观察（缩放、分页、可选显示 CPU/磁盘/内存）【已核实】；Microsoft RDCMan 选组即显示下面服务器的缩略视图，可显示实际窗口或只显示连接状态；VNC Thumbnail Viewer 类工具（最后版本 2008）【二手】。**本次没有找到现代远控产品（Parsec、AnyDesk、Jump 等）有缩略墙的证据**，未核实。Jump 官方页写 AI Agent 可经 Jump CLI 同时控制最多 8 台【已核实】，说明同行把"多机同时"主要给 Agent，不给人看墙。
- **需求推导（推断）**：几十台的"墙"不需要实时视频，需要的是**低帧率低分辨率的状态画面**（每台 1 fps、320×180 左右、静止时不传），点开某台才升级成全速。因为我们的传输层本来就是"静止时几乎不发"（CLAUDE.md），这反而是优势。
- **带宽/CPU 估算（推断，必须实测）**：50 台 × 每台约 20–50 kbit/s 的缩略流 ≈ 1–2.5 Mbps；解码 50 路 320×180 的 H.264 对 CPU 压力应该不大，但**每台被控端要为缩略流单独编码一路**（或用同一路码流的低层），这会增加被控端 CPU，要在 e2e 里量。**没有任何实测数据，这些数字只是量级，不能当承诺。**
- **设计**：缩略流是独立的"观察"通道，**只读**；一次最多 N 路（按带宽预算自适应降低帧率，不降低到看不清）；缩略墙只显示状态异常的优先；中继不转发缩略流（走直连，否则几十路会吃爆中继成本——这直接影响计费，见第 8 节）。

### 7.7 当前代码在这个规模下会坏在哪

1. `MAX_PRESENCE = 64`：一次最多问 64 台，`Board::tick` 取前 64 个（按 BTreeSet 排序），**第 65 台起永远没有状态**。几百台需要订阅式推送（服务端合并变更，客户端只收增量），不是轮询。
2. 设备簿是本机文本文件，逐行解析，墓碑上限 500 条；几百台设备加批量部署每天可能增删几十台，墓碑会很快被挤出（"最老的先丢"），导致已删设备被同步回来。要换成服务端权威的设备表。
3. 30 秒轮询 × 数百台 × 多个控制端，服务器压力线性涨；要用长连接订阅。
4. 树形分组一台设备一个位置，横切属性放不下（见 7.1）。
5. "同一网络"标签页是占位。

这些是**需要正式工程评估**的结论，我只读了代码，没有跑压测。

---

## 8. 计费单位建议

### 8.1 先回答"按设备数还是按用户数"

**建议：按账户（人）分档，设备数只作宽松软上限，只数被控主机；规模用户用阶梯加购并封顶。**

理由：
1. **目标用户的设备数天然不小**：一个人一台 Mac mini + 一台 Windows + 一台 Linux 盒子 + 笔记本 + 手机，被控主机常 3–6 台，规模用户是几十到几百台。设备数越多，按设备收费越贵，用户越会少装、拼凑（比如一台主机上开虚拟机而不是各装一个），**抑制的正是我们想要的"越多越好用"**。
2. **同行主流是按人**：Tailscale（设备不限）、Jump Desktop（电脑不限）、Parsec（按用户）都是这样【已核实】；按设备的是 TeamViewer（$24.90 含 3 台，每台约 $8.3，是这批人最反感的类型）、Splashtop Solo（2 台）。
3. **成本不跟设备数走**：中继流量才是成本（赚钱方案 3.4）。一台不用的主机成本约等于心跳（几乎零）。按设备收费会收到不产生成本的东西，同时漏掉真正产生成本的重度中继用户，所以：**设备数做软上限防滥用，中继 GB 做成本控制**。
4. **一个反方观点要说清**：纯"按人、设备不限"在规模化后会被一个个人账户养 500 台机房吃掉。所以要有**软上限 + 加购阶梯**，不是无限。
5. 计费挂在账户上（CLAUDE.md），设备指纹不参与计费；克隆镜像的重复指纹不会影响计费。

### 8.2 规模定价的同行数据

| 同行 | 怎么处理几百台 | 单台边际成本 |
|---|---|---|
| Tailscale | 按用户，设备不限；Standard $8/用户 | 用户视角 $0/台 |
| RustDesk Pro | $23.88 起，**每多 1 设备 $0.12/月**；Basic 100 台；用户 $1.20/月 | 250 台（Customized）估算：基础 $23.88 + 超出台数 × $0.12，基础含多少台页面没写 |
| AnyDesk | Solo 100 台、Standard 500 台、Advanced 1000 台（档位封顶，价格互相冲突） | Standard $49.90 含 500 台 ≈ $0.10/台 |
| TeamViewer | Remote Access 3 台 $24.90；Business 200 台 $50.90；Premium 300 台 $112.90；Corporate 500 台 $229.90 | 被管 200 台约 $0.25/台，但入门档 3 台 $8.3/台 |
| Splashtop | Solo 2 台；Pro/Performance 10 台，按用户 | — |
| RealVNC | 按设备档 3/25/150 之后转定制（各页不一致） | 未核实 |

读法：**规模档位的单台价在 $0.10–0.25/台/月**（RustDesk、AnyDesk、TeamViewer Business 级别），入门档单台价在 $3–8。我们要避免的是入门档那种高单价。

### 8.3 与《赚钱方案》3.2 的对齐和需要修改的地方

| 项 | 原方案 | 建议 | 理由 |
|---|---|---|---|
| Free | 2 台设备 | **3 台被控主机**（控制端不限） | Jump、Tailscale 等免费档设备不限；2 台装不下"Mac mini + 一台 Windows + 一台 Linux"，用户刚试就撞墙，转化会变差。成本不变（空闲主机几乎零成本，中继仍 2 GB 卡紧）。**未验证转化，推断** |
| Plus $7/月 | 10 台 | **15 台** | 中位用户 3–6 台，留余量 |
| Pro $14/月 | 25 台 | **50 台** | 小机房、实验室 |
| **新增 Fleet 加购** | 无 | **每多 25 台被控主机 +$X/月（建议起步试 $3–5，上限封顶到 $Y/账户）**；超过封顶转 MSP/企业报价 | 参考 RustDesk $0.12/台（$3/25 台）与 AnyDesk Standard ≈ $0.10/台，起步价放在 $0.12–0.20/台区间。**具体数字是推断，要用预售数据和中继成本实测来定** |
| MSP 档（90 天后） | $29/并发技术员，含 100 台，超出 $0.50/台 | **超出 $0.50/台偏贵**，对规模用户改为与 Fleet 同一阶梯（$0.12–0.20/台），MSP 用"并发"维度收费即可 | $0.50/台比所有读到的同行规模价贵 2–4 倍；除非功能（多租户）明显更强，否则会被比价淘汰 |
| 中继额度 | Plus 100 GB、Pro 500 GB | 不变，但**Pro 500 GB 在 Cloudflare TURN 单价下会亏**（赚钱方案已指出），Fleet 用户尤其要走自建中继 | — |
| 缩略墙 | 无 | 缩略流**只走直连，不走中继**；中继时降级为"状态图标"并说明原因 | 否则 50 路缩略流在中继上线性放大成本 |
| 局域网直连 | — | **局域网直连不计中继流量，不限量**，作为卖点 | 对规模用户（机房）几乎零成本 |
| 审计 | Plus 7 天、Pro 30 天、Team 90 天 | 不变；Fleet 批量操作的审计是必选项 | — |

**关于定价的产品决策**：这是产品负责人的决定，不是我能下的。我只给方向：**按账户分档 + 被控主机软上限 + Fleet 阶梯加购并封顶**。

### 8.4 一个必须避免的陷阱

按设备收费的"设备"如果用设备指纹计，克隆镜像、重装系统会算成新设备，用户被多收或设备额度被占满；计费必须按**账户下注册的主机记录**算，且删除/重装后同一台主机能"认领回"。这与 CLAUDE.md 的原则一致。

---

## 9. 分阶段计划和粗略工作量

工作量按"一名熟悉代码库的工程师全职"估，**全部是推断**，没有排期依据；macOS 平台层有不确定性要先做实验。

### 阶段 0（1 周，先做，零代码）：实验和证据
- 一台真 Mac mini（Apple Silicon，macOS 26）：拔掉 HDMI，测 ScreenCaptureKit 返回的显示器数量和分辨率；测 SSH 预启动解锁（以太网/Wi-Fi）；测断电自启（有/无 FileVault）；测屏幕录制每月提示的真实表现（正式版）。
- 一台 macOS 14.x：验证 LaunchAgent（Aqua+LoginWindow）+ 登录窗口采集（对照 `G-unattended.md` 2.1）。
- 补 Reddit 原帖（用能访问 Reddit 的环境）。
- 产出：把 `G-unattended.md` 里两处过时/冲突（2.4 FileVault、2.5 无显示器）更新。

### 阶段 1（约 6–8 周，必须能闭环）
- 账户设备表（服务端权威）+ 设备卡片状态句 + 主机体检（第 3 节 7 项）+ 健康徽标。
- 订阅式状态推送替换 30 秒轮询（至少能撑 300 台，不再受 64 限制）。
- 本机 `peerdesk mcp`（stdio）：`list_devices`、`get_device_status`、`screenshot`、只读文件两三个工具；远程走已认证通道；工具带风险标注；审计本地+上报。
- e2e 门槛：新加指标（见第 10 节）；在 CI 里用 Linux 真实二进制验证设备表和状态；macOS 体检用 `[mac]` 标记跑。
- **闭环**：新装一台 Mac mini → 登录 → 体检全绿 → 手机上看到它在线和健康 → 重启一次，状态从"在线→离线→在线"，列表上的告警准确。

### 阶段 2（再 6–8 周）
托管 MCP 端点（OAuth 2.1：RFC 9728、8707、CIMD、`iss`、step-up）、手机审批收件箱、macOS 虚拟显示器（私有接口，带兜底）、标签与"只看异常"视图、部署令牌与一行安装、批量升级（金丝雀滚动）、Fleet 计费。

### 阶段 3
缩略墙、局域网 mDNS 发现和离线验证、批量重启/授权、MDM 配置描述文件、审计留存和导出。

---

## 10. 怎么衡量做得好（指标，进 e2e，只升不降）

| 指标 | 目标（初始，需要实测后定） | 衡量方式 |
|---|---|---|
| 新装到第一次成功远程的时间 | 中位 < 5 分钟（推断） | 埋点；e2e 脚本化 |
| 加第 2 台设备的步骤数 | ≤ 2 | UI 流程测试 |
| 体检覆盖：Mac 无显示器 7 项检测准确率 | 实机 100%（每项有真实 mac 实测用例） | `[mac]` 实机测试 |
| 设备状态延迟：真实上线/离线到列表更新 | p95 < 5 秒（推断） | e2e，300 台模拟 |
| 状态规模：300 台下列表可用 | 订阅推送，服务器 CPU 与客户端内存有门槛 | e2e 压测（这是新增，当前代码不过） |
| 授权失效发现时间 | 更新后 < 1 分钟内在手机上看到告警 | 实机 |
| Agent 卡住到用户看到通知 | p95 < 10 秒 | e2e |
| 批量升级成功率 / 升级后授权保持率 | 授权保持率 100%（签名和路径不变） | 实机滚动升级测试 |
| MCP：无授权调用被拒绝率 | 100% | 安全用例 |
| MCP：注入攻击成功率 | 沿用业内调研的测法，只降不升 | 红队用例 |
| 局域网：同网段发现时间 | < 3 秒；LAN 直连不依赖服务器 | e2e 断网 |
| 缩略墙：50 路 CPU/带宽 | 上限待实测；超限自动降帧率并提示 | e2e |
| 商业：免费→付费、设备数分布、Fleet 加购率、每用户中继 GB | 沿用赚钱方案第 5.1 节，加"被控主机数分布" | 埋点 |

对标"对标 Parsec、ToDesk 一档"：这三块本身不涉及画质，但缩略墙和 MCP 截图会，要沿用现有画质门槛。

---

## 11. 未核实和需要产品负责人决定的问题

### 11.1 未核实（实现前必须查）
1. ScreenCaptureKit 在无显示器 Mac mini 上的真实行为（Astropad 说自动创建 1080p 1x，`G-unattended.md` 说通常不创建）。**需要实机。**
2. macOS 15/26 正式版对常驻远控 App 的屏幕录制提示频率；重启后是否再提示。
3. `pmset autorestart` 的具体参数和 FileVault 是否影响断电自启；"FileVault 下自动登录不可用"的官方表述。
4. MDM（PPPC）能否预授予屏幕录制；`.mobileconfig` 的具体字段。
5. 私有 `CGVirtualDisplay` 对公证和未来系统更新的风险。
6. 同行设备列表字段（AnyDesk、TeamViewer、RustDesk、Parsec 的在线/上次在线怎么显示）、局域网发现（AnyDesk、TeamViewer、Parsec、Jump）、TeamViewer 批量部署参数、Jump CLI 的具体能力。
7. Reddit 原帖（r/macmini、r/LocalLLaMA、r/ClaudeAI）。
8. MCP 工具注解字段名（`readOnlyHint` 等）在 2026-07-28 版规范中的确切定义。
9. Claude Code 工具层面的审批设置、`managedMcpServers` 细节（文档后半部分没读到）。
10. 价格：Astropad 三处冲突、AnyDesk 来源冲突、Splashtop 电脑数、RealVNC 档位、Screens 5 买断价冲突；TeamViewer、AnyDesk 官方价格页被拦。
11. 缩略墙的带宽/CPU 数字，全是推断。

### 11.2 需要产品负责人决定
1. **FileVault 立场**：我们引导用户在"关 FileVault"和"开 FileVault + macOS 26 SSH 解锁"之间选，不替用户决定，也不存管理员密码代解锁。同意吗？
2. **虚拟显示器**：同意阶段 2 用私有接口（带 HDMI 假负载兜底并在文档里写明风险）吗？还是只引导用户买假负载？
3. **计费单位**：同意"按账户分档 + 被控主机软上限 + Fleet 阶梯加购并封顶"吗？Free 从 2 台改 3 台是否接受（成本小、转化大概率更好，但没有数据）？Fleet 起步价（建议每 25 台 $3–5）和封顶线定多少？MSP 档"超出 $0.50/台"是否降到与 Fleet 一致？
4. **局域网离线验证的安全取舍**：断网时用缓存验证对端，撤销有延迟。接受吗？
5. **批量部署**：macOS 无 MDM 就必须每台到场点权限，我们是否接受把"带 MDM 的机房用户"作为规模用户的第一批目标，而不承诺"装完即用"？
6. **MCP 写工具**（点击、命令）P1 是否只做本机 stdio，不做托管端点？默认全只读？
7. **P1 范围**：同意 P1 只做"设备健康面板 + Mac 体检 + 本机 MCP 只读"，把虚拟显示器、托管 MCP、批量、局域网发现、缩略墙放到后面吗？
8. **阶段 0 实机**：是否能提供 Mac mini（Apple Silicon）、macOS 14.x 机器、能上 Reddit 的环境？
9. **签名身份**：macOS 授权绑定签名和路径，Developer ID 和安装路径一旦发布不能换（赚钱方案已提）。需要现在确定主体和安装路径 `/Applications/PeerDesk.app`。

---

## 附：本次实际打开的一手来源

- Apple 平台安全指南（FileVault SSH 解锁）、Apple 125517（macOS 26.5 开机设置）、Apple 开发者论坛 756908 / 814152 / 831442 / 842949（均为 DTS 工程师回复）、Apple Remote Desktop 手册。
- MCP 规范：传输、授权、工具、安全最佳实践（2026-07-28 版）。
- Claude Code：computer use 文档、MCP 文档；GitHub issue 80806、47796、41101。Codex MCP 文档；OpenClaw MCP 和 macOS 权限文档。
- Tailscale 定价与设备页；Parsec、Jump、RustDesk、Splashtop 定价页；Google Chrome 远程桌面帮助页；Astropad 博客与 TechCrunch。
- HN Algolia API 取得的评论（有限）。
- 被网络拦截或不可读：Reddit、claudeissues.com、MacRumors 论坛页、Apple 讨论区个别页（重定向）、AnyDesk/TeamViewer 官方价格页。
