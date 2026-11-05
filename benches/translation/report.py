#!/usr/bin/env python3
"""Summarizes run.sh's results as a Markdown table: medians over the runs of each configuration."""
import json
import sys


def median(values):
    ordered = sorted(values)
    middle = len(ordered) // 2
    return ordered[middle] if len(ordered) % 2 else (ordered[middle - 1] + ordered[middle]) / 2


rows = {}
for line in open(sys.argv[1]):
    entry = json.loads(line)
    rows.setdefault(entry["label"], []).append(entry["result"])
mib = sys.argv[2]

reference = None
native_warm = None
print(f"| Configuration ({mib} MiB) | SHA-256 ms | deflate ms | Kernel ms | % of native speed | First-run extra ms | Peak RSS MiB | Private dirty MiB |")
print("|---|---|---|---|---|---|---|---|")
for label, results in rows.items():
    outputs = {(r["digest"], r["compressed"]) for r in results}
    if len(outputs) != 1:
        sys.exit(f"{label}: results differ between runs: {outputs}")
    if reference is None:
        reference = outputs
    elif outputs != reference:
        sys.exit(f"{label}: result {outputs} differs from {reference}")
    sha = median([r["sha_ms"][1] for r in results])
    deflate = median([r["deflate_ms"][1] for r in results])
    warm = sha + deflate
    first = median([r["sha_ms"][0] + r["deflate_ms"][0] for r in results])
    if native_warm is None:
        native_warm = warm
    speed = 100 * native_warm / warm
    rss = median([r["peak_rss_kib"] for r in results]) / 1024
    dirty = median([r["private_dirty_kib"] for r in results]) / 1024
    print(f"| {label} | {sha:.0f} | {deflate:.0f} | {warm:.0f} | {speed:.1f}% | {first - warm:.1f} | {rss:.1f} | {dirty:.1f} |")
digest, compressed = next(iter(reference))
print(f"\nEvery configuration produced SHA-256 {digest} and {compressed} compressed bytes; {len(next(iter(rows.values())))} runs each.")
