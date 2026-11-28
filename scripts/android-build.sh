#!/bin/bash
# 编 Android 控制端 APK（debug）：先编 Rust 核心 .so，拷进 jniLibs，再跑 Gradle。
# 前提：已跑过 scripts/android-setup.sh（SDK、NDK、Rust 的 Android 目标）。失败时说明原因并以非零退出，不静默。
# 用法：scripts/android-build.sh [--skip-rust]   （--skip-rust：.so 已经编好，只跑 Gradle）
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
fail() { echo "错误：$*" >&2; exit 1; }

SKIP_RUST=0
[ "${1:-}" = "--skip-rust" ] && SKIP_RUST=1

# shellcheck source=android-env.sh
source "$ROOT/scripts/android-env.sh"
[ -d "$ANDROID_HOME/platforms" ] || fail "找不到 Android SDK（$ANDROID_HOME）。先跑 scripts/android-setup.sh。"
[ -d "$ANDROID_NDK_HOME" ] || fail "找不到 NDK（$ANDROID_NDK_HOME）。先跑 scripts/android-setup.sh。"

SO_SRC="$ROOT/target-android/aarch64-linux-android/release/libpeerdesk_mobile.so"
if [ "$SKIP_RUST" = 0 ]; then
  echo "== 编 Rust 核心 =="
  CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$ROOT/target-android" \
    cargo build -p peerdesk-mobile --target aarch64-linux-android --release \
    || fail "Rust 核心编译失败（见上面的 cargo 输出）。"
fi
[ -f "$SO_SRC" ] || fail "没有找到 $SO_SRC。去掉 --skip-rust 重新编。"

JNI_DIR="$ROOT/android/app/src/main/jniLibs/arm64-v8a"
mkdir -p "$JNI_DIR"
cp "$SO_SRC" "$JNI_DIR/libpeerdesk_mobile.so"
echo "已拷贝 .so（$(du -h "$JNI_DIR/libpeerdesk_mobile.so" | cut -f1)）到 $JNI_DIR"

# local.properties 不进仓库，sdk.dir 每次由这里写
printf 'sdk.dir=%s\n' "$ANDROID_HOME" > "$ROOT/android/local.properties"

echo "== Gradle assembleDebug =="
(cd "$ROOT/android" && ./gradlew --no-daemon assembleDebug) \
  || fail "Gradle 构建失败（见上面的输出）。首次构建要联网下载依赖；离线时会失败。"

APK="$ROOT/android/app/build/outputs/apk/debug/app-debug.apk"
[ -f "$APK" ] || fail "Gradle 说成功了，但没有找到 $APK。"
echo "APK：$APK（$(du -h "$APK" | cut -f1)）"
