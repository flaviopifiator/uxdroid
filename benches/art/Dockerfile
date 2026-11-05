# Runs uxdroid's ART and art_standalone side by side. Both are mounted read-only from volumes.
FROM debian:trixie-slim@sha256:f6b08b09a05725f9431502d3b5714f5c4576e82c1a84fa1debe0c24a7e0c3ec4

RUN apt-get update \
    && apt-get install -y --no-install-recommends gcc libc6-dev libbsd0 libunwind8 libelf1t64 libicu76 \
       libssl3t64 libexpat1 liblz4-1 liblzma5 libcap2 zlib1g libpng16-16t64 libegl1 default-jre-headless
COPY benches/art/first_output.c /opt/first_output.c
RUN gcc -O2 /opt/first_output.c -o /opt/first_output && rm /opt/first_output.c
# art_standalone needs a passwd entry, as a normal system would have.
RUN useradd -u 1000 -m bench
USER 1000:1000
