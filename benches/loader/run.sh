#!/bin/sh
# Compares load time of real libraries across loaders, each in a fresh process.
# Expects extracted x86_64 libraries in /samples/rustdesk (see benches/loader/README.md).
set -eu

RUNS="${RUNS:-50}"
LIBS=/samples/rustdesk
WORK=$(mktemp -d)
TOOLCHAIN="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin"
BENCH=/opt/bench
UXDROID=/cache/target/release/uxdroid

# Platform libraries are milestone 3. All loaders get the same stubs, which are never called.
# Each library gets every symbol, as only the libraries a workload needs join its search scope.
STUBS="$WORK/stubs"
mkdir -p "$STUBS"
sed -E 's/(.*)/void \1(void) { __builtin_trap(); }/' /work/benches/loader/platform_symbols.txt > "$WORK/platform.c"
for name in libEGL libandroid libGLESv2 libOpenSLES libjnigraphics libmediandk; do
    "$TOOLCHAIN/x86_64-linux-android23-clang" -shared "$WORK/platform.c" -o "$STUBS/$name.so"
done

# The competitor expects its host application to provide liblog, and its configuration maps
# platform libraries to system ones. Keep only its libc mappings, so that it also uses the stubs.
COMPETITOR_STUBS="$WORK/competitor-stubs"
mkdir -p "$COMPETITOR_STUBS"
cat > "$WORK/log.c" <<'LOG'
int __android_log_print(void) { return 0; }
int __android_log_vprint(void) { return 0; }
int __android_log_write(void) { return 0; }
int __android_log_buf_write(void) { return 0; }
void __android_log_assert(void) { __builtin_trap(); }
LOG
"$TOOLCHAIN/x86_64-linux-android23-clang" -shared "$WORK/log.c" -o "$COMPETITOR_STUBS/liblog.so"

competitor_config() {
    prefix="$1"
    config="$WORK/config-$(basename "$prefix")/bionic_translation/cfg.d"
    mkdir -p "$config"
    grep -E '^(libc|libstdc\+\+)\.so' "$prefix/share/bionic_translation/cfg.d/bionic_translation.cfg" \
        > "$config/bionic_translation.cfg"
    dirname "$(dirname "$config")"
}
QUIET_CONFIG=$(competitor_config /opt/competitor-quiet)
SHIPPED_CONFIG=$(competitor_config /opt/competitor)

measure() {
    label="$1"
    shift
    printf '  %-26s ' "$label"
    "$BENCH/measure" "$RUNS" "$@" || true
}

# Runs the competitor's build as shipped, or with its debug logging compiled out.
competitor() {
    label="$1"
    prefix="$2"
    config="$3"
    library="$4"
    LD_LIBRARY_PATH="$prefix/lib64" XDG_DATA_DIRS="$config" \
        measure "$label" "$BENCH/competitor_load" "$library" "$LIBS:$STUBS:$COMPETITOR_STUBS"
}

for name in libapp.so libc++_shared.so libflutter.so librustdesk.so; do
    echo "$name ($(du -k "$LIBS/$name" | cut -f1) KiB)"
    measure uxdroid "$UXDROID" load "$LIBS/$name" --search-dir "$STUBS"
    competitor bionic_translation-no-logs /opt/competitor-quiet "$QUIET_CONFIG" "$LIBS/$name"
    competitor bionic_translation /opt/competitor "$SHIPPED_CONFIG" "$LIBS/$name"
    # The system linker cannot load the others: they need bionic's libc.so and each other.
    if [ "$name" = libapp.so ]; then
        measure glibc-ld.so "$BENCH/glibc_load" "$LIBS/$name"
    fi
done
