#!/bin/bash
# Builds uxdroid for one CPU and packages it as a .deb and a .tar.gz in OUT, with their SHA-256 sums.
# Runs in packaging/Dockerfile: packaging/build.sh amd64|arm64 [OUT]. VERSION overrides the
# workspace's version, as a release tag does, and VERSION_SUFFIX is appended, as CI's +ci.<run>.
#
# The package installs the program and its libraries in /usr/lib/uxdroid, which uxdroid finds beside
# itself, and links /usr/bin/uxdroid to it. The runtime is the uxdroid-runtime package, which
# installs /usr/lib/uxdroid/runtime (packaging/runtime.sh).
set -eu -o pipefail

ARCH=${1:?usage: packaging/build.sh amd64|arm64 [OUT]}
OUT=$(realpath -m "${2:-dist}")
case "$ARCH" in
    amd64) TARGET=x86_64-unknown-linux-gnu STRIP=strip ;;
    arm64) TARGET=aarch64-unknown-linux-gnu STRIP=aarch64-linux-gnu-strip ;;
    *) echo "error: unknown architecture $ARCH, expected amd64 or arm64" >&2; exit 2 ;;
esac
VERSION=${VERSION:-$(cargo metadata -q --format-version 1 --no-deps --manifest-path /work/Cargo.toml \
    | python3 -c 'import json, sys; print(json.load(sys.stdin)["packages"][0]["version"])')}
VERSION=${VERSION#v}${VERSION_SUFFIX:-}

cargo build --release --locked --manifest-path /work/Cargo.toml --target "$TARGET" \
    -p uxdroid-cli -p uxdroid-system -p uxdroid-nativebridge
RELEASE="${CARGO_TARGET_DIR:-/work/target}/$TARGET/release"
FILES=(uxdroid libuxdroid_system.so libuxdroid_nativebridge.so)

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"

# The .deb.
ROOT="$WORK/deb"
mkdir -p "$ROOT/DEBIAN" "$ROOT/usr/lib/uxdroid" "$ROOT/usr/bin" "$ROOT/usr/share/doc/uxdroid"
for file in "${FILES[@]}"; do
    install -m 0755 "$RELEASE/$file" "$ROOT/usr/lib/uxdroid/$file"
    "$STRIP" --strip-unneeded "$ROOT/usr/lib/uxdroid/$file"
done
ln -s ../lib/uxdroid/uxdroid "$ROOT/usr/bin/uxdroid"
# copyright: uxdroid's notice, the third-party code it carries, then its license.
cat /work/NOTICE /work/LICENSE > "$ROOT/usr/share/doc/uxdroid/copyright"
chmod 0644 "$ROOT/usr/share/doc/uxdroid/copyright"
install -m 0644 /work/README.md "$ROOT/usr/share/doc/uxdroid/README.md"
# The icon and a menu entry that opens the terminal UI.
install -D -m 0644 /work/docs/logo/icon.svg "$ROOT/usr/share/icons/hicolor/scalable/apps/uxdroid.svg"
install -D -m 0644 /work/packaging/uxdroid.desktop "$ROOT/usr/share/applications/uxdroid.desktop"
# Linked: glibc and libgcc only. Opened when needed, so recommended: Mesa's EGL, GLES and GBM for
# apps that draw with the GPU, xkbcommon for typed characters, and PulseAudio or ALSA for sound.
cat > "$ROOT/DEBIAN/control" <<EOF
Package: uxdroid
Version: $VERSION
Architecture: $ARCH
Maintainer: uxdroid developers <noreply@github.com>
Installed-Size: $(du -sk --exclude=DEBIAN "$ROOT" | cut -f1)
Depends: libc6 (>= 2.35), libgcc-s1
Recommends: uxdroid-runtime (= $VERSION), libegl1, libegl-mesa0, libgles2, libgbm1, libxkbcommon0, libpulse0, libasound2t64 | libasound2
Section: otherosfs
Priority: optional
Homepage: https://github.com/flaviopifiator/uxdroid
Description: Run Android apps on Linux as native processes
 uxdroid runs each Android app as an ordinary Linux process with its own
 Wayland window, sound on the desktop's audio server and input from its seat,
 with no emulator, virtual machine or container. It installs apps from
 F-Droid repositories and verifies every index and APK.
EOF
DEB="$OUT/uxdroid_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group -Zxz --build "$ROOT" "$DEB" > /dev/null

# The tarball: the same files in one directory, which runs from anywhere it is unpacked.
NAME="uxdroid-${VERSION}-linux-${ARCH}"
mkdir -p "$WORK/$NAME"
cp -a "$ROOT/usr/lib/uxdroid/." "$WORK/$NAME/"
cp /work/LICENSE /work/NOTICE /work/README.md "$WORK/$NAME/"
cp /work/docs/logo/icon.svg "$WORK/$NAME/uxdroid.svg"
TAR="$OUT/$NAME.tar.gz"
tar -C "$WORK" --owner=0 --group=0 --numeric-owner -czf "$TAR" "$NAME"

(cd "$OUT" && sha256sum "$(basename "$DEB")" "$(basename "$TAR")" > "SHA256SUMS-$ARCH")
ls -l "$DEB" "$TAR"
