#!/usr/bin/env bash
# PeerDesk 服务端安装脚本：需要 root，可以重复运行（升级时再跑一次即可）。
# 用法：sudo ./install.sh [--public-ip <公网IP>]
#   --public-ip  写进配置里的 relay_advertise。不给时自动探测，探测不到就报错退出。
set -euo pipefail
cd "$(dirname "$0")"

if [ "$(id -u)" -ne 0 ]; then
  echo "错误：需要 root 权限。请用 sudo ./install.sh 运行。" >&2
  exit 1
fi
if ! command -v systemctl >/dev/null 2>&1 || [ ! -d /run/systemd/system ]; then
  echo "错误：这台机器没有运行 systemd，脚本无法托管服务。" >&2
  echo "解决办法：手工运行 peerdesk-server <配置文件>，见 部署说明.md。" >&2
  exit 1
fi

public_ip=""
while [ $# -gt 0 ]; do
  case $1 in
    --public-ip) public_ip=${2:-}; shift 2 ;;
    *) echo "用法：sudo ./install.sh [--public-ip <公网IP>]" >&2; exit 2 ;;
  esac
done

# 1. 用户和文件
if ! id peerdesk >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/peerdesk --no-create-home --shell /usr/sbin/nologin peerdesk
  echo "已创建系统用户 peerdesk"
fi
install -m 0755 peerdesk-server /usr/local/bin/peerdesk-server
install -d -m 0755 /etc/peerdesk
install -m 0644 peerdesk-server.service /etc/systemd/system/peerdesk-server.service

# 2. 配置：已有就保留，不覆盖用户的修改
if [ -f /etc/peerdesk/server.toml ]; then
  echo "保留已有配置 /etc/peerdesk/server.toml（新版示例见 $PWD/server.toml）"
else
  install -m 0644 server.toml /etc/peerdesk/server.toml
fi
if grep -q '__PUBLIC_IP__' /etc/peerdesk/server.toml; then
  if [ -z "$public_ip" ] && command -v curl >/dev/null 2>&1; then
    for url in https://api.ipify.org https://ifconfig.me https://icanhazip.com; do
      public_ip=$(curl -4 -fsS --max-time 5 "$url" 2>/dev/null | tr -d '[:space:]' || true)
      case $public_ip in *[!0-9.]*) public_ip="" ;; *) ;; esac
      if [ -n "$public_ip" ]; then break; fi
    done
  fi
  if [ -z "$public_ip" ]; then
    echo "错误：探测不到公网 IP，中继地址没法填。" >&2
    echo "解决办法：sudo ./install.sh --public-ip <服务器公网IP>" >&2
    exit 1
  fi
  sed -i "s/__PUBLIC_IP__/$public_ip/" /etc/peerdesk/server.toml
  echo "中继地址 relay_advertise 已设为 $public_ip:3478（不对请改 /etc/peerdesk/server.toml）"
fi

# 3. 启动（升级时重启）
systemctl daemon-reload
systemctl enable peerdesk-server >/dev/null 2>&1
systemctl restart peerdesk-server
sleep 2
if ! systemctl is-active --quiet peerdesk-server; then
  echo "错误：服务没有起来。最近的日志：" >&2
  journalctl -u peerdesk-server -n 20 --no-pager >&2 || true
  echo "解决办法：根据上面的错误修改 /etc/peerdesk/server.toml，再 sudo systemctl restart peerdesk-server。" >&2
  exit 1
fi

# 4. 打印指纹（来自本次启动的日志）
fp=""
for _ in 1 2 3 4 5; do
  fp=$(journalctl -u peerdesk-server -n 50 --no-pager -o cat 2>/dev/null | grep -o '服务器指纹 [0-9a-f]*' | tail -1 | cut -d' ' -f2 || true)
  if [ -n "$fp" ]; then break; fi
  sleep 1
done
echo
echo "安装完成，服务已启动并设为开机自启。"
if [ -n "$fp" ]; then
  echo "服务器指纹：$fp"
  echo "客户端填入：服务器地址 <公网IP>:4433 和上面的指纹。"
else
  echo "没能从日志里读到指纹，请手工查看：journalctl -u peerdesk-server | grep 指纹"
fi
echo "别忘了放行防火墙/云安全组的 UDP 4433 和 UDP 3478，见 部署说明.md。"
