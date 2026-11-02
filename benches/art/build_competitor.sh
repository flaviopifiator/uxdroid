#!/bin/bash
# Builds art_standalone, the runtime of Android Translation Layer, as a comparison target only.
# Its license is undetermined, so it stays in a Docker volume and never enters uxdroid.
set -eu
COMMIT=a1edfc40e8cea3644d80e4e776d3fa7fc397a1c2
BRIDGE_COMMIT=6253dec7f24a57a94039a088cb436d880e92e3be
WOLFSSL_TAG=v5.8.2-stable
PREFIX=/competitor/install

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq --no-install-recommends ca-certificates git gcc g++ make autoconf automake libtool \
    default-jdk python-is-python3 meson ninja-build libegl-dev pkg-config libbsd-dev libunwind-dev \
    libelf-dev valgrind libicu-dev libssl-dev libexpat1-dev zip unzip liblz4-dev liblzma-dev libcap-dev \
    zlib1g-dev >/dev/null

mkdir -p /competitor/src
cd /competitor/src

[ -d wolfssl ] || git clone -q --depth 1 -b "$WOLFSSL_TAG" https://github.com/wolfSSL/wolfssl.git
cd wolfssl
./autogen.sh >/dev/null 2>&1
./configure -q --prefix="$PREFIX" --enable-shared --disable-opensslall --disable-opensslextra \
    --enable-aescbc-length-checks --enable-curve25519 --enable-ed25519 --enable-ed25519-stream \
    --enable-oldtls --enable-base64encode --enable-tlsx --enable-scrypt --disable-examples --enable-crl \
    --with-rsa --enable-certs --enable-session-certs --enable-encrypted-keys --enable-cert-gen \
    --enable-cert-ext --enable-clr-monitor --enable-jni
make -s -j6 install >/dev/null
cd ..

export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:$PREFIX/lib64/pkgconfig:$PREFIX/lib/x86_64-linux-gnu/pkgconfig"
export LD_LIBRARY_PATH="$PREFIX/lib:$PREFIX/lib64"
export CPATH="$PREFIX/include" LIBRARY_PATH="$PREFIX/lib:$PREFIX/lib64"

if [ ! -d bionic_translation ]; then
    git clone -q https://gitlab.com/android_translation_layer/bionic_translation.git
    git -C bionic_translation checkout -q "$BRIDGE_COMMIT"
fi
cd bionic_translation
[ -d build ] || meson setup build --prefix="$PREFIX" --buildtype=release >/dev/null
ninja -C build install >/dev/null
cd ..

if [ ! -d art_standalone ]; then
    git clone -q https://gitlab.com/android_translation_layer/art_standalone.git
    git -C art_standalone checkout -q "$COMMIT"
fi
cd art_standalone
make -j6 ____PREFIX="$PREFIX" ____LIBDIR=lib 2>&1 | tail -15 | cut -c1-220
make ____PREFIX="$PREFIX" ____LIBDIR=lib install 2>&1 | tail -3 | cut -c1-220
du -sh "$PREFIX"
ls "$PREFIX/bin" | head -20
