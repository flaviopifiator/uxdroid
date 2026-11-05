#!/bin/bash
# Downloads AOSP's framework as Robolectric publishes it, built for a glibc host, and its notices.
# Every file is pinned by checksum or by tag, so a build never changes under us.
set -eu -o pipefail
DL=/aosp/framework-dl
mkdir -p "$DL/notices"
MAVEN=https://repo1.maven.org/maven2/org/robolectric

fetch() {
    local url=$1 sum=$2 file="$DL/${1##*/}"
    if ! echo "$sum  $file" | sha256sum -c --status 2>/dev/null; then
        curl -sfL --retry 3 -o "$file.part" "$url"
        echo "$sum  $file.part" | sha256sum -c --quiet
        mv "$file.part" "$file"
    fi
}

# The framework classes, framework-res, ICU data and the host libandroid_runtime.so of Android 16.
fetch "$MAVEN/android-all/16-robolectric-13921718/android-all-16-robolectric-13921718.jar" \
    8b74a0a137330658d2f33f0dc715d42734f74ba8b2d7014fc2e95aa40d3f682d
# Android's system fonts.
fetch "$MAVEN/nativeruntime-dist-compat/1.0.19/nativeruntime-dist-compat-1.0.19.jar" \
    33e0a0982b3ba599763af250b86eb8d67ab6c186c2fa2c5e823ee003bdcd4ed8

# Notices of what those files contain, from the matching AOSP release. Gitiles serves them base64.
TAG=android-16.0.0_r1
while read -r name project path; do
    curl -sfL --retry 3 "https://android.googlesource.com/platform/$project/+/refs/tags/$TAG/$path?format=TEXT" \
        | base64 -d > "$DL/notices/$name"
    test -s "$DL/notices/$name"
done <<'NOTICES'
frameworks-base frameworks/base NOTICE
robolectric external/robolectric LICENSE
skia external/skia LICENSE
harfbuzz external/harfbuzz_ng COPYING
freetype external/freetype LICENSE.TXT
icu external/icu LICENSE
libpng external/libpng LICENSE
libjpeg-turbo external/libjpeg-turbo LICENSE.md
expat external/expat COPYING
sqlite external/sqlite LICENSE
roboto-fonts external/roboto-fonts NOTICE
NOTICES
ls -la "$DL"
