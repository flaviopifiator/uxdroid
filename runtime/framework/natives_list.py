"""Lists the framework's native methods, for the default bodies bound at startup.

Usage: natives_list.py <javap -p -s output> <boot class names> <out>

Each output line is `<class in JNI form> <method> <descriptor>`. Classes ART's boot class path also
has are left out: the boot copy is the one apps get, and its natives are ART's own.
"""

import re
import sys

# Modifiers are optional: a package-private class starts with `class` itself.
HEADER = re.compile(r"^(?:(?:public|protected|private|abstract|static|final|sealed|non-sealed|strictfp)\s+)*"
                    r"(?:class|interface|enum|@interface)\s+([\w.$]+)")
NATIVE = re.compile(r"\snative\s.*?(\w+)\(")
DESCRIPTOR = re.compile(r"^\s+descriptor:\s+(\S+)")


def natives(javap_lines, boot):
    current, method, found = None, None, []
    for line in javap_lines:
        header = HEADER.match(line)
        if header:
            current = header.group(1)
            method = None
            continue
        native = NATIVE.search(line) if current and line.startswith("  ") and "(" in line else None
        if native:
            method = native.group(1)
            continue
        descriptor = DESCRIPTOR.match(line)
        if descriptor and method:
            if current not in boot:
                found.append((current.replace(".", "/"), method, descriptor.group(1)))
            method = None
    return found


def main():
    with open(sys.argv[1]) as f:
        lines = f.read().splitlines()
    with open(sys.argv[2]) as f:
        boot = set(f.read().split())
    found = natives(lines, boot)
    with open(sys.argv[3], "w") as out:
        out.writelines("{} {} {}\n".format(*entry) for entry in found)
    print("natives: {} in {} classes".format(len(found), len({c for c, _, _ in found})))


if __name__ == "__main__":
    main()
