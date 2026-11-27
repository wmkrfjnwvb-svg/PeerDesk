#!/usr/bin/env bash
# 打包 macOS 通用（arm64 + x86_64）测试版：PeerDesk.app（本机签名）和测试说明放进 DMG。
# 服务端单独打一个压缩包（普通用户用不到服务端，不放进客户端安装包）。
# 用法：scripts/package-macos.sh <输出目录> <名字> [服务端输出目录，默认 <输出目录>-server]
set -euo pipefail
cd "$(dirname "$0")/.."
out=$(mkdir -p "$1" && cd "$1" && pwd)
name=$2
server_out=$(mkdir -p "${3:-$1-server}" && cd "${3:-$1-server}" && pwd)
server_name=${name/peerdesk-/peerdesk-server-}
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
# 图标由 scripts/make-icons.py 生成（苹果图标网格），Info.plist 里的 CFBundleIconFile 指向它
cp packaging/icons/peerdesk.icns "$app/Contents/Resources/peerdesk.icns"
sed -e "s/@VERSION@/$version/" -e "s/@BUILD@/$build/" packaging/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"
# 本机（ad-hoc）签名。屏幕录制和辅助功能的授权按“指定要求”（designated requirement）认程序：
# ad-hoc 签名默认的要求是代码哈希，每个新版本都不同，于是覆盖安装后系统里那一条授权对不上新程序，
# 用户得先删掉旧的 PeerDesk 再重新授权。这里把要求改成只认程序标识，升级后授权保留。
# 代价：同标识的其他 ad-hoc 程序也能满足这条要求。这是没有 Apple 开发者证书时的过渡做法，
# 拿到 Developer ID 后换成证书签名（要求变成“标识 + Apple 签发的团队证书”），见 docs/research/I-release.md。
codesign --force --deep -s - --identifier org.peerdesk.client \
  -r='designated => identifier "org.peerdesk.client"' "$app"
codesign -d -r- "$app"
codesign --verify --verbose "$app"
cp "packaging/macos/测试说明.txt" "$stage/"
ln -s /Applications "$stage/Applications"

rm -f "$out/$name.dmg"
hdiutil create -volname "PeerDesk" -srcfolder "$stage" -ov -format UDZO "$out/$name.dmg"
ls -l "$out"

# 服务端：通用二进制（本机签名）加说明，单独一个压缩包
server_stage="target/package/$server_name"
rm -rf "$server_stage" && mkdir -p "$server_stage"
cp "$universal/peerdesk-server" "$server_stage/peerdesk-server"
codesign --force -s - "$server_stage/peerdesk-server"
cp packaging/server-README.txt "$server_stage/README.txt"
tar -C target/package -czf "$server_out/$server_name.tar.gz" "$server_name"
ls -l "$server_out"
