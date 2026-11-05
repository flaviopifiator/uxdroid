#!/bin/bash
# Installs an app delivered as a base APK and a feature split, and starts it: the split's code joins the
# app's class loader, compiled at install, and its resources join the app's. Runs in the dev service; the
# APKs are ours, built by build_system.sh.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; tail -"${TAIL:-25}" "$WORK/out" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-cli --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
LIBRARY="$TARGET/libuxdroid_system.so"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
SAMPLES="$RUNTIME/samples/framework"
touch "$WORK/out"
expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }

UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$SAMPLES/split-base.apk" "$SAMPLES/split-feature.apk" \
    --data "$ANDROID_DATA" >"$WORK/out" 2>&1 || fail "install"
INSTALLED="$ANDROID_DATA/app/com.example.split"
expect "^installed splits of com\.example\.split: feature \(feature\)$" "the install did not take the feature split"
expect "^prepared com\.example\.split in $INSTALLED: " "the install did not prepare the app"
[ -f "$INSTALLED/split_feature.apk" ] && [ -f "$INSTALLED/oat/x86_64/split_feature.odex" ] \
    || fail "the split or its compiled code is missing"

launch() {
    timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -verbose:oat -cp "$CP" dev.uxdroid.system.Launcher \
        --once "$RUNTIME" "$LIBRARY" "$INSTALLED/base.apk" >"$WORK/out" 2>&1 || fail "$1: exit $?"
    expect "^splits \[feature\]$" "$1: the app's ApplicationInfo does not name its split"
    # The split's class is found through the app's loader, and reads the split's own string.
    expect "^split code com\.example\.split\.feature\.Feature says \"Hola desde un split\"$" \
        "$1: the split's code or resources are not the app's"
}

# 1. From the install record.
launch "recorded"
expect "GetBestOatFile\(\)=$INSTALLED/oat/x86_64/split_feature\.odex \(executable=1\)" \
    "ART did not use the split's code compiled at install"
if grep -E "uxdroid: (the install record|default bodies of) " "$WORK/out"; then fail "what the install prepared was not used"; fi

# 2. Without a record, the launcher parses the code directory as one package, as Android does.
rm "$INSTALLED/package.record"
launch "parsed"
echo "ok: a feature split's code and resources joined the app's, from the install record and from a parse"
