#!/usr/bin/env bash
# 打包 Windows x64 测试版：ZIP（客户端加测试说明），有 Inno Setup 时再做一个安装程序。
# 服务端单独打一个 ZIP（普通用户用不到服务端，不放进客户端安装包）。
# 用法：scripts/package-windows.sh <输出目录> <名字> [服务端输出目录，默认 <输出目录>-server]
# 在 Windows 的 Git Bash 里运行。
set -euo pipefail
cd "$(dirname "$0")/.."
out=$(mkdir -p "$1" && cd "$1" && pwd)
name=$2
server_out=$(mkdir -p "${3:-$1-server}" && cd "${3:-$1-server}" && pwd)
server_name=${name/peerdesk-/peerdesk-server-}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

cargo build --release -p peerdesk -p peerdesk-server
stage="target/package/$name"
rm -rf "$stage" && mkdir -p "$stage"
cp target/release/peerdesk.exe "$stage/"
cp "packaging/windows/测试说明.txt" "$stage/"
ls -l "$stage"

(cd target/package && 7z a -tzip -bso0 "$out/$name.zip" "$name")

iscc=$(command -v iscc || true)
for candidate in "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" "/c/Program Files/Inno Setup 6/ISCC.exe"; do
  if [ -z "$iscc" ] && [ -f "$candidate" ]; then iscc=$candidate; fi
done
if [ -n "$iscc" ]; then
  # Git Bash 会把以 / 开头的参数当成路径改写，对 iscc 的 /D 参数要关掉。
  MSYS2_ARG_CONV_EXCL='*' "$iscc" -Q "/DAppVersion=$version" "/DSourceDir=$(cygpath -w "$PWD/$stage")" \
    "/DOutputDir=$(cygpath -w "$out")" "/DOutputName=$name-setup" "$(cygpath -w "$PWD/packaging/windows/peerdesk.iss")"
else
  echo "没有找到 Inno Setup（iscc），跳过安装程序。"
fi
ls -l "$out"

# 服务端：单独一个 ZIP
server_stage="target/package/$server_name"
rm -rf "$server_stage" && mkdir -p "$server_stage"
cp target/release/peerdesk-server.exe "$server_stage/"
cp packaging/server-README.txt "$server_stage/README.txt"
(cd target/package && 7z a -tzip -bso0 "$server_out/$server_name.zip" "$server_name")
ls -l "$server_out"
