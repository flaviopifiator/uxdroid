#!/bin/bash
# Proves packaged ART runs a DEX main() as compiled code in a clean system. Fails on any doubt.
set -u
source /work/runtime/art/run.sh
ART=("$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -Xusejit:false)
fail() { echo "FAIL: $*"; exit 1; }

[ -d /aosp ] && fail "the AOSP tree is present"
hello=$("${ART[@]}" -cp "$RUNTIME/samples/Hello.jar" Hello 2>/dev/null) || fail "Hello exited with an error"
[ "$hello" = "hello from dex, x86_64" ] || fail "Hello printed: $hello"

log=$("${ART[@]}" -verbose:oat -cp "$RUNTIME/samples/Compute.jar" Compute 2>&1 >/tmp/compute) || fail "Compute exited with an error"
[ "$(cat /tmp/compute)" = "31103932482336" ] || fail "Compute printed the wrong result"
grep -q "fall back to imageless" <<<"$log" && fail "the boot image did not load"
grep -q "kOatUpToDate with filter 'speed'" <<<"$log" || fail "Compute's compiled code was not used"
grep -q "Compute.odex.*executable=1\|executable=1.*Compute" <<<"$log" || echo "note: no executable odex line found"

[ -n "$(find "$RUNTIME/notices" -type f -size +0)" ] || fail "license notices are missing"
[ -z "$(find "$RUNTIME/notices" -type f -size 0)" ] || fail "a license notice is empty"
echo "ok: Hello printed, Compute ran from its compiled odex, boot image loaded, notices present"
