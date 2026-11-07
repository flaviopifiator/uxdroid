#!/bin/bash
# Packages a built runtime, ART and the framework with uxdroid's own Java, as uxdroid-runtime, which
# installs it in /usr/lib/uxdroid/runtime beside the program. Building the runtime takes AOSP's trees,
# so this runs where they are, not on CI's hosted runners; docs/guide.md (Packages) has the commands:
#   packaging/runtime.sh amd64|arm64 <runtime built for /usr/lib/uxdroid/runtime> [OUT]
# The runtime and uxdroid's libraries share their JNI natives, so the version must be uxdroid's own:
# VERSION and VERSION_SUFFIX as for packaging/build.sh.
set -eu -o pipefail

ARCH=${1:?usage: packaging/runtime.sh amd64|arm64 <runtime dir> [OUT]}
RUNTIME=$(realpath "${2:?usage: packaging/runtime.sh amd64|arm64 <runtime dir> [OUT]}")
OUT=$(realpath -m "${3:-dist}")
case "$ARCH" in
    amd64) ISA=x86_64 ;;
    arm64) ISA=arm64 ;;
    *) echo "error: unknown architecture $ARCH, expected amd64 or arm64" >&2; exit 2 ;;
esac
# A runtime for another CPU would install and then fail at the first start.
[ -f "$RUNTIME/framework/framework.jar" ] || { echo "error: no framework in $RUNTIME" >&2; exit 1; }
[ -d "$RUNTIME/framework/$ISA" ] || { echo "error: $RUNTIME has no boot image for $ISA" >&2; exit 1; }
# The framework's classes name the system partition by the root the runtime was built for
# (runtime/framework/rewrite_classes.py), and the boot image its jars, so a runtime only works where
# it was built: it must have been built with RUNTIME=/usr/lib/uxdroid/runtime.
python3 - "$RUNTIME/framework/framework.jar" <<'EOF' || exit 1
import sys, zipfile
root = b"/usr/lib/uxdroid/runtime/fonts/"
with zipfile.ZipFile(sys.argv[1]) as jar:
    if not any(root in jar.read(name) for name in jar.namelist() if name.endswith(".dex")):
        sys.exit("error: this runtime was not built for /usr/lib/uxdroid/runtime; build it with "
                 "RUNTIME=/usr/lib/uxdroid/runtime (docs/guide.md, Packages)")
EOF
VERSION=${VERSION:-$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["packages"][0]["version"])')}
VERSION=${VERSION#v}${VERSION_SUFFIX:-}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
ROOT="$WORK/deb"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/lib/uxdroid/runtime" "$ROOT/usr/share/doc/uxdroid-runtime" "$OUT"
# samples holds the smoke tests' programs, not part of what apps run on.
tar -C "$RUNTIME" --exclude=./samples -cf - . | tar -C "$ROOT/usr/lib/uxdroid/runtime" -xf -
cat /work/NOTICE /work/LICENSE > "$ROOT/usr/share/doc/uxdroid-runtime/copyright"
chmod 0644 "$ROOT/usr/share/doc/uxdroid-runtime/copyright"
cat > "$ROOT/DEBIAN/control" <<EOF
Package: uxdroid-runtime
Version: $VERSION
Architecture: $ARCH
Maintainer: uxdroid developers <noreply@github.com>
Installed-Size: $(du -sk --exclude=DEBIAN "$ROOT" | cut -f1)
Depends: libc6 (>= 2.31), uxdroid (= $VERSION)
Section: otherosfs
Priority: optional
Homepage: https://github.com/flaviopifiator/uxdroid
Description: Android runtime and framework for uxdroid
 ART and the Android 16 framework, built from AOSP sources for a glibc host,
 with uxdroid's platform classes. uxdroid finds them in /usr/lib/uxdroid/runtime.
EOF
DEB="$OUT/uxdroid-runtime_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group -Zxz --build "$ROOT" "$DEB" > /dev/null
(cd "$OUT" && sha256sum "$(basename "$DEB")" > "SHA256SUMS-runtime-$ARCH")
ls -l "$DEB"
