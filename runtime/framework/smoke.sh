#!/bin/bash
# Starts AOSP's framework on uxdroid's ART with uxdroid's natives below it. Runs in the dev service,
# or with ARCH=arm64 in dev-arm64, on the arm64 package under qemu-aarch64.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; tail -15 "$WORK/out" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-system --manifest-path /work/Cargo.toml || fail "build"
source /work/runtime/art/release_dir.sh
LIBRARY="$RELEASE/libuxdroid_system.so"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/$ARCH"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar:$RUNTIME/samples/framework/smoke.jar"
"${RUNNER[@]}" "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" FrameworkSmoke "$RUNTIME" "$LIBRARY" \
    >"$WORK/out" 2>&1 || fail "exit $?"

expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
expect "^defaults [0-9]{4}" "default bodies were not bound"
expect "^rect ff00c000 00000000$" "the rectangle is not where it was drawn"
expect "^text [0-9]{3}" "no text was drawn"
expect "^string OK$" "framework-res.apk was not read"
ABI=x86_64
[ "$ARCH" = arm64 ] && ABI=arm64-v8a
expect "^sdk 36 abi $ABI$" "build.prop did not reach android.os.Build"
expect "^I FrameworkSmoke: log reaches stderr$" "android.util.Log is not ours"
# A framework TextView, with its services answered in process.
expect "^view [0-9]{3}x[0-9]{2} [0-9]{3,}$" "the framework's TextView did not draw"
expect "^unhandled \{.*IWindowManager\.hasNavigationBar" "service calls are not counted"
# Every native this program reaches is implemented: a default body called here is a regression.
# Under emulation two more are reached by timing alone, on both CPUs' libraries unregistered: a
# background GC runs BinderInternal.handleGc, and slow operations write EventLog events.
CALLED=$(grep "unimplemented native" "$WORK/out")
if [ ${#RUNNER[@]} -gt 0 ]; then
    CALLED=$(grep -v -E "native (com\.android\.internal\.os\.BinderInternal\.handleGc|android\.util\.EventLog\.writeEvent)\(" <<<"$CALLED")
fi
if [ -n "$CALLED" ]; then fail "default bodies were called: $CALLED"; fi
echo "ok: AOSP's framework drew, wrote text and read its resources on uxdroid's ART"
