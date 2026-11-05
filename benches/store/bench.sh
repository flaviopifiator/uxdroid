#!/bin/bash
# Times the store on a snapshot of an F-Droid repository served over loopback, so no Internet
# transfer is in the numbers. Runs in the sandbox, offline:
#
#   UXDROID_SAMPLES=<snapshot> docker compose run --rm -T sandbox bash /work/benches/store/bench.sh
#
# <snapshot> holds entry.jar and index-v2.json fetched together from https://f-droid.org/repo/, and
# net.gsantner.markor_164.apk (or set APK and PACKAGE). BASE=<uxdroid> also times that build's
# install of the same APK as a file, to compare with an older uxdroid.
set -u
TARGET=${TARGET:-/cache/target/release}
B=$TARGET/uxdroid
SNAPSHOT=${SNAPSHOT:-/samples}
APK=${APK:-net.gsantner.markor_164.apk}
PACKAGE=${PACKAGE:-net.gsantner.markor}
RUNS=${RUNS:-5}
export UXDROID_RUNTIME=${UXDROID_RUNTIME:-/runtime/art}
work=$(mktemp -d)
export HOME=$work/home XDG_DATA_HOME=$work/xdg
python3 -I "$(dirname "$0")/serve.py" "$SNAPSHOT" 8765 >"$work/http.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null; rm -rf "$work"' EXIT
sleep 1
D=$work/data
"$B" sources disable fdroid --data "$D" >/dev/null
"$B" sources add local http://127.0.0.1:8765 --fingerprint 43238d512c1e5eb2d6569f4a3afbf5523418b82e0a3ed1552770abb9a9c9ccab --data "$D" >/dev/null
TIMEFORMAT=%R
times() { # name, command...: runs it, appending its wall time in seconds
    local name=$1; shift
    { time "$@" >"$work/$name.out" 2>"$work/$name.err"; } 2>>"$work/$name.s" || { cat "$work/$name.err"; exit 1; }
}
median() { sort -n "$work/$1.s" | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }
for _ in $(seq "$RUNS"); do rm -rf "$D/store/sources"; times cold "$B" search markor --data "$D"; done
for _ in $(seq $((RUNS * 4))); do times warm "$B" search markor --data "$D"; done
for _ in $(seq "$RUNS"); do
    "$B" remove "$PACKAGE" --data "$D" >/dev/null 2>&1
    times store "$B" install "$PACKAGE" --data "$D"
done
for _ in $(seq "$RUNS"); do rm -rf "$work/file"; times file "$B" install "$SNAPSHOT/$APK" --data "$work/file"; done
if [ -n "${BASE:-}" ]; then
    for _ in $(seq "$RUNS"); do rm -rf "$work/base"; times base "$BASE" install "$SNAPSHOT/$APK" --data "$work/base"; done
fi
echo "search, no cached index (fetch, verify and keep a $(stat -c %s "$SNAPSHOT/index-v2.json")-byte index): median $(median cold) s of $(tr '\n' ' ' <"$work/cold.s")"
echo "search, cached index: median $(median warm) s"
echo "install $PACKAGE from the store: median $(median store) s of $(tr '\n' ' ' <"$work/store.s")"
echo "install $APK as a file: median $(median file) s of $(tr '\n' ' ' <"$work/file.s")"
[ -n "${BASE:-}" ] && echo "install $APK as a file with $BASE: median $(median base) s of $(tr '\n' ' ' <"$work/base.s")"
