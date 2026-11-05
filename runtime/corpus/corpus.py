"""The F-Droid catalog runner's logic, one file so `python3 -I` can run it (isolated mode does not put
the script's directory on the import path) and stdlib only, since the containers' Python is
python3-minimal.

Commands (see docs/corpus.md):
  select <index.json> [--sample <n>] [--seed <s>]   apps to run, as TSV, from uxdroid's verified index
  pending <apps.tsv> <report.tsv>                    the rows of apps.tsv the report does not have yet
  download <apps.tsv> <dir> [--repo <url>]           fetch each APK, checked against the index's size
                                                     and SHA-256 (host only; never opens the APK)
  flat <file.png>                                    yes, no, or - when the image cannot be read
  target <ui.json>                                   "x y node" to tap, or "x y center" of the top
                                                     window when no node is clickable; exits 1 if none
  changed <ui-before> <ui-after> <shot-a> <shot-b> <shot-after>   yes or no
  blocker <install.log> <run.log> <installed> <alive> <frame> <flat>   what stopped the app, or -
  rank <report.tsv>                                  outcome counts and blockers by apps stopped
"""

import hashlib
import json
import os
import random
import re
import struct
import sys
import zlib

SDK = 36
ABIS = ("x86_64", "arm64-v8a")
REPO = "https://f-droid.org/repo"
COLUMNS = ["package", "version", "version_name", "abi", "file", "sha256", "size"]
REPORT_COLUMNS = ["package", "version", "abi", "installed", "first_frame_ms", "alive", "flat",
                  "tap_changed", "blocker"]
FLAT_SHARE = 0.98

# The same rules as uxdroid's store (crates/store): a package is letters, digits, `.` and `_`; a file
# is a plain path inside the repository.
PACKAGE = re.compile(r"^[A-Za-z0-9_]+(\.[A-Za-z0-9_]+)*$")
FILE_SEGMENT = re.compile(r"^[A-Za-z0-9._+~-]+$")
SHA256 = re.compile(r"^[0-9a-f]{64}$")


# Selection


def valid_file_name(name):
    if not isinstance(name, str) or not name.startswith("/"):
        return False
    segments = name[1:].split("/")
    return all(FILE_SEGMENT.match(s) and s not in (".", "..") for s in segments)


def select_releases(index, sdk=SDK, dropped=None):
    """The newest stable release of each package whose native code, if any, includes x86_64 or
    arm64-v8a and whose SDK range holds `sdk`, sorted by package. `dropped` counts the reasons
    for leaving an app out."""
    def drop(reason):
        if dropped is not None:
            dropped[reason] = dropped.get(reason, 0) + 1

    chosen = []
    for package, entry in sorted((index.get("packages") or {}).items()):
        if not PACKAGE.match(package) or not isinstance(entry, dict):
            drop("invalid entry")
            continue
        stable = []
        for version in (entry.get("versions") or {}).values():
            if not isinstance(version, dict) or version.get("releaseChannels"):
                continue
            manifest = version.get("manifest") or {}
            code = manifest.get("versionCode")
            if isinstance(code, int) and not isinstance(code, bool):
                stable.append((code, version, manifest))
        if not stable:
            drop("no stable release")
            continue
        code, version, manifest = max(stable, key=lambda item: item[0])
        min_sdk = (manifest.get("usesSdk") or {}).get("minSdkVersion", 1)
        max_sdk = manifest.get("maxSdkVersion")
        if not isinstance(min_sdk, int) or min_sdk > sdk or (isinstance(max_sdk, int) and max_sdk < sdk):
            drop(f"SDK range does not hold {sdk}")
            continue
        native = manifest.get("nativecode") or []
        abi = next((a for a in ABIS if a in native), None) or ("none" if not native else None)
        if abi is None:
            drop("native code for other ABIs only")
            continue
        file = version.get("file") or {}
        name, sha256, size = file.get("name"), file.get("sha256"), file.get("size")
        if (not valid_file_name(name) or not isinstance(sha256, str) or not SHA256.match(sha256)
                or not isinstance(size, int) or size <= 0):
            drop("invalid file")
            continue
        version_name = manifest.get("versionName")
        chosen.append({
            "package": package, "version_code": code,
            "version_name": clean(version_name) if isinstance(version_name, str) else "-",
            "abi": abi, "file": name, "sha256": sha256, "size": size,
        })
    return chosen


def sample(releases, n, seed):
    picked = random.Random(seed).sample(releases, min(n, len(releases)))
    return sorted(picked, key=lambda r: r["package"])


def clean(text, limit=300):
    """One TSV field: no tabs, newlines or other control characters."""
    return re.sub(r"[\x00-\x1f\x7f]+", " ", text).strip()[:limit] or "-"


def read_tsv(path):
    with open(path, encoding="utf-8", errors="replace") as f:
        lines = [line.rstrip("\n").split("\t") for line in f if line.strip()]
    if not lines:
        return []
    header = lines[0]
    return [dict(zip(header, row)) for row in lines[1:]]


def read_report(path):
    return read_tsv(path) if os.path.exists(path) else []


# The flat-screenshot check


def decode_png(data, max_pixels=50_000_000):
    """(width, height, rows) of an 8-bit, non-interlaced PNG, each row the raw pixel bytes."""
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("not a PNG")
    pos, header, idat, ended = 8, None, [], False
    while pos + 8 <= len(data):
        length, tag = struct.unpack(">I4s", data[pos:pos + 8])
        body = data[pos + 8:pos + 8 + length]
        if len(body) != length or pos + 12 + length > len(data):
            raise ValueError("truncated chunk")
        crc = struct.unpack(">I", data[pos + 8 + length:pos + 12 + length])[0]
        if zlib.crc32(tag + body) != crc:
            raise ValueError("bad chunk checksum")
        if tag == b"IHDR":
            header = struct.unpack(">IIBBBBB", body) if length == 13 else None
        elif tag == b"IDAT":
            idat.append(body)
        elif tag == b"IEND":
            ended = True
            break
        pos += 12 + length
    if header is None or not idat or not ended:
        raise ValueError("incomplete PNG")
    width, height, depth, color, _, _, interlace = header
    bpp = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}.get(color)
    if depth != 8 or bpp is None or interlace != 0:
        raise ValueError("unsupported PNG format")
    if width == 0 or height == 0 or width * height > max_pixels:
        raise ValueError("unsupported PNG size")
    stride = width * bpp
    expected = (stride + 1) * height
    raw = zlib.decompressobj().decompress(b"".join(idat), expected + 1)
    if len(raw) != expected:
        raise ValueError("wrong image data size")
    rows, prior = [], bytearray(stride)
    for y in range(height):
        kind = raw[y * (stride + 1)]
        row = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        if kind == 1:
            for i in range(bpp, stride):
                row[i] = (row[i] + row[i - bpp]) & 0xFF
        elif kind == 2:
            for i in range(stride):
                row[i] = (row[i] + prior[i]) & 0xFF
        elif kind == 3:
            for i in range(stride):
                left = row[i - bpp] if i >= bpp else 0
                row[i] = (row[i] + ((left + prior[i]) >> 1)) & 0xFF
        elif kind == 4:
            for i in range(stride):
                a = row[i - bpp] if i >= bpp else 0
                b = prior[i]
                c = prior[i - bpp] if i >= bpp else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                predictor = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                row[i] = (row[i] + predictor) & 0xFF
        elif kind != 0:
            raise ValueError("unknown PNG filter")
        rows.append(bytes(row))
        prior = row
    return width, height, rows


def is_flat(data, share=FLAT_SHARE):
    """True when one color covers at least `share` of the pixels."""
    width, height, rows = decode_png(data)
    bpp = len(rows[0]) // width
    counts = {}
    for row in rows:
        for i in range(0, len(row), bpp):
            pixel = row[i:i + bpp]
            counts[pixel] = counts.get(pixel, 0) + 1
    return max(counts.values()) >= share * width * height


# The tap check


def roots(tree):
    """The root nodes of a `ui` answer, bottom window first: dicts with bounds, found anywhere."""
    if isinstance(tree, dict):
        # A window carries bounds of its own beside its root node.
        if isinstance(tree.get("root"), dict):
            return roots(tree["root"])
        if "bounds" in tree:
            return [tree]
        return [root for value in tree.values() for root in roots(value)]
    if isinstance(tree, list):
        return [root for value in tree for root in roots(value)]
    return []


def usable(node):
    bounds = node.get("bounds")
    if not (isinstance(bounds, list) and len(bounds) == 4 and all(isinstance(v, int) for v in bounds)):
        return False
    left, top, right, bottom = bounds
    return node.get("visible") is True and node.get("clickable") is True and right > left and bottom > top


def tap_target(tree):
    """The center of the first visible clickable node with no such node under it, in the topmost
    window that has one; None when there is none."""
    for root in reversed(roots(tree)):
        found = []

        def visit(node):
            below = [visit(c) for c in node.get("children") or [] if isinstance(c, dict)]
            if not any(below) and usable(node):
                found.append(node)
                return True
            return any(below)

        visit(root)
        if found:
            left, top, right, bottom = found[0]["bounds"]
            return ((left + right) // 2, (top + bottom) // 2)
    return None


def screen_center(tree):
    """The center of the topmost window, for screens that describe no clickable node, such as a
    game's SurfaceView."""
    for root in reversed(roots(tree)):
        bounds = root.get("bounds")
        if isinstance(bounds, list) and len(bounds) == 4 and all(isinstance(v, int) for v in bounds):
            left, top, right, bottom = bounds
            if right > left and bottom > top:
                return ((left + right) // 2, (top + bottom) // 2)
    return None


def ui_digest(tree):
    """The tree without what changes between snapshots of the same screen: snapshot and node
    numbers."""
    def strip(value):
        if isinstance(value, dict):
            return {k: strip(v) for k, v in value.items() if k not in ("snapshot", "id")}
        if isinstance(value, list):
            return [strip(v) for v in value]
        return value

    return json.dumps(strip(tree), sort_keys=True)


def tap_changed(ui_before, ui_after, shots_before, shot_after):
    """yes when the tree changed after the tap, or the screen did and was still before it."""
    if ui_before is not None and ui_after is not None and ui_digest(ui_before) != ui_digest(ui_after):
        return "yes"
    first, second = shots_before
    if first is not None and first == second and shot_after is not None and shot_after != second:
        return "yes"
    return "no"


# Blockers

LOGCAT_PREFIX = re.compile(r"^\d\d-\d\d \d\d:\d\d:\d\d\.\d+\s+\d+\s+\d+\s+[VDIWEF]\s+[^:]*:\s*"
                           r"|^[VDIWEF] [\w.$-]+\s*: ")
JAVA_CRASH = re.compile(r"^(Exception in thread \"[^\"]*\" |FATAL EXCEPTION)")
# uxdroid's own refusals and the loader's, in the words its messages use: they explain the signal
# that usually follows them.
REFUSAL = re.compile(r"not audited|no shim|unresolved symbols|which was not found")
SIGNAL = re.compile(r"Fatal signal \d+|Abort message:")
OTHER = re.compile(r"cannot launch|Unable to start activity|System\.exit called|^error: ")
CAUSE = re.compile(r"^Caused by: ")


def strip_prefix(line):
    return LOGCAT_PREFIX.sub("", line.rstrip("\n"))


def java_crash(lines):
    for start, line in enumerate(lines):
        if not JAVA_CRASH.match(line):
            continue
        exception = line[len(JAVA_CRASH.match(line).group(0)):].strip() if line.startswith("Exception") else ""
        cause = None
        for follow in lines[start + 1:]:
            if CAUSE.match(follow):
                cause = follow.strip()
            elif follow.startswith("Process:") or follow.lstrip().startswith(("at ", "...", "Suppressed:")):
                pass
            elif follow[:1].isspace():
                pass
            elif not exception and not follow.startswith(":"):
                exception = follow.strip()
            else:
                break
        return cause or exception or None
    return None


def find_blocker(install_log, run_log, outcome):
    """The line that best names what stopped the app, or - when it passed. `outcome` has installed,
    alive, frame (bool) and flat (bool, or None when no screenshot was read)."""
    if not outcome["installed"]:
        lines = [line.strip() for line in install_log.splitlines() if line.strip()]
        errors = [line for line in lines if line.startswith("error")]
        return clean((errors or lines or ["install failed (nothing logged)"])[-1])
    if outcome["alive"] and outcome["frame"] and outcome["flat"] is not True:
        return "-"
    lines = [strip_prefix(line) for line in run_log.splitlines()]
    found = java_crash(lines)
    for pattern in (REFUSAL, SIGNAL, OTHER, CAUSE):
        if found:
            break
        matches = [line for line in lines if pattern.search(line) and "logcat" not in line]
        # A line of its own says it better than the same words inside a stack trace.
        found = next((line.strip() for line in matches if not line[:1].isspace()),
                     matches[0].strip() if matches else None)
    if found:
        return clean(found)
    if not outcome["alive"]:
        return "exited (no error logged)"
    if not outcome["frame"]:
        return "no first frame (no error logged)"
    return "flat screen (no error logged)"


def normalize(line):
    """A blocker's signature: the same failure in different apps and runs reads the same."""
    text = strip_prefix(line)
    text = re.sub(r"^Exception in thread \"[^\"]*\" ", "", text)
    text = re.sub(r"ComponentInfo\{[^}]*\}", "ComponentInfo{<component>}", text)
    text = re.sub(r"\btid \d+ \([^)]*\)", "tid N (<thread>)", text)
    text = re.sub(r"\bpid \d+ \([^)]*\)", "pid N (<process>)", text)
    text = re.sub(r"0x[0-9a-fA-F]+", "<addr>", text)
    text = re.sub(r"\b(?=[0-9a-f]*\d)[0-9a-f]{8,}\b", "<addr>", text)
    text = re.sub(r"(?<![\w.$/<])/[^\s:'\",;)\]]+", "<path>", text)
    text = re.sub(r"(?<![\w.])\d+(?![\w.])", "N", text)
    return re.sub(r"\s+", " ", text).strip()[:200]


def rank(rows):
    """(signature, packages) for each blocker, the one that stops most apps first."""
    groups = {}
    for row in rows:
        blocker = row.get("blocker", "-")
        if blocker and blocker != "-":
            groups.setdefault(normalize(blocker), []).append(row.get("package", "?"))
    return sorted(((sig, sorted(pkgs)) for sig, pkgs in groups.items()), key=lambda g: (-len(g[1]), g[0]))


def format_ranking(rows, examples=3):
    def count(test):
        return sum(1 for row in rows if test(row))

    lines = [
        f"apps: {len(rows)}",
        f"installed: {count(lambda r: r.get('installed') == 'yes')}",
        f"first frame: {count(lambda r: r.get('first_frame_ms', '-') != '-')}",
        f"alive: {count(lambda r: r.get('alive') == 'yes')}",
        f"not flat: {count(lambda r: r.get('flat') == 'no')}",
        f"tap changed the UI: {count(lambda r: r.get('tap_changed') == 'yes')}",
        f"blocked: {count(lambda r: r.get('blocker', '-') != '-')}",
        "",
        "blockers, by how many apps each stops:",
    ]
    for signature, packages in rank(rows):
        shown = ", ".join(packages[:examples]) + (", ..." if len(packages) > examples else "")
        lines.append(f"{signature}: {len(packages)} app{'s' if len(packages) != 1 else ''} ({shown})")
    return "\n".join(lines) + "\n"


# Downloads (host only: the containers' Python has no urllib)


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        while chunk := f.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def local_name(release):
    return f"{release['package']}_{release['version']}.apk"


def download(release, directory, repo=REPO, attempts=2):
    """Fetch one APK into `directory`, keeping it only when its size and SHA-256 are the index's."""
    import urllib.request

    size, sha256 = int(release["size"]), release["sha256"]
    path = os.path.join(directory, local_name(release))
    if os.path.exists(path) and os.path.getsize(path) == size and sha256_of(path) == sha256:
        return "cached"
    if not valid_file_name(release["file"]):
        raise ValueError("unsafe file name")
    error = None
    for _ in range(attempts):
        part = path + ".part"
        try:
            request = urllib.request.Request(repo + release["file"],
                                             headers={"User-Agent": "uxdroid-corpus"})
            digest, total = hashlib.sha256(), 0
            with urllib.request.urlopen(request, timeout=60) as response, open(part, "wb") as out:
                while chunk := response.read(1 << 20):
                    total += len(chunk)
                    if total > size:
                        raise ValueError("larger than the index says")
                    digest.update(chunk)
                    out.write(chunk)
            if total != size:
                raise ValueError(f"{total} bytes, the index says {size}")
            if digest.hexdigest() != sha256:
                raise ValueError("SHA-256 differs from the index")
            os.replace(part, path)
            return "downloaded"
        except Exception as failure:  # noqa: BLE001 - reported per app, the batch goes on
            error = failure
            if os.path.exists(part):
                os.remove(part)
    raise RuntimeError(clean(str(error)))


# Commands


def option(args, name, default=None):
    if name in args:
        i = args.index(name)
        value = args[i + 1]
        del args[i:i + 2]
        return value
    return default


def write_rows(rows, columns, out=sys.stdout):
    out.write("\t".join(columns) + "\n")
    for row in rows:
        out.write("\t".join(str(row[c]) for c in columns) + "\n")


def read_text(path):
    try:
        with open(path, encoding="utf-8", errors="replace") as f:
            return f.read()
    except OSError:
        return ""


def read_json(path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def read_bytes(path):
    try:
        with open(path, "rb") as f:
            return f.read()
    except OSError:
        return None


def main(args):
    command, args = (args[0], args[1:]) if args else ("", [])
    if command == "select":
        n, seed = option(args, "--sample"), option(args, "--seed", "0")
        with open(args[0], encoding="utf-8") as f:
            index = json.load(f)
        dropped = {}
        releases = select_releases(index, dropped=dropped)
        print(f"{len(releases)} apps selected of {len(index.get('packages') or {})}", file=sys.stderr)
        for reason, number in sorted(dropped.items()):
            print(f"  left out, {reason}: {number}", file=sys.stderr)
        if n is not None:
            releases = sample(releases, int(n), int(seed))
        rows = [dict(r, version=r["version_code"]) for r in releases]
        write_rows(rows, COLUMNS)
    elif command == "pending":
        done = {row.get("package") for row in read_report(args[1])}
        rows = [row for row in read_tsv(args[0]) if row.get("package") not in done]
        write_rows(rows, COLUMNS)
    elif command == "download":
        repo = option(args, "--repo", REPO)
        for release in read_tsv(args[0]):
            try:
                result = download(release, args[1], repo)
                print(f"{release['package']}\tok\t{result}", flush=True)
            except Exception as failure:  # noqa: BLE001
                print(f"{release['package']}\tfail\t{clean(str(failure))}", flush=True)
    elif command == "flat":
        data = read_bytes(args[0])
        try:
            print("yes" if data is not None and is_flat(data) else ("-" if data is None else "no"))
        except ValueError:
            print("-")
    elif command == "target":
        tree = read_json(args[0])
        target, kind = tap_target(tree), "node"
        if target is None:
            target, kind = screen_center(tree), "center"
        if target is None:
            return 1
        print(f"{target[0]} {target[1]} {kind}")
    elif command == "changed":
        print(tap_changed(read_json(args[0]), read_json(args[1]),
                          (read_bytes(args[2]), read_bytes(args[3])), read_bytes(args[4])))
    elif command == "blocker":
        yes = [value == "yes" for value in args[2:5]]
        flat = {"yes": True, "no": False}.get(args[5])
        outcome = {"installed": yes[0], "alive": yes[1], "frame": yes[2], "flat": flat}
        print(find_blocker(read_text(args[0]), read_text(args[1]), outcome))
    elif command == "rank":
        sys.stdout.write(format_ranking(read_report(args[0])))
    else:
        sys.stderr.write(__doc__)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
