#!/usr/bin/env bash
# 打包 macOS 通用（arm64 + x86_64）测试版：PeerDesk.app（本机签名）放进 DMG，
# 连同服务端和测试说明。用法：scripts/package-macos.sh <输出目录> <名字>
set -euo pipefail
cd "$(dirname "$0")/.."
out=$(mkdir -p "$1" && cd "$1" && pwd)
name=$2
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
build=${GITHUB_RUN_NUMBER:-0}
targets=(aarch64-apple-darwin x86_64-apple-darwin)

rustup target add "${targets[@]}"
for t in "${targets[@]}"; do
  cargo build --release --target "$t" -p peerdesk -p peerdesk-server
done
universal=target/universal
mkdir -p "$universal"
for bin in peerdesk peerdesk-server; do
  lipo -create -output "$universal/$bin" \
    "target/aarch64-apple-darwin/release/$bin" "target/x86_64-apple-darwin/release/$bin"
  lipo -info "$universal/$bin"
done
ls -l target/*-apple-darwin/release/peerdesk target/*-apple-darwin/release/peerdesk-server "$universal"/*

stage="target/package/$name"
rm -rf "$stage" && mkdir -p "$stage"
app="$stage/PeerDesk.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$universal/peerdesk" "$app/Contents/MacOS/peerdesk"
sed -e "s/@VERSION@/$version/" -e "s/@BUILD@/$build/" packaging/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
# 本机（ad-hoc）签名：屏幕录制和辅助功能的授权记在这个程序包上。
codesign --force --deep -s - --identifier org.peerdesk.client "$app"
codesign --verify --verbose "$app"
cp "$universal/peerdesk-server" "$stage/peerdesk-server"
codesign --force -s - "$stage/peerdesk-server"
cp "packaging/macos/测试说明.txt" "$stage/"
ln -s /Applications "$stage/Applications"

rm -f "$out/$name.dmg"
hdiutil create -volname "PeerDesk" -srcfolder "$stage" -ov -format UDZO "$out/$name.dmg"
ls -l "$out"
