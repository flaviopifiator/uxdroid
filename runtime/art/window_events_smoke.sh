#!/bin/bash
# Sends close and resize at the exact moment a frame is being presented. Runs in dev.
set -u -o pipefail
WORK=$(mktemp -d)
RUNTIME=/runtime/art
BIN=/cache/target/release
fail() { echo "FAIL: $*"; tail -8 "$WORK/compositor" 2>/dev/null | cut -c1-160; tail -5 "$WORK/out" 2>/dev/null | cut -c1-160; exit 1; }
cargo build -q --release -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-platform -p uxdroid-testkit \
    --manifest-path /work/Cargo.toml || fail "build"
source /work/runtime/art/test_lib.sh
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
"$TOOLCHAIN/bin/x86_64-linux-android26-clang++" -shared -fPIC -O2 -static-libstdc++ \
    /work/runtime/art/samples/first/first_native.cpp -o "$WORK/libfirstnative.so" || fail "NDK build"
"$BIN/make-first-apk" "$RUNTIME/samples/first/classes.dex" "$WORK/libfirstnative.so" "$WORK/first.apk" || fail "APK"

scenario() {
    python3 /work/runtime/art/fake_compositor.py "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" "$1" >"$WORK/compositor" 2>&1 &
    local server=$!
    pause 0.3
    timeout 20 "$BIN/uxdroid" run "$WORK/first.apk" >"$WORK/out" 2>&1
    local status=$?
    kill "$server" 2>/dev/null
    wait "$server" 2>/dev/null
    return "$status"
}

scenario race-close || fail "close during a frame: exit $?"
echo "ok: a close during a frame ends the app"
scenario race-resize || fail "resize during a frame: exit $?"
grep -q "frame 2: buffer 1000x700" "$WORK/compositor" || fail "resize during a frame drew no new frame"
echo "ok: a resize during a frame redraws at the new size"
scenario resize || fail "resize sequence: exit $?"
grep -q "frame 4: buffer 1x1" "$WORK/compositor" || fail "resize sequence did not reach 1x1"
echo "ok: a resize sequence works down to 1x1"
scenario huge
grep -q "cannot" "$WORK/out" && fail "an oversized request killed the app"
echo "ok: an oversized request is clamped"
