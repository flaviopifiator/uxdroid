#!/bin/bash
# Measures `uxdroid install` and RustDesk 1.5.1 to its first Flutter frame on AOSP's framework.
# RustDesk's native code is untrusted, so this runs only in the sandbox service. The sandbox's /tmp
# is tmpfs, so the app data goes to a Docker volume mounted at /var/tmp, which is on disk:
#
#   docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
#   UXDROID_SAMPLES=<directory with the APK> docker compose run --rm -T \
#       -v uxdroid-framework-bench:/var/tmp sandbox bash /work/benches/framework/rustdesk.sh
#
# RUNS sets the measured runs per row (default 30); RUSTDESK_APK names another APK.
set -u -o pipefail
WORK=$(mktemp -d)
# Removed however the script ends, a failure before the full trap below included.
trap 'rm -rf "$WORK"' EXIT
RUNS="${RUNS:-30}"
APK="${RUSTDESK_APK:-/samples/rustdesk-1.5.1-universal-signed.apk}"
PACKAGE=com.carriez.flutter_hbb
TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
MEASURE=(python3 -I /work/benches/framework/measure.py)
fail() { echo "FAIL: $*"; exit 1; }

for file in uxdroid libuxdroid_system.so libuxdroid_nativebridge.so; do
    [ -e "$TARGET/$file" ] || fail "no $TARGET/$file: build the release libraries in the dev service first"
done
[ -r "$APK" ] || fail "no APK at $APK: set UXDROID_SAMPLES to its directory, or RUSTDESK_APK"

BENCH=/var/tmp/uxdroid-framework-bench
STORAGE=$(findmnt -n -o FSTYPE -T /var/tmp)
if [ "$STORAGE" = tmpfs ] || ! mkdir -p "$BENCH" 2>/dev/null; then
    BENCH="$WORK/bench"
    STORAGE=tmpfs
    mkdir -p "$BENCH"
    echo "WARNING: no disk volume at /var/tmp; app data is on tmpfs, so its reads cost no disk I/O and"
    echo "         its pages count as private dirty memory. Mount one with -v uxdroid-framework-bench:/var/tmp."
fi
# Untrusted state from earlier runs never carries over.
rm -rf "${BENCH:?}"/*
DATA="$BENCH/data"
# The APK too is read from that storage: UXDROID_SAMPLES may point at a tmpfs directory.
mkdir -p "$BENCH/apk"
cp "$APK" "$BENCH/apk/" || fail "cannot copy $APK"
APK="$BENCH/apk/$(basename "$APK")"

source /work/runtime/art/run.sh
export ANDROID_DATA="$DATA"
EVICT=(--evict "$RUNTIME" --evict "$APK" --evict "$BENCH")
for file in uxdroid libuxdroid_system.so libuxdroid_nativebridge.so; do
    EVICT+=(--evict "$TARGET/$file")
done

# uxdroid install, into an emptied data directory each time: without a framework it only extracts the
# native libraries; with one it also compiles the DEX, records the manifest and writes default bodies.
INSTALL=(sh -c 'rm -rf "$1" && mkdir -p "$1/dalvik-cache/x86_64" && exec "$2" install "$3" --data "$1"' install
    "$BENCH/install" "$TARGET/uxdroid" "$APK")
for mode in --cold ""; do
    UXDROID_RUNTIME=/nonexistent "${MEASURE[@]}" --label "uxdroid install RustDesk, libraries only" --runs "$RUNS" \
        $mode "${EVICT[@]}" --until-exit \
        --marker "^installed $PACKAGE: [1-9][0-9]* native libraries for x86_64 in " -- "${INSTALL[@]}" || exit 1
    UXDROID_RUNTIME="$RUNTIME" "${MEASURE[@]}" --label "uxdroid install RustDesk, prepared" --runs "$RUNS" \
        $mode "${EVICT[@]}" --until-exit --marker "^prepared $PACKAGE in " -- "${INSTALL[@]}" || exit 1
done
UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$APK" --data "$DATA" | tee "$WORK/install"
mkdir -p "$DATA/dalvik-cache/x86_64"
echo "install size: APK $(du -m "$APK" | cut -f1) MiB; data after install $(du -sm "$DATA" | cut -f1) MiB, of which" \
    "libraries $(du -sm "$DATA/data/$PACKAGE/lib" | cut -f1) MiB and code directory $(du -sm "$DATA/app/$PACKAGE" | cut -f1) MiB:"
du -k "$DATA/app/$PACKAGE"/* "$DATA/app/$PACKAGE"/oat/x86_64/* 2>/dev/null | sed "s#$DATA/app/$PACKAGE/##"

source /work/runtime/art/test_lib.sh
trap 'kill "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT
start_compositor

# The warm-up launch is the app's first start, which creates its preferences and Dart state; it is
# not counted. Every measured launch then starts with that state, as a user's second start does.
export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
# From the APK, as every start was before uxdroid install prepared apps, then installed.
for mode in --cold ""; do
    for start in "from the APK:$APK" "installed:$DATA/app/$PACKAGE/base.apk"; do
        "${MEASURE[@]}" --label "RustDesk 1.5.1 to its first Flutter frame (activity fully drawn), ${start%%:*}" \
            --runs "$RUNS" $mode "${EVICT[@]}" --timeout 120 --cwd "$DATA" \
            --marker "^uxdroid: activity fully drawn: $PACKAGE/\.MainActivity$" \
            --expect "^I/flutter: " --expect "^uxdroid: first frame posted on .*SurfaceView\[$PACKAGE/" -- \
            "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
            -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" "${start#*:}" || exit 1
    done
done
echo "data after the launches: $(du -sm "$DATA" | cut -f1) MiB"
echo "storage: app data on $STORAGE, $RUNTIME on $(findmnt -n -o FSTYPE -T "$RUNTIME"), APK on $(findmnt -n -o FSTYPE -T "$APK")"
