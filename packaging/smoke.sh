#!/bin/bash
# Installs a built package on this distribution and uses it as a person would; CI runs it on each
# distribution of its matrix. Runs as root in a clean Debian or Ubuntu image, with network for apt
# and F-Droid: packaging/smoke.sh <dir holding the .deb and .tar.gz for this CPU>.
set -eu -o pipefail

DIST=$(realpath "${1:?usage: packaging/smoke.sh <dist dir>}")
ARCH=$(dpkg --print-architecture)
DEB=$(echo "$DIST"/uxdroid_*_"$ARCH".deb)
TAR=$(echo "$DIST"/uxdroid-*-linux-"$ARCH".tar.gz)
export DEBIAN_FRONTEND=noninteractive
step() { echo "== $*"; }
fail() { echo "FAIL: $*" >&2; exit 1; }

step "install $(basename "$DEB")"
apt-get update -qq
apt-get install -y -qq --no-install-recommends ca-certificates "$DEB" > /dev/null

step "every recommended library has a package here"
recommends=$(dpkg-deb -f "$DEB" Recommends)
IFS=, read -ra entries <<< "$recommends"
for entry in "${entries[@]}"; do
    found=
    IFS='|' read -ra alternatives <<< "$entry"
    for alternative in "${alternatives[@]}"; do
        name=$(echo "$alternative" | sed 's/(.*)//; s/ //g')
        [ "$name" = uxdroid-runtime ] && found=yes
        apt-cache show "$name" > /dev/null 2>&1 && found=yes
    done
    [ -n "$found" ] || fail "no package provides$entry"
done

step "the program and its libraries"
[ "$(readlink -f "$(command -v uxdroid)")" = /usr/lib/uxdroid/uxdroid ] || fail "uxdroid is not /usr/lib/uxdroid/uxdroid"
for file in /usr/lib/uxdroid/*; do
    if ldd "$file" | grep -q "not found"; then ldd "$file"; fail "$file has unresolved libraries"; fi
done

useradd -m tester
as_tester() { runuser -u tester -- env HOME=/home/tester "$@"; }

step "uxdroid with no command prints its usage"
status=0
usage=$(as_tester uxdroid 2>&1) || status=$?
[ "$status" -eq 2 ] || fail "uxdroid exited $status, expected 2"
echo "$usage" | grep -q "^usage: uxdroid inspect" || fail "no usage: $usage"

step "search, install, list and remove an app from F-Droid"
as_tester uxdroid search markor --limit 5 | tee /tmp/search.txt
grep -q "net.gsantner.markor .*fdroid" /tmp/search.txt || fail "markor not found"
as_tester uxdroid install net.gsantner.markor
as_tester uxdroid list | tee /tmp/list.txt
grep -q net.gsantner.markor /tmp/list.txt || fail "markor is not listed after install"
as_tester uxdroid remove net.gsantner.markor
as_tester uxdroid list | grep -q net.gsantner.markor && fail "markor is still listed after remove"

step "the tarball runs from where it is unpacked"
mkdir /tmp/unpacked
tar -C /tmp/unpacked -xzf "$TAR"
chown -R tester /tmp/unpacked
as_tester /tmp/unpacked/uxdroid-*/uxdroid search markor --limit 1 | grep -q net.gsantner.markor \
    || fail "the unpacked uxdroid cannot search"

step "remove the package"
apt-get remove -y -qq uxdroid > /dev/null
[ ! -e /usr/lib/uxdroid ] && [ ! -e /usr/bin/uxdroid ] || fail "files are left after remove"
echo "OK: $(. /etc/os-release && echo "$PRETTY_NAME") $ARCH"
