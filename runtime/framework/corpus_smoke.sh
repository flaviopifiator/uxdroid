#!/bin/bash
# Runs the compatibility corpus, open-source apps built and signed by F-Droid, on AOSP's framework,
# each until it shows a usable first screen. Their code is untrusted, so this runs only in the sandbox:
#
#   docker compose run --rm dev cargo build --release -p uxdroid-system -p uxdroid-nativebridge -p uxdroid-cli
#   UXDROID_SAMPLES=<directory with the APKs> docker compose run --rm sandbox \
#       bash /work/runtime/framework/corpus_smoke.sh
#
# An app whose APK is not in the directory is skipped. CORPUS_APPS names a subset by package; WAIT is
# how long an app may take to its first frame (90 s); SHOTS, when it names a writable directory, keeps
# each screenshot and output there as <package>.png and .out. Each app must post a frame of its
# activity's window, still run 5 s later, and its screenshot must not be blank. An app whose output
# shows a gap uxdroid knows of, listed in KNOWN, is reported as such: it reached its window, but the
# screen is not usable yet.
set -u -o pipefail
TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
SAMPLES="${SAMPLES_DIR:-/samples}"
# package, APK file name pattern, what its first screen is.
CORPUS="org.isoron.uhabits org.isoron.uhabits_*.apk Loop Habit Tracker's welcome screen
net.gsantner.markor net.gsantner.markor_*.apk Markor's introduction
com.beemdevelopment.aegis com.beemdevelopment.aegis_*.apk Aegis' welcome screen
com.ichi2.anki com.ichi2.anki_*.apk AnkiDroid's deck list
io.anuke.mindustry io.anuke.mindustry_*.apk Mindustry's main menu"

# package|a line of its output that shows the gap|what is missing.
KNOWN="com.ichi2.anki|^E AnkiDroid: Instrumentation/ checkWebViewAvailable$|no WebView provider: AnkiDroid's deck list is under a fatal error dialog"

for file in uxdroid libuxdroid_system.so libuxdroid_nativebridge.so; do
    [ -e "$TARGET/$file" ] || { echo "FAIL: no $TARGET/$file: build the release libraries in the dev service first"; exit 1; }
done
source /work/runtime/art/run.sh
[ -e "$RUNTIME/framework/uxdroid-system.jar" ] || { echo "FAIL: no framework in $RUNTIME: run runtime/framework/build.sh"; exit 1; }
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
ROOT=$(mktemp -d)
app="" compositor=""
trap 'rm -rf "$ROOT"' EXIT

# Whether a PNG screenshot shows something where the app's window is, the middle 400 pixels under the
# compositor's panel: at least MIN_COLORS colors, and no color covering more than MAX_SHARE of them,
# which an unpainted or one-color window would.
not_blank() {
    PYTHONDONTWRITEBYTECODE=1 python3 - "$1" <<'EOF'
import struct, sys, zlib
from collections import Counter

MIN_COLORS, MAX_SHARE = 16, 0.97
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
counts, previous = Counter(), bytearray(stride)
left, right, top = max(0, width // 2 - 200), min(width, width // 2 + 200), min(32, height - 1)
for y in range(height):
    kind, row = raw[offset], bytearray(raw[offset + 1:offset + 1 + stride])
    offset += stride + 1
    for i in range(stride):
        # The byte before, not the region's left edge, which this loop must not overwrite.
        before = row[i - channels] if i >= channels else 0
        up, corner = previous[i], previous[i - channels] if i >= channels else 0
        if kind == 1:
            row[i] = (row[i] + before) & 255
        elif kind == 2:
            row[i] = (row[i] + up) & 255
        elif kind == 3:
            row[i] = (row[i] + (before + up) // 2) & 255
        elif kind == 4:
            p = before + up - corner
            pa, pb, pc = abs(p - before), abs(p - up), abs(p - corner)
            row[i] = (row[i] + (before if pa <= pb and pa <= pc else up if pb <= pc else corner)) & 255
    if y >= top:
        counts.update(bytes(row[x * channels:x * channels + 3]) for x in range(left, right))
    previous = row
share = counts.most_common(1)[0][1] / sum(counts.values())
print(f"{len(counts)} colors in the window, the most common covers {share:.0%}")
if len(counts) < MIN_COLORS or share > MAX_SHARE:
    raise SystemExit(1)
EOF
}

# Installs and runs one app; prints its stages and returns non-zero naming the furthest one reached.
run_app() {
    local package=$1 apk=$2 screen=$3 work="$ROOT/$1" stage="nothing" result
    mkdir -p "$work"
    export ANDROID_DATA="$work/data"
    mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
    WORK="$work"
    source /work/runtime/art/test_lib.sh
    fail() {
        echo "FAIL $package: $1 (furthest stage reached: $stage)"
        grep -E "^(Caused by|Exception|java\.|[a-z]+\.[a-zA-Z.]+(Exception|Error))|FATAL" "$work/out" 2>/dev/null \
            | grep -v -E "ServiceNotFoundException|AconfigStorageReadException" | head -"${TAIL:-6}" | cut -c1-200
        kill "$app" "$compositor" 2>/dev/null
        wait 2>/dev/null
        keep
        return 1
    }
    # With SHOTS, the app's output is kept beside its screenshot.
    keep() { [ -n "${SHOTS:-}" ] && [ -w "${SHOTS:-}" ] && cp "$work/out" "$SHOTS/$package.out" 2>/dev/null; true; }
    UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$apk" --data "$ANDROID_DATA" >"$work/install" 2>&1 \
        || { cat "$work/install"; fail "install"; return 1; }
    stage="installed"
    start_compositor
    trap 'kill "$app" "$compositor" 2>/dev/null; rm -rf "$ROOT"' EXIT
    export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
    local start
    start=$(date +%s%N)
    "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
        -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" \
        "$ANDROID_DATA/app/$package/base.apk" >"$work/out" 2>&1 &
    app=$!
    result=""
    for _ in $(seq $(( ${WAIT:-90} * 20 ))); do
        if grep -q "^uxdroid: first frame posted on $package/" "$work/out"; then
            result="$(( ($(date +%s%N) - start) / 1000000 ))"; break
        fi
        kill -0 "$app" 2>/dev/null || { fail "the app exited before its first frame"; return 1; }
        pause 0.05
    done
    [ -n "$result" ] || { fail "no first frame of its window within ${WAIT:-90} s"; return 1; }
    stage="first frame after $result ms"
    pause 5
    kill -0 "$app" 2>/dev/null || { fail "the app exited within 5 s of its first frame"; return 1; }
    stage="alive 5 s after its first frame"
    (cd "$work" && weston-screenshooter >"$work/shot" 2>&1) || { cat "$work/shot"; fail "screenshot"; return 1; }
    local shot
    shot=$(ls "$work"/wayland-screenshot-*.png 2>/dev/null | head -1)
    [ -n "$shot" ] || { fail "the screenshooter wrote no file"; return 1; }
    [ -n "${SHOTS:-}" ] && [ -w "${SHOTS:-}" ] && cp "$shot" "$SHOTS/$package.png"
    local look
    look=$(not_blank "$shot") || { echo "screenshot: $look"; fail "the screenshot is blank"; return 1; }
    stage="a screenshot that is not blank ($look)"
    kill "$app" "$compositor" 2>/dev/null
    wait 2>/dev/null
    keep
    local gap_package pattern missing
    while IFS='|' read -r gap_package pattern missing; do
        if [ "$gap_package" = "$package" ] && grep -q -E "$pattern" "$work/out"; then
            echo "known gap $package: $missing; $stage"
            return 2
        fi
    done <<<"$KNOWN"
    echo "ok $package: $screen: $stage; $(grep -c -E '^uxdroid: (unhandled|unimplemented native) ' "$work/out") defaults hit"
}

ran=0 failed=0 known=0
while read -r package pattern screen; do
    if [ -n "${CORPUS_APPS:-}" ] && ! [[ " $CORPUS_APPS " == *" $package "* ]]; then continue; fi
    apk=$(ls $SAMPLES/$pattern 2>/dev/null | sort -V | tail -1)
    if [ -z "$apk" ]; then
        echo "skip $package: no $pattern in $SAMPLES"
        continue
    fi
    ran=$((ran + 1))
    run_app "$package" "$apk" "$screen"
    case $? in
        0) ;;
        2) known=$((known + 1)) ;;
        *) failed=$((failed + 1)) ;;
    esac
done <<<"$CORPUS"
[ "$ran" -gt 0 ] || { echo "FAIL: no corpus APK in $SAMPLES"; exit 1; }
[ "$failed" -eq 0 ] || { echo "FAIL: $failed of $ran corpus apps did not reach a usable first screen"; exit 1; }
echo "ok: $((ran - known)) of $ran corpus apps reached a usable first screen, $known stopped at a known gap"
