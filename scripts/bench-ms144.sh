#!/bin/bash
# 在固定的 144 个世界（12 条微软轨迹 × 12 个种子）上跑一遍，和存下的基线逐对比较。
# 说明和规则见 crates/e2e/bench/ms144/README.md。
#
# 用法：scripts/bench-ms144.sh [基线 CSV]（默认 crates/e2e/bench/ms144/baseline.csv）
#   JOBS=4（默认）并行跑的会话数；4 核约 11 分钟。量速度要单线程，见批量运行器的说明。
set -euo pipefail
cd "$(dirname "$0")/.."
base=${1:-crates/e2e/bench/ms144/baseline.csv}
out=$(mktemp -d)
cargo build --release -p peerdesk-e2e --example batch
target/release/examples/batch --jobs "${JOBS:-4}" --out "$out/now.csv" crates/e2e/bench/ms144/worlds
python3 -I scripts/paired-compare.py "$base" "$out/now.csv" --by-name
echo
echo "这次的结果：$out/now.csv（正式改进合并时用它换掉基线，见 README）"
