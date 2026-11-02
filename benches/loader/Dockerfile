# Benchmark environment. bionic_translation is only a comparison target: it is built here and
# never copied into uxdroid. Build `uxdroid-dev` first.
FROM uxdroid-dev

USER root
ARG COMPETITOR_COMMIT=6253dec7f24a57a94039a088cb436d880e92e3be
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git meson ninja-build pkg-config \
       g++ unzip libbsd-dev libegl-dev libelf-dev libunwind-dev \
    && git clone https://gitlab.com/android_translation_layer/bionic_translation.git /opt/competitor-src \
    && git -C /opt/competitor-src checkout "${COMPETITOR_COMMIT}" \
    && cd /opt/competitor-src \
    && meson setup build --prefix=/opt/competitor --buildtype=release \
    && ninja -C build install \
    && sed -i "s/-DLINKER_DEBUG=1/-DLINKER_DEBUG=0/; s/-DVERBOSE_FUNCTIONS=1/-DVERBOSE_FUNCTIONS=0/" meson.build \
    && meson setup build-quiet --prefix=/opt/competitor-quiet --buildtype=release \
    && ninja -C build-quiet install

COPY benches/loader/competitor_load.c benches/loader/glibc_load.c benches/loader/measure.c /opt/bench/
RUN cd /opt/bench \
    && gcc -O2 measure.c -o measure \
    && gcc -O2 glibc_load.c -o glibc_load \
    && gcc -O2 -D_GNU_SOURCE -include dlfcn.h -rdynamic competitor_load.c /opt/competitor-src/main_executable/bionic_compat.c \
       -L/opt/competitor/lib64 -l:libdl_bio.so.0 -o competitor_load

USER 1000:1000
