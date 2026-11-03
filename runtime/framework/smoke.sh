#!/bin/bash
# Starts AOSP's framework on uxdroid's ART with uxdroid's natives below it. Runs in the dev service.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; tail -15 "$WORK/out" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-system --manifest-path /work/Cargo.toml || fail "build"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar:$RUNTIME/samples/framework/smoke.jar"
"$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" FrameworkSmoke "$RUNTIME" /cache/target/release/libuxdroid_system.so \
    >"$WORK/out" 2>&1 || fail "exit $?"

expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
expect "^defaults [0-9]{4}" "default bodies were not bound"
expect "^rect ff00c000 00000000$" "the rectangle is not where it was drawn"
expect "^text [0-9]{3}" "no text was drawn"
expect "^string OK$" "framework-res.apk was not read"
expect "^sdk 36 abi x86_64$" "build.prop did not reach android.os.Build"
expect "^I FrameworkSmoke: log reaches stderr$" "android.util.Log is not ours"
grep -q "unimplemented native" "$WORK/out" && grep "unimplemented native" "$WORK/out" | head -5
echo "ok: AOSP's framework drew, wrote text and read its resources on uxdroid's ART"
