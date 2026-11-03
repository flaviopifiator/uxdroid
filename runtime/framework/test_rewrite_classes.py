"""Tests rewrite_classes.py on a hand-built class file. Run: python3 -m unittest test_rewrite_classes"""

import struct
import unittest

from rewrite_classes import NOT_CRITICAL, rewrite


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


if __name__ == "__main__":
    unittest.main()
