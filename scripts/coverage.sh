#!/usr/bin/env bash
# 运行全部测试并检查行覆盖率不低于 99%。
# 排除的文件：tls_verifier.rs（原因写在该文件开头），以及平台层的三个系统胶水模块
# x11.rs、windows.rs、macos.rs：它们只是调用系统接口，要有真实的桌面才能运行，
# 由 CI 的 platform-live 任务在三个系统上实机测试；平台层的逻辑都在别的模块里，照常计入覆盖率。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo llvm-cov --workspace --fail-under-lines 99 --summary-only \
  --ignore-filename-regex 'crates/net/src/tls_verifier\.rs|crates/platform/src/(x11|windows|macos)\.rs' "$@"
