#!/bin/bash
# Regenerates crates/system/src/gl/gles.rs, the JNI natives of android.opengl.GLES20 to GLES32 and the
# list of GLImpl's, from the framework's own declarations and the GLES signature table. Runs in
# art-builder:
#
#   docker compose --profile art run --rm -T art-builder bash /work/runtime/framework/gles_jni.sh \
#       > crates/system/src/gl/gles.rs
set -eu -o pipefail
ALL=/aosp/framework-dl/android-all-16-robolectric-13921718.jar
JDK=/aosp/$(cd /aosp && ls -d prebuilts/jdk/jdk*/linux-x86 | sort -V | tail -1)
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
"$JDK/bin/javap" -s -p -cp "$ALL" android.opengl.GLES20 android.opengl.GLES30 android.opengl.GLES31 android.opengl.GLES32 \
    com.google.android.gles_jni.GLImpl > "$W/javap.txt"
python3 -I /work/crates/system/gles_jni.py "$W/javap.txt" /work/crates/ndk/src/gles_table.rs
