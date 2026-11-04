#!/bin/bash
# Compiles uxdroid's android.* classes and the test app's DEX into the runtime volume.
set -eu -o pipefail
source /work/runtime/art/run.sh
cd /aosp
JDK=$(ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
D8=("$JDK/bin/java" -cp prebuilts/r8/r8.jar com.android.tools.r8.D8 --min-api 26)
WORK=$(mktemp -d)

mkdir -p "$WORK/platform" "$RUNTIME/platform/oat/$ARCH" "$RUNTIME/samples/first" "$WORK/first"
find /work/platform/java -name '*.java' > "$WORK/sources"
"$JDK/bin/javac" --release 8 -d "$WORK/platform" @"$WORK/sources" 2>&1 | { grep -v "warning" || true; }
"${D8[@]}" --output "$RUNTIME/platform/platform.jar" $(find "$WORK/platform" -name '*.class')

"$JDK/bin/javac" --release 8 -cp "$WORK/platform" -d "$WORK/first" \
    /work/runtime/art/samples/first/com/example/first/MainActivity.java 2>&1 | { grep -v "warning" || true; }
"${D8[@]}" --classpath "$RUNTIME/platform/platform.jar" --output "$RUNTIME/samples/first" \
    $(find "$WORK/first" -name '*.class')

"${DEX2OAT[@]}" --compiler-filter=speed --instruction-set="$ARCH" --host --android-root="$RUNTIME" \
    --runtime-arg -Xbootclasspath:"$BOOT" --runtime-arg -Xbootclasspath-locations:"$BOOT" \
    --boot-image="$RUNTIME/framework/boot.art" --dex-file="$RUNTIME/platform/platform.jar" \
    --oat-file="$RUNTIME/platform/oat/$ARCH/platform.odex" 2>&1 | tail -1 | cut -c1-160
ls -la "$RUNTIME/platform" "$RUNTIME/samples/first" | grep -E "jar|dex"
