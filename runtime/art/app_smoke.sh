#!/bin/bash
# Runs a real APK end to end: DEX asks a bionic library for a color and paints a Wayland window,
# checked by reading pixels back from the compositor. Runs in the dev service; with ARCH=arm64 in
# dev-arm64 on the arm64 package, as runtime/framework/window_smoke.sh describes for qemu.
set -u -o pipefail
WORK=$(mktemp -d)
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
source /work/runtime/art/run.sh
NDK_TARGET=x86_64-linux-android26
[ "$ARCH" = arm64 ] && NDK_TARGET=aarch64-linux-android26
fail() { echo "FAIL: $*"; [ -f "$WORK/app" ] && tail -15 "$WORK/app" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-platform \
    -p uxdroid-testkit --manifest-path /work/Cargo.toml || fail "build"
source /work/runtime/art/release_dir.sh
BIN="$RELEASE"
"$TOOLCHAIN/bin/$NDK_TARGET-clang++" -shared -fPIC -O2 -static-libstdc++ \
    /work/runtime/art/samples/first/first_native.cpp -o "$WORK/libfirstnative.so" || fail "NDK build"
"${RUNNER[@]}" "$BIN/make-first-apk" "$RUNTIME/samples/first/classes.dex" "$WORK/libfirstnative.so" "$WORK/first.apk" || fail "APK"

source /work/runtime/art/test_lib.sh
start_compositor

"${RUNNER[@]}" "$BIN/uxdroid" inspect "$WORK/first.apk" | grep -E "package|launcher|execution"
"${RUNNER[@]}" "$BIN/uxdroid" run "$WORK/first.apk" --runtime "$RUNTIME" >"$WORK/app" 2>&1 &
app=$!
for _ in $(seq 400); do grep -q "frame presented" "$WORK/app" && break; kill -0 "$app" 2>/dev/null || break; pause 0.05; done
grep -q "activity created in com.example.first" "$WORK/app" || fail "the Activity did not run"
grep -q "frame presented" "$WORK/app" || fail "no frame"

# The compositor fades the desktop in, which shifts colors until it finishes.
pause 2
cd "$WORK"
weston-screenshooter >"$WORK/shot" 2>&1 || { cat "$WORK/shot"; fail "screenshot"; }
SHOT=$(ls "$WORK"/wayland-screenshot-*.png | head -1)
python3 /work/runtime/art/check_pixels.py "$SHOT" || fail "pixels"
echo "ok: an APK opened a Wayland window, with its color from bionic and its text from DEX"
