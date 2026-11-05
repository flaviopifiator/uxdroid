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
    oat="$(dirname "$dex_jar")/oat/$ARCH/$(basename "$dex_jar" .jar).odex"
    mkdir -p "$(dirname "$oat")"
    "${DEX2OAT[@]}" --compiler-filter=verify --instruction-set="$ARCH" --host --android-root="$RUNTIME" \
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
CROSS=()
[ "$ARCH" = arm64 ] && CROSS=(--target=aarch64-linux-gnu -fuse-ld=lld)
for library in stats_jni framework-connectivity-jni framework-connectivity-tiramisu-jni media_jni; do
    "$CLANG" "${CROSS[@]}" -shared -nostdlib -o "$RUNTIME/lib64/lib$library.so" -x c /dev/null
done

mkdir -p "$RUNTIME/samples/framework"
compile smoke "$RUNTIME/samples/framework/smoke.jar" "PCL[$FRAMEWORK:$RUNTIME/framework/uxdroid-system.jar]" \
    "$ALL:$W/system" /work/runtime/framework/samples/FrameworkSmoke.java
compile parcel "$RUNTIME/samples/framework/parcel.jar" "PCL[$FRAMEWORK:$RUNTIME/framework/uxdroid-system.jar]" \
    "$ALL:$W/system" /work/runtime/framework/samples/ParcelSmoke.java

# Sample apps built as the SDK would: resources, assets and manifest by aapt2 against framework-res.apk,
# code against the framework jar. They know nothing about uxdroid. The native libraries of the NDK and audio
# samples are built by their smokes in the dev service, which has the NDK.
AAPT2=/aosp/prebuilts/sdk/tools/linux/bin/aapt2
# uxdroid install takes only signed APKs, as Android does, so each sample is signed with the public test
# key in runtime/keys by AOSP's apksigner (v2 and v3, and v1 for old minimum SDKs).
sign_apk() {
    "$JDK/bin/java" -jar /aosp/prebuilts/sdk/tools/linux/lib/apksigner.jar sign \
        --key /work/runtime/keys/test.pk8 --cert /work/runtime/keys/test.x509.pem --out "$2" "$1"
    rm -f "$2.idsig"
}
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
    mv "$out" "$work/unsigned.apk"
    sign_apk "$work/unsigned.apk" "$out"
}
build_app /work/runtime/framework/samples/app "$RUNTIME/samples/framework/app.apk"
build_app /work/runtime/framework/samples/ndk "$RUNTIME/samples/framework/ndk.apk"
build_app /work/runtime/framework/samples/audio "$RUNTIME/samples/framework/audio.apk"
build_app /work/runtime/framework/samples/input "$RUNTIME/samples/framework/input.apk"
build_app /work/runtime/framework/samples/compose "$RUNTIME/samples/framework/compose.apk"
build_app /work/runtime/framework/samples/compat "$RUNTIME/samples/framework/compat.apk"
build_app /work/runtime/framework/samples/peer "$RUNTIME/samples/framework/peer.apk"
build_app /work/runtime/framework/samples/uidtwin "$RUNTIME/samples/framework/uidtwin.apk"

# An app delivered as a base APK and a feature split, linked as bundletool links them: the split's
# resources in a package of their own id, against the base's, and its code in its own classes.dex.
build_split() {
    local app=/work/runtime/framework/samples/split work="$W/split"
    for part in base feature; do
        mkdir -p "$work/$part/gen" "$work/$part/classes" "$work/$part/dex"
        "$AAPT2" compile --dir "$app/$part/res" -o "$work/$part/res.zip"
    done
    "$AAPT2" link -I "$RUNTIME/framework/framework-res.apk" --manifest "$app/base/AndroidManifest.xml" \
        --java "$work/base/gen" -o "$work/base/linked.apk" "$work/base/res.zip"
    "$AAPT2" link -I "$RUNTIME/framework/framework-res.apk" -I "$work/base/linked.apk" \
        --manifest "$app/feature/AndroidManifest.xml" --package-id 0x80 \
        --custom-package com.example.split.feature --java "$work/feature/gen" \
        -o "$work/feature/linked.apk" "$work/feature/res.zip"
    for part in base feature; do
        "$JDK/bin/javac" --release 11 -cp "$ALL" -d "$work/$part/classes" \
            $(find "$app/$part" "$work/$part/gen" -name '*.java')
        "${D8[@]}" --classpath "$FRAMEWORK" --output "$work/$part/dex" \
            $(find "$work/$part/classes" -name '*.class') 2>&1 | { grep -v -i warning || true; }
        PYTHONDONTWRITEBYTECODE=1 python3 - "$work/$part/linked.apk" "$work/$part/dex/classes.dex" \
            "$work/$part/unsigned.apk" <<'PY'
import sys, zipfile
sys.path.insert(0, "/work/runtime/framework")
import assets
linked, dex, out = sys.argv[1:]
with zipfile.ZipFile(linked) as src:
    entries = [(n, src.read(n)) for n in src.namelist()]
assets.write_stored(out, entries + [("classes.dex", open(dex, "rb").read())])
PY
        sign_apk "$work/$part/unsigned.apk" "$RUNTIME/samples/framework/split-$part.apk"
    done
}
build_split
ls -la "$RUNTIME/framework/uxdroid-system.jar" "$RUNTIME/samples/framework/smoke.jar" "$RUNTIME/samples/framework/app.apk" \
    "$RUNTIME/samples/framework/ndk.apk" "$RUNTIME/samples/framework/audio.apk" "$RUNTIME/samples/framework/input.apk"
