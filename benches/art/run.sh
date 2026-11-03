#!/bin/bash
# Compares time to first output of a DEX main() between uxdroid's ART and art_standalone.
set -u
RUNS="${RUNS:-30}"
MEASURE=/opt/first_output
source /work/runtime/art/run.sh
SAMPLES="$RUNTIME/samples"
UXDROID=("$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}")
UXDROID_ENV=(env "LD_LIBRARY_PATH=$RUNTIME/lib64")

P=/competitor/install
# uxdroid's variables must not reach the competitor: ANDROID_ROOT disables its boot image.
COMPETITOR_ENV=(env -u ANDROID_ROOT -u ANDROID_ART_ROOT -u ANDROID_I18N_ROOT -u ANDROID_TZDATA_ROOT
    "PATH=$P/bin:$PATH" "LD_LIBRARY_PATH=$P/lib:$P/lib64" "ANDROID_DATA=/tmp/competitor-data" "XDG_DATA_DIRS=$P/share")
mkdir -p /tmp/competitor-data

# The options that make uxdroid faster or smaller apply to the competitor too.
SHARED_FLAGS=(-Xnorelocate -XX:HeapMinFree=256k -XX:HeapMaxFree=1m -XX:LargeObjectSpace=disabled
    -XX:ForegroundHeapGrowthMultiplier=1.0)

# The competitor compiles its boot image and each app on first use, which is not counted here.
"${COMPETITOR_ENV[@]}" "$P/bin/dalvikvm" -cp "$SAMPLES/Hello.jar" Hello >/dev/null 2>&1
"${COMPETITOR_ENV[@]}" "$P/bin/dalvikvm" -cp "$SAMPLES/Compute.jar" Compute >/dev/null 2>&1

# Peak memory depends on how the page cache holds a file: a freshly written copy read up to 6 MiB
# higher than the same bytes read back. Both runtimes start from the same state.
sync
find "$RUNTIME" "$P" "$HOME/.cache/art" -type f -exec dd if={} iflag=nocache count=0 status=none \; 2>/dev/null

for program in Hello Compute; do
    echo "$program"
    printf '  %-20s ' uxdroid-art
    "${UXDROID_ENV[@]}" "$MEASURE" "$RUNS" "${UXDROID[@]}" -cp "$SAMPLES/$program.jar" "$program"
    printf '  %-20s ' uxdroid-art-nojit
    "${UXDROID_ENV[@]}" "$MEASURE" "$RUNS" "${UXDROID[@]}" -Xusejit:false -cp "$SAMPLES/$program.jar" "$program"
    printf '  %-20s ' art_standalone
    "${COMPETITOR_ENV[@]}" "$MEASURE" "$RUNS" "$P/bin/dalvikvm" "${SHARED_FLAGS[@]}" -cp "$SAMPLES/$program.jar" "$program"
    printf '  %-20s ' art_standalone-nojit
    "${COMPETITOR_ENV[@]}" "$MEASURE" "$RUNS" "$P/bin/dalvikvm" "${SHARED_FLAGS[@]}" -Xusejit:false -cp "$SAMPLES/$program.jar" "$program"
    # uxdroid's launcher avoids touching the first thread's stack. This is how close a smaller stack
    # limit brings the competitor, at the price of a 256 KiB main thread.
    printf '  %-20s ' art_standalone-256k
    (ulimit -s 256; "${COMPETITOR_ENV[@]}" "$MEASURE" "$RUNS" "$P/bin/dalvikvm" "${SHARED_FLAGS[@]}" -Xusejit:false -cp "$SAMPLES/$program.jar" "$program")
done
echo "installed: uxdroid $(du -sh "$RUNTIME" | cut -f1), art_standalone $(du -sh "$P" | cut -f1) plus generated boot image $(du -sh "$HOME/.cache/art" 2>/dev/null | cut -f1)"
