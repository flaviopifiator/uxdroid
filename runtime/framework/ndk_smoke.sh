#!/bin/bash
# Runs an APK whose native library, built here by the NDK, uses the Android system libraries apps link
# against: liblog, libandroid's assets, looper and native window, libjnigraphics, EGL and GLES over
# Mesa, and OpenSL ES. The native bridge loads it and binds those libraries to their signature tables.
# Two screenshots from a headless compositor check the ANativeWindow frame, then the EGL one. Runs in
# the dev service, which has the NDK and Mesa; the APK is ours, built by build_system.sh. With
# KEEP=<directory>, the log and both screenshots are copied there.
set -u -o pipefail
WORK=$(mktemp -d)
fail() { echo "FAIL: $*"; tail -"${TAIL:-25}" "$WORK/out" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-nativebridge --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"

# The installer's part: the app's native library goes where ApplicationInfo.nativeLibraryDir points.
LIBS="$ANDROID_DATA/data/com.example.ndk/lib"
mkdir -p "$LIBS"
"$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang" -shared -fPIC -O2 \
    -Wall -Werror /work/runtime/framework/samples/ndk/jni/ndksample.c -o "$LIBS/libndksample.so" \
    -landroid -ljnigraphics -llog -lEGL -lGLESv2 -lOpenSLES || fail "the NDK could not build the sample"

source /work/runtime/art/test_lib.sh
start_compositor

export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
"$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
    -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" \
    "$RUNTIME/samples/framework/ndk.apk" >"$WORK/out" 2>&1 &
app=$!
trap 'kill "$app" "$compositor" 2>/dev/null; wait 2>/dev/null; [ -n "${KEEP:-}" ] && cp "$WORK/out" "$WORK"/*.png "$KEEP"; rm -rf "$WORK"' EXIT

wait_for() {
    for _ in $(seq 600); do
        grep -q "$1" "$WORK/out" && return
        kill -0 "$app" 2>/dev/null || fail "the app exited before \"$1\""
        pause 0.05
    done
    fail "no \"$1\""
}
expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }

wait_for "ndk cpu frame:"
expect "^ndk check: assets bitmap opensl looper ok$" "the native checks failed"
expect "^ndk bitmap pixel written natively: ff405060$" "AndroidBitmap_unlockPixels did not write back"
expect "^I/ndksample: liblog print 42$" "__android_log_print"
expect "^W/ndksample: liblog write$" "__android_log_write"
expect "^E/ndksample: liblog buf_write$" "__android_log_buf_write"
expect "^I/ndksample: liblog vprint ok$" "__android_log_vprint"
expect "^I/ndksample: looper callback fired: byte x, events 1, data 7, main thread yes$" \
    "the pipe's ALooper callback did not run on the main Looper"
expect "^ndk cpu frame: ([0-9]+)x([0-9]+) format 1, window \\1x\\2, posted$" "ANativeWindow did not post"

shoot() {
    rm -f "$WORK"/wayland-screenshot-*.png
    (cd "$WORK" && weston-screenshooter >"$WORK/shot" 2>&1) || { cat "$WORK/shot"; fail "screenshot"; }
    mv "$(ls "$WORK"/wayland-screenshot-*.png | head -1)" "$WORK/$1.png"
    echo "$WORK/$1.png"
}

# Colors and the layout drawGl makes: an ANativeWindow frame in teal, then an EGL frame in pink
# with its top-left quarter white.
check() {
    PYTHONDONTWRITEBYTECODE=1 python3 - "$@" <<'EOF' || fail "pixels: $1"
import struct, sys, zlib

TEAL, PINK, WHITE = (0x00, 0x89, 0x7B), (204, 51, 153), (255, 255, 255)
MIN_PIXELS = 50_000

data = open(sys.argv[2], "rb").read()
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

def where(target):
    return [(x, y) for y in range(height) for x in range(width)
            if all(abs(a - b) <= 1 for a, b in zip(rows[y][x * channels:x * channels + 3], target))]

def centre(points):
    return sum(x for x, _ in points) / len(points), sum(y for _, y in points) / len(points)

if sys.argv[1] == "cpu":
    teal = where(TEAL)
    print(f"cpu screenshot {width}x{height}: {len(teal)} teal pixels")
    if len(teal) < MIN_PIXELS:
        raise SystemExit("the ANativeWindow frame is missing")
else:
    pink, white = where(PINK), where(WHITE)
    print(f"egl screenshot {width}x{height}: {len(pink)} pink and {len(white)} white pixels")
    if len(pink) < MIN_PIXELS or len(white) < MIN_PIXELS // 4:
        raise SystemExit("the EGL frame is missing")
    (pink_x, pink_y), (white_x, white_y) = centre(pink), centre(white)
    # The window's white quarter: white inside the pink's bounds, unlike the desktop's.
    left, top = min(x for x, _ in pink), min(y for _, y in pink)
    window = [(x, y) for x, y in white if x >= left and y >= top]
    if len(window) < MIN_PIXELS // 4:
        raise SystemExit(f"the white quarter is missing: {len(window)} pixels")
    wx, wy = centre(window)
    if not (wx < pink_x and wy < pink_y):
        raise SystemExit(f"the white quarter is not at the top left: white {wx:.0f},{wy:.0f}, pink {pink_x:.0f},{pink_y:.0f}")
EOF
}

# The compositor fades the desktop in, which shifts colors until it finishes.
pause 2
shoot cpu >/dev/null
check cpu "$WORK/cpu.png"
wait_for "ndk egl frame:"
expect "^ndk egl frame: EGL 1\.[0-9]+, .+, [0-9]+x[0-9]+, visual 1, corner 204 51 153, swapped$" \
    "EGL did not render and swap"
pause 0.5
shoot egl >/dev/null
check egl "$WORK/egl.png"
kill -0 "$app" 2>/dev/null || fail "the app exited after its frames"

grep -E "^ndk |looper callback|unhandled|unimplemented native|uxdroid: " "$WORK/out" | cut -c1-240
echo "ok: an NDK library drew through ANativeWindow and EGL, read its assets and ran on the main ALooper"
