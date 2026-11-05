#!/bin/bash
# Sends Wayland seat input to an SDK-built APK on AOSP's framework and checks that its Views get it:
# a mouse click and a finger tap on a Button, typing into an EditText, Escape as the back key, focus,
# and pings answered while the window is idle. A scripted compositor (input_compositor.py) plays the
# seat and reads the window's frames back. Runs in the dev service; the APK is ours, built by
# build_system.sh.
set -u -o pipefail
WORK=$(mktemp -d)
# Removed however the script ends, a failure before the full trap below included.
trap 'rm -rf "$WORK"' EXIT
fail() {
    echo "FAIL: $*"
    tail -"${TAIL:-25}" "$WORK/out" | cut -c1-220
    tail -8 "$WORK/compositor" | cut -c1-160
    exit 1
}

cargo build -q --release -p uxdroid-system --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
LIBRARY="$TARGET/libuxdroid_system.so"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
source /work/runtime/art/test_lib.sh
APK="$RUNTIME/samples/framework/input.apk"
[ -e "$APK" ] || fail "no $APK: run runtime/framework/build_system.sh"

mkfifo "$WORK/fifo"
"$TARGET/uxdroid-keymap" us >"$WORK/keymap" || fail "no us keymap"
python3 /work/runtime/framework/input_compositor.py "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" "$WORK/fifo" \
    "$WORK/keymap" >"$WORK/compositor" 2>&1 &
compositor=$!
for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; pause 0.05; done
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
"$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" dev.uxdroid.system.Launcher \
    "$RUNTIME" "$LIBRARY" "$APK" >"$WORK/out" 2>&1 &
app=$!
trap 'kill "$app" "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT

# Waits until the app's output, or the compositor's with C=1, matches.
wait_for() {
    local file="$WORK/out"
    [ "${C:-0}" = 1 ] && file="$WORK/compositor"
    for _ in $(seq $(( ${WAIT:-20} * 20 ))); do
        grep -q -E "$1" "$file" && return
        kill -0 "$app" 2>/dev/null || fail "the app exited before $2"
        pause 0.05
    done
    fail "no $2 within ${WAIT:-20} s"
}
# A FIFO with no reader blocks its writer, so a compositor that died fails the script, never hangs it.
send() {
    timeout 5 sh -c 'echo "$1" >"$2"' send "$*" "$WORK/fifo" || fail "the compositor took no $*"
}

wait_for "^input: window focus true$" "window focus from the activated toplevel"
echo "ok: an activated toplevel focuses the window"
wait_for "^input: layout field " "a layout"
read -r _ _ _ BX BY < <(grep "^input: layout button " "$WORK/out" | tail -1)
read -r _ _ _ FX FY < <(grep "^input: layout field " "$WORK/out" | tail -1)
pause 0.5

# Green pixels in the last frame: the button turns the page green.
green() {
    send "shot $WORK/shot.ppm"
    C=1 wait_for "shot $WORK/shot.ppm" "a screenshot"
    python3 - "$WORK/shot.ppm" <<'EOF'
import sys
data = open(sys.argv[1], "rb").read()
header, pixels = data.split(b"\n", 1)
print(sum(1 for i in range(0, len(pixels), 3) if pixels[i:i + 3] == bytes((0x66, 0xBB, 0x6A))))
EOF
}
[ "$(green)" -lt 1000 ] || fail "the page is green before any click"

send "click $BX $BY"
wait_for "^input: clicked 1$" "a click from the mouse"
grep -q "^input: motion ACTION_DOWN source 0x2002 at $BX,$BY$" "$WORK/out" || fail "the click was not a mouse event at $BX,$BY"
C=1 wait_for "changed .* ms after click" "a frame after the click"
GREEN=$(green)
[ "$GREEN" -gt 10000 ] || fail "the click did not turn the page green ($GREEN pixels)"
echo "ok: a mouse click reached the Button, which turned $GREEN pixels green"

send "tap $BX $BY"
wait_for "^input: clicked 2$" "a click from a finger"
grep -q "^input: motion ACTION_DOWN source 0x1002 at $BX,$BY$" "$WORK/out" || fail "the tap was not a touchscreen event"
echo "ok: a finger tap reached the Button"

send "tap $FX $FY"
wait_for "^input: field focused true$" "the EditText's focus"
send "type Hi uxdroid 2!"
wait_for '^input: text "Hi uxdroid 2!"$' "typed text in the EditText"
grep -q "^input: key KEYCODE_H char 72 meta 0x41$" "$WORK/out" || fail "shift+h did not type H with shift's meta state"
echo "ok: typed text reached the focused EditText, with the compositor's keymap"

send "key 14"
wait_for '^input: text "Hi uxdroid 2"$' "backspace deleting a character"
echo "ok: backspace deletes"

# As on Android, Escape first takes the focus from a text field; once unhandled, it is the back key.
send "key 1"
wait_for "^input: field focused false$" "Escape clearing the text field's focus"
grep -q "^input: back$" "$WORK/out" && fail "a handled Escape also went back"
send "key 1"
wait_for "^input: back$" "the back callback from Escape"
echo "ok: Escape, unhandled, falls back to the back key"

# Idle: no frame is drawing, so only the Looper reads the connection.
pause 1
send "ping"
C=1 WAIT=2 wait_for "pong 4242" "an answer to a ping while idle"
echo "ok: an idle window answers the compositor's ping"

send "unfocus"
wait_for "^input: window focus false$" "focus loss"
echo "ok: a deactivated toplevel unfocuses the window"

grep -E "changed .* ms after" "$WORK/compositor" | sed 's/^\[compositor\] /latency: /'
grep -E "unhandled|unimplemented native" "$WORK/out" | grep -i -E "input|key|motion" | cut -c1-200
echo "ok: Wayland input reaches AOSP's Views"
