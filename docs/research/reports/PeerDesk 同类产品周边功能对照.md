# PeerDesk 同类产品周边功能对照（截至 2026-11-28）

范围：除“画面传输本身”以外的周边功能（账户、连接方式、会话内功能、安全、团队管理、平台、商业条款）。
只做调研，没改代码、没提交。价格一律以官方页为准，查看日期 2026-11-28，除非注明是二手。

标记：
- 【原文】本次打开了官方页面（功能页、价格页、帮助文档、微软文档）并读到该句或该表。
- 【二手】搜索结果摘要、第三方文章、竞争对手博客；没读到官方原文。
- 【未核实】没查到。表里写“?”。

表里的符号：✔ 有（免费档或所有档）；$ 只在付费档；✗ 页面明确没有或不支持；~ 部分有；? 未查到（不等于没有）。
列名：Parsec、TV＝TeamViewer、AD＝AnyDesk、RD＝RustDesk、Splash＝Splashtop、CRD＝Chrome 远程桌面、Jump＝Jump Desktop、MS＝微软 Windows App/RDP、ToDesk、向日葵＝向日葵（国际版 AweSun）。

**表格可信度**：格子里只有读到原文或明确标了【二手】的才写 ✔/$/✗；个别格子（如 MS、CRD 的账户类功能、各产品多语言）依据的是页面的间接描述，可信度低于其余格子，引用前请回到来源核对。

**取证限制（诚实）**：AnyDesk 官网（价格、功能页）和 Parsec 的功能矩阵、Splashtop 客户端页对本环境返回 403/404；TeamViewer 价格页只读到半截（方案表是空的），帮助中心只读到文章标题目录。
所以 TeamViewer 和 AnyDesk 两列“?”最多，价格只有二手。这两家是业内最大的，结论里凡是依赖它们的地方都标了可信度。
网页里夹带“要 AI 做某事”的文字：Chrome 远程桌面首页、RustDesk 文档首页各有一处要 AI 按某种格式回答/列清单，均未执行，也没当作产品信息。其余页面没发现。

---

## 0. 结论先行

### 0.1 标配清单（多数产品都有；按“对收费的影响”排序）

“标配”的判据：本次读到的产品里多数（至少 5–6 家）有，且免费档或所有档都有。表里 TV、AD 读得少，所以判断主要靠另外 8 家。

1. **账户 + 同账户下的设备列表（含在线状态）**：RustDesk 免费档就有在线状态、Splashtop/ToDesk/CRD/Jump/Parsec 都有账户下的电脑列表。没有它就没法把“订阅”挂在账户上收钱。ToDesk 免费档 100 台列表、3 个分组【原文】。
2. **无人值守（固定密码或账户内免密）**：Splashtop 所有档、AD、TV、CRD、RD、Jump、向日葵 AweSun 都有【原文】。对我们核心用户（Mac mini 常驻）是第一刚需。
3. **按账户/用户计费，免费档限“个人非商业”或限功能**：Parsec、TV、AD、Tailscale 都限个人非商业【原文/二手】；ToDesk 免费档限画质和带宽【原文】；Jump 免费版“个人和商用都可”【原文】，是例外。
4. **两步验证**：TV（账户 2FA、可强制公司成员开启）【原文，标题】、AD（无人值守登录 6 位验证码）【原文】、Splash（安全页列出）【原文】、ToDesk（移动端主设备二次验证）【原文】、RD 付费档 $、Jump 企业档 $。
5. **文件传输**：Splash 所有档、RD、TV、ToDesk、AweSun、MS（磁盘重定向）、CRD（二手）都有；Parsec 和 Jump 的官方页没提。
6. **剪贴板同步，且可关闭**：MS（双向、单向、默认关）【原文】、Jump（可关）【原文】、CRD（二手）、RD 仓库有文件复制粘贴组件【原文】。
7. **声音转发**：RD、MS（扬声器重定向）、Parsec 等都有；几乎无人把它当卖点，说明是标配。
8. **多显示器**：Splash（Pro 起 $）、Parsec（个人付费档 $，最多 3 个）、Jump（最多 6 个）、MS（Windows/macOS 有，iOS/Web 没有）、ToDesk（性能版 $）、AweSun（付费档）。**标配，但常被放在付费档**。
9. **端到端加密声明**：Parsec、Jump（“关不掉”）、Splash、RD（NaCl）、CRD、AweSun（RSA2048+AES）都写明【原文】。
10. **各平台客户端 + 手机端**：Windows/macOS/Linux/iOS/Android/网页，多数产品齐全【原文】；Parsec 主要是游戏，主机端只有 Windows 10+ 和 macOS 10.15+【原文】。
11. **隐私屏/黑屏**：ToDesk 免费档卡片里写了隐私屏【原文】；Parsec 付费档 $（Windows 需虚拟显示器，macOS 会关掉所有物理屏并在最后一个访客断开后锁定）【二手：搜索到官方支持文章的摘要】；Splash（安全页“Blank screen”）、Jump、TV（“Black Screen”帮助标题）、向日葵（企业安全项）都有。
12. **虚拟显示器（无屏幕主机）**：Jump 所有档至少 1 个、企业档最多 4 个【原文】；TV 帮助标题“Add a virtual monitor”【原文，标题】；ToDesk 免费档卡片列了虚拟屏【原文】；Parsec 要装虚拟显示器驱动【二手】。
13. **局域网连接**：TV（“Use TeamViewer Remote in LAN”标题）、Jump（Cloudless Fluid，按主机名/IP，企业档 $）【原文】。
14. **Wake-on-LAN**：Splash 所有档【原文】、AweSun【原文】、TV（“Wake up a device remotely”标题）【原文，标题】。
15. **远程重启后自动重连**：Splash（“remotely reboot”）【原文】；AD（2FA 文档提到远程重启用的短期密码）【原文】。
16. **会话内聊天**：Splash（Pro 起 $）、Parsec、TV（保存聊天记录）；**会话录像**：Splash（Pro 起 $）、TV（录像策略）、AD（屏幕录制）、RD（设置页有）。
17. **审计日志**：RD（Individual 起 $）、Parsec（Teams 7 天、企业全量 $）、Jump（Teams Pro 起 5 年云端访问日志 $）、TV（连接报告）、Splash。**多为付费档**。
18. **SSO**：Parsec Teams $、RD Basic（OIDC）$、Splash 企业 $、Jump 企业 $、向日葵企业。**一律是团队/企业档，不是个人档**。
19. **命令行/静默安装**：RD（`--silent-install`、`--config`、`--password`）【原文】；其他未查到。
20. **商用检测**：TV 有，且明文写了触发条件（见 0.3）【原文】；AD“分析使用数据并可停用免费版”【二手】；Parsec 靠条款【原文】。

没找到“标配”证据的：开机自启、托盘、自动更新、多语言、深链接、状态页、数据区域（本次没能读到相应官方页，不下结论）。

### 0.2 付费档常见功能（从价格页读到的）

| 功能 | 在哪一档 | 来源 |
| --- | --- | --- |
| 多显示器 | Parsec 个人付费 $8.33/月起（最多 3 个）；Splashtop Pro 起；ToDesk 性能版 | 【原文】 |
| 隐私屏 | Parsec 个人付费档 | 【原文】价格页 |
| 4:4:4 色彩 | Parsec 个人付费；ToDesk 设计版；AweSun | 【原文】 |
| 聊天、会话录像、多用户同控一台、角色权限 | Splashtop Pro 起（$8.25/用户/月） | 【原文】 |
| 地址簿大容量、ACL、审计日志、2FA、改 ID、Web 控制台 | RustDesk Individual（$11.88/月，1 用户 20 台） | 【原文】 |
| OIDC、LDAP、自定义客户端生成 | RustDesk Basic（$23.88/月） | 【原文】 |
| SSO、SCIM、角色权限、全量审计 | Parsec 企业 $45；Jump 企业 $15.99；Splash 企业（报价）；Tailscale Standard 起 SCIM | 【原文】 |
| 访客/邀请 | Parsec Teams：访客邀请 $25 一次，最长 5 天 | 【原文】 |
| 更多并发会话、更多设备、更高画质 | ToDesk 付费档（并发 2→5/8/10，设备 100→150–300，带宽 4→20–60 Mbps，画质 1080p→2K/4K/8K） | 【原文】 |
| USB 重定向、笔压 | Jump 企业；MS 仅 Windows | 【原文】 |
| API | Parsec 企业；Jump Teams Pro $7.99；AD Ultimate（用例里提到 REST API） | 【原文】 |
| IP 白名单、定时访问、SIEM | Splashtop 企业 | 【原文】 |
| 更多设备数 | TV Remote Access 约 $24.90/月只含 3 台被管设备【二手】；AD 受管设备 Solo 100/Standard 500/Advanced 1000【原文：AnyDesk 授权文档】 | 见 7.2 |

### 0.3 对我们核心用户（常驻跑 AI Agent 的个人/独立开发者）最有价值的差异化

1. **Jump Desktop 已经把“AI Agent 用远程屏幕”做进产品**：首页写“AI agents (Beta): Agents can use the screen, mouse, and keyboard on Windows, Mac, and Linux, with live monitoring in Mission Control. Free with any account; up to 8 computers at once.”【原文】。这是本次读到的所有产品里唯一把 Agent 当卖点的，**和我们的目标用户直接重叠**，要对标。他们的 Agent 是“Agent 操作屏幕”，我们规划的是“MCP 让 Agent 安全连到自己的电脑”，思路不同，但用户会拿来比。
2. **无显示器主机**：Jump（虚拟屏 120/144/240 Hz，睡眠覆盖 Sleep Override）【原文】、TV（无头设备连接、macOS 需屏幕录制授权，更新于 2026-03-15）【原文：帮助标题和搜索摘要】、Parsec（虚拟显示器）都在做。**没有一家单独把“Mac mini 无显示器”当完整故事讲**，现成的空位（和 `PeerDesk 核心用户功能设计.md` 的判断一致）。
3. **免费档策略的反面教材**：TV 的“商用检测”会让免费账户被封，论坛大量误判投诉（见 7.1），AD 同类。Jump 免费版明说“个人和商用都可”【原文】是反向差异化。我们的核心用户是个人开发者，**很多人用的是自己的“商用”电脑**，被检测误封会直接流失。（建议见第 8 节）
4. **自托管/开源**：RustDesk 免费自托管，Pro 服务器 $11.88 起【原文】；我们是 Apache-2.0，RustDesk 是 AGPL-3.0【原文：GitHub】，对想嵌入或商用的人，许可证是可讲的差别。
5. **手机批准/审计**：见 `PeerDesk 核心用户功能设计.md`。本次没在竞品里读到“手机上批准 Agent 的权限弹窗”这类功能，属于未发现，不是确认没有。

---

## 1. 账户和设备

| 功能 | Parsec | TV | AD | RD | Splash | CRD | Jump | MS | ToDesk | 向日葵 | 标配？ | PeerDesk 现状 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 邮箱注册登录 | ✔ | ✔ | ✔ | ✔(Pro) | ✔ | ✔(Google) | ✔ | ✔(Entra) | ✔ | ✔ | 是 | ⬜ 账户未做（README“账户、Stripe 收费”⬜；调研见 `O-accounts-research.md`） |
| Google/Apple 登录 | ? | ? | ? | ? | ? | ✔ Google | ? | ? | ? | ? | 未查到 | ⬜ |
| SSO（SAML/OIDC） | $ Teams | ? | ? | $ Basic(OIDC) | $ 企业 | ? | $ 企业 | ✔ Entra | ? | ✔ 企业 | 团队档才有 | ⬜ |
| 同账户多端登录、设备列表 | ✔ | ✔ | ✔ 地址簿（免费 5 个联系人，更多要授权【原文】） | $ 地址簿 | ✔ | ✔ | ✔ | ✔ | ✔ 免费 100 台 | ✔ | 是 | 🟡 只有本机设备簿（备注、分组、搜索）；账户下的设备列表没做 |
| 分组 | $ Teams | ✔(标题) | ? | $ | ? | ✗ | $ Access Groups 5/不限 | ? | ✔ 免费 3 个，付费不限 | ✔ 企业多层级 | 是（常带上限） | ✅ 本机设备簿有树形分组 |
| 标签 | ? | ? | ? | ? | ? | ✗ | ? | ? | ? | ? | 未查到 | ⬜ 未确认（README 只写备注、分组、搜索） |
| 备注/自定义字段 | ? | ✔ 设备自定义字段(标题) | ? | ? | ? | ✗ | ? | ? | ? | ? | 未查到 | ✅ 备注 |
| 在线状态 | ? | ? | ? | ✔ 免费档写明“Online status” | ? | ✔(列表) | ? | ✗ | ? | ? | 是（RD 免费档就有） | 🟡 分支 `presence` 半成品；服务端在线查询每次有上限（见核心用户设计） |
| 最后在线时间 | ? | ? | ? | ? | ? | ? | ? | ✗ | ? | ? | 未查到 | ⬜ 未确认 |
| 设备改名 | ? | ? | ? | ✔ 改 ID（Individual $） | ? | ? | ? | ? | ? | ? | 未查到 | 🟡 显示机器名，改名未确认 |
| 移除设备 | ? | ? | ? | ? | ? | ✔ 禁用远程连接/卸载 | ? | ? | ? | ? | 未查到 | 🟡 本机设备簿可删；账户侧未做 |
| 同账户免密码连接 | ? | ✔ | ? | ? | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ | 是 | ⬜ 要等账户 |
| 可信设备（推送确认） | ? | ✔ “Trust devices via push notification”(标题) | ? | ? | ✔ 强制设备认证 | ? | ? | ? | ✔ 手机二次验证 | ? | 否，少数 | ⬜ |
| 两步验证 | ? | ✔ 账户+可强制(标题) | ✔ 仅无人值守连接 | $ Individual | ✔ 列出，未标档 | ? | $ 企业强制 | ✔ Entra | ✔ 手机二次验证 | ? | 多数有 | ⬜ README“双因素 ⬜” |
| 登录提醒/新设备提醒 | ? | ? | ? | ? | ✗ 安全页没提 | ? | ? | ? | ? | ? | 未查到 | ⬜ |

来源：Parsec/RD/Splash/Jump/ToDesk 价格页、AD 授权与 2FA 文档、TV 帮助中心目录、Tailscale 价格页均【原文】；CRD 官方页只写了“通过 Google 账户登录/按 PIN 访问”（首页和帮助页）。未标注的格子多为“?”。

补充：Tailscale（设备管理参照）【原文】：Personal 免费，最多 6 用户，无限用户设备，50 个标签资源，3 个 ACL 组；Standard $8/用户/月，Premium $18；设备审批、设备态势（posture）在页面里有但没读清分档；配置审计日志所有档都有，网络流日志从 Premium 起。设备“Needs approval”徽标见 `PeerDesk 核心用户功能设计.md`。

---

## 2. 连接方式

| 功能 | Parsec | TV | AD | RD | Splash | CRD | Jump | MS | ToDesk | 向日葵 | 标配？ | PeerDesk 现状 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 设备码 + 密码 | ✗(账户制) | ✔ ID+密码 | ✔ | ✔ | ? | ✔ 一次性码 | ✗ | ✗ | ✔ | ✔ | 是 | ✅ 设备码 + SPAKE2 密码（`core/auth.rs`） |
| 临时/一次性密码 | ? | ? | ? | ✔ 主界面“One Time Password”【原文】 | ? | ✔ 分享码一次有效 | ? | ✗ | ? | ? | 常见 | 🟡 `gui/app.rs` 里有相关字样，行为未确认 |
| 固定密码/无人值守 | ✔ | ✔ 标题 | ✔ 设密码即启用 | ✔ `--password` | ✔ 所有档 | ✔ PIN | ✔ | ✔ | ✔ | ✔ | **是（最核心）** | 🟡 Windows 服务 ✅（登录界面/UAC 待真机）；macOS LaunchDaemon、Linux systemd ⬜；设置页卡片 ⬜ |
| 连接请求需被控端确认 | ? | ? | ? | ? | ? | ✔ 分享时选 Share，每 30 分钟重确认 | ? | ✗ | ? | ? | 未查到 | ⬜ README“被控端有人连入时通知，可接受或拒绝 ⬜” |
| 邀请链接 | ✔ 单链接分享；访客 $ | ✔ 添加设备链接 | ? | ? | ? | ✔ 一次性码 | ? | ✗ | ? | ? | 常见 | ⬜ 未确认 |
| 局域网发现 | ? | ✔ LAN 连接(标题) | ? | ? | ? | ✗ | ✔ Cloudless Fluid 按主机名/IP $ | ✔ 按地址 | ? | ? | 少数 | 🟡 局域网地址直连 ✅（`net/candidates.rs`），自动发现 ⬜（crates 里没找到 mDNS/广播发现，**未确认**） |
| Wake-on-LAN | ? | ✔ 标题 | ? | ? | ✔ 所有档 | ✗ | ✗ 页面提睡眠覆盖 | ✗ | ? | ✔ | 3 家以上有，主机会睡的用户必需 | ⬜ grep crates 无 |
| 断线自动重连 | ? | ? | ? | ? | ? | ? | ? | ? | ? | ? | 未查到 | ✅ 最多 3 次（`net/viewer.rs` `RECONNECT_TRIES`）；GUI 行为未确认 |
| 远程重启并自动重连 | ? | ? | ✔ 远程重启短期密码 | ? | ✔ 远程重启 | ? | ? | ✗ | ? | ? | 常见 | ⬜ |
| 深链接/URL 启动 | ? | ? | ? | ? | ? | ? | ? | ? | ? | ? | 未查到 | ⬜ crates 无 `peerdesk://` |

---

## 3. 会话内功能

| 功能 | Parsec | TV | AD | RD | Splash | CRD | Jump | MS | ToDesk | 向日葵 | 标配？ | PeerDesk 现状 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 剪贴板（文本） | ? | ✔ 可控制同步 | ? | ✔ | ? | ✔(二手) | ✔ 可关 | ✔ 默认关，可单向 | ✔ 复制粘贴 | ✔ | 是 | ✅ 文本/图片/20 MB 以内文件，防回声；**会话内开关 ⬜**（助手里“剪贴板开关 ⬜”） |
| 剪贴板（文件） | ? | ? | ? | ✔ | ? | ✔(二手) | ? | ✔ | ✔ | ✔ | 常见 | ✅ 同上 |
| 文件传输 | ✗ 页面没提 | ✔ 文件传输会话+文件盒 | ? | ✔ | ✔ 所有档，含不开会话传输 | ✔(二手，单向、无拖放) | ✗ 页面没提 | ✔ 磁盘重定向 | ✔ 免费有基础服务器 | ✔ 称无大小限制，实际看套餐 | **是** | 🟡 协议 ✅（断点续传、哈希、防逃逸）、两栏文件管理第一版 🟡 待真机 |
| 断点续传 | ? | ? | ? | ? | ? | ? | ? | ✗ | ? | ? | 未查到 | ✅ 协议带 |
| 多显示器切换/全部显示 | $ 最多 3 个 | ✔ 虚拟屏帮助 | ? | ? | $ Pro 起 | ✔(二手，来源互相矛盾) | ✔ 最多 6 个 4K | ✔ Win/mac，iOS/Web ✗ | $ 性能版 | $ | 是（常付费） | ⬜ 助手“切换显示器 ⬜”；crates 有显示器枚举（platform），会话内切换**未确认** |
| 分辨率/缩放 | ✔ | ? | ? | ? | ? | ? | ✔ | ✔ 动态分辨率 Win/mac/Web | ✔ | ✔ | 是 | ✅ 按控制端窗口大小编码、原始大小、全屏 |
| 画质/帧率选项 | ✔ 高级设置 | ? | ? | ? | ? | ✗ | ✔ 到 480 fps | ✔ | $ 1080p/30→8K/360 按档 | ✔ 144Hz | 是 | ⬜ 助手“画质和帧率 ⬜”；码率自动调整 ✅ |
| 声音 | ✔ | ? | ? | ✔ | ? | ✔(二手) | ✔ | ✔ 扬声器、麦克风 | ✔ | ? | 是 | ✅ Opus、抖动缓冲；实机听感待测 |
| 键盘映射/快捷键 | ✔ 键盘映射 | ✔ 直接键盘模式、剪贴板当按键粘贴 | ? | ? | ? | ? | ? | ✔ | ? | ? | 是 | ✅ 12 个快捷键、Ctrl↔Command 对调；Win 键/Alt+Tab 原生捕获 ⬜ |
| 黑屏/隐私模式 | $ 付费档，Win 需虚拟屏 | ✔ Black Screen(标题) | ? | ? | ✔ | ✗ | ✔ Privacy Mode | ✗ | ✔ 隐私屏、远程关物理屏 | ✔ 企业 | 是 | ⬜ README“隐私屏、锁定被控端键鼠 ⬜” |
| 锁定被控端键鼠 | ? | ? | ? | ? | ? | ? | ? | ✗ | ? | ? | 未查到 | ⬜ |
| 会话录像 | ? | ✔ 录像策略 | ✔ 屏幕录制 | ✔ 设置页 | $ Pro 起 | ✗ | ? | ✗ | ? | ? | 常见，多为付费 | ⬜ 未确认（`net/recorder.rs` 是诊断记录线程，不是录视频） |
| 会话内聊天 | ✔ | ✔ 保存聊天记录 | ? | ? | $ Pro 起 | ✗ | ? | ✗ | ? | ? | 常见 | ⬜ crates 无 |
| 白板/标注 | ? | ? | ? | ? | ? | ✗ | ? | ✗ | ✔ 白板批注 | ? | 少数 | ⬜ |
| 远程打印 | ? | ✔ TeamViewer Printing | ? | ? | ✔ 所有档 | ✗ | ? | ✔ 打印机重定向（Win/mac，iOS/Android ✗） | ? | ? | 常见（偏办公） | ⬜ |
| USB 重定向 | ? | ? | ? | ? | ? | ✗ | $ 企业 | ✔ 仅 Windows | ? | ? | 少数 | ⬜ |
| 虚拟显示器 | ✔(二手) | ✔ 标题 | ? | ? | ? | ✗ | ✔ 1 个所有档，企业最多 4 个 | ✗ | ✔ 虚拟屏 | ? | 越来越常见 | ⬜ README“无显示器主机的虚拟显示器 ⬜” |
| 只看模式 | ? | ? | ? | ✔ 权限里可限制 | ? | ? | ✔ Watch | ? | ? | ? | 常见 | ⬜ 未确认（crates 里 `view_only` 无命中，只命中无关文件） |
| 多人同时查看/控制 | ? | ? | ? | ? | $ Pro 起，最多 3 人 | ✗ | ? | ✗ | $ 并发会话；“多控一” 加购 | ? | 少数 | ⬜ 未确认 |
| 远程终端 | ✗ | ✔ Remote terminal(标题) | ? | ? | ? | ✗ | ? | ✗ | ? | ? | 少数 | ⬜ 命令行有，会话内终端 ⬜ |
| 空闲超时断开/水印 | ? | ? | ? | ? | ✔ 空闲超时 | ? | ✔ 空闲超时、水印 | ? | ? | ✔ 自动锁定 | 常见（安全档） | ⬜ |
| TCP 隧道/端口转发 | ✗ | ✗ | ? | ✔ | ? | ✗ | ? | ✗ | ? | ? | 少数 | ⬜ 报告 D 里的“端口转发”也没做 |

---

## 4. 安全

| 功能 | Parsec | TV | AD | RD | Splash | CRD | Jump | MS | ToDesk | 向日葵 | 标配？ | PeerDesk 现状 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 端到端加密声明 | ✔ 加密 P2P | ✔(Security Statement 标题) | ? | ✔ NaCl | ✔ 屏幕流端到端加密 | ✔ 全程加密 | ✔ “关不掉” | ✔ TLS | ? | ✔ RSA2048 + AES | 是 | ✅ 有（中继只见密文，`core/relay.rs`）；对外说明文档 ⬜ 未确认 |
| 访问控制列表/白名单 | ? | ✔ Block and allowlist(标题) | ✔ ACL（按 ID/别名） | $ Individual | $ 企业：IP 白名单 | ✗ | ? | ✔ 域 | ? | ✔ 企业 | 常见 | ⬜ README“访问控制列表 ⬜” |
| 权限（只看、禁文件传输、禁剪贴板） | ✔ 剪贴板可关 | ✔ 策略 | ✔ | ✔ 控制权限设置 | ✔ 角色 | ✗ | ✔ Connect Settings(企业) | ✔ 重定向默认关、管理员控制 | ✔ 文件传输权限按用户 | ✔ 企业 | 是 | ⬜ 未确认 |
| 审计日志 | $ Teams 7 天/企业全量 | ✔ 连接报告 | ? | $ Individual | ✔ 标 2FA 与会话审计 | ✗ | $ Teams Pro：5 年云端日志 | ✔ | $ AI 审计 ¥0.99/次 | ✔ 企业 | 常见（付费） | ⬜ |
| 会话记录（谁连过） | $ | ✔ | ? | $ | ✔ | ✗ | $ | ✔ | ? | ✔ | 常见 | 🟡 被控端日志有，面向用户的记录 ⬜ 未确认 |
| 自动锁屏/断开后锁 | ✔ macOS 最后访客断开后锁定(二手) | ? | ? | ? | ✔ 自动锁屏 | ? | ? | ? | ? | ✔ 自动会话锁定 | 常见 | ✅ 被控端锁屏时拒绝本次连接并说明原因（`client/local.rs`）；断开后锁屏 ⬜ |
| 超时断开 | ? | ? | ? | ? | ✔ | ? | ✔ | ? | ? | ? | 常见 | ⬜ |
| 暴力破解防护 | ? | ? | ? | ? | ? | ? | ? | ? | ? | ? | **未查到** | ✅ 密码失败限次（`core/auth.rs`，SPAKE2） |
| 合规（SOC 2、ISO 27001、GDPR） | ? | ? | ? | ? | ✔ ISO 27001/SOC 2/GDPR/CCPA | ? | ✔ SOC 2 | ? | ? | ? | 团队档要求 | ⬜ |
| 屏幕共享指示器 | ? | ✔(标题) | ? | ? | ✔ 远程连接通知 | ? | ? | ? | ? | ? | 少数 | ⬜ 被控端连入通知 ⬜ |

---

## 5. 管理和团队

| 功能 | Parsec | TV | AD | RD | Splash | CRD | Jump | MS | ToDesk | 向日葵 | 标配？ | PeerDesk 现状 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 团队/组织 | $ Teams（$30/用户/月） | ✔ | ✔ Standard 20 用户起 | $ Basic 10 用户 | $ Pro 起 | ✗ | $ Teams Pro | ✔ | ✔ 团队版 | ✔ | 是（团队档） | ⬜ |
| 角色权限 | $ 企业 | ✔ 经理权限(标题) | ? | ? | $ Pro 起 | ✗ | ? | ✔ | ✔ | ✔ | 是（团队档） | ⬜ |
| 集中部署（MSI/MDM） | ? | ✔ 推出配置(标题) | ? | ✔ MSI/静默安装 | ✗ 页面没提 MSI | ✗ | ✔ 自定义安装包 | ✔ MSI | ✔ 企业批量部署 | ✔ 批量部署 | 常见 | ⬜ 安装包有，MSI/MDM 预置令牌 ⬜（核心用户设计 P2） |
| 策略 | ? | ✔ 策略与规则 | ? | ✔ 按设备/用户/组下发 | ? | ✗ | ✔ Connect Settings $ | ✔ 管理员配置 | ? | ✔ | 团队档 | ⬜ |
| 使用报表 | ? | ✔ 连接报告 | ? | ? | ? | ✗ | ? | ? | ? | ✔ | 团队档 | ⬜ |
| 计费管理、发票 | ✔ 集中计费 $ | ✔ | ? | ✔ 银行转账可 | ? | ✗ | ? | ✗ | ? | ? | 团队档 | 🟡 中继计费账本 ✅（`core/billing.rs`），收费 ⬜ |
| SCIM | $ 企业 | ? | ? | ? | ? | ✗ | $ 企业 | ✔ | ? | ? | 企业档 | ⬜ |
| 自定义品牌客户端 | ? | ? | ? | $ Basic | ? | ✗ | $ 自定义安装包 | ✗ | ? | ? | 少数 | ⬜ |
| API | $ 企业 Teams API | ? | $ Ultimate（用例里写 REST API） | ? | ? | ✗ | $ Teams Pro REST API | ✔ | ? | ? | 团队档 | ⬜ 服务端有 HTTP 接口，面向用户的 API ⬜ |

---

## 6. 平台和体验

| 功能 | Parsec | TV | AD | RD | Splash | CRD | Jump | MS | ToDesk | 向日葵 | 标配？ | PeerDesk 现状 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Windows | ✔ 主机 Win10+ | ? | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ | 是 | ✅ |
| macOS | ✔ 主机 10.15+ | ? | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ | ✔ | 是 | ✅（ScreenCaptureKit、VideoToolbox；待真机） |
| Linux | ✔(客户端，图标) | ? | ✔ | ✔ | ✔ | ✔ 主机 Debian 包 | ✔ | ✗ | ✔ | ✔ | 是 | ✅ X11；Wayland ⬜ |
| iOS | ✔(平板/手机) | ? | ? | ✔ | ✔ 一次一台 | ✔ | ✔ 一次性买断（Mac/iOS） | ✔ | ✔ | ✔ | 是 | ⬜（有 iOS 方案报告，未做） |
| Android | ? | ? | ✔ | ✔ | ✔ 最多 4 台 | ✔ | ✔ | ✔ | ✔ | ✔ | 是 | 🟡 工作树 `android`、报告 R，状态未在总表写明 |
| 网页客户端 | ? | ? | ? | ✔ | ✔ web 控制台 | ✔ | ✔ | ✔ | ✔ 付费 | ? | 常见 | ⬜ |
| 安装包签名 | ? | ? | ? | ✔ 自定义客户端含代码签名 $ | ? | ✔ Google 分发 | ? | ✔ | ? | ? | 是（收费必需） | ⬜ README“代码签名、公证、自动更新 ⬜”，需公司主体和 Apple 开发者账号 |
| 自动更新 | ? | ? | ? | ? | ? | ? | ? | ? | ? | ? | 未查到 | ⬜ |
| 开机自启、托盘、后台常驻 | ? | ? | ? | ✔ 服务控制 | ? | ✔ 主机服务 | ✔ Connect | ✗ | ? | ✔ 无感守护 | 是 | ✅ 待真机（托盘、进程守护、自启） |
| 多语言 | ? | ? | ? | ? | ? | ? | ? | ? | ✔ | ✔ | 未查到 | 🟡 中英（跟随系统或手动） |
| 命令行 | ? | ? | ? | ✔ `--config/--password/--silent-install` | ? | ✗ | ? | ✔ | ? | ? | 少数 | 🟡 `peerdesk service/agent/hw-check` 等；会话命令行可用 |
| 深链接 | ? | ? | ? | ? | ? | ? | ? | ? | ? | ? | 未查到 | ⬜ |

---

## 7. 支持和商业

### 7.1 免费档限制和商用检测

| 产品 | 免费档 | 商用规则 | 来源 |
| --- | --- | --- | --- |
| Parsec | 个人免费，无多显示器/隐私屏/4:4:4 | 个人免费版和 $8.33 的 Individuals 都“仅限个人非商业”，**商用必须 Teams（$30）或 Enterprise（$45）** | 【原文】parsec.app/pricing |
| TeamViewer | 免费个人版 | 商用定义：“any use … directly or indirectly paid for”；居家办公查一下工作邮件也算；监控服务器系统设备也算；被控方连入也算。被标记后，**连接超时/无法连接**；卸载不能解除，只能提交重置申请，官方称 72 小时内回复 | 【原文】TeamViewer 知识库“commercial use suspected”；论坛大量误判投诉【二手】 |
| AnyDesk | 免费个人版 | 官网：用于工作任务、付费技术支持、或访问工作用设备都要授权；条款允许分析使用数据，模式像商用就可停用；官方有“个人使用白名单申请”。会话时长限制多为用户反映 | 【二手】搜索摘要（AnyDesk 官网本环境打不开） |
| RustDesk | 开源免费（AGPL-3.0）；自托管 Pro 服务器 $11.88/月起，不是 SaaS | 无商用检测（自托管） | 【原文】 |
| Splashtop | 无免费档，只有“功能完整、无需信用卡”的试用（时长页面未写） | 每用户一个订阅 | 【原文】 |
| Chrome 远程桌面 | 免费（首页没写价格，**免费**只能从未写价格推断，不算原文） | 未读到 | 【原文】首页/帮助页 |
| Jump Desktop | 免费 Windows/Android/网页；Mac、iOS 一次性买断，Mac 14 天试用；免费版“fully functional and can be used for personal and business use” | **不限商用**；Teams Pro $7.99、Enterprise $15.99，14 天试用无需信用卡 | 【原文】 |
| 微软 Windows App | 免费；连接靠 RDP 服务（需 Windows 专业版以上或 Azure/365） | 不适用 | 【原文】 |
| ToDesk | 个人版免费：1080P、30 FPS、4 Mbps、100 台、3 个分组、并发 2 | “仅限个人使用”；Pro ¥158/年（2K/60FPS/20 Mbps）…性能版 ¥638/年（8K/360FPS/60 Mbps） | 【原文】todesk.com/pricing.html |
| 向日葵 | 个人免费；7 天付费版试用 | 价格页未读到 | 【原文】首页；价格不详 |
| Tailscale | Personal $0，最多 6 用户，“非商业” | Standard $8/用户/月，Premium $18；新带自有域的 tailnet 有 14 天企业试用 | 【原文】 |

### 7.2 价格档位（官方页；查看日期 2026-11-28；未读到官方页的写二手）

| 产品 | 档位（年付折月，美元） | 关键数量限制 | 标记 |
| --- | --- | --- | --- |
| Parsec | Free；Individuals $8.33（原价 $9.99）；Teams $30（$35）；Enterprise $45（只年付）；访客邀请 $25/次 | 多显示器最多 3 个；Teams 试用 14 天 | 【原文】 |
| TeamViewer | Remote Access $24.90；Business $50.90；Premium $112.90；Corporate $229.90（年付、美元、不含税；各来源数字不一致） | Remote Access 1 并发、3 台被管设备；Business 200 台；Corporate 30 许可、500 台 | 【二手】（官方价格页方案表没加载出来；该页写：自动续约 12 个月，退订需提前 28 天） |
| AnyDesk | Solo / Standard / Advanced / Ultimate；价格来源互相矛盾（G2：$22.90/$35.90/$79.90；Splashtop 引用的 2026 价：$28.90/$49.90/$111.90） | 官方授权文档：Solo 1 用户 100 台，Standard 20 用户 500 台，Advanced 100 用户 1000 台，并发连接 1/1/2；地址簿、命名空间是 Standard/Advanced 的加购 | 价格【二手】，限额【原文】 |
| RustDesk | Free；Individual $11.88（1 用户 20 台）；Basic $23.88（10 用户 100 台）；Customized 基础 $23.88 + $1.20/用户 + $0.12/台 | 只年付，不自动续约，到期前 14 天提醒；没有试用，需联系 | 【原文】 |
| Splashtop | Solo $6（2 台）；Pro $8.25/用户（10 台）；Performance $13/用户；Enterprise 报价 | 只年付；每个用户一个订阅 | 【原文】 |
| Chrome 远程桌面 | 免费 | — | 价格【未核实】 |
| Jump Desktop | 个人免费/一次性买断；Teams Pro $7.99；Enterprise $15.99 | 年付省 20% | 【原文】 |
| 微软 | 不收客户端费 | — | — |
| ToDesk | 见 7.1，人民币，月付/自动续费/年付三种 | 加购：多控一 ¥29.9/月，全球节点 ¥66/月，屏幕墙 ¥99/月/5 台 | 【原文】 |
| Tailscale | Personal $0；Standard $8；Premium $18；Enterprise 报价 | 用户设备不限 | 【原文】 |

**对我们的定价含义**（和 `PeerDesk 美国市场赚钱方案.md` 对照时用）：
- 按用户计、年付折月的个人付费档集中在 **$6–$12/月**（Splashtop Solo $6、Parsec $8.33、RustDesk $11.88、Jump Teams Pro $7.99）。TeamViewer 个人级到 $24.90 是少数高价，但只含 3 台被管设备，对常驻多台机器的用户是痛点。
- **设备数**是最常用的分档轴：Splashtop Solo 只有 2 台、TeamViewer 3 台、RustDesk Individual 20 台、ToDesk 免费 100 台。
- 退款：Parsec、RustDesk、Splashtop 价格页都**没写退款**【原文】；TeamViewer 有“7-day money back guarantee”的搜索摘要（二手，来源页标题）。其余未查到。

### 7.3 支持、状态页、隐私政策、数据区域

| 项目 | 读到的 |
| --- | --- |
| 客服渠道 | Parsec：个人档只有账户类工单，Teams 起产品工单且首次响应 24 小时 SLA【原文】；Tailscale：标准支持“best-effort、工作日”，SLA 另买【原文】；RustDesk 免费档社区支持【原文】。其余未查到 |
| 状态页 | 本次没有去找，**未查到** |
| 隐私政策、数据区域 | Splash 安全页：基础设施在 AWS 等，**没写数据存放区域**【原文】；其余**未查到**。我们的 Supabase 选项目区域美东，见 `O-accounts-research.md` |
| 合规 | Splash：ISO/IEC 27001、SOC 2、GDPR、CCPA，支持 HIPAA/PCI/FERPA 需求；Jump：SOC 2【原文】 |

---

## 8. 建议补齐的顺序

原则：目标是“能收钱”，所以先补**收费的前提**（账户、设备、信任），再补**核心用户的刚需**（无人值守、无显示器、唤醒），最后才是团队档功能。每条一句理由。

### 第一批：没有就收不了钱，或收了留不住

1. **账户注册登录 + 账户下的设备列表 + 在线状态/最后在线**：计费挂账户（CLAUDE.md 定的）；竞品免费档普遍有，RustDesk 免费档就有在线状态。
2. **同账户免密码连接 + 设备新增/移除/改名**：这是账户存在的最直接价值，Splash、TV、CRD、Jump、ToDesk 都有。
3. **被控端连入通知和确认（接受/拒绝）、断开后锁屏**：安全底线，CRD 分享需确认，Splash/向日葵有连接通知，我们 README 里是 ⬜。
4. **两步验证（邮箱 OTP 或 TOTP）+ 新设备登录提醒**：付费账户被盗是最贵的客诉；TV、AD、Splash、ToDesk 都有 2FA，新设备提醒本次没在任何一家读到，属于可做出来的差异。
5. **代码签名、公证、自动更新**：没签名的安装包会被 SmartScreen/Gatekeeper 拦，没人敢付费；这项我们 ⬜，需公司主体和 Apple 账号。
6. **订阅与免费档规则（Stripe）**：先决定“商用是否限制”（见第 9 节），Jump 不限、TV 严格，两个极端各有代价。

### 第二批：核心用户（常驻主机）的刚需

7. **macOS LaunchDaemon / Linux systemd 无人值守 + 设置页“无人值守”卡片**：我们的核心用户用 Mac mini 和 Linux 主机，现在只有 Windows 服务；Splash 所有档都有无人值守。
8. **虚拟显示器（无显示器主机）+ 黑屏/隐私屏 + 锁定被控端键鼠**：Jump、TV、Parsec、ToDesk 都有虚拟屏或隐私屏，且“Mac mini 没接屏幕”是核心用户第一痛点之一。
9. **Wake-on-LAN + 远程重启后自动重连**：主机会睡会重启，Splash 所有档、TV、AweSun 有；没有它“不在家就失联”。
10. **多显示器切换 + 会话内剪贴板/画质开关**：竞品常把多显示器放付费档，我们可以做成付费升级点；剪贴板开关是安全需要。
11. **文件管理真机验证 + 文件传输权限开关**：协议已做完，竞品几乎都有，缺的是权限控制。

### 第三批：可以以后

12. **聊天、白板、远程打印、USB 重定向**：竞品多放付费档，对 AI Agent 用户价值低。
13. **团队/组织、角色、SSO、SCIM、审计日志、策略、MSI/MDM、API**：要等有团队客户再做；但“审计日志”可以先做个人版（谁在何时连过我的机器），对 Agent 用户有安全价值，也是差异化。
14. **会话录像**：Splash Pro 起、TV 有，对排查 Agent 做了什么有用，但存储成本要先算。
15. **网页客户端、深链接、命令行扩展**：命令行对开发者有用，深链接对 MCP 接入有用（查不到竞品，可视为差异化）。
16. **局域网自动发现、标签、设备缩略墙**：核心用户设计里 P3。

### 本次没能回答的、需要后续补查

- TeamViewer、AnyDesk 的官方功能页和价格页（官网拒绝访问）：账户、分组、审计、会话内功能的分档只能靠帮助中心标题。
- Parsec 官方功能矩阵（403）：剪贴板、文件传输、2FA 在 Parsec 的状态。
- Chrome 远程桌面：文件传输、多显示器、剪贴板的官方说明（只读到二手，且来源互相矛盾）。
- 向日葵国内版价格、免费版限制。
- 各产品的自动更新、深链接、状态页、数据区域、退款条款、登录提醒。
- 微软 Windows App 的 Wake、会话内登录提示。

---

## 9. 需要产品负责人决定的问题

1. **免费档是否限制商用**：核心用户常在“商用”电脑上用，TV/AD 式检测会误伤；Jump 的“个人商用都免费，团队功能收费”更贴合“卖连接”。建议先不做商用检测，按设备数和并发会话分档，团队功能（SSO、审计、SCIM）单独收。
2. **“标配”里哪些免费、哪些付费**：竞品常把多显示器、隐私屏、聊天、录像放付费档；账户、设备列表、无人值守、文件传输、剪贴板、声音基本是免费。我们的免费档至少要包含后者。
3. **Jump Desktop 的 Agent 功能**要不要正面对标（见 0.3 第 1 条）。

---

## 来源（打开过的网址）

【原文】官方页面，已读到内容：
- https://parsec.app/pricing
- https://parsec.app/features（只有营销语）
- https://rustdesk.com/pricing/
- https://rustdesk.com/docs/en/self-host/client-configuration/
- https://rustdesk.com/docs/en/client/
- https://rustdesk.com/docs/en/（首页夹带了让 AI 列清单的文字，未执行）
- https://github.com/rustdesk/rustdesk
- https://www.splashtop.com/pricing/remote-access
- https://www.splashtop.com/products/remote-access（本环境返回的是功能表）
- https://www.splashtop.com/security
- https://remotedesktop.google.com/（夹带了格式要求文字，未执行）
- https://support.google.com/chrome/answer/1649523
- https://jumpdesktop.com/
- https://jumpdesktop.com/pricing-plans.html
- https://learn.microsoft.com/en-us/windows-app/compare-platforms-features
- https://www.todesk.com/（只读到前 10 万字符）
- https://www.todesk.com/pricing.html
- https://sunlogin.oray.com/
- https://awesun.aweray.com/en
- https://tailscale.com/pricing
- https://www.teamviewer.com/en-us/pricing/（方案表为空，只读到条款）
- https://www.teamviewer.com/en-us/global/support/knowledge-base/teamviewer-remote/remote-control/（帮助中心目录）
- https://www.teamviewer.com/en-us/global/support/knowledge-base/teamviewer-remote/security/two-factor-authentication-for-your-account/（目录）
- https://www.teamviewer.com/en-us/global/support/knowledge-base/teamviewer-classic/licensing/personal-use/commercial-use-suspected/
- https://support.anydesk.com/docs/features
- https://support.anydesk.com/docs/two-factor-authentication
- https://support.anydesk.com/Licensing

【二手】搜索结果摘要（没读原文）：
- TeamViewer 价格：teamviewer.com/license-assistant、community.teamviewer.com 的许可概述、RealVNC 与 Splashtop 的对比文章；TeamViewer 商用误判：community.teamviewer.com 论坛帖
- AnyDesk 价格与商用：anydesk.com/en/professional-use、anydesk.com/en/commercial-use、G2、Capterra、Splashtop/RustDesk 博客
- Parsec 隐私模式、多显示器：support.parsec.app 的 Privacy Mode 与 Multiple Monitors and Virtual Displays 文章（只看到摘要）
- Chrome 远程桌面文件传输、多显示器：AirDroid、AnyViewer、TSplus、PCWorld 文章
- 微软 Windows App 重定向：learn.microsoft.com 的比较页（主表已读，见上）

打不开（403/404）：anydesk.com/en/pricing、anydesk.com/en/features、anydesk.com/en/professional-use、teamviewer.com/en-us/pricing/remote-access/、teamviewer.com/en-us/remote-access/、support.parsec.app/hc/en-us 及 Feature Matrix、splashtop.com/pricing/personal 类页面、realvnc.com 的 TeamViewer 价格文章、jumpdesktop.com/pricing/、microsoft 的 windows-app-overview 与 windows-app-features 旧链接。

PeerDesk 现状来源：`docs/research/README.md`“实现状态总表”、`docs/NEXT.md`、`crates/` grep（Wake-on-LAN、mDNS、深链接、聊天、view-only、显示器切换均无命中，写“未确认”）。
