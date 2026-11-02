# Development and test environment only. uxdroid itself runs apps without containers.
FROM rust:1.99-slim-trixie@sha256:b8e062b1c3a80183df83d9ba187cadd2b5ab84bc78d453dbfeb2fccd5f982247

RUN rustup component add clippy rustfmt \
    && mkdir -p /cache \
    && chown 1000:1000 /cache

ENV CARGO_HOME=/cache/cargo \
    CARGO_TARGET_DIR=/cache/target

WORKDIR /work
USER 1000:1000
