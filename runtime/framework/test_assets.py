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


class BuildPropTest(unittest.TestCase):
    ROBOLECTRIC = "\n".join([
        "ro.build.id=BLR2.250402.052",
        "ro.build.display.id=robolectric 16 BLR2.250402.052",
        "ro.product.name=robolectric",
        "ro.product.device=robolectric",
        "ro.product.brand=robolectric",
        "ro.product.manufacturer=robolectric",
        "ro.product.model=robolectric",
        "ro.hardware=robolectric",
        "ro.build.version.release=16",
        "ro.build.tags=robolectric",
        "ro.build.fingerprint=robolectric",
        "ro.build.characteristics=robolectric",
        "ro.product.cpu.abi=unknown",
        "persist.debug.new_insets=0",
    ]) + "\n"

    def properties(self, *abi):
        from assets import build_prop
        with tempfile.TemporaryDirectory() as work:
            source, out = os.path.join(work, "in"), os.path.join(work, "out")
            with open(source, "w") as f:
                f.write(self.ROBOLECTRIC)
            build_prop(source, out, *abi)
            with open(out) as f:
                return dict(line.split("=", 1) for line in f.read().splitlines() if "=" in line)

    def test_the_device_is_uxdroid_on_x86_64_not_robolectric(self):
        # Apps skip their native code when Build.FINGERPRINT says Robolectric, as AnkiDroid does.
        props = self.properties()
        self.assertEqual([k for k, v in props.items() if "robolectric" in v.lower()], [])
        self.assertEqual(props["ro.build.fingerprint"],
                         "uxdroid/uxdroid/uxdroid:16/BLR2.250402.052/1:user/release-keys")
        self.assertEqual(props["ro.product.model"], "uxdroid")
        self.assertEqual(props["ro.build.display.id"], "BLR2.250402.052")
        self.assertEqual(props["ro.product.cpu.abi"], "x86_64")
        self.assertEqual(props["persist.debug.new_insets"], "0")

    def test_the_arm64_package_describes_an_arm64_device(self):
        # The arm64 package's apps pick arm64-v8a code, as Build.SUPPORTED_ABIS lists it.
        props = self.properties("arm64-v8a")
        for key in ("ro.product.cpu.abi", "ro.product.cpu.abilist", "ro.product.cpu.abilist64"):
            self.assertEqual(props[key], "arm64-v8a")
        self.assertEqual(props["ro.product.cpu.abilist32"], "")
        self.assertEqual(props["ro.product.model"], "uxdroid")
