# 剪贴板“粘贴时才传”（2026-11-26）

## 1. 问题

- What：产品负责人实测，在控制端复制文件，鼠标移到远程窗口、还没粘贴，远程就卡。
- Why：`crates/net/src/clipsync.rs` 每 100 ms 看一次本机剪贴板，一变就读盘、打包、整份传给对方
  （文件最多 20 MB）。这条流和画面的反馈、输入走同一条连接、同一个上行，没有限速。
  用户只是复制，可能根本不粘贴，流量已经花掉了。
- How：复制时只告诉对方“有什么”，粘贴时才传数据。这也是 RDP 剪贴板重定向的做法（见第 5 节）。

## 2. 已实现：控制端复制 → 被控端粘贴

对方声明能力位 `Caps::CLIPBOARD_LAZY`（第 30 位）时启用；对方是老版本时和原来一样。

| 时机 | 做什么 |
| --- | --- |
| 控制端复制任何内容（文字、图片、文件） | 不读盘、不编码、不发数据；只发一条 `ClipMsg::Held`（几个字节）。文字原来有“64 KB 以内马上同步”的例外，产品负责人定了去掉（2026-11-26） |
| 在远程窗口按粘贴键（Ctrl/Command+V、Shift+Insert） | 这一下按键扣住；本机有还没给对方的内容就现在读出来，限速传 |
| 传输期间 | 鼠标、其他按键照常发；工具条显示“正在把 3 个文件（12 MB）传给对方，已传 40%，传完自动粘贴”和“取消” |
| 对方写进剪贴板、回 `ClipMsg::Applied` | 发 `InputEvent::Paste`，被控端原子地按一次用户当时按的组合键 |
| 取消、读不出来、太大、对方写不进去 | 这次粘贴不执行（不粘贴出旧内容），工具条说原因和办法 |
| 同一份内容再粘贴 | 剪贴板没变：不读、不传；变了但哈希相同：读了也不传 |

几个决定：

- **推迟的粘贴按用户当时的组合键执行，不按被控端的系统换键。** 任务原来的设想是“被控端按自己的系统按
  Ctrl+V 或 Command+V”。没有这样做的原因：快捷键是一一对应的（Mac 上 Command+V 发过去是 Win+V），
  如果只在“推迟”时换成被控端的粘贴键，同一个按键推迟和不推迟的效果就不一样；Ctrl+Shift+V
  （终端里的粘贴、无格式粘贴）也会被换成 Ctrl+V。所以 `InputEvent::Paste` 带着修饰键和键码，
  被控端先抬起现在按着的其他修饰键、按下这些修饰键、敲键、再恢复（`KeyState::paste`）。
- **粘贴键马上能发时原样发**（没有要传的、键还按着、修饰键没变）：按住 Ctrl+V 连续粘贴之类的行为不变。
- **限速**（`crates/net/src/throttle.rs`）：控制端的上行没有现成的带宽估计（媒体层的估计在被控端，
  量的是画面方向；QUIC 窗口平时量到的是输入和反馈的几十 kbps）。所以从 1 Mbps 起步，每 200 ms 翻倍，
  上限 32 Mbps；往返时间比见过的最小值高出一倍且至少 40 ms 时，认为当前速率就是容量，上限定为它的一半，
  60 秒后过期重探。思路取自 LEDBAT（RFC 6817：后台流量看排队延迟让路），没有照它的公式实现。
  **这些常数没有在真实慢链路上测过**，只有单元测试和回环上的会话测试。
- **优先级**：剪贴板流 `set_priority(-1)`，低于控制流和文件流。数据报（画面、输入）在 quinn 里本来就先于
  所有流的数据（quinn-proto 0.11.19 `connection/mod.rs` 组包时先写 DATAGRAM 再写 STREAM）。
  但拥塞窗口是共用的，所以优先级不能代替限速。

## 3. 系统的延迟提供数据：任何程序来取时才传（2026-11-26 实现，Windows 和 macOS）

只靠“在远程窗口按粘贴键”有两个缺口：被控端上右键菜单的“粘贴”拿不到控制端刚复制的内容；
被控端复制的内容到不了控制端（粘贴发生在控制端本机的程序里，我们收不到按键）。
补法和 RDP 一样：收到对方的 `Held` 以后，在本机系统剪贴板上放一个**承诺**（只声明格式，不放数据），
本机任何程序来取时系统回调我们，我们再向对方要。两个方向用同一套逻辑。

### 3.0 已经实现的

| 环节 | 做法 | 代码 |
| --- | --- | --- |
| 协议 | `Held` 带项数；`Want`（来要数据，相当于 RDP 的 Format Data Request）；`Unavailable`（给不了）；`Same`（和上次发的一样，接着用上一份，不重传） | `core/src/clipboard.rs` |
| 逻辑 | 收到 `Held` 放承诺；程序来取时发 `Want`，等数据（最多 10 秒），取到的留着重用；本机复制了新东西承诺作废 | `net/src/clipsync.rs`（`Remote`、`Promise`） |
| 取数和格式转换 | 文字转 UTF-16、图片转 `CF_DIBV5`、文件解到临时目录再给 `DROPFILES` 或 `file://` 网址 | `platform/src/promised.rs`（有单元测试） |
| Windows | 延迟渲染：只收消息的窗口加消息循环，处理 `WM_RENDERFORMAT`、`WM_RENDERALLFORMATS`、`WM_DESTROYCLIPBOARD` | `platform/src/clipboard_win.rs` |
| macOS | 每项一个 `NSPasteboardItem`，数据提供者实现 `pasteboard:item:provideDataForType:` | `platform/src/clipboard_mac.rs` |
| Linux（X11） | **没有做**。放承诺返回“不支持”，退回上一步的行为：控制端 → 被控端只有在远程窗口按粘贴键时才传；被控端 → 控制端不同步，工具条提示 | 见 3.3 |

和“按粘贴键”的配合：在远程窗口按粘贴键时仍然先把内容传过去、对方确认以后再执行粘贴
（有进度、能取消、不让对方的程序干等）；对方有承诺时内容不写进它的剪贴板，而是留给来取的程序，
按键到了程序一取就有。右键菜单“粘贴”走系统回调。

系统回调里的限制（各系统都一样）：**回调期间来粘贴的那个程序在等**，所以等数据有 10 秒上限
（`FETCH_TIMEOUT`）。超时以后给系统空结果，工具条提示“等了 10 秒还没传完……传输还在继续，
过一会儿再粘贴一次”；传输不会停，传完以后再粘贴直接拿到。限速从 1 Mbps 起步（见第 2 节），
回环上 3 MB 约 1.5 秒；慢链路上几 MB 的内容第一次经右键粘贴可能超时，快捷键粘贴没有这个问题。

没有做的：
- **Windows 文件没有用 `CFSTR_FILEDESCRIPTORW` / `CFSTR_FILECONTENTS`**，用的是延迟渲染 `CF_HDROP`：
  资源管理器来取时把文件全部收到临时目录，再给路径。差别：资源管理器在回调里干等（最多 10 秒），
  没有系统的复制进度；10 秒传不完的文件第一次粘贴会失败。`FILECONTENTS` 要实现 OLE 的
  `IDataObject` 和 `IStream`（见 3.1），估计 1.5–2 周，而且没有 Windows 真机没法调，这次没有做。
- 接收的一侧没有进度条（发送的一侧有）。
- Linux。

### 3.0.1 要用 CI `[win][mac]` 和真机验证的清单

Linux 上验证过的：协议、同步逻辑、取数和格式转换（单元测试和真实 QUIC 会话测试，系统剪贴板用假的）；
`clipboard_win.rs` 和整个平台层对 `x86_64-pc-windows-gnu` 通过了 `cargo clippy -D warnings`
（只检查，没有链接、没有运行）；`clipboard_mac.rs` 单独对 `aarch64-apple-darwin` 通过了同样的检查。

没有验证过的：
1. `clipboard_sys.rs` 里 macOS 的分支（`promise`、`ours`、`has_image`）没有编译过。
2. Windows：放承诺以后记事本、资源管理器、Office 里 Ctrl+V 和右键“粘贴”能不能取到文字、图片、文件；
   `CF_DIBV5` 的字节布局各程序认不认（照 arboard 3.7 写的）；`PNG` 格式。
3. Windows：`ExcludeClipboardContentFromMonitorProcessing` 是否真的让剪贴板历史（Win+V）不在复制时来取。
   如果它还是来取，就等于复制时就传了。
4. Windows：被控端以服务身份运行、或者在登录界面时，只收消息的窗口能不能当剪贴板所有者。
5. macOS：回调在哪个线程上来；被控端没有界面主循环时回调会不会来。
6. macOS：Finder 对延迟提供的 `public.file-url` 是否照常粘贴（见 3.2）；多个文件时是否每项各来取一次。
7. macOS：数据交给系统以后 `changeCount` 变不变（代码两种情况都处理了，但没有实测）。
8. 两个系统：剪贴板管理工具、其他远程软件在复制时就来取的情况有多普遍。
9. 本机的观看窗口自己读剪贴板（egui 在按 Ctrl/Command+V 时读文字）会触发一次取数：
   对方复制的是文字时，在远程窗口按粘贴会顺带把对方的文字取到本机（很小，但界面线程在等）。

### 3.1 Windows

原文（Microsoft Learn, *Clipboard Operations*, Delayed Rendering 一节，
<https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-operations>）：

> To do so, an application can specify NULL for the hData parameter of the SetClipboardData function.
> … The system sends the clipboard owner a WM_RENDERFORMAT message when a request is received for a
> specific format that has not been rendered. Upon receiving this message, the window should call the
> SetClipboardData function to place a global memory handle on the clipboard in the requested format.

> An application must not open the clipboard before calling SetClipboardData in response to the
> WM_RENDERFORMAT message.

> If the clipboard owner is about to be destroyed and has delayed rendering some or all clipboard
> formats, it receives the WM_RENDERALLFORMATS message.

剪贴板序号在渲染时才变（`GetClipboardSequenceNumber` 的说明，
<https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getclipboardsequencenumber>）：

> If clipboard rendering is delayed, the sequence number is not incremented until the changes are rendered.

所以判断“剪贴板里还是不是我们的承诺”看的是所有者窗口，不是序号。

不让剪贴板历史在复制时就来取（*Clipboard Formats*，
<https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats>）：

> ExcludeClipboardContentFromMonitorProcessing: Place any data on the clipboard in this format to prevent
> all clipboard formats being included in the clipboard history or synchronized to the user's other devices.

同一页的代价说明，对我们很关键：

> With delayed rendering, if the data is eventually needed, the window must render the data while
> processing a rendering window message … if the data is very time-consuming to render, the application
> may become visibly unresponsive (hung) while rendering occurs … perhaps providing progress or
> cancellation options, which are not available when using delayed rendering.

- 文字、图片：`SetClipboardData(CF_UNICODETEXT / CF_DIBV5, NULL)`，在 `WM_RENDERFORMAT` 里向被控端要数据、
  等它到齐再 `SetClipboardData`。要一个自己的隐藏窗口当剪贴板所有者，消息循环在专门的线程上，
  处理 `WM_RENDERFORMAT` 时阻塞等网络。**阻塞的是来粘贴的那个程序**（它在 `GetClipboardData` 里等），
  20 MB 走慢链路时它会“未响应”几十秒，系统不给进度和取消；我们只能在自己的窗口里显示进度。
- 文件：`CF_HDROP` 是“已经存在的文件的路径”（原文：*used when transferring the locations of a group of
  existing files*），做不到延迟（除非延迟渲染 `CF_HDROP`，在回调里把文件全部下载到临时目录再给路径，
  效果同上：资源管理器卡住）。正路是 `CFSTR_FILEDESCRIPTOR` 加 `CFSTR_FILECONTENTS`
  （<https://learn.microsoft.com/en-us/windows/win32/shell/clipboard>）：

  > These two formats are the preferred way to transfer Shell objects that are not stored as
  > file-system files.

  > The file is normally represented as a stream object, which avoids having to place the contents of
  > the file in memory. In that case, the tymed member of the STGMEDIUM structure is set to
  > TYMED_ISTREAM … the target … specifies a particular file by setting the lindex member of the
  > FORMATETC structure to the zero-based index of the file's FILEDESCRIPTOR structure.

  这要走 OLE 剪贴板：实现 `IDataObject`（`GetData`、`QueryGetData`、`EnumFormatEtc`）和 `IStream`，
  用 `OleSetClipboard` 放上去；资源管理器粘贴时逐个文件 `GetData(CFSTR_FILECONTENTS, lindex=i)`，
  从我们的 `IStream::Read` 里读，读多少我们向被控端要多少。资源管理器自己显示复制进度、可以取消。
  这就是 RDP 客户端的做法（第 5 节）。
- arboard 3.7.0 没有延迟渲染的接口（读了 `src/platform/windows.rs`：`Set::text/image/file_list`
  都是直接 `SetClipboardData` 放数据），所以自己写了 `clipboard_win.rs`（文字、图片、`CF_HDROP`）。
- 剩下的工作量：文件（`IDataObject` + `IStream` + 协议里加“按文件、按范围要数据”）约 1.5–2 周，另加真机调试。
  风险：`IStream` 的读取在资源管理器的线程上同步进行，网络断了要能让它尽快失败。

### 3.2 macOS

原文（Apple Developer Documentation，
<https://developer.apple.com/documentation/appkit/nspasteboarditemdataprovider>）：

> A set of methods implemented by the data provider of a pasteboard item to provide the data for a
> particular UTI type. … The data provider must implement this protocol to provide data upon request.

`pasteboard(_:item:provideDataForType:)`：

> Asks the receiver to provide data for a specified type to a given pasteboard.

`NSFilePromiseProvider`（<https://developer.apple.com/documentation/appkit/nsfilepromiseprovider>）：

> A file promise is a possible future file of a specified type. When you're working with drag and drop,
> use promises to indicate intent for future action. … The NSFilePromiseProvider will write the
> promised file to the destination directory.

- 文字、图片：已实现（`clipboard_mac.rs`）。`NSPasteboardItem.setDataProvider(_:forTypes:)`，
  在 `provideDataForType` 回调里向对方要数据再 `setData`。和 Windows 一样，回调期间来粘贴的程序在等。
- 文件：已实现的是“延迟提供的 `public.file-url`”：每个文件一个剪贴板项，被取时把文件全部收到临时目录，
  再给 `file://` 网址（Finder 等着，没有系统进度）。没有用 `NSFilePromiseProvider`：它的原文说的是拖放
  （*When you're working with drag and drop*），**Finder 的 Command+V 接受哪一种，原文没有说，
  我也没有在真机上验证**；选 `public.file-url` 是因为 Finder 自己复制文件时放的就是它。
  真机上如果 Finder 不认延迟提供的 `public.file-url`，再试文件承诺。

### 3.3 Linux（X11）

X11 的选区天生是延迟的：复制只是声明所有权，数据在别人来要时才给。原文（ICCCM 第 2 节，
<https://tronche.com/gui/x/icccm/sec-2.html>）：

> Selection owners are required to support the following targets. … TARGETS - The owner should return a
> list of atoms that represent the targets for which an attempt to convert the current selection will
> succeed

> Selection owners should transfer the data describing a large selection (relative to the
> maximum-request-size they received in the connection handshake) using the INCR property mechanism

- 机制上最容易：收到 `SelectionRequest` 时再向被控端要数据，大内容用 INCR 分段写。
- 现状：arboard 的 X11 实现自己持有选区、在后台线程里应答请求，但只能应答已经放进去的数据，
  没有“应答时再取”的钩子。要用 x11rb（平台层已经依赖）自己当 CLIPBOARD 的所有者。
- 文件：目标是 `text/uri-list`（已存在文件的路径），和 `CF_HDROP` 一样做不到流式，
  只能在应答前全部下载到临时目录；请求方通常有几秒的超时（各工具包不同，没有查证具体值）。
- Wayland：`wl_data_source` 的 `send` 事件给一个文件描述符让我们往里写，也是延迟的；
  我们目前在 Wayland 上的剪贴板支持情况没有查。
- 工作量估计：X11 文字和图片约 1 周（选区协议、INCR、和 arboard 并存或者替换它）。

### 3.4 协议还要加什么（做文件流式传输时）

- 现在的 `Held` 只有种类和项数。按文件、按范围取数据（`CFSTR_FILECONTENTS`）需要文件名、大小、
  修改时间的列表，以及“要某个文件的某一段”的消息。
- 被控端有媒体层的带宽估计，被控端 → 控制端方向的限速可以直接用“估计的一半”，现在两个方向都是自己探。

### 3.5 接下来的顺序

1. 用 `[win][mac]` 和真机把 3.0.1 的清单过一遍。
2. Windows 文件改成 `IDataObject` + `CFSTR_FILECONTENTS`（资源管理器自己显示进度、不干等）。
3. Linux（X11）的选区所有者。
4. 接收一侧的进度显示。

## 4. 只在 Linux 上验证过、要真机确认的

- 延迟提供数据的系统代码：见 3.0.1 的清单。
- `crates/platform/src/clipboard_sys.rs` 的 `peek`（只看有没有文件、文字、图片，不读内容）：
  Windows 用 `IsClipboardFormatAvailable`（对 Windows 目标编译检查过，没有运行过），
  macOS 看 `NSPasteboard.types`（没有编译过）。
- 被控端展开 `InputEvent::Paste`：同一时刻连续注入“修饰键按下、V 按下、V 抬起、修饰键抬起”，
  Windows（`SendInput`）、macOS（`CGEvent`）、X11（XTest）上目标程序是否都认。
- 限速的常数在真实慢链路（1–4 Mbps 上行、Wi-Fi 丢包）上的表现；往返时间上涨的判定会不会被画面方向的
  拥塞误触发。
- 工具条的进度和取消按钮（`gui/viewer.rs`）没有界面测试。

## 5. RDP 的做法（对照）

MS-RDPECLIP（<https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpeclip/fb9b7e0b-6db4-41c2-b83c-f889c1ee7688>）
的首页只有一句概述：

> Specifies the Remote Desktop Protocol: Clipboard Virtual Channel Extension, which enables users to
> seamlessly transfer data via the system clipboard between applications that are running on different
> computers.

下面的细节是按记忆写的，**没有对照规范正文核对**：复制时一端发 Format List PDU（只有格式的编号和名字）；
另一端把这些格式以延迟渲染的方式放上本机剪贴板；本机程序粘贴时触发 Format Data Request，
对端回 Format Data Response；文件用 FileGroupDescriptorW 加 File Contents Request/Response
（先问大小，再按偏移和长度分段取），对应第 3.1 节的 `CFSTR_FILEDESCRIPTOR` / `CFSTR_FILECONTENTS`。
