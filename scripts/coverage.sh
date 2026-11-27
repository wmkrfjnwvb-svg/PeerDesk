#!/usr/bin/env bash
# 运行全部测试并检查行覆盖率不低于 99%。
# 排除的文件：
# - tls_verifier.rs，原因写在该文件开头；
# - 媒体层的系统胶水 mf.rs、vt.rs、vaapi.rs（要真的显卡才能跑，由 hw-check 和实机测试验证）、gl.rs（控制端用 OpenGL
#   着色器显示 NV12，要有 GL 上下文，由界面截图测试和实机验证）；
# - 平台层的系统胶水模块 x11.rs、windows.rs、wingpu.rs（Windows 采集的显卡转换）、macos.rs、clipboard_sys.rs（读写真实剪贴板）、audio_sys.rs（打开真实声卡），测试构建不碰开发者电脑的剪贴板和声卡：它们只是调用系统接口，要有真实的
#   桌面才能运行，由 CI 的 platform-live 任务在三个系统上实机测试；平台层的逻辑照常计入覆盖率；
# - 客户端的 keychain.rs：只读写系统钥匙串（存保存密码的加密密钥），测试构建不碰开发者电脑的钥匙串；
#   加密、存取的逻辑在 vault.rs，用内存里的钥匙串测试，照常计入覆盖率；
# - 客户端图形界面的 eframe 接线 gui/app.rs、gui/viewer.rs、gui/files_window.rs 和 gui/yuv.rs（GL 绘制回调）：它们只画控件、把事件转给
#   其他模块，必须有窗口系统和 OpenGL 才能运行。界面背后的逻辑（键位映射、坐标换算、
#   输入翻译、状态机、密码、最近连接、语言、后台会话）都在 gui/ 的其他文件里，照常计入覆盖率。
# - gui/tray.rs、gui/background.rs：系统托盘（菜单栏）图标和系统通知的接线，只调用托盘库（Windows、macOS 的 tray-icon，Linux 的 ksni），
#   要有桌面的托盘宿主才能运行。关闭退到托盘、退出、首次提示、图标状态的逻辑在 gui/tray_logic.rs，开机自启和进程守护在
#   autostart.rs、guard.rs，单实例在 instance.rs，这些照常计入覆盖率。
#
# 不含端到端仿真（crates/e2e，peerdesk-e2e）：它的测试在覆盖率插桩下要跑约 30 分钟，整个脚本从 4 分钟变成 35 分钟。
# 仿真单独跑（`cargo test --release -p peerdesk-e2e`，CI 的 e2e 任务），只在改了传输、编解码、网络、仿真时才跑，见 CLAUDE.md。
# 代价是码率控制状态机、运行中探测等逻辑不能靠仿真场景“顺便”算进覆盖率，必须有自己的单元测试
# （estimator.rs、media.rs、cc/ 里的测试），门槛仍是 99%。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo llvm-cov --workspace --exclude peerdesk-e2e --fail-under-lines 99 --summary-only \
  --ignore-filename-regex 'crates/net/src/tls_verifier\.rs|crates/platform/src/(x11|windows|wingpu|winsession|winservice|macos|clipboard_sys|audio_sys)\.rs|crates/media/src/(mf|vt|vaapi|gl)\.rs|crates/client/src/gui/(app|viewer|files_window|yuv|tray|background)\.rs|crates/client/src/keychain\.rs' "$@"
