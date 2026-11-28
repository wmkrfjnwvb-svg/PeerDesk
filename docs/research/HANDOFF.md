# 交接：所有进行中的工作和下一步（2026-11-28 13:00 UTC，主会话写）

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

### 2.1 Android 控制端（主会话）——分支 `android`，已在云端

- 方案和进度：`R-android.md`（第 7 节进度表）。真机验证过：能连上、能看、能点（11-28）。
- 259aa39：手势照 ToDesk（`gesture.rs` 开头的表）、虚拟鼠标（小鼠标 ↔ 大鼠标：左键、右键、身体、关闭，`MouseAssist.kt`）；APK 发给产品负责人测过。
- 产品负责人第二轮反馈和处理（fcca0e3，Rust 已完成、测过）：
  - 捏合抖：根因是一个触摸事件拆成逐根手指更新、缩放不跟手、每帧改 SurfaceView 尺寸、编码尺寸提示每步都发。
    改成整个事件一起交（`Gestures::moves`、JNI `moves`）、以旧中点缩放再跟中点平移；
  - 双指滚动和捏合冲突：判据改成比较两指距离变化和中点走过的距离，分不出看方向（照 Android ScaleGestureDetector、libinput），真实手势的测试；
  - 点击告诉界面位置（`tap`）：画水波、记住输入位置。
- **进行中**（Kotlin，子代理在做，WIP 44d2023、b7b130f 未审）：批量移动、SurfaceView 改用缩放平移变换、编码尺寸提示 300 ms 防抖、箭头尖挪到左键正上方、点击水波和按键时箭头缩小、
  软键盘弹出时把输入位置让到键盘上面（adjustPan 的思路）、导出手势记录（真机问题拿原始触点回放）。
  下一步：审代码 → `scripts/android-build.sh` 和 JVM 测试 → 提交 → APK 发产品负责人。
- 合进 dev 前的本地门槛（已把 dev 758ac0b 合进 android）：覆盖率过（行 99.12%）；e2e 在跑；clippy 要重跑（合并后 e2e 的 `Picture` 匹配缺 `Presented`，已补）。
  过了以后去掉 `[skip ci]` 推一次，让 CI（含新的 android 任务）跑，再合进 dev。
- CI 新的 `android` 任务（f031521）：改了 `android/`、`crates/mobile/` 才跑；`[android]`、`[pkg]` 上传 APK。CLAUDE.md 表里加了 `[android]`（在 android 分支，合并后生效）。
- 之后：诊断页、切后台不断开。
- 构建：`source scripts/android-env.sh`；SDK 在 `~/android-sdk`（`scripts/android-setup.sh` 装，许可协议产品负责人已同意）。新容器要重新装。

### 2.2 仿真框架 A2-4（diskagent 会话）——分支 `sim-a2`

- A2-3 已合进 dev（758ac0b，CI 过）。diskagent 已把 dev 合回 sim-a2（eb31696）。
- A2-4 设计（a2-plan 第 5 节“A2-4 的设计”）主会话审过、同意，补充：golden 153 行全部逐字节不变；SACK 默认开，受影响的指纹前后对比后由主会话改 golden；
  收敛时间加“未收敛”记法和“稳定值 ÷ 公平份额”；最终报告 12 个种子。
- 分三步：A2-4a 多会话（进行中，12:36 起）→ A2-4b NR-BPDV 抖动和 TCP SACK → A2-4c RFC 8867 八个用例和 RFC 8868 指标的报告。
- 为了早点进 A3：A2-4b 合并后 diskagent 就开始写 A3 的设计，和 A2-4c 的长时间运行并行。
- 记下的问题（不是仿真的错）：A 家有人上传、出口没有 AQM 时码率掉到 2.1 Mbps、冻结 16.8 次；长 TCP 把码率挤到 106 kbps；换路后约 0.7 秒才恢复。

### 2.3 账户设备表（主会话）——分支 `devices-s1`，最后 4acff3a

- `devices` 分支 rebase 到 dev 之后推成了新分支 `devices-s1`（原来的 `devices` 不能强推）。S0、S1a–S1d，测试和 lint 都过。
- 下一步：O-phase1-plan P1 的两处改动——账户号改成 16 字节（老的 8 字节补零，UUID 第 9 字节是变体位、不会重号；令牌、账本、中继票、心跳的兼容做法见本次会话的设计：老账户号仍写 16 位十六进制）；入表令牌改成绑定账户、默认一次性、1 小时。然后合进 dev。

## 3. 排队（按 `O-standard-plan.md` 的顺序）

1. Android 第三轮改完、真机测过、合进 dev（2.1）。
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
