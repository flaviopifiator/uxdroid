#!/bin/bash
# Compiles the boot image and sample apps to x86_64 machine code with dex2oat.
set -eu
source /work/runtime/art/run.sh
IMAGE_DIR="$RUNTIME/framework/x86_64"
mkdir -p "$IMAGE_DIR" "$RUNTIME/samples/oat/x86_64"
DEX_ARGS=()
for jar in "${JARS[@]}"; do
    DEX_ARGS+=("--dex-file=$RUNTIME/javalib/$jar.jar" "--dex-location=$RUNTIME/javalib/$jar.jar")
done
time "$RUNTIME/bin/dex2oat64" --runtime-arg -Xms64m --runtime-arg -Xmx1g \
    --compiler-filter=speed --instruction-set=x86_64 --android-root="$RUNTIME" --host \
    --base=0x70000000 "${DEX_ARGS[@]}" \
    --image="$IMAGE_DIR/boot.art" --oat-file="$IMAGE_DIR/boot.oat" --no-inline-from=core-oj.jar 2>&1 | tail -3 | cut -c1-200
du -sh "$IMAGE_DIR"
for program in Hello Compute; do
    "$RUNTIME/bin/dex2oat64" --compiler-filter=speed --instruction-set=x86_64 --host \
        --android-root="$RUNTIME" --runtime-arg -Xbootclasspath:"$BOOT" --runtime-arg -Xbootclasspath-locations:"$BOOT" \
        --boot-image="$RUNTIME/framework/boot.art" --dex-file="$RUNTIME/samples/$program.jar" \
        --oat-file="$RUNTIME/samples/oat/x86_64/$program.odex" 2>&1 | tail -2 | cut -c1-200
done
ls -la "$RUNTIME/samples/oat/x86_64/"
