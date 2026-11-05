#!/bin/bash
# Proves DEX reaches a bionic library through the uxdroid loader. Runs in the dev service, which
# has the NDK and the packaged ART.
set -eu -o pipefail
source /work/runtime/art/run.sh
WORK=$(mktemp -d)
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
"$TOOLCHAIN/bin/x86_64-linux-android23-clang++" -shared -fPIC -O2 -static-libstdc++ \
    /work/runtime/art/samples/jni/nativecall.cpp -llog -o "$WORK/libnativecall.so"
cargo build -q --release -p uxdroid-nativebridge --manifest-path /work/Cargo.toml
cp /cache/target/release/libuxdroid_nativebridge.so "$WORK/"

export LD_LIBRARY_PATH="$RUNTIME/lib64:$WORK"
"$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
    -Djava.library.path="$WORK" -cp "$RUNTIME/samples/NativeCall.jar" NativeCall 2>"$WORK/err" | tee "$WORK/out"
status=$?
expected='add: 5
greet: hello from bionic, dex
callBack: 42
caught: thrown in C++'
if [ "$status" -ne 0 ] || [ "$(cat "$WORK/out")" != "$expected" ]; then
    echo "FAIL: status $status"
    tail -20 "$WORK/err" | cut -c1-240
    exit 1
fi
grep -q "greeting hello from bionic, dex" "$WORK/err" || { echo "FAIL: bionic log missing"; exit 1; }
echo "ok: JNI crossed into bionic and back, through the uxdroid loader"
