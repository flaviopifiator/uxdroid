"""Tests rewrite_classes.py on a hand-built class file. Run: python3 -m unittest test_rewrite_classes"""

import struct
import unittest

from rewrite_classes import NOT_CRITICAL, redirect_calls, rewrite


def utf8(text):
    return b"\x01" + struct.pack(">H", len(text)) + text


def class_file(entries, tail=b"TAIL"):
    """A constant pool of the given raw entries, then bytes that must come out unchanged."""
    slots = 1 + sum(2 if e[0] in (5, 6) else 1 for e in entries)
    return b"\xca\xfe\xba\xbe" + struct.pack(">HHH", 0, 52, slots) + b"".join(entries) + tail


def utf8_values(data):
    count = struct.unpack_from(">H", data, 8)[0]
    pos, index, found = 10, 1, []
    while index < count:
        tag = data[pos]
        if tag == 1:
            length = struct.unpack_from(">H", data, pos + 1)[0]
            found.append(data[pos + 3:pos + 3 + length])
            pos += 3 + length
        else:
            size = {5: 8, 6: 8, 7: 2, 8: 2, 10: 4}[tag]
            pos += 1 + size
            index += 1 if tag in (5, 6) else 0
        index += 1
    return found, data[pos:]


class RewriteTest(unittest.TestCase):
    def test_renames_critical_native_and_moves_partition_paths(self):
        data = class_file([
            utf8(b"Ldalvik/annotation/optimization/CriticalNative;"),
            b"\x05" + struct.pack(">q", 7),  # a Long takes two slots
            utf8(b"/system/etc/fonts.xml"),
            b"\x08" + struct.pack(">H", 3),
            utf8(b"/product/etc/fonts_customization.xml"),
            utf8(b"Ljava/lang/String;"),
            b"\x0a" + struct.pack(">HH", 1, 2),
        ])
        new, changes = rewrite(data, b"/opt/rt")
        values, tail = utf8_values(new)
        self.assertEqual(values, [NOT_CRITICAL, b"/opt/rt/etc/fonts.xml",
                                  b"/opt/rt/product/etc/fonts_customization.xml", b"Ljava/lang/String;"])
        self.assertEqual(tail, b"TAIL")
        self.assertEqual(changes, {"critical": 1, "paths": 2})

    def test_leaves_other_classes_byte_for_byte(self):
        data = class_file([utf8(b"java/lang/Object"), utf8(b"system/etc"), utf8(b"x/system/y")])
        self.assertEqual(rewrite(data, b"/opt/rt")[0], data)

    def test_refuses_what_is_not_a_class_file(self):
        with self.assertRaises(ValueError):
            rewrite(b"PK\x03\x04", b"/opt/rt")


def method_class(this, entries):
    """A class file whose constant pool is entries and whose this_class is entry this."""
    return class_file(entries, tail=struct.pack(">HHH", 0x31, this, 0) + b"REST")


def resolved_methodrefs(data):
    """Every Methodref of the pool as (owner, name, descriptor)."""
    count = struct.unpack_from(">H", data, 8)[0]
    pos, index, pool = 10, 1, {}
    while index < count:
        tag = data[pos]
        if tag == 1:
            length = struct.unpack_from(">H", data, pos + 1)[0]
            pool[index] = (1, data[pos + 3:pos + 3 + length])
            pos += 3 + length
        else:
            size = {5: 8, 6: 8, 7: 2, 8: 2, 10: 4, 12: 4}[tag]
            pool[index] = (tag, data[pos + 1:pos + 1 + size])
            pos += 1 + size
            index += 1 if tag in (5, 6) else 0
        index += 1

    def text(i):
        return pool[i][1]

    def ref(i, at=0):
        return struct.unpack_from(">H", pool[i][1], at)[0]
    found = []
    for tag, body in pool.values():
        if tag == 10:
            owner, nat = struct.unpack(">HH", body)
            found.append((text(ref(owner)), text(ref(nat)), text(ref(nat, 2))))
    return found, data[pos:]


class RedirectTest(unittest.TestCase):
    # android.os.Process: myUid calls Os.getuid, myPid Os.getpid, through one Class entry.
    ENTRIES = [
        utf8(b"android/os/Process"),             # 1
        b"\x07" + struct.pack(">H", 1),          # 2 this class
        utf8(b"android/system/Os"),              # 3
        b"\x07" + struct.pack(">H", 3),          # 4
        utf8(b"getuid"),                         # 5
        utf8(b"()I"),                            # 6
        b"\x0c" + struct.pack(">HH", 5, 6),      # 7 getuid:()I
        b"\x0a" + struct.pack(">HH", 4, 7),      # 8 Os.getuid
        utf8(b"getpid"),                         # 9
        b"\x0c" + struct.pack(">HH", 9, 6),      # 10 getpid:()I
        b"\x0a" + struct.pack(">HH", 4, 10),     # 11 Os.getpid
    ]
    UID = {"android/os/Process": [("android/system/Os", "getuid", "()I", "android/os/Binder", "getCallingUid")]}

    def test_moves_only_the_named_call_of_the_named_class(self):
        new, moved = redirect_calls(method_class(2, self.ENTRIES), self.UID)
        refs, tail = resolved_methodrefs(new)
        self.assertEqual(moved, 1)
        self.assertEqual(refs, [(b"android/os/Binder", b"getCallingUid", b"()I"),
                                (b"android/system/Os", b"getpid", b"()I")])
        self.assertEqual(tail, struct.pack(">HHH", 0x31, 2, 0) + b"REST")

    def test_leaves_other_classes_byte_for_byte(self):
        entries = list(self.ENTRIES)
        entries[0] = utf8(b"android/os/Other")
        data = method_class(2, entries)
        self.assertEqual(redirect_calls(data, self.UID), (data, 0))


if __name__ == "__main__":
    unittest.main()
