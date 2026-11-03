#!/bin/bash
# Installs AOSP's framework into the runtime, which is laid out as Android's system partition, and compiles it.
# Runs after runtime/art/package.sh and runtime/framework/fetch.sh.
set -eu -o pipefail
source /work/runtime/art/run.sh
DL=/aosp/framework-dl
ALL="$DL/android-all-16-robolectric-13921718.jar"
FONTS_JAR="$DL/nativeruntime-dist-compat-1.0.19.jar"
TOOLS=/work/runtime/framework
cd /aosp
JDK=/aosp/$(ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT

# ART's own files share this tree, so only the framework's are replaced.
rm -rf "$RUNTIME/framework/framework.jar" "$RUNTIME/framework/framework-res.apk" "$RUNTIME/framework/oat" \
    "$RUNTIME/etc" "$RUNTIME/fonts" "$RUNTIME/usr" "$RUNTIME/build.prop" "$RUNTIME/lib64/libandroid_runtime.so" \
    "$RUNTIME/notices/framework"
mkdir -p "$RUNTIME/framework/oat/x86_64" "$RUNTIME/etc" "$RUNTIME/fonts" "$RUNTIME/usr/icu" "$RUNTIME/notices/framework"

# Framework classes. What ART's boot class path already has stays out, so the boot copy is the one used.
mkdir "$W/classes"
unzip -q "$ALL" 'android/*' 'com/*' -x 'android/icu/*' -d "$W/classes"
python3 "$TOOLS/rewrite_classes.py" "$W/classes" "$RUNTIME"
find "$W/classes" -name '*.class' > "$W/list"
"$JDK/bin/java" -Xmx12g -cp prebuilts/r8/r8.jar com.android.tools.r8.D8 --min-api 30 --release \
    --output "$W/framework-dex.jar" @"$W/list" 2>&1 | { grep -v -i warning || true; }
python3 "$TOOLS/assets.py" aligned-jar "$W/framework-dex.jar" "$RUNTIME/framework/framework.jar"
python3 "$TOOLS/assets.py" framework-res "$ALL" "$RUNTIME/framework/framework-res.apk"

# The native half of the framework, built by AOSP for a glibc host, and the data it reads.
unzip -q -j "$ALL" native/linux/x86_64/libandroid_runtime.so -d "$RUNTIME/lib64"
unzip -q -j "$ALL" 'icu/icudt*.dat' -d "$RUNTIME/usr/icu"
unzip -p "$ALL" build.prop > "$W/build.prop"
python3 "$TOOLS/assets.py" build-prop "$W/build.prop" "$RUNTIME/build.prop"

# Fonts. CJK alone is 120 MiB; DroidSansFallback covers those scripts in 4 MiB.
unzip -q -j "$FONTS_JAR" 'fonts/*' -x 'fonts/NotoSansCJK*' 'fonts/fonts.xml' -d "$RUNTIME/fonts"
unzip -p "$FONTS_JAR" fonts/fonts.xml > "$W/fonts.xml"
python3 "$TOOLS/assets.py" fonts "$RUNTIME/fonts" "$W/fonts.xml" "$RUNTIME/etc/fonts.xml"
# Android 16 reads the same families again as its fallback list.
cp "$RUNTIME/etc/fonts.xml" "$RUNTIME/etc/font_fallback.xml"

cp "$DL"/notices/* "$RUNTIME/notices/framework/"

# Compile at install time. Verified ahead of time, the framework starts at 38 MiB instead of 119 MiB.
"$RUNTIME/bin/dex2oat64" --compiler-filter=verify --instruction-set=x86_64 --host --android-root="$RUNTIME" \
    --runtime-arg -Xbootclasspath:"$BOOT" --runtime-arg -Xbootclasspath-locations:"$BOOT" \
    --boot-image="$RUNTIME/framework/boot.art" --dex-file="$RUNTIME/framework/framework.jar" \
    --class-loader-context='PCL[]' --oat-file="$RUNTIME/framework/oat/x86_64/framework.odex" 2>&1 \
    | grep -E "^.{0,40}(E|F) dex2oat" || true
test -s "$RUNTIME/framework/oat/x86_64/framework.odex"
du -shc "$RUNTIME/framework/framework.jar" "$RUNTIME/framework/framework-res.apk" "$RUNTIME/framework/oat" \
    "$RUNTIME/fonts" "$RUNTIME/usr" "$RUNTIME/lib64/libandroid_runtime.so"
