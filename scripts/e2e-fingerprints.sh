#!/usr/bin/env bash
# 端到端仿真结果的指纹：证明一次重构“行为逐字节不变”。
#
# 用法：
#   scripts/e2e-fingerprints.sh           跑全量端到端仿真（release），和 crates/e2e/golden/fingerprints.txt 比
#   scripts/e2e-fingerprints.sh --update  重新录基准（算法本来就要变的改动之后用，提交信息里说明为什么变）
#
# 每次仿真运行在指纹文件里留一行：场景的身份和结果的指纹（除调试记录以外的每个数、每个阶段结束时两端的画面像素，
# 见 crates/e2e/src/lib.rs 的 fingerprint）。测试并行跑，行的顺序不定，所以排序后比。
# 指纹和编码器的每个字节有关：基准是在 Linux x86_64 上录的，只在同样的系统上比。
set -euo pipefail
cd "$(dirname "$0")/.."
golden=crates/e2e/golden/fingerprints.txt
out=$(mktemp)
trap 'rm -f "$out" "$out.sorted"' EXIT
PEERDESK_E2E_FINGERPRINTS="$out" cargo test --release -p peerdesk-e2e --no-fail-fast
sort "$out" > "$out.sorted"
if [ "${1:-}" = "--update" ]; then
    cp "$out.sorted" "$golden"
    echo "基准已更新：$(wc -l < "$golden") 次运行"
    exit 0
fi
if diff "$golden" "$out.sorted" > /dev/null; then
    echo "指纹相同：$(wc -l < "$golden") 次运行逐字节不变"
else
    echo "指纹不同（< 基准，> 这一次）："
    diff "$golden" "$out.sorted" | head -40
    exit 1
fi
