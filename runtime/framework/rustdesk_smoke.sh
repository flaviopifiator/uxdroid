#!/bin/bash
# Runs RustDesk's x86_64 build (Flutter) on AOSP's framework until its first Flutter frame is on
# screen. Its native libraries are untrusted, so this runs only in the sandbox service:
#
#   docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
#   UXDROID_SAMPLES=<directory with the APK> docker compose run --rm sandbox \
#       bash /work/runtime/framework/rustdesk_smoke.sh
#
# The APK is /samples/rustdesk-1.5.1-universal-signed.apk unless RUSTDESK_APK names another. Stages,
# in order: install the native libraries, run Dart, post a Flutter frame on the SurfaceView, the
# activity reports itself fully drawn, the window shows it. A failure names the furthest stage reached.
set -u -o pipefail
WORK=$(mktemp -d)
APK="${RUSTDESK_APK:-/samples/rustdesk-1.5.1-universal-signed.apk}"
PACKAGE=com.carriez.flutter_hbb
STAGE="nothing"
fail() {
    echo "FAIL: $* (furthest stage reached: $STAGE)"
    [ -f "$WORK/out" ] && grep -v -E "jni_internal|^	at " "$WORK/out" | tail -"${TAIL:-25}" | cut -c1-220
    exit 1
}
reached() { STAGE="$1"; echo "stage: $1"; }

TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
for file in uxdroid libuxdroid_system.so libuxdroid_nativebridge.so; do
    [ -e "$TARGET/$file" ] || fail "no $TARGET/$file: build the release libraries in the dev service first"
done
[ -r "$APK" ] || fail "no APK at $APK: set UXDROID_SAMPLES to its directory, or RUSTDESK_APK"
source /work/runtime/art/run.sh
[ -e "$RUNTIME/framework/uxdroid-system.jar" ] || fail "no framework in $RUNTIME: run runtime/framework/build.sh"
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"

# The installer's part, as Android's does it: native libraries where nativeLibraryDir points.
"$TARGET/uxdroid" install "$APK" --data "$ANDROID_DATA" >"$WORK/install" 2>&1 || { cat "$WORK/install"; fail "install"; }
grep -q -E "^installed $PACKAGE: [1-9][0-9]* native libraries for x86_64 in " "$WORK/install" \
    || { cat "$WORK/install"; fail "the install found no x86_64 libraries"; }
reached "installed ($(grep -o '[0-9]* native libraries' "$WORK/install"))"

source /work/runtime/art/test_lib.sh
start_compositor

export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
"$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
    -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" "$APK" >"$WORK/out" 2>&1 &
app=$!
trap 'kill "$app" "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT

# Each stage waits at most WAIT seconds, in steps of 50 ms.
wait_for() {
    for _ in $(seq $(( ${WAIT:-60} * 20 ))); do
        grep -q -E "$1" "$WORK/out" && return
        kill -0 "$app" 2>/dev/null || fail "the app exited before $2"
        pause 0.05
    done
    fail "no $2 within ${WAIT:-60} s"
}

wait_for "^I/flutter: " "Dart output from the app"
reached "Dart runs: the native libraries loaded and the Flutter engine started"
wait_for "^uxdroid: first frame posted on .*SurfaceView\[$PACKAGE/" "a frame on Flutter's SurfaceView"
reached "Flutter posted a frame on its SurfaceView"
wait_for "^uxdroid: activity fully drawn: $PACKAGE/\.MainActivity$" "Activity.reportFullyDrawn"
reached "the activity reported its first Flutter frame drawn"
wait_for "^uxdroid: first frame posted on $PACKAGE/$PACKAGE\.MainActivity " "a frame of the activity's window"
reached "the window posted"

# The compositor fades the desktop in, which shifts colors until it finishes.
pause 2
kill -0 "$app" 2>/dev/null || fail "the app exited after its first frame"
(cd "$WORK" && weston-screenshooter >"$WORK/shot" 2>&1) || { cat "$WORK/shot"; fail "screenshot"; }
SHOT=$(ls "$WORK"/wayland-screenshot-*.png 2>/dev/null | head -1)
[ -n "$SHOT" ] || fail "the screenshooter wrote no file"

# RustDesk's first screen has a Material blue app bar over a white page with text and icons. The
# window lies below the bar; it must hold several colors and almost no black, which is what an
# uncomposed SurfaceView leaves.
PYTHONDONTWRITEBYTECODE=1 python3 - "$SHOT" <<'EOF' || fail "the screenshot does not show RustDesk's screen"
import struct, sys, zlib

BLUE = (0x21, 0x96, 0xF3)
MIN_BAR, MIN_COLORS, MAX_BLACK = 10_000, 16, 0.10

data = open(sys.argv[1], "rb").read()
position, idat = 8, b""
while position < len(data):
    length, kind = struct.unpack(">I4s", data[position:position + 8])
    body = data[position + 8:position + 8 + length]
    if kind == b"IHDR":
        width, height, depth, color = struct.unpack(">IIBB", body[:10])
    elif kind == b"IDAT":
        idat += body
    position += 12 + length
assert depth == 8 and color in (2, 6), f"unexpected PNG format {depth} {color}"
channels = 3 if color == 2 else 4
raw, stride, offset = zlib.decompress(idat), width * channels, 0
rows, previous = [], bytearray(stride)
for _ in range(height):
    kind, row = raw[offset], bytearray(raw[offset + 1:offset + 1 + stride])
    offset += stride + 1
    for i in range(stride):
        left = row[i - channels] if i >= channels else 0
        up, corner = previous[i], previous[i - channels] if i >= channels else 0
        if kind == 1:
            row[i] = (row[i] + left) & 255
        elif kind == 2:
            row[i] = (row[i] + up) & 255
        elif kind == 3:
            row[i] = (row[i] + (left + up) // 2) & 255
        elif kind == 4:
            p = left + up - corner
            pa, pb, pc = abs(p - left), abs(p - up), abs(p - corner)
            row[i] = (row[i] + (left if pa <= pb and pa <= pc else up if pb <= pc else corner)) & 255
    rows.append(row)
    previous = row

def pixel(x, y):
    return tuple(rows[y][x * channels:x * channels + 3])

bar = [(x, y) for y in range(height) for x in range(width)
       if all(abs(a - b) <= 2 for a, b in zip(pixel(x, y), BLUE))]
print(f"screenshot {width}x{height}: {len(bar)} app bar pixels")
if len(bar) < MIN_BAR:
    raise SystemExit("no app bar")
left, right = min(x for x, _ in bar), max(x for x, _ in bar)
below = max(y for _, y in bar) + 1
page = [pixel(x, y) for y in range(below, height) for x in range(left, right + 1)]
colors = len(set(page))
black = sum(1 for p in page if max(p) <= 8) / max(len(page), 1)
print(f"window below the bar: {len(page)} pixels, {colors} colors, {black:.1%} black")
if colors < MIN_COLORS:
    raise SystemExit("the window is nearly one color")
if black > MAX_BLACK:
    raise SystemExit("the window is mostly black: the Flutter layer is not composed into it")
EOF
reached "the screenshot shows RustDesk's screen"

grep -E "^uxdroid: (first frame|activity fully drawn)" "$WORK/out"
echo "unhandled service calls and default native bodies: $(grep -c -E '^uxdroid: (unhandled|unimplemented native) ' "$WORK/out")"
grep -E "^uxdroid: (unhandled|unimplemented native) " "$WORK/out" | sed -E 's/^uxdroid: //; s/ returned its default$//' | sort | cut -c1-160
echo "ok: RustDesk's first Flutter frame is in a Wayland window"
