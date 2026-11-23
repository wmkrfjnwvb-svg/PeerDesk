#!/usr/bin/env bash
# 运行全部测试并检查行覆盖率不低于 99%。
# 排除的文件：
# - tls_verifier.rs，原因写在该文件开头；
# - 平台层的系统胶水模块 x11.rs、windows.rs、macos.rs、clipboard_sys.rs（读写真实剪贴板）、audio_sys.rs（打开真实声卡），测试构建不碰开发者电脑的剪贴板和声卡：它们只是调用系统接口，要有真实的
#   桌面才能运行，由 CI 的 platform-live 任务在三个系统上实机测试；平台层的逻辑照常计入覆盖率；
# - 客户端图形界面的 eframe 接线 gui/app.rs 和 gui/viewer.rs：它们只画控件、把事件转给
#   其他模块，必须有窗口系统和 OpenGL 才能运行。界面背后的逻辑（键位映射、坐标换算、
#   输入翻译、状态机、密码、最近连接、语言、后台会话）都在 gui/ 的其他文件里，照常计入覆盖率。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo llvm-cov --workspace --fail-under-lines 99 --summary-only \
  --ignore-filename-regex 'crates/net/src/tls_verifier\.rs|crates/platform/src/(x11|windows|winsession|macos|clipboard_sys|audio_sys)\.rs|crates/client/src/gui/(app|viewer)\.rs' "$@"
