#!/bin/bash
# What the compatibility corpus (corpus_smoke.sh) needed from the platform, checked with an app of
# ours, so it runs in the dev service: the app's identity, its storage and the system services it
# asks for. The APK is built by build_system.sh.
set -u -o pipefail
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; grep -v -E "jni_internal|^	at (android\.os|java\.lang\.reflect)" "$WORK/out" | tail -"${TAIL:-25}" | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-cli --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
APK="$RUNTIME/samples/framework/compat.apk"
[ -r "$APK" ] || fail "no $APK: run runtime/framework/build_system.sh"
UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$APK" --data "$ANDROID_DATA" >"$WORK/install" 2>&1 \
    || { cat "$WORK/install"; fail "install"; }
timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" dev.uxdroid.system.Launcher --once \
    "$RUNTIME" "$TARGET/libuxdroid_system.so" "$ANDROID_DATA/app/com.example.compat/base.apk" >"$WORK/out" 2>&1 \
    || fail "exit $?"
expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
grep -E "^(identity|database|factory|network|activity|no activity|launched) " "$WORK/out"

# 1. The app runs under an application uid of its own, as Android gives each installed app, whatever
# the Linux uid: the framework treats 1000 as the system and refuses it WebView.
line=$(grep -E "^identity " "$WORK/out") || fail "the app did not report its identity"
read -r _ _ my _ calling _ info _ user _ is_app <<<"$line"
[ "$my" = "$calling" ] && [ "$my" = "$info" ] \
    || fail "Process.myUid, Binder.getCallingUid and ApplicationInfo.uid differ: $line"
[ "$my" -ge 10000 ] && [ "$my" -le 19999 ] || fail "the app's uid $my is not an application uid"
[ "$user" = "UserHandle{0}" ] && [ "$is_app" = "true" ] || fail "the app is not an application of user 0: $line"
# 2. A database opens in the app's data directory, with SQLite's page size from that file system:
# the framework took it from a literal /data.
expect "^database $ANDROID_DATA/data/com\.example\.compat/databases/compat\.db rows 1 page size [1-9][0-9]*$" \
    "a database did not open in the app's data directory"
# 3. The manifest's appComponentFactory, a class of the app, is loaded by the app's class loader and
# makes its components; every androidx app names androidx.core's.
expect "^factory instantiates com\.example\.compat\.CompatApplication with the app's class loader true$" \
    "the app's component factory did not make its Application"
grep -q "Unable to instantiate appComponentFactory" "$WORK/out" \
    && fail "the component factory was looked up outside the app's class loader"
# 5. The network the host has, as ConnectivityManager and TrafficStats report it: a default route is
# an active network with internet; the host's counters are the totals, an app's own traffic is not
# counted apart. Every corpus app looked both services up.
if awk 'NR > 1 && $2 == "00000000" { found = 1 } END { exit !found }' /proc/net/route; then online=true; else online=false; fi
expect "^network active $online internet $online connected $online total received true own received -1$" \
    "ConnectivityManager or TrafficStats do not report the host's network (default route: $online)"
grep -q "TrafficStats not initialized" "$WORK/out" && fail "TrafficStats did not find its service"

# 4. The app's own startActivity reaches its task: by class and by action, and an intent no activity
# takes fails with ActivityNotFoundException, as on Android.
expect "^activity created com\.example\.compat\.SecondActivity by class$" "an activity the app started by class was not made"
expect "^activity created com\.example\.compat\.SecondActivity by action$" "an activity the app started by action was not made"
expect "^no activity takes No Activity found to handle Intent$" "an intent no activity takes did not fail as on Android"
expect "^launched com\.example\.compat/\.CompatActivity " "the activity was not launched"
echo "ok: the app has its own identity, storage and the system services the corpus asks for"
