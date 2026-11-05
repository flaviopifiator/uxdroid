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
# Other apps over the same data directory, which the compat app's PackageManager must know: the peer,
# and a package whose uid is the compat app's own.
for other in peer uidtwin; do
    UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$RUNTIME/samples/framework/$other.apk" --data "$ANDROID_DATA" \
        >"$WORK/install-$other" 2>&1 || { cat "$WORK/install-$other"; fail "install of the $other app"; }
done
timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" dev.uxdroid.system.Launcher --once \
    "$RUNTIME" "$TARGET/libuxdroid_system.so" "$ANDROID_DATA/app/com.example.compat/base.apk" >"$WORK/out" 2>&1 \
    || fail "exit $?"
expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
grep -E "^(identity|process|telephony|managers|database|factory|network|notification|alarm|appops|shortcuts|game|device|khronos|graphics|graphics31|graphics32|features|textservices|pending|activity|no activity|launched|result|broadcast|provider|stores|packages) " "$WORK/out"

# 1. The app runs under an application uid of its own, as Android gives each installed app, whatever
# the Linux uid: the framework treats 1000 as the system and refuses it WebView.
line=$(grep -E "^identity " "$WORK/out") || fail "the app did not report its identity"
read -r _ _ my _ calling _ info _ user _ is_app <<<"$line"
[ "$my" = "$calling" ] && [ "$my" = "$info" ] \
    || fail "Process.myUid, Binder.getCallingUid and ApplicationInfo.uid differ: $line"
[ "$my" -ge 10000 ] && [ "$my" -le 19999 ] || fail "the app's uid $my is not an application uid"
[ "$user" = "UserHandle{0}" ] && [ "$is_app" = "true" ] || fail "the app is not an application of user 0: $line"
# Instagram finds its main process by name: Android's running processes, the app's memory state
# and /proc/self/cmdline all name the package, as the zygote leaves them.
expect "^process name com\\.example\\.compat running com\\.example\\.compat:true importance 100 cmdline com\\.example\\.compat$" \
    "the process is not named after the app where Android names it"
expect "^telephony subscriptions listened$" "the telephony registry is missing, so SIM listeners throw"
expect "^managers missing fingerprint carrier_config wifi midi credential$" \
    "a manager every Android device has is null (only those of absent hardware may be: fingerprint, SIM, Wi-Fi, MIDI, credentials)"
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
expect "^graphics32 mask \[true, true, true, true\] short mask refused short indices refused$" \
    "an indexed query or client indices are not checked against what GL writes and reads"

# 8. The log and the console stay where the process started even after the app points its stderr
# at a pipe it copies to the log, as AnkiDroid's backend does: otherwise each line loops back.
expect "^I compat: log after redirect$" "the log went into the app's own stderr pipe"
expect "^err after redirect$" "System.err went into the app's own stderr pipe"
grep -q "STDERR: .*STDERR: " "$WORK/out" && fail "the log loops through the app's stderr pipe"

# 4. The app's own startActivity reaches its task: by class and by action, and an intent no activity
# takes fails with ActivityNotFoundException, as on Android.
expect "^activity created com\.example\.compat\.SecondActivity by class$" "an activity the app started by class was not made"
expect "^activity created com\.example\.compat\.SecondActivity by action$" "an activity the app started by action was not made"
expect "^activity created com\.example\.compat\.SecondActivity by activities$" "an activity the app started with startActivities was not made"
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
# 10. The app's own content providers are installed as it starts, before Application.onCreate, and
# read their meta-data as androidx-startup's provider does; a disabled one is never made, and one
# declared for another process is made in the app's process when first queried.
expect "^provider created before application onCreate true meta-data startup$" \
    "the app's provider was not installed before Application.onCreate with its meta-data"
expect "^provider query provider CompatProvider$" "a query of the app's own authority did not reach its provider"
expect "^provider created RemoteProvider after application onCreate true$" \
    "a provider of another process was not made when first queried"
expect "^provider query remote RemoteProvider$" "a query did not reach the provider of another process"
expect "^uxdroid: provider com\.example\.compat\.RemoteProvider of process com\.example\.compat:remote runs in the app's process com\.example\.compat$" \
    "a provider moved into the app's process was not reported"
expect "^provider query disabled none$" "a disabled provider answered"
grep -q "^provider created DisabledProvider" "$WORK/out" && fail "a disabled provider was made"
expect "^provider resolved com\.example\.compat\.CompatProvider meta-data null$" \
    "resolveContentProvider did not find the provider, or gave meta-data nobody asked for"
# 11. What Aurora Store and microG asked on start: a package installer with no sessions that refuses
# to create one by name, power, domain verification of the app's web links, no pending jobs and no
# accounts.
expect "^stores installer sessions 0 all 0$" "the package installer does not list its sessions"
expect "^stores session refused IPackageInstaller\.createSession is not implemented: " \
    "creating an install session did not fail naming the call"
expect "^stores power interactive true save false idle false$" "PowerManager does not describe a desktop in use"
# Nothing restricts an app in the background on a desktop, as Aurora's onboarding asks.
expect "^stores power ignoring battery optimizations true$" "the app is not exempt from battery optimizations"
expect "^stores domains allowed true hosts \{links\.example\.com=0\}$" \
    "domain verification does not list the app's web domains"
expect "^stores domains of another package not found$" "domain verification answered for another package"
expect "^stores jobs pending 0 job null$" "JobScheduler does not list its pending jobs"
expect "^stores accounts 0 google 0 authenticators 0$" "AccountManager does not list its accounts"
# 12. The PackageManager knows every app installed over the data directory, as an app store asks it:
# the app's own meta-data with GET_META_DATA only (Sentry stops without it), the other app's package,
# meta-data, uid, launcher, exported activities, services and providers by intent and by name, and its
# own default activity. An app turns its own components off, not another's, and that lasts.
expect "^packages meta-data auto-init false without flag null$" \
    "getApplicationInfo with GET_META_DATA did not give the app's meta-data, or gave it without the flag"
expect "^packages installed com\.example\.a11245,com\.example\.compat,com\.example\.peer applications 3$" \
    "getInstalledPackages does not list every installed app"
expect "^packages own uid names com\.example\.compat,com\.example\.a11245 name com\.example\.compat$" \
    "the app's own uid named another package that shares it, or not every package"
expect "^packages peer version 1\.3 3 meta-data peer$" "another app's package or meta-data is not known"
expect "^packages uid peer application true named com\.example\.peer$" "another app's uid is not its own"
expect "^packages missing not found$" "a package nobody installed was found"
expect "^packages launch peer com\.example\.peer\.PeerActivity own com\.example\.compat\.CompatActivity$" \
    "getLaunchIntentForPackage does not find the launcher activities"
expect "^packages view peer handlers com\.example\.peer\.ViewerActivity resolved com\.example\.peer\.ViewerActivity https none$" \
    "an intent another app takes is not resolved to its exported activity alone"
expect "^packages service com\.example\.peer\.PeerService meta-data bound$" "another app's service is not found"
expect "^packages providers com\.example\.peer\.PeerProvider$" "another app's provider is not found by its intent"
expect "^packages activity com\.example\.peer\.ViewerActivity exported true application com\.example\.peer$" \
    "another app's activity is not found by name"
expect "^packages toggled before 0 found 1 after 2 found 0$" "a component the app turned off is still offered"
expect "^packages toggled another app refused$" "an app changed another app's component"
expect "^packages toggled list with a bad entry refused, state 2$" \
    "setComponentEnabledSettings wrote entries of a list it refused"
expect "^packages can request installs true$" "an app that asks to install others may not"
expect "^packages start other app refused uxdroid cannot start com\.example\.peer/\.ViewerActivity from com\.example\.compat: it is another app" \
    "starting another app's activity did not fail naming it"
[ "$(cat "$ANDROID_DATA/system/components/com.example.compat" 2>/dev/null)" = "2 com.example.compat.ToggledActivity" ] \
    || fail "the component setting was not kept in system/components"
expect "^launched com\.example\.compat/\.CompatActivity " "the activity was not launched"
# The other app, started next, sees what the first one set and declared. The first one's install
# record is then one another app's PackageManager must not trust: nested deeper than any record, then
# larger than any, both behind the header that makes it current; and finally it is gone. Each time its
# APK is parsed instead, and the reading app keeps running.
RECORD="$ANDROID_DATA/app/com.example.compat/package.record"
cp "$RECORD" "$WORK/record"
hostile() {
    PYTHONDONTWRITEBYTECODE=1 python3 - "$WORK/record" "$RECORD" "$1" <<'PY'
import struct, sys
original, out, kind = sys.argv[1:]
data = open(original, "rb").read()
end = 0
for _ in range(2):  # the magic and the identity, as DataOutputStream.writeUTF wrote them
    end += 2 + struct.unpack(">H", data[end:end + 2])[0]
if kind == "deep":  # lists in lists, as no framework object nests
    body = (b"\x06" + struct.pack(">i", 1)) * 300000 + b"\x00"
else:  # an array of 64 Mi longs, which a reader would allocate before reading them
    body = b"\x04" + struct.pack(">i", 4) + b"long" + struct.pack(">i", 64 << 20) + b"\x00" * (64 << 20)
open(out, "wb").write(data[:end] + body)
PY
}
for record in deep large missing; do
    case $record in
        missing) rm "$RECORD" ;;
        *) hostile "$record" ;;
    esac
    timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$CP" dev.uxdroid.system.Launcher --once \
        "$RUNTIME" "$TARGET/libuxdroid_system.so" "$ANDROID_DATA/app/com.example.peer/base.apk" >"$WORK/out" 2>&1 \
        || fail "the peer app, with a $record install record of the compat app: exit $?"
    grep -E "^peer |install record .*com\.example\.compat" "$WORK/out" | cut -c1-200
    case $record in
        deep) expect "^uxdroid: the install record .*com\.example\.compat/package\.record is not used, the APK is parsed instead: the record nests deeper than 64 values$" \
            "a record nested too deep was not refused by its depth" ;;
        large) expect "^uxdroid: the install record .*com\.example\.compat/package\.record is not used, the APK is parsed instead: the record is larger than 4194304 bytes$" \
            "a record too large was not refused by its size" ;;
    esac
    expect "^peer sees compat toggled 2 found 0$" "a component setting did not last into another app's start ($record record)"
    expect "^peer resolves compat launcher com\.example\.compat\.CompatActivity auto-init false$" \
        "an app with a $record install record is not known from its APK"
    expect "^peer own launcher com\.example\.peer\.PeerActivity$" "an app does not find its own launcher activity ($record record)"
    expect "^launched com\.example\.peer/\.PeerActivity " "the peer app was not launched ($record record)"
done
echo "ok: the app has its own identity, storage and the system services the corpus asks for, and knows the other installed app"
