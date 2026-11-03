#!/bin/bash
# Apps as systemd user services, in a container without systemd: uxdroid service enable writes the units,
# socket_unit.py plays the user manager with exactly those files, and a connection to the control socket
# starts the SDK-built sample on demand, again after it stops, and once per profile. socket_activate.py
# then passes a socket as systemd-socket-activate does, and a service start over a data
# directory an instance outside the service already runs refuses with status 75. Runs in the dev
# service; the APK is ours, built by build_system.sh.
set -u -o pipefail
WORK=$(mktemp -d)
fail() {
    echo "FAIL: $*"
    for f in "$WORK/out" "$WORK/manager.log" "$WORK/app.log" "$WORK/activate.log"; do
        [ -f "$f" ] && { echo "--- $f"; tail -"${TAIL:-15}" "$f" | cut -c1-220; }
    done
    exit 1
}

cargo build -q --release -p uxdroid-system -p uxdroid-cli --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
source /work/runtime/art/run.sh
source /work/runtime/art/test_lib.sh
start_compositor
launcher_pids() {
    for cmdline in /proc/[0-9]*/cmdline; do
        { tr '\0' ' ' <"$cmdline"; } 2>/dev/null | grep -q "dalvikvm64 .*dev\.uxdroid\.system\.Launcher" \
            && basename "$(dirname "$cmdline")"
    done
}
MANAGERS=()
trap 'kill -KILL $(launcher_pids) "${MANAGERS[@]}" "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT
PACKAGE=com.example.framework
U="$TARGET/uxdroid"
UNITS="$HOME/.config/systemd/user"
CONTROL="$XDG_RUNTIME_DIR/uxdroid"
DATA="$WORK/data"
WORK_DATA="$HOME/.local/share/uxdroid/profiles/work"
for data in "$DATA" "$WORK_DATA" "$WORK/data-alt"; do
    "$U" install "$RUNTIME/samples/framework/app.apk" --data "$data" >"$WORK/out" 2>&1 || fail "install into $data"
done

run() { "$U" "$@" >"$WORK/out" 2>&1; }
expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }
waitfor() {
    for _ in $(seq 400); do
        grep -q -E "$2" "$1" 2>/dev/null && return
        pause 0.05
    done
    fail "$3"
}
launchers() { launcher_pids | wc -l; }
# Plays the user manager for one profile's units; its log says when it listens, starts and reaps.
manage() {
    local unit="$UNITS/uxdroid-$PACKAGE@$1" log="$WORK/manager-$1.log"
    python3 -I /work/runtime/framework/socket_unit.py "$unit.socket" "$unit.service" "$log" >>"$WORK/app-$1.log" 2>&1 &
    MANAGERS+=($!)
    waitfor "$log" "^listening " "the manager of profile $1 does not listen"
}
pid_of() { grep -o -E "process [0-9]+" "$WORK/out" | head -1 | grep -o -E "[0-9]+"; }

# 1. The units, written for this data directory; nothing runs systemctl.
run service enable "$PACKAGE" --data-dir "$DATA" --runtime "$RUNTIME" || fail "enable: exit $?"
expect "^to start $PACKAGE on demand, run: systemctl --user daemon-reload && systemctl --user enable --now uxdroid-$PACKAGE@default\.socket$" \
    "enable did not say what to run"
grep -q -x "RestrictAddressFamilies=AF_UNIX AF_NETLINK" "$UNITS/uxdroid-$PACKAGE@default.service" \
    || fail "an app without INTERNET was given the network"
run service status "$PACKAGE" || fail "status: exit $?"
expect "^running: no$" "status of a service that never started"

# 2. On demand: nothing runs until a connection; uxdroid app status does not start it, start does.
manage default
ln -sf "$WORK/manager-default.log" "$WORK/manager.log"; ln -sf "$WORK/app-default.log" "$WORK/app.log"
[ "$(stat -c %a "$CONTROL/$PACKAGE.sock")" = 600 ] || fail "the socket unit's socket is not mode 0600"
run app "$PACKAGE" status; [ $? -eq 3 ] || fail "status before any start must exit 3"
run app list; [ -s "$WORK/out" ] && fail "list showed an app that does not run"
[ "$(launchers)" = 0 ] || fail "status or list started the app"
grep -q "^started" "$WORK/manager.log" && fail "status or list connected to the socket"
run app "$PACKAGE" start || fail "start through the socket unit: exit $?"
expect "^$PACKAGE is already running as process [0-9]+; the intent went to it: brought_forward $PACKAGE/\.MainActivity$" \
    "start did not reach the app the socket started"
PID=$(pid_of)
grep -q -x "started $PID" "$WORK/manager.log" || fail "the app answering is not the one the manager started"
waitfor "$WORK/app.log" "first frame posted" "the app started on demand drew no frame"
[ "$(launchers)" = 1 ] || fail "expected one app process"
[ "$(stat -c %a "$CONTROL/$PACKAGE.sock")" = 600 ] || fail "the served socket is not mode 0600"
run service status "$PACKAGE" || fail "service status: exit $?"
expect "^running: process $PID$" "service status does not show the running app"
run app "$PACKAGE" status || fail "status: exit $?"
expect "^pid: $PID$" "status did not reach the app"

# 3. Stop ends the process; the socket stays the manager's, and the next connection starts it again.
run app "$PACKAGE" stop || fail "stop: exit $?"
waitfor "$WORK/manager.log" "^exited $PID 0$" "the stopped app did not exit 0"
[ -S "$CONTROL/$PACKAGE.sock" ] || fail "the app removed the socket its manager owns"
run app "$PACKAGE" status; [ $? -eq 3 ] || fail "status after stop must exit 3"
run app "$PACKAGE" start || fail "start again: exit $?"
AGAIN=$(pid_of)
[ "$AGAIN" != "$PID" ] && grep -q -x "started $AGAIN" "$WORK/manager.log" || fail "a connection after stop did not start the app again"

# 4. Two profiles of one package at once, each through its own units and socket.
run service enable "$PACKAGE" --profile work --runtime "$RUNTIME" || fail "enable work: exit $?"
manage work
run app "$PACKAGE" start --profile work || fail "start work: exit $?"
WORK_PID=$(pid_of)
grep -q -x "started $WORK_PID" "$WORK/manager-work.log" || fail "profile work was not started by its own unit"
[ "$(launchers)" = 2 ] || fail "two profiles must run as two processes"
run app "$PACKAGE" status --profile work || fail "status work: exit $?"
expect "^data: $WORK_DATA$" "profile work does not run over its own data"
run app list || fail "list: exit $?"
expect "^$PACKAGE $AGAIN $DATA$" "list does not show the default profile"
expect "^$PACKAGE@work $WORK_PID $WORK_DATA$" "list does not show profile work"
run app "$PACKAGE" stop --profile work || fail "stop work: exit $?"
run app "$PACKAGE" status || fail "the default profile stopped with work: exit $?"
run app "$PACKAGE" stop || fail "stop: exit $?"

# 5. A socket passed and the command started at once, as systemd-socket-activate does (the image lacks
# it): bound with the mode the umask leaves, which the launcher makes 0600.
python3 -I /work/runtime/framework/socket_activate.py "$CONTROL/$PACKAGE@alt.sock" control "$U" service run "$PACKAGE" --profile alt --data-dir "$WORK/data-alt" --runtime "$RUNTIME" \
    >"$WORK/activate.log" 2>&1 &
ACTIVATED=$!
for _ in $(seq 400); do run app "$PACKAGE" status --profile alt && break; pause 0.05; done
expect "^pid: $ACTIVATED$" "the app started with a passed socket does not serve it"
[ "$(stat -c %a "$CONTROL/$PACKAGE@alt.sock")" = 600 ] || fail "a passed socket was not made private"
run app "$PACKAGE" stop --profile alt || fail "stop alt: exit $?"

# 6. Outside the service: a launch while the socket unit listens leaves its socket alone, and a service
# start over the data directory that launch holds refuses with 75 instead of waiting on its own socket.
"$U" service run "$PACKAGE" --data-dir "$DATA" --runtime "$RUNTIME" >"$WORK/manual.log" 2>&1 &
MANUAL=$!
waitfor "$WORK/manual.log" "runs without a control socket: .*belongs to the app's systemd socket unit" \
    "a launch outside the service did not leave the socket unit's socket alone"
[ -S "$CONTROL/$PACKAGE.sock" ] || fail "a launch outside the service removed the socket unit's socket"
python3 -I /work/runtime/framework/socket_activate.py "$CONTROL/$PACKAGE@dup.sock" control "$U" service run "$PACKAGE" --profile dup --data-dir "$DATA" --runtime "$RUNTIME" \
    >"$WORK/activate.log" 2>&1
STATUS=$?
[ "$STATUS" = 75 ] || fail "a service start over a running data directory exited $STATUS, not 75"
grep -q "is already running over $DATA outside its service" "$WORK/activate.log" || fail "the refusal says why"
kill "$MANUAL"
echo "ok: units written; the app starts on demand through its socket, again after stop, once per profile, passed at once, and refuses to run twice over one data directory"
