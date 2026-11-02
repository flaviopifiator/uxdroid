# Builds ART for the linux-x86_64 glibc host from AOSP. Development environment only.
FROM ubuntu@sha256:fe81560990fae86dc4743202b2cb4d3df98d0bcf949173cecef15ce1127ef852

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl git python3 repo \
       build-essential unzip zip rsync bison flex \
    && mkdir -p /aosp /runtime \
    && chown 1000:1000 /aosp /runtime

USER 1000:1000
WORKDIR /aosp
ENV GIT_CONFIG_COUNT=2 \
    GIT_CONFIG_KEY_0=user.name GIT_CONFIG_VALUE_0=uxdroid \
    GIT_CONFIG_KEY_1=user.email GIT_CONFIG_VALUE_1=uxdroid@invalid
