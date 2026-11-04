#!/bin/bash
# Builds Android 16's libandroid_runtime and ICU for an arm64 glibc host inside the android16-builder
# service, from the partial android-16.0.0_r1 tree sync.sh made: the same linux_glibc arm64
# host-cross target as runtime/art/build_arm64.sh, with that tree's own Soong patched the same way,
# the same sysroot and a libc++ built from that tree's clang. Output in /a16/out-arm64.
# JOBS sets the parallelism, TARGETS what to build.
set -u
cd /a16
# The build system patches are the master-art tree's; they apply to this tree unchanged.
PATCHES=/work/runtime/art/patches
apply() {
    local tree=$1 patch=$2
    if git -C "$tree" apply --reverse --check "$patch" 2>/dev/null; then
        echo "already applied: $(basename "$patch")"
    else
        git -C "$tree" apply "$patch" || exit 1
        echo "applied: $(basename "$patch")"
    fi
}
apply build/soong "$PATCHES/soong-0001-linux-glibc-arm64-host-cross.patch"
apply build/make "$PATCHES/make-0001-linux-glibc-host-cross.patch"
apply prebuilts/clang/host/linux-x86 "$PATCHES/clang-0001-linux-glibc-arm64-runtimes.patch"
# This partial tree's own: what a full checkout has and this one leaves out.
for patch in /work/runtime/framework/android16/patches/soong-*.patch; do
    apply build/soong "$patch"
done
# Only C and C++ is built here, and the framework's Java needs the SDK prebuilts and modules this tree
# leaves out, which Soong cannot analyze: its Java, app and API stub modules are removed from every
# blueprint file of frameworks/base, and build/make's system images with them, starting from the
# file at the tag each time.
for project in frameworks/base build/make; do
    git -C "$project" ls-files -- '*.bp' | while read -r bp; do
        [ -e "$project/$bp" ] || continue
        git -C "$project" show "HEAD:$bp" > "$project/$bp.tag"
        python3 /work/runtime/framework/android16/strip_java.py "$project/$bp.tag" "$project/$bp"
        rm "$project/$bp.tag"
    done
done
git -C build/soong checkout -q fsgen/Android.bp
grep -q 'name: "soong-api"' frameworks/base/api/Android.bp || exit 1
[ -d prebuilts/gcc/linux-x86/host/aarch64-linux-glibc2.31-10/sysroot ] \
    || bash /work/runtime/art/sysroot_arm64.sh /a16 || exit 1
[ -e prebuilts/clang/host/linux-x86/clang-r547379/lib/aarch64-unknown-linux-gnu/libc++.so ] \
    || bash /work/runtime/art/libcxx_arm64.sh /a16 || exit 1

MIN_FREE_KIB=$((60 * 1024 * 1024))
JOBS="${JOBS:-6}"
ARM64=out-arm64/host/linux-arm64
DEFAULT_TARGETS="$ARM64/lib64/libandroid_runtime.so $ARM64/lib64/libicu.so $ARM64/lib64/libicuuc-host.so"
DEFAULT_TARGETS="$DEFAULT_TARGETS $ARM64/lib64/libicui18n-host.so $ARM64/lib64/libicu_jni.so $ARM64/lib64/libjavacrypto.so"
TARGETS="${TARGETS:-$DEFAULT_TARGETS}"

(
    while sleep 30; do
        free_kib=$(df -k --output=avail /a16 | tail -1)
        if [ "$free_kib" -lt "$MIN_FREE_KIB" ]; then
            echo "disk guard: less than 60 GiB free, stopping the build" >&2
            pkill -f soong_ui
            pkill ninja
            exit 1
        fi
    done
) &
guard=$!

export OUT_DIR=out-arm64 TARGET_RELEASE=bp2a
export HOST_CROSS_OS=linux_glibc HOST_CROSS_ARCH=arm64
export SOONG_ALLOW_MISSING_DEPENDENCIES=true TARGET_BUILD_UNBUNDLED=true
# No ART in this tree, and no Java is built from it.
export WITH_DEXPREOPT=false
set +u
source build/envsetup.sh
# android-16.0.0_r1 is the bp2a release.
lunch "${LUNCH:-aosp_arm64-bp2a-eng}"
set -u
# shellcheck disable=SC2086
build/soong/soong_ui.bash --make-mode --soong-only -j"$JOBS" $TARGETS
status=$?

kill "$guard" 2>/dev/null
exit "$status"
