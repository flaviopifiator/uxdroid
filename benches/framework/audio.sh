#!/bin/bash
# Measures what audio costs: startup and memory of the framework sample APK, which plays nothing, to
# its first frame (the row of run.sh, to compare with milestone 4), and of the audio sample APK
# through its AudioTrack tone; then the latency and pacing of streams on a private PulseAudio server.
# Runs in the dev service, from disk, cold and warm:
#
#   docker compose run --rm -T dev bash /work/benches/framework/audio.sh
#
# SOURCE names the tree to build (default /work), so an older checkout can be measured the same way
# with ONLY=framework; RUNS sets the measured runs per row (default 30). The runtime must have been
# built by build_system.sh at the path it runs from, or ART runs without its boot image.
set -u -o pipefail
WORK=$(mktemp -d)
# Removed however the script ends, a failure before the full trap below included.
trap 'rm -rf "$WORK"' EXIT
RUNS="${RUNS:-30}"
SOURCE="${SOURCE:-/work}"
MEASURE=(python3 -I /work/benches/framework/measure.py)
fail() { echo "FAIL: $*"; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-nativebridge --manifest-path "$SOURCE/Cargo.toml" || fail "build"
BIN="$(cargo metadata -q --format-version 1 --no-deps --manifest-path "$SOURCE/Cargo.toml" \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
LIBRARY="$BIN/libuxdroid_system.so"
source /work/runtime/art/run.sh
# ART finds the native bridge here.
export LD_LIBRARY_PATH="$RUNTIME/lib64:$BIN"
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64"
source /work/runtime/art/test_lib.sh
trap 'kill "$compositor" "${server:-}" "${keep_sink:-}" "${keep_source:-}" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT
start_compositor
[ "$(findmnt -n -o FSTYPE -T "$WORK")" != tmpfs ] || fail "$WORK is on tmpfs; the numbers would not be from disk"
[ -e "$RUNTIME/framework/x86_64/boot.art" ] || fail "no boot image in $RUNTIME"

EVICT=(--evict "$RUNTIME" --evict "$WORK" --evict "$BIN/libuxdroid_system.so" --evict "$BIN/libuxdroid_nativebridge.so")
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
VM=("$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}")
ls -l "$LIBRARY" "$BIN/libuxdroid_nativebridge.so" | awk '{print "size:", $5, $9}'

if [ "${ONLY:-}" != framework ]; then
    LIBS="$ANDROID_DATA/data/com.example.audio/lib"
    mkdir -p "$LIBS"
    "$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang" -shared -fPIC -O2 \
        /work/runtime/framework/samples/audio/jni/audiosample.c -o "$LIBS/libaudiosample.so" -lOpenSLES -laaudio \
        || fail "the NDK could not build the audio sample"
fi

for mode in --cold ""; do
    "${MEASURE[@]}" --label "Framework sample APK to first frame" --runs "$RUNS" $mode "${EVICT[@]}" \
        --marker "first frame posted" --expect "^activity resumed com\.example\.framework\.MainActivity$" -- \
        "${VM[@]}" -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$LIBRARY" \
        "$RUNTIME/samples/framework/app.apk" || exit 1
    [ "${ONLY:-}" = framework ] && continue
    # Start, then a 100 ms tone written to an AudioTrack and drained, with the null device: the cost of
    # the audio path itself, without a sound server.
    UXDROID_AUDIO=null "${MEASURE[@]}" --label "Audio sample APK through its AudioTrack tone" --runs "$RUNS" $mode \
        "${EVICT[@]}" --marker "^audiotrack: " --expect "played 4800, underruns 0" -- \
        "${VM[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing -cp "$CP" \
        dev.uxdroid.system.Launcher "$RUNTIME" "$LIBRARY" "$RUNTIME/samples/framework/audio.apk" || exit 1
done
[ "${ONLY:-}" = framework ] && exit 0

# Latency and pacing on a real server: a private PulseAudio with null devices, kept awake by silent
# streams, since null devices idle in 2 s blocks that hardware does not have.
export PULSE_RUNTIME_PATH="$WORK/pulse" PULSE_STATE_PATH="$WORK/pulse" PULSE_SERVER="unix:$WORK/pulse/native"
mkdir -p "$WORK/pulse"
pulseaudio -n --daemonize=no --exit-idle-time=-1 --use-pid-file=no \
    -L "module-native-protocol-unix socket=$WORK/pulse/native auth-anonymous=1" \
    -L "module-null-sink sink_name=speaker rate=48000 channels=2" -L "module-null-source source_name=microphone" \
    >"$WORK/pulse.log" 2>&1 &
server=$!
for _ in $(seq 100); do [ -S "$WORK/pulse/native" ] && break; pause 0.05; done
[ -S "$WORK/pulse/native" ] || fail "PulseAudio did not start: $(tail -n 3 "$WORK/pulse.log")"
pacat --latency-msec=10 --rate=48000 --channels=2 /dev/zero &
keep_sink=$!
parec -d microphone --latency-msec=10 --raw >/dev/null &
keep_source=$!
for run in 1 2 3; do
    UXDROID_TEST_PULSE="$PULSE_SERVER" cargo test -q --release -p uxdroid-audio --test pulse \
        --manifest-path "$SOURCE/Cargo.toml" -- --nocapture --test-threads=1 >"$WORK/pulse-test" 2>&1 \
        || fail "the PulseAudio stream tests failed: $(grep -E "FAIL|panicked" "$WORK/pulse-test" | head -n 3)"
    grep -E "Hz:" "$WORK/pulse-test" || fail "the PulseAudio stream tests measured nothing"
done
LIBS="$ANDROID_DATA/data/com.example.audio/lib"
for run in 1 2 3; do
    UXDROID_AUDIO=pulse "${VM[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing -cp "$CP" \
        dev.uxdroid.system.Launcher "$RUNTIME" "$LIBRARY" "$RUNTIME/samples/framework/audio.apk" >"$WORK/app" 2>&1 &
    app=$!
    for _ in $(seq 400); do grep -q "^audio sample done" "$WORK/app" && break; kill -0 "$app" 2>/dev/null || break; pause 0.05; done
    kill "$app" 2>/dev/null
    wait "$app" 2>/dev/null
    grep -q "^audio sample done" "$WORK/app" || fail "the audio sample did not finish: $(tail -n 3 "$WORK/app")"
    grep -E "^aaudio: " "$WORK/app" | sed 's/; read.*//' | grep . || fail "the audio sample reported no AAudio stream"
done
echo "storage: $WORK on $(findmnt -n -o FSTYPE -T "$WORK"), $RUNTIME on $(findmnt -n -o FSTYPE -T "$RUNTIME")"
