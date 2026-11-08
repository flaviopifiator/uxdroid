#!/bin/bash
# One process per app and data directory, and commands to it through its control socket: uxdroid app
# starts the SDK-built sample in a Wayland window, a second launch goes to the running process, and
# intents, status, list and stop reach it. Runs in the dev service; the APK is ours, built by
# build_system.sh.
set -u -o pipefail
WORK=$(mktemp -d)
# Removed however the script ends, a failure before the full trap below included.
trap 'rm -rf "$WORK"' EXIT
fail() { echo "FAIL: $*"; for f in "$WORK/out" "$LOG"; do [ -f "$f" ] && tail -"${TAIL:-25}" "$f" | cut -c1-220; done; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-cli --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
source /work/runtime/art/test_lib.sh
start_compositor
# The app processes, found by their command line: the image has no procps. A process that runs the
# app takes the package's name as its command line, as Android names app processes.
launcher_pids() {
    for cmdline in /proc/[0-9]*/cmdline; do
        tr '\0' ' ' <"$cmdline" 2>/dev/null | grep -q -E "dalvikvm64 .*dev\.uxdroid\.system\.Launcher|^$PACKAGE +$" \
            && basename "$(dirname "$cmdline")"
    done
}
trap 'kill -KILL $(launcher_pids) "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT
PACKAGE=com.example.framework
LOG="$XDG_RUNTIME_DIR/uxdroid/$PACKAGE.log"
UXDROID_RUNTIME="$RUNTIME" "$TARGET/uxdroid" install "$RUNTIME/samples/framework/app.apk" --data "$ANDROID_DATA" \
    >"$WORK/install" 2>&1 || { cat "$WORK/install"; fail "install"; }

app() { "$TARGET/uxdroid" app "$PACKAGE" "$@" >"$WORK/out" 2>&1; }
expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
logged() {
    for _ in $(seq 200); do
        grep -q -E "$1" "$LOG" && return
        pause 0.05
    done
    fail "$2"
}
launchers() { launcher_pids | wc -l; }

app status; [ $? -eq 3 ] || fail "status of an app that is not running must exit 3"
expect "^$PACKAGE is not running$" "status did not say the app is not running"

# 1. Started in the background, in a window.
app start --data-dir "$ANDROID_DATA" || fail "start: exit $?"
expect "^started $PACKAGE as process [0-9]+: $PACKAGE/\.MainActivity, log in $LOG$" "start did not report the process"
PID=$(grep -o -E "process [0-9]+" "$WORK/out" | grep -o -E "[0-9]+")
logged "first frame posted" "the started app drew no frame"
[ "$(stat -c %a "$XDG_RUNTIME_DIR/uxdroid/$PACKAGE.sock")" = 600 ] || fail "the control socket is not mode 0600"
[ "$(launchers)" = 1 ] || fail "expected one app process"

# 2. A second launch of the same APK over the same data goes to the running process and exits 0.
timeout 60 "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -cp "$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar" \
    dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" "$ANDROID_DATA/app/$PACKAGE/base.apk" \
    >"$WORK/out" 2>&1 || fail "a second launch: exit $?"
expect "^uxdroid: $PACKAGE is already running as process $PID over $ANDROID_DATA, so this launch went to it: brought_forward $PACKAGE/\.MainActivity$" \
    "a second launch did not go to the running app"
app start || fail "start while running: exit $?"
expect "^$PACKAGE is already running as process $PID; the intent went to it: brought_forward $PACKAGE/\.MainActivity$" \
    "start did not bring the running app forward"
[ "$(launchers)" = 1 ] || fail "a second launch started a second process"

# 3. Intents follow the launch mode and flags: the singleTop launcher activity gets onNewIntent, an
# implicit VIEW intent starts DetailActivity on top, and CLEAR_TOP|SINGLE_TOP goes back to the first.
app intent --activity .MainActivity --action com.example.PING --extra n=1 --extra word=hola || fail "intent: exit $?"
expect "^$PACKAGE \(process $PID\): new_intent $PACKAGE/\.MainActivity$" "the intent was not delivered"
logged "^activity new intent com\.example\.framework\.MainActivity action com\.example\.PING data null n=1 word=hola$" \
    "onNewIntent did not run with the intent's extras"
app intent --action android.intent.action.VIEW --data example://detail/7 --extra k=v || fail "view: exit $?"
expect "^$PACKAGE \(process $PID\): launched $PACKAGE/\.DetailActivity$" "the VIEW intent did not launch DetailActivity"
logged "^activity created com\.example\.framework\.DetailActivity action android\.intent\.action\.VIEW data example://detail/7 k=v$" \
    "DetailActivity did not get the intent"
logged "^activity paused com\.example\.framework\.MainActivity$" "the covered activity was not paused"
logged "^activity resumed com\.example\.framework\.DetailActivity$" "DetailActivity was not resumed"
app status || fail "status: exit $?"
expect "^activity: $PACKAGE/\.MainActivity \(paused\)$" "status does not list the paused activity"
expect "^activity: $PACKAGE/\.DetailActivity \(resumed\)$" "status does not list the resumed activity"
app intent --activity .MainActivity --flags 0x24000000 || fail "clear top: exit $?"
expect "^$PACKAGE \(process $PID\): new_intent $PACKAGE/\.MainActivity$" "CLEAR_TOP did not deliver to the first activity"
logged "^activity destroyed com\.example\.framework\.DetailActivity$" "CLEAR_TOP did not finish the activity above"
app intent --activity .Nope; [ $? -eq 1 ] || fail "an undeclared activity must be refused"
expect "^error: $PACKAGE refused: $PACKAGE declares no activity $PACKAGE\.Nope \(no_such_activity\)$" "no refusal reason"
"$TARGET/uxdroid" app list >"$WORK/out" 2>&1 || fail "list: exit $?"
expect "^$PACKAGE $PID $ANDROID_DATA$" "list does not show the app"

# 4. Stop finishes the activities and the process exits, leaving no socket and no lock.
app stop || fail "stop: exit $?"
expect "^stopped $PACKAGE \(process $PID, 1 activities finished\)$" "stop did not report"
logged "^activity destroyed com\.example\.framework\.MainActivity$" "stop did not finish the activity"
kill -0 "$PID" 2>/dev/null && fail "the process outlived stop"
[ -e "$XDG_RUNTIME_DIR/uxdroid/$PACKAGE.sock" ] && fail "stop left the socket"
app status; [ $? -eq 3 ] || fail "status after stop must exit 3"

# 5. A crash leaves no stale lock: a killed app starts again.
app start --data-dir "$ANDROID_DATA" || fail "start: exit $?"
CRASHED=$(grep -o -E "process [0-9]+" "$WORK/out" | grep -o -E "[0-9]+")
kill -KILL "$CRASHED"
for _ in $(seq 100); do kill -0 "$CRASHED" 2>/dev/null || break; pause 0.05; done
app start --data-dir "$ANDROID_DATA" || fail "start after a crash: exit $?"
expect "^started $PACKAGE as process [0-9]+: " "the app did not start again after a crash"
grep -q "process $CRASHED:" "$WORK/out" && fail "the crashed process answered"
app stop || fail "stop: exit $?"

# 6. A start that names another activity, by its name relative to the package, launches it first.
app start --activity .DetailActivity --extra k=v --data-dir "$ANDROID_DATA" || fail "start of an activity: exit $?"
expect "^started $PACKAGE as process [0-9]+: $PACKAGE/\.DetailActivity, " "start did not launch the named activity"
logged "^activity created com\.example\.framework\.DetailActivity action android\.intent\.action\.MAIN data null k=v$" \
    "the named activity did not get the intent"
app stop || fail "stop: exit $?"

# 7. An exception nothing catches on the main thread goes through the app's own handler, is logged as
# Android logs it, and ends the process, which would otherwise hang with its main thread gone.
app start --data-dir "$ANDROID_DATA" || fail "start: exit $?"
CRASHED=$(grep -o -E "process [0-9]+" "$WORK/out" | grep -o -E "[0-9]+")
logged "first frame posted" "the started app drew no frame"
app intent --activity .MainActivity --extra crash=deliberate || fail "intent to crash: exit $?"
for _ in $(seq 200); do kill -0 "$CRASHED" 2>/dev/null || break; pause 0.05; done
kill -0 "$CRASHED" 2>/dev/null && fail "the process outlived an uncaught exception on its main thread"
logged "^app handler saw deliberate$" "the app's own handler did not run"
logged "^E AndroidRuntime: FATAL EXCEPTION: main$" "the crash was not logged"
logged "^E AndroidRuntime: java\.lang\.IllegalStateException: deliberate$" "the crash's exception was not logged"
logged "^E AndroidRuntime: Process: com\.example\.framework, PID: $CRASHED$" "the crash does not name its process"
app status; [ $? -eq 3 ] || fail "status after a crash must exit 3"
echo "ok: one process per app and data directory; a second launch, start, intents, status, list and stop reach it through its control socket; an uncaught exception ends it"
