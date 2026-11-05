#!/bin/bash
# Times `uxdroid search` and `uxdroid install <package> --from play` against the real Google Play,
# signed in with your own account. It needs the network, which the sandbox does not have, so it runs in a copy of the
# sandbox service with the project's default network (see README.md); no app code runs in it: the
# install compiles the app and parses its manifest, and nothing starts it.
#
#   docker compose -f compose.yaml -f <override with network> run --rm -T sandbox-net \
#       bash /work/benches/store/play.sh
#
# DATA is a data directory where Play is enabled and signed in with `uxdroid sources play --account
# <email>`. Every request counts against the account, so RUNS stays small.
set -u
TARGET=${TARGET:-/cache/target/release}
B=$TARGET/uxdroid
QUERY=${QUERY:-markor}
PACKAGE=${PACKAGE:-net.gsantner.markor}
RUNS=${RUNS:-3}
export UXDROID_RUNTIME=${UXDROID_RUNTIME:-/runtime/art} LANG=en_US.UTF-8
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export HOME=$work/home
D=${DATA:?DATA must name a data directory where Play is enabled and signed in}
TIMEFORMAT=%R
times() { # name, command...: runs it, appending its wall time in seconds
    local name=$1; shift
    { time "$@" >"$work/$name.out" 2>"$work/$name.err"; } 2>>"$work/$name.s" || { cat "$work/$name.err"; exit 1; }
    sleep 1
}
median() { sort -n "$work/$1.s" | awk '{ a[NR] = $1 } END { print a[int((NR + 1) / 2)] }'; }
# The first search signs in when no sign-in is kept; it is timed apart.
times first "$B" search "$QUERY" --data "$D"
for _ in $(seq "$RUNS"); do times search "$B" search "$QUERY" --data "$D"; done
for _ in $(seq "$RUNS"); do
    "$B" remove "$PACKAGE" --data "$D" >/dev/null 2>&1
    times install "$B" install "$PACKAGE" --from play --data "$D"
done
"$B" remove "$PACKAGE" --data "$D" >/dev/null 2>&1
head -1 "$work/install.out"
echo "first search, signing in if needed: $(cat "$work/first.s") s"
echo "search $QUERY on Google Play: median $(median search) s of $(tr '\n' ' ' <"$work/search.s")"
echo "install $PACKAGE from Google Play: median $(median install) s of $(tr '\n' ' ' <"$work/install.s")"
