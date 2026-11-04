#!/bin/bash
# Makes the aarch64 glibc sysroot the arm64 host build links against, from Debian's arm64 packages
# pinned by SHA-256: Debian 11's glibc 2.31, GCC 10's libgcc and crt files and libcap, and Debian
# 12's Linux 6.1 kernel headers. AOSP ships no aarch64 glibc sysroot; 2.31 keeps the result running
# on Debian 11, Ubuntu 20.04 and anything newer. Like AOSP's own x86_64 sysroot, glibc 2.17 with
# newer kernel headers and a few later glibc constants, this one adds the few later constants ART
# and libunwindstack use that glibc 2.31 predates (below). Laid out as prebuilts/gcc/linux-x86/host/x86_64-linux-glibc2.17-4.8 is:
# the Soong patch names this directory. Usage: sysroot_arm64.sh [<AOSP tree>], /aosp by default.
set -eu -o pipefail
TREE="${1:-/aosp}"
ROOT="$TREE/prebuilts/gcc/linux-x86/host/aarch64-linux-glibc2.31-10"
DL="${SYSROOT_DL:-/aosp/sysroot-dl}"
SNAPSHOT=https://snapshot.debian.org/archive
mkdir -p "$DL"

fetch() {
    local url=$1 sum=$2 file="$DL/${1##*/}"
    if ! echo "$sum  $file" | sha256sum -c --status 2>/dev/null; then
        curl -sfL --retry 5 --retry-all-errors --retry-delay 5 -o "$file.part" "$url"
        echo "$sum  $file.part" | sha256sum -c --quiet
        mv "$file.part" "$file"
    fi
}

PACKAGES=()
while read -r archive path sum; do
    fetch "$SNAPSHOT/$archive/20251001T000000Z/$path" "$sum"
    PACKAGES+=("$DL/${path##*/}")
done <<'DEBS'
debian-security pool/updates/main/g/glibc/libc6_2.31-13+deb11u13_arm64.deb 349b6a8043ebf4d608ce93d26fc66a64a311c952a7197d472b4c97accc8c21ea
debian-security pool/updates/main/g/glibc/libc6-dev_2.31-13+deb11u13_arm64.deb a029f2556305509ba9649d90d335ceb5b0cac17e8715cc50163da91b75bf7870
debian pool/main/l/linux/linux-libc-dev_6.1.148-1_arm64.deb 55baaddd21bcaa7fed8894c89e4d65b684e85abcb48acf443519438185e1153f
debian pool/main/libc/libcap2/libcap2_2.44-1_arm64.deb 7c5729a1cfd14876685217c5f0545301e7ff1b839262fb487d6a778e8cd8c05a
debian pool/main/libc/libcap2/libcap-dev_2.44-1_arm64.deb 0797e39cea3e67085b7ca31718856c55c6afa828f852b71b1c74e26ec6ec98d2
debian pool/main/libx/libxcrypt/libcrypt1_4.4.18-4_arm64.deb 22b586b29e840dabebf0bf227d233376628b87954915d064bc142ae85d1b7979
debian pool/main/libx/libxcrypt/libcrypt-dev_4.4.18-4_arm64.deb 5309fdf445acc72794b9d134e9625746ffad2b865c6432abcd08ef097587bde1
debian pool/main/g/gcc-10/libgcc-10-dev_10.2.1-6_arm64.deb 80fc73d339ada2e194175afe83cd89014565242153b6e3c2128849d8817da367
debian pool/main/g/gcc-10/libgcc-s1_10.2.1-6_arm64.deb e2fcdb378d3c1ad1bcb64d4fb6b37aab44011152beca12a4944f435a2582df1f
debian pool/main/g/gcc-10/libatomic1_10.2.1-6_arm64.deb d0b8a02896479a6eac49019d71142da9561482759f40eb7aeb15cfdc7d3e39ec
debian pool/main/g/gcc-10/gcc-10-base_10.2.1-6_arm64.deb 7d782bece7b4a36bed045a7e17d17244cb8f7e4732466091b01412ebf215defb
DEBS

rm -rf "$ROOT"
mkdir -p "$ROOT/sysroot"
for deb in "${PACKAGES[@]}"; do
    dpkg-deb -x "$deb" "$ROOT/sysroot"
done
# Absolute symbolic links, such as libm.so to /lib/aarch64-linux-gnu/libm.so.6, would name the
# build machine's files: each becomes relative to the sysroot.
find "$ROOT/sysroot" -type l | while read -r link; do
    target=$(readlink "$link")
    case "$target" in
        /*) ln -sfn "$(realpath -m --relative-to="$(dirname "$link")" "$ROOT/sysroot$target")" "$link" ;;
    esac
done
# The sanitizer and OpenMP libraries libgcc-10-dev links to are not shipped, and nothing uses them.
find "$ROOT/sysroot" -xtype l -delete
# glibc 2.32's MREMAP_DONTUNMAP and 2.33's MTE fault codes, which ART's GC and fault handler use; the
# kernel has both (Linux 5.7 and 5.10).
BITS="$ROOT/sysroot/usr/include/aarch64-linux-gnu/bits"
sed -i 's|^# define MREMAP_FIXED\t2$|&\n# define MREMAP_DONTUNMAP 4 /* uxdroid: from glibc 2.32 */|' "$BITS/mman-shared.h"
grep -q MREMAP_DONTUNMAP "$BITS/mman-shared.h"
# The guard's last #endif closes the file.
printf '%s\n' '' '/* uxdroid: from glibc 2.33 */' '#if !defined SEGV_MTEAERR && defined _BITS_SIGINFO_CONSTS_H' \
    '# define SEGV_MTEAERR 8' '# define SEGV_MTESERR 9' '#endif' >> "$BITS/siginfo-consts.h"
# elf.h's ELFCOMPRESS_ZSTD (glibc 2.37) and NT_RISCV_VECTOR (2.39), which libunwindstack reads.
sed -i 's|^#endif\t/\* elf.h \*/$|/* uxdroid: from glibc 2.37 and 2.39 */\n#ifndef ELFCOMPRESS_ZSTD\n# define ELFCOMPRESS_ZSTD 2\n#endif\n#ifndef NT_RISCV_VECTOR\n# define NT_RISCV_VECTOR 0x901\n#endif\n&|' "$ROOT/sysroot/usr/include/elf.h"
grep -q ELFCOMPRESS_ZSTD "$ROOT/sysroot/usr/include/elf.h"
rm -rf "$ROOT/sysroot/usr/share/doc" "$ROOT/sysroot/usr/share/lintian" "$ROOT/sysroot/usr/share/man"
mkdir -p "$ROOT/notices"
# GCC's libraries carry the notice of gcc-10-base, GCC 10's runtime library exception.
for package in libc6 linux-libc-dev libcrypt1 gcc-10-base libcap2; do
    dpkg-deb --fsys-tarfile "$(printf '%s\n' "${PACKAGES[@]}" | grep "/${package}_")" \
        | tar -xO "./usr/share/doc/$package/copyright" > "$ROOT/notices/$package"
done
du -sh "$ROOT"
