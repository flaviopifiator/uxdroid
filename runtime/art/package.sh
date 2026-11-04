#!/bin/bash
# Copies only what running ART needs into /runtime, with its notices, and builds the sample apps.
# ARCH picks the package: x86_64 (/runtime/art) or arm64 (/runtime/art-arm64-host, from
# build_arm64.sh and runtime/framework/android16/build.sh, made in the android16-builder service). The jars and the ICU and time zone data are the same files for every CPU, from
# the x86_64 build of the same tree, which this only reads.
set -eu -o pipefail
ARCH="${ARCH:-x86_64}"
COMMON=/aosp/out/host/linux-x86
case "$ARCH" in
    x86_64) HOST=$COMMON RUNTIME="${RUNTIME:-/runtime/art}" ;;
    arm64) HOST=/aosp/out-arm64/host/linux-arm64 RUNTIME="${RUNTIME:-/runtime/art-arm64-host}" ;;
    *) echo "unknown ARCH $ARCH" >&2; exit 2 ;;
esac
rm -rf "$RUNTIME"
mkdir -p "$RUNTIME/bin" "$RUNTIME/samples" "$RUNTIME/javalib" "$RUNTIME/notices"
cp "$HOST/bin/dalvikvm64" "$HOST/bin/dex2oat64" "$RUNTIME/bin/"
cp -r "$HOST/lib64" "$RUNTIME/lib64"
# ICU, Conscrypt and SQLite for arm64 come from the Android 16 tree (runtime/framework/android16), so
# the arm64 package is made in the android16-builder service, which has both trees.
if [ "$ARCH" = arm64 ]; then
    for library in icu icuuc-host icui18n-host icu_jni javacrypto sqlite; do
        cp "/a16/out-arm64/host/linux-arm64/lib64/lib$library.so" "$RUNTIME/lib64/"
    done
fi
# Debug builds of the runtime are not needed to run apps.
rm -f "$RUNTIME"/lib64/lib*d.so.debug
find "$RUNTIME/lib64" -regextype posix-extended \
    -regex '.*/lib(artd|artd-.*|dexfiled|dexfiled_.*|artbased|profiled|openjdkd|openjdkjvmd|openjdkjvmtid|adbconnectiond|tid|dt_fd_forward_debug)\.so' -delete
cp -r "$COMMON/com.android.i18n" "$COMMON/com.android.tzdata" "$RUNTIME/"
# The arm64 package's ICU, 76 as its tree has it, opens its data beside the module's ICU 68 data.
if [ "$ARCH" = arm64 ]; then
    rm -f "$RUNTIME/com.android.i18n/etc/icu/icudt76l.dat"
    cp /a16/external/icu/icu4c/source/stubdata/icudt76l.dat "$RUNTIME/com.android.i18n/etc/icu/"
fi
for jar in \
    apex/com.android.art/javalib/core-oj.jar apex/com.android.art/javalib/core-libart.jar \
    apex/com.android.art/javalib/okhttp.jar apex/com.android.art/javalib/bouncycastle.jar \
    apex/com.android.art/javalib/apache-xml.jar apex/com.android.i18n/javalib/core-icu4j.jar \
    apex/com.android.conscrypt/javalib/conscrypt.jar; do
    cp "$COMMON/$jar" "$RUNTIME/javalib/"
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
# The arm64 package's libc++, libc++abi, libunwind and compiler-rt runtimes, built by libcxx_arm64.sh.
if [ "$ARCH" = arm64 ]; then
    cp "$(ls -d /aosp/llvm-project-*/compiler-rt | head -1)/LICENSE.TXT" "$RUNTIME/notices/compiler-rt"
fi

JDK=$(ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
WORK=$(mktemp -d)
for program in Hello Compute NativeCall ProfileWork Launcher Workload Holdout; do
    "$JDK/bin/javac" --release 8 -d "$WORK/$program" "/work/runtime/art/samples/$program.java" 2>&1 | { grep -v "warning:" || true; }
    "$JDK/bin/java" -cp prebuilts/r8/r8.jar com.android.tools.r8.D8 --min-api 26 \
        --output "$RUNTIME/samples/$program.jar" $(find "$WORK/$program" -name '*.class')
done
du -sh "$RUNTIME"
ls "$RUNTIME/notices" | wc -l
