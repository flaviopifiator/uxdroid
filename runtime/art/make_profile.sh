#!/bin/bash
# Records which boot classes and methods real work uses, into runtime/art/boot-profile.txt.
# Run after package.sh and compile.sh, then copy the printed file into the repository.
set -eu -o pipefail
source /work/runtime/art/run.sh
HOST=/aosp/out/host/linux-x86
WORK=$(mktemp -d)
export ANDROID_DATA="$WORK/data" LD_LIBRARY_PATH="$RUNTIME/lib64"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
touch "$WORK/boot.prof"
# Workload reaches what apps use at startup: a profile from a small program alone compiled so little
# that real library code ran three times slower. Holdout stays out, to test what the profile missed.
profile_run() {
    "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -Xusejit:true -Xjitthreshold:10 -Xjitsaveprofilinginfo \
        -Xps-profile-boot-class-path -Xps-profile-path:"$WORK/boot.prof" -Xps-min-first-save-ms:100 \
        -Xps-save-resolved-classes-delay-ms:100 -Xps-min-methods-to-save:0 -Xps-min-classes-to-save:0 \
        -cp "$RUNTIME/samples/$1.jar" "$@" >"$WORK/log" 2>&1 \
        || { head -3 "$WORK/log" | cut -c1-200; echo "FAIL: the profiling run of $1"; exit 1; }
}
profile_run ProfileWork 6000
profile_run Workload 40
[ -s "$WORK/boot.prof" ] || { echo "FAIL: the profile is empty"; exit 1; }
APKS=()
for jar in "${JARS[@]}"; do
    APKS+=("--apk=$RUNTIME/javalib/$jar.jar" "--dex-location=$RUNTIME/javalib/$jar.jar")
done
LD_LIBRARY_PATH="$HOST/lib64" "$HOST/bin/profman" --profile-file="$WORK/boot.prof" "${APKS[@]}" \
    --dump-classes-and-methods 2>/dev/null | sort -u > "$WORK/boot-profile.txt"
wc -l "$WORK/boot-profile.txt"
cp "$WORK/boot-profile.txt" /runtime/art/boot-profile.txt
