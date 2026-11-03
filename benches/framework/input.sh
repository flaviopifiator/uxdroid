#!/bin/bash
# Measures input latency: from the moment the compositor sends a click or a key to the first frame
# whose pixels differ, on the framework's input sample APK (dev service) or RustDesk (sandbox only).
# The compositor is runtime/framework/input_compositor.py, which reads every frame back.
#
#   docker compose run --rm -T dev bash /work/benches/framework/input.sh
#   UXDROID_SAMPLES=<directory with the RustDesk APK> docker compose run --rm -T \
#       -v uxdroid-framework-bench:/var/tmp -e APP=rustdesk sandbox bash /work/benches/framework/input.sh
#
# EVENTS sets the measured clicks and keys (default 30 each). Each one toggles what is drawn, so
# every event must change the frame; one that does not within 2 s fails the benchmark.
set -u -o pipefail
APP="${APP:-sample}"
EVENTS="${EVENTS:-30}"
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

mkfifo "$WORK/fifo"
"$TARGET/uxdroid-keymap" us >"$WORK/keymap" || fail "no keymap tool in $TARGET"
python3 /work/runtime/framework/input_compositor.py "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" "$WORK/fifo" \
    "$WORK/keymap" >"$WORK/compositor" 2>&1 &
compositor=$!
for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; pause 0.05; done
"${VM[@]}" -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" "$APK" >"$WORK/out" 2>&1 &
app=$!
trap 'kill "$app" "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT

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
# Sends a command after the screen settled, waits for the frame it changes, and appends the time to
# the file $WORK/<name>.
measure_one() {
    local name=$1 command=$2 before
    settle
    before=$(grep -c "changed .* ms after" "$WORK/compositor")
    send "$command"
    WAIT=2 wait_for "$WORK/compositor" "changed .* ms after" $((before + 1))
    grep "changed .* ms after" "$WORK/compositor" | tail -1 | grep -q "after $command\$" \
        || fail "the frame after $command answered another command"
    grep "changed .* ms after" "$WORK/compositor" | tail -1 | sed 's/.*changed \([0-9.]*\) ms.*/\1/' >>"$WORK/$name"
}

if [ "$APP" = sample ]; then
    wait_for "$WORK/out" "^input: layout field "
    read -r _ _ _ BX BY < <(grep "^input: layout button " "$WORK/out" | tail -1)
    read -r _ _ _ FX FY < <(grep "^input: layout field " "$WORK/out" | tail -1)
    CLICK=("click $BX $BY")
    KEYS=("key 30" "key 14")  # a, then backspace: each changes the field
    FOCUS="tap $FX $FY"
else
    wait_for "$WORK/out" "^uxdroid: first frame posted on com.carriez.flutter_hbb/"
    CLICK=("click 150 672" "click 350 672")  # Chat and Settings tabs
    KEYS=("key 3" "key 14")  # 2, then backspace, in the Remote ID field
    FOCUS="click 150 99"
fi
settle
clicks=()
for i in $(seq "$EVENTS"); do clicks+=("${CLICK[$(( i % ${#CLICK[@]} ))]}"); done
measure_all() { local name=$1; shift; : >"$WORK/$name"; for c in "$@"; do measure_one "$name" "$c"; done; }
measure_all click "${clicks[@]}"
if [ "$APP" = rustdesk ]; then send "click 50 672"; settle; fi  # back to the Connection tab
send "$FOCUS"
settle
keys=()
for i in $(seq "$EVENTS"); do keys+=("${KEYS[$(( (i - 1) % 2 ))]}"); done
measure_all key "${keys[@]}"
python3 - "$APP" "$WORK/click" "$WORK/key" <<'PY'
import sys
app = sys.argv[1]
for name, path in (("click", sys.argv[2]), ("key", sys.argv[3])):
    values = sorted(float(line) for line in open(path) if line.strip())
    # Linear interpolation between ranks, as statistics.quantiles' inclusive method.
    def quantile(p):
        rank = p * (len(values) - 1)
        low = int(rank)
        high = min(low + 1, len(values) - 1)
        return values[low] + (values[high] - values[low]) * (rank - low)
    print(f"{app} {name}: {len(values)} events, median {quantile(0.5):.1f} ms "
          f"({quantile(0.25):.1f}-{quantile(0.75):.1f}), min {values[0]:.1f}, max {values[-1]:.1f}")
PY
echo "storage: app data on $(findmnt -n -o FSTYPE -T "$WORK"), $RUNTIME on $(findmnt -n -o FSTYPE -T "$RUNTIME")"
