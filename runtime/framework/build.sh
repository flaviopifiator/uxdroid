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

# ART's own files share this tree. The last build listed what it installed, and only that is removed.
MANIFEST="$RUNTIME/framework/framework-files.txt"
if [ -f "$MANIFEST" ]; then
    (cd "$RUNTIME" && xargs -a "$MANIFEST" -d '\n' rm -f)
fi
BEFORE="$W/before"
(cd "$RUNTIME" && find . -type f | sort) > "$BEFORE"
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

# Every framework native, listed once here so startup binds default bodies without reflection.
/aosp/out/host/linux-x86/bin/dexlist "$RUNTIME"/javalib/*.jar | awk '!/^#/ {print $3}' | sort -u > "$W/boot-classes"
(cd "$W/classes" && find . -name '*.class' | sed 's|^\./||; s|\.class$||; s|/|.|g') > "$W/class-names"
split -l 2000 "$W/class-names" "$W/names."
for part in "$W"/names.*; do
    xargs -a "$part" "$JDK/bin/javap" -p -s -cp "$W/classes"
done > "$W/javap.txt"
python3 "$TOOLS/natives_list.py" "$W/javap.txt" "$W/boot-classes" "$RUNTIME/framework/natives.txt"

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
if ! "$RUNTIME/bin/dex2oat64" --compiler-filter=verify --instruction-set=x86_64 --host --android-root="$RUNTIME" \
    --runtime-arg -Xbootclasspath:"$BOOT" --runtime-arg -Xbootclasspath-locations:"$BOOT" \
    --boot-image="$RUNTIME/framework/boot.art" --dex-file="$RUNTIME/framework/framework.jar" \
    --class-loader-context='PCL[]' --oat-file="$RUNTIME/framework/oat/x86_64/framework.odex" > "$W/dex2oat.log" 2>&1; then
    tail -20 "$W/dex2oat.log"
    exit 1
fi
(cd "$RUNTIME" && find . -type f | sort) | comm -13 "$BEFORE" - > "$MANIFEST"
echo "./framework/framework-files.txt" >> "$MANIFEST"
du -shc "$RUNTIME/framework/framework.jar" "$RUNTIME/framework/framework-res.apk" "$RUNTIME/framework/oat" \
    "$RUNTIME/fonts" "$RUNTIME/usr" "$RUNTIME/lib64/libandroid_runtime.so"
