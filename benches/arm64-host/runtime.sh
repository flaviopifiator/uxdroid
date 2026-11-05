#!/bin/bash
# Starts each package's ART on the Hello and Compute samples, RUNS times each, and prints the median
# wall time from process start to exit and the median peak resident memory. ARCH=arm64 measures the
# arm64 package; on an x86_64 machine it runs under QEMU user, so its numbers are EMULATED: they show
# the package works and what QEMU costs, never arm64 hardware speed, and its memory includes QEMU's
# own (translation cache and the emulator). Peak memory is sampled every 2 ms, so a peak shorter than
# that can be missed. Not measured: arm64 hardware.
#
#   docker compose run --rm sandbox bash /work/benches/arm64-host/runtime.sh
#   docker compose run --rm -e ARCH=arm64 sandbox-arm64 bash /work/benches/arm64-host/runtime.sh
set -eu -o pipefail
RUNS=${RUNS:-5}
source /work/runtime/art/run.sh
ART=("${RUNNER[@]}" "$RUNTIME/bin/dalvikvm64" "${ART_FLAGS[@]}")
for program in Hello Compute; do
    PYTHONDONTWRITEBYTECODE=1 python3 - "$RUNS" "$program" "${ART[@]}" -cp "$RUNTIME/samples/$program.jar" \
        "$program" <<'PY'
import os, sys, time
median = lambda values: sorted(values)[len(values) // 2]  # python3-minimal has no statistics
runs, program, command = int(sys.argv[1]), sys.argv[2], sys.argv[3:]
times, peaks = [], []
for _ in range(runs):
    start = time.monotonic()
    pid = os.fork()
    if pid == 0:
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, 1)
        os.dup2(devnull, 2)
        os.execvp(command[0], command)
    # Peak RSS as the kernel keeps it (VmHWM), read until the process exits: python3-minimal has no
    # resource module for wait4's usage.
    peak = 0
    while True:
        try:
            with open(f"/proc/{pid}/status") as f:
                for line in f:
                    if line.startswith("VmHWM:"):
                        peak = max(peak, int(line.split()[1]))
        except OSError:
            pass
        done, status = os.waitpid(pid, os.WNOHANG)
        if done:
            break
        time.sleep(0.002)
    times.append((time.monotonic() - start) * 1000)
    peaks.append(peak / 1024)
    if status != 0:
        sys.exit(f"{program} exited with status {status}")
print(f"{os.environ.get('ARCH', 'x86_64')} {program}: {median(times):.0f} ms, "
      f"peak RSS {median(peaks):.1f} MiB (median of {runs})")
PY
done
