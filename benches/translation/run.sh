#!/bin/sh
# The translation benchmark: the same kernel (SHA-256 and zlib deflate over one buffer) built for
# x86_64 and run natively through the bridge, built for arm64 and run through the bridge's
# translator, and the same arm64 code as a static program under QEMU user.
#
# Run in the translation service, which has the engine, QEMU user and the zlib source:
#   docker compose run --rm translation benches/translation/run.sh [MiB] [runs]
set -eu
MIB=${1:-64}
RUNS=${2:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
TOOLS=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin
ZLIB=${UXDROID_ZLIB:?run in the translation service}
OUT=$(mktemp -d)

# Freestanding, so every build runs the kernel's and zlib's own code and nothing else.
build() {
    compiler=$1
    output=$2
    shift 2
    "$TOOLS/$compiler" -O2 -fPIC -ffreestanding -fvisibility=hidden -DZ_SOLO -I"$ZLIB" "$@" \
        "$HERE/kernel.c" "$ZLIB/adler32.c" "$ZLIB/crc32.c" "$ZLIB/deflate.c" "$ZLIB/trees.c" \
        "$ZLIB/zutil.c" -o "$OUT/$output"
}
build x86_64-linux-android24-clang libkernel_x86_64.so -shared -nostdlib
build aarch64-linux-android24-clang libkernel_arm64.so -shared -nostdlib
build aarch64-linux-android24-clang kernel_qemu -static -nostdlib "$HERE/qemu_main.c"

cargo build --release -q -p uxdroid-translate --example kernel_bench
BENCH=${CARGO_TARGET_DIR:-target}/release/examples/kernel_bench

echo "qemu: $(qemu-aarch64 --version | head -1)" >&2
echo "engine: $UXDROID_TRANSLATOR" >&2
for run in $(seq "$RUNS"); do
    printf '{"label":"native x86_64","result":%s}\n' "$("$BENCH" "$OUT/libkernel_x86_64.so" "$MIB")"
    printf '{"label":"translated (uxdroid, Dynarmic)","result":%s}\n' "$("$BENCH" "$OUT/libkernel_arm64.so" "$MIB")"
    printf '{"label":"QEMU user","result":%s}\n' "$(qemu-aarch64 "$OUT/kernel_qemu" "$MIB")"
done > "$OUT/results.jsonl"
python3 "$HERE/report.py" "$OUT/results.jsonl" "$MIB"
rm -rf "$OUT"
