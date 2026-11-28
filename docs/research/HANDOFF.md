# 交接：所有进行中的工作和下一步（2026-11-28 12:00 UTC，主会话写）

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

### 2.1 Android 控制端（主会话）——分支 `android`，最后 024f73d（WIP）

- 方案和进度：`R-android.md`（第 7 节进度表）。
- 已完成并真机验证过：A0 工具链、A1 Rust 核心（`crates/mobile`）、A2 MediaCodec 解到 Surface 加 JNI、A3 Kotlin + Compose 界面。
  产品负责人 11-28 实测：**能连上、能看到画面、能点击**。
- 修过的真机问题：句柄按位放进 long（Android 11 起堆指针带标记，30b885c）；密码显示/隐藏；记住设备码和密码（Keystore 加密）。
- **做到一半（024f73d，编不过）**：手势按 ToDesk 的习惯改（产品负责人反馈“操作有点怪”）。新手势表写在 `crates/mobile/src/gesture.rs` 开头：
  - 触屏模式：单指点左键、单指滑动平移画面、长按震一下后松开右键或拖动拖拽、双指滑动滚动、捏合缩放、双指点右键；
  - 鼠标模式（鼠标助手）：屏幕上有光标、手指精细移动光标、屏幕上的左键右键按钮（按住左键再滑动 = 精细选中文字）。
  - 下一步：
    1. 按新表重写 `gesture.rs` 的测试；
    2. `lib.rs` 的 `apply` 把 `Action::Haptic`、`Action::Cursor` 转成 JSON（`{"type":"haptic"}`、`{"type":"cursor","x":..,"y":..}`）；
    3. `android/bridge.rs`：`setView` 去掉 `zoomed` 参数、`setMode` 返回动作 JSON（进鼠标模式时交出光标）、新加 `button(h, button, down)`（调 `Gestures::button`）；
    4. Kotlin：工具条加“鼠标”按钮切鼠标模式；收到 cursor 画光标；浮动的“左键”“右键”按钮（按下、抬起分别调 `button`）；收到 haptic 震一下；画面放大时光标到边上跟着平移；`Native.kt`、`NativeSession.kt` 跟着改签名；
    5. `scripts/android-build.sh` 编 APK 发给产品负责人。
- 之后：A4（CI 里 `[android]` 标记出 APK）、诊断页导出日志、切后台不断开、Android 合进 dev（等手势改完、CI 的 lint 和覆盖率过）。
- 构建：`source scripts/android-env.sh`；SDK 在 `~/android-sdk`（`scripts/android-setup.sh` 装，许可协议产品负责人已同意）。新容器要重新装。

### 2.2 仿真框架 A2-3（diskagent 会话）——分支 `sim-a2`，最后 4a21217（已在云端，工作区干净）

- 第一步（Network 接口、布局 2、路径变了事件）：**主会话审过，通过**，等 sim-a2 的 CI（3eec326）过了合进 dev，golden 加它的 1 行 `layout2 … c417b89064bb3f42`。
- 第一步和第二小步“分段路径”：**都已审过、合进 dev**（2026-11-28 合并提交，golden 加了 5 行：layout2 1 行、homes 4 行）。结果在 `Q-redesign/a2-plan.md` 第 8 节末尾。
  - 合并之后：diskagent 把 dev 合回 sim-a2 再往下做（下一步看 a2-plan，A2-4 或主会话安排）。
  - 家庭场景已经量出产品问题：A 家有人上传、出口没有 AQM 时，码率掉到 2.1 Mbps、冻结 16.8 次（开 CoDel 时 0 次）——要靠分段测量认出“A 自己出口在堵”（B 阶段）。
  - 已知限制：交叉流的往返只含它挂的那一段（A3 补）；家庭参数是一个点，不是分布。
- 记下的问题（不是仿真的错）：长 TCP 把码率挤到 106 kbps；换路后约 0.7 秒才恢复；布局 1 在高丢包链路上偏乐观（A5 迁移门槛时按布局 2 重定）。

### 2.3 账户设备表（主会话）——分支 `devices-s1`，最后 4acff3a

- `devices` 分支 rebase 到 dev 之后推成了新分支 `devices-s1`（原来的 `devices` 不能强推）。S0、S1a–S1d，测试和 lint 都过。
- 下一步：O-phase1-plan P1 的两处改动——账户号改成 16 字节（老的 8 字节补零，UUID 第 9 字节是变体位、不会重号；令牌、账本、中继票、心跳的兼容做法见本次会话的设计：老账户号仍写 16 位十六进制）；入表令牌改成绑定账户、默认一次性、1 小时。然后合进 dev。

## 3. 排队（按 `O-standard-plan.md` 的顺序）

1. Android 手势改完、合进 dev（2.1）。
2. S1 连入确认（设计已定，见 `O-standard-plan.md` 2.1）。
3. S2 多显示器、S3 Mac 和 Linux 无人值守、S6 唤醒和重启连回、S7 会话内开关、S8 文件权限（S7、S8 可以派 Sonnet）。
4. 账户线 P1–P3b（本地用自己生成的密钥测，不等 Supabase）。
5. Agent 功能 G1–G4（工具分层草案在 `industry-agent-remote-control.md`）。
6. 隐私屏、虚拟显示器（做法在 `industry-privacy-virtual-display.md`；Mac 虚拟屏要无显示器的 Mac mini 实测）。
7. “停下后没变清晰”在微软轨迹上很普遍（11-28 基线 144 次里 111 次）：和 #79 补清晰策略一起查。

## 4. 推到云端的其他分支（没合进 dev，保存着）

`worktree-agent-a5dace8a1cc798296`（电影模式音画同步）、`worktree-agent-aa350fb390850f11b`（剪贴板按需取）、
`worktree-agent-a8b1b3f174e8aa283`、`worktree-agent-a9615c6713cd1d397`、`a2-cross`（已合）、`tray`、`panel`、`rate2`（已合）、
`presence`（在线状态轮询，不合并，换成订阅）、`music-mode`、`ui-redesign`、`ui-stage3`。

## 5. 要产品负责人办的（不急，到了再准备）

- 真机测：#71 的安装包（7c56f8c 的 CI 产物）、Android APK（手势改完后的新版）。
- 以后：Supabase 测试项目、Stripe 沙盒、发信服务（账户线 P2、P8）；苹果开发者账号和 Mac（iOS、Mac 公证）；
  微软 Partner Center 和 EV 代码签名证书（Windows 虚拟显示器驱动）；无显示器的 Mac mini（虚拟屏实测）。
