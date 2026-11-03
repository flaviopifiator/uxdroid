#!/bin/bash
# Measures what a frame costs the app: CPU time of all its threads per frame the compositor received,
# over a fixed sequence of input that redraws part of the window, on the framework's input sample APK
# (dev service) or RustDesk (sandbox only). The compositor is runtime/framework/input_compositor.py.
#
#   docker compose run --rm -T dev bash /work/benches/framework/frames.sh
#   UXDROID_SAMPLES=<directory with the RustDesk APK> docker compose run --rm -T \
#       -v uxdroid-framework-bench:/var/tmp -e APP=rustdesk sandbox bash /work/benches/framework/frames.sh
#
# The sample types a and backspace into its text field, which redraws the field only; RustDesk does
# the same in its Remote ID field, which Flutter draws into its SurfaceView. EVENTS sets the keys per
# run (default 60) and RUNS the runs (default 5), each in a fresh process. Every key must change the
# frame within 2 s.
set -u -o pipefail
APP="${APP:-sample}"
EVENTS="${EVENTS:-60}"
RUNS="${RUNS:-5}"
WORK=$(mktemp -d -p "$([ -w /var/tmp ] && echo /var/tmp || echo /tmp)")
TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
fail() { echo "FAIL: $*"; tail -5 "$WORK/out" 2>/dev/null; tail -5 "$WORK/compositor" 2>/dev/null; exit 1; }

source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
source /work/runtime/art/test_lib.sh
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
VM=("$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}")
case "$APP" in
    sample)
        cargo build -q --release -p uxdroid-system --manifest-path /work/Cargo.toml || fail "build"
        APK="$RUNTIME/samples/framework/input.apk" ;;
    rustdesk)
        APK="${RUSTDESK_APK:-/samples/rustdesk-1.5.1-universal-signed.apk}"
        "$TARGET/uxdroid" install "$APK" --data "$ANDROID_DATA" >"$WORK/install" 2>&1 || fail "install"
        export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
        VM+=(-XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing) ;;
    *) fail "APP is sample or rustdesk" ;;
esac
"$TARGET/uxdroid-keymap" us >"$WORK/keymap" || fail "no keymap tool in $TARGET"
trap 'kill "${app:-}" "${compositor:-}" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT

wait_for() {
    local file=$1 pattern=$2 count=${3:-1}
    for _ in $(seq $(( ${WAIT:-60} * 20 ))); do
        [ "$(grep -c -E "$pattern" "$file")" -ge "$count" ] && return
        kill -0 "$app" 2>/dev/null || fail "the app exited waiting for $pattern"
        pause 0.05
    done
    fail "no $pattern within ${WAIT:-60} s"
}
send() { echo "$*" >"$WORK/fifo"; }
settle() {
    local last=-1 now
    for _ in $(seq 40); do
        now=$(grep -c "commit [0-9]" "$WORK/compositor")
        [ "$now" = "$last" ] && return
        last=$now
        pause 0.5
    done
}
# CPU time of every thread of the app, in nanoseconds.
cpu() { cat /proc/"$app"/task/*/schedstat 2>/dev/null | awk '{ sum += $1 } END { printf "%d\n", sum }'; }
frames() { grep -c -E "\] commit [0-9]+ " "$WORK/compositor"; }

: >"$WORK/results"
for run in $(seq "$RUNS"); do
    rm -f "$WORK/fifo"
    mkfifo "$WORK/fifo"
    python3 /work/runtime/framework/input_compositor.py "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" "$WORK/fifo" \
        "$WORK/keymap" >"$WORK/compositor" 2>&1 &
    compositor=$!
    for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; pause 0.05; done
    "${VM[@]}" -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" "$APK" \
        >"$WORK/out" 2>&1 &
    app=$!
    if [ "$APP" = sample ]; then
        wait_for "$WORK/out" "^input: layout field "
        read -r _ _ _ FX FY < <(grep "^input: layout field " "$WORK/out" | tail -1)
        FOCUS="tap $FX $FY"
        KEYS=("key 30" "key 14")  # a, then backspace
    else
        wait_for "$WORK/out" "^uxdroid: first frame posted on com.carriez.flutter_hbb/"
        FOCUS="click 150 99"
        KEYS=("key 3" "key 14")  # 2, then backspace, in the Remote ID field
    fi
    settle
    send "$FOCUS"
    settle
    start_cpu=$(cpu)
    start_frames=$(frames)
    for i in $(seq "$EVENTS"); do
        before=$(grep -c "changed .* ms after" "$WORK/compositor")
        send "${KEYS[$(( (i - 1) % 2 ))]}"
        WAIT=2 wait_for "$WORK/compositor" "changed .* ms after" $((before + 1))
        pause 0.1
    done
    settle
    echo "$(( $(cpu) - start_cpu )) $(( $(frames) - start_frames ))" >>"$WORK/results"
    kill "$app" "$compositor" 2>/dev/null
    wait 2>/dev/null
    pause 0.3
done
python3 - "$APP" "$EVENTS" "$WORK/results" <<'PY'
import sys
def median(values):
    values = sorted(values)
    middle = len(values) // 2
    return values[middle] if len(values) % 2 else (values[middle - 1] + values[middle]) / 2
app, events = sys.argv[1], int(sys.argv[2])
runs = [tuple(map(int, line.split())) for line in open(sys.argv[3]) if line.strip()]
per_frame = [cpu / frames / 1e6 for cpu, frames in runs if frames]
per_key = [cpu / events / 1e6 for cpu, _ in runs]
frames = [frames for _, frames in runs]
print(f"{app}: {len(runs)} runs of {events} keys; frames per run {min(frames)}-{max(frames)}; "
      f"CPU per frame median {median(per_frame):.2f} ms ({min(per_frame):.2f}-{max(per_frame):.2f}); "
      f"CPU per key median {median(per_key):.2f} ms")
PY
echo "storage: app data on $(findmnt -n -o FSTYPE -T "$WORK"), $RUNTIME on $(findmnt -n -o FSTYPE -T "$RUNTIME")"
