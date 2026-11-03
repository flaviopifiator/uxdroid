"""Rewrites constants in the framework's class files before they become DEX.

Usage: rewrite_classes.py <class root> <runtime dir>

Three changes, all in the constant pool, which every other part of a class file refers to by index
rather than by offset, so entries can change length:

- The host libandroid_runtime.so was built for a JVM, which ignores @CriticalNative and passes
  JNIEnv and jclass to every native. ART drops both for @CriticalNative methods, so the annotation
  is renamed to a type ART does not know.
- Paths of the Android partitions point into the runtime, which is laid out as the system partition:
  /system/etc/fonts.xml becomes <runtime>/etc/fonts.xml, the path Environment.getRootDirectory()
  also gives, since ANDROID_ROOT is the runtime.
- A few calls move to another method of the same shape, listed in REDIRECTS: Process.myUid calls
  Binder.getCallingUid instead of Os.getuid, so the framework sees the app's Android uid while the
  process keeps the user's Linux uid (crates/system/src/identity.rs). New entries are appended to the
  pool and only the call's Methodref points to them, so other calls through the same class stay.
"""

import os
import struct
import sys

CRITICAL_NATIVE = b"Ldalvik/annotation/optimization/CriticalNative;"
NOT_CRITICAL = b"Luxdroid/annotation/HostNative;"
SYSTEM = b"/system/"
OTHER_PARTITIONS = (b"/system_ext/", b"/product/", b"/vendor/")

# Per class: (owner, name, descriptor) of a static call, and the owner and name it calls instead.
REDIRECTS = {
    "android/os/Process": [("android/system/Os", "getuid", "()I", "android/os/Binder", "getCallingUid")],
}

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


def parse_pool(data):
    """The constant pool as {index: (tag, body)}, and the offset where it ends."""
    count = struct.unpack_from(">H", data, 8)[0]
    pos, index, pool = 10, 1, {}
    while index < count:
        tag = data[pos]
        size = 2 + struct.unpack_from(">H", data, pos + 1)[0] if tag == 1 else SIZES[tag]
        pool[index] = (tag, data[pos + 1:pos + 1 + size])
        pos += 1 + size
        index += 2 if tag in (5, 6) else 1
    return pool, count, pos


def redirect_calls(data, redirects):
    """Moves the calls REDIRECTS names for this class. Returns the class and how many calls moved."""
    if data[:4] != b"\xca\xfe\xba\xbe":
        raise ValueError("not a class file")
    pool, count, end = parse_pool(data)

    def text(index):
        return pool[index][1][2:].decode("utf-8", "replace")

    def ref(index, at=0):
        return struct.unpack_from(">H", pool[index][1], at)[0]
    this = text(ref(struct.unpack_from(">H", data, end + 2)[0]))
    wanted = redirects.get(this)
    if not wanted:
        return data, 0
    added, moved = [], 0

    def append(tag, body):
        nonlocal count
        added.append(bytes([tag]) + body)
        count += 1
        return count - 1

    def utf8(value):
        raw = value.encode()
        return append(1, struct.pack(">H", len(raw)) + raw)
    for index, (tag, body) in list(pool.items()):
        if tag != 10:
            continue
        owner, nat = struct.unpack(">HH", body)
        call = (text(ref(owner)), text(ref(nat)), text(ref(nat, 2)))
        for old_owner, name, descriptor, new_owner, new_name in wanted:
            if call == (old_owner, name, descriptor):
                new_class = append(7, struct.pack(">H", utf8(new_owner)))
                new_nat = append(12, struct.pack(">HH", utf8(new_name), ref(nat, 2)))
                pool[index] = (10, struct.pack(">HH", new_class, new_nat))
                moved += 1
    if not moved:
        return data, 0
    if count > 0xFFFF:
        raise ValueError(f"{this}: the constant pool would overflow")
    out = bytearray(data[:8]) + struct.pack(">H", count)
    for index in sorted(pool):
        tag, body = pool[index]
        out += bytes([tag]) + body
    return bytes(out + b"".join(added) + data[end:]), moved


def main():
    root, runtime = sys.argv[1], os.fsencode(sys.argv[2].rstrip("/"))
    totals = {"classes": 0, "critical": 0, "paths": 0, "calls": 0}
    for directory, _, files in os.walk(root):
        for name in files:
            if not name.endswith(".class"):
                continue
            path = os.path.join(directory, name)
            with open(path, "rb") as f:
                data = f.read()
            new, changes = rewrite(data, runtime)
            new, moved = redirect_calls(new, REDIRECTS)
            totals["calls"] += moved
            if new != data:
                with open(path, "wb") as f:
                    f.write(new)
                totals["classes"] += 1
                totals["critical"] += changes["critical"]
                totals["paths"] += changes["paths"]
    print("rewrote {classes} classes: {critical} @CriticalNative, {paths} partition paths, {calls} calls".format(**totals))
    if totals["critical"] == 0:
        sys.exit("no @CriticalNative found: the framework jar changed shape")
    expected = sum(len(calls) for calls in REDIRECTS.values())
    if totals["calls"] != expected:
        sys.exit(f"moved {totals['calls']} of the {expected} calls REDIRECTS names: the framework jar changed shape")


if __name__ == "__main__":
    main()
