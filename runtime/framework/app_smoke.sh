#!/bin/bash
# Launches an SDK-built APK's Activity through AOSP's own ActivityThread on uxdroid's ART. Runs in the
# dev service; the APK is ours, built by build_system.sh.
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
APK="$RUNTIME/samples/framework/app.apk"
expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
check_launch() {
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
}

# 1. An APK nobody installed: the launcher parses it and makes default bodies at start.
timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" dev.uxdroid.system.Launcher --once \
    "$RUNTIME" "$LIBRARY" "$APK" >"$WORK/out" 2>&1 || fail "exit $?"
check_launch
grep -E "^(application|activity|launched|window|content|unhandled) |unimplemented native" "$WORK/out" | cut -c1-2000
grep -o "unimplemented native [^ ]*" "$WORK/out" | sort > "$WORK/defaults-uninstalled"

# 2. The same APK after uxdroid install: compiled code, the recorded manifest and the default bodies'
# library are used, and the app sees the same.
UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$APK" --data "$ANDROID_DATA" >"$WORK/install" 2>&1 \
    || { cat "$WORK/install"; fail "install"; }
INSTALLED="$ANDROID_DATA/app/com.example.framework"
grep -q -E "^prepared com\.example\.framework in $INSTALLED: code compiled \(verify\), manifest recorded with launcher com\.example\.framework\.MainActivity, [0-9]{4} default bodies$" \
    "$WORK/install" || { cat "$WORK/install"; fail "the install did not prepare the app"; }
timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -verbose:oat -cp "$CP" dev.uxdroid.system.Launcher --once \
    "$RUNTIME" "$LIBRARY" "$INSTALLED/base.apk" >"$WORK/out" 2>&1 || fail "installed: exit $?"
check_launch
expect "GetBestOatFile\(\)=$INSTALLED/oat/x86_64/base\.odex \(executable=1\)" "ART did not use the code compiled at install"
if grep -E "uxdroid: (the install record|default bodies of) " "$WORK/out"; then fail "what the install prepared was not used"; fi
grep -o "unimplemented native [^ ]*" "$WORK/out" | sort > "$WORK/defaults-installed"
# Default bodies bound by name must not answer a native that something else answers otherwise.
# Process.readProcFile is reached in some runs and not others, installed or not.
comm -13 "$WORK/defaults-uninstalled" "$WORK/defaults-installed" | grep -v "Process\.readProcFile" \
    && fail "natives reached default bodies only once installed"

# 3. A stale install, here an APK changed after it: the record is refused with a reason, and the app
# still starts the way an APK nobody installed does.
touch -d "1 minute ago" "$INSTALLED/base.apk"
timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" dev.uxdroid.system.Launcher --once \
    "$RUNTIME" "$LIBRARY" "$INSTALLED/base.apk" >"$WORK/out" 2>&1 || fail "stale: exit $?"
check_launch
expect "^uxdroid: the install record $INSTALLED/package\.record is not used, the APK is parsed instead until uxdroid install runs again: the runtime or the APK changed since install$" \
    "a stale install record was not refused"
echo "ok: AOSP's ActivityThread launched an APK's Activity, with its Application, resources and PhoneWindow, before and after uxdroid install"
