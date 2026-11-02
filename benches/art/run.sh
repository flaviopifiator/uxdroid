#!/bin/bash
# Compares time to first output of a DEX main() between uxdroid's ART and art_standalone.
set -u
RUNS="${RUNS:-30}"
MEASURE=/opt/first_output
source /work/runtime/art/run.sh
SAMPLES="$RUNTIME/samples"
UXDROID=("$RUNTIME/bin/dalvikvm64" -Xbootclasspath:"$BOOT" -Xbootclasspath-locations:"$BOOT" -Ximage:"$RUNTIME/framework/boot.art")
UXDROID_ENV=(env "LD_LIBRARY_PATH=$RUNTIME/lib64")

P=/competitor/install
COMPETITOR_ENV=(env "PATH=$P/bin:$PATH" "LD_LIBRARY_PATH=$P/lib:$P/lib64" "ANDROID_DATA=/tmp/competitor-data" "XDG_DATA_DIRS=$P/share")
mkdir -p /tmp/competitor-data

# The competitor compiles its boot image and each app on first use, which is not counted here.
"${COMPETITOR_ENV[@]}" "$P/bin/dalvikvm" -cp "$SAMPLES/Hello.jar" Hello >/dev/null 2>&1
"${COMPETITOR_ENV[@]}" "$P/bin/dalvikvm" -cp "$SAMPLES/Compute.jar" Compute >/dev/null 2>&1

for program in Hello Compute; do
    echo "$program"
    printf '  %-16s ' uxdroid-art
    "${UXDROID_ENV[@]}" "$MEASURE" "$RUNS" "${UXDROID[@]}" -cp "$SAMPLES/$program.jar" "$program"
    printf '  %-16s ' art_standalone
    "${COMPETITOR_ENV[@]}" "$MEASURE" "$RUNS" "$P/bin/dalvikvm" -cp "$SAMPLES/$program.jar" "$program"
done
du -sh "$RUNTIME" "$P" | tr '\n' ' '; echo
