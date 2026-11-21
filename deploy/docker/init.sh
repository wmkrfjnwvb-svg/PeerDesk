#!/usr/bin/env bash
# 生成示例集群的密钥（.env）和各节点的配置（run/node*.toml）。可以重复运行：.env 已有就沿用密钥。
# 用法：deploy/docker/init.sh [公网IP]    不写公网 IP 时自动探测；探测不到会报错并说明怎么办。
set -euo pipefail
cd "$(dirname "$0")"

rand() { head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n'; }

if [ ! -f .env ]; then
  public_ip="${1:-}"
  if [ -z "$public_ip" ]; then
    public_ip=$(curl -fsS --max-time 5 https://api.ipify.org 2>/dev/null || true)
  fi
  if [ -z "$public_ip" ]; then
    echo "错误：探测不到公网 IP。" >&2
    echo "解决办法：手工指定，例如 deploy/docker/init.sh 1.2.3.4（局域网试用就填本机局域网 IP）。" >&2
    exit 1
  fi
  umask 077
  cat > .env <<ENV
PEERDESK_PUBLIC_IP=$public_ip
PEERDESK_CLUSTER_SECRET=$(rand)
PEERDESK_BILLING_SECRET=$(rand)
PEERDESK_ADMIN_TOKEN=$(rand)
ENV
  echo "已生成 .env（权限 0600，里面是密钥，不要提交到 git）。"
elif [ -n "${1:-}" ]; then
  echo "提示：.env 已存在，忽略参数 $1。要换公网 IP 请直接改 .env 里的 PEERDESK_PUBLIC_IP。" >&2
fi

set -a
# shellcheck disable=SC1091
. ./.env
set +a
mkdir -p run
for n in 1 2 3; do
  sed -e "s|\${PEERDESK_PUBLIC_IP}|$PEERDESK_PUBLIC_IP|g" \
      -e "s|\${PEERDESK_CLUSTER_SECRET}|$PEERDESK_CLUSTER_SECRET|g" \
      -e "s|\${PEERDESK_BILLING_SECRET}|$PEERDESK_BILLING_SECRET|g" \
      -e "s|\${PEERDESK_ADMIN_TOKEN}|$PEERDESK_ADMIN_TOKEN|g" \
      "node$n.toml" > "run/node$n.toml"
  # 容器里是 nonroot（65532），要能读到配置；密钥不会因此泄露给宿主机以外的人。
  chmod 0644 "run/node$n.toml"
done
echo "已生成 run/node1.toml run/node2.toml run/node3.toml，公网 IP：$PEERDESK_PUBLIC_IP"
echo "下一步：docker compose up -d --build"
