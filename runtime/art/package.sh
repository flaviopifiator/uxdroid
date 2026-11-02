#!/bin/bash
# Copies only what running ART needs into /runtime, then compiles the boot image and sample apps.
set -eu
HOST=/aosp/out/host/linux-x86
RUNTIME=/runtime/art
rm -rf "$RUNTIME"
mkdir -p "$RUNTIME/bin" "$RUNTIME/samples"
cp "$HOST/bin/dalvikvm64" "$HOST/bin/dex2oat64" "$HOST/bin/oatdump64" "$RUNTIME/bin/" 2>/dev/null || true
cp -r "$HOST/lib64" "$RUNTIME/lib64"
cp -r "$HOST/com.android.i18n" "$HOST/com.android.tzdata" "$RUNTIME/"
mkdir -p "$RUNTIME/javalib"
for jar in \
    apex/com.android.art/javalib/core-oj.jar apex/com.android.art/javalib/core-libart.jar \
    apex/com.android.art/javalib/okhttp.jar apex/com.android.art/javalib/bouncycastle.jar \
    apex/com.android.art/javalib/apache-xml.jar apex/com.android.i18n/javalib/core-icu4j.jar \
    apex/com.android.conscrypt/javalib/conscrypt.jar; do
    cp "$HOST/$jar" "$RUNTIME/javalib/"
done
find "$RUNTIME/lib64" -name '*.so' -exec /aosp/prebuilts/clang/host/linux-x86/clang-r547379/bin/llvm-strip --strip-debug {} + 2>/dev/null || true

cd /aosp
JDK=$(ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
WORK=$(mktemp -d)
for program in Hello Compute; do
    "$JDK/bin/javac" --release 8 -d "$WORK/$program" "/work/runtime/art/samples/$program.java" 2>&1 | grep -v "warning:" || true
    "$JDK/bin/java" -cp prebuilts/r8/r8.jar com.android.tools.r8.D8 --min-api 26 \
        --output "$RUNTIME/samples/$program.jar" "$WORK/$program/$program.class"
done
du -sh "$RUNTIME"
