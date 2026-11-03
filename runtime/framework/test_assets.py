"""Tests assets.py. Run: python3 -m unittest test_assets"""

import os
import tempfile
import unittest
import zipfile

from assets import write_stored


class StoredZipTest(unittest.TestCase):
    def test_every_entry_is_uncompressed_and_4_byte_aligned(self):
        with tempfile.TemporaryDirectory() as scratch:
            path = os.path.join(scratch, "out.jar")
            entries = [("classes.dex", b"dex\n039\0" + b"x" * 13), ("a", b"1"), ("classes2.dex", b"y" * 7)]
            write_stored(path, entries)
            with zipfile.ZipFile(path) as z:
                for info, (name, data) in zip(z.infolist(), entries):
                    self.assertEqual(info.compress_type, zipfile.ZIP_STORED)
                    self.assertEqual(z.read(name), data)
                    start = info.header_offset + 30 + len(info.filename.encode()) + len(info.extra)
                    self.assertEqual(start % 4, 0, name)


if __name__ == "__main__":
    unittest.main()
