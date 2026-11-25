#!/usr/bin/env bash
# 发送节奏（pacing）校准：真实二进制在限速的回环网络上（tbf 限速 + netem 延迟），
# 对比不开 pacing 和几种倍数，量第一幅画面用时、送达帧数、最长停顿、往返延迟。
# 仿真的瓶颈模型和真实限速器不完全一样（见 crates/core/src/pacer.rs），默认值要两边都看。
# 需要 root（tc）、Xvfb、xterm。用法：scripts/pace-lab.sh [peerdesk 二进制]；结果在 $OUT/pace.md。
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${1:-target/release/peerdesk}
OUT=${OUT:-target/pace-lab}
DISPLAY_NUM=${DISPLAY_NUM:-:92}
SECONDS_PER_RUN=${SECONDS_PER_RUN:-8}
rm -rf "$OUT" && mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  tc qdisc del dev lo root 2>/dev/null || true
}
trap cleanup EXIT

Xvfb "$DISPLAY_NUM" -screen 0 1280x720x24 >/dev/null 2>&1 & pids+=($!)
sleep 1
# 不停滚动的终端：画面持续变化（相当于看日志），每次连接都要先送一个完整关键帧
DISPLAY=$DISPLAY_NUM xterm -geometry 210x56+0+0 -fa Monospace -fs 10 \
  -e sh -c 'while :; do head -c 2000 /dev/urandom | base64; sleep 0.2; done' >/dev/null 2>&1 & pids+=($!)
sleep 1.5

table="$OUT/pace.md"
echo "| 网络 | pacing | 第一幅画面 | 送达帧 | 最长停顿 ms | 往返 ms | 画面 KB |" > "$table"
echo "|---|---|---|---|---|---|---|" >> "$table"

shape() { # 速率 延迟ms 缓冲（排队上限 ms）
  tc qdisc del dev lo root 2>/dev/null || true
  # 有 netem 时加固定延迟；没有（有些内核没编进来）时只限速，测的是“突发撞上浅缓冲”
  if tc qdisc add dev lo root handle 1: netem delay "$2ms" 2>/dev/null; then
    tc qdisc add dev lo parent 1: handle 2: tbf rate "$1" burst 5kb latency "$3ms"
  else
    tc qdisc add dev lo root tbf rate "$1" burst 5kb latency "$3ms"
  fi
}

run() { # 网络名 pacing（off 或倍数千分比）
  local net=$1 pace=$2 w="$OUT/$1-$2"
  mkdir -p "$w"
  local env=(PEERDESK_PACE=0)
  if [ "$pace" != off ]; then env=(PEERDESK_PACE=1 PEERDESK_PACE_FACTOR="$pace"); fi
  env "${env[@]}" PEERDESK_DATA_DIR="$w/host" DISPLAY=$DISPLAY_NUM "$BIN" host --listen 127.0.0.1:0 --data-dir "$w/host" --open --once \
    >"$w/host.log" 2>&1 & local host=$!
  pids+=($host)
  for _ in $(seq 60); do grep -q '^指纹' "$w/host.log" && break; sleep 0.25; done
  local addr pin
  addr=$(sed -n 's/^监听 //p' "$w/host.log")
  pin=$(sed -n 's/^指纹 //p' "$w/host.log")
  PEERDESK_DATA_DIR="$w/ctrl" "$BIN" connect "$addr" --pin "$pin" --data-dir "$w/ctrl" --seconds "$SECONDS_PER_RUN" >"$w/ctrl.log" 2>&1 || true
  wait "$host" 2>/dev/null || true
  local first frames gap rtt bytes
  first=$(sed -n 's/^第一幅画面 *//p' "$w/ctrl.log")
  frames=$(sed -n 's/^帧 *送达 \([0-9]*\).*/\1/p' "$w/ctrl.log")
  gap=$(sed -n 's/^最长停顿 *\([0-9]*\).*/\1/p' "$w/ctrl.log")
  rtt=$(sed -n 's/^往返延迟 *\([0-9]*\).*/\1/p' "$w/ctrl.log")
  bytes=$(sed -n 's/^画面数据 *\([0-9]*\).*/\1/p' "$w/ctrl.log")
  echo "| $net | $pace | ${first:-?} | ${frames:-?} | ${gap:-?} | $(( ${rtt:-0} / 1000 )) | $(( ${bytes:-0} / 1024 )) |" >> "$table"
}

# 网络：速率、单程延迟、tbf 排队上限（缓冲大小 = 速率 × 时长）
NETS=${NETS:-"400kbit:20:200 1500kbit:20:200 1500kbit:40:60 5mbit:15:100 20mbit:5:50"}
PACES=${PACES:-"off 1250 2500"}
for spec in $NETS; do
  IFS=: read -r rate delay buf <<<"$spec"
  shape "$rate" "$delay" "$buf"
  for p in $PACES; do run "$rate-${delay}ms-q${buf}ms" "$p"; done
done
tc qdisc del dev lo root 2>/dev/null || true
cat "$table"
