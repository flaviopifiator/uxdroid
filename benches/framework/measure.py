"""Measures a program from process start to a marker line on its output, repeatedly.

Each run starts a fresh process, reads its stdout and stderr, and at the first line matching
--marker stops the process (SIGSTOP), samples /proc/<pid>/smaps_rollup, and then either kills it
or, with --until-exit, lets it finish. Peak RSS is VmHWM from /proc/<pid>/status at that
moment, so it covers the process up to the marker, not to its exit. With --cold, the page cache of every file under each --evict path is dropped before
each run, so the files are read from disk again. Each run waits --settle seconds first: here, a
killed process's mappings were seen to outlive wait() briefly, which made the next run's PSS read
about half and kept its pages from being evicted.

usage: measure.py --label NAME --marker REGEX [--expect REGEX]... [--until-exit] [--runs N]
                  [--cold] [--evict PATH]... [--timeout S] [--cwd DIR] -- command...

Prints one summary line, and one JSON object per run to --json when given.
"""

import argparse
import json
import os
import re
import select
import signal
import subprocess
import sys
import time


def evict(paths):
    """Drops cached pages of every regular file under paths. Pages another process maps stay."""
    os.sync()
    for top in paths:
        if os.path.isdir(top):
            found = (os.path.join(root, name) for root, _, files in os.walk(top) for name in files)
        else:
            found = [top]
        for path in found:
            if os.path.islink(path) or not os.path.isfile(path):
                continue
            try:
                fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
            except OSError:
                continue
            try:
                os.posix_fadvise(fd, 0, 0, os.POSIX_FADV_DONTNEED)
            finally:
                os.close(fd)


def smaps(pid):
    values = {}
    try:
        with open(f"/proc/{pid}/smaps_rollup") as rollup:
            for line in rollup:
                parts = line.split()
                if len(parts) == 3 and parts[2] == "kB":
                    values[parts[0].rstrip(":")] = int(parts[1])
    except OSError:
        pass
    return values


def peak_rss(pid):
    try:
        with open(f"/proc/{pid}/status") as status:
            for line in status:
                if line.startswith("VmHWM:"):
                    return int(line.split()[1])
    except OSError:
        pass
    return None


def run_once(args, marker, expects):
    start = time.perf_counter()
    process = subprocess.Popen(args.command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                               stdin=subprocess.DEVNULL, cwd=args.cwd)
    fd = process.stdout.fileno()
    output, buffer = [], b""
    marker_ms, sample, peak, eof = None, {}, None, False
    deadline = start + args.timeout
    while not eof:
        remaining = deadline - time.perf_counter()
        if remaining <= 0:
            break
        ready, _, _ = select.select([fd], [], [], remaining)
        if not ready:
            continue
        chunk = os.read(fd, 65536)
        if not chunk:
            eof = True
        if chunk:
            lines = (buffer + chunk).split(b"\n")
            buffer = lines.pop()
        else:
            lines, buffer = ([buffer] if buffer else []), b""
        for raw in lines:
            line = raw.decode("utf-8", "replace")
            output.append(line)
            if marker_ms is None and marker.search(line):
                marker_ms = (time.perf_counter() - start) * 1e3
                os.kill(process.pid, signal.SIGSTOP)
                sample = smaps(process.pid)
                peak = peak_rss(process.pid)
                if args.until_exit:
                    os.kill(process.pid, signal.SIGCONT)
                else:
                    os.kill(process.pid, signal.SIGKILL)
                    eof = True
                    break
    if not eof:
        os.kill(process.pid, signal.SIGKILL)
    _, status = os.waitpid(process.pid, 0)
    process.returncode = status
    end_ms = (time.perf_counter() - start) * 1e3
    process.stdout.close()
    text = "\n".join(output)
    missing = [pattern.pattern for pattern in expects if not pattern.search(text)]
    problem = None
    if marker_ms is None:
        problem = f"no line matched {marker.pattern!r} within {args.timeout} s"
    elif missing:
        problem = f"output lacks {missing}"
    elif args.until_exit and not (os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0):
        problem = f"exit status {status}"
    return {
        "marker_ms": marker_ms,
        "exit_ms": end_ms if args.until_exit else None,
        "peak_rss_kib": peak,
        "rss_kib": sample.get("Rss"),
        "pss_kib": sample.get("Pss"),
        "private_dirty_kib": sample.get("Private_Dirty"),
        "pss_anon_kib": sample.get("Pss_Anon"),
        "pss_file_kib": sample.get("Pss_File"),
        "problem": problem,
        "tail": output[-15:] if problem else None,
    }


def quantile(ordered, q):
    """Linear interpolation between closest ranks; the container's Python has no statistics module."""
    position = (len(ordered) - 1) * q
    low = int(position)
    high = min(low + 1, len(ordered) - 1)
    return ordered[low] + (ordered[high] - ordered[low]) * (position - low)


def describe(values, unit, scale=1.0, digits=1):
    values = sorted(v * scale for v in values if v is not None)
    if not values:
        return "n/a"
    return (f"median {quantile(values, 0.5):.{digits}f} {unit} "
            f"(IQR {quantile(values, 0.25):.{digits}f}-{quantile(values, 0.75):.{digits}f}, "
            f"min {values[0]:.{digits}f}, max {values[-1]:.{digits}f})")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--label", required=True)
    parser.add_argument("--marker", required=True)
    parser.add_argument("--expect", action="append", default=[])
    parser.add_argument("--until-exit", action="store_true")
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--cold", action="store_true")
    parser.add_argument("--evict", action="append", default=[])
    parser.add_argument("--timeout", type=float, default=60)
    parser.add_argument("--settle", type=float, default=0.3)
    parser.add_argument("--cwd")
    parser.add_argument("--json")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command:
        parser.error("no command")
    marker = re.compile(args.marker)
    expects = [re.compile(pattern, re.M) for pattern in args.expect]

    for _ in range(args.warmup):
        result = run_once(args, marker, expects)
        if result["problem"]:
            print(f"FAIL {args.label} warm-up: {result['problem']}\n  " + "\n  ".join(result["tail"]))
            sys.exit(1)
    results = []
    for _ in range(args.runs):
        time.sleep(args.settle)
        if args.cold:
            evict(args.evict)
        result = run_once(args, marker, expects)
        if result["problem"]:
            print(f"FAIL {args.label}: {result['problem']}\n  " + "\n  ".join(result["tail"]))
            sys.exit(1)
        results.append(result)
    if args.json:
        with open(args.json, "a") as out:
            for result in results:
                out.write(json.dumps({"label": args.label, "cold": args.cold, **result}) + "\n")

    mib = 1 / 1024
    mode = "cold" if args.cold else "warm"
    print(f"{args.label} [{mode}, {args.runs} runs, every output verified]")
    print(f"  to marker:     {describe([r['marker_ms'] for r in results], 'ms')}")
    if args.until_exit:
        print(f"  to exit:       {describe([r['exit_ms'] for r in results], 'ms')}")
    for name, key in (("peak RSS", "peak_rss_kib"), ("PSS", "pss_kib"), ("private dirty", "private_dirty_kib")):
        values = [r[key] for r in results]
        if any(value is None for value in values):
            # The process exited before it could be sampled.
            print(f"  {name + ':':<15}not sampled")
        else:
            print(f"  {name + ':':<15}{describe(values, 'MiB', mib)}")


if __name__ == "__main__":
    main()
