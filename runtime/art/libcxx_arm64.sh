#!/bin/bash
# Builds libc++ and the minimal UBSan runtime for an aarch64 glibc host, which the clang prebuilt
# lacks (it has x86_64 glibc and aarch64 musl builds), from the sources of that same prebuilt:
# Android's toolchain/llvm-project at the revision its x86_64 libc++ names in its .comment, so a
# tree with another drop of clang-r547379 gets its own. Configured as that x86_64 libc++ is, with
# libc++abi and LLVM's libunwind linked in and the soname libc++.so. The libraries and their
# __config_site go beside the prebuilt's own, under lib/ and include/aarch64-unknown-linux-gnu, and
# libclang_rt.ubsan_minimal-aarch64.a beside the builtins, where the Soong patch's Glibc_arm64
# entries name them. Usage: libcxx_arm64.sh [<AOSP tree>].
set -eu -o pipefail
TREE="${1:-/aosp}"
CLANG="$TREE/prebuilts/clang/host/linux-x86/clang-r547379"
# The revision the prebuilt's own x86_64 libc++ was built from, as its .comment names it.
REVISION=$("$CLANG/bin/llvm-readelf" -p .comment "$CLANG/lib/x86_64-unknown-linux-gnu/libc++.so" \
    | grep -o -m1 'toolchain/llvm-project [0-9a-f]\{40\}' | cut -d' ' -f2)
[ -n "$REVISION" ] || { echo "cannot tell which llvm-project revision $CLANG was built from"; exit 1; }
SYSROOT="$TREE/prebuilts/gcc/linux-x86/host/aarch64-linux-glibc2.31-10/sysroot"
NINJA="$TREE/prebuilts/build-tools/linux-x86/bin/ninja"
SRC="${LLVM_SRC:-/aosp/llvm-project-$REVISION}"
TRIPLE=aarch64-unknown-linux-gnu
[ -d "$SYSROOT" ] || { echo "no sysroot: run sysroot_arm64.sh first"; exit 1; }

# Only the runtimes' sources are checked out.
if [ ! -e "$SRC/.done-compiler-rt" ]; then
    rm -rf "$SRC"
    git init -q "$SRC"
    git -C "$SRC" remote add origin https://android.googlesource.com/toolchain/llvm-project
    git -C "$SRC" sparse-checkout set --no-cone /runtimes /libcxx /libcxxabi /libunwind /compiler-rt /cmake /llvm/cmake \
        /llvm/utils/llvm-lit /third-party/benchmark/cmake
    git -C "$SRC" fetch -q --depth 1 --filter=blob:none origin "$REVISION"
    git -C "$SRC" checkout -q FETCH_HEAD
    touch "$SRC/.done-compiler-rt"
fi

BUILD=$(mktemp -d)
trap 'rm -rf "$BUILD"' EXIT
FLAGS="--gcc-toolchain=$SYSROOT/usr"
cmake -G Ninja -S "$SRC/runtimes" -B "$BUILD" -Wno-dev \
    -DCMAKE_MAKE_PROGRAM="$NINJA" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_SYSTEM_NAME=Linux -DCMAKE_SYSTEM_PROCESSOR=aarch64 \
    -DCMAKE_SYSROOT="$SYSROOT" \
    -DCMAKE_C_COMPILER="$CLANG/bin/clang" -DCMAKE_CXX_COMPILER="$CLANG/bin/clang++" \
    -DCMAKE_ASM_COMPILER="$CLANG/bin/clang" \
    -DCMAKE_C_COMPILER_TARGET=$TRIPLE -DCMAKE_CXX_COMPILER_TARGET=$TRIPLE -DCMAKE_ASM_COMPILER_TARGET=$TRIPLE \
    -DCMAKE_C_FLAGS="$FLAGS" -DCMAKE_CXX_FLAGS="$FLAGS" -DCMAKE_ASM_FLAGS="$FLAGS" \
    -DCMAKE_SHARED_LINKER_FLAGS="-fuse-ld=lld $FLAGS" -DCMAKE_EXE_LINKER_FLAGS="-fuse-ld=lld $FLAGS" \
    -DCMAKE_AR="$CLANG/bin/llvm-ar" -DCMAKE_RANLIB="$CLANG/bin/llvm-ranlib" -DCMAKE_NM="$CLANG/bin/llvm-nm" \
    -DCMAKE_PLATFORM_NO_VERSIONED_SONAME=ON \
    -DCMAKE_TRY_COMPILE_TARGET_TYPE=STATIC_LIBRARY -DCMAKE_CXX_COMPILER_WORKS=ON \
    -DLLVM_ENABLE_RUNTIMES="compiler-rt;libunwind;libcxxabi;libcxx" \
    -DCOMPILER_RT_DEFAULT_TARGET_ONLY=ON -DLLVM_ENABLE_PER_TARGET_RUNTIME_DIR=OFF \
    -DCOMPILER_RT_BUILD_BUILTINS=OFF -DCOMPILER_RT_BUILD_SANITIZERS=ON \
    -DCOMPILER_RT_SANITIZERS_TO_BUILD=ubsan_minimal -DCOMPILER_RT_BUILD_XRAY=OFF \
    -DCOMPILER_RT_BUILD_LIBFUZZER=OFF -DCOMPILER_RT_BUILD_PROFILE=OFF -DCOMPILER_RT_BUILD_MEMPROF=OFF \
    -DCOMPILER_RT_BUILD_ORC=OFF -DCOMPILER_RT_BUILD_CTX_PROFILE=OFF -DCOMPILER_RT_INCLUDE_TESTS=OFF \
    -DLIBUNWIND_ENABLE_SHARED=OFF \
    -DLIBCXXABI_USE_LLVM_UNWINDER=ON -DLIBCXXABI_ENABLE_STATIC_UNWINDER=ON \
    -DLIBCXXABI_STATICALLY_LINK_UNWINDER_IN_SHARED_LIBRARY=ON \
    -DLIBCXX_ENABLE_STATIC_ABI_LIBRARY=ON -DLIBCXX_STATICALLY_LINK_ABI_IN_SHARED_LIBRARY=ON \
    -DLIBCXX_ENABLE_ABI_LINKER_SCRIPT=OFF -DLIBCXX_HAS_ATOMIC_LIB=OFF \
    -DLIBCXX_HARDENING_MODE=none \
    -DLIBCXX_INCLUDE_BENCHMARKS=OFF -DLIBCXX_INCLUDE_TESTS=OFF -DLIBCXXABI_INCLUDE_TESTS=OFF \
    -DLIBUNWIND_INCLUDE_TESTS=OFF > "$BUILD/cmake.log" || { tail -30 "$BUILD/cmake.log"; exit 1; }
"$NINJA" -C "$BUILD" cxx cxxabi unwind clang_rt.ubsan_minimal-aarch64 > "$BUILD/ninja.log" || { tail -30 "$BUILD/ninja.log"; exit 1; }

LIB="$CLANG/lib/$TRIPLE"
INCLUDE="$CLANG/include/$TRIPLE/c++/v1"
mkdir -p "$LIB" "$INCLUDE"
cp "$BUILD/lib/libc++.so" "$BUILD/lib/libc++.a" "$BUILD/lib/libc++abi.a" "$BUILD/lib/libunwind.a" "$LIB/"
cp "$BUILD/lib/libc++abi.so" "$LIB/" 2>/dev/null || true
cp "$BUILD/include/c++/v1/__config_site" "$INCLUDE/"
cp "$(find "$BUILD" -name libclang_rt.ubsan_minimal-aarch64.a | head -1)" "$CLANG/lib/clang/20/lib/linux/"
# The headers come from the prebuilt, so its __config_site must say the same apart from the libc.
diff "$CLANG/include/x86_64-unknown-linux-gnu/c++/v1/__config_site" "$INCLUDE/__config_site" \
    && echo "__config_site matches x86_64's"
"$CLANG/bin/llvm-readelf" -d "$LIB/libc++.so" | grep -E "NEEDED|SONAME"
ls -la "$LIB"
