#!/bin/sh
# The translation benchmark's game kernel (games.c): built for x86_64 and run natively through the
# bridge, built for arm64 and run through the bridge's translator, and the compute phases as a
# static arm64 program under QEMU user.
#
# Run in the translation service, which has the engine and QEMU user:
#   docker compose run --rm translation benches/translation/games.sh [runs]
set -eu
RUNS=${1:-5}
HERE=$(cd "$(dirname "$0")" && pwd)
TOOLS=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin
OUT=$(mktemp -d)

# -ffp-contract=off: no fused multiply-add on either CPU, so every build computes the same bits.
FLAGS="-O2 -fPIC -ffreestanding -fno-builtin -fvisibility=hidden -ffp-contract=off"
"$TOOLS/x86_64-linux-android24-clang" $FLAGS -shared -nostdlib "$HERE/games.c" -o "$OUT/libgames_x86_64.so"
"$TOOLS/aarch64-linux-android24-clang" $FLAGS -shared -nostdlib "$HERE/games.c" -o "$OUT/libgames_arm64.so"
"$TOOLS/aarch64-linux-android24-clang" $FLAGS -static -nostdlib "$HERE/games.c" "$HERE/games_qemu.c" -o "$OUT/games_qemu"

cargo build --release -q -p uxdroid-translate --example game_bench
BENCH=${CARGO_TARGET_DIR:-target}/release/examples/game_bench

echo "qemu: $(qemu-aarch64 --version | head -1)" >&2
echo "engine: $UXDROID_TRANSLATOR" >&2
for run in $(seq "$RUNS"); do
    printf '{"label":"native x86_64","result":%s}\n' "$("$BENCH" "$OUT/libgames_x86_64.so")"
    printf '{"label":"translated (uxdroid, Dynarmic)","result":%s}\n' "$("$BENCH" "$OUT/libgames_arm64.so")"
    printf '{"label":"QEMU user","result":%s}\n' "$(qemu-aarch64 "$OUT/games_qemu")"
done > "$OUT/results.jsonl"
python3 "$HERE/games_report.py" "$OUT/results.jsonl"
rm -rf "$OUT"
