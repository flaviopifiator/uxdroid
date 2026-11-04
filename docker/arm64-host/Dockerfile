# The aarch64 host build: the dev image plus Rust's aarch64-unknown-linux-gnu target, Debian's
# aarch64 cross linker and glibc, and QEMU user to run what it builds on an x86_64 machine. Build
# `dev` first: it is the base. Development and test only; nothing here is shipped.
FROM uxdroid-dev
USER root
# The host libraries uxdroid opens at run time, xkbcommon, ALSA, and EGL and GLES with Mesa's
# llvmpipe for apps that draw with the GPU, come as Debian's arm64 builds beside the dev image's
# x86_64 ones. The compositor stays the dev image's x86_64 Weston: Wayland is a socket.
RUN dpkg --add-architecture arm64 \
    && apt-get update \
    && apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross \
        qemu-user libxkbcommon0:arm64 libasound2t64:arm64 xkb-data \
        libegl1:arm64 libgles2:arm64 libegl-mesa0:arm64 libgl1-mesa-dri:arm64 \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add aarch64-unknown-linux-gnu
# QEMU_LD_PREFIX lets qemu-aarch64 find the aarch64 glibc. Tests that start the built `uxdroid`
# run it through UXDROID_TEST_RUNNER, since the host kernel has no binfmt entry for aarch64.
ENV CARGO_TARGET_DIR=/cache/target-arm64-host \
    CARGO_BUILD_TARGET=aarch64-unknown-linux-gnu \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUNNER=qemu-aarch64 \
    QEMU_LD_PREFIX=/usr/aarch64-linux-gnu \
    UXDROID_TEST_RUNNER=qemu-aarch64
USER 1000:1000
