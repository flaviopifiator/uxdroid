"""Samples, once a second, the CPU time and memory of every app that run_apps.sh starts.

usage: python3 -I sample_cpu.py <out dir>     (runs until killed)

run_apps.sh starts each app as `setsid uxdroid service run <package> ...`, which then names its
process after the package, as Android does; the app and everything it starts share one session whose
ID is that process's PID. Each second this sums user and system
time (of live processes, plus the children the leader reaped) and the resident memory of the
session, and appends `<seconds since first seen>\t<cpu seconds>\t<rss MiB>` to <out>/<package>.tsv.
Standard library only: the sandbox's Python is python3-minimal.
"""

import os
import re
import sys
import time

# A package name with at least one dot, as corpus.py accepts them; no path, so not a helper program.
PACKAGE = re.compile(r"^[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)+$")
TICK = os.sysconf("SC_CLK_TCK")
PAGE = os.sysconf("SC_PAGE_SIZE")


def stat(pid):
    """The fields of /proc/<pid>/stat after the command name, or None."""
    try:
        with open(f"/proc/{pid}/stat", encoding="ascii", errors="replace") as f:
            text = f.read()
    except OSError:
        return None
    return text[text.rfind(")") + 2:].split()


def leaders():
    """{pid: package} of the session leaders that are apps: `uxdroid service run <package>` until
    the app renames its process, as Android does, to its package name."""
    found = {}
    for name in os.listdir("/proc"):
        if not name.isdigit() or name == "1":
            continue
        fields = stat(name)
        if not fields or len(fields) < 4 or fields[3] != name:
            continue
        try:
            with open(f"/proc/{name}/cmdline", "rb") as f:
                args = [a.strip().decode(errors="replace") for a in f.read().split(b"\0")]
        except OSError:
            continue
        if len(args) > 3 and args[0].endswith("uxdroid") and args[1:3] == ["service", "run"]:
            found[int(name)] = args[3]
        elif args and PACKAGE.match(args[0]):
            found[int(name)] = args[0]
    return found


def main(out):
    os.makedirs(out, exist_ok=True)
    first = {}
    while True:
        heads = leaders()
        totals = {pid: [0, 0] for pid in heads}
        for name in os.listdir("/proc"):
            if not name.isdigit():
                continue
            fields = stat(name)
            if not fields or len(fields) < 22:
                continue
            # Field numbers of proc(5) minus 3: session 6, utime 14, stime 15, cutime 16, cstime 17,
            # rss 24.
            session = int(fields[3])
            if session in totals:
                ticks = int(fields[11]) + int(fields[12])
                if int(name) == session:
                    ticks += int(fields[13]) + int(fields[14])
                totals[session][0] += ticks
                totals[session][1] += int(fields[21])
        now = time.monotonic()
        for pid, (ticks, pages) in totals.items():
            package = heads[pid]
            start = first.setdefault(pid, now)
            with open(os.path.join(out, package + ".tsv"), "a", encoding="ascii") as f:
                f.write(f"{now - start:.1f}\t{ticks / TICK:.2f}\t{pages * PAGE / 1048576:.1f}\n")
        time.sleep(1)


if __name__ == "__main__":
    main(sys.argv[1])
