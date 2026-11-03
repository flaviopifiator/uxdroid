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
# Every shipped component with its notice. A missing file fails the package, never skipping quietly.
while read -r name source; do
    cp "$source" "$RUNTIME/notices/$name"
done <<'NOTICES'
art art/NOTICE
libcore libcore/NOTICE
libcore-license libcore/LICENSE
libnativehelper libnativehelper/NOTICE
okhttp external/okhttp/LICENSE
bouncycastle external/bouncycastle/NOTICE
apache-xml external/apache-xml/NOTICE
zlib external/zlib/LICENSE
lz4 external/lz4/LICENSE
fmtlib external/fmtlib/NOTICE
libcxx external/libcxx/LICENSE.TXT
libcxxabi external/libcxxabi/NOTICE
libbase system/libbase/NOTICE
oj-libjdwp-GPL-with-Classpath-exception external/oj-libjdwp/LICENSE
icu prebuilts/runtime/mainline/i18n/sdk/licenses/external/icu/NOTICE
conscrypt prebuilts/module_sdk/conscrypt/current/licenses/external/conscrypt/NOTICE
vixl external/vixl/LICENCE
expat external/expat/COPYING
sqlite external/sqlite/LICENSE
lzma external/lzma/NOTICE
NOTICES

JDK=$(ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
WORK=$(mktemp -d)
for program in Hello Compute NativeCall ProfileWork Launcher Workload Holdout; do
    "$JDK/bin/javac" --release 8 -d "$WORK/$program" "/work/runtime/art/samples/$program.java" 2>&1 | { grep -v "warning:" || true; }
    "$JDK/bin/java" -cp prebuilts/r8/r8.jar com.android.tools.r8.D8 --min-api 26 \
        --output "$RUNTIME/samples/$program.jar" $(find "$WORK/$program" -name '*.class')
done
du -sh "$RUNTIME"
ls "$RUNTIME/notices" | wc -l
