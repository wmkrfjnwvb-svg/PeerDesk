#!/bin/bash
# 交叉编译到 Android（arm64）的环境变量。用法：source scripts/android-env.sh
# 要先跑过 scripts/android-setup.sh。C 代码（ring、libopus）用 NDK 的 clang；libopus 用 CMake，走 NDK 的工具链文件。
ANDROID_HOME="${ANDROID_HOME:-$HOME/android-sdk}"
export ANDROID_HOME
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/29.0.14206865"
export ANDROID_NDK_ROOT="$ANDROID_NDK_HOME"
_tc="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin"
# 最低 Android 10（API 29），和 android/ 工程的 minSdk 一致
export CC_aarch64_linux_android="$_tc/aarch64-linux-android29-clang"
export CXX_aarch64_linux_android="$_tc/aarch64-linux-android29-clang++"
export AR_aarch64_linux_android="$_tc/llvm-ar"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$_tc/aarch64-linux-android29-clang"
export CMAKE_TOOLCHAIN_FILE_aarch64_linux_android="$ANDROID_NDK_HOME/build/cmake/android.toolchain.cmake"
export ANDROID_ABI=arm64-v8a
export ANDROID_PLATFORM=android-29
unset _tc
