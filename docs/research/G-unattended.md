# G 无人值守访问（2026-11）

范围：远端没人时的控制。锁屏、登录界面、已注销、UAC 安全桌面、Ctrl+Alt+Del、快速用户切换；没接显示器的主机；同行做法；安全模型。
来源标注规则：带链接的是本次读过的一手资料；没读到原文、凭记忆或二手信息写的，标 **[未核实]**。
RustDesk 是 AGPL-3.0（[LICENCE](https://github.com/rustdesk/rustdesk/blob/master/LICENCE) 已确认），只读文档，不读代码、不抄代码。

## 0. 结论先行

1. **无人值守的核心是“常驻服务 + 每会话代理”**：服务以最高权限常驻，负责连接、鉴权、拉起代理；代理跑在目标桌面里做采集和输入。三个平台都是这个形状，只是“目标桌面”的含义不同。
2. **Windows 最难也最值钱**：要覆盖登录界面、UAC 安全桌面、RDP 断开后的控制台会话。必须做 SYSTEM 服务加跟随桌面切换的代理。
3. **macOS 能做到登录窗口，做不到 FileVault 预启动**：必须诚实告知，不静默降级。
4. **Linux 分两条路**：X11 好做；Wayland 靠 portal 的 restore token（首次需要人点一次）或 GNOME Remote Desktop 系统级模式。没有统一方案，按桌面环境分别做。
5. **虚拟显示器**：Windows 用 IddCx 驱动（MIT 的 Virtual-Display-Driver 可用，要自己签名）；macOS 用私有 API CGVirtualDisplay；Linux 用 Xvfb/Xdummy 或 compositor 的 headless 后端。HDMI 假负载是兜底，要写进文档。
6. **账户绑定的受信设备为主，设备级固定密码为辅**。理由见第 5 节和 CLAUDE.md 的计费原则（计费挂账户，不挂设备指纹）。
7. **驱动签名是最长的前置任务**（Windows 要微软合作伙伴中心的账户和证书，审核周期不可控），要最早启动。

## 1. Windows

### 1.1 会话和桌面模型

- 服务跑在 Session 0，没有用户界面；用户登录后在 Session 1 及之后。Session 0 隔离从 Vista 起就有。**[未核实]** 具体措辞请对照 [Session 0 Isolation](https://learn.microsoft.com/en-us/windows/win32/services/interactive-services)。
- 每个窗口站（WinSta0）下有多个桌面：`Default`（用户桌面）、`Winlogon`（登录界面、锁屏、UAC 提示、Ctrl+Alt+Del 界面）。UAC 提示默认出现在安全桌面（Winlogon 桌面）上，普通用户进程看不到也无法注入输入。**[未核实]**（凭 Windows 内部机制的通行说法，没读到 Microsoft Learn 原文）
- 因此采集和输入必须能：
  1. 找到当前活动会话；
  2. 在该会话里以 SYSTEM 身份运行；
  3. 线程能切换到当前输入桌面。

### 1.2 服务拉起代理的标准做法

- `WTSGetActiveConsoleSessionId` 取控制台会话 ID（本次没读到原文页面，链接 404：**[未核实]**，建议到 Microsoft Learn 搜该函数确认。它在没有控制台会话时返回 0xFFFFFFFF）。
- 取会话令牌：`WTSQueryUserToken` 只能拿到登录用户的令牌，登录界面时没有用户，所以拿不到。无人登录时的做法是：复制 `winlogon.exe`（在目标会话里的那个）进程的令牌（`OpenProcessToken` + `DuplicateTokenEx`），再 `SetTokenInformation(TokenSessionId)` 或直接用该令牌 `CreateProcessAsUser`，并指定 `lpDesktop = "winsta0\\winlogon"` 或 `winsta0\\default`。这是 TightVNC、UltraVNC、Sunshine、RustDesk 等通行做法。**[未核实]**（来自对这些项目行为的记忆，没读源码；RustDesk 不读）
- 订阅会话变化：服务注册 `SERVICE_CONTROL_SESSIONCHANGE`（登录、注销、锁定、解锁、控制台连接/断开、远程连接/断开），事件到来时重启或迁移代理。
- 代理一旦跑在 SYSTEM + 目标会话里，就可以：
  - 循环 `OpenInputDesktop`，若输入桌面名变了（Default ↔ Winlogon），调 `SetThreadDesktop` 切换，并重建采集器。**[未核实]**
  - 注意：`SetThreadDesktop` 要求线程没有该桌面上的窗口或钩子，所以采集和输入各用独立线程，切换时重建线程最稳。

### 1.3 采集

| 方案 | 登录界面/UAC | 说明 |
| --- | --- | --- |
| DXGI Desktop Duplication | 可以，前提是线程已在当前输入桌面上，且进程是 SYSTEM | 切桌面时 `AcquireNextFrame` 会返回 `DXGI_ERROR_ACCESS_LOST`，要释放重建。UAC 安全桌面、全屏独占应用也会触发。**[未核实]**（凭经验） |
| Windows.Graphics.Capture (WGC) | 不行 | 依赖窗口或显示器的合成，且需要用户会话，不适合登录界面。**[未核实]** |
| GDI BitBlt / 镜像驱动 | 可以但慢 | 只做兜底（例如 DXGI 失败时） |

对标：Parsec 和 Sunshine 用 DXGI 复制；Sunshine 官方文档明确说它以服务方式运行时权限和用户不同，见 [Sunshine 排障](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2troubleshooting.html)（该页没有讲 UAC/锁屏）。

### 1.4 输入和 Ctrl+Alt+Del

- 注入：在目标桌面上的线程用 `SendInput`。安全桌面上也有效的前提是线程已 `SetThreadDesktop` 到 Winlogon 桌面且进程是 SYSTEM。**[未核实]**
- UIPI：中高完整性窗口不接收低完整性进程的输入；以 SYSTEM 运行的代理没有这个问题。
- Ctrl+Alt+Del：无法用 `SendInput` 模拟，只能用 [`SendSAS`](https://learn.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas)（已读）：
  - 调用者必须是服务，或 manifest 里 `uiAccess="true"`（需要 Authenticode 签名且装在 `Program Files` 或 `System32`）；非服务时还要求 UAC 开着。
  - 本地策略“启用或禁用软件安全注意序列”（Windows 登录选项）必须允许服务/应用模拟 SAS，**这是个需要写注册表的系统设置**：`HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System\SoftwareSASGeneration`（路径 **[未核实]**，请对照组策略文档）。安装器要在用户同意后设置，并在界面里说明。
  - 服务可以模拟另一个进程的令牌来对该令牌所在会话发 SAS。
  - 所以架构上：**由服务（而不是代理）调用 SendSAS**，代理通过 IPC 请求。
- 会话切换（快速用户切换）：被切走的用户会话变成非活动会话，其桌面不再合成。我们只跟随“活动控制台会话”，并在远端界面上提示“当前显示的是用户 X 的会话/登录界面”。

### 1.5 RDP 会话与断开之后

- 用户通过 RDP 登录时，会话是 Session 2 等，控制台（Session 1 或登录界面）被锁定。**[未核实]**
- RDP 断开（不是注销）时，会话保持运行但没有显示输出，此时 DXGI 复制通常失败或黑屏；常见现象是黑屏、`AcquireNextFrame` 报错。**[未核实]**
- 常见处理：用 `tscon <id> /dest:console` 把会话拉回控制台（需要 SYSTEM，会把会话锁屏）。这是 TeamViewer/AnyDesk 等文档里常见的建议。**[未核实]**
- 方案：服务检测到“要控制的会话是断开的 RDP 会话”时，在界面上告知原因，并提供“把该会话切回控制台”这个明确动作，而不是黑屏。这也符合“失败要给原因和解决办法”的要求。

### 1.6 虚拟显示器（没接显示器的主机）

- Windows 10/11 没有显示器时，DXGI 复制通常没有输出（没有活动的显示适配器输出）。**[未核实]**
- 微软的官方答案是 [Indirect Display Driver (IddCx)](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)（已读）：
  - 用户态 UMDF 驱动，没有内核组件，跑在 Session 0，驱动崩溃不拖垮系统；
  - 官方明确列出“远程显示”和“虚拟显示器”为典型场景；
  - 驱动拿到的是 DirectX 表面，可以自己处理画面；
  - 微软给了 [示例驱动](https://github.com/Microsoft/Windows-driver-samples/tree/main/video/IndirectDisplay)（MIT 授权 **[未核实]**，微软 Windows-driver-samples 仓库整体是 MIT，但请再确认）。
- 现成的开源实现：
  - [Virtual-Display-Driver (VirtualDrivers)](https://github.com/VirtualDrivers/Virtual-Display-Driver)（已读）：**MIT**；Windows 10/11；自定义分辨率和刷新率、SDR/HDR、硬件光标、自定义 EDID；用 `vdd_settings.xml` 配置；发布版通过 SignPath Foundation 免费代码签名；`winget` 可装；ARM64 在 Win11 24H2 上可能要测试签名。
  - [parsec-vdd (nomi-san)](https://github.com/nomi-san/parsec-vdd)（已读）：MIT；**驱动本体是 Parsec 的**，该仓库只是控制库。注意：驱动需要定期“ping”，约 1 秒不 ping，显示器就被拔掉；不支持 HDR；与 Parsec 隐私模式不兼容；官方的 Parsec VDD 驱动许可对第三方分发的限制 **[未核实]**，**不要直接分发 Parsec 的驱动二进制**。
  - 微软示例驱动：许可友好，但要自己补全功能。
- 许可结论：Virtual-Display-Driver 是 MIT，可以改、可以集成，但要在 `THIRD_PARTY_NOTICES.md` 注明；Parsec 驱动不用。
- **签名要求（关键路径）**：
  - 64 位 Windows 10/11 加载内核驱动要微软签名；IddCx 是 UMDF 用户态驱动，但驱动包（INF + 目录文件）仍要有效的签名，否则安装时 Windows 会拦或要开测试模式。
  - 正式路线是：EV 代码签名证书 + 微软合作伙伴中心（Partner Center）账户，提交驱动包做**证明签名（attestation signing）**。本项目作为发布方，主体需要通过企业验证。**[未核实]**（流程细节、耗时、费用请核对 [Microsoft 文档](https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-attestation)）。
  - 现实中 Virtual-Display-Driver 用的是 SignPath Foundation 证书（开源项目免费），这是“用户态加载、被系统接受”的另一条路径，**不等于微软认证**，是否能在所有 Windows 配置下无警告安装 **[未核实]**。
  - 我们想自己出品牌安装包的话，要自己的证书。
- 兜底：HDMI/DP 假负载（dummy plug，价格几美元到十几美元）。文档里写出来，作为“驱动装不上或被安全软件拦”的备选。
- 另一个设计要点：虚拟显示器只在“需要时”创建（无物理显示器，或用户选择“隐私模式”），避免用户本机多出一块显示器（窗口被拖进去找不到）。

### 1.7 隐私模式（遮住物理屏幕）

- 需求：远程控制时，坐在机器前的人看不到内容（例如机房、家里电脑给别人远程）。
- 做法（通行）：创建虚拟显示器，把桌面扩展到虚拟显示器并关闭物理显示器的输出，或设为“仅虚拟显示器”。**[未核实]**（Parsec 的 Privacy Mode 据称就是这样，见 parsec-vdd 页的“与 Privacy Mode 不兼容”说明）
- 同时屏蔽本地输入（`BlockInput` 需要管理员，且 Ctrl+Alt+Del 不能被屏蔽）。**[未核实]**

## 2. macOS

### 2.1 LaunchDaemon 和 LaunchAgent

- LaunchDaemon：开机即起，root，属于系统域，没有 GUI 会话（不在任何用户的 Aqua 会话里）。
- LaunchAgent：在某个用户的会话里起，可以访问窗口服务器；登录窗口（loginwindow）本身也有一个会话，叫 LoginWindow 会话，可以通过 `LimitLoadToSessionType = LoginWindow` 在登录窗口会话里加载代理。**[未核实]**（来自 launchd.plist 手册，没读原文；建议核对 `man launchd.plist`）
- 因此架构和 Windows 同构：LaunchDaemon（root，常驻）负责联网、鉴权、状态；LaunchAgent 分两类：
  - `Aqua` 会话类型：用户登录后的采集和输入；
  - `LoginWindow` 会话类型：登录窗口时的采集和输入。
  TeamViewer 等产品在 macOS 上也是 daemon + 多个 agent 的结构。**[未核实]**（凭印象）

### 2.2 采集和 TCC 权限

- 屏幕录制（Screen Recording）和辅助功能（Accessibility）受 TCC 管控，用户必须在“系统设置 → 隐私与安全性”里手动授权，**没有公开 API 能静默授权**。MDM 管理的设备可以通过 PPPC 配置文件预授权 Accessibility，但 **Screen Recording 不能由 MDM 静默授权（只能允许标准用户自己开启，或预先拒绝）**。**[未核实]**（来自 Apple 的 PPPC 文档，没读原文）
- 授权是按“可执行文件/代码签名身份”记录的，所以：
  - 要有稳定的 **Developer ID 签名**，否则每次更新后授权会丢；
  - daemon 的二进制路径和签名要稳定。
- 授权需要在有 GUI 的会话里点；首次部署要有人在场（或走 MDM 的 Accessibility 预授权，但屏幕录制仍要点）。因此**“全新机器第一次完全无人值守”做不到**，产品设计要有“首次配置向导 + 检查清单 + 明确报错”。
- 采集 API：
  - ScreenCaptureKit（macOS 12.3+，现代，性能好）。
  - `CGDisplayStream`（较老，仍可用）。
  - `CGWindowListCreateImage`（已在 macOS 15 起被弃用/移除 **[未核实]**，不要用）。
- **登录窗口能不能采**：屏幕录制 API 作用于“调用者所在的会话”。要采登录窗口，进程必须在 loginwindow 会话里（见上）。Apple 自带的“屏幕共享/远程管理”可以在登录窗口控制，说明系统层是可行的；第三方在登录窗口里的采集是否受 TCC 同样限制 **[未核实]**，需要实机验证，**放进第一轮实机验证清单**。

### 2.3 输入注入

- `CGEventPost` 需要 Accessibility 授权。
- 登录窗口输入：进程要在 LoginWindow 会话里，且被授权。**[未核实]**
- 解锁屏幕（锁屏/屏保密码）：能不能通过注入键盘输入密码解锁，取决于会话；从 macOS 的设计看，锁屏界面是 loginwindow 的一部分，可以接受键盘事件 **[未核实]**。

### 2.4 FileVault（预启动）：做不到

- FileVault 开启且重启后，在磁盘解锁前系统没起来，没有网络（据报告解锁界面没有 Wi-Fi/以太网菜单），任何第三方软件都无从运行。来源：二手讨论，见 [Kandji 登录界面说明](https://support.kandji.io/kb/user-experience-with-macos-login-screens) 和 [Apple 社区帖](https://discussions.apple.com/thread/253682124)（搜索摘要，**[未核实]** 原文细节）。
- 变通：
  - 管理员远程重启前执行 `sudo fdesetup authrestart`，下一次启动自动解锁一次（需要先有会话和密码；安全上有争议）。
  - 据报道 macOS 26（Tahoe）起可通过 SSH 远程解锁 FileVault（[Jeff Geerling 的文章](https://www.jeffgeerling.com/blog/2025/you-can-finally-manage-macs-filevault-remotely-tahoe/)，搜索摘要，**[未核实]**）。
- 产品对策：检测 FileVault 状态，在设备页和连接失败时明确说“该设备开启了 FileVault，重启后需要有人本地输入密码才能被远程连接”，并提供“重启前先做 authrestart”的可选功能（做之前要用户明确同意）。**不要假装能做**。

### 2.5 虚拟显示器与无显示器的 Mac

- 无显示器的 Mac mini：不接显示器时 GPU 通常不会创建桌面，远程看到的是低分辨率、很卡，甚至没有输出。**[未核实]**（社区常见现象）
- CGVirtualDisplay：私有 API（在 CoreGraphics 里，没有公开头文件），被 BetterDisplay、DeskPad 等使用。**[未核实]**（凭记忆，建议读 DeskPad 源码确认接口；这些项目的许可各异，不要抄代码，只参考接口形式）。
  - 风险：私有 API 可能在系统更新时变化；上架 App Store 会被拒（我们走 Developer ID + 公证直发，不上架，可以接受）。
- 兜底：HDMI 假负载，对 Mac mini 同样有效。
- 官方方案：macOS 自带屏幕共享在无显示器时的行为 **[未核实]**，需要实机验证。

### 2.6 同行做法

- TeamViewer、AnyDesk、Splashtop、Jump Desktop 在 macOS 上都要求用户在首次安装时授权 Screen Recording 和 Accessibility，并安装一个 LaunchDaemon 和相关 Agent。**[未核实]**（凭各家安装文档的印象，没读原文）
- FileVault 重启后都不能连。**[未核实]**

### 2.7 签名和公证

- 必须：Developer ID Application 证书签名所有二进制和 `.app`，开启 Hardened Runtime，提交 notarization，stapling 到 pkg/dmg。
- 安装 LaunchDaemon 需要管理员权限（用 SMAppService 或 pkg 安装器）。macOS 13+ 的“登录项与扩展”里会显示后台项，用户可关闭，**关闭后服务不会起，要检测并提示**。**[未核实]**
- 本次没有 Apple 账户信息，费用（每年 99 美元）是通行价 **[未核实]**。

## 3. Linux

### 3.1 X11

- 登录界面（GDM/LightDM/SDDM greeter）：X11 greeter 本身是一个 X 会话（display `:0` 或 `:1`，属于 `gdm`/`lightdm` 用户）。服务以 root 运行，可以拿 greeter 的 `XAUTHORITY` 去连，从而采集和注入（x11 的 XTEST、XShm、XDamage 都能用）。**[未核实]**（经验之谈）
- 登录后换成用户的 X 会话；服务需要监听 logind（`org.freedesktop.login1` 的会话信号）或 loginctl 来找到当前活动会话和它的 DISPLAY/XAUTHORITY，并拉起或迁移代理。
- 多会话：快速用户切换在 Linux 上是多个 VT/多个 display；跟随 logind 的“活动会话”（`loginctl show-seat seat0` 的 ActiveSession）。

### 3.2 Wayland（GNOME、KDE）

- Wayland 不允许普通进程抓屏和注入输入，只能经合成器给出的接口：
  - xdg-desktop-portal 的 ScreenCast + RemoteDesktop（PipeWire 出画面，经 portal 注入输入）。
  - 合成器自己的接口（wlroots 的 `wlr-screencopy`/`virtual-pointer`、KDE 的 KWin 私有协议、GNOME 的 Mutter 私有 D-Bus）。
- 无人值守的关键：[RemoteDesktop portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)（已读）：
  - `SelectDevices` 的 `persist_mode`：0 不持久；1 应用运行期间持久；2 **直到用户显式撤销**；
  - 持久时 `Start()` 响应里返回 `restore_token`；下次 `SelectDevices` 传入即可免弹窗；
  - **token 只能用一次**，用完要换成响应里的新 token，并存好；
  - 失败（显示器或窗口变了、权限撤销）时 token 被忽略，回到弹窗；这时没人在场就连不上，必须给出“远端需要有人点一次允许”的明确提示；
  - 该特性是 portal 接口第 2 版加入的；
  - RemoteDesktop 的持久化只能通过该 portal，不能用 ScreenCast 的持久化选项。
- 限制：portal 在用户会话内运行，**登录界面（greeter）下没有用户会话，用不了**；用户注销后也不行。也就是说 portal 方案只覆盖“用户已登录（含锁屏？）”的场景，锁屏下能否继续 **[未核实]**（GNOME 锁屏时 Mutter 仍可共享，但画面是锁屏，要实测）。
- Sunshine 的 Linux 做法是（[排障文档](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2troubleshooting.html)，已读）：KMS 抓屏需要特权（Flatpak/AppImage 没有）；portal 抓屏需要在主机上批准一次远程桌面提示，保存的 token 可以在 Web UI 里重置。也就是说 Sunshine 也是“首次要人点一下”。
- GNOME Remote Desktop（[gnome-remote-desktop](https://gitlab.gnome.org/GNOME/gnome-remote-desktop/-/blob/main/README.md)，页面只显示它覆盖无头、系统守护进程、登录界面场景，细节未读）：
  - 它提供**系统级**（登录界面可连、无用户登录也可连）和无头模式，用 `grdctl` 配置，协议是 RDP（也有 VNC）。**[未核实]** 具体 GNOME 版本和选项，查 [GNOME 文档](https://gitlab.gnome.org/GNOME/gnome-remote-desktop)。
  - 对 PeerDesk 的意义：在 GNOME 上，系统级 RDP 可能是“登录界面也能连”的唯一合规路径；PeerDesk 作为 RDP 客户端接入它（见 D-rdp.md）比自己造更省。**[未核实]**
- Chrome Remote Desktop 的 Linux 做法（[Chromium 源码 linux_me2me_host.py](https://chromium.googlesource.com/chromium/src.git/+/57504b7decf3eeae3a508ad83c28ddb7ffdc68a8/remoting/host/linux/linux_me2me_host.py) 和 [systemd 单元](https://chromium.googlesource.com/chromium/src.git/+/main/remoting/host/installer/linux/chrome-remote-desktop%40.service)，据搜索摘要）：
  - **不抓真实桌面，而是起一个独立的虚拟 X 会话**（Xorg dummy 驱动，或 Xvfb），在里面跑桌面会话（由 `~/.chrome-remote-desktop-session` 或 `/etc/chrome-remote-desktop-session` 指定）；
  - 以 systemd 模板单元按用户运行；
  - 与 GDM 显示管理器会冲突（Google 云文档建议关掉 GDM）；
  - 这个方式最稳，但看到的不是用户本机屏幕。

### 3.3 systemd 服务设计

- 系统服务 `peerdesk-server`？注意：CLAUDE.md 规定只有两个二进制：客户端 `peerdesk` 和服务端 `peerdesk-server`。这里的“被控端服务”应该是 **`peerdesk` 客户端二进制的一个子命令**（例如 `peerdesk service` 和 `peerdesk agent`），而不是新二进制。`peerdesk-server` 是信令/中继，不跑在被控端。
- 单元：
  - `peerdesk.service`（system，root 或专用用户）：常驻，监听 logind；
  - `peerdesk-agent@.service`（user 或 system 模板）：按会话拉起，`User=`、`PAMName=`、`Environment=DISPLAY/WAYLAND_DISPLAY/XDG_RUNTIME_DIR`；
  - `loginctl enable-linger` 让用户服务在没人登录时也起。
- 无头主机：见 3.4。

### 3.4 无头和虚拟显示器

- X11：`Xvfb`（纯软件、无 GPU）；`Xdummy`（用 Xorg + dummy 驱动，可有 GLX 但无硬件加速）；NVIDIA/AMD/Intel 的 Xorg 配置里可以强制“假显示器”（`ConnectedMonitor`、自定义 EDID）。**[未核实]**
- Wayland：Mutter 有 headless 后端和虚拟显示器（用于 gnome-remote-desktop 的无头模式）；Weston 有 headless 后端；wlroots 有 headless 后端；**[未核实]**，都要有 GPU 的话仍需要 DRM 渲染节点。
- VKMS（虚拟 KMS 内核模块）：可以造一个没有硬件的 DRM 设备，用于 CI 和测试，不适合生产（无硬件编码）。**[未核实]**
- 带 GPU 的无头机器（云 GPU 主机最常见）要硬件编码时，需要显示输出才能让 NVENC/VAAPI 接上屏幕画面（或用 EDID 假显示）；Sunshine 的社区文档有大量此类说明，本次未读 **[未核实]**。

## 4. 同行对照

| 产品 | Windows 登录界面/UAC | macOS | Linux | 虚拟显示器 | 备注 |
| --- | --- | --- | --- | --- | --- |
| Parsec | 主机服务模式支持（Windows）**[未核实]** | 主机支持，要授权 | 主机仅有限支持 **[未核实]** | 自家 VDD（付费版 Teams/Warp 可用，见 [parsec-vdd 页](https://github.com/nomi-san/parsec-vdd)） | 隐私模式与 VDD 不兼容 |
| RustDesk（AGPL，只读文档） | 有服务模式，支持登录界面和 UAC **[未核实]** | daemon + agent | X11 为主，Wayland 靠 portal | 有虚拟显示器和隐私模式 **[未核实]** | 文档站 [rustdesk.com/docs](https://rustdesk.com/docs/en/) |
| AnyDesk | 服务 + 无人值守密码，支持登录界面和 UAC **[未核实]** | 需授权 | 有限 | 无内置（需假负载） **[未核实]** | |
| TeamViewer | 同上，且有“黑屏”功能 **[未核实]** | 同上 | 有限 | 无内置 | |
| Chrome Remote Desktop | 支持 | 支持 | **独立虚拟会话**（见 3.2） | 虚拟 X 会话 | 要用 Google 账户 |
| Sunshine/Moonlight | 服务模式；锁屏/UAC 情况见 Sunshine 文档 **[未核实]** | 不适用 | KMS/portal | 需第三方 VDD | 虚拟输入驱动要付费许可 |

说明：上表除有链接的外，其它单元格 **[未核实]**，是凭产品常识填的，写代码前应对照各家的官方文档再确认一轮。对标不要写死在这里，实机买一台测试。

## 5. 安全模型

### 5.1 两种授权方式

| | 设备级固定密码（TeamViewer/AnyDesk/RustDesk 传统方式） | 账户绑定的受信设备 |
| --- | --- | --- |
| 方式 | 被控端设一个永久密码，知道 ID + 密码就能连 | 设备在被控端用户的账户下登记；只有同账户（或被授权的账户/团队）下的控制端能连 |
| 优点 | 简单，朋友间临时用方便 | 没有密码可被撞库、泄露；可吊销；可审计；便于团队和计费 |
| 缺点 | 被暴力破解和钓鱼；强度不可控；已有大量遭滥用的先例 | 需要注册账户；离线时的首次绑定要有流程 |
| 对本项目 | 做，但**默认关闭**，且要强制强度和锁定策略 | **默认和主推**，托管云的计费也是挂在账户上（CLAUDE.md：设备指纹可重新生成，不适合计费） |

建议：
- 受信设备方案：控制端设备持有账户签发的证书/密钥，被控端用账户公钥验证；握手时双向认证（我们已有的 QUIC/TLS 层），不经过密码。
- 固定密码方案：必须是服务端或本地限速（例如 5 次失败锁 15 分钟）、最短长度和强度检查，且不得用 4 位 PIN。**[未核实]** 具体阈值需要和产品负责人讨论。
- 一次性临时码（用于“有人在场”的协助）：独立于无人值守，用完即失效。

### 5.2 双因素和访问控制

- 对无人值守连接，被控端可以要求：控制端账户已启用 2FA（TOTP/Passkey）。**[未核实]** 具体实现不在本次范围。
- 对新设备首次连接：邮件/推送通知被控端账户拥有者，并可选“需要批准”。
- 设备级策略：允许的时间段、允许的控制端账户、是否允许文件传输/剪贴板/ Ctrl+Alt+Del。

### 5.3 隐私模式和物理屏幕

- 遮挡被控端物理屏幕（第 1.7 节），并（可选）锁定本地键鼠；这是敏感功能：用户在机器前时必须看到“正在被远程控制”的明显提示，不允许完全隐藏（避免被当成间谍软件，也有利于过应用商店和杀毒软件审查）。**[未核实]** 杀毒软件的实际行为，需要实测。
- 无人在场时，连接结束要恢复物理屏幕和本地输入，并有看门狗：服务崩溃或网络断开时自动恢复（避免把用户锁在黑屏外）。

### 5.4 审计

- 记录：谁（账户和设备）、什么时候、从哪个 IP、连了多久、做了什么类别的操作（输入、剪贴板、文件、Ctrl+Alt+Del）、断开原因。
- 本地也记一份（被控端用户可以看），云端按账户保存，保留期可配置。
- 不记录屏幕内容。（和 C-ai.md 的“不收集用户数据”一致。）

### 5.5 本地服务面的安全

- 服务和代理之间的 IPC 要防“本地提权”：命名管道（Windows）要设 ACL，仅允许 SYSTEM 和同一用户；Unix 套接字要检查对端 UID（`SO_PEERCRED`/`LOCAL_PEERCRED`）。
- SYSTEM 服务拉起的代理路径要放在受保护的目录（`Program Files`），避免被普通用户替换成恶意程序（SendSAS 的 uiAccess 也有同样的要求，见 1.4）。
- 不要让服务因为 IPC 请求执行任意命令；IPC 只接受固定的、有限的消息集合。

## 6. 推荐设计（PeerDesk）

### 6.1 总体结构（三个平台同构）

```
peerdesk 客户端二进制（子命令）：
  peerdesk service   常驻服务，最高权限
  peerdesk agent     每会话代理，采集 + 输入 + 编码
  peerdesk（默认）   普通界面客户端

service：  联网（信令、打洞、QUIC）、鉴权、策略、审计、拉起/重启 agent、
           需要特权的动作（SendSAS、切会话、改显示器）
agent：    采集 → 编码 → （经 service 或直接）传输；输入注入；
IPC：      本机的进程间通信，两边认证；视频帧不走 service 的话，agent 直接和对端建 QUIC，
           service 只负责授权和调度（减少一次内存拷贝）
```

说明：视频是否由 agent 直接发出取决于打洞和连接归属。**第一版让 service 持有连接、agent 通过共享内存/本地管道送已编码帧**，实现简单；后续再评估让 agent 持有连接。**[未核实]**（这是设计权衡，需要实测延迟）

### 6.2 Windows

| 项目 | 方案 |
| --- | --- |
| 服务 | Windows Service，LocalSystem，开机自启，崩溃自动重启 |
| 代理 | service 监听会话事件，用 winlogon 令牌在活动控制台会话里拉起 agent，桌面 `winsta0\default` 或 `winsta0\winlogon` |
| 采集 | agent 内 DXGI Desktop Duplication；每次切桌面重建；失败时明确上报原因（例如 RDP 已断开、无显示器） |
| 输入 | agent 内 SendInput，跟随输入桌面 |
| Ctrl+Alt+Del | service 调 SendSAS；安装器经用户同意设置 SAS 策略 |
| 无显示器 | 集成 Virtual-Display-Driver（MIT，加声明），按需创建；兜底假负载 |
| 隐私模式 | 虚拟显示器 + 本地输入锁定 + 可见提示 |
| 签名 | 安装包和所有 exe：EV 代码签名；驱动：合作伙伴中心证明签名 |

### 6.3 macOS

| 项目 | 方案 |
| --- | --- |
| 服务 | LaunchDaemon（root），pkg 安装 |
| 代理 | LaunchAgent（Aqua）+ LaunchAgent（LoginWindow）；后者待实机验证可行性 |
| 采集 | ScreenCaptureKit；登录窗口会话里用同一代码路径 |
| 输入 | CGEventPost |
| 权限 | 首次配置向导引导 Screen Recording + Accessibility；MDM 给企业客户出 PPPC 配置文件说明（Accessibility 可预授权，Screen Recording 不可 **[未核实]**） |
| FileVault | 检测并明示限制；可选 authrestart（需用户明确开启） |
| 无显示器 | CGVirtualDisplay（私有 API，包一层，失败时给提示）；兜底假负载 |
| 签名 | Developer ID + Hardened Runtime + 公证，pkg 一起公证 |

### 6.4 Linux

| 项目 | 方案 |
| --- | --- |
| 服务 | systemd 系统单元，监听 logind |
| X11 | 在 greeter 和用户会话里分别拉起 agent；XShm/XDamage 采集，XTEST 注入 |
| Wayland | portal ScreenCast + RemoteDesktop，`persist_mode=2` 保存 token（单次使用，每次用完更新），首次需要有人点允许；token 失效时给出明确提示 |
| 登录界面（GNOME） | 评估对接 GNOME Remote Desktop 系统级 RDP，而不是自己造（见 D-rdp.md） |
| 无头 | 提供“虚拟会话模式”（类似 Chrome Remote Desktop：Xvfb/Xdummy 起独立桌面会话），文档写明与真实桌面不同；有 GPU 时用 EDID 假显示 |
| 打包 | deb/rpm + 脚本安装，无需公证；是否签名包仓库待定 |

## 7. 实施步骤和工作量估计

估计的单位是“一个熟悉 Rust 和该平台的工程师的人·周”，包含端到端测试接入，不含签名审核等外部等待。数字是拍脑袋估计，**[未核实]**，做第 1 步后要回头校准。

| 步骤 | 内容 | 估计 |
| --- | --- | --- |
| 0 | 启动外部长周期任务：EV 证书、Partner Center 账户、Apple Developer 账户 | 0.5 周的人工，外部等待数周 |
| 1 | Windows service + agent 骨架：会话事件、winlogon 令牌拉起、IPC（带 ACL），e2e 里的测试桩 | 2–3 周 |
| 2 | Windows 桌面跟随：DXGI + SetThreadDesktop，登录界面/锁屏/UAC 采集和输入，SendSAS | 2–3 周 |
| 3 | Windows 的 RDP 断开和会话切换处理、给出明确的错误和“拉回控制台”动作 | 1–2 周 |
| 4 | Windows 虚拟显示器：集成 Virtual-Display-Driver，按需创建/销毁，保活，隐私模式 | 2–3 周（含驱动签名外部等待） |
| 5 | 受信设备（账户绑定）授权 + 审计 + 无人值守设置界面 | 3–4 周（与托管云/账户系统一起排） |
| 6 | macOS daemon + Aqua agent，权限向导，ScreenCaptureKit 接入 | 3 周 |
| 7 | macOS 登录窗口 agent 的可行性验证，FileVault 检测和提示 | 1–2 周（验证失败则只出提示） |
| 8 | macOS CGVirtualDisplay 封装 | 1–2 周 |
| 9 | Linux X11 service + agent（含 greeter），logind 监听 | 2–3 周 |
| 10 | Linux Wayland：portal + restore token；GNOME Remote Desktop 评估 | 3–4 周 |
| 11 | Linux 虚拟会话（Xvfb/Xdummy） | 1–2 周 |
| 12 | 固定密码（可选）+ 限速锁定 + 2FA 要求 | 1–2 周 |

顺序理由：先做 Windows（用户基数最大、与付费意愿最相关，也是难度最高、最能拉开差距的）；第 0 步的外部等待要最早开始；macOS 的“登录窗口代理”是风险点，先小范围验证再投入。

## 8. 指标和测试（进 e2e 门槛，只升不降）

- **会话切换恢复时间**：登录界面 → 桌面、桌面 → 锁屏、UAC 弹出和关闭，从切换发生到首帧可见 ≤ 1 秒（与“冻结不超过 1 秒”硬门槛一致）。
- **桌面跟随正确率**：自动化脚本做 100 次切换，不丢帧、不黑屏。
- **无头采集**：无物理显示器时，采集帧率、分辨率和有物理显示器时一致。
- **输入到达**：在登录界面输入密码，成功登录；UAC 提示能点“是”；SendSAS 能弹出 Ctrl+Alt+Del 界面。
- 测试必须在**真实 Windows 虚拟机/实机**上跑（CI 有 `[win]` 标记），不能用合成画面顶替。
- 测试不能动开发者的键鼠（CLAUDE.md）：登录界面类测试只在专用虚拟机/CI Runner 里跑，本地默认跳过。
- 覆盖率 99% 门槛：采集和系统接口的胶水文件按 `scripts/coverage.sh` 规则排除，会话状态机、策略、IPC 协议、授权必须是纯逻辑，可测试。

## 9. 风险

| 风险 | 影响 | 对策 |
| --- | --- | --- |
| 驱动签名/证书审核慢或被拒 | 无头 Windows 主机无法交付 | 第 0 步最早启动；备选：SignPath 证书路径、假负载文档 |
| Windows 更新改变 DXGI/IddCx/会话行为（尤其 24H2、ARM64） | 采集黑屏 | 实机矩阵覆盖 Win10/11、x64/ARM64；错误要有明确原因 |
| macOS 屏幕录制无法静默授权 | 首次部署必须有人在场 | 向导 + 企业文档；不承诺“零接触” |
| macOS 登录窗口会话内的采集/注入受 TCC 或系统限制 | 登录前无法控制 | 第 7 步先验证，失败则文档写明，只支持登录后 |
| FileVault | 重启后无法连 | 检测、明示、可选 authrestart，不隐瞒 |
| CGVirtualDisplay 是私有 API | 系统升级后失效 | 封装 + 特性检测 + 降级到提示用假负载 |
| Wayland 的 restore token 失效 | 无人在场时连不上 | 明确报错、通知账户拥有者、文档说明 |
| 杀毒软件和 EDR 把“SYSTEM 服务 + 抓屏 + 注入”判为恶意软件 | 误杀、用户流失 | EV 签名、稳定的发布者名誉、白名单申请、可见提示，不做隐藏 |
| 无人值守被滥用（间谍软件、诈骗） | 法律和品牌风险 | 默认强制可见提示；账户绑定、审计、举报机制 **[未核实]** 法律要求按地区另查 |
| 许可：Parsec 驱动、RustDesk 代码 | AGPL/专有 | 只用 MIT 的 Virtual-Display-Driver，加声明；RustDesk 只读文档 |
| 本地提权（IPC、拉起路径） | 严重漏洞 | ACL、对端身份检查、受保护路径、模糊测试、第三方审计 |

## 10. 待确认的问题（给产品负责人）

1. 无人值守的第一版是否先只做 Windows？（建议是，见第 7 节顺序。）
2. 是否接受“首次使用需要有人在场点一次授权”（macOS 屏幕录制、Wayland portal）？（技术上无法避免。）
3. 固定密码是否做，默认关闭？
4. 隐私模式是否默认允许，还是需要被控端明确开启？
5. 驱动：是否现在就开始申请 EV 证书和 Partner Center 账户？

## 附：本次读过的来源

- Microsoft Learn: [IDD 概览](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview)、[SendSAS](https://learn.microsoft.com/en-us/windows/win32/api/sas/nf-sas-sendsas)
- GitHub: [Virtual-Display-Driver（MIT）](https://github.com/VirtualDrivers/Virtual-Display-Driver)、[parsec-vdd（MIT，驱动来自 Parsec）](https://github.com/nomi-san/parsec-vdd)、[RustDesk LICENCE（AGPL-3.0）](https://github.com/rustdesk/rustdesk/blob/master/LICENCE)
- [xdg-desktop-portal RemoteDesktop](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html)
- [Sunshine 排障](https://docs.lizardbyte.dev/projects/sunshine/latest/md_docs_2troubleshooting.html)
- 仅看到搜索摘要、未读原文：Chromium 的 Linux 远程桌面主机脚本和 systemd 单元，FileVault 相关社区讨论和文章（链接见正文）。
- 未能取到（404/403/拦截）：`WTSGetActiveConsoleSessionId` 文档页、RustDesk 手册页、Parsec 支持页、Arch Wiki（Anubis 拦截）。这些内容相关结论都已标 **[未核实]**。

## 7. 关闭退到托盘和进程守护（2026-11-27）

产品要求：关闭窗口不退出，退到托盘（菜单栏）；只有用户从托盘点“退出”才真退出；被杀、崩溃后自动恢复。
这是 GUI 进程的常驻，和上面的 Windows SYSTEM 服务（锁屏、登录界面）是两件事：服务没有桌面，出不了托盘图标。

### 7.1 退出码约定（守护靠它区分“用户要退”和“出事了”）

| 退出码 | 含义 | 守护 |
|---|---|---|
| 0 | 用户从托盘退出；第二次启动发现已有实例 | 不重启 |
| 78（`EXIT_NO_RETRY`） | 缺系统库等重试也没用的错误 | 不重启，原因在日志和标准错误里 |
| 其他、被信号杀 | 崩溃、被杀、任务管理器结束 | 重启 |

### 7.2 各平台机制

| 系统 | 机制 | 备注 |
|---|---|---|
| macOS | `~/Library/LaunchAgents/org.peerdesk.client.plist`：`RunAtLoad`，`KeepAlive` 里 `SuccessfulExit=false`，`ThrottleInterval=10`，`LimitLoadToSessionType=Aqua` | 下次登录由 launchd 加载；不在运行时 `launchctl bootstrap`（会多起一个实例） |
| Linux | systemd 用户单元 `peerdesk.service`：`Restart=on-failure`，`RestartPreventExitStatus=78`，`WantedBy=graphical-session.target` | 没有 systemd 用户实例时改写 XDG 自启动项，运行 `peerdesk guard` |
| Windows | `HKCU\...\Run` 里的 `peerdesk.exe guard` | 守护模式是同一个二进制的子命令，不新增第三个二进制；不需要管理员 |

Windows 没用计划任务：它的“失败后重启”是否覆盖进程崩溃的退出码没有核实；守护模式的行为是确定的，也能测试。
代价：守护进程本身被杀就没人拉起了，要等下次登录。守护只做等待和重启，几乎不会出错。

### 7.3 托盘库

| 系统 | 库 | 许可证 | 系统库依赖 |
|---|---|---|---|
| Windows、macOS | `tray-icon` 0.24（Tauri 维护） | MIT OR Apache-2.0 | 无 |
| Linux | `ksni` 0.3（StatusNotifierItem，纯 Rust D-Bus） | Unlicense（公有领域） | 无（运行时要桌面有 StatusNotifierWatcher） |

没有用 `tray-icon` 的 Linux 实现：它需要 GTK3 和 libayatana-appindicator（LGPL 系统库），还要在另一个线程里跑 GTK 主循环，
和 winit 的事件循环打架。`ksni` 不依赖系统库，和 keyring 的纯 Rust D-Bus 路线一致。
代价：GNOME 默认没有托盘宿主，要装 AppIndicator 扩展（Ubuntu 默认带）；没有时建不了托盘，**此时关闭窗口就是退出**，
并在状态栏告诉用户原因，不静默降级成“藏起来找不到”。

### 7.4 同行做法（凭产品行为的印象，**[未核实]**，没读原文）

- Tailscale、RustDesk：关闭窗口只是隐藏，托盘菜单里退出才真退出；Windows 左键点托盘图标打开窗口。
- 我们的差异：第一次关闭发一条系统通知说“还在后台运行”（窗口马上隐藏，窗口里的提示没人看得到）。

### 7.5 待实机验证

- macOS：窗口隐藏后 eframe 是否还收到托盘点击的重绘；从 launchd 直接启动 `.app/Contents/MacOS/peerdesk` 时屏幕录制、辅助功能授权是否仍然生效；Dock 图标在窗口隐藏后仍在（要用 `NSApplicationActivationPolicyAccessory` 隐藏，未做）。
- Windows：窗口隐藏后重绘是否继续；`Run` 项加守护的开机行为；PowerShell 气泡通知。
- Linux：GNOME（带 AppIndicator 扩展）、KDE 上托盘显示和菜单；`graphical-session.target` 下环境变量（DISPLAY、WAYLAND_DISPLAY）是否已导入单元。
