#!/bin/bash
# Measures what starting an app through its systemd socket unit costs against a direct start, on the
# framework sample APK, installed, from disk: the time from `uxdroid app <package> start` until the
# started app answers it through its control socket (it answers once its main thread is first idle,
# after the first frame). Direct: the command finds no socket and starts the launcher itself. Socket
# unit: runtime/framework/socket_unit.py plays the user manager with the units uxdroid service enable
# wrote, and the command's connection makes it start `uxdroid service run`. The two alternate, over
# two data directories installed the same way, so both see the same machine load.
#
#   docker compose run --rm -T dev bash /work/benches/framework/service.sh
#
# RUNS sets the measured starts of each kind (default 20), after 3 unmeasured ones each.
set -u -o pipefail
RUNS="${RUNS:-20}"
WORK=$(mktemp -d -p "$([ -w /var/tmp ] && echo /var/tmp || echo /tmp)")
TARGET="${CARGO_TARGET_DIR:-/cache/target}/release"
fail() { echo "FAIL: $*"; tail -5 "$WORK/out" 2>/dev/null; exit 1; }

cargo build -q --release -p uxdroid-system -p uxdroid-cli --manifest-path /work/Cargo.toml || fail "build"
source /work/runtime/art/run.sh
source /work/runtime/art/test_lib.sh
start_compositor
trap 'kill "$MANAGER" "$compositor" 2>/dev/null; wait 2>/dev/null; rm -rf "$WORK"' EXIT
PACKAGE=com.example.framework
U="$TARGET/uxdroid"
for data in "$WORK/direct" "$HOME/.local/share/uxdroid/profiles/unit"; do
    "$U" install "$RUNTIME/samples/framework/app.apk" --data "$data" >"$WORK/out" 2>&1 || fail "install"
done
"$U" service enable "$PACKAGE" --profile unit --runtime "$RUNTIME" >"$WORK/out" 2>&1 || fail "enable"
UNIT="$HOME/.config/systemd/user/uxdroid-$PACKAGE@unit"
python3 -I /work/runtime/framework/socket_unit.py "$UNIT.socket" "$UNIT.service" "$WORK/manager.log" \
    >"$WORK/app.log" 2>&1 &
MANAGER=$!
for _ in $(seq 100); do grep -q "^listening" "$WORK/manager.log" 2>/dev/null && break; pause 0.05; done

python3 -I - "$U" "$PACKAGE" "$WORK/direct" "$RUNTIME" "$RUNS" "$XDG_RUNTIME_DIR/uxdroid/$PACKAGE.log" \
    "$WORK/app.log" <<'EOF'
import subprocess, sys, time

uxdroid, package, direct_data, runtime, runs, direct_log, unit_log = sys.argv[1:]
# The activity's own onResume, before the first vsync, and the first frame, which waits for a tick of
# the 60 Hz vsync clock and so moves in steps of up to 16.7 ms.
MARKS = {"resumed": "activity resumed com.example.framework.MainActivity", "first frame": "first frame posted"}
kinds = {
    "direct start": (["--data-dir", direct_data, "--runtime", runtime], [], direct_log),
    "through the socket unit": (["--profile", "unit"], ["--profile", "unit"], unit_log),
}


def seen(log):
    try:
        with open(log, encoding="utf-8", errors="replace") as text:
            written = text.read()
    except FileNotFoundError:
        written = ""
    return {name: written.count(mark) for name, mark in MARKS.items()}


times = {kind: {name: [] for name in [*MARKS, "start answered"]} for kind in kinds}
for run in range(3 + int(runs)):
    for kind, (options, stop, log) in kinds.items():
        # A direct start writes its log afresh; the manager's log grows.
        if log == direct_log:
            subprocess.run(["rm", "-f", log], check=True)
        before = seen(log)
        began = time.monotonic()
        command = subprocess.Popen([uxdroid, "app", package, "start", *options],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        at = {}
        while len(at) < len(MARKS):
            now = seen(log)
            for name in MARKS:
                if name not in at and now[name] > before[name]:
                    at[name] = (time.monotonic() - began) * 1000
            if time.monotonic() - began > 30:
                sys.exit(f"FAIL: {kind}: no first frame")
            time.sleep(0.001)
        _, err = command.communicate()
        at["start answered"] = (time.monotonic() - began) * 1000
        if command.returncode != 0:
            sys.exit(f"FAIL: {kind}: {err.strip()}")
        stopped = subprocess.run([uxdroid, "app", package, "stop", *stop], capture_output=True, text=True)
        if stopped.returncode != 0:
            sys.exit(f"FAIL: stop after {kind}: {stopped.stderr.strip()}")
        if run >= 3:
            for name, value in at.items():
                times[kind][name].append(value)


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


def summary(values):
    return f"{median(values):.1f} ms ({min(values):.1f}-{max(values):.1f})"


for kind, measured in times.items():
    print(f"{kind}, {len(measured['resumed'])} runs: "
          + ", ".join(f"{name} {summary(values)}" for name, values in measured.items()))
EOF
