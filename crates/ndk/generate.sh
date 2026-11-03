#!/bin/bash
# Regenerates src/egl_table.rs and src/gles_table.rs from the NDK. Runs in the dev service:
#   docker compose run --rm -T dev bash /work/crates/ndk/generate.sh
set -eu -o pipefail
API=35
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64"
STUBS="$TOOLCHAIN/sysroot/usr/lib/x86_64-linux-android/$API"
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT
for library in EGL GLESv2 GLESv3; do
    "$TOOLCHAIN/bin/llvm-nm" -D --defined-only "$STUBS/lib$library.so" | awk '$2 == "T" { print $3 }' > "$W/$library"
done
PYTHONDONTWRITEBYTECODE=1 python3 /work/crates/ndk/generate.py "$TOOLCHAIN/sysroot/usr/include" \
    "$W/EGL" "$W/GLESv2" "$W/GLESv3" /work/crates/ndk/src
echo "EGL $(wc -l < "$W/EGL"), GLESv2 $(wc -l < "$W/GLESv2"), GLESv3 $(wc -l < "$W/GLESv3") exports"
rustfmt --edition 2024 /work/crates/ndk/src/egl_table.rs /work/crates/ndk/src/gles_table.rs
