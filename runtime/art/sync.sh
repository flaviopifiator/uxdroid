#!/bin/bash
# Downloads the exact AOSP revisions in manifest.xml into the art_src volume.
set -eu -o pipefail
cd /aosp
repo init -q -u https://android.googlesource.com/platform/manifest -b master-art --depth=1 </dev/null
cp /work/runtime/art/manifest.xml .repo/manifests/uxdroid-pinned.xml
repo init -q -m uxdroid-pinned.xml </dev/null
repo sync -c -j6 --no-tags --no-clone-bundle -q
