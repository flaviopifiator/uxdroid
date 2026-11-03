#!/bin/bash
# Compiles the boot image from a usage profile, and the sample apps, to x86_64 machine code.
set -eu -o pipefail
source /work/runtime/art/run.sh
IMAGE_DIR="$RUNTIME/framework/x86_64"
mkdir -p "$IMAGE_DIR" "$RUNTIME/samples/oat/x86_64"
DEX_ARGS=()
for jar in "${JARS[@]}"; do
    DEX_ARGS+=("--dex-file=$RUNTIME/javalib/$jar.jar" "--dex-location=$RUNTIME/javalib/$jar.jar")
done
# Compiling only what the profile lists shrank the image from 33 to 9.7 MiB and peak memory by
# 11 to 14%, with no slower startup on the measured programs. Everything else runs through the JIT.
HOST=/aosp/out/host/linux-x86
APKS=()
for jar in "${JARS[@]}"; do
    APKS+=("--apk=$RUNTIME/javalib/$jar.jar" "--dex-location=$RUNTIME/javalib/$jar.jar")
done
# The first build has no profile yet: make_profile.sh records one from this full image.
PROFILE="${PROFILE:-/work/runtime/art/boot-profile.txt}"
FILTER=(--compiler-filter=speed)
if [ -e "$PROFILE" ]; then
    # An empty profile would compile nothing and still look like a working image.
    [ -s "$PROFILE" ] || { echo "FAIL: $PROFILE is empty"; exit 1; }
    LD_LIBRARY_PATH="$HOST/lib64" "$HOST/bin/profman" --create-profile-from="$PROFILE" \
        --output-profile-type=boot "${APKS[@]}" --reference-profile-file="$IMAGE_DIR/boot.prof" 2>/dev/null
    FILTER=(--compiler-filter=speed-profile --profile-file="$IMAGE_DIR/boot.prof")
fi
time "$RUNTIME/bin/dex2oat64" --runtime-arg -Xms64m --runtime-arg -Xmx1g \
    "${FILTER[@]}" --instruction-set=x86_64 --android-root="$RUNTIME" --host \
    --base=0x70000000 "${DEX_ARGS[@]}" \
    --image="$IMAGE_DIR/boot.art" --oat-file="$IMAGE_DIR/boot.oat" 2>&1 | tail -3 | cut -c1-200
du -sh "$IMAGE_DIR"
# A boot image with no compiled methods still loads and runs, only slower, so check directly.
MIN_COMPILED_METHODS=100
compiled=$(LD_LIBRARY_PATH="$HOST/lib64" "$HOST/bin/oatdump" --oat-file="$IMAGE_DIR/boot.oat" --no-disassemble 2>/dev/null \
    | grep -cE "CODE: \(code_offset=0x0*[1-9a-f]" || true)
echo "compiled boot methods: $compiled"
[ "$compiled" -ge "$MIN_COMPILED_METHODS" ] || { echo "FAIL: too few compiled boot methods"; exit 1; }
for program in Hello Compute NativeCall; do
    "$RUNTIME/bin/dex2oat64" --compiler-filter=speed --instruction-set=x86_64 --host \
        --android-root="$RUNTIME" --runtime-arg -Xbootclasspath:"$BOOT" --runtime-arg -Xbootclasspath-locations:"$BOOT" \
        --boot-image="$RUNTIME/framework/boot.art" --dex-file="$RUNTIME/samples/$program.jar" \
        --oat-file="$RUNTIME/samples/oat/x86_64/$program.odex" 2>&1 | tail -2 | cut -c1-200
done
ls -la "$RUNTIME/samples/oat/x86_64/"
