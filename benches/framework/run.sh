#!/bin/bash
# Measures startup and memory of AOSP's framework on uxdroid's ART, against milestone 3's own
# platform, cold (page cache evicted before every run) and warm. Runs in the dev service, whose
# /tmp is on the container's disk-backed overlay, not tmpfs:
#
#   docker compose run --rm -T dev bash /work/benches/framework/run.sh
#
# RUNS sets the measured runs per row (default 30). RustDesk is measured by rustdesk.sh in sandbox.
set -u -o pipefail
WORK=$(mktemp -d)
RUNS="${RUNS:-30}"
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
BIN="${CARGO_TARGET_DIR:-/cache/target}/release"
MEASURE=(python3 -I /work/benches/framework/measure.py)
fail() { echo "FAIL: $*"; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-platform \
    -p uxdroid-testkit --manifest-path /work/Cargo.toml || fail "build"
LIBRARY="$BIN/libuxdroid_system.so"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
source /work/runtime/art/test_lib.sh
trap 'kill "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT
start_compositor
[ "$(findmnt -n -o FSTYPE -T "$WORK")" != tmpfs ] || fail "$WORK is on tmpfs; the numbers would not be from disk"

# Milestone 3's sample, as runtime/art/app_smoke.sh builds it.
"$TOOLCHAIN/bin/x86_64-linux-android26-clang++" -shared -fPIC -O2 -static-libstdc++ \
    /work/runtime/art/samples/first/first_native.cpp -o "$WORK/libfirstnative.so" || fail "NDK build"
"$BIN/make-first-apk" "$RUNTIME/samples/first/classes.dex" "$WORK/libfirstnative.so" "$WORK/first.apk" || fail "APK"

# Everything uxdroid reads: the runtime, our binaries, the APKs and the app data. System libraries
# of the container (glibc, Mesa, Wayland) are not evicted.
EVICT=(--evict "$RUNTIME" --evict "$WORK")
for file in uxdroid libuxdroid_system.so libuxdroid_nativebridge.so libuxdroid_platform.so; do
    EVICT+=(--evict "$BIN/$file")
done

CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
FRAMEWORK_VM=("$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}")
# The sample installed as uxdroid install leaves it: compiled, its manifest recorded, its default bodies.
UXDROID_RUNTIME="$RUNTIME" "$BIN/uxdroid" install "$RUNTIME/samples/framework/app.apk" --data "$ANDROID_DATA" \
    || fail "install"
INSTALLED="$ANDROID_DATA/app/com.example.framework/base.apk"

for mode in --cold ""; do
    # 1. Framework start and a TextView drawn, to process exit. Memory is sampled at the TextView.
    "${MEASURE[@]}" --label "FrameworkSmoke (framework + TextView)" --runs "$RUNS" $mode "${EVICT[@]}" \
        --marker "^view [0-9]+x[0-9]+ " --until-exit \
        --expect "^string OK$" --expect "^sdk 36 abi x86_64$" --expect "^text [0-9]{3}" -- \
        "${FRAMEWORK_VM[@]}" -cp "$CP:$RUNTIME/samples/framework/smoke.jar" FrameworkSmoke "$RUNTIME" "$LIBRARY" \
        || exit 1
    # 2. The framework's sample APK to its first frame posted to Wayland, from the APK as every start
    # was before uxdroid install prepared apps, then installed.
    "${MEASURE[@]}" --label "Framework sample APK to first frame, from the APK" --runs "$RUNS" $mode "${EVICT[@]}" \
        --marker "first frame posted" --expect "^activity resumed com\.example\.framework\.MainActivity$" -- \
        "${FRAMEWORK_VM[@]}" -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$LIBRARY" \
        "$RUNTIME/samples/framework/app.apk" || exit 1
    "${MEASURE[@]}" --label "Framework sample APK to first frame, installed" --runs "$RUNS" $mode "${EVICT[@]}" \
        --marker "first frame posted" --expect "^activity resumed com\.example\.framework\.MainActivity$" -- \
        "${FRAMEWORK_VM[@]}" -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$LIBRARY" "$INSTALLED" || exit 1
    # 4. Milestone 3: uxdroid run on our own 8-class platform, to its first frame.
    "${MEASURE[@]}" --label "Milestone 3 platform, uxdroid run to first frame" --runs "$RUNS" $mode "${EVICT[@]}" \
        --marker "frame presented" --expect "activity created in com\.example\.first" -- \
        "$BIN/uxdroid" run "$WORK/first.apk" || exit 1
done

size() { python3 -I -c 'import os, sys
root = sys.argv[1]
files = [os.path.join(root, line.strip()) for line in open(sys.argv[2]) if line.strip()]
print(f"{sum(os.path.getsize(f) for f in files if os.path.isfile(f)) / 1e6:.0f} MB in {len(files)} files")' "$@"; }
echo "installed: framework adds $(size "$RUNTIME" "$RUNTIME/framework/framework-files.txt"); whole runtime $(du -sm "$RUNTIME" | cut -f1) MiB"
echo "storage: $WORK on $(findmnt -n -o FSTYPE -T "$WORK"), $RUNTIME on $(findmnt -n -o FSTYPE -T "$RUNTIME")"
