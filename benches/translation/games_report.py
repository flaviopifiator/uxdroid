#!/usr/bin/env python3
"""Summarizes games.sh's results as a Markdown table: medians over the runs of each configuration,
the second run of each phase, and its speed as a percentage of native."""
import json
import sys

PHASES = ["particles", "matrices", "host_calls", "tick"]


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


rows = {}
for line in open(sys.argv[1]):
    entry = json.loads(line)
    rows.setdefault(entry["label"], []).append(entry["result"])

reference = {}
native = {}
print("| Configuration | " + " | ".join(f"{p} ms (% native)" for p in PHASES) + " | First-run extra ms |")
print("|---|" + "---|" * (len(PHASES) + 1))
for label, results in rows.items():
    cells = []
    extra = 0.0
    for phase in PHASES:
        runs = [r[phase] for r in results if r[phase] is not None]
        if not runs:
            cells.append("-")
            continue
        sums = {r["sum"] for r in runs}
        if len(sums) != 1:
            sys.exit(f"{label} {phase}: checksums differ between runs: {sums}")
        if reference.setdefault(phase, sums) != sums:
            sys.exit(f"{label} {phase}: checksum {sums} differs from {reference[phase]}")
        warm = median([r["ms"][1] for r in runs])
        extra += median([r["ms"][0] for r in runs]) - warm
        native.setdefault(phase, warm)
        cells.append(f"{warm:.0f} ({100 * native[phase] / warm:.1f}%)")
    print(f"| {label} | " + " | ".join(cells) + f" | {extra:.1f} |")
print(f"\nEvery configuration that ran a phase produced the same checksum; {len(next(iter(rows.values())))} runs each.")
