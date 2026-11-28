# 交接：所有进行中的工作和下一步（2026-11-28 17:20 UTC，主会话写）

会话没了也能接上：每一项写了分支、最后的提交、做到哪、下一步。决定和方案的详细内容在各自的文档里，这里只是索引。
分工照 CLAUDE.md：主会话调度、审查、合并、做产品；diskagent 会话做仿真框架；指纹（`crates/e2e/golden/fingerprints.txt`）只由主会话改。

## 1. 已经合进 dev 的（2026-11-28）

| 项 | 提交 | 说明 |
|---|---|---|
| #71 帧率、码率不稳修完 | 7c56f8c（[pkg]，CI 过，安装包给产品负责人测） | `P-fps-bitrate.md` 第 10 节；固定基线 `data/ms-traces-baseline-2026-11-28.md` |
| golden 补 ms-02651 十二行、真实日志节奏测试的调试构建门槛 | 81b5bfb（CI 过） | |
| 账户和订阅调研 | f14bfbf | `O-accounts-research.md`；胶水代码定 Cloudflare Workers + Rust |
| 第一阶段计划（付钱和留存，P1–P10，含 1.1 同账户免密） | 0330976、705ef55 | `O-phase1-plan.md` |
| 补齐标配的优先计划（S1–S8、G1–G4）和 S1 连入确认的设计 | 98f68ce、8d3bf8c | `O-standard-plan.md` |
| 调研：同类产品周边功能、Agent 远程操作（Jump）、隐私屏和虚拟显示器 | a4fabad、bef2823、c2e8b80 | `reports/PeerDesk 同类产品周边功能对照.md`、`industry-agent-remote-control.md`、`industry-privacy-virtual-display.md` |
| CLAUDE.md：常规决定主会话自己定；Mac 版官网分发不上 Mac App Store；Windows 虚拟显示器驱动走 attestation 签名 | 05ede3c | |

## 2. 进行中

### 2.1 Android 控制端（主会话）——**已合进 dev**（57b9fc2，后续 ab79991：中文输入、手机诊断日志、截图测试）

- 方案和进度：`R-android.md`（第 7 节进度表，真机反馈和根因都写在里面）。
- 合进 dev 的：`peerdesk-net::viewer`（桌面和手机共用的访问驱动）、OpenH264 可选、`crates/mobile`（会话、手势状态机、按键映射、MediaCodec 解到 Surface、JNI）、
  `android/` Kotlin + Compose 界面（连接页、会话页、虚拟鼠标、点击水波、软键盘让位、会话全屏、返回键按两次、手势记录导出）、CI 的 `android` 任务（`[android]` 标记）。
- 真机记录回放（`crates/mobile/tests/trace_replay.rs`，两份产品负责人的记录）：滚动 25 次、捏合 26 次全对；修过的根因：系统取消只带一根手指时卡死、
  两指上下叠着滚动时一根比另一根早动 40 多毫秒被当成捏合。
- 合并的 CI 带 `[mac]`（桌面控制端改用共用驱动、OpenH264 改可选，先在 macOS 上验证），过了再推一次 `[win]`。
- 下一步：等产品负责人测 4d3f302 的 APK；诊断页（首帧、解码耗时、帧率、冻结）、切后台不断开；iOS 以后。
- 构建：`source scripts/android-env.sh`；SDK 在 `~/android-sdk`（`scripts/android-setup.sh` 装，许可协议产品负责人已同意）。新容器要重新装。

### 2.2 仿真框架 A2-4（diskagent 会话）——分支 `sim-a2`

- **A2-4a（多会话共享瓶颈）和 A2-4b（NR-BPDV 抖动、TCP SACK）已审过、合进 dev**（本次合并，sim-a2 38fa76c）。golden：带 TCP 交叉流的 3 行改了、A2-4a 新增 `zero` 一行（154 行）。
  - A2-4a 审出的 `Shared::pop` 提前送达已修（断言又抓到同函数第二个洞，一起修）。
  - SACK 让量尺更接近真实，带长 TCP、A 家上传丢尾的场景变差（最长等待 410→790、657→988 ms）：是产品问题（和 TCP 抢带宽，B 阶段），不是仿真的错。
  - 两条同 RTT 的 TCP 收敛到 0.9995–1.0005：丢尾同步丢包，比真实规整；A3 给交叉流随机的开始时刻和确认抖动。
- A2-4c（RFC 8867 用例，360p 主报告 + 720p 5.1 补充表）在跑 12 个种子。11-28 的发现：
  - 720p udp-50 宽段偏低是内容上限（1002 kbps），按“稳定÷min(公平, 上限)”是 0.91。
  - **5.2 窄段一路被饿死的机制**：不是重传，是“常驻队列”规则（没用满码率时，100 ms 窗口里排队读数的最小值 > 12 ms 就降）
    在另一路的周期性突发上锁相采样：大流每帧在 2 Mbps 上要排约 19 ms，小流每帧一个包总落在同一位置，把别人的突发当成常驻队列。
    udp（4 Mbps，突发约 9.5 ms，低于门限）不饿死。已登记进已知产品问题和 a3-plan §11，不改算法。
  - 是不是仿真假象要先修量尺：仿真里编码耗时为 0、帧严格按时钟出发。diskagent 在加“帧出发时刻”层（逐帧编码耗时分布，默认关、指纹不变），
    再扫相位；另加“我们 + 30 fps 周期突发的视频通话流”用例（同一机制更常见的形态）。真机逐帧数据从被控端 `records/*.pdsr` 来（默认开）。
- B 阶段“帧大小和重传时机按链路送一帧要多久缩放”有三个验收场景：5.1 窄段低利用率、4G 虚假重传、5.2 窄段饿死；两条规则（NACK 定时、常驻队列门限）都要看。

### 2.3 账户设备表（主会话）——分支 `devices-s1`，最后 4acff3a

- `devices` 分支 rebase 到 dev 之后推成了新分支 `devices-s1`（原来的 `devices` 不能强推）。S0、S1a–S1d，测试和 lint 都过。
- 下一步：O-phase1-plan P1 的两处改动——账户号改成 16 字节（老的 8 字节补零，UUID 第 9 字节是变体位、不会重号；令牌、账本、中继票、心跳的兼容做法见本次会话的设计：老账户号仍写 16 位十六进制）；入表令牌改成绑定账户、默认一次性、1 小时。然后合进 dev。

### 2.4 终端、文件、界面的打磨（主会话，2026-11-28 产品负责人提）

- 计划和标准：`N-terminal-files-ui-plan.md`（文件像 Windows 资源管理器、终端到 iTerm2 一档、界面照国外最好的做各种状态；文件和拷贝合成一个传输引擎；观看模式以后加；不做帧率设置）。
- 调研：`industry-terminal.md`、`industry-file-manager.md`（带我们代码缺口的行号）、`industry-ui-reference.md`、`industry-lowlatency-hevc-avc444.md`（编解码，v1 之后）。
- 审查：`U1-review-desktop.md`、`U1-review-android.md`（问题表按严重度、验收各 8 条）。
- 截图：桌面端按 CI 的 gui-screenshot 在 Xvfb 下跑；Android 用 `android/screenshots.sh`（Paparazzi，256 张，PNG 不进仓库）。
- **进行中**：
  - U2-a（子代理，分支 `ui-u2a`）——默认官方服务器（桌面和手机）、结构化的失败原因（核心给原因代码，两端翻成原因加办法，英文原样进“详情”，文件传输失败不再是调试输出）。
  - F1（子代理，分支 `files-f1`，4088a02 已审）：冲突（`Exists`/`Resolve`，替换、跳过、都保留 `a (2).txt`，默认不静默覆盖）、取消（`Cancel`，两端 0.15 s 内返回、删 `.part`）、
    符号链接不跟随、坏名字只拒绝那一个、失败带原因和办法（`F1-file-errors.md`），能力位 `FILE_CONFLICT`（第 14 位），老对端字节不变。
    在补：下载时被控端跳过的项传到控制端（`Skipped`）、不区分大小写的系统上同名自动改名、客户端 crate 编译和 lint。合并顺序：U2-a → F1 → 跑一次全门槛。之后 F1b 冲突对话框和结果文案。
  - T1（子代理，分支 `term-t1a`）：设计 `T-terminal-design.md`。T1a（`crates/term`：协议、快照和逐格标准答案、环形缓冲、背压状态机、会话表，覆盖率 99.59%）已审；
    T1b 在做（PTY 胶水、被控端流处理、权限默认关、审计日志、Ack、控制端接口、重连和 100 MB 测试）。T1a、T1b 一起合进 dev。之后 T2a/T2b 桌面终端控件（Opus）。
- 剪贴板重做分 C1（跨平台按需取，修“一复制就卡”）、C2（系统延迟提供）、C3（Windows 文件流式），见 N 计划“文件和拷贝是一件事”。在 F1 之后。
- 接下来：U2-b Android 状态（连接中步骤、重连压暗加横幅、三格信号、横屏自动隐藏、配色和对比度、手势提示）；U2-c 桌面“状态截图台”和修。
- 磁盘：三个子代理同时编时只剩几百 MB（一次全工作区覆盖率要 9 GB）。规矩：大构建前 `df` ≥ 8 GB；测试要自己删临时目录（不少老测试不删，`/tmp/peerdesk-*` 会攒上万个）。

## 3. 排队（按 `O-standard-plan.md` 的顺序）

1. Android 真机反馈继续（2.1），诊断页、切后台不断开。
2. S1 连入确认（设计已定，见 `O-standard-plan.md` 2.1）。
3. S2 多显示器、S3 Mac 和 Linux 无人值守、S6 唤醒和重启连回、S7 会话内开关、S8 文件权限（S7、S8 可以派 Sonnet）。
4. 账户线 P1–P3b（本地用自己生成的密钥测，不等 Supabase）。
5. Agent 功能 G1–G4（工具分层草案在 `industry-agent-remote-control.md`）。
6. 隐私屏、虚拟显示器（做法在 `industry-privacy-virtual-display.md`；Mac 虚拟屏要无显示器的 Mac mini 实测）。
7. “停下后没变清晰”在微软轨迹上很普遍（11-28 基线 144 次里 111 次）：和 #79 补清晰策略一起查。
   真机：静止时补清晰的小帧让闲着也有 140–300 kbps（中继成本），小变化只补变化的那块（bench-competitors 第 2 节）。
8. 打字的“操作到画面”量尺（分支 `reaction`，Fable 在做第 1 步）；4G 虚假重传（NACK 3b，B 阶段第一步，bench-competitors 第 4 节）。

## 4. 推到云端的其他分支（没合进 dev，保存着）

`worktree-agent-a5dace8a1cc798296`（电影模式音画同步）、`worktree-agent-aa350fb390850f11b`（剪贴板按需取）、
`worktree-agent-a8b1b3f174e8aa283`、`worktree-agent-a9615c6713cd1d397`、`a2-cross`（已合）、`tray`、`panel`、`rate2`（已合）、
`presence`（在线状态轮询，不合并，换成订阅）、`music-mode`、`ui-redesign`、`ui-stage3`。

## 4.5 对标实测

`bench-competitors-2026-11-28.md`：ToDesk、UU 远程（响应、帧率、画质），4G 虚假重传的两端日志和根因，以后怎么量。

## 5. 要产品负责人办的（不急，到了再准备）

- Lumos5G 数据集的下载账号（A3 的 5G 类型要用；没有之前 5G 用合成轨迹，标“可信度低”）。

- 真机测（11-28 已发 ab79991 的 Windows、Mac 安装包）：两台 Windows 局域网互连，和 ToDesk、UU 同样操作对比；
  每台打包发回 `%APPDATA%\PeerDesk\logs` 和 `%APPDATA%\PeerDesk\records`（逐帧记录，校准仿真的帧出发时刻）；Mac 是 `~/.config/peerdesk/` 下同名目录。
- 以后：Supabase 测试项目、Stripe 沙盒、发信服务（账户线 P2、P8）；苹果开发者账号和 Mac（iOS、Mac 公证）；
  微软 Partner Center 和 EV 代码签名证书（Windows 虚拟显示器驱动）；无显示器的 Mac mini（虚拟屏实测）。
