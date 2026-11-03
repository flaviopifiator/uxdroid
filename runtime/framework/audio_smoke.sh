#!/bin/bash
# Runs the audio sample APK, which plays a tone and records through Java's AudioTrack and AudioRecord,
# then through OpenSL ES and AAudio from its native library, built here by the NDK. The file backend
# stands in for the sound card: every sample a stream hands the device lands in a file, and every
# recording reads a file this script writes. Each stream's samples are checked exactly against the
# formulas the app used, and each recording's sum against the file. When a PulseAudio server is
# installed, a second run plays the same APK into a null sink of a private server and records what the
# server received. Runs in the dev service; the APK is ours, built by build_system.sh. With
# KEEP=<directory>, the logs and audio files are copied there.
set -u -o pipefail
WORK=$(mktemp -d)
fail() { echo "FAIL: $*"; tail -"${TAIL:-25}" "$WORK/out" 2>/dev/null | cut -c1-220; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-nativebridge --manifest-path /work/Cargo.toml || fail "build"
TARGET="$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')/release"
source /work/runtime/art/run.sh
export ANDROID_DATA="$WORK/data"
mkdir -p "$ANDROID_DATA/dalvik-cache/x86_64" "$WORK/audio"

LIBS="$ANDROID_DATA/data/com.example.audio/lib"
mkdir -p "$LIBS"
"$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/x86_64-linux-android30-clang" -shared -fPIC -O2 \
    -Wall -Werror /work/runtime/framework/samples/audio/jni/audiosample.c -o "$LIBS/libaudiosample.so" \
    -lOpenSLES -laaudio || fail "the NDK could not build the sample"

# What the microphone hears: integers that 16-bit recordings hold exactly.
PYTHONDONTWRITEBYTECODE=1 python3 - "$WORK/audio/input.f32" <<'EOF'
import struct, sys
values = [((k * 37) % 2001 - 1000) / 32768 for k in range(48000)]
open(sys.argv[1], "wb").write(struct.pack(f"<{len(values)}f", *values))
EOF

source /work/runtime/art/test_lib.sh
start_compositor

export LD_LIBRARY_PATH="$RUNTIME/lib64:$TARGET"
CP="$RUNTIME/framework/framework.jar:$RUNTIME/framework/uxdroid-system.jar"
run_app() {
    "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}" -XX:NativeBridge=libuxdroid_nativebridge.so -Xforce-nb-testing \
        -cp "$CP" dev.uxdroid.system.Launcher "$RUNTIME" "$TARGET/libuxdroid_system.so" \
        "$RUNTIME/samples/framework/audio.apk" >"$WORK/out" 2>&1 &
    app=$!
}
trap 'kill "${app:-}" "$compositor" "${server:-}" 2>/dev/null; wait 2>/dev/null; [ -n "${KEEP:-}" ] && cp -r "$WORK/out" "$WORK/audio" "$KEEP"; rm -rf "$WORK"' EXIT

wait_for() {
    for _ in $(seq 600); do
        grep -q "$1" "$WORK/out" && return
        kill -0 "$app" 2>/dev/null || fail "the app exited before \"$1\""
        pause 0.05
    done
    fail "no \"$1\""
}
expect() { grep -q -E "$1" "$WORK/out" || fail "$2"; }

UXDROID_AUDIO="file:$WORK/audio" run_app
wait_for "audio sample done"
kill "$app" 2>/dev/null
wait "$app" 2>/dev/null
grep -E "^(audiotrack|audiorecord|opensl|aaudio): |uxdroid: |Exception" "$WORK/out" | cut -c1-300

expect "^audiotrack: rate 48000, burst 480, min buffer [0-9]+, wrote 4800, played 4800, underruns 0, sample rate 48000$" \
    "AudioTrack did not play the whole tone"
expect "^opensl: played 4410 frames in 10 buffers, " "the OpenSL ES player did not play its buffers"
expect "^aaudio: rate 32000, burst 320, callbacks [0-9]+, written [0-9]+, xruns 0, " "AAudio did not play"

PYTHONDONTWRITEBYTECODE=1 python3 - "$WORK/audio" "$WORK/out" <<'EOF' || fail "the samples"
import glob, os, re, struct, sys

directory, log = sys.argv[1:]
lines = open(log).read().splitlines()

def output(rate, channels):
    names = glob.glob(os.path.join(directory, f"output-*-{rate}hz-{channels}ch.f32"))
    assert len(names) == 1, f"{rate} Hz {channels} ch: {names}"
    data = open(names[0], "rb").read()
    samples = list(struct.unpack(f"<{len(data) // 4}f", data))
    # The device plays silence until the app's first frames arrive.
    start = next(i for i, s in enumerate(samples) if s != 0.0)
    assert start % channels == 0, f"misaligned start {start}"
    return samples[start:]

def saw(i, hz, rate):
    return ((i * hz * 65536 // rate) & 0xFFFF) - 32768

def check(name, got, expected):
    for index, (a, b) in enumerate(zip(got, expected)):
        assert a == b, f"{name}: sample {index} is {a}, expected {b}"
    assert len(got) >= len(expected), f"{name}: {len(got)} samples, expected {len(expected)}"
    print(f"{name}: {len(expected)} samples exact")

track = output(48000, 1)
check("AudioTrack 16-bit mono", [round(s * 32768) for s in track], [saw(i, 440, 48000) for i in range(4800)])
sl = output(44100, 2)
check("OpenSL ES 16-bit stereo", [round(s * 32768) for s in sl],
      [v for i in range(4410) for v in (saw(i, 330, 44100), saw(i, 660, 44100))])
aa = output(32000, 2)
check("AAudio float stereo", aa, [v for i in range(3200) for v in ((i % 64) / 64 - 0.5, -((i % 64) / 64 - 0.5))])

heard = [(k * 37) % 2001 - 1000 for k in range(1600)]
expected = f"sum {sum(heard)}, first {heard[0]} {heard[1]} {heard[2]}"
for api in ("audiorecord", "opensl", "aaudio"):
    line = next(l for l in lines if l.startswith(api + ": "))
    assert expected in line, f"{api} recorded {line!r}, expected {expected!r}"
    print(f"{api} recorded the microphone file exactly: {expected}")

aaudio = next(l for l in lines if l.startswith("aaudio: "))
first, period = map(int, re.search(r"first callback (\d+) us, period (\d+) us", aaudio).groups())
assert period in range(9000, 11001), f"AAudio callbacks every {period} us, expected 10 ms"
assert first < 20000, f"the first AAudio callback came {first} us after start"
print(f"AAudio callbacks every {period} us, the first {first} us after start")
EOF

if command -v pulseaudio >/dev/null; then
    # A private server whose only sink is a null sink, and a recording of what reaches it.
    export PULSE_RUNTIME_PATH="$WORK/pulse" PULSE_STATE_PATH="$WORK/pulse" PULSE_SERVER="unix:$WORK/pulse/native"
    mkdir -p "$WORK/pulse"
    pulseaudio -n --daemonize=no --exit-idle-time=-1 --disallow-exit --use-pid-file=no --system=no \
        -L "module-native-protocol-unix socket=$WORK/pulse/native auth-anonymous=1" \
        -L "module-null-sink sink_name=speaker rate=48000 channels=1 format=float32le" \
        -L "module-null-source source_name=microphone rate=16000 channels=1" >"$WORK/pulse.log" 2>&1 &
    server=$!
    for _ in $(seq 100); do [ -S "$WORK/pulse/native" ] && break; pause 0.05; done
    [ -S "$WORK/pulse/native" ] || { cat "$WORK/pulse.log"; fail "no PulseAudio server"; }
    pactl set-default-sink speaker && pactl set-default-source microphone || fail "pactl"
    # A null device idles in 2 s blocks and only wakes for a new stream at the end of one, which a
    # hardware device does not do; a silent stream on each keeps their blocks short.
    pacat --latency-msec=10 --format=s16le --rate=48000 --channels=1 /dev/zero &
    keep_sink=$!
    parec -d microphone --latency-msec=10 --raw >/dev/null &
    keep_source=$!
    parec -d speaker.monitor --format=float32le --rate=48000 --channels=1 --raw >"$WORK/heard.f32" &
    recorder=$!
    UXDROID_TEST_PULSE="$PULSE_SERVER" cargo test -q --release -p uxdroid-audio --test pulse \
        --manifest-path /work/Cargo.toml -- --nocapture --test-threads=1 >"$WORK/pulse-tests" 2>&1 \
        || { cat "$WORK/pulse-tests"; fail "the PulseAudio stream tests"; }
    grep -E "Hz:" "$WORK/pulse-tests"
    UXDROID_AUDIO=pulse run_app
    wait_for "audio sample done"
    kill "$app" "$recorder" "$keep_sink" "$keep_source" 2>/dev/null
    wait "$app" "$recorder" "$keep_sink" "$keep_source" 2>/dev/null
    expect "^audiotrack: rate 48000, burst 480, min buffer [0-9]+, wrote 4800, played 4800, underruns 0, sample rate 48000$" \
        "AudioTrack did not play through PulseAudio"
    # The first two callbacks fill the server's queue at once; the server's clock paces the rest.
    expect "^aaudio: rate 32000, burst 320, callbacks [0-9]+, written [0-9]+, xruns 0, first callback [0-9]+ us, period ([7-9][0-9]{3}|10[0-9]{3}) us;" \
        "AAudio's callbacks did not keep the server's pace"
    grep -E "^(audiotrack|opensl|aaudio): " "$WORK/out" | cut -c1-200
    PYTHONDONTWRITEBYTECODE=1 python3 - "$WORK/heard.f32" <<'EOF' || fail "the server's samples"
import struct, sys
data = open(sys.argv[1], "rb").read()
samples = struct.unpack(f"<{len(data) // 4}f", data)
saw = [(((i * 440 * 65536 // 48000) & 0xFFFF) - 32768) / 32768 for i in range(4800)]
# The AudioTrack stream matches the sink's rate and format, so the server passes it through unchanged
# when nothing else plays at the same time; look for the tone's first 480 samples in a row.
needle = saw[:480]
for start in range(len(samples) - 480):
    if samples[start] == needle[0] and list(samples[start:start + 480]) == needle:
        print(f"PulseAudio received the AudioTrack tone, sample-exact, {start} samples into the recording")
        break
else:
    loud = sum(1 for s in samples if abs(s) > 0.01)
    raise SystemExit(f"the AudioTrack tone is not in the {len(samples)} samples the server received ({loud} loud)")
EOF
else
    echo "skipped: no PulseAudio server installed"
fi
echo "ok: AudioTrack, AudioRecord, OpenSL ES and AAudio played and recorded the exact samples"
