#!/bin/bash
# Checks composition on AOSP's framework through a scripted compositor (input_compositor.py), with an
# SDK-built APK that knows nothing about uxdroid: a layer of the app's own moved, scaled, cropped,
# faded and rounded by one transaction that shows nothing until it is applied, and then all at once;
# only damaged pixels sent to the compositor; a dialog and a popup as subsurfaces that take their own
# input; frame callbacks pacing an animation; a resize of the toplevel laying the window out again;
# and a close destroying the activity.
# Runs in the dev service; the APK is ours, built by build_system.sh.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() {
    echo "FAIL: $*"
    tail -"${TAIL:-25}" "$WORK/out" | cut -c1-220
    tail -"${CTAIL:-12}" "$WORK/compositor" | cut -c1-160
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
APK="$RUNTIME/samples/framework/compose.apk"
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
        [ "${GONE:-0}" = 1 ] || kill -0 "$app" 2>/dev/null || fail "the app exited before $2"
        pause 0.05
    done
    fail "no $2 within ${WAIT:-20} s"
}
send() { echo "$*" >"$WORK/fifo"; }
# The compositor's lines after the last one that matched $1 so far.
mark() { wc -l <"$WORK/compositor"; }
since() { tail -n +"$(( $1 + 1 ))" "$WORK/compositor"; }
# Pixels of the composed toplevel at each X,Y, as RRGGBB.
SHOTS=0
pixels() {
    SHOTS=$((SHOTS + 1))
    local shot="$WORK/shot-$SHOTS.ppm"
    send "shot $shot"
    C=1 wait_for "shot $shot\$" "a screenshot"
    python3 - "$shot" "$@" <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
header, rgb = data.split(b"\n", 1)
width = int(header.split()[1])
out = []
for point in sys.argv[2:]:
    x, y = map(int, point.split(","))
    i = (y * width + x) * 3
    out.append(rgb[i:i + 3].hex())
print(" ".join(out))
PY
    rm -f "$shot"
}
near() { # near HEX HEX: within 2 per channel
    python3 -c 'import sys; a, b = bytes.fromhex(sys.argv[1]), bytes.fromhex(sys.argv[2]); sys.exit(0 if all(abs(x - y) <= 2 for x, y in zip(a, b)) else 1)' "$1" "$2"
}
AMBER=ffe082
BLUE=1e88e5
FADED_BLUE=8eb4b3   # BLUE at alpha 0.5 over AMBER
FADED_GREEN=a1c064  # 0xFF43A047 at alpha 0.5 over AMBER

wait_for "^compose: placed$" "the app's own layer"
wait_for "^compose: window " "a layout"
pause 0.5
read -r AT OUT < <(pixels 70,70 260,350)
near "$AT" "$BLUE" || fail "the layer is not at its position: $AT"
near "$OUT" "$AMBER" || fail "the window is not amber: $OUT"
echo "ok: a layer of the app's own shows over its window"

send "key 49"
wait_for "^compose: unapplied$" "a frame with an unapplied transaction"
pause 0.3
read -r AT OUT < <(pixels 70,70 260,350)
near "$AT" "$BLUE" && near "$OUT" "$AMBER" || fail "a transaction showed before apply: $AT $OUT"
echo "ok: a transaction's changes do not show before it is applied, though the window drew again"

LINE=$(mark)
send "key 49"
wait_for "^compose: applied$" "the apply"
C=1 wait_for "damage 20,20,300,380" "the damage of the move"
since "$LINE" | grep -q -E "commit [0-9]+ 400x700 damage 0,0,400,700" && fail "a move redrew the whole window"
read -r OLD MIDDLE EDGE CROPPED CORNER < <(pixels 70,70 260,350 205,350 330,350 201,301)
near "$OLD" "$AMBER" || fail "the layer is still at its old position: $OLD"
near "$MIDDLE" "$FADED_BLUE" || fail "the layer is not moved, scaled and faded: $MIDDLE"
near "$EDGE" "$FADED_BLUE" || fail "the layer is not scaled from its new position: $EDGE"
near "$CROPPED" "$AMBER" || fail "the crop did not cut the layer: $CROPPED"
near "$CORNER" "$AMBER" || fail "the corner is not rounded: $CORNER"
echo "ok: one transaction moved, scaled, cropped, faded and rounded the layer at once, redrawing 300x380 of 400x700"

LINE=$(mark)
send "key 49"
wait_for "^compose: redrawn$" "the layer's new frame"
C=1 wait_for "damage 200,300,120,100" "the damage of the layer's frame"
read -r MIDDLE < <(pixels 260,350)
near "$MIDDLE" "$FADED_GREEN" || fail "the layer's new frame does not show: $MIDDLE"
echo "ok: a layer's new frame redraws only where it shows"

send "key 49"
wait_for "^compose: dialog shown$" "a dialog"
C=1 wait_for "subsurface [0-9]+ commit [0-9]+x[0-9]+ at -?[0-9]+,-?[0-9]+" "the dialog's subsurface"
DIALOG=$(grep -o -E "subsurface [0-9]+ commit [0-9]+x[0-9]+ at -?[0-9]+,-?[0-9]+" "$WORK/compositor" | tail -1)
echo "ok: the dialog is a subsurface: $DIALOG"
send "tap 5 5"
wait_for "^compose: dialog canceled$" "the dialog canceled by a touch outside it"
C=1 wait_for "subsurface [0-9]+ destroyed" "the dialog's subsurface removed"
echo "ok: a touch outside the touch-modal dialog reached it and canceled it, and its subsurface left"

send "key 49"
wait_for "^compose: popup at " "a popup"
read -r _ _ _ PX PY < <(grep "^compose: popup at " "$WORK/out" | tail -1)
C=1 wait_for "subsurface [0-9]+ commit 160x80 at 50,500" "the popup's subsurface where the window manager put it"
send "click $PX $PY"
wait_for "^compose: popup clicked$" "a click on the popup's button"
echo "ok: a popup is a subsurface at 50,500 and its button takes a click at $PX,$PY"

# Frames per second the compositor received, over two seconds.
rate() {
    local first last
    pause 0.5
    first=$(grep -c -E "\] commit [0-9]+ " "$WORK/compositor")
    pause 2
    last=$(grep -c -E "\] commit [0-9]+ " "$WORK/compositor")
    echo $(( (last - first) / 2 ))
}
send "key 49"
wait_for "^compose: animating$" "an animation"
send "frames 30"
RATE=$(rate)
[ "$RATE" -ge 24 ] && [ "$RATE" -le 34 ] || fail "an animation drew $RATE frames per second with frame callbacks at 30 Hz"
echo "ok: frame callbacks at 30 Hz pace Choreographer: $RATE frames per second"
send "frames 0"
RATE=$(rate)
[ "$RATE" -ge 45 ] && [ "$RATE" -le 62 ] || fail "an animation drew $RATE frames per second with immediate frame callbacks"
echo "ok: an eager compositor gets no more than the display's 60 Hz: $RATE frames per second"
send "frames stop"
RATE=$(rate)
[ "$RATE" -ge 10 ] && [ "$RATE" -le 25 ] || fail "an animation drew $RATE frames per second with no frame callbacks"
echo "ok: with no frame callbacks, as for a hidden window, the app does not stall: $RATE frames per second"
send "frames 0"

send "resize 600 500"
wait_for "^compose: window 600x500$" "a layout at the new size"
C=1 wait_for "commit [0-9]+ 600x500 " "a frame at the new size"
echo "ok: a configure of 600x500 laid the window out again and its frames follow"

send "close"
GONE=1 wait_for "^compose: destroyed$" "the activity destroyed"
for _ in $(seq 100); do kill -0 "$app" 2>/dev/null || break; pause 0.05; done
kill -0 "$app" 2>/dev/null && fail "the app is still running after its window closed"
wait "$app"; status=$?
[ "$status" = 0 ] || fail "the app exited with $status after its window closed"
echo "ok: closing the toplevel destroyed the activity and the app exited"
echo "ok: composition, windows, resize and close"
