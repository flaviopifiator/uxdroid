"""Removes Java, app and API stub modules from Android.bp files of the partial Android 16 tree.

The tree builds C and C++ only (libandroid_runtime and ICU for the arm64 host), and lacks the SDK
prebuilts and modules its Java depends on, which Soong cannot analyze. Every other top-level block,
such as cc modules, filegroups, genrules, aconfig declarations, packages, licenses and variable
assignments, is kept as written.

Usage: strip_java.py <Android.bp in> <Android.bp out>
"""

import re
import sys

JAVA = re.compile(
    r"^(java_\w*|android_\w*|droidstubs|droiddoc\w*|\w*sdk_library\w*|stubs_defaults|combined_apis"
    r"|platform_compat_config|prebuilt_apis|\w*classpath_fragment|apex\w*|override_\w*"
    r"|runtime_resource_overlay|non_updatable_\w*|\w*_droidstubs|ravenwood_\w*)$"
)


def blocks(text):
    """Splits a blueprint file into top-level blocks: (module type or None, text)."""
    out, i, n = [], 0, len(text)
    while i < n:
        m = re.compile(r"(\w+)\s*\{").match(text, i)
        if m and (i == 0 or text[i - 1] == "\n"):
            depth, j, quote = 0, m.end() - 1, None
            while j < n:
                c = text[j]
                if quote:
                    if c == "\\":
                        j += 1
                    elif c == quote:
                        quote = None
                elif c in "\"'":
                    quote = c
                elif c == "/" and text.startswith("//", j):
                    j = text.find("\n", j)
                    if j < 0:
                        j = n
                    continue
                elif c == "{":
                    depth += 1
                elif c == "}":
                    depth -= 1
                    if depth == 0:
                        j += 1
                        break
                j += 1
            out.append((m.group(1), text[i:j]))
            i = j
        else:
            j = text.find("\n", i)
            j = n if j < 0 else j + 1
            out.append((None, text[i:j]))
            i = j
    return out


def strip(text):
    return "".join(body for kind, body in blocks(text) if kind is None or not JAVA.match(kind))


if __name__ == "__main__":
    source, target = sys.argv[1:]
    with open(source) as f:
        stripped = strip(f.read())
    with open(target, "w") as f:
        f.write(stripped)
