# PeerDesk 调研 J：远程键盘、系统快捷键、跨系统互操作

> 调研日期：2026-11-21。标注说明：**[已核实]** 表示读过代码或文档原文（含本仓库代码）；**[未核实]** 表示只基于记忆、二手资料或未打开的链接，落地前要再确认。
> 本报告在写作时没有能力逐条打开外部链接，所以外部产品的行为大多是 **[未核实]**；本仓库现状是 **[已核实]**。

## 0. 结论先行

1. **物理键位（USB HID 用途码）为主，文字（Unicode）为辅，不用 keysym 做主通道。** 这是 RDP、Parsec、Moonlight 的共同选择，也是我们现在的选择。VNC 用 keysym，所以布局不一致时问题最多。
2. **现状已有扎实的底子**：`InputEvent::Key{code: HID}` + `InputEvent::Text` + `ReleaseAll`，三个平台都已实现，焦点丢失会释放所有键 [已核实]。缺的是三块：
   - 系统快捷键捕获（Win 键、Alt+Tab、Cmd+Tab 现在都被本地系统吃掉，egui/winit 做不到）；
   - 跨系统修饰键映射（Mac→Windows 的 Cmd→Ctrl 等）和预设；
   - 锁定键（Caps/Num）状态同步、键盘布局相关的诚实提示。
3. **系统快捷键捕获只能靠原生代码**，放在客户端的平台层（符合“unsafe 只在平台层”）。三个平台各约 1–2 周，Wayland 最不可靠，要诚实降级并明说。
4. **Ctrl+Alt+Del 不能捕获，只能发“按钮”**。被控端是 Windows 时，必须由被控端服务以 SYSTEM 身份调用 SAS（见 G-unattended 的服务形态）。
5. **文字通道（Text）只给输入法上屏用**；普通按键绝不转成文字，否则游戏、快捷键、终端全坏。
6. 测试：映射表做表驱动单测（HID↔三个平台互查、往返），e2e 用 Xvfb + `xinput test-xi2`/`xev` 或自己写的 XI2 监听器校验被控端实际收到的键码。Windows/macOS 只能在 `[win]`/`[mac]` 实机跑，且不得动开发者键鼠（见 CLAUDE.md）。

---

## 1. 三种转发方式与同行做法

### 1.1 三种方式的本质

| 方式 | 发的是什么 | 优点 | 缺点 |
| --- | --- | --- | --- |
| 扫描码 / HID 用途码 | 物理位置（“键盘上从左数第二行第一个键”） | 被控端按自己的布局解释，行为和本地敲键盘完全一样；游戏、快捷键、热键正确；延迟最低 | 客户端布局和被控端布局不同时，用户看到的字符可能和键帽不一致；非键盘输入（手写、语音、IME 结果）发不了 |
| 虚拟键码 / keysym | 逻辑含义（“字母 a”“符号 @”） | 用户敲出的字符和键帽一致 | 被控端要把 keysym 反查成键位，找不到时得临时改键位映射（X11 的做法）；修饰键状态（Shift）要补发；游戏不认 |
| 文字（Unicode 字符串） | 最终字符 | 布局、IME、死键全绕开，CJK 上屏正确 | 不是“按键”：没有 down/up、没有修饰键语义，快捷键、游戏不可用；Windows 的 `KEYEVENTF_UNICODE` 对部分程序（游戏、某些终端）无效 [未核实] |

### 1.2 同行怎么做（均需落地前核对）

| 产品 | 做法 | 来源 / 状态 |
| --- | --- | --- |
| RDP | 两套并存：Fast-Path 键盘事件发 **Set 1 扫描码**（含 extended 标志），另有 **Unicode 键盘事件**（`TS_FP_UNICODE_KEYBOARD_EVENT`）；服务端按自己的布局把扫描码转成字符。连接时还会发客户端键盘布局 ID 和键盘类型（含日文 JIS 的 subtype），用于服务端会话的布局初始值 | [MS-RDPBCGR 2.2.8.1.2](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/) [未核实具体小节号]；FreeRDP 的 `freerdp_keyboard_*` 是现成的“系统键码→RDP 扫描码”表，Apache-2.0 [未核实] |
| VNC (RFB) | **keysym**（X11 定义的符号）。服务端要把 keysym 反查成键位，布局不同就错位；`ExtendedKeyEvent` 扩展（QEMU 提出）额外带键盘扫描码，解决这个问题 | [RFB 协议 KeyEvent](https://github.com/rfbproto/rfbproto/blob/master/rfbproto.rst)；QEMU 扩展见同文档 “QEMU Extended Key Event Message” [未核实] |
| Moonlight / Sunshine | 客户端发 **Windows 虚拟键码（VK）**，并带 `SS_KBE_FLAG_NON_NORMALIZED` 等标志；Sunshine 在主机上转成扫描码用 SendInput 注入。较新版本加了“用扫描码”选项 [未核实]。另有 UTF-8 文字事件（`LiSendUtf8TextEvent`） | [moonlight-common-c Limelight.h](https://github.com/moonlight-stream/moonlight-common-c) [未核实]；[Sunshine input.cpp](https://github.com/LizardByte/Sunshine) [未核实] |
| Parsec | 官方说法是发物理键位（扫描码），有 “Keyboard layout/Use scancodes” 之类设置；Mac 客户端有 “Swap Cmd and Ctrl” 之类选项 | 官方帮助中心 [未核实]，没有打开具体页面 |
| Chrome Remote Desktop | 浏览器客户端取 `KeyboardEvent.code`（物理键位）+ `key`；主机端据此注入，另有 “发送 Ctrl+Alt+Del / PrtScn” 菜单项 | [chromium remoting 源码](https://source.chromium.org/chromium/chromium/src/+/main:remoting/) [未核实]，protocol 里有 `KeyEvent{usb_keycode, pressed}`，即 **HID 用途码** [未核实，但印象较确定] |
| AnyDesk / TeamViewer | 私有协议，无公开文档。公开设置项有 “发送快捷键到远端（Win 键、Alt+Tab）”“键盘布局同步/忽略键盘布局”“Mac 上 Cmd 映射为 Ctrl” 等 | 官方帮助文档 [未核实]，行为推断自界面设置，没读到实现 |
| Jump Desktop | Fluid/RDP/VNC 三种协议，设置里有 “Mac 键盘：Cmd→Ctrl / Cmd→Win / Option→Alt” 的可选映射和“全局快捷键捕获”（需要辅助功能权限） | [Jump Desktop 帮助：键盘](https://support.jumpdesktop.com/) [未核实具体页面] |

对 PeerDesk 的启示：**HID 用途码在 CRD 和我们这里已经是“标准答案”**；RDP 用扫描码和 Unicode 并存，和我们 `Key` + `Text` 的二分法一致。

### 1.3 键盘布局不一致

场景：客户端美式键盘，被控端德语（QWERTZ）。

- 按 HID 发 “Y 键”（美式键盘上写着 Y 的位置，HID 0x1C）→ 被控端德语布局下产生 `z`。用户眼里：我按 Y 出来 Z。这和“把美式键盘插到德语电脑上”完全一致，**是预期行为**，但用户会觉得是 bug。
- 按 keysym/字符发 `y` → 被控端要找到哪个键位产出 `y`（德语布局下是 Z 键位）；快捷键（Ctrl+Y）会跟着字符走，游戏 WASD 会错位。
- 同行：RDP 在连接时同步布局 ID，让服务端会话采用客户端布局，所以“按物理位置”和“按字符”恰好一致 [未核实]。

PeerDesk 建议（见 §6）：
1. 默认**物理键位**（现状）；
2. 被控端把当前布局名（如 `de`、`us`、`0x0407`）周期性回报给客户端，客户端在布局不同时在状态栏提示一次：“被控端键盘布局为 德语，和本机不同，字母键位置会不同”（符合“诚实，不静默”）；
3. 提供“按字符发送”模式作为可选项（文字通道的扩展，用于可打印字符、不带 Ctrl/Alt/Cmd 时）。优先级低。

### 1.4 IME / 中日韩输入、死键

- **客户端组字（现状）**：用户在本机输入法里打拼音，上屏的最终文本通过 `Text` 发给被控端，被控端用 Unicode 注入（Windows `KEYEVENTF_UNICODE`，macOS `keyboard_set_unicode_string`，X11 临时绑定空闲键码）[已核实，见 `input.rs` 的 `ImeEvent::Commit` 和三个平台文件]。优点：客户端输入法的词库和习惯保持不变，延迟低（组字过程不走网络）；缺点：看不到被控端的候选框位置，被控端程序的“输入法感知”功能（如日文的重新转换、Windows 的 TSF 上下文）用不上。
- **被控端组字**：把每个物理键都发过去，被控端输入法处理。优点：和本地一致，游戏里的输入法状态一致；缺点：被控端要装并切换到输入法，候选框画在被控端画面里（有延迟），客户端 IME 要关掉，否则双重输入。RDP 默认走这条（并有 IME 同步状态的扩展）[未核实]。
- **同行**：Parsec 默认被控端组字 [未核实]；Chrome Remote Desktop 主要靠客户端浏览器 IME 的 `compositionend` 发文字 [未核实]。
- **PeerDesk 建议**：保持“客户端组字 + Text 上屏”为默认；组字期间按键不发（现状 `composing` 标志）[已核实]。增加“直通模式”开关（所有键直通，客户端 IME 关闭）供游戏和被控端有特殊输入法的用户。egui 的 IME 事件在各平台完整度不一，Linux（尤其 Wayland、fcitx5/ibus）要实测 **[未核实]**。
- **死键**（`´` + `e` → `é`）：
  - 按键直通时，死键和后续字母都作为物理键发出，由被控端布局合成，天然正确，前提是两端布局一致。
  - 本机合成（客户端 IME/布局把死键吃掉了）时 egui 给的是 `Text("é")`，而我们现在丢掉 `Event::Text` [已核实]。这在“被控端布局没有死键、客户端有”时会丢字。建议：**当一个 `Text` 事件没有对应的按键事件（没有来自可打印物理键的按下）时发文字通道**。现状代码假设“字符已经通过物理键位发了”，对死键/Compose/AltGr 这类组合是漏洞，需要补测试 **[已核实代码；漏洞为推断，需用实机验证]**。
  - AltGr：Windows 上 AltGr 在系统里表现为 Ctrl+Alt，直接转发物理右 Alt，被控端会自己处理；但如果被控端是 Mac/Linux，右 Alt 的含义不同（见 §3）。

---

## 2. 在客户端捕获系统快捷键

egui/winit 只能收到**操作系统已经决定交给应用的事件**。Win 键、Alt+Tab、Cmd+Tab、Cmd+Space 这类系统级组合，在应用拿到之前就被系统消费，所以必须用原生钩子。

### 2.1 Windows：WH_KEYBOARD_LL

- `SetWindowsHookEx(WH_KEYBOARD_LL, proc, hInstance, 0)`，回调收到 `KBDLLHOOKSTRUCT`（`vkCode`、`scanCode`、`flags`，含 `LLKHF_EXTENDED`、`LLKHF_INJECTED`、`LLKHF_ALTDOWN`）。回调里返回非零就吞掉按键，系统不再处理 [Microsoft 文档 LowLevelKeyboardProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc) [未核实，API 本身确定]。
- 能拦：Win 键（及 Win+X 组合）、Alt+Tab、Alt+Esc、Ctrl+Esc、Alt+F4 等。
- **不能拦**：Ctrl+Alt+Del（SAS，由 winlogon 处理，用户态钩子看不到）；Win+L 在部分版本不可靠 [未核实]；UAC 安全桌面和锁屏界面上的钩子不生效；Windows 在一些情况下会对回调超时（约 300 ms，注册表 `LowLevelHooksTimeout`）后静默摘除钩子 [未核实具体数值]，所以回调必须极快：只做“判断是否拦截 + 入队”，不在回调里做网络或锁。
- 钩子线程需要消息循环；使用 `LLKHF_INJECTED` 忽略我们自己或别的程序注入的事件，避免回路（本机测试注入也会触发）。
- 触发条件：**只有远程窗口获得焦点且处于“捕获模式”时才拦**；窗口失焦、最小化时立即释放钩子并 `ReleaseAll`。
- 辅助：`RegisterHotKey` 拦不了 Win 组合键，不要用。
- Ctrl+Alt+Del：提供菜单项/快捷键（如 Ctrl+Alt+End，RDP 同款）“发送 Ctrl+Alt+Del”，协议里加一个 `SendSas` 事件；被控端由 SYSTEM 服务调 `sas.dll` 的 `SendSAS(FALSE)`（需要组策略 “SoftwareSASGeneration” 允许服务调用）[未核实，需实测]。被控端是 Mac/Linux 时，映射成各自的“强制退出/锁屏”或不提供，并提示原因。
- 安全：钩子能看到所有按键，属于键盘记录敏感能力。必须**只在捕获模式开启**、文档写清楚、不记录不上传。杀毒软件可能告警，签名（见 I-release）能缓解。

### 2.2 macOS：CGEventTap

- `CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault, mask, callback, ...)`，回调返回 NULL 即吞掉 [Apple 文档 Quartz Event Services](https://developer.apple.com/documentation/coregraphics/quartz_event_services) [未核实]。
- 需要权限：主动拦截（非 listen-only）要 **辅助功能（Accessibility）**；仅监听要 **输入监控（Input Monitoring）**。未授权时 `CGEventTapCreate` 返回 NULL；用 `AXIsProcessTrustedWithOptions` 检测并提示去系统设置 [未核实]。**不要静默降级**：没有权限时在界面明确写“未授权，Cmd+Tab 等会在本机触发”和一键打开设置的按钮。
- 能拦：Cmd+Tab、Cmd+Space（Spotlight）、Cmd+Q、Cmd+H、Mission Control 的 Ctrl+方向键和 F3 等；多数系统快捷键在 tap 之前被 WindowServer 处理，**部分仍拦不到**，已知的有：电源键/Touch ID 键、触控栏、`Fn+` 特殊功能的一部分、Secure Input 开启期间（密码框聚焦）tap 收不到按键 [未核实]。
- 媒体键/亮度键是 `NSSystemDefined`（subtype 8）事件，不是普通 keyDown，要单独处理（见 §4）。
- 系统有时会因回调慢或用户输入禁用 tap（`kCGEventTapDisabledByTimeout`），回调里要处理并重新启用。
- 另一种较轻的方案：`NSApplication.presentationOptions`（`DisableProcessSwitching` 等，全屏下可用）能禁用 Cmd+Tab、Dock、菜单栏弹出，无需额外权限，但只在应用进入特定全屏模式时有效、且不能禁 Cmd+Space [未核实]。Jump Desktop 等就是组合使用 [未核实]。

### 2.3 Linux

- **X11**：`XGrabKeyboard`（整个键盘独占）或 `XGrabKey`。窗口管理器的快捷键（Super、Alt+Tab）通常是 WM 自己用 `XGrabKey` 抓的，我们对**整个键盘**抓取后，WM 就收不到了，Alt+Tab 会到我们这里。注意：抓取可能因其他程序已抓取失败（`GrabAlreadyGrabbed`），要明确报错 [X11 协议文档](https://www.x.org/releases/current/doc/xproto/x11protocol.html#requests:GrabKeyboard) [未核实]。某些桌面（GNOME）对抓取有“特殊处理”，仍可能保留 Super 键 [未核实]。
- **Wayland**：客户端不能全局抓键盘。协议 `zwp_keyboard_shortcuts_inhibit_manager_v1`（keyboard-shortcuts-inhibit-unstable-v1）：窗口请求合成器“暂时不要处理快捷键，交给我”，合成器可以拒绝，且通常留一个逃生组合（GNOME 是长按 Super，约 1 秒？）[协议：wayland-protocols](https://wayland.app/protocols/keyboard-shortcuts-inhibit-unstable-v1) [已知存在，细节未核实]。GNOME（mutter）、KDE（KWin）、sway 支持；不支持的合成器得告诉用户 [未核实各合成器现状]。winit 0.30 没有暴露这个协议 [未核实]，要自己用 `wayland-client` 拿到 `wl_surface` 去请求。
- 捕获模式下被拦到的 Super、Alt+Tab 转成 HID 发出去；退出捕获要有明确的“逃生键”，防止用户被困在远程里。

### 2.4 egui/winit 的限制，我们必须原生做的事

| 需求 | egui/winit 能否 | 要自己做 |
| --- | --- | --- |
| Win/Cmd/Super 单键事件 | egui 的 `Modifiers` 有 `mac_cmd`、`command`，没有独立的 Win 键状态；winit 的 `KeyCode::SuperLeft/SuperRight` 有，但 egui 不转成 `Key` [已核实 `keys.rs` 只从 `mac_cmd` 合成左 GUI] | 在平台层读原始键事件（Windows 钩子、macOS tap、Linux evdev/XI2） |
| 左右修饰键区分 | egui 不区分 | 同上；现在统一发“左”侧键码 [已核实] |
| 复制/剪切/粘贴 | egui 把 Ctrl/Cmd+C/X/V 截成 `Event::Copy/Cut/Paste` | 现已补发（`shortcut()`）[已核实] |
| 系统保留组合 | 拿不到 | 原生钩子/tap/抓取 |
| 按键重复标志 | 有 `repeat` | 现已丢弃重复（见 §3.4）[已核实：`key_down` 对重复的处理需再看，见下] |
| 媒体键、Fn、PrtSc | 大部分没有 | 原生 |
| 锁定键状态 | 无 | 原生读取 |

**设计原则**：新增一个平台层模块 `crates/platform/src/capture/{windows,macos,x11,wayland}.rs`，输出统一的 `RawKey{hid, down, source}` 流，客户端 GUI 在“捕获模式”时优先用它，不用 egui 的按键事件；未捕获模式仍走 egui 路径。这样 e2e 和单测可以把这个流替换为脚本（符合“测试构建里系统输入注入一律不打开”：测试用假的 `RawKey` 源，不真的装钩子）。

---

## 3. 修饰键映射、重复键、锁定键

### 3.1 同行的选项

| 产品 | 提供 |
| --- | --- |
| Microsoft Remote Desktop for Mac | “Use Mac-style keyboard shortcuts”或类似：Cmd 当作 Ctrl [未核实] |
| Parsec | Mac 客户端 “Swap Command and Ctrl” [未核实] |
| Jump Desktop | Cmd→Ctrl / Cmd→Win、Option→Alt，可分别设置 [未核实] |
| Citrix / VMware Horizon | 按平台提供“映射 Cmd 到 Ctrl”和“发送 Mac 原生快捷键”两种模式 [未核实] |

### 3.2 PeerDesk 的映射模型

以**“物理键位 + 修饰键重映射”两层**为核心，规则表驱动。

**修饰键的物理对应**（HID）：

| 物理键 | Windows 键帽 | macOS 键帽 | 说明 |
| --- | --- | --- | --- |
| 0xE0/0xE4 | Ctrl | Control | |
| 0xE1/0xE5 | Shift | Shift | |
| 0xE2/0xE6 | Alt | Option | Windows 右 Alt 常是 AltGr |
| 0xE3/0xE7 | Win | Command | 位置：Mac 键盘空格键左侧顺序是 Control、Option、Command；PC 是 Ctrl、Win、Alt |

注意**位置不同**：Mac 键盘空格键左边是 Option、Command（外侧 Ctrl 之后），PC 键盘是 Win、Alt。所以“物理位置映射”（Mac 的 Option 键位置恰好是 PC 的 Win 键位置）和“语义映射”（Cmd 当 Ctrl）会冲突，**用户需要选**。

**预设**（用户可改、可按被控端系统自动默认）：

| 预设 | 客户端 → 被控端 | 适用 |
| --- | --- | --- |
| 原样（物理） | HID 原样发 | 同系统；游戏 |
| Mac→Windows/Linux 语义 | Cmd→Ctrl，Option→Alt，Control→Win（可选），Cmd+Tab→Alt+Tab，Cmd+Space→Win（可选） | Mac 控制 Windows/Linux，默认 |
| Windows/Linux→Mac 语义 | Ctrl→Cmd，Alt→Option，Win→Control（或 Win 保持 Cmd），Alt+Tab→Cmd+Tab | PC 控制 Mac，默认 |
| 自定义 | 每个修饰键独立下拉 | 高级 |

**默认选择规则**：客户端 OS ≠ 被控端 OS 时，默认启用对应“语义”预设，并在连接时的状态栏提示“已启用 Mac→Windows 键位映射，点击修改”（诚实、可见）。客户端 OS = 被控端 OS 时默认“原样”。

### 3.3 常用快捷键的跨系统对照

| 动作 | Windows/Linux | macOS | 语义预设下的处理 |
| --- | --- | --- | --- |
| 复制/粘贴/剪切/全选/撤销/保存 | Ctrl+C/V/X/A/Z/S | Cmd+C/V/X/A/Z/S | 只换修饰键，字母不变 |
| 重做 | Ctrl+Y 或 Ctrl+Shift+Z | Cmd+Shift+Z | 保持 Shift+Z，被控端多数软件认；Word 等只认 Ctrl+Y [未核实] |
| 切应用 | Alt+Tab | Cmd+Tab | 整体转换 |
| 关闭窗口/退出 | Alt+F4 / Ctrl+W | Cmd+W / Cmd+Q | Cmd+Q→Alt+F4（可选） |
| 光标按词跳 | Ctrl+←/→ | Option+←/→ | 语义映射需要特例表，否则会错 |
| 行首行尾 | Home/End | Cmd+←/→ | 特例表 |
| 删除到行首、整词删除 | Ctrl+Backspace | Option+Backspace / Cmd+Backspace | 特例表 |
| 截图 | PrtSc、Win+Shift+S | Cmd+Shift+3/4/5 | **默认不转换**，因为截图指向本机还是被控端有歧义：提供“截取被控端画面”按钮（客户端本地截图）和“发送被控端截图快捷键”两项，明说差别 |
| 锁屏 | Win+L | Ctrl+Cmd+Q | 提供菜单项，不靠组合键 |
| 搜索/启动 | Win、Win+S | Cmd+Space | Cmd+Space→Win（可选） |

“特例表”做成可数据化的规则：`(客户端修饰集, 键) → (被控端修饰集, 键)`，先匹配特例再退回通用修饰键替换。

### 3.4 防止修饰键卡住

- 现状：`WindowFocused(false)` 和会话中断时发 `ReleaseAll`，并清客户端的按下集合 [已核实，`input.rs` 第 275 行、`viewer.rs` 第 65 行]，被控端三个平台实现了 `ReleaseAll`。
- 还缺的场景：
  1. 捕获模式下按 Cmd+Tab/Alt+Tab 切走：钩子已拦，焦点切换事件可能晚于键抬起，要在“捕获模式退出”时无条件 `ReleaseAll`。
  2. 网络丢包：按下发到了、抬起没到。传输层是 QUIC 可靠流还是数据报？输入是否可靠有序要确认，**输入事件必须走可靠通道或带序号重传**，否则抬起丢失就卡键 [见 A-transport，输入优先；具体通道待核对代码]。
  3. 被控端自保护：每个键按下后启动“看门狗”，超过 N 秒（如 30 s）没有任何输入更新且客户端心跳丢失，则被控端自动 `ReleaseAll`；客户端断线时被控端必须 `ReleaseAll`。
  4. 客户端定期（如每秒）发送“当前按下集合”快照，被控端比对修正（防御性，成本只有几十字节）。
  5. Windows 的 Win 键：Win 按下后再被系统吃掉抬起（如打开开始菜单），需要在钩子里自己记账。

### 3.5 按键重复

两种实现：
- **客户端重复**：客户端按住时系统不断发 `repeat` 事件，我们每次都转发 down。优点：重复速率符合用户本机设置；缺点：网络抖动时重复节奏不均，网络中断时按键“卡住连发”。
- **被控端重复**：只发一次 down，被控端系统自己重复直到收到 up。优点：节奏均匀，不额外占带宽；缺点：被控端系统的重复延迟/速率可能和用户习惯不同，且丢了 up 会无限重复（所以必须有看门狗）。
- Windows `SendInput` 一个 down 不会自己重复，实际上**被控端系统不会为注入的键自动重复** [未核实]：X11 XTEST 注入的键，X server 会对其自动重复（autorepeat）[已知行为，未核实具体细节]；Windows 的 SendInput 注入的 down，只要不发 up，应用收到的是一次 down，其后按键重复由键盘驱动层产生，注入键不会 [未核实，需实测]。
- **建议**：客户端重复 + 被控端对 `repeat` 与非 `repeat` 区分（`InputEvent::Key` 目前没有 repeat 字段）。需要实测三个平台对“同一键连续 down”的处理；Windows 允许连续 down（应用看到 repeat 位），X11 XTEST 连续 down 无 up 也可 [未核实]，macOS CGEvent 需要设置 `kCGKeyboardEventAutorepeat` 字段 [未核实]。**现状 `key_down` 的行为再核对**：当前代码对已按下的键是否丢弃重复需要看 `key_down`/`keys` 集合逻辑 [已核实存在 `keys` 集合；重复是否被去重需看第 118 行附近]。

### 3.6 锁定键（Caps / Num / Scroll）同步

问题：本机 Caps 灯亮，被控端 Caps 状态不一致 → 用户敲出大小写反的字。

- RDP 有 `TS_SYNC_EVENT`（同步 Scroll/Num/Caps/Kana 状态），连接时和焦点回来时发 [MS-RDPBCGR 2.2.8.1.1.3.1.1.5 TS_FP_SYNC_EVENT [未核实小节号]]。VNC/Parsec 多数不做 [未核实]。
- **建议**：
  1. 协议加 `LockState{caps, num, scroll}`，被控端每次变化回报，客户端在状态栏显示被控端 Caps/Num 状态（比强行同步更诚实，也不会意外改动被控端）；
  2. 客户端获得焦点时，如果两端 Caps 不同，**默认不自动改**，只提示；提供选项“自动同步”（通过发一次 Caps 键切换）；
  3. Caps 是**切换键**，发 down+up 即切换一次，需注意 macOS 的 Caps 有延迟 down 的特殊行为（要按住约 0.1 s 才生效）[未核实]。
  4. 读取被控端状态：Windows `GetKeyState(VK_CAPITAL) & 1`，X11 `XkbGetIndicatorState`，macOS `CGEventSourceFlagsState` 里的 `alphaShift` [未核实，API 名为印象]。

---

## 4. 特殊键

| 键 | 说明 | 做法 |
|---|---|---|
| 媒体键（音量、播放、静音） | HID Consumer 页 0x0C（如 0xE9 音量+，0xCD 播放/暂停），不在键盘页 0x07；macOS 是 `NSSystemDefined` 事件；Windows 是 `VK_VOLUME_*`/`VK_MEDIA_*` | **默认不转发到被控端**（用户按音量键多半想调本机音量），提供选项“媒体键发给被控端”。协议里 `Key` 的 `code` 现在是 `u16`，需要加“页”（`usage_page`）或用高位编码 Consumer 页 |
| Fn | 多数键盘的 Fn 是固件内部状态，不产生 HID 事件（Apple 的 Fn/地球键产生 Apple 自定义页事件） | 不转发 Fn 本身，只转发它组合出的结果（F 键、方向键、Home/End）；Mac 笔记本的“F 键默认功能/媒体功能”由本机决定，写进文档 |
| Print Screen | HID 0x46；Windows 上 PrtSc 在钩子里可见；macOS 没有对应键 | 转发，客户端本机也可能被截图软件拦截，捕获模式下拦下 |
| Pause/Break | HID 0x48；Windows 扫描码特殊（Pause 是 `E1 1D 45` 多字节序列，Set 1 里是 0xE1 前缀，常见的实现坑）[MS Keyboard Scan Code Specification，未核实具体字节] | 已有 `PAUSE` 常量 [已核实]；Windows 发注入时要特殊处理多字节序列或用 VK_PAUSE，要用实机测试 |
| 日文 JIS / 韩文键盘 | JIS 额外键：`¥`（HID 0x89）、`_`（0x87，International1）、变换（0x8A）、无变换（0x8B）、平假名/カタカナ（0x88）、半角/全角（Windows 对应 HID 0x35 在 JIS 上的位置，实际有差异）；韩文：한/영（0x90）、汉字（0x91） | 在 HID 表中补齐 0x87–0x8B、0x90–0x91 和 Linux evdev 对应（`KEY_RO`、`KEY_YEN`、`KEY_HENKAN`、`KEY_MUHENKAN`、`KEY_KATAKANAHIRAGANA`、`KEY_HANGEUL`、`KEY_HANJA`），macOS 对应 `kVK_JIS_*`。被控端布局是 JIS 时才有意义；ISO 键盘的额外键（`<>|`，HID 0x64 Non-US Backslash）也要补 |
| 小键盘 | HID 0x53（Num Lock）、0x54–0x63；Windows 的 Numpad 键在 NumLock 关闭时是方向键/Home 等，扫描码相同但 extended 位不同（**坑**：小键盘 Enter 是 extended，普通 Enter 不是；方向键/Home/End 区分“小键盘版（非 extended）”和“独立版（extended）”）| 表里按 extended 位精确处理，写测试（见 §5）；Mac 没有 NumLock，Mac 控制 PC 时 Mac 键盘的 Clear 键（0x53）作 NumLock |
| 右 Alt / AltGr | Windows 上 AltGr = Ctrl+Alt（右 Alt 键发 Ctrl 假事件） | 客户端若是 Windows，钩子里会看到“假 Ctrl”（`LLKHF` 里不标记，通过扫描码 0x21D 可识别）要过滤 [未核实] |

当前 `keymap.rs` 的 `to_evdev`/`to_x11` 和 `keys.rs` 的 `hid_of` 覆盖字母、数字、F1–F12、方向键、修饰键等；JIS/Consumer 页、`F13–F24`、小键盘细节是否齐全需要核对（`keys.rs` 里测试用了 `Key::F30` 说明未映射键被忽略）[已核实：未映射键直接丢弃，没有给用户任何提示，应加日志和计数指标]。

---

## 5. 测试策略

### 5.1 单元测试（表驱动，本地 + CI Linux）

1. **映射表完整性与往返**：
   - 对每个受支持的 HID 码：`hid → evdev → hid`、`hid → x11 keycode`、`hid → Windows scancode(+extended) → hid`、`hid → macOS kVK`，断言无冲突（不同 HID 不映射到同一目标）、无遗漏（与一份“权威清单”比较，清单可来自 Linux `hid-input.c` 和 USB HID 表的子集）。
   - 反向：`egui::Key → hid` 全覆盖，未映射的键白名单固定，新增 egui 键时测试失败提醒。
2. **修饰键映射预设**：每个预设一组“输入序列 → 输出序列”的黄金测试，如 Mac 预设下 `[Cmd↓, C↓, C↑, Cmd↑]` → `[Ctrl↓, C↓, C↑, Ctrl↑]`；Cmd+Tab 整体转换时修饰键顺序、不泄漏多余的 down/up。
3. **状态机性质测试**（proptest）：任意事件序列（含焦点丢失、断线、重复）之后执行 `release_all`，客户端和被控端的按下集合必为空；任何 up 都对应过 down。
4. **IME 序列**：`Preedit` → 按键被吞 → `Commit` 发 `Text`；死键序列；纯 ASCII 上屏不重复（已有部分测试 [已核实]）。
5. **覆盖率**：映射表是纯数据 + 查表，能达到 99% 门槛；钩子/tap/grab 的系统调用胶水按 `scripts/coverage.sh` 现有做法排除，理由写在脚本注释里，但**把“事件→RawKey”的转换逻辑拆成纯函数**放在可测的文件里。

### 5.2 端到端（Linux CI：Xvfb）

被控端真实运行 `peerdesk-server` 的 X11 注入（XTEST），在同一个 Xvfb 里跑一个**校验进程**：
- 方案 A：`xinput test-xi2 --root`（或自写小程序用 XI2 `RawKeyPress`）记录收到的键码序列和时间戳；
- 方案 B：`xev -root -event keyboard` 解析输出；
- 方案 C（更稳）：用 `x11rb` 写测试辅助程序，订阅 XI2 原始事件并输出 JSON，测试断言“键码序列、修饰键状态、`XkbGetState` 的 mods/锁定位”。
- 用例：
  1. 字母/数字/符号/F 键/方向键/小键盘逐一发 HID，断言 X11 keycode = evdev+8；
  2. 修饰组合（Ctrl+Shift+A）顺序和状态；
  3. `Text("é你好")` 经临时键码绑定后，校验应用层收到的 keysym（`XLookupString`）并确认**绑定被还原**，不污染键盘映射（`xmodmap -pke` 前后对比）；
  4. 断线 `ReleaseAll`：按住 Shift 后断开，校验 Shift 已抬起；
  5. 布局：用 `setxkbmap de` 切布局，发 HID 0x1C，校验产出 `z`（证明走的是物理键位）；
  6. 重复：长按 2 s，统计 down 次数，对比选定的重复策略。
- 端到端的“真实”要求：用真实的客户端输入翻译层（`InputState::translate`）喂入 egui 构造的事件，经真实协议、真实被控端，再到 Xvfb 校验——整条链路，不手写协议包 [符合 CLAUDE.md “闭环优先”]。
- **Windows / macOS**：`[win]`/`[mac]` 实机跑；测试里注入不能动开发者键鼠，CI 里在专用会话下运行，写入一个测试窗口（Win32 简单窗口 / AppKit 窗口）里记录收到的消息（`WM_KEYDOWN` 的 `scancode`、`WM_CHAR`）；与现有“测试构建里系统输入注入一律不打开”规则要对齐：只在 CI 专用 feature 打开 **[待与现有实现核对]**。
- 钩子/tap：用 `SendInput`/`CGEventPost` 模拟系统快捷键很难覆盖“系统先吃掉”的真实行为，这部分只能**半手动验收清单**（每个发布前在三平台各过一遍：Win、Alt+Tab、Cmd+Tab、Cmd+Space、Super、Ctrl+Alt+Del 按钮），写进 `docs/` 的发布检查表，不假装自动化。

### 5.3 指标（进 e2e 门槛，只升不降）

| 指标 | 含义 | 初始门槛（建议） |
| --- | --- | --- |
| 键到注入延迟 | 客户端按下 → 被控端 XTEST 完成（仿真时钟，局域网） | 中位数 ≤ 5 ms 加网络单程，P99 ≤ 20 ms 加网络单程 |
| 键序保持 | 乱序/丢失的键事件数 | 0 |
| 卡键 | 断线/失焦/丢包后被控端仍处于按下状态的键数 | 0，注入丢包 5% 的仿真里仍为 0 |
| 映射覆盖 | 受支持 HID 码数、未映射被丢弃的键事件计数 | 只升不降；丢弃事件必须被记录 |
| 布局正确性 | 切布局用例通过数 | 全过 |
| 文字上屏正确率 | `Text` 序列 vs 被控端收到的字符 | 100%（含 emoji 代理对 [已核实 Windows 按 UTF-16 发]） |

### 5.4 对标差距（写明）

- Parsec/AnyDesk：系统快捷键捕获、Mac 修饰键映射都是标配，我们目前**完全没有**，属于“能不能用”的差距，不是细节。
- Parsec：游戏用的原始输入（相对鼠标、手柄）不在本报告范围。
- RDP：布局/锁定键同步、Unicode 事件我们有一半（Unicode 有，布局同步没有）。

---

## 6. 推荐设计与实施步骤

### 6.1 设计总览

```
客户端（每平台）                               被控端
 egui 事件 ──┐                                          
 平台捕获层 ─┴→ RawKey{hid, down, repeat} ──→ 修饰键映射（预设/自定义）──→ InputEvent::Key
 IME Commit / 无按键对应的 Text ───────────────────────→ InputEvent::Text
 菜单：发送 Ctrl+Alt+Del、媒体键开关、映射预设 ─→ InputEvent::Sas / 配置
                      ↑ 被控端回报：布局名、Caps/Num 状态、是否支持 SAS
```

要点：
1. **映射放在客户端**（发送前），被控端只做“HID → 本系统键码”，这样被控端不需要知道客户端是什么系统，协议稳定；被控端回报 `os` 和布局，客户端据此选默认预设。
2. **协议**：`Key` 加 `repeat: bool` 和（可选）`page: u8`（键盘页/Consumer 页）；新增 `Sas`、`LockState`（被控端→客户端）、`HostKeyboardInfo{os, layout}`。向后兼容按现有协议版本规则做 [待核对 `core/src/input.rs` 的版本机制]。
3. **捕获模式**：连接窗口获得焦点时默认开启（可设置），菜单栏显示状态图标“键盘已捕获 / 未捕获：原因”；退出键（如 Ctrl+Alt+Shift+Esc 或长按 Esc 1 秒）；失焦/最小化立即释放；任何失败（macOS 没权限、Wayland 不支持、X11 抓取冲突）**明确提示原因和解决办法**，不静默降级。
4. **安全与隐私**：钩子只在捕获模式运行，不落盘，文档里写明；符合“账户计费、不收集用户数据”的方向。
5. **许可证**：FreeRDP 键码表可借鉴但要核对许可证（Apache-2.0）并在 `THIRD_PARTY_NOTICES.md` 注明；不引入 GPL 的 xrdp/TigerVNC 代码；Sunshine 是 GPL-3.0，**只看不抄** [未核实许可证，务必确认后再谈借鉴]。

### 6.2 实施步骤与工作量

| # | 步骤 | 内容 | 估计 |
|---|---|---|---|
| 1 | 映射表补全 + 单测 | JIS/韩文/ISO 额外键、小键盘 extended 细节、F13–F24、Pause 特殊序列；往返与冲突测试；未映射键记日志和指标 | 2–3 天 |
| 2 | 修饰键映射层 | 预设（原样/Mac→Win/Win→Mac/自定义）、特例表、设置界面、连接时按两端系统选默认；黄金测试 | 3–4 天 |
| 3 | 卡键与重复键 | 协议加 `repeat`；被控端看门狗和断线释放；按下集合快照；丢包仿真下的卡键 e2e；三平台重复行为实测 | 3–4 天 |
| 4 | 文字通道补洞 | 死键、Compose、AltGr 产生的 `Text` 与按键去重策略；IME 直通模式开关；Linux fcitx5/ibus 实测 | 3–5 天 |
| 5 | 锁定键同步与布局提示 | `HostKeyboardInfo`、`LockState`，状态栏提示；按需自动同步 | 2–3 天 |
| 6 | Windows 捕获 | `WH_KEYBOARD_LL` 线程、捕获模式、过滤 injected、AltGr 假 Ctrl；`SendSas`（依赖被控端服务，见 G-unattended） | 5–7 天（SAS 另 3–5 天，视服务形态） |
| 7 | macOS 捕获 | CGEventTap、权限检测与引导、tap 被禁用后恢复、媒体键 `NSSystemDefined`；Secure Input 提示 | 5–7 天 |
| 8 | Linux 捕获 | X11 `XGrabKeyboard`；Wayland `keyboard-shortcuts-inhibit`（自己接 `wayland-client`）；不支持时的提示 | 4–6 天 |
| 9 | 媒体键等特殊键 | Consumer 页协议、选项开关、三平台注入 | 3–4 天 |
| 10 | e2e 与发布检查表 | Xvfb + XI2 校验器、布局/卡键/文字用例、指标入门槛；三平台手工验收清单 | 4–5 天（与上面并行一部分） |

合计约 5–7 人周（单人串行）。**优先顺序**：1 → 3 → 2 → 6/7（用户最先撞墙的是 Win 键和 Cmd+Tab）→ 4 → 5 → 8 → 9 → 10 贯穿。第 1、2、3、10 可以交给子代理（Sonnet）做，我审查；6–8 的系统接口代码由我自己做。

### 6.3 待产品负责人确认

1. 捕获模式默认开还是关？（建议：窗口聚焦且全屏时默认开，窗口化时默认关，菜单一键切换。）
2. 跨系统默认是否自动启用“语义映射”？（建议：是，并在状态栏显眼提示。）
3. 媒体键默认给本机还是被控端？（建议：本机。）
4. Windows 被控端 Ctrl+Alt+Del 依赖 SYSTEM 服务，是否把它列为服务形态的硬性要求？（与 G-unattended 一起定。）

---

## 附：本仓库现状速查 [已核实]

- `crates/core/src/input.rs`：`InputEvent::{Key{code(HID),down}, MouseMove, MouseRelative, MouseButton, ..., Text(String), ReleaseAll}`。
- `crates/platform/src/keymap.rs`：HID → evdev / X11 键码 / Windows 扫描码 / macOS kVK 的物理键位表，`x11_keysym(ch)` 供文字输入。
- `crates/platform/src/{x11,windows,macos}.rs`：XTEST；`SendInput`（扫描码 + `KEYEVENTF_UNICODE`）；`CGEvent`（键码 + Unicode 字符串）。
- `crates/client/src/gui/keys.rs`：`egui::Key → HID`，优先物理键位；修饰键由 `Modifiers` 状态变化合成，只有“左”侧，GUI 键只来自 `mac_cmd`（即 Windows/Linux 客户端的 Win/Super 键目前发不出去）。
- `crates/client/src/gui/input.rs`：`F12` 是叠层开关；IME 组字期间不发按键；`Commit` 发 `Text`；`Event::Text` 被丢弃（死键/Compose 的漏洞来源）；焦点丢失发 `ReleaseAll`。
