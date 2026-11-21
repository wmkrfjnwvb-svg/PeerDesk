#!/usr/bin/env bash
# NAT 仿真实验室：用 Linux 网络命名空间和 nftables 搭出“公网 + 两个家庭/公司路由器”，
# 用真实的 peerdesk-server、peerdesk host、peerdesk connect 跑通“按设备码连接”，
# 记录每种 NAT 组合下走的是直连还是中继、各阶段用了多久。
#
# 拓扑（公网 203.0.113.0/24 是一个网桥，链路上加了单向 LAN_DELAY/WAN_DELAY 的延迟）：
#
#   [被控端 10.0.1.2] ─ [路由器 A：10.0.1.1 | 203.0.113.11] ─┐
#                                                         ├─ 公网 ─ [服务器 203.0.113.1]
#   [控制端 10.0.2.2] ─ [路由器 B：10.0.2.1 | 203.0.113.12] ─┘
#
# 路由器的行为（RFC 4787 的说法）：
#   cone  Linux 默认的 masquerade：映射与目的地无关（端口尽量保持），过滤与地址和端口有关（端口受限锥形）。
#         家用路由器和光猫最常见。
#   sym   masquerade random：每个新目的地换一个随机端口（对称型）。企业防火墙、部分运营商 CGNAT。
#   open  没有 NAT，本机直接有公网地址。
#   same  两台机器在同一个路由器（cone）后面，路由器不支持回环（hairpin）。
#   cgnat 家庭路由器（cone）外面再套一层运营商 NAT（cone）。
#
# 用法：sudo scripts/nat-lab.sh [二进制目录，默认 target/release] [每种组合跑几次，默认 3]
# 需要 root（命名空间、nftables、tc）。结果在 $OUT（默认 target/nat-lab）的 summary.md。
set -euo pipefail
cd "$(dirname "$0")/.."
BIN_DIR=${1:-target/release}
RUNS=${2:-3}
OUT=${OUT:-target/nat-lab}
WAN_DELAY=${WAN_DELAY:-15ms}
CASES=${CASES:-"open-open cone-cone cone-open open-cone cone-sym sym-cone sym-sym same cgnat-cone"}
BIN_DIR=$(cd "$BIN_DIR" && pwd)
rm -rf "$OUT" && mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)

if [ "$(id -u)" -ne 0 ]; then
  echo "错误：需要 root（创建网络命名空间）。解决办法：sudo $0 $*" >&2
  exit 1
fi
for tool in ip nft tc; do
  command -v $tool >/dev/null || { echo "错误：缺少 $tool。解决办法：apt install iproute2 nftables" >&2; exit 1; }
done

P=pdlab
pids=()
cleanup() {
  for p in "${pids[@]}"; do kill "$p" 2>/dev/null || true; done
  wait 2>/dev/null || true
  for n in $(ip netns list | awk '{print $1}' | grep "^$P-" || true); do ip netns del "$n" 2>/dev/null || true; done
}
trap cleanup EXIT
cleanup

ns() { ip netns exec "$P-$1" "${@:2}"; }

# 公网网桥和服务器。
ip netns add $P-inet
ns inet ip link add br0 type bridge
ns inet ip link set br0 up
ns inet ip link set lo up

# 把命名空间 $1 用一对网卡接到公网网桥上，地址 $2，网卡名 $3。
attach_inet() {
  local name=$1 addr=$2 dev=$3
  ip link add "$P-$name-$dev" type veth peer name "$P-x-$name"
  ip link set "$P-$name-$dev" netns "$P-$name"
  ip link set "$P-x-$name" netns "$P-inet"
  ns "$name" ip link set "$P-$name-$dev" name "$dev"
  ns "$name" ip addr add "$addr/24" dev "$dev"
  ns "$name" ip link set "$dev" up
  ns inet ip link set "$P-x-$name" master br0
  ns inet ip link set "$P-x-$name" up
  # 这个内核没有 netem 时不加延迟（结果里注明）；GitHub 的 Ubuntu 主机有。
  if ! ns "$name" tc qdisc add dev "$dev" root netem delay "$WAN_DELAY" 2>/dev/null; then
    NO_DELAY=1
  fi
}

# 把 $1（内网主机）接到路由器 $2 的内网口，内网 $3（例如 10.0.1）。
attach_lan() {
  local host=$1 router=$2 net=$3
  ip link add "$P-$host-l" type veth peer name "$P-$router-$host"
  ip link set "$P-$host-l" netns "$P-$host"
  ip link set "$P-$router-$host" netns "$P-$router"
  ns "$host" ip link set "$P-$host-l" name eth0
  ns "$host" ip link set lo up
  ns "$host" ip link set eth0 up
  ns "$router" ip link set "$P-$router-$host" name "lan-$host"
  ns "$router" ip link set "lan-$host" up
  # 同一路由器后可能有两台主机：它们在同一个网段，路由器在两个口上做代理 ARP。
  if ! ns "$router" ip addr show | grep -q " $net.1/"; then
    ns "$router" ip addr add "$net.1/24" dev "lan-$host"
  else
    ns "$router" ip addr add "$net.1/32" dev "lan-$host" 2>/dev/null || true
  fi
}

# 路由器：$1 名字，$2 公网地址，$3 行为（cone 或 sym），$4 上游网关（可选）。
router() {
  local name=$1 wan=$2 mode=$3
  ip netns add "$P-$name"
  ns "$name" ip link set lo up
  ns "$name" sysctl -qw net.ipv4.ip_forward=1
  local how="masquerade"
  if [ "$mode" = sym ]; then how="masquerade random"; fi
  ns "$name" nft -f - <<EOF
table ip lab {
  chain natout { type nat hook postrouting priority 100; oifname "wan" $how; }
  chain filterfw {
    type filter hook forward priority 0; policy drop;
    ct state established,related accept
    iifname != "wan" accept
  }
  # 和真实路由器一样，外网主动发给路由器自己的包丢掉。被丢掉的包不会留下连接跟踪记录；
  # 不丢的话，打洞包先到时会在路由器上留下一条记录，占住映射，路由器只好给内网主机换端口。
  chain filterin {
    type filter hook input priority 0; policy accept;
    iifname "wan" ct state new drop
  }
}
EOF
}

server_ns() {
  ip netns add $P-srv
  ns srv ip link set lo up
  attach_inet srv 203.0.113.1 eth0
  cat >"$OUT/server.toml" <<EOF
node_id = 1
region = "lab"
http_listen = "127.0.0.1:18080"
cluster_listen = "127.0.0.1:17946"
advertise = "127.0.0.1:17946"
signal_listen = "0.0.0.0:4433"
data_dir = "$OUT/server-data"
relay_listen = "0.0.0.0:3478"
relay_advertise = "203.0.113.1:3478"
[crypto]
preset = "recommended"
EOF
  ns srv "$BIN_DIR/peerdesk-server" "$OUT/server.toml" >"$OUT/server.log" 2>&1 &
  pids+=($!)
  for _ in $(seq 40); do grep -q '服务器指纹' "$OUT/server.log" && break; sleep 0.25; done
  FP=$(grep -o '服务器指纹 [0-9a-f]*' "$OUT/server.log" | cut -d' ' -f2)
  if [ -z "$FP" ]; then echo "服务器没有启动："; cat "$OUT/server.log"; exit 1; fi
}

# 一台主机 $1 放在 $2 后面：open（直接公网 $3）、cone/sym（独占路由器）。
place() {
  local host=$1 mode=$2 pub=$3 net=$4
  ip netns add "$P-$host"
  case $mode in
    open)
      ns "$host" ip link set lo up
      attach_inet "$host" "$pub" eth0
      ;;
    cone|sym)
      router "r$host" "$pub" "$mode"
      attach_inet "r$host" "$pub" wan
      attach_lan "$host" "r$host" "$net"
      ns "$host" ip addr add "$net.2/24" dev eth0
      ns "$host" ip route add default via "$net.1"
      ;;
  esac
}

build_case() {
  local c=$1
  # 先删公网一侧的网卡：删命名空间时内核是异步回收的，同名网卡可能还在。
  for l in $(ns inet ip -o link show | awk -F': ' '{print $2}' | cut -d@ -f1 | grep "^$P-x-" | grep -v "^$P-x-srv$" || true); do
    ns inet ip link del "$l"
  done
  for n in $(ip netns list | awk '{print $1}' | grep "^$P-" | grep -v -e "^$P-inet" -e "^$P-srv" || true); do
    ip netns del "$n"
  done
  case $c in
    same)
      # 两台主机在同一个 cone 路由器后面（10.0.1.2 和 10.0.1.3）。
      router ra 203.0.113.11 cone
      attach_inet ra 203.0.113.11 wan
      ip netns add $P-a; ip netns add $P-b
      attach_lan a ra 10.0.1
      ns a ip addr add 10.0.1.2/24 dev eth0; ns a ip route add default via 10.0.1.1
      ip link add $P-b-l type veth peer name $P-ra-b
      ip link set $P-b-l netns $P-b; ip link set $P-ra-b netns $P-ra
      ns b ip link set $P-b-l name eth0; ns b ip link set lo up; ns b ip link set eth0 up
      ns ra ip link set $P-ra-b name lan-b; ns ra ip link set lan-b up
      # 一个网桥把两台主机和路由器的内网口连在一起。
      ns ra ip link add lanbr type bridge; ns ra ip link set lanbr up
      ns ra ip addr del 10.0.1.1/24 dev lan-a
      ns ra ip link set lan-a master lanbr; ns ra ip link set lan-b master lanbr
      ns ra ip addr add 10.0.1.1/24 dev lanbr
      ns b ip addr add 10.0.1.3/24 dev eth0; ns b ip route add default via 10.0.1.1
      ;;
    cgnat-*)
      # 被控端：家庭路由器（cone，WAN 100.64.0.2）→ 运营商 NAT（cone，公网 203.0.113.11）。
      router cg 203.0.113.11 cone
      attach_inet cg 203.0.113.11 wan
      router ra 100.64.0.2 cone
      ip link add $P-ra-w type veth peer name $P-cg-ra
      ip link set $P-ra-w netns $P-ra; ip link set $P-cg-ra netns $P-cg
      ns ra ip link set $P-ra-w name wan; ns ra ip addr add 100.64.0.2/24 dev wan; ns ra ip link set wan up
      ns cg ip link set $P-cg-ra name lan-ra; ns cg ip addr add 100.64.0.1/24 dev lan-ra; ns cg ip link set lan-ra up
      ns ra ip route add default via 100.64.0.1
      ip netns add $P-a
      attach_lan a ra 10.0.1
      ns a ip addr add 10.0.1.2/24 dev eth0; ns a ip route add default via 10.0.1.1
      place b "${c#cgnat-}" 203.0.113.12 10.0.2
      ;;
    *)
      place a "${c%-*}" 203.0.113.11 10.0.1
      place b "${c#*-}" 203.0.113.12 10.0.2
      ;;
  esac
}

summary="$OUT/summary.md"
{
  echo "| 被控端-控制端 | 次 | 结果 | 路径 | 应为 | 打洞方式 | 连服务器 ms | 查询 ms | 局域网 ms | 直连 ms | 中继 ms | 直连失败原因 |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|---|"
} >"$summary"

server_ns
direct=0; total=0; bad=0
for c in $CASES; do
  build_case "$c"
  for i in $(seq "$RUNS"); do
    w="$OUT/$c-$i"; mkdir -p "$w"
    ns a "$BIN_DIR/peerdesk" host --listen 0.0.0.0:0 --server 203.0.113.1:4433 --server-pin "$FP" \
      --source demo --open --once --data-dir "$w/host" >"$w/host.log" 2>&1 &
    hostpid=$!; pids+=($hostpid)
    for _ in $(seq 40); do grep -q '^设备码' "$w/host.log" && break; sleep 0.25; done
    code=$(sed -n 's/^设备码 //p' "$w/host.log" | tr -d ' ')
    result="失败"; path="-"; plan="-"; t="- | - | - | - | -"; why="-"
    if [ -n "$code" ]; then
      if timeout 30 ip netns exec "$P-b" "$BIN_DIR/peerdesk" connect --server 203.0.113.1:4433 --server-pin "$FP" \
        --device "$code" --seconds 1 --data-dir "$w/ctrl" >"$w/ctrl.log" 2>&1; then
        result="成功"
      fi
      path=$(sed -n 's/^连接路径 *//p' "$w/ctrl.log")
      plan=$(sed -n 's/^打洞方式 *//p' "$w/ctrl.log")
      t=$(sed -n 's/^连接耗时 *服务器 \([0-9]*\) ms，查询 \([0-9]*\) ms，局域网 \([0-9]*\) ms，直连 \([0-9]*\) ms，中继 \([0-9]*\) ms/\1 | \2 | \3 | \4 | \5/p' "$w/ctrl.log")
      why=$(sed -n 's/^直连失败原因 *//p' "$w/ctrl.log")
    fi
    kill "$hostpid" 2>/dev/null || true
    wait "$hostpid" 2>/dev/null || true
    # 两端都是对称型时打洞几乎不可能，应当马上走中继；其余组合都应当直连。
    want="直连"
    if [ "$c" = sym-sym ]; then want="中继"; fi
    verdict="$want"
    if [ "$result" != "成功" ] || [ "$path" != "$want" ]; then verdict="**$want（未达标）**"; bad=$((bad + 1)); fi
    echo "| $c | $i | $result | ${path:--} | $verdict | ${plan:--} | ${t:-- | - | - | - | -} | ${why:--} |" >>"$summary"
    total=$((total + 1))
    if [ "$path" = "直连" ] && [ "$result" = "成功" ]; then direct=$((direct + 1)); fi
  done
done
echo >>"$summary"
echo "直连 $direct / $total" >>"$summary"
if [ -n "${NO_DELAY:-}" ]; then
  echo "注意：内核没有 netem，公网链路没有加 $WAN_DELAY 的延迟，打洞时序比真实网络宽松。" >>"$summary"
else
  echo "公网每段链路单向延迟 $WAN_DELAY。" >>"$summary"
fi
cat "$summary"
if [ "$bad" -gt 0 ]; then
  echo "有 $bad 次不符合预期（见上表）。每次的日志在 $OUT/<组合>-<次>/。" >&2
  exit 1
fi
