# Builds uxdroid's packages. Ubuntu 22.04's glibc 2.35 is the oldest of the distributions packages
# are tested on, so what links here runs on all of them. amd64 builds natively and arm64 through
# Ubuntu's aarch64 cross toolchain. Build context: the repository root.
FROM ubuntu:22.04@sha256:076e7bf82319619f28dd0aa9c02ab34b055ad37100395b4f49a2ffba04fb5e11
ARG RUST_VERSION=1.99.0
ARG RUSTUP_VERSION=1.28.2
# Published beside the binary, at .../rustup-init.sha256
ARG RUSTUP_SHA256=20a06e644b0d9bd2fbdbfd52d42540bdde820ea7df86e92e533c073da0cdd43c
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl gcc libc6-dev \
        gcc-aarch64-linux-gnu libc6-dev-arm64-cross binutils-aarch64-linux-gnu dpkg-dev python3 \
    && rm -rf /var/lib/apt/lists/* \
    && curl -fsSLo /tmp/rustup-init \
        "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
    && echo "${RUSTUP_SHA256}  /tmp/rustup-init" | sha256sum -c - \
    && chmod +x /tmp/rustup-init \
    && CARGO_HOME=/opt/cargo RUSTUP_HOME=/opt/rustup /tmp/rustup-init -y --no-modify-path \
        --profile minimal --default-toolchain "${RUST_VERSION}" --target aarch64-unknown-linux-gnu \
    && rm /tmp/rustup-init \
    && chmod -R a+rX /opt/cargo /opt/rustup \
    && mkdir -p /cache && chmod 1777 /cache
ENV RUSTUP_HOME=/opt/rustup \
    CARGO_HOME=/cache/cargo \
    PATH=/opt/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
    CARGO_TARGET_DIR=/cache/target-package \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
WORKDIR /work
