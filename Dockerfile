# Development and test environment only. uxdroid itself runs apps without containers.
FROM rust:1.99-slim-trixie@sha256:b8e062b1c3a80183df83d9ba187cadd2b5ab84bc78d453dbfeb2fccd5f982247 AS base

# The NDK builds the bionic-linked fixtures that prove the native bridge works.
FROM base AS ndk
ARG NDK_VERSION=r30
# Published at https://developer.android.com/ndk/downloads
ARG NDK_SHA1=5107f898313790e449e87eee2183d9a20602dee9
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl unzip \
    && curl -fsSLo /tmp/ndk.zip "https://dl.google.com/android/repository/android-ndk-${NDK_VERSION}-linux.zip" \
    && echo "${NDK_SHA1}  /tmp/ndk.zip" | sha1sum -c - \
    && unzip -q /tmp/ndk.zip "android-ndk-${NDK_VERSION}/toolchains/llvm/prebuilt/linux-x86_64/*" -d /opt \
    && mv "/opt/android-ndk-${NDK_VERSION}" /opt/android-ndk \
    && rm /tmp/ndk.zip

FROM base AS dev
COPY --from=ndk /opt/android-ndk /opt/android-ndk

# Weston's headless backend gives tests a real Wayland compositor. Mesa serves the EGL and GLES that
# app native code links against, with llvmpipe where there is no GPU.
RUN apt-get update \
    && apt-get install -y --no-install-recommends weston python3-minimal \
        libegl1 libgles2 libegl-mesa0 libgl1-mesa-dri \
    && rustup component add clippy rustfmt \
    && mkdir -p /cache \
    && chown 1000:1000 /cache

ENV CARGO_HOME=/cache/cargo \
    CARGO_TARGET_DIR=/cache/target \
    ANDROID_NDK_ROOT=/opt/android-ndk

WORKDIR /work
USER 1000:1000
