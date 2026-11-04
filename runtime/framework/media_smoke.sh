#!/bin/bash
# Runs NDK media code from DEX: a library built here by the NDK against libmediandk is loaded by ART
# through the native bridge, which binds it to uxdroid's libmediandk table. It looks up codecs and
# finds none, as an app's probe does before taking its software path, and keeps an AMediaFormat. The
# Java side is NativeCall (runtime/art/samples/NativeCall.java), whose four natives the library
# answers with media checks. Runs in the dev service, which has the NDK; the code is ours.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; tail -"${TAIL:-25}" "$WORK/err" 2>/dev/null | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-nativebridge --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
source /work/runtime/art/run.sh

"$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang" -shared -fPIC -O2 \
    -Wall -Werror /work/crates/ndk/tests/media/mediacheck.c -o "$WORK/libnativecall.so" -lmediandk \
    || fail "the NDK could not build the media check"
cp "$TARGET/libuxdroid_nativebridge.so" "$WORK/"

export LD_LIBRARY_PATH="$RUNTIME/lib64:$WORK"
"$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
    -Djava.library.path="$WORK" -cp "$RUNTIME/samples/NativeCall.jar" NativeCall >"$WORK/out" 2>"$WORK/err"
status=$?
# add runs every check of media_check(); a negative sum is the number of the first that failed.
expected='add: 5
greet: mime: string(dex)}
callBack: 42
caught: no codec for video/hevc'
[ "$status" -eq 0 ] || fail "status $status"
[ "$(cat "$WORK/out")" = "$expected" ] || { cat "$WORK/out"; fail "unexpected output"; }
grep -q "uxdroid: libmediandk: no decoder for video/avc: no codec is offered yet" "$WORK/err" \
    || fail "the codec lookup was not reported"
echo "ok: an NDK library found no codec and kept an AMediaFormat, from DEX through the native bridge"
