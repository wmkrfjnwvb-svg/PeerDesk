# R. Android 控制端：方案和分步（2026-11-28，产品负责人定先做 Android，iOS 放后）

产品负责人定的路线：**Rust 做底座，上层用原生界面**；Android 和 iOS 共用同一个 Rust 核心。iOS 的调研见
`reports/PeerDesk iOS 客户端和官网方案.md`，这里不重复。

## 0. 目标和闭环验收

先做**控制端**：用安卓手机连自己的电脑，看画面、操作。被控端仍然是桌面版。

闭环验收（产品负责人用自己的安卓手机做一遍）：
1. 装 APK，输入电脑的设备码和密码，连上。
2. 首帧出现。画面清楚，跟手，不比桌面控制端差太多；具体数字在第 5 步量出来再定门槛。
3. 触屏手势（照 ToDesk 的习惯，2026-11-28 产品负责人定）：单指点是左键（点哪是哪），单指滑动平移画面，
   长按震一下后松开是右键、接着拖是拖拽，双指点是右键，双指滑动是滚动，捏合是缩放画面。
4. 虚拟鼠标：默认是可以拖到任何位置的小鼠标（画面外的空白处也行）；点一下变成大鼠标。
   大鼠标顶上的箭头尖就是远程光标；左键（按下就按，按住拖动整只鼠标 = 选中文字）、右键、身体（拖着移动光标）、
   右上角关闭（变回小鼠标）。鼠标外面的手指照常按触屏手势走。位置和展开状态记住。
5. 软键盘能打字。快捷键栏有 Esc、Tab、Ctrl、Alt、Cmd/Win、方向键，Ctrl+C/V 能用。
6. 网络差时工具条写明原因（丢包多少，是本机到路由器还是公网），不弹窗。
7. 切到后台再回来，几秒内恢复画面。Wi-Fi 和移动数据之间切换后会重连。
8. 诊断页能导出日志：首帧时间、解码耗时、帧率、冻结次数，和桌面端同一份格式。

能自动的部分进测试（第 4 节）。

## 1. 结构

```
android/                     Android 工程（Kotlin + Jetpack Compose，Gradle）
  app/                       界面：连接页、会话页（SurfaceView）、快捷键栏、诊断页
crates/mobile/               Rust 核心（编成 libpeerdesk_mobile.so）
  src/lib.rs                 平台无关的接口：Core、Session、事件队列
  src/gesture.rs             触控到鼠标的状态机（纯逻辑，Android 和 iOS 共用，计入覆盖率）
  src/android/jni.rs         JNI 导出函数（系统接口胶水，unsafe 只在这里，每处写 SAFETY）
  src/android/codec.rs       MediaCodec 解码到 Surface（系统接口胶水）
```

### 1.1 关键决定和理由

| 决定 | 理由 | 不这样做会怎样 |
|---|---|---|
| **解码用系统 MediaCodec，直接解到 Surface**（`SurfaceView`），不把像素交给界面 | 零拷贝，硬件解码，系统负责合成。Moonlight 等同类低延迟串流的常规做法 | 解成 NV12 交给界面，1080p 60 帧每秒要过 180 MB，费电、发热、加延迟 |
| **Android 版不带 OpenH264** | 手机系统的解码器由厂商付了专利许可；我们自己带的软件解码没有 Cisco 二进制那层专利保护，也让安装包变大。不支持硬解的老机型明确提示，不静默降级 | 自带 OpenH264 有专利风险，安装包大几 MB |
| **低延迟解码设置**：`KEY_LOW_LATENCY`（API 30 起）、`KEY_PRIORITY=0`，加各厂商的低延迟开关（高通、联发科、三星，Moonlight 用的那组） | 默认 MediaCodec 会攒几帧再出，延迟高几十毫秒 | 跟手变差 |
| **绑定手写 JNI**（`jni` crate），不用 uniffi | 接口约 20 个函数，手写最直接；Surface 必须经 JNI 拿 `ANativeWindow`，uniffi 做不了；uniffi 的 Kotlin 绑定走 JNA，多一层开销；uniffi 是 MPL-2.0 | 两套绑定机制并存 |
| **Rust → Kotlin 用事件队列**：Kotlin 在后台线程调 `nextEvent(超时)` 取事件（状态、质量、错误），不从 Rust 线程回调 Java | 不用在 Rust 线程上挂接 JVM，生命周期简单；事件频率低（每秒一次质量） | 回调要处理线程挂接、异常、对象引用，容易泄漏 |
| **手势识别放在 Rust**（`gesture.rs`），Kotlin 只转发原始触点（编号、坐标、按下/移动/抬起、时间） | 纯逻辑，能单元测试、计入覆盖率；iOS 以后直接复用 | 两个平台各写一套，行为不一致 |
| **最低 Android 10（API 29）**，目标 API 36 | 低延迟解码的设置要 API 30，API 29 上照常能用，只是少一个开关；Android 10 以上覆盖绝大多数在用的手机（推断，没查最新份额） | — |
| 画面尺寸：连接时告诉被控端手机的显示区域（已有的 `ControllerOptions::view`），被控端按这个大小编码 | 手机屏幕小，不用传 4K 再缩小，省流量 | 白传大画面 |

### 1.2 对现有代码的改动（都在 Linux 上能测）

1. `peerdesk-net` 能编到 Android：
   - `ring` 等 C 代码要 NDK 的编译器；
   - `portmap`、`gateway` 里只认 `target_os = "linux"` 的地方，Android 是 `target_os = "android"`，要逐个看用 Linux 的实现还是不做。
2. `ControllerOptions` 加“解码器从哪来”：默认是现在的 `AutoH264Decoder`；Android 传 MediaCodec 解码器。
3. `Picture` 加一种“已经画到系统显示面上了”（只带尺寸），控制端照常统计帧率、冻结，界面不用画。
4. `peerdesk-media` 的 OpenH264 改成可选（feature），Android 不编它。桌面版行为不变。

## 2. 分步（每步能单独合进 dev）

| 步 | 做什么 | 验收 | 工作量（推断） |
|---|---|---|---|
| A0 | 工具链：`scripts/android-setup.sh`（命令行工具、平台 36、编译工具 36.0.0、NDK r29、Rust 的 Android 目标）；`peerdesk-core`、`peerdesk-net` 编到 `aarch64-linux-android` | 两个 crate 在 Android 目标上编过；桌面版测试全过 | 0.5–1 天 |
| A1 | `crates/mobile` 平台无关部分：`Core`（身份、服务器设置）、`Session`（按设备码和密码连接、断开、事件队列、输入）、`gesture.rs` 手势状态机；第 1.2 节第 2、3、4 条的改动 | 单元测试覆盖手势表的每一条；**Linux 上的闭环测试**：起真实的服务器和被控端进程，`mobile::Session` 按设备码连上，收到首帧、连续解出帧，发出的点击和按键被被控端收到（被控端在测试构建里只记录、不注入） | 2–3 天 |
| A2 | `android/codec.rs`：MediaCodec 解码到 Surface，低延迟设置，出错时请求关键帧；`android/jni.rs` 导出函数 | 能编进 `.so`；解码逻辑里不碰系统接口的部分（SPS/PPS 提取、出错后的状态）单元测试 | 1–2 天 |
| A3 | Android 界面（Kotlin + Compose）：连接页、会话页、快捷键栏、工具条（延迟、帧率、码率、丢包、原因）、诊断页导出日志 | Gradle 在 Linux 上编出 APK；Compose 界面的逻辑用 JVM 上的测试跑（不需要模拟器） | 2–3 天 |
| A4 | CI：提交信息带 `[android]` 时编 APK 并上传（保留期短）；`[pkg]` 也带上 APK | CI 上编出 APK | 0.5 天 |
| A5 | 真机闭环：产品负责人装 APK，按第 0 节走一遍；把诊断日志交回来，定指标的初始门槛 | 第 0 节 8 条 | 你测 |

合计约 1–1.5 周到第一个能用的闭环（推断）。

## 3. 测试和指标

- **Linux 上能自动跑的**（进 CI）：A1 的闭环测试（真实服务器和被控端进程，画面是真实录屏素材，不是合成画面）；手势状态机；事件队列；Kotlin 界面逻辑的 JVM 测试。
- **只能真机测的**：MediaCodec 的解码延迟、发热、耗电、各厂商的低延迟开关是否生效。这个环境没有 KVM，跑不了模拟器；模拟器的解码本来也不能代表真机。
- **指标**：首帧时间、每帧解码耗时（MediaCodec 送入到出帧）、显示帧率、冻结次数、操作延迟（点下到画面变化）。A5 量出来后定初始门槛，只升不降。

## 4. 和 CLAUDE.md 的关系（要产品负责人确认）

1. **“只发两个二进制”**：手机 App 是另外的发行物（APK，以后 iOS 的 App），不算第三个桌面二进制。建议在 CLAUDE.md 里写明“桌面和服务器只发两个二进制，手机端另有 App”。
2. **`unsafe` 的范围**：JNI 和 MediaCodec 是系统接口，放在 `crates/mobile/src/android/` 两个文件里，每处写 `// SAFETY:`，和平台层、媒体层同样的规矩；覆盖率按 `scripts/coverage.sh` 的规则排除这两个文件并写理由。
3. **新提交标记 `[android]`**：只编 Android 的 APK。
4. **许可证**：
   - 我们用到的都是 MIT 或 Apache-2.0：`jni`、`ndk` 这两个 crate，Kotlin、Jetpack Compose、AndroidX、Gradle；
   - 不引入 GPL、AGPL；
   - Google 的 Android SDK 和 NDK 是工具链，有它自己的许可协议，不进我们的产品代码。

## 5. 要产品负责人做的

1. **接受 Android SDK 许可协议**：装 SDK 和 NDK 要先接受 Google 的 Android SDK 许可协议。脚本不替人接受，需要你同意后我再装（或者你自己执行脚本里提示的命令）。
2. **一台安卓手机**：最好 Android 10 以上；打开“允许安装未知来源应用”装测试版 APK。
3. 以后上 Google Play 要开发者账号（一次性 25 美元，推断，没核实当前价格）和签名密钥，测试阶段用调试签名就够。

## 6. 风险和没核实的

- 各厂商低延迟开关的键名凭对 Moonlight 源码的记忆，A2 动手前去读原文核对。
- `ring` 在 NDK r29 下编译是否顺利没试过（预计可以，quinn 和 rustls 在 Android 上有人用）。
- 切网（Wi-Fi 和移动数据之间）时 QUIC 连接迁移：现在的代码遇到断开会重连，迁移不做；先按重连验收，再看要不要做迁移。
- 后台：Android 切到后台后网络和解码会被限制，第一版切后台就暂停画面（告诉被控端停发），回来时请求关键帧。

## 7. 进度（每完成一步更新）

| 步 | 状态 |
|---|---|
| A0 | 完成：SDK 平台 36、编译工具 36.0.0、NDK r29 装好（`scripts/android-setup.sh`，许可协议由产品负责人同意）；`scripts/android-env.sh` 交叉编译环境；`peerdesk-net`（带 media、不带 OpenH264）编到 `aarch64-linux-android` 通过 |
| A1 | 完成：控制端驱动挪进 `peerdesk-net::viewer`，桌面改用它（行为不变，测试全过）；OpenH264 改成可选；解码器可由调用方提供；`crates/mobile`：`Session`（事件队列、输入、显示区域、断开）、`gesture`（手势表 8 个测试）；**Linux 闭环测试**：真实 QUIC、真实被控端会话，手机核心连上、连续出画面、点击按顺序到被控端、密码错误给原因（0.5 秒跑完）；`libpeerdesk_mobile.so` 编到 arm64 |
| A2 | 完成（真机待测）：`android/codec.rs` MediaCodec 解到 Surface（先默认解码器、再系统软件解码器；低延迟设置分 6 档，`configure` 失败退一档，键名按 Moonlight 核对过）；`android/bridge.rs` 11 个 JNI 函数（开始、取事件、触点、长按计时、画面位置、触控模式、按键、文字、松开全部、断开、释放）；`Picture::Presented`；按键码到 HID、SPS/PPS 提取、低延迟档位都是纯逻辑有测试；`libpeerdesk_mobile.so` 2.7 MB，不含 OpenH264 |
| A3 | 完成（真机待测）：`android/` Gradle 工程（Gradle 8.14.3、AGP 8.13.2、Kotlin 2.3.21、Compose BOM 2026.02.01）；连接页、会话页（SurfaceView 在裁剪容器里改尺寸来缩放平移）、工具条（延迟、帧率、码率、丢包和原因）、快捷键栏（修饰键按一下锁住）、软键盘、外接键盘；`scripts/android-build.sh` 一条命令编出 APK（debug 33 MB）；40 个 JVM 单元测试。服务器名的 DNS 改到会话线程里查，不卡界面；被控端按画面实际画出的大小编码（放大时更清楚） |
| A3 真机反馈 | 产品负责人 11-28 实测能连上、能看、能点。修了：句柄按位放进 long（Android 11 起堆指针带标记）；密码显示开关；记住设备码和密码（Keystore 加密）。手势按 ToDesk 改（`gesture.rs` 开头的表，14 个测试），加虚拟鼠标（`MouseAssist.kt`：按手指按下的位置分流，19 个 JVM 测试）；JNI 加 `pointAt`、`button`，去掉触控模式。画面矩形没变时不再发 `setView`/`pointAt`（否则触屏拖拽中途来一个质量事件，选区会跳到箭头尖）。JVM 测试共 60 个。真机待测：鼠标尺寸（大 96×180 dp、小 48 dp）、手感 |
| A4、A5 | A4（CI 出 APK）和 A5（真机闭环）下一步 |

