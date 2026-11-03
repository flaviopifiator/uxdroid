#!/bin/bash
# Compiles uxdroid's Java below the framework, and the framework smoke program, into the runtime.
# Runs after runtime/framework/build.sh.
set -eu -o pipefail
source /work/runtime/art/run.sh
ALL=/aosp/framework-dl/android-all-16-robolectric-13921718.jar
FRAMEWORK="$RUNTIME/framework/framework.jar"
cd /aosp
JDK=/aosp/$(ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
D8=("$JDK/bin/java" -cp prebuilts/r8/r8.jar com.android.tools.r8.D8 --min-api 30 --release)
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT

compile() {
    local out=$1 dex_jar=$2 context=$3 classpath=$4
    shift 4
    mkdir -p "$W/$out"
    "$JDK/bin/javac" --release 11 -cp "$classpath" -d "$W/$out" "$@" 2>&1 | { grep -v -E "^Note:" || true; }
    "${D8[@]}" --classpath "$FRAMEWORK" --output "$W/$out.jar" $(find "$W/$out" -name '*.class') 2>&1 \
        | { grep -v -i warning || true; }
    python3 /work/runtime/framework/assets.py aligned-jar "$W/$out.jar" "$dex_jar"
    local oat
    oat="$(dirname "$dex_jar")/oat/x86_64/$(basename "$dex_jar" .jar).odex"
    mkdir -p "$(dirname "$oat")"
    "$RUNTIME/bin/dex2oat64" --compiler-filter=verify --instruction-set=x86_64 --host --android-root="$RUNTIME" \
        --runtime-arg -Xbootclasspath:"$BOOT" --runtime-arg -Xbootclasspath-locations:"$BOOT" \
        --boot-image="$RUNTIME/framework/boot.art" --dex-file="$dex_jar" --class-loader-context="$context" \
        --oat-file="$oat" > "$W/dex2oat.log" 2>&1 || { tail -20 "$W/dex2oat.log"; exit 1; }
}

mkdir -p "$W/generated"
"$JDK/bin/javap" -p -cp "$ALL" android.content.Context > "$W/context.txt"
python3 /work/runtime/framework/default_context.py "$W/context.txt" "$W/generated/DefaultContext.java"
{ find /work/system/java -name '*.java'; echo "$W/generated/DefaultContext.java"; } > "$W/system-sources"
compile system "$RUNTIME/framework/uxdroid-system.jar" "PCL[$FRAMEWORK]" "$ALL" @"$W/system-sources"

# Libraries the framework loads by name only to register natives, which already have default bodies.
# Each is an empty library, so System.loadLibrary finds it.
CLANG=/aosp/$(ls -d prebuilts/clang/host/linux-x86/clang-r* | sort -V | tail -1)/bin/clang
for library in stats_jni framework-connectivity-tiramisu-jni media_jni; do
    "$CLANG" -shared -nostdlib -o "$RUNTIME/lib64/lib$library.so" -x c /dev/null
done

mkdir -p "$RUNTIME/samples/framework"
compile smoke "$RUNTIME/samples/framework/smoke.jar" "PCL[$FRAMEWORK:$RUNTIME/framework/uxdroid-system.jar]" \
    "$ALL:$W/system" /work/runtime/framework/samples/FrameworkSmoke.java
compile parcel "$RUNTIME/samples/framework/parcel.jar" "PCL[$FRAMEWORK:$RUNTIME/framework/uxdroid-system.jar]" \
    "$ALL:$W/system" /work/runtime/framework/samples/ParcelSmoke.java

# Sample apps built as the SDK would: resources, assets and manifest by aapt2 against framework-res.apk,
# code against the framework jar. They know nothing about uxdroid. The NDK sample's native library is
# built by ndk_smoke.sh in the dev service, which has the NDK.
AAPT2=/aosp/prebuilts/sdk/tools/linux/bin/aapt2
build_app() {
    local app=$1 out=$2 work="$W/$(basename "$1")"
    local assets=()
    [ -d "$app/assets" ] && assets=(-A "$app/assets")
    mkdir -p "$work/gen" "$work/classes" "$work/dex"
    "$AAPT2" compile --dir "$app/res" -o "$work/res.zip"
    "$AAPT2" link -I "$RUNTIME/framework/framework-res.apk" --manifest "$app/AndroidManifest.xml" \
        "${assets[@]}" --java "$work/gen" -o "$work/base.apk" "$work/res.zip"
    "$JDK/bin/javac" --release 11 -cp "$ALL" -d "$work/classes" $(find "$app" "$work/gen" -name '*.java')
    "${D8[@]}" --classpath "$FRAMEWORK" --output "$work/dex" $(find "$work/classes" -name '*.class') 2>&1 \
        | { grep -v -i warning || true; }
    # Entries stay uncompressed and aligned, as Android requires of resources.arsc and classes.dex from
    # targetSdk 30; stored assets can then be opened as file descriptors, as on Android.
    PYTHONDONTWRITEBYTECODE=1 python3 - "$work/base.apk" "$work/dex/classes.dex" "$out" <<'PY'
import sys, zipfile
sys.path.insert(0, "/work/runtime/framework")
import assets
base, dex, out = sys.argv[1:]
with zipfile.ZipFile(base) as src:
    entries = [(n, src.read(n)) for n in src.namelist()]
assets.write_stored(out, entries + [("classes.dex", open(dex, "rb").read())])
PY
}
build_app /work/runtime/framework/samples/app "$RUNTIME/samples/framework/app.apk"
build_app /work/runtime/framework/samples/ndk "$RUNTIME/samples/framework/ndk.apk"
ls -la "$RUNTIME/framework/uxdroid-system.jar" "$RUNTIME/samples/framework/smoke.jar" "$RUNTIME/samples/framework/app.apk" \
    "$RUNTIME/samples/framework/ndk.apk"
