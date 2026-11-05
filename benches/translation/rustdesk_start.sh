#!/bin/bash
# RustDesk 1.5.1 started to its first Flutter frame, translated (arm64-v8a) against native (x86_64),
# the same APK's two builds. Its native code is untrusted, so this runs only in a sandbox with the
# translation engine, as runtime/framework/rustdesk_arm64_smoke.sh does:
#
#   benches/translation/rustdesk_start.sh [runs]
#
# Needs /samples/rustdesk-1.5.1-universal-signed.apk (x86_64) and
# /samples/rustdesk-1.5.1-arm64-only.apk (its arm64 libraries only), the release build in
# CARGO_TARGET_DIR and UXDROID_TRANSLATOR. Each ABI is installed once; every run starts from a fresh
# copy of that install, and runs alternate between the builds. A run measures, when the first frame
# is posted on Flutter's SurfaceView: the time since the VM started, Private_Dirty of the process,
# its user plus system CPU time, and the resident part of its anonymous executable and writable
# mappings, which is where Dynarmic keeps translated code.
#
# The arm64 build runs twice per round: without a translation profile, as on its first start
# (UXDROID_TRANSLATION_PROFILE=0, so it neither reads nor writes one), and with the profile one
# earlier start saved beside its libraries (lib/arm64-v8a.profile), whose blocks it translates ahead
# on a thread of its own. That earlier start runs once, before the rounds, until its profile is
# saved, 3 s after its first arm64 library loaded.
set -u -o pipefail
RUNS="${1:-5}"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
PACKAGE=com.carriez.flutter_hbb
TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
source /work/runtime/art/run.sh
source /work/runtime/art/test_lib.sh
source /work/runtime/gpu/compositor.sh
start_compositor
export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
TICK_MS=$((1000 / $(getconf CLK_TCK)))

declare -A APK=([x86_64]=/samples/rustdesk-1.5.1-universal-signed.apk
    [arm64-v8a]=/samples/rustdesk-1.5.1-arm64-only.apk)
for abi in x86_64 arm64-v8a; do
    mkdir -p "$WORK/$abi/dalvik-cache/x86_64"
    start=$(date +%s%N)
    UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "${APK[$abi]}" --data "$WORK/$abi" \
        >"$WORK/install-$abi" 2>&1 || { cat "$WORK/install-$abi"; exit 1; }
    grep -q "native libraries for $abi in" "$WORK/install-$abi" \
        || { cat "$WORK/install-$abi"; echo "FAIL: $abi was not installed"; exit 1; }
    echo "install $abi: $(( ($(date +%s%N) - start) / 1000000 )) ms"
done

PROFILE="$WORK/arm64-v8a/data/$PACKAGE/lib/arm64-v8a.profile"

# One run: prints "<label> <ms to frame> <private dirty KiB> <cpu ms> <anonymous rwx KiB> <their count>".
# Labels: x86_64, arm64-v8a (no profile) and arm64-v8a+profile.
run() {
    local label=$1 abi=${1%+profile} data="$WORK/run" out="$WORK/out" app start ms use_profile=0
    [ "$label" = "$abi" ] || use_profile=1
    rm -rf "$data" && cp -a "$WORK/$abi" "$data"
    if [ "$abi" = arm64-v8a ] && [ "$use_profile" = 0 ]; then
        rm -f "$data/data/$PACKAGE/lib/arm64-v8a.profile"
    fi
    start=$(date +%s%N)
    UXDROID_TRANSLATION_PROFILE=$use_profile ANDROID_DATA="$data" "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" \
        -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing -cp "$CP" \
        dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" \
        "$data/app/$PACKAGE/base.apk" >"$out" 2>&1 &
    app=$!
    for _ in $(seq 12000); do
        if grep -q -E "^uxdroid: first frame posted on .*SurfaceView\[$PACKAGE/" "$out"; then
            ms=$(( ($(date +%s%N) - start) / 1000000 ))
            local dirty cpu rwx count
            dirty=$(awk '/^Private_Dirty:/ {print $2}' "/proc/$app/smaps_rollup")
            cpu=$(awk -v tick="$TICK_MS" '{sub(/^.*\) /, ""); print ($12 + $13) * tick}' "/proc/$app/stat")
            rwx=$(awk '/^[0-9a-f]+-[0-9a-f]+ / {exec = ($2 ~ /^rwx/ && NF == 5)} \
                exec && /^Rss:/ {sum += $2} END {print sum + 0}' "/proc/$app/smaps")
            count=$(grep -c -E '^[0-9a-f]+-[0-9a-f]+ rwx. [0-9a-f]+ 00:00 0 *$' "/proc/$app/maps")
            kill "$app" 2>/dev/null
            wait "$app" 2>/dev/null
            echo "$label $ms $dirty $cpu $rwx $count"
            return
        fi
        kill -0 "$app" 2>/dev/null || break
        sleep 0.01
    done
    kill "$app" 2>/dev/null
    wait "$app" 2>/dev/null
    grep -v "^	at " "$out" | tail -15 >&2
    echo "FAIL: $label did not post a frame" >&2
    exit 1
}

# The earlier start that saves the profile, kept in the arm64 install for the profiled runs.
profile_run() {
    local data="$WORK/run" out="$WORK/out" app saved="$WORK/run/data/$PACKAGE/lib/arm64-v8a.profile"
    rm -rf "$data" && cp -a "$WORK/arm64-v8a" "$data"
    UXDROID_TRANSLATION_PROFILE=1 ANDROID_DATA="$data" "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" \
        -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing -cp "$CP" \
        dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" \
        "$data/app/$PACKAGE/base.apk" >"$out" 2>&1 &
    app=$!
    for _ in $(seq 3000); do
        [ -s "$saved" ] && break
        kill -0 "$app" 2>/dev/null || break
        sleep 0.01
    done
    kill "$app" 2>/dev/null
    wait "$app" 2>/dev/null
    [ -s "$saved" ] || { tail -15 "$out" >&2; echo "FAIL: no translation profile was saved" >&2; exit 1; }
    cp "$saved" "$PROFILE"
    echo "profile: $(stat -c %s "$PROFILE") bytes"
}
profile_run

for i in $(seq "$RUNS"); do
    for label in x86_64 arm64-v8a arm64-v8a+profile; do
        run "$label" | tee -a "$WORK/results"
    done
done

PYTHONDONTWRITEBYTECODE=1 python3 -I - "$WORK/results" <<'EOF'
import sys


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


rows = {}
for line in open(sys.argv[1]):
    abi, *values = line.split()
    rows.setdefault(abi, []).append([int(v) for v in values])
medians = {abi: [median(c) for c in zip(*runs)] for abi, runs in rows.items()}
print(f"medians of {len(rows['x86_64'])} runs")
print("| Build | First frame ms | Private dirty MiB | CPU ms | Anonymous rwx MiB | rwx mappings |")
print("|---|---|---|---|---|---|")
labels = (("x86_64", "native x86_64"), ("arm64-v8a", "translated arm64-v8a, no profile"),
          ("arm64-v8a+profile", "translated arm64-v8a, with profile"))
for abi, label in labels:
    ms, dirty, cpu, rwx, count = medians[abi]
    print(f"| {label} | {ms:.0f} | {dirty / 1024:.1f} | {cpu:.0f} | {rwx / 1024:.1f} | {count:.0f} |")
native = medians["x86_64"]
for abi, label in labels[1:]:
    translated = medians[abi]
    print(f"{label} first frame: {translated[0] / native[0]:.2f}x the native time, "
          f"{(translated[1] - native[1]) / 1024:+.1f} MiB private dirty, "
          f"{translated[2] - native[2]:+.0f} ms CPU")
EOF
