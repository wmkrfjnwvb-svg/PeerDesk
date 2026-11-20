#!/usr/bin/env bash
# 运行全部测试并检查行覆盖率不低于 99%。
# 排除的文件：
# - tls_verifier.rs，原因写在该文件开头；
# - 客户端图形界面的 eframe 接线 gui/app.rs 和 gui/viewer.rs：它们只画控件、把事件转给
#   其他模块，必须有窗口系统和 OpenGL 才能运行，单元测试里没有。界面背后的逻辑
#   （键位映射、坐标换算、输入翻译、状态机、密码、最近连接、语言、后台会话）都在 gui/ 的
#   其他文件里，照常计入覆盖率。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo llvm-cov --workspace --fail-under-lines 99 --summary-only \
  --ignore-filename-regex 'crates/net/src/tls_verifier\.rs|crates/client/src/gui/(app|viewer)\.rs' "$@"
