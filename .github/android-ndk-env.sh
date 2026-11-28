#!/usr/bin/env bash
# Prints the environment Cargo needs to build for Android (aarch64) with the
# NDK on a GitHub runner, for "$GITHUB_ENV". Elsewhere, point
# ANDROID_NDK_LATEST_HOME at an NDK (r26 or newer) first.
set -euo pipefail
ndk=${ANDROID_NDK_LATEST_HOME:-${ANDROID_NDK_HOME:-}}
bin=$ndk/toolchains/llvm/prebuilt/linux-x86_64/bin
if [ -z "$ndk" ] || [ ! -x "$bin/aarch64-linux-android24-clang" ]; then
    echo "Android NDK not found (set ANDROID_NDK_LATEST_HOME)" >&2
    exit 1
fi
# The linker for the app, and the C compiler for ring (rustls' crypto).
echo "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$bin/aarch64-linux-android24-clang"
echo "CC_aarch64_linux_android=$bin/aarch64-linux-android24-clang"
echo "AR_aarch64_linux_android=$bin/llvm-ar"
