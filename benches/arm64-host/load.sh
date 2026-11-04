#!/bin/sh
# Times `uxdroid load` of the translation spike's arm64 library with one call, as the median of
# RUNS runs, for the aarch64 host build. On an x86_64 machine it runs under QEMU user, so the
# numbers are EMULATED: they show the build works and what QEMU costs, never arm64 hardware speed.
# With UXDROID pointing at an x86_64 build in the `translation` service and UXDROID_TEST_RUNNER
# empty, the same library runs translated by Dynarmic instead. Not measured: arm64 hardware.
#
#   docker compose run --rm dev-arm64 cargo build --release -p uxdroid-cli
#   docker compose run --rm dev-arm64 sh /work/benches/arm64-host/load.sh
set -eu
RUNS=${RUNS:-5}
UXDROID=${UXDROID:-/cache/target-arm64-host/aarch64-unknown-linux-gnu/release/uxdroid}
RUNNER=${UXDROID_TEST_RUNNER:-}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
"$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android24-clang++" \
    /work/crates/bridge/tests/fixtures/arm64/spike.cpp -shared -fPIC -O2 -static-libstdc++ -llog \
    -o "$work/libspike.so"
for call in spike_qsort spike_dl; do
    times=""
    for _ in $(seq "$RUNS"); do
        start=$(date +%s%N)
        $RUNNER "$UXDROID" load "$work/libspike.so" --call "$call" > /dev/null
        times="$times $(( ($(date +%s%N) - start) / 1000 ))"
    done
    median=$(echo "$times" | tr ' ' '\n' | sed '/^$/d' | sort -n | sed -n "$(( (RUNS + 1) / 2 ))p")
    echo "$call: median $(( median / 1000 )).$(( median % 1000 / 100 )) ms of $RUNS runs${RUNNER:+ (emulated by $RUNNER)}"
done
