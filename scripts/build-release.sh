#!/usr/bin/env bash
# 构建两个发布二进制。先构建管理页面，它会被嵌进服务端。
set -euo pipefail
cd "$(dirname "$0")/.."

(cd web && npm ci --no-audit --no-fund && npm run build)
cargo build --release -p peerdesk -p peerdesk-server
ls -lh target/release/peerdesk target/release/peerdesk-server
