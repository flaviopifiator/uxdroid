# Loader benchmark

Measures how long each loader takes to load a real library and exit, in a fresh process with a warm page cache.

## Run it

```sh
docker compose build dev
docker compose run --rm dev cargo build --release -p uxdroid-cli
docker compose --profile bench build bench

# Extract the x86_64 libraries of RustDesk 1.5.1 outside the repository.
mkdir -p "$SAMPLES/rustdesk"
unzip -oj rustdesk-1.5.1-universal-signed.apk 'lib/x86_64/*.so' -d "$SAMPLES/rustdesk"

UXDROID_SAMPLES="$SAMPLES" docker compose --profile bench run --rm bench sh /work/benches/loader/run.sh
```

## What is compared

| Label | Loader |
|-------|--------|
| `uxdroid` | `uxdroid load`, release build |
| `bionic_translation-no-logs` | The loader used by Android Translation Layer, with debug logging compiled out |
| `bionic_translation` | The same loader as its build ships, with `LINKER_DEBUG=1` |
| `glibc-ld.so` | The system linker, only for `libapp.so`, the one library with no imports |

`bionic_translation` is built from a pinned commit in `Dockerfile` and is never copied into uxdroid.

## Last result

2026-11-02, RustDesk 1.5.1 x86_64 libraries, 50 runs each, in one Docker container on one x86_64 machine. Median wall time, with peak memory in parentheses.

| Library | `uxdroid` | `bionic_translation-no-logs` | `bionic_translation` | `glibc-ld.so` |
|---------|-----------|------------------------------|----------------------|---------------|
| `libapp.so`, 12 MiB | 0.75 ms (2.4 MiB) | 10.66 ms (14.8 MiB) | 10.54 ms (14.9 MiB) | 0.56 ms (1.3 MiB) |
| `libc++_shared.so`, 1.5 MiB | 1.29 ms (3.6 MiB) | 5.16 ms (5.9 MiB) | 22.05 ms (5.9 MiB) | Cannot load it |
| `libflutter.so`, 11 MiB | 1.84 ms (5.7 MiB) | 15.51 ms (17.9 MiB) | 72.52 ms (17.9 MiB) | Cannot load it |
| `librustdesk.so`, 40 MiB | 3.96 ms (10.3 MiB) | 46.15 ms (52.5 MiB) | 279.39 ms (52.4 MiB) | Cannot load it |

uxdroid was 4 to 14 times faster than `bionic_translation-no-logs`, and used less memory, on these libraries. The system linker was still 0.19 ms faster on the one library it can load.

## Limits

- Platform libraries are stubs, the same ones for every loader, until milestone 3. Constructors run, and no app function is called.
- Load time is one workload. It says nothing about app startup or graphics.
- Waydroid and the Android emulator are not measured: they need kernel modules or a VM, and they boot a whole system.
