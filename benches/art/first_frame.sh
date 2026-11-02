#!/bin/bash
# Measures uxdroid run from process start to the first frame on a headless compositor. Runs in dev.
set -u -o pipefail
WORK=$(mktemp -d)
RUNS="${RUNS:-30}"
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
BIN=/cache/target/release
cargo build -q --release -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-platform -p uxdroid-testkit \
    --manifest-path /work/Cargo.toml || exit 1
source /work/runtime/art/test_lib.sh
start_compositor
"$TOOLCHAIN/bin/x86_64-linux-android26-clang++" -shared -fPIC -O2 -static-libstdc++ \
    /work/runtime/art/samples/first/first_native.cpp -o "$WORK/libfirstnative.so" || exit 1
"$BIN/make-first-apk" /runtime/art/samples/first/classes.dex "$WORK/libfirstnative.so" "$WORK/first.apk" || exit 1
"$BIN/uxdroid" run "$WORK/first.apk" --exit-after-first-frame >/dev/null 2>&1
python3 - "$RUNS" "$BIN/uxdroid" run "$WORK/first.apk" <<'PY'
import subprocess
import sys
import time

runs = int(sys.argv[1])
samples = []
for _ in range(runs):
    start = time.perf_counter()
    process = subprocess.Popen(sys.argv[2:], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    for line in process.stdout:
        if "frame presented" in line:
            break
    samples.append((time.perf_counter() - start) * 1e3)
    process.kill()
    process.wait()
samples.sort()
print(f"first frame: median {samples[runs // 2]:.1f} ms, min {samples[0]:.1f} ms, p90 {samples[runs * 9 // 10]:.1f} ms")
PY
