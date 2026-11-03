"""Builds the framework's files from the downloaded archives.

Usage:
  assets.py aligned-jar <dex jar> <out jar>
  assets.py framework-res <android-all jar> <out apk>
  assets.py fonts <fonts dir> <fonts.xml in> <fonts.xml out>
  assets.py build-prop <build.prop in> <build.prop out>
"""

import os
import sys
import xml.etree.ElementTree as ET
import zipfile


def write_stored(out_path, entries):
    """Writes entries uncompressed, each one's data 4-byte aligned, so ART and the resource code map them."""
    with open(out_path, "wb") as raw, zipfile.ZipFile(raw, "w", zipfile.ZIP_STORED) as out:
        for name, data in entries:
            info = zipfile.ZipInfo(name, date_time=(2008, 1, 1, 0, 0, 0))
            offset = raw.tell() + 30 + len(name.encode())
            info.extra = b"\0" * ((-offset) % 4)
            out.writestr(info, data)


def aligned_jar(dex_jar, out):
    with zipfile.ZipFile(dex_jar) as src:
        names = sorted(n for n in src.namelist() if n.endswith(".dex"))
        if not names:
            sys.exit("no DEX in " + dex_jar)
        write_stored(out, [(n, src.read(n)) for n in names])


def framework_res(android_all, out):
    with zipfile.ZipFile(android_all) as src:
        names = [n for n in src.namelist()
                 if n in ("AndroidManifest.xml", "resources.arsc") or (n.startswith("res/") and not n.endswith("/"))]
        if "resources.arsc" not in names:
            sys.exit("no resources.arsc in " + android_all)
        write_stored(out, [(n, src.read(n)) for n in sorted(names)])


def fonts(directory, xml_in, xml_out):
    """Keeps the families whose files were shipped, and the aliases that still point somewhere."""
    tree = ET.parse(xml_in)
    root = tree.getroot()
    shipped = set(os.listdir(directory))
    names = set()
    for family in list(root.findall("family")):
        for font in list(family.findall("font")):
            if (font.text or "").strip() not in shipped:
                family.remove(font)
        if family.find("font") is None:
            root.remove(family)
        elif family.get("name"):
            names.add(family.get("name"))
    for alias in list(root.findall("alias")):
        if alias.get("to") not in names:
            root.remove(alias)
        else:
            names.add(alias.get("name"))
    tree.write(xml_out, encoding="utf-8", xml_declaration=True)
    print("fonts: {} families".format(len(root.findall("family"))))


def build_prop(prop_in, prop_out):
    """Describes this machine: an x86_64 host named uxdroid, not the ARM device Robolectric pretends to be.

    Robolectric names itself in the product, tags and fingerprint, and apps read that as a unit test:
    AnkiDroid skips loading its native library when Build.FINGERPRINT is "robolectric". The fingerprint
    keeps Android's form, brand/product/device:release/id/incremental:type/tags, of a user build signed
    with release keys, as an app expects of a device that is not a debug build.
    """
    props = {}
    with open(prop_in) as f:
        for line in f:
            if "=" in line and not line.lstrip().startswith("#"):
                key, value = line.split("=", 1)
                props[key.strip()] = value.strip()
    build_id = props.get("ro.build.id", "unknown")
    release = props.get("ro.build.version.release", "unknown")
    overrides = {
        "ro.product.cpu.abi": "x86_64",
        "ro.product.cpu.abi2": "",
        "ro.product.cpu.abilist": "x86_64",
        "ro.product.cpu.abilist64": "x86_64",
        "ro.product.cpu.abilist32": "",
        "ro.product.name": "uxdroid",
        "ro.product.device": "uxdroid",
        "ro.product.board": "uxdroid",
        "ro.product.manufacturer": "uxdroid",
        "ro.product.brand": "uxdroid",
        "ro.product.model": "uxdroid",
        "ro.hardware": "uxdroid",
        "ro.build.type": "user",
        "ro.build.tags": "release-keys",
        "ro.build.version.incremental": "1",
        "ro.build.display.id": build_id,
        "ro.build.characteristics": "default",
        "ro.build.fingerprint": "uxdroid/uxdroid/uxdroid:{}/{}/1:user/release-keys".format(release, build_id),
    }
    lines = []
    with open(prop_in) as f:
        for line in f:
            key = line.split("=", 1)[0].strip()
            if key not in overrides:
                lines.append(line.rstrip("\n"))
    lines += ["{}={}".format(k, v) for k, v in overrides.items()]
    with open(prop_out, "w") as f:
        f.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    command, args = sys.argv[1], sys.argv[2:]
    {"aligned-jar": aligned_jar, "framework-res": framework_res, "fonts": fonts, "build-prop": build_prop}[command](*args)
