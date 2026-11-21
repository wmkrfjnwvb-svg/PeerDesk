# 下次继续（2026-11-21 中断点）

## 已完成未推 CI 验证
- 新旧版本兼容（docs/compat.md，crates/core/tests/compat.rs）、海外优先 IPv6、中英文切换。
- 剪贴板：协议和逻辑 crates/core/src/clipboard.rs；会话接线 crates/net/src/clipsync.rs（控制端开流、
  被控端按权限接受、能力位 CLIPBOARD）；端到端测试 crates/net/tests/clipboard.rs 通过；
  系统剪贴板 crates/platform/src/clipboard_sys.rs（arboard + 各系统变化计数 + 敏感内容跳过）。

## 剪贴板还差
1. 客户端接线：gui/session.rs、remote.rs 里 ControllerOptions.clipboard 和 HostOptions.clipboard
   用 `peerdesk_platform::SystemClip::open(...)`；两端 SessionConfig.caps 加上 `Caps::CLIPBOARD`。
2. 界面：ClipNote 提示（超限 → “请用文件传输”），被控端权限开关，远程窗口里的开关。
3. Windows/macOS 交叉编译本机做不了（ring 的 C 构建），用 `[win]`/`[mac]` 在 CI 验证 clipboard_sys.rs。
4. 跑全量 clippy、测试、覆盖率后推送。

## 调研已完成（待据此排期）
- docs/research/G-unattended.md 无人值守、锁屏、无显示器
- docs/research/H-audio.md 声音（NetEQ 式抖动缓冲、Opus、RED）
- docs/research/I-release.md 硬件编码、签名、自动更新

## 新需求：远程键盘和快捷键
键盘操作和系统快捷键要完整转到远程，并处理跨系统互操作（Mac↔Windows↔Linux 的 Cmd/Ctrl 映射、
输入法、键盘布局、Win 键/Alt+Tab/Cmd+Tab 捕获、Ctrl+Alt+Del、防止修饰键卡住）。
调研报告 docs/research/J-keyboard.md（子代理写，可能在下次开始时已提交）。排在剪贴板收尾之后。

## 之后的顺序（产品负责人同意的优先级）
声音 → 硬件编码 → 无人值守（Windows 先）→ 自动更新和签名 → 文件传输界面 → 账户 + Stripe（数据库用 Supabase）
→ 客户端美观 + 英文官网（中英切换）。域名待定。
