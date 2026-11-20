#!/usr/bin/env bash
# 打包 Windows x64 测试版：ZIP（两个 exe 加测试说明），有 Inno Setup 时再做一个安装程序。
# 用法：scripts/package-windows.sh <输出目录> <名字>   在 Windows 的 Git Bash 里运行。
set -euo pipefail
cd "$(dirname "$0")/.."
out=$(mkdir -p "$1" && cd "$1" && pwd)
name=$2
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

cargo build --release -p peerdesk -p peerdesk-server
stage="target/package/$name"
rm -rf "$stage" && mkdir -p "$stage"
cp target/release/peerdesk.exe target/release/peerdesk-server.exe "$stage/"
cp "packaging/windows/测试说明.txt" "$stage/"
ls -l "$stage"

(cd target/package && 7z a -tzip -bso0 "$out/$name.zip" "$name")

iscc=$(command -v iscc || true)
for candidate in "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" "/c/Program Files/Inno Setup 6/ISCC.exe"; do
  if [ -z "$iscc" ] && [ -f "$candidate" ]; then iscc=$candidate; fi
done
if [ -n "$iscc" ]; then
  "$iscc" -Q "/DAppVersion=$version" "/DSourceDir=$(cygpath -w "$PWD/$stage")" \
    "/DOutputDir=$(cygpath -w "$out")" "/DOutputName=$name-setup" packaging/windows/peerdesk.iss
else
  echo "没有找到 Inno Setup（iscc），跳过安装程序。"
fi
ls -l "$out"
