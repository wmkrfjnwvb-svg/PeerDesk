#!/usr/bin/env bash
# 提交前的全部检查：格式、严格 lint、测试、覆盖率门槛。
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
# 行覆盖率低于 99% 即失败。需要：cargo install cargo-llvm-cov && rustup component add llvm-tools-preview
./scripts/coverage.sh
