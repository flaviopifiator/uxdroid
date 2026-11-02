#!/bin/bash
# Copies only what running ART needs into /runtime, with its notices, and builds the sample apps.
set -eu -o pipefail
HOST=/aosp/out/host/linux-x86
RUNTIME=/runtime/art
rm -rf "$RUNTIME"
mkdir -p "$RUNTIME/bin" "$RUNTIME/samples" "$RUNTIME/javalib" "$RUNTIME/notices"
cp "$HOST/bin/dalvikvm64" "$HOST/bin/dex2oat64" "$RUNTIME/bin/"
cp -r "$HOST/lib64" "$RUNTIME/lib64"
# Debug builds of the runtime are not needed to run apps.
rm -f "$RUNTIME"/lib64/lib*d.so.debug
find "$RUNTIME/lib64" -regextype posix-extended \
    -regex '.*/lib(artd|artd-.*|dexfiled|dexfiled_.*|artbased|profiled|openjdkd|openjdkjvmd|openjdkjvmtid|adbconnectiond|tid|dt_fd_forward_debug)\.so' -delete
cp -r "$HOST/com.android.i18n" "$HOST/com.android.tzdata" "$RUNTIME/"
for jar in \
    apex/com.android.art/javalib/core-oj.jar apex/com.android.art/javalib/core-libart.jar \
    apex/com.android.art/javalib/okhttp.jar apex/com.android.art/javalib/bouncycastle.jar \
    apex/com.android.art/javalib/apache-xml.jar apex/com.android.i18n/javalib/core-icu4j.jar \
    apex/com.android.conscrypt/javalib/conscrypt.jar; do
    cp "$HOST/$jar" "$RUNTIME/javalib/"
done
find "$RUNTIME/lib64" -name '*.so' -exec /aosp/prebuilts/clang/host/linux-x86/clang-r547379/bin/llvm-strip --strip-debug {} +

cd /aosp
for project in art libcore libnativehelper external/icu external/okhttp external/bouncycastle \
    external/conscrypt external/apache-xml external/zlib external/lz4 external/libcxx external/libcxxabi \
    external/libunwind_llvm external/fmtlib external/tinyxml2 system/libbase system/logging \
    system/unwinding bionic; do
    for name in NOTICE LICENSE; do
        if [ -f "$project/$name" ]; then
            target="$RUNTIME/notices/${project//\//_}.$name"
            cp "$project/$name" "$target"
        fi
    done
done

JDK=$(ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
WORK=$(mktemp -d)
for program in Hello Compute; do
    "$JDK/bin/javac" --release 8 -d "$WORK/$program" "/work/runtime/art/samples/$program.java" 2>&1 | { grep -v "warning:" || true; }
    "$JDK/bin/java" -cp prebuilts/r8/r8.jar com.android.tools.r8.D8 --min-api 26 \
        --output "$RUNTIME/samples/$program.jar" "$WORK/$program/$program.class"
done
du -sh "$RUNTIME"
ls "$RUNTIME/notices" | wc -l
