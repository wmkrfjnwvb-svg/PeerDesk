#!/usr/bin/env bash
# 打包 Linux x64 客户端：tar.gz（二进制、桌面快捷方式、安装脚本、测试说明）。图标还没有单独的文件，菜单里用系统默认图标。
# 用法：scripts/package-linux.sh [输出目录，默认 dist]
# 在尽量老的发行版上构建（CI 用 Ubuntu 22.04），新的发行版都能运行（glibc 向后兼容）。
# 前置：sudo apt install libasound2-dev
set -euo pipefail
cd "$(dirname "$0")/.."
out=$(mkdir -p "${1:-dist}" && cd "${1:-dist}" && pwd)
short=$(git rev-parse --short=7 HEAD 2>/dev/null || echo dev)
name="peerdesk-$short-linux-x64"

cargo build --release -p peerdesk
stage="target/package/$name"
rm -rf "$stage" && mkdir -p "$stage"
cp target/release/peerdesk "$stage/"
strip "$stage/peerdesk" || true
cp packaging/linux/peerdesk.desktop "packaging/linux/测试说明.txt" "$stage/"
cat > "$stage/install.sh" <<'SH'
#!/usr/bin/env bash
# 装到当前用户（~/.local），不需要 root。卸载：删掉 ~/.local/bin/peerdesk 和下面两个文件。
set -euo pipefail
cd "$(dirname "$0")"
mkdir -p ~/.local/bin ~/.local/share/applications ~/.local/share/icons/hicolor/256x256/apps
install -m 755 peerdesk ~/.local/bin/peerdesk
sed "s|^Exec=.*|Exec=$HOME/.local/bin/peerdesk|" peerdesk.desktop > ~/.local/share/applications/peerdesk.desktop
if [ -f peerdesk.png ]; then cp peerdesk.png ~/.local/share/icons/hicolor/256x256/apps/peerdesk.png; fi
echo "已安装：应用菜单里搜索 PeerDesk。"
SH
chmod +x "$stage/install.sh"
echo "需要的动态库（目标机器上要有）："
ldd "$stage/peerdesk" | awk '{print "  " $1}'
tar -C target/package -czf "$out/$name.tar.gz" "$name"

# Debian / Ubuntu 安装包（.deb）：双击用“软件”安装，或 sudo apt install ./xxx.deb（会自动装依赖）。
# Ubuntu 24.04 起 libasound2 改名为 libasound2t64，两个名字都接受。
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
deb="target/package/deb"
rm -rf "$deb" && mkdir -p "$deb/DEBIAN" "$deb/usr/bin" "$deb/usr/share/applications" "$deb/usr/share/doc/peerdesk"
install -m 755 "$stage/peerdesk" "$deb/usr/bin/peerdesk"
sed "s|^Exec=.*|Exec=/usr/bin/peerdesk|" packaging/linux/peerdesk.desktop > "$deb/usr/share/applications/peerdesk.desktop"
cp "packaging/linux/测试说明.txt" "$deb/usr/share/doc/peerdesk/"
size=$(du -sk "$deb/usr" | cut -f1)
cat > "$deb/DEBIAN/control" <<CTRL
Package: peerdesk
Version: $version~$short
Architecture: amd64
Maintainer: PeerDesk <noreply@peerdesk.invalid>
Installed-Size: $size
Depends: libc6 (>= 2.35), libasound2 | libasound2t64, libxkbcommon0, libxkbcommon-x11-0, libgl1, libegl1, libx11-6, libx11-xcb1, libxcursor1, libxrandr2, libxi6, libxtst6, libxdamage1, libxfixes3, libxext6, libva2, libva-drm2, libgbm1
Recommends: fonts-noto-cjk
Section: net
Priority: optional
Homepage: https://github.com/wmkrfjnwvb-svg/PeerDesk
Description: PeerDesk remote desktop (test build)
 Low-latency peer-to-peer remote desktop over QUIC. 远程桌面测试版。
CTRL
dpkg-deb --root-owner-group --build "$deb" "$out/peerdesk_${version}~${short}_amd64.deb"
ls -l "$out"
