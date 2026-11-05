#!/bin/bash
# Measures what an EGL frame costs the app on each path: the frame time and the CPU time of all the
# app's threads per frame, while a GL app redraws continuously. Paths, by MODE:
#
#   dmabuf     the GPU through GBM, frames handed to the compositor as dmabufs (needs the GPU)
#   readback   the same GPU, frames read back with glReadPixels and copied through shared memory
#   llvmpipe   Mesa's software renderer, read back as on a machine without a GPU
#
# The app is Mindustry from the F-Droid corpus, whose main menu animates every frame. Its code is
# untrusted, so this runs in the sandbox's isolation with the GPU nodes and driver libraries of
# dev-gpu added, never in dev:
#
#   docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
#   <sandbox flags of compose.yaml, plus the devices and mounts of dev-gpu> \
#       -e MODE=dmabuf uxdroid-dev bash /work/benches/framework/gpu.sh
#
# The compositor is weston's headless backend with its GL renderer for every mode, so only the app's
# side differs. WARM seconds pass after the first frame (default 10), then the app is measured for
# MEASURE seconds (default 20); RUNS (default 3) fresh processes. APK names the APK (default Mindustry's in
# /samples). Each run prints `frame <ms> cpu <ms per frame> swap <us> swaps <n> readback <n>`, where swap is
# what eglSwapBuffers itself took, in microseconds.
set -u -o pipefail
MODE="${MODE:-dmabuf}"
RUNS="${RUNS:-3}"
WARM="${WARM:-10}"
MEASURE="${MEASURE:-20}"
APK="${APK:-$(ls /samples/io.anuke.mindustry_*.apk 2>/dev/null | head -1)}"
ROOT=$(mktemp -d -p "$([ -w /var/tmp ] && echo /var/tmp || echo /tmp)")
trap 'rm -rf "$ROOT"' EXIT
TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
fail() { echo "FAIL: $*"; tail -5 "$WORK/out" 2>/dev/null | cut -c1-200; exit 1; }
[ -r "${APK:-}" ] || fail "no APK: put Mindustry's in /samples or set APK"

export RENDERER=gl UXDROID_EGL_STATS=1
case "$MODE" in
    dmabuf) unset UXDROID_EGL ;;
    readback) export UXDROID_EGL=readback ;;
    llvmpipe) export UXDROID_EGL=readback __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json ;;
    *) fail "MODE is dmabuf, readback or llvmpipe" ;;
esac
source /work/runtime/art/run.sh
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
TICK=$(getconf CLK_TCK)

cpu_ticks() { awk '{ print $14 + $15 }' "/proc/$1/stat"; }

for run in $(seq "$RUNS"); do
    WORK="$ROOT/$run"
    mkdir -p "$WORK"
    export ANDROID_DATA="$WORK/data"
    mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
    source /work/runtime/art/test_lib.sh
    source /work/runtime/gpu/compositor.sh
    UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$APK" --data "$ANDROID_DATA" >"$WORK/install" 2>&1 \
        || { cat "$WORK/install"; fail "install"; }
    package=$(ls "$ANDROID_DATA/app" | grep -v '^\.' | head -1)
    start_compositor
    export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
    "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
        -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" \
        "$ANDROID_DATA/app/$package/base.apk" >"$WORK/out" 2>&1 &
    app=$!
    trap 'kill "$app" "$compositor" 2>/dev/null; rm -rf "$ROOT"' EXIT
    for _ in $(seq 1800); do
        grep -q "^uxdroid: first frame posted on $package/" "$WORK/out" && break
        kill -0 "$app" 2>/dev/null || fail "the app exited before its first frame"
        pause 0.05
    done
    pause "$WARM"
    swaps() { sed -n -E 's/^uxdroid: egl stats: ([0-9]+) swaps.*/\1/p' "$WORK/out" | tail -1; }
    readback() { sed -n -E 's/^uxdroid: egl stats: [0-9]+ swaps, ([0-9]+) read back.*/\1/p' "$WORK/out" | awk '{ s += $1 } END { print s + 0 }'; }
    before_cpu=$(cpu_ticks "$app") before_swaps=$(swaps) before_readback=$(readback) before=$(date +%s%N)
    pause "$MEASURE"
    after_cpu=$(cpu_ticks "$app") after_swaps=$(swaps) after_readback=$(readback) after=$(date +%s%N)
    kill -0 "$app" 2>/dev/null || fail "the app exited while measured"
    frames=$(( ${after_swaps:-0} - ${before_swaps:-0} ))
    [ "$frames" -gt 0 ] || fail "no EGL frames while measured"
    # The swaps' own cost, from the reports of the measured swaps: blit, fence and hand-over, or
    # readback and post.
    handover=$(sed -n -E 's/^uxdroid: egl stats: .* ([0-9.]+) us each.*/\1/p' "$WORK/out" | tail -n $(( frames / 30 )) \
        | awk '{ s += $1; n++ } END { if (n) printf "%.0f", s / n; else print 0 }')
    python3 -c "
frames, cpu, wall = $frames, ($after_cpu - $before_cpu) * 1000 / $TICK, ($after - $before) / 1e6
print(f'$MODE run $run: frame {wall / frames:.2f} ms cpu {cpu / frames:.2f} ms swap $handover us swaps {frames} readback $(( after_readback - before_readback ))')
"
    grep -E "^uxdroid: (EGL renders|GPU frames|EGL frames are read back)" "$WORK/out" | head -3
    kill "$app" "$compositor" 2>/dev/null
    wait 2>/dev/null
done
