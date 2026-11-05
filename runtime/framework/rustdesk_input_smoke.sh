#!/bin/bash
# Sends Wayland seat input to RustDesk's x86_64 build (Flutter) on AOSP's framework: a click on a
# widget and typed text must reach the Flutter engine and change what it draws. Its native
# libraries are untrusted, so this runs only in the sandbox service:
#
#   docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
#   UXDROID_SAMPLES=<directory with the APK> docker compose run --rm sandbox \
#       bash /work/runtime/framework/rustdesk_input_smoke.sh
#
# A scripted compositor (input_compositor.py) plays the seat and reads the window's frames back.
# SHOTS=<writable directory> keeps the screenshots, as PPM.
set -u -o pipefail
WORK=$(mktemp -d)
# Removed however the script ends, a failure before the full trap below included.
trap 'rm -rf "$WORK"' EXIT
APK="${RUSTDESK_APK:-/samples/rustdesk-1.5.1-universal-signed.apk}"
PACKAGE=com.carriez.flutter_hbb
fail() {
    echo "FAIL: $*"
    [ -f "$WORK/out" ] && grep -v -E "jni_internal|^	at " "$WORK/out" | tail -"${TAIL:-25}" | cut -c1-220
    [ -f "$WORK/compositor" ] && tail -8 "$WORK/compositor" | cut -c1-160
    exit 1
}

TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
for file in uxdroid uxdroid-keymap libuxdroid_system.so libuxdroid_nativebridge.so; do
    [ -e "$TARGET/$file" ] || fail "no $TARGET/$file: build the release libraries in the dev service first"
done
[ -r "$APK" ] || fail "no APK at $APK: set UXDROID_SAMPLES to its directory, or RUSTDESK_APK"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
"$TARGET/uxdroid" install "$APK" --data "$ANDROID_DATA" >"$WORK/install" 2>&1 || { cat "$WORK/install"; fail "install"; }

source /work/runtime/art/test_lib.sh
mkfifo "$WORK/fifo"
"$TARGET/uxdroid-keymap" us >"$WORK/keymap" || fail "no us keymap"
python3 /work/runtime/framework/input_compositor.py "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" "$WORK/fifo" \
    "$WORK/keymap" >"$WORK/compositor" 2>&1 &
compositor=$!
for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; pause 0.05; done
export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
"$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
    -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" "$APK" >"$WORK/out" 2>&1 &
app=$!
trap 'kill "$app" "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT

wait_for() {
    local file="$WORK/out"
    [ "${C:-0}" = 1 ] && file="$WORK/compositor"
    for _ in $(seq $(( ${WAIT:-60} * 20 ))); do
        grep -q -E "$1" "$file" && return
        kill -0 "$app" 2>/dev/null || fail "the app exited before $2"
        pause 0.05
    done
    fail "no $2 within ${WAIT:-60} s"
}
# A FIFO with no reader blocks its writer, so a compositor that died fails the script, never hangs it.
send() {
    timeout 5 sh -c 'echo "$1" >"$2"' send "$*" "$WORK/fifo" || fail "the compositor took no $*"
}
# Writes the last frame to $WORK/$1.ppm, and to $SHOTS when set.
shot() {
    send "shot $WORK/$1.ppm"
    C=1 WAIT=10 wait_for "shot $WORK/$1.ppm" "a screenshot"
    [ -n "${SHOTS:-}" ] && cp "$WORK/$1.ppm" "$SHOTS/"
    return 0
}
# Waits until no frame came for a second, so the screen is settled.
settle() {
    local last=-1 now
    for _ in $(seq 40); do
        now=$(grep -c "commit [0-9]" "$WORK/compositor")
        [ "$now" = "$last" ] && return
        last=$now
        pause 1
    done
}
# Pixels that differ between two screenshots, within a box x0 y0 x1 y1.
changed() {
    python3 - "$WORK/$1.ppm" "$WORK/$2.ppm" "$3" "$4" "$5" "$6" <<'EOF'
import sys
def read(path):
    data = open(path, "rb").read()
    header, pixels = data.split(b"\n", 1)
    _, width, height, _ = header.split()
    return int(width), int(height), pixels
w, h, a = read(sys.argv[1])
w2, h2, b = read(sys.argv[2])
x0, y0, x1, y1 = (int(v) for v in sys.argv[3:7])
if (w, h) != (w2, h2):
    print(w * h)
else:
    print(sum(1 for y in range(max(0, y0), min(h, y1)) for x in range(max(0, x0), min(w, x1))
              if a[(y * w + x) * 3:(y * w + x) * 3 + 3] != b[(y * w + x) * 3:(y * w + x) * 3 + 3]))
EOF
}

wait_for "^uxdroid: first frame posted on $PACKAGE/$PACKAGE\.MainActivity " "a frame of the activity's window"
settle
shot home
echo "stage: RustDesk's home screen is drawn"

# Coordinates of RustDesk 1.5.1's mobile home screen in a 400x700 window: the Remote ID field
# under the app bar, and the Settings tab at the bottom right.
FIELD="150 99"
SETTINGS="350 672"

# A click on the field focuses it: Flutter shows its cursor and asks for the keyboard.
send "click $FIELD"
C=1 WAIT=10 wait_for "changed .* ms after click $FIELD" "a frame after clicking the Remote ID field"
settle
shot focused
send "type 123456789"
C=1 WAIT=10 wait_for "changed .* ms after type 123456789" "a frame after typing"
settle
shot typed
TYPED=$(changed focused typed 12 66 340 133)
[ "$TYPED" -gt 150 ] || fail "typing changed only $TYPED pixels of the Remote ID field"
echo "ok: typed digits reached Flutter's text field, which redrew $TYPED pixels"

send "click $SETTINGS"
C=1 WAIT=10 wait_for "changed .* ms after click $SETTINGS" "a frame after clicking the Settings tab"
settle
shot settings
PAGE=$(changed typed settings 0 56 400 640)
[ "$PAGE" -gt 20000 ] || fail "the Settings tab changed only $PAGE pixels of the page"
echo "ok: a click on the Settings tab reached Flutter, which drew another page ($PAGE pixels changed)"

grep -E "changed .* ms after" "$WORK/compositor" | sed 's/^\[compositor\] /latency: /'
echo "ok: Wayland input reaches RustDesk's Flutter widgets"
