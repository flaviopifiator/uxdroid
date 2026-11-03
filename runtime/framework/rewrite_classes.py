"""Rewrites constants in the framework's class files before they become DEX.

Usage: rewrite_classes.py <class root> <runtime dir>

Two changes, both in the constant pool, which every other part of a class file refers to by index
rather than by offset, so entries can change length:

- The host libandroid_runtime.so was built for a JVM, which ignores @CriticalNative and passes
  JNIEnv and jclass to every native. ART drops both for @CriticalNative methods, so the annotation
  is renamed to a type ART does not know.
- Paths of the Android partitions point into the runtime, which is laid out as the system partition:
  /system/etc/fonts.xml becomes <runtime>/etc/fonts.xml, the path Environment.getRootDirectory()
  also gives, since ANDROID_ROOT is the runtime.
"""

import os
import struct
import sys

CRITICAL_NATIVE = b"Ldalvik/annotation/optimization/CriticalNative;"
NOT_CRITICAL = b"Luxdroid/annotation/HostNative;"
SYSTEM = b"/system/"
OTHER_PARTITIONS = (b"/system_ext/", b"/product/", b"/vendor/")

# Constant pool entry sizes after the tag byte, for every tag but Utf8.
SIZES = {3: 4, 4: 4, 5: 8, 6: 8, 7: 2, 8: 2, 9: 4, 10: 4, 11: 4, 12: 4, 15: 3, 16: 2, 17: 4, 18: 4, 19: 2, 20: 2}


def rewrite(data, runtime):
    if data[:4] != b"\xca\xfe\xba\xbe":
        raise ValueError("not a class file")
    count = struct.unpack_from(">H", data, 8)[0]
    out = bytearray(data[:10])
    pos, index, changes = 10, 1, {"critical": 0, "paths": 0}
    while index < count:
        tag = data[pos]
        if tag == 1:
            length = struct.unpack_from(">H", data, pos + 1)[0]
            text = data[pos + 3:pos + 3 + length]
            if text == CRITICAL_NATIVE:
                text = NOT_CRITICAL
                changes["critical"] += 1
            elif text.startswith(SYSTEM):
                text = runtime + b"/" + text[len(SYSTEM):]
                changes["paths"] += 1
            elif text.startswith(OTHER_PARTITIONS):
                text = runtime + text
                changes["paths"] += 1
            out += b"\x01" + struct.pack(">H", len(text)) + text
            pos += 3 + length
        else:
            size = SIZES[tag]
            out += data[pos:pos + 1 + size]
            pos += 1 + size
            # Long and Double take two slots.
            index += 1 if tag in (5, 6) else 0
        index += 1
    out += data[pos:]
    return bytes(out), changes


def main():
    root, runtime = sys.argv[1], os.fsencode(sys.argv[2].rstrip("/"))
    totals = {"classes": 0, "critical": 0, "paths": 0}
    for directory, _, files in os.walk(root):
        for name in files:
            if not name.endswith(".class"):
                continue
            path = os.path.join(directory, name)
            with open(path, "rb") as f:
                data = f.read()
            new, changes = rewrite(data, runtime)
            if new != data:
                with open(path, "wb") as f:
                    f.write(new)
                totals["classes"] += 1
                totals["critical"] += changes["critical"]
                totals["paths"] += changes["paths"]
    print("rewrote {classes} classes: {critical} @CriticalNative, {paths} partition paths".format(**totals))
    if totals["critical"] == 0:
        sys.exit("no @CriticalNative found: the framework jar changed shape")


if __name__ == "__main__":
    main()
