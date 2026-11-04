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
grep -E "^(identity|database|factory|network|notification|alarm|appops|shortcuts|game|device|khronos|graphics|graphics31|features|textservices|pending|activity|no activity|launched|result|broadcast) " "$WORK/out"

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
# A network callback hears of the current network once, with its capabilities; offline, of none.
for name in default internet; do
    if [ "$online" = true ]; then
        [ "$(grep -c "^network callback $name available$" "$WORK/out")" = 1 ] \
            || fail "the $name network callback did not hear of the network once"
        expect "^network callback $name internet true$" "the $name network callback got no capabilities"
    else
        grep -q "^network callback $name" "$WORK/out" && fail "the $name network callback heard of a network offline"
    fi
done
grep -q "TrafficStats not initialized" "$WORK/out" && fail "TrafficStats did not find its service"

# 6. Services the corpus stopped on without them: notification channels and posted notifications,
# alarms that fire their listeners, app ops allowed, dynamic shortcuts kept, and a game mode.
expect "^notification channel General enabled true active 1$" "notification channels or posting do not work"
expect "^notification cancelled active 0$" "a cancelled notification stayed active"
expect "^alarm fired on main thread true$" "a due alarm did not fire its listener on the main thread"
expect "^alarm next clock true exact true$" "the next alarm clock is not the one set"
expect "^appops note 0 check 0$" "app ops are not allowed"
expect "^shortcuts dynamic 1 scan max 15 pin false$" "dynamic shortcuts are not kept"
expect "^shortcuts over the limit refused kept 1$" "a refused list of shortcuts was kept in part"
expect "^alarm clock went off, next in the future true$" "an alarm clock that went off is still the next one"
expect "^features pc true touchscreen true output true microphone true camera false telephony false webview false listed true$" \
    "the device does not list the features a desktop has"
expect "^textservices true spell checker false$" "the text services manager is missing"
expect "^game mode 1$" "the game mode is not standard"

# 7. The device is uxdroid, not the Robolectric build the framework comes from, which apps take for a
# unit test and skip their native code; and the platform has the Khronos GL classes GLSurfaceView
# hands its renderer.
expect "^device fingerprint uxdroid/uxdroid/uxdroid:16/[A-Z0-9.]+/1:user/release-keys model uxdroid tags release-keys$" \
    "Build does not describe uxdroid"
expect "^khronos javax\.microedition\.khronos\.opengles\.GL10 javax\.microedition\.khronos\.egl\.EGLContext$" \
    "the javax.microedition.khronos classes are missing"

# 9. EGL10 and GLES20 of the framework reach the host's EGL and GLES: an ES 2 context on a pbuffer is
# cleared to orange and read back, a shader compiles, and an array too short for what GL would write
# is refused. GLSurfaceView apps such as Mindustry need all of it.
expect "^graphics EGL 1\.[0-9]+ current true GL true pixel ff(7f|80)00ff shader 1 refused a short array$" \
    "EGL10 or GLES20 does not reach the host's GL"

# GLES31 and GLES32 reach the host's GL too, with the same checks on what GL writes.
expect "^graphics31 pipeline true log true border \[1, 2, 3, 4\] short border refused$" \
    "GLES31 or GLES32 does not reach the host's GL"

# 8. The log and the console stay where the process started even after the app points its stderr
# at a pipe it copies to the log, as AnkiDroid's backend does: otherwise each line loops back.
expect "^I compat: log after redirect$" "the log went into the app's own stderr pipe"
expect "^err after redirect$" "System.err went into the app's own stderr pipe"
grep -q "STDERR: .*STDERR: " "$WORK/out" && fail "the log loops through the app's stderr pipe"

# 4. The app's own startActivity reaches its task: by class and by action, and an intent no activity
# takes fails with ActivityNotFoundException, as on Android.
expect "^activity created com\.example\.compat\.SecondActivity by class$" "an activity the app started by class was not made"
expect "^activity created com\.example\.compat\.SecondActivity by action$" "an activity the app started by action was not made"
expect "^no activity takes No Activity found to handle Intent$" "an intent no activity takes did not fail as on Android"
# A PendingIntent is the same for the same request, and sending one for an activity starts it.
expect "^pending broadcast true same true creator com\.example\.compat absent true$" "PendingIntents are not made as on Android"
expect "^pending flags apart true first kept true canceled refused$" "a PendingIntent's flags are not part of its identity"
expect "^activity created com\.example\.compat\.SecondActivity by pending intent$" "a PendingIntent for an activity did not start it"
expect "^off the main thread, no activity takes it$" "a start off the main thread was told it succeeded"
# Broadcasts inside the app: registered and manifest receivers on the main thread, an ordered one
# through its receivers by priority to its result receiver, an alarm's PendingIntent when it is due
# and not before, and no implicit broadcast to a manifest receiver, as on Android since 8.0.
expect "^broadcast registered com\.example\.compat\.PING main true$" "a registered receiver did not get its broadcast"
expect "^broadcast manifest com\.example\.compat\.REMIND by package main true$" "a manifest receiver did not get a broadcast for its package"
expect "^broadcast ordered result ab code 2$" "an ordered broadcast did not pass its receivers by priority to its result"
expect "^broadcast manifest com\.example\.compat\.REMIND by alarm main true$" "a due alarm's PendingIntent did not broadcast"
grep -q "^broadcast manifest .* by implicit" "$WORK/out" && fail "an implicit broadcast reached a manifest receiver"
grep -q "^broadcast manifest .* by later alarm" "$WORK/out" && fail "an alarm broadcast before its time"
# A result set before finish reaches the activity that started it for one, with its request code.
expect "^result request 7 code -1 answer 42$" "a result of startActivityForResult did not come back"
expect "^launched com\.example\.compat/\.CompatActivity " "the activity was not launched"
echo "ok: the app has its own identity, storage and the system services the corpus asks for"
