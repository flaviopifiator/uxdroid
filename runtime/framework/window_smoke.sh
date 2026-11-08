#!/bin/bash
# Shows an SDK-built APK's Activity in a Wayland window: AOSP's ActivityThread resumes it, its
# ViewRootImpl draws the decor view in software, and the pixels are read back from a headless
# compositor. Runs in the dev service; the APK is ours, built by build_system.sh. With ARCH=arm64 it
# checks the arm64 package from dev-arm64; on an x86_64 machine, under qemu-aarch64, with
# runtime/art/qemu-bin over the package's bin/ for the programs uxdroid starts:
#
#   docker compose run --rm -e ARCH=arm64 -v ./runtime/art/qemu-bin:/runtime/art-arm64-host/bin:ro \
#       -v uxdroid-art_art_runtime:/runtime-real:ro dev-arm64 bash /work/runtime/framework/window_smoke.sh
set -u -o pipefail
WORK=$(mktemp -d)
# Removed however the script ends, a failure before the full trap below included.
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; tail -"${TAIL:-25}" "$WORK/out" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-cli --manifest-path /work/Cargo.toml || fail "build"
source /work/runtime/art/release_dir.sh
TARGET="$RELEASE"
LIBRARY="$TARGET/libuxdroid_system.so"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/$ARCH"
source /work/runtime/art/test_lib.sh
start_compositor
# Installed as uxdroid install leaves it: compiled, with its manifest recorded and its default bodies.
UXDROID_RUNTIME="$RUNTIME" "${RUNNER[@]}" "$TARGET/uxdroid" install "$RUNTIME/samples/framework/app.apk" --data "$ANDROID_DATA" \
    >"$WORK/install" 2>&1 || { cat "$WORK/install"; fail "install"; }

CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
# The wrapper over bin/ already runs the program under qemu.
DALVIKVM=("${RUNNER[@]}" "$RUNTIME/bin/dalvikvm64")
[ -d /runtime-real ] && DALVIKVM=("$RUNTIME/bin/dalvikvm64")
"${DALVIKVM[@]}" "${ART_FLAGS[@]}" -cp "$CP" dev.uxdroid.system.Launcher \
    "$RUNTIME" "$LIBRARY" "$ANDROID_DATA/app/com.example.framework/base.apk" >"$WORK/out" 2>&1 &
app=$!
trap 'kill "$app" "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT
for _ in $(seq 600); do
    grep -q "first frame posted" "$WORK/out" && break
    kill -0 "$app" 2>/dev/null || break
    pause 0.05
done

expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
expect "^activity resumed com\.example\.framework\.MainActivity$" "the Activity did not reach onResume"
expect "first frame posted" "ViewRootImpl posted no frame"
if grep -E "uxdroid: (the install record|default bodies of) " "$WORK/out"; then fail "what the install prepared was not used"; fi
kill -0 "$app" 2>/dev/null || fail "the app exited after its first frame"

# The compositor fades the desktop in, which shifts colors until it finishes.
pause 2
# The attached window has one id for all its views, which AndroidX transitions compare.
expect "^window id present, the same for the window's views$" "the attached window has no window id"
(cd "$WORK" && weston-screenshooter >"$WORK/shot" 2>&1) || { cat "$WORK/shot"; fail "screenshot"; }
SHOT=$(ls "$WORK"/wayland-screenshot-*.png | head -1)
PYTHONDONTWRITEBYTECODE=1 python3 - "$SHOT" <<'EOF' || fail "pixels"
import struct, sys, zlib

AMBER = (0xFF, 0xE0, 0x82)  # MainActivity.BACKGROUND
MIN_BACKGROUND = 100_000
MIN_INK = 150

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

background = [(x, y) for y in range(height) for x in range(width)
              if all(abs(a - b) <= 1 for a, b in zip(pixel(x, y), AMBER))]
if len(background) < MIN_BACKGROUND:
    raise SystemExit(f"the content view's background is missing: {len(background)} amber pixels")
x0, x1 = min(x for x, _ in background), max(x for x, _ in background)
y0, y1 = min(y for _, y in background), max(y for _, y in background)
# The text is dark on amber, inside the content view: well under amber's brightness.
ink = sum(1 for y in range(y0, y1 + 1) for x in range(x0, x1 + 1) if sum(pixel(x, y)) < 0.6 * sum(AMBER))
print(f"screenshot {width}x{height}: content view at {x0},{y0}-{x1},{y1}, {len(background)} amber pixels, {ink} dark text pixels")
if ink < MIN_INK:
    raise SystemExit("the content view's text is missing")
EOF
grep -E "^(application|activity) |first frame posted|unhandled|unimplemented native" "$WORK/out" | cut -c1-300
echo "ok: AOSP's ViewRootImpl drew an APK's Activity into a Wayland window"
