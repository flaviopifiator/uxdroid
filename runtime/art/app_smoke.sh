#!/bin/bash
# Runs a real APK end to end: DEX asks a bionic library for a color and paints a Wayland window,
# checked by reading pixels back from the compositor. Runs in the dev service.
set -u -o pipefail
WORK=$(mktemp -d)
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
RUNTIME=/runtime/art
fail() { echo "FAIL: $*"; [ -f "$WORK/app" ] && tail -15 "$WORK/app" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-platform \
    -p uxdroid-testkit --manifest-path /work/Cargo.toml || fail "build"
BIN=/cache/target/release
"$TOOLCHAIN/bin/x86_64-linux-android26-clang++" -shared -fPIC -O2 -static-libstdc++ \
    /work/runtime/art/samples/first/first_native.cpp -o "$WORK/libfirstnative.so" || fail "NDK build"
"$BIN/make-first-apk" "$RUNTIME/samples/first/classes.dex" "$WORK/libfirstnative.so" "$WORK/first.apk" || fail "APK"

export XDG_RUNTIME_DIR="$WORK/run" HOME="$WORK/home" WAYLAND_DISPLAY=uxdroid-test UXDROID_RUNTIME="$RUNTIME"
mkdir -p "$XDG_RUNTIME_DIR" "$HOME"; chmod 700 "$XDG_RUNTIME_DIR"
weston --backend=headless --renderer=pixman --socket="$WAYLAND_DISPLAY" --debug --idle-time=0 \
    --width=800 --height=600 >"$WORK/weston" 2>&1 &
compositor=$!
trap 'kill "$compositor" "${app:-}" 2>/dev/null' EXIT
for _ in $(seq 100); do [ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] && break; python3 -c 'import time; time.sleep(0.05)'; done
[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ] || { tail -5 "$WORK/weston"; fail "no compositor"; }

"$BIN/uxdroid" inspect "$WORK/first.apk" | grep -E "package|launcher|execution"
"$BIN/uxdroid" run "$WORK/first.apk" >"$WORK/app" 2>&1 &
app=$!
for _ in $(seq 400); do grep -q "frame presented" "$WORK/app" && break; kill -0 "$app" 2>/dev/null || break; python3 -c 'import time; time.sleep(0.05)'; done
grep -q "activity created in com.example.first" "$WORK/app" || fail "the Activity did not run"
grep -q "frame presented" "$WORK/app" || fail "no frame"

# The compositor paints on its own schedule, after the app's frame arrives.
python3 -c 'import time; time.sleep(0.5)'
cd "$WORK"
weston-screenshooter >"$WORK/shot" 2>&1 || { cat "$WORK/shot"; fail "screenshot"; }
SHOT=$(ls "$WORK"/wayland-screenshot-*.png | head -1)
python3 /work/runtime/art/check_pixels.py "$SHOT" || fail "pixels"
echo "ok: an APK opened a Wayland window, with its color from bionic and its text from DEX"
