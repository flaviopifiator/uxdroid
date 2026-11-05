#!/bin/bash
# Downloads the part of AOSP's android-16.0.0_r1 that libandroid_runtime's host build needs, into the
# android16_src volume at /a16: the projects in projects.txt, each at the tag, one commit deep, some
# sparse. android-16.0.0_r1 is the release whose notices the x86_64 package already ships with
# Robolectric's build of the same library (runtime/framework/fetch.sh). Files the manifest copies or
# links into place for those projects are made too. Safe to run again: a finished project is kept.
set -eu -o pipefail
TAG=android-16.0.0_r1
A16=/a16
GOOGLESOURCE=https://android.googlesource.com
LIST=/work/runtime/framework/android16/projects.txt
cd "$A16"

if [ ! -s .manifest.xml ]; then
    curl -sfL --retry 3 "$GOOGLESOURCE/platform/manifest/+/refs/tags/$TAG/default.xml?format=TEXT" \
        | base64 -d > .manifest.xml.part
    mv .manifest.xml.part .manifest.xml
fi

# path name [sparse paths...], one project a line, from the manifest's own names.
grep -v -e '^#' -e '^$' "$LIST" | while read -r path sparse; do
    name=$(python3 - "$path" <<'PY'
import sys, xml.etree.ElementTree as ET
for p in ET.parse("/a16/.manifest.xml").getroot().iter("project"):
    if p.get("path", p.get("name")) == sys.argv[1]:
        print(p.get("name")); break
else:
    sys.exit("not in the manifest: " + sys.argv[1])
PY
)
    [ -e "$path/.uxdroid-synced" ] && continue
    rm -rf "$path"
    mkdir -p "$path"
    git -C "$path" init -q
    git -C "$path" remote add aosp "$GOOGLESOURCE/$name"
    if [ -n "$sparse" ]; then
        # The patterns are git's, never the shell's: no globbing.
        set -f
        # shellcheck disable=SC2086
        git -C "$path" sparse-checkout set --no-cone $sparse
        set +f
    fi
    # A sparse project downloads only the files it checks out.
    filter=()
    [ -n "$sparse" ] && filter=(--filter=blob:none)
    git -C "$path" fetch -q --depth 1 "${filter[@]}" aosp "refs/tags/$TAG" \
        || git -C "$path" fetch -q --depth 1 "${filter[@]}" aosp "refs/tags/$TAG"
    git -C "$path" checkout -q FETCH_HEAD
    touch "$path/.uxdroid-synced"
    echo "synced $path ($(du -sh "$path" | cut -f1))"
done

# The manifest's copyfile and linkfile entries of the synced projects, as repo makes them.
python3 - <<'PY'
import os, xml.etree.ElementTree as ET
root = "/a16"
for p in ET.parse(root + "/.manifest.xml").getroot().iter("project"):
    path = p.get("path", p.get("name"))
    if not os.path.exists(os.path.join(root, path, ".uxdroid-synced")):
        continue
    for entry in p:
        src = os.path.join(root, path, entry.get("src", ""))
        dest = os.path.join(root, entry.get("dest", ""))
        if entry.tag == "copyfile":
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            with open(src, "rb") as f, open(dest, "wb") as g:
                g.write(f.read())
        elif entry.tag == "linkfile":
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            if os.path.lexists(dest):
                os.remove(dest)
            os.symlink(os.path.relpath(src, os.path.dirname(dest)), dest)
PY
du -sh "$A16"
