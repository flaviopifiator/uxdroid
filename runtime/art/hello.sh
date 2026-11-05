#!/bin/bash
# Compiles Hello.java to DEX and runs it with the freshly built ART, using no Android system.
set -eu
cd /aosp
HOST=/aosp/out/host/linux-x86
WORK=$(mktemp -d)
cat > "$WORK/Hello.java" <<'JAVA'
public class Hello {
    public static void main(String[] args) {
        System.out.println("hello from dex, " + System.getProperty("os.arch") + ", " + System.getProperty("java.vm.name"));
    }
}
JAVA
JDK=$(ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
"$JDK/bin/javac" --release 8 -d "$WORK" "$WORK/Hello.java" 2>&1 | grep -v "warning:" || true
"$JDK/bin/java" -cp prebuilts/r8/r8.jar com.android.tools.r8.D8 --min-api 26 --output "$WORK" "$WORK/Hello.class"

export ANDROID_ROOT="$HOST"
export ANDROID_ART_ROOT="$HOST/com.android.art"
export ANDROID_I18N_ROOT="$HOST/com.android.i18n"
export ANDROID_TZDATA_ROOT="$HOST/com.android.tzdata"
export ANDROID_DATA="$WORK/data"
export LD_LIBRARY_PATH="$HOST/lib64"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
FRAMEWORK="$HOST/apex/com.android.art/javalib"
BOOT="$FRAMEWORK/core-oj.jar:$FRAMEWORK/core-libart.jar:$FRAMEWORK/okhttp.jar:$FRAMEWORK/bouncycastle.jar:$FRAMEWORK/apache-xml.jar:$HOST/apex/com.android.i18n/javalib/core-icu4j.jar:$HOST/apex/com.android.conscrypt/javalib/conscrypt.jar"
ls "$HOST/bin" | grep -E "^(dalvikvm|dex2oat)" | tr '\n' ' '; echo
find "$HOST" -name "*.art" | head -5
"$HOST/bin/dalvikvm64" -Xbootclasspath:"$BOOT" -Xbootclasspath-locations:"$BOOT" -cp "$WORK/classes.dex" Hello
