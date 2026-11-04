#!/bin/bash
# Builds ART's native half for an arm64 glibc host inside the art-builder service: the same pinned
# master-art tree as build.sh, cross-compiled from x86_64 as the linux_glibc arm64 host-cross target
# the build system patches add, against the sysroot of sysroot_arm64.sh. Its own out directory,
# out-arm64, so the x86_64 build in out/ is never touched. The build machine's own dex2oat, built
# there too, gets arm64 code generation to compile the arm64 boot image ahead of time.
# JOBS sets the parallelism, TARGETS what to build (Soong output paths or module names).
set -u
cd /aosp
bash /work/runtime/art/apply_patches.sh || exit 1
[ -d prebuilts/gcc/linux-x86/host/aarch64-linux-glibc2.31-10/sysroot ] \
    || bash /work/runtime/art/sysroot_arm64.sh /aosp || exit 1
# libcap for arm64 comes from the sysroot, where the platform SDK patch's prebuilt names it.
mkdir -p prebuilts/runtime/mainline/platform/sdk/linux_glibc/arm64/lib
cp prebuilts/gcc/linux-x86/host/aarch64-linux-glibc2.31-10/sysroot/usr/lib/aarch64-linux-gnu/libcap.a \
    prebuilts/runtime/mainline/platform/sdk/linux_glibc/arm64/lib/
[ -e prebuilts/clang/host/linux-x86/clang-r547379/lib/aarch64-unknown-linux-gnu/libc++.so ] \
    || bash /work/runtime/art/libcxx_arm64.sh /aosp || exit 1

MIN_FREE_KIB=$((60 * 1024 * 1024))
JOBS="${JOBS:-6}"
# What the arm64 runtime ships, as runtime/art/package.sh copies for x86_64, and the build machine's
# dex2oat and oatdump for the boot image.
ARM64=out-arm64/host/linux-arm64
DEFAULT_TARGETS="$ARM64/bin/dalvikvm64 $ARM64/bin/dex2oat64"
for library in art artbase art-disassembler artpalette androidio base c++ dexfile dt_socket expat-host \
    javacore jdwp log lz4 lzma nativebridge nativehelper \
    nativeloader npt openjdk openjdkjvm openjdkjvmti profile sigchain sqlite unwindstack z-host ziparchive; do
    DEFAULT_TARGETS="$DEFAULT_TARGETS $ARM64/lib64/lib$library.so"
done
DEFAULT_TARGETS="$DEFAULT_TARGETS out-arm64/host/linux-x86/bin/dex2oat64 out-arm64/host/linux-x86/bin/oatdump"
TARGETS="${TARGETS:-$DEFAULT_TARGETS}"

(
    while sleep 30; do
        free_kib=$(df -k --output=avail /aosp | tail -1)
        if [ "$free_kib" -lt "$MIN_FREE_KIB" ]; then
            echo "disk guard: less than 60 GiB free, stopping the build" >&2
            pkill -f soong_ui
            pkill ninja
            exit 1
        fi
    done
) &
guard=$!

export OUT_DIR=out-arm64
export HOST_CROSS_OS=linux_glibc HOST_CROSS_ARCH=arm64
export SOONG_ALLOW_MISSING_DEPENDENCIES=true TARGET_BUILD_UNBUNDLED=true
# arm64 for the arm64 runtime's JIT and dex2oat, and for the build machine's dex2oat. The scheduler
# needs arm beside arm64, and x86_64 alone does not compile: its code generator shares nodes with x86.
export ART_HOST_CODEGEN_ARCHS="${ART_HOST_CODEGEN_ARCHS:-arm arm64 x86 x86_64}"
set +u
source build/envsetup.sh
lunch armv8-trunk_staging-eng
set -u
# shellcheck disable=SC2086
build/soong/soong_ui.bash --make-mode --soong-only -j"$JOBS" $TARGETS
status=$?

kill "$guard" 2>/dev/null
exit "$status"
