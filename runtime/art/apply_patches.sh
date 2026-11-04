#!/bin/bash
# Applies uxdroid's small changes to the AOSP sources. Safe to run again. Fails on any mismatch.
# The build system's patches add the linux_glibc arm64 host-cross target, inert unless a build asks
# for it (build_arm64.sh). TREE is the AOSP tree, /aosp by default; ART_PATCHES=0 leaves ART alone,
# for a tree without ART such as the Android 16 one libandroid_runtime comes from.
set -eu -o pipefail
TREE="${TREE:-/aosp}"
apply() {
    local tree=$1 patch=$2
    if git -C "$tree" apply --reverse --check "$patch" 2>/dev/null; then
        echo "already applied: $(basename "$patch")"
    else
        git -C "$tree" apply "$patch"
        echo "applied: $(basename "$patch")"
    fi
}
if [ "${ART_PATCHES:-1}" = 1 ]; then
    for patch in /work/runtime/art/patches/art-*.patch; do
        apply "$TREE/art" "$patch"
    done
fi
for patch in /work/runtime/art/patches/soong-*.patch; do
    apply "$TREE/build/soong" "$patch"
done
for patch in /work/runtime/art/patches/make-*.patch; do
    apply "$TREE/build/make" "$patch"
done
for patch in /work/runtime/art/patches/clang-*.patch; do
    apply "$TREE/prebuilts/clang/host/linux-x86" "$patch"
done
if [ "${ART_PATCHES:-1}" = 1 ]; then
    for patch in /work/runtime/art/patches/runtime-*.patch; do
        apply "$TREE/prebuilts/runtime" "$patch"
    done
    for patch in /work/runtime/art/patches/unwinding-*.patch; do
        apply "$TREE/system/unwinding" "$patch"
    done
fi
