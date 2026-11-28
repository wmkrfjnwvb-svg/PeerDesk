# 调研：远程终端（2026-11-28，Sonnet 子代理读原文；【未核实】的要再核）

目标：内置远程终端（被控端的 shell，走我们已有的加密 QUIC 会话，不开 SSH 端口），给“在自己电脑上常驻跑 AI Agent”的核心用户用。
产品负责人用过 ToDesk 的终端：“只能将就能用”。

## 1. 竞品

- **ToDesk**：设备详情里的“终端”，首次要安全密码；Windows 开 CMD/PowerShell，Mac/Linux 开对应 shell。没有官方文档，只有博客（【未核实】）。
- **RustDesk**（最值得参考）：1.4.1（2025-08）加入终端。之后的修复正好是坑的位置（https://github.com/rustdesk/rustdesk/releases）：
  - 1.4.6 重连缓冲区（持久会话）；
  - 1.4.7 UTF-8；
  - 1.4.9 shell 退出后关标签；
  - 1.5.0 手机端 Ctrl/Alt 开关、OSC 52 默认关、原生复制粘贴快捷键；
  - 1.5.1 回放缓冲区裁剪。
  RustDesk 是 AGPL：只参考做法，不借代码。
- **TeamViewer**：只 Windows、要付费授权和 Windows 凭据（https://www.teamviewer.com/en/global/support/knowledge-base/teamviewer-remote/remote-control/access-the-command-prompt-via-remote-terminal-connection/）。
- **AnyDesk**：Remote Shell 只 Linux 8.0+，默认关、权限开关控制、撤销立即关闭，以登录用户身份运行（https://support.anydesk.com/docs/remote-shell）。
- **Splashtop**：只给管理员的命令行。Jump Desktop：没找到终端【未核实】。
- **Blink Shell + Mosh**：Smart Keys 行（Ctrl/Alt/Esc/方向键，按住修饰键连按），两指点开新 shell、左右滑切换、捏合调字号（https://docs.blink.sh/）。Blink 是 GPL-3.0，只参考交互。
- **Mosh**：漫游、本地预测回显、Ctrl-C 可靠；只同步可见屏幕、没有 scrollback（https://mosh.org/）。论文：EV-DO 上击键响应中位 SSH 503 ms、Mosh < 5 ms（https://mosh.org/mosh-paper.pdf）。
- **Eternal Terminal**：断网、换 IP 会话不断，原生滚动（https://eternalterminal.dev/）。**Tailscale SSH**：check 模式（重新认证，默认 12 小时）、改策略几秒内终止现有会话、会话录制（https://tailscale.com/kb/1193/tailscale-ssh）。

## 2. 好终端的标准

- 仿真：xterm-256color、真彩、备用屏、鼠标上报、括号粘贴、OSC 52、OSC 8、CJK 宽度、IME、resize（SIGWINCH）。用 vttest、esctest 验证。
- Claude Code 的实际用法（https://code.claude.com/docs/en/fullscreen 、https://code.claude.com/docs/en/terminal-config）：
  - 全屏模式用备用屏加鼠标捕获；
  - 用 OSC 52 复制；
  - Shift+Enter 依赖 kitty 键盘协议；
  - Windows ConPTY 下会残留字符，要整屏重绘；
  - 只在 Ghostty、Kitty、iTerm2 里发桌面通知，其他终端可配成响铃。
- 会话持久：主机侧常驻 PTY + scrollback，客户端重连后回放（RustDesk 的做法；Mosh 没有 scrollback）。
- 预测回显：30–60 ms 往返下收益是几十毫秒级，v1 不做，以后按数据评估（推断）。

## 3. AI Agent 场景

- 长会话不能因断线而死：主机侧 PTY 常驻是硬需求。
- 通知：主机侧解析 BEL、OSC 9、OSC 777（规范原文【未核实】），转成手机推送；OSC 133（语义提示符）可以识别命令结束（【未核实】）。
- 只读观看、共享会话、审计：自己设计；命令会进 shell 历史（AnyDesk 的提醒）。
- 手机上批准 Agent 的权限提示：Claude Code 全屏模式下选项能用鼠标点，触屏要能映射成点击或方向键加回车。

## 4. 组件和授权（都和 Apache-2.0 兼容）

| 组件 | 授权 | 状态 |
|---|---|---|
| alacritty_terminal | Apache-2.0，0.26.0（2026-04） | 活跃 |
| vte | Apache-2.0 OR MIT，0.15.0 | 稳定 |
| portable-pty（wezterm） | MIT，0.9.0 | wezterm 仓库仍在更新 |
| egui_term | MIT，0.1.0，作者称开发中、没在 Windows 测过、依赖 alacritty_terminal 0.25 和 egui 0.31 | 大概率自己写渲染，不直接依赖 |
| Termux terminal-emulator / terminal-view | 仓库 GPLv3，但根目录 LICENSE.md 的例外说这两个库是 Apache-2.0（https://github.com/termux/termux-app/blob/master/LICENSE.md） | 引入前核对这两个目录自己的 LICENSE |

ConPTY：CreatePseudoConsole、ResizePseudoConsole；输入输出各一个线程，否则死锁；关伪控制台会终止进程树，所以“关窗口命令不断”要靠我们自己的常驻进程
（https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session）。

## 5. 主机侧安全

- Windows：服务以 LocalSystem 跑，**不能给用户一个 SYSTEM shell**。要以登录用户的令牌启动，没人登录时明确拒绝并说原因（不静默降级）。
- Linux：固定为被控端的登录用户，支持无头。
- macOS：Full Disk Access 对 shell 的影响要实测【未核实】。
- 权限：照 AnyDesk——默认关、按连接控制、撤销立即断开；加 Tailscale 式的重新验证。

## 6. 建议

v1 必做：
1. 主机侧常驻 PTY（Unix portable-pty，Windows ConPTY），多会话，scrollback 放主机，重连回放。
2. alacritty_terminal 做仿真（上面的清单，OSC 52 默认关、提示用户）。
3. 手机端 Smart Keys 行。
4. 权限开关加每次连接授权，撤销立即断开。
5. 响铃和命令结束推到手机。
6. 会话审计日志。

以后：预测回显、只读观看和分享、拖文件进终端、输出搜索、OSC 8、OSC 133 命令块、kitty 键盘协议、手机上批准 Agent 权限的快捷入口。

架构（判断，待验证）：主机跑 PTY + alacritty_terminal 保存屏幕和 scrollback；传输走字节流；重连时先发屏幕快照再接增量。

验收（目标值，实测后定）：
- 回显延迟：局域网 p95 ≤ 30 ms；RTT 50 ms 时 p95 ≤ RTT + 20 ms。
- 重连：网络恢复后 2 秒内可操作，scrollback 不丢。
- 保真：vttest 逐项过；esctest 通过率只升不降。
- 稳定：Agent 长时间输出（100 MB 级）不卡界面，ConPTY 下无残留字符。

没核实的：ToDesk 官方说明、Termius、Jump、Cloudflare 浏览器终端、Termux 子目录 LICENSE、Mosh 论文正文、会话 0 隔离原文、macOS FDA、OSC 9/777/133 规范、Codex 终端行为、JuiceSSH。
