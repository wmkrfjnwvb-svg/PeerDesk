#!/bin/bash
# 装 Android 编译环境（Linux）：命令行工具、平台、编译工具、NDK，加 Rust 的 Android 目标。
# 本地和 CI 共用。装到 $ANDROID_HOME（默认 ~/android-sdk），已经装好的跳过。
#
# 版本固定（升级时改这里，一起改 android/ 里的 compileSdk、ndkVersion）：
#   平台 android-36、编译工具 36.0.0、NDK r29（29.0.14206865）。
set -euo pipefail
ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
CMDLINE_ZIP=commandlinetools-linux-16111833_latest.zip
PLATFORM="platforms;android-36"
BUILD_TOOLS="build-tools;36.0.0"
NDK="ndk;29.0.14206865"

mkdir -p "$ANDROID_HOME"
if [ ! -x "$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager" ]; then
  tmp=$(mktemp -d)
  curl -fsSL "https://dl.google.com/android/repository/$CMDLINE_ZIP" -o "$tmp/tools.zip"
  unzip -q "$tmp/tools.zip" -d "$tmp"
  mkdir -p "$ANDROID_HOME/cmdline-tools"
  rm -rf "$ANDROID_HOME/cmdline-tools/latest"
  mv "$tmp/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"
  rm -rf "$tmp"
fi
SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
# 许可证：sdkmanager 要求先接受 Google 的 Android SDK 许可协议。脚本不替人接受：
# 没接受过就停下，说明怎么做（GitHub 的 Ubuntu 机器预装了 SDK，许可已接受，CI 不走这一步）。
if [ ! -d "$ANDROID_HOME/licenses" ] || [ -z "$(ls -A "$ANDROID_HOME/licenses" 2>/dev/null)" ]; then
  echo "还没有接受 Android SDK 许可协议。请先阅读并接受：" >&2
  echo "  $SDKMANAGER --sdk_root=$ANDROID_HOME --licenses" >&2
  exit 3
fi
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" "$PLATFORM" "$BUILD_TOOLS" "platform-tools" "$NDK" > /dev/null
rustup target add aarch64-linux-android x86_64-linux-android
echo "ANDROID_HOME=$ANDROID_HOME"
echo "ANDROID_NDK_HOME=$ANDROID_HOME/ndk/29.0.14206865"
