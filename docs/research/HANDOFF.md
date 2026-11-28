# 交接：所有进行中的工作和下一步（2026-11-28 14:00 UTC，主会话写）

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

### 2.1 Android 控制端（主会话）——**已合进 dev**（本次合并），分支 `android` 最后 4d3f302

- 方案和进度：`R-android.md`（第 7 节进度表，真机反馈和根因都写在里面）。
- 合进 dev 的：`peerdesk-net::viewer`（桌面和手机共用的访问驱动）、OpenH264 可选、`crates/mobile`（会话、手势状态机、按键映射、MediaCodec 解到 Surface、JNI）、
  `android/` Kotlin + Compose 界面（连接页、会话页、虚拟鼠标、点击水波、软键盘让位、会话全屏、返回键按两次、手势记录导出）、CI 的 `android` 任务（`[android]` 标记）。
- 真机记录回放（`crates/mobile/tests/trace_replay.rs`，两份产品负责人的记录）：滚动 25 次、捏合 26 次全对；修过的根因：系统取消只带一根手指时卡死、
  两指上下叠着滚动时一根比另一根早动 40 多毫秒被当成捏合。
- 合并的 CI 带 `[mac]`（桌面控制端改用共用驱动、OpenH264 改可选，先在 macOS 上验证），过了再推一次 `[win]`。
- 下一步：等产品负责人测 4d3f302 的 APK；诊断页（首帧、解码耗时、帧率、冻结）、切后台不断开；iOS 以后。
- 构建：`source scripts/android-env.sh`；SDK 在 `~/android-sdk`（`scripts/android-setup.sh` 装，许可协议产品负责人已同意）。新容器要重新装。

### 2.2 仿真框架 A2-4（diskagent 会话）——分支 `sim-a2`

- A2-3 已合进 dev（758ac0b，CI 过）。diskagent 已把 dev 合回 sim-a2（eb31696）。
- A2-4 设计（a2-plan 第 5 节“A2-4 的设计”）主会话审过、同意，补充：golden 153 行全部逐字节不变；SACK 默认开，受影响的指纹前后对比后由主会话改 golden；
  收敛时间加“未收敛”记法和“稳定值 ÷ 公平份额”；最终报告 12 个种子。
- 分三步：A2-4a 多会话（进行中，12:36 起）→ A2-4b NR-BPDV 抖动和 TCP SACK → A2-4c RFC 8867 八个用例和 RFC 8868 指标的报告。
- 为了早点进 A3：A2-4b 合并后 diskagent 就开始写 A3 的设计，和 A2-4c 的长时间运行并行。
- A2-4a 交审（sim-a2 39f7e89）：主循环拆分通过（153 行逐字节相同、144/144 相同、两对共享瓶颈测试）；**有一个 bug 要先修**：`Shared::pop` 会把加了额外延迟的包提前送到
  （内层下一个包比延迟的晚时直接返回延迟的那个），5.5 的 RTT 公平会判错。修完和 A2-4b 一起合进 dev，golden 加 A2-4a 新增的 `zero` 一行。
- 记下的问题（不是仿真的错）：A 家有人上传、出口没有 AQM 时码率掉到 2.1 Mbps、冻结 16.8 次；长 TCP 把码率挤到 106 kbps；换路后约 0.7 秒才恢复。

### 2.3 账户设备表（主会话）——分支 `devices-s1`，最后 4acff3a

- `devices` 分支 rebase 到 dev 之后推成了新分支 `devices-s1`（原来的 `devices` 不能强推）。S0、S1a–S1d，测试和 lint 都过。
- 下一步：O-phase1-plan P1 的两处改动——账户号改成 16 字节（老的 8 字节补零，UUID 第 9 字节是变体位、不会重号；令牌、账本、中继票、心跳的兼容做法见本次会话的设计：老账户号仍写 16 位十六进制）；入表令牌改成绑定账户、默认一次性、1 小时。然后合进 dev。

## 3. 排队（按 `O-standard-plan.md` 的顺序）

1. Android 真机反馈继续（2.1），诊断页、切后台不断开。
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
