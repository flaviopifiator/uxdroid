#!/bin/sh
# Lists the libc types whose size or alignment differs between bionic and glibc for one CPU.
# Usage: compare.sh x86_64|aarch64. Needs the NDK (dev image) and, for aarch64, Debian's aarch64
# glibc headers (dev-arm64 image). Prints "type: bionic <size>/<align>, glibc <size>/<align>".
set -eu
arch=$1
here=$(dirname "$0")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
ndk=$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin
"$ndk/clang" --target="$arch-linux-android24" -c "$here/layout_probe.c" -o "$work/bionic.o"
if [ "$arch" = aarch64 ]; then
    "$ndk/clang" --target=aarch64-linux-gnu -nostdinc -isystem /usr/aarch64-linux-gnu/include \
        -isystem "$("$ndk/clang" -print-resource-dir)/include" \
        -c "$here/layout_probe.c" -o "$work/glibc.o"
else
    "$ndk/clang" --target=x86_64-linux-gnu -nostdinc -isystem /usr/include \
        -isystem /usr/include/x86_64-linux-gnu \
        -isystem "$("$ndk/clang" -print-resource-dir)/include" \
        -c "$here/layout_probe.c" -o "$work/glibc.o"
fi
for libc in bionic glibc; do
    "$ndk/llvm-nm" -S --radix=d "$work/$libc.o" | awk '{ print $4, $2 + 0 }' | sort > "$work/$libc"
done
join "$work/bionic" "$work/glibc" | awk '
    { kind = substr($1, 1, index($1, "_") - 1); name = substr($1, index($1, "_") + 1)
      bionic[name, kind] = $2; glibc[name, kind] = $3; names[name] = 1 }
    END { for (name in names)
            if (bionic[name, "size"] != glibc[name, "size"] || bionic[name, "align"] != glibc[name, "align"])
                printf "%s: bionic %d/%d, glibc %d/%d\n", name, bionic[name, "size"], bionic[name, "align"],
                    glibc[name, "size"], glibc[name, "align"] }' | sort
