"""The popular corpus's planning and report, standard library only.

Commands:
  plan <manifest.tsv> <fetched.tsv> <inspect.tsv> <out> [<,id,...,>]
      writes <out>/plan-native.tsv and plan-translated.tsv (the columns run_apps.sh reads) and
      <out>/map.tsv, which ties each run back to its manifest row
  collect <out>
      gathers the sandbox's rows, logs, screenshots and CPU samples from <out>/stage into
      <out>/report.tsv (manifest columns, run_apps.sh's columns, CPU and memory),
      <out>/corpus-report.tsv (run_apps.sh's columns only, for corpus.py rank) and logs/, shots/, cpu/
  table <report.tsv>
      the report as a Markdown table, with translated and native side by side for apps run both ways
"""

import os
import shutil
import sys

PLAN_COLUMNS = ["package", "version", "version_name", "abi", "file", "sha256", "size"]
RUN_COLUMNS = ["package", "version", "abi", "installed", "first_frame_ms", "alive", "flat",
               "tap_changed", "blocker", "framework"]
PASSES = ("native", "translated")
CPU_WINDOW = 30.0


def read_tsv(path, comments=False):
    with open(path, encoding="utf-8") as f:
        lines = [line.rstrip("\n") for line in f if not (comments and line.startswith("#"))]
    lines = [line for line in lines if line]
    if not lines:
        return []
    header = lines[0].split("\t")
    return [dict(zip(header, line.split("\t"))) for line in lines[1:]]


def write_tsv(path, columns, rows):
    with open(path, "w", encoding="utf-8") as f:
        f.write("\t".join(columns) + "\n")
        for row in rows:
            f.write("\t".join(str(row.get(c, "-")) for c in columns) + "\n")


def abis_of(text):
    """The ABIs of `uxdroid inspect`'s `abis:` line; it says `none` for an APK without native code."""
    abis = [part.split(" (")[0].strip() for part in text.split(",") if part.strip()]
    return [] if abis == ["none"] else abis


def plan(manifest_path, fetched_path, inspect_path, out, only=""):
    manifest = {(r["id"], r["variant"]): r for r in read_tsv(manifest_path, comments=True)}
    inspected = {}
    with open(inspect_path, encoding="utf-8") as f:
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) == 5:
                inspected[parts[0]] = parts[1:]
    runs = {p: [] for p in PASSES}
    mapping, skipped = [], []
    for fetched in read_tsv(fetched_path):
        key = (fetched["id"], fetched["variant"])
        if only and f",{fetched['id']}," not in only:
            continue
        entry = manifest.get(key)
        info = inspected.get(fetched["file"])
        if entry is None:
            continue
        if not info or not info[0]:
            skipped.append(f"{fetched['id']} ({fetched['variant']}): uxdroid inspect could not read it")
            continue
        package, version, version_name, abis = info[0], info[1], info[2], abis_of(info[3])
        variant = fetched["variant"]
        if variant == "arm64":
            pass_, abi = "translated", "arm64-v8a"
        elif variant == "x86_64":
            pass_, abi = "native", "x86_64"
        else:
            pass_ = "native"
            abi = next((a for a in ("x86_64", "arm64-v8a") if a in abis), "none" if not abis else "")
        if not abi or (abi != "none" and abi not in abis):
            skipped.append(f"{fetched['id']} ({variant}): native code only for {', '.join(abis)}")
            continue
        runs[pass_].append({"package": package, "version": version, "version_name": version_name,
                            "abi": abi, "file": fetched["file"], "sha256": fetched["sha256"],
                            "size": fetched["size"]})
        mapping.append({"pass": pass_, "package": package, "id": entry["id"], "kind": entry["kind"],
                        "source": entry["source"], "variant": variant, "abis": ",".join(abis) or "none",
                        "version_name": version_name, "file": fetched["file"],
                        "size": fetched["size"], "sha256": fetched["sha256"]})
    for pass_ in PASSES:
        write_tsv(os.path.join(out, f"plan-{pass_}.tsv"), PLAN_COLUMNS, runs[pass_])
    write_tsv(os.path.join(out, "map.tsv"), ["pass", "package", "id", "kind", "source", "variant",
                                            "abis", "version_name", "file", "size", "sha256"], mapping)
    with open(os.path.join(out, "skipped.txt"), "w", encoding="utf-8") as f:
        f.write("".join(s + "\n" for s in skipped))
    for s in skipped:
        print("skipped " + s, file=sys.stderr)


def cpu_summary(path):
    """(CPU seconds in the first CPU_WINDOW seconds, CPU seconds over the whole run, peak RSS MiB)."""
    if not os.path.exists(path):
        return "-", "-", "-"
    samples = []
    with open(path, encoding="ascii") as f:
        for line in f:
            parts = line.split()
            if len(parts) == 3:
                samples.append(tuple(float(p) for p in parts))
    if not samples:
        return "-", "-", "-"
    window = [s for s in samples if s[0] >= CPU_WINDOW]
    first = f"{window[0][1]:.1f}" if window else "-"
    return first, f"{samples[-1][1]:.1f}", f"{max(s[2] for s in samples):.0f}"


def copy_tree(src, dst):
    if os.path.isdir(src):
        os.makedirs(dst, exist_ok=True)
        for name in os.listdir(src):
            shutil.copyfile(os.path.join(src, name), os.path.join(dst, name))


def collect(out):
    mapping = {(r["pass"], r["package"]): r for r in read_tsv(os.path.join(out, "map.tsv"))}
    report, corpus = [], []
    for pass_ in PASSES:
        stage = os.path.join(out, "stage", pass_)
        rows = {}
        if os.path.isdir(stage):
            for shard in sorted(os.listdir(stage)):
                base = os.path.join(stage, shard)
                if not os.path.isdir(base):
                    continue
                rows_dir = os.path.join(base, "rows")
                if os.path.isdir(rows_dir):
                    for name in os.listdir(rows_dir):
                        with open(os.path.join(rows_dir, name), encoding="utf-8") as f:
                            for line in f:
                                parts = line.rstrip("\n").split("\t")
                                if len(parts) == len(RUN_COLUMNS):
                                    rows[parts[0]] = dict(zip(RUN_COLUMNS, parts))
                for kind in ("logs", "shots", "cpu"):
                    copy_tree(os.path.join(base, kind), os.path.join(out, kind, pass_))
        for (p, package), meta in sorted(mapping.items(), key=lambda kv: (kv[0][0], kv[1]["id"])):
            if p != pass_:
                continue
            row = rows.get(package) or {"package": package, "abi": "-", "installed": "-",
                                        "blocker": "no result: the runner stopped before this app"}
            cpu30, cpu_all, rss = cpu_summary(os.path.join(out, "cpu", pass_, package + ".tsv"))
            merged = dict(meta)
            merged.update(row)
            merged.update({"cpu_30s": cpu30, "cpu_run_s": cpu_all, "rss_peak_mib": rss})
            report.append(merged)
            ranked = dict(row)
            if pass_ == "translated":
                ranked["package"] = package + "@arm64"
            corpus.append(ranked)
    columns = ["id", "kind", "source", "variant", "pass", "version_name", "abis"] + RUN_COLUMNS + \
        ["cpu_30s", "cpu_run_s", "rss_peak_mib", "size", "sha256", "file"]
    write_tsv(os.path.join(out, "report.tsv"), columns, report)
    write_tsv(os.path.join(out, "corpus-report.tsv"), RUN_COLUMNS, corpus)


def table(path):
    rows = read_tsv(path)
    print("| App | Kind | Source | ABI run | First frame (ms) | Alive | Flat | Tap changed | CPU s, first 30 s"
          " | Peak RSS MiB | Framework | Blocker |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|")
    for r in rows:
        abi = r["abi"] + (" (translated)" if r["abi"] == "arm64-v8a" else "")
        blocker = r["blocker"].replace("|", "\\|")
        if len(blocker) > 110:
            blocker = blocker[:107] + "..."
        print(f"| {r['id']} {r['version_name']} | {r['kind']} | {r['source']} | {abi} | {r['first_frame_ms']}"
              f" | {r['alive']} | {r['flat']} | {r['tap_changed']} | {r['cpu_30s']} | {r['rss_peak_mib']}"
              f" | {r['framework']} | `{blocker}` |")
    both = {}
    for r in rows:
        if r["variant"] in ("x86_64", "arm64"):
            both.setdefault(r["id"], {})[r["variant"]] = r
    pairs = [(k, v) for k, v in sorted(both.items()) if len(v) == 2]
    if pairs:
        print()
        print("| App | First frame native (ms) | First frame translated (ms) | Ratio | CPU s first 30 s native"
              " | translated | Peak RSS MiB native | translated |")
        print("|---|---|---|---|---|---|---|---|")
        for app, v in pairs:
            n, t = v["x86_64"], v["arm64"]
            try:
                ratio = f"{float(t['first_frame_ms']) / float(n['first_frame_ms']):.2f}x"
            except (ValueError, ZeroDivisionError):
                ratio = "-"
            print(f"| {app} | {n['first_frame_ms']} | {t['first_frame_ms']} | {ratio} | {n['cpu_30s']}"
                  f" | {t['cpu_30s']} | {n['rss_peak_mib']} | {t['rss_peak_mib']} |")


def main(args):
    if args[:1] == ["plan"] and len(args) in (5, 6):
        plan(*args[1:])
    elif args[:1] == ["collect"] and len(args) == 2:
        collect(args[1])
    elif args[:1] == ["table"] and len(args) == 2:
        table(args[1])
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
