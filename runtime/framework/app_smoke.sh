#!/bin/bash
# Launches an SDK-built APK's Activity through AOSP's own ActivityThread on uxdroid's ART. Runs in the
# dev service; the APK is ours, built by build_system.sh.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; tail -"${TAIL:-25}" "$WORK/out" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-system --manifest-path /work/Cargo.toml || fail "build"
LIBRARY="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release/libuxdroid_system.so"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" dev.uxdroid.system.Launcher --once \
    "$RUNTIME" "$LIBRARY" "$RUNTIME/samples/framework/app.apk" >"$WORK/out" 2>&1 || fail "exit $?"

expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
# The app's own Application, made by the framework, reading its label from the APK's resources.
expect "^application created com\.example\.framework\.SampleApplication label Framework sample$" \
    "the framework did not create the app's Application with its label"
# onCreate ran, and its text came from the APK's resources.arsc.
expect "^activity created com\.example\.framework\.MainActivity in com\.example\.framework\.SampleApplication text \"Hola desde una app real\"$" \
    "onCreate did not run with the app's resources"
expect "^launched com\.example\.framework/\.MainActivity in com\.example\.framework\.SampleApplication$" \
    "the framework does not hold the launched Activity"
expect "^window com\.android\.internal\.policy\.PhoneWindow$" "the Activity's window is not the framework's PhoneWindow"
expect "^content android\.widget\.TextView [1-9][0-9]*x[1-9][0-9]*$" "the content view was not laid out"
expect "^unhandled \{" "service calls are not counted"
grep -E "^(application|activity|launched|window|content|unhandled) |unimplemented native" "$WORK/out" | cut -c1-2000
echo "ok: AOSP's ActivityThread launched an APK's Activity, with its Application, resources and PhoneWindow"
