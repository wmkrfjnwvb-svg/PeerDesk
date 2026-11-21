#!/usr/bin/env bash
# 打包 Linux 服务端：静态链接（musl），任何发行版都能运行。
# 产出 dist/peerdesk-server-<短提交>-linux-x64.tar.gz，里面有二进制、配置、systemd 单元、安装脚本和部署说明。
# 用法：scripts/package-server-linux.sh [输出目录，默认 dist]
# 前置：rustup target add x86_64-unknown-linux-musl；apt install musl-tools。
# 管理页面（web/dist）有就嵌入，没有就用占位页面；要完整页面先 (cd web && npm ci && npm run build)。
set -euo pipefail
cd "$(dirname "$0")/.."
out=$(mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd)
target=x86_64-unknown-linux-musl
short=$(git rev-parse --short=7 HEAD 2>/dev/null || echo dev)
name="peerdesk-server-$short-linux-x64"

if ! rustup target list --installed | grep -qx "$target"; then
  echo "错误：缺少 Rust 目标 $target。" >&2
  echo "解决办法：rustup target add $target" >&2
  exit 1
fi
# 服务端不链接视频编解码（peerdesk-net 不开 media 特性），只有 ring 要编译 C/汇编，
# 用 musl-tools 的 musl-gcc 即可（Ubuntu/Debian：apt install musl-tools）。
if ! command -v musl-gcc >/dev/null 2>&1; then
  echo "错误：缺少 musl-gcc。" >&2
  echo "解决办法：sudo apt install musl-tools" >&2
  exit 1
fi
export CC_x86_64_unknown_linux_musl=musl-gcc
cargo build --release --target "$target" -p peerdesk-server
bin="target/$target/release/peerdesk-server"

# 确认真的是静态的：动态可执行文件有解释器，静态的没有。
if command -v readelf >/dev/null 2>&1 && readelf -l "$bin" | grep -q 'Requesting program interpreter'; then
  echo "错误：$bin 不是静态链接，换了发行版可能跑不起来。" >&2
  exit 1
fi

stage="target/package/$name"
rm -rf "$stage" && mkdir -p "$stage"
cp "$bin" "$stage/peerdesk-server"
cp packaging/linux-server/server.toml packaging/linux-server/peerdesk-server.service \
   packaging/linux-server/install.sh "packaging/linux-server/部署说明.md" "$stage/"
chmod 0755 "$stage/install.sh" "$stage/peerdesk-server"

mkdir -p "$out"
tar -C target/package -czf "$out/$name.tar.gz" "$name"
ls -l "$bin" "$out/$name.tar.gz"
