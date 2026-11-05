# The arm64 translation engine and its benchmark. Build context: the repository root.
#
# The `engine` stage builds libuxdroid_dynarmic.so, Dynarmic's A64 JIT behind a C interface, from
# the sources pinned in sources.txt. The final stage is the dev image plus that library, QEMU user
# (a benchmark competitor, never shipped) and the compute kernel's zlib source.
FROM debian:trixie-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4 AS engine
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl cmake make g++ libboost-dev patch \
    && rm -rf /var/lib/apt/lists/*
COPY docker/translation/fetch.sh docker/translation/sources.txt docker/translation/dynarmic.patch /src/
RUN sh /src/fetch.sh /src/sources.txt /src \
    && patch -d /src/dynarmic -p1 < /src/dynarmic.patch
COPY crates/translate/shim /src/shim
RUN cmake -S /src/shim -B /build -DCMAKE_BUILD_TYPE=Release -DDYNARMIC_SOURCE=/src/dynarmic \
    && cmake --build /build --target uxdroid_dynarmic -j"$(nproc)" \
    && mkdir -p /out && cp /build/libuxdroid_dynarmic.so /out/ \
    && strip --strip-unneeded /out/libuxdroid_dynarmic.so

FROM uxdroid-dev AS translation
USER root
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl qemu-user \
    && rm -rf /var/lib/apt/lists/*
COPY docker/translation/fetch.sh docker/translation/bench-sources.txt /tmp/
RUN sh /tmp/fetch.sh /tmp/bench-sources.txt /opt && rm /tmp/fetch.sh /tmp/bench-sources.txt
COPY --from=engine /out/libuxdroid_dynarmic.so /opt/uxdroid/lib/libuxdroid_dynarmic.so
ENV UXDROID_TRANSLATOR=/opt/uxdroid/lib/libuxdroid_dynarmic.so \
    UXDROID_ZLIB=/opt/zlib
USER 1000:1000
