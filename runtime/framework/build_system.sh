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

mkdir -p "$RUNTIME/samples/framework"
compile smoke "$RUNTIME/samples/framework/smoke.jar" "PCL[$FRAMEWORK:$RUNTIME/framework/uxdroid-system.jar]" \
    "$ALL:$W/system" /work/runtime/framework/samples/FrameworkSmoke.java
ls -la "$RUNTIME/framework/uxdroid-system.jar" "$RUNTIME/samples/framework/smoke.jar"
