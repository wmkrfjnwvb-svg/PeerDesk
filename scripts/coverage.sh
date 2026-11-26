#!/usr/bin/env bash
# 运行全部测试并检查行覆盖率不低于 99%。
# 排除的文件：
# - tls_verifier.rs，原因写在该文件开头；
# - 媒体层的系统胶水 mf.rs、vt.rs、vaapi.rs（要真的显卡才能跑，由 hw-check 和实机测试验证）、gl.rs（控制端用 OpenGL
#   着色器显示 NV12，要有 GL 上下文，由界面截图测试和实机验证）；
# - 平台层的系统胶水模块 x11.rs、windows.rs、wingpu.rs（Windows 采集的显卡转换）、macos.rs、clipboard_sys.rs（读写真实剪贴板）、clipboard_win.rs 和 clipboard_mac.rs（剪贴板延迟提供数据的窗口消息循环和 Objective-C 数据提供者，取数和格式转换的逻辑在 promised.rs、照常计入）、audio_sys.rs（打开真实声卡），测试构建不碰开发者电脑的剪贴板和声卡：它们只是调用系统接口，要有真实的
#   桌面才能运行，由 CI 的 platform-live 任务在三个系统上实机测试；平台层的逻辑照常计入覆盖率；
# - 客户端图形界面的 eframe 接线 gui/app.rs、gui/viewer.rs、gui/files_window.rs 和 gui/yuv.rs（GL 绘制回调）：它们只画控件、把事件转给
#   其他模块，必须有窗口系统和 OpenGL 才能运行。界面背后的逻辑（键位映射、坐标换算、
#   输入翻译、状态机、密码、最近连接、语言、后台会话）都在 gui/ 的其他文件里，照常计入覆盖率。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo llvm-cov --workspace --fail-under-lines 99 --summary-only \
  --ignore-filename-regex 'crates/net/src/tls_verifier\.rs|crates/platform/src/(x11|windows|wingpu|winsession|winservice|macos|clipboard_sys|clipboard_win|clipboard_mac|audio_sys)\.rs|crates/media/src/(mf|vt|vaapi|gl)\.rs|crates/client/src/gui/(app|viewer|files_window|yuv)\.rs' "$@"
