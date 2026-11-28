# 业内调研：隐私屏、本地键鼠锁定、虚拟显示器

调研日期：2026-11-28。只做调研，没改代码。

## 说明

- **标注**：每个结论后面标来源强度。
  - 【原文】：我打开了官方文档、源码或仓库原文。
  - 【二手】：搜索摘要、第三方文章、论坛、厂商营销页。
  - 【未核实】：没查到，或只是我的推断。推断会写明“推断”。
  - 【我们的判断】：基于上面的事实做的设计判断，不是事实。
- **没有实机**：这个会话是 Linux 云容器，没有 Windows、macOS 真机，也没有显示器。所有“实际会不会被截进画面”的结论，除非原文写明，都要真机验证。第 7 节列了验证清单。
- **AGPL 声明**：RustDesk（`rustdesk/rustdesk`，AGPL-3.0，LICENCE 文件开头为 GNU AFFERO GENERAL PUBLIC LICENSE 【原文】）只读了思路，没有复制任何代码。文档里出现的函数名、API 名是为了说明“它调用了哪个系统接口”，系统接口本身不受它的许可证约束。我们自己写实现时不对照着抄。
- **网页里夹带的指令**：读过的网页里没有发现要求 AI 做事的文字。
- **读不到的页面**：Parsec 帮助中心、Splashtop 帮助中心、Jump Desktop 帮助中心三处对机器抓取返回 403，只能看到搜索摘要，一律按【二手】处理。AnyDesk 的页面 404。
- **数据时效**：Apple 开发者论坛里有 2026 年 3 月到 11 月的帖子，提到 macOS 26.x、27.x。这些是目前能查到的最新状态。

---

## 0. 结论先行

### 0.1 每个系统推荐的做法

| 系统 | 隐私屏（物理屏黑，采集不黑） | 锁本地键鼠 | 虚拟显示器 |
|---|---|---|---|
| Windows | 每个物理显示器一个置顶全黑窗口，用 `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` 排除出采集（Win10 2004 / build 19041 起）。老系统不支持，明确告知“不可用”。 | 低级键鼠钩子 + 给自己注入的事件打标记，本地事件丢掉、标记事件放行；并留一个本地退出组合键。 | IddCx 虚拟显示器驱动（UMDF）。需要微软签名，这是最大的外部依赖。HDMI 假负载写进文档做兜底。 |
| macOS | 首选：每块显示器一个最高层级黑色覆盖窗口，采集用 ScreenCaptureKit 的 `SCContentFilter(display:excludingWindows:)` 把覆盖窗口排除。次选（只在新硬件前）：把伽马表清零（RustDesk 的做法），但 2026 年有大范围失效，**不能单独依赖**。 | `CGEventTap`（默认选项，可丢事件），靠 `kCGEventSourceUserData` 打标记区分自己注入的事件，其余来自 HID 的事件丢掉。要辅助功能权限，建议同时检查输入监控。 | `CGVirtualDisplay`（私有接口，苹果明确说没有公开接口）。只能走官网分发（Developer ID + 公证），不能上 Mac App Store。兜底：HDMI 假负载。 |
| Linux X11 | 暂不做覆盖窗口方案（见 3.1 的原因）。可选：DPMS 关屏或关闭物理输出。**都要实机验证采集是否还有画面**。 | `xinput` 禁用物理（slave）设备，保留 XTEST 虚拟设备。 | Xvfb 或 Xdummy 起虚拟屏；有物理 GPU 时用 `xrandr --setmonitor` / `--newmode`。 |
| Linux Wayland | **做不到**（见下）。 | **做不到**（见下）。 | 无头合成器：GNOME 用 `gnome-shell --headless --virtual-monitor`，wlroots 系用 `WLR_BACKENDS=headless`，或 `swaymsg create_output`。 |

### 0.2 做不到的地方（明确写）

1. **Windows，UAC 安全桌面、登录界面、Ctrl+Alt+Del 界面上的隐私屏和键鼠锁：做不到（用户态方案）。** 原因：覆盖窗口和低级钩子都只在“创建它的那个桌面”上生效。钩子文档写明全局钩子作用于“和调用线程同一个桌面上的所有线程”【原文】；UAC 文档写明提示框显示在安全桌面，“只有 Windows 进程能访问安全桌面”【原文】。结果：本地用户能在物理屏上看到并操作 UAC 提示。需要另外在安全桌面里跑一份进程才可能补上，没找到任何同行的公开做法，按【未核实】处理。IddCx 方案（把物理屏从桌面拓扑里摘掉）理论上不受这个限制，但没有找到证据，【未核实】。
2. **Windows，`BlockInput`：不能作为主方案。** 用户按 Ctrl+Alt+Del 或系统弹出硬错误对话框时，系统会自动解除阻止【原文】；而且只有调用 `BlockInput` 的那个线程才能用 `SendInput` 影响按键状态，别的线程不行【原文】。
3. **macOS，没接显示器时的采集：没有公开接口。** 有第三方 PR 明确写“没有公开 API 能在 macOS 零显示器时给 ScreenCaptureKit 一个帧缓冲”【二手】。必须有虚拟显示器（私有接口）或假负载。
4. **macOS，用 `NSWindow.sharingType = .none` 隐藏窗口：不可靠，不能用。** 苹果 DTS 2025-07 的原话：“目前没有公开 API 能阻止屏幕采集”，原帖报告 macOS 15.4 起 ScreenCaptureKit 仍会采到 sharingNone 的窗口【原文】。我们的方案反过来：不隐藏覆盖窗口，而是采集时主动把它排除。
5. **macOS，伽马清零隐私屏在新硬件上无声失效：** 苹果 DTS 在 M5 Max / macOS 26.3.1 上复现，`CGSetDisplayTransferByTable` 返回成功、读回值也对，但“屏幕没有任何变化”，到 macOS 27.0.1 仍未修，没有修复时间【原文】。这违反“不静默降级”：本地屏幕没黑，我们却以为黑了。
6. **macOS，本地键盘输入在“安全输入”状态下拦不住：** 系统密码框等场景开启 Secure Event Input 后，键盘事件不再交给事件拦截进程【原文：苹果 TN2150】。TeamViewer 官方文档也写了“显示系统密码框时无法阻止键盘输入，这是 macOS 的限制”【原文】。
7. **Linux Wayland，隐私屏和锁本地键鼠：做不到（通用方案）。** 没有协议允许普通客户端覆盖整个输出并排除出别人的采集；也没有协议允许普通客户端屏蔽本地物理输入。细节见 3.2。唯一接近的 `ext-session-lock-v1` 会锁整个会话，不能边锁边让远程继续用。
8. **Windows 虚拟显示器：没有微软签名就装不上。** 这不是代码问题，是账号和证书问题。见 4.2。
9. **Mac App Store：不能上架。** 用了私有接口 `CGVirtualDisplay`，违反审核准则 2.5.1【原文】；还要装全局事件拦截和辅助功能权限，也和沙盒冲突【我们的判断】。我们本来也要官网分发（要装 daemon），影响不大。

---

## 1. Windows

### 1.1 隐私屏

| 做法 | 原理 | 权限 | 限制 | 是否被截进画面 | 来源 |
|---|---|---|---|---|---|
| **A. 覆盖窗口 + `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`（推荐）** | 每个物理显示器上放一个置顶全黑窗口。`WDA_EXCLUDEFROMCAPTURE` 的文档原文：“窗口只在显示器上显示，在别处完全不出现”。 | 不需要管理员。窗口必须属于调用进程的顶层窗口【原文】，所以要在用户会话里的进程创建。 | Windows 10 2004（build 19041）起才支持；更早的系统该值按 `WDA_MONITOR` 处理【原文】。`WDA_MONITOR` 在采集里显示为黑块，不能用。只在 DWM 合成桌面时有效【原文】。文档声明这不是 DRM，拍屏幕拦不住【原文】。UAC 安全桌面上没有这个窗口（见 0.2）。 | 不会（文档原文）。具体到 DXGI Desktop Duplication / Windows Graphics Capture 是否都遵守，文档没逐一写，**【未核实】，需真机测**。RustDesk 把它设为 Windows 默认方案【原文】，间接说明可行。 | [SetWindowDisplayAffinity 文档](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity) 【原文】；RustDesk `privacy_mode.rs`、`win_exclude_from_capture.rs`（版本门槛 10.0.19041）【原文】 |
| **B. 放大镜接口 `MagSetWindowFilterList` 排除** | 建放大镜覆盖窗口显示黑色，采集用放大镜接口抓图，抓图时用过滤列表排除覆盖窗口。 | 文档未写要 UIAccess 或管理员，**【未核实】**。要求 WDDM 显卡【原文】。 | 适用于 Win8 以前到 Win10 2004 之前的系统。RustDesk 只在方案 A 不支持时才用它【原文】。抓图要换成放大镜的采集路径，不是我们现有的 DXGI 路径，改动大。 | 排除列表里的窗口不会（文档原文：放大镜窗口自己被自动排除，另可排除其他窗口）。 | [MagSetWindowFilterList](https://learn.microsoft.com/en-us/windows/win32/api/magnification/nf-magnification-magsetwindowfilterlist) 【原文】；RustDesk `win_mag.rs`（`CapturerMag`、`exclude`）【原文】 |
| **C. 放大镜全屏黑色变换 `MagSetFullscreenColorEffect`** | 给全屏放大镜一个把颜色全变成黑的矩阵。文档原文：“把颜色变换矩阵应用到整个桌面”。 | Windows 8 起【原文】。其他权限文档没写，**【未核实】**。 | 关键问题：**采集会不会也被变黑**。文档没说，DXGI Desktop Duplication 取的是变换前还是变换后的画面，**【未核实】**。如果采集取的是合成后的画面，这个方案会连远程画面一起黑掉，不能用。同行里没有找到用它做隐私屏的公开做法。 | 【未核实】 | [MagSetFullscreenColorEffect](https://learn.microsoft.com/en-us/windows/win32/api/magnification/nf-magnification-magsetfullscreencoloreffect) 【原文】 |
| **D. IddCx 虚拟显示器切主屏并摘除物理屏（RustDesk 的“虚拟显示器”模式）** | 装虚拟显示器，设为主屏（`ChangeDisplaySettingsExW` + `CDS_SET_PRIMARY`），把物理显示器移到 (10000,10000) 并尺寸设为 0x0。 | 要装驱动（见 4.2）。改显示配置要用户会话权限。 | RustDesk 自己的注释和代码显示了一堆麻烦：Win11 24H2 必须先设虚拟屏为主屏，原因不明；会写注册表（`CDS_UPDATEREGISTRY`），要快照连接状态以便崩溃后恢复；DPI 缩放恢复不了；快速反复插拔 IDD 显示器会崩溃；有看门狗，发现物理屏重新亮起就关隐私模式。 | 不会，物理屏已不在桌面上。 | RustDesk `win_virtual_display.rs`【原文，经摘要读取，非逐行】 |
| **E. DWM/驱动层黑屏（关显示器电源、DDC 调亮度）** | 不是 Windows 公开的通用接口。 | — | 没查到可用做法。**【未核实】** | — | 未查到 |

【我们的判断】Windows 先做 A。理由：不要驱动、不改显示拓扑、崩溃时窗口随进程消失，本地屏自动恢复（比 D 安全）。D 作为有虚拟显示器驱动后的后续选项。B、C 不做。

### 1.2 锁本地键鼠

| 做法 | 原理 | 权限 | 限制 | 来源 |
|---|---|---|---|---|
| **A. 低级键鼠钩子（`WH_KEYBOARD_LL` / `WH_MOUSE_LL`）+ 标记放行（推荐）** | 钩子回调里：自己注入的事件放行，其余返回 1 吞掉。 | 不需要管理员。 | 全局钩子只作用于“同一个桌面”（见 0.2）。钩子线程必须一直泵消息，否则会拖慢系统【原文：文档对 32/64 位全局钩子的说明】。系统对钩子回调有超时限制，超时会被跳过，**文档本页没写具体数值，【未核实】**。本地用户必须有退出办法（见下）。 | [SetWindowsHookEx](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowshookexa) 【原文】；RustDesk `win_input.rs`【原文】 |
| 区分“我们注入的”和“别人注入的”：`LLKHF_INJECTED`（bit 4）表示事件是注入的，`LLKHF_LOWER_IL_INJECTED`（bit 1）表示来自更低完整性级别的进程【原文】。 | 文档只能告诉你“是注入的”，不能告诉你是谁注入的。 | — | RustDesk **没有**用 `LLKHF_INJECTED`，而是在注入时往 `dwExtraInfo` 写一个固定标记（`ENIGO_INPUT_EXTRA_VALUE`），钩子里只放行带标记的【原文】。【我们的判断】原因大概是：触摸键盘、其他自动化软件也会注入，用 `LLKHF_INJECTED` 会把它们一起放行，导致“锁了但没锁住”。标记可被本地恶意程序伪造，这个方案防的是误触和旁观者，不是本机恶意软件。 | KBDLLHOOKSTRUCT 文档【原文】；RustDesk 源码【原文】 |
| RustDesk 的细节（只记设计）：键盘钩子里对没标记的 Alt 一律吞掉避免 Alt+Tab；除 Ctrl 和 P 外按键一律吞；本地按 Ctrl+P 退出隐私模式；只拦按下，抬起放行。 | 给本地用户一个明确的逃生口，防止“被锁在自己电脑外”。 | — | 【我们的判断】我们也必须有本地逃生口，并且写进产品说明。TeamViewer 也是类似思路：在被控端按 Ctrl+Alt+Del 会撤掉黑屏、锁定会话并恢复输入（官方文档原文）。 | RustDesk `win_input.rs`【原文】；[TeamViewer 黑屏文档](https://www.teamviewer.com/en/global/support/knowledge-base/teamviewer-remote/security/teamviewer-black-screen/) 【原文】 |
| **B. `BlockInput(TRUE)`** | 系统级阻止物理键鼠输入。 | 文档没写权限要求。一个 AutoIt 论坛用户说需要管理员，官方没有确认，**【二手】**。 | 只有调用线程能用 `SendInput` 影响按键状态，别的线程不行【原文】。所以我们的注入必须和 `BlockInput` 在同一个线程，架构上很别扭。Ctrl+Alt+Del 或硬错误框时系统自动解除【原文】。在 UAC 安全桌面上是否有效，文档没写，**【未核实】**。 | [BlockInput](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-blockinput) 【原文】 |

【我们的判断】键鼠锁用 A，不用 B。

---

## 2. macOS

### 2.1 隐私屏

| 做法 | 原理 | 权限 | 限制 | 是否被截进画面 | 来源 |
|---|---|---|---|---|---|
| **A. 覆盖窗口 + `SCContentFilter(display:excludingWindows:)`（推荐，需实机验证）** | 每块显示器上建一个黑色无边框窗口，窗口层级用 `CGShieldingWindowLevel()`（文档：被抓取显示器的“遮罩窗口”层级）。ScreenCaptureKit 采集时，把这些覆盖窗口放进排除列表。 | 屏幕录制权限（我们本来就要）。覆盖窗口不需要额外权限。 | 排除接口是公开接口，macOS 12.3 起可用【原文】。我们的仓库已经用 ScreenCaptureKit 采集（`crates/platform/src/macos.rs`）【原文：仓库源码】，所以改动小。限制：覆盖窗口不是系统级的，盖不住登录窗口、锁屏、屏保，**【未核实】**；可能被全屏 App 的 Space 切换露出来，**【未核实】**。“排除是否 100% 生效、有没有一帧漏出”没有任何资料，只能真机测。**没有找到任何同行公开说自己这样做，【未核实】**。 | 理论上不会（排除列表）。**【未核实】** | [SCContentFilter](https://developer.apple.com/documentation/screencapturekit/sccontentfilter.md) 【原文】、[init(display:excludingWindows:)](https://developer.apple.com/documentation/screencapturekit/sccontentfilter/init(display:excludingwindows:).md) 【原文】、[Quartz Display Services 目录](https://developer.apple.com/documentation/coregraphics/quartz-display-services.md) 【原文】 |
| **B. 伽马表清零 `CGSetDisplayTransferByTable`（RustDesk 的做法）** | 保存每块显示器的伽马表（用显示器 UUID 作键），全部写 0，显示器就黑了；关闭时恢复，并调 `CGDisplayRestoreColorSyncSettings()` 兜底。注册显示器重新配置的回调，插拔显示器后 5 秒内重复应用，因为 “macOS 可能在显示器变化后的不确定时刻通过 ColorSync 恢复默认伽马”（源码注释）。要求“所有在线显示器都成功变黑”，否则整体失败并回滚。 | 伽马接口本身不要权限。采集仍要屏幕录制权限。 | **2026 年新硬件上无声失效**：见 0.2 第 5 点。更早的 macOS Tahoe 测试版里，开着“自动调节亮度”就被忽略（2025-07，beta 5 修复，2026-03 在 M5 Pro/Max、MacBook Neo 复发）。读回伽马表时返回的是设置值，**所以代码检测不出“没黑”**。无法设为“真黑”：只是让像素变 0，背光还亮。 | 伽马发生在显示管线末端，采集取的是帧缓冲，推断不会被采到。RustDesk 源码没有说明这一点。**推断，【未核实】**。 | RustDesk `src/platform/macos.mm`（`MacSetPrivacyMode`、`ApplyBlackoutToDisplay`、`RestoreAllGammas`、`CGDisplayRegisterReconfigurationCallback`）【原文，已直接下载文件核对】；[CGSetDisplayTransferByTable](https://developer.apple.com/documentation/coregraphics/cgsetdisplaytransferbytable(_:_:_:_:_:).md) 【原文】；[论坛 795074](https://developer.apple.com/forums/thread/795074) 【原文】；[论坛 819331](https://developer.apple.com/forums/thread/819331) 【原文】 |
| `NSWindow.sharingType = .none` 的覆盖窗口 | 想让窗口不被采集。 | — | **不要用**：苹果 DTS 说没有公开 API 能阻止采集，macOS 15.4+ 上 ScreenCaptureKit 仍采到 sharingNone 窗口。`sharingType` 官方文档只写了“其他进程对窗口内容的访问级别”，没承诺对 ScreenCaptureKit 生效。 | 会（实测帖）。 | [论坛 792152](https://developer.apple.com/forums/thread/792152) 【原文】；[sharingType 文档](https://developer.apple.com/documentation/appkit/nswindow/sharingtype-swift.property.md) 【原文】 |
| `CGDisplayCapture`（独占显示器） | 文档：“获得显示器独占使用权，阻止其他应用和系统服务使用它或改变配置”。 | — | 这是给全屏游戏用的，会让别的采集（包括我们自己的）拿不到正常内容，**不适合**。具体对 ScreenCaptureKit 的影响，**【未核实】**。 | 【未核实】 | [Quartz Display Services](https://developer.apple.com/documentation/coregraphics/quartz-display-services.md) 【原文】 |
| 调亮度到 0（DDC / 内置屏亮度接口） | — | — | 没有找到公开的通用接口；DDC 只对部分外接显示器有效。**【未核实】** | — | 未查到 |

### 2.2 锁本地键鼠

| 做法 | 原理 | 权限 | 限制 | 来源 |
|---|---|---|---|---|
| **`CGEventTapCreate`，`kCGHIDEventTap` + `kCGEventTapOptionDefault`（可丢事件）（推荐）** | 事件回调里：`kCGEventSourceUserData` 等于我们的标记就放行；否则如果 `kCGEventSourceStateID == kCGEventSourceStateHIDSystemState`（来自物理 HID）就返回 NULL 丢掉。 | 苹果文档：事件拦截要满足“进程以 root 运行”或“启用辅助设备访问”（即辅助功能权限）。把拦截点放在“HID 事件进入窗口服务器的位置”，文档写“只有 root 进程可以”，**其他用户返回 NULL**。RustDesk 还检查输入监控权限（`IOHIDCheckAccess`）。论坛对“默认拦截 vs 只监听需要哪个权限”说法有冲突，**【二手】**。 | 1) Secure Event Input 开启时（密码框）键盘事件不进拦截器，拦不住本地键盘【原文：TN2150；TeamViewer 同样说明】。2) 有论坛报告：事件拦截器还在时撤销辅助功能权限，Sequoia 之后可能导致系统输入卡死，**【二手】**。3) 拦截器可能被系统因超时或用户输入而禁用，必须在回调里重新启用（RustDesk 源码有处理）。4) 另一个应用开着 Secure Input 不关，会让所有应用都装不了键盘拦截器（1Password 社区帖），**【二手】**。 | [CGEvent.tapCreate](https://developer.apple.com/documentation/coregraphics/cgevent/tapcreate(tap:place:options:eventsofinterest:callback:userinfo:).md) 【原文】；[TN2150](https://developer.apple.com/library/mac/technotes/tn2150/_index.html) 【原文】；RustDesk `macos.mm`（`kCGEventSourceUserData` = 100 作标记，`kCGEventSourceStateHIDSystemState` 判定本地）【原文，已核对】 |

【我们的判断】标记值要用我们自己的、随机或随会话变化的值，别用固定的 100（RustDesk 用的是固定常量，本机恶意程序可以伪造）。同样的考虑也适用于 Windows 的 `dwExtraInfo`。

### 2.3 同行怎么做 / 为什么不支持（Mac 被控端）

| 产品 | 说法 | 强度 |
|---|---|---|
| RustDesk | Mac 上有隐私模式：伽马清零 + `CGEventTap`（见上），只有一种实现【原文，源码】。其隐私模式文件在 macOS 上不提供多种方式。 | 【原文】 |
| TeamViewer | 黑屏功能支持 macOS 被控端，要求 TeamViewer ≥ 15.8、“随系统启动”打开、给辅助功能权限；**同时禁用远程输入**（用户在被控端屏幕看到黑屏，远程方不能操作）；Mac 上显示系统密码框时无法阻止键盘输入。注意：TeamViewer 的黑屏**语义和我们不同**——它是“黑屏时远程不可操作”，用于保护屏幕内容，不是“一边远程操作一边遮住本地”。 | 【原文】 |
| Jump Desktop | Fluid 协议的 Privacy Mode：“把被控机物理显示器变黑，并阻止物理键鼠输入，会话结束后自动锁屏”。2022-11 上线，当时仅支持 Mac 客户端连 Windows 或 Mac 被控端。Mac 上有“外接显示器”的专门说明页（读不到正文）。具体用什么接口没有公开。 | 【二手】（更新日志页读到了，帮助页 403） |
| Splashtop | Mac 和 Windows 都有“空白屏幕”功能，在被控端 Streamer 的“安全”标签里启用；帮助页列出限制：Mac 上鼠标指针外观可能变、部分 macOS 10.14+ 机器可能只显示空白窗口或工具栏、登录前界面要先打开 Streamer 才生效。 | 【二手】（搜索摘要，帮助页 403） |
| ToDesk | 官网说明提到“隐私屏”，没有写 Mac 被控端是否支持、系统版本要求。查不到。 | 【未核实】 |
| AnyDesk | 有“屏幕隐私”，用来在会话中遮住被控端屏幕并锁本地输入；帮助页 404，没读到平台范围。 | 【二手】（搜索摘要） |

结论：没找到任何同行公开说明 Mac 上用的是哪种私有或公开接口，除了 RustDesk（伽马）。RustDesk 的方案在 2026 年的新硬件上已经出问题，是否已经有人在跟进修复，没查到。

---

## 3. Linux

### 3.1 X11

| 做法 | 原理 | 权限 | 限制 | 是否被截进画面 | 来源 |
|---|---|---|---|---|---|
| 覆盖窗口（override-redirect 全屏黑窗） | 在每个输出上盖一个黑窗。 | 普通用户。 | **X11 采集多半是对根窗口做 `XGetImage` 或 XShm，会把覆盖窗口截进去**；要避免得用 XComposite 对其他窗口离屏重定向并自己合成，复杂。**【未核实】**，没查到同行公开做法。RustDesk 在 Linux 上没有隐私模式（`get_supported_privacy_mode_impl()` 返回空）【原文】。 | 大概率会（推断，【未核实】） | RustDesk `privacy_mode.rs`【原文】 |
| DPMS 关屏 / `xrandr --output X --off` | 让物理显示器熄灭或移出输出。 | 普通用户。 | 采集在输出被关闭后还有没有画面，取决于驱动。关输出会让该输出从屏幕布局消失，桌面窗口会被挪走，**【未核实】**。鼠标一动会唤醒 DPMS。没有同行公开案例。 | 【未核实】 | [xrandr 手册](https://man.archlinux.org/man/xrandr.1.en) 只写了 `--off` “禁用输出”【原文】 |
| 禁用物理输入设备 `xinput disable <id>`（等价于设 "Device Enabled" 为 0） | 禁用物理键鼠，XTEST 虚拟设备（`xdotool`、XTest 注入）不受影响（推断）。 | 普通用户可对自己的 X 会话操作。 | 需要按设备 ID 找物理设备，热插拔后要重新做。手册只确认了 `enable`/`disable` 命令存在，没有提 XTEST 设备，所以“XTEST 不受影响”是推断，**【未核实】**，需实机验证。 | 不涉及采集 | [xinput 手册](https://man.archlinux.org/man/xinput.1.en) 【原文】 |

### 3.2 Wayland：基本做不到，原因

- **输入注入有标准通道，但没有“屏蔽本地输入”的通道。** `xdg-desktop-portal` 的 RemoteDesktop 接口提供指针、键盘、触摸注入和 `ConnectToEIS`（libei），文档里没有任何阻止或抢占本地物理输入的方法【原文】。
- **InputCapture 门户不是用来锁本地输入的。** 它用于把“跨过屏幕边缘的指针”转交给应用（KVM 场景），触发条件只有指针屏障，需要用户同意，合成器随时可以停止捕获【原文】。文档没说被捕获的输入是否对本地会话完全隐藏，**【未核实】**，不能依赖。
- **没有“排除某窗口出采集”的通用协议。** 本次没有查到相关的标准协议，**【未核实】**；Wayland 的安全模型是客户端互相看不到对方，覆盖整个输出的窗口需要特权协议，普通客户端拿不到。
- **`ext-session-lock-v1`** 能让合成器把所有输出盖成不透明色，普通客户端失去渲染和输入。但这是“锁屏”，只有特权客户端能用，客户端死掉会话不会自动解锁；协议文本没提采集如何处理【原文】。它会让远程也看不到桌面，不符合需求。
- 每个桌面环境各自的私有接口（GNOME Shell 扩展、KWin 脚本）不通用，且需要用户装扩展，**不建议做**【我们的判断】。

结论：Wayland 上对用户明确说“本机不支持隐私屏”，并给出原因和建议（切到 X11 会话，或用虚拟显示器 + 本机接口由用户自己关屏）。

---

## 4. 虚拟显示器

### 4.1 macOS

| 做法 | 原理 | 权限与签名 | 限制 | 许可证 | 来源 |
|---|---|---|---|---|---|
| **A. `CGVirtualDisplay`（推荐）** | 私有类：`CGVirtualDisplayDescriptor`、`CGVirtualDisplayMode`、`CGVirtualDisplaySettings`、`CGVirtualDisplay`。创建后系统里多一块可设分辨率、HiDPI 的显示器。 | 不需要特殊 entitlement（从开源项目的用法看），**但没有任何官方文档**。Developer ID 签名 + 公证即可官网分发（我们本来就要）。 | **是私有接口。** ① 苹果 DTS（2026-08，被标为已采纳回答）：“没有公开接口。有一个相当广为人知、多个应用依赖的 SPI，但我们从未提供公开 API。”② 苹果官方 Quartz Display Services 目录里没有它的任何条目【原文】。③ 头文件不是苹果发布的：开源项目用的是 Khaos Tian 2021 年写的 `CGVirtualDisplayPrivate.h`（注释 “Created by Khaos Tian on 2/17/21”，属性带 `@synthesize` 注释，是反推出来的声明）；Chromium 测试代码里也有自己声明的同名类。④ 随时可能被改名或删除（第三方 Go 封装的说法，**【二手】**）。⑤ 不能上 Mac App Store：审核准则 2.5.1“只能使用公开 API”【原文】；第三方 PR 提醒“审核可能仅凭类名就拒绝”【二手】。⑥ ScreenCaptureKit 在多个虚拟显示器反复插拔后，会把新的 `SCStream` 指向最近连上的虚拟屏（苹果论坛 786829，原帖用 BetterDisplay 复现，DTS 回复“值得提 bug”，没有修复）【原文】。我们只开一块虚拟屏，影响较小，但要避免反复销毁重建。 | 无开源代码依赖，只用反推出来的类声明。参考项目：DeskPad（MIT，LICENSE.md 开头为 MIT License【原文】）；Chromium（BSD 风格头注释）【原文】。BetterDisplay：**闭源商业软件**（Pro 授权 21.99 美元，README 只是产品页），不能参考代码【原文：README】。 | [苹果论坛 842949](https://developer.apple.com/forums/thread/842949) 【原文】；[Quartz Display Services](https://developer.apple.com/documentation/coregraphics/quartz-display-services.md) 【原文】；DeskPad `CGVirtualDisplayPrivate.h`【原文】；Chromium `virtual_display_util_mac.mm`【原文】；[论坛 786829](https://developer.apple.com/forums/thread/786829) 【原文】；[审核准则](https://developer.apple.com/app-store/review/guidelines/) 【原文】；[iMote PR #3](https://github.com/slvssb/iMote/pull/3) 【二手】；[go-macos/virtualdisplay](https://pkg.go.dev/github.com/go-macos/virtualdisplay) 【二手】 |
| B. DriverKit 虚拟显示驱动（公开路线） | 第三方 Go 封装页说“公开路线是 DriverKit 驱动扩展，需要苹果授予的 entitlement”。 | 要向苹果申请 entitlement。 | 申请周期和是否批给显示类用途，**【未核实】**。DriverKit 有没有现成的“虚拟显示”家族，我没查到官方资料。 | — | [go-macos/virtualdisplay](https://pkg.go.dev/github.com/go-macos/virtualdisplay) 【二手】 |
| **C. HDMI / DP 假负载（兜底）** | 插一个 EDID 模拟器，系统认为接了显示器。 | 无。 | 需要用户买硬件（便宜）；分辨率受假负载 EDID 限制，一个来源称假负载不能模拟比电脑支持更高的分辨率【二手】。有用户反馈假负载不能解决所有黑屏：锁屏会话仍然出黑画面，屏幕录制权限没给也黑【二手】。 | — | [Astropad 博客](https://astropad.com/blog/dummy-plug-headless-mac-mini/)（厂商营销页）、iMote PR【二手】 |

**没接显示器时 ScreenCaptureKit 给不给画面：**
- 没有任何苹果官方资料。【未核实】
- 第三方 PR：“没有公开 API 能在 macOS 零显示器时给 ScreenCaptureKit 一个帧缓冲”，所以用虚拟屏当工作绕过；这份 PR 自己写明“代码没有编译”，没有实测记录。【二手】
- Astropad（卖 Workbench 的厂商）说无显示器的 Mac mini 默认 1920x1080，非 Retina；同一页还说 Apple Silicon（M1 到 M4）会自动创建虚拟显示器，没有引用测试。这条说法和上一条矛盾，**以我们实机测为准**。【二手，营销页】
- 搜索里没有找到“CGVirtualDisplay 在无头 Mac 上配合 ScreenCaptureKit 正常出画面”的权威确认（Go 封装页只说显示器 ID “可用于 ScreenCaptureKit”，没有给采集测试结果）。【二手】

【我们的判断】macOS 无头的验证必须在真机上做：一台没接显示器的 Apple Silicon Mac mini，开机、登录后、锁屏后三种状态，分别看 `SCShareableContent` 能否列出显示器、能否出画面。

### 4.2 Windows

| 做法 | 原理 | 签名与分发 | 限制 | 许可证 | 来源 |
|---|---|---|---|---|---|
| **IddCx 间接显示驱动（推荐）** | UMDF 用户态驱动，用 IddCx 类扩展：创建图形适配器、上报显示器接入、提供显示模式、处理桌面图像。运行在 Session 0，驱动不稳定不会拖垮系统。官方列出的典型场景含“远程显示”和“虚拟显示器”。 | **驱动仍然需要微软签名。** ① 官方：Windows 10 1607 起，新的内核模式驱动必须由 Dev Portal 签名；开 Partner Center 硬件开发者账号需要 EV 代码签名证书。② “测试用”的 attestation 签名：只需 EV 证书，不用跑 HLK，**支持内核和用户态驱动**，只在 Windows 10 及以后有效；但**不能发布到 Windows Update 给零售用户**，且“没有微软认证（not Windows Certified）”。要上 Windows Update 或支持 Win7/8，必须走 HLK/WHCP 认证。③ 签名要求表：Win10 零售版接受 attestation 签名；Windows Server 2016+ 只接受通过 HLK 的。④ 文档本身把 attestation 签名归类在“用于测试场景”，能不能用它长期给终端用户分发，文档用的措辞是“仅用于测试”，**【我们的判断】存在合规风险，需要和微软确认或走 HLK**。 | 各 IddCx 版本对应的最低系统：Win10 1607/RS1 为 1.0，19H2 为 1.4，Win10 2004 起 1.5，Win11 21H2 为 1.8，Win11 23H2 为 1.10。要 HDR 需 1.10。 | 示例和现成实现的许可证见下。 | [IDD 概述](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview) 【原文】；[驱动签名选项](https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/driver-signing-offerings) 【原文】；[attestation 签名](https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-attestation) 【原文】；[签名策略](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-) 【原文】；[IddCx 版本](https://learn.microsoft.com/en-us/windows-hardware/drivers/display/iddcx-versions) 【原文】 |

**开源实现与许可证：**

| 项目 | 许可证 | 说明 | 来源 |
|---|---|---|---|
| Microsoft 示例 `Windows-driver-samples`（含 `video/IndirectDisplay`） | 仓库根 LICENSE 为 MS-PL（Microsoft Public License）。MS-PL 不是 GPL，可以用，但要保留版权和许可声明；`IndirectDisplay` 子目录有无单独许可文件，**【未核实】**。 | 示例只做“枚举一块显示器、响应模式设置、接收帧”，代码里到处是 TODO，需要大量补全。 | [仓库](https://github.com/Microsoft/Windows-driver-samples) 【原文：LICENSE 开头】 |
| `VirtualDrivers/Virtual-Display-Driver` | **MIT**（Copyright 2024 Virtual Display）【原文】。支持 IddCx 1.10、HDR、硬件光标、自定义 EDID、小数刷新率；README 表格标 Signed；ARM64 在 Win11 24H2 起可能要开测试签名；致谢写“Free code signing by SignPath.io，证书由 SignPath Foundation 提供”。**这是“代码签名”，是不是微软 attestation 签名，README 没写，【未核实】。** | 我们的 G-unattended 文档计划集成它（要自己签名）。 | [仓库 README、LICENSE](https://github.com/VirtualDrivers/Virtual-Display-Driver) 【原文】 |
| `ge9/IddSampleDriver` | 仓库根没有 LICENSE 文件（master 上 LICENSE、LICENSE.md 都 404）。README 写：“MIT 和 CC0 或公有领域（我做的改动），微软部分的许可请向微软确认，取限制最少的那个”。**许可证表述不清，不建议直接依赖，【原文】。** | 单显示器示例的改版，用 `C:\IddSampleDriver\option.txt` 配置。 | [README](https://github.com/ge9/IddSampleDriver) 【原文】 |
| `nomi-san/parsec-vdd` | 封装库 MIT（Copyright 2023 Nguyen Duy）【原文】。**驱动二进制是 Parsec 公司的，不是这个仓库的开源代码，许可见 Parsec 条款，【未核实】。** README 说 VDD “由 Parsec 构建，用 IddCx，最高 4K 240Hz，Windows 10+”。 | 对我们价值：一份“业内怎么用 IDD”的参考，不是可复制的驱动。 | [README](https://github.com/nomi-san/parsec-vdd) 【原文】 |
| RustDesk 的 IDD 驱动（`RustDeskIddDriver`，基于 IddSampleDriver 1.2，README 对比表里 Signed 一栏是空的）、Amyuni IDD | RustDesk 主仓库 AGPL；Amyuni 的 usbmmidd_v2 是商业驱动。 | 不使用。 | Virtual-Display-Driver README 对比表【原文】；RustDesk `win_virtual_display.rs`（`is_amyuni_idd`）【原文】 |

**Parsec 的虚拟显示器怎么分发：**
- Parsec 官方帮助中心（403，读不到正文）在搜索摘要里说：Windows 被控端需要装 Parsec Virtual Display Driver，在 Parsec 设置的 Host 页里安装；要求 Windows 10 1607+ 或 Windows Server 2019+；多块虚拟显示器仅限 Teams / Warp 付费客户，免费用户有“无显示器时退回到一块虚拟显示器”的选项。【二手】
- Parsec 帮助页还说：无显示器时 Windows 和 GPU 无法采集画面，建议接 HDMI 假负载，或开启 Parsec 虚拟鼠标（需要 vUSB 驱动）。【二手】
- 驱动 0.45 起可选择虚拟适配器挂在哪块物理 GPU 上，对虚拟机场景更稳。【二手】
- 驱动靠主程序安装向导装到用户机器，具体签名链、是否经 WHQL，**【未核实】**。

**没接显示器时 Windows 的行为：** Parsec 说采集失败（见上，【二手】）。`parsec-vdd` README 写无头主机和云 GPU 虚拟机用 VDD“给采集工具、启动器一个真实的桌面会话”【原文，但是第三方封装库的自述】。

### 4.3 Linux

| 做法 | 原理 | 权限 | 限制 | 许可证 | 来源 |
|---|---|---|---|---|---|
| **Xvfb** | 无显示硬件、无物理输入设备也能跑的 X 服务器；`-screen N WxHxD` 指定分辨率，默认单屏 1280x1024、8 位色深。 | 普通用户。 | 没有 GPU 加速，画面在内存里；只有软件渲染。桌面环境能不能在上面完整跑要自己配。 | X.Org，MIT 类许可证（不是 GPL）**【未核实】**，没有打开许可证页面。 | [Xvfb 手册](https://xorg.freedesktop.org/archive/X11R7.6/doc/man/man1/Xvfb.1.xhtml) 【原文】 |
| `xrandr --setmonitor` / `--newmode` / `--addmode` / `--fb` | 在已有 GPU 输出上定义虚拟监视器、加自定义模式、扩大帧缓冲。 | 普通用户。 | 手册没写“输出必须存在于驱动里”。实际上没有物理连接的输出需要驱动提供（比如 NVIDIA 要 `ConnectedMonitor` 之类选项），**手册没写，【未核实】**。 | — | [xrandr 手册](https://man.archlinux.org/man/xrandr.1.en) 【原文】 |
| **`evdi`**（DisplayLink 的虚拟显示内核模块） | 内核模块，作为标准 DRM 设备，用户态程序可以增删虚拟显示并接收屏幕更新，`xrandr` 或桌面设置能控制它。 | **要 root 安装内核模块**（通常 DKMS），内核 ≥ 4.15。 | **许可证：`module` 目录是 GPL-2，`library` 目录是 LGPL-2.1-or-later，其余是 MIT**【原文：README 和子目录 LICENSE 文件】。内核模块是 GPL-2：我们不能把它的代码带进来；让用户另装这个模块并通过 libevdi 动态链接调用，属于 LGPL 库的动态使用，**合规性要法务确认，【未核实】**。运维成本高（内核升级要重编，Secure Boot 要签名）。 | GPL-2 / LGPL-2.1+ / MIT（分目录） | [evdi README、各 LICENSE](https://github.com/DisplayLink/evdi) 【原文】 |
| **无头 Wayland 合成器：GNOME** | `gnome-shell --wayland --headless --virtual-monitor 1920x1080 --no-x11`（或 `mutter` 同参数）；配合 gnome-remote-desktop 的无头会话。 | 普通用户（会话内）。 | 社区文章称“实验性”；Fedora 36 上曾有失败报告；没有显示管理器时钥匙环是锁着的，RDP 连接要输密码；GDM 本身不创建虚拟显示器。命令行参数我没找到官方手册（`man mutter` 页里没有这些选项，只列了老的几个）。 | — | [SUSE 无头会话文章](https://www.suse.com/c/headless-remote-sessions-in-gnome-part-1)、社区帖【二手】 |
| **无头 Wayland 合成器：wlroots 系（Sway、Wayfire）** | `WLR_BACKENDS=headless` + `WLR_LIBINPUT_NO_DEVICES=1` 启动合成器，再用 wayvnc；Sway 里 `swaymsg create_output` 建虚拟输出。 | 普通用户。 | wayvnc 手册确认了上面两个环境变量的写法，说屏幕拷贝协议由 wlroots 系合成器实现；`-d` 选项可在合成器不支持虚拟指针/键盘时关闭远程输入。有 wlroots issue 报告无头后端上 wayvnc 只出灰屏。采集协议在 GNOME 和 KDE 上不同，我们的采集要走各自的接口。 | — | [wayvnc 手册](https://man.archlinux.org/man/wayvnc.1.en) 【原文】；搜索结果里的 wlroots issue、mailing list【二手】 |
| 无头 + 采集（KMS） | Sunshine 的默认 KMS 采集读取已连接的显示器；社区说合成器层面的虚拟显示器 KMS 看不到，需要换合成器自己的采集方式。 | — | 属于别人项目的社区说法，**【二手】** | — | Sunshine issue、社区项目（搜索结果） |

---

## 5. 同行对照（汇总）

| 产品 | Windows 隐私屏 | macOS 隐私屏 | 虚拟显示器 | 强度 |
|---|---|---|---|---|
| RustDesk（AGPL，只读） | 三种：排除出采集（默认，Win10 19041+）、放大镜、虚拟显示器 | 伽马清零 + CGEventTap（2026 新硬件上有失效风险） | Windows 用 Amyuni IDD；Mac 没看到 | 【原文，源码】 |
| TeamViewer | Win7+ 支持；Win7 及更老要装显示器驱动；**远程输入同时禁用** | 15.8+，辅助功能权限，随系统启动；Mac 密码框拦不住键盘 | 未查 | 【原文，官方文档】 |
| Jump Desktop | 支持（Mac 客户端连 Windows/Mac 被控端，2022-11） | 同上 | 有（Windows 上有虚拟显示器反馈帖） | 【二手】 |
| Splashtop | 有“空白屏幕”，Win/Mac 都有，限制见帮助页标题 | 有，有限制 | 未查 | 【二手】 |
| Parsec | 隐私模式（据 parsec-vdd 页说与虚拟显示器相关，我们的 G 文档里标未核实） | 无头 Mac 需要假负载（Parsec 帮助页，403） | Windows 自带 IDD 驱动，付费计划多屏 | 【二手】 |
| ToDesk | 有隐私屏，Mac 被控端范围没写 | 查不到 | 有“虚拟屏”，开启后有黑屏问题的 FAQ | 【未核实】 |

---

## 6. 对我们的建议

### 6.1 先做哪个系统、哪个做法

按用户画像（Mac mini 无显示器、Windows、Linux 常驻主机跑 AI Agent）和“先做真正闭环”的原则：

1. **第一步：macOS 虚拟显示器（`CGVirtualDisplay`）+ HDMI 假负载兜底。** 理由：核心用户最集中在 Mac mini，不接显示器是常态，没有它就没有画面，比隐私屏优先级高。先做“创建、按需销毁、可选分辨率、保活”，不做隐私屏。**先在一台真实无头 Mac mini 上验证 ScreenCaptureKit 到底给不给画面**（见 6.3），结果决定后面。
2. **第二步：Windows 隐私屏方案 A（覆盖窗口 + `WDA_EXCLUDEFROMCAPTURE`）+ 本地键鼠锁 A（钩子 + 标记）+ 本地退出组合键。** 理由：不需要驱动，立刻能做，可进 e2e。UAC 安全桌面上的限制必须在界面上说清楚，不静默假装生效。
3. **第三步：Windows 虚拟显示器（IddCx，集成 Virtual-Display-Driver，MIT）。** 受微软签名账号限制，先把账号和证书拉起来（外部等待最长），代码和签名并行。
4. **第四步：macOS 隐私屏。** 方案 A（覆盖窗口 + `SCContentFilter` 排除）为主；伽马清零只当补充，不当唯一手段；每次启用后做“是否真黑”的检测（见 6.2）；键鼠锁用 `CGEventTap`。
5. **第五步：Linux。** 虚拟显示器优先（Xvfb / Xdummy；Wayland 无头合成器），隐私屏和键鼠锁只在 X11 上做 `xinput` 方案，Wayland 明确提示“不支持”。

### 6.2 工程要点

- **不静默降级。** 任何隐私屏方案，启用后要有“本地屏确实黑了”的检测，检测不过就告诉用户并拒绝开启隐私模式，不能让用户以为在保护隐私。伽马方案读回值检测不出来（原文），所以 macOS 要靠方案 A 加覆盖窗口自身状态检测；没有硬件手段的地方，在界面上写明“无法确认”。
- **本地逃生口。** 键鼠锁一律给本地退出组合键，并在被控端屏幕上（黑屏之外）给不可忽略的提示；进程崩溃或会话结束要自动解锁、恢复显示（Windows 覆盖窗口随进程消失；macOS 事件拦截器随进程消失，伽马要注册退出恢复）。
- **注入标记。** 用每次会话随机的标记，不用固定常量（RustDesk 用固定值）；文档里写明：这防的是误触和旁观者，不是本机恶意软件。
- **UAC / 登录界面。** 在文档和界面上明确“隐私屏和键鼠锁在 UAC、登录界面上不生效”，这是用户态方案的固有限制。
- **许可证。** `evdi` 的内核模块是 GPL-2，不进我们的二进制；MS-PL 的 Microsoft 示例、MIT 的 Virtual-Display-Driver、MIT 的 DeskPad 如果借用要写进 `THIRD_PARTY_NOTICES.md`；`ge9/IddSampleDriver` 许可证不清，不用；RustDesk 代码不抄。`CGVirtualDisplay` 的类声明我们自己按公开资料写（不抄 DeskPad 头文件，也不抄 RustDesk）。

### 6.3 需要产品负责人做的事

| 事项 | 为什么 | 紧急程度 |
|---|---|---|
| 注册 Microsoft Partner Center 硬件开发者账号，买 **EV 代码签名证书** | IDD 驱动要微软签名，开账号要 EV 证书【原文】。等待时间不可控，要最先启动。 | 高 |
| 决定 Windows 驱动签名路线：attestation（快，但微软文档只写“用于测试场景”，不能上 Windows Update）还是 HLK/WHCP（慢，正式） | 影响 Windows 虚拟显示器能否面向付费用户长期分发。需要和微软或有经验的人确认，不能我们自己猜。 | 高 |
| 准备 **一台没接显示器的 Apple Silicon Mac mini**（最好两种机型、不同系统版本），再买 2 到 3 个 HDMI/DP 假负载 | 验证 ScreenCaptureKit 在零显示器、登录后、锁屏后各状态下有没有画面；验证 `CGVirtualDisplay` 创建的虚拟屏能否被采集；验证假负载兜底。 | 高 |
| 准备一台 **最新 Mac（M5 系列或 MacBook Neo）+ macOS 26.4 以上 / 27** | 验证伽马清零是否真的失效，决定要不要保留伽马方案。 | 中 |
| 准备 **Windows 10 22H2 和 Windows 11 24H2 各一台，各接一块真实显示器**，再准备一台无显示器的 | 验证 `WDA_EXCLUDEFROMCAPTURE` 对 DXGI Desktop Duplication 是否生效、一帧也不漏；验证 UAC 时本地屏的表现；验证钩子方案。 | 高 |
| 确认 Apple Developer 账号：Developer ID 签名和公证流程是否已就绪 | 官网分发的前提；是否要申请 DriverKit entitlement 作为 `CGVirtualDisplay` 失效后的备选路线，是产品决策。 | 中 |
| 决定是否接受“不上 Mac App Store” | `CGVirtualDisplay` 是私有接口，上架必被拒。我们本来要装 daemon 和权限，本来就走官网。 | 低（建议直接定） |
| 法务确认两件事：`libevdi`（LGPL）动态链接是否符合我们“不引入 GPL/AGPL”的规定；MS-PL 的合规要求 | Linux 虚拟显示器路线和 Windows 驱动借用 | 低 |

### 6.4 真机验证清单（每项通过才算数）

1. Windows：覆盖窗口开着时，用我们的 DXGI 采集连续抓 1000 帧，统计出现黑块的帧数，目标 0。
2. Windows：覆盖窗口开着时触发 UAC，记录本地物理屏上是否显示 UAC、远程是否看到。
3. Windows：钩子方案下，本地插 USB 键鼠操作，确认全被吞；远程注入全部通过；Ctrl+Alt+Del 行为。
4. macOS：无显示器 Mac mini 上 `SCShareableContent.displays` 是否为空；创建 `CGVirtualDisplay` 后能否出画面、能否切分辨率、重启后是否自动恢复。
5. macOS：覆盖窗口 + `excludingWindows` 连续采 1000 帧，统计是否漏出黑块；切换 Space、全屏 App、屏保、锁屏各看一次。
6. macOS：伽马清零在最新硬件上本地屏是否真的黑（用相机拍或人工看），以及采集是否保持正常。
7. macOS：`CGEventTap` 在权限被撤销时的行为（是否会卡死输入），以及密码框场景。
8. Linux：Xvfb 能否跑完整桌面；GNOME 无头 + 我们的采集路径；`xinput disable` 是否影响 XTEST。

---

## 7. 没查到或证据弱的地方（汇总）

- Windows：`MagSetFullscreenColorEffect` 黑色矩阵对采集的影响；`BlockInput` 的管理员要求和 UAC 行为；低级钩子超时具体数值；`WDA_EXCLUDEFROMCAPTURE` 对每种采集 API 的覆盖。
- Windows 驱动：attestation 签名能否长期面向终端用户分发的政策边界；Virtual-Display-Driver 的 Signed 具体指什么签名；Parsec 驱动的分发和签名链（帮助页读不到）。
- macOS：`CGVirtualDisplay` 在无头 Mac 上与 ScreenCaptureKit 的配合（只有二手说法，且互相矛盾）；覆盖窗口 + 排除过滤的稳定性；DriverKit 的显示类 entitlement；Jump、Splashtop、ToDesk、AnyDesk 的 Mac 隐私屏具体实现。
- Linux：X11 上覆盖窗口是否会被采集；DPMS 关屏后采集是否仍有画面；无头 GNOME 的官方命令行文档；Wayland 是否有标准的“排除窗口出采集”机制；`xinput disable` 对 XTEST 的影响。

---

## 8. 打开过的网址

微软文档（均【原文】）：
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-blockinput
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-kbdllhookstruct
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity
- https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowshookexa
- https://learn.microsoft.com/en-us/windows/win32/api/magnification/nf-magnification-magsetfullscreencoloreffect
- https://learn.microsoft.com/en-us/windows/win32/api/magnification/nf-magnification-magsetwindowfilterlist
- https://learn.microsoft.com/en-us/windows/win32/api/magnification/nf-magnification-maginitialize
- https://learn.microsoft.com/en-us/windows/security/application-security/application-control/user-account-control/how-it-works
- https://learn.microsoft.com/en-us/windows-hardware/drivers/display/indirect-display-driver-model-overview
- https://learn.microsoft.com/en-us/windows-hardware/drivers/display/iddcx-versions
- https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/code-signing-attestation
- https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/driver-signing-offerings
- https://learn.microsoft.com/en-us/windows-hardware/drivers/install/kernel-mode-code-signing-policy--windows-vista-and-later-

苹果文档和论坛：
- https://developer.apple.com/documentation/coregraphics/quartz-display-services.md
- https://developer.apple.com/documentation/coregraphics/cgsetdisplaytransferbytable(_:_:_:_:_:).md
- https://developer.apple.com/documentation/coregraphics/cgevent/tapcreate(tap:place:options:eventsofinterest:callback:userinfo:).md
- https://developer.apple.com/documentation/screencapturekit/sccontentfilter.md
- https://developer.apple.com/documentation/screencapturekit/sccontentfilter/init(display:excludingwindows:).md
- https://developer.apple.com/documentation/appkit/nswindow/sharingtype-swift.property.md
- https://developer.apple.com/library/mac/technotes/tn2150/_index.html
- https://developer.apple.com/app-store/review/guidelines/ （只读了前 100000 字符）
- https://developer.apple.com/forums/thread/792152
- https://developer.apple.com/forums/thread/842949
- https://developer.apple.com/forums/thread/786829
- https://developer.apple.com/forums/thread/795074
- https://developer.apple.com/forums/thread/819331
- （打开失败，404）https://developer.apple.com/documentation/coregraphics/cgvirtualdisplay 和 https://developer.apple.com/library/archive/documentation/GraphicsImaging/Conceptual/QuartzDisplayServicesConceptual/Articles/GammaCorrection.html

GitHub 仓库原文（直接下载核对）：
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/platform/macos.mm （RustDesk macOS 隐私模式实现，AGPL，只读）
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/privacy_mode.rs
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/privacy_mode/macos.rs
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/privacy_mode/win_mag.rs
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/privacy_mode/win_exclude_from_capture.rs
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/privacy_mode/win_input.rs
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/privacy_mode/win_topmost_window.rs
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/src/privacy_mode/win_virtual_display.rs
- https://raw.githubusercontent.com/rustdesk/rustdesk/master/LICENCE
- https://raw.githubusercontent.com/Stengo/DeskPad/main/README.md 、LICENSE.md 、DeskPad/CGVirtualDisplayPrivate.h
- https://raw.githubusercontent.com/chromium/chromium/main/ui/display/mac/test/virtual_display_util_mac.mm
- https://raw.githubusercontent.com/VirtualDrivers/Virtual-Display-Driver/master/README.md 、LICENSE
- https://raw.githubusercontent.com/ge9/IddSampleDriver/master/README.md
- https://raw.githubusercontent.com/Microsoft/Windows-driver-samples/main/LICENSE
- https://raw.githubusercontent.com/nomi-san/parsec-vdd/main/README.md 、LICENSE
- https://raw.githubusercontent.com/DisplayLink/evdi/main/README.md 、LICENSE 、module/LICENSE 、library/LICENSE
- https://raw.githubusercontent.com/waydabber/BetterDisplay/master/README.md
- https://github.com/slvssb/iMote/pull/3 【二手】
- https://github.com/Stengo/DeskPad

厂商和其他：
- https://www.teamviewer.com/en/global/support/knowledge-base/teamviewer-remote/security/teamviewer-black-screen/ 【原文】
- https://changelog.jumpdesktop.com/privacy-mode-for-fluid-qkivS 【二手】
- https://astropad.com/blog/dummy-plug-headless-mac-mini/ 【二手，厂商营销页】
- https://pkg.go.dev/github.com/go-macos/virtualdisplay 【二手】
- https://man.archlinux.org/man/xrandr.1.en 、xinput.1.en 、wayvnc.1.en 【原文】
- https://xorg.freedesktop.org/archive/X11R7.6/doc/man/man1/Xvfb.1.xhtml 【原文】
- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.RemoteDesktop.html 【原文】
- https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html 【原文】
- https://wayland.app/protocols/ext-session-lock-v1 【原文】
- 打开失败：Parsec、Splashtop、Jump Desktop 帮助中心（403）；AnyDesk Screen_Privacy（404）；wlroots GitLab（被反爬挡住）；mutter MR 1698（406）；`man.archlinux.org/man/sway-ipc.7`、`mutter.1`（页面里没有需要的选项）。

只有搜索摘要、没打开原页的资料（均按【二手】）：Parsec 帮助中心三篇、Splashtop “Limitations of Blank Screen”、Jump Desktop 帮助中心两篇、SUSE 无头 GNOME 两篇、1Password 社区关于 Secure Input 的帖子、MacRun / Remio / SimpleViewer 等博客、苹果论坛关于事件拦截权限的若干帖子。
