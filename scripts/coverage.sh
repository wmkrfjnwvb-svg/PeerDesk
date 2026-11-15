#!/usr/bin/env bash
# 运行全部测试并检查行覆盖率不低于 99%。
# 唯一排除的文件是 tls_verifier.rs，原因写在该文件开头。
set -euo pipefail
cd "$(dirname "$0")/.."
cargo llvm-cov --workspace --fail-under-lines 99 --summary-only \
  --ignore-filename-regex 'crates/net/src/tls_verifier\.rs' "$@"
