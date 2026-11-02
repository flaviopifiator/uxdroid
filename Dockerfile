# Development and test environment only. uxdroid itself runs apps without containers.
FROM rust:1.99-slim-trixie

RUN rustup component add clippy rustfmt \
    && mkdir -p /cache \
    && chown 1000:1000 /cache

ENV CARGO_HOME=/cache/cargo \
    CARGO_TARGET_DIR=/cache/target

WORKDIR /work
USER 1000:1000
