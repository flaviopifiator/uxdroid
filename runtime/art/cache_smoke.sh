#!/bin/bash
# Proves the app cache never runs stale code: each APK runs its own contents. Runs in dev.
set -u -o pipefail
WORK=$(mktemp -d)
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
RUNTIME=/runtime/art
BIN=/cache/target/release
fail() { echo "FAIL: $*"; [ -f "$WORK/out" ] && tail -8 "$WORK/out" | cut -c1-200; exit 1; }
cargo build -q --release -p uxdroid-cli -p uxdroid-nativebridge -p uxdroid-platform -p uxdroid-testkit \
    --manifest-path /work/Cargo.toml || fail "build"
source /work/runtime/art/test_lib.sh
start_compositor

library() {
    "$TOOLCHAIN/bin/x86_64-linux-android26-clang++" -shared -fPIC -O2 -static-libstdc++ -DLABEL="\"$1\"" \
        /work/runtime/art/samples/first/first_native.cpp -o "$WORK/lib$1.so" || fail "NDK build"
}
apk() { # name, library or -, optional extra entry
    "$BIN/make-first-apk" "$RUNTIME/samples/first/classes.dex" "$2" "$WORK/$1.apk" ${3:+"$3"} || fail "APK $1"
}
run() { timeout 60 "$BIN/uxdroid" run "$@" --exit-after-first-frame >"$WORK/out" 2>&1; }
expect_label() {
    run "$WORK/$1.apk" || fail "$1 exited with an error"
    grep -q "label: hello from $2" "$WORK/out" || fail "$1 did not run its own code, expected $2"
}

library one
library two
apk one "$WORK/libone.so"
apk two "$WORK/libtwo.so"
apk none -
apk bad "$WORK/libone.so" 'lib/x86_64/a\b.so'

expect_label one one
expect_label two two
touch -d '2001-01-01' "$WORK/one.apk"
expect_label one one
echo "ok: the named APK runs, whatever its timestamp"

expect_label two two
run "$WORK/none.apk" && fail "an APK without the library still found it"
grep -q "UnsatisfiedLinkError" "$WORK/out" || fail "expected a missing library error"
echo "ok: libraries of another version do not survive"

for attempt in 1 2; do
    run "$WORK/bad.apk" && fail "a failed extraction started on attempt $attempt"
    grep -q "cannot extract native library" "$WORK/out" || fail "expected the extraction error on attempt $attempt"
done
echo "ok: a failed install stays failed"

chmod 444 "$WORK/one.apk"
rm -rf "$HOME/.cache/uxdroid"
expect_label one one
expect_label two two
echo "ok: a read-only APK does not block the next install"

rm -rf "$HOME/.cache/uxdroid"
pids=()
for index in 1 2 3 4 5 6; do
    timeout 90 "$BIN/uxdroid" run "$WORK/one.apk" --exit-after-first-frame >"$WORK/parallel$index" 2>&1 &
    pids+=($!)
done
for pid in "${pids[@]}"; do wait "$pid" || fail "a parallel first run failed"; done
echo "ok: parallel first runs all work"

mkdir "$WORK/clean"
cd "$WORK/clean"
run "$WORK/one.apk" || fail "run from a clean directory"
[ -z "$(ls -A "$WORK/clean")" ] || fail "a run wrote to the current directory: $(ls -A "$WORK/clean")"
echo "ok: the current directory stays untouched"
