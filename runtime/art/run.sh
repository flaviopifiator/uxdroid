#!/bin/bash
# Shared environment for running the packaged ART: nothing from an Android system or AOSP tree.
# ARCH is the package's CPU as ART names it, x86_64 or arm64, whose runtime is /runtime/art-arm64-host.
ARCH="${ARCH:-x86_64}"
if [ "$ARCH" = arm64 ]; then
    RUNTIME="${RUNTIME:-/runtime/art-arm64-host}"
else
    RUNTIME="${RUNTIME:-/runtime/art}"
fi
# Programs of the arm64 package run under qemu-aarch64 on a machine of another CPU.
RUNNER=()
[ "$ARCH" = arm64 ] && [ "$(uname -m)" != aarch64 ] && RUNNER=(qemu-aarch64)
# The dex2oat that compiles ahead of time on the build machine: the package's own for x86_64, and
# for arm64 the build machine's, built beside it by build_arm64.sh with arm64 code generation.
if [ "$ARCH" = arm64 ]; then
    DEX2OAT=(env LD_LIBRARY_PATH=/aosp/out-arm64/host/linux-x86/lib64 /aosp/out-arm64/host/linux-x86/bin/dex2oat64)
else
    DEX2OAT=("$RUNTIME/bin/dex2oat64")
fi
export ANDROID_ROOT="$RUNTIME"
export ANDROID_ART_ROOT="$RUNTIME"
export ANDROID_I18N_ROOT="$RUNTIME/com.android.i18n"
export ANDROID_TZDATA_ROOT="$RUNTIME/com.android.tzdata"
export ANDROID_DATA="${ANDROID_DATA:-/tmp/art-data}"
export LD_LIBRARY_PATH="$RUNTIME/lib64"
mkdir -p "$ANDROID_DATA/dalvik-cache/$ARCH"
JARS=(core-oj core-libart okhttp bouncycastle apache-xml core-icu4j conscrypt)
BOOT=""
for jar in "${JARS[@]}"; do
    BOOT="$BOOT:$RUNTIME/javalib/$jar.jar"
done
BOOT="${BOOT:1}"
# -Xnorelocate keeps the boot image at its compiled address, so its pages stay clean and shared
# between processes. It cut private dirty memory by 41% and start time by 25%. The heap limits
# return freed memory sooner, which cut a garbage-heavy program from 72 to 50 MB at no time cost.
ART_FLAGS=(-Xbootclasspath:"$BOOT" -Xbootclasspath-locations:"$BOOT" -Ximage:"$RUNTIME/framework/boot.art"
    -Xnorelocate -XX:HeapMinFree=256k -XX:HeapMaxFree=1m
    # Large arrays cost far less in the regular heap: a garbage-heavy program went from 73 to 46 ms.
    -XX:LargeObjectSpace=disabled -XX:ForegroundHeapGrowthMultiplier=1.0)
