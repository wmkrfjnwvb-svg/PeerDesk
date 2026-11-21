#!/usr/bin/env bash
# 真实二进制的端到端闭环（Linux，需要 Xvfb、xterm、python3）：
#   被控端在虚拟桌面上采集真实屏幕、注入真实键鼠；控制端经真实网络连接，保存看到的画面，
#   并在远端终端里输入一条命令。验证三件事：
#   1. 控制端看到的画面和被控端自己截的屏一致（亮度 PSNR）；
#   2. 输入的命令真的在被控端执行了（文件出现）；
#   3. 有 tc 和权限时，在加了延迟和丢包的回环网络上再跑一遍。
# 用法：scripts/e2e-live.sh [peerdesk 二进制]；结果和截图在 $OUT（默认 target/e2e-live）。
set -euo pipefail
cd "$(dirname "$0")/.."
BIN=${1:-target/release/peerdesk}
OUT=${OUT:-target/e2e-live}
DISPLAY_NUM=${DISPLAY_NUM:-:91}
rm -rf "$OUT" && mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  if [ -n "${NETEM:-}" ]; then sudo tc qdisc del dev lo root 2>/dev/null || true; fi
}
trap cleanup EXIT

Xvfb "$DISPLAY_NUM" -screen 0 1280x720x24 >/dev/null 2>&1 & pids+=($!)
sleep 1
# 铺满屏幕的终端：指针在画面中央时键盘输入就到它（没有窗口管理器时焦点跟着指针）。
DISPLAY=$DISPLAY_NUM xterm -geometry 210x56+0+0 -fa Monospace -fs 10 -bg white -fg black \
  -e bash --norc --noprofile >/dev/null 2>&1 & pids+=($!)
sleep 1.5

summary="$OUT/summary.md"
echo "| 网络 | 画面 PSNR dB | 输入执行 | 送达帧 | 往返延迟 |" > "$summary"
echo "|---|---|---|---|---|" >> "$summary"
fail=0

run_case() {
  local name=$1
  local mark="$OUT/typed-$name"
  local w="$OUT/$name"
  mkdir -p "$w"
  DISPLAY=$DISPLAY_NUM "$BIN" host --listen 127.0.0.1:0 --data-dir "$w/host" --open --once \
    >"$w/host.log" 2>&1 & local host=$!
  pids+=($host)
  for _ in $(seq 60); do grep -q '^指纹' "$w/host.log" && break; sleep 0.25; done
  local addr pin
  addr=$(sed -n 's/^监听 //p' "$w/host.log")
  pin=$(sed -n 's/^指纹 //p' "$w/host.log")
  if [ -z "$addr" ]; then echo "被控端没有启动："; cat "$w/host.log"; return 1; fi
  "$BIN" connect "$addr" --pin "$pin" --data-dir "$w/ctrl" --seconds 6 \
    --snapshot "$w/seen.bmp" --type-line "touch $mark" >"$w/ctrl.log" 2>&1 || true
  wait "$host" 2>/dev/null || true
  # 被控端自己截一张作为标准答案（会话刚结束，画面没再变）
  DISPLAY=$DISPLAY_NUM "$BIN" screen-check --out "$w/truth.bmp" >"$w/truth.log" 2>&1 || true
  local psnr typed frames rtt
  psnr=$(python3 -I scripts/bmp_psnr.py "$w/seen.bmp" "$w/truth.bmp" 2>&1 | tail -1) || psnr="失败"
  if [ -e "$mark" ]; then typed="是"; else typed="否"; fi
  frames=$(sed -n 's/^帧 *送达 \([0-9]*\).*/\1/p' "$w/ctrl.log")
  rtt=$(sed -n 's/^往返延迟 *\(.*\)/\1/p' "$w/ctrl.log")
  echo "| $name | $psnr | $typed | ${frames:-?} | ${rtt:-?} |" >> "$summary"
  local ok=1
  python3 -I scripts/bmp_psnr.py "$w/seen.bmp" "$w/truth.bmp" 30 >/dev/null 2>&1 || ok=0
  [ "$typed" = "是" ] || ok=0
  if [ $ok = 0 ]; then
    echo "失败（$name）：画面 PSNR $psnr，输入执行 $typed"
    echo "--- 被控端"; cat "$w/host.log"; echo "--- 控制端"; cat "$w/ctrl.log"
    fail=1
  fi
}

run_case loopback
if command -v tc >/dev/null && sudo -n tc qdisc add dev lo root netem delay 25ms 5ms loss 2% 2>/dev/null; then
  NETEM=1
  run_case "delay25ms-loss2%"
  sudo tc qdisc del dev lo root; NETEM=
else
  echo "| 延迟+丢包 | 跳过（没有 tc 或权限） | | | |" >> "$summary"
fi
cat "$summary"
exit $fail
