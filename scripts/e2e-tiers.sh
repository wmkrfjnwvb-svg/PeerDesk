#!/usr/bin/env bash
# 同一批端到端场景在 L1（真编解码）和 L0.5（假后端吃真像素，见 docs/research/Q-redesign/a1-plan.md）上各跑一遍，
# 逐场景对比关键数字：冻结、最长等待、发出的字节、关键帧数、花屏（解码失败）、停下变清晰的时间。
#
# 用法：scripts/e2e-tiers.sh [目录]    结果放在目录里（默认临时目录）：l1.txt、l05.txt、compare.txt
#
# L0.5 下很多测试的断言会不过（没有像素可比，信噪比是 NaN；字节模型是占位），所以这里只收摘要，不看测试成败。
# 摘要由环境变量 PEERDESK_E2E_SUMMARY 打开（crates/e2e/src/lib.rs 的 note_summary）。
# 速度另量：加 PEERDESK_E2E_PROFILE=1 和 -- --test-threads=1（并行时墙上时间被抢占，摘要里的 wall 只是参考）。
set -uo pipefail
cd "$(dirname "$0")/.."
dir=${1:-$(mktemp -d)}
mkdir -p "$dir"
rm -f "$dir/l1.txt" "$dir/l05.txt"
PEERDESK_E2E_SUMMARY="$dir/l1.txt" cargo test --release -p peerdesk-e2e --no-fail-fast > "$dir/l1.log" 2>&1
PEERDESK_E2E_CODEC=h264-l05 PEERDESK_E2E_SUMMARY="$dir/l05.txt" cargo test --release -p peerdesk-e2e --no-fail-fast > "$dir/l05.log" 2>&1
# 同一个场景身份可能出现多次（同一测试里重复跑，结果相同）：去掉最后一列墙上时间（每次不同）再按整行去重
# 块编码的对照运行（codec 列是 tile）两边都一样，不比
cut -d'|' -f1-11 "$dir/l1.txt" | grep -v '|tile|' | sort -u > "$dir/l1.sorted"
cut -d'|' -f1-11 "$dir/l05.txt" | grep -v '|tile|' | sort -u > "$dir/l05.sorted"
awk -F'|' '
function num(s, key,   a) { if (match(s, key " [0-9.]+")) { split(substr(s, RSTART, RLENGTH), a, " "); return a[2] + 0 } return 0 }
FNR == NR { l1[$1] = $0; next }
{
    id = $1
    if (!(id in l1)) { only05++; next }
    split(l1[id], a, "|")
    n++
    f1 += num(a[5], "freezes"); f5 += num($5, "freezes")
    b1 += num(a[9], "bytes");   b5 += num($9, "bytes")
    k1 += num(a[4], "keys");    k5 += num($4, "keys")
    bad1 += num(a[8], "bad");   bad5 += num($8, "bad")
    printf "%s\n  L1   %s\n  L0.5 %s\n", id, substr(l1[id], length(id) + 2), substr($0, length(id) + 2)
}
END {
    printf "\n共 %d 个场景可比（L0.5 多出 %d 个）\n", n, only05
    printf "冻结合计：L1 %d，L0.5 %d；关键帧合计：L1 %d，L0.5 %d；发出字节合计：L1 %d，L0.5 %d；解码失败合计：L1 %d，L0.5 %d\n", f1, f5, k1, k5, b1, b5, bad1, bad5
}' "$dir/l1.sorted" "$dir/l05.sorted" > "$dir/compare.txt"
tail -3 "$dir/compare.txt"
echo "详情：$dir/compare.txt"
