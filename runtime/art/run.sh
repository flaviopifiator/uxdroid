#!/bin/bash
# Shared environment for running the packaged ART: nothing from an Android system or AOSP tree.
RUNTIME="${RUNTIME:-/runtime/art}"
export ANDROID_ROOT="$RUNTIME"
export ANDROID_ART_ROOT="$RUNTIME"
export ANDROID_I18N_ROOT="$RUNTIME/com.android.i18n"
export ANDROID_TZDATA_ROOT="$RUNTIME/com.android.tzdata"
export ANDROID_DATA="${ANDROID_DATA:-/tmp/art-data}"
export LD_LIBRARY_PATH="$RUNTIME/lib64"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
JARS=(core-oj core-libart okhttp bouncycastle apache-xml core-icu4j conscrypt)
BOOT=""
for jar in "${JARS[@]}"; do
    BOOT="$BOOT:$RUNTIME/javalib/$jar.jar"
done
BOOT="${BOOT:1}"
